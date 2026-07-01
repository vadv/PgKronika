//! Shared code for Parquet section codecs.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, LazyLock};

use arrow_array::{
    Array, ArrayRef, ArrowPrimitiveType, BooleanArray, PrimitiveArray, RecordBatch,
    RecordBatchReader,
};
use arrow_ord::sort::{SortColumn, lexsort_to_indices};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use arrow_select::take::take;
use bytes::Bytes;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::contract::{ColumnType, TypeContract};

pub mod bgwriter_checkpointer;
pub mod instance_metadata;
pub mod pg_prepared_xacts;
pub mod pg_stat_activity;
pub mod pg_stat_archiver;
pub mod pg_stat_database;
pub mod pg_stat_io;
pub mod pg_stat_progress_vacuum;
pub mod pg_stat_user_indexes;
pub mod pg_stat_user_tables;
pub mod pg_stat_wal;
pub mod replication_instance;
pub mod reset_metadata;

/// Maximum rows in one snapshot section.
///
/// Encode and decode reject larger sections before materializing rows.
pub const MAX_SECTION_ROWS: usize = 65_536;

/// Maximum accepted section byte length on decode.
///
/// Checked before Parquet metadata is parsed.
pub const MAX_SECTION_BYTES: usize = 8 * 1024 * 1024;

/// Maximum Parquet row groups accepted in one snapshot section.
///
/// Decode rejects excessive row groups before reading column data.
pub const MAX_ROW_GROUPS: usize = 16;

/// Why a section failed to encode or decode.
#[derive(Debug)]
pub enum CodecError {
    /// An Arrow operation failed (building the record batch).
    Arrow(arrow_schema::ArrowError),
    /// A Parquet operation failed (writing or reading the file).
    Parquet(parquet::errors::ParquetError),
    /// More rows than [`MAX_SECTION_ROWS`] were given to encode, or a
    /// section claims or holds more on decode.
    TooManyRows {
        /// The row count that exceeded the cap.
        rows: usize,
        /// The enforced cap.
        max: usize,
    },
    /// Parquet metadata reports a negative or unrepresentable row count.
    InvalidRowCount {
        /// The raw `num_rows` from Parquet metadata.
        raw: i64,
    },
    /// The section byte length is above [`MAX_SECTION_BYTES`].
    SectionTooLarge {
        /// The byte length that exceeded the cap.
        len: usize,
        /// The enforced cap.
        max: usize,
    },
    /// The section has more than [`MAX_ROW_GROUPS`] row groups.
    TooManyRowGroups {
        /// The row-group count that exceeded the cap.
        groups: usize,
        /// The enforced cap.
        max: usize,
    },
    /// A column required by the contract is absent from the decoded file.
    MissingColumn {
        /// The missing column name.
        name: &'static str,
    },
    /// A decoded column has a different Arrow type than the contract.
    ColumnType {
        /// The column name.
        name: &'static str,
    },
    /// A `NULL` appeared in a column the contract declares non-nullable.
    ///
    /// Required columns must not decode a missing value as zero.
    NullInRequiredColumn {
        /// The column name.
        name: &'static str,
    },
    /// No registered type has the requested `type_id`.
    UnknownType {
        /// The unrecognized id.
        type_id: u32,
    },
    /// A decoded section's schema does not match the contract it was decoded
    /// against (column set, order, types, or nullability).
    SchemaMismatch,
    /// A section's computed CRC does not match the catalog's, so the bytes are
    /// corrupt (or not the section the catalog points at).
    SectionCrcMismatch {
        /// The CRC the catalog claims.
        expected: u32,
        /// The CRC computed over the bytes.
        got: u32,
    },
    /// A decode failed for a known `type_id`.
    Section {
        /// The section's `type_id`.
        type_id: u32,
        /// Input section bytes.
        bytes_in: usize,
        /// The underlying decode error.
        source: Box<Self>,
    },
}

