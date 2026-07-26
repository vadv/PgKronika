//! Bounded completion of journal parts into one compact PGM.
//!
//! The writer validates and admits the journal, creates sorted per-type runs,
//! merges them with fixed fan-in, and streams the final bodies into one
//! sibling temporary. Publication never replaces an existing destination.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use arrow_array::{
    Array, BinaryArray, BooleanArray, FixedSizeBinaryArray, RecordBatch, UInt64Array,
};
use kronika_format::{
    Catalog, Entry, EntrySnapshot, FORMAT_VERSION, HotMark, MAGIC, PartError, Placement, StrId,
    crc32c, validate_part,
};
use kronika_registry::{
    Bytes, COMPACTION_MEMORY_LIMIT, COMPACTION_PAGE_BYTES, CodecError, ColumnType,
    DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, MAX_ROW_GROUPS, MAX_SECTION_BYTES, MAX_SECTION_ROWS,
    READ_WORK_MEMORY_LIMIT, VerifiedSection, canonicalize_batches, compact_parquet_profile_matches,
    compact_section_bound, compact_unregistered_bound, compaction_memory_bound, decode_any,
    dictionary_schema_matches, encode_compact_ordered_batch, read_work_memory,
    read_work_memory_bound, registry,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::dict::DictSection;
use crate::io_error::parent_directory;
use crate::{DEFAULT_MAX_JOURNAL_LEN, FilesystemError, FilesystemOperation, Journal, JournalError};

/// Maximum catalog entries accepted from one seal input by default.
pub const DEFAULT_MAX_INPUT_SECTIONS: usize = 65_536;
const MERGE_FAN_IN: usize = 32;

/// Hard resources admitted before seal work begins.
#[allow(
    clippy::struct_field_names,
    reason = "the public fields state explicitly that each value is a hard maximum"
)]
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

/// How the immutable destination became visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// This attempt published it.
    Created,
    /// The exact deterministic bytes were already present.
    AlreadyPresent,
}

/// Completed segment and physical-work accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealSummary {
    /// Final catalog entries.
    pub sections: usize,
    /// Logical data rows; dictionary rows are excluded.
    pub rows: u64,
    /// Final PGM bytes.
    pub bytes: u64,
    /// Source journal bytes.
    pub source_bytes: u64,
    /// Aggregate external-run bytes written.
    pub spill_bytes: u64,
    /// `spill_bytes + bytes`.
    pub write_bytes: u64,
    /// Largest admitted working set.
    pub admitted_memory_bytes: usize,
    /// Minimum data timestamp.
    pub min_ts: i64,
    /// Maximum data timestamp.
    pub max_ts: i64,
    /// Publication result.
    pub publication: Publication,
}

/// Resource rejected by a hard seal limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealResource {
    /// Decoded/sort memory.
    Memory,
    /// External-run disk.
    SpillDisk,
    /// Final temporary disk.
    OutputDisk,
    /// Input catalog entries.
    InputSections,
    /// Distinct normalized dictionary entries.
    DictionaryEntries,
    /// Projected PLAIN column page.
    ColumnPage,
    /// Projected PLAIN section body.
    SectionBody,
    /// Projected reader work.
    ReaderMemory,
}

impl fmt::Display for SealResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Memory => "memory",
            Self::SpillDisk => "spill disk",
            Self::OutputDisk => "output disk",
            Self::InputSections => "input sections",
            Self::DictionaryEntries => "dictionary entries",
            Self::ColumnPage => "column page",
            Self::SectionBody => "section body",
            Self::ReaderMemory => "reader memory",
        })
    }
}

/// Why a seal failed.
#[derive(Debug)]
pub enum SealError {
    /// Filesystem operation failed.
    Io(FilesystemError),
    /// Journal read failed.
    Journal(JournalError),
    /// Part framing or CRC failed.
    Part(PartError),
    /// Arrow/Parquet section failed.
    Codec(CodecError),
    /// Journal has no parts.
    Empty,
    /// Configured hard resource was crossed.
    Resource {
        /// Resource.
        resource: SealResource,
        /// Required amount.
        needed: u64,
        /// Ceiling.
        limit: u64,
    },
    /// Non-zero source ids disagree.
    SourceIdMismatch {
        /// First id.
        expected: u64,
        /// Conflicting id.
        got: u64,
    },
    /// Timestamp endpoints are reversed.
    InvalidTimestampRange {
        /// Minimum.
        min_ts: i64,
        /// Maximum.
        max_ts: i64,
    },
    /// Catalog and decoded rows disagree.
    RowCountMismatch {
        /// Section id.
        type_id: u32,
        /// Catalog rows.
        declared: u32,
        /// Decoded rows.
        decoded: usize,
    },
    /// Checked arithmetic overflowed.
    ArithmeticOverflow {
        /// Quantity.
        what: &'static str,
    },
    /// A different destination already exists.
    PublicationConflict {
        /// Destination path.
        path: PathBuf,
    },
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "segment {error}"),
            Self::Journal(error) => write!(f, "journal: {error}"),
            Self::Part(error) => write!(f, "part: {error}"),
            Self::Codec(error) => write!(f, "section: {error}"),
            Self::Empty => f.write_str("the journal holds no parts to seal"),
            Self::Resource {
                resource,
                needed,
                limit,
            } => write!(
                f,
                "seal needs {needed} of {resource}, above the limit {limit}"
            ),
            Self::SourceIdMismatch { expected, got } => {
                write!(f, "journal mixes source_id {expected} and {got}")
            }
            Self::InvalidTimestampRange { min_ts, max_ts } => {
                write!(f, "invalid timestamp range {min_ts}..{max_ts}")
            }
            Self::RowCountMismatch {
                type_id,
                declared,
                decoded,
            } => write!(
                f,
                "type {type_id} declares {declared} rows but decodes {decoded}"
            ),
            Self::ArithmeticOverflow { what } => write!(f, "{what} overflow"),
            Self::PublicationConflict { path } => {
                write!(f, "different destination exists at {}", path.display())
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
            _ => None,
        }
    }
}

