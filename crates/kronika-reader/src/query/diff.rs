//! Series folding: turn a section's snapshot rows into per-series diffs.
//!
//! [`diff_section`] groups a page's rows by identity, sorts each series by time,
//! and folds adjacent pairs through [`kronika_diff::diff_pair`]. It reads no
//! registry: the caller passes the identity and cumulative column names it
//! resolved from the contract.

use std::collections::BTreeMap;

use kronika_diff::{DiffPoint, Reason, Scalar, diff_pair};

use crate::query::value::{Gap, OutRow, Value};

/// One diff point placed at the snapshot time it was computed for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffAt {
    /// Snapshot time of the later sample, unix microseconds.
    pub ts: i64,
    /// The delta and rate, or the reason they are absent.
    pub point: DiffPoint,
}

/// One cumulative column's diff series for one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDiff {
    /// Column name.
    pub name: String,
    /// Points in snapshot-time order; the first is always `FirstPoint`.
    pub points: Vec<DiffAt>,
}

/// One entity's diffs: its identity values and a diff series per cumulative
/// column.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesDiff {
    /// Identity values, in the order of the identity column list.
    pub key: Vec<Value>,
    /// One entry per cumulative column, in the given order.
    pub columns: Vec<ColumnDiff>,
}

/// A hashable, ordered projection of an identity [`Value`].
///
/// Identity columns are `Label` class, never floats or lists, so this covers
/// every real identity; other kinds fold to `Other`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum KeyPart {
    Null,
    Int(i128),
    Bool(bool),
    Text(String),
    Other,
}

#[allow(
    clippy::match_same_arms,
    reason = "integer widths widen to i128 identically but bind different types, \
              so the arms cannot be merged"
)]
fn key_part(value: &Value) -> KeyPart {
    match value {
        Value::Null => KeyPart::Null,
        Value::I64(v) => KeyPart::Int(i128::from(*v)),
        Value::U64(v) => KeyPart::Int(i128::from(*v)),
        Value::Ts(v) => KeyPart::Int(i128::from(*v)),
        Value::Bool(v) => KeyPart::Bool(*v),
        Value::Str(s) => KeyPart::Text(s.clone()),
        Value::Blob { text, .. } => KeyPart::Text(text.clone()),
        Value::F64(_) | Value::ListI32(_) => KeyPart::Other,
    }
}

pub(crate) fn column<'a>(row: &'a OutRow, name: &str) -> Option<&'a Value> {
    row.iter().find(|(n, _)| n.as_str() == name).map(|(_, v)| v)
}

fn row_ts(row: &OutRow) -> Option<i64> {
    match column(row, "ts")? {
        Value::Ts(v) => Some(*v),
        _ => None,
    }
}

#[allow(
    clippy::match_same_arms,
    reason = "i64 and u64 widen to i128 identically but bind different types, \
              so the arms cannot be merged"
)]
fn scalar(value: &Value) -> Option<Scalar> {
    match value {
        Value::I64(v) => Some(Scalar::Int(i128::from(*v))),
        Value::U64(v) => Some(Scalar::Int(i128::from(*v))),
        Value::F64(v) => Some(Scalar::Float(*v)),
        _ => None,
    }
}

/// Whether the interval `(prev_ts, cur_ts)` crosses a coverage gap.
fn spans_gap(prev_ts: i64, cur_ts: i64, gaps: &[Gap]) -> bool {
    gaps.iter().any(|g| g.from < cur_ts && prev_ts < g.to)
}

/// One grouped series: its identity values and time-ordered `(ts, row)` pairs.
pub(crate) type GroupedSeries<'a> = (Vec<Value>, Vec<(i64, &'a OutRow)>);

/// Group a page's rows by identity and sort each group by snapshot time.
///
/// Returns `(identity values, time-ordered rows)` per series, in identity
/// order. A row without a readable `ts` sorts first.
pub(crate) fn group_series<'a>(identity: &[&str], rows: &'a [OutRow]) -> Vec<GroupedSeries<'a>> {
    let mut groups: BTreeMap<Vec<KeyPart>, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let key: Vec<KeyPart> = identity
            .iter()
            .map(|name| column(row, name).map_or(KeyPart::Null, key_part))
            .collect();
        groups.entry(key).or_default().push(i);
    }

    let mut out = Vec::with_capacity(groups.len());
    for indices in groups.into_values() {
        let mut series: Vec<(i64, &OutRow)> = indices
            .iter()
            .map(|&i| (row_ts(&rows[i]).unwrap_or(i64::MIN), &rows[i]))
            .collect();
        series.sort_by_key(|(ts, _)| *ts);

        let key: Vec<Value> = identity
            .iter()
            .map(|name| column(series[0].1, name).cloned().unwrap_or(Value::Null))
            .collect();

        out.push((key, series));
    }
    out
}

/// Fold a page's rows into per-series, per-column diffs.
///
/// `rows` may arrive in any order; each must carry a `ts` column plus the named
/// identity and cumulative columns. Series are returned in identity order.
#[must_use]
pub fn diff_section(
    identity: &[&str],
    cumulative: &[&str],
    rows: &[OutRow],
    gaps: &[Gap],
) -> Vec<SeriesDiff> {
    group_series(identity, rows)
        .into_iter()
        .map(|(key, series)| {
            let columns = cumulative
                .iter()
                .map(|&name| ColumnDiff {
                    name: name.to_owned(),
                    points: fold_column(&series, name, gaps),
                })
                .collect();
            SeriesDiff { key, columns }
        })
        .collect()
}

