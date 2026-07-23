//! Bounded counter and gauge reductions.
//!
//! Counter pairs are valid only within one series, reset epoch, and continuous
//! interval. A broken pair contributes no synthetic zero. Gauge reductions
//! retain their bounded canonical sample set so merges recompute the same sum
//! regardless of partitioning.

use super::coverage::{BoundaryQuality, Coverage, CoverageSpan};
use super::finite::FiniteF64;

/// Stable identity of one metric series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricSeriesId(pub [u8; 16]);

/// Identity of the entity/snapshot alignment shared by ratio inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AlignmentId(pub [u8; 16]);

/// Allocation bounds for a reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the max_ prefix distinguishes hard caps in the public limits API"
)]
pub struct ReductionLimits {
    /// Maximum input records scanned by one operation.
    pub max_input_items: usize,
    /// Maximum known-gap spans scanned by one operation.
    pub max_gap_spans: usize,
    /// Maximum retained valid counter pairs.
    pub max_counter_pairs: usize,
    /// Maximum retained gauge samples.
    pub max_gauge_samples: usize,
}

/// Failure to build or combine a reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionError {
    /// A configured sample or pair bound was exceeded.
    LimitExceeded,
    /// Checked integer accumulation overflowed.
    CountOverflow,
    /// Records from different series were combined.
    MixedSeries,
    /// Two reductions contain overlapping evidence intervals.
    OverlappingSupport,
    /// Alignment, pair boundaries, or boundary attribution differ.
    MisalignedRatio,
    /// Two gauge samples use the same timestamp.
    DuplicateTimestamp,
    /// Input samples are not in strictly increasing time order.
    NonMonotonicSamples,
    /// Floating-point accumulation produced a non-finite result.
    NonFiniteResult,
}

/// One cumulative counter sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterSample {
    series_id: MetricSeriesId,
    alignment_id: AlignmentId,
    ts_us: i64,
    value: u64,
    reset_epoch: u64,
}

impl CounterSample {
    /// Builds a cumulative counter sample.
    #[must_use]
    pub const fn new(
        series_id: MetricSeriesId,
        alignment_id: AlignmentId,
        ts_us: i64,
        value: u64,
        reset_epoch: u64,
    ) -> Self {
        Self {
            series_id,
            alignment_id,
            ts_us,
            value,
            reset_epoch,
        }
    }

    /// Series identity.
    #[must_use]
    pub const fn series_id(self) -> MetricSeriesId {
        self.series_id
    }

    /// Entity/snapshot alignment identity.
    #[must_use]
    pub const fn alignment_id(self) -> AlignmentId {
        self.alignment_id
    }

    /// Sample timestamp.
    #[must_use]
    pub const fn ts_us(self) -> i64 {
        self.ts_us
    }

    /// Cumulative value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }

    /// Reset epoch.
    #[must_use]
    pub const fn reset_epoch(self) -> u64 {
        self.reset_epoch
    }
}

/// Classification of a candidate counter pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairQuality {
    /// Comparable samples.
    Valid,
    /// Reset epoch changed or the cumulative value decreased.
    Reset,
    /// A known collection gap intersects the pair.
    Gap,
    /// The current timestamp does not advance.
    NonMonotonicTime,
    /// Samples belong to different series.
    DifferentSeries,
    /// Samples use different entity/snapshot alignment.
    DifferentAlignment,
    /// No predecessor is available.
    Missing,
}

/// One classified candidate interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterInterval {
    previous: Option<CounterSample>,
    current: CounterSample,
    quality: PairQuality,
}

impl CounterInterval {
    /// Classifies one pair against normalized known gaps.
    #[must_use]
    pub fn classify(
        previous: Option<CounterSample>,
        current: CounterSample,
        known_gaps: &Coverage,
    ) -> Self {
        let has_gap = previous.is_some_and(|previous| {
            known_gaps
                .spans()
                .iter()
                .any(|gap| gap.start_us() < current.ts_us && gap.end_us() > previous.ts_us)
        });
        Self::classify_with_gap(previous, current, has_gap)
    }

    fn classify_with_gap(
        previous: Option<CounterSample>,
        current: CounterSample,
        has_gap: bool,
    ) -> Self {
        let quality = match previous {
            None => PairQuality::Missing,
            Some(previous) if previous.series_id != current.series_id => {
                PairQuality::DifferentSeries
            }
            Some(previous) if previous.alignment_id != current.alignment_id => {
                PairQuality::DifferentAlignment
            }
            Some(previous) if current.ts_us <= previous.ts_us => PairQuality::NonMonotonicTime,
            Some(previous)
                if previous.reset_epoch != current.reset_epoch
                    || current.value < previous.value =>
            {
                PairQuality::Reset
            }
            Some(_) if has_gap => PairQuality::Gap,
            Some(_) => PairQuality::Valid,
        };
        Self {
            previous,
            current,
            quality,
        }
    }

