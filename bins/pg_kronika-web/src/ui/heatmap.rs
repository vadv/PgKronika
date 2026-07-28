//! Bounded range merge of view-addressed web-index entity series.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::web_projection::{WebAggregation, WebMetric, WebView};
use kronika_reader::{
    EntitySeriesBlock, LIMIT, LiveState, LiveView, LocalDirSnapshot, MetricStatus,
    SegmentDescriptor, SegmentFacts, WebIndexReadError,
};
use serde::Serialize;

use crate::overview::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS;

const MAX_HEATMAP_CANDIDATES: usize = 16_384;

#[derive(Debug)]
pub(crate) enum HeatmapError {
    Read(WebIndexReadError),
    TooManySegments,
    TooManyCandidates,
    Arithmetic,
}

impl std::fmt::Display for HeatmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(f, "{error}"),
            Self::TooManySegments => f.write_str("heatmap segment limit exceeded"),
            Self::TooManyCandidates => f.write_str("heatmap candidate limit exceeded"),
            Self::Arithmetic => f.write_str("heatmap arithmetic overflow"),
        }
    }
}

impl std::error::Error for HeatmapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::TooManySegments | Self::TooManyCandidates | Self::Arithmetic => None,
        }
    }
}

impl From<WebIndexReadError> for HeatmapError {
    fn from(error: WebIndexReadError) -> Self {
        Self::Read(error)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct HeatmapResponse {
    grid: HeatmapGrid,
    ranking: Ranking,
    rows: Vec<HeatmapRow>,
    quality: HeatmapQuality,
}

#[derive(Debug, Serialize)]
struct HeatmapGrid {
    from_us: String,
    to_us: String,
    bucket_count: usize,
}

#[derive(Debug, Serialize)]
struct Ranking {
    exact: bool,
    unseen_upper: f64,
}

#[derive(Debug, Serialize)]
struct HeatmapRow {
    entity: String,
    label: String,
    unit: &'static str,
    score: ScoreBounds,
    values: Vec<Option<f64>>,
}

#[derive(Debug, Serialize)]
struct ScoreBounds {
    lower: f64,
    upper: f64,
}

#[derive(Debug, Serialize)]
struct HeatmapQuality {
    status: &'static str,
    snapshots: usize,
    gaps: Vec<RangeGap>,
    gated: Vec<String>,
    unavailable_revision: Vec<String>,
    resource_limited: Vec<String>,
    unbounded_segments: Vec<String>,
    active_tail: bool,
}

#[derive(Debug, Serialize)]
struct RangeGap {
    from_us: String,
    to_us: String,
}

struct Candidate {
    label: String,
    lower: f64,
    upper: f64,
    values: BucketValues,
}

struct BucketValues {
    values: Vec<f64>,
    presence: Vec<u8>,
}

impl BucketValues {
    fn new(bucket_count: usize) -> Self {
        Self {
            values: vec![0.0; bucket_count],
            presence: vec![0; bucket_count.div_ceil(8)],
        }
    }

    fn get(&self, index: usize) -> Option<f64> {
        (self.presence[index / 8] & (1 << (index % 8)) != 0).then_some(self.values[index])
    }

    fn set(&mut self, index: usize, value: f64) {
        self.presence[index / 8] |= 1 << (index % 8);
        self.values[index] = value;
    }

    fn into_optional(self) -> Vec<Option<f64>> {
        self.values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                (self.presence[index / 8] & (1 << (index % 8)) != 0).then_some(value)
            })
            .collect()
    }
}

struct BlockProof {
    cutoff: f64,
    retained: BTreeSet<Vec<u8>>,
}

#[derive(Default)]
struct MergeState {
    candidates: BTreeMap<Vec<u8>, Candidate>,
    proofs: Vec<BlockProof>,
    unbounded_segments: Vec<String>,
    gated: Vec<String>,
    unavailable_revision: Vec<String>,
    resource_limited: Vec<String>,
    snapshots: usize,
}

