//! One-shot cold build of sealed-segment overview facts from a PGM container.
//!
//! [`SegmentFacts::extract`] reads a sealed PGM once through the reader
//! primitives and materializes the retained observations, the catalog
//! manifest, and the segment coverage. The value encodes into a `PGKOVF`
//! container ([`SegmentFacts::encode`]) and reloads from it
//! ([`SegmentFacts::from_bytes`]) without touching PGM section bodies again.
//!
//! The same value answers repeated and restart-warm queries through
//! [`RawOracle`]: the disk round trip is a semantic identity, so an index read
//! and a forced raw decode of the same rows return equal observations, counts,
//! and coverage.

use kronika_analytics::overview::{
    Applicability, Coverage, CoverageSpan, DroppedFieldCount, EventObservation, EvidenceQuality,
    LifecyclePayload, MemoryOracle, NamingContractId, ObservationPayload, ObservationProvenance,
    ObservationShape, ObservationTime, OracleError, OracleLimits, OracleResult, PeriodQuality,
    PhysicalCountSemantics, QualityFlags, RawOracle, RetainedExactness, SectionBodyId,
    SegmentIdentity, SegmentLocator, SourceCompleteness, TimeQuality,
};
use kronika_format::ReadAt;
use kronika_registry::{Cell, Row};

use crate::unit::PgmUnit;
use crate::{Dictionary, ReadError, Resolved};

use super::block::{BlockKind, LossCoverageBlock, SourceManifestBlock, StringTableBlock};
use super::container::{BlockContent, CacheReadError, FactFile, FactFileReader, HeaderIdentity};
use super::descriptors::{
    CatalogEntryDescriptor, DictionaryContextEntry, ManifestEntryDescriptor, dictionary_context_id,
    section_body_id, source_scope_id,
};
use super::limits::Bounds;
use super::observations::EventObservationsBlock;

/// Registry type of the server-lifecycle log section.
const LIFECYCLE_TYPE_ID: u32 = 1_028_001;

/// Reader configuration that scopes a sealed segment's facts.
///
/// The namespace and identity fields come from the reader's segment registry,
/// not from the PGM bytes, so the same segment resealed verbatim keeps its
/// lineage and cache scope.
#[derive(Debug, Clone)]
pub struct SegmentContext {
    /// Stable store namespace; an empty namespace is deployment-scoped.
    pub normalized_store_namespace: Vec<u8>,
    /// Naming-contract identity shared by a segment's lineage.
    pub naming_contract_id: NamingContractId,
    /// Sealed segment locator proving the lineage is content-derived.
    pub segment_locator: SegmentLocator,
}

/// A source read that changes coverage instead of being a cache miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// The source could not be read.
    Io,
    /// A checksum, frame, or catalog validation failed.
    Corrupt,
    /// The PGM container format is unsupported.
    UnsupportedFormat,
    /// A section layout is unsupported by the current extractor.
    UnsupportedLayout,
}

/// Why a cold build could not produce canonical facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// The PGM source failed; it is not masked as a cache miss.
    Source(SourceError),
    /// A safety limit was exceeded before or during encoding.
    LimitExceeded,
    /// Checked integer arithmetic overflowed.
    Overflow,
    /// An internal invariant was violated while building facts.
    Internal,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Io => "source read failed",
            Self::Corrupt => "source integrity check failed",
            Self::UnsupportedFormat => "unsupported source format",
            Self::UnsupportedLayout => "unsupported source layout",
        };
        f.write_str(text)
    }
}

