//! Typed counter evidence forwarded to lenses.
//!
//! Anomaly episodes only locate an incident; a finding needs the underlying
//! values. This holds the honest per-column counter diffs the reader already
//! folded, keyed by series, so a lens reports measured numbers and excludes
//! intervals the reader marked unusable (reset, gap, first point, timestamp
//! anomaly, or a disabled source).

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

/// Typed counter series for one analysis request, keyed by
/// `(section, column, identity)`.
pub(crate) struct TypedInputs {
    counters: BTreeMap<TrackKey, CounterTrack>,
}

impl TypedInputs {
    pub(crate) const fn new() -> Self {
        Self {
            counters: BTreeMap::new(),
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
}
