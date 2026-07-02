//! Generic, column-name-addressable section decode.
//!
//! [`decode_rows`] turns any registered section into `Vec<Row>`, where a [`Row`]
//! maps each contract column name to a [`Cell`]. This is the primitive the BDD
//! harness uses to assert an arbitrary section's rows by column name, without a
//! per-metric typed struct. `StrId` cells stay as the raw `u64` id; the caller
//! resolves them through the segment dictionary.

use std::collections::BTreeMap;

use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};

use crate::codec::{CodecError, decode_batches};
use crate::contract::{ColumnType, TypeContract};
use crate::{VerifiedSection, registry};

/// One decoded section value, addressed by column name.
///
/// The variants mirror [`ColumnType`]: every on-disk column type has one cell
/// kind. `Ts` carries unix microseconds; `StrId` carries the raw dictionary id
/// (not the resolved bytes). `Null` is a `NULL` cell in a nullable column.
/// `ListI32` has no column type in the current registry; it is reserved for a
/// future list column so the harness does not need a breaking change to gain it.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// Signed 16-bit integer (also carries `I8`).
    I16(i16),
    /// Signed 32-bit integer.
    I32(i32),
    /// Signed 64-bit integer.
    I64(i64),
    /// Unsigned 32-bit integer (also carries `U8`/`U16`).
    U32(u32),
    /// Unsigned 64-bit integer.
    U64(u64),
    /// 64-bit float (also carries `F32`, widened).
    F64(f64),
    /// Boolean.
    Bool(bool),
    /// Timestamp, unix microseconds.
    Ts(i64),
    /// A dictionary id; resolve through the segment dictionary for the string.
    StrId(u64),
    /// A list of signed 32-bit integers (reserved; no current column uses it).
    ListI32(Vec<i32>),
    /// A `NULL` in a nullable column.
    Null,
}

/// A decoded section row: column name to [`Cell`], in contract column order.
pub type Row = BTreeMap<&'static str, Cell>;

/// Decode a verified section into generic, column-addressable rows.
///
/// The contract is selected by `type_id`. Each row maps every contract column
/// name to a [`Cell`]; `StrId` columns keep the raw dictionary id.
///
/// # Errors
///
/// Returns [`CodecError`] for an unknown `type_id`, a schema mismatch, a cap
/// breach, or a Parquet decode failure — the same failures as [`decode_any`].
///
/// [`decode_any`]: crate::decode_any
pub fn decode_rows(type_id: u32, section: VerifiedSection) -> Result<Vec<Row>, CodecError> {
    let contract = registry()
        .iter()
        .find(|contract| contract.type_id.get() == type_id)
        .ok_or(CodecError::UnknownType { type_id })?;
    let bytes_in = section.len();
    decode_rows_with(contract, section).map_err(|source| CodecError::Section {
        type_id,
        bytes_in,
        source: Box::new(source),
    })
}

