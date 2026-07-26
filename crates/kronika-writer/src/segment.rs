//! Bounded completion of journal parts into one immutable compact PGM.
//!
//! Sealing makes two bounded passes over the synchronized journal. The first
//! validates every part, closes registry/row/catalog arithmetic, and admits
//! the complete job against memory and disk limits. The second decodes one
//! input section at a time, canonicalizes it, and spills a compact sorted run.
//! Runs are then coalesced one registered type at a time. A segment is never
//! materialized as one in-memory object.
//!
//! Publication uses a sibling `create_new` temporary, full data sync, a
//! streaming reopen/CRC verification, no-overwrite hard-link publication,
//! directory sync, temporary unlink, and a second directory sync. An exact
//! already-published file is idempotent; a different file at the destination
//! is a conflict. The caller clears the journal only after success.

use std::collections::{BTreeMap, btree_map};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use arrow_array::{
    Array, ArrayRef, BinaryArray, BooleanArray, FixedSizeBinaryArray, RecordBatch, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use kronika_format::{
    Catalog, Crc32c, Entry, FORMAT_VERSION, MAGIC, PartError, TAIL_INDEX_LEN, TailIndex, crc32c,
    validate_part,
};
use kronika_registry::{
    Bytes, COMPACTION_MEMORY_LIMIT, CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID,
    MAX_ROW_GROUPS, MAX_SECTION_BYTES, MAX_SECTION_ROWS, VerifiedSection, canonicalize_batches,
    compaction_memory_bound, decode_any, encode_compact_batch, encode_compact_ordered_batch,
    registry,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use sha2::{Digest, Sha256};

use crate::{DEFAULT_MAX_JOURNAL_LEN, Journal, JournalError};

/// Maximum catalog sections accepted from one seal input by default.
pub const DEFAULT_MAX_INPUT_SECTIONS: usize = 65_536;

/// Hard resources admitted before seal work begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealLimits {
    /// Peak estimated decoded/sort working memory.
    pub max_memory_bytes: usize,
    /// Aggregate bytes allowed for external sorted runs.
    pub max_spill_bytes: u64,
    /// Maximum completed temporary PGM length.
    pub max_output_bytes: u64,
    /// Maximum catalog entries across all input parts.
    pub max_input_sections: usize,
}

impl Default for SealLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: COMPACTION_MEMORY_LIMIT,
            max_spill_bytes: DEFAULT_MAX_JOURNAL_LEN as u64,
            max_output_bytes: DEFAULT_MAX_JOURNAL_LEN as u64,
            max_input_sections: DEFAULT_MAX_INPUT_SECTIONS,
        }
    }
}

/// Seal controls, including an optional cooperative cancellation flag.
#[derive(Debug, Clone, Copy)]
pub struct SealOptions<'a> {
    /// Resource bounds checked before and during the seal.
    pub limits: SealLimits,
    /// When set, sealing stops between bounded units of work.
    pub cancelled: Option<&'a AtomicBool>,
}

impl Default for SealOptions<'_> {
    fn default() -> Self {
        Self {
            limits: SealLimits::default(),
            cancelled: None,
        }
    }
}

/// How the immutable destination became visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// This seal published the destination.
    Created,
    /// Exact deterministic bytes were already present.
    AlreadyPresent,
}

/// What a completed segment contains, including physical work accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealSummary {
    /// Number of canonical catalog entries written.
    pub sections: usize,
    /// Total logical data rows (dictionary rows excluded).
    pub rows: u64,
    /// Total segment length, bytes.
    pub bytes: u64,
    /// Synchronized source journal length, bytes.
    pub source_bytes: u64,
    /// Aggregate external-run bytes written.
    pub spill_bytes: u64,
    /// Temporary write bytes (`spill_bytes + bytes`).
    pub write_bytes: u64,
    /// Largest admitted data-type plus dictionary working set.
    pub admitted_memory_bytes: usize,
    /// Minimal timestamp across the segment, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp across the segment, unix microseconds.
    pub max_ts: i64,
    /// Publication outcome.
    pub publication: Publication,
}

/// Which hard seal resource rejected a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealResource {
    /// Estimated Arrow/dictionary working memory.
    Memory,
    /// External sorted-run bytes.
    SpillDisk,
    /// Temporary output PGM bytes.
    OutputDisk,
    /// Number of input catalog entries.
    InputSections,
}

impl fmt::Display for SealResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Memory => "memory",
            Self::SpillDisk => "spill disk",
            Self::OutputDisk => "output disk",
            Self::InputSections => "input sections",
        };
        f.write_str(name)
    }
}

/// Why exact dictionary normalization failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryError {
    /// Dictionary Parquet schema differs from the current exact schema.
    Schema {
        /// Dictionary `type_id`.
        type_id: u32,
    },
    /// Catalog and Parquet row counts disagree.
    RowCount {
        /// Dictionary `type_id`.
        type_id: u32,
        /// Catalog row count.
        declared: u32,
        /// Decoded or metadata row count.
        actual: u64,
    },
    /// IDs inside one section are not strictly increasing and non-zero.
    IdOrder {
        /// Dictionary `type_id`.
        type_id: u32,
        /// Previous id, or zero for an invalid first id.
        previous: u64,
        /// Current id.
        current: u64,
    },
    /// A full value does not hash to its stored `str_id`.
    IdMismatch {
        /// Rejected id.
        str_id: u64,
    },
    /// Blob truncation/full-length/hash metadata is internally inconsistent.
    BlobMetadata {
        /// Rejected id.
        str_id: u64,
    },
    /// Repeated rows for one id carry different exact representations.
    Conflict {
        /// Conflicting id.
        str_id: u64,
    },
    /// String/blob placement rows for one id do not describe the same value.
    PlacementConflict {
        /// Conflicting id.
        str_id: u64,
    },
    /// Canonical output would exceed the dictionary row cap.
    TooManyEntries {
        /// Distinct dictionary entries.
        entries: usize,
        /// Enforced cap.
        max: usize,
    },
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema { type_id } => {
                write!(f, "dictionary type {type_id} has a non-current schema")
            }
            Self::RowCount {
                type_id,
                declared,
                actual,
            } => write!(
                f,
                "dictionary type {type_id} declares {declared} rows but contains {actual}"
            ),
            Self::IdOrder {
                type_id,
                previous,
                current,
            } => write!(
                f,
                "dictionary type {type_id} id {current:#018x} does not follow {previous:#018x}"
            ),
            Self::IdMismatch { str_id } => {
                write!(f, "dictionary id {str_id:#018x} does not match its bytes")
            }
            Self::BlobMetadata { str_id } => {
                write!(f, "dictionary blob {str_id:#018x} has invalid metadata")
            }
            Self::Conflict { str_id } => {
                write!(f, "dictionary id {str_id:#018x} has conflicting duplicates")
            }
            Self::PlacementConflict { str_id } => write!(
                f,
                "dictionary id {str_id:#018x} conflicts between string and blob placement"
            ),
            Self::TooManyEntries { entries, max } => {
                write!(
                    f,
                    "dictionary has {entries} entries, above the cap of {max}"
                )
            }
        }
    }
}

