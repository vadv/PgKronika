//! Type `1_006_001`: `pg_stat_bgwriter` + `pg_stat_checkpointer`, a
//! single-row snapshot (README.md, "Type `1_006_001`").
//!
//! `PostgreSQL` 17 moved some `pg_stat_bgwriter` counters to
//! `pg_stat_checkpointer`. The collector reads both views and writes one row;
//! columns removed from `PostgreSQL` 17 (`buffers_backend`,
//! `buffers_backend_fsync`) are written as `NULL`.
//!
//! This is the first manual codec. It encodes rows to a Parquet section body
//! and decodes them back. When `kronika-derive` is available, it will generate
//! this from the [`CONTRACT`].

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, Float64Array, Int64Array, RecordBatch};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use super::{CodecError, MAX_ROW_GROUPS, MAX_SECTION_BYTES, MAX_SECTION_ROWS, arrow_schema};
use crate::contract::{Column, ColumnClass, ColumnType, Semantics, TypeContract};
use crate::type_id::TypeId;

/// One row of type `1_006_001`. Nullable fields are `None` on PG17+.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BgwriterCheckpointer {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Scheduled checkpoints.
    pub checkpoints_timed: i64,
    /// Requested checkpoints.
    pub checkpoints_req: i64,
    /// Time spent writing checkpoints, milliseconds.
    pub checkpoint_write_time: f64,
    /// Time spent syncing checkpoints, milliseconds.
    pub checkpoint_sync_time: f64,
    /// Buffers written during checkpoints.
    pub buffers_checkpoint: i64,
    /// Buffers written by the background writer.
    pub buffers_clean: i64,
    /// Times the background writer stopped a cleaning scan early.
    pub maxwritten_clean: i64,
    /// Buffers written by backends; `None` on PG17+.
    pub buffers_backend: Option<i64>,
    /// Backend fsync calls; `None` on PG17+.
    pub buffers_backend_fsync: Option<i64>,
    /// Buffers allocated.
    pub buffers_alloc: i64,
}

/// Columns of type `1_006_001`, in schema order.
const COLUMNS: &[Column] = &[
    Column {
        name: "ts",
        ty: ColumnType::Ts,
        class: ColumnClass::Timestamp,
        nullable: false,
    },
    Column {
        name: "checkpoints_timed",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "checkpoints_req",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "checkpoint_write_time",
        ty: ColumnType::F64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "checkpoint_sync_time",
        ty: ColumnType::F64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "buffers_checkpoint",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "buffers_clean",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "maxwritten_clean",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
    Column {
        name: "buffers_backend",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: true,
    },
    Column {
        name: "buffers_backend_fsync",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: true,
    },
    Column {
        name: "buffers_alloc",
        ty: ColumnType::I64,
        class: ColumnClass::Cumulative,
        nullable: false,
    },
];

/// The registry contract for type `1_006_001`.
pub const CONTRACT: TypeContract = TypeContract {
    type_id: TypeId::declared(1_006_001),
    name: "pg_stat_bgwriter + pg_stat_checkpointer",
    semantics: Semantics::SnapshotFull,
    columns: COLUMNS,
    sort_key: &["ts"],
    deprecated: false,
};

/// Encode rows into a Parquet section body (zstd).
///
/// Peak memory is the caller's input slice plus one Parquet row group. One
/// section is one row group, capped at [`MAX_SECTION_ROWS`].
///
/// # Errors
///
/// [`CodecError::TooManyRows`] above the cap; [`CodecError::Arrow`] or
/// [`CodecError::Parquet`] if Arrow rejects the batch or Parquet writing
/// fails.
pub fn encode(rows: &[BgwriterCheckpointer]) -> Result<Vec<u8>, CodecError> {
    if rows.len() > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows: rows.len(),
            max: MAX_SECTION_ROWS,
        });
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.ts))),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.checkpoints_timed),
        )),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.checkpoints_req),
        )),
        Arc::new(Float64Array::from_iter_values(
            rows.iter().map(|r| r.checkpoint_write_time),
        )),
        Arc::new(Float64Array::from_iter_values(
            rows.iter().map(|r| r.checkpoint_sync_time),
        )),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.buffers_checkpoint),
        )),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.buffers_clean),
        )),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.maxwritten_clean),
        )),
        Arc::new(
            rows.iter()
                .map(|r| r.buffers_backend)
                .collect::<Int64Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| r.buffers_backend_fsync)
                .collect::<Int64Array>(),
        ),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.buffers_alloc),
        )),
    ];

    let schema = arrow_schema(&CONTRACT);
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns)?;

    // Level 3 matches the format's compression guidance for incremental
    // writes. Later writer code can choose a higher level when closing a
    // segment. `try_new(3)` is expected to succeed.
    //
    // `set_created_by("")` drops the Arrow-version string, and the writer
    // options skip the embedded `ARROW:schema` blob: the section schema
    // lives in the registry, so storing a second copy in every file is pure
    // overhead and would also make the bytes vary with the Arrow version.
    // The native Parquet schema (physical types and column layout) stays —
    // it is what the decoder needs to read the column chunks.
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_max_row_group_size(MAX_SECTION_ROWS)
        .set_created_by(String::new())
        .build();
    let options = ArrowWriterOptions::new()
        .with_properties(props)
        .with_skip_arrow_metadata(true);

    let mut buf = Vec::new();
    let mut writer = ArrowWriter::try_new_with_options(&mut buf, schema, options)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(buf)
}