impl SealError {
    /// Whether sealing the current segment can make a candidate part admissible.
    #[must_use]
    pub const fn is_admission_boundary(&self) -> bool {
        matches!(
            self,
            Self::Resource {
                resource: SealResource::Memory
                    | SealResource::InputSections
                    | SealResource::DictionaryEntries
                    | SealResource::ColumnPage
                    | SealResource::SectionBody
                    | SealResource::ReaderMemory,
                ..
            } | Self::Codec(CodecError::TooManyRows { .. })
        )
    }
}

impl From<FilesystemError> for SealError {
    fn from(error: FilesystemError) -> Self {
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

#[derive(Debug, Clone, Default)]
struct TypePlan {
    rows: usize,
    list_values: usize,
}

#[derive(Debug, Clone, Default)]
struct DictionaryPlan {
    rows: usize,
    column_uncompressed_bytes: Vec<usize>,
}

/// Incremental collector-side admission for the future compact seal.
#[derive(Debug, Clone, Default)]
pub struct SealAdmission {
    types: BTreeMap<u32, TypePlan>,
    dictionaries: BTreeMap<u32, DictionaryPlan>,
    dictionary_rows: usize,
    dictionary_bytes: usize,
    input_sections: usize,
}

impl SealAdmission {
    /// Return the state after adding one part without mutating `self`.
    ///
    /// # Errors
    ///
    /// Returns [`SealError`] for invalid input or a crossed hard limit.
    pub fn with_part(&self, part: &[u8], limits: SealLimits) -> Result<Self, SealError> {
        let mut next = self.clone();
        let catalog = validate_part(part)?;
        next.admit_part(part, &catalog, limits)?;
        Ok(next)
    }

    fn admit_part(
        &mut self,
        part: &[u8],
        catalog: &Catalog,
        limits: SealLimits,
    ) -> Result<(), SealError> {
        if catalog.format_version != FORMAT_VERSION {
            return Err(CodecError::SchemaMismatch.into());
        }
        self.input_sections = add(
            self.input_sections,
            catalog.entries.len(),
            "input section count",
        )?;
        admit(
            SealResource::InputSections,
            self.input_sections as u64,
            limits.max_input_sections as u64,
        )?;

        for entry in &catalog.entries {
            let body = section(part, entry)?;
            if is_dictionary(entry.type_id) {
                let inspected = inspect_dictionary(body, entry)?;
                self.dictionary_rows =
                    add(self.dictionary_rows, inspected.rows, "dictionary rows")?;
                self.dictionary_bytes = add(
                    self.dictionary_bytes,
                    inspected.decoded_bytes,
                    "dictionary bytes",
                )?;
                let plan = self.dictionaries.entry(entry.type_id).or_default();
                plan.rows = add(plan.rows, inspected.rows, "dictionary rows per type")?;
                if plan.column_uncompressed_bytes.is_empty() {
                    plan.column_uncompressed_bytes
                        .resize(inspected.column_uncompressed_bytes.len(), 0);
                } else if plan.column_uncompressed_bytes.len()
                    != inspected.column_uncompressed_bytes.len()
                {
                    return Err(CodecError::SchemaMismatch.into());
                }
                for (total, bytes) in plan
                    .column_uncompressed_bytes
                    .iter_mut()
                    .zip(inspected.column_uncompressed_bytes)
                {
                    *total = add(*total, bytes, "dictionary column bytes")?;
                }
                admit_dictionary(plan)?;
                continue;
            }
            let inspected = inspect_data(body, entry)?;
            if inspected.rows == 0 {
                continue;
            }
            let plan = self.types.entry(entry.type_id).or_default();
            plan.rows = add(plan.rows, inspected.rows, "rows per type")?;
            plan.list_values = add(
                plan.list_values,
                inspected.list_values,
                "list values per type",
            )?;
            admit_type(entry.type_id, plan)?;
        }
        admit(
            SealResource::Memory,
            self.peak_memory()? as u64,
            limits.max_memory_bytes as u64,
        )
    }

    fn peak_memory(&self) -> Result<usize, SealError> {
        let dictionary = self
            .dictionary_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(self.dictionary_rows.saturating_mul(128)))
            .and_then(|bytes| bytes.checked_add(self.input_sections.saturating_mul(64)))
            .and_then(|bytes| bytes.checked_add(MAX_SECTION_BYTES))
            .ok_or(SealError::ArithmeticOverflow {
                what: "dictionary memory",
            })?;
        self.types
            .iter()
            .try_fold(dictionary, |peak, (&type_id, plan)| {
                Ok(peak.max(add(
                    dictionary,
                    compaction_memory_bound(type_id, plan.rows)?,
                    "seal memory",
                )?))
            })
    }
}

#[derive(Debug)]
struct SealPlan {
    admission: SealAdmission,
    rows: u64,
    min_ts: i64,
    max_ts: i64,
    source_id: u64,
}

/// Seal with default production limits.
///
/// # Errors
///
/// Returns [`SealError`] without resetting the journal or replacing `dest`.
pub fn seal(journal: &Journal, dest: &Path) -> Result<SealSummary, SealError> {
    seal_with_limits(journal, dest, SealLimits::default())
}

/// Seal with explicit hard limits.
///
/// # Errors
///
/// Returns [`SealError`] without resetting the journal or replacing `dest`.
pub fn seal_with_limits(
    journal: &Journal,
    dest: &Path,
    limits: SealLimits,
) -> Result<SealSummary, SealError> {
    if journal.parts().is_empty() {
        return Err(SealError::Empty);
    }
    let plan = plan(journal, limits)?;
    if plan.rows == 0 {
        return Err(SealError::Empty);
    }

    let generation = generation();
    let mut artifacts = Artifacts::default();
    let (runs, dictionary, mut spill_bytes, mut ordinal) =
        create_runs(journal, dest, generation, limits, &mut artifacts)?;
    let runs = merge_runs(
        runs,
        dest,
        generation,
        &mut ordinal,
        &mut spill_bytes,
        limits,
        &mut artifacts,
    )?;
    let temporary = artifact_path(dest, generation, "segment", 0);
    artifacts.track(temporary.clone());
    let (bytes, sections) = write_segment(&temporary, runs, &dictionary, &plan, limits)?;
    let publication = publish(&temporary, dest)?;
    let source_bytes =
        u64::try_from(journal.bytes()).map_err(|_error| SealError::ArithmeticOverflow {
            what: "journal bytes",
        })?;
    Ok(SealSummary {
        sections,
        rows: plan.rows,
        bytes,
        source_bytes,
        spill_bytes,
        write_bytes: spill_bytes
            .checked_add(bytes)
            .ok_or(SealError::ArithmeticOverflow {
                what: "write bytes",
            })?,
        admitted_memory_bytes: plan.admission.peak_memory()?,
        min_ts: plan.min_ts,
        max_ts: plan.max_ts,
        publication,
    })
}

fn plan(journal: &Journal, limits: SealLimits) -> Result<SealPlan, SealError> {
    let mut admission = SealAdmission::default();
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;
    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part(&part)?;
        admission.admit_part(&part, &catalog, limits)?;
        if catalog.min_ts != i64::MAX || catalog.max_ts != i64::MIN {
            if catalog.min_ts > catalog.max_ts {
                return Err(SealError::InvalidTimestampRange {
                    min_ts: catalog.min_ts,
                    max_ts: catalog.max_ts,
                });
            }
            min_ts = min_ts.min(catalog.min_ts);
            max_ts = max_ts.max(catalog.max_ts);
        }
        if catalog.source_id != 0 {
            if source_id != 0 && source_id != catalog.source_id {
                return Err(SealError::SourceIdMismatch {
                    expected: source_id,
                    got: catalog.source_id,
                });
            }
            source_id = catalog.source_id;
        }
    }
    if min_ts > max_ts {
        min_ts = 0;
        max_ts = 0;
    }
    let rows = admission.types.values().try_fold(0_u64, |total, plan| {
        total
            .checked_add(plan.rows as u64)
            .ok_or(SealError::ArithmeticOverflow {
                what: "segment rows",
            })
    })?;
    Ok(SealPlan {
        admission,
        rows,
        min_ts,
        max_ts,
        source_id,
    })
}