impl Error for DictionaryError {}

/// Why sealing a segment failed.
#[derive(Debug)]
pub enum SealError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// Reading a part back from the journal failed.
    Journal(JournalError),
    /// A journal part did not validate as a current canonical PGM.
    Part(PartError),
    /// A registered Arrow/Parquet section failed validation or encoding.
    Codec(CodecError),
    /// Exact dictionary normalization failed.
    Dictionary(DictionaryError),
    /// The journal holds no parts.
    Empty,
    /// The job crossed a configured hard resource.
    Resource {
        /// Rejected resource.
        resource: SealResource,
        /// Required amount.
        needed: u64,
        /// Configured maximum.
        limit: u64,
    },
    /// Two parts carry different non-zero `source_id`s.
    SourceIdMismatch {
        /// The first non-zero source id seen.
        expected: u64,
        /// A later conflicting source id.
        got: u64,
    },
    /// A part catalog has an invalid timestamp interval.
    InvalidTimestampRange {
        /// Lower endpoint.
        min_ts: i64,
        /// Upper endpoint.
        max_ts: i64,
    },
    /// A catalog references a `type_id` absent from the current registry.
    UnknownType {
        /// Unknown id.
        type_id: u32,
    },
    /// Catalog and decoded row counts disagree.
    RowCountMismatch {
        /// Registered id.
        type_id: u32,
        /// Catalog rows.
        declared: u32,
        /// Decoded rows.
        decoded: usize,
    },
    /// The current-format version field is inconsistent.
    UnsupportedFormat {
        /// Version found in a part.
        version: u32,
    },
    /// Checked integer arithmetic failed.
    ArithmeticOverflow {
        /// Operation whose bound overflowed.
        what: &'static str,
    },
    /// Cooperative cancellation was requested before publication.
    Cancelled,
    /// A different valid or partial file already occupies the destination.
    PublicationConflict {
        /// Conflicting destination.
        path: PathBuf,
    },
    /// Reopened temporary bytes differ from the just-built catalog.
    OutputVerification {
        /// Concise invariant that failed.
        reason: &'static str,
    },
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "segment I/O: {error}"),
            Self::Journal(error) => write!(f, "reading a journal part: {error}"),
            Self::Part(error) => write!(f, "invalid journal part: {error}"),
            Self::Codec(error) => write!(f, "section codec: {error}"),
            Self::Dictionary(error) => write!(f, "dictionary: {error}"),
            Self::Empty => write!(f, "the journal holds no parts to seal"),
            Self::Resource {
                resource,
                needed,
                limit,
            } => write!(
                f,
                "seal requires {needed} bytes/entries of {resource}, above the limit of {limit}"
            ),
            Self::SourceIdMismatch { expected, got } => {
                write!(f, "journal mixes source_id {expected} and {got}")
            }
            Self::InvalidTimestampRange { min_ts, max_ts } => {
                write!(
                    f,
                    "journal part has invalid timestamp range {min_ts}..{max_ts}"
                )
            }
            Self::UnknownType { type_id } => {
                write!(f, "journal part uses unknown type_id {type_id}")
            }
            Self::RowCountMismatch {
                type_id,
                declared,
                decoded,
            } => write!(
                f,
                "section {type_id} declares {declared} rows but decodes to {decoded}"
            ),
            Self::UnsupportedFormat { version } => {
                write!(f, "journal part uses unsupported format version {version}")
            }
            Self::ArithmeticOverflow { what } => {
                write!(f, "checked arithmetic overflow while computing {what}")
            }
            Self::Cancelled => write!(f, "segment seal was cancelled before publication"),
            Self::PublicationConflict { path } => write!(
                f,
                "destination {} already contains different bytes",
                path.display()
            ),
            Self::OutputVerification { reason } => {
                write!(f, "temporary PGM verification failed: {reason}")
            }
        }
    }
}

impl Error for SealError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Part(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::Dictionary(error) => Some(error),
            Self::Empty
            | Self::Resource { .. }
            | Self::SourceIdMismatch { .. }
            | Self::InvalidTimestampRange { .. }
            | Self::UnknownType { .. }
            | Self::RowCountMismatch { .. }
            | Self::UnsupportedFormat { .. }
            | Self::ArithmeticOverflow { .. }
            | Self::Cancelled
            | Self::PublicationConflict { .. }
            | Self::OutputVerification { .. } => None,
        }
    }
}

impl SealError {
    /// Whether an open segment can resolve this failure by sealing before the
    /// candidate part and admitting that part into a fresh segment.
    #[must_use]
    pub const fn is_admission_boundary(&self) -> bool {
        matches!(
            self,
            Self::Resource {
                resource: SealResource::Memory | SealResource::InputSections,
                ..
            } | Self::Codec(CodecError::TooManyRows { .. })
        )
    }
}

impl From<io::Error> for SealError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<JournalError> for SealError {
    fn from(error: JournalError) -> Self {
        Self::Journal(error)
    }
}

impl From<PartError> for SealError {
    fn from(error: PartError) -> Self {
        Self::Part(error)
    }
}

impl From<CodecError> for SealError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

impl From<parquet::errors::ParquetError> for SealError {
    fn from(error: parquet::errors::ParquetError) -> Self {
        Self::Codec(CodecError::Parquet(error))
    }
}

impl From<arrow_schema::ArrowError> for SealError {
    fn from(error: arrow_schema::ArrowError) -> Self {
        Self::Codec(CodecError::Arrow(error))
    }
}

impl From<DictionaryError> for SealError {
    fn from(error: DictionaryError) -> Self {
        Self::Dictionary(error)
    }
}

/// Seal journal parts with default production limits.
///
/// `dest` is never overwritten. The journal remains intact on every result;
/// the caller resets it only after `Ok`.
///
/// # Errors
///
/// Returns [`SealError`] on input corruption, admission, codec, cancellation,
/// I/O, verification, or publication conflict.
pub fn seal(journal: &Journal, dest: &Path) -> Result<SealSummary, SealError> {
    seal_with_options(journal, dest, SealOptions::default())
}

