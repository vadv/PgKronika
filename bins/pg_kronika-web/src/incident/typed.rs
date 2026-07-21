//! Typed counter, gauge, and snapshot evidence forwarded to lenses.
//!
//! Anomaly episodes only locate an incident; a finding needs the underlying
//! values. This holds the per-column counter diffs the reader already
//! folded, keyed by series, so a lens reports measured numbers and excludes
//! intervals the reader marked unusable (reset, gap, first point, timestamp
//! anomaly, or a disabled source).
//!
//! Gauges are instantaneous levels, so they are indexed without differencing.
//! It also carries the multi-row snapshots used by activity and lock lenses:
//! `pg_stat_activity` backends and `pg_locks`
//! blocking edges captured at each collection time. A snapshot is a moment, not
//! a rated timeline, so a lens over it reports a sampled observation, never a
//! direction inferred from a trend.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kronika_analytics::DiffPoint;

use super::evidence::{
    SourceWindow, observed_period_from_durations, observed_period_from_timestamps,
};
use super::model::IdentityValue;

const MAX_EXACT_F64_INTEGER_I128: i128 = 1_i128 << 53;
const MAX_EXACT_F64_INTEGER: f64 = 9_007_199_254_740_992.0;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TrackKey {
    section: &'static str,
    column: &'static str,
    identity: Arc<[IdentityValue]>,
}

/// One cumulative column's folded diffs for one series, in snapshot-time order.
struct CounterTrack {
    points: Vec<CounterPoint>,
}

#[derive(Clone, Copy)]
struct CounterPoint {
    end_us: i64,
    point: DiffPoint,
}

#[derive(Clone, Copy)]
struct CounterDelta {
    value: f64,
    integer: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CounterDeltaError {
    Unusable,
    NumericLimit,
}

impl CounterPoint {
    /// Return a usable delta or classify why it cannot enter numeric evidence.
    #[allow(
        clippy::cast_precision_loss,
        reason = "incident ratios are f64 values; the diff layer keeps integer subtraction exact"
    )]
    fn delta(self) -> Result<CounterDelta, CounterDeltaError> {
        match self.point {
            DiffPoint::Value {
                delta: kronika_analytics::Scalar::Int(delta),
                ..
            } if (0..=MAX_EXACT_F64_INTEGER_I128).contains(&delta) => Ok(CounterDelta {
                value: delta as f64,
                integer: true,
            }),
            DiffPoint::Value {
                delta: kronika_analytics::Scalar::Int(delta),
                ..
            } if delta > MAX_EXACT_F64_INTEGER_I128 => Err(CounterDeltaError::NumericLimit),
            DiffPoint::Value {
                delta: kronika_analytics::Scalar::Float(delta),
                ..
            } if delta.is_finite() && delta >= 0.0 => Ok(CounterDelta {
                value: delta,
                integer: false,
            }),
            DiffPoint::Value { .. } | DiffPoint::NoData { .. } => Err(CounterDeltaError::Unusable),
        }
    }

    fn interval_us(self) -> Option<u64> {
        match self.point {
            DiffPoint::Value { dt_micros, .. } => {
                u64::try_from(dt_micros).ok().filter(|dt| *dt > 0)
            }
            DiffPoint::NoData { .. } => None,
        }
    }
}

/// Sum of two columns' deltas over the intervals where both are usable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PairedSums {
    pub sum_a: f64,
    pub sum_b: f64,
    /// Number of intervals both columns contributed to.
    pub intervals: usize,
    /// Distinct interval endpoints seen in either column inside the window.
    pub candidate_intervals: usize,
    pub unmatched_endpoint_intervals: usize,
    pub unusable_delta_intervals: usize,
    pub unaligned_duration_intervals: usize,
    pub numeric_limit_intervals: usize,
    pub first_start_us: Option<i64>,
    pub first_end_us: Option<i64>,
    pub last_end_us: Option<i64>,
    /// Sum of the usable, aligned interval durations.
    pub elapsed_us: u64,
    /// Median of the usable interval durations; `None` when too few intervals
    /// pin a stable source cadence.
    pub observed_period_us: Option<u64>,
}

impl PairedSums {
    /// Require enough usable pairs and at least 70% of the endpoints observed
    /// in either operand. This checks pairing, not expected source coverage.
    pub(crate) const fn meets_pairing_coverage(self, minimum_intervals: usize) -> bool {
        self.intervals >= minimum_intervals
            && self.candidate_intervals > 0
            && (self.intervals as u128) * 10 >= (self.candidate_intervals as u128) * 7
    }

    pub(crate) const fn excluded_intervals(self) -> usize {
        self.candidate_intervals.saturating_sub(self.intervals)
    }
}

#[derive(Clone, Copy)]
enum PairExclusion {
    UnusableDelta,
    UnalignedDuration,
    NumericLimit,
}

/// On success returns the accepted interval's duration in microseconds, so the
/// caller can gather durations for the observed-period median.
fn accumulate_counter_pair(
    sums: &mut PairedSums,
    first_sum_integer: &mut bool,
    second_sum_integer: &mut bool,
    first: CounterPoint,
    second: CounterPoint,
) -> Result<u64, PairExclusion> {
    let (first_delta, second_delta) = match (first.delta(), second.delta()) {
        (Ok(first_delta), Ok(second_delta)) => (first_delta, second_delta),
        (Err(CounterDeltaError::NumericLimit), _) | (_, Err(CounterDeltaError::NumericLimit)) => {
            return Err(PairExclusion::NumericLimit);
        }
        (Err(CounterDeltaError::Unusable), _) | (_, Err(CounterDeltaError::Unusable)) => {
            return Err(PairExclusion::UnusableDelta);
        }
    };
    let (Some(first_interval_us), Some(second_interval_us)) =
        (first.interval_us(), second.interval_us())
    else {
        return Err(PairExclusion::UnalignedDuration);
    };
    if first_interval_us != second_interval_us {
        return Err(PairExclusion::UnalignedDuration);
    }
    let interval_i64 =
        i64::try_from(first_interval_us).map_err(|_conversion| PairExclusion::UnalignedDuration)?;
    let interval_start_us = first
        .end_us
        .checked_sub(interval_i64)
        .ok_or(PairExclusion::UnalignedDuration)?;
    let next_first_sum = sums.sum_a + first_delta.value;
    let next_second_sum = sums.sum_b + second_delta.value;
    let first_sum_exact = !*first_sum_integer
        || !first_delta.integer
        || sums.sum_a <= MAX_EXACT_F64_INTEGER - first_delta.value;
    let second_sum_exact = !*second_sum_integer
        || !second_delta.integer
        || sums.sum_b <= MAX_EXACT_F64_INTEGER - second_delta.value;
    let elapsed_us = sums
        .elapsed_us
        .checked_add(first_interval_us)
        .ok_or(PairExclusion::NumericLimit)?;
    if !next_first_sum.is_finite()
        || !next_second_sum.is_finite()
        || !first_sum_exact
        || !second_sum_exact
    {
        return Err(PairExclusion::NumericLimit);
    }
    sums.sum_a = next_first_sum;
    sums.sum_b = next_second_sum;
    sums.intervals = sums.intervals.saturating_add(1);
    sums.first_start_us.get_or_insert(interval_start_us);
    sums.first_end_us.get_or_insert(first.end_us);
    sums.last_end_us = Some(first.end_us);
    sums.elapsed_us = elapsed_us;
    *first_sum_integer &= first_delta.integer;
    *second_sum_integer &= second_delta.integer;
    Ok(first_interval_us)
}

struct AlignedInterval {
    deltas: [f64; 16],
    integer_deltas: [bool; 16],
    duration_us: u64,
}

fn aligned_interval(tracks: &[&CounterTrack], indexes: &[usize]) -> Option<AlignedInterval> {
    let mut deltas = [0.0_f64; 16];
    let mut integer_deltas = [false; 16];
    let mut duration_us = None;
    for ((slot, integer_delta), (track, &index)) in deltas
        .iter_mut()
        .zip(&mut integer_deltas)
        .zip(tracks.iter().zip(indexes))
    {
        let point = track.points[index];
        let delta = point.delta().ok()?;
        let point_duration_us = point.interval_us()?;
        let point_duration_i64 = i64::try_from(point_duration_us).ok()?;
        point.end_us.checked_sub(point_duration_i64)?;
        if duration_us.is_some_and(|expected| expected != point_duration_us) {
            return None;
        }
        duration_us = Some(point_duration_us);
        *slot = delta.value;
        *integer_delta = delta.integer;
    }
    Some(AlignedInterval {
        deltas,
        integer_deltas,
        duration_us: duration_us?,
    })
}

