//! Segment completion: merge the journal's parts into one immutable segment.
//!
//! Coalesces collection-window sections by type into a temporary file and
//! writes the end catalog last.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use arrow_array::{
    Array, BinaryArray, BooleanArray, FixedSizeBinaryArray, RecordBatch, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use kronika_format::{
    Catalog, Entry, EntrySnapshot, FORMAT_VERSION, HotMark, MAGIC, PartError, PartRef, Placement,
    StrId, crc32c, validate_part,
};
use kronika_registry::{
    Bytes, CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, MAX_ROW_GROUPS,
    MAX_SECTION_BYTES, MAX_SECTION_ROWS, VerifiedSection, decode_any, encode_sealed_batches,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::{Journal, JournalError};

/// What a completed segment contains, for the caller's metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealSummary {
    /// Number of catalog entries (sections) written.
    pub sections: usize,
    /// Total segment length, bytes.
    pub bytes: u64,
    /// Minimal timestamp across the segment, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp across the segment, unix microseconds.
    pub max_ts: i64,
}

/// Why sealing a segment failed.
#[derive(Debug)]
pub enum SealError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// Reading a part back from the journal failed.
    Journal(JournalError),
    /// A journal part did not validate as a PGM container.
    Part(PartError),
    /// A registered or dictionary Parquet section was invalid.
    Codec(CodecError),
    /// The journal holds no parts, so there is nothing to seal.
    Empty,
    /// A segment already exists at `dest`; it is never overwritten.
    AlreadyExists,
    /// Two parts carry different non-zero `source_id`s.
    SourceIdMismatch {
        /// The first non-zero source id seen.
        expected: u64,
        /// A later, conflicting source id.
        got: u64,
    },
    /// A journal part uses a different internal format version.
    UnsupportedFormat {
        /// Version read from the part catalog.
        version: u32,
    },
    /// Catalog rows and decoded rows do not agree.
    RowCountMismatch {
        /// Section type.
        type_id: u32,
        /// Rows declared by the catalog.
        declared: u32,
        /// Rows produced by Parquet decode.
        decoded: usize,
    },
    /// One dictionary id has conflicting bytes, metadata, or placement.
    DictionaryConflict {
        /// Conflicting dictionary id.
        str_id: u64,
    },
    /// A checked size, count, or offset calculation overflowed.
    ArithmeticOverflow {
        /// Quantity that overflowed.
        what: &'static str,
    },
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "segment io: {err}"),
            Self::Journal(err) => write!(f, "reading a journal part: {err}"),
            Self::Part(err) => write!(f, "invalid journal part: {err}"),
            Self::Codec(err) => write!(f, "invalid section: {err}"),
            Self::Empty => write!(f, "the journal holds no parts to seal"),
            Self::AlreadyExists => write!(f, "a segment already exists at the destination"),
            Self::SourceIdMismatch { expected, got } => {
                write!(f, "journal mixes source_id {expected} and {got}")
            }
            Self::UnsupportedFormat { version } => {
                write!(f, "journal part uses unsupported format version {version}")
            }
            Self::RowCountMismatch {
                type_id,
                declared,
                decoded,
            } => write!(
                f,
                "section {type_id} declares {declared} rows but decodes {decoded}"
            ),
            Self::DictionaryConflict { str_id } => {
                write!(f, "dictionary id {str_id} has conflicting representations")
            }
            Self::ArithmeticOverflow { what } => write!(f, "{what} overflow"),
        }
    }
}

impl Error for SealError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Journal(err) => Some(err),
            Self::Part(err) => Some(err),
            Self::Codec(err) => Some(err),
            Self::Empty
            | Self::AlreadyExists
            | Self::SourceIdMismatch { .. }
            | Self::UnsupportedFormat { .. }
            | Self::RowCountMismatch { .. }
            | Self::DictionaryConflict { .. }
            | Self::ArithmeticOverflow { .. } => None,
        }
    }
}

impl From<io::Error> for SealError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<JournalError> for SealError {
    fn from(err: JournalError) -> Self {
        Self::Journal(err)
    }
}

impl From<CodecError> for SealError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

impl From<parquet::errors::ParquetError> for SealError {
    fn from(err: parquet::errors::ParquetError) -> Self {
        Self::Codec(CodecError::Parquet(err))
    }
}

impl From<arrow_schema::ArrowError> for SealError {
    fn from(err: arrow_schema::ArrowError) -> Self {
        Self::Codec(CodecError::Arrow(err))
    }
}