#[derive(Debug, Clone, Copy)]
struct DataInspection {
    rows: usize,
    list_values: usize,
}

fn inspect_data(body: &[u8], entry: &Entry) -> Result<DataInspection, SealError> {
    let contract = registry()
        .iter()
        .find(|contract| contract.type_id.get() == entry.type_id)
        .ok_or(CodecError::UnknownType {
            type_id: entry.type_id,
        })?;
    let builder = section_builder(body, entry)?;
    let rows = entry.rows as usize;
    let mut list_values = 0_usize;
    for column in contract
        .columns
        .iter()
        .filter(|column| column.ty == ColumnType::ListI32)
    {
        for row_group in builder.metadata().row_groups() {
            for chunk in row_group.columns().iter().filter(|chunk| {
                chunk
                    .column_path()
                    .parts()
                    .first()
                    .is_some_and(|name| name == column.name)
            }) {
                let raw = chunk.num_values();
                list_values = add(
                    list_values,
                    usize::try_from(raw)
                        .map_err(|_error| CodecError::InvalidDecodedSize { raw })?,
                    "list values",
                )?;
            }
        }
    }
    Ok(DataInspection { rows, list_values })
}

#[derive(Debug)]
struct DictionaryInspection {
    rows: usize,
    decoded_bytes: usize,
    column_uncompressed_bytes: Vec<usize>,
}

fn inspect_dictionary(body: &[u8], entry: &Entry) -> Result<DictionaryInspection, SealError> {
    let builder = section_builder(body, entry)?;
    if !dictionary_schema_matches(builder.schema(), entry.type_id) {
        return Err(CodecError::SchemaMismatch.into());
    }
    let rows = entry.rows as usize;
    let columns = builder
        .metadata()
        .file_metadata()
        .schema_descr()
        .num_columns();
    let mut column_uncompressed_bytes = vec![0_usize; columns];
    for row_group in builder.metadata().row_groups() {
        if row_group.columns().len() != columns {
            return Err(CodecError::SchemaMismatch.into());
        }
        for (index, column) in row_group.columns().iter().enumerate() {
            let raw = column.uncompressed_size();
            let bytes =
                usize::try_from(raw).map_err(|_error| CodecError::InvalidDecodedSize { raw })?;
            column_uncompressed_bytes[index] = add(
                column_uncompressed_bytes[index],
                bytes,
                "dictionary decoded bytes",
            )?;
        }
    }
    let decoded_bytes = column_uncompressed_bytes
        .iter()
        .try_fold(0_usize, |total, &bytes| {
            add(total, bytes, "dictionary decoded bytes")
        })?;
    Ok(DictionaryInspection {
        rows,
        decoded_bytes,
        column_uncompressed_bytes,
    })
}

