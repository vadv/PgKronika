//! Counter and gauge reductions that never invent samples.
//!
//! A cumulative counter yields information only between two comparable
//! samples: same reset family, forward time, no coverage gap, no decrease.
//! Anything else is a boundary — a broken pair asserts nothing and never
//! becomes a zero delta. Valid pairs aggregate as `sum(delta)/sum(duration)`,
//! not as a mean of per-pair rates, and a ratio divides aggregate sums after
//! merge, so partitioning a series cannot bend the result.
//!
//! A gauge is reduced over the samples that actually exist. Extrema and the
//! sample mean use real samples in the bucket; time weighting exists only
//! under an explicitly declared zero-order-hold model, and a known gap always
//! breaks the hold — a value is never carried across proven silence.

use super::counts::CountOverflow;
use super::coverage::{Coverage, CoverageSpan};

/// One cumulative counter sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterSample {
    /// Sample timestamp.
    pub ts_us: i64,
    /// The cumulative value.
    pub value: u64,
    /// The writer-declared reset family; a mismatch always breaks a pair.
    pub reset_epoch: u64,
}

/// The classification of one candidate counter interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairQuality {
    /// Comparable samples: a usable delta and duration exist.
    Valid,
    /// The counter reset: the epoch changed or the value decreased.
    Reset,
    /// A known coverage gap lies between the samples.
    Gap,
    /// The current sample does not move forward in time.
    NonMonotonicTime,
    /// No predecessor exists; the candidate asserts nothing.
    Missing,
}

/// One candidate interval between adjacent samples of a series.
///
/// Built only by classification, so the quality always matches the samples.
/// The delta and duration are readable only from a valid pair: a reset or a
/// gap is a boundary, not a zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterInterval {
    previous: Option<CounterSample>,
    current: CounterSample,
    quality: PairQuality,
}

impl CounterInterval {
    /// Classifies one adjacent pair against the known coverage gaps.
    ///
    /// The order of checks: a missing predecessor, non-monotonic time, a
    /// reset (epoch mismatch, or the primary detector — a decreased value),
    /// then a known gap between the samples.
    #[must_use]
    pub fn classify(
        previous: Option<CounterSample>,
        current: CounterSample,
        known_gaps: &Coverage,
    ) -> Self {
        let quality = match previous {
            None => PairQuality::Missing,
            Some(previous) if current.ts_us <= previous.ts_us => PairQuality::NonMonotonicTime,
            Some(previous)
                if current.reset_epoch != previous.reset_epoch
                    || current.value < previous.value =>
            {
                PairQuality::Reset
            }
            Some(previous) if gap_between(known_gaps, previous.ts_us, current.ts_us) => {
                PairQuality::Gap
            }
            Some(_) => PairQuality::Valid,
        };
        Self {
            previous,
            current,
            quality,
        }
    }

    /// The classification.
    #[must_use]
    pub const fn quality(&self) -> PairQuality {
        self.quality
    }

    /// The current sample; a valid pair belongs to its bucket only.
    #[must_use]
    pub const fn current(&self) -> CounterSample {
        self.current
    }

    /// The predecessor, when one exists.
    #[must_use]
    pub const fn previous(&self) -> Option<CounterSample> {
        self.previous
    }

    /// The non-negative increment, only for a valid pair.
    #[must_use]
    pub fn delta(&self) -> Option<u64> {
        if self.quality != PairQuality::Valid {
            return None;
        }
        self.current.value.checked_sub(self.previous?.value)
    }

    /// The positive duration, only for a valid pair.
    #[must_use]
    pub fn duration_us(&self) -> Option<u64> {
        if self.quality != PairQuality::Valid {
            return None;
        }
        let diff = self.current.ts_us.checked_sub(self.previous?.ts_us)?;
        (diff > 0).then(|| diff.cast_unsigned())
    }
}

/// Candidate intervals of one series, in sample order.
///
/// `halo_previous` is the last sample before the queried range; the pair it
/// forms with the first sample is the bridge pair and belongs to the bucket
/// of that first sample, so a partition of the series counts it exactly
/// once. Samples must be in ascending time order.
#[must_use]
pub fn classify_series(
    halo_previous: Option<CounterSample>,
    samples: &[CounterSample],
    known_gaps: &Coverage,
) -> Vec<CounterInterval> {
    let mut previous = halo_previous;
    let mut out = Vec::with_capacity(samples.len());
    for &sample in samples {
        out.push(CounterInterval::classify(previous, sample, known_gaps));
        previous = Some(sample);
    }
    out
}