    /// Pair classification.
    #[must_use]
    pub const fn quality(self) -> PairQuality {
        self.quality
    }

    /// Current sample.
    #[must_use]
    pub const fn current(self) -> CounterSample {
        self.current
    }

    /// Predecessor, when present.
    #[must_use]
    pub const fn previous(self) -> Option<CounterSample> {
        self.previous
    }

    /// Non-negative delta of a valid pair.
    #[must_use]
    pub fn delta(self) -> Option<u64> {
        (self.quality == PairQuality::Valid)
            .then(|| self.current.value.checked_sub(self.previous?.value))?
    }

    /// Positive duration of a valid pair.
    #[must_use]
    pub fn duration_us(self) -> Option<u64> {
        if self.quality != PairQuality::Valid {
            return None;
        }
        let previous = self.previous?;
        Some(
            self.current
                .ts_us
                .wrapping_sub(previous.ts_us)
                .cast_unsigned(),
        )
    }
}

/// Classifies one ordered series in `O(samples + gaps)` work.
///
/// # Errors
/// Returns [`ReductionError`] for a configured bound, mixed series or
/// alignments, or non-monotonic input.
pub fn classify_series(
    halo_previous: Option<CounterSample>,
    samples: &[CounterSample],
    known_gaps: &Coverage,
    limits: ReductionLimits,
) -> Result<Vec<CounterInterval>, ReductionError> {
    if samples.len() > limits.max_input_items || known_gaps.spans().len() > limits.max_gap_spans {
        return Err(ReductionError::LimitExceeded);
    }
    if samples.len() > limits.max_counter_pairs {
        return Err(ReductionError::LimitExceeded);
    }
    let mut previous = halo_previous;
    let mut gap_index = 0;
    let mut out = Vec::with_capacity(samples.len());
    for &sample in samples {
        if let Some(previous) = previous {
            if sample.series_id != previous.series_id {
                return Err(ReductionError::MixedSeries);
            }
            if sample.alignment_id != previous.alignment_id {
                return Err(ReductionError::MisalignedRatio);
            }
            if sample.ts_us <= previous.ts_us {
                return Err(ReductionError::NonMonotonicSamples);
            }
        }
        let has_gap = next_pair_has_gap(previous, sample, known_gaps, &mut gap_index);
        out.push(CounterInterval::classify_with_gap(
            previous, sample, has_gap,
        ));
        previous = Some(sample);
    }
    Ok(out)
}

fn next_pair_has_gap(
    previous: Option<CounterSample>,
    current: CounterSample,
    known_gaps: &Coverage,
    gap_index: &mut usize,
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    while known_gaps
        .spans()
        .get(*gap_index)
        .is_some_and(|gap| gap.end_us() <= previous.ts_us)
    {
        *gap_index += 1;
    }
    known_gaps
        .spans()
        .get(*gap_index)
        .is_some_and(|gap| gap.start_us() < current.ts_us && gap.end_us() > previous.ts_us)
}

/// Exact aggregate of valid counter pairs for one series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterReduction {
    series_id: MetricSeriesId,
    alignment_id: AlignmentId,
    sum_delta: u64,
    sum_duration_us: u64,
    pair_support: Vec<(i64, i64)>,
    boundary_quality: BoundaryQuality,
}

