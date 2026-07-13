use kronika_reader::{OutRow, SectionPage, Value as CellValue};
use kronika_registry::{ColumnClass, ColumnType, Semantics};
use serde_json::{Value, json};

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
}