fn fold_column(series: &[(i64, &OutRow)], name: &str, gaps: &[Gap]) -> Vec<DiffAt> {
    let mut points = Vec::with_capacity(series.len());
    for (i, &(ts, row)) in series.iter().enumerate() {
        let point = if i == 0 {
            DiffPoint::NoData {
                reason: Reason::FirstPoint,
            }
        } else {
            let (prev_ts, prev_row) = series[i - 1];
            fold_pair(prev_ts, prev_row, ts, row, name, gaps)
        };
        points.push(DiffAt { ts, point });
    }
    points
}

fn fold_pair(
    prev_ts: i64,
    prev: &OutRow,
    cur_ts: i64,
    cur: &OutRow,
    name: &str,
    gaps: &[Gap],
) -> DiffPoint {
    if spans_gap(prev_ts, cur_ts, gaps) {
        return DiffPoint::NoData {
            reason: Reason::Gap,
        };
    }
    let (Some(p), Some(c)) = (
        column(prev, name).and_then(scalar),
        column(cur, name).and_then(scalar),
    ) else {
        return DiffPoint::NoData {
            reason: Reason::Anomaly,
        };
    };
    diff_pair(p, c, prev_ts, cur_ts)
}

#[cfg(test)]
mod tests {
    use super::{DiffAt, SeriesDiff, diff_section};
    use crate::query::value::{Gap, OutRow, Value};
    use kronika_diff::{DiffPoint, Reason, Scalar};

    const SEC: i64 = 1_000_000;

    /// Build a row: `ts`, one identity column `id`, one cumulative column `n`.
    fn row(ts: i64, id: i64, n: i64) -> OutRow {
        vec![
            ("ts".to_owned(), Value::Ts(ts)),
            ("id".to_owned(), Value::I64(id)),
            ("n".to_owned(), Value::I64(n)),
        ]
    }

    fn points(series: &[SeriesDiff], key: i64) -> Vec<DiffPoint> {
        series
            .iter()
            .find(|s| s.key == vec![Value::I64(key)])
            .expect("series present")
            .columns[0]
            .points
            .iter()
            .map(|p| p.point)
            .collect()
    }

    #[test]
    fn interleaved_series_group_and_fold_independently() {
        // Two entities' rows interleaved by time, as the sort key would order.
        let rows = vec![
            row(0, 1, 100),
            row(0, 2, 5),
            row(2 * SEC, 1, 110),
            row(2 * SEC, 2, 9),
        ];
        let out = diff_section(&["id"], &["n"], &rows, &[]);
        assert_eq!(out.len(), 2);

        assert_eq!(
            points(&out, 1),
            vec![
                DiffPoint::NoData {
                    reason: Reason::FirstPoint
                },
                DiffPoint::Value {
                    delta: Scalar::Int(10),
                    rate: 5.0,
                    dt_micros: 2 * SEC,
                },
            ]
        );
        assert_eq!(
            points(&out, 2),
            vec![
                DiffPoint::NoData {
                    reason: Reason::FirstPoint
                },
                DiffPoint::Value {
                    delta: Scalar::Int(4),
                    rate: 2.0,
                    dt_micros: 2 * SEC,
                },
            ]
        );
    }

    #[test]
    fn a_fall_in_the_series_is_a_reset() {
        let rows = vec![row(0, 1, 500), row(SEC, 1, 10)];
        let out = diff_section(&["id"], &["n"], &rows, &[]);
        assert_eq!(
            points(&out, 1),
            vec![
                DiffPoint::NoData {
                    reason: Reason::FirstPoint
                },
                DiffPoint::NoData {
                    reason: Reason::Reset
                },
            ]
        );
    }

    #[test]
    fn a_pair_across_a_coverage_gap_is_not_diffed() {
        let rows = vec![row(0, 1, 100), row(10 * SEC, 1, 200)];
        let gaps = vec![Gap {
            from: 2 * SEC,
            to: 8 * SEC,
        }];
        let out = diff_section(&["id"], &["n"], &rows, &gaps);
        assert_eq!(
            points(&out, 1),
            vec![
                DiffPoint::NoData {
                    reason: Reason::FirstPoint
                },
                DiffPoint::NoData {
                    reason: Reason::Gap
                },
            ]
        );
    }

    #[test]
    fn out_of_order_rows_are_sorted_by_time_first() {
        let rows = vec![row(2 * SEC, 1, 110), row(0, 1, 100)];
        let out = diff_section(&["id"], &["n"], &rows, &[]);
        assert_eq!(
            out[0].columns[0].points,
            vec![
                DiffAt {
                    ts: 0,
                    point: DiffPoint::NoData {
                        reason: Reason::FirstPoint
                    },
                },
                DiffAt {
                    ts: 2 * SEC,
                    point: DiffPoint::Value {
                        delta: Scalar::Int(10),
                        rate: 5.0,
                        dt_micros: 2 * SEC,
                    },
                },
            ]
        );
    }
}
