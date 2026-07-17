//! The per-pair diff: [`diff_pair`] over two consecutive samples of one series.

/// A numeric sample value fed to [`diff_pair`].
///
/// The reader collapses integer widths to `i64`/`u64` and timings to `f64`; the
/// query layer maps those onto this scalar. Integer counters keep exact
/// arithmetic (`i128` subtraction); float timings diff in `f64`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scalar {
    /// An integer counter value, widened from any width.
    Int(i128),
    /// A float counter value (a timing), in the source unit.
    Float(f64),
}

/// Why a diff point carries no value.
///
/// [`diff_pair`] returns only `Reset` and `Anomaly`; `Gap`, `FirstPoint`, and
/// `NotCollected` are assigned by the series-folding layer, which has the
/// coverage, series, and collection-gating context a single pair lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The value fell (`cur < prev`): the counter reset.
    Reset,
    /// The pair spans a coverage gap: no readable snapshots cover the interval.
    Gap,
    /// No predecessor: series start, first point after a gap, or the preceding
    /// segment is gone to retention.
    FirstPoint,
    /// The interval is non-positive (`cur_ts <= prev_ts`): timestamps do not
    /// advance, so no rate is defined.
    Anomaly,
    /// The column's source is disabled for this window (a gated GUC is off), so
    /// a zero would misread as a measured zero.
    NotCollected,
}

/// One diff point: an interval delta and rate, or a reason it is absent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiffPoint {
    /// A computed increase over `dt_micros`. `delta` matches the input scalar
    /// kind; `rate` is `delta` per second.
    Value {
        /// Increase over the interval, exact for integer counters.
        delta: Scalar,
        /// `delta` per second.
        rate: f64,
        /// Interval length, microseconds.
        dt_micros: i64,
    },
    /// No value; see [`Reason`].
    NoData {
        /// Why the point is absent.
        reason: Reason,
    },
}

/// Diff two consecutive samples of one series.
///
/// Both scalars carry the same kind (a column's type is fixed). A fall in value
/// is a reset; a non-positive interval is an anomaly. No extrapolation: the rate
/// is the exact delta over the real interval, so an integer counter yields an
/// integer delta.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "rate is an f64 monitoring value; real counter magnitudes and \
              intervals stay within f64's exact-integer range"
)]
pub fn diff_pair(prev: Scalar, cur: Scalar, prev_ts: i64, cur_ts: i64) -> DiffPoint {
    let dt_micros = cur_ts - prev_ts;
    if dt_micros <= 0 {
        return DiffPoint::NoData {
            reason: Reason::Anomaly,
        };
    }
    let dt_secs = dt_micros as f64 / 1_000_000.0;

    match (prev, cur) {
        (Scalar::Int(a), Scalar::Int(b)) => {
            let delta = b - a;
            if delta < 0 {
                return DiffPoint::NoData {
                    reason: Reason::Reset,
                };
            }
            DiffPoint::Value {
                delta: Scalar::Int(delta),
                rate: delta as f64 / dt_secs,
                dt_micros,
            }
        }
        (Scalar::Float(a), Scalar::Float(b)) => {
            let delta = b - a;
            if delta < 0.0 {
                return DiffPoint::NoData {
                    reason: Reason::Reset,
                };
            }
            DiffPoint::Value {
                delta: Scalar::Float(delta),
                rate: delta / dt_secs,
                dt_micros,
            }
        }
        // A column's type is fixed, so mixed kinds mean corrupt input.
        _ => DiffPoint::NoData {
            reason: Reason::Anomaly,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffPoint, Reason, Scalar, diff_pair};

    const SEC: i64 = 1_000_000;

    #[test]
    fn monotonic_integer_growth_is_an_exact_delta() {
        let p = diff_pair(Scalar::Int(100), Scalar::Int(112), 0, 4 * SEC);
        assert_eq!(
            p,
            DiffPoint::Value {
                delta: Scalar::Int(12),
                rate: 3.0,
                dt_micros: 4 * SEC,
            }
        );
    }

    #[test]
    fn large_unsigned_counters_stay_exact_in_i128() {
        // Values near u64::MAX widened to i128; the delta is exact and integral.
        let prev = Scalar::Int(i128::from(u64::MAX) - 1_000);
        let cur = Scalar::Int(i128::from(u64::MAX));
        let p = diff_pair(prev, cur, 0, SEC);
        assert_eq!(
            p,
            DiffPoint::Value {
                delta: Scalar::Int(1_000),
                rate: 1_000.0,
                dt_micros: SEC,
            }
        );
    }

    #[test]
    fn a_fall_in_an_integer_counter_is_a_reset() {
        let p = diff_pair(Scalar::Int(500), Scalar::Int(10), 0, SEC);
        assert_eq!(
            p,
            DiffPoint::NoData {
                reason: Reason::Reset
            }
        );
    }

    #[test]
    fn float_timings_diff_and_reset_like_integers() {
        let grow = diff_pair(Scalar::Float(1.0), Scalar::Float(3.5), 0, 5 * SEC);
        assert_eq!(
            grow,
            DiffPoint::Value {
                delta: Scalar::Float(2.5),
                rate: 0.5,
                dt_micros: 5 * SEC,
            }
        );
        let fall = diff_pair(Scalar::Float(5.0), Scalar::Float(2.0), 0, SEC);
        assert_eq!(
            fall,
            DiffPoint::NoData {
                reason: Reason::Reset
            }
        );
    }

    #[test]
    fn a_non_positive_interval_is_an_anomaly() {
        let equal = diff_pair(Scalar::Int(1), Scalar::Int(2), 5 * SEC, 5 * SEC);
        let backward = diff_pair(Scalar::Int(1), Scalar::Int(2), 6 * SEC, 5 * SEC);
        assert_eq!(
            equal,
            DiffPoint::NoData {
                reason: Reason::Anomaly
            }
        );
        assert_eq!(
            backward,
            DiffPoint::NoData {
                reason: Reason::Anomaly
            }
        );
    }

    #[test]
    fn a_flat_counter_reports_a_zero_delta_not_a_gap() {
        // No change is a real zero rate, distinct from missing data.
        let p = diff_pair(Scalar::Int(42), Scalar::Int(42), 0, 2 * SEC);
        assert_eq!(
            p,
            DiffPoint::Value {
                delta: Scalar::Int(0),
                rate: 0.0,
                dt_micros: 2 * SEC,
            }
        );
    }
}
