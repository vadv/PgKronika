use std::fmt;

use kronika_analytics::web_projection::{WebAggregation, WebMetric};
use kronika_reader::{
    EntitySeriesBlock, Gap, LIMIT, LiveState, LiveView, LocalDirSnapshot, MetricStatus,
    WebIndexReadError,
};

use super::FrameRequest;
use super::projection::ProjectedFrame;
use crate::overview::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS;

const SPARK_BUCKETS: usize = 60;

#[derive(Debug)]
pub(crate) enum SparkError {
    Read(WebIndexReadError),
    TooManySegments,
    Arithmetic,
}

impl fmt::Display for SparkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(f, "{error}"),
            Self::TooManySegments => f.write_str("frame spark segment limit exceeded"),
            Self::Arithmetic => f.write_str("frame spark arithmetic overflow"),
        }
    }
}

impl From<WebIndexReadError> for SparkError {
    fn from(error: WebIndexReadError) -> Self {
        Self::Read(error)
    }
}

pub(crate) fn attach_sparks(
    snapshot: &LocalDirSnapshot,
    live: &LiveView,
    request: &FrameRequest,
    frame: &mut ProjectedFrame,
) -> Result<(), SparkError> {
    let metric = request
        .view
        .metrics
        .iter()
        .find(|metric| metric.canonical)
        .ok_or(SparkError::Arithmetic)?;
    let from_us = frame
        .snapshot_ts_us
        .checked_sub(request.span_us)
        .ok_or(SparkError::Arithmetic)?;
    let to_us = frame.snapshot_ts_us;
    for row in &mut frame.rows {
        row.spark.values = vec![None; SPARK_BUCKETS];
        row.spark.complete = true;
    }

    let mut descriptors = snapshot
        .sealed_descriptors()
        .filter(|descriptor| descriptor.max_ts >= from_us && descriptor.min_ts <= to_us)
        .collect::<Vec<_>>();
    let live_chunks = if live.state() == LiveState::Current {
        live.chunks()
            .iter()
            .filter(|facts| {
                let identity = facts.identity();
                identity.source_max_ts_us >= from_us && identity.source_min_ts_us <= to_us
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
        return Err(SparkError::TooManySegments);
    }
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));
    let mut coverage = descriptors
        .iter()
        .map(|descriptor| (descriptor.min_ts, descriptor.max_ts))
        .collect::<Vec<_>>();
    let mut blocks = 0_usize;
    for descriptor in &descriptors {
        let (block, _stats) = snapshot.read_entity_series(descriptor, request.view.code, &LIMIT)?;
        let Some(block) = block else {
            mark_incomplete(frame);
            continue;
        };
        merge_block(&block, request, metric, frame, from_us, to_us)?;
        blocks = blocks.checked_add(1).ok_or(SparkError::Arithmetic)?;
    }
    for facts in live_chunks {
        let identity = facts.identity();
        coverage.push((identity.source_min_ts_us, identity.source_max_ts_us));
        let Some(block) = facts
            .entity_series()
            .iter()
            .find(|block| block.view_code() == request.view.code)
        else {
            mark_incomplete(frame);
            continue;
        };
        merge_block(block, request, metric, frame, from_us, to_us)?;
        blocks = blocks.checked_add(1).ok_or(SparkError::Arithmetic)?;
    }
    if blocks == 0 {
        mark_incomplete(frame);
    }
    let gaps = coverage_gaps(&coverage, from_us, to_us);
    if !gaps.is_empty() {
        mark_incomplete(frame);
        frame.quality.gaps.extend(gaps);
        frame.quality.gaps.sort_by_key(|gap| (gap.from, gap.to));
        frame.quality.gaps.dedup();
    }
    frame.quality.active_tail =
        live.state() == LiveState::Current && coverage.len() > descriptors.len();
    if !matches!(live.state(), LiveState::Empty | LiveState::Current) {
        mark_incomplete(frame);
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one selected view block is validated and merged in a single bounded pass"
)]
fn merge_block(
    block: &EntitySeriesBlock,
    request: &FrameRequest,
    metric: &WebMetric,
    frame: &mut ProjectedFrame,
    from_us: i64,
    to_us: i64,
) -> Result<(), SparkError> {
    if block.view_revision() != request.view.revision
        || block.identity_revision() != request.view.identity_revision
    {
        frame
            .quality
            .unavailable_revision
            .push(request.view.name.to_owned());
        mark_incomplete(frame);
        return Ok(());
    }
    if block.status() != kronika_reader::IndexStatus::Complete {
        frame
            .quality
            .resource_limited
            .push(request.view.name.to_owned());
        mark_incomplete(frame);
        return Ok(());
    }
    let Some(series_metric) = block
        .metrics()
        .iter()
        .find(|candidate| candidate.metric_code() == metric.code)
    else {
        frame
            .quality
            .unavailable_revision
            .push(metric.name.to_owned());
        mark_incomplete(frame);
        return Ok(());
    };
    if series_metric.metric_revision() != metric.revision
        || series_metric.unit_code() != metric.unit.code()
    {
        frame
            .quality
            .unavailable_revision
            .push(metric.name.to_owned());
        mark_incomplete(frame);
        return Ok(());
    }
    match series_metric.status() {
        MetricStatus::Complete => {}
        MetricStatus::Gated => frame.quality.gated.push(metric.name.to_owned()),
        MetricStatus::UnsupportedType => frame
            .quality
            .unavailable_revision
            .push(metric.name.to_owned()),
        MetricStatus::ResourceLimited => {
            frame.quality.resource_limited.push(metric.name.to_owned());
        }
    }
    if series_metric.status() != MetricStatus::Complete {
        mark_incomplete(frame);
        return Ok(());
    }

    for row in &mut frame.rows {
        let Some((entity_ref, _dictionary)) = block
            .dictionary()
            .iter()
            .enumerate()
            .find(|(_index, dictionary)| dictionary.key() == row.spark_entity)
        else {
            row.spark.complete = false;
            continue;
        };
        let Ok(entity_ref) = u16::try_from(entity_ref) else {
            return Err(SparkError::Arithmetic);
        };
        let Some(series) = series_metric
            .series()
            .iter()
            .find(|series| series.entity_ref() == entity_ref)
        else {
            row.spark.complete = false;
            continue;
        };
        let grid = block.grid();
        let width_us = i64::from(grid.bucket_width_s())
            .checked_mul(1_000_000)
            .ok_or(SparkError::Arithmetic)?;
        for (source_bucket, value) in series.observed_values() {
            let source_bucket =
                i64::try_from(source_bucket).map_err(|_error| SparkError::Arithmetic)?;
            let bucket_start = width_us
                .checked_mul(source_bucket)
                .and_then(|offset| grid.start_us().checked_add(offset))
                .ok_or(SparkError::Arithmetic)?;
            let bucket_end = bucket_start
                .checked_add(width_us.saturating_sub(1))
                .ok_or(SparkError::Arithmetic)?;
            if bucket_end < from_us || bucket_start > to_us {
                continue;
            }
            let timestamp = bucket_start.max(from_us).min(to_us);
            let Some(target) = target_bucket(timestamp, from_us, to_us) else {
                continue;
            };
            let slot = row
                .spark
                .values
                .get_mut(target)
                .ok_or(SparkError::Arithmetic)?;
            *slot = Some(match (*slot, metric.aggregation) {
                (Some(current), WebAggregation::Sum) => current + value,
                (Some(current), WebAggregation::Max) => current.max(value),
                (None, _) => value,
            });
        }
    }
    Ok(())
}