fn section_builder(
    body: &[u8],
    entry: &Entry,
) -> Result<ParquetRecordBatchReaderBuilder<Bytes>, SealError> {
    if body.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: body.len(),
            max: MAX_SECTION_BYTES,
        }
        .into());
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(body))?;
    let groups = builder.metadata().num_row_groups();
    if groups > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups,
            max: MAX_ROW_GROUPS,
        }
        .into());
    }
    let raw_rows = builder.metadata().file_metadata().num_rows();
    let rows = usize::try_from(raw_rows)
        .map_err(|_error| CodecError::InvalidRowCount { raw: raw_rows })?;
    if rows != entry.rows as usize {
        return Err(SealError::RowCountMismatch {
            type_id: entry.type_id,
            declared: entry.rows,
            decoded: rows,
        });
    }
    if rows > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows,
            max: MAX_SECTION_ROWS,
        }
        .into());
    }
    if is_dictionary(entry.type_id) && !compact_parquet_profile_matches(builder.metadata()) {
        return Err(CodecError::SchemaMismatch.into());
    }
    Ok(builder)
}

fn admit_type(type_id: u32, plan: &TypePlan) -> Result<(), SealError> {
    let bound = compact_section_bound(type_id, plan.rows, plan.list_values)?;
    admit(
        SealResource::ColumnPage,
        bound.max_column_page_bytes as u64,
        COMPACTION_PAGE_BYTES as u64,
    )?;
    admit(
        SealResource::SectionBody,
        bound.plain_body_bytes as u64,
        MAX_SECTION_BYTES as u64,
    )?;
    admit(
        SealResource::ReaderMemory,
        read_work_memory_bound(
            type_id,
            plan.rows,
            bound.plain_body_bytes,
            bound.plain_body_bytes,
        )? as u64,
        READ_WORK_MEMORY_LIMIT as u64,
    )
}

fn admit_dictionary(plan: &DictionaryPlan) -> Result<(), SealError> {
    let bound = compact_unregistered_bound(plan.rows, &plan.column_uncompressed_bytes)?;
    admit(
        SealResource::ColumnPage,
        bound.max_column_page_bytes as u64,
        COMPACTION_PAGE_BYTES as u64,
    )?;
    admit(
        SealResource::SectionBody,
        bound.plain_body_bytes as u64,
        MAX_SECTION_BYTES as u64,
    )?;
    admit(
        SealResource::ReaderMemory,
        read_work_memory(bound.plain_body_bytes, bound.decoded_arrow_bytes)? as u64,
        READ_WORK_MEMORY_LIMIT as u64,
    )
}

#[derive(Debug)]
struct Run {
    path: PathBuf,
    rows: u32,
    crc: u32,
}

type Runs = BTreeMap<u32, Vec<Run>>;

