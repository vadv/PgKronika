//! Canonical Arrow ordering and the physical Parquet profile used at seal time.
//!
//! Collection-window sections favor cheap appends. Segment completion decodes
//! those bounded sections, coalesces rows by registered `type_id`, applies a
//! total order, and writes one compact body. Keeping the schema lookup,
//! comparator, resource estimate, and Parquet properties beside the registry
//! prevents the storage writer from inventing type-specific behavior.

use std::cmp::Ordering;
use std::sync::LazyLock;

use arrow_array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array,
    Int64Array, ListArray, RecordBatch, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow_select::concat::concat_batches;
use arrow_select::take::take;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::{EnabledStatistics, WriterProperties};

use crate::codec::{
    MAX_LIST_I32_VALUES_PER_SECTION, check_row_cap, schema_matches, validate_list_i32_batch,
};
use crate::{CodecError, ColumnType, MAX_SECTION_BYTES, MAX_SECTION_ROWS, TypeContract, registry};

/// Zstd level used by the current sealed PGM physical profile.
pub const COMPACTION_ZSTD_LEVEL: i32 = 6;

/// Maximum target data-page size for a compact section.
pub const COMPACTION_PAGE_BYTES: usize = 1024 * 1024;

/// Default peak working-memory admission for one coalesced type.
///
/// The estimate is deliberately conservative and includes decoded input,
/// concatenation, sort/take output, indexes, and Arrow bookkeeping. The writer
/// processes one type at a time and rejects a seal before crossing this bound.
pub const COMPACTION_MEMORY_LIMIT: usize = 128 * 1024 * 1024;

/// Parquet properties for every final data and dictionary section.
static COMPACT_WRITER_PROPERTIES: LazyLock<WriterProperties> = LazyLock::new(|| {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(COMPACTION_ZSTD_LEVEL).expect("Zstd level 6 is valid"),
        ))
        .set_max_row_group_size(MAX_SECTION_ROWS)
        .set_data_page_size_limit(COMPACTION_PAGE_BYTES)
        .set_data_page_row_count_limit(MAX_SECTION_ROWS)
        .set_dictionary_enabled(false)
        .set_statistics_enabled(EnabledStatistics::None)
        .set_offset_index_disabled(true)
        .set_created_by(String::new())
        .build()
});

/// Return a conservative peak-byte bound for coalescing `rows` of `type_id`.
///
/// The bound covers all fixed-width values, validity bitmaps, the registry's
/// maximum allowed `List<Int32>` child population, four simultaneously live
/// Arrow representations, two row-index vectors, and per-column bookkeeping.
///
/// # Errors
///
/// Returns [`CodecError::UnknownType`] for an unregistered id,
/// [`CodecError::TooManyRows`] above the section cap, or
/// [`CodecError::SectionTooLarge`] when checked arithmetic overflows.
pub fn compaction_memory_bound(type_id: u32, rows: usize) -> Result<usize, CodecError> {
    check_row_cap(rows)?;
    let contract = contract(type_id)?;
    let mut one_copy = 0_usize;
    for column in contract.columns {
        let fixed = match column.ty {
            ColumnType::I8 | ColumnType::U8 | ColumnType::Bool => 1,
            ColumnType::I16 | ColumnType::U16 => 2,
            ColumnType::I32 | ColumnType::U32 | ColumnType::F32 => 4,
            ColumnType::I64
            | ColumnType::U64
            | ColumnType::F64
            | ColumnType::Ts
            | ColumnType::StrId => 8,
            // List offsets; child storage is added once below.
            ColumnType::ListI32 => 4,
        };
        one_copy = checked_add(
            one_copy,
            checked_mul(rows, fixed, "compaction fixed-width estimate")?,
            "compaction column estimate",
        )?;
        if column.nullable {
            one_copy = checked_add(one_copy, rows.div_ceil(8), "compaction validity estimate")?;
        }
    }
    if contract
        .columns
        .iter()
        .any(|column| column.ty == ColumnType::ListI32)
    {
        one_copy = checked_add(
            one_copy,
            checked_mul(
                MAX_LIST_I32_VALUES_PER_SECTION,
                size_of::<i32>(),
                "compaction list-child estimate",
            )?,
            "compaction list estimate",
        )?;
    }

    let live_copies = checked_mul(one_copy, 4, "compaction live Arrow copies")?;
    let indexes = checked_mul(
        checked_mul(rows, size_of::<u32>(), "compaction row index")?,
        2,
        "compaction row indexes",
    )?;
    let bookkeeping = checked_add(
        1024 * 1024,
        checked_mul(
            contract.columns.len(),
            4096,
            "compaction column bookkeeping",
        )?,
        "compaction bookkeeping",
    )?;
    checked_add(
        checked_add(live_copies, indexes, "compaction data and indexes")?,
        bookkeeping,
        "compaction total estimate",
    )
}