/// On success returns the accepted interval's duration in microseconds, for the
/// observed-period median; `None` when the interval was skipped.
fn accumulate_aligned_interval(
    result: &mut AlignedSums,
    integer_sums: &mut [bool; 16],
    interval: &AlignedInterval,
    end_us: i64,
) -> Option<u64> {
    let next_elapsed_us = result.elapsed_us.checked_add(interval.duration_us)?;
    let sums_are_usable = result
        .sums
        .iter()
        .zip(interval.deltas)
        .zip(integer_sums.iter().zip(interval.integer_deltas))
        .take(result.len)
        .all(|((&sum, delta), (&integer_sum, integer_delta))| {
            let next = sum + delta;
            next.is_finite()
                && (!integer_sum || !integer_delta || sum <= MAX_EXACT_F64_INTEGER - delta)
        });
    if !sums_are_usable {
        return None;
    }
    for ((sum, integer_sum), (delta, integer_delta)) in result
        .sums
        .iter_mut()
        .zip(integer_sums)
        .zip(interval.deltas.into_iter().zip(interval.integer_deltas))
        .take(result.len)
    {
        *sum += delta;
        *integer_sum &= integer_delta;
    }
    result.intervals = result.intervals.saturating_add(1);
    result.last_end_us = end_us;
    result.elapsed_us = next_elapsed_us;
    Some(interval.duration_us)
}

/// Sums for a fixed set of cumulative columns over exactly the same usable
/// intervals. Lenses use `len` entries of `sums`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AlignedSums {
    pub sums: [f64; 16],
    pub len: usize,
    pub intervals: usize,
    pub last_end_us: i64,
    pub elapsed_us: u64,
    /// Incident-window coverage: span, observed cadence, and usable intervals.
    pub source_window: SourceWindow,
}

/// One privacy-reduced `pg_store_plans` row. Plan text and names never enter
/// incident input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlanSample {
    pub ts: i64,
    pub fork: PlanFork,
    pub queryid: i64,
    pub planid: i64,
    pub userid: u64,
    pub dbid: u64,
    pub calls: f64,
    pub total_time_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PlanFork {
    Ossc,
    Vadv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessCgroupSample {
    pub ts: i64,
    pub pid: i64,
    pub starttime: i64,
    pub cgroup_path: Box<str>,
}

/// One gauge column's raw readings for one series, in snapshot-time order.
struct GaugeTrack {
    points: Vec<GaugePoint>,
    breaks: Vec<i64>,
    shared_breaks: Arc<[i64]>,
    gaps: Arc<[GaugeGap]>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GaugePoint {
    ts: i64,
    value: f64,
}

#[derive(Clone, Copy)]
struct GaugeGap {
    from: i64,
    to: i64,
}

/// Section-level coverage gaps shared by tracks built from one reader page.
#[derive(Clone)]
pub(crate) struct GaugeQuality {
    gaps: Arc<[GaugeGap]>,
}

pub(crate) struct GaugeTrackInput {
    pub section: &'static str,
    pub column: &'static str,
    pub identity: Arc<[IdentityValue]>,
    pub raw_points: Vec<(i64, f64)>,
    pub breaks: Vec<i64>,
    pub shared_breaks: Arc<[i64]>,
}

impl GaugeQuality {
    pub(crate) fn new(gaps: &[(i64, i64)]) -> Self {
        Self {
            gaps: Arc::from(normalize_gaps(gaps)),
        }
    }
}

type GaugeEntityTracks = BTreeMap<Arc<[IdentityValue]>, GaugeTrack>;
type GaugeTracks = BTreeMap<(&'static str, &'static str), GaugeEntityTracks>;

/// A single gauge column reduced to one worst-case level over a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GaugeReading {
    pub value: f64,
    pub observed_at_us: i64,
    /// Valid readings that fell inside the window.
    pub samples: usize,
    pub source_window: SourceWindow,
}

/// Two gauge columns of one series at the shared timestamp that is worst for a
/// [`GaugeObjective`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PairedGauge {
    pub a: f64,
    pub b: f64,
    pub observed_at_us: i64,
    /// Shared timestamps where both readings were valid and the objective was
    /// finite.
    pub samples: usize,
    pub source_window: SourceWindow,
}

/// How a lens combines two same-timestamp gauge readings and which extreme is
/// the worst case. A reading whose score is non-finite (a guarded denominator)
/// is skipped, so a lens never divides by zero.
#[derive(Clone, Copy)]
pub(crate) enum GaugeObjective {
    /// `a / (a + b)`, worst = max. A saturation share in `[0, 1]`; skips
    /// readings where `a + b <= 0`.
    ShareMax,
    /// `a / b`, worst = max. Skips readings where `b <= 0`.
    RatioMax,
    /// `a / b`, worst = min. Skips readings where `b <= 0`.
    RatioMin,
    /// `a - b`, worst = max. A byte gap between two positions.
    DiffMax,
    /// `a / max(abs(b), 1)`, worst = max.
    RatioAbsOneMax,
}

impl GaugeObjective {
    fn score(self, a: f64, b: f64) -> f64 {
        match self {
            Self::ShareMax => {
                let total = a + b;
                if total > 0.0 { a / total } else { f64::NAN }
            }
            Self::RatioMax | Self::RatioMin => {
                if b > 0.0 {
                    a / b
                } else {
                    f64::NAN
                }
            }
            Self::DiffMax => a - b,
            Self::RatioAbsOneMax => a / b.abs().max(1.0),
        }
    }

    const fn maximizes(self) -> bool {
        matches!(
            self,
            Self::ShareMax | Self::RatioMax | Self::DiffMax | Self::RatioAbsOneMax
        )
    }
}

/// Observed source cadence over a gauge window from its own sample timestamps.
fn gauge_source_window(
    from_us: i64,
    to_us: i64,
    sample_ts: &[i64],
    samples: usize,
) -> SourceWindow {
    // Completeness compares intervals to intervals: N gauge samples span N-1
    // sampling intervals, the same unit as expected_interval_count.
    let usable_intervals = samples.saturating_sub(1);
    SourceWindow::from_bounds(
        from_us,
        to_us,
        observed_period_from_timestamps(sample_ts),
        usable_intervals,
    )
}

pub(crate) struct GaugeWindow<'a> {
    points: &'a [GaugePoint],
    from_us: i64,
    to_us: i64,
}

impl GaugeWindow<'_> {
    pub(crate) const fn inspected_points(&self) -> usize {
        self.points.len()
    }

    fn source_window(&self) -> SourceWindow {
        let sample_ts: Vec<i64> = self.points.iter().map(|point| point.ts).collect();
        gauge_source_window(self.from_us, self.to_us, &sample_ts, self.points.len())
    }

    pub(crate) fn max(&self) -> Option<GaugeReading> {
        let mut best: Option<GaugePoint> = None;
        for &point in self.points {
            if best.is_none_or(|current| point.value > current.value) {
                best = Some(point);
            }
        }
        best.map(|point| GaugeReading {
            value: point.value,
            observed_at_us: point.ts,
            samples: self.points.len(),
            source_window: self.source_window(),
        })
    }
}

pub(crate) struct PairedGaugeWindow<'a> {
    a: &'a [GaugePoint],
    b: &'a [GaugePoint],
    from_us: i64,
    to_us: i64,
}