impl CounterReduction {
    /// Aggregates valid pairs owned by the bucket of their current sample.
    ///
    /// # Errors
    /// Returns [`ReductionError`] for mixed series, overlapping evidence,
    /// overflow, or a configured bound.
    pub fn from_intervals(
        intervals: &[CounterInterval],
        bucket: CoverageSpan,
        limits: ReductionLimits,
    ) -> Result<Option<Self>, ReductionError> {
        if intervals.len() > limits.max_input_items {
            return Err(ReductionError::LimitExceeded);
        }
        let mut selected: Vec<(CounterSample, CounterSample)> =
            Vec::with_capacity(intervals.len().min(limits.max_counter_pairs));
        for interval in intervals {
            if interval.quality != PairQuality::Valid
                || interval.current.ts_us < bucket.start_us()
                || interval.current.ts_us >= bucket.end_us()
            {
                continue;
            }
            let Some(previous) = interval.previous else {
                continue;
            };
            if selected.len() == limits.max_counter_pairs {
                return Err(ReductionError::LimitExceeded);
            }
            selected.push((previous, interval.current));
        }
        if selected.is_empty() {
            return Ok(None);
        }
        selected.sort_unstable_by_key(|&(previous, current)| (current.ts_us, previous.ts_us));
        let series_id = selected[0].1.series_id;
        let alignment_id = selected[0].1.alignment_id;
        if selected.iter().any(|&(previous, current)| {
            previous.series_id != series_id || current.series_id != series_id
        }) {
            return Err(ReductionError::MixedSeries);
        }
        if selected.iter().any(|&(previous, current)| {
            previous.alignment_id != alignment_id || current.alignment_id != alignment_id
        }) {
            return Err(ReductionError::MisalignedRatio);
        }
        validate_pair_support(&selected)?;

        let mut sum_delta = 0_u64;
        let mut sum_duration_us = 0_u64;
        let mut has_contained_support = false;
        let mut has_cross_boundary_support = false;
        let mut pair_support = Vec::with_capacity(selected.len());
        for (previous, current) in selected {
            sum_delta = sum_delta
                .checked_add(current.value - previous.value)
                .ok_or(ReductionError::CountOverflow)?;
            sum_duration_us = sum_duration_us
                .checked_add(current.ts_us.wrapping_sub(previous.ts_us).cast_unsigned())
                .ok_or(ReductionError::CountOverflow)?;
            if previous.ts_us < bucket.start_us() {
                has_cross_boundary_support = true;
            } else {
                has_contained_support = true;
            }
            pair_support.push((previous.ts_us, current.ts_us));
        }
        let boundary_quality = match (has_contained_support, has_cross_boundary_support) {
            (true, true) => BoundaryQuality::Mixed,
            (false, true) => BoundaryQuality::EndpointAttributedCrossBoundary,
            _ => BoundaryQuality::Contained,
        };
        Ok(Some(Self {
            series_id,
            alignment_id,
            sum_delta,
            sum_duration_us,
            pair_support,
            boundary_quality,
        }))
    }

    /// Combines disjoint reductions of the same series.
    ///
    /// # Errors
    /// Returns [`ReductionError`] for mixed series, overlapping support,
    /// overflow, or a configured bound.
    pub fn merge(&self, other: &Self, limits: ReductionLimits) -> Result<Self, ReductionError> {
        if self.series_id != other.series_id {
            return Err(ReductionError::MixedSeries);
        }
        if self.alignment_id != other.alignment_id {
            return Err(ReductionError::MisalignedRatio);
        }
        let combined_len = self
            .pair_support
            .len()
            .checked_add(other.pair_support.len())
            .ok_or(ReductionError::LimitExceeded)?;
        if combined_len > limits.max_input_items || combined_len > limits.max_counter_pairs {
            return Err(ReductionError::LimitExceeded);
        }
        let mut pair_support = self.pair_support.clone();
        pair_support.extend_from_slice(&other.pair_support);
        pair_support.sort_unstable_by_key(|&(previous, current)| (current, previous));
        validate_support_ranges(&pair_support)?;
        Ok(Self {
            series_id: self.series_id,
            alignment_id: self.alignment_id,
            sum_delta: self
                .sum_delta
                .checked_add(other.sum_delta)
                .ok_or(ReductionError::CountOverflow)?,
            sum_duration_us: self
                .sum_duration_us
                .checked_add(other.sum_duration_us)
                .ok_or(ReductionError::CountOverflow)?,
            pair_support,
            boundary_quality: if self.boundary_quality == other.boundary_quality {
                self.boundary_quality
            } else {
                BoundaryQuality::Mixed
            },
        })
    }

    /// Series identity.
    #[must_use]
    pub const fn series_id(&self) -> MetricSeriesId {
        self.series_id
    }

    /// Entity/snapshot alignment identity.
    #[must_use]
    pub const fn alignment_id(&self) -> AlignmentId {
        self.alignment_id
    }

    /// Total increment.
    #[must_use]
    pub const fn sum_delta(&self) -> u64 {
        self.sum_delta
    }

    /// Total pair duration.
    #[must_use]
    pub const fn sum_duration_us(&self) -> u64 {
        self.sum_duration_us
    }

    /// Number of valid pairs.
    #[must_use]
    pub const fn valid_pairs(&self) -> usize {
        self.pair_support.len()
    }

    /// Exact pair boundaries used by the reduction.
    #[must_use]
    pub fn pair_support(&self) -> &[(i64, i64)] {
        &self.pair_support
    }

    /// Boundary attribution quality.
    #[must_use]
    pub const fn boundary_quality(&self) -> BoundaryQuality {
        self.boundary_quality
    }

    /// Approximate rate per microsecond.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "rates are floating-point projections of exact sums"
    )]
    pub fn rate_per_us(&self) -> f64 {
        self.sum_delta as f64 / self.sum_duration_us as f64
    }
}

fn validate_pair_support(pairs: &[(CounterSample, CounterSample)]) -> Result<(), ReductionError> {
    let ranges: Vec<(i64, i64)> = pairs
        .iter()
        .map(|&(previous, current)| (previous.ts_us, current.ts_us))
        .collect();
    validate_support_ranges(&ranges)
}

