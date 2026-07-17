//! Collection-state gating of diff intervals.

use kronika_analytics::{DiffPoint, Reason};
use kronika_registry::{CollectionGate, SectionColumnRef};

use crate::query::diff::{SeriesDiff, column};
use crate::query::value::{OutRow, Value};

/// One gate reading: snapshot time and the GUC state, `None` when the
/// collector could not read the GUC.
pub type GateReading = (i64, Option<bool>);

/// Resolve a row-conditional gate from the diff identity.
#[must_use]
pub fn select_gate(
    gate: CollectionGate,
    identity: &[&str],
    key: &[Value],
) -> Option<SectionColumnRef> {
    for rule in gate.overrides {
        let index = identity.iter().position(|name| *name == rule.column)?;
        match key.get(index)? {
            Value::Str(value) if value == rule.value => return Some(rule.gate),
            Value::Str(_) => {}
            _ => return None,
        }
    }
    Some(gate.default)
}

/// Apply a registry gate, including row overrides, to one diff column.
pub fn apply_collection_gating<'a>(
    series: &mut [SeriesDiff],
    column: &str,
    identity: &[&str],
    gate: CollectionGate,
    readings: impl Fn(SectionColumnRef) -> &'a [GateReading],
) {
    let mut references = Vec::new();
    for reference in gate.references() {
        if !references.contains(&reference) {
            references.push(reference);
        }
    }
    for reference in references {
        apply_gating_where(series, column, readings(reference), |one| {
            select_gate(gate, identity, &one.key) == Some(reference)
        });
    }
    apply_gating_where(series, column, &[], |one| {
        select_gate(gate, identity, &one.key).is_none()
    });
}

/// Rewrite values unless the gate is known on throughout their sampled interval.
///
/// `gate` must be in ascending `ts` order; the state at a timestamp is the
/// latest reading at or before it. A false or unknown reading inside the
/// interval makes the value `NotCollected`. Changes between samples cannot be
/// observed.
pub fn apply_gating(series: &mut [SeriesDiff], column: &str, gate: &[GateReading]) {
    apply_gating_where(series, column, gate, |_| true);
}

fn apply_gating_where(
    series: &mut [SeriesDiff],
    column: &str,
    gate: &[GateReading],
    include: impl Fn(&SeriesDiff) -> bool,
) {
    debug_assert!(
        gate.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "gate readings must be sorted by ts"
    );
    let timeline = GateTimeline::new(gate);
    for one in series.iter_mut().filter(|one| include(one)) {
        for gated in one.columns.iter_mut().filter(|c| c.name == column) {
            let mut prev_ts: Option<i64> = None;
            for at in &mut gated.points {
                if let DiffPoint::Value { dt_micros, .. } = at.point {
                    let start = prev_ts.unwrap_or_else(|| at.ts.saturating_sub(dt_micros));
                    if !timeline.collected_throughout(start, at.ts) {
                        at.point = DiffPoint::NoData {
                            reason: Reason::NotCollected,
                        };
                    }
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

struct GateTimeline<'a> {
    readings: &'a [GateReading],
    invalid_prefix: Vec<usize>,
}

impl<'a> GateTimeline<'a> {
    fn new(readings: &'a [GateReading]) -> Self {
        let mut invalid_prefix = Vec::with_capacity(readings.len() + 1);
        invalid_prefix.push(0);
        for &(_, state) in readings {
            let invalid = usize::from(state != Some(true));
            invalid_prefix.push(invalid_prefix.last().copied().unwrap_or(0) + invalid);
        }
        Self {
            readings,
            invalid_prefix,
        }
    }

    fn collected_throughout(&self, from: i64, to: i64) -> bool {
        if state_at(self.readings, from) != Some(true) {
            return false;
        }
        let first = self.readings.partition_point(|&(ts, _)| ts <= from);
        let last = self.readings.partition_point(|&(ts, _)| ts <= to);
        self.invalid_prefix[last] == self.invalid_prefix[first]
    }
}

#[cfg(test)]
mod tests {
    use kronika_analytics::{DiffPoint, Reason, Scalar};

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
    fn unknown_and_before_first_are_not_collected() {
        let mut s = series(vec![value_at(SEC), value_at(2 * SEC)]);
        apply_gating(&mut s, "blk_read_time", &[(0, None)]);
        assert_eq!(
            reasons(&s),
            vec![Some(Reason::NotCollected), Some(Reason::NotCollected)]
        );
        let mut s = series(vec![value_at(SEC)]);
        apply_gating(&mut s, "blk_read_time", &[(5 * SEC, Some(false))]);
        assert_eq!(reasons(&s), vec![Some(Reason::NotCollected)]);
    }

    #[test]
    fn interior_false_reading_invalidates_the_interval() {
        let mut s = series(vec![value_at(SEC), value_at(2 * SEC)]);
        let gate = vec![
            (0, Some(true)),
            (SEC + SEC / 4, Some(false)),
            (SEC + SEC / 2, Some(true)),
        ];
        apply_gating(&mut s, "blk_read_time", &gate);
        assert_eq!(reasons(&s), vec![None, Some(Reason::NotCollected)]);
    }

    #[test]
    fn endpoint_transitions_are_conservative() {
        let mut off_to_on = series(vec![value_at(SEC)]);
        apply_gating(
            &mut off_to_on,
            "blk_read_time",
            &[(0, Some(false)), (SEC, Some(true))],
        );
        assert_eq!(reasons(&off_to_on), vec![Some(Reason::NotCollected)]);

        let mut on_to_off = series(vec![value_at(SEC)]);
        apply_gating(
            &mut on_to_off,
            "blk_read_time",
            &[(0, Some(true)), (SEC, Some(false))],
        );
        assert_eq!(reasons(&on_to_off), vec![Some(Reason::NotCollected)]);
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