/// Seal journal parts into an immutable segment at `dest`.
///
/// `dest` is never overwritten. Call `Journal::reset` only after `Ok`.
///
/// # Errors
///
/// Returns [`SealError`] when the journal is empty, a part is invalid, I/O
/// fails, or `dest` already exists.
pub fn seal(journal: &Journal, dest: &Path) -> Result<SealSummary, SealError> {
    if journal.parts().is_empty() {
        return Err(SealError::Empty);
    }

    let tmp = tmp_path(dest);
    let summary = match write_tmp(journal, &tmp) {
        Ok(summary) => summary,
        Err(err) => {
            fs::remove_file(&tmp).ok();
            return Err(err);
        }
    };
    // Hard-link publish fails if `dest` exists. The data file is already synced.
    if let Err(err) = fs::hard_link(&tmp, dest) {
        // Drop the temporary best-effort and keep `AlreadyExists` distinguishable.
        fs::remove_file(&tmp).ok();
        return Err(if err.kind() == io::ErrorKind::AlreadyExists {
            SealError::AlreadyExists
        } else {
            SealError::Io(err)
        });
    }
    // Make the new link durable before the temporary name is removed.
    sync_parent_dir(dest)?;
    fs::remove_file(&tmp)?;
    Ok(summary)
}

/// A process-unique temporary path beside `dest`.
///
/// Uses pid plus a counter so stale temporary names cannot collide.
fn tmp_path(dest: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = dest.as_os_str().to_owned();
    name.push(format!(".{}.{seq}.tmp", std::process::id()));
    PathBuf::from(name)
}

#[derive(Debug, Clone, Copy)]
struct SectionDescriptor {
    part: PartRef,
    entry: Entry,
}

#[derive(Debug)]
struct SegmentPlan {
    by_type: BTreeMap<u32, Vec<SectionDescriptor>>,
    min_ts: i64,
    max_ts: i64,
    source_id: u64,
}

/// Write the coalesced segment to `tmp` and fsync it. The caller publishes it.
fn write_tmp(journal: &Journal, tmp: &Path) -> Result<SealSummary, SealError> {
    let mut plan = plan_segment(journal)?;
    let strings = plan.by_type.remove(&DICT_STRINGS_TYPE_ID).unwrap_or_default();
    let blobs = plan.by_type.remove(&DICT_BLOBS_TYPE_ID).unwrap_or_default();
    let dictionary = normalize_dictionary(journal, &strings, &blobs)?;

    // Never truncate an existing temporary.
    let file = File::options().create_new(true).write(true).open(tmp)?;
    let mut out = BufWriter::new(file);

    out.write_all(&MAGIC)?;
    let mut offset = MAGIC.len() as u64;
    let mut entries: Vec<Entry> = Vec::new();

    for (type_id, descriptors) in plan.by_type {
        let declared_rows = aggregate_rows(type_id, &descriptors)?;
        if declared_rows == 0 {
            continue;
        }
        let mut batches = Vec::<RecordBatch>::new();
        let mut decoded_rows = 0_usize;
        for descriptor in descriptors {
            let decoded = decode_any(type_id, read_verified_body(journal, descriptor)?)?;
            if decoded.stats.rows != descriptor.entry.rows as usize {
                return Err(SealError::RowCountMismatch {
                    type_id,
                    declared: descriptor.entry.rows,
                    decoded: decoded.stats.rows,
                });
            }
            decoded_rows = decoded_rows.checked_add(decoded.stats.rows).ok_or(
                SealError::ArithmeticOverflow {
                    what: "decoded row count",
                },
            )?;
            batches.extend(decoded.batches);
        }
        if decoded_rows != declared_rows {
            return Err(SealError::RowCountMismatch {
                type_id,
                declared: u32::try_from(declared_rows).unwrap_or(u32::MAX),
                decoded: decoded_rows,
            });
        }
        let body = encode_sealed_batches(type_id, &batches)?;
        write_section(
            &mut out,
            &mut entries,
            &mut offset,
            type_id,
            u32::try_from(declared_rows).map_err(|_error| SealError::ArithmeticOverflow {
                what: "section row count",
            })?,
            &body,
        )?;
    }

    for section in dictionary.sections()? {
        write_section(
            &mut out,
            &mut entries,
            &mut offset,
            section.type_id,
            section.rows,
            &section.body,
        )?;
    }

    let sections = entries.len();
    let catalog = Catalog {
        entries,
        min_ts: plan.min_ts,
        max_ts: plan.max_ts,
        source_id: plan.source_id,
        format_version: FORMAT_VERSION,
    };
    out.write_all(&catalog.encode())?;

    let file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
    let bytes = file.metadata()?.len();
    file.sync_all()?;
    Ok(SealSummary {
        sections,
        bytes,
        min_ts: plan.min_ts,
        max_ts: plan.max_ts,
    })
}