impl CodecError {
    /// The section `type_id` this error is about, if known.
    #[must_use]
    pub const fn section_type_id(&self) -> Option<u32> {
        match self {
            Self::UnknownType { type_id } | Self::Section { type_id, .. } => Some(*type_id),
            // Add new type-tagged variants here so failure metrics keep their
            // `{type_id}` label.
            _ => None,
        }
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arrow(err) => write!(f, "arrow: {err}"),
            Self::Parquet(err) => write!(f, "parquet: {err}"),
            Self::TooManyRows { rows, max } => {
                write!(f, "section has {rows} rows, above the cap of {max}")
            }
            Self::InvalidRowCount { raw } => {
                write!(f, "section claims an invalid row count of {raw}")
            }
            Self::SectionTooLarge { len, max } => {
                write!(f, "section is {len} bytes, above the cap of {max}")
            }
            Self::TooManyRowGroups { groups, max } => {
                write!(f, "section has {groups} row groups, above the cap of {max}")
            }
            Self::MissingColumn { name } => write!(f, "decoded section lacks column {name:?}"),
            Self::ColumnType { name } => write!(f, "decoded column {name:?} has the wrong type"),
            Self::NullInRequiredColumn { name } => {
                write!(
                    f,
                    "decoded column {name:?} has a NULL but the contract forbids it"
                )
            }
            Self::UnknownType { type_id } => write!(f, "no registered type has id {type_id}"),
            Self::SchemaMismatch => {
                write!(f, "decoded section schema does not match the contract")
            }
            Self::SectionCrcMismatch { expected, got } => {
                write!(
                    f,
                    "section CRC {got:#010x} does not match the catalog's {expected:#010x}"
                )
            }
            Self::Section {
                type_id,
                bytes_in,
                source,
            } => write!(f, "decoding type {type_id} ({bytes_in} bytes): {source}"),
        }
    }
}

impl Error for CodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Arrow(err) => Some(err),
            Self::Parquet(err) => Some(err),
            Self::TooManyRows { .. }
            | Self::InvalidRowCount { .. }
            | Self::SectionTooLarge { .. }
            | Self::TooManyRowGroups { .. }
            | Self::MissingColumn { .. }
            | Self::ColumnType { .. }
            | Self::NullInRequiredColumn { .. }
            | Self::UnknownType { .. }
            | Self::SchemaMismatch
            | Self::SectionCrcMismatch { .. } => None,
            Self::Section { source, .. } => Some(source.as_ref()),
        }
    }
}

impl From<arrow_schema::ArrowError> for CodecError {
    fn from(err: arrow_schema::ArrowError) -> Self {
        Self::Arrow(err)
    }
}

impl From<parquet::errors::ParquetError> for CodecError {
    fn from(err: parquet::errors::ParquetError) -> Self {
        Self::Parquet(err)
    }
}

/// Arrow schema of a section, in contract column order.
#[must_use]
pub fn arrow_schema(contract: &TypeContract) -> SchemaRef {
    static CACHE: LazyLock<HashMap<u32, SchemaRef>> = LazyLock::new(|| {
        crate::registry()
            .iter()
            .map(|contract| (contract.type_id.get(), build_arrow_schema(contract)))
            .collect()
    });
    CACHE
        .get(&contract.type_id.get())
        .map_or_else(|| build_arrow_schema(contract), Arc::clone)
}

fn build_arrow_schema(contract: &TypeContract) -> SchemaRef {
    let fields: Vec<Field> = contract
        .columns
        .iter()
        .map(|column| {
            let data_type = match column.ty {
                ColumnType::I8 => DataType::Int8,
                ColumnType::I16 => DataType::Int16,
                ColumnType::I32 => DataType::Int32,
                ColumnType::I64 | ColumnType::Ts => DataType::Int64,
                ColumnType::U8 => DataType::UInt8,
                ColumnType::U16 => DataType::UInt16,
                ColumnType::U32 => DataType::UInt32,
                ColumnType::U64 | ColumnType::StrId => DataType::UInt64,
                ColumnType::F32 => DataType::Float32,
                ColumnType::F64 => DataType::Float64,
                ColumnType::Bool => DataType::Boolean,
            };
            Field::new(column.name, data_type, column.nullable)
        })
        .collect();
    Arc::new(Schema::new(fields))
}

