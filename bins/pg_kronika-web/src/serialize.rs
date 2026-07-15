use kronika_anomaly::Direction;
use kronika_reader::{
    DiffAt, DiffPoint, OutRow, Reason, Scalar, SectionPage, SeriesDiff, Value as CellValue,
};
use kronika_registry::{ColumnClass, ColumnType, Semantics};
use serde_json::{Value, json};

use crate::anomaly::EpisodeHit;

/// Map one reader [`CellValue`] to its JSON form (see the API contract).
pub(crate) fn value_to_json(value: &CellValue) -> Value {
    match value {
        CellValue::Null => Value::Null,
        CellValue::I64(n) => (*n).into(),
        CellValue::U64(n) => (*n).into(),
        CellValue::F64(n) => (*n).into(),
        CellValue::Bool(b) => (*b).into(),
        CellValue::Ts(t) => (*t).into(),
        CellValue::Str(s) => Value::String(s.clone()),
        CellValue::Blob {
            text,
            full_len,
            truncated,
        } => json!({ "text": text.as_str(), "full_len": *full_len, "truncated": *truncated }),
        CellValue::ListI32(items) => Value::from(items.clone()),
    }
}

/// Shape one output row as a JSON object keyed by column name.
pub(crate) fn row_to_json(row: &OutRow) -> Value {
    let object = row
        .iter()
        .map(|(name, value)| (name.clone(), value_to_json(value)))
        .collect();
    Value::Object(object)
}

/// Shape a [`SectionPage`] as the `/v1/section` response body.
pub(crate) fn page_to_json(page: &SectionPage) -> Value {
    let rows: Vec<Value> = page.rows.iter().map(row_to_json).collect();
    let gaps: Vec<Value> = page
        .gaps
        .iter()
        .map(|gap| json!({ "from": gap.from, "to": gap.to }))
        .collect();
    let next_cursor = page
        .next_cursor
        .as_ref()
        .map_or(Value::Null, |cursor| Value::String(cursor.encode()));
    json!({
        "section": page.section,
        "source_id": page.source_id,
        "rows": rows,
        "gaps": gaps,
        "next_cursor": next_cursor,
    })
}

/// Shape a section's diff over a window: an array of per-entity series, each
/// with its identity key and a point list per cumulative column.
pub(crate) fn series_diff_to_json(identity: &[&str], series: &[SeriesDiff]) -> Value {
    let out = series
        .iter()
        .map(|s| {
            let key: serde_json::Map<String, Value> = identity
                .iter()
                .zip(&s.key)
                .map(|(name, value)| ((*name).to_owned(), value_to_json(value)))
                .collect();
            let columns: serde_json::Map<String, Value> = s
                .columns
                .iter()
                .map(|c| {
                    let points = c.points.iter().map(point_to_json).collect();
                    (c.name.clone(), Value::Array(points))
                })
                .collect();
            json!({ "key": key, "columns": columns })
        })
        .collect();
    Value::Array(out)
}

fn point_to_json(at: &DiffAt) -> Value {
    match at.point {
        DiffPoint::Value {
            delta,
            rate,
            dt_micros,
        } => json!({
            "ts": at.ts,
            "delta": scalar_to_json(delta),
            "rate": finite(rate),
            "dt_micros": dt_micros,
        }),
        DiffPoint::NoData { reason } => json!({
            "ts": at.ts,
            "nodata": reason_name(reason),
        }),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a delta above i64::MAX is astronomically rare for a real counter; \
              the lossy f64 fallback keeps the response numeric"
)]
fn scalar_to_json(scalar: Scalar) -> Value {
    match scalar {
        Scalar::Int(v) => i64::try_from(v).map_or_else(|_| finite(v as f64), Value::from),
        Scalar::Float(v) => finite(v),
    }
}

/// A finite `f64` as a JSON number, or `null` for NaN/infinity, which JSON
/// cannot represent.
fn finite(v: f64) -> Value {
    if v.is_finite() { v.into() } else { Value::Null }
}

const fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::Reset => "reset",
        Reason::Gap => "gap",
        Reason::FirstPoint => "first_point",
        Reason::Anomaly => "anomaly",
        Reason::NotCollected => "not_collected",
    }
}