fn plan_segment(journal: &Journal) -> Result<SegmentPlan, SealError> {
    let mut by_type = BTreeMap::<u32, Vec<SectionDescriptor>>::new();
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;
    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part(&part).map_err(SealError::Part)?;
        if catalog.format_version != FORMAT_VERSION {
            return Err(SealError::UnsupportedFormat {
                version: catalog.format_version,
            });
        }
        min_ts = min_ts.min(catalog.min_ts);
        max_ts = max_ts.max(catalog.max_ts);
        if catalog.source_id != 0 {
            if source_id != 0 && source_id != catalog.source_id {
                return Err(SealError::SourceIdMismatch {
                    expected: source_id,
                    got: catalog.source_id,
                });
            }
            source_id = catalog.source_id;
        }
        for entry in catalog.entries {
            let descriptors = by_type.entry(entry.type_id).or_default();
            descriptors
                .try_reserve(1)
                .map_err(|_error| SealError::ArithmeticOverflow {
                    what: "section descriptor allocation",
                })?;
            descriptors.push(SectionDescriptor {
                part: part_ref,
                entry,
            });
        }
    }
    if min_ts > max_ts {
        min_ts = 0;
        max_ts = 0;
    }
    Ok(SegmentPlan {
        by_type,
        min_ts,
        max_ts,
        source_id,
    })
}

fn aggregate_rows(
    type_id: u32,
    descriptors: &[SectionDescriptor],
) -> Result<usize, SealError> {
    let rows = descriptors.iter().try_fold(0_usize, |rows, descriptor| {
        rows.checked_add(descriptor.entry.rows as usize)
            .ok_or(SealError::ArithmeticOverflow {
                what: "section row count",
            })
    })?;
    if rows > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows,
            max: MAX_SECTION_ROWS,
        }
        .into());
    }
    if type_id == 0 {
        return Err(CodecError::UnknownType { type_id }.into());
    }
    Ok(rows)
}

fn read_verified_body(
    journal: &Journal,
    descriptor: SectionDescriptor,
) -> Result<VerifiedSection, SealError> {
    let part = journal.read_part(descriptor.part)?;
    let start = usize::try_from(descriptor.entry.offset).map_err(|_error| {
        SealError::ArithmeticOverflow {
            what: "section offset",
        }
    })?;
    let len = usize::try_from(descriptor.entry.len).map_err(|_error| {
        SealError::ArithmeticOverflow {
            what: "section length",
        }
    })?;
    let end = start
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "section end",
        })?;
    let body = part
        .get(start..end)
        .ok_or(SealError::Part(PartError::SectionOutOfBounds {
            type_id: descriptor.entry.type_id,
        }))?;
    VerifiedSection::verify(
        Bytes::copy_from_slice(body),
        descriptor.entry.crc32c,
        crc32c,
    )
    .map_err(SealError::Codec)
}

fn write_section(
    out: &mut BufWriter<File>,
    entries: &mut Vec<Entry>,
    offset: &mut u64,
    type_id: u32,
    rows: u32,
    body: &[u8],
) -> Result<(), SealError> {
    if entries.last().is_some_and(|entry| entry.type_id >= type_id) {
        return Err(CodecError::SchemaMismatch.into());
    }
    let len = u64::try_from(body.len()).map_err(|_error| SealError::ArithmeticOverflow {
        what: "section length",
    })?;
    out.write_all(body)?;
    entries.push(Entry {
        type_id,
        flags: 0,
        offset: *offset,
        len,
        rows,
        crc32c: crc32c(body),
    });
    *offset = offset
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "segment offset",
        })?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DictionaryValue {
    String(Vec<u8>),
    Blob {
        bytes: Vec<u8>,
        full_len: u64,
        truncated: bool,
        full_sha256: Option<[u8; 32]>,
    },
}

#[derive(Debug, Default)]
struct NormalizedDictionary {
    values: BTreeMap<StrId, DictionaryValue>,
    stored_bytes: usize,
}