/// Seal journal parts with explicit hard limits and cancellation.
///
/// # Errors
///
/// Returns [`SealError`] under the same conditions as [`seal`].
pub fn seal_with_options(
    journal: &Journal,
    dest: &Path,
    options: SealOptions<'_>,
) -> Result<SealSummary, SealError> {
    if journal.parts().is_empty() {
        return Err(SealError::Empty);
    }
    check_cancelled(options.cancelled)?;
    let plan = plan_seal(journal, options.limits)?;
    check_cancelled(options.cancelled)?;

    let generation = next_generation();
    let mut artifacts = Artifacts::default();
    let (runs, dictionary, spill_bytes) =
        create_runs(journal, dest, generation, options, &mut artifacts)?;
    let tmp = artifact_path(dest, generation, "segment", 0);
    artifacts.track(tmp.clone());
    let built = write_compact_temp(&tmp, &plan, &runs, &dictionary, options, &mut artifacts)?;
    verify_temp(&tmp, &built.catalog, built.bytes)?;
    check_cancelled(options.cancelled)?;
    let publication = publish(&tmp, dest)?;

    let source_bytes =
        u64::try_from(journal.bytes()).map_err(|_overflow| SealError::ArithmeticOverflow {
            what: "source journal bytes",
        })?;
    let write_bytes =
        spill_bytes
            .checked_add(built.bytes)
            .ok_or(SealError::ArithmeticOverflow {
                what: "seal write bytes",
            })?;
    Ok(SealSummary {
        sections: built.catalog.entries.len(),
        rows: plan.total_rows,
        bytes: built.bytes,
        source_bytes,
        spill_bytes,
        write_bytes,
        admitted_memory_bytes: plan.admitted_memory_bytes,
        min_ts: plan.min_ts,
        max_ts: plan.max_ts,
        publication,
    })
}

#[derive(Debug, Clone, Default)]
struct TypePlan {
    rows: usize,
}

/// Incremental collector-side seal footprint.
///
/// The collector validates each just-encoded part and computes the next state
/// before appending it. This makes row, registry, catalog-count, and decoded
/// memory limits pre-admission boundaries rather than failures discovered only
/// after the journal has crossed them.
#[derive(Debug, Clone, Default)]
pub struct SealAdmission {
    types: BTreeMap<u32, TypePlan>,
    dictionary_decoded_bytes: usize,
    dictionary_rows: usize,
    input_sections: usize,
}

impl SealAdmission {
    /// Return the admitted state after adding `part`, without mutating `self`.
    ///
    /// # Errors
    ///
    /// Returns [`SealError`] when the part is corrupt/obsolete/unknown or the
    /// resulting open segment crosses row, catalog, or memory limits.
    pub fn with_part(&self, part: &[u8], limits: SealLimits) -> Result<Self, SealError> {
        let catalog = validate_part(part)?;
        if catalog.format_version != FORMAT_VERSION {
            return Err(SealError::UnsupportedFormat {
                version: catalog.format_version,
            });
        }
        let mut next = self.clone();
        next.input_sections = next
            .input_sections
            .checked_add(catalog.entries.len())
            .ok_or(SealError::ArithmeticOverflow {
                what: "collector input sections",
            })?;
        admit_u64(
            SealResource::InputSections,
            next.input_sections as u64,
            limits.max_input_sections as u64,
        )?;
        for entry in &catalog.entries {
            let body = section_slice(part, entry)?;
            if is_dictionary(entry.type_id) {
                let metadata = inspect_dictionary(body, entry.type_id, entry.rows)?;
                next.dictionary_decoded_bytes = next
                    .dictionary_decoded_bytes
                    .checked_add(metadata.decoded_bytes)
                    .ok_or(SealError::ArithmeticOverflow {
                        what: "collector dictionary bytes",
                    })?;
                next.dictionary_rows = next.dictionary_rows.checked_add(metadata.rows).ok_or(
                    SealError::ArithmeticOverflow {
                        what: "collector dictionary rows",
                    },
                )?;
            } else {
                ensure_registered(entry.type_id)?;
                let type_plan = next.types.entry(entry.type_id).or_default();
                type_plan.rows = type_plan.rows.checked_add(entry.rows as usize).ok_or(
                    SealError::ArithmeticOverflow {
                        what: "collector rows per type",
                    },
                )?;
                if type_plan.rows > MAX_SECTION_ROWS {
                    return Err(CodecError::TooManyRows {
                        rows: type_plan.rows,
                        max: MAX_SECTION_ROWS,
                    }
                    .into());
                }
            }
        }
        let dictionary = dictionary_memory_bound(
            next.dictionary_decoded_bytes,
            next.dictionary_rows,
            next.input_sections,
        )?;
        let mut peak = dictionary;
        for (&type_id, type_plan) in &next.types {
            peak = peak.max(
                dictionary
                    .checked_add(compaction_memory_bound(type_id, type_plan.rows)?)
                    .ok_or(SealError::ArithmeticOverflow {
                        what: "collector seal memory",
                    })?,
            );
        }
        admit_u64(
            SealResource::Memory,
            peak as u64,
            limits.max_memory_bytes as u64,
        )?;
        Ok(next)
    }
}

#[derive(Debug)]
struct SealPlan {
    types: BTreeMap<u32, TypePlan>,
    total_rows: u64,
    min_ts: i64,
    max_ts: i64,
    source_id: u64,
    admitted_memory_bytes: usize,
}

/// Validate and admit the whole job without retaining section bodies.
fn plan_seal(journal: &Journal, limits: SealLimits) -> Result<SealPlan, SealError> {
    let mut types = BTreeMap::<u32, TypePlan>::new();
    let mut total_rows = 0_u64;
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;
    let mut input_sections = 0_usize;
    let mut dictionary_decoded_bytes = 0_usize;
    let mut dictionary_rows = 0_usize;

    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part(&part)?;
        if catalog.format_version != FORMAT_VERSION {
            return Err(SealError::UnsupportedFormat {
                version: catalog.format_version,
            });
        }
        fold_segment_metadata(&catalog, &mut min_ts, &mut max_ts, &mut source_id)?;
        input_sections = input_sections.checked_add(catalog.entries.len()).ok_or(
            SealError::ArithmeticOverflow {
                what: "input section count",
            },
        )?;
        admit_u64(
            SealResource::InputSections,
            input_sections as u64,
            limits.max_input_sections as u64,
        )?;

        for entry in &catalog.entries {
            let body = section_slice(&part, entry)?;
            if is_dictionary(entry.type_id) {
                let metadata = inspect_dictionary(body, entry.type_id, entry.rows)?;
                dictionary_rows = dictionary_rows.checked_add(metadata.rows).ok_or(
                    SealError::ArithmeticOverflow {
                        what: "dictionary input rows",
                    },
                )?;
                dictionary_decoded_bytes = dictionary_decoded_bytes
                    .checked_add(metadata.decoded_bytes)
                    .ok_or(SealError::ArithmeticOverflow {
                        what: "dictionary decoded bytes",
                    })?;
            } else {
                ensure_registered(entry.type_id)?;
                if entry.len > MAX_SECTION_BYTES as u64 {
                    return Err(CodecError::SectionTooLarge {
                        len: usize::try_from(entry.len).unwrap_or(usize::MAX),
                        max: MAX_SECTION_BYTES,
                    }
                    .into());
                }
                let type_plan = types.entry(entry.type_id).or_default();
                type_plan.rows = type_plan.rows.checked_add(entry.rows as usize).ok_or(
                    SealError::ArithmeticOverflow {
                        what: "rows per type",
                    },
                )?;
                if type_plan.rows > MAX_SECTION_ROWS {
                    return Err(CodecError::TooManyRows {
                        rows: type_plan.rows,
                        max: MAX_SECTION_ROWS,
                    }
                    .into());
                }
                total_rows = total_rows.checked_add(u64::from(entry.rows)).ok_or(
                    SealError::ArithmeticOverflow {
                        what: "segment data rows",
                    },
                )?;
            }
        }
    }

    if min_ts > max_ts {
        min_ts = 0;
        max_ts = 0;
    }
    let dictionary_memory_bytes =
        dictionary_memory_bound(dictionary_decoded_bytes, dictionary_rows, input_sections)?;
    let mut admitted_memory_bytes = dictionary_memory_bytes;
    for (&type_id, type_plan) in &types {
        let type_bytes = compaction_memory_bound(type_id, type_plan.rows)?;
        let peak = dictionary_memory_bytes.checked_add(type_bytes).ok_or(
            SealError::ArithmeticOverflow {
                what: "seal peak memory",
            },
        )?;
        admitted_memory_bytes = admitted_memory_bytes.max(peak);
    }
    admit_u64(
        SealResource::Memory,
        admitted_memory_bytes as u64,
        limits.max_memory_bytes as u64,
    )?;
    Ok(SealPlan {
        types,
        total_rows,
        min_ts,
        max_ts,
        source_id,
        admitted_memory_bytes,
    })
}