/// Concatenate registered Arrow batches and put their rows in canonical order.
///
/// The order starts with every declared sort-key column, then uses every
/// remaining column in schema order as a complete tie-break. `NULL` sorts
/// before a value. Floating-point tie-breaks compare their exact IEEE bit
/// patterns, preserving NaN payloads and signed zero. Equal physical rows
/// retain their multiplicity; their relative order cannot affect bytes.
///
/// # Errors
///
/// Returns [`CodecError`] for an unknown type, schema mismatch, row/list cap,
/// or Arrow operation failure.
pub fn canonicalize_batches(
    type_id: u32,
    batches: &[RecordBatch],
) -> Result<RecordBatch, CodecError> {
    let contract = contract(type_id)?;
    let schema = crate::arrow_schema(contract);
    let mut rows = 0_usize;
    for batch in batches {
        if !schema_matches(batch.schema().as_ref(), contract) {
            return Err(CodecError::SchemaMismatch);
        }
        rows = rows
            .checked_add(batch.num_rows())
            .ok_or(CodecError::TooManyRows {
                rows: usize::MAX,
                max: MAX_SECTION_ROWS,
            })?;
        check_row_cap(rows)?;
        validate_lists(batch, contract)?;
    }
    if batches.is_empty() {
        return Ok(RecordBatch::new_empty(schema));
    }
    let merged = concat_batches(&schema, batches)?;
    canonical_sort(&merged, contract)
}

/// Encode one canonical compact Parquet body.
///
/// The caller may pass an unsorted batch; this function validates the schema
/// and applies the same total order before encoding. The physical profile is
/// PLAIN values, Zstd level 6, one bounded row group, page indexes and
/// statistics disabled, and no Arrow schema metadata.
///
/// # Errors
///
/// Returns [`CodecError`] for schema/cap violations, Arrow/Parquet failures, or
/// a final body above [`MAX_SECTION_BYTES`].
pub fn encode_compact_batch(type_id: u32, batch: &RecordBatch) -> Result<Vec<u8>, CodecError> {
    let canonical = canonicalize_batches(type_id, std::slice::from_ref(batch))?;
    encode_canonical_batch(&canonical)
}

/// Encode a row-capped batch already in its required canonical order.
///
/// This is the physical-profile entry point for dictionary schemas, which are
/// part of PGM but deliberately not registry `TypeContract`s. Callers are
/// responsible for exact schema validation and ordering before this function.
///
/// # Errors
///
/// Returns [`CodecError`] for a row-cap, Arrow/Parquet, or final byte-cap
/// violation.
pub fn encode_compact_ordered_batch(batch: &RecordBatch) -> Result<Vec<u8>, CodecError> {
    check_row_cap(batch.num_rows())?;
    let options = ArrowWriterOptions::new()
        .with_properties(COMPACT_WRITER_PROPERTIES.clone())
        .with_skip_arrow_metadata(true);
    let mut body = Vec::with_capacity(4096);
    let mut writer = ArrowWriter::try_new_with_options(&mut body, batch.schema(), options)?;
    writer.write(batch)?;
    writer.close()?;
    if body.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: body.len(),
            max: MAX_SECTION_BYTES,
        });
    }
    Ok(body)
}

/// Encode a batch already validated and canonically sorted by this module.
fn encode_canonical_batch(batch: &RecordBatch) -> Result<Vec<u8>, CodecError> {
    encode_compact_ordered_batch(batch)
}