impl std::error::Error for SourceError {}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(source) => write!(f, "source error: {source}"),
            Self::LimitExceeded => f.write_str("safety limit exceeded"),
            Self::Overflow => f.write_str("checked arithmetic overflow"),
            Self::Internal => f.write_str("internal build invariant violated"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<ReadError> for SourceError {
    fn from(error: ReadError) -> Self {
        match error {
            ReadError::Io(_) | ReadError::Store(_) | ReadError::StaleSnapshot { .. } => Self::Io,
            ReadError::BadMagic { .. } | ReadError::UnsupportedFormat { .. } => {
                Self::UnsupportedFormat
            }
            ReadError::SectionOutOfBounds { .. }
            | ReadError::DictionarySection { .. }
            | ReadError::SectionTooLarge { .. }
            | ReadError::CatalogOrdinalOutOfRange { .. }
            | ReadError::CatalogRowCountMismatch { .. } => Self::UnsupportedLayout,
            ReadError::TooSmall { .. }
            | ReadError::Tail(_)
            | ReadError::BadCatalogLen { .. }
            | ReadError::Catalog(_)
            | ReadError::Codec(_)
            | ReadError::CounterOverflow => Self::Corrupt,
        }
    }
}

impl From<ReadError> for BuildError {
    fn from(error: ReadError) -> Self {
        Self::Source(SourceError::from(error))
    }
}

impl From<CacheReadError> for BuildError {
    fn from(error: CacheReadError) -> Self {
        match error {
            CacheReadError::Oversized => Self::LimitExceeded,
            CacheReadError::Io(_) => Self::Source(SourceError::Io),
            CacheReadError::Incompatible
            | CacheReadError::Corrupt
            | CacheReadError::WrongSource => Self::Internal,
        }
    }
}

/// Retained sealed-segment facts materialized from one PGM container.
#[derive(Debug, Clone)]
pub struct SegmentFacts {
    identity: HeaderIdentity,
    lineage: SegmentIdentity,
    manifest_entries: Vec<ManifestEntryDescriptor>,
    observations: Vec<EventObservation>,
    coverage: Coverage,
}

impl SegmentFacts {
    /// Reads a sealed PGM once and materializes its canonical overview facts.
    ///
    /// Every catalog entry contributes a manifest descriptor; supported event
    /// sections additionally contribute retained observations. Coverage spans
    /// the catalog time range.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::Source`] when the PGM cannot be read or decoded,
    /// and [`BuildError::Overflow`] when a catalog ordinal does not fit `u32`.
    pub fn extract<R: ReadAt>(
        unit: &PgmUnit<R>,
        context: &SegmentContext,
    ) -> Result<Self, BuildError> {
        let catalog = unit.catalog();
        let (identity, lineage) = Self::provenance(unit, context)?;

        let dictionary = unit.dictionary()?;
        let mut manifest_entries = Vec::with_capacity(catalog.entries.len());
        let mut observations = Vec::new();
        for (ordinal, entry) in catalog.entries.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_error| BuildError::Overflow)?;
            if is_supported_event_section(entry.type_id) {
                let section = unit.read_overview_section(ordinal)?;
                let body_id = section_body_id(entry.type_id, section.body());
                let rows = unit.decode_rows(entry)?;
                extract_event_rows(
                    entry.type_id,
                    &rows,
                    lineage,
                    &dictionary,
                    context.segment_locator,
                    ordinal,
                    body_id,
                    &mut observations,
                )?;
                manifest_entries.push(ManifestEntryDescriptor::from_verified(
                    entry,
                    section.body(),
                ));
            } else {
                manifest_entries.push(ManifestEntryDescriptor::from_catalog(entry));
            }
        }
        observations.sort_by(EventObservation::canonical_cmp);
        let coverage = segment_coverage(catalog.min_ts, catalog.max_ts)?;
        Ok(Self {
            identity,
            lineage,
            manifest_entries,
            observations,
            coverage,
        })
    }

    /// Derives the header identity and lineage from the catalog alone.
    ///
    /// Reads no section bodies: the cache lookup and the restart-warm reload
    /// derive the same identity a cold build would, so a matching disk file
    /// serves the segment without touching the PGM.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::Source`] when the PGM has no catalog entries.
    pub fn provenance<R: ReadAt>(
        unit: &PgmUnit<R>,
        context: &SegmentContext,
    ) -> Result<(HeaderIdentity, SegmentIdentity), BuildError> {
        let catalog = unit.catalog();
        let first = catalog
            .entries
            .first()
            .ok_or(BuildError::Source(SourceError::UnsupportedLayout))?;
        let source_scope = source_scope_id(&context.normalized_store_namespace, catalog.source_id);
        let lineage = SegmentIdentity::sealed(
            source_scope,
            context.naming_contract_id,
            context.segment_locator,
            first.type_id,
            &CatalogEntryDescriptor::of(first).canonical_bytes(),
        );
        let identity = HeaderIdentity::from_current_contract(
            catalog.format_version,
            catalog.source_id,
            catalog.min_ts,
            catalog.max_ts,
            unit.source_file_len(),
            source_scope,
            unit.source_descriptor(),
        );
        Ok((identity, lineage))
    }

    /// Header identity carrying source scope, descriptor, and version axes.
    #[must_use]
    pub const fn identity(&self) -> &HeaderIdentity {
        &self.identity
    }

    /// Rebuild-stable lineage of the sealed segment.
    #[must_use]
    pub const fn lineage(&self) -> &SegmentIdentity {
        &self.lineage
    }

    /// Retained observations in canonical order.
    #[must_use]
    pub fn observations(&self) -> &[EventObservation] {
        &self.observations
    }

    /// Segment coverage spans.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// Catalog manifest descriptors in catalog order.
    #[must_use]
    pub fn manifest_entries(&self) -> &[ManifestEntryDescriptor] {
        &self.manifest_entries
    }

    /// Encodes the facts into a durable `PGKOVF` fact-file buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError::Oversized`] when a block exceeds a safety
    /// bound and [`CacheReadError::Corrupt`] when a block fails a canonical
    /// invariant while encoding.
    pub fn encode(&self, bounds: &Bounds) -> Result<Vec<u8>, CacheReadError> {
        let manifest = SourceManifestBlock::new(
            self.identity.pgm_source_id,
            self.identity.source_format_version,
            self.identity.source_min_ts_us,
            self.identity.source_max_ts_us,
            self.identity.source_file_len,
            self.manifest_entries.clone(),
            bounds,
        )?;
        let observations = EventObservationsBlock::new(self.observations.clone(), bounds)?;
        let strings = observations.string_table().clone();
        let coverage = LossCoverageBlock::new(
            self.coverage.clone(),
            Coverage::empty(),
            Applicability::Applicable,
            PeriodQuality::Unknown,
            SourceCompleteness::BoundedSubset,
            RetainedExactness::Exact,
            PhysicalCountSemantics::LowerBound,
            0,
            bounds,
        )?;
        FactFile::build(
            &self.identity,
            vec![
                BlockContent::SourceManifest(Box::new(manifest)),
                BlockContent::StringTable(Box::new(strings)),
                BlockContent::EventObservations(Box::new(observations)),
                BlockContent::LossCoverage(Box::new(coverage)),
            ],
            bounds,
        )
    }

    /// Reloads facts from a fully validated in-memory fact-file buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when admission rejects the buffer or a block
    /// fails to decode.
    pub fn from_bytes(
        bytes: &[u8],
        expected: &HeaderIdentity,
        lineage: &SegmentIdentity,
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        let file = FactFile::admit(bytes, expected, lineage, bounds)?;
        let manifest =
            SourceManifestBlock::decode(body(&file, BlockKind::SourceManifest)?, bounds)?;
        let strings = StringTableBlock::decode(body(&file, BlockKind::StringTable)?, bounds)?;
        let observations = EventObservationsBlock::decode(
            body(&file, BlockKind::EventObservations)?,
            lineage,
            &strings,
            bounds,
        )?;
        let coverage = LossCoverageBlock::decode(body(&file, BlockKind::LossCoverage)?, bounds)?;
        Ok(Self {
            identity: file.header().identity,
            lineage: *lineage,
            manifest_entries: manifest.entries().to_vec(),
            observations: observations.observations().to_vec(),
            coverage: coverage.covered().clone(),
        })
    }

    /// Reloads facts with selective positional reads over a fact-file source.
    ///
    /// This is the restart-warm path: it reads the fixed header, the bounded
    /// directory, and only the required block bodies, and never touches the
    /// PGM.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when admission rejects the file or a block
    /// fails to decode.
    pub fn from_reader<R: ReadAt>(
        reader: R,
        expected: &HeaderIdentity,
        lineage: &SegmentIdentity,
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
        let manifest = SourceManifestBlock::decode(
            first_body(&mut fact_reader, BlockKind::SourceManifest)?.as_slice(),
            bounds,
        )?;
        let strings = StringTableBlock::decode(
            first_body(&mut fact_reader, BlockKind::StringTable)?.as_slice(),
            bounds,
        )?;
        let observations = EventObservationsBlock::decode(
            first_body(&mut fact_reader, BlockKind::EventObservations)?.as_slice(),
            lineage,
            &strings,
            bounds,
        )?;
        let coverage = LossCoverageBlock::decode(
            first_body(&mut fact_reader, BlockKind::LossCoverage)?.as_slice(),
            bounds,
        )?;
        Ok(Self {
            identity: fact_reader.header().identity,
            lineage: *lineage,
            manifest_entries: manifest.entries().to_vec(),
            observations: observations.observations().to_vec(),
            coverage: coverage.covered().clone(),
        })
    }
}