fn create_runs(
    journal: &Journal,
    dest: &Path,
    generation: u64,
    limits: SealLimits,
    artifacts: &mut Artifacts,
) -> Result<(Runs, NormalizedDictionary, u64, u64), SealError> {
    let mut runs = Runs::new();
    let mut dictionary = NormalizedDictionary::default();
    let mut spill_bytes = 0_u64;
    let mut ordinal = 0_u64;
    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part(&part)?;
        for entry in &catalog.entries {
            let body = section(&part, entry)?;
            if is_dictionary(entry.type_id) {
                dictionary.ingest(body, entry)?;
                continue;
            }
            if entry.rows == 0 {
                continue;
            }
            let decoded = decode_any(
                entry.type_id,
                VerifiedSection::verify(Bytes::copy_from_slice(body), entry.crc32c, crc32c)?,
            )?;
            if decoded.stats.rows != entry.rows as usize {
                return Err(SealError::RowCountMismatch {
                    type_id: entry.type_id,
                    declared: entry.rows,
                    decoded: decoded.stats.rows,
                });
            }
            let batch = canonicalize_batches(entry.type_id, &decoded.batches)?;
            let encoded = encode_compact_ordered_batch(&batch)?;
            let run = write_run(
                dest,
                generation,
                &mut ordinal,
                entry.rows,
                &encoded,
                &mut spill_bytes,
                limits.max_spill_bytes,
                artifacts,
            )?;
            runs.entry(entry.type_id).or_default().push(run);
        }
    }
    dictionary.finish()?;
    Ok((runs, dictionary, spill_bytes, ordinal))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the merge owns one attempt identity and its two mutable counters"
)]
fn merge_runs(
    runs: Runs,
    dest: &Path,
    generation: u64,
    ordinal: &mut u64,
    spill_bytes: &mut u64,
    limits: SealLimits,
    artifacts: &mut Artifacts,
) -> Result<Runs, SealError> {
    let mut merged = Runs::new();
    for (type_id, mut current) in runs {
        while current.len() > 1 {
            let mut next = Vec::with_capacity(current.len().div_ceil(MERGE_FAN_IN));
            for group in current.chunks(MERGE_FAN_IN) {
                let mut batches = Vec::with_capacity(group.len());
                let mut rows = 0_u32;
                for run in group {
                    let body = read_run(run)?;
                    let decoded =
                        decode_any(type_id, VerifiedSection::verify(body, run.crc, crc32c)?)?;
                    rows = rows
                        .checked_add(run.rows)
                        .ok_or(SealError::ArithmeticOverflow { what: "run rows" })?;
                    batches.extend(decoded.batches);
                }
                let batch = canonicalize_batches(type_id, &batches)?;
                if batch.num_rows() != rows as usize {
                    return Err(SealError::RowCountMismatch {
                        type_id,
                        declared: rows,
                        decoded: batch.num_rows(),
                    });
                }
                let encoded = encode_compact_ordered_batch(&batch)?;
                next.push(write_run(
                    dest,
                    generation,
                    ordinal,
                    rows,
                    &encoded,
                    spill_bytes,
                    limits.max_spill_bytes,
                    artifacts,
                )?);
                for run in group {
                    remove(&run.path)?;
                }
            }
            current = next;
        }
        merged.insert(type_id, current);
    }
    Ok(merged)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one helper owns run naming, accounting, limit, and cleanup registration"
)]
fn write_run(
    dest: &Path,
    generation: u64,
    ordinal: &mut u64,
    rows: u32,
    body: &[u8],
    spill_bytes: &mut u64,
    max_spill_bytes: u64,
    artifacts: &mut Artifacts,
) -> Result<Run, SealError> {
    let len = u64::try_from(body.len())
        .map_err(|_error| SealError::ArithmeticOverflow { what: "run length" })?;
    let next = spill_bytes
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "spill bytes",
        })?;
    admit(SealResource::SpillDisk, next, max_spill_bytes)?;
    let path = artifact_path(dest, generation, "run", *ordinal);
    *ordinal = ordinal
        .checked_add(1)
        .ok_or(SealError::ArithmeticOverflow {
            what: "run ordinal",
        })?;
    artifacts.track(path.clone());
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| io_error(FilesystemOperation::CreateNew, &path, error))?;
    file.write_all(body)
        .map_err(|error| io_error(FilesystemOperation::Write, &path, error))?;
    *spill_bytes = next;
    Ok(Run {
        path,
        rows,
        crc: crc32c(body),
    })
}

fn read_run(run: &Run) -> Result<Bytes, SealError> {
    let file = File::open(&run.path)
        .map_err(|error| io_error(FilesystemOperation::Open, &run.path, error))?;
    let mut body = Vec::new();
    file.take(MAX_SECTION_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| io_error(FilesystemOperation::Read, &run.path, error))?;
    if body.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: body.len(),
            max: MAX_SECTION_BYTES,
        }
        .into());
    }
    Ok(Bytes::from(body))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DictionaryValue {
    String(Vec<u8>),
    Blob {
        bytes: Vec<u8>,
        full_len: u64,
        full_sha256: Option<[u8; 32]>,
    },
}

#[derive(Debug, Default)]
struct NormalizedDictionary {
    values: BTreeMap<StrId, DictionaryValue>,
}

impl NormalizedDictionary {
    fn ingest(&mut self, body: &[u8], entry: &Entry) -> Result<(), SealError> {
        let builder = section_builder(body, entry)?;
        if !dictionary_schema_matches(builder.schema(), entry.type_id) {
            return Err(CodecError::SchemaMismatch.into());
        }
        let mut rows = 0_usize;
        let mut previous = 0_u64;
        for batch in builder.with_batch_size(4_096).build()? {
            let batch = batch?;
            let ids = required_u64(&batch, "str_id")?;
            rows = add(rows, batch.num_rows(), "dictionary rows")?;
            if entry.type_id == DICT_STRINGS_TYPE_ID {
                let bytes = required_binary(&batch, "bytes")?;
                for row in 0..batch.num_rows() {
                    let id = ordered_id(ids.value(row), &mut previous)?;
                    let value = bytes.value(row);
                    if StrId::of(value) != Some(id) {
                        return Err(CodecError::SchemaMismatch.into());
                    }
                    self.insert(id, DictionaryValue::String(value.to_vec()))?;
                }
            } else {
                let bytes = required_binary(&batch, "stored_bytes")?;
                let full_len = required_u64(&batch, "full_len")?;
                let truncated = required_bool(&batch, "truncated")?;
                let sha = fixed_binary(&batch, "full_sha256")?;
                for row in 0..batch.num_rows() {
                    let id = ordered_id(ids.value(row), &mut previous)?;
                    let stored = bytes.value(row);
                    let full_len = full_len.value(row);
                    let truncated = truncated.value(row);
                    let full_sha256 = if sha.is_null(row) {
                        None
                    } else {
                        Some(
                            sha.value(row)
                                .try_into()
                                .map_err(|_error| CodecError::SchemaMismatch)?,
                        )
                    };
                    let valid = if truncated {
                        (stored.len() as u64) < full_len && full_sha256.is_some()
                    } else {
                        stored.len() as u64 == full_len
                            && full_sha256.is_none()
                            && StrId::of(stored) == Some(id)
                    };
                    if !valid {
                        return Err(CodecError::SchemaMismatch.into());
                    }
                    self.insert(
                        id,
                        DictionaryValue::Blob {
                            bytes: stored.to_vec(),
                            full_len,
                            full_sha256,
                        },
                    )?;
                }
            }
        }
        if rows != entry.rows as usize {
            return Err(SealError::RowCountMismatch {
                type_id: entry.type_id,
                declared: entry.rows,
                decoded: rows,
            });
        }
        Ok(())
    }

