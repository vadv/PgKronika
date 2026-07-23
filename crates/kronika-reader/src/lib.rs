//! Verified local PGM reads and bounded logical queries.
//!
//! [`PgmUnit`] decodes a PGM container over any [`kronika_format::ReadAt`]
//! implementation: a sealed [`std::fs::File`] or an in-memory journal part
//! `&[u8]`.
//!
//! [`LocalDirSnapshot`] combines sealed segments with live `active.parts`
//! entries, suppresses exact sealed/live duplicates, and exposes scan
//! diagnostics.
//!
//! Section queries join registered layout versions under one logical name,
//! filter by source and time, order rows by the registry contract, and return
//! coverage gaps plus an opaque cursor. [`QueryLimits`] bounds returned rows
//! and materialized cells; format, catalog, section, row-group, and row-count
//! ceilings are checked before or during decode.
//!
//! Counter folds preserve reset, gap, first-point, invalid-interval, and
//! collection-disabled states as distinct [`Reason`] values. A zero rate is
//! emitted only for a measured unchanged counter. Callers should treat
//! [`ReadError::StaleSnapshot`] as a refresh-and-retry signal; bounded query
//! helpers perform a limited refresh retry and then expose the remaining gap.

mod overview;
mod query;
mod refresh;
mod snapshot;
mod unit;

// criterion and mimalloc are used only by the `serving` bench; anchored for the
// `unused_crate_dependencies` lint, which checks each target separately.
#[cfg(test)]
use criterion as _;
#[cfg(test)]
use mimalloc as _;

pub use kronika_analytics::{DiffPoint, Reason, Scalar};
pub use kronika_format::DamageRegion;
pub use overview::{
    BlockCodec, BlockContent, BlockDirectoryEntry, BlockError, BlockFlags, BlockKind, Bounds,
    BuildError, CacheReadError, CacheRebuildReason, CatalogEntryDescriptor, CounterSamplesBlock,
    DictionaryContextEntry, EntityStateRecord, EntityStatesBlock, EventObservationsBlock, FactFile,
    FactFileHeader, FactFileReader, FactKey, FactLoad, FactOrigin, FactReadStats, FactStore,
    FileKind, FoldEffect, GaugeSamplesBlock, HeaderIdentity, LIMIT, LiveBuilder, LiveFoldError,
    LiveState, LiveView, LossCoverageBlock, ManifestEntryDescriptor, PersistError, ResetMarker,
    ResetMarkersBlock, ResolvedPattern, SealOutcome, SegmentContext, SegmentContextError,
    SegmentFacts, SourceDescriptor, SourceError, SourceManifestBlock, StringTableBlock,
    TargetedDictionaryRead, TargetedDictionaryStats, dictionary_context_id, lineage_from_catalog,
    placement, placement_dir, reconcile_seal, resolve_targeted, section_body_id, source_scope_id,
};
pub use query::{
    ColumnDiff, ColumnValues, Cursor, DiffAt, Gap, GateReading, LogicalColumn, LogicalSection,
    OutRow, QueryError, QueryLimits, SectionPage, SeriesDiff, SeriesValues, Value,
    apply_collection_gating, apply_gating, diff_section, gate_readings, gauge_section,
    logical_section, section, section_with_limits, sections, sections_with_limits, select_gate,
};
pub use refresh::{
    ByteRange, CatalogDigest, JournalDelta, JournalGenerationId, JournalIdentity, PartDescriptor,
    PartId, PartTransition, RefreshDelta, SegmentDescriptor, catalog_digest, classify_transition,
    part_id,
};
pub use snapshot::{LocalDirSnapshot, OpenUnit, SealedFactError, UnitMeta};

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::path::Path;