impl RawOracle for SegmentFacts {
    fn query(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<OracleResult, OracleError> {
        MemoryOracle::new(self.observations.clone(), self.coverage.clone())?.query(range, limits)
    }
}

/// Whether the extractor materializes observations for a section type.
const fn is_supported_event_section(type_id: u32) -> bool {
    matches!(type_id, LIFECYCLE_TYPE_ID)
}

/// Half-open coverage of an inclusive catalog time range.
fn segment_coverage(min_ts_us: i64, max_ts_us: i64) -> Result<Coverage, BuildError> {
    let end = max_ts_us.checked_add(1).ok_or(BuildError::Overflow)?;
    let span = CoverageSpan::new(min_ts_us, end).ok_or(BuildError::Internal)?;
    Ok(Coverage::from_spans(vec![span]))
}

/// First body of `kind` in a validated in-memory fact file.
fn body(file: &FactFile, kind: BlockKind) -> Result<&[u8], CacheReadError> {
    file.block_body(kind).ok_or(CacheReadError::Corrupt)
}

/// First positionally read body of `kind`, or an empty body when absent.
fn first_body<R: ReadAt>(
    reader: &mut FactFileReader<R>,
    kind: BlockKind,
) -> Result<Vec<u8>, CacheReadError> {
    Ok(reader
        .read_blocks(kind)?
        .into_iter()
        .next()
        .unwrap_or_default())
}

/// Appends observations decoded from one supported event section.
#[allow(
    clippy::too_many_arguments,
    reason = "row provenance needs the full segment coordinates"
)]
fn extract_event_rows(
    type_id: u32,
    rows: &[Row],
    lineage: SegmentIdentity,
    dictionary: &Dictionary,
    locator: SegmentLocator,
    catalog_entry_ordinal: u32,
    section_body_id: SectionBodyId,
    observations: &mut Vec<EventObservation>,
) -> Result<(), BuildError> {
    for (row_index, row) in rows.iter().enumerate() {
        let row_ordinal = u32::try_from(row_index).map_err(|_error| BuildError::Overflow)?;
        let provenance = ObservationProvenance {
            segment_locator: Some(locator),
            section_body_id,
            catalog_entry_ordinal,
            row_ordinal,
            dictionary_context_id: row_context_id(row, dictionary)?,
            source_locator: None,
        };
        let observation = match type_id {
            LIFECYCLE_TYPE_ID => lifecycle_observation(row, lineage, dictionary, provenance)?,
            _ => return Err(BuildError::Source(SourceError::UnsupportedLayout)),
        };
        observations.push(observation);
    }
    Ok(())
}