impl NormalizedDictionary {
    fn insert(&mut self, str_id: StrId, value: DictionaryValue) -> Result<(), SealError> {
        match self.values.get(&str_id) {
            Some(existing) if existing == &value => return Ok(()),
            Some(_) => {
                return Err(SealError::DictionaryConflict {
                    str_id: str_id.get(),
                });
            }
            None => {}
        }
        let value_bytes = match &value {
            DictionaryValue::String(bytes) | DictionaryValue::Blob { bytes, .. } => bytes.len(),
        };
        self.stored_bytes = self.stored_bytes.checked_add(value_bytes).ok_or(
            SealError::ArithmeticOverflow {
                what: "dictionary stored bytes",
            },
        )?;
        if self.stored_bytes > MAX_SECTION_BYTES {
            return Err(CodecError::SectionTooLarge {
                len: self.stored_bytes,
                max: MAX_SECTION_BYTES,
            }
            .into());
        }
        if self.values.len() >= MAX_SECTION_ROWS {
            return Err(CodecError::TooManyRows {
                rows: self.values.len() + 1,
                max: MAX_SECTION_ROWS,
            }
            .into());
        }
        self.values.insert(str_id, value);
        Ok(())
    }

    fn sections(&self) -> Result<Vec<crate::dict::DictSection>, SealError> {
        let snapshots = self.values.iter().map(|(&str_id, value)| {
            let (stored_bytes, full_len, truncated, full_sha256, placement) = match value {
                DictionaryValue::String(bytes) => (
                    bytes.as_slice(),
                    bytes.len() as u64,
                    false,
                    None,
                    Placement::Strings,
                ),
                DictionaryValue::Blob {
                    bytes,
                    full_len,
                    truncated,
                    full_sha256,
                } => (
                    bytes.as_slice(),
                    *full_len,
                    *truncated,
                    *full_sha256,
                    Placement::Blobs,
                ),
            };
            EntrySnapshot {
                str_id,
                stored_bytes,
                full_len,
                truncated,
                full_sha256,
                placement,
                hot: HotMark::None,
                blob_required: placement == Placement::Blobs,
            }
        });
        crate::dict::encode_sealed_entries(snapshots).map_err(SealError::Codec)
    }
}

fn normalize_dictionary(
    journal: &Journal,
    strings: &[SectionDescriptor],
    blobs: &[SectionDescriptor],
) -> Result<NormalizedDictionary, SealError> {
    let mut normalized = NormalizedDictionary::default();
    for &descriptor in strings.iter().chain(blobs) {
        decode_dictionary_body(journal, descriptor, &mut normalized)?;
    }
    Ok(normalized)
}