use arrow_array::{
    Array, BinaryArray, BooleanArray, FixedSizeBinaryArray, RecordBatch, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use kronika_format::{Catalog, DecodeError, Entry};
use kronika_registry::{
    Bytes, CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, DecodedSection, MAX_ROW_GROUPS,
    MAX_SECTION_ROWS,
};
use kronika_store::StoreError;

pub use kronika_registry::{Cell, Row};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

pub use unit::{OverviewSectionBody, PgmBodyReadStats, PgmUnit};

/// A sealed segment opened for reading.
#[derive(Debug)]
pub struct Segment {
    inner: PgmUnit<File>,
}

/// Why a segment could not be opened or a section decoded.
#[derive(Debug)]
pub enum ReadError {
    /// A filesystem read failed.
    Io(std::io::Error),
    /// The file is shorter than a tail index.
    TooSmall {
        /// The file length found.
        len: u64,
    },
    /// The file does not start with the segment magic.
    BadMagic {
        /// The bytes found at the start of the file.
        actual: [u8; 4],
    },
    /// The catalog declares a container format this build does not read.
    UnsupportedFormat {
        /// The `format_version` found.
        version: u32,
    },
    /// A catalog entry points outside the segment's section area.
    SectionOutOfBounds {
        /// The entry's `type_id`.
        type_id: u32,
    },
    /// `decode` was called on a dictionary section; use
    /// [`dictionary`](Segment::dictionary) instead.
    DictionarySection {
        /// The dictionary section's `type_id`.
        type_id: u32,
    },
    /// The tail index did not decode.
    Tail(DecodeError),
    /// `catalog_len` does not fit between the magic and the tail index, or
    /// exceeds the catalog cap.
    BadCatalogLen {
        /// `catalog_len` from the tail index.
        catalog_len: u32,
    },
    /// The catalog block did not decode (length, count, or CRC).
    Catalog(DecodeError),
    /// A catalog entry's length is above
    /// [`kronika_registry::MAX_SECTION_BYTES`].
    SectionTooLarge {
        /// The section length claimed by the catalog.
        len: u64,
    },
    /// A section failed CRC verification or decoding; a malformed dictionary
    /// section (bad Parquet, missing columns) arrives here too.
    Codec(CodecError),
    /// The store rejected a local snapshot item before segment decode.
    Store(StoreError),
    /// The active part's catalog changed between snapshot time and decode time.
    ///
    /// Callers must call `refresh()` and retry.
    StaleSnapshot {
        /// Index of the unit that triggered the staleness check.
        unit_idx: usize,
    },
    /// A diagnostic byte, row, or operation counter overflowed.
    CounterOverflow,
    /// A requested segment-global catalog ordinal does not exist.
    CatalogOrdinalOutOfRange {
        /// Requested ordinal.
        ordinal: u32,
    },
    /// Decoded section rows disagree with their PGM catalog entry.
    CatalogRowCountMismatch {
        /// Section type.
        type_id: u32,
        /// Catalog-declared row count.
        declared: u32,
        /// Rows produced by the section decoder.
        decoded: u64,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "segment io: {err}"),
            Self::TooSmall { len } => write!(f, "file of {len} bytes is too small for a segment"),
            Self::BadMagic { actual } => {
                write!(f, "segment magic is {actual:02x?}, expected \"PGM1\"")
            }
            Self::UnsupportedFormat { version } => {
                write!(f, "segment format_version {version} is not supported")
            }
            Self::SectionOutOfBounds { type_id } => {
                write!(f, "section {type_id} points outside the segment")
            }
            Self::DictionarySection { type_id } => {
                write!(f, "section {type_id} is a dictionary; use dictionary()")
            }
            Self::Tail(err) => write!(f, "segment tail index: {err}"),
            Self::BadCatalogLen { catalog_len } => {
                write!(f, "segment catalog_len {catalog_len} does not fit the file")
            }
            Self::Catalog(err) => write!(f, "segment catalog: {err}"),
            Self::SectionTooLarge { len } => write!(f, "section of {len} bytes is above the cap"),
            Self::Codec(err) => write!(f, "section decode: {err}"),
            Self::Store(err) => write!(f, "store read: {err}"),
            Self::StaleSnapshot { unit_idx } => {
                write!(
                    f,
                    "unit {unit_idx}: active part changed since snapshot; call refresh()"
                )
            }
            Self::CounterOverflow => write!(f, "segment diagnostic counter overflow"),
            Self::CatalogOrdinalOutOfRange { ordinal } => {
                write!(f, "catalog ordinal {ordinal} is out of range")
            }
            Self::CatalogRowCountMismatch {
                type_id,
                declared,
                decoded,
            } => write!(
                f,
                "section type {type_id} decoded {decoded} rows, but the catalog declares {declared}"
            ),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Tail(err) | Self::Catalog(err) => Some(err),
            Self::Codec(err) => Some(err),
            Self::Store(err) => Some(err),
            Self::TooSmall { .. }
            | Self::BadMagic { .. }
            | Self::UnsupportedFormat { .. }
            | Self::SectionOutOfBounds { .. }
            | Self::DictionarySection { .. }
            | Self::BadCatalogLen { .. }
            | Self::SectionTooLarge { .. }
            | Self::StaleSnapshot { .. }
            | Self::CounterOverflow
            | Self::CatalogOrdinalOutOfRange { .. }
            | Self::CatalogRowCountMismatch { .. } => None,
        }
    }
}