/// Decode a Parquet section body back into rows.
///
/// The registry contract is the schema of truth: decode imposes the
/// contract's columns and types and returns a typed error on any mismatch.
/// A `type_id` whose bytes do not decode means the writer violated the
/// contract, so the reader's policy is to skip that section and raise a
/// diagnostic — not to guess. Decode itself only surfaces the error.
///
/// Layered memory bounds (README.md, "Memory Bounds"): the section byte
/// length is capped before Parquet reads metadata; the row-group count is
/// capped after metadata is read; the metadata row count and the decoded row
/// count are both capped at [`MAX_SECTION_ROWS`]. Parquet can still allocate
/// internally while decoding a valid-size page, so callers should pass bytes
/// only after catalog CRC verification.
///
/// # Errors
///
/// [`CodecError::SectionTooLarge`], [`CodecError::TooManyRowGroups`], or
/// [`CodecError::TooManyRows`] if a bound is exceeded; [`CodecError::Parquet`]
/// on malformed Parquet; [`CodecError::MissingColumn`],
/// [`CodecError::ColumnType`], or [`CodecError::NullInRequiredColumn`] if the
/// file does not match the contract.
pub fn decode(bytes: &[u8]) -> Result<Vec<BgwriterCheckpointer>, CodecError> {
    // Cap the byte length before Parquet reads file metadata.
    if bytes.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: bytes.len(),
            max: MAX_SECTION_BYTES,
        });
    }
    // One bounded copy of the section.
    let data = bytes::Bytes::copy_from_slice(bytes);
    let builder = ParquetRecordBatchReaderBuilder::try_new(data)?;

    let groups = builder.metadata().num_row_groups();
    if groups > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups,
            max: MAX_ROW_GROUPS,
        });
    }

    let claimed = builder.metadata().file_metadata().num_rows();
    // A negative or oversized claim is rejected before any column is read.
    let claimed_rows = usize::try_from(claimed).ok();
    if claimed_rows.is_none_or(|rows| rows > MAX_SECTION_ROWS) {
        return Err(CodecError::TooManyRows {
            rows: claimed_rows.unwrap_or(usize::MAX),
            max: MAX_SECTION_ROWS,
        });
    }

    let batch_size = MAX_SECTION_ROWS.min(8192);
    let reader = builder.with_batch_size(batch_size).build()?;

    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch?;
        // The metadata row count was checked, but row groups drive iteration.
        if rows.len() + batch.num_rows() > MAX_SECTION_ROWS {
            return Err(CodecError::TooManyRows {
                rows: rows.len() + batch.num_rows(),
                max: MAX_SECTION_ROWS,
            });
        }

        let ts = required_i64(&batch, "ts")?;
        let checkpoints_timed = required_i64(&batch, "checkpoints_timed")?;
        let checkpoints_req = required_i64(&batch, "checkpoints_req")?;
        let checkpoint_write_time = required_f64(&batch, "checkpoint_write_time")?;
        let checkpoint_sync_time = required_f64(&batch, "checkpoint_sync_time")?;
        let buffers_checkpoint = required_i64(&batch, "buffers_checkpoint")?;
        let buffers_clean = required_i64(&batch, "buffers_clean")?;
        let maxwritten_clean = required_i64(&batch, "maxwritten_clean")?;
        let buffers_backend = i64_column(&batch, "buffers_backend")?;
        let buffers_backend_fsync = i64_column(&batch, "buffers_backend_fsync")?;
        let buffers_alloc = required_i64(&batch, "buffers_alloc")?;

        for i in 0..batch.num_rows() {
            rows.push(BgwriterCheckpointer {
                ts: ts.value(i),
                checkpoints_timed: checkpoints_timed.value(i),
                checkpoints_req: checkpoints_req.value(i),
                checkpoint_write_time: checkpoint_write_time.value(i),
                checkpoint_sync_time: checkpoint_sync_time.value(i),
                buffers_checkpoint: buffers_checkpoint.value(i),
                buffers_clean: buffers_clean.value(i),
                maxwritten_clean: maxwritten_clean.value(i),
                buffers_backend: nullable(buffers_backend, i),
                buffers_backend_fsync: nullable(buffers_backend_fsync, i),
                buffers_alloc: buffers_alloc.value(i),
            });
        }
    }
    Ok(rows)
}

