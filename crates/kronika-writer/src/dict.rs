//! `dict.strings` / `dict.blobs` section encoders.
//!
//! Encode `dict.strings` and `dict.blobs` section bodies.

use std::sync::{Arc, LazyLock};

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, FixedSizeBinaryArray, RecordBatch, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use kronika_format::{EntrySnapshot, Placement, SegmentDicts};
use kronika_registry::{
    CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, MAX_SECTION_BYTES, MAX_SECTION_ROWS,
};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

/// Parquet writer properties shared by dictionary sections.
static DICT_WRITER_PROPS: LazyLock<WriterProperties> = LazyLock::new(|| {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).expect("zstd level 3 is valid"),
        ))
        .set_max_row_group_size(MAX_SECTION_ROWS)
        .set_created_by(String::new())
        .build()
});

/// One encoded dictionary section: its type id, row count, and Parquet body.
#[derive(Debug, Clone)]
pub struct DictSection {
    /// [`DICT_STRINGS_TYPE_ID`] or [`DICT_BLOBS_TYPE_ID`].
    pub type_id: u32,
    /// Number of dictionary entries in the body.
    pub rows: u32,
    /// The Parquet section body.
    pub body: Vec<u8>,
}

/// Encode a dictionary window into section bodies.
pub fn encode(window: &SegmentDicts) -> Result<Vec<DictSection>, CodecError> {
    let mut strings: Vec<EntrySnapshot<'_>> = Vec::new();
    let mut blobs: Vec<EntrySnapshot<'_>> = Vec::new();
    for entry in window.entries() {
        match entry.placement {
            Placement::Strings => strings.push(entry),
            Placement::Blobs => blobs.push(entry),
        }
    }

    let mut sections = Vec::new();
    if !strings.is_empty() {
        sections.push(encode_strings(&mut strings)?);
    }
    if !blobs.is_empty() {
        sections.push(encode_blobs(&mut blobs)?);
    }
    Ok(sections)
}

/// Reject an over-cap dictionary section before Arrow arrays are built.
const fn check_dict_rows(rows: usize) -> Result<(), CodecError> {
    if rows > MAX_SECTION_ROWS {
        Err(CodecError::TooManyRows {
            rows,
            max: MAX_SECTION_ROWS,
        })
    } else {
        Ok(())
    }
}

/// `dict.strings`: `str_id u64, bytes binary`, sorted by `str_id`.
fn encode_strings(entries: &mut [EntrySnapshot<'_>]) -> Result<DictSection, CodecError> {
    check_dict_rows(entries.len())?;
    entries.sort_unstable_by_key(|entry| entry.str_id.get());
    let ids = UInt64Array::from_iter_values(entries.iter().map(|entry| entry.str_id.get()));
    let bytes = BinaryArray::from_iter_values(entries.iter().map(|entry| entry.stored_bytes));
    let schema = Arc::new(Schema::new(vec![
        Field::new("str_id", DataType::UInt64, false),
        Field::new("bytes", DataType::Binary, false),
    ]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(ids), Arc::new(bytes)])?;
    section(DICT_STRINGS_TYPE_ID, &batch)
}

/// `dict.blobs`: `str_id`, `stored_bytes`, `full_len`, `truncated`, and the
/// optional `full_sha256` present only for truncated values.
fn encode_blobs(entries: &mut [EntrySnapshot<'_>]) -> Result<DictSection, CodecError> {
    check_dict_rows(entries.len())?;
    entries.sort_unstable_by_key(|entry| entry.str_id.get());
    let ids = UInt64Array::from_iter_values(entries.iter().map(|entry| entry.str_id.get()));
    let stored = BinaryArray::from_iter_values(entries.iter().map(|entry| entry.stored_bytes));
    let full_len = UInt64Array::from_iter_values(entries.iter().map(|entry| entry.full_len));
    let truncated: BooleanArray = entries.iter().map(|entry| Some(entry.truncated)).collect();
    let sha = FixedSizeBinaryArray::try_from_sparse_iter_with_size(
        entries.iter().map(|entry| entry.full_sha256),
        32,
    )?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("str_id", DataType::UInt64, false),
        Field::new("stored_bytes", DataType::Binary, false),
        Field::new("full_len", DataType::UInt64, false),
        Field::new("truncated", DataType::Boolean, false),
        Field::new("full_sha256", DataType::FixedSizeBinary(32), true),
    ]));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(ids),
        Arc::new(stored),
        Arc::new(full_len),
        Arc::new(truncated),
        Arc::new(sha),
    ];
    let batch = RecordBatch::try_new(schema, columns)?;
    section(DICT_BLOBS_TYPE_ID, &batch)
}

/// Write `batch` to a capped zstd Parquet body.
fn section(type_id: u32, batch: &RecordBatch) -> Result<DictSection, CodecError> {
    if batch.num_rows() > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows: batch.num_rows(),
            max: MAX_SECTION_ROWS,
        });
    }

    let options = ArrowWriterOptions::new()
        .with_properties(DICT_WRITER_PROPS.clone())
        .with_skip_arrow_metadata(true);
    let mut body = Vec::new();
    let mut writer = ArrowWriter::try_new_with_options(&mut body, batch.schema(), options)?;
    writer.write(batch)?;
    writer.close()?;

    if body.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: body.len(),
            max: MAX_SECTION_BYTES,
        });
    }
    Ok(DictSection {
        type_id,
        rows: u32::try_from(batch.num_rows()).unwrap_or(u32::MAX),
        body,
    })
}

#[cfg(test)]
mod tests {
    use kronika_format::DictLimits;
    use kronika_registry::{DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID};

    use super::encode;
    use crate::Interner;

    #[test]
    fn an_empty_window_encodes_no_sections() {
        let interner = Interner::new(DictLimits::new(8, 1024).expect("limits"));
        assert!(encode(interner.window()).expect("encode").is_empty());
    }

    #[test]
    fn strings_and_blobs_split_by_placement() {
        // blob_threshold 8: short values are strings, longer ones blobs.
        let mut interner = Interner::new(DictLimits::new(8, 1024).expect("limits"));
        interner.intern(b"short").expect("string");
        interner.intern(b"also").expect("string");
        interner
            .intern(b"a value longer than eight bytes")
            .expect("blob by size");

        let sections = encode(interner.window()).expect("encode");
        assert_eq!(sections.len(), 2, "one strings section, one blobs section");

        let strings = sections
            .iter()
            .find(|s| s.type_id == DICT_STRINGS_TYPE_ID)
            .expect("strings section");
        assert_eq!(strings.rows, 2);
        let blobs = sections
            .iter()
            .find(|s| s.type_id == DICT_BLOBS_TYPE_ID)
            .expect("blobs section");
        assert_eq!(blobs.rows, 1);
        assert_eq!(&blobs.body[..4], b"PAR1", "a Parquet body");
    }
}