#[allow(
    clippy::too_many_lines,
    reason = "one pass validates ordering, schema, hashes, and blob metadata"
)]
fn decode_dictionary_body(
    journal: &Journal,
    descriptor: SectionDescriptor,
    normalized: &mut NormalizedDictionary,
) -> Result<(), SealError> {
    let type_id = descriptor.entry.type_id;
    let is_blob = match type_id {
        DICT_STRINGS_TYPE_ID => false,
        DICT_BLOBS_TYPE_ID => true,
        _ => return Err(CodecError::UnknownType { type_id }.into()),
    };
    let body = read_verified_body(journal, descriptor)?.into_bytes();
    let builder = ParquetRecordBatchReaderBuilder::try_new(body)?;
    let groups = builder.metadata().num_row_groups();
    if groups > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups,
            max: MAX_ROW_GROUPS,
        }
        .into());
    }
    let claimed = builder.metadata().file_metadata().num_rows();
    let claimed_rows = usize::try_from(claimed)
        .ok()
        .filter(|&rows| rows <= MAX_SECTION_ROWS)
        .ok_or_else(|| match usize::try_from(claimed) {
            Ok(rows) => CodecError::TooManyRows {
                rows,
                max: MAX_SECTION_ROWS,
            },
            Err(_) => CodecError::InvalidRowCount { raw: claimed },
        })?;
    if claimed_rows != descriptor.entry.rows as usize {
        return Err(SealError::RowCountMismatch {
            type_id,
            declared: descriptor.entry.rows,
            decoded: claimed_rows,
        });
    }
    if !dictionary_schema_matches(builder.schema(), is_blob) {
        return Err(CodecError::SchemaMismatch.into());
    }

    let mut previous = 0_u64;
    let mut decoded_rows = 0_usize;
    for batch in builder.with_batch_size(4_096).build()? {
        let batch = batch?;
        decoded_rows = decoded_rows.checked_add(batch.num_rows()).ok_or(
            SealError::ArithmeticOverflow {
                what: "dictionary row count",
            },
        )?;
        let ids = required_u64(&batch, "str_id")?;
        if is_blob {
            let bytes = required_binary(&batch, "stored_bytes")?;
            let full_len = required_u64(&batch, "full_len")?;
            let truncated = required_bool(&batch, "truncated")?;
            let full_sha256 = fixed_binary(&batch, "full_sha256")?;
            for row in 0..batch.num_rows() {
                let str_id = ordered_str_id(ids.value(row), &mut previous)?;
                let stored = bytes.value(row);
                let full_len = full_len.value(row);
                let truncated = truncated.value(row);
                let full_sha256 = if full_sha256.is_null(row) {
                    None
                } else {
                    Some(
                        full_sha256
                            .value(row)
                            .try_into()
                            .map_err(|_error| CodecError::SchemaMismatch)?,
                    )
                };
                let valid = if truncated {
                    (stored.len() as u64) < full_len && full_sha256.is_some()
                } else {
                    stored.len() as u64 == full_len
                        && full_sha256.is_none()
                        && StrId::of(stored) == Some(str_id)
                };
                if !valid {
                    return Err(CodecError::SchemaMismatch.into());
                }
                normalized.insert(
                    str_id,
                    DictionaryValue::Blob {
                        bytes: stored.to_vec(),
                        full_len,
                        truncated,
                        full_sha256,
                    },
                )?;
            }
        } else {
            let bytes = required_binary(&batch, "bytes")?;
            for row in 0..batch.num_rows() {
                let str_id = ordered_str_id(ids.value(row), &mut previous)?;
                let stored = bytes.value(row);
                if StrId::of(stored) != Some(str_id) {
                    return Err(CodecError::SchemaMismatch.into());
                }
                normalized.insert(str_id, DictionaryValue::String(stored.to_vec()))?;
            }
        }
    }
    if decoded_rows != claimed_rows {
        return Err(SealError::RowCountMismatch {
            type_id,
            declared: descriptor.entry.rows,
            decoded: decoded_rows,
        });
    }
    Ok(())
}

fn ordered_str_id(raw: u64, previous: &mut u64) -> Result<StrId, SealError> {
    let str_id = StrId::from_raw(raw).ok_or(CodecError::SchemaMismatch)?;
    if raw <= *previous {
        return Err(CodecError::SchemaMismatch.into());
    }
    *previous = raw;
    Ok(str_id)
}

fn dictionary_schema_matches(schema: &Schema, is_blob: bool) -> bool {
    let fields = schema.fields();
    if is_blob {
        fields.len() == 5
            && field_matches(&fields[0], "str_id", &DataType::UInt64, false)
            && field_matches(&fields[1], "stored_bytes", &DataType::Binary, false)
            && field_matches(&fields[2], "full_len", &DataType::UInt64, false)
            && field_matches(&fields[3], "truncated", &DataType::Boolean, false)
            && field_matches(
                &fields[4],
                "full_sha256",
                &DataType::FixedSizeBinary(32),
                true,
            )
    } else {
        fields.len() == 2
            && field_matches(&fields[0], "str_id", &DataType::UInt64, false)
            && field_matches(&fields[1], "bytes", &DataType::Binary, false)
    }
}

fn field_matches(field: &Field, name: &str, data_type: &DataType, nullable: bool) -> bool {
    field.name() == name && field.data_type() == data_type && field.is_nullable() == nullable
}

fn required_u64<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a UInt64Array, CodecError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or(CodecError::ColumnType { name })?;
    reject_nulls(array, name).map(|()| array)
}

fn required_binary<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BinaryArray, CodecError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BinaryArray>())
        .ok_or(CodecError::ColumnType { name })?;
    reject_nulls(array, name).map(|()| array)
}

fn required_bool<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BooleanArray, CodecError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BooleanArray>())
        .ok_or(CodecError::ColumnType { name })?;
    reject_nulls(array, name).map(|()| array)
}

fn fixed_binary<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a FixedSizeBinaryArray, CodecError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<FixedSizeBinaryArray>())
        .ok_or(CodecError::ColumnType { name })
}