impl MergeState {
    fn mark_unbounded(&mut self, token: String) {
        self.unbounded_segments.push(token);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one block is validated and merged in a single bounded pass"
    )]
    fn merge_block(
        &mut self,
        block: &EntitySeriesBlock,
        segment_range: (i64, i64),
        token: String,
        request: HeatmapRequest,
    ) -> Result<(), HeatmapError> {
        if block.status() != kronika_reader::IndexStatus::Complete {
            self.resource_limited.push(token.clone());
            self.mark_unbounded(token);
            return Ok(());
        }
        if block.view_revision() != request.view.revision
            || block.identity_revision() != request.view.identity_revision
        {
            self.unavailable_revision.push(token.clone());
            self.mark_unbounded(token);
            return Ok(());
        }
        let Some(block_metric) = block
            .metrics()
            .iter()
            .find(|candidate| candidate.metric_code() == request.metric.code)
        else {
            self.unavailable_revision.push(token.clone());
            self.mark_unbounded(token);
            return Ok(());
        };
        if block_metric.metric_revision() != request.metric.revision {
            self.unavailable_revision.push(token.clone());
            self.mark_unbounded(token);
            return Ok(());
        }
        let expected_aggregation = match request.metric.aggregation {
            WebAggregation::Sum => kronika_reader::MetricAggregation::Sum,
            WebAggregation::Max => kronika_reader::MetricAggregation::Max,
        };
        if block_metric.unit_code() != request.metric.unit.code()
            || block_metric.aggregation() != expected_aggregation
        {
            self.unavailable_revision.push(token.clone());
            self.mark_unbounded(token);
            return Ok(());
        }
        match block_metric.status() {
            MetricStatus::Complete => {}
            MetricStatus::Gated => self.gated.push(token.clone()),
            MetricStatus::UnsupportedType => self.unavailable_revision.push(token.clone()),
            MetricStatus::ResourceLimited => self.resource_limited.push(token.clone()),
        }
        if block_metric.status() != MetricStatus::Complete {
            self.mark_unbounded(token);
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
            .ok_or(HeatmapError::Arithmetic)?;
        let contained = segment_range.0 >= request.from_us && segment_range.1 < request.to_us;
        let mut retained = BTreeSet::new();
        for series in block_metric.series() {
            let dictionary = block
                .dictionary()
                .get(usize::from(series.entity_ref()))
                .ok_or(HeatmapError::Arithmetic)?;
            let key = dictionary.key().to_vec();
            retained.insert(key.clone());
            if !self.candidates.contains_key(&key)
                && self.candidates.len() == MAX_HEATMAP_CANDIDATES
            {
                return Err(HeatmapError::TooManyCandidates);
            }
            let candidate = self.candidates.entry(key).or_insert_with(|| Candidate {
                label: dictionary.label().to_owned(),
                lower: 0.0,
                upper: 0.0,
                values: BucketValues::new(request.bucket_count),
            });
            dictionary.label().clone_into(&mut candidate.label);
            if contained {
                candidate.lower = aggregate_score(
                    candidate.lower,
                    series.exact_score(),
                    request.metric.aggregation,
                )?;
            }
            candidate.upper = aggregate_score(
                candidate.upper,
                series.exact_score(),
                request.metric.aggregation,
            )?;
            merge_values(
                &mut candidate.values,
                series,
                block.grid(),
                request.from_us,
                request.to_us,
                request.metric.aggregation,
            )?;
        }
        self.proofs.push(BlockProof {
            cutoff: block_metric.cutoff_score(),
            retained,
        });
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct HeatmapRequest {
    pub(crate) source: u64,
    pub(crate) view: &'static WebView,
    pub(crate) metric: &'static WebMetric,
    pub(crate) from_us: i64,
    pub(crate) to_us: i64,
    pub(crate) bucket_count: usize,
    pub(crate) top: usize,
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded one-pass merge keeps score proofs and bucket accumulation together"
)]
pub(crate) fn heatmap(
    snapshot: &LocalDirSnapshot,
    live: &LiveView,
    request: HeatmapRequest,
) -> Result<Option<HeatmapResponse>, HeatmapError> {
    let HeatmapRequest {
        source,
        view,
        metric,
        from_us,
        to_us,
        bucket_count,
        top,
    } = request;
    let sealed_source_found = snapshot
        .sealed_descriptors()
        .any(|descriptor| descriptor.source_id == source);
    let live_source = live.source_id() == Some(source);
    if !sealed_source_found && !live_source {
        return Ok(None);
    }
    let mut descriptors = snapshot
        .sealed_descriptors()
        .filter(|descriptor| {
            descriptor.source_id == source
                && descriptor.max_ts >= from_us
                && descriptor.min_ts < to_us
        })
        .collect::<Vec<_>>();
    let live_chunks = if live_source && live.state() == LiveState::Current {
        live.chunks()
            .iter()
            .filter(|facts| {
                let identity = facts.identity();
                identity.pgm_source_id == source
                    && identity.source_max_ts_us >= from_us
                    && identity.source_min_ts_us < to_us
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
        return Err(HeatmapError::TooManySegments);
    }
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));

    let mut merged = MergeState::default();
    let mut coverage_spans = descriptors
        .iter()
        .map(|descriptor| (descriptor.min_ts, descriptor.max_ts))
        .collect::<Vec<_>>();
    for descriptor in &descriptors {
        let (block, _stats) = snapshot.read_entity_series(descriptor, view.code, &LIMIT)?;
        let Some(block) = block else {
            merged.mark_unbounded(segment_token(descriptor));
            continue;
        };
        merged.merge_block(
            &block,
            (descriptor.min_ts, descriptor.max_ts),
            segment_token(descriptor),
            request,
        )?;
    }
    for facts in live_chunks {
        let identity = facts.identity();
        coverage_spans.push((identity.source_min_ts_us, identity.source_max_ts_us));
        let token = live_segment_token(facts);
        let Some(block) = facts
            .entity_series()
            .iter()
            .find(|block| block.view_code() == view.code)
        else {
            merged.mark_unbounded(token);
            continue;
        };
        merged.merge_block(
            block,
            (identity.source_min_ts_us, identity.source_max_ts_us),
            token,
            request,
        )?;
    }
    let active_tail = !coverage_spans.is_empty()
        && live_source
        && live.state() == LiveState::Current
        && coverage_spans.len() > descriptors.len();
    if live_source && !matches!(live.state(), LiveState::Empty | LiveState::Current) {
        merged.mark_unbounded("active_tail".to_owned());
    }

    for (key, candidate) in &mut merged.candidates {
        for proof in &merged.proofs {
            if !proof.retained.contains(key) {
                candidate.upper =
                    aggregate_score(candidate.upper, proof.cutoff, metric.aggregation)?;
            }
        }
    }
    let unseen_upper = merged.proofs.iter().try_fold(0.0, |upper, proof| {
        aggregate_score(upper, proof.cutoff, metric.aggregation)
    })?;
    let mut ranked = merged.candidates.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_key, left), (right_key, right)| {
        right
            .lower
            .total_cmp(&left.lower)
            .then_with(|| left_key.cmp(right_key))
    });
    let gaps = coverage_gaps(&coverage_spans, from_us, to_us);
    let exact = ranking_is_exact(
        &ranked,
        top,
        unseen_upper,
        merged.unbounded_segments.is_empty() && gaps.is_empty(),
    );
    ranked.truncate(top);
    let rows = ranked
        .into_iter()
        .map(|(key, candidate)| HeatmapRow {
            entity: URL_SAFE_NO_PAD.encode(key),
            label: candidate.label,
            unit: metric.unit.as_str(),
            score: ScoreBounds {
                lower: candidate.lower,
                upper: candidate.upper,
            },
            values: candidate.values.into_optional(),
        })
        .collect();
    let partial = !gaps.is_empty()
        || !merged.gated.is_empty()
        || !merged.unavailable_revision.is_empty()
        || !merged.resource_limited.is_empty()
        || !merged.unbounded_segments.is_empty();

    Ok(Some(HeatmapResponse {
        grid: HeatmapGrid {
            from_us: from_us.to_string(),
            to_us: to_us.to_string(),
            bucket_count,
        },
        ranking: Ranking {
            exact,
            unseen_upper,
        },
        rows,
        quality: HeatmapQuality {
            status: if partial { "partial" } else { "complete" },
            snapshots: merged.snapshots,
            gaps,
            gated: merged.gated,
            unavailable_revision: merged.unavailable_revision,
            resource_limited: merged.resource_limited,
            unbounded_segments: merged.unbounded_segments,
            active_tail,
        },
    }))
}