/// Whether a decoded file's schema matches the contract.
fn schema_matches(got: &Schema, contract: &TypeContract) -> bool {
    let want = arrow_schema(contract);
    got.fields().len() == want.fields().len()
        && got.fields().iter().zip(want.fields()).all(|(g, w)| {
            g.name() == w.name()
                && g.data_type() == w.data_type()
                && g.is_nullable() == w.is_nullable()
        })
}

// ---- Encode shared code ----------------------------------------------------

/// Build a required primitive column from one value per row.
#[must_use]
pub fn write_required<T: ArrowPrimitiveType>(values: impl Iterator<Item = T::Native>) -> ArrayRef {
    Arc::new(PrimitiveArray::<T>::from_iter_values(values))
}

/// Build a nullable primitive column; `None` becomes a `NULL` cell.
#[must_use]
pub fn write_nullable<T: ArrowPrimitiveType>(
    values: impl Iterator<Item = Option<T::Native>>,
) -> ArrayRef {
    Arc::new(values.collect::<PrimitiveArray<T>>())
}

/// Build a required boolean column.
#[must_use]
pub fn write_bool(values: impl Iterator<Item = bool>) -> ArrayRef {
    Arc::new(values.map(Some).collect::<BooleanArray>())
}

/// Build a nullable boolean column.
#[must_use]
pub fn write_bool_nullable(values: impl Iterator<Item = Option<bool>>) -> ArrayRef {
    Arc::new(values.collect::<BooleanArray>())
}

/// Reject a row count above [`MAX_SECTION_ROWS`] before columns are built.
pub(crate) const fn check_row_cap(rows: usize) -> Result<(), CodecError> {
    if rows > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows,
            max: MAX_SECTION_ROWS,
        });
    }
    Ok(())
}

/// Initial capacity for a small snapshot section.
const ENCODE_BUF_HINT: usize = 4096;

/// Parquet writer properties shared by every snapshot section.
static WRITER_PROPS: LazyLock<WriterProperties> = LazyLock::new(|| {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).expect("zstd level 3 is valid"),
        ))
        .set_max_row_group_size(MAX_SECTION_ROWS)
        .set_created_by(String::new())
        .build()
});

/// Encode pre-built columns into a Parquet section body.
pub(crate) fn encode_section(
    contract: &TypeContract,
    columns: Vec<ArrayRef>,
) -> Result<Vec<u8>, CodecError> {
    let schema = arrow_schema(contract);
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns)?;
    check_row_cap(batch.num_rows())?;
    let batch = sort_by_sort_key(&batch, contract)?;

    let options = ArrowWriterOptions::new()
        .with_properties(WRITER_PROPS.clone())
        .with_skip_arrow_metadata(true);

    let mut buf = Vec::with_capacity(ENCODE_BUF_HINT);
    let mut writer = ArrowWriter::try_new_with_options(&mut buf, schema, options)?;
    writer.write(&batch)?;
    writer.close()?;

    if buf.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: buf.len(),
            max: MAX_SECTION_BYTES,
        });
    }
    Ok(buf)
}

/// Reorder `batch` by the contract's sort-key columns.
fn sort_by_sort_key(
    batch: &RecordBatch,
    contract: &TypeContract,
) -> Result<RecordBatch, CodecError> {
    if contract.sort_key.is_empty() || batch.num_rows() <= 1 {
        return Ok(batch.clone());
    }
    let mut sort_columns = Vec::with_capacity(contract.sort_key.len());
    for &name in contract.sort_key {
        let values = batch
            .column_by_name(name)
            .ok_or(CodecError::MissingColumn { name })?;
        sort_columns.push(SortColumn {
            values: Arc::clone(values),
            options: None,
        });
    }
    let indices = lexsort_to_indices(&sort_columns, None)?;
    let columns = batch
        .columns()
        .iter()
        .map(|column| take(column.as_ref(), &indices, None))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RecordBatch::try_new(batch.schema(), columns)?)
}