    fn insert(&mut self, id: StrId, value: DictionaryValue) -> Result<(), SealError> {
        match self.values.get(&id) {
            None => {
                self.values.insert(id, value);
                Ok(())
            }
            Some(existing) if existing == &value => Ok(()),
            Some(existing)
                if matches!(
                    (existing, &value),
                    (DictionaryValue::String(_), DictionaryValue::Blob { .. })
                        | (DictionaryValue::Blob { .. }, DictionaryValue::String(_))
                ) =>
            {
                Err(CodecError::SchemaMismatch.into())
            }
            Some(_) => Err(CodecError::SchemaMismatch.into()),
        }
    }

    fn finish(&self) -> Result<(), SealError> {
        admit(
            SealResource::DictionaryEntries,
            self.values.len() as u64,
            MAX_SECTION_ROWS as u64,
        )
    }

    fn sections(&self) -> Result<Vec<DictSection>, SealError> {
        Ok(crate::dict::encode_entries(self.values.iter().map(
            |(&str_id, value)| {
                let (stored_bytes, full_len, full_sha256, placement) = match value {
                    DictionaryValue::String(bytes) => (
                        bytes.as_slice(),
                        bytes.len() as u64,
                        None,
                        Placement::Strings,
                    ),
                    DictionaryValue::Blob {
                        bytes,
                        full_len,
                        full_sha256,
                        ..
                    } => (bytes.as_slice(), *full_len, *full_sha256, Placement::Blobs),
                };
                EntrySnapshot {
                    str_id,
                    stored_bytes,
                    full_len,
                    truncated: full_sha256.is_some(),
                    full_sha256,
                    placement,
                    hot: HotMark::None,
                    blob_required: false,
                }
            },
        ))?)
    }
}

fn ordered_id(id: u64, previous: &mut u64) -> Result<StrId, SealError> {
    let id = StrId::from_raw(id)
        .filter(|id| id.get() > *previous)
        .ok_or(SealError::Codec(CodecError::SchemaMismatch))?;
    *previous = id.get();
    Ok(id)
}

fn write_segment(
    path: &Path,
    runs: Runs,
    dictionary: &NormalizedDictionary,
    plan: &SealPlan,
    limits: SealLimits,
) -> Result<(u64, usize), SealError> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error(FilesystemOperation::CreateNew, path, error))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(&MAGIC)
        .map_err(|error| io_error(FilesystemOperation::Write, path, error))?;
    let mut bytes = MAGIC.len() as u64;
    let mut entries = Vec::with_capacity(runs.len() + 2);
    for (type_id, type_runs) in runs {
        let [run] = type_runs.as_slice() else {
            return Err(SealError::Codec(CodecError::SchemaMismatch));
        };
        let body = read_run(run)?.to_vec();
        write_section(
            path,
            &mut writer,
            &mut entries,
            &mut bytes,
            &DictSection {
                type_id,
                rows: run.rows,
                body,
            },
            limits.max_output_bytes,
        )?;
        remove(&run.path)?;
    }
    for section in dictionary.sections()? {
        write_section(
            path,
            &mut writer,
            &mut entries,
            &mut bytes,
            &section,
            limits.max_output_bytes,
        )?;
    }
    let catalog = Catalog {
        entries,
        min_ts: plan.min_ts,
        max_ts: plan.max_ts,
        source_id: plan.source_id,
        format_version: FORMAT_VERSION,
    };
    let encoded = catalog
        .try_encode()
        .map_err(|_error| SealError::ArithmeticOverflow {
            what: "catalog length",
        })?;
    let final_bytes =
        bytes
            .checked_add(encoded.len() as u64)
            .ok_or(SealError::ArithmeticOverflow {
                what: "output bytes",
            })?;
    admit(
        SealResource::OutputDisk,
        final_bytes,
        limits.max_output_bytes,
    )?;
    writer
        .write_all(&encoded)
        .map_err(|error| io_error(FilesystemOperation::Write, path, error))?;
    let file = writer
        .into_inner()
        .map_err(io::IntoInnerError::into_error)
        .map_err(|error| io_error(FilesystemOperation::Flush, path, error))?;
    file.sync_all()
        .map_err(|error| io_error(FilesystemOperation::SyncFile, path, error))?;
    Ok((final_bytes, catalog.entries.len()))
}

fn write_section(
    path: &Path,
    writer: &mut BufWriter<File>,
    entries: &mut Vec<Entry>,
    bytes: &mut u64,
    section: &DictSection,
    max_output_bytes: u64,
) -> Result<(), SealError> {
    if entries
        .last()
        .is_some_and(|entry| entry.type_id >= section.type_id)
    {
        return Err(SealError::Codec(CodecError::SchemaMismatch));
    }
    let len =
        u64::try_from(section.body.len()).map_err(|_error| SealError::ArithmeticOverflow {
            what: "section length",
        })?;
    let next = bytes
        .checked_add(len)
        .ok_or(SealError::ArithmeticOverflow {
            what: "output section",
        })?;
    admit(SealResource::OutputDisk, next, max_output_bytes)?;
    writer
        .write_all(&section.body)
        .map_err(|error| io_error(FilesystemOperation::Write, path, error))?;
    entries.push(Entry {
        type_id: section.type_id,
        flags: 0,
        offset: *bytes,
        len,
        rows: section.rows,
        crc32c: crc32c(&section.body),
    });
    *bytes = next;
    Ok(())
}

