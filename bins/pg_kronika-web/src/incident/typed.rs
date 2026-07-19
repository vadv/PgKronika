//! Typed counter and gauge evidence forwarded to lenses.
//!
//! Anomaly episodes only locate an incident; a finding needs the underlying
//! values. This holds the honest per-column counter diffs the reader already
//! folded, keyed by series, so a lens reports measured numbers and excludes
//! intervals the reader marked unusable (reset, gap, first point, timestamp
//! anomaly, or a disabled source).
//!
//! Gauges are kept as a separate branch: a gauge is an instantaneous level, not
//! a rate, so it is never differenced. Each retained reading is a value the
//! reader already validated (NULL, non-numeric, and non-finite readings left no
//! point), and a lens reduces the readings inside the incident window to one
//! worst-case number.

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
}

#[derive(Clone, Copy)]
struct GaugePoint {
    ts: i64,
    value: f64,
}

/// A single gauge column reduced to one worst-case level over a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GaugeReading {
    pub value: f64,
    /// Valid readings that fell inside the window.
    pub samples: usize,
}

/// Two gauge columns of one series at the shared timestamp that is worst for a
/// [`GaugeObjective`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PairedGauge {
    pub a: f64,
    pub b: f64,
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
        }
    }

    const fn maximizes(self) -> bool {
        matches!(self, Self::ShareMax | Self::RatioMax | Self::DiffMax)
    }
}

/// Typed counter and gauge series for one analysis request, keyed by
/// `(section, column, identity)`.
pub(crate) struct TypedInputs {
    counters: BTreeMap<TrackKey, CounterTrack>,
    gauges: BTreeMap<TrackKey, GaugeTrack>,
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
        let points = points
            .into_iter()
            .map(|(ts, value)| GaugePoint { ts, value })
            .collect();
        self.gauges.insert(
            TrackKey {
                section,
                column,
                identity,
            },
            GaugeTrack { points },
        );
    }

    fn gauge(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
    ) -> Option<&GaugeTrack> {
        self.gauges.get(&TrackKey {
            section,
            column,
            identity: Arc::from(identity),
        })
    }

    /// The largest valid reading of one gauge column inside `[from_us, to_us]`,
    /// the worst case for a headroom or level threshold.
    ///
    /// Returns `None` when the column is absent for the series or no reading
    /// lands in the window. A non-finite reading is skipped defensively; the
    /// reader already dropped such readings, so `samples` counts real levels.
    pub(crate) fn gauge_max(
        &self,
        section: &'static str,
        column: &'static str,
        identity: &[IdentityValue],
        from_us: i64,
        to_us: i64,
    ) -> Option<GaugeReading> {
        let track = self.gauge(section, column, identity)?;
        let mut peak: Option<f64> = None;
        let mut samples = 0;
        for point in &track.points {
            if !(from_us..=to_us).contains(&point.ts) || !point.value.is_finite() {
                continue;
            }
            samples += 1;
            peak = Some(peak.map_or(point.value, |current| current.max(point.value)));
        }
        peak.map(|value| GaugeReading { value, samples })
    }

    /// Reduce two gauge columns of one series to the shared-timestamp pair that
    /// is worst for `objective`, over `[from_us, to_us]`. `columns` is the
    /// `(a, b)` pair the objective scores as `score(a, b)`.
    ///
    /// Only timestamps where both columns carry a valid reading and the
    /// objective's score is finite contribute, so a guarded denominator drops
    /// the reading instead of dividing by zero. Returns `None` when either
    /// column is absent or no shared reading qualifies.
    pub(crate) fn paired_gauge(
        &self,
        section: &'static str,
        identity: &[IdentityValue],
        columns: (&'static str, &'static str),
        from_us: i64,
        to_us: i64,
        objective: GaugeObjective,
    ) -> Option<PairedGauge> {
        let (column_a, column_b) = columns;
        let track_a = self.gauge(section, column_a, identity)?;
        let track_b = self.gauge(section, column_b, identity)?;

        let mut best: Option<(f64, f64, f64)> = None;
        let mut samples = 0;
        let (mut i, mut j) = (0, 0);
        while i < track_a.points.len() && j < track_b.points.len() {
            let a = track_a.points[i];
            let b = track_b.points[j];
            match a.ts.cmp(&b.ts) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    if (from_us..=to_us).contains(&a.ts)
                        && a.value.is_finite()
                        && b.value.is_finite()
                    {
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
                                best = Some((score, a.value, b.value));
                            }
                        }
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        best.map(|(_, a, b)| PairedGauge { a, b, samples })
    }
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
}