fn reject_nulls(array: &dyn Array, name: &'static str) -> Result<(), CodecError> {
    if array.null_count() == 0 {
        Ok(())
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

/// fsync the directory holding `dest` so the new link survives a crash.
fn sync_parent_dir(dest: &Path) -> io::Result<()> {
    if let Some(dir) = dest.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kronika_format::{DictLimits, validate_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::{Bytes, Ts, VerifiedSection, decode_any};

    use super::{SealError, seal};
    use crate::{Interner, Journal, JournalConfig, SectionBuffers, dict};

    fn bgwriter(ts: i64) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts: Ts(ts),
            checkpoints_timed: 10,
            checkpoints_req: 2,
            checkpoint_write_time: 1.0,
            checkpoint_sync_time: 2.0,
            buffers_checkpoint: 4096,
            restartpoints_timed: None,
            restartpoints_req: None,
            restartpoints_done: None,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_backend: Some(128),
            buffers_backend_fsync: Some(0),
            buffers_alloc: 9000,
            bgwriter_stats_reset: Ts(ts - 100),
            checkpointer_stats_reset: None,
        }
    }

    /// One collection window: buffer a bgwriter row and append its part.
    fn append_window(journal: &mut Journal, ts: i64) {
        let mut buffers = SectionBuffers::new();
        buffers.push(bgwriter(ts)).expect("buffer not full");
        let part = buffers.flush(&[], 0).expect("encode").expect("a part");
        journal.append(&part).expect("append");
    }

    #[test]
    fn seals_journal_parts_into_a_readable_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let segment_path = dir.path().join("143000.pgm");

        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1_000);
        append_window(&mut journal, 2_000);

        let summary = seal(&journal, &segment_path).expect("seal");
        assert_eq!(summary.sections, 1, "one bgwriter section per segment");
        assert_eq!((summary.min_ts, summary.max_ts), (1_000, 2_000));

        // A chartless segment has the same container shape as a PGM part.
        let segment = std::fs::read(&segment_path).expect("read segment");
        assert_eq!(u64::try_from(segment.len()).unwrap(), summary.bytes);
        let catalog = validate_part(&segment).expect("segment validates");
        let [entry] = catalog.entries.as_slice() else {
            panic!("the sealed segment must coalesce to one section");
        };
        assert_eq!(entry.type_id, 1_006_001);
        let start = usize::try_from(entry.offset).unwrap();
        let len = usize::try_from(entry.len).unwrap();
        let body = Bytes::copy_from_slice(&segment[start..start + len]);
        let verified = VerifiedSection::verify(body, entry.crc32c, kronika_format::crc32c)
            .expect("section crc matches");
        assert_eq!(
            decode_any(1_006_001, verified).expect("decode").stats.rows,
            2
        );
    }

    #[test]
    fn a_sealed_segment_carries_the_window_dictionary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let segment_path = dir.path().join("d.pgm");
        let (mut journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("open journal");

        // Intern two short strings and encode the window dictionary.
        let mut interner = Interner::new(DictLimits::new(4096, 1 << 20).expect("limits"));
        interner.intern(b"db-host-01").expect("intern");
        interner.intern(b"node-7").expect("intern");
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");

        // One data section plus the dictionary in a single part.
        let mut buffers = SectionBuffers::new();
        buffers.push(bgwriter(1_000)).expect("buffer not full");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush")
            .expect("a part");
        journal.append(&part).expect("append");

        let summary = seal(&journal, &segment_path).expect("seal");
        assert_eq!(summary.sections, 2, "bgwriter + dict.strings");

        let segment = std::fs::read(&segment_path).expect("read segment");
        let catalog = validate_part(&segment).expect("segment validates");
        let dict_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == kronika_registry::DICT_STRINGS_TYPE_ID)
            .expect("the dictionary section reached the segment");
        assert_eq!(dict_entry.rows, 2, "both interned strings");
        let start = usize::try_from(dict_entry.offset).unwrap();
        let end = start + usize::try_from(dict_entry.len).unwrap();
        assert_eq!(&segment[start..start + 4], b"PAR1", "a Parquet dict body");
        assert_eq!(&segment[end - 4..end], b"PAR1", "intact to its last byte");
    }

    #[test]
    fn sealing_an_empty_journal_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("open journal");
        assert!(matches!(
            seal(&journal, &dir.path().join("s.pgm")),
            Err(SealError::Empty)
        ));
    }

    #[test]
    fn an_existing_segment_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let segment_path = dir.path().join("s.pgm");
        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);

        seal(&journal, &segment_path).expect("first seal");
        let err = seal(&journal, &segment_path).expect_err("must not overwrite");
        assert!(matches!(err, SealError::AlreadyExists));
    }
}