/// Builds one lifecycle observation from a `pg_log_lifecycle` row.
fn lifecycle_observation(
    row: &Row,
    lineage: SegmentIdentity,
    dictionary: &Dictionary,
    provenance: ObservationProvenance,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let kind = cell_small_uint(row, "kind")?;
    let pid = cell_opt_i32(row, "pid");
    let signal = cell_opt_i32(row, "signal");
    let shutdown_mode = resolve_text(dictionary, cell_opt_str_id(row, "shutdown_mode"));
    let message = resolve_text(dictionary, cell_opt_str_id(row, "message"));
    let query_detail = resolve_text(dictionary, cell_opt_str_id(row, "query_detail"));
    let dropped = DroppedFieldCount(cell_small_uint(row, "dict_dropped_fields").unwrap_or(0));

    let payload = match kind {
        0 => ObservationPayload::ChildSignalTermination(Box::new(LifecyclePayload {
            pid,
            signal,
            shutdown_mode: None,
            message,
            query_detail,
            dropped_field_count: dropped,
        })),
        1 => ObservationPayload::ShutdownRequested(Box::new(LifecyclePayload {
            pid,
            signal,
            shutdown_mode,
            message,
            query_detail,
            dropped_field_count: dropped,
        })),
        2 => ObservationPayload::ReadyObserved(Box::new(LifecyclePayload {
            pid: None,
            signal: None,
            shutdown_mode: None,
            message,
            query_detail: None,
            dropped_field_count: dropped,
        })),
        _ => return Err(BuildError::Source(SourceError::Corrupt)),
    };

    EventObservation::new(
        lineage,
        LIFECYCLE_TYPE_ID,
        provenance,
        ObservationShape::Individual,
        ObservationTime {
            sort_ts_us: ts,
            occurred_at_us: Some(ts),
            observed_interval: None,
            quality: TimeQuality::Exact,
        },
        1,
        payload,
        EvidenceQuality::Structured,
        QualityFlags(0),
        None,
    )
    .map_err(|_error| BuildError::Internal)
}

