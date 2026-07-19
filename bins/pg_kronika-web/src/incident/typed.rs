//! Typed counter and gauge evidence forwarded to lenses.
//!
//! Anomaly episodes only locate an incident; a finding needs the underlying
//! values. This holds the per-column counter diffs the reader already
//! folded, keyed by series, so a lens reports measured numbers and excludes
//! intervals the reader marked unusable (reset, gap, first point, timestamp
//! anomaly, or a disabled source).
//!
//! Gauges are instantaneous levels, so they are indexed without differencing.

use std::collections::BTreeMap;
use std::sync::Arc;

use kronika_analytics::DiffPoint;

use super::model::IdentityValue;

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

impl CounterPoint {
    /// The interval delta as an `f64`, or `None` when the interval is unusable:
    /// the reader marked it absent, or its rate is non-finite. Recovered as
    /// `rate * dt` so the domain core never depends on the reader's scalar kind.
    #[allow(
        clippy::cast_precision_loss,
        reason = "dt_micros is a real interval length within f64's exact-integer range"
    )]
    fn delta(self) -> Option<f64> {
        match self.point {
            DiffPoint::Value {
                rate, dt_micros, ..
            } if rate.is_finite() => Some(rate * (dt_micros as f64) / 1_000_000.0),
            DiffPoint::Value { .. } | DiffPoint::NoData { .. } => None,
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

pub(crate) struct GaugeWindow<'a> {
    points: &'a [GaugePoint],
}

impl GaugeWindow<'_> {
    pub(crate) const fn inspected_points(&self) -> usize {
        self.points.len()
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
        })
    }
}

pub(crate) struct PairedGaugeWindow<'a> {
    a: &'a [GaugePoint],
    b: &'a [GaugePoint],
}

impl PairedGaugeWindow<'_> {
    pub(crate) const fn inspected_points(&self) -> usize {
        self.a.len().saturating_add(self.b.len())
    }

    pub(crate) fn reduce(&self, objective: GaugeObjective) -> Option<PairedGauge> {
        let mut best: Option<(f64, GaugePoint, GaugePoint)> = None;
        let mut samples = 0;
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
                        samples += 1;
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
            samples,
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
}

pub(crate) struct TripleGaugeWindow<'a> {
    a: &'a [GaugePoint],
    b: &'a [GaugePoint],
    denominator: &'a [GaugePoint],
}

/// Exact-timestamp readings for a small fixed lens contract.
pub(crate) struct GaugeSnapshotWindow<'a> {
    tracks: Vec<&'a [GaugePoint]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GaugeSnapshot {
    pub values: [f64; 8],
    pub len: usize,
    pub observed_at_us: i64,
    pub samples: usize,
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
        let mut samples = 0_usize;
        loop {
            let mut min_ts = i64::MAX;
            let mut max_ts = i64::MIN;
            for (track, &index) in self.tracks.iter().zip(&indexes) {
                let Some(point) = track.get(index) else {
                    return best.map(|mut reading| {
                        reading.samples = samples;
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
            samples = samples.saturating_add(1);
            let candidate = GaugeSnapshot {
                values,
                len: self.tracks.len(),
                observed_at_us: min_ts,
                samples,
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
        let mut samples = 0;
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
                    samples += 1;
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
            samples,
        })
    }
}

/// Typed counter and gauge series for one analysis request, keyed by
/// `(section, column, identity)`.
pub(crate) struct TypedInputs {
    counters: BTreeMap<TrackKey, CounterTrack>,
    gauges: GaugeTracks,
}

impl TypedInputs {
    pub(crate) const fn new() -> Self {
        Self {
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
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
        };
        let (mut i, mut j) = (0, 0);
        while i < track_a.points.len() && j < track_b.points.len() {
            let a = track_a.points[i];
            let b = track_b.points[j];
            match a.end_us.cmp(&b.end_us) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    if (from_us..=to_us).contains(&a.end_us)
                        && let (Some(delta_a), Some(delta_b)) = (a.delta(), b.delta())
                    {
                        sums.sum_a += delta_a;
                        sums.sum_b += delta_b;
                        sums.intervals += 1;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        Some(sums)
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
            section,
            column,
            identity,
            raw_points,
            breaks,
            Arc::from([]),
            &GaugeQuality::new(gaps),
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "reader retention passes one bounded track and its shared quality provenance"
    )]
    pub(crate) fn insert_gauge_with_shared_quality(
        &mut self,
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
        mut raw_points: Vec<(i64, f64)>,
        mut breaks: Vec<i64>,
        mut shared_breaks: Arc<[i64]>,
        quality: &GaugeQuality,
    ) {
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
        Some(GaugeSnapshotWindow { tracks })
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

    // A one-second interval, so `CounterPoint::delta` (rate * dt) equals `delta`.
    // The `Scalar` field is unread: the domain core recovers delta from `rate`.
    fn value(delta: f64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Int(0),
            rate: delta,
            dt_micros: 1_000_000,
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
        assert!(sums.sum_a.abs() < 1e-9);
        assert!(sums.sum_b.abs() < 1e-9);
    }

    #[test]
    fn a_non_finite_rate_interval_is_unusable() {
        let nan = DiffPoint::Value {
            delta: Scalar::Int(0),
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
        assert_eq!(sums.intervals, 1, "the non-finite interval is excluded");
        assert!((sums.sum_a - 7.0).abs() < 1e-9);
        assert!((sums.sum_b - 70.0).abs() < 1e-9);
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
        assert!((sums.sum_a - 10.0).abs() < 1e-9);
        assert!((sums.sum_b - 100.0).abs() < 1e-9);
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
}