fn target_bucket(timestamp: i64, from_us: i64, to_us: i64) -> Option<usize> {
    if timestamp < from_us || timestamp > to_us {
        return None;
    }
    let span = to_us.checked_sub(from_us)?.checked_add(1)?;
    let offset = timestamp.checked_sub(from_us)?;
    let index = offset
        .checked_mul(i64::try_from(SPARK_BUCKETS).ok()?)?
        .checked_div(span)?;
    usize::try_from(index)
        .ok()
        .map(|index| index.min(SPARK_BUCKETS - 1))
}

fn mark_incomplete(frame: &mut ProjectedFrame) {
    for row in &mut frame.rows {
        row.spark.complete = false;
    }
}

fn coverage_gaps(coverage: &[(i64, i64)], from_us: i64, to_us: i64) -> Vec<Gap> {
    let mut ranges = coverage
        .iter()
        .map(|(from, to)| ((*from).max(from_us), (*to).min(to_us)))
        .filter(|(from, to)| from <= to)
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut gaps = Vec::new();
    let mut cursor = from_us;
    for (from, to) in ranges {
        if from > cursor {
            gaps.push(Gap {
                from: cursor,
                to: from.saturating_sub(1),
            });
        }
        cursor = cursor.max(to.saturating_add(1));
    }
    if cursor <= to_us {
        gaps.push(Gap {
            from: cursor,
            to: to_us,
        });
    }
    gaps
}
