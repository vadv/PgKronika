//! OVF-only DTO assembly for UI data endpoints.

use std::collections::BTreeSet;

use kronika_analytics::web_projection::web_views;
use kronika_reader::{IndexStatus, LIMIT, LocalDirSnapshot, WebIndexReadError};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ViewSummaryResponse {
    at_us: String,
    views: Vec<ViewSummaryItem>,
    quality: SummaryQuality,
}

#[derive(Debug, Serialize)]
struct ViewSummaryItem {
    view: &'static str,
    snapshot_ts_us: Option<String>,
    population: Option<u64>,
    status: &'static str,
    notable: bool,
}

#[derive(Debug, Serialize)]
struct SummaryQuality {
    status: &'static str,
    snapshots: usize,
    gaps: Vec<String>,
    gated: Vec<&'static str>,
    unavailable_revision: Vec<&'static str>,
    resource_limited: Vec<&'static str>,
    active_tail: bool,
}

#[derive(Clone, Copy)]
struct ResolvedView {
    snapshot: Option<(i64, u64)>,
    status: IndexStatus,
}

pub(crate) fn view_summary(
    snapshot: &LocalDirSnapshot,
    source: u64,
    at_us: i64,
) -> Result<Option<ViewSummaryResponse>, WebIndexReadError> {
    let mut descriptors = snapshot
        .sealed_descriptors()
        .filter(|descriptor| descriptor.source_id == source)
        .collect::<Vec<_>>();
    if descriptors.is_empty() {
        return Ok(None);
    }
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));

    let mut resolved = vec![None; web_views().len()];
    for descriptor in descriptors
        .iter()
        .rev()
        .filter(|descriptor| descriptor.min_ts <= at_us)
    {
        let (summary, _stats) = snapshot.read_ui_summary(descriptor, &LIMIT)?;
        for (index, view) in web_views().iter().enumerate() {
            if resolved[index].is_some() {
                continue;
            }
            let Some(block_view) = summary
                .views()
                .iter()
                .find(|candidate| candidate.view_code() == view.code)
            else {
                continue;
            };
            let exact = summary.snapshot_at(view.code, at_us);
            if exact.is_some() || block_view.status() != IndexStatus::Complete {
                resolved[index] = Some(ResolvedView {
                    snapshot: exact,
                    status: block_view.status(),
                });
            }
        }
        if resolved.iter().all(Option::is_some) {
            break;
        }
    }

    let mut snapshots = BTreeSet::new();
    let mut gated = Vec::new();
    let mut unavailable_revision = Vec::new();
    let mut resource_limited = Vec::new();
    let views = web_views()
        .iter()
        .enumerate()
        .map(|(index, view)| {
            let resolved = resolved[index].as_ref();
            let status = resolved.map_or("unavailable", |resolved| status_code(resolved.status));
            let exact = resolved.and_then(|resolved| resolved.snapshot);
            if let Some((timestamp, _population)) = exact {
                snapshots.insert(timestamp);
            }
            match resolved.map(|resolved| resolved.status) {
                Some(IndexStatus::Gated) => gated.push(view.name),
                Some(IndexStatus::UnsupportedType) => unavailable_revision.push(view.name),
                Some(IndexStatus::ResourceLimited) => resource_limited.push(view.name),
                _ => {}
            }
            ViewSummaryItem {
                view: view.name,
                snapshot_ts_us: exact.map(|(timestamp, _population)| timestamp.to_string()),
                population: exact.map(|(_timestamp, population)| population),
                status,
                notable: false,
            }
        })
        .collect();
    let partial = resolved.iter().any(Option::is_none)
        || !gated.is_empty()
        || !unavailable_revision.is_empty()
        || !resource_limited.is_empty();

    Ok(Some(ViewSummaryResponse {
        at_us: at_us.to_string(),
        views,
        quality: SummaryQuality {
            status: if partial { "partial" } else { "complete" },
            snapshots: snapshots.len(),
            gaps: Vec::new(),
            gated,
            unavailable_revision,
            resource_limited,
            active_tail: false,
        },
    }))
}

const fn status_code(status: IndexStatus) -> &'static str {
    match status {
        IndexStatus::Complete => "complete",
        IndexStatus::Empty => "empty",
        IndexStatus::Gated => "gated",
        IndexStatus::UnsupportedType => "unsupported_type",
        IndexStatus::ResourceLimited => "resource_limited",
    }
}
