//! Shared bounded snapshot selection for UI projections.

use std::collections::BTreeMap;

use kronika_analytics::web_projection::WebView;
use kronika_reader::{
    IndexStatus, LIMIT, LocalDirSnapshot, SegmentDescriptor, SnapshotNeighbors, WebIndexReadError,
};

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
    for descriptor in descriptors {
        if descriptor.min_ts > at_us {
            continue;
        }
        let (summary, _stats) = snapshot.read_ui_summary(&descriptor, &LIMIT)?;
        let upper = summary
            .snapshot_times()
            .partition_point(|timestamp| *timestamp <= at_us);
        let Some(timestamp_us) = upper
            .checked_sub(1)
            .map(|index| summary.snapshot_times()[index])
        else {
            continue;
        };
        if resolved
            .as_ref()
            .is_none_or(|current: &ResolvedSnapshotAt| timestamp_us > current.timestamp_us)
        {
            resolved = Some(ResolvedSnapshotAt {
                timestamp_us,
                descriptor,
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
    for descriptor in descriptors {
        let (summary, _stats) = snapshot.read_ui_summary(&descriptor, &LIMIT)?;
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
        if descriptor.min_ts <= at_us {
            fallback_quality = Some(quality);
        }
        if view_summary.view_revision() != view.revision {
            continue;
        }
        let evidence = SnapshotEvidence {
            descriptor,
            quality,
        };
        let local_neighbors = summary.snapshot_neighbors(view.code, at_us);
        if let Some(local_neighbors) = local_neighbors {
            if let Some(previous) = local_neighbors.previous {
                snapshots.insert(previous, evidence);
            }
            snapshots.insert(local_neighbors.current, evidence);
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
