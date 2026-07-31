//! Descriptor and UI-summary-only retained-data quality projection.

use std::collections::{BTreeMap, BTreeSet};

use kronika_analytics::web_projection::web_views;
use kronika_layout::{ProducerState, ProducerStatus};
use kronika_reader::{CollectionReadState, IndexStatus, LIMIT, LocalDirSnapshot};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DataQualityRequest {
    pub(crate) from_us: i64,
    pub(crate) to_us: i64,
    pub(crate) stale_after_us: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DataQualityResponse {
    status: &'static str,
    freshness: FreshnessDto,
    producer: ProducerDto,
    coverage: CoverageDto,
    gaps: Vec<QualityGap>,
    capabilities: Vec<CapabilityDto>,
    integrity: IntegrityDto,
    quality: QualityDto,
}

#[derive(Debug, Serialize, ToSchema)]
struct FreshnessDto {
    #[schema(required = true)]
    data_through_us: Option<String>,
    #[schema(required = true)]
    age_us: Option<String>,
    #[schema(required = true)]
    expected_period_us: Option<String>,
    state: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
struct ProducerDto {
    state: &'static str,
    #[schema(required = true)]
    collector_pid: Option<u32>,
    #[schema(required = true)]
    collector_started_at_us: Option<String>,
    #[schema(required = true)]
    last_status_at_us: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct CoverageDto {
    #[schema(required = true)]
    expected_snapshots: Option<u64>,
    observed_snapshots: u64,
    complete_snapshots: u64,
}

#[derive(Debug, Serialize, ToSchema)]
struct QualityGap {
    from_us: String,
    to_us: String,
    reason: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
struct CapabilityDto {
    kind: &'static str,
    code: &'static str,
    status: &'static str,
    #[schema(required = true)]
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct IntegrityDto {
    status: &'static str,
    readable_segments: usize,
    corrupt_segments: usize,
    quarantined_entries: usize,
    #[schema(required = true)]
    last_catalog_refresh_us: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct QualityDto {
    status: &'static str,
    resource_limited: Vec<&'static str>,
    active_tail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreshnessState {
    Fresh,
    Late,
    Stale,
    Unknown,
}

impl FreshnessState {
    const fn wire(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Late => "late",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy)]
struct CapabilityState {
    available: bool,
    partial: bool,
    unsupported: bool,
    resource_limited: bool,
}

impl Default for CapabilityState {
    fn default() -> Self {
        Self {
            available: false,
            partial: false,
            unsupported: false,
            resource_limited: false,
        }
    }
}

pub(crate) fn build_data_quality(
    snapshot: &LocalDirSnapshot,
    producer_status: Option<ProducerStatus>,
    request: DataQualityRequest,
) -> DataQualityResponse {
    let mut descriptors = snapshot
        .sealed_descriptors()
        .filter(|descriptor| {
            descriptor.max_ts >= request.from_us && descriptor.min_ts < request.to_us
        })
        .collect::<Vec<_>>();
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));
    let coverage_spans = descriptors
        .iter()
        .map(|descriptor| (descriptor.min_ts, descriptor.max_ts))
        .collect::<Vec<_>>();
    let mut observed = BTreeSet::new();
    let mut complete = BTreeSet::new();
    let mut capability_states = BTreeMap::<u16, CapabilityState>::new();
    let mut readable_segments = 0_usize;
    let mut corrupt_segments = 0_usize;
    let mut resource_limited = BTreeSet::new();
    for descriptor in &descriptors {
        let Ok((summary, _stats)) = snapshot.read_ui_summary(descriptor, &LIMIT) else {
            corrupt_segments = corrupt_segments.saturating_add(1);
            continue;
        };
        readable_segments = readable_segments.saturating_add(1);
        for timestamp in summary
            .snapshot_times()
            .iter()
            .copied()
            .filter(|timestamp| *timestamp >= request.from_us && *timestamp < request.to_us)
        {
            observed.insert(timestamp);
            if summary.views().iter().all(|view| {
                summary
                    .collection_state_at(view.view_code(), timestamp)
                    .is_some_and(|(collected_at, collection)| {
                        collected_at == timestamp
                            && collection.read_state() == CollectionReadState::Complete
                    })
            }) {
                complete.insert(timestamp);
            }
        }
        for view in summary.views() {
            let state = capability_states.entry(view.view_code()).or_default();
            match view.status() {
                IndexStatus::Complete | IndexStatus::Empty => state.available = true,
                IndexStatus::Gated => state.partial = true,
                IndexStatus::UnsupportedType => state.unsupported = true,
                IndexStatus::ResourceLimited => {
                    state.resource_limited = true;
                    if let Some(web_view) = web_views()
                        .iter()
                        .find(|candidate| candidate.code == view.view_code())
                    {
                        resource_limited.insert(web_view.name);
                    }
                }
            }
        }
    }

    let data_through_us = observed.iter().next_back().copied().or_else(|| {
        descriptors
            .iter()
            .map(|descriptor| descriptor.max_ts.min(request.to_us))
            .max()
    });
    let expected_period_us = observed_period(&observed);
    let age_us = data_through_us.map(|through| request.to_us.saturating_sub(through).max(0));
    let freshness_state = match (age_us, expected_period_us) {
        (Some(age), Some(expected)) if age <= expected => FreshnessState::Fresh,
        (Some(age), Some(_expected)) if age <= request.stale_after_us => FreshnessState::Late,
        (Some(_age), Some(_expected)) => FreshnessState::Stale,
        _ => FreshnessState::Unknown,
    };
    let expected_snapshots = expected_period_us.and_then(|period| {
        let span = request.to_us.checked_sub(request.from_us)?;
        let quotient = span / period;
        let rounded = quotient.checked_add(i64::from(span % period != 0))?;
        u64::try_from(rounded).ok()
    });
    let gaps = coverage_gaps(&coverage_spans, request);
    let coverage_partial = expected_snapshots
        .is_some_and(|expected| u64::try_from(observed.len()).unwrap_or(u64::MAX) < expected);
    let integrity_degraded = corrupt_segments != 0;
    let status = aggregate_status(
        readable_segments != 0,
        freshness_state,
        !gaps.is_empty(),
        coverage_partial,
        integrity_degraded,
    );
    let capabilities = web_views()
        .iter()
        .map(|view| {
            let state = capability_states
                .get(&view.code)
                .copied()
                .unwrap_or_default();
            let (status, reason) = if state.available && !state.partial && !state.resource_limited {
                ("available", None)
            } else if state.available || state.partial || state.resource_limited {
                (
                    "partial",
                    Some(if state.resource_limited {
                        "resource_limited"
                    } else {
                        "not_collected"
                    }),
                )
            } else {
                (
                    "unavailable",
                    Some(if state.unsupported {
                        "unsupported_type"
                    } else {
                        "not_collected"
                    }),
                )
            };
            CapabilityDto {
                kind: "projection",
                code: view.name,
                status,
                reason,
            }
        })
        .collect();
    let producer = producer_dto(producer_status);
    let last_catalog_refresh_us = descriptors
        .iter()
        .map(|descriptor| descriptor.max_ts)
        .max()
        .map(|timestamp| timestamp.to_string());
    DataQualityResponse {
        status,
        freshness: FreshnessDto {
            data_through_us: data_through_us.map(|timestamp| timestamp.to_string()),
            age_us: age_us.map(|age| age.to_string()),
            expected_period_us: expected_period_us.map(|period| period.to_string()),
            state: freshness_state.wire(),
        },
        producer,
        coverage: CoverageDto {
            expected_snapshots,
            observed_snapshots: u64::try_from(observed.len()).unwrap_or(u64::MAX),
            complete_snapshots: u64::try_from(complete.len()).unwrap_or(u64::MAX),
        },
        gaps,
        capabilities,
        integrity: IntegrityDto {
            status: if descriptors.is_empty() {
                "unknown"
            } else if integrity_degraded {
                "degraded"
            } else {
                "complete"
            },
            readable_segments,
            corrupt_segments,
            quarantined_entries: 0,
            last_catalog_refresh_us,
        },
        quality: QualityDto {
            status: if resource_limited.is_empty() {
                "complete"
            } else {
                "partial"
            },
            resource_limited: resource_limited.into_iter().collect(),
            active_tail: false,
        },
    }
}

fn observed_period(observed: &BTreeSet<i64>) -> Option<i64> {
    let mut periods = observed
        .iter()
        .copied()
        .zip(observed.iter().copied().skip(1))
        .filter_map(|(previous, current)| current.checked_sub(previous))
        .filter(|period| *period > 0)
        .collect::<Vec<_>>();
    if periods.is_empty() {
        return None;
    }
    periods.sort_unstable();
    periods.get(periods.len() / 2).copied()
}

fn coverage_gaps(spans: &[(i64, i64)], request: DataQualityRequest) -> Vec<QualityGap> {
    let mut gaps = Vec::new();
    let mut covered_to = request.from_us;
    let mut ordered = spans.to_vec();
    ordered.sort_unstable();
    for (minimum, maximum) in ordered {
        let start = minimum.max(request.from_us);
        let end = maximum.saturating_add(1).min(request.to_us);
        if start > covered_to {
            gaps.push(QualityGap {
                from_us: covered_to.to_string(),
                to_us: start.to_string(),
                reason: "unknown",
            });
        }
        covered_to = covered_to.max(end);
    }
    if covered_to < request.to_us {
        gaps.push(QualityGap {
            from_us: covered_to.to_string(),
            to_us: request.to_us.to_string(),
            reason: "unknown",
        });
    }
    gaps
}

fn producer_dto(status: Option<ProducerStatus>) -> ProducerDto {
    status.map_or(
        ProducerDto {
            state: "unknown",
            collector_pid: None,
            collector_started_at_us: None,
            last_status_at_us: None,
        },
        |status| ProducerDto {
            state: match status.state {
                ProducerState::Running => "running",
                ProducerState::Stopped => "stopped",
            },
            collector_pid: Some(status.collector_pid),
            collector_started_at_us: Some(status.collector_started_at_us.to_string()),
            last_status_at_us: Some(status.last_status_at_us.to_string()),
        },
    )
}

const fn aggregate_status(
    readable: bool,
    freshness: FreshnessState,
    has_gap: bool,
    coverage_partial: bool,
    integrity_degraded: bool,
) -> &'static str {
    if !readable {
        "unavailable"
    } else if matches!(freshness, FreshnessState::Stale) {
        "stale"
    } else if has_gap
        || coverage_partial
        || integrity_degraded
        || matches!(freshness, FreshnessState::Unknown)
    {
        "partial"
    } else if matches!(freshness, FreshnessState::Late) {
        "late"
    } else {
        "fresh"
    }
}

#[cfg(test)]
mod tests {
    use super::{FreshnessState, aggregate_status};

    #[test]
    fn unknown_freshness_cannot_be_reported_as_fresh() {
        assert_eq!(
            aggregate_status(true, FreshnessState::Unknown, false, false, false),
            "partial"
        );
    }
}