fn validate_support_ranges(ranges: &[(i64, i64)]) -> Result<(), ReductionError> {
    for window in ranges.windows(2) {
        let [previous, current] = window else {
            continue;
        };
        let duplicate_endpoint = current.1 == previous.1;
        let overlapping_range = current.0 < previous.1;
        if duplicate_endpoint || overlapping_range {
            return Err(ReductionError::OverlappingSupport);
        }
    }
    Ok(())
}

/// Aligned numerator and denominator counter aggregates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatioReduction {
    numerator: CounterReduction,
    denominator: CounterReduction,
}

impl RatioReduction {
    /// Builds a ratio only when both sides use identical alignment, pair
    /// boundaries, and boundary attribution.
    ///
    /// # Errors
    /// Returns [`ReductionError::MisalignedRatio`] when any alignment
    /// property differs.
    pub fn new(
        numerator: CounterReduction,
        denominator: CounterReduction,
    ) -> Result<Self, ReductionError> {
        if numerator.alignment_id != denominator.alignment_id
            || numerator.pair_support != denominator.pair_support
            || numerator.boundary_quality != denominator.boundary_quality
        {
            return Err(ReductionError::MisalignedRatio);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Ratio of aggregate deltas, or `None` for a zero denominator.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "ratios are floating-point projections of exact sums"
    )]
    pub fn ratio(&self) -> Option<f64> {
        (self.denominator.sum_delta > 0)
            .then(|| self.numerator.sum_delta as f64 / self.denominator.sum_delta as f64)
    }

    /// Numerator aggregate.
    #[must_use]
    pub const fn numerator(&self) -> &CounterReduction {
        &self.numerator
    }

    /// Denominator aggregate.
    #[must_use]
    pub const fn denominator(&self) -> &CounterReduction {
        &self.denominator
    }
}

/// One validated instantaneous gauge sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GaugeSample {
    series_id: MetricSeriesId,
    ts_us: i64,
    value: FiniteF64,
}

impl GaugeSample {
    /// Builds a sample, rejecting `NaN` and infinities.
    #[must_use]
    pub fn new(series_id: MetricSeriesId, ts_us: i64, value: f64) -> Option<Self> {
        Some(Self {
            series_id,
            ts_us,
            value: FiniteF64::new(value)?,
        })
    }

    /// Series identity.
    #[must_use]
    pub const fn series_id(self) -> MetricSeriesId {
        self.series_id
    }

    /// Sample timestamp.
    #[must_use]
    pub const fn ts_us(self) -> i64 {
        self.ts_us
    }

    /// Finite sample value.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.value.get()
    }
}

/// A bounded canonical set of real gauge samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaugeReduction {
    series_id: MetricSeriesId,
    samples: Vec<GaugeSample>,
}

impl GaugeReduction {
    /// Selects samples inside a half-open bucket.
    ///
    /// # Errors
    /// Returns [`ReductionError`] for mixed series, duplicate timestamps, or
    /// a configured sample bound.
    pub fn from_samples(
        samples: &[GaugeSample],
        bucket: CoverageSpan,
        limits: ReductionLimits,
    ) -> Result<Option<Self>, ReductionError> {
        if samples.len() > limits.max_input_items {
            return Err(ReductionError::LimitExceeded);
        }
        let mut selected = Vec::with_capacity(samples.len().min(limits.max_gauge_samples));
        for &sample in samples {
            if sample.ts_us < bucket.start_us() || sample.ts_us >= bucket.end_us() {
                continue;
            }
            if selected.len() == limits.max_gauge_samples {
                return Err(ReductionError::LimitExceeded);
            }
            selected.push(sample);
        }
        Self::from_selected(selected, limits)
    }

    fn from_selected(
        mut samples: Vec<GaugeSample>,
        limits: ReductionLimits,
    ) -> Result<Option<Self>, ReductionError> {
        if samples.is_empty() {
            return Ok(None);
        }
        if samples.len() > limits.max_gauge_samples {
            return Err(ReductionError::LimitExceeded);
        }
        samples.sort_unstable_by_key(|sample| sample.ts_us);
        let series_id = samples[0].series_id;
        if samples.iter().any(|sample| sample.series_id != series_id) {
            return Err(ReductionError::MixedSeries);
        }
        if samples
            .windows(2)
            .any(|pair| pair[0].ts_us == pair[1].ts_us)
        {
            return Err(ReductionError::DuplicateTimestamp);
        }
        Ok(Some(Self { series_id, samples }))
    }