fn fold_segment_metadata(
    catalog: &Catalog,
    min_ts: &mut i64,
    max_ts: &mut i64,
    source_id: &mut u64,
) -> Result<(), SealError> {
    let dictionary_only = catalog.min_ts == i64::MAX && catalog.max_ts == i64::MIN;
    if !dictionary_only {
        if catalog.min_ts > catalog.max_ts {
            return Err(SealError::InvalidTimestampRange {
                min_ts: catalog.min_ts,
                max_ts: catalog.max_ts,
            });
        }
        *min_ts = (*min_ts).min(catalog.min_ts);
        *max_ts = (*max_ts).max(catalog.max_ts);
    }
    if catalog.source_id != 0 {
        if *source_id != 0 && *source_id != catalog.source_id {
            return Err(SealError::SourceIdMismatch {
                expected: *source_id,
                got: catalog.source_id,
            });
        }
        *source_id = catalog.source_id;
    }
    Ok(())
}

fn dictionary_memory_bound(
    decoded_bytes: usize,
    rows: usize,
    input_sections: usize,
) -> Result<usize, SealError> {
    let retained = decoded_bytes
        .checked_mul(2)
        .ok_or(SealError::ArithmeticOverflow {
            what: "dictionary retained bytes",
        })?;
    let row_nodes = rows.checked_mul(128).ok_or(SealError::ArithmeticOverflow {
        what: "dictionary map nodes",
    })?;
    let descriptors = input_sections
        .checked_mul(256)
        .ok_or(SealError::ArithmeticOverflow {
            what: "seal descriptors",
        })?;
    retained
        .checked_add(row_nodes)
        .and_then(|value| value.checked_add(descriptors))
        .and_then(|value| value.checked_add(MAX_SECTION_BYTES))
        .ok_or(SealError::ArithmeticOverflow {
            what: "dictionary working memory",
        })
}

#[derive(Debug, Clone, Copy)]
struct DictionaryMetadata {
    rows: usize,
    decoded_bytes: usize,
}

fn inspect_dictionary(
    body: &[u8],
    type_id: u32,
    declared_rows: u32,
) -> Result<DictionaryMetadata, SealError> {
    if body.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: body.len(),
            max: MAX_SECTION_BYTES,
        }
        .into());
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(body))?;
    if builder.metadata().num_row_groups() > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups: builder.metadata().num_row_groups(),
            max: MAX_ROW_GROUPS,
        }
        .into());
    }
    let rows =
        usize::try_from(builder.metadata().file_metadata().num_rows()).map_err(|_overflow| {
            CodecError::InvalidRowCount {
                raw: builder.metadata().file_metadata().num_rows(),
            }
        })?;
    if rows > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows,
            max: MAX_SECTION_ROWS,
        }
        .into());
    }
    if rows as u64 != u64::from(declared_rows) {
        return Err(DictionaryError::RowCount {
            type_id,
            declared: declared_rows,
            actual: rows as u64,
        }
        .into());
    }
    if !dictionary_schema_matches(builder.schema(), type_id) {
        return Err(DictionaryError::Schema { type_id }.into());
    }
    let mut decoded_bytes = 0_usize;
    for group in builder.metadata().row_groups() {
        for column in group.columns() {
            let bytes = usize::try_from(column.uncompressed_size()).map_err(|_overflow| {
                SealError::ArithmeticOverflow {
                    what: "dictionary metadata bytes",
                }
            })?;
            decoded_bytes =
                decoded_bytes
                    .checked_add(bytes)
                    .ok_or(SealError::ArithmeticOverflow {
                        what: "dictionary metadata bytes",
                    })?;
        }
    }
    Ok(DictionaryMetadata {
        rows,
        decoded_bytes,
    })
}

#[derive(Debug)]
struct Run {
    path: PathBuf,
    rows: u32,
    len: u64,
    crc32c: u32,
}

type RunsByType = BTreeMap<u32, Vec<Run>>;