/// Whether a known gap intersects the open span between two timestamps.
fn gap_between(known_gaps: &Coverage, from_us: i64, to_us: i64) -> bool {
    known_gaps
        .spans()
        .iter()
        .any(|gap| gap.start_us() < to_us && gap.end_us() > from_us)
}

/// Exact aggregate of the valid pairs owned by one bucket.
///
/// Kept as checked sums so that merging partial reductions equals reducing
/// the whole, and the rate is always `sum(delta)/sum(duration)` — never an
/// average of per-pair rates, which would weight a short pair like a long
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterReduction {
    sum_delta: u64,
    sum_duration_us: u64,
    valid_pairs: u64,
}

impl CounterReduction {
    /// Aggregates the valid pairs whose current sample lies in `bucket`.
    ///
    /// Returns `Ok(None)` when no valid pair is owned by the bucket: the
    /// absence of evidence is not a zero rate.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if a checked sum exceeds [`u64::MAX`].
    pub fn from_intervals(
        intervals: &[CounterInterval],
        bucket: CoverageSpan,
    ) -> Result<Option<Self>, CountOverflow> {
        let mut reduction = Self {
            sum_delta: 0,
            sum_duration_us: 0,
            valid_pairs: 0,
        };
        for interval in intervals {
            let (Some(delta), Some(duration_us)) = (interval.delta(), interval.duration_us())
            else {
                continue;
            };
            let ts = interval.current().ts_us;
            if ts < bucket.start_us() || ts >= bucket.end_us() {
                continue;
            }
            reduction = Self {
                sum_delta: reduction
                    .sum_delta
                    .checked_add(delta)
                    .ok_or(CountOverflow)?,
                sum_duration_us: reduction
                    .sum_duration_us
                    .checked_add(duration_us)
                    .ok_or(CountOverflow)?,
                valid_pairs: reduction.valid_pairs.checked_add(1).ok_or(CountOverflow)?,
            };
        }
        Ok((reduction.valid_pairs > 0).then_some(reduction))
    }

    /// Merges two aggregates of the same series semantics.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if a checked sum exceeds [`u64::MAX`].
    pub fn merge(&self, other: &Self) -> Result<Self, CountOverflow> {
        Ok(Self {
            sum_delta: self
                .sum_delta
                .checked_add(other.sum_delta)
                .ok_or(CountOverflow)?,
            sum_duration_us: self
                .sum_duration_us
                .checked_add(other.sum_duration_us)
                .ok_or(CountOverflow)?,
            valid_pairs: self
                .valid_pairs
                .checked_add(other.valid_pairs)
                .ok_or(CountOverflow)?,
        })
    }

    /// Total increment across the aggregated pairs.
    #[must_use]
    pub const fn sum_delta(&self) -> u64 {
        self.sum_delta
    }

    /// Total covered duration across the aggregated pairs.
    #[must_use]
    pub const fn sum_duration_us(&self) -> u64 {
        self.sum_duration_us
    }

    /// How many valid pairs were aggregated, always positive.
    #[must_use]
    pub const fn valid_pairs(&self) -> u64 {
        self.valid_pairs
    }

    /// The aggregate rate per microsecond, `sum(delta)/sum(duration)`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "deltas and durations stay far below 2^53, so the f64 \
                  quotient is exact enough for a rate"
    )]
    pub fn rate_per_us(&self) -> f64 {
        self.sum_delta as f64 / self.sum_duration_us as f64
    }
}

/// A ratio counter: numerator and denominator aggregated separately.
///
/// Division happens once, after merge. Averaging per-pair ratios would let a
/// pair with tiny traffic outvote a busy one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatioReduction {
    /// The numerator series aggregate.
    pub numerator: CounterReduction,
    /// The denominator series aggregate.
    pub denominator: CounterReduction,
}

impl RatioReduction {
    /// Merges both sides pairwise.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if a checked sum exceeds [`u64::MAX`].
    pub fn merge(&self, other: &Self) -> Result<Self, CountOverflow> {
        Ok(Self {
            numerator: self.numerator.merge(&other.numerator)?,
            denominator: self.denominator.merge(&other.denominator)?,
        })
    }

    /// The ratio of aggregate sums; `None` when the denominator did not
    /// move.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "deltas stay far below 2^53, so the f64 quotient is exact \
                  enough for a ratio"
    )]
    pub fn ratio(&self) -> Option<f64> {
        (self.denominator.sum_delta > 0)
            .then(|| self.numerator.sum_delta as f64 / self.denominator.sum_delta as f64)
    }
}