/// Digest of the canonically ordered dictionary values a row references.
fn row_context_id(
    row: &Row,
    dictionary: &Dictionary,
) -> Result<kronika_analytics::overview::DictionaryContextId, BuildError> {
    let mut owned: Vec<(u64, Vec<u8>, u64, bool)> = Vec::new();
    for cell in row.cells() {
        let Cell::StrId(str_id) = cell else {
            continue;
        };
        match dictionary.resolve(*str_id) {
            None => {}
            Some(Resolved::String(bytes)) => {
                owned.push((*str_id, bytes.to_vec(), bytes.len() as u64, false));
            }
            Some(Resolved::Blob {
                bytes,
                full_len,
                truncated,
            }) => {
                owned.push((*str_id, bytes.to_vec(), full_len, truncated));
            }
        }
    }
    owned.sort_by_key(|entry| entry.0);
    owned.dedup_by_key(|entry| entry.0);
    let entries: Vec<DictionaryContextEntry<'_>> = owned
        .iter()
        .map(
            |(str_id, bytes, full_len, truncated)| DictionaryContextEntry {
                str_id: *str_id,
                bytes,
                full_len: *full_len,
                truncated: *truncated,
            },
        )
        .collect();
    dictionary_context_id(&entries).ok_or(BuildError::Internal)
}

/// Resolves a dictionary reference to owned UTF-8 text, dropping invalid bytes.
fn resolve_text(dictionary: &Dictionary, str_id: Option<u64>) -> Option<Box<str>> {
    let (Resolved::String(bytes) | Resolved::Blob { bytes, .. }) = dictionary.resolve(str_id?)?;
    std::str::from_utf8(bytes).ok().map(Box::from)
}

/// Reads a required timestamp cell.
fn cell_ts(row: &Row, name: &str) -> Result<i64, BuildError> {
    match row.get(name) {
        Some(Cell::Ts(ts)) => Ok(*ts),
        _ => Err(BuildError::Source(SourceError::UnsupportedLayout)),
    }
}