fn publish(temporary: &Path, dest: &Path) -> Result<Publication, SealError> {
    match fs::hard_link(temporary, dest) {
        Ok(()) => {
            sync_parent(dest)?;
            remove(temporary)?;
            sync_parent(dest)?;
            Ok(Publication::Created)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !files_equal(temporary, dest)? {
                return Err(SealError::PublicationConflict {
                    path: dest.to_owned(),
                });
            }
            remove(temporary)?;
            sync_parent(dest)?;
            Ok(Publication::AlreadyPresent)
        }
        Err(error) => Err(io_error(FilesystemOperation::PublishNoReplace, dest, error)),
    }
}

fn files_equal(left_path: &Path, right_path: &Path) -> Result<bool, SealError> {
    let mut left = File::open(left_path)
        .map_err(|error| io_error(FilesystemOperation::Open, left_path, error))?;
    let mut right = File::open(right_path)
        .map_err(|error| io_error(FilesystemOperation::Open, right_path, error))?;
    if left
        .metadata()
        .map_err(|error| io_error(FilesystemOperation::Metadata, left_path, error))?
        .len()
        != right
            .metadata()
            .map_err(|error| io_error(FilesystemOperation::Metadata, right_path, error))?
            .len()
    {
        return Ok(false);
    }
    let mut left_buffer = vec![0_u8; 64 * 1024];
    let mut right_buffer = vec![0_u8; 64 * 1024];
    loop {
        let left_len = left
            .read(&mut left_buffer)
            .map_err(|error| io_error(FilesystemOperation::Read, left_path, error))?;
        let right_len = right
            .read(&mut right_buffer)
            .map_err(|error| io_error(FilesystemOperation::Read, right_path, error))?;
        if left_len != right_len {
            return Ok(false);
        }
        if left_buffer[..left_len] != right_buffer[..left_len] {
            return Ok(false);
        }
        if left_len == 0 {
            return Ok(true);
        }
    }
}

fn sync_parent(path: &Path) -> Result<(), SealError> {
    let parent = parent_directory(path);
    File::open(parent)
        .map_err(|error| io_error(FilesystemOperation::Open, parent, error))?
        .sync_all()
        .map_err(|error| io_error(FilesystemOperation::SyncDirectory, parent, error))
}

fn remove(path: &Path) -> Result<(), SealError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(FilesystemOperation::Remove, path, error)),
    }
}

fn section<'a>(part: &'a [u8], entry: &Entry) -> Result<&'a [u8], SealError> {
    let start = usize::try_from(entry.offset).map_err(|_error| SealError::ArithmeticOverflow {
        what: "section offset",
    })?;
    let len = usize::try_from(entry.len).map_err(|_error| SealError::ArithmeticOverflow {
        what: "section length",
    })?;
    part.get(
        start
            ..start
                .checked_add(len)
                .ok_or(SealError::ArithmeticOverflow {
                    what: "section end",
                })?,
    )
    .ok_or(SealError::Part(PartError::SectionOutOfBounds {
        type_id: entry.type_id,
    }))
}

fn required_u64<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a UInt64Array, CodecError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or(CodecError::ColumnType { name })?;
    required(array, name).map(|()| array)
}

fn required_binary<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BinaryArray, CodecError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BinaryArray>())
        .ok_or(CodecError::ColumnType { name })?;
    required(array, name).map(|()| array)
}

fn required_bool<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BooleanArray, CodecError> {
    let array = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BooleanArray>())
        .ok_or(CodecError::ColumnType { name })?;
    required(array, name).map(|()| array)
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