/// One instantaneous gauge sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GaugeSample {
    /// Sample timestamp.
    pub ts_us: i64,
    /// The sampled value; the caller keeps it finite.
    pub value: f64,
}

/// Extrema and sum over the real samples inside one bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GaugeReduction {
    max: f64,
    min: f64,
    sum: f64,
    count: u64,
}

impl GaugeReduction {
    /// Reduces the samples whose timestamp lies in `bucket`.
    ///
    /// Returns `Ok(None)` when the bucket holds no sample: an empty bucket
    /// has no extrema and no mean, not zeros.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if the sample count exceeds [`u64::MAX`].
    pub fn from_samples(
        samples: &[GaugeSample],
        bucket: CoverageSpan,
    ) -> Result<Option<Self>, CountOverflow> {
        let mut reduction: Option<Self> = None;
        for sample in samples {
            if sample.ts_us < bucket.start_us() || sample.ts_us >= bucket.end_us() {
                continue;
            }
            reduction = Some(match reduction {
                None => Self {
                    max: sample.value,
                    min: sample.value,
                    sum: sample.value,
                    count: 1,
                },
                Some(r) => Self {
                    max: r.max.max(sample.value),
                    min: r.min.min(sample.value),
                    sum: r.sum + sample.value,
                    count: r.count.checked_add(1).ok_or(CountOverflow)?,
                },
            });
        }
        Ok(reduction)
    }

    /// Merges two reductions of the same series semantics.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if the sample count exceeds [`u64::MAX`].
    pub fn merge(&self, other: &Self) -> Result<Self, CountOverflow> {
        Ok(Self {
            max: self.max.max(other.max),
            min: self.min.min(other.min),
            sum: self.sum + other.sum,
            count: self.count.checked_add(other.count).ok_or(CountOverflow)?,
        })
    }

    /// The largest real sample.
    #[must_use]
    pub const fn max(&self) -> f64 {
        self.max
    }

    /// The smallest real sample.
    #[must_use]
    pub const fn min(&self) -> f64 {
        self.min
    }

    /// How many samples were reduced, always positive.
    #[must_use]
    pub const fn sample_count(&self) -> u64 {
        self.count
    }

    /// The sample mean `sum/count` — never a mean of partial means.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "sample counts stay far below 2^53, so the f64 quotient is \
                  exact enough for a mean"
    )]
    pub fn sample_mean(&self) -> f64 {
        self.sum / self.count as f64
    }
}

/// An explicitly declared zero-order-hold model.
///
/// Time weighting exists only for factors that declare one; without it a
/// gauge has extrema and a sample mean, nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldModel {
    /// The longest span one sample may be held; longer silence is a hole.
    pub max_gap_us: u64,
}