    /// Combines disjoint samples from the same series.
    ///
    /// # Errors
    /// Returns [`ReductionError`] for mixed series, duplicate timestamps, or
    /// a configured sample bound.
    pub fn merge(&self, other: &Self, limits: ReductionLimits) -> Result<Self, ReductionError> {
        if self.series_id != other.series_id {
            return Err(ReductionError::MixedSeries);
        }
        let combined_len = self
            .samples
            .len()
            .checked_add(other.samples.len())
            .ok_or(ReductionError::LimitExceeded)?;
        if combined_len > limits.max_input_items || combined_len > limits.max_gauge_samples {
            return Err(ReductionError::LimitExceeded);
        }
        let mut samples = self.samples.clone();
        samples.extend_from_slice(&other.samples);
        Self::from_selected(samples, limits)?.ok_or(ReductionError::LimitExceeded)
    }

    /// Largest retained sample.
    #[must_use]
    pub fn max(&self) -> f64 {
        let first = self.samples[0].value();
        self.samples
            .iter()
            .skip(1)
            .map(|sample| sample.value())
            .fold(first, f64::max)
    }

    /// Smallest retained sample.
    #[must_use]
    pub fn min(&self) -> f64 {
        let first = self.samples[0].value();
        self.samples
            .iter()
            .skip(1)
            .map(|sample| sample.value())
            .fold(first, f64::min)
    }

    /// Number of retained samples.
    #[must_use]
    pub const fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Mean computed in canonical timestamp order.
    ///
    /// # Errors
    /// Returns [`ReductionError::NonFiniteResult`] when the finite inputs
    /// overflow during accumulation.
    #[allow(
        clippy::cast_precision_loss,
        reason = "the mean is an approximate floating-point projection"
    )]
    pub fn sample_mean(&self) -> Result<f64, ReductionError> {
        let sum = self
            .samples
            .iter()
            .try_fold(0.0_f64, |sum, sample| finite_add(sum, sample.value()))?;
        let mean = sum / self.samples.len() as f64;
        mean.is_finite()
            .then_some(mean)
            .ok_or(ReductionError::NonFiniteResult)
    }
}

fn finite_add(left: f64, right: f64) -> Result<f64, ReductionError> {
    let result = left + right;
    result
        .is_finite()
        .then_some(result)
        .ok_or(ReductionError::NonFiniteResult)
}

/// Explicit zero-order-hold parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldModel {
    /// Longest permitted hold interval.
    pub max_gap_us: u64,
}

/// Result of a zero-order-hold reduction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeWeightedReduction {
    mean: f64,
    covered_duration_us: u64,
}

impl TimeWeightedReduction {
    /// Time-weighted mean.
    #[must_use]
    pub const fn mean(self) -> f64 {
        self.mean
    }

    /// Duration covered by modeled holds.
    #[must_use]
    pub const fn covered_duration_us(self) -> u64 {
        self.covered_duration_us
    }

    /// Boundary attribution used by this reduction.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "the accessor is uniform with reductions whose quality is data-dependent"
    )]
    pub const fn boundary_quality(self) -> BoundaryQuality {
        BoundaryQuality::ModeledHold
    }
}