/// Read `i` as `Option`, mapping a null cell to `None`.
fn nullable(array: &Int64Array, i: usize) -> Option<i64> {
    if array.is_null(i) {
        None
    } else {
        Some(array.value(i))
    }
}

/// A non-nullable `i64` column: rejects a `NULL` so it cannot read as `0`.
fn required_i64<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a Int64Array, CodecError> {
    let array = i64_column(batch, name)?;
    if array.null_count() == 0 {
        Ok(array)
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

/// A non-nullable `f64` column: rejects a `NULL` so it cannot read as `0.0`.
fn required_f64<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a Float64Array, CodecError> {
    let array = f64_column(batch, name)?;
    if array.null_count() == 0 {
        Ok(array)
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

fn i64_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a Int64Array, CodecError> {
    let column = batch
        .column_by_name(name)
        .ok_or(CodecError::MissingColumn { name })?;
    column
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or(CodecError::ColumnType { name })
}

fn f64_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a Float64Array, CodecError> {
    let column = batch
        .column_by_name(name)
        .ok_or(CodecError::MissingColumn { name })?;
    column
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(CodecError::ColumnType { name })
}

#[cfg(test)]
mod tests {
    use super::{BgwriterCheckpointer, CONTRACT, MAX_SECTION_ROWS, decode, encode};
    use crate::contract::lint;

    fn pg16_row(ts: i64) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts,
            checkpoints_timed: 10,
            checkpoints_req: 2,
            checkpoint_write_time: 1234.5,
            checkpoint_sync_time: 67.0,
            buffers_checkpoint: 4096,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_backend: Some(128),
            buffers_backend_fsync: Some(0),
            buffers_alloc: 9000,
        }
    }

    fn pg17_row(ts: i64) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            buffers_backend: None,
            buffers_backend_fsync: None,
            ..pg16_row(ts)
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[CONTRACT]), Ok(()));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        // Mix a PG16 row with values and a PG17 row with nulls in one section.
        let rows = vec![pg16_row(1_000_000), pg17_row(2_000_000)];
        let bytes = encode(&rows).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded, rows);
    }

    #[test]
    fn empty_section_roundtrips() {
        let bytes = encode(&[]).expect("encode empty");
        assert_eq!(decode(&bytes).expect("decode empty"), Vec::new());
    }

    #[test]
    fn nulls_survive_distinct_from_zero() {
        // A null buffers_backend must not decode to Some(0).
        let row = pg17_row(5);
        let decoded = decode(&encode(&[row]).expect("encode")).expect("decode");
        assert_eq!(decoded[0].buffers_backend, None);
    }

    #[test]
    fn section_is_a_parquet_file() {
        // The body is a self-contained Parquet file: "PAR1" at both ends.
        let bytes = encode(&[pg16_row(1)]).expect("encode");
        assert_eq!(&bytes[..4], b"PAR1");
        assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
    }

    #[test]
    fn encode_rejects_too_many_rows() {
        // Build a row count over the cap cheaply by repeating one row.
        let rows = vec![pg16_row(0); MAX_SECTION_ROWS + 1];
        assert!(matches!(
            encode(&rows),
            Err(super::CodecError::TooManyRows { .. })
        ));
    }

    #[test]
    fn decode_rejects_an_oversized_section() {
        let bytes = vec![0_u8; super::MAX_SECTION_BYTES + 1];
        assert!(matches!(
            decode(&bytes),
            Err(super::CodecError::SectionTooLarge { .. })
        ));
    }

    #[test]
    fn section_does_not_embed_the_arrow_schema() {
        // The registry is the schema of truth; arrow-rs would otherwise write
        // a second copy as an "ARROW:schema" key-value blob. It must not.
        let bytes = encode(&[pg16_row(1)]).expect("encode");
        let needle = b"ARROW:schema";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "the embedded Arrow schema blob leaked back into the section"
        );
    }

    #[test]
    fn encode_is_deterministic() {
        // No Arrow-version string and no embedded schema, so the same rows
        // encode to the same bytes within one build.
        let rows = [pg16_row(1), pg17_row(2)];
        assert_eq!(encode(&rows).expect("a"), encode(&rows).expect("b"));
    }

    #[test]
    fn decode_rejects_a_null_in_a_required_column() {
        use std::sync::Arc;

        use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};
        use parquet::arrow::ArrowWriter;

        // A malformed section with the contract columns, but `ts` nullable and
        // holding a NULL. A NULL in a non-nullable column must be rejected,
        // not read as 0.
        let i64f = |name: &str, nullable: bool| Field::new(name, DataType::Int64, nullable);
        let f64f = |name: &str| Field::new(name, DataType::Float64, false);
        let schema = Arc::new(Schema::new(vec![
            i64f("ts", true),
            i64f("checkpoints_timed", false),
            i64f("checkpoints_req", false),
            f64f("checkpoint_write_time"),
            f64f("checkpoint_sync_time"),
            i64f("buffers_checkpoint", false),
            i64f("buffers_clean", false),
            i64f("maxwritten_clean", false),
            i64f("buffers_backend", true),
            i64f("buffers_backend_fsync", true),
            i64f("buffers_alloc", false),
        ]));
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![None::<i64>])), // ts = NULL
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Float64Array::from_iter_values([0.0_f64])),
            Arc::new(Float64Array::from_iter_values([0.0_f64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
            Arc::new(Int64Array::from_iter_values([0_i64])),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).expect("batch");

        let mut buf = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut buf, schema, None).expect("writer");
        writer.write(&batch).expect("write");
        writer.close().expect("close");

        assert!(matches!(
            decode(&buf),
            Err(super::CodecError::NullInRequiredColumn { name: "ts" })
        ));
    }
}
