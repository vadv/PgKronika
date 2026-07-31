//! Bounded merge of internal host-signal OVF blocks.

use kronika_reader::{
    EntityMetric, EntitySeriesBlock, HOST_SIGNALS_IDENTITY_REVISION, HOST_SIGNALS_VIEW_CODE,
    HOST_SIGNALS_VIEW_REVISION, IndexStatus, LIMIT, LOAD_PER_CPU_METRIC_CODE, LiveState, LiveView,
    LocalDirSnapshot, MetricAggregation, MetricStatus, PSI_IO_SOME_METRIC_CODE, WebIndexReadError,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::overview::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS;

const LOAD_UNIT_CODE: u16 = 4;
const PSI_UNIT_CODE: u16 = 7;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SpineRequest {
    pub(crate) from_us: i64,
    pub(crate) to_us: i64,
    pub(crate) bucket_count: usize,
}

#[derive(Debug)]
pub(crate) enum SpineError {
    Read(WebIndexReadError),
    TooManySegments,
    Arithmetic,
}

impl std::fmt::Display for SpineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(f, "{error}"),
            Self::TooManySegments => f.write_str("spine segment limit exceeded"),
            Self::Arithmetic => f.write_str("spine arithmetic overflow"),
        }
    }
}

impl std::error::Error for SpineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::TooManySegments | Self::Arithmetic => None,
        }
    }
}