/// Time-weighted mean over a bucket under zero-order-hold.
///
/// Each sample holds its value until the next sample, at most `max_gap_us`,
/// and never across a known gap — proven silence breaks the hold where it
/// starts. The last sample holds up to `max_gap_us` on its own. Hold
/// intervals intersect the bucket mathematically; when nothing proven
/// overlaps the bucket the result is `None`, not zero. Samples must be in
/// ascending time order; `halo_previous` may hold into the bucket head.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "held durations stay far below 2^53 microseconds, so the f64 \
              weights are exact"
)]
pub fn time_weighted_mean(
    halo_previous: Option<GaugeSample>,
    samples: &[GaugeSample],
    bucket: CoverageSpan,
    hold: HoldModel,
    known_gaps: &Coverage,
) -> Option<f64> {
    let mut weighted_sum = 0.0_f64;
    let mut weight_us = 0_u64;

    let chain: Vec<GaugeSample> = halo_previous
        .into_iter()
        .chain(samples.iter().copied())
        .collect();
    for (index, sample) in chain.iter().enumerate() {
        let hold_cap = sample.ts_us.saturating_add_unsigned(hold.max_gap_us);
        let next_ts = chain.get(index + 1).map(|next| next.ts_us);
        let mut hold_end = next_ts.map_or(hold_cap, |ts| ts.min(hold_cap));

        // A known gap forbids carry-forward: the hold stops where proven
        // silence starts.
        for gap in known_gaps.spans() {
            if gap.start_us() < hold_end && gap.end_us() > sample.ts_us {
                hold_end = hold_end.min(gap.start_us().max(sample.ts_us));
            }
        }

        let overlap_from = sample.ts_us.max(bucket.start_us());
        let overlap_to = hold_end.min(bucket.end_us());
        if overlap_from < overlap_to {
            let duration = overlap_to.wrapping_sub(overlap_from).cast_unsigned();
            weighted_sum = sample.value.mul_add(duration as f64, weighted_sum);
            weight_us = weight_us.checked_add(duration)?;
        }
    }

    (weight_us > 0).then(|| weighted_sum / weight_us as f64)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "rates and means asserted are exact dyadic values"
    )]

    use super::*;

    fn sample(ts_us: i64, value: u64) -> CounterSample {
        CounterSample {
            ts_us,
            value,
            reset_epoch: 1,
        }
    }

    fn gauge(ts_us: i64, value: f64) -> GaugeSample {
        GaugeSample { ts_us, value }
    }

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid span in fixture")
    }

    fn no_gaps() -> Coverage {
        Coverage::empty()
    }

    fn reduce_all(intervals: &[CounterInterval]) -> Option<CounterReduction> {
        CounterReduction::from_intervals(intervals, span(i64::MIN, i64::MAX))
            .expect("no overflow in fixture")
    }

    #[test]
    fn a_bridge_pair_is_counted_exactly_once_for_any_split() {
        let series = [
            sample(0, 10),
            sample(10, 25),
            sample(20, 25),
            sample(30, 100),
            sample(40, 130),
        ];
        let whole =
            reduce_all(&classify_series(None, &series, &no_gaps())).expect("valid pairs exist");
        for split in 1..series.len() {
            let (head, tail) = series.split_at(split);
            let head_pairs = classify_series(None, head, &no_gaps());
            // The bridge predecessor is the halo: the last sample before the
            // second part.
            let tail_pairs = classify_series(Some(head[split - 1]), tail, &no_gaps());
            let head_reduction = reduce_all(&head_pairs);
            let tail_reduction = reduce_all(&tail_pairs).expect("tail has the bridge pair");
            let merged = head_reduction
                .map_or(Ok(tail_reduction), |h| h.merge(&tail_reduction))
                .expect("no overflow");
            assert_eq!(merged, whole, "split at {split}");
        }
    }

    #[test]
    fn an_epoch_change_forbids_the_bridge_and_is_not_a_zero_delta() {
        let previous = sample(0, 100);
        let mut current = sample(10, 100);
        current.reset_epoch = 2;
        let pair = CounterInterval::classify(Some(previous), current, &no_gaps());
        assert_eq!(pair.quality(), PairQuality::Reset);
        assert_eq!(pair.delta(), None);
        assert_eq!(pair.duration_us(), None);
    }

    #[test]
    fn a_decrease_is_a_reset_boundary_not_a_zero_delta() {
        let pair = CounterInterval::classify(Some(sample(0, 100)), sample(10, 40), &no_gaps());
        assert_eq!(pair.quality(), PairQuality::Reset);
        assert_eq!(pair.delta(), None);
    }

    #[test]
    fn a_known_gap_between_samples_forbids_the_bridge() {
        let gaps = Coverage::from_spans(vec![span(4, 6)]);
        let pair = CounterInterval::classify(Some(sample(0, 10)), sample(10, 20), &gaps);
        assert_eq!(pair.quality(), PairQuality::Gap);
        assert_eq!(pair.delta(), None);

        // A gap entirely outside the pair does not break it.
        let outside = Coverage::from_spans(vec![span(20, 30)]);
        let pair = CounterInterval::classify(Some(sample(0, 10)), sample(10, 20), &outside);
        assert_eq!(pair.quality(), PairQuality::Valid);
    }

    #[test]
    fn non_monotonic_time_rejects_the_pair() {
        let pair = CounterInterval::classify(Some(sample(10, 10)), sample(10, 20), &no_gaps());
        assert_eq!(pair.quality(), PairQuality::NonMonotonicTime);
        let pair = CounterInterval::classify(Some(sample(10, 10)), sample(5, 20), &no_gaps());
        assert_eq!(pair.quality(), PairQuality::NonMonotonicTime);
    }

    #[test]
    fn a_missing_predecessor_asserts_nothing() {
        let pair = CounterInterval::classify(None, sample(10, 20), &no_gaps());
        assert_eq!(pair.quality(), PairQuality::Missing);
        assert_eq!(pair.delta(), None);
    }

    #[test]
    fn rate_is_aggregate_sum_over_sum_not_mean_of_rates() {
        // Pair one: 10 over 10us (rate 1.0); pair two: 0 over 90us (rate 0).
        let intervals = classify_series(
            None,
            &[sample(0, 0), sample(10, 10), sample(100, 10)],
            &no_gaps(),
        );
        let reduction = reduce_all(&intervals).expect("two valid pairs");
        // Mean of per-pair rates would claim 0.5; the truth is 10/100.
        assert_eq!(reduction.rate_per_us(), 0.1);
    }

    #[test]
    fn a_ratio_divides_aggregates_not_mean_ratios() {
        let numerator = classify_series(
            None,
            &[sample(0, 0), sample(10, 1), sample(20, 2)],
            &no_gaps(),
        );
        let denominator = classify_series(
            None,
            &[sample(0, 0), sample(10, 1), sample(20, 10)],
            &no_gaps(),
        );
        let bucket = span(0, 100);
        let ratio = RatioReduction {
            numerator: CounterReduction::from_intervals(&numerator, bucket)
                .expect("no overflow")
                .expect("valid pairs"),
            denominator: CounterReduction::from_intervals(&denominator, bucket)
                .expect("no overflow")
                .expect("valid pairs"),
        };
        // Per-pair ratios are 1/1 and 1/9; their mean would be ~0.56.
        assert_eq!(ratio.ratio(), Some(0.2));
    }

    #[test]
    fn a_zero_denominator_ratio_is_none_not_zero() {
        let flat = classify_series(None, &[sample(0, 5), sample(10, 5)], &no_gaps());
        let reduction = reduce_all(&flat).expect("one valid pair");
        let ratio = RatioReduction {
            numerator: reduction,
            denominator: reduction,
        };
        assert_eq!(ratio.ratio(), None);
    }

    #[test]
    fn no_valid_pairs_reduce_to_none_not_zero() {
        let broken = classify_series(None, &[sample(0, 100), sample(10, 40)], &no_gaps());
        assert_eq!(reduce_all(&broken), None);
        assert_eq!(reduce_all(&[]), None);
    }

    #[test]
    fn a_valid_pair_belongs_only_to_the_bucket_of_its_current_sample() {
        let series = [sample(0, 0), sample(10, 5), sample(20, 9), sample(30, 10)];
        let intervals = classify_series(None, &series, &no_gaps());
        let whole = reduce_all(&intervals).expect("valid pairs");

        let buckets = [span(0, 15), span(15, 25), span(25, 100)];
        let mut merged: Option<CounterReduction> = None;
        for bucket in buckets {
            if let Some(part) =
                CounterReduction::from_intervals(&intervals, bucket).expect("no overflow")
            {
                merged = Some(
                    merged
                        .map_or(Ok(part), |m| m.merge(&part))
                        .expect("no overflow"),
                );
            }
        }
        assert_eq!(merged, Some(whole));
    }

    #[test]
    fn sparse_cadence_yields_the_long_interval_rate_or_none() {
        // §17.5 golden: samples at t0 and t10s with a missing t5 sample.
        let series = [sample(0, 0), sample(10_000_000, 10)];
        let intervals = classify_series(None, &series, &no_gaps());
        let reduction = reduce_all(&intervals).expect("continuity is provable");
        // 10 units over 10 seconds: 100% of one unit per second, not 200%.
        assert_eq!(reduction.rate_per_us() * 1_000_000.0, 1.0);

        // With a proven gap in between, continuity fails: None, not zero.
        let gaps = Coverage::from_spans(vec![span(4_000_000, 6_000_000)]);
        let broken = classify_series(None, &series, &gaps);
        assert_eq!(reduce_all(&broken), None);
    }

    #[test]
    fn a_flat_cumulative_counter_is_a_valid_zero_delta() {
        // OOM counter staying 1 -> 1 is one valid pair with delta zero — a
        // proven absence of new events, not a boundary.
        let pair = CounterInterval::classify(Some(sample(0, 1)), sample(10, 1), &no_gaps());
        assert_eq!(pair.quality(), PairQuality::Valid);
        assert_eq!(pair.delta(), Some(0));
    }

    #[test]
    fn counter_merge_is_associative_and_commutative() {
        let series = [sample(0, 0), sample(10, 4), sample(20, 6), sample(30, 12)];
        let intervals = classify_series(None, &series, &no_gaps());
        let parts: Vec<CounterReduction> = [span(0, 11), span(11, 21), span(21, 31)]
            .into_iter()
            .filter_map(|bucket| {
                CounterReduction::from_intervals(&intervals, bucket).expect("no overflow")
            })
            .collect();
        let [a, b, c] = parts.try_into().expect("three buckets with pairs");
        let left = a.merge(&b).and_then(|ab| ab.merge(&c));
        let right = b.merge(&c).and_then(|bc| a.merge(&bc));
        assert_eq!(left, right);
        assert_eq!(a.merge(&b), b.merge(&a));
    }

    fn gauge_reduce(samples: &[GaugeSample], bucket: CoverageSpan) -> GaugeReduction {
        GaugeReduction::from_samples(samples, bucket)
            .expect("no overflow in fixture")
            .expect("samples in bucket")
    }

    #[test]
    fn gauge_extrema_use_only_real_samples_inside_the_bucket() {
        let samples = [gauge(0, 100.0), gauge(10, 1.0), gauge(20, 3.0)];
        let reduction = gauge_reduce(&samples, span(5, 25));
        // The 100.0 sample sits outside the bucket and must not leak in.
        assert_eq!(reduction.max(), 3.0);
        assert_eq!(reduction.min(), 1.0);
        assert_eq!(reduction.sample_count(), 2);
        assert_eq!(
            GaugeReduction::from_samples(&samples, span(30, 40)),
            Ok(None)
        );
    }

    #[test]
    fn gauge_merge_is_associative_and_commutative_on_exact_values() {
        let a = gauge_reduce(&[gauge(0, 2.0)], span(0, 10));
        let b = gauge_reduce(&[gauge(1, 8.0)], span(0, 10));
        let c = gauge_reduce(&[gauge(2, -4.0)], span(0, 10));
        let left = a.merge(&b).and_then(|ab| ab.merge(&c));
        let right = b.merge(&c).and_then(|bc| a.merge(&bc));
        assert_eq!(left, right);
        assert_eq!(a.merge(&b), b.merge(&a));
    }

    #[test]
    fn sample_mean_is_sum_over_count_not_mean_of_partial_means() {
        let one = gauge_reduce(&[gauge(0, 0.0)], span(0, 10));
        let three = gauge_reduce(
            &[gauge(11, 2.0), gauge(12, 2.0), gauge(13, 2.0)],
            span(10, 20),
        );
        let merged = one.merge(&three).expect("no overflow");
        // Mean of partial means would claim 1.0.
        assert_eq!(merged.sample_mean(), 1.5);
    }

    #[test]
    fn time_weighting_holds_between_samples_and_caps_at_max_gap() {
        let hold = HoldModel { max_gap_us: 10 };
        // The 100.0 sample would hold 100us to the next sample; the model
        // caps it at 10us of proven hold.
        let mean = time_weighted_mean(
            None,
            &[gauge(0, 100.0), gauge(100, 0.0)],
            span(0, 100),
            hold,
            &no_gaps(),
        );
        assert_eq!(mean, Some(100.0));

        let generous = HoldModel { max_gap_us: 1_000 };
        let mean = time_weighted_mean(
            None,
            &[gauge(0, 100.0), gauge(50, 0.0)],
            span(0, 100),
            generous,
            &no_gaps(),
        );
        // [0,50) at 100.0 and [50,100) at 0.0.
        assert_eq!(mean, Some(50.0));
    }

    #[test]
    fn a_known_gap_breaks_the_hold_where_silence_starts() {
        let hold = HoldModel { max_gap_us: 1_000 };
        let gaps = Coverage::from_spans(vec![span(5, 15)]);
        let mean = time_weighted_mean(
            None,
            &[gauge(0, 100.0), gauge(20, 0.0)],
            span(0, 25),
            hold,
            &gaps,
        );
        // 100.0 holds only [0,5); 0.0 holds [20,25): (500 + 0) / 10.
        assert_eq!(mean, Some(50.0));
    }

    #[test]
    fn a_halo_boundary_sample_holds_into_the_bucket_head() {
        let hold = HoldModel { max_gap_us: 1_000 };
        let mean = time_weighted_mean(
            Some(gauge(-10, 7.0)),
            &[gauge(5, 7.0)],
            span(0, 10),
            hold,
            &no_gaps(),
        );
        assert_eq!(mean, Some(7.0));
    }

    #[test]
    fn no_proven_overlap_means_none_not_zero() {
        let hold = HoldModel { max_gap_us: 10 };
        assert_eq!(
            time_weighted_mean(None, &[], span(0, 10), hold, &no_gaps()),
            None
        );
        // The sample's capped hold ends before the bucket starts.
        assert_eq!(
            time_weighted_mean(None, &[gauge(0, 5.0)], span(50, 60), hold, &no_gaps()),
            None
        );
    }
}