// ---- Decode shared code ----------------------------------------------------

/// Section bytes whose CRC has been checked against the catalog.
///
/// Decode entry points take this instead of raw `Bytes`.
#[derive(Clone, Debug)]
pub struct VerifiedSection(Bytes);

impl VerifiedSection {
    /// Verify `bytes` against their catalog CRC and wrap them for decode.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::SectionCrcMismatch`] when the CRC differs.
    pub fn verify(
        bytes: Bytes,
        expected: u32,
        crc32c: impl FnOnce(&[u8]) -> u32,
    ) -> Result<Self, CodecError> {
        let got = crc32c(&bytes);
        if got == expected {
            Ok(Self(bytes))
        } else {
            Err(CodecError::SectionCrcMismatch { expected, got })
        }
    }

    /// Wrap bytes without a CRC check, for tests that decode their own output.
    #[cfg(test)]
    pub(crate) const fn for_test(bytes: Bytes) -> Self {
        Self(bytes)
    }

    /// Unwrap the verified bytes.
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.0
    }

    /// The section byte length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the section is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod verified_section_tests {
    use bytes::Bytes;

    use super::{CodecError, VerifiedSection};

    #[test]
    fn verify_accepts_a_matching_crc_and_rejects_a_mismatch() {
        let bytes = Bytes::from_static(b"section"); // len 7, the stand-in crc
        let crc = |b: &[u8]| u32::try_from(b.len()).unwrap_or(u32::MAX);
        assert!(VerifiedSection::verify(bytes.clone(), 7, crc).is_ok());
        assert!(matches!(
            VerifiedSection::verify(bytes, 99, crc),
            Err(CodecError::SectionCrcMismatch {
                expected: 99,
                got: 7
            })
        ));
    }
}

#[cfg(test)]
mod codec_error_tests {
    use super::CodecError;

    #[test]
    fn section_type_id_labels_the_two_type_tagged_outcomes_and_nothing_else() {
        assert_eq!(
            CodecError::UnknownType { type_id: 5 }.section_type_id(),
            Some(5)
        );
        let wrapped = CodecError::Section {
            type_id: 7,
            bytes_in: 64,
            source: Box::new(CodecError::SchemaMismatch),
        };
        assert_eq!(wrapped.section_type_id(), Some(7));
        assert_eq!(CodecError::SchemaMismatch.section_type_id(), None);
        assert_eq!(
            CodecError::TooManyRows { rows: 9, max: 8 }.section_type_id(),
            None,
            "errors not tied to one section have no label"
        );
    }

    #[test]
    fn required_column_rejects_a_null_so_it_cannot_read_as_zero() {
        use std::sync::Arc;

        use arrow_array::types::Int64Type;
        use arrow_array::{ArrayRef, Int64Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};

        use super::required_column;

        // Required columns must not decode NULL as zero.
        let schema = Arc::new(Schema::new(vec![Field::new("ts", DataType::Int64, true)]));
        let column: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), None]));
        let batch = RecordBatch::try_new(schema, vec![column]).expect("batch");
        assert!(matches!(
            required_column::<Int64Type>(&batch, "ts"),
            Err(CodecError::NullInRequiredColumn { name: "ts" })
        ));
    }
}

/// What a section decode processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeStats {
    /// The decoded section's `type_id`.
    pub type_id: u32,
    /// Input section bytes.
    pub bytes_in: usize,
    /// Parquet row groups read.
    pub row_groups: usize,
    /// Arrow batches produced.
    pub batches: usize,
    /// Rows decoded.
    pub rows: usize,
}