/// Build the complete comparison-column sequence once per sort.
fn canonical_sort(batch: &RecordBatch, contract: &TypeContract) -> Result<RecordBatch, CodecError> {
    if batch.num_rows() <= 1 {
        return Ok(batch.clone());
    }
    let mut order = Vec::with_capacity(contract.columns.len());
    for &name in contract.sort_key {
        let index = contract
            .columns
            .iter()
            .position(|column| column.name == name)
            .ok_or(CodecError::MissingColumn { name })?;
        if !order.contains(&index) {
            order.push(index);
        }
    }
    for index in 0..contract.columns.len() {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    let comparable = contract
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            ComparableColumn::new(batch.column(index).as_ref(), column.ty, column.name)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut indices: Vec<u32> = (0..batch.num_rows())
        .map(|index| {
            u32::try_from(index).map_err(|_overflow| CodecError::TooManyRows {
                rows: batch.num_rows(),
                max: MAX_SECTION_ROWS,
            })
        })
        .collect::<Result<_, _>>()?;
    indices.sort_unstable_by(|left, right| {
        let left = *left as usize;
        let right = *right as usize;
        for &column in &order {
            let ordering = comparable[column].compare(left, right);
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    });
    let indices = UInt32Array::from(indices);
    let columns = batch
        .columns()
        .iter()
        .map(|column| take(column.as_ref(), &indices, None))
        .collect::<Result<Vec<ArrayRef>, _>>()?;
    Ok(RecordBatch::try_new(batch.schema(), columns)?)
}

/// One schema-checked Arrow column with an infallible row comparator.
enum ComparableColumn<'a> {
    I8(&'a Int8Array),
    I16(&'a Int16Array),
    I32(&'a Int32Array),
    I64(&'a Int64Array),
    U8(&'a UInt8Array),
    U16(&'a UInt16Array),
    U32(&'a UInt32Array),
    U64(&'a UInt64Array),
    F32(&'a Float32Array),
    F64(&'a Float64Array),
    Bool(&'a BooleanArray),
    ListI32(&'a ListArray),
}

impl<'a> ComparableColumn<'a> {
    fn new(array: &'a dyn Array, ty: ColumnType, name: &'static str) -> Result<Self, CodecError> {
        macro_rules! downcast {
            ($array:ty, $variant:ident) => {
                array
                    .as_any()
                    .downcast_ref::<$array>()
                    .map(Self::$variant)
                    .ok_or(CodecError::ColumnType { name })
            };
        }
        match ty {
            ColumnType::I8 => downcast!(Int8Array, I8),
            ColumnType::I16 => downcast!(Int16Array, I16),
            ColumnType::I32 => downcast!(Int32Array, I32),
            ColumnType::I64 | ColumnType::Ts => downcast!(Int64Array, I64),
            ColumnType::U8 => downcast!(UInt8Array, U8),
            ColumnType::U16 => downcast!(UInt16Array, U16),
            ColumnType::U32 => downcast!(UInt32Array, U32),
            ColumnType::U64 | ColumnType::StrId => downcast!(UInt64Array, U64),
            ColumnType::F32 => downcast!(Float32Array, F32),
            ColumnType::F64 => downcast!(Float64Array, F64),
            ColumnType::Bool => downcast!(BooleanArray, Bool),
            ColumnType::ListI32 => downcast!(ListArray, ListI32),
        }
    }

    fn compare(&self, left: usize, right: usize) -> Ordering {
        let null_order = |array: &dyn Array| match (array.is_null(left), array.is_null(right)) {
            (true, true) => Some(Ordering::Equal),
            (true, false) => Some(Ordering::Less),
            (false, true) => Some(Ordering::Greater),
            (false, false) => None,
        };
        macro_rules! scalar {
            ($array:expr) => {
                null_order(*$array).unwrap_or_else(|| $array.value(left).cmp(&$array.value(right)))
            };
        }
        match self {
            Self::I8(array) => scalar!(array),
            Self::I16(array) => scalar!(array),
            Self::I32(array) => scalar!(array),
            Self::I64(array) => scalar!(array),
            Self::U8(array) => scalar!(array),
            Self::U16(array) => scalar!(array),
            Self::U32(array) => scalar!(array),
            Self::U64(array) => scalar!(array),
            Self::F32(array) => null_order(*array).unwrap_or_else(|| {
                array
                    .value(left)
                    .to_bits()
                    .cmp(&array.value(right).to_bits())
            }),
            Self::F64(array) => null_order(*array).unwrap_or_else(|| {
                array
                    .value(left)
                    .to_bits()
                    .cmp(&array.value(right).to_bits())
            }),
            Self::Bool(array) => scalar!(array),
            Self::ListI32(array) => {
                null_order(*array).unwrap_or_else(|| compare_list(array, left, right))
            }
        }
    }
}

fn compare_list(array: &ListArray, left: usize, right: usize) -> Ordering {
    let left = array.value(left);
    let right = array.value(right);
    let left = left
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("schema validation guarantees Int32 list children");
    let right = right
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("schema validation guarantees Int32 list children");
    for index in 0..left.len().min(right.len()) {
        let ordering = left.value(index).cmp(&right.value(index));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn validate_lists(batch: &RecordBatch, contract: &TypeContract) -> Result<(), CodecError> {
    for column in contract
        .columns
        .iter()
        .filter(|column| column.ty == ColumnType::ListI32)
    {
        validate_list_i32_batch(batch, column.name)?;
    }
    Ok(())
}

fn contract(type_id: u32) -> Result<&'static TypeContract, CodecError> {
    registry()
        .iter()
        .find(|contract| contract.type_id.get() == type_id)
        .ok_or(CodecError::UnknownType { type_id })
}

fn checked_mul(left: usize, right: usize, _what: &'static str) -> Result<usize, CodecError> {
    left.checked_mul(right).ok_or(CodecError::SectionTooLarge {
        len: usize::MAX,
        max: COMPACTION_MEMORY_LIMIT,
    })
}

fn checked_add(left: usize, right: usize, _what: &'static str) -> Result<usize, CodecError> {
    left.checked_add(right).ok_or(CodecError::SectionTooLarge {
        len: usize::MAX,
        max: COMPACTION_MEMORY_LIMIT,
    })
}

#[cfg(test)]
mod tests {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::basic::Encoding;

    use super::*;
    use crate::pg_stat_archiver::PgStatArchiver;
    use crate::{Section, StrId, Ts};

    fn archiver_batch(rows: &[PgStatArchiver]) -> RecordBatch {
        let body = PgStatArchiver::encode(rows).expect("encode fixture");
        crate::decode_any(
            PgStatArchiver::CONTRACT.type_id.get(),
            crate::VerifiedSection::for_test(body.into()),
        )
        .expect("decode fixture")
        .batches
        .into_iter()
        .next()
        .expect("one non-empty batch")
    }

    #[test]
    fn complete_tie_break_makes_input_order_irrelevant() {
        let a = PgStatArchiver {
            ts: Ts(7),
            archived_count: 2,
            last_archived_wal: Some(StrId(9)),
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        };
        let b = PgStatArchiver {
            archived_count: 1,
            last_archived_wal: Some(StrId(8)),
            ..a.clone()
        };
        let forward = archiver_batch(&[a.clone(), b.clone()]);
        let reverse = archiver_batch(&[b, a]);
        let forward = encode_compact_batch(1_008_001, &forward).expect("compact");
        let reverse = encode_compact_batch(1_008_001, &reverse).expect("compact");
        assert_eq!(forward, reverse, "complete tie-break yields exact bytes");
    }

    #[test]
    fn float_bits_distinguish_signed_zero_and_nan_payloads() {
        let floats = Float64Array::from(vec![
            f64::from_bits(0x7ff8_0000_0000_0002),
            -0.0,
            0.0,
            f64::from_bits(0x7ff8_0000_0000_0001),
        ]);
        let column = ComparableColumn::F64(&floats);
        let mut rows = vec![0_usize, 1, 2, 3];
        rows.sort_unstable_by(|left, right| column.compare(*left, *right));
        let bits: Vec<u64> = rows
            .iter()
            .map(|&row| floats.value(row).to_bits())
            .collect();
        assert_eq!(
            bits,
            vec![
                0,
                0x7ff8_0000_0000_0001,
                0x7ff8_0000_0000_0002,
                0x8000_0000_0000_0000
            ]
        );
    }

    #[test]
    fn compact_profile_has_one_group_plain_values_and_no_page_indexes() {
        let batch = archiver_batch(&[PgStatArchiver {
            ts: Ts(7),
            archived_count: 2,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }]);
        let body = encode_compact_batch(1_008_001, &batch).expect("compact");
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(body)).expect("metadata");
        assert_eq!(builder.metadata().num_row_groups(), 1);
        for column in builder.metadata().row_group(0).columns() {
            assert_eq!(column.column_index_length(), None);
            assert_eq!(column.offset_index_length(), None);
            assert!(
                column.encodings().contains(&Encoding::PLAIN),
                "value pages use PLAIN"
            );
            assert!(
                !column.encodings().contains(&Encoding::PLAIN_DICTIONARY)
                    && !column.encodings().contains(&Encoding::RLE_DICTIONARY),
                "Parquet dictionaries stay disabled"
            );
        }
    }

    #[test]
    fn memory_bound_is_checked_and_within_default_for_every_layout() {
        for contract in registry() {
            let bound = compaction_memory_bound(contract.type_id.get(), MAX_SECTION_ROWS)
                .expect("registered layout has a finite bound");
            assert!(
                bound <= COMPACTION_MEMORY_LIMIT,
                "{} requires {bound} bytes",
                contract.name
            );
        }
        assert!(matches!(
            compaction_memory_bound(999, 1),
            Err(CodecError::UnknownType { type_id: 999 })
        ));
    }
}
