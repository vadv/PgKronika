//! Segment completion: merge the journal's parts into one immutable segment.
//!
//! Coalesces collection-window sections by type into a temporary file and
//! writes the end catalog last.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use arrow_array::{
    Array, BinaryArray, BooleanArray, FixedSizeBinaryArray, RecordBatch, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use kronika_format::{
    Catalog, Entry, EntrySnapshot, FORMAT_VERSION, HotMark, MAGIC, PartError, PartRef, Placement,
    StrId, crc32c, validate_part_catalog,
};
use kronika_registry::{
    Bytes, CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, MAX_DECODED_SECTION_BYTES,
    MAX_ROW_GROUPS, MAX_SECTION_BYTES, MAX_SECTION_ROWS, VerifiedSection, decode_any,
    encode_sealed_batches, sealed_data_body_bound, validate_parquet_decode_work,
};
use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};

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
    /// The journal contains more section descriptors than sealing will retain.
    TooManySections {
        /// Descriptor count encountered.
        sections: usize,
        /// Enforced bound.
        max: usize,
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
            Self::TooManySections { sections, max } => {
                write!(
                    f,
                    "journal has {sections} sections, above the limit of {max}"
                )
            }
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
            | Self::ArithmeticOverflow { .. }
            | Self::TooManySections { .. } => None,
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
/// `dest` is never overwritten. An existing byte-identical destination is
/// accepted as an idempotent retry; a different one is rejected. This function
/// never changes the journal, so call [`Journal::reset`] only after `Ok`.
///
/// # Errors
///
/// Returns [`SealError`] when the journal is empty, a part is invalid, I/O
/// fails, or `dest` exists with different bytes.
pub fn seal(journal: &Journal, dest: &Path) -> Result<SealSummary, SealError> {
    if journal.parts().is_empty() {
        return Err(SealError::Empty);
    }

    let (tmp, file) = create_tmp(dest)?;
    let summary = match write_tmp(journal, file) {
        Ok(summary) => summary,
        Err(err) => {
            fs::remove_file(&tmp).ok();
            return Err(err);
        }
    };
    // Hard-link publish fails if `dest` exists. The data file is already synced.
    if let Err(err) = fs::hard_link(&tmp, dest) {
        if err.kind() != io::ErrorKind::AlreadyExists {
            fs::remove_file(&tmp).ok();
            return Err(SealError::Io(err));
        }
        let identical = match files_equal(&tmp, dest) {
            Ok(identical) => identical,
            Err(err) => {
                fs::remove_file(&tmp).ok();
                return Err(err);
            }
        };
        if !identical {
            fs::remove_file(&tmp).ok();
            return Err(SealError::AlreadyExists);
        }
        let sync = sync_parent_dir(dest);
        fs::remove_file(&tmp).ok();
        sync?;
        return Ok(summary);
    }
    // Make the new link durable before the temporary name is removed.
    let sync = sync_parent_dir(dest);
    fs::remove_file(&tmp).ok();
    sync?;
    Ok(summary)
}

/// Create one invocation-owned temporary beside `dest` without removing stale files.
fn create_tmp(dest: &Path) -> Result<(PathBuf, File), SealError> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    for _attempt in 0..64 {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut name = dest.as_os_str().to_owned();
        name.push(format!(".{}.{seq}.tmp", std::process::id()));
        let path = PathBuf::from(name);
        match File::options().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(SealError::Io(err)),
        }
    }
    Err(SealError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a segment temporary path",
    )))
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, SealError> {
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    let mut left_buf = vec![0_u8; 64 * 1024];
    let mut right_buf = vec![0_u8; 64 * 1024];
    loop {
        let left_len = left.read(&mut left_buf)?;
        let right_len = right.read(&mut right_buf)?;
        if left_len != right_len {
            return Ok(false);
        }
        if left_buf[..left_len] != right_buf[..left_len] {
            return Ok(false);
        }
        if left_len == 0 {
            return Ok(true);
        }
    }
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
fn write_tmp(journal: &Journal, file: File) -> Result<SealSummary, SealError> {
    let mut plan = plan_segment(journal)?;
    let strings = plan
        .by_type
        .remove(&DICT_STRINGS_TYPE_ID)
        .unwrap_or_default();
    let blobs = plan.by_type.remove(&DICT_BLOBS_TYPE_ID).unwrap_or_default();
    let dictionary = normalize_dictionary(journal, &strings, &blobs)?;

    let mut out = BufWriter::new(file);

    out.write_all(&MAGIC)?;
    let mut offset = MAGIC.len() as u64;
    let mut entries: Vec<Entry> = Vec::new();

    for (type_id, descriptors) in plan.by_type {
        let declared_rows = aggregate_rows(type_id, &descriptors)?;
        let mut batches = Vec::<RecordBatch>::new();
        let mut decoded_rows = 0_usize;
        let mut list_i32_child_values = 0_usize;
        for descriptor in descriptors {
            let decoded = decode_any(type_id, read_verified_body(journal, descriptor)?)?;
            if decoded.stats.rows != descriptor.entry.rows as usize {
                return Err(SealError::RowCountMismatch {
                    type_id,
                    declared: descriptor.entry.rows,
                    decoded: decoded.stats.rows,
                });
            }
            let projected_rows = decoded_rows.checked_add(decoded.stats.rows).ok_or(
                SealError::ArithmeticOverflow {
                    what: "decoded row count",
                },
            )?;
            let projected_list_values = list_i32_child_values
                .checked_add(decoded.stats.list_i32_child_values)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "decoded ListI32 child count",
                })?;
            sealed_data_body_bound(type_id, projected_rows, projected_list_values)?;
            decoded_rows = projected_rows;
            list_i32_child_values = projected_list_values;
            batches.extend(decoded.batches);
        }
        if decoded_rows != declared_rows {
            return Err(SealError::RowCountMismatch {
                type_id,
                declared: u32::try_from(declared_rows).unwrap_or(u32::MAX),
                decoded: decoded_rows,
            });
        }
        let body = encode_sealed_batches(type_id, batches)?;
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
    let mut section_count = 0_usize;
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;
    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part_catalog(&part).map_err(SealError::Part)?;
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
            section_count = section_count
                .checked_add(1)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "section descriptor count",
                })?;
            if section_count > MAX_SECTION_ROWS {
                return Err(SealError::TooManySections {
                    sections: section_count,
                    max: MAX_SECTION_ROWS,
                });
            }
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