impl PairedGaugeWindow<'_> {
    pub(crate) const fn inspected_points(&self) -> usize {
        self.a.len().saturating_add(self.b.len())
    }

    pub(crate) fn reduce(&self, objective: GaugeObjective) -> Option<PairedGauge> {
        let mut best: Option<(f64, GaugePoint, GaugePoint)> = None;
        let mut shared_ts: Vec<i64> = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < self.a.len() && j < self.b.len() {
            let a = self.a[i];
            let b = self.b[j];
            match a.ts.cmp(&b.ts) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    let score = objective.score(a.value, b.value);
                    if score.is_finite() {
                        shared_ts.push(a.ts);
                        let improves = best.is_none_or(|(best_score, _, _)| {
                            if objective.maximizes() {
                                score > best_score
                            } else {
                                score < best_score
                            }
                        });
                        if improves {
                            best = Some((score, a, b));
                        }
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        best.map(|(_, a, b)| PairedGauge {
            a: a.value,
            b: b.value,
            observed_at_us: a.ts,
            samples: shared_ts.len(),
            source_window: gauge_source_window(
                self.from_us,
                self.to_us,
                &shared_ts,
                shared_ts.len(),
            ),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TripleGauge {
    pub a: f64,
    pub b: f64,
    pub denominator: f64,
    pub observed_at_us: i64,
    pub samples: usize,
    pub source_window: SourceWindow,
}

pub(crate) struct TripleGaugeWindow<'a> {
    a: &'a [GaugePoint],
    b: &'a [GaugePoint],
    denominator: &'a [GaugePoint],
    from_us: i64,
    to_us: i64,
}

/// Exact-timestamp readings for a small fixed lens contract.
pub(crate) struct GaugeSnapshotWindow<'a> {
    tracks: Vec<&'a [GaugePoint]>,
    from_us: i64,
    to_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GaugeSnapshot {
    pub values: [f64; 8],
    pub len: usize,
    pub observed_at_us: i64,
    pub samples: usize,
    pub source_window: SourceWindow,
}

impl GaugeSnapshotWindow<'_> {
    pub(crate) fn inspected_points(&self) -> usize {
        self.tracks
            .iter()
            .fold(0_usize, |total, points| total.saturating_add(points.len()))
    }

    pub(crate) fn extreme(&self, score_index: usize, maximize: bool) -> Option<GaugeSnapshot> {
        if self.tracks.is_empty() || self.tracks.len() > 8 || score_index >= self.tracks.len() {
            return None;
        }
        let mut indexes = vec![0_usize; self.tracks.len()];
        let mut best: Option<GaugeSnapshot> = None;
        let mut shared_ts: Vec<i64> = Vec::new();
        loop {
            let mut min_ts = i64::MAX;
            let mut max_ts = i64::MIN;
            for (track, &index) in self.tracks.iter().zip(&indexes) {
                let Some(point) = track.get(index) else {
                    let source_window =
                        gauge_source_window(self.from_us, self.to_us, &shared_ts, shared_ts.len());
                    return best.map(|mut reading| {
                        reading.samples = shared_ts.len();
                        reading.source_window = source_window;
                        reading
                    });
                };
                min_ts = min_ts.min(point.ts);
                max_ts = max_ts.max(point.ts);
            }
            if min_ts != max_ts {
                for (track, index) in self.tracks.iter().zip(&mut indexes) {
                    if track[*index].ts == min_ts {
                        *index += 1;
                    }
                }
                continue;
            }
            let mut values = [0.0_f64; 8];
            for (slot, (track, index)) in values.iter_mut().zip(self.tracks.iter().zip(&indexes)) {
                *slot = track[*index].value;
            }
            shared_ts.push(min_ts);
            let candidate = GaugeSnapshot {
                values,
                len: self.tracks.len(),
                observed_at_us: min_ts,
                samples: shared_ts.len(),
                source_window: gauge_source_window(
                    self.from_us,
                    self.to_us,
                    &shared_ts,
                    shared_ts.len(),
                ),
            };
            let score = candidate.values[score_index];
            if best.is_none_or(|current| {
                if maximize {
                    score > current.values[score_index]
                } else {
                    score < current.values[score_index]
                }
            }) {
                best = Some(candidate);
            }
            for index in &mut indexes {
                *index += 1;
            }
        }
    }

    pub(crate) fn value_range(&self, index: usize) -> Option<(f64, f64, usize)> {
        let minimum = self.extreme(index, false)?;
        let maximum = self.extreme(index, true)?;
        (minimum.samples == maximum.samples).then_some((
            minimum.values[index],
            maximum.values[index],
            minimum.samples,
        ))
    }
}

/// Gap-free change between the first and last valid sample in a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GaugeTrend {
    pub first: f64,
    pub last: f64,
    pub first_at_us: i64,
    pub last_at_us: i64,
    pub samples: usize,
    pub source_window: SourceWindow,
}

impl GaugeWindow<'_> {
    pub(crate) fn trend(&self) -> Option<GaugeTrend> {
        let first = *self.points.first()?;
        let last = *self.points.last()?;
        (self.points.len() >= 2 && first.ts < last.ts).then_some(GaugeTrend {
            first: first.value,
            last: last.value,
            first_at_us: first.ts,
            last_at_us: last.ts,
            samples: self.points.len(),
            source_window: self.source_window(),
        })
    }
}

impl TripleGaugeWindow<'_> {
    pub(crate) const fn inspected_points(&self) -> usize {
        self.a
            .len()
            .saturating_add(self.b.len())
            .saturating_add(self.denominator.len())
    }

    pub(crate) fn sum_ratio_max(&self) -> Option<TripleGauge> {
        let mut best: Option<(f64, GaugePoint, GaugePoint, GaugePoint)> = None;
        let mut shared_ts: Vec<i64> = Vec::new();
        let (mut a_index, mut b_index, mut denominator_index) = (0, 0, 0);
        while a_index < self.a.len()
            && b_index < self.b.len()
            && denominator_index < self.denominator.len()
        {
            let first = self.a[a_index];
            let second = self.b[b_index];
            let denominator = self.denominator[denominator_index];
            let min_ts = first.ts.min(second.ts).min(denominator.ts);
            let max_ts = first.ts.max(second.ts).max(denominator.ts);
            if min_ts != max_ts {
                if first.ts == min_ts {
                    a_index += 1;
                }
                if second.ts == min_ts {
                    b_index += 1;
                }
                if denominator.ts == min_ts {
                    denominator_index += 1;
                }
                continue;
            }
            if denominator.value > 0.0 {
                let score = (first.value + second.value) / denominator.value;
                if score.is_finite() {
                    shared_ts.push(first.ts);
                    if best.is_none_or(|(best_score, _, _, _)| score > best_score) {
                        best = Some((score, first, second, denominator));
                    }
                }
            }
            a_index += 1;
            b_index += 1;
            denominator_index += 1;
        }
        best.map(|(_, a, b, denominator)| TripleGauge {
            a: a.value,
            b: b.value,
            denominator: denominator.value,
            observed_at_us: a.ts,
            samples: shared_ts.len(),
            source_window: gauge_source_window(
                self.from_us,
                self.to_us,
                &shared_ts,
                shared_ts.len(),
            ),
        })
    }
}

/// One `pg_stat_activity` backend, reduced to the fields the sampled activity
/// lenses read. Text labels are resolved dictionary strings; `None` mirrors the
/// view's own `NULL` — a background backend, a backend that is not waiting, or
/// one with no assigned xmin.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActivityBackend {
    /// Stable session identity together with `backend_start`.
    pub pid: i64,
    /// `PostgreSQL` backend start, Unix microseconds.
    pub backend_start: i64,
    /// `age(backend_xid)` in transactions when assigned.
    pub xid_age: Option<i64>,
    /// `age(backend_xmin)` in transactions; `Some` only while the backend pins
    /// the vacuum horizon.
    pub xmin_age: Option<i64>,
    /// Session state (`active`, `idle in transaction`, …).
    pub state: Option<Box<str>>,
    /// Wait-event class (`LWLock`, `IO`, `BufferPin`, …); `None` when the
    /// backend is not waiting.
    pub wait_event_type: Option<Box<str>>,
    /// Wait-event name (`SyncRep`, …); `None` when the backend is not waiting.
    pub wait_event: Option<Box<str>>,
    /// Open-transaction age at snapshot time, microseconds; `None` outside a
    /// transaction or when the clock disagrees (`xact_start` after the snapshot).
    pub xact_age_us: Option<i64>,
}