fn required(array: &dyn Array, name: &'static str) -> Result<(), CodecError> {
    if array.null_count() == 0 {
        Ok(())
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

const fn is_dictionary(type_id: u32) -> bool {
    matches!(type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID)
}

const fn admit(resource: SealResource, needed: u64, limit: u64) -> Result<(), SealError> {
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

fn add(left: usize, right: usize, what: &'static str) -> Result<usize, SealError> {
    left.checked_add(right)
        .ok_or(SealError::ArithmeticOverflow { what })
}

fn generation() -> u64 {
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

fn io_error(operation: FilesystemOperation, path: &Path, error: io::Error) -> SealError {
    FilesystemError {
        operation,
        path: path.to_owned(),
        source: error,
    }
    .into()
}

#[derive(Debug, Default)]
struct Artifacts {
    paths: Vec<PathBuf>,
}

impl Artifacts {
    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        for path in &self.paths {
            let _result = remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_schema::{DataType, Field, Schema};
    use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
    use kronika_registry::Ts;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;

    use super::*;
    use crate::{JournalConfig, SectionBuffers};

    fn append(journal: &mut Journal, ts: i64) {
        let mut buffers = SectionBuffers::new();
        buffers
            .push(BgwriterCheckpointer {
                ts: Ts(ts),
                checkpoints_timed: ts,
                checkpoints_req: 0,
                checkpoint_write_time: 0.0,
                checkpoint_sync_time: 0.0,
                buffers_checkpoint: 0,
                restartpoints_timed: None,
                restartpoints_req: None,
                restartpoints_done: None,
                buffers_clean: 0,
                maxwritten_clean: 0,
                buffers_backend: None,
                buffers_backend_fsync: None,
                buffers_alloc: 0,
                bgwriter_stats_reset: Ts(1),
                checkpointer_stats_reset: None,
            })
            .expect("buffer row");
        let part = buffers
            .flush(&[], 7)
            .expect("flush")
            .expect("nonempty part");
        journal.append(&part).expect("append");
    }

    #[test]
    fn one_writer_path_compacts_repeated_types_and_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("segment.pgm");
        let (mut journal, report) = Journal::open(
            &directory.path().join("active.parts"),
            JournalConfig::default(),
        )
        .expect("journal");
        assert!(report.is_clean(), "fresh journal");
        append(&mut journal, 2);
        append(&mut journal, 1);

        let first = seal(&journal, &path).expect("seal");
        let second = seal(&journal, &path).expect("idempotent seal");
        assert_eq!(first.publication, Publication::Created);
        assert_eq!(second.publication, Publication::AlreadyPresent);
        assert_eq!(first.bytes, second.bytes);
        assert!(first.spill_bytes > 0, "real runs are accounted");

        let bytes = fs::read(path).expect("read segment");
        let catalog = validate_part(&bytes).expect("valid PGM");
        assert_eq!(catalog.entries.len(), 1, "one body per type");
        assert_eq!(catalog.entries[0].rows, 2);
        let body = section(&bytes, &catalog.entries[0]).expect("body");
        let decoded = decode_any(
            catalog.entries[0].type_id,
            VerifiedSection::verify(
                Bytes::copy_from_slice(body),
                catalog.entries[0].crc32c,
                crc32c,
            )
            .expect("verified"),
        )
        .expect("decode");
        assert_eq!(decoded.stats.rows, 2);
    }

    #[test]
    fn conflict_and_limit_leave_the_journal() {
        let directory = tempfile::tempdir().expect("tempdir");
        let destination = directory.path().join("segment.pgm");
        let (mut journal, _) = Journal::open(
            &directory.path().join("active.parts"),
            JournalConfig::default(),
        )
        .expect("journal");
        append(&mut journal, 1);
        fs::write(&destination, b"occupied").expect("occupy destination");
        assert!(matches!(
            seal(&journal, &destination),
            Err(SealError::PublicationConflict { .. })
        ));
        assert_eq!(journal.parts().len(), 1, "seal never resets the journal");

        let small = SealLimits {
            max_memory_bytes: 1,
            ..SealLimits::default()
        };
        assert!(matches!(
            seal_with_limits(&journal, &directory.path().join("small.pgm"), small),
            Err(SealError::Resource {
                resource: SealResource::Memory,
                ..
            })
        ));
    }

    #[test]
    fn unknown_type_stays_an_ordinary_integrity_error() {
        let body = Vec::new();
        let part = build_part(
            &[SectionInput {
                type_id: 4_000_000,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 1,
            },
        );
        let directory = tempfile::tempdir().expect("tempdir");
        let (mut journal, _) = Journal::open(
            &directory.path().join("active.parts"),
            JournalConfig::default(),
        )
        .expect("journal");
        journal
            .append(&part)
            .expect("append structurally valid part");
        assert!(matches!(
            seal(&journal, &directory.path().join("segment.pgm")),
            Err(SealError::Codec(CodecError::UnknownType {
                type_id: 4_000_000
            }))
        ));
    }

    #[test]
    fn dictionary_page_growth_is_rejected_during_admission() {
        let mut interner =
            crate::Interner::new(DictLimits::new(2_048, 2_048).expect("dictionary limits"));
        for ordinal in 0_u64..1_024 {
            let mut value = vec![b'x'; 1_024];
            value[1_016..].copy_from_slice(&ordinal.to_le_bytes());
            interner.intern(&value).expect("intern unique value");
        }
        let sections = crate::dict::encode(interner.window()).expect("encode dictionary");
        let inputs = sections
            .iter()
            .map(|section| SectionInput {
                type_id: section.type_id,
                rows: section.rows,
                body: &section.body,
            })
            .collect::<Vec<_>>();
        let part = build_part(
            &inputs,
            PartMeta {
                min_ts: i64::MAX,
                max_ts: i64::MIN,
                source_id: 0,
            },
        );
        assert!(matches!(
            SealAdmission::default().with_part(&part, SealLimits::default()),
            Err(SealError::Resource {
                resource: SealResource::ColumnPage,
                ..
            })
        ));
    }

    #[test]
    fn parquet_dictionary_encoded_input_is_rejected_during_admission() {
        let repeated = vec![b'x'; 8 * 1024];
        let ids = UInt64Array::from_iter_values(1_u64..=512);
        let values = BinaryArray::from_iter_values((0..512).map(|_ordinal| repeated.as_slice()));
        let schema = Arc::new(Schema::new(vec![
            Field::new("str_id", DataType::UInt64, false),
            Field::new("bytes", DataType::Binary, false),
        ]));
        let batch =
            RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(ids), Arc::new(values)])
                .expect("dictionary batch");
        let mut body = Vec::new();
        let properties = WriterProperties::builder()
            .set_dictionary_enabled(true)
            .build();
        let mut writer =
            ArrowWriter::try_new(&mut body, schema, Some(properties)).expect("Parquet writer");
        writer.write(&batch).expect("write dictionary batch");
        writer.close().expect("close dictionary body");
        let part = build_part(
            &[SectionInput {
                type_id: DICT_STRINGS_TYPE_ID,
                rows: 512,
                body: &body,
            }],
            PartMeta {
                min_ts: i64::MAX,
                max_ts: i64::MIN,
                source_id: 0,
            },
        );

        assert!(matches!(
            SealAdmission::default().with_part(&part, SealLimits::default()),
            Err(SealError::Codec(CodecError::SchemaMismatch))
        ));
    }
}
