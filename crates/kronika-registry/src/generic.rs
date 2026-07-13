//! Generic, column-name-addressable section decode.
//!
//! [`decode_rows`] turns any registered section into `Vec<Row>`, where a [`Row`]
//! carries one [`Cell`] per contract column, addressable by column name. This is
//! the primitive the BDD harness uses to assert an arbitrary section's rows by
//! column name, without a per-metric typed struct. `StrId` cells stay as the raw
//! `u64` id; the caller resolves them through the segment dictionary.

use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
    ListArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};

use crate::codec::{CodecError, decode_batches};
use crate::contract::{ColumnType, TypeContract};
use crate::{VerifiedSection, registry};

/// One decoded section value, addressed by column name.
///
/// The variants mirror [`ColumnType`]: every on-disk column type has one cell
/// kind. `Ts` carries unix microseconds; `StrId` carries the raw dictionary id
/// (not the resolved bytes). `ListI32` carries list columns such as
/// `pg_locks.blocked_by`. `Null` is a `NULL` cell in a nullable column.
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
    /// A list of signed 32-bit integers.
    ListI32(Vec<i32>),
    /// A `NULL` in a nullable column.
    Null,
}

/// A decoded section row: cells in contract column order, addressable by name.
///
/// Cells sit in a vector positionally aligned with the contract's columns, so
/// decode is a straight per-column push with no per-cell map insert. Name
/// lookups walk the contract's column list.
#[derive(Debug, Clone)]
pub struct Row {
    /// The contract the row was decoded against; names cells positionally.
    contract: &'static TypeContract,
    /// One cell per contract column, in contract column order.
    cells: Vec<Cell>,
}

impl PartialEq for Row {
    /// Rows are equal when decoded against the same registry contract (compared
    /// by address — contracts are registry statics) with equal cells.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.contract, other.contract) && self.cells == other.cells
    }
}

impl Row {
    /// Assemble a row of `cells` in `contract` column order.
    ///
    /// The decode path always supplies one cell per column; a shorter vector
    /// leaves the tail columns absent (`get` returns `None`).
    #[must_use]
    pub const fn new(contract: &'static TypeContract, cells: Vec<Cell>) -> Self {
        Self { contract, cells }
    }

    /// The cell under `name`, or `None` when the contract lacks that column.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Cell> {
        let at = self
            .contract
            .columns
            .iter()
            .position(|column| column.name == name)?;
        self.cells.get(at)
    }

    /// The contract this row was decoded against.
    #[must_use]
    pub const fn contract(&self) -> &'static TypeContract {
        self.contract
    }

    /// Cells in contract column order.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// `(column name, cell)` pairs in contract column order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Cell)> {
        self.contract
            .columns
            .iter()
            .map(|column| column.name)
            .zip(&self.cells)
    }
}