/// A decoded section: its Arrow batches and the [`DecodeStats`] for the call.
#[derive(Debug)]
pub struct DecodedSection {
    /// The section's rows, in contract column order.
    pub batches: Vec<RecordBatch>,
    /// What the decode processed.
    pub stats: DecodeStats,
}

/// Parquet read batch size: the reader yields batches of at most this many rows.
const DECODE_BATCH_SIZE: usize = if MAX_SECTION_ROWS < 8192 {
    MAX_SECTION_ROWS
} else {
    8192
};

/// Build a Parquet reader after byte, row-group, and claimed-row caps pass.
///
/// Returns row-group and claimed-row counts for stats and preallocation.
fn capped_reader(bytes: Bytes) -> Result<(ParquetRecordBatchReader, usize, usize), CodecError> {
    if bytes.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: bytes.len(),
            max: MAX_SECTION_BYTES,
        });
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)?;

    let groups = builder.metadata().num_row_groups();
    if groups > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups,
            max: MAX_ROW_GROUPS,
        });
    }

    let claimed = builder.metadata().file_metadata().num_rows();
    let row_count = match usize::try_from(claimed) {
        Ok(rows) if rows <= MAX_SECTION_ROWS => rows,
        Ok(rows) => {
            return Err(CodecError::TooManyRows {
                rows,
                max: MAX_SECTION_ROWS,
            });
        }
        Err(_) => return Err(CodecError::InvalidRowCount { raw: claimed }),
    };

    Ok((
        builder.with_batch_size(DECODE_BATCH_SIZE).build()?,
        groups,
        row_count,
    ))
}

/// Decode a Parquet section body, streaming batches into `push_rows`.
pub(crate) fn decode_section<Row>(
    contract: &TypeContract,
    section: VerifiedSection,
    mut push_rows: impl FnMut(&RecordBatch, &mut Vec<Row>) -> Result<(), CodecError>,
) -> Result<Vec<Row>, CodecError> {
    let (reader, _row_groups, claimed_rows) = capped_reader(section.into_bytes())?;
    if !schema_matches(&reader.schema(), contract) {
        return Err(CodecError::SchemaMismatch);
    }
    // Claimed rows are capped above; typed gather pushes one row per source row.
    let mut rows = Vec::with_capacity(claimed_rows);
    for batch in reader {
        let batch = batch?;
        if rows.len() + batch.num_rows() > MAX_SECTION_ROWS {
            return Err(CodecError::TooManyRows {
                rows: rows.len() + batch.num_rows(),
                max: MAX_SECTION_ROWS,
            });
        }
        push_rows(&batch, &mut rows)?;
    }
    Ok(rows)
}

/// Decode a section body to Arrow batches.
pub(crate) fn decode_batches(
    contract: &TypeContract,
    section: VerifiedSection,
) -> Result<DecodedSection, CodecError> {
    let bytes = section.into_bytes();
    let bytes_in = bytes.len();
    let (reader, row_groups, claimed_rows) = capped_reader(bytes)?;

    if !schema_matches(&reader.schema(), contract) {
        return Err(CodecError::SchemaMismatch);
    }

    let mut batches = Vec::with_capacity(claimed_rows.div_ceil(DECODE_BATCH_SIZE).max(1));
    let mut rows = 0_usize;
    for batch in reader {
        let batch = batch?;
        rows += batch.num_rows();
        if rows > MAX_SECTION_ROWS {
            return Err(CodecError::TooManyRows {
                rows,
                max: MAX_SECTION_ROWS,
            });
        }
        batches.push(batch);
    }
    let stats = DecodeStats {
        type_id: contract.type_id.get(),
        bytes_in,
        row_groups,
        batches: batches.len(),
        rows,
    };
    Ok(DecodedSection { batches, stats })
}