fn create_runs(
    journal: &Journal,
    dest: &Path,
    generation: u64,
    options: SealOptions<'_>,
    artifacts: &mut Artifacts,
) -> Result<(RunsByType, NormalizedDictionary, u64), SealError> {
    let mut runs = RunsByType::new();
    let mut dictionary = NormalizedDictionary::default();
    let mut spill_bytes = 0_u64;
    let mut ordinal = 0_u64;
    for &part_ref in journal.parts() {
        check_cancelled(options.cancelled)?;
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part(&part)?;
        for entry in &catalog.entries {
            check_cancelled(options.cancelled)?;
            let body = section_slice(&part, entry)?;
            if is_dictionary(entry.type_id) {
                dictionary.ingest(body, entry)?;
                continue;
            }
            let verified =
                VerifiedSection::verify(Bytes::copy_from_slice(body), entry.crc32c, crc32c)?;
            let decoded = decode_any(entry.type_id, verified)?;
            if decoded.stats.rows != entry.rows as usize {
                return Err(SealError::RowCountMismatch {
                    type_id: entry.type_id,
                    declared: entry.rows,
                    decoded: decoded.stats.rows,
                });
            }
            let canonical = canonicalize_batches(entry.type_id, &decoded.batches)?;
            let run_body = encode_compact_batch(entry.type_id, &canonical)?;
            let run_len = u64::try_from(run_body.len()).map_err(|_overflow| {
                SealError::ArithmeticOverflow {
                    what: "spill run length",
                }
            })?;
            let needed = spill_bytes
                .checked_add(run_len)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "spill bytes",
                })?;
            admit_u64(
                SealResource::SpillDisk,
                needed,
                options.limits.max_spill_bytes,
            )?;
            let path = artifact_path(dest, generation, "run", ordinal);
            ordinal = ordinal
                .checked_add(1)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "spill ordinal",
                })?;
            write_new_file(&path, &run_body)?;
            artifacts.track(path.clone());
            runs.entry(entry.type_id).or_default().push(Run {
                path,
                rows: entry.rows,
                len: run_len,
                crc32c: crc32c(&run_body),
            });
            spill_bytes = needed;
        }
    }
    dictionary.finish()?;
    Ok((runs, dictionary, spill_bytes))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobValue {
    bytes: Vec<u8>,
    full_len: u64,
    truncated: bool,
    full_sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DictionaryValue {
    String(Vec<u8>),
    Blob(BlobValue),
}

#[derive(Debug, Default)]
struct NormalizedDictionary {
    by_id: BTreeMap<u64, DictionaryValue>,
}

impl NormalizedDictionary {
    fn ingest(&mut self, body: &[u8], entry: &Entry) -> Result<(), SealError> {
        let metadata = inspect_dictionary(body, entry.type_id, entry.rows)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(body))?;
        let mut previous = 0_u64;
        let mut actual_rows = 0_u64;
        for batch in builder.with_batch_size(4096).build()? {
            let batch = batch?;
            actual_rows = actual_rows.checked_add(batch.num_rows() as u64).ok_or(
                SealError::ArithmeticOverflow {
                    what: "dictionary decoded rows",
                },
            )?;
            let ids = required_u64(&batch, "str_id", entry.type_id)?;
            match entry.type_id {
                DICT_STRINGS_TYPE_ID => {
                    let values = required_binary(&batch, "bytes", entry.type_id)?;
                    for row in 0..batch.num_rows() {
                        let id = ids.value(row);
                        check_dictionary_order(entry.type_id, previous, id)?;
                        previous = id;
                        let value = values.value(row).to_vec();
                        if kronika_format::StrId::of(&value).map(kronika_format::StrId::get)
                            != Some(id)
                        {
                            return Err(DictionaryError::IdMismatch { str_id: id }.into());
                        }
                        self.insert_string(id, value)?;
                    }
                }
                DICT_BLOBS_TYPE_ID => {
                    let values = required_binary(&batch, "stored_bytes", entry.type_id)?;
                    let full_lengths = required_u64(&batch, "full_len", entry.type_id)?;
                    let truncated = required_bool(&batch, "truncated", entry.type_id)?;
                    let hashes = fixed_binary(&batch, "full_sha256", entry.type_id)?;
                    for row in 0..batch.num_rows() {
                        let id = ids.value(row);
                        check_dictionary_order(entry.type_id, previous, id)?;
                        previous = id;
                        let full_sha256 = if hashes.is_null(row) {
                            None
                        } else {
                            Some(hashes.value(row).try_into().map_err(|_error| {
                                DictionaryError::Schema {
                                    type_id: entry.type_id,
                                }
                            })?)
                        };
                        let value = BlobValue {
                            bytes: values.value(row).to_vec(),
                            full_len: full_lengths.value(row),
                            truncated: truncated.value(row),
                            full_sha256,
                        };
                        validate_blob(id, &value)?;
                        self.insert_blob(id, value)?;
                    }
                }
                _ => {
                    return Err(DictionaryError::Schema {
                        type_id: entry.type_id,
                    }
                    .into());
                }
            }
        }
        if actual_rows != metadata.rows as u64 {
            return Err(DictionaryError::RowCount {
                type_id: entry.type_id,
                declared: entry.rows,
                actual: actual_rows,
            }
            .into());
        }
        Ok(())
    }

    fn insert_string(&mut self, id: u64, value: Vec<u8>) -> Result<(), DictionaryError> {
        match self.by_id.entry(id) {
            btree_map::Entry::Vacant(slot) => {
                slot.insert(DictionaryValue::String(value));
                Ok(())
            }
            btree_map::Entry::Occupied(slot) => match slot.get() {
                DictionaryValue::String(existing) if existing == &value => Ok(()),
                DictionaryValue::String(_) => Err(DictionaryError::Conflict { str_id: id }),
                DictionaryValue::Blob(blob) if representations_agree(&value, blob) => Ok(()),
                DictionaryValue::Blob(_) => Err(DictionaryError::PlacementConflict { str_id: id }),
            },
        }
    }

    fn insert_blob(&mut self, id: u64, value: BlobValue) -> Result<(), DictionaryError> {
        match self.by_id.entry(id) {
            btree_map::Entry::Vacant(slot) => {
                slot.insert(DictionaryValue::Blob(value));
                Ok(())
            }
            btree_map::Entry::Occupied(mut slot) => match slot.get() {
                DictionaryValue::Blob(existing) if existing == &value => Ok(()),
                DictionaryValue::Blob(_) => Err(DictionaryError::Conflict { str_id: id }),
                DictionaryValue::String(full) if representations_agree(full, &value) => {
                    slot.insert(DictionaryValue::Blob(value));
                    Ok(())
                }
                DictionaryValue::String(_) => {
                    Err(DictionaryError::PlacementConflict { str_id: id })
                }
            },
        }
    }

    fn finish(&self) -> Result<(), DictionaryError> {
        if self.by_id.len() > MAX_SECTION_ROWS {
            return Err(DictionaryError::TooManyEntries {
                entries: self.by_id.len(),
                max: MAX_SECTION_ROWS,
            });
        }
        Ok(())
    }

    fn encode_sections(&self) -> Result<Vec<OutputSection>, SealError> {
        let mut strings = Vec::new();
        let mut blobs = Vec::new();
        for (&id, value) in &self.by_id {
            match value {
                DictionaryValue::String(bytes) => strings.push((id, bytes.as_slice())),
                DictionaryValue::Blob(blob) => blobs.push((id, blob)),
            }
        }
        let mut sections = Vec::new();
        if !strings.is_empty() {
            let ids = UInt64Array::from_iter_values(strings.iter().map(|(id, _bytes)| *id));
            let values = BinaryArray::from_iter_values(strings.iter().map(|(_id, bytes)| *bytes));
            let batch = RecordBatch::try_new(
                string_dictionary_schema(),
                vec![std::sync::Arc::new(ids), std::sync::Arc::new(values)],
            )?;
            let body = encode_compact_ordered_batch(&batch)?;
            sections.push(OutputSection {
                type_id: DICT_STRINGS_TYPE_ID,
                rows: u32::try_from(strings.len()).map_err(|_overflow| {
                    SealError::ArithmeticOverflow {
                        what: "dictionary string rows",
                    }
                })?,
                body,
            });
        }
        if !blobs.is_empty() {
            let ids = UInt64Array::from_iter_values(blobs.iter().map(|(id, _value)| *id));
            let stored = BinaryArray::from_iter_values(
                blobs.iter().map(|(_id, value)| value.bytes.as_slice()),
            );
            let full_len =
                UInt64Array::from_iter_values(blobs.iter().map(|(_id, value)| value.full_len));
            let truncated: BooleanArray = blobs
                .iter()
                .map(|(_id, value)| Some(value.truncated))
                .collect();
            let hashes: Vec<Option<[u8; 32]>> =
                blobs.iter().map(|(_id, value)| value.full_sha256).collect();
            let sha = FixedSizeBinaryArray::try_from_sparse_iter_with_size(
                hashes.iter().map(Option::as_ref),
                32,
            )?;
            let columns: Vec<ArrayRef> = vec![
                std::sync::Arc::new(ids),
                std::sync::Arc::new(stored),
                std::sync::Arc::new(full_len),
                std::sync::Arc::new(truncated),
                std::sync::Arc::new(sha),
            ];
            let batch = RecordBatch::try_new(blob_dictionary_schema(), columns)?;
            let body = encode_compact_ordered_batch(&batch)?;
            sections.push(OutputSection {
                type_id: DICT_BLOBS_TYPE_ID,
                rows: u32::try_from(blobs.len()).map_err(|_overflow| {
                    SealError::ArithmeticOverflow {
                        what: "dictionary blob rows",
                    }
                })?,
                body,
            });
        }
        Ok(sections)
    }
}

