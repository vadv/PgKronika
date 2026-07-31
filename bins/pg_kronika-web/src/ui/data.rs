//! Web-index-only DTO assembly for UI data endpoints.

use std::collections::BTreeSet;

use kronika_analytics::web_projection::web_views;
use kronika_reader::{
    CollectionReadState, CollectionStatus, CollectionVisibility, IndexStatus, LiveState, LiveView,
    LocalDirSnapshot, Notability, NotableLevel, UiSummaryBlock, WebIndexReadError,
};
use serde::Serialize;
use utoipa::ToSchema;

use super::snapshot::read_summary_tolerant;

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ViewSummaryResponse {
    at_us: String,
    views: Vec<ViewSummaryItem>,
    quality: SummaryQuality,
}

#[derive(Debug, Serialize, ToSchema)]
struct ViewSummaryItem {
    view: &'static str,
    #[schema(required = true)]
    snapshot_ts_us: Option<String>,
    #[schema(required = true)]
    population: Option<u64>,
    status: &'static str,
    notable: bool,
    notable_level: NotableLevelDto,
    notable_count: u64,
    #[schema(required = true)]
    collection: Option<CollectionStatusDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum NotableLevelDto {
    None,
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Serialize, ToSchema)]
struct CollectionStatusDto {
    collected: u64,
    #[schema(required = true)]
    source_total: Option<u64>,
    read_state: &'static str,
    visibility: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
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
    snapshot: Option<(i64, u64, Notability)>,
    collection: Option<CollectionStatus>,
    status: IndexStatus,
}

pub(crate) fn view_summary(
    snapshot: &LocalDirSnapshot,
    live: &LiveView,
    at_us: i64,
) -> Result<ViewSummaryResponse, WebIndexReadError> {
    let mut descriptors = snapshot.sealed_descriptors().collect::<Vec<_>>();
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));

    let mut resolved = vec![None; web_views().len()];
    let mut active_tail = false;
    if live.state() == LiveState::Current {
        for facts in live
            .chunks()
            .iter()
            .rev()
            .filter(|facts| facts.identity().source_min_ts_us <= at_us)
        {
            active_tail = true;
            resolve_summary(facts.ui_summary(), at_us, &mut resolved);
            if resolved.iter().all(Option::is_some) {
                break;
            }
        }
    }
    let mut incompatible_seen = false;
    for descriptor in descriptors
        .iter()
        .rev()
        .filter(|descriptor| descriptor.min_ts <= at_us)
    {
        let Some(summary) = read_summary_tolerant(snapshot, descriptor)? else {
            incompatible_seen = true;
            continue;
        };
        resolve_summary(&summary, at_us, &mut resolved);
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
            if let Some((timestamp, _population, _notability)) = exact {
                snapshots.insert(timestamp);
            }
            match resolved.map(|resolved| resolved.status) {
                Some(IndexStatus::Gated) => gated.push(view.name),
                Some(IndexStatus::UnsupportedType) => unavailable_revision.push(view.name),
                Some(IndexStatus::ResourceLimited) => resource_limited.push(view.name),
                None if incompatible_seen => unavailable_revision.push(view.name),
                _ => {}
            }
            ViewSummaryItem {
                view: view.name,
                snapshot_ts_us: exact
                    .map(|(timestamp, _population, _notability)| timestamp.to_string()),
                population: exact.map(|(_timestamp, population, _notability)| population),
                status,
                notable: exact.is_some_and(|(_timestamp, _population, notability)| {
                    notability.level() != NotableLevel::None
                }),
                notable_level: exact.map_or(
                    NotableLevelDto::None,
                    |(_timestamp, _population, notability)| notable_level(notability.level()),
                ),
                notable_count: exact.map_or(0, |(_timestamp, _population, notability)| {
                    notability.count()
                }),
                collection: resolved
                    .and_then(|resolved| resolved.collection)
                    .map(collection_status_dto),
            }
        })
        .collect();
    let partial = resolved.iter().any(Option::is_none)
        || !gated.is_empty()
        || !unavailable_revision.is_empty()
        || !resource_limited.is_empty()
        || !matches!(live.state(), LiveState::Empty | LiveState::Current);

    Ok(ViewSummaryResponse {
        at_us: at_us.to_string(),
        views,
        quality: SummaryQuality {
            status: if partial { "partial" } else { "complete" },
            snapshots: snapshots.len(),
            gaps: Vec::new(),
            gated,
            unavailable_revision,
            resource_limited,
            active_tail,
        },
    })
}

fn resolve_summary(summary: &UiSummaryBlock, at_us: i64, resolved: &mut [Option<ResolvedView>]) {
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
        let exact = summary.snapshot_notability_at(view.code, at_us);
        let collection = exact.and_then(|(timestamp, _population, _notability)| {
            summary
                .collection_state_at(view.code, timestamp)
                .filter(|(collection_ts, _status)| *collection_ts == timestamp)
                .map(|(_collection_ts, status)| status)
        });
        if exact.is_some() || block_view.status() != IndexStatus::Complete {
            resolved[index] = Some(ResolvedView {
                snapshot: exact,
                collection,
                status: block_view.status(),
            });
        }
    }
}

const fn notable_level(level: NotableLevel) -> NotableLevelDto {
    match level {
        NotableLevel::None => NotableLevelDto::None,
        NotableLevel::Info => NotableLevelDto::Info,
        NotableLevel::Warning => NotableLevelDto::Warning,
        NotableLevel::Critical => NotableLevelDto::Critical,
    }
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

const fn collection_status_dto(status: CollectionStatus) -> CollectionStatusDto {
    CollectionStatusDto {
        collected: status.collected(),
        source_total: status.source_total(),
        read_state: collection_read_state(status.read_state()),
        visibility: collection_visibility(status.visibility()),
    }
}

const fn collection_read_state(state: CollectionReadState) -> &'static str {
    match state {
        CollectionReadState::Complete => "complete",
        CollectionReadState::SourceLimit => "source_limit",
        CollectionReadState::Permission => "permission",
        CollectionReadState::ReadFailure => "read_failure",
        CollectionReadState::CollectorLimitOrLoss => "collector_limit_or_loss",
    }
}

const fn collection_visibility(visibility: CollectionVisibility) -> &'static str {
    match visibility {
        CollectionVisibility::Full => "full",
        CollectionVisibility::Restricted => "restricted",
        CollectionVisibility::Unknown => "unknown",
    }
}