fn primitive_column<'a, T: ArrowPrimitiveType>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a PrimitiveArray<T>, CodecError> {
    let column = batch
        .column_by_name(name)
        .ok_or(CodecError::MissingColumn { name })?;
    column
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .ok_or(CodecError::ColumnType { name })
}

/// A required primitive column; rejects `NULL`.
///
/// # Errors
///
/// Returns [`CodecError`] when the column is missing, has a different type, or
/// contains `NULL`.
pub fn required_column<'a, T: ArrowPrimitiveType>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a PrimitiveArray<T>, CodecError> {
    let array = primitive_column::<T>(batch, name)?;
    if array.null_count() == 0 {
        Ok(array)
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

/// A nullable primitive column.
///
/// # Errors
///
/// Returns [`CodecError`] when the column is missing or has a different type.
pub fn nullable_column<'a, T: ArrowPrimitiveType>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a PrimitiveArray<T>, CodecError> {
    primitive_column::<T>(batch, name)
}

fn boolean_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BooleanArray, CodecError> {
    let column = batch
        .column_by_name(name)
        .ok_or(CodecError::MissingColumn { name })?;
    column
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or(CodecError::ColumnType { name })
}

/// A required boolean column; rejects `NULL`.
///
/// # Errors
///
/// Returns [`CodecError`] when the column is missing, has a different type, or
/// contains `NULL`.
pub fn required_bool<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BooleanArray, CodecError> {
    let array = boolean_column(batch, name)?;
    if array.null_count() == 0 {
        Ok(array)
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

/// A nullable boolean column.
///
/// # Errors
///
/// Returns [`CodecError`] when the column is missing or has a different type.
pub fn nullable_bool<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BooleanArray, CodecError> {
    boolean_column(batch, name)
}

/// Read primitive cell `i` as `Option`, mapping a null cell to `None`.
#[must_use]
pub fn opt_primitive<T: ArrowPrimitiveType>(
    array: &PrimitiveArray<T>,
    i: usize,
) -> Option<T::Native> {
    if array.is_null(i) {
        None
    } else {
        Some(array.value(i))
    }
}

/// Read boolean cell `i` as `Option`, mapping a null cell to `None`.
#[must_use]
pub fn opt_bool(array: &BooleanArray, i: usize) -> Option<bool> {
    if array.is_null(i) {
        None
    } else {
        Some(array.value(i))
    }
}

#[cfg(test)]
mod hygiene_tests {
    use crate::{Section, StrId, Ts, VerifiedSection};

    // These names collide with generated locals and tuple structs if hygiene
    // regresses.
    #[allow(
        non_snake_case,
        reason = "fields are deliberately named like the Ts/StrId types to test decode hygiene"
    )]
    #[derive(Debug, Clone, Copy, PartialEq, Section)]
    #[section(id = 1_099_001, name = "hygiene probe", semantics = snapshot_full, sort_key("ts"))]
    struct Weird {
        #[column(t)]
        ts: Ts,
        #[column(c)]
        batch: i64,
        #[column(c)]
        out: i64,
        #[column(c)]
        i: i64,
        #[column(c)]
        rows: Option<i64>,
        #[column(g)]
        columns: bool,
        #[column(l)]
        label: StrId,
        #[column(c)]
        Ts: i64,
        #[column(l)]
        StrId: u64,
    }

    #[test]
    fn collision_named_fields_roundtrip() {
        let want = vec![
            Weird {
                ts: Ts(1),
                batch: 2,
                out: 3,
                i: 4,
                rows: Some(5),
                columns: true,
                label: StrId(10),
                Ts: 11,
                StrId: 12,
            },
            Weird {
                ts: Ts(6),
                batch: 7,
                out: 8,
                i: 9,
                rows: None,
                columns: false,
                label: StrId(13),
                Ts: 14,
                StrId: 15,
            },
        ];
        let bytes = Weird::encode(&want).expect("encode");
        assert_eq!(
            Weird::decode(VerifiedSection::for_test(bytes.into())).expect("decode"),
            want
        );
    }
}
