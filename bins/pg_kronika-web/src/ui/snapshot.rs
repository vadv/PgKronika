//! Shared bounded snapshot selection for UI projections.

use std::collections::BTreeMap;

use kronika_analytics::web_projection::WebView;
use kronika_reader::{
    CacheReadError, IndexStatus, LIMIT, LocalDirSnapshot, SegmentDescriptor, SnapshotNeighbors,
    UiSummaryBlock, WebIndexReadError,
};

/// Outcome of a tolerance-aware summary read for one sealed descriptor.
#[derive(Debug)]
pub(crate) enum SummaryRead {
    /// The admitted summary block.
    Block(UiSummaryBlock),
    /// The sidecar was written under another contract: the data is intact,
    /// only the index is stale.
    StaleContract,
    /// The descriptor is published but its derived index is not built yet.
    /// Fact files are built lazily by admitted timeline requests, so this is
    /// an expected transient state, not a storage failure.
    IndexPending,
}

/// Reads one descriptor's summary, degrading a stale-contract or not-yet-built
/// sidecar into a typed skip. Corruption and I/O failures stay hard errors.
pub(crate) fn read_summary_tolerant(
    snapshot: &LocalDirSnapshot,
    descriptor: &SegmentDescriptor,
) -> Result<SummaryRead, WebIndexReadError> {
    match snapshot.read_ui_summary(descriptor, &LIMIT) {
        Ok((summary, _stats)) => Ok(SummaryRead::Block(summary)),
        Err(WebIndexReadError::Cache(CacheReadError::Incompatible)) => {
            Ok(SummaryRead::StaleContract)
        }
        Err(WebIndexReadError::SidecarAbsent) => Ok(SummaryRead::IndexPending),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedSnapshotAt {
    pub(crate) timestamp_us: i64,
    pub(crate) descriptor: SegmentDescriptor,
}

#[derive(Debug)]
pub(crate) struct ResolvedViewSnapshot {
    pub(crate) neighbors: Option<SnapshotNeighbors>,
    pub(crate) current_descriptor: Option<SegmentDescriptor>,
    pub(crate) previous_descriptor: Option<SegmentDescriptor>,
    pub(crate) current_quality: Option<SnapshotSummaryQuality>,
    pub(crate) previous_quality: Option<SnapshotSummaryQuality>,
    pub(crate) fallback_quality: Option<SnapshotSummaryQuality>,
    pub(crate) next: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct SnapshotEvidence {
    descriptor: SegmentDescriptor,
    quality: SnapshotSummaryQuality,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SnapshotSummaryQuality {
    Complete,
    Gated,
    UnavailableRevision,
    ResourceLimited,
}

pub(crate) fn resolve_snapshot_at(
    snapshot: &LocalDirSnapshot,
    at_us: i64,
) -> Result<Option<ResolvedSnapshotAt>, WebIndexReadError> {
    let mut resolved = None;
    let mut descriptors = snapshot.sealed_descriptors().collect::<Vec<_>>();
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));
    // Newest-first: a descriptor whose coverage ends at or before the current
    // best cannot improve it, and neither can anything older.
    for descriptor in descriptors.iter().rev() {
        if descriptor.min_ts > at_us {
            continue;
        }
        if resolved
            .as_ref()
            .is_some_and(|current: &ResolvedSnapshotAt| descriptor.max_ts <= current.timestamp_us)
        {
            break;
        }
        let summary = match read_summary_tolerant(snapshot, descriptor)? {
            SummaryRead::Block(summary) => summary,
            SummaryRead::StaleContract | SummaryRead::IndexPending => continue,
        };
        let upper = summary
            .snapshot_times()
            .partition_point(|timestamp| *timestamp <= at_us);
        let timestamp_us = upper.checked_sub(1).map_or_else(
            || descriptor.max_ts.min(at_us),
            |index| summary.snapshot_times()[index],
        );
        if resolved
            .as_ref()
            .is_none_or(|current: &ResolvedSnapshotAt| timestamp_us > current.timestamp_us)
        {
            resolved = Some(ResolvedSnapshotAt {
                timestamp_us,
                descriptor: *descriptor,
            });
        }
    }
    Ok(resolved)
}

pub(crate) fn resolve_view_snapshot(
    snapshot: &LocalDirSnapshot,
    view: &WebView,
    at_us: i64,
) -> Result<ResolvedViewSnapshot, WebIndexReadError> {
    let mut snapshots = BTreeMap::<i64, SnapshotEvidence>::new();
    let mut fallback_quality = None;
    let mut next = None;
    let mut descriptors = snapshot.sealed_descriptors().collect::<Vec<_>>();
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));
    // Newest-first with first-wins inserts (equivalent to the old ascending
    // last-wins): once a runner-up exists, a descriptor ending at or before it
    // — and anything older — cannot displace either neighbor candidate.
    for descriptor in descriptors.iter().rev() {
        let settled = snapshots
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|(runner_up, _)| descriptor.max_ts <= *runner_up);
        if settled {
            break;
        }
        let summary = match read_summary_tolerant(snapshot, descriptor)? {
            SummaryRead::Block(summary) => summary,
            SummaryRead::StaleContract => {
                if fallback_quality.is_none() && descriptor.min_ts <= at_us {
                    fallback_quality = Some(SnapshotSummaryQuality::UnavailableRevision);
                }
                continue;
            }
            // A pending index answers nothing about the view; reporting a
            // revision or resource verdict for it would be a false reason.
            SummaryRead::IndexPending => continue,
        };
        let Some(view_summary) = summary
            .views()
            .iter()
            .find(|candidate| candidate.view_code() == view.code)
        else {
            continue;
        };
        let quality = summary_quality(
            view_summary.view_revision(),
            view.revision,
            view_summary.status(),
        );
        // The fallback answers "how did the newest segment see this view"
        // when the exact snapshot cannot be resolved.
        if fallback_quality.is_none() && descriptor.min_ts <= at_us {
            fallback_quality = Some(quality);
        }
        if view_summary.view_revision() != view.revision {
            continue;
        }
        let evidence = SnapshotEvidence {
            descriptor: *descriptor,
            quality,
        };
        let local_neighbors = summary.snapshot_neighbors(view.code, at_us);
        if let Some(local_neighbors) = local_neighbors {
            if let Some(previous) = local_neighbors.previous {
                snapshots.entry(previous).or_insert(evidence);
            }
            snapshots.entry(local_neighbors.current).or_insert(evidence);
        }
        let local_next = local_neighbors.map_or_else(
            || {
                let upper = summary
                    .snapshot_times()
                    .partition_point(|timestamp| *timestamp <= at_us);
                (upper..summary.snapshot_times().len())
                    .find(|index| {
                        view_summary
                            .snapshot_presence()
                            .get(index / 8)
                            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
                    })
                    .map(|index| summary.snapshot_times()[index])
            },
            |local_neighbors| local_neighbors.next,
        );
        if let Some(local_next) = local_next {
            next = Some(next.map_or(local_next, |current: i64| current.min(local_next)));
        }
    }

    let mut newest = snapshots.iter().rev();
    let current_evidence = newest
        .next()
        .map(|(timestamp, evidence)| (*timestamp, *evidence));
    let previous_evidence = newest
        .next()
        .map(|(timestamp, evidence)| (*timestamp, *evidence));
    let neighbors = current_evidence.map(|(current, _evidence)| SnapshotNeighbors {
        previous: previous_evidence.map(|(previous, _evidence)| previous),
        current,
        next,
    });
    Ok(ResolvedViewSnapshot {
        neighbors,
        current_descriptor: current_evidence.map(|(_timestamp, evidence)| evidence.descriptor),
        previous_descriptor: previous_evidence.map(|(_timestamp, evidence)| evidence.descriptor),
        current_quality: current_evidence.map(|(_timestamp, evidence)| evidence.quality),
        previous_quality: previous_evidence.map(|(_timestamp, evidence)| evidence.quality),
        fallback_quality,
        next,
    })
}

const fn summary_quality(
    actual_revision: u16,
    expected_revision: u16,
    status: IndexStatus,
) -> SnapshotSummaryQuality {
    if actual_revision != expected_revision {
        return SnapshotSummaryQuality::UnavailableRevision;
    }
    match status {
        IndexStatus::Complete | IndexStatus::Empty => SnapshotSummaryQuality::Complete,
        IndexStatus::Gated => SnapshotSummaryQuality::Gated,
        IndexStatus::UnsupportedType => SnapshotSummaryQuality::UnavailableRevision,
        IndexStatus::ResourceLimited => SnapshotSummaryQuality::ResourceLimited,
    }
}