impl From<std::io::Error> for ReadError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl Segment {
    /// Open a sealed segment and read its end catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] on I/O errors or invalid segment framing.
    pub fn open(path: &Path) -> Result<Self, ReadError> {
        let file = File::open(path)?;
        Ok(Self {
            inner: PgmUnit::open(file)?,
        })
    }

    /// The segment's end catalog.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        self.inner.catalog()
    }

    /// Read and decode one section by its catalog `entry`.
    ///
    /// `entry` must come from this segment's [`catalog`](Segment::catalog).
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the section is a dictionary, out of bounds,
    /// fails CRC, or fails typed decode.
    pub fn decode(&self, entry: &Entry) -> Result<DecodedSection, ReadError> {
        self.inner.decode(entry)
    }

    /// Read the segment's dictionary sections into a `str_id` -> bytes map.
    ///
    /// Loads the segment dictionary into memory.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when a dictionary section cannot be read or
    /// decoded.
    pub fn dictionary(&self) -> Result<Dictionary, ReadError> {
        self.inner.dictionary()
    }
}

/// A value a `str_id` resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved<'a> {
    /// A `dict.strings` value, stored in full.
    String(&'a [u8]),
    /// A `dict.blobs` value; `bytes` is a prefix of the original when `truncated`.
    Blob {
        /// The stored bytes — a prefix of the original when `truncated`.
        bytes: &'a [u8],
        /// Length of the full original value, bytes.
        full_len: u64,
        /// Whether `bytes` is only a prefix of the original.
        truncated: bool,
    },
}

/// One stored dictionary value, with a blob's truncation metadata.
#[derive(Debug, Clone)]
pub(crate) enum Stored {
    String(Vec<u8>),
    Blob {
        bytes: Vec<u8>,
        full_len: u64,
        truncated: bool,
    },
}

impl Stored {
    fn resolved(&self) -> Resolved<'_> {
        match self {
            Self::String(bytes) => Resolved::String(bytes),
            Self::Blob {
                bytes,
                full_len,
                truncated,
            } => Resolved::Blob {
                bytes,
                full_len: *full_len,
                truncated: *truncated,
            },
        }
    }
}

/// A segment's `str_id` -> value map, built from its dictionary sections.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    pub(crate) by_id: HashMap<u64, Stored>,
}

impl Dictionary {
    /// The value a `str_id` resolves to, if the segment carries it.
    #[must_use]
    pub fn resolve(&self, str_id: u64) -> Option<Resolved<'_>> {
        self.by_id.get(&str_id).map(Stored::resolved)
    }

    /// Number of distinct ids resolved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the segment carries no dictionary entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Decode a dictionary section body into `(str_id, value)` pairs.
///
/// Applies row-group and row-count caps before reading dictionary columns.
pub(crate) fn decode_dictionary(
    body: Bytes,
    type_id: u32,
) -> Result<Vec<(u64, Stored)>, CodecError> {
    decode_dictionary_selected(body, type_id, None, &LIMIT)
        .map(|(entries, _rows, _decoded_bytes)| entries)
}

type DictionarySelection = (Vec<(u64, Stored)>, u64, u64);

