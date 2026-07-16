//! GUC gating of diff series: rewrite pairs measured while a gate was off.
//!
//! A gated column (registry `gated_by`) reads zero whenever its GUC is off;
//! folding that zero into a delta claims "measured zero". This pass rewrites
//! a `Value` point to `NoData { NotCollected }` when the gate was off at
//! either end of the pair's interval, so the timeline says "not measured".

use kronika_diff::{DiffPoint, Reason};

use crate::query::diff::{SeriesDiff, column};
use crate::query::value::{OutRow, Value};

/// One gate reading: snapshot time and the GUC state, `None` when the
/// collector could not read the GUC.
pub type GateReading = (i64, Option<bool>);

/// Rewrite `column`'s `Value` points to `NotCollected` where `gate` was off.
///
/// `gate` must be in ascending `ts` order; the state at a timestamp is the
/// most recent reading at or before it. A pair is gated when either end of
/// its interval reads `false` — a gate that flipped mid-interval measured
/// only part of it. An unknown state (`None`, or a timestamp before the
/// first reading) gates nothing: absence of knowledge is not evidence the
/// GUC was off.
pub fn apply_gating(series: &mut [SeriesDiff], column: &str, gate: &[GateReading]) {
    debug_assert!(
        gate.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "apply_gating expects gate readings in ascending ts order"
    );
    for one in series.iter_mut() {
        for gated in one.columns.iter_mut().filter(|c| c.name == column) {
            let mut prev_ts: Option<i64> = None;
            for at in &mut gated.points {
                let off_at = |ts: i64| state_at(gate, ts) == Some(false);
                if matches!(at.point, DiffPoint::Value { .. })
                    && (off_at(at.ts) || prev_ts.is_some_and(off_at))
                {
                    at.point = DiffPoint::NoData {
                        reason: Reason::NotCollected,
                    };
                }
                prev_ts = Some(at.ts);
            }
        }
    }
}

/// Collect one Bool gate column's readings from section rows, ascending by
/// `ts`. A NULL or non-Bool cell reads as unknown (`None`); rows without a
/// readable `ts` are dropped.
#[must_use]
pub fn gate_readings(rows: &[OutRow], gate_column: &str) -> Vec<GateReading> {
    let mut readings: Vec<GateReading> = rows
        .iter()
        .filter_map(|row| {
            let Some(Value::Ts(ts)) = column(row, "ts") else {
                return None;
            };
            let state = match column(row, gate_column) {
                Some(Value::Bool(state)) => Some(*state),
                _ => None,
            };
            Some((*ts, state))
        })
        .collect();
    readings.sort_by_key(|&(ts, _)| ts);
    readings
}

/// The gate state at `ts`: the most recent reading at or before it.
fn state_at(gate: &[GateReading], ts: i64) -> Option<bool> {
    let idx = gate.partition_point(|&(reading_ts, _)| reading_ts <= ts);
    if idx == 0 { None } else { gate[idx - 1].1 }
}

#[cfg(test)]
mod tests {
    use kronika_diff::{DiffPoint, Reason, Scalar};

    use super::apply_gating;
    use crate::query::diff::{ColumnDiff, DiffAt, SeriesDiff};
    use crate::query::value::Value;

    const SEC: i64 = 1_000_000;

    fn value_at(ts: i64) -> DiffAt {
        DiffAt {
            ts,
            point: DiffPoint::Value {
                delta: Scalar::Int(10),
                rate: 1.0,
                dt_micros: SEC,
            },
        }
    }

    fn series(points: Vec<DiffAt>) -> Vec<SeriesDiff> {
        vec![SeriesDiff {
            key: vec![Value::I64(1)],
            columns: vec![ColumnDiff {
                name: "blk_read_time".to_owned(),
                points,
            }],
        }]
    }

    fn reasons(series: &[SeriesDiff]) -> Vec<Option<Reason>> {
        series[0].columns[0]
            .points
            .iter()
            .map(|at| match at.point {
                DiffPoint::Value { .. } => None,
                DiffPoint::NoData { reason } => Some(reason),
            })
            .collect()
    }

    #[test]
    fn gate_readings_sort_and_treat_null_as_unknown() {
        let row = |ts: i64, state: Value| -> crate::query::value::OutRow {
            vec![
                ("ts".to_owned(), Value::Ts(ts)),
                ("track_io_timing".to_owned(), state),
            ]
        };
        let rows = vec![
            row(2 * SEC, Value::Bool(true)),
            row(SEC, Value::Null),
            row(0, Value::Bool(false)),
        ];
        assert_eq!(
            super::gate_readings(&rows, "track_io_timing"),
            vec![(0, Some(false)), (SEC, None), (2 * SEC, Some(true))]
        );
    }

    #[test]
    fn pairs_inside_an_off_interval_become_not_collected() {
        let mut s = series(vec![value_at(SEC), value_at(2 * SEC), value_at(3 * SEC)]);
        // Off from t=0, back on at t=2s: the pairs ending at 1s and 2s touch
        // the off state (at their start or end), the 2s..3s pair is clean.
        let gate = vec![(0, Some(false)), (2 * SEC, Some(true))];
        apply_gating(&mut s, "blk_read_time", &gate);
        assert_eq!(
            reasons(&s),
            vec![Some(Reason::NotCollected), Some(Reason::NotCollected), None]
        );
    }

    #[test]
    fn an_always_on_gate_rewrites_nothing() {
        let mut s = series(vec![value_at(SEC), value_at(2 * SEC)]);
        let gate = vec![(0, Some(true))];
        apply_gating(&mut s, "blk_read_time", &gate);
        assert_eq!(reasons(&s), vec![None, None]);
    }

    #[test]
    fn an_unknown_gate_state_rewrites_nothing() {
        let mut s = series(vec![value_at(SEC), value_at(2 * SEC)]);
        apply_gating(&mut s, "blk_read_time", &[(0, None)]);
        assert_eq!(reasons(&s), vec![None, None], "None state gates nothing");
        let mut s = series(vec![value_at(SEC)]);
        apply_gating(&mut s, "blk_read_time", &[(5 * SEC, Some(false))]);
        assert_eq!(
            reasons(&s),
            vec![None],
            "a timestamp before the first reading gates nothing"
        );
    }

    #[test]
    fn other_columns_and_nodata_points_stay_untouched() {
        let mut s = series(vec![DiffAt {
            ts: SEC,
            point: DiffPoint::NoData {
                reason: Reason::FirstPoint,
            },
        }]);
        s[0].columns.push(ColumnDiff {
            name: "blks_read".to_owned(),
            points: vec![value_at(2 * SEC)],
        });
        let gate = vec![(0, Some(false))];
        apply_gating(&mut s, "blk_read_time", &gate);
        assert_eq!(reasons(&s), vec![Some(Reason::FirstPoint)]);
        assert!(
            matches!(s[0].columns[1].points[0].point, DiffPoint::Value { .. }),
            "an ungated column keeps its values"
        );
    }
}