/// Decode against an explicit contract, without the [`CodecError::Section`] wrap.
fn decode_rows_with(
    contract: &TypeContract,
    section: VerifiedSection,
) -> Result<Vec<Row>, CodecError> {
    let decoded = decode_batches(contract, section)?;
    let mut rows: Vec<Row> = Vec::with_capacity(decoded.stats.rows);
    for batch in &decoded.batches {
        for i in 0..batch.num_rows() {
            let mut row = Row::new();
            for column in contract.columns {
                let array = batch
                    .column_by_name(column.name)
                    .ok_or(CodecError::MissingColumn { name: column.name })?;
                row.insert(
                    column.name,
                    cell_at(array.as_ref(), column.ty, column.name, i)?,
                );
            }
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Read cell `i` of `array` as a [`Cell`], per the column's [`ColumnType`].
fn cell_at(
    array: &dyn Array,
    ty: ColumnType,
    name: &'static str,
    i: usize,
) -> Result<Cell, CodecError> {
    if array.is_null(i) {
        return Ok(Cell::Null);
    }
    let cell = match ty {
        ColumnType::I8 => Cell::I16(i16::from(typed::<Int8Array>(array, name)?.value(i))),
        ColumnType::I16 => Cell::I16(typed::<Int16Array>(array, name)?.value(i)),
        ColumnType::I32 => Cell::I32(typed::<Int32Array>(array, name)?.value(i)),
        ColumnType::I64 => Cell::I64(typed::<Int64Array>(array, name)?.value(i)),
        ColumnType::U8 => Cell::U32(u32::from(typed::<UInt8Array>(array, name)?.value(i))),
        ColumnType::U16 => Cell::U32(u32::from(typed::<UInt16Array>(array, name)?.value(i))),
        ColumnType::U32 => Cell::U32(typed::<UInt32Array>(array, name)?.value(i)),
        ColumnType::U64 => Cell::U64(typed::<UInt64Array>(array, name)?.value(i)),
        ColumnType::F32 => Cell::F64(f64::from(typed::<Float32Array>(array, name)?.value(i))),
        ColumnType::F64 => Cell::F64(typed::<Float64Array>(array, name)?.value(i)),
        ColumnType::Bool => Cell::Bool(typed::<BooleanArray>(array, name)?.value(i)),
        ColumnType::Ts => Cell::Ts(typed::<Int64Array>(array, name)?.value(i)),
        ColumnType::StrId => Cell::StrId(typed::<UInt64Array>(array, name)?.value(i)),
    };
    Ok(cell)
}

/// Downcast `array` to the concrete Arrow array the column type maps to.
fn typed<'a, A: Array + 'static>(
    array: &'a dyn Array,
    name: &'static str,
) -> Result<&'a A, CodecError> {
    array
        .as_any()
        .downcast_ref::<A>()
        .ok_or(CodecError::ColumnType { name })
}

#[cfg(test)]
mod tests {
    use super::{Cell, decode_rows};
    use crate::pg_stat_archiver::PgStatArchiver;
    use crate::{Section, StrId, Ts, VerifiedSection};

    #[test]
    fn roundtrips_every_cell_kind_through_the_archiver_section() {
        // pg_stat_archiver covers i64 counters, Ts gauges, nullable Ts, and
        // nullable StrId labels in one contract.
        let want = vec![
            PgStatArchiver {
                ts: Ts(1_700_000_000_000_000),
                archived_count: 42,
                last_archived_wal: Some(StrId(7)),
                last_archived_time: Some(Ts(1_699_999_999_000_000)),
                failed_count: 3,
                last_failed_wal: None,
                last_failed_time: None,
                stats_reset: Some(Ts(1_600_000_000_000_000)),
            },
            PgStatArchiver {
                ts: Ts(1_700_000_001_000_000),
                archived_count: 0,
                last_archived_wal: None,
                last_archived_time: None,
                failed_count: 0,
                last_failed_wal: Some(StrId(9)),
                last_failed_time: Some(Ts(1_699_999_998_000_000)),
                stats_reset: None,
            },
        ];
        let bytes = PgStatArchiver::encode(&want).expect("encode");
        let rows =
            decode_rows(1_008_001, VerifiedSection::for_test(bytes.into())).expect("decode_rows");
        assert_eq!(rows.len(), 2, "two rows decode back");

        // Rows are sorted by the `ts` sort key, so the first encoded row (lower
        // ts) is first.
        let first = &rows[0];
        assert_eq!(first["ts"], Cell::Ts(1_700_000_000_000_000), "Ts cell");
        assert_eq!(first["archived_count"], Cell::I64(42), "I64 counter cell");
        assert_eq!(
            first["last_archived_wal"],
            Cell::StrId(7),
            "StrId keeps the raw id, unresolved"
        );
        assert_eq!(
            first["last_archived_time"],
            Cell::Ts(1_699_999_999_000_000),
            "present nullable Ts"
        );
        assert_eq!(
            first["last_failed_wal"],
            Cell::Null,
            "absent nullable StrId decodes to Null, distinct from a zero id"
        );
        assert_eq!(first["stats_reset"], Cell::Ts(1_600_000_000_000_000));

        let second = &rows[1];
        assert_eq!(second["archived_count"], Cell::I64(0));
        assert_eq!(second["last_archived_wal"], Cell::Null);
        assert_eq!(second["last_failed_wal"], Cell::StrId(9));
        assert_eq!(second["stats_reset"], Cell::Null, "absent nullable Ts");
    }

    #[test]
    fn rejects_an_unregistered_type() {
        assert!(
            decode_rows(2_999_999, VerifiedSection::for_test(bytes::Bytes::new())).is_err(),
            "an unregistered type_id has no contract to decode against"
        );
    }

    // The archiver contract only exercises Ts/I64/StrId/Null. These tests drive
    // `cell_at` directly over every other column type, since no registered
    // contract carries a small integer, unsigned, float, or bool column.
    #[test]
    fn cell_at_maps_each_column_type_to_its_widened_cell() {
        use super::cell_at;
        use crate::ColumnType;
        use arrow_array::{
            BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
            Int64Array, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
        };

        assert_eq!(
            cell_at(&Int8Array::from(vec![-5_i8]), ColumnType::I8, "c", 0).unwrap(),
            Cell::I16(-5),
            "i8 widens to I16"
        );
        assert_eq!(
            cell_at(&Int16Array::from(vec![-300_i16]), ColumnType::I16, "c", 0).unwrap(),
            Cell::I16(-300)
        );
        assert_eq!(
            cell_at(&Int32Array::from(vec![70_000_i32]), ColumnType::I32, "c", 0).unwrap(),
            Cell::I32(70_000)
        );
        assert_eq!(
            cell_at(&UInt8Array::from(vec![200_u8]), ColumnType::U8, "c", 0).unwrap(),
            Cell::U32(200),
            "u8 widens to U32"
        );
        assert_eq!(
            cell_at(
                &UInt16Array::from(vec![60_000_u16]),
                ColumnType::U16,
                "c",
                0
            )
            .unwrap(),
            Cell::U32(60_000)
        );
        assert_eq!(
            cell_at(
                &UInt32Array::from(vec![4_000_000_000_u32]),
                ColumnType::U32,
                "c",
                0
            )
            .unwrap(),
            Cell::U32(4_000_000_000)
        );
        assert_eq!(
            cell_at(
                &UInt64Array::from(vec![18_000_000_000_000_000_000_u64]),
                ColumnType::U64,
                "c",
                0
            )
            .unwrap(),
            Cell::U64(18_000_000_000_000_000_000)
        );
        assert_eq!(
            cell_at(&Float32Array::from(vec![1.5_f32]), ColumnType::F32, "c", 0).unwrap(),
            Cell::F64(1.5),
            "f32 widens to F64"
        );
        assert_eq!(
            cell_at(&Float64Array::from(vec![2.25_f64]), ColumnType::F64, "c", 0).unwrap(),
            Cell::F64(2.25)
        );
        assert_eq!(
            cell_at(&BooleanArray::from(vec![true]), ColumnType::Bool, "c", 0).unwrap(),
            Cell::Bool(true)
        );
        assert_eq!(
            cell_at(
                &Int64Array::from(vec![Some(9_i64)]),
                ColumnType::I64,
                "c",
                0
            )
            .unwrap(),
            Cell::I64(9)
        );
    }

    #[test]
    fn cell_at_errors_when_the_arrow_type_is_wrong_for_the_column() {
        use super::cell_at;
        use crate::ColumnType;
        use arrow_array::Int32Array;

        // A column declared U64 but backed by an Int32 array cannot downcast.
        let err = cell_at(&Int32Array::from(vec![1_i32]), ColumnType::U64, "c", 0)
            .expect_err("a type mismatch is an error, not a panic");
        assert!(
            matches!(err, crate::CodecError::ColumnType { name: "c" }),
            "the error names the offending column"
        );
    }
}