/// Decodes a dictionary section while copying only selected values.
#[allow(
    clippy::too_many_lines,
    reason = "one pass enforces cross-batch ordering and cumulative decode bounds"
)]
pub(crate) fn decode_dictionary_selected(
    body: Bytes,
    type_id: u32,
    wanted: Option<&BTreeSet<u64>>,
    bounds: &Bounds,
) -> Result<DictionarySelection, CodecError> {
    let is_blob = match type_id {
        DICT_STRINGS_TYPE_ID => false,
        DICT_BLOBS_TYPE_ID => true,
        _ => return Err(CodecError::UnknownType { type_id }),
    };
    let builder = ParquetRecordBatchReaderBuilder::try_new(body)?;
    let groups = builder.metadata().num_row_groups();
    if groups > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups,
            max: MAX_ROW_GROUPS,
        });
    }
    let claimed = builder.metadata().file_metadata().num_rows();
    let claimed_rows = match usize::try_from(claimed) {
        Ok(rows) if rows <= MAX_SECTION_ROWS => rows,
        Ok(rows) => {
            return Err(CodecError::TooManyRows {
                rows,
                max: MAX_SECTION_ROWS,
            });
        }
        Err(_) => return Err(CodecError::InvalidRowCount { raw: claimed }),
    };
    let mut declared_decoded_bytes = 0_u64;
    for row_group in builder.metadata().row_groups() {
        for column in row_group.columns() {
            let bytes = u64::try_from(column.uncompressed_size())
                .map_err(|_error| CodecError::SchemaMismatch)?;
            declared_decoded_bytes =
                declared_decoded_bytes
                    .checked_add(bytes)
                    .ok_or(CodecError::SectionTooLarge {
                        len: usize::MAX,
                        max: usize::try_from(bounds.decoded_block_len).unwrap_or(usize::MAX),
                    })?;
        }
    }
    if declared_decoded_bytes > bounds.decoded_block_len {
        return Err(CodecError::SectionTooLarge {
            len: usize::try_from(declared_decoded_bytes).unwrap_or(usize::MAX),
            max: usize::try_from(bounds.decoded_block_len).unwrap_or(usize::MAX),
        });
    }
    if !dictionary_schema_matches(builder.schema(), is_blob) {
        return Err(CodecError::SchemaMismatch);
    }

    let value_column = if is_blob { "stored_bytes" } else { "bytes" };
    let mut out = Vec::new();
    let mut rows_scanned = 0_u64;
    let mut decoded_bytes = 0_u64;
    let mut previous_id = None;
    for batch in builder.with_batch_size(4_096).build()? {
        let batch = batch?;
        rows_scanned =
            rows_scanned
                .checked_add(batch.num_rows() as u64)
                .ok_or(CodecError::TooManyRows {
                    rows: usize::MAX,
                    max: MAX_SECTION_ROWS,
                })?;
        let ids = u64_column(&batch, "str_id")?;
        let values = binary_column(&batch, value_column)?;
        if is_blob {
            let full_len = u64_column(&batch, "full_len")?;
            let truncated = bool_column(&batch, "truncated")?;
            let full_sha256 = fixed_binary_column(&batch, "full_sha256")?;
            for row in 0..batch.num_rows() {
                let str_id = ids.value(row);
                if str_id == 0 || previous_id.is_some_and(|previous| str_id <= previous) {
                    return Err(CodecError::SchemaMismatch);
                }
                previous_id = Some(str_id);
                let bytes = values.value(row);
                decoded_bytes = decoded_bytes.checked_add(bytes.len() as u64).ok_or(
                    CodecError::SectionTooLarge {
                        len: usize::MAX,
                        max: usize::try_from(bounds.decoded_block_len).unwrap_or(usize::MAX),
                    },
                )?;
                let full_len = full_len.value(row);
                let truncated = truncated.value(row);
                let has_full_hash = !full_sha256.is_null(row);
                let valid_storage = if truncated {
                    (bytes.len() as u64) < full_len && has_full_hash
                } else {
                    bytes.len() as u64 == full_len
                        && !has_full_hash
                        && kronika_format::StrId::of(bytes).map(kronika_format::StrId::get)
                            == Some(str_id)
                };
                if !valid_storage {
                    return Err(CodecError::SchemaMismatch);
                }
                if wanted.is_some_and(|wanted| !wanted.contains(&ids.value(row))) {
                    continue;
                }
                out.push((
                    str_id,
                    Stored::Blob {
                        bytes: bytes.to_vec(),
                        full_len,
                        truncated,
                    },
                ));
            }
        } else {
            for row in 0..batch.num_rows() {
                let str_id = ids.value(row);
                if str_id == 0 || previous_id.is_some_and(|previous| str_id <= previous) {
                    return Err(CodecError::SchemaMismatch);
                }
                previous_id = Some(str_id);
                let bytes = values.value(row);
                decoded_bytes = decoded_bytes.checked_add(bytes.len() as u64).ok_or(
                    CodecError::SectionTooLarge {
                        len: usize::MAX,
                        max: usize::try_from(bounds.decoded_block_len).unwrap_or(usize::MAX),
                    },
                )?;
                if kronika_format::StrId::of(bytes).map(kronika_format::StrId::get) != Some(str_id)
                {
                    return Err(CodecError::SchemaMismatch);
                }
                if wanted.is_some_and(|wanted| !wanted.contains(&ids.value(row))) {
                    continue;
                }
                out.push((str_id, Stored::String(bytes.to_vec())));
            }
        }
        if decoded_bytes > bounds.decoded_block_len {
            return Err(CodecError::SectionTooLarge {
                len: usize::try_from(decoded_bytes).unwrap_or(usize::MAX),
                max: usize::try_from(bounds.decoded_block_len).unwrap_or(usize::MAX),
            });
        }
    }
    if rows_scanned != claimed_rows as u64 {
        return Err(CodecError::SchemaMismatch);
    }
    Ok((out, rows_scanned, decoded_bytes))
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