fn merge_values(
    output: &mut BucketValues,
    series: &kronika_reader::EntitySeries,
    grid: kronika_reader::TimeGrid,
    from_us: i64,
    to_us: i64,
    aggregation: WebAggregation,
) -> Result<(), HeatmapError> {
    let width_us = i64::from(grid.bucket_width_s())
        .checked_mul(1_000_000)
        .ok_or(HeatmapError::Arithmetic)?;
    let span = to_us.checked_sub(from_us).ok_or(HeatmapError::Arithmetic)?;
    for (bucket, value) in series.observed_values() {
        let bucket_offset = i64::try_from(bucket)
            .map_err(|_error| HeatmapError::Arithmetic)?
            .checked_mul(width_us)
            .ok_or(HeatmapError::Arithmetic)?;
        let start = grid
            .start_us()
            .checked_add(bucket_offset)
            .ok_or(HeatmapError::Arithmetic)?;
        let end = start
            .checked_add(width_us)
            .ok_or(HeatmapError::Arithmetic)?;
        if end <= from_us || start >= to_us {
            continue;
        }
        let anchor = start.max(from_us);
        let offset = i128::from(anchor - from_us);
        let index = offset
            .checked_mul(
                i128::try_from(output.values.len()).map_err(|_error| HeatmapError::Arithmetic)?,
            )
            .and_then(|scaled| scaled.checked_div(i128::from(span)))
            .and_then(|index| usize::try_from(index).ok())
            .ok_or(HeatmapError::Arithmetic)?
            .min(output.values.len() - 1);
        let merged = match (output.get(index), aggregation) {
            (Some(current), WebAggregation::Sum) => finite(current + value)?,
            (Some(current), WebAggregation::Max) => current.max(value),
            (None, _) => value,
        };
        output.set(index, merged);
    }
    Ok(())
}