/// Reads a required small unsigned cell decoded as `u32`.
fn cell_small_uint(row: &Row, name: &str) -> Result<u32, BuildError> {
    match row.get(name) {
        Some(Cell::U32(value)) => Ok(*value),
        _ => Err(BuildError::Source(SourceError::UnsupportedLayout)),
    }
}

/// Reads an optional signed 32-bit cell.
fn cell_opt_i32(row: &Row, name: &str) -> Option<i32> {
    match row.get(name) {
        Some(Cell::I32(value)) => Some(*value),
        _ => None,
    }
}

/// Reads an optional dictionary reference cell.
fn cell_opt_str_id(row: &Row, name: &str) -> Option<u64> {
    match row.get(name) {
        Some(Cell::StrId(str_id)) => Some(*str_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{CountLimits, SemanticDivergence, semantic_divergences};
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};

    use super::super::limits::LIMIT;
    use super::*;

    const LIMITS: OracleLimits = OracleLimits {
        max_observations: 256,
        max_coverage_spans: 256,
        count_limits: CountLimits {
            max_input_entries: 256,
            max_joint_keys: 256,
            max_signal_keys: 256,
        },
    };

    fn context() -> SegmentContext {
        SegmentContext {
            normalized_store_namespace: b"store-a".to_vec(),
            naming_contract_id: NamingContractId([0x33; 16]),
            segment_locator: SegmentLocator([0x44; 32]),
        }
    }

    fn lifecycle_row(ts: i64, kind: u8, pid: Option<i32>, signal: Option<i32>) -> PgLogLifecycleV1 {
        PgLogLifecycleV1 {
            ts: Ts(ts),
            kind,
            pid,
            signal,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        }
    }

    fn lifecycle_pgm(rows: &[PgLogLifecycleV1], min_ts: i64, max_ts: i64) -> Vec<u8> {
        let body = PgLogLifecycleV1::encode(rows).expect("encode lifecycle section");
        let rows_len = u32::try_from(rows.len()).expect("row count fits u32");
        build_part(
            &[SectionInput {
                type_id: LIFECYCLE_TYPE_ID,
                rows: rows_len,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        )
    }

    fn three_lifecycle_events() -> Vec<u8> {
        lifecycle_pgm(
            &[
                lifecycle_row(1_500, 2, None, None),
                lifecycle_row(1_600, 1, None, None),
                lifecycle_row(1_700, 0, Some(42), Some(9)),
            ],
            1_500,
            1_700,
        )
    }

    fn full_range() -> CoverageSpan {
        CoverageSpan::new(0, 10_000).expect("valid range")
    }

    #[test]
    fn extract_materializes_one_observation_per_lifecycle_row() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context()).expect("extract");
        assert_eq!(facts.observations().len(), 3);
        assert_eq!(facts.manifest_entries().len(), 1);
        assert_eq!(
            facts.observations()[0].time().sort_ts_us,
            1_500,
            "canonical order starts at the earliest timestamp"
        );
        assert!(matches!(
            facts.observations()[2].payload(),
            ObservationPayload::ChildSignalTermination(_)
        ));
    }

    #[test]
    fn the_index_path_semantically_equals_a_forced_raw_decode() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context()).expect("raw extract");
        let encoded = raw.encode(&LIMIT).expect("encode fact file");
        let index = SegmentFacts::from_bytes(&encoded, raw.identity(), raw.lineage(), &LIMIT)
            .expect("admit");
        let divergences =
            semantic_divergences(&index, &raw, full_range(), LIMITS).expect("bounded comparison");
        assert_eq!(divergences, Vec::<SemanticDivergence>::new());
    }

    #[test]
    fn retained_observations_survive_the_disk_round_trip_exactly() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context()).expect("raw extract");
        let encoded = raw.encode(&LIMIT).expect("encode fact file");
        let index = SegmentFacts::from_bytes(&encoded, raw.identity(), raw.lineage(), &LIMIT)
            .expect("admit");
        assert_eq!(index.observations(), raw.observations());
        assert_eq!(index.coverage(), raw.coverage());
        assert_eq!(index.manifest_entries(), raw.manifest_entries());
        let index_result = index.query(full_range(), LIMITS).expect("index query");
        let raw_result = raw.query(full_range(), LIMITS).expect("raw query");
        assert_eq!(index_result.counts(), raw_result.counts());
    }

    #[test]
    fn a_forced_recompute_matches_the_derived_answer() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let derived = SegmentFacts::extract(&unit, &context()).expect("first build");
        let recomputed = SegmentFacts::extract(&unit, &context()).expect("forced recompute");
        let divergences = semantic_divergences(&derived, &recomputed, full_range(), LIMITS)
            .expect("bounded comparison");
        assert!(divergences.is_empty());
    }

    #[test]
    fn range_slices_partition_retained_facts_without_double_counting() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context()).expect("extract");

        let left = facts
            .query(CoverageSpan::new(0, 1_600).expect("left"), LIMITS)
            .expect("left query");
        let right = facts
            .query(CoverageSpan::new(1_600, 10_000).expect("right"), LIMITS)
            .expect("right query");
        assert_eq!(left.observations().len(), 1, "boundary is half-open");
        assert_eq!(right.observations().len(), 2);
        assert_eq!(
            left.observations().len() + right.observations().len(),
            facts.observations().len(),
            "a split range neither drops nor duplicates observations"
        );
    }

    #[test]
    fn the_restart_warm_path_reloads_without_touching_the_pgm() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context()).expect("raw extract");
        let encoded = raw.encode(&LIMIT).expect("encode fact file");

        // The positional reader is handed only the fact-file bytes; it never has
        // a handle to the PGM, so a successful reload proves the sealed interior
        // is served without any PGM body read.
        let warm =
            SegmentFacts::from_reader(encoded.as_slice(), raw.identity(), raw.lineage(), &LIMIT)
                .expect("positional reload");
        assert_eq!(warm.observations(), raw.observations());
        assert_eq!(warm.coverage(), raw.coverage());
    }

    #[test]
    fn a_different_namespace_changes_scope_and_identity_but_not_counts() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let here = SegmentFacts::extract(&unit, &context()).expect("here");
        let elsewhere = SegmentFacts::extract(
            &unit,
            &SegmentContext {
                normalized_store_namespace: b"store-b".to_vec(),
                ..context()
            },
        )
        .expect("elsewhere");
        assert_ne!(
            here.identity().source_scope_id,
            elsewhere.identity().source_scope_id
        );
        // Scope feeds the lineage, so observation identities differ, yet the
        // scope-independent counts are unchanged.
        assert_eq!(here.observations().len(), elsewhere.observations().len());
        let here_result = here.query(full_range(), LIMITS).expect("here query");
        let elsewhere_result = elsewhere
            .query(full_range(), LIMITS)
            .expect("elsewhere query");
        assert_eq!(here_result.counts(), elsewhere_result.counts());
    }

    #[test]
    fn a_corrupt_fact_buffer_is_a_typed_error_not_a_panic() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context()).expect("raw extract");
        let mut encoded = raw.encode(&LIMIT).expect("encode fact file");
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;
        let outcome = SegmentFacts::from_bytes(&encoded, raw.identity(), raw.lineage(), &LIMIT);
        assert!(matches!(
            outcome,
            Err(CacheReadError::Corrupt | CacheReadError::Incompatible | CacheReadError::Oversized)
        ));
    }

    #[test]
    fn lifecycle_events_fold_into_lifecycle_counts() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context()).expect("extract");
        let result = facts.query(full_range(), LIMITS).expect("query");
        let lifecycle = result.counts().lifecycle();
        assert_eq!(lifecycle.crashes(), 1);
        assert_eq!(lifecycle.shutdowns(), 1);
        assert_eq!(lifecycle.ready(), 1);
        assert_eq!(lifecycle.signals(), &[(9, 1)]);
        // No error groups were retained, so the joint occurrence total is zero.
        assert_eq!(result.counts().total_occurrences(), Ok(0));
    }
}