impl From<WebIndexReadError> for SpineError {
    fn from(error: WebIndexReadError) -> Self {
        Self::Read(error)
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SpineResponse {
    grid: SpineGrid,
    series: Vec<SpineSeries>,
    quality: SpineQuality,
}

#[derive(Debug, Serialize, ToSchema)]
struct SpineGrid {
    from_us: String,
    to_us: String,
    bucket_count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
struct SpineSeries {
    code: &'static str,
    unit: &'static str,
    aggregation: &'static str,
    values: Vec<Option<f64>>,
    value_statuses: Vec<SpineValueStatus>,
}

#[derive(Debug, Serialize, ToSchema)]
struct SpineValueStatus {
    status: &'static str,
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct SpineQuality {
    status: &'static str,
    snapshots: usize,
    gaps: Vec<SpineGap>,
    gated: Vec<&'static str>,
    resource_limited: Vec<&'static str>,
    active_tail: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
struct SpineGap {
    from_us: String,
    to_us: String,
    reason: &'static str,
}

struct MergeState {
    load: Vec<Option<f64>>,
    psi: Vec<Option<f64>>,
    snapshots: usize,
    gated: Vec<&'static str>,
    resource_limited: Vec<&'static str>,
}

impl MergeState {
    fn new(bucket_count: usize) -> Self {
        Self {
            load: vec![None; bucket_count],
            psi: vec![None; bucket_count],
            snapshots: 0,
            gated: Vec::new(),
            resource_limited: Vec::new(),
        }
    }

    fn mark_missing_block(&mut self) {
        push_unique(&mut self.gated, "load_per_cpu");
        push_unique(&mut self.gated, "psi_io_some");
    }

    fn merge_block(
        &mut self,
        block: &EntitySeriesBlock,
        request: SpineRequest,
    ) -> Result<(), SpineError> {
        if block.status() != IndexStatus::Complete
            || block.view_revision() != HOST_SIGNALS_VIEW_REVISION
            || block.identity_revision() != HOST_SIGNALS_IDENTITY_REVISION
        {
            push_unique(&mut self.resource_limited, "host_signals");
            return Ok(());
        }
        self.snapshots = self
            .snapshots
            .checked_add(
                block
                    .coverage_mask()
                    .iter()
                    .map(|byte| byte.count_ones() as usize)
                    .sum::<usize>(),
            )
            .ok_or(SpineError::Arithmetic)?;
        merge_metric(
            block,
            LOAD_PER_CPU_METRIC_CODE,
            LOAD_UNIT_CODE,
            "load_per_cpu",
            &mut self.load,
            &mut self.gated,
            &mut self.resource_limited,
            request,
        )?;
        merge_metric(
            block,
            PSI_IO_SOME_METRIC_CODE,
            PSI_UNIT_CODE,
            "psi_io_some",
            &mut self.psi,
            &mut self.gated,
            &mut self.resource_limited,
            request,
        )
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the fixed host metric contract and its merge targets stay explicit"
)]
fn merge_metric(
    block: &EntitySeriesBlock,
    metric_code: u16,
    unit_code: u16,
    wire_code: &'static str,
    output: &mut [Option<f64>],
    gated: &mut Vec<&'static str>,
    resource_limited: &mut Vec<&'static str>,
    request: SpineRequest,
) -> Result<(), SpineError> {
    let Some(metric) = block
        .metrics()
        .iter()
        .find(|metric| metric.metric_code() == metric_code)
    else {
        push_unique(gated, wire_code);
        return Ok(());
    };
    if metric.metric_revision() != 1
        || metric.unit_code() != unit_code
        || metric.aggregation() != MetricAggregation::Max
    {
        push_unique(resource_limited, wire_code);
        return Ok(());
    }
    match metric.status() {
        MetricStatus::Complete => merge_complete_metric(metric, block, output, request),
        MetricStatus::Gated | MetricStatus::UnsupportedType => {
            push_unique(gated, wire_code);
            Ok(())
        }
        MetricStatus::ResourceLimited => {
            push_unique(resource_limited, wire_code);
            Ok(())
        }
    }
}

fn merge_complete_metric(
    metric: &EntityMetric,
    block: &EntitySeriesBlock,
    output: &mut [Option<f64>],
    request: SpineRequest,
) -> Result<(), SpineError> {
    for series in metric.series() {
        let Some(dictionary) = block.dictionary().get(usize::from(series.entity_ref())) else {
            return Err(SpineError::Arithmetic);
        };
        if dictionary.label() != "host" {
            continue;
        }
        merge_values(output, series, block.grid(), request)?;
    }
    Ok(())
}

fn merge_values(
    output: &mut [Option<f64>],
    series: &kronika_reader::EntitySeries,
    grid: kronika_reader::TimeGrid,
    request: SpineRequest,
) -> Result<(), SpineError> {
    let width_us = i64::from(grid.bucket_width_s())
        .checked_mul(1_000_000)
        .ok_or(SpineError::Arithmetic)?;
    let span = request
        .to_us
        .checked_sub(request.from_us)
        .ok_or(SpineError::Arithmetic)?;
    for (bucket, value) in series.observed_values() {
        let offset = i64::try_from(bucket)
            .ok()
            .and_then(|bucket| bucket.checked_mul(width_us))
            .ok_or(SpineError::Arithmetic)?;
        let start = grid
            .start_us()
            .checked_add(offset)
            .ok_or(SpineError::Arithmetic)?;
        let end = start.checked_add(width_us).ok_or(SpineError::Arithmetic)?;
        if end <= request.from_us || start >= request.to_us {
            continue;
        }
        let anchor = start.max(request.from_us);
        let index = i128::from(anchor - request.from_us)
            .checked_mul(i128::try_from(output.len()).map_err(|_error| SpineError::Arithmetic)?)
            .and_then(|scaled| scaled.checked_div(i128::from(span)))
            .and_then(|index| usize::try_from(index).ok())
            .ok_or(SpineError::Arithmetic)?
            .min(output.len() - 1);
        output[index] = Some(output[index].map_or(value, |current| current.max(value)));
    }
    Ok(())
}

pub(crate) fn spine(
    snapshot: &LocalDirSnapshot,
    live: &LiveView,
    request: SpineRequest,
) -> Result<SpineResponse, SpineError> {
    let mut descriptors = snapshot
        .sealed_descriptors()
        .filter(|descriptor| {
            descriptor.max_ts >= request.from_us && descriptor.min_ts < request.to_us
        })
        .collect::<Vec<_>>();
    let live_chunks = if live.state() == LiveState::Current {
        live.chunks()
            .iter()
            .filter(|facts| {
                let identity = facts.identity();
                identity.source_max_ts_us >= request.from_us
                    && identity.source_min_ts_us < request.to_us
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if descriptors
        .len()
        .checked_add(live_chunks.len())
        .is_none_or(|count| count > ABSOLUTE_MAX_SELECTED_SEGMENTS)
    {
        return Err(SpineError::TooManySegments);
    }
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));

    let mut merged = MergeState::new(request.bucket_count);
    let mut coverage = descriptors
        .iter()
        .map(|descriptor| (descriptor.min_ts, descriptor.max_ts))
        .collect::<Vec<_>>();
    for descriptor in &descriptors {
        let (block, _stats) =
            snapshot.read_entity_series(descriptor, HOST_SIGNALS_VIEW_CODE, &LIMIT)?;
        match block {
            Some(block) => merged.merge_block(&block, request)?,
            None => merged.mark_missing_block(),
        }
    }
    for facts in &live_chunks {
        let identity = facts.identity();
        coverage.push((identity.source_min_ts_us, identity.source_max_ts_us));
        match facts
            .entity_series()
            .iter()
            .find(|block| block.view_code() == HOST_SIGNALS_VIEW_CODE)
        {
            Some(block) => merged.merge_block(block, request)?,
            None => merged.mark_missing_block(),
        }
    }
    let active_tail = !live_chunks.is_empty();
    if !matches!(live.state(), LiveState::Empty | LiveState::Current) {
        push_unique(&mut merged.resource_limited, "active_tail");
    }

    let gaps = coverage_gaps(&coverage, request);
    let load_statuses = value_statuses(&merged.load, &gaps, merged.gated.contains(&"load_per_cpu"));
    let psi_statuses = value_statuses(&merged.psi, &gaps, merged.gated.contains(&"psi_io_some"));
    merged.gated.sort_unstable();
    merged.resource_limited.sort_unstable();
    let partial =
        !gaps.is_empty() || !merged.gated.is_empty() || !merged.resource_limited.is_empty();
    Ok(SpineResponse {
        grid: SpineGrid {
            from_us: request.from_us.to_string(),
            to_us: request.to_us.to_string(),
            bucket_count: request.bucket_count,
        },
        series: vec![
            SpineSeries {
                code: "load_per_cpu",
                unit: "ratio",
                aggregation: "max",
                values: merged.load,
                value_statuses: load_statuses,
            },
            SpineSeries {
                code: "psi_io_some",
                unit: "percent",
                aggregation: "max",
                values: merged.psi,
                value_statuses: psi_statuses,
            },
        ],
        quality: SpineQuality {
            status: if partial { "partial" } else { "complete" },
            snapshots: merged.snapshots,
            gaps,
            gated: merged.gated,
            resource_limited: merged.resource_limited,
            active_tail,
        },
    })
}

fn value_statuses(values: &[Option<f64>], gaps: &[SpineGap], gated: bool) -> Vec<SpineValueStatus> {
    values
        .iter()
        .map(|value| {
            if value.is_some() {
                SpineValueStatus {
                    status: "available",
                    reason: None,
                }
            } else {
                SpineValueStatus {
                    status: "unavailable",
                    reason: Some(if gated {
                        "not_collected"
                    } else if gaps.is_empty() {
                        "no_sample"
                    } else {
                        "producer_gap"
                    }),
                }
            }
        })
        .collect()
}

fn coverage_gaps(spans: &[(i64, i64)], request: SpineRequest) -> Vec<SpineGap> {
    let mut gaps = Vec::new();
    let mut covered_to = request.from_us;
    let mut ordered = spans.to_vec();
    ordered.sort_unstable();
    for (minimum, maximum) in ordered {
        let start = minimum.max(request.from_us);
        let end = maximum.saturating_add(1).min(request.to_us);
        if start > covered_to {
            gaps.push(SpineGap {
                from_us: covered_to.to_string(),
                to_us: start.to_string(),
                reason: "producer_gap",
            });
        }
        covered_to = covered_to.max(end);
    }
    if covered_to < request.to_us {
        gaps.push(SpineGap {
            from_us: covered_to.to_string(),
            to_us: request.to_us.to_string(),
            reason: "producer_gap",
        });
    }
    gaps
}

fn push_unique(values: &mut Vec<&'static str>, value: &'static str) {
    if !values.contains(&value) {
        values.push(value);
    }
}