fn check_dictionary_order(
    type_id: u32,
    previous: u64,
    current: u64,
) -> Result<(), DictionaryError> {
    if current == 0 || current <= previous {
        Err(DictionaryError::IdOrder {
            type_id,
            previous,
            current,
        })
    } else {
        Ok(())
    }
}

fn validate_blob(id: u64, value: &BlobValue) -> Result<(), DictionaryError> {
    let stored_len = value.bytes.len() as u64;
    let valid = if value.truncated {
        stored_len < value.full_len && value.full_sha256.is_some()
    } else {
        stored_len == value.full_len
            && value.full_sha256.is_none()
            && kronika_format::StrId::of(&value.bytes).map(kronika_format::StrId::get) == Some(id)
    };
    if valid {
        Ok(())
    } else {
        Err(DictionaryError::BlobMetadata { str_id: id })
    }
}

fn representations_agree(full: &[u8], blob: &BlobValue) -> bool {
    if blob.full_len != full.len() as u64 {
        return false;
    }
    if blob.truncated {
        blob.bytes.len() < full.len()
            && full.starts_with(&blob.bytes)
            && blob
                .full_sha256
                .is_some_and(|hash| Sha256::digest(full).as_slice() == hash)
    } else {
        blob.bytes == full && blob.full_sha256.is_none()
    }
}

fn required_u64<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
    type_id: u32,
) -> Result<&'a UInt64Array, DictionaryError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or(DictionaryError::Schema { type_id })?;
    if array.null_count() != 0 {
        return Err(DictionaryError::Schema { type_id });
    }
    Ok(array)
}

fn required_binary<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
    type_id: u32,
) -> Result<&'a BinaryArray, DictionaryError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BinaryArray>())
        .ok_or(DictionaryError::Schema { type_id })?;
    if array.null_count() != 0 {
        return Err(DictionaryError::Schema { type_id });
    }
    Ok(array)
}

fn required_bool<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
    type_id: u32,
) -> Result<&'a BooleanArray, DictionaryError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BooleanArray>())
        .ok_or(DictionaryError::Schema { type_id })?;
    if array.null_count() != 0 {
        return Err(DictionaryError::Schema { type_id });
    }
    Ok(array)
}

fn fixed_binary<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
    type_id: u32,
) -> Result<&'a FixedSizeBinaryArray, DictionaryError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<FixedSizeBinaryArray>())
        .ok_or(DictionaryError::Schema { type_id })
}

fn dictionary_schema_matches(schema: &Schema, type_id: u32) -> bool {
    let fields = schema.fields();
    match type_id {
        DICT_STRINGS_TYPE_ID => {
            fields.len() == 2
                && field_matches(&fields[0], "str_id", &DataType::UInt64, false)
                && field_matches(&fields[1], "bytes", &DataType::Binary, false)
        }
        DICT_BLOBS_TYPE_ID => {
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
        }
        _ => false,
    }
}

fn field_matches(field: &Field, name: &str, data_type: &DataType, nullable: bool) -> bool {
    field.name() == name && field.data_type() == data_type && field.is_nullable() == nullable
}

fn string_dictionary_schema() -> SchemaRef {
    std::sync::Arc::new(Schema::new(vec![
        Field::new("str_id", DataType::UInt64, false),
        Field::new("bytes", DataType::Binary, false),
    ]))
}

fn blob_dictionary_schema() -> SchemaRef {
    std::sync::Arc::new(Schema::new(vec![
        Field::new("str_id", DataType::UInt64, false),
        Field::new("stored_bytes", DataType::Binary, false),
        Field::new("full_len", DataType::UInt64, false),
        Field::new("truncated", DataType::Boolean, false),
        Field::new("full_sha256", DataType::FixedSizeBinary(32), true),
    ]))
}

#[derive(Debug)]
struct OutputSection {
    type_id: u32,
    rows: u32,
    body: Vec<u8>,
}

#[derive(Debug)]
struct BuiltTemp {
    catalog: Catalog,
    bytes: u64,
}

fn write_compact_temp(
    tmp: &Path,
    plan: &SealPlan,
    runs: &RunsByType,
    dictionary: &NormalizedDictionary,
    options: SealOptions<'_>,
    artifacts: &mut Artifacts,
) -> Result<BuiltTemp, SealError> {
    let file = OpenOptions::new().create_new(true).write(true).open(tmp)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(&MAGIC)?;
    let mut offset = MAGIC.len() as u64;
    let mut entries = Vec::with_capacity(plan.types.len() + 2);
    let mut output_bytes = offset;

    for (&type_id, type_plan) in &plan.types {
        check_cancelled(options.cancelled)?;
        let type_runs = runs.get(&type_id).ok_or(SealError::OutputVerification {
            reason: "a planned type has no spill run",
        })?;
        let mut batches = Vec::new();
        let mut run_rows = 0_u64;
        for run in type_runs {
            check_cancelled(options.cancelled)?;
            let body = read_run(run)?;
            let verified = VerifiedSection::verify(body, run.crc32c, crc32c)?;
            let decoded = decode_any(type_id, verified)?;
            if decoded.stats.rows != run.rows as usize {
                return Err(SealError::RowCountMismatch {
                    type_id,
                    declared: run.rows,
                    decoded: decoded.stats.rows,
                });
            }
            run_rows =
                run_rows
                    .checked_add(u64::from(run.rows))
                    .ok_or(SealError::ArithmeticOverflow {
                        what: "coalesced run rows",
                    })?;
            batches.extend(decoded.batches);
        }
        if run_rows != type_plan.rows as u64 {
            return Err(SealError::OutputVerification {
                reason: "planned and spill row counts differ",
            });
        }
        let canonical = canonicalize_batches(type_id, &batches)?;
        let body = encode_compact_batch(type_id, &canonical)?;
        write_output_section(
            &mut writer,
            &mut entries,
            &mut offset,
            &mut output_bytes,
            OutputSection {
                type_id,
                rows: u32::try_from(type_plan.rows).map_err(|_overflow| {
                    SealError::ArithmeticOverflow {
                        what: "final section rows",
                    }
                })?,
                body,
            },
            options.limits.max_output_bytes,
        )?;
        for run in type_runs {
            remove_generated(&run.path)?;
        }
    }

    for section in dictionary.encode_sections()? {
        check_cancelled(options.cancelled)?;
        write_output_section(
            &mut writer,
            &mut entries,
            &mut offset,
            &mut output_bytes,
            section,
            options.limits.max_output_bytes,
        )?;
    }
    let catalog = Catalog {
        entries,
        min_ts: plan.min_ts,
        max_ts: plan.max_ts,
        source_id: plan.source_id,
        format_version: FORMAT_VERSION,
    };
    let encoded_catalog = catalog
        .try_encode()
        .map_err(|_error| SealError::ArithmeticOverflow {
            what: "output catalog",
        })?;
    let final_bytes = output_bytes
        .checked_add(encoded_catalog.len() as u64)
        .ok_or(SealError::ArithmeticOverflow {
            what: "output length",
        })?;
    admit_u64(
        SealResource::OutputDisk,
        final_bytes,
        options.limits.max_output_bytes,
    )?;
    writer.write_all(&encoded_catalog)?;
    let file = writer
        .into_inner()
        .map_err(io::IntoInnerError::into_error)?;
    file.sync_all()?;
    artifacts.track(tmp.to_owned());
    Ok(BuiltTemp {
        catalog,
        bytes: final_bytes,
    })
}