/// Decode a verified section into generic, column-addressable rows.
///
/// The contract is selected by `type_id`. Each row carries one [`Cell`] per
/// contract column; `StrId` columns keep the raw dictionary id.
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
    contract: &'static TypeContract,
    section: VerifiedSection,
) -> Result<Vec<Row>, CodecError> {
    let decoded = decode_batches(contract, section)?;
    let mut rows: Vec<Row> = Vec::with_capacity(decoded.stats.rows);
    for batch in &decoded.batches {
        let arrays: Vec<&dyn Array> = contract
            .columns
            .iter()
            .map(|column| {
                batch
                    .column_by_name(column.name)
                    .map(AsRef::as_ref)
                    .ok_or(CodecError::MissingColumn { name: column.name })
            })
            .collect::<Result<_, _>>()?;
        for i in 0..batch.num_rows() {
            let mut cells = Vec::with_capacity(contract.columns.len());
            for (column, array) in contract.columns.iter().zip(&arrays) {
                cells.push(cell_at(*array, column.ty, column.name, i)?);
            }
            rows.push(Row { contract, cells });
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
        ColumnType::ListI32 => Cell::ListI32(list_i32_at(array, name, i)?),
    };
    Ok(cell)
}

/// Read one `List<Int32>` row into an owned vector.
fn list_i32_at(array: &dyn Array, name: &'static str, i: usize) -> Result<Vec<i32>, CodecError> {
    let lists = typed::<ListArray>(array, name)?;
    let values = lists.value(i);
    let ints = values
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or(CodecError::ColumnType { name })?;
    let mut out = Vec::with_capacity(ints.len());
    for j in 0..ints.len() {
        if ints.is_null(j) {
            return Err(CodecError::NullInRequiredColumn { name });
        }
        out.push(ints.value(j));
    }
    Ok(out)
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
    use super::{Cell, Row, decode_rows};
    use crate::contract::TypeContract;
    use crate::pg_stat_archiver::PgStatArchiver;
    use crate::{Section, StrId, Ts, VerifiedSection, registry};

    /// The archiver contract straight from the registry.
    fn archiver_contract() -> &'static TypeContract {
        registry()
            .iter()
            .find(|contract| contract.type_id.get() == 1_008_001)
            .expect("archiver contract registered")
    }

    /// A full cell vector for the archiver contract: `Null` everywhere except
    /// the first column (`ts`).
    fn archiver_cells(ts: i64) -> Vec<Cell> {
        let contract = archiver_contract();
        let mut cells = vec![Cell::Null; contract.columns.len()];
        cells[0] = Cell::Ts(ts);
        cells
    }

    // ---- positional Row API ----

    #[test]
    fn row_get_resolves_each_contract_column_positionally() {
        let contract = archiver_contract();
        let cells: Vec<Cell> = (0..contract.columns.len())
            .map(|i| Cell::I64(i64::try_from(i).expect("small index")))
            .collect();
        let row = Row::new(contract, cells);
        for (i, column) in contract.columns.iter().enumerate() {
            assert_eq!(
                row.get(column.name),
                Some(&Cell::I64(i64::try_from(i).expect("small index"))),
                "column {} resolves to its position",
                column.name
            );
        }
    }

    #[test]
    fn row_get_is_none_for_a_column_outside_the_contract() {
        let row = Row::new(archiver_contract(), archiver_cells(1));
        assert_eq!(row.get("no_such_column"), None);
    }

    #[test]
    fn row_shorter_cells_leave_tail_columns_absent() {
        let contract = archiver_contract();
        let row = Row::new(contract, vec![Cell::Ts(5)]);
        assert_eq!(row.get(contract.columns[0].name), Some(&Cell::Ts(5)));
        let last = contract.columns.last().expect("archiver has columns");
        assert_eq!(row.get(last.name), None, "missing tail cell reads absent");
    }

    #[test]
    fn row_iter_follows_contract_column_order() {
        let contract = archiver_contract();
        let row = Row::new(contract, archiver_cells(1));
        let names: Vec<&str> = row.iter().map(|(name, _)| name).collect();
        let want: Vec<&str> = contract.columns.iter().map(|column| column.name).collect();
        assert_eq!(names, want);
    }

    #[test]
    fn row_iter_stops_at_the_shorter_of_columns_and_cells() {
        let contract = archiver_contract();
        let row = Row::new(contract, vec![Cell::Ts(5), Cell::I64(2)]);
        assert_eq!(row.iter().count(), 2, "iter pairs only the present cells");
    }

    #[test]
    fn rows_compare_equal_only_on_same_contract_and_cells() {
        let archiver = archiver_contract();
        let row = Row::new(archiver, archiver_cells(1));
        assert_eq!(row, Row::new(archiver, archiver_cells(1)));
        assert_ne!(row, Row::new(archiver, archiver_cells(2)), "cells differ");

        let other = registry()
            .iter()
            .find(|contract| contract.type_id.get() != 1_008_001)
            .expect("registry has more than one contract");
        let foreign = Row::new(other, archiver_cells(1));
        assert_ne!(row, foreign, "same cells under another contract differ");
    }

    #[test]
    fn decoded_row_cells_align_with_contract_columns() {
        let want = vec![PgStatArchiver {
            ts: Ts(77),
            archived_count: 5,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 1,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }];
        let bytes = PgStatArchiver::encode(&want).expect("encode");
        let rows =
            decode_rows(1_008_001, VerifiedSection::for_test(bytes.into())).expect("decode_rows");
        let row = &rows[0];
        let contract = row.contract();
        assert_eq!(contract.type_id.get(), 1_008_001, "contract travels along");
        assert_eq!(
            row.cells().len(),
            contract.columns.len(),
            "decode fills every contract column"
        );
        // The positional view and the by-name view agree cell for cell.
        for (at, column) in contract.columns.iter().enumerate() {
            assert_eq!(
                row.cells().get(at),
                row.get(column.name),
                "cell {} equal by index and by name",
                column.name
            );
        }
    }

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
        assert_eq!(
            first.get("ts"),
            Some(&Cell::Ts(1_700_000_000_000_000)),
            "Ts cell"
        );
        assert_eq!(
            first.get("archived_count"),
            Some(&Cell::I64(42)),
            "I64 counter cell"
        );
        assert_eq!(
            first.get("last_archived_wal"),
            Some(&Cell::StrId(7)),
            "StrId keeps the raw id, unresolved"
        );
        assert_eq!(
            first.get("last_archived_time"),
            Some(&Cell::Ts(1_699_999_999_000_000)),
            "present nullable Ts"
        );
        assert_eq!(
            first.get("last_failed_wal"),
            Some(&Cell::Null),
            "absent nullable StrId decodes to Null, distinct from a zero id"
        );
        assert_eq!(
            first.get("stats_reset"),
            Some(&Cell::Ts(1_600_000_000_000_000))
        );

        let second = &rows[1];
        assert_eq!(second.get("archived_count"), Some(&Cell::I64(0)));
        assert_eq!(second.get("last_archived_wal"), Some(&Cell::Null));
        assert_eq!(second.get("last_failed_wal"), Some(&Cell::StrId(9)));
        assert_eq!(
            second.get("stats_reset"),
            Some(&Cell::Null),
            "absent nullable Ts"
        );
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
    fn cell_at_maps_list_i32_to_a_list_cell() {
        use super::cell_at;
        use crate::ColumnType;
        use arrow_array::ListArray;
        use arrow_array::types::Int32Type;

        let array = ListArray::from_iter_primitive::<Int32Type, _, _>([
            Some(vec![Some(1), Some(2)]),
            Some(vec![]),
        ]);
        assert_eq!(
            cell_at(&array, ColumnType::ListI32, "c", 0).unwrap(),
            Cell::ListI32(vec![1, 2])
        );
        assert_eq!(
            cell_at(&array, ColumnType::ListI32, "c", 1).unwrap(),
            Cell::ListI32(Vec::new())
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