/// Computes a bounded time-weighted reduction in `O(samples + gaps)` work.
///
/// A value never crosses a known gap. The optional halo sample may cover the
/// bucket head. Samples must be strictly ordered and belong to one series.
///
/// # Errors
/// Returns [`ReductionError`] for invalid ordering, mixed series, non-finite
/// arithmetic, or a configured bound.
#[allow(
    clippy::cast_precision_loss,
    reason = "time weighting is an approximate floating-point projection"
)]
pub fn time_weighted_mean(
    halo_previous: Option<GaugeSample>,
    samples: &[GaugeSample],
    bucket: CoverageSpan,
    hold: HoldModel,
    known_gaps: &Coverage,
    limits: ReductionLimits,
) -> Result<Option<TimeWeightedReduction>, ReductionError> {
    let total_samples = samples
        .len()
        .checked_add(usize::from(halo_previous.is_some()))
        .ok_or(ReductionError::LimitExceeded)?;
    if total_samples > limits.max_input_items
        || total_samples > limits.max_gauge_samples
        || known_gaps.spans().len() > limits.max_gap_spans
    {
        return Err(ReductionError::LimitExceeded);
    }
    let first_series = halo_previous
        .map(GaugeSample::series_id)
        .or_else(|| samples.first().copied().map(GaugeSample::series_id));
    if let Some(series_id) = first_series
        && (halo_previous.is_some_and(|sample| sample.series_id != series_id)
            || samples.iter().any(|sample| sample.series_id != series_id))
    {
        return Err(ReductionError::MixedSeries);
    }
    let at = |index: usize| -> Option<GaugeSample> {
        match halo_previous {
            Some(halo) if index == 0 => Some(halo),
            Some(_) => samples.get(index - 1).copied(),
            None => samples.get(index).copied(),
        }
    };
    for index in 1..total_samples {
        let previous = at(index - 1).ok_or(ReductionError::NonMonotonicSamples)?;
        let current = at(index).ok_or(ReductionError::NonMonotonicSamples)?;
        if current.ts_us <= previous.ts_us {
            return Err(if current.ts_us == previous.ts_us {
                ReductionError::DuplicateTimestamp
            } else {
                ReductionError::NonMonotonicSamples
            });
        }
    }

    let mut weighted_sum = 0.0_f64;
    let mut weight_us = 0_u64;
    let mut gap_index = 0;
    for index in 0..total_samples {
        let sample = at(index).ok_or(ReductionError::NonMonotonicSamples)?;
        let hold_cap = sample.ts_us.saturating_add_unsigned(hold.max_gap_us);
        let mut hold_end = at(index + 1).map_or(hold_cap, |next| next.ts_us.min(hold_cap));
        while known_gaps
            .spans()
            .get(gap_index)
            .is_some_and(|gap| gap.end_us() <= sample.ts_us)
        {
            gap_index += 1;
        }
        if let Some(gap) = known_gaps.spans().get(gap_index)
            && gap.start_us() < hold_end
            && gap.end_us() > sample.ts_us
        {
            hold_end = hold_end.min(gap.start_us().max(sample.ts_us));
        }
        let overlap_from = sample.ts_us.max(bucket.start_us());
        let overlap_to = hold_end.min(bucket.end_us());
        if overlap_from < overlap_to {
            let duration = overlap_to.wrapping_sub(overlap_from).cast_unsigned();
            let weighted = sample.value() * duration as f64;
            if !weighted.is_finite() {
                return Err(ReductionError::NonFiniteResult);
            }
            weighted_sum = finite_add(weighted_sum, weighted)?;
            weight_us = weight_us
                .checked_add(duration)
                .ok_or(ReductionError::CountOverflow)?;
        }
    }
    if weight_us == 0 {
        return Ok(None);
    }
    let mean = weighted_sum / weight_us as f64;
    if !mean.is_finite() {
        return Err(ReductionError::NonFiniteResult);
    }
    Ok(Some(TimeWeightedReduction {
        mean,
        covered_duration_us: weight_us,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "fixtures use exactly representable values"
    )]

    use super::*;

    const LIMITS: ReductionLimits = ReductionLimits {
        max_input_items: 64,
        max_gap_spans: 64,
        max_counter_pairs: 64,
        max_gauge_samples: 64,
    };

    fn series(value: u8) -> MetricSeriesId {
        MetricSeriesId([value; 16])
    }

    fn alignment(value: u8) -> AlignmentId {
        AlignmentId([value; 16])
    }

    fn counter(ts_us: i64, value: u64) -> CounterSample {
        CounterSample::new(series(1), alignment(1), ts_us, value, 1)
    }

    fn gauge(ts_us: i64, value: f64) -> GaugeSample {
        GaugeSample::new(series(2), ts_us, value).expect("finite fixture")
    }

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid fixture span")
    }

    fn pairs(samples: &[CounterSample], gaps: &Coverage) -> Vec<CounterInterval> {
        classify_series(None, samples, gaps, LIMITS).expect("bounded fixture")
    }

    fn reduce(samples: &[CounterSample], bucket: CoverageSpan) -> Option<CounterReduction> {
        CounterReduction::from_intervals(&pairs(samples, &Coverage::empty()), bucket, LIMITS)
            .expect("valid fixture")
    }

    #[test]
    fn reset_gap_and_mixed_series_never_become_zero_deltas() {
        let previous = counter(0, 100);
        let reset = CounterSample::new(series(1), alignment(1), 10, 40, 2);
        assert_eq!(
            CounterInterval::classify(Some(previous), reset, &Coverage::empty()).quality(),
            PairQuality::Reset
        );
        let gaps = Coverage::from_spans(vec![span(4, 6)]);
        assert_eq!(
            CounterInterval::classify(Some(previous), counter(10, 110), &gaps).delta(),
            None
        );
        let other = CounterSample::new(series(9), alignment(1), 10, 110, 1);
        assert_eq!(
            CounterInterval::classify(Some(previous), other, &Coverage::empty()).quality(),
            PairQuality::DifferentSeries
        );
    }

    #[test]
    fn full_timestamp_span_has_a_valid_u64_duration() {
        let previous = counter(i64::MIN, 0);
        let current = counter(i64::MAX, 1);
        let pair = CounterInterval::classify(Some(previous), current, &Coverage::empty());
        assert_eq!(pair.duration_us(), Some(u64::MAX));
    }

    #[test]
    fn pair_attribution_records_cross_boundary_evidence() {
        let intervals = pairs(&[counter(0, 0), counter(10, 5)], &Coverage::empty());
        let contained = CounterReduction::from_intervals(&intervals, span(0, 20), LIMITS)
            .expect("valid")
            .expect("one pair");
        assert_eq!(contained.boundary_quality(), BoundaryQuality::Contained);
        let crossing = CounterReduction::from_intervals(&intervals, span(5, 20), LIMITS)
            .expect("valid")
            .expect("one pair");
        assert_eq!(
            crossing.boundary_quality(),
            BoundaryQuality::EndpointAttributedCrossBoundary
        );
    }

    #[test]
    fn boundary_attribution_is_partition_invariant() {
        let intervals = pairs(
            &[counter(0, 0), counter(10, 5), counter(20, 9)],
            &Coverage::empty(),
        );
        let bucket = span(5, 30);
        let whole = CounterReduction::from_intervals(&intervals, bucket, LIMITS)
            .expect("valid")
            .expect("two pairs");
        let head = CounterReduction::from_intervals(&intervals[1..2], bucket, LIMITS)
            .expect("valid")
            .expect("cross-boundary pair");
        let tail = CounterReduction::from_intervals(&intervals[2..], bucket, LIMITS)
            .expect("valid")
            .expect("contained pair");
        assert_eq!(whole.boundary_quality(), BoundaryQuality::Mixed);
        assert_eq!(head.merge(&tail, LIMITS), Ok(whole));
    }

    #[test]
    fn halo_bridge_is_counted_once_for_every_partition() {
        let samples = [
            counter(0, 10),
            counter(10, 25),
            counter(20, 25),
            counter(30, 100),
            counter(40, 130),
        ];
        let bucket = span(i64::MIN, i64::MAX);
        let whole =
            CounterReduction::from_intervals(&pairs(&samples, &Coverage::empty()), bucket, LIMITS)
                .expect("valid")
                .expect("pairs");
        for split in 1..samples.len() {
            let (head, tail) = samples.split_at(split);
            let head =
                classify_series(None, head, &Coverage::empty(), LIMITS).expect("bounded head");
            let tail = classify_series(Some(samples[split - 1]), tail, &Coverage::empty(), LIMITS)
                .expect("bounded tail");
            let head = CounterReduction::from_intervals(&head, bucket, LIMITS).expect("valid head");
            let tail = CounterReduction::from_intervals(&tail, bucket, LIMITS)
                .expect("valid tail")
                .expect("tail includes the bridge");
            let merged = head.map_or_else(|| Ok(tail.clone()), |head| head.merge(&tail, LIMITS));
            assert_eq!(merged, Ok(whole.clone()), "split at {split}");
        }
    }

    #[test]
    fn aggregate_rate_uses_sums_not_mean_pair_rates() {
        let reduction = reduce(
            &[counter(0, 0), counter(10, 10), counter(100, 10)],
            span(i64::MIN, i64::MAX),
        )
        .expect("valid pairs");
        assert_eq!(reduction.rate_per_us(), 0.1);
    }

    #[test]
    fn counter_merge_rejects_wrong_series_and_overlapping_support() {
        let a = reduce(&[counter(0, 0), counter(10, 1)], span(0, 11)).expect("pair");
        assert_eq!(a.merge(&a, LIMITS), Err(ReductionError::OverlappingSupport));
        let other_samples = [
            CounterSample::new(series(9), alignment(1), 20, 0, 1),
            CounterSample::new(series(9), alignment(1), 30, 1, 1),
        ];
        let b = reduce(&other_samples, span(0, 40)).expect("pair");
        assert_eq!(a.merge(&b, LIMITS), Err(ReductionError::MixedSeries));
    }

    #[test]
    fn aligned_ratio_divides_aggregate_deltas() {
        let numerator = reduce(
            &[counter(0, 0), counter(10, 1), counter(20, 2)],
            span(0, 30),
        )
        .expect("pairs");
        let denominator_samples = [
            CounterSample::new(series(3), alignment(1), 0, 0, 1),
            CounterSample::new(series(3), alignment(1), 10, 1, 1),
            CounterSample::new(series(3), alignment(1), 20, 10, 1),
        ];
        let denominator = CounterReduction::from_intervals(
            &pairs(&denominator_samples, &Coverage::empty()),
            span(0, 30),
            LIMITS,
        )
        .expect("valid")
        .expect("pairs");
        let ratio = RatioReduction::new(numerator, denominator).expect("aligned");
        assert_eq!(ratio.ratio(), Some(0.2));
    }

    #[test]
    fn ratio_rejects_different_pair_boundaries() {
        let a = reduce(&[counter(0, 0), counter(10, 1)], span(0, 20)).expect("pair");
        let b = reduce(&[counter(0, 0), counter(11, 1)], span(0, 20)).expect("pair");
        assert_eq!(
            RatioReduction::new(a, b),
            Err(ReductionError::MisalignedRatio)
        );
    }

    #[test]
    fn gauge_values_are_finite_and_zero_is_canonical() {
        assert_eq!(GaugeSample::new(series(1), 0, f64::NAN), None);
        assert_eq!(GaugeSample::new(series(1), 0, f64::INFINITY), None);
        assert_eq!(
            GaugeSample::new(series(1), 0, -0.0),
            GaugeSample::new(series(1), 0, 0.0)
        );
    }

    #[test]
    fn gauge_merge_is_partition_invariant_for_adversarial_values() {
        let all = [gauge(0, 1.0e16), gauge(1, -1.0e16), gauge(2, 1.0)];
        let whole = GaugeReduction::from_samples(&all, span(0, 3), LIMITS)
            .expect("valid")
            .expect("samples");
        let head = GaugeReduction::from_samples(&all[..1], span(0, 3), LIMITS)
            .expect("valid")
            .expect("sample");
        let tail = GaugeReduction::from_samples(&all[1..], span(0, 3), LIMITS)
            .expect("valid")
            .expect("samples");
        let merged = head.merge(&tail, LIMITS).expect("disjoint");
        assert_eq!(merged.sample_mean(), whole.sample_mean());
        assert_eq!(tail.merge(&head, LIMITS), Ok(merged));

        let middle = GaugeReduction::from_samples(&all[1..2], span(0, 3), LIMITS)
            .expect("valid")
            .expect("sample");
        let end = GaugeReduction::from_samples(&all[2..], span(0, 3), LIMITS)
            .expect("valid")
            .expect("sample");
        let left = head
            .merge(&middle, LIMITS)
            .and_then(|partial| partial.merge(&end, LIMITS));
        let right = middle
            .merge(&end, LIMITS)
            .and_then(|partial| head.merge(&partial, LIMITS));
        assert_eq!(left, right);
    }

    #[test]
    fn gauge_rejects_duplicate_timestamps_and_overflowing_sum() {
        let duplicates = [gauge(0, 1.0), gauge(0, 2.0)];
        assert_eq!(
            GaugeReduction::from_samples(&duplicates, span(-1, 1), LIMITS),
            Err(ReductionError::DuplicateTimestamp)
        );
        let huge = [gauge(0, f64::MAX), gauge(1, f64::MAX)];
        let reduction = GaugeReduction::from_samples(&huge, span(0, 2), LIMITS)
            .expect("valid samples")
            .expect("samples");
        assert_eq!(
            reduction.sample_mean(),
            Err(ReductionError::NonFiniteResult)
        );
    }

    #[test]
    fn time_weighting_stops_at_a_gap_and_uses_linear_work_contract() {
        let samples = [gauge(0, 100.0), gauge(20, 0.0)];
        let gaps = Coverage::from_spans(vec![span(5, 15)]);
        let mean = time_weighted_mean(
            None,
            &samples,
            span(0, 25),
            HoldModel { max_gap_us: 1_000 },
            &gaps,
            LIMITS,
        );
        let reduction = mean.expect("valid reduction").expect("covered duration");
        assert_eq!(reduction.mean(), 50.0);
        assert_eq!(reduction.covered_duration_us(), 10);
        assert_eq!(reduction.boundary_quality(), BoundaryQuality::ModeledHold);
    }

    #[test]
    fn reduction_limits_fail_before_unbounded_output() {
        let tight = ReductionLimits {
            max_input_items: 1,
            max_gap_spans: 1,
            max_counter_pairs: 1,
            max_gauge_samples: 1,
        };
        assert_eq!(
            classify_series(
                None,
                &[counter(0, 0), counter(1, 1)],
                &Coverage::empty(),
                tight,
            ),
            Err(ReductionError::LimitExceeded)
        );
        assert_eq!(
            GaugeReduction::from_samples(&[gauge(0, 1.0), gauge(1, 2.0)], span(0, 2), tight),
            Err(ReductionError::LimitExceeded)
        );

        let wide = ReductionLimits {
            max_input_items: 8,
            ..tight
        };
        let counter_head = reduce(&[counter(0, 0), counter(1, 1)], span(0, 2)).expect("one pair");
        let counter_tail = reduce(&[counter(2, 1), counter(3, 2)], span(2, 4)).expect("one pair");
        assert_eq!(
            counter_head.merge(&counter_tail, tight),
            Err(ReductionError::LimitExceeded)
        );

        let gauge_head = GaugeReduction::from_samples(&[gauge(0, 1.0)], span(0, 2), wide)
            .expect("valid")
            .expect("one sample");
        let gauge_tail = GaugeReduction::from_samples(&[gauge(1, 2.0)], span(0, 2), wide)
            .expect("valid")
            .expect("one sample");
        assert_eq!(
            gauge_head.merge(&gauge_tail, tight),
            Err(ReductionError::LimitExceeded)
        );
    }
}