fn write_output_section(
    writer: &mut BufWriter<File>,
    entries: &mut Vec<Entry>,
    offset: &mut u64,
    output_bytes: &mut u64,
    section: OutputSection,
    max_output_bytes: u64,
) -> Result<(), SealError> {
    let len =
        u64::try_from(section.body.len()).map_err(|_overflow| SealError::ArithmeticOverflow {
            what: "output section length",
        })?;
    let needed = output_bytes
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "output section bytes",
        })?;
    admit_u64(SealResource::OutputDisk, needed, max_output_bytes)?;
    if entries
        .last()
        .is_some_and(|entry| entry.type_id >= section.type_id)
    {
        return Err(SealError::OutputVerification {
            reason: "output type order is not strictly increasing",
        });
    }
    writer.write_all(&section.body)?;
    entries.push(Entry {
        type_id: section.type_id,
        flags: 0,
        offset: *offset,
        len,
        rows: section.rows,
        crc32c: crc32c(&section.body),
    });
    *offset = offset
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "output section offset",
        })?;
    *output_bytes = needed;
    Ok(())
}

fn read_run(run: &Run) -> Result<Bytes, SealError> {
    let mut file = File::open(&run.path)?;
    let actual = file.metadata()?.len();
    if actual != run.len {
        return Err(SealError::OutputVerification {
            reason: "spill run length changed",
        });
    }
    let len = usize::try_from(run.len).map_err(|_overflow| SealError::ArithmeticOverflow {
        what: "spill run allocation",
    })?;
    let mut body = vec![0_u8; len];
    file.read_exact(&mut body)?;
    Ok(Bytes::from(body))
}

fn verify_temp(path: &Path, expected: &Catalog, expected_len: u64) -> Result<(), SealError> {
    let file = File::open(path)?;
    let actual_len = file.metadata()?.len();
    if actual_len != expected_len {
        return Err(SealError::OutputVerification {
            reason: "temporary length differs from the built length",
        });
    }
    let tail_at =
        actual_len
            .checked_sub(TAIL_INDEX_LEN as u64)
            .ok_or(SealError::OutputVerification {
                reason: "temporary is shorter than a tail index",
            })?;
    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    file.read_exact_at(&mut tail_bytes, tail_at)?;
    let tail = TailIndex::decode(tail_bytes).map_err(PartError::Tail)?;
    let catalog_len = u64::from(tail.catalog_len);
    let catalog_at = tail_at
        .checked_sub(catalog_len)
        .ok_or(SealError::OutputVerification {
            reason: "temporary catalog length is out of bounds",
        })?;
    let catalog_len =
        usize::try_from(catalog_len).map_err(|_overflow| SealError::ArithmeticOverflow {
            what: "temporary catalog allocation",
        })?;
    let mut catalog_bytes = vec![0_u8; catalog_len];
    file.read_exact_at(&mut catalog_bytes, catalog_at)?;
    let got = Catalog::decode(&catalog_bytes).map_err(PartError::Catalog)?;
    if &got != expected {
        return Err(SealError::OutputVerification {
            reason: "reopened catalog differs from the built catalog",
        });
    }
    let mut magic = [0_u8; 4];
    file.read_exact_at(&mut magic, 0)?;
    if magic != MAGIC {
        return Err(SealError::OutputVerification {
            reason: "reopened leading magic differs",
        });
    }
    let mut scratch = vec![0_u8; 64 * 1024];
    for entry in &got.entries {
        let mut checksum = Crc32c::new();
        let mut consumed = 0_u64;
        while consumed < entry.len {
            let remaining = entry.len - consumed;
            let take =
                usize::try_from(remaining.min(scratch.len() as u64)).map_err(|_overflow| {
                    SealError::ArithmeticOverflow {
                        what: "verification chunk length",
                    }
                })?;
            let at = entry
                .offset
                .checked_add(consumed)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "verification section offset",
                })?;
            file.read_exact_at(&mut scratch[..take], at)?;
            checksum.update(&scratch[..take]);
            consumed = consumed
                .checked_add(take as u64)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "verification consumed bytes",
                })?;
        }
        if checksum.finalize() != entry.crc32c {
            return Err(SealError::OutputVerification {
                reason: "reopened section CRC differs",
            });
        }
    }
    Ok(())
}

fn publish(tmp: &Path, dest: &Path) -> Result<Publication, SealError> {
    match fs::hard_link(tmp, dest) {
        Ok(()) => {
            sync_parent_dir(dest)?;
            remove_generated(tmp)?;
            sync_parent_dir(dest)?;
            Ok(Publication::Created)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !files_equal(tmp, dest)? {
                return Err(SealError::PublicationConflict {
                    path: dest.to_owned(),
                });
            }
            remove_generated(tmp)?;
            sync_parent_dir(dest)?;
            Ok(Publication::AlreadyPresent)
        }
        Err(error) => Err(SealError::Io(error)),
    }
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, io::Error> {
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
        if left_len != right_len || left_buf[..left_len] != right_buf[..right_len] {
            return Ok(false);
        }
        if left_len == 0 {
            return Ok(true);
        }
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn remove_generated(path: &Path) -> Result<(), io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn section_slice<'a>(part: &'a [u8], entry: &Entry) -> Result<&'a [u8], SealError> {
    let start =
        usize::try_from(entry.offset).map_err(|_overflow| SealError::ArithmeticOverflow {
            what: "section start",
        })?;
    let len = usize::try_from(entry.len).map_err(|_overflow| SealError::ArithmeticOverflow {
        what: "section length",
    })?;
    let end = start
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "section end",
        })?;
    part.get(start..end)
        .ok_or(SealError::Part(PartError::SectionOutOfBounds {
            type_id: entry.type_id,
        }))
}