fn aggregate_rows(type_id: u32, descriptors: &[SectionDescriptor]) -> Result<usize, SealError> {
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
    let start = usize::try_from(descriptor.entry.offset).map_err(|_error| {
        SealError::ArithmeticOverflow {
            what: "section offset",
        }
    })?;
    let len =
        usize::try_from(descriptor.entry.len).map_err(|_error| SealError::ArithmeticOverflow {
            what: "section length",
        })?;
    let body = journal.read_part_range(descriptor.part, start, len)?;
    VerifiedSection::verify(Bytes::from(body), descriptor.entry.crc32c, crc32c)
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
    string_rows: usize,
    blob_rows: usize,
    string_bytes: usize,
    blob_bytes: usize,
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
        let (rows, stored_bytes, value_bytes) = match &value {
            DictionaryValue::String(bytes) => {
                (&mut self.string_rows, &mut self.string_bytes, bytes.len())
            }
            DictionaryValue::Blob { bytes, .. } => {
                (&mut self.blob_rows, &mut self.blob_bytes, bytes.len())
            }
        };
        let next_rows = rows.checked_add(1).ok_or(SealError::ArithmeticOverflow {
            what: "dictionary row count",
        })?;
        if next_rows > MAX_SECTION_ROWS {
            return Err(CodecError::TooManyRows {
                rows: next_rows,
                max: MAX_SECTION_ROWS,
            }
            .into());
        }
        let next_bytes =
            stored_bytes
                .checked_add(value_bytes)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "dictionary stored bytes",
                })?;
        if next_bytes > MAX_SECTION_BYTES {
            return Err(CodecError::SectionTooLarge {
                len: next_bytes,
                max: MAX_SECTION_BYTES,
            }
            .into());
        }
        *rows = next_rows;
        *stored_bytes = next_bytes;
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
    validate_parquet_decode_work(body.as_ref(), MAX_DECODED_SECTION_BYTES)?;
    let options = ArrowReaderOptions::new().with_skip_arrow_metadata(true);
    let builder = ParquetRecordBatchReaderBuilder::try_new_with_options(body, options)?;
    let groups = builder.metadata().num_row_groups();
    if groups > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups,
            max: MAX_ROW_GROUPS,
        }
        .into());
    }
    let claimed = builder.metadata().file_metadata().num_rows();
    let claimed_rows = match usize::try_from(claimed) {
        Ok(rows) if rows <= MAX_SECTION_ROWS => rows,
        Ok(rows) => {
            return Err(CodecError::TooManyRows {
                rows,
                max: MAX_SECTION_ROWS,
            }
            .into());
        }
        Err(_) => return Err(CodecError::InvalidRowCount { raw: claimed }.into()),
    };
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
        decoded_rows =
            decoded_rows
                .checked_add(batch.num_rows())
                .ok_or(SealError::ArithmeticOverflow {
                    what: "dictionary row count",
                })?;
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
    use kronika_format::{DictLimits, Entry, validate_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::{
        Bytes, DICT_STRINGS_TYPE_ID, PgLocksV2, Section, StrId, Ts, VerifiedSection, decode_any,
    };
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    use super::{SealError, required_binary, required_u64, seal};
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

    fn pg_locks(ts: i64, pid: i32) -> PgLocksV2 {
        PgLocksV2 {
            ts: Ts(ts),
            pid,
            blocked_by: vec![pid; 4_096],
            depth: 0,
            root_pid: pid,
            datid: 16_384,
            datname: StrId(1),
            usename: Some(StrId(2)),
            application_name: StrId(3),
            client_addr: StrId(4),
            backend_type: StrId(5),
            state: Some(StrId(6)),
            wait_event_type: None,
            wait_event: None,
            query: StrId(7),
            backend_xid_age: None,
            backend_xmin_age: None,
            backend_start: Some(Ts(ts - 60)),
            xact_start: Some(Ts(ts - 5)),
            query_start: Some(Ts(ts - 1)),
            state_change: Some(Ts(ts - 1)),
            lock_locktype: None,
            lock_mode: None,
            lock_granted: None,
            lock_database: None,
            lock_relation: None,
            lock_relname: None,
            lock_page: None,
            lock_tuple: None,
            lock_virtualxid: None,
            lock_transactionid: None,
            lock_classid: None,
            lock_objid: None,
            lock_objsubid: None,
            lock_fastpath: None,
            lock_target: None,
            waitstart: None,
        }
    }

    fn append_lock_window(journal: &mut Journal, ts: i64, first_pid: i32) {
        let mut buffers = SectionBuffers::new();
        for row in 0..33 {
            buffers
                .push(pg_locks(ts + i64::from(row), first_pid + row))
                .expect("lock row fits");
        }
        let part = buffers.flush(&[], 0).expect("encode").expect("a part");
        journal.append(&part).expect("append lock window");
    }

    #[derive(Clone, Copy)]
    struct FixtureTextIds {
        datname: StrId,
        usename: StrId,
        application_name: StrId,
        client_addr: StrId,
        backend_type: StrId,
        state: StrId,
        query: StrId,
    }

    fn fixture_dictionary() -> (Interner, FixtureTextIds) {
        let mut interner =
            Interner::new(DictLimits::new(4_096, 4_096).expect("small dictionary limits"));
        let mut intern =
            |bytes: &[u8]| StrId(interner.intern(bytes).expect("fixture text fits").get());
        let ids = FixtureTextIds {
            datname: intern(b"appdb"),
            usename: intern(b"alice"),
            application_name: intern(b"psql"),
            client_addr: intern(b"127.0.0.1"),
            backend_type: intern(b"client backend"),
            state: intern(b"active"),
            query: intern(b"select 1"),
        };
        (interner, ids)
    }

    fn lossless_bgwriter(write_time_bits: u64, pg17: bool) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts: Ts(42),
            checkpoint_write_time: f64::from_bits(write_time_bits),
            restartpoints_timed: pg17.then_some(7),
            restartpoints_req: pg17.then_some(1),
            restartpoints_done: pg17.then_some(6),
            buffers_backend: (!pg17).then_some(128),
            buffers_backend_fsync: (!pg17).then_some(0),
            checkpointer_stats_reset: pg17.then_some(Ts(7)),
            ..bgwriter(42)
        }
    }

    fn lossless_lock(
        ids: FixtureTextIds,
        pid: i32,
        blocked_by: Vec<i32>,
        nullable_text: bool,
    ) -> PgLocksV2 {
        PgLocksV2 {
            ts: Ts(84),
            pid,
            depth: i32::from(!blocked_by.is_empty()),
            root_pid: 10,
            blocked_by,
            datid: 16_384,
            datname: ids.datname,
            usename: nullable_text.then_some(ids.usename),
            application_name: ids.application_name,
            client_addr: ids.client_addr,
            backend_type: ids.backend_type,
            state: (!nullable_text).then_some(ids.state),
            query: ids.query,
            backend_xid_age: nullable_text.then_some(11),
            backend_xmin_age: None,
            backend_start: Some(Ts(12)),
            xact_start: nullable_text.then_some(Ts(13)),
            query_start: Some(Ts(14)),
            state_change: None,
            wait_event_type: None,
            wait_event: None,
            lock_locktype: None,
            lock_mode: None,
            lock_granted: None,
            lock_database: None,
            lock_relation: None,
            lock_relname: None,
            lock_page: None,
            lock_tuple: None,
            lock_virtualxid: None,
            lock_transactionid: None,
            lock_classid: None,
            lock_objid: None,
            lock_objsubid: None,
            lock_fastpath: None,
            lock_target: None,
            waitstart: nullable_text.then_some(Ts(15)),
        }
    }

    fn append_lossless_part(
        journal: &mut Journal,
        bgwriter_rows: &[BgwriterCheckpointer],
        lock_rows: &[PgLocksV2],
    ) {
        let (interner, _ids) = fixture_dictionary();
        let dictionary = dict::encode(interner.window()).expect("encode fixture dictionary");
        let mut buffers = SectionBuffers::new();
        for &row in bgwriter_rows {
            buffers.push(row).expect("bgwriter row fits");
        }
        for row in lock_rows {
            buffers.push(row.clone()).expect("lock row fits");
        }
        let part = buffers
            .flush(&dictionary, 7)
            .expect("encode fixture part")
            .expect("fixture part has rows");
        journal.append(&part).expect("append fixture part");
    }

    fn verified_section(segment: &[u8], entry: &Entry) -> VerifiedSection {
        let start = usize::try_from(entry.offset).expect("fixture offset fits");
        let len = usize::try_from(entry.len).expect("fixture length fits");
        VerifiedSection::verify(
            Bytes::copy_from_slice(&segment[start..start + len]),
            entry.crc32c,
            kronika_format::crc32c,
        )
        .expect("fixture section CRC matches")
    }

    fn decode_string_dictionary(section: VerifiedSection) -> Vec<(u64, Vec<u8>)> {
        let reader = ParquetRecordBatchReaderBuilder::try_new(section.into_bytes())
            .expect("open string dictionary")
            .build()
            .expect("build string dictionary reader");
        let mut entries = Vec::new();
        for batch in reader {
            let batch = batch.expect("decode string dictionary batch");
            let ids = required_u64(&batch, "str_id").expect("dictionary ids");
            let bytes = required_binary(&batch, "bytes").expect("dictionary bytes");
            entries.extend(
                (0..batch.num_rows()).map(|row| (ids.value(row), bytes.value(row).to_vec())),
            );
        }
        entries
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
            .find(|entry| entry.type_id == DICT_STRINGS_TYPE_ID)
            .expect("the dictionary section reached the segment");
        assert_eq!(dict_entry.rows, 2, "both interned strings");
        let start = usize::try_from(dict_entry.offset).unwrap();
        let end = start + usize::try_from(dict_entry.len).unwrap();
        assert_eq!(&segment[start..start + 4], b"PAR1", "a Parquet dict body");
        assert_eq!(&segment[end - 4..end], b"PAR1", "intact to its last byte");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end assertion keeps the two equivalent journals and all lossless fields together"
    )]
    fn sealing_is_lossless_across_journal_order_and_partitioning() {
        const NAN_1: u64 = 0x7ff8_0000_0000_0001;
        const NAN_2: u64 = 0x7ff8_0000_0000_0002;
        const NEGATIVE_ZERO: u64 = (-0.0_f64).to_bits();
        const POSITIVE_ZERO: u64 = 0.0_f64.to_bits();

        let dir = tempfile::tempdir().expect("tempdir");
        let first_journal_path = dir.path().join("first.parts");
        let second_journal_path = dir.path().join("second.parts");
        let (mut first, _) = Journal::open(&first_journal_path, JournalConfig::default())
            .expect("open first journal");
        let (mut second, _) = Journal::open(&second_journal_path, JournalConfig::default())
            .expect("open second journal");
        let (_interner, ids) = fixture_dictionary();

        let nan_1 = lossless_bgwriter(NAN_1, false);
        let nan_2 = lossless_bgwriter(NAN_2, true);
        let negative_zero = lossless_bgwriter(NEGATIVE_ZERO, false);
        let positive_zero = lossless_bgwriter(POSITIVE_ZERO, true);
        let root = lossless_lock(ids, 10, Vec::new(), false);
        let blocked = lossless_lock(ids, 20, vec![10, 0], true);

        append_lossless_part(
            &mut first,
            &[nan_2, negative_zero],
            std::slice::from_ref(&blocked),
        );
        append_lossless_part(
            &mut first,
            &[positive_zero, nan_1, negative_zero],
            &[root.clone(), blocked.clone()],
        );

        append_lossless_part(
            &mut second,
            &[negative_zero, nan_1, positive_zero],
            &[blocked.clone(), root.clone()],
        );
        append_lossless_part(&mut second, &[nan_2], &[]);
        append_lossless_part(
            &mut second,
            &[negative_zero],
            std::slice::from_ref(&blocked),
        );
        assert_ne!(
            std::fs::read(&first_journal_path).expect("read first journal"),
            std::fs::read(&second_journal_path).expect("read second journal"),
            "the equivalent input uses different order and part boundaries"
        );

        let first_path = dir.path().join("first.pgm");
        let second_path = dir.path().join("second.pgm");
        let first_summary = seal(&first, &first_path).expect("seal first journal");
        let second_summary = seal(&second, &second_path).expect("seal second journal");
        let segment = std::fs::read(&first_path).expect("read first segment");
        assert_eq!(first_summary, second_summary);
        assert_eq!(
            segment,
            std::fs::read(&second_path).expect("read second segment"),
            "equivalent journals must produce exact PGM bytes"
        );
        assert_eq!((first_summary.min_ts, first_summary.max_ts), (42, 84));

        let catalog = validate_part(&segment).expect("sealed PGM validates");
        let bgwriter_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == BgwriterCheckpointer::CONTRACT.type_id.get())
            .expect("bgwriter section");
        let decoded_bgwriter =
            BgwriterCheckpointer::decode(verified_section(&segment, bgwriter_entry))
                .expect("decode bgwriter rows");
        assert_eq!(
            decoded_bgwriter
                .iter()
                .map(|row| row.checkpoint_write_time.to_bits())
                .collect::<Vec<_>>(),
            [NEGATIVE_ZERO, NEGATIVE_ZERO, POSITIVE_ZERO, NAN_1, NAN_2],
            "raw float bits, canonical order, and duplicate rows survive sealing"
        );
        assert_eq!(decoded_bgwriter[0].buffers_backend, Some(128));
        assert_eq!(decoded_bgwriter[0].restartpoints_timed, None);
        assert_eq!(decoded_bgwriter[2].buffers_backend, None);
        assert_eq!(decoded_bgwriter[2].restartpoints_timed, Some(7));
        assert_eq!(decoded_bgwriter[2].checkpointer_stats_reset, Some(Ts(7)));

        let locks_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == PgLocksV2::CONTRACT.type_id.get())
            .expect("locks section");
        let decoded_locks =
            PgLocksV2::decode(verified_section(&segment, locks_entry)).expect("decode lock rows");
        assert_eq!(
            decoded_locks,
            [root, blocked.clone(), blocked.clone()],
            "canonical rows retain the duplicate lock observation"
        );
        assert!(decoded_locks[0].blocked_by.is_empty());
        assert_eq!(decoded_locks[0].usename, None);
        assert_eq!(decoded_locks[0].state, Some(ids.state));
        assert_eq!(decoded_locks[1].blocked_by, [10, 0]);
        assert_eq!(decoded_locks[1].usename, Some(ids.usename));
        assert_eq!(decoded_locks[1].state, None);
        assert_eq!(decoded_locks[1].xact_start, Some(Ts(13)));
        assert_eq!(decoded_locks[1].state_change, None);

        let dictionary_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == DICT_STRINGS_TYPE_ID)
            .expect("string dictionary section");
        let dictionary = decode_string_dictionary(verified_section(&segment, dictionary_entry));
        let mut expected_dictionary = vec![
            (ids.datname.0, b"appdb".to_vec()),
            (ids.usename.0, b"alice".to_vec()),
            (ids.application_name.0, b"psql".to_vec()),
            (ids.client_addr.0, b"127.0.0.1".to_vec()),
            (ids.backend_type.0, b"client backend".to_vec()),
            (ids.state.0, b"active".to_vec()),
            (ids.query.0, b"select 1".to_vec()),
        ];
        expected_dictionary.sort_unstable_by_key(|entry| entry.0);
        assert_eq!(dictionary, expected_dictionary);
    }

    #[test]
    fn aggregate_list_bound_is_checked_before_retaining_the_next_part() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let segment_path = dir.path().join("locks.pgm");
        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("open journal");
        append_lock_window(&mut journal, 1_000, 1);
        append_lock_window(&mut journal, 2_000, 100);
        let journal_before = std::fs::read(&journal_path).expect("snapshot journal");

        let err = seal(&journal, &segment_path).expect_err("aggregate list stream is rejected");

        assert!(matches!(
            err,
            SealError::Codec(kronika_registry::CodecError::TooManyListValues {
                name: "blocked_by",
                ..
            })
        ));
        assert_eq!(
            std::fs::read(&journal_path).expect("read journal"),
            journal_before
        );
        assert_eq!(journal.parts().len(), 2);
        assert!(!segment_path.exists());
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
    fn resealing_the_same_journal_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let segment_path = dir.path().join("s.pgm");
        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);

        let first = seal(&journal, &segment_path).expect("first seal");
        let bytes = std::fs::read(&segment_path).expect("read first segment");
        let second = seal(&journal, &segment_path).expect("idempotent retry");

        assert_eq!(second, first);
        assert_eq!(std::fs::read(&segment_path).expect("read retry"), bytes);
        assert_eq!(journal.parts().len(), 1, "seal never resets the journal");
    }

    #[test]
    fn a_different_existing_segment_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let segment_path = dir.path().join("s.pgm");
        let (mut first, _) =
            Journal::open(&dir.path().join("first.parts"), JournalConfig::default())
                .expect("open first journal");
        append_window(&mut first, 1);
        seal(&first, &segment_path).expect("first seal");
        let segment_before = std::fs::read(&segment_path).expect("read destination");

        let second_path = dir.path().join("second.parts");
        let (mut second, _) =
            Journal::open(&second_path, JournalConfig::default()).expect("open second journal");
        append_window(&mut second, 2);
        let journal_before = std::fs::read(&second_path).expect("read second journal");
        let err = seal(&second, &segment_path).expect_err("must not overwrite");

        assert!(matches!(err, SealError::AlreadyExists));
        assert_eq!(
            std::fs::read(&segment_path).expect("read destination after collision"),
            segment_before
        );
        assert_eq!(
            std::fs::read(&second_path).expect("read journal after collision"),
            journal_before
        );
        assert_eq!(second.parts().len(), 1);
    }
}