/// Provenance of one stored multi-row snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotCompleteness {
    /// The source read completed and the collector could see all activity fields.
    Complete,
    /// The read completed, but activity fields for other sessions may be NULL.
    Restricted,
    /// No valid marker exists (including old layouts and conflicting markers).
    Unknown,
}

impl SnapshotCompleteness {
    pub(crate) const fn denominator_usable(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// A `pg_stat_activity` snapshot: the backends captured at one collection time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActivitySnapshot {
    pub ts: i64,
    pub backends: Vec<ActivityBackend>,
    pub completeness: SnapshotCompleteness,
}

/// One directed blocking edge from a `pg_locks` snapshot: `waiter_pid` waits on
/// `blocker_pid`. A `blocker_pid` of `0` is a prepared-transaction holder with
/// no live backend (`pg_blocking_pids` reports it as `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LockEdge {
    pub waiter_pid: i64,
    pub blocker_pid: i64,
}

/// A `pg_locks` snapshot: the blocking edges captured at one collection time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LockSnapshot {
    pub ts: i64,
    pub edges: Vec<LockEdge>,
}

/// Typed evidence for one analysis request: counter series and instantaneous
/// gauge series keyed by `(section, column, identity)`, plus the activity and
/// lock snapshots the sampled lenses read, each list in ascending collection
/// time.
pub(crate) struct TypedInputs {
    counters: BTreeMap<TrackKey, CounterTrack>,
    gauges: GaugeTracks,
    activity: Vec<ActivitySnapshot>,
    locks: Vec<LockSnapshot>,
    plans: Vec<PlanSample>,
    process_cgroups: BTreeMap<(i64, i64, i64), Box<str>>,
    cgroup_devices: BTreeMap<Box<str>, Vec<Arc<[IdentityValue]>>>,
    postgres_storage_devices: BTreeSet<(i64, i64)>,
}

impl TypedInputs {
    pub(crate) const fn new() -> Self {
        Self {
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
            activity: Vec::new(),
            locks: Vec::new(),
            plans: Vec::new(),
            process_cgroups: BTreeMap::new(),
            cgroup_devices: BTreeMap::new(),
            postgres_storage_devices: BTreeSet::new(),
        }
    }

    pub(crate) fn insert_counter(
        &mut self,
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
        points: Vec<(i64, DiffPoint)>,
    ) {
        let points = points
            .into_iter()
            .map(|(end_us, point)| CounterPoint { end_us, point })
            .collect();
        if section == "os_cgroup_io"
            && column == "rbytes"
            && let [IdentityValue::Text(path), ..] = identity.as_ref()
        {
            let entities = self.cgroup_devices.entry(path.clone().into()).or_default();
            if !entities.contains(&identity) {
                entities.push(Arc::clone(&identity));
            }
        }
        self.counters.insert(
            TrackKey {
                section,
                column,
                identity,
            },
            CounterTrack { points },
        );
    }

    fn counter(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
    ) -> Option<&CounterTrack> {
        self.counters.get(&TrackKey {
            section,
            column,
            identity: Arc::from(identity),
        })
    }

    pub(crate) fn counter_identity_count(
        &self,
        section: &'static str,
        column: &'static str,
    ) -> usize {
        self.counters
            .keys()
            .filter(|key| key.section == section && key.column == column)
            .count()
    }

    pub(crate) fn has_counter(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
    ) -> bool {
        self.counter(section, column, identity).is_some()
    }

    pub(crate) fn counter_identities(
        &self,
        section: &'static str,
        column: &'static str,
    ) -> Vec<Arc<[IdentityValue]>> {
        self.counters
            .keys()
            .filter(|key| key.section == section && key.column == column)
            .map(|key| Arc::clone(&key.identity))
            .collect()
    }