fn ensure_registered(type_id: u32) -> Result<(), SealError> {
    if registry()
        .iter()
        .any(|contract| contract.type_id.get() == type_id)
    {
        Ok(())
    } else {
        Err(SealError::UnknownType { type_id })
    }
}

const fn is_dictionary(type_id: u32) -> bool {
    matches!(type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID)
}

fn admit_u64(resource: SealResource, needed: u64, limit: u64) -> Result<(), SealError> {
    if needed > limit {
        Err(SealError::Resource {
            resource,
            needed,
            limit,
        })
    } else {
        Ok(())
    }
}

fn check_cancelled(cancelled: Option<&AtomicBool>) -> Result<(), SealError> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err(SealError::Cancelled)
    } else {
        Ok(())
    }
}

fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn artifact_path(dest: &Path, generation: u64, kind: &str, ordinal: u64) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(format!(
        ".{}.{}.{}.{}.tmp",
        std::process::id(),
        generation,
        kind,
        ordinal
    ));
    PathBuf::from(name)
}

/// Deletes only paths created and registered by this seal attempt.
#[derive(Debug, Default)]
struct Artifacts {
    paths: Vec<PathBuf>,
}

impl Artifacts {
    fn track(&mut self, path: PathBuf) {
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ignored = remove_generated(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::DictLimits;
    use kronika_registry::Ts;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use parquet::basic::Encoding;

    use super::*;
    use crate::{Interner, JournalConfig, SectionBuffers, dict};

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

    fn append_window(journal: &mut Journal, ts: i64) {
        let mut buffers = SectionBuffers::new();
        buffers.push(bgwriter(ts)).expect("buffer not full");
        let part = buffers.flush(&[], 0).expect("encode").expect("a part");
        journal.append(&part).expect("append");
    }

    #[test]
    fn seal_coalesces_repeated_types_into_one_compact_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let segment_path = dir.path().join("143000.pgm");
        let (mut journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("open journal");
        append_window(&mut journal, 2_000);
        append_window(&mut journal, 1_000);

        let summary = seal(&journal, &segment_path).expect("seal");
        assert_eq!(summary.sections, 1);
        assert_eq!(summary.rows, 2);
        assert_eq!((summary.min_ts, summary.max_ts), (1_000, 2_000));
        assert_eq!(summary.publication, Publication::Created);
        assert!(summary.spill_bytes > 0);
        assert_eq!(summary.write_bytes, summary.spill_bytes + summary.bytes);

        let segment = fs::read(&segment_path).expect("read segment");
        let catalog = validate_part(&segment).expect("segment validates");
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].type_id, 1_006_001);
        assert_eq!(catalog.entries[0].rows, 2);
        let body = section_slice(&segment, &catalog.entries[0]).expect("body");
        let metadata =
            ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(body)).expect("body");
        assert_eq!(metadata.metadata().num_row_groups(), 1);
        for column in metadata.metadata().row_group(0).columns() {
            assert_eq!(column.column_index_length(), None);
            assert_eq!(column.offset_index_length(), None);
            assert!(column.encodings().contains(&Encoding::PLAIN));
        }
    }

    #[test]
    fn deterministic_bytes_do_not_depend_on_part_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let make = |path: &Path, timestamps: &[i64]| {
            let (mut journal, _) = Journal::open(path, JournalConfig::default()).expect("journal");
            for &ts in timestamps {
                append_window(&mut journal, ts);
            }
            journal
        };
        let a = make(&dir.path().join("a.parts"), &[3, 1, 2]);
        let b = make(&dir.path().join("b.parts"), &[2, 3, 1]);
        let a_path = dir.path().join("a.pgm");
        let b_path = dir.path().join("b.pgm");
        seal(&a, &a_path).expect("seal a");
        seal(&b, &b_path).expect("seal b");
        assert_eq!(fs::read(a_path).unwrap(), fs::read(b_path).unwrap());
    }

    #[test]
    fn duplicate_dictionary_windows_normalize_to_one_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let segment_path = dir.path().join("dictionary.pgm");
        let (mut journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("journal");
        for ts in [1_000, 2_000] {
            let mut interner = Interner::new(DictLimits::new(4096, 1 << 20).expect("limits"));
            interner.intern(b"db-host-01").expect("intern");
            let dictionary = dict::encode(interner.window()).expect("dictionary");
            let mut buffers = SectionBuffers::new();
            buffers.push(bgwriter(ts)).expect("row");
            let part = buffers.flush(&dictionary, 0).expect("flush").expect("part");
            journal.append(&part).expect("append");
        }
        seal(&journal, &segment_path).expect("seal");
        let bytes = fs::read(segment_path).expect("read");
        let catalog = validate_part(&bytes).expect("valid");
        assert_eq!(catalog.entries.len(), 2);
        let dictionary = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == DICT_STRINGS_TYPE_ID)
            .expect("dictionary");
        assert_eq!(dictionary.rows, 1);
    }

    #[test]
    fn exact_existing_destination_is_idempotent_but_conflict_is_not_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("journal");
        append_window(&mut journal, 1);
        let path = dir.path().join("segment.pgm");
        let first = seal(&journal, &path).expect("first");
        assert_eq!(first.publication, Publication::Created);
        let second = seal(&journal, &path).expect("idempotent retry");
        assert_eq!(second.publication, Publication::AlreadyPresent);

        let conflict = dir.path().join("conflict.pgm");
        fs::write(&conflict, b"different").expect("conflict fixture");
        let before = fs::read(&conflict).unwrap();
        assert!(matches!(
            seal(&journal, &conflict),
            Err(SealError::PublicationConflict { .. })
        ));
        assert_eq!(fs::read(conflict).unwrap(), before);
    }

    #[test]
    fn admission_and_cancellation_leave_journal_and_destination_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("journal");
        append_window(&mut journal, 1);
        let journal_before = fs::read(&journal_path).expect("journal bytes");
        let destination = dir.path().join("segment.pgm");

        let tiny = SealOptions {
            limits: SealLimits {
                max_memory_bytes: 1,
                ..SealLimits::default()
            },
            cancelled: None,
        };
        assert!(matches!(
            seal_with_options(&journal, &destination, tiny),
            Err(SealError::Resource {
                resource: SealResource::Memory,
                ..
            })
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read(&journal_path).unwrap(), journal_before);

        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            seal_with_options(
                &journal,
                &destination,
                SealOptions {
                    cancelled: Some(&cancelled),
                    ..SealOptions::default()
                }
            ),
            Err(SealError::Cancelled)
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read(journal_path).unwrap(), journal_before);
    }

    #[test]
    fn an_empty_journal_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("journal");
        assert!(matches!(
            seal(&journal, &dir.path().join("segment.pgm")),
            Err(SealError::Empty)
        ));
    }
}
