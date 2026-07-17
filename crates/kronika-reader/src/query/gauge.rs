//! Raw gauge series: group a section's rows into per-series value timelines.
//!
//! The anomaly detector scores gauge columns on raw readings, so unlike
//! [`diff_section`](crate::query::diff::diff_section) nothing is folded:
//! each numeric value becomes a `(ts, f64)` point.

use crate::query::diff::{column, group_series};
use crate::query::value::{OutRow, Value};

/// One gauge column's raw values for one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnValues {
    /// Column name.
    pub name: String,
    /// `(ts, value)` in snapshot-time order.
    pub points: Vec<(i64, f64)>,
    /// Timestamps whose invalid value splits timeline continuity.
    pub breaks: Vec<i64>,
    /// Rows whose value was NULL, non-numeric, or non-finite and left no point.
    pub skipped: usize,
}

/// One entity's raw gauge series: identity values and points per column.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesValues {
    /// Identity values, in the order of the identity column list.
    pub key: Vec<Value>,
    /// One entry per requested column, in the given order.
    pub columns: Vec<ColumnValues>,
}

/// Collect per-series raw values of the `gauges` columns.
///
/// `rows` may arrive in any order; each must carry a `ts` column plus the
/// named identity columns. Series are returned in identity order. Integers,
/// finite floats, booleans (as 0/1), and timestamps (as microseconds) become
/// points; NULL, non-numeric, and non-finite values are counted in `skipped`.
#[must_use]
pub fn gauge_section(identity: &[&str], gauges: &[&str], rows: &[OutRow]) -> Vec<SeriesValues> {
    group_series(identity, rows)
        .into_iter()
        .map(|(key, series)| {
            let columns = gauges
                .iter()
                .map(|&name| collect_column(&series, name))
                .collect();
            SeriesValues { key, columns }
        })
        .collect()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "gauge readings are monitoring values; real magnitudes stay within \
              f64's exact-integer range"
)]
const fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::I64(v) | Value::Ts(v) => Some(*v as f64),
        Value::U64(v) => Some(*v as f64),
        // A non-finite reading is dropped like a NULL: scoring it would poison
        // the whole series' reference, so it is counted as skipped, not a point.
        Value::F64(v) => {
            if v.is_finite() {
                Some(*v)
            } else {
                None
            }
        }
        Value::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
        Value::Null | Value::Str(_) | Value::Blob { .. } | Value::ListI32(_) => None,
    }
}

fn collect_column(series: &[(i64, &OutRow)], name: &str) -> ColumnValues {
    let mut points = Vec::with_capacity(series.len());
    let mut breaks = Vec::new();
    for &(ts, row) in series {
        match column(row, name).and_then(numeric) {
            Some(value) => points.push((ts, value)),
            None => breaks.push(ts),
        }
    }
    let skipped = breaks.len();
    ColumnValues {
        name: name.to_owned(),
        points,
        breaks,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::gauge_section;
    use crate::query::value::{OutRow, Value};

    fn row(ts: i64, id: i64, value: Value) -> OutRow {
        vec![
            ("ts".to_owned(), Value::Ts(ts)),
            ("id".to_owned(), Value::I64(id)),
            ("g".to_owned(), value),
        ]
    }

    #[test]
    fn series_group_by_identity_and_sort_by_time() {
        let rows = vec![
            row(20, 1, Value::I64(12)),
            row(0, 2, Value::I64(5)),
            row(0, 1, Value::I64(10)),
        ];
        let out = gauge_section(&["id"], &["g"], &rows);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].key, vec![Value::I64(1)]);
        assert_eq!(out[0].columns[0].points, vec![(0, 10.0), (20, 12.0)]);
        assert_eq!(out[1].key, vec![Value::I64(2)]);
        assert_eq!(out[1].columns[0].points, vec![(0, 5.0)]);
    }

    #[test]
    fn null_and_non_numeric_values_are_skipped_and_counted() {
        let rows = vec![
            row(0, 1, Value::Null),
            row(10, 1, Value::Str("busy".to_owned())),
            row(20, 1, Value::F64(2.5)),
        ];
        let out = gauge_section(&["id"], &["g"], &rows);
        assert_eq!(out[0].columns[0].points, vec![(20, 2.5)]);
        assert_eq!(out[0].columns[0].breaks, vec![0, 10]);
        assert_eq!(out[0].columns[0].skipped, 2);
    }

    #[test]
    fn non_finite_float_values_are_skipped_not_scored() {
        // A NaN or infinite gauge reading must leave no point: scoring it would
        // poison the whole series' reference for every window position.
        let rows = vec![
            row(0, 1, Value::F64(f64::NAN)),
            row(10, 1, Value::F64(f64::INFINITY)),
            row(20, 1, Value::F64(2.5)),
        ];
        let out = gauge_section(&["id"], &["g"], &rows);
        assert_eq!(out[0].columns[0].points, vec![(20, 2.5)]);
        assert_eq!(out[0].columns[0].breaks, vec![0, 10]);
        assert_eq!(out[0].columns[0].skipped, 2, "NaN and inf leave no point");
    }

    #[test]
    fn booleans_and_timestamps_read_as_numbers() {
        let rows = vec![
            row(0, 1, Value::Bool(false)),
            row(10, 1, Value::Bool(true)),
            row(20, 1, Value::Ts(1_000_000)),
        ];
        let out = gauge_section(&["id"], &["g"], &rows);
        assert_eq!(
            out[0].columns[0].points,
            vec![(0, 0.0), (10, 1.0), (20, 1_000_000.0)]
        );
        assert_eq!(out[0].columns[0].skipped, 0);
        assert!(out[0].columns[0].breaks.is_empty());
    }

    #[test]
    fn empty_rows_yield_no_series() {
        assert!(gauge_section(&["id"], &["g"], &[]).is_empty());
    }
}