    /// Sum `column_a` and `column_b` deltas of one series inside
    /// `[from_us, to_us]`, over intervals where both columns carry a usable
    /// value and share a snapshot time.
    ///
    /// Returns `None` when either column is absent for the series;
    /// `intervals == 0` means the columns share no usable interval in the
    /// requested window.
    pub(crate) fn paired_delta_sums(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        column_a: &'static str,
        column_b: &'static str,
        from_us: i64,
        to_us: i64,
    ) -> Option<PairedSums> {
        let track_a = self.counter(section, column_a, identity)?;
        let track_b = self.counter(section, column_b, identity)?;

        let mut sums = PairedSums {
            sum_a: 0.0,
            sum_b: 0.0,
            intervals: 0,
            candidate_intervals: 0,
            unmatched_endpoint_intervals: 0,
            unusable_delta_intervals: 0,
            unaligned_duration_intervals: 0,
            numeric_limit_intervals: 0,
            first_start_us: None,
            first_end_us: None,
            last_end_us: None,
            elapsed_us: 0,
            observed_period_us: None,
        };
        let mut durations_us: Vec<u64> = Vec::new();
        let (mut first_sum_integer, mut second_sum_integer) = (true, true);
        let (mut i, mut j) = (0, 0);
        while i < track_a.points.len() && j < track_b.points.len() {
            let a = track_a.points[i];
            let b = track_b.points[j];
            match a.end_us.cmp(&b.end_us) {
                std::cmp::Ordering::Less => {
                    if (from_us..=to_us).contains(&a.end_us) {
                        sums.candidate_intervals = sums.candidate_intervals.saturating_add(1);
                        sums.unmatched_endpoint_intervals =
                            sums.unmatched_endpoint_intervals.saturating_add(1);
                    }
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    if (from_us..=to_us).contains(&b.end_us) {
                        sums.candidate_intervals = sums.candidate_intervals.saturating_add(1);
                        sums.unmatched_endpoint_intervals =
                            sums.unmatched_endpoint_intervals.saturating_add(1);
                    }
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    if (from_us..=to_us).contains(&a.end_us) {
                        sums.candidate_intervals = sums.candidate_intervals.saturating_add(1);
                        match accumulate_counter_pair(
                            &mut sums,
                            &mut first_sum_integer,
                            &mut second_sum_integer,
                            a,
                            b,
                        ) {
                            Ok(interval_us) => durations_us.push(interval_us),
                            Err(PairExclusion::UnusableDelta) => {
                                sums.unusable_delta_intervals =
                                    sums.unusable_delta_intervals.saturating_add(1);
                            }
                            Err(PairExclusion::UnalignedDuration) => {
                                sums.unaligned_duration_intervals =
                                    sums.unaligned_duration_intervals.saturating_add(1);
                            }
                            Err(PairExclusion::NumericLimit) => {
                                sums.numeric_limit_intervals =
                                    sums.numeric_limit_intervals.saturating_add(1);
                            }
                        }
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        while i < track_a.points.len() {
            let end_us = track_a.points[i].end_us;
            if (from_us..=to_us).contains(&end_us) {
                sums.candidate_intervals = sums.candidate_intervals.saturating_add(1);
                sums.unmatched_endpoint_intervals =
                    sums.unmatched_endpoint_intervals.saturating_add(1);
            }
            i += 1;
        }
        while j < track_b.points.len() {
            let end_us = track_b.points[j].end_us;
            if (from_us..=to_us).contains(&end_us) {
                sums.candidate_intervals = sums.candidate_intervals.saturating_add(1);
                sums.unmatched_endpoint_intervals =
                    sums.unmatched_endpoint_intervals.saturating_add(1);
            }
            j += 1;
        }
        sums.observed_period_us = observed_period_from_durations(&mut durations_us);
        Some(sums)
    }

    pub(crate) fn aligned_counter_points(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: &[&'static str],
    ) -> usize {
        columns.iter().fold(0_usize, |total, column| {
            total.saturating_add(
                self.counter(section, column, identity)
                    .map_or(0, |track| track.points.len()),
            )
        })
    }

    /// Sum at most 16 columns over the intersection of their usable interval
    /// endpoints. A reset, gate, gap, or missing point removes the whole
    /// interval from every operand.
    pub(crate) fn aligned_delta_sums(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: &[&'static str],
        from_us: i64,
        to_us: i64,
    ) -> Option<AlignedSums> {
        if columns.is_empty() || columns.len() > 16 || from_us > to_us {
            return None;
        }
        let tracks: Vec<&CounterTrack> = columns
            .iter()
            .map(|column| self.counter(section, column, identity))
            .collect::<Option<_>>()?;
        let mut indexes = vec![0_usize; tracks.len()];
        let mut integer_sums = [true; 16];
        let mut result = AlignedSums {
            sums: [0.0; 16],
            len: tracks.len(),
            intervals: 0,
            last_end_us: from_us,
            elapsed_us: 0,
            source_window: SourceWindow::from_bounds(from_us, to_us, None, 0),
        };
        let mut durations_us: Vec<u64> = Vec::new();
        loop {
            let mut minimum = i64::MAX;
            let mut maximum = i64::MIN;
            for (track, &index) in tracks.iter().zip(&indexes) {
                let Some(point) = track.points.get(index) else {
                    result.source_window = SourceWindow::from_bounds(
                        from_us,
                        to_us,
                        observed_period_from_durations(&mut durations_us),
                        result.intervals,
                    );
                    return Some(result);
                };
                minimum = minimum.min(point.end_us);
                maximum = maximum.max(point.end_us);
            }
            if minimum != maximum {
                for (track, index) in tracks.iter().zip(&mut indexes) {
                    if track.points[*index].end_us == minimum {
                        *index += 1;
                    }
                }
                continue;
            }
            if (from_us..=to_us).contains(&minimum)
                && let Some(interval) = aligned_interval(&tracks, &indexes)
                && let Some(duration_us) =
                    accumulate_aligned_interval(&mut result, &mut integer_sums, &interval, minimum)
            {
                durations_us.push(duration_us);
            }
            for index in &mut indexes {
                *index += 1;
            }
        }
    }

    pub(crate) fn insert_plan_samples(&mut self, mut samples: Vec<PlanSample>) {
        self.plans.append(&mut samples);
        self.plans.sort_by_key(|sample| {
            (
                sample.ts,
                sample.fork,
                sample.dbid,
                sample.userid,
                sample.queryid,
                sample.planid,
            )
        });
    }

    pub(crate) fn plan_window(&self, from_us: i64, to_us: i64) -> &[PlanSample] {
        let start = self.plans.partition_point(|sample| sample.ts < from_us);
        let end = self.plans.partition_point(|sample| sample.ts <= to_us);
        &self.plans[start..end]
    }

    pub(crate) fn process_is_postgres_backend(
        &self,
        pid: i64,
        starttime: i64,
        from_us: i64,
        to_us: i64,
    ) -> bool {
        self.activity_window(from_us, to_us).any(|snapshot| {
            snapshot
                .backends
                .iter()
                .any(|backend| backend.pid == pid && backend.backend_start == starttime)
        })
    }

    pub(crate) fn insert_process_cgroups(&mut self, samples: Vec<ProcessCgroupSample>) {
        for sample in samples {
            self.process_cgroups.insert(
                (sample.pid, sample.starttime, sample.ts),
                sample.cgroup_path,
            );
        }
    }

    pub(crate) fn process_cgroup_at(&self, pid: i64, starttime: i64, ts: i64) -> Option<&str> {
        self.process_cgroups
            .get(&(pid, starttime, ts))
            .map(AsRef::as_ref)
    }

    pub(crate) fn cgroup_devices(&self, cgroup_path: &str) -> &[Arc<[IdentityValue]>] {
        self.cgroup_devices
            .get(cgroup_path)
            .map_or(&[], Vec::as_slice)
    }

    pub(crate) fn insert_postgres_storage_device(&mut self, major: i64, minor: i64) {
        if major > 0 && minor >= 0 {
            self.postgres_storage_devices.insert((major, minor));
        }
    }

    pub(crate) fn is_postgres_storage_device(&self, major: i64, minor: i64) -> bool {
        self.postgres_storage_devices.contains(&(major, minor))
    }

    pub(crate) fn insert_gauge(
        &mut self,
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
        points: Vec<(i64, f64)>,
    ) {
        self.insert_gauge_with_quality(section, column, identity, points, Vec::new(), &[]);
    }

    pub(crate) fn insert_gauge_with_quality(
        &mut self,
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
        raw_points: Vec<(i64, f64)>,
        breaks: Vec<i64>,
        gaps: &[(i64, i64)],
    ) {
        self.insert_gauge_with_shared_quality(
            GaugeTrackInput {
                section,
                column,
                identity,
                raw_points,
                breaks,
                shared_breaks: Arc::from([]),
            },
            &GaugeQuality::new(gaps),
        );
    }

    pub(crate) fn insert_gauge_with_shared_quality(
        &mut self,
        input: GaugeTrackInput,
        quality: &GaugeQuality,
    ) {
        let GaugeTrackInput {
            section,
            column,
            identity,
            mut raw_points,
            mut breaks,
            mut shared_breaks,
        } = input;
        raw_points.sort_by_key(|point| point.0);
        let mut points = Vec::with_capacity(raw_points.len());
        for (ts, value) in raw_points {
            if !value.is_finite()
                || points
                    .last()
                    .is_some_and(|point: &GaugePoint| point.ts == ts)
            {
                breaks.push(ts);
                continue;
            }
            points.push(GaugePoint { ts, value });
        }
        breaks.sort_unstable();
        breaks.dedup();
        if !shared_breaks.windows(2).all(|pair| pair[0] < pair[1]) {
            let mut normalized = shared_breaks.to_vec();
            normalized.sort_unstable();
            normalized.dedup();
            shared_breaks = normalized.into();
        }
        self.gauges.entry((section, column)).or_default().insert(
            identity,
            GaugeTrack {
                points,
                breaks,
                shared_breaks,
                gaps: Arc::clone(&quality.gaps),
            },
        );
    }

    fn gauge(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
    ) -> Option<&GaugeTrack> {
        self.gauges.get(&(section, column))?.get(identity)
    }

    pub(crate) fn gauge_window(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
        from_us: i64,
        to_us: i64,
    ) -> Option<GaugeWindow<'_>> {
        if from_us >= to_us {
            return None;
        }
        let track = self.gauge(section, column, identity)?;
        if sorted_timestamp_in_window(&track.breaks, from_us, to_us)
            || sorted_timestamp_in_window(&track.shared_breaks, from_us, to_us)
            || sorted_gap_overlaps(&track.gaps, from_us, to_us)
        {
            return None;
        }
        let start = track.points.partition_point(|point| point.ts < from_us);
        let end = track.points.partition_point(|point| point.ts < to_us);
        (start < end).then_some(GaugeWindow {
            points: &track.points[start..end],
            from_us,
            to_us,
        })
    }

    #[cfg(test)]
    pub(crate) fn gauge_max(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
        from_us: i64,
        to_us: i64,
    ) -> Option<GaugeReading> {
        self.gauge_window(section, column, identity, from_us, to_us)?
            .max()
    }

    /// Reduce two gauge columns of one series to the shared-timestamp pair that
    /// is worst for `objective`, over `[from_us, to_us]`. `columns` is the
    /// `(a, b)` pair the objective scores as `score(a, b)`.
    ///
    /// Only timestamps where both columns carry a valid reading and the
    /// objective's score is finite contribute, so a guarded denominator drops
    /// the reading instead of dividing by zero. Returns `None` when either
    /// column is absent or no shared reading qualifies.
    pub(crate) fn paired_gauge_window(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: (&'static str, &'static str),
        from_us: i64,
        to_us: i64,
    ) -> Option<PairedGaugeWindow<'_>> {
        let (column_a, column_b) = columns;
        let a = self.gauge_window(section, column_a, identity, from_us, to_us)?;
        let b = self.gauge_window(section, column_b, identity, from_us, to_us)?;
        Some(PairedGaugeWindow {
            a: a.points,
            b: b.points,
            from_us,
            to_us,
        })
    }

    #[cfg(test)]
    pub(crate) fn paired_gauge(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: (&'static str, &'static str),
        from_us: i64,
        to_us: i64,
        objective: GaugeObjective,
    ) -> Option<PairedGauge> {
        self.paired_gauge_window(section, identity, columns, from_us, to_us)?
            .reduce(objective)
    }

    pub(crate) fn triple_gauge_window(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: (&'static str, &'static str, &'static str),
        from_us: i64,
        to_us: i64,
    ) -> Option<TripleGaugeWindow<'_>> {
        let a = self.gauge_window(section, columns.0, identity, from_us, to_us)?;
        let b = self.gauge_window(section, columns.1, identity, from_us, to_us)?;
        let denominator = self.gauge_window(section, columns.2, identity, from_us, to_us)?;
        Some(TripleGaugeWindow {
            a: a.points,
            b: b.points,
            denominator: denominator.points,
            from_us,
            to_us,
        })
    }

    pub(crate) fn gauge_snapshot_window(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: &[&'static str],
        from_us: i64,
        to_us: i64,
    ) -> Option<GaugeSnapshotWindow<'_>> {
        if columns.is_empty() || columns.len() > 8 {
            return None;
        }
        let mut tracks = Vec::with_capacity(columns.len());
        for &column in columns {
            tracks.push(
                self.gauge_window(section, column, identity, from_us, to_us)?
                    .points,
            );
        }
        Some(GaugeSnapshotWindow {
            tracks,
            from_us,
            to_us,
        })
    }

    /// Append one `pg_stat_activity` snapshot. The adapter feeds snapshots in
    /// ascending collection time; `activity_window` filters by that time and
    /// does not depend on the order.
    pub(crate) fn insert_activity_snapshot(&mut self, snapshot: ActivitySnapshot) {
        self.activity.push(snapshot);
    }

    /// Append one `pg_locks` snapshot, in ascending collection time.
    pub(crate) fn insert_lock_snapshot(&mut self, snapshot: LockSnapshot) {
        self.locks.push(snapshot);
    }

    /// Activity snapshots whose collection time lies inside `[from_us, to_us)`.
    pub(crate) fn activity_window(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> impl Iterator<Item = &ActivitySnapshot> {
        self.activity
            .iter()
            .filter(move |snapshot| (from_us..to_us).contains(&snapshot.ts))
    }

    /// Lock snapshots whose collection time lies inside `[from_us, to_us)`.
    pub(crate) fn lock_window(
        &self,
        from_us: i64,
        to_us: i64,
    ) -> impl Iterator<Item = &LockSnapshot> {
        self.locks
            .iter()
            .filter(move |snapshot| (from_us..to_us).contains(&snapshot.ts))
    }
}

fn normalize_gaps(gaps: &[(i64, i64)]) -> Vec<GaugeGap> {
    let mut gaps: Vec<GaugeGap> = gaps
        .iter()
        .filter_map(|&(from, to)| (from < to).then_some(GaugeGap { from, to }))
        .collect();
    gaps.sort_by_key(|gap| (gap.from, gap.to));
    let mut merged: Vec<GaugeGap> = Vec::with_capacity(gaps.len());
    for gap in gaps {
        if let Some(previous) = merged.last_mut()
            && gap.from <= previous.to
        {
            previous.to = previous.to.max(gap.to);
        } else {
            merged.push(gap);
        }
    }
    merged
}

fn sorted_timestamp_in_window(timestamps: &[i64], from_us: i64, to_us: i64) -> bool {
    let index = timestamps.partition_point(|&timestamp| timestamp < from_us);
    timestamps
        .get(index)
        .is_some_and(|&timestamp| timestamp < to_us)
}

fn sorted_gap_overlaps(gaps: &[GaugeGap], from_us: i64, to_us: i64) -> bool {
    let index = gaps.partition_point(|gap| gap.to <= from_us);
    gaps.get(index).is_some_and(|gap| gap.from < to_us)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kronika_analytics::{Reason, Scalar};

    fn id(value: i64) -> Arc<[IdentityValue]> {
        Arc::from(vec![IdentityValue::I64(value)])
    }

    // A one-second interval with the same delta and rate.
    fn value(delta: f64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Float(delta),
            rate: delta,
            dt_micros: 1_000_000,
        }
    }

    fn value_with_dt(delta: f64, dt_micros: i64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Float(delta),
            rate: delta,
            dt_micros,
        }
    }

    fn integer_value(delta: i128, dt_micros: i64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Int(delta),
            rate: 0.0,
            dt_micros,
        }
    }

    fn absent(reason: Reason) -> DiffPoint {
        DiffPoint::NoData { reason }
    }

    fn inputs(read: Vec<(i64, DiffPoint)>, hit: Vec<(i64, DiffPoint)>) -> TypedInputs {
        let mut typed = TypedInputs::new();
        typed.insert_counter("db", "blks_read", id(1), read);
        typed.insert_counter("db", "blks_hit", id(1), hit);
        typed
    }

    #[test]
    fn paired_sums_add_deltas_over_shared_intervals() {
        let typed = inputs(
            vec![(10, value(3.0)), (20, value(7.0))],
            vec![(10, value(30.0)), (20, value(70.0))],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", i64::MIN, i64::MAX)
            .expect("both columns present");
        assert_eq!(sums.intervals, 2);
        assert_eq!(sums.candidate_intervals, 2);
        assert_eq!(sums.first_start_us, Some(-999_990));
        assert_eq!(sums.first_end_us, Some(10));
        assert_eq!(sums.last_end_us, Some(20));
        assert_eq!(sums.elapsed_us, 2_000_000);
        assert!((sums.sum_a - 10.0).abs() < 1e-9);
        assert!((sums.sum_b - 100.0).abs() < 1e-9);
    }

    #[test]
    fn an_unusable_interval_drops_the_whole_pair() {
        let typed = inputs(
            vec![(10, value(3.0)), (20, absent(Reason::Reset))],
            vec![(10, value(30.0)), (20, value(70.0))],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", i64::MIN, i64::MAX)
            .expect("both columns present");
        assert_eq!(
            sums.intervals, 1,
            "the reset interval is excluded from both"
        );
        assert_eq!(sums.candidate_intervals, 2);
        assert_eq!(sums.excluded_intervals(), 1);
        assert_eq!(sums.unusable_delta_intervals, 1);
        assert!((sums.sum_a - 3.0).abs() < 1e-9);
        assert!((sums.sum_b - 30.0).abs() < 1e-9);
    }

    #[test]
    fn only_matching_snapshot_times_pair() {
        let typed = inputs(
            vec![(10, value(3.0)), (20, value(7.0))],
            vec![(20, value(70.0)), (30, value(90.0))],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", i64::MIN, i64::MAX)
            .expect("both columns present");
        assert_eq!(sums.intervals, 1, "only ts=20 is shared");
        assert_eq!(sums.candidate_intervals, 3);
        assert_eq!(sums.excluded_intervals(), 2);
        assert_eq!(sums.unmatched_endpoint_intervals, 2);
        assert!((sums.sum_a - 7.0).abs() < 1e-9);
        assert!((sums.sum_b - 70.0).abs() < 1e-9);
    }

    #[test]
    fn a_missing_column_yields_none() {
        let mut typed = TypedInputs::new();
        typed.insert_counter("db", "blks_read", id(1), vec![(10, value(3.0))]);
        assert!(
            typed
                .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", i64::MIN, i64::MAX)
                .is_none()
        );
    }

    #[test]
    fn a_reset_only_pair_reports_zero_usable_intervals() {
        let typed = inputs(vec![(10, absent(Reason::Reset))], vec![(10, value(30.0))]);
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", i64::MIN, i64::MAX)
            .expect("both columns present");
        assert_eq!(sums.intervals, 0);
        assert_eq!(sums.candidate_intervals, 1);
        assert_eq!(sums.unusable_delta_intervals, 1);
        assert!(sums.sum_a.abs() < 1e-9);
        assert!(sums.sum_b.abs() < 1e-9);
    }

    #[test]
    fn a_typed_delta_does_not_depend_on_the_derived_rate() {
        let nan = DiffPoint::Value {
            delta: Scalar::Int(5),
            rate: f64::NAN,
            dt_micros: 1_000_000,
        };
        let typed = inputs(
            vec![(10, nan), (20, value(7.0))],
            vec![(10, value(30.0)), (20, value(70.0))],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", i64::MIN, i64::MAX)
            .expect("both columns present");
        assert_eq!(sums.intervals, 2, "the exact delta remains usable");
        assert!((sums.sum_a - 12.0).abs() < 1e-9);
        assert!((sums.sum_b - 100.0).abs() < 1e-9);
    }

    #[test]
    fn paired_sums_only_count_intervals_ending_inside_the_window() {
        let typed = inputs(
            vec![
                (0, value(100.0)),
                (10, value(3.0)),
                (20, value(7.0)),
                (30, value(200.0)),
            ],
            vec![
                (0, value(10.0)),
                (10, value(30.0)),
                (20, value(70.0)),
                (30, value(20.0)),
            ],
        );

        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", 10, 20)
            .expect("both columns present");

        assert_eq!(sums.intervals, 2);
        assert_eq!(sums.candidate_intervals, 2);
        assert!((sums.sum_a - 10.0).abs() < 1e-9);
        assert!((sums.sum_b - 100.0).abs() < 1e-9);
    }

    #[test]
    fn mismatched_interval_lengths_are_excluded_and_counted() {
        let typed = inputs(
            vec![
                (1_000_000, value_with_dt(3.0, 1_000_000)),
                (2_000_000, value_with_dt(7.0, 1_000_000)),
                (3_000_000, value_with_dt(8.0, 1_000_000)),
            ],
            vec![
                (1_000_000, value_with_dt(30.0, 2_000_000)),
                (2_000_000, value_with_dt(70.0, 1_000_000)),
                (3_000_000, value_with_dt(80.0, 1_000_000)),
            ],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", 0, 3_000_000)
            .expect("both columns present");

        assert_eq!(sums.intervals, 2);
        assert_eq!(sums.candidate_intervals, 3);
        assert_eq!(sums.excluded_intervals(), 1);
        assert_eq!(sums.unaligned_duration_intervals, 1);
        assert_eq!(sums.elapsed_us, 2_000_000);
        assert_eq!(sums.first_start_us, Some(1_000_000));
        assert!(!sums.meets_pairing_coverage(2));
    }

    #[test]
    fn three_of_four_aligned_intervals_meet_pairing_coverage() {
        let typed = inputs(
            vec![
                (1, value(1.0)),
                (2, value(1.0)),
                (3, absent(Reason::Reset)),
                (4, value(1.0)),
            ],
            vec![
                (1, value(1.0)),
                (2, value(1.0)),
                (3, value(1.0)),
                (4, value(1.0)),
            ],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", 0, 10)
            .expect("both columns present");

        assert_eq!((sums.intervals, sums.candidate_intervals), (3, 4));
        assert!(sums.meets_pairing_coverage(3));
    }

    #[test]
    fn integer_deltas_that_cannot_be_published_exactly_are_excluded() {
        let too_large = (1_i128 << 53) + 1;
        let typed = inputs(
            vec![(1_000_000, integer_value(too_large, 1_000_000))],
            vec![(1_000_000, integer_value(1, 1_000_000))],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", 0, 1_000_000)
            .expect("both columns present");

        assert_eq!((sums.intervals, sums.candidate_intervals), (0, 1));
        assert_eq!(sums.numeric_limit_intervals, 1);
    }

    #[test]
    fn integer_sum_precision_loss_excludes_the_overflowing_pair() {
        let exact_limit = 1_i128 << 53;
        let typed = inputs(
            vec![
                (1_000_000, integer_value(exact_limit, 1_000_000)),
                (2_000_000, integer_value(1, 1_000_000)),
            ],
            vec![
                (1_000_000, integer_value(1, 1_000_000)),
                (2_000_000, integer_value(1, 1_000_000)),
            ],
        );
        let sums = typed
            .paired_delta_sums("db", &id(1), "blks_read", "blks_hit", 0, 2_000_000)
            .expect("both columns present");

        assert_eq!((sums.intervals, sums.candidate_intervals), (1, 2));
        assert_eq!(sums.numeric_limit_intervals, 1);
        assert!((sums.sum_a - 9_007_199_254_740_992.0).abs() < f64::EPSILON);
    }

    #[test]
    fn elapsed_duration_overflow_excludes_the_interval() {
        let endpoints = [i64::MAX - 2, i64::MAX - 1, i64::MAX];
        let points: Vec<_> = endpoints
            .into_iter()
            .map(|end| (end, value_with_dt(1.0, end)))
            .collect();
        let typed = inputs(points.clone(), points);
        let sums = typed
            .paired_delta_sums(
                "db",
                &id(1),
                "blks_read",
                "blks_hit",
                i64::MAX - 2,
                i64::MAX,
            )
            .expect("both columns present");

        assert_eq!((sums.intervals, sums.candidate_intervals), (2, 3));
        assert_eq!(sums.numeric_limit_intervals, 1);
        assert!(!sums.meets_pairing_coverage(2));
    }

    #[test]
    fn aligned_sums_exclude_an_interval_start_before_i64_min() {
        let typed = inputs(
            vec![(i64::MIN, value_with_dt(1.0, 1))],
            vec![(i64::MIN, value_with_dt(1.0, 1))],
        );
        let sums = typed
            .aligned_delta_sums("db", &id(1), &["blks_read", "blks_hit"], i64::MIN, i64::MIN)
            .expect("both columns present");

        assert_eq!(sums.intervals, 0);
    }

    fn gauges(section: &'static str, columns: &[(&'static str, &[(i64, f64)])]) -> TypedInputs {
        let mut typed = TypedInputs::new();
        for &(column, points) in columns {
            typed.insert_gauge(section, column, id(1), points.to_vec());
        }
        typed
    }

    #[test]
    fn gauge_max_reports_the_window_peak_and_sample_count() {
        let typed = gauges("db", &[("age", &[(10, 3.0), (20, 7.0), (30, 5.0)])]);
        let reading = typed
            .gauge_max("db", "age", &id(1), 10, 25)
            .expect("readings land in the window");
        assert!((reading.value - 7.0).abs() < 1e-9);
        assert_eq!(
            reading.samples, 2,
            "the ts=30 reading ends after the window"
        );
    }

    #[test]
    fn gauge_max_without_a_reading_in_the_window_is_none() {
        let typed = gauges("db", &[("age", &[(10, 3.0), (20, 7.0)])]);
        assert!(typed.gauge_max("db", "age", &id(1), 40, 50).is_none());
    }

    #[test]
    fn gauge_max_on_a_missing_column_is_none() {
        let typed = gauges("db", &[("age", &[(10, 3.0)])]);
        assert!(typed.gauge_max("db", "other", &id(1), 0, 100).is_none());
    }

    #[test]
    fn paired_gauge_share_max_selects_the_worst_saturation() {
        let typed = gauges(
            "tbl",
            &[
                ("dead", &[(10, 3.0), (20, 80.0)]),
                ("live", &[(10, 97.0), (20, 20.0)]),
            ],
        );
        let pair = typed
            .paired_gauge(
                "tbl",
                &id(1),
                ("dead", "live"),
                0,
                100,
                GaugeObjective::ShareMax,
            )
            .expect("both columns present");
        assert_eq!(pair.samples, 2);
        // 80 / (80 + 20) = 0.8, the worst share, beats 3 / 100 = 0.03.
        assert!((pair.a - 80.0).abs() < 1e-9 && (pair.b - 20.0).abs() < 1e-9);
    }

    #[test]
    fn paired_gauge_ratio_min_selects_the_lowest_headroom() {
        let typed = gauges(
            "fs",
            &[
                ("free", &[(10, 50.0), (20, 5.0)]),
                ("total", &[(10, 100.0), (20, 100.0)]),
            ],
        );
        let pair = typed
            .paired_gauge(
                "fs",
                &id(1),
                ("free", "total"),
                0,
                100,
                GaugeObjective::RatioMin,
            )
            .expect("both columns present");
        assert!(
            (pair.a - 5.0).abs() < 1e-9,
            "5 / 100 = 0.05 is the low point"
        );
    }

    #[test]
    fn paired_gauge_diff_max_selects_the_widest_gap() {
        let typed = gauges(
            "repl",
            &[
                ("sent", &[(10, 100.0), (20, 500.0)]),
                ("replay", &[(10, 90.0), (20, 100.0)]),
            ],
        );
        let pair = typed
            .paired_gauge(
                "repl",
                &id(1),
                ("sent", "replay"),
                0,
                100,
                GaugeObjective::DiffMax,
            )
            .expect("both columns present");
        assert!(
            (pair.a - pair.b - 400.0).abs() < 1e-9,
            "500 - 100 beats 100 - 90"
        );
    }

    #[test]
    fn paired_gauge_skips_a_guarded_zero_denominator() {
        let typed = gauges(
            "cg",
            &[
                ("current", &[(10, 5.0), (20, 9.0)]),
                ("max", &[(10, 0.0), (20, 10.0)]),
            ],
        );
        let pair = typed
            .paired_gauge(
                "cg",
                &id(1),
                ("current", "max"),
                0,
                100,
                GaugeObjective::RatioMax,
            )
            .expect("one shared reading has a positive denominator");
        assert_eq!(
            pair.samples, 1,
            "the max=0 reading is skipped, not divided by"
        );
        assert!((pair.a - 9.0).abs() < 1e-9 && (pair.b - 10.0).abs() < 1e-9);
    }

    #[test]
    fn paired_gauge_only_pairs_shared_timestamps() {
        let typed = gauges(
            "repl",
            &[
                ("sent", &[(10, 100.0), (20, 500.0)]),
                ("replay", &[(20, 100.0), (30, 110.0)]),
            ],
        );
        let pair = typed
            .paired_gauge(
                "repl",
                &id(1),
                ("sent", "replay"),
                0,
                100,
                GaugeObjective::DiffMax,
            )
            .expect("ts=20 is shared");
        assert_eq!(pair.samples, 1);
        assert!((pair.a - 500.0).abs() < 1e-9);
    }

    #[test]
    fn paired_gauge_with_a_missing_column_is_none() {
        let typed = gauges("fs", &[("free", &[(10, 5.0)])]);
        assert!(
            typed
                .paired_gauge(
                    "fs",
                    &id(1),
                    ("free", "total"),
                    0,
                    100,
                    GaugeObjective::RatioMin
                )
                .is_none()
        );
    }

    #[test]
    fn first_valid_gauge_sample_is_an_observation() {
        let typed = gauges("db", &[("age", &[(10, 7.0)])]);
        let reading = typed
            .gauge_window("db", "age", &id(1), 10, 11)
            .and_then(|window| window.max())
            .expect("one sample is valid gauge evidence");
        assert_eq!(reading.samples, 1);
        assert_eq!(reading.observed_at_us, 10);
        assert!((reading.value - 7.0).abs() < 1e-9);
    }

    #[test]
    fn gauge_windows_are_half_open_and_indexed_to_local_points() {
        let typed = gauges(
            "db",
            &[("age", &[(0, 99.0), (10, 7.0), (20, 100.0), (30, 101.0)])],
        );
        let window = typed
            .gauge_window("db", "age", &id(1), 10, 20)
            .expect("ts=10 is local");
        assert_eq!(window.inspected_points(), 1);
        let reading = window.max().expect("one point");
        assert_eq!(reading.observed_at_us, 10);
        assert!((reading.value - 7.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_reading_or_gap_rejects_the_window() {
        let mut typed = TypedInputs::new();
        typed.insert_gauge_with_quality(
            "db",
            "age",
            id(1),
            vec![(10, 7.0), (30, 8.0)],
            vec![20],
            &[],
        );
        assert!(typed.gauge_window("db", "age", &id(1), 10, 30).is_none());

        let mut typed = TypedInputs::new();
        typed.insert_gauge_with_quality(
            "db",
            "age",
            id(1),
            vec![(10, 7.0)],
            Vec::new(),
            &[(15, 25)],
        );
        assert!(typed.gauge_window("db", "age", &id(1), 10, 30).is_none());
    }

    #[test]
    fn duplicate_or_non_finite_raw_timestamp_breaks_evidence() {
        let mut typed = TypedInputs::new();
        typed.insert_gauge(
            "db",
            "age",
            id(1),
            vec![(10, 7.0), (10, 7.0), (20, f64::NAN)],
        );
        assert!(typed.gauge_window("db", "age", &id(1), 0, 30).is_none());
    }

    #[test]
    fn triple_ratio_requires_one_timestamp_for_every_operand() {
        let typed = gauges(
            "mem",
            &[
                ("dirty", &[(10, 6.0)]),
                ("writeback", &[(20, 4.0)]),
                ("total", &[(10, 100.0), (20, 100.0)]),
            ],
        );
        let window = typed
            .triple_gauge_window("mem", &id(1), ("dirty", "writeback", "total"), 0, 30)
            .expect("all tracks exist");
        assert!(window.sum_ratio_max().is_none());
    }

    #[test]
    fn snapshot_reduction_requires_all_columns_at_one_timestamp() {
        let typed = gauges(
            "join",
            &[
                ("value", &[(10, 1.0), (20, 9.0)]),
                ("state", &[(10, 1.0), (30, 1.0)]),
            ],
        );
        let window = typed
            .gauge_snapshot_window("join", &id(1), &["value", "state"], 0, 40)
            .expect("both tracks exist");
        let reading = window.extreme(0, true).expect("ts=10 is shared");
        assert_eq!(reading.samples, 1);
        assert_eq!(reading.observed_at_us, 10);
        assert_eq!(reading.values[..reading.len], [1.0, 1.0]);
    }

    #[test]
    fn trend_needs_two_ordered_samples_and_gap_free_coverage() {
        let typed = gauges("slot", &[("retained", &[(10, 7.0), (20, 9.0)])]);
        let trend = typed
            .gauge_window("slot", "retained", &id(1), 0, 30)
            .and_then(|window| window.trend())
            .expect("two ordered samples");
        assert_eq!(trend.samples, 2);
        assert_eq!((trend.first, trend.last), (7.0, 9.0));

        let mut gapped = TypedInputs::new();
        gapped.insert_gauge_with_quality(
            "slot",
            "retained",
            id(1),
            vec![(10, 7.0), (20, 9.0)],
            Vec::new(),
            &[(15, 16)],
        );
        assert!(
            gapped
                .gauge_window("slot", "retained", &id(1), 0, 30)
                .is_none()
        );
    }

    fn backend(state: &str) -> ActivityBackend {
        ActivityBackend {
            pid: 1,
            backend_start: 1,
            xid_age: None,
            xmin_age: None,
            state: Some(state.into()),
            wait_event_type: None,
            wait_event: None,
            xact_age_us: None,
        }
    }

    fn activity_at(ts: i64, state: &str) -> ActivitySnapshot {
        ActivitySnapshot {
            ts,
            backends: vec![backend(state)],
            completeness: SnapshotCompleteness::Complete,
        }
    }

    #[test]
    fn activity_window_uses_half_open_request_bounds() {
        let mut typed = TypedInputs::new();
        typed.insert_activity_snapshot(activity_at(5, "before"));
        typed.insert_activity_snapshot(activity_at(10, "start"));
        typed.insert_activity_snapshot(activity_at(15, "inside"));
        typed.insert_activity_snapshot(activity_at(20, "end"));
        typed.insert_activity_snapshot(activity_at(25, "after"));

        let states: Vec<_> = typed
            .activity_window(10, 20)
            .map(|snapshot| snapshot.backends[0].state.as_deref().expect("state"))
            .collect();
        assert_eq!(states, ["start", "inside"], "the end bound is excluded");
    }

    #[test]
    fn lock_window_selects_snapshots_by_collection_time() {
        let edge = LockEdge {
            waiter_pid: 20,
            blocker_pid: 10,
        };
        let mut typed = TypedInputs::new();
        typed.insert_lock_snapshot(LockSnapshot {
            ts: 5,
            edges: vec![edge],
        });
        typed.insert_lock_snapshot(LockSnapshot {
            ts: 12,
            edges: vec![edge],
        });

        let times: Vec<_> = typed
            .lock_window(10, 20)
            .map(|snapshot| snapshot.ts)
            .collect();
        assert_eq!(times, [12]);
    }

    #[test]
    fn empty_inputs_expose_no_snapshots() {
        let typed = TypedInputs::new();
        assert_eq!(typed.activity_window(i64::MIN, i64::MAX).count(), 0);
        assert_eq!(typed.lock_window(i64::MIN, i64::MAX).count(), 0);
    }
}