/// Shape one ranked anomaly episode: the series it belongs to, the scored
/// column, the episode interval, and every number behind the peak verdict.
pub(crate) fn episode_to_json(section: &str, identity: &[&'static str], hit: &EpisodeHit) -> Value {
    let series: serde_json::Map<String, Value> = identity
        .iter()
        .zip(&hit.key)
        .map(|(name, value)| ((*name).to_owned(), value_to_json(value)))
        .collect();
    let peak = hit.episode.peak;
    json!({
        "section": section,
        "series": series,
        "column": hit.column,
        "start": hit.episode.start,
        "end": hit.episode.end,
        "peak_ts": hit.episode.peak_ts,
        "direction": direction_name(peak.dir),
        "peak": {
            "m": finite(peak.m),
            "med_cur": finite(peak.med_cur),
            "med_ref": finite(peak.med_ref),
            "mad_ref": finite(peak.mad_ref),
            "sigma_used": finite(peak.sigma_used),
            "n_cur": peak.n_cur,
            "n_ref": peak.n_ref,
        },
    })
}

/// Stable wire name for a deviation direction.
const fn direction_name(dir: Direction) -> &'static str {
    match dir {
        Direction::Up => "up",
        Direction::Down => "down",
        Direction::Flat => "flat",
    }
}

/// Stable wire name for a column's on-disk type.
pub(crate) const fn column_type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::I8 => "i8",
        ColumnType::I16 => "i16",
        ColumnType::I32 => "i32",
        ColumnType::I64 => "i64",
        ColumnType::U8 => "u8",
        ColumnType::U16 => "u16",
        ColumnType::U32 => "u32",
        ColumnType::U64 => "u64",
        ColumnType::F32 => "f32",
        ColumnType::F64 => "f64",
        ColumnType::Bool => "bool",
        ColumnType::Ts => "ts",
        ColumnType::StrId => "str",
        ColumnType::ListI32 => "list_i32",
    }
}

/// Stable wire name for a column's role: cumulative / gauge / label / timestamp.
pub(crate) const fn column_class_name(class: ColumnClass) -> &'static str {
    match class {
        ColumnClass::Cumulative => "c",
        ColumnClass::Gauge => "g",
        ColumnClass::Label => "l",
        ColumnClass::Timestamp => "t",
    }
}

/// Stable wire name for a section's collection semantics.
pub(crate) const fn semantics_name(semantics: Semantics) -> &'static str {
    match semantics {
        Semantics::SnapshotFull => "snapshot_full",
        Semantics::ConditionalFull => "conditional_full",
        Semantics::EventStream => "event_stream",
        Semantics::Changed => "changed",
        Semantics::OnChange => "on_change",
    }
}

#[cfg(test)]
mod tests {
    use kronika_reader::Value as CellValue;

    use super::value_to_json;

    #[test]
    fn value_to_json_maps_every_variant() {
        assert_eq!(
            value_to_json(&CellValue::Null),
            serde_json::json!(null),
            "null"
        );
        assert_eq!(
            value_to_json(&CellValue::I64(-5)),
            serde_json::json!(-5),
            "i64"
        );
        assert_eq!(
            value_to_json(&CellValue::U64(5)),
            serde_json::json!(5),
            "u64"
        );
        assert_eq!(
            value_to_json(&CellValue::F64(1.5)),
            serde_json::json!(1.5),
            "f64"
        );
        assert_eq!(
            value_to_json(&CellValue::Bool(true)),
            serde_json::json!(true),
            "bool"
        );
        assert_eq!(
            value_to_json(&CellValue::Ts(1_234)),
            serde_json::json!(1_234),
            "ts serializes as a number"
        );
        assert_eq!(
            value_to_json(&CellValue::Str("x".to_owned())),
            serde_json::json!("x"),
            "str"
        );
        assert_eq!(
            value_to_json(&CellValue::Blob {
                text: "ab".to_owned(),
                full_len: 10,
                truncated: true,
            }),
            serde_json::json!({ "text": "ab", "full_len": 10, "truncated": true }),
            "blob carries text, full_len and truncated"
        );
        assert_eq!(
            value_to_json(&CellValue::ListI32(vec![1, 2, 3])),
            serde_json::json!([1, 2, 3]),
            "list of i32"
        );
    }

    #[test]
    fn series_diff_to_json_shapes_keys_columns_and_reasons() {
        use kronika_reader::{ColumnDiff, DiffAt, DiffPoint, Reason, Scalar, SeriesDiff};

        use super::series_diff_to_json;

        let series = vec![SeriesDiff {
            key: vec![CellValue::I64(42)],
            columns: vec![ColumnDiff {
                name: "calls".to_owned(),
                points: vec![
                    DiffAt {
                        ts: 1_000,
                        point: DiffPoint::NoData {
                            reason: Reason::FirstPoint,
                        },
                    },
                    DiffAt {
                        ts: 3_000,
                        point: DiffPoint::Value {
                            delta: Scalar::Int(10),
                            rate: 5.0,
                            dt_micros: 2_000,
                        },
                    },
                ],
            }],
        }];

        assert_eq!(
            series_diff_to_json(&["queryid"], &series),
            serde_json::json!([{
                "key": { "queryid": 42 },
                "columns": {
                    "calls": [
                        { "ts": 1_000, "nodata": "first_point" },
                        { "ts": 3_000, "delta": 10, "rate": 5.0, "dt_micros": 2_000 },
                    ]
                }
            }])
        );
    }
}