fn u64_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a UInt64Array, CodecError> {
    let column = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or(CodecError::ColumnType { name })?;
    reject_nulls(column, name).map(|()| column)
}

fn binary_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BinaryArray, CodecError> {
    let column = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BinaryArray>())
        .ok_or(CodecError::ColumnType { name })?;
    reject_nulls(column, name).map(|()| column)
}

fn bool_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a BooleanArray, CodecError> {
    let column = batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<BooleanArray>())
        .ok_or(CodecError::ColumnType { name })?;
    reject_nulls(column, name).map(|()| column)
}

fn fixed_binary_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<&'a FixedSizeBinaryArray, CodecError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<FixedSizeBinaryArray>())
        .ok_or(CodecError::ColumnType { name })
}

/// A dictionary column carries no `NULL`s.
fn reject_nulls(array: &dyn Array, name: &'static str) -> Result<(), CodecError> {
    if array.null_count() == 0 {
        Ok(())
    } else {
        Err(CodecError::NullInRequiredColumn { name })
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use super::{ReadError, Resolved, Segment};

    /// Write a one-section segment to a temp file. A chartless segment is
    /// structurally a PGM part, so `build_part` writes a valid one.
    fn segment_with(
        body: &[u8],
        type_id: u32,
        rows: u32,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let bytes = build_part(
            &[SectionInput {
                type_id,
                rows,
                body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("143000.pgm");
        std::fs::write(&path, &bytes).expect("write segment");
        (dir, path)
    }

    #[test]
    fn opens_a_segment_and_decodes_a_section() {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode empty section");
        let (_dir, path) = segment_with(&body, 1_006_001, 0);

        let segment = Segment::open(&path).expect("open");
        assert_eq!(segment.catalog().source_id, 7);
        assert_eq!(segment.catalog().entries.len(), 1);

        let entry = segment.catalog().entries[0];
        let decoded = segment.decode(&entry).expect("decode");
        assert_eq!(decoded.stats.type_id, 1_006_001);
        assert_eq!(decoded.stats.rows, 0);
    }

    #[test]
    fn a_corrupted_section_body_fails_the_crc_check() {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode");
        let (_dir, path) = segment_with(&body, 1_006_001, 0);

        // Flip a byte inside the section body, just past the segment magic.
        let mut bytes = std::fs::read(&path).expect("read");
        bytes[6] ^= 0x01;
        std::fs::write(&path, &bytes).expect("rewrite");

        let segment = Segment::open(&path).expect("the catalog is intact");
        let entry = segment.catalog().entries[0];
        // The injected crc32c rejects the tampered body before Parquet sees it.
        assert!(matches!(segment.decode(&entry), Err(ReadError::Codec(_))));
    }

    #[test]
    fn a_too_small_file_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tiny.pgm");
        std::fs::write(&path, [0_u8; 4]).expect("write");
        assert!(matches!(
            Segment::open(&path),
            Err(ReadError::TooSmall { len: 4 })
        ));
    }

    #[test]
    fn resolves_interned_strings_from_the_dictionary() {
        use kronika_format::DictLimits;
        use kronika_writer::{Interner, dict};

        // Covers the write/read boundary for segment dictionaries.
        let mut interner = Interner::new(DictLimits::new(4096, 1 << 20).expect("limits"));
        let host = interner.intern(b"db-host-01").expect("intern");
        let node = interner.intern(b"node-7").expect("intern");
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");

        let sections: Vec<_> = dict_sections
            .iter()
            .map(|section| SectionInput {
                type_id: section.type_id,
                rows: section.rows,
                body: &section.body,
            })
            .collect();
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("d.pgm");
        std::fs::write(&path, &bytes).expect("write");

        let segment = Segment::open(&path).expect("open");
        let dictionary = segment.dictionary().expect("read dictionary");
        assert_eq!(dictionary.len(), 2);
        assert_eq!(
            dictionary.resolve(host.get()),
            Some(Resolved::String(b"db-host-01"))
        );
        assert_eq!(
            dictionary.resolve(node.get()),
            Some(Resolved::String(b"node-7"))
        );
        assert_eq!(
            dictionary.resolve(999),
            None,
            "an absent id resolves to None"
        );
    }
}