fn aggregate_score(
    current: f64,
    value: f64,
    aggregation: WebAggregation,
) -> Result<f64, HeatmapError> {
    finite(match aggregation {
        WebAggregation::Sum => current + value,
        WebAggregation::Max => current.max(value),
    })
}

fn finite(value: f64) -> Result<f64, HeatmapError> {
    (value.is_finite() && value >= 0.0)
        .then_some(value)
        .ok_or(HeatmapError::Arithmetic)
}

fn ranking_is_exact(
    ranked: &[(Vec<u8>, Candidate)],
    top: usize,
    unseen_upper: f64,
    bounded: bool,
) -> bool {
    if !bounded {
        return false;
    }
    if ranked.is_empty() {
        return unseen_upper == 0.0;
    }
    let returned = top.min(ranked.len());
    (0..returned).all(|index| {
        let later_upper = ranked[index + 1..]
            .iter()
            .map(|(_key, candidate)| candidate.upper)
            .fold(unseen_upper, f64::max);
        (ranked[index + 1..].is_empty() && unseen_upper == 0.0)
            || ranked[index].1.lower > later_upper
    })
}

fn coverage_gaps(spans: &[(i64, i64)], from_us: i64, to_us: i64) -> Vec<RangeGap> {
    let mut gaps = Vec::new();
    let mut covered_to = from_us;
    let mut ordered = spans.to_vec();
    ordered.sort_unstable();
    for (minimum, maximum) in ordered {
        let start = minimum.max(from_us);
        let end = maximum.saturating_add(1).min(to_us);
        if start > covered_to {
            gaps.push(RangeGap {
                from_us: covered_to.to_string(),
                to_us: start.to_string(),
            });
        }
        covered_to = covered_to.max(end);
    }
    if covered_to < to_us {
        gaps.push(RangeGap {
            from_us: covered_to.to_string(),
            to_us: to_us.to_string(),
        });
    }
    gaps
}

fn segment_token(descriptor: &SegmentDescriptor) -> String {
    URL_SAFE_NO_PAD.encode(descriptor.locator.as_bytes())
}

fn live_segment_token(facts: &SegmentFacts) -> String {
    URL_SAFE_NO_PAD.encode(facts.lineage().id().0)
}

#[cfg(test)]
mod tests {
    use super::{BucketValues, Candidate, coverage_gaps, ranking_is_exact};

    fn candidate(lower: f64, upper: f64) -> Candidate {
        Candidate {
            label: String::new(),
            lower,
            upper,
            values: BucketValues::new(1),
        }
    }

    #[test]
    fn empty_retained_set_is_exact_only_without_an_unseen_cutoff() {
        assert!(ranking_is_exact(&[], 8, 0.0, true));
        assert!(!ranking_is_exact(&[], 8, 1.0, true));
        assert!(!ranking_is_exact(&[], 8, 0.0, false));
    }

    #[test]
    fn ranking_requires_each_returned_lower_bound_to_beat_later_uppers() {
        let proven = vec![
            (vec![1], candidate(100.0, 101.0)),
            (vec![2], candidate(90.0, 91.0)),
        ];
        assert!(ranking_is_exact(&proven, 2, 1.0, true));

        let overlapping = vec![
            (vec![1], candidate(100.0, 110.0)),
            (vec![2], candidate(99.0, 101.0)),
        ];
        assert!(!ranking_is_exact(&overlapping, 2, 1.0, true));
    }

    #[test]
    fn coverage_gaps_merge_overlapping_out_of_order_segments() {
        let gaps = coverage_gaps(&[(20, 39), (0, 29), (50, 59)], 0, 60);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].from_us, "40");
        assert_eq!(gaps[0].to_us, "50");
    }
}
