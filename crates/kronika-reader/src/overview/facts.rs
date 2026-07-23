//! Cold extraction and reload of sealed-segment overview facts.
//!
//! [`SegmentFacts::extract`] reads each supported event section body once,
//! resolves referenced dictionary values, and materializes retained
//! observations, manifest descriptors, and loss/coverage facts. [`encode`]
//! writes a `PGKOVF` buffer; [`from_reader`] reloads it without reading PGM
//! section bodies. [`SegmentFacts`] implements [`RawOracle`], so cached and
//! freshly extracted facts use the same bounded query contract.
//!
//! [`encode`]: SegmentFacts::encode
//! [`from_reader`]: SegmentFacts::from_reader

use kronika_analytics::overview::{
    Applicability, Coverage, CoverageSpan, EventObservation, MemoryOracle, NamingContractId,
    OracleError, OracleLimits, OracleResult, PeriodQuality, PhysicalCountSemantics, RawOracle,
    RetainedExactness, SegmentIdentity, SegmentLocator, SourceCompleteness,
};
use kronika_format::ReadAt;

use crate::unit::PgmUnit;
use crate::{PgmBodyReadStats, ReadError};

use super::block::{BlockKind, LossCoverageBlock, SourceManifestBlock, StringTableBlock};
use super::container::{
    BlockContent, CacheReadError, FactFile, FactFileReader, FactReadStats, HeaderIdentity,
    validate_block_descriptor, validate_observation_provenance, verify_manifest_identity,
};
use super::descriptors::{CatalogEntryDescriptor, ManifestEntryDescriptor, source_scope_id};
use super::event_extract::extract_events;
use super::limits::Bounds;
use super::observations::EventObservationsBlock;

const MAX_STORE_NAMESPACE_BYTES: usize = 4 * 1024;

/// Reader configuration that scopes a sealed segment's facts.
///
/// The namespace and locator come from the reader's segment registry, not from
/// PGM row timestamps.
#[derive(Debug, Clone)]
pub struct SegmentContext {
    normalized_store_namespace: Vec<u8>,
    naming_contract_id: NamingContractId,
    segment_locator: SegmentLocator,
}

/// Invalid reader configuration for sealed-segment identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentContextError {
    /// Production identity requires a stable non-empty store namespace.
    EmptyStoreNamespace,
    /// The configured namespace exceeds the fixed identity-input bound.
    StoreNamespaceTooLong,
}

impl std::fmt::Display for SegmentContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyStoreNamespace => f.write_str("store namespace is empty"),
            Self::StoreNamespaceTooLong => f.write_str("store namespace exceeds 4096 bytes"),
        }
    }
}

impl std::error::Error for SegmentContextError {}

impl SegmentContext {
    /// Builds a bounded context from a reader-supplied namespace and sealed
    /// segment locator.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentContextError`] for an empty or oversized namespace.
    pub fn new(
        normalized_store_namespace: impl Into<Vec<u8>>,
        naming_contract_id: NamingContractId,
        segment_locator: SegmentLocator,
    ) -> Result<Self, SegmentContextError> {
        let normalized_store_namespace = normalized_store_namespace.into();
        if normalized_store_namespace.is_empty() {
            return Err(SegmentContextError::EmptyStoreNamespace);
        }
        if normalized_store_namespace.len() > MAX_STORE_NAMESPACE_BYTES {
            return Err(SegmentContextError::StoreNamespaceTooLong);
        }
        Ok(Self {
            normalized_store_namespace,
            naming_contract_id,
            segment_locator,
        })
    }

    /// Stable normalized store namespace.
    #[must_use]
    pub fn store_namespace(&self) -> &[u8] {
        &self.normalized_store_namespace
    }

    /// Versioned naming contract for the supplied locator.
    #[must_use]
    pub const fn naming_contract_id(&self) -> NamingContractId {
        self.naming_contract_id
    }

    /// Reader-supplied locator of the sealed segment.
    #[must_use]
    pub const fn segment_locator(&self) -> SegmentLocator {
        self.segment_locator
    }
}

/// Why reading or decoding the PGM source failed.
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
    /// A fact-building or read-accounting invariant failed.
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
    loss_coverage: LossCoverageBlock,
}

impl SegmentFacts {
    /// Reads each supported event body once and materializes canonical facts.
    ///
    /// Every catalog entry contributes a manifest descriptor; supported event
    /// sections additionally contribute retained observations. Coverage spans
    /// the catalog time range.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] for source failures, unsupported event layouts,
    /// unsafe work bounds, or checked-arithmetic overflow.
    pub fn extract<R: ReadAt>(
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<Self, BuildError> {
        Self::extract_with_stats(unit, context, bounds).map(|(facts, _stats)| facts)
    }

    pub(super) fn extract_with_stats<R: ReadAt>(
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<(Self, PgmBodyReadStats), BuildError> {
        let catalog = unit.catalog();
        let (identity, lineage) = Self::provenance(unit, context)?;
        let extracted = extract_events(unit, lineage, context.segment_locator, bounds)?;
        let pgm_body_read_stats = extracted.pgm_body_read_stats;
        let covered = segment_coverage(catalog.min_ts, catalog.max_ts)?;
        let loss_coverage = LossCoverageBlock::new(
            covered,
            extracted.known_gaps,
            Applicability::Applicable,
            PeriodQuality::Unknown,
            SourceCompleteness::BoundedSubset,
            RetainedExactness::Exact,
            PhysicalCountSemantics::LowerBound,
            extracted.dropped_lower_bound,
            bounds,
        )
        .map_err(|error| match error {
            super::block::BlockError::AboveBound => BuildError::LimitExceeded,
            _ => BuildError::Internal,
        })?;
        Ok((
            Self {
                identity,
                lineage,
                manifest_entries: extracted.manifest_entries,
                observations: extracted.observations,
                loss_coverage,
            },
            pgm_body_read_stats,
        ))
    }

    /// Derives the header identity and lineage from the catalog alone.
    ///
    /// Reads no section bodies: the cache lookup and the restart-warm reload
    /// derive the same identity a cold build would, so a matching fact file
    /// loads without reading a PGM section body.
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
        let source_scope = source_scope_id(context.store_namespace(), catalog.source_id);
        let lineage = SegmentIdentity::sealed(
            source_scope,
            context.naming_contract_id(),
            context.segment_locator(),
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
        self.loss_coverage.covered()
    }

    /// Coverage, explicit gap, and retained-exactness metadata.
    #[must_use]
    pub const fn loss_coverage(&self) -> &LossCoverageBlock {
        &self.loss_coverage
    }

    /// Catalog manifest descriptors in catalog order.
    #[must_use]
    pub fn manifest_entries(&self) -> &[ManifestEntryDescriptor] {
        &self.manifest_entries
    }

    /// Offset-independent catalog descriptors expected on a cache reload.
    #[must_use]
    pub fn catalog_descriptors(&self) -> Vec<CatalogEntryDescriptor> {
        self.manifest_entries
            .iter()
            .map(|entry| entry.catalog)
            .collect()
    }

    /// Encodes the facts into a `PGKOVF` fact-file buffer.
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
        FactFile::build(
            &self.identity,
            vec![
                BlockContent::SourceManifest(Box::new(manifest)),
                BlockContent::StringTable(Box::new(strings)),
                BlockContent::EventObservations(Box::new(observations)),
                BlockContent::LossCoverage(Box::new(self.loss_coverage.clone())),
            ],
            bounds,
        )
    }

    /// Admits an in-memory fact-file buffer and reloads its facts.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when admission rejects the buffer or a block
    /// fails to decode.
    pub fn from_bytes(
        bytes: &[u8],
        expected: &HeaderIdentity,
        lineage: &SegmentIdentity,
        expected_catalog: &[CatalogEntryDescriptor],
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        FactFile::admit(bytes, expected, lineage, bounds)?;
        Self::from_reader(bytes, expected, lineage, expected_catalog, bounds)
    }

    /// Reloads facts with selective positional reads over a fact-file source.
    ///
    /// This is the restart-warm path: it reads the fixed header, the bounded
    /// directory, and only the required fact block bodies. It takes no PGM
    /// source.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when admission rejects the file or a block
    /// fails to decode.
    pub fn from_reader<R: ReadAt>(
        reader: R,
        expected: &HeaderIdentity,
        lineage: &SegmentIdentity,
        expected_catalog: &[CatalogEntryDescriptor],
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        Self::from_reader_with_stats(reader, expected, lineage, expected_catalog, bounds)
            .map(|(facts, _stats)| facts)
    }

    /// Reloads facts and returns exact positional fact-file read counters.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] under the same conditions as
    /// [`Self::from_reader`].
    pub fn from_reader_with_stats<R: ReadAt>(
        reader: R,
        expected: &HeaderIdentity,
        lineage: &SegmentIdentity,
        expected_catalog: &[CatalogEntryDescriptor],
        bounds: &Bounds,
    ) -> Result<(Self, FactReadStats), CacheReadError> {
        if lineage.source_scope_id() != expected.source_scope_id {
            return Err(CacheReadError::WrongSource);
        }
        let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
        let (manifest_entry, manifest_body) =
            singleton_body(&mut fact_reader, BlockKind::SourceManifest)?;
        let manifest = SourceManifestBlock::decode(&manifest_body, bounds)?;
        validate_block_descriptor(&manifest_entry, &manifest)?;
        verify_manifest_identity(&manifest, expected)?;
        if manifest.entries().len() != expected_catalog.len()
            || manifest
                .entries()
                .iter()
                .zip(expected_catalog)
                .any(|(actual, expected)| actual.catalog != *expected)
        {
            return Err(CacheReadError::WrongSource);
        }

        let (strings_entry, strings_body) =
            singleton_body(&mut fact_reader, BlockKind::StringTable)?;
        let strings = StringTableBlock::decode(&strings_body, bounds)?;
        validate_block_descriptor(&strings_entry, &strings)?;

        let mut observations = Vec::new();
        let mut referenced_strings = Vec::new();
        let mut remaining_observations = bounds.items_per_block;
        let mut text_budget = bounds.decoded_block_len;
        for (entry, body) in fact_reader.read_blocks_with_entries(BlockKind::EventObservations)? {
            let block = EventObservationsBlock::decode_with_budgets(
                &body,
                lineage,
                &strings,
                bounds,
                &mut remaining_observations,
                &mut text_budget,
            )?;
            validate_block_descriptor(&entry, &block)?;
            validate_observation_provenance(&block, &manifest)?;
            referenced_strings.extend(block.string_table().values().iter().cloned());
            observations.extend(block.into_observations());
        }
        let observations = EventObservationsBlock::new(observations, bounds)?;
        if StringTableBlock::new(referenced_strings, bounds)? != strings
            || observations.string_table() != &strings
        {
            return Err(CacheReadError::Corrupt);
        }

        let coverage = merge_coverage_blocks(&mut fact_reader, bounds)?;
        let stats = fact_reader.stats();
        Ok((
            Self {
                identity: fact_reader.header().identity,
                lineage: *lineage,
                manifest_entries: manifest.entries().to_vec(),
                observations: observations.into_observations(),
                loss_coverage: coverage,
            },
            stats,
        ))
    }
}

impl RawOracle for SegmentFacts {
    fn query(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<OracleResult, OracleError> {
        MemoryOracle::new(self.observations.clone(), self.coverage().clone())?.query(range, limits)
    }
}

/// Half-open coverage of an inclusive catalog time range.
fn segment_coverage(min_ts_us: i64, max_ts_us: i64) -> Result<Coverage, BuildError> {
    let end = max_ts_us.checked_add(1).ok_or(BuildError::Overflow)?;
    let span = CoverageSpan::new(min_ts_us, end).ok_or(BuildError::Internal)?;
    Ok(Coverage::from_spans(vec![span]))
}

fn singleton_body<R: ReadAt>(
    reader: &mut FactFileReader<R>,
    kind: BlockKind,
) -> Result<(super::container::BlockDirectoryEntry, Vec<u8>), CacheReadError> {
    let mut bodies = reader.read_blocks_with_entries(kind)?;
    if bodies.len() != 1 {
        return Err(CacheReadError::Corrupt);
    }
    bodies.pop().ok_or(CacheReadError::Corrupt)
}

fn merge_coverage_blocks<R: ReadAt>(
    reader: &mut FactFileReader<R>,
    bounds: &Bounds,
) -> Result<LossCoverageBlock, CacheReadError> {
    let blocks = reader.read_blocks_with_entries(BlockKind::LossCoverage)?;
    if blocks.is_empty() {
        return Err(CacheReadError::Corrupt);
    }
    let mut covered = Coverage::empty();
    let mut known_gaps = Coverage::empty();
    let mut applicability = None;
    let mut period_quality = None;
    let mut source_completeness = None;
    let mut retained_exactness = None;
    let mut physical_count = None;
    let mut dropped_lower_bound = 0_u64;
    let mut covered_span_budget = bounds.coverage_spans;
    let mut gap_span_budget = bounds.coverage_spans;
    for (entry, body) in blocks {
        let block = LossCoverageBlock::decode_with_span_budgets(
            &body,
            bounds,
            &mut covered_span_budget,
            &mut gap_span_budget,
        )?;
        validate_block_descriptor(&entry, &block)?;
        covered = covered.union(block.covered());
        known_gaps = known_gaps.union(block.known_gaps());
        applicability = Some(merge_applicability(applicability, block.applicability()));
        period_quality = Some(merge_period_quality(period_quality, block.period_quality()));
        source_completeness = Some(merge_source_completeness(
            source_completeness,
            block.source_completeness(),
        ));
        retained_exactness = Some(merge_retained_exactness(
            retained_exactness,
            block.retained_exactness(),
        ));
        physical_count = Some(merge_physical_count(physical_count, block.physical_count()));
        dropped_lower_bound = dropped_lower_bound
            .checked_add(block.dropped_lower_bound())
            .ok_or(CacheReadError::Corrupt)?;
    }
    LossCoverageBlock::new(
        covered,
        known_gaps,
        applicability.ok_or(CacheReadError::Corrupt)?,
        period_quality.ok_or(CacheReadError::Corrupt)?,
        source_completeness.ok_or(CacheReadError::Corrupt)?,
        retained_exactness.ok_or(CacheReadError::Corrupt)?,
        physical_count.ok_or(CacheReadError::Corrupt)?,
        dropped_lower_bound,
        bounds,
    )
    .map_err(Into::into)
}

fn merge_applicability(current: Option<Applicability>, next: Applicability) -> Applicability {
    match current {
        None => next,
        Some(previous) if previous == next => next,
        Some(_) => Applicability::Unsupported,
    }
}

const fn merge_period_quality(
    current: Option<PeriodQuality>,
    next: PeriodQuality,
) -> PeriodQuality {
    let Some(previous) = current else {
        return next;
    };
    if period_quality_rank(previous) >= period_quality_rank(next) {
        previous
    } else {
        next
    }
}

const fn period_quality_rank(value: PeriodQuality) -> u8 {
    match value {
        PeriodQuality::PersistedConfigEpoch => 0,
        PeriodQuality::ObservedStable => 1,
        PeriodQuality::AssumedCurrentConfig => 2,
        PeriodQuality::Unknown => 3,
    }
}

const fn merge_source_completeness(
    current: Option<SourceCompleteness>,
    next: SourceCompleteness,
) -> SourceCompleteness {
    let Some(previous) = current else {
        return next;
    };
    if source_completeness_rank(previous) >= source_completeness_rank(next) {
        previous
    } else {
        next
    }
}

const fn source_completeness_rank(value: SourceCompleteness) -> u8 {
    match value {
        SourceCompleteness::Full => 0,
        SourceCompleteness::BoundedSubset => 1,
        SourceCompleteness::Unknown => 2,
    }
}

const fn merge_retained_exactness(
    current: Option<RetainedExactness>,
    next: RetainedExactness,
) -> RetainedExactness {
    let Some(previous) = current else {
        return next;
    };
    if retained_exactness_rank(previous) >= retained_exactness_rank(next) {
        previous
    } else {
        next
    }
}

const fn retained_exactness_rank(value: RetainedExactness) -> u8 {
    match value {
        RetainedExactness::Exact => 0,
        RetainedExactness::LowerBound => 1,
        RetainedExactness::Unknown => 2,
    }
}

fn merge_physical_count(
    current: Option<PhysicalCountSemantics>,
    next: PhysicalCountSemantics,
) -> PhysicalCountSemantics {
    let Some(previous) = current else {
        return next;
    };
    if previous == next {
        return next;
    }
    if previous == PhysicalCountSemantics::Unknown
        || next == PhysicalCountSemantics::Unknown
        || previous == PhysicalCountSemantics::NotApplicable
        || next == PhysicalCountSemantics::NotApplicable
    {
        PhysicalCountSemantics::Unknown
    } else {
        PhysicalCountSemantics::LowerBound
    }
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{
        CountLimits, ErrorCategory, EvidenceQuality, LossReason, ObservationPayload,
        SemanticDivergence, SqlState, TimeQuality, semantic_divergences,
    };
    use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_log::{
        PgLogAutovacuumV1, PgLogCheckpointV1, PgLogErrorV1, PgLogGapV1, PgLogLifecycleV1,
        PgLogLockWaitV1, PgLogSlowQueryV1, PgLogTempFileV1,
    };
    use kronika_registry::{Section, StrId, Ts};
    use kronika_writer::{Interner, dict};

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
        SegmentContext::new(
            b"store-a".to_vec(),
            NamingContractId([0x33; 16]),
            SegmentLocator([0x44; 32]),
        )
        .expect("valid context")
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
        let rows_len = row_count(rows);
        build_part(
            &[SectionInput {
                type_id: 1_028_001,
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

    fn row_count<T>(rows: &[T]) -> u32 {
        u32::try_from(rows.len()).expect("fixture row count fits u32")
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

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture populates every retained field across the eight registered layouts"
    )]
    fn all_log_event_types_pgm() -> Vec<u8> {
        let mut interner =
            Interner::new(DictLimits::new(4_096, 1 << 20).expect("dictionary limits"));
        let sqlstate = StrId(interner.intern(b"40P01").expect("intern SQLSTATE").get());
        let error_pattern = StrId(
            interner
                .intern(b"deadlock detected")
                .expect("intern pattern")
                .get(),
        );
        let error_sample = StrId(
            interner
                .intern(b"deadlock detected while waiting")
                .expect("intern sample")
                .get(),
        );
        let checkpoint_reason = StrId(
            interner
                .intern(b"time")
                .expect("intern checkpoint reason")
                .get(),
        );
        let relation = StrId(
            interner
                .intern(b"public.orders")
                .expect("intern relation")
                .get(),
        );
        let slow_pattern = StrId(
            interner
                .intern(b"select * from orders where id = ...")
                .expect("intern slow pattern")
                .get(),
        );
        let slow_sample = StrId(
            interner
                .intern(b"select * from orders where id = 42")
                .expect("intern slow sample")
                .get(),
        );
        let lock_mode = StrId(
            interner
                .intern(b"ShareLock")
                .expect("intern lock mode")
                .get(),
        );
        let lock_target = StrId(
            interner
                .intern(b"transaction 123")
                .expect("intern lock target")
                .get(),
        );
        let lifecycle_message = StrId(
            interner
                .intern(b"server process terminated")
                .expect("intern lifecycle message")
                .get(),
        );
        let source_path = StrId(
            interner
                .intern(b"/var/log/postgresql/postgresql.log")
                .expect("intern source path")
                .get(),
        );
        let temp_path = StrId(
            interner
                .intern(b"base/pgsql_tmp/pgsql_tmp42.0")
                .expect("intern temp path")
                .get(),
        );

        let mut sections = vec![
            (
                1_022_001,
                1,
                PgLogErrorV1::encode(&[PgLogErrorV1 {
                    ts: Ts(1_100),
                    severity: 0,
                    category: 0,
                    sqlstate: Some(sqlstate),
                    pattern: Some(error_pattern),
                    count: 3,
                    sample: Some(error_sample),
                    detail: None,
                    hint: None,
                    context: None,
                    statement: None,
                    database: None,
                    username: None,
                    dict_dropped_fields: 0,
                }])
                .expect("encode errors"),
            ),
            (
                1_024_001,
                1,
                PgLogCheckpointV1::encode(&[PgLogCheckpointV1 {
                    ts: Ts(1_200),
                    phase: 1,
                    reason: Some(checkpoint_reason),
                    seconds_apart: None,
                    buffers_written: Some(10),
                    write_ms: Some(1.0),
                    sync_ms: Some(2.0),
                    total_ms: Some(3.0),
                    distance_kb: Some(16),
                    estimate_kb: Some(32),
                    wal_added: Some(1),
                    wal_removed: Some(0),
                    wal_recycled: Some(2),
                    sync_files: Some(3),
                    longest_sync_ms: Some(0.5),
                    average_sync_ms: Some(0.25),
                    dict_dropped_fields: 0,
                }])
                .expect("encode checkpoints"),
            ),
            (
                1_025_001,
                1,
                PgLogAutovacuumV1::encode(&[PgLogAutovacuumV1 {
                    ts: Ts(1_300),
                    kind: 0,
                    relation: Some(relation),
                    index_scans: Some(1),
                    pages_removed: Some(2),
                    pages_remaining: Some(3),
                    tuples_removed: Some(4),
                    tuples_remaining: Some(5),
                    tuples_dead_not_removable: Some(6),
                    elapsed_ms: Some(7.0),
                    buffer_hits: Some(8),
                    buffer_misses: Some(9),
                    buffer_dirtied: Some(10),
                    avg_read_rate_mbs: Some(11.0),
                    avg_write_rate_mbs: Some(12.0),
                    cpu_user_ms: Some(13.0),
                    cpu_system_ms: Some(14.0),
                    wal_records: Some(15),
                    wal_fpi: Some(16),
                    wal_bytes: Some(17),
                    dict_dropped_fields: 0,
                }])
                .expect("encode autovacuum"),
            ),
            (
                1_026_001,
                1,
                PgLogSlowQueryV1::encode(&[PgLogSlowQueryV1 {
                    ts: Ts(1_400),
                    pattern: Some(slow_pattern),
                    sample: Some(slow_sample),
                    count: 2,
                    max_duration_ms: 10.0,
                    total_duration_ms: 15.0,
                    dict_dropped_fields: 0,
                }])
                .expect("encode slow query"),
            ),
            (
                1_027_001,
                1,
                PgLogLockWaitV1::encode(&[PgLogLockWaitV1 {
                    ts: Ts(1_500),
                    kind: 1,
                    pid: Some(42),
                    lock_mode: Some(lock_mode),
                    lock_target: Some(lock_target),
                    duration_ms: Some(250.0),
                    detail: None,
                    context: None,
                    statement: None,
                    dict_dropped_fields: 0,
                }])
                .expect("encode lock wait"),
            ),
            (
                1_028_001,
                1,
                PgLogLifecycleV1::encode(&[PgLogLifecycleV1 {
                    ts: Ts(1_600),
                    kind: 0,
                    pid: Some(43),
                    signal: Some(9),
                    shutdown_mode: None,
                    message: Some(lifecycle_message),
                    query_detail: None,
                    dict_dropped_fields: 0,
                }])
                .expect("encode lifecycle"),
            ),
            (
                1_029_001,
                1,
                PgLogGapV1::encode(&[PgLogGapV1 {
                    ts: Ts(1_700),
                    source_path: Some(source_path),
                    parser_kind: 0,
                    reason: 2,
                    dev: Some(1),
                    inode: Some(2),
                    offset: Some(3),
                    bytes_skipped: 4,
                    truncated_lines: 0,
                    invalid_utf8: 1,
                    binary_dropped: 0,
                    rotations: 0,
                    missing_files: 0,
                    budget_exhaustions: 0,
                    dict_dropped_fields: 0,
                    parser_dropped_lines: 2,
                }])
                .expect("encode gap"),
            ),
            (
                1_030_001,
                1,
                PgLogTempFileV1::encode(&[PgLogTempFileV1 {
                    ts: Ts(1_800),
                    path: Some(temp_path),
                    size_bytes: 4_096,
                    statement: None,
                    dict_dropped_fields: 0,
                }])
                .expect("encode temp file"),
            ),
            (
                1_006_001,
                0,
                BgwriterCheckpointer::encode(&[]).expect("encode unrelated section"),
            ),
        ];
        sections.extend(
            dict::encode(interner.window())
                .expect("encode dictionary")
                .into_iter()
                .map(|section| (section.type_id, section.rows, section.body)),
        );
        let inputs: Vec<_> = sections
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        build_part(
            &inputs,
            PartMeta {
                min_ts: 1_100,
                max_ts: 1_800,
                source_id: 7,
            },
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the fixture enumerates every event sub-kind and gap reason in one PGM"
    )]
    fn all_event_variants_pgm() -> Vec<u8> {
        let errors: Vec<_> = (0_u8..=10)
            .map(|category| PgLogErrorV1 {
                ts: Ts(2_000 + i64::from(category)),
                severity: 0,
                category,
                sqlstate: None,
                pattern: None,
                count: 1,
                sample: None,
                detail: None,
                hint: None,
                context: None,
                statement: None,
                database: None,
                username: None,
                dict_dropped_fields: 0,
            })
            .collect();
        let checkpoints: Vec<_> = (0_u8..=2)
            .map(|phase| PgLogCheckpointV1 {
                ts: Ts(3_000 + i64::from(phase)),
                phase,
                reason: None,
                seconds_apart: None,
                buffers_written: None,
                write_ms: None,
                sync_ms: None,
                total_ms: None,
                distance_kb: None,
                estimate_kb: None,
                wal_added: None,
                wal_removed: None,
                wal_recycled: None,
                sync_files: None,
                longest_sync_ms: None,
                average_sync_ms: None,
                dict_dropped_fields: 0,
            })
            .collect();
        let maintenance: Vec<_> = (0_u8..=1)
            .map(|kind| PgLogAutovacuumV1 {
                ts: Ts(4_000 + i64::from(kind)),
                kind,
                relation: None,
                index_scans: None,
                pages_removed: None,
                pages_remaining: None,
                tuples_removed: None,
                tuples_remaining: None,
                tuples_dead_not_removable: None,
                elapsed_ms: None,
                buffer_hits: None,
                buffer_misses: None,
                buffer_dirtied: None,
                avg_read_rate_mbs: None,
                avg_write_rate_mbs: None,
                cpu_user_ms: None,
                cpu_system_ms: None,
                wal_records: None,
                wal_fpi: None,
                wal_bytes: None,
                dict_dropped_fields: 0,
            })
            .collect();
        let slow_queries = [PgLogSlowQueryV1 {
            ts: Ts(5_000),
            pattern: None,
            sample: None,
            count: 1,
            max_duration_ms: 5.0,
            total_duration_ms: 5.0,
            dict_dropped_fields: 0,
        }];
        let lock_waits: Vec<_> = (0_u8..=1)
            .map(|kind| PgLogLockWaitV1 {
                ts: Ts(6_000 + i64::from(kind)),
                kind,
                pid: Some(42),
                lock_mode: None,
                lock_target: None,
                duration_ms: None,
                detail: None,
                context: None,
                statement: None,
                dict_dropped_fields: 0,
            })
            .collect();
        let lifecycle = [
            lifecycle_row(7_000, 0, Some(42), Some(9)),
            lifecycle_row(7_001, 0, Some(43), None),
            lifecycle_row(7_002, 1, None, None),
            lifecycle_row(7_003, 2, None, None),
        ];
        let gaps: Vec<_> = (0_u8..=15)
            .map(|reason| PgLogGapV1 {
                ts: Ts(8_000 + i64::from(reason)),
                source_path: None,
                parser_kind: reason % 3,
                reason,
                dev: Some(1),
                inode: Some(2),
                offset: Some(3),
                bytes_skipped: 4,
                truncated_lines: 0,
                invalid_utf8: u32::from(reason == 2),
                binary_dropped: if reason == 3 { 2 } else { 0 },
                rotations: 0,
                missing_files: 0,
                budget_exhaustions: 0,
                dict_dropped_fields: 0,
                parser_dropped_lines: match reason {
                    2 => 1,
                    10 => 3,
                    _ => 0,
                },
            })
            .collect();
        let temp_files = [PgLogTempFileV1 {
            ts: Ts(9_000),
            path: None,
            size_bytes: 1,
            statement: None,
            dict_dropped_fields: 0,
        }];

        let sections = [
            (
                1_022_001,
                row_count(&errors),
                PgLogErrorV1::encode(&errors).expect("encode errors"),
            ),
            (
                1_024_001,
                row_count(&checkpoints),
                PgLogCheckpointV1::encode(&checkpoints).expect("encode checkpoints"),
            ),
            (
                1_025_001,
                row_count(&maintenance),
                PgLogAutovacuumV1::encode(&maintenance).expect("encode maintenance"),
            ),
            (
                1_026_001,
                row_count(&slow_queries),
                PgLogSlowQueryV1::encode(&slow_queries).expect("encode slow queries"),
            ),
            (
                1_027_001,
                row_count(&lock_waits),
                PgLogLockWaitV1::encode(&lock_waits).expect("encode lock waits"),
            ),
            (
                1_028_001,
                row_count(&lifecycle),
                PgLogLifecycleV1::encode(&lifecycle).expect("encode lifecycle"),
            ),
            (
                1_029_001,
                row_count(&gaps),
                PgLogGapV1::encode(&gaps).expect("encode gaps"),
            ),
            (
                1_030_001,
                row_count(&temp_files),
                PgLogTempFileV1::encode(&temp_files).expect("encode temp files"),
            ),
        ];
        let inputs: Vec<_> = sections
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        build_part(
            &inputs,
            PartMeta {
                min_ts: 2_000,
                max_ts: 9_000,
                source_id: 7,
            },
        )
    }

    #[test]
    fn extracts_registered_log_event_layouts_once_with_conservative_quality() {
        let bytes = all_log_event_types_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        let facts = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("extract");
        assert_eq!(facts.observations().len(), 8);
        let kinds: std::collections::BTreeSet<_> = facts
            .observations()
            .iter()
            .map(|observation| observation.payload().kind_code())
            .collect();
        assert_eq!(kinds.len(), 8);

        let error = facts
            .observations()
            .iter()
            .find(|observation| observation.source_type_id() == 1_022_001)
            .expect("error observation");
        assert_eq!(error.occurrence_count(), 3);
        assert_eq!(
            error.time().quality,
            TimeQuality::ParsedWithoutVerifiedOffset
        );
        assert_eq!(error.evidence_quality(), EvidenceQuality::Heuristic);
        match error.payload() {
            ObservationPayload::ErrorGroup(payload) => {
                assert_eq!(payload.sqlstate, Some(SqlState(*b"40P01")));
            }
            _ => panic!("error row must produce an error-group payload"),
        }

        let slow = facts
            .observations()
            .iter()
            .find(|observation| observation.source_type_id() == 1_026_001)
            .expect("slow-query observation");
        assert_eq!(slow.time().quality, TimeQuality::MaxDurationSample);
        assert_eq!(slow.occurrence_count(), 2);

        let gap = facts
            .observations()
            .iter()
            .find(|observation| observation.source_type_id() == 1_029_001)
            .expect("gap observation");
        assert_eq!(gap.time().quality, TimeQuality::IntervalOnly);
        assert_eq!(gap.evidence_quality(), EvidenceQuality::DerivedExact);
        assert_eq!(
            gap.time().observed_interval,
            CoverageSpan::new(1_700, 1_701)
        );
        assert_eq!(
            facts.loss_coverage().retained_exactness(),
            RetainedExactness::Exact
        );
        assert_eq!(facts.loss_coverage().dropped_lower_bound(), 2);
        assert_eq!(
            facts.loss_coverage().known_gaps().spans(),
            &[CoverageSpan::new(1_700, 1_701).expect("gap span")]
        );

        let expected_body_reads = unit
            .catalog()
            .entries
            .iter()
            .filter(|entry| {
                ((1_022_001..=1_030_001).contains(&entry.type_id) && entry.type_id != 1_023_001)
                    || matches!(entry.type_id, 3_001_001 | 3_002_001)
            })
            .count() as u64;
        assert_eq!(unit.body_read_stats().read_calls, expected_body_reads);
        assert_eq!(
            facts
                .manifest_entries()
                .iter()
                .filter(|entry| entry.section_body_id.is_some())
                .count(),
            8
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table-style assertion covers every event sub-kind and gap disposition"
    )]
    fn subkinds_gap_dispositions_and_timestamp_fallback_are_preserved() {
        let bytes = all_event_variants_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        let facts = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("extract");
        assert_eq!(facts.observations().len(), 40);

        let categories: Vec<_> = facts
            .observations()
            .iter()
            .filter_map(|observation| match observation.payload() {
                ObservationPayload::ErrorGroup(payload) => Some(payload.category),
                _ => None,
            })
            .collect();
        assert_eq!(
            categories,
            vec![
                ErrorCategory::Lock,
                ErrorCategory::Constraint,
                ErrorCategory::Serialization,
                ErrorCategory::Timeout,
                ErrorCategory::Resource,
                ErrorCategory::DataCorruption,
                ErrorCategory::System,
                ErrorCategory::Connection,
                ErrorCategory::Auth,
                ErrorCategory::Syntax,
                ErrorCategory::Other,
            ]
        );

        let payload_count = |matches_payload: fn(&ObservationPayload) -> bool| {
            facts
                .observations()
                .iter()
                .filter(|observation| matches_payload(observation.payload()))
                .count()
        };
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::CheckpointStarted(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::CheckpointCompleted(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(
                payload,
                ObservationPayload::CheckpointTooFrequent(_)
            )),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::AutovacuumReported(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::AutoanalyzeReported(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::LockWaitReported(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(
                payload,
                ObservationPayload::LockAcquiredAfterWait(_)
            )),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(
                payload,
                ObservationPayload::ChildSignalTermination(_)
            )),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::ChildProcessCrash(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::ShutdownRequested(_))),
            1
        );
        assert_eq!(
            payload_count(|payload| matches!(payload, ObservationPayload::ReadyObserved(_))),
            1
        );

        let mut parser_kinds = std::collections::BTreeSet::new();
        let mut gap_reasons = std::collections::BTreeSet::new();
        for observation in facts.observations() {
            if let ObservationPayload::LogGap(payload) = observation.payload() {
                parser_kinds.insert(payload.parser_kind);
                gap_reasons.insert(payload.reason);
                let expected_reasons = match payload.reason {
                    2 | 10 => vec![LossReason::ParserBound],
                    9 => vec![LossReason::DictionaryBound],
                    11..=13 | 15 => Vec::new(),
                    _ => vec![LossReason::TailerBound],
                };
                let actual_reasons = observation.loss().map_or(&[][..], |loss| loss.reasons());
                assert_eq!(
                    actual_reasons,
                    expected_reasons.as_slice(),
                    "gap reason {}",
                    payload.reason
                );
            } else {
                assert_eq!(observation.time().quality, TimeQuality::CollectionFallback);
                assert_eq!(observation.time().occurred_at_us, None);
                let expected_evidence =
                    if matches!(observation.payload(), ObservationPayload::ErrorGroup(_)) {
                        EvidenceQuality::Heuristic
                    } else {
                        EvidenceQuality::Parsed
                    };
                assert_eq!(observation.evidence_quality(), expected_evidence);
            }
        }
        assert_eq!(parser_kinds, std::collections::BTreeSet::from([0, 1, 2]));
        assert_eq!(
            gap_reasons,
            (0_u8..=15).collect::<std::collections::BTreeSet<_>>()
        );
        assert_eq!(facts.loss_coverage().dropped_lower_bound(), 6);
        assert_eq!(
            facts.loss_coverage().retained_exactness(),
            RetainedExactness::Exact
        );
    }

    #[test]
    fn extraction_rejects_item_count_above_caller_bound() {
        let bytes = all_log_event_types_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        let tight = Bounds {
            items_per_block: 4,
            ..LIMIT
        };
        assert!(matches!(
            SegmentFacts::extract(&unit, &context(), &tight),
            Err(BuildError::LimitExceeded)
        ));
    }

    #[test]
    fn catalog_bound_is_checked_before_any_section_body_read() {
        let body = PgLogLifecycleV1::encode(&[]).expect("encode empty lifecycle section");
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: 1_028_001,
                    rows: 0,
                    body: &body,
                },
                SectionInput {
                    type_id: 1_028_001,
                    rows: 0,
                    body: &body,
                },
            ],
            PartMeta {
                min_ts: 1_000,
                max_ts: 1_000,
                source_id: 7,
            },
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        let tight = Bounds {
            directory_entries: 1,
            ..LIMIT
        };
        assert!(matches!(
            SegmentFacts::extract(&unit, &context(), &tight),
            Err(BuildError::LimitExceeded)
        ));
        assert_eq!(unit.body_read_stats(), PgmBodyReadStats::default());
    }

    #[test]
    fn extraction_reports_operation_local_body_reads() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        unit.decode_overview_rows(0)
            .expect("independent section read");
        let before = unit.body_read_stats();
        let (_facts, local) =
            SegmentFacts::extract_with_stats(&unit, &context(), &LIMIT).expect("extract");
        assert_eq!(local.read_calls, 1);
        assert_eq!(local.stored_bytes_read, unit.catalog().entries[0].len);
        assert_eq!(
            unit.body_read_stats(),
            PgmBodyReadStats {
                read_calls: before.read_calls + local.read_calls,
                stored_bytes_read: before.stored_bytes_read + local.stored_bytes_read,
            }
        );
    }

    #[test]
    fn unresolved_dictionary_reference_rejects_source() {
        let body = PgLogErrorV1::encode(&[PgLogErrorV1 {
            ts: Ts(1_100),
            severity: 0,
            category: 0,
            sqlstate: Some(StrId(999)),
            pattern: None,
            count: 1,
            sample: None,
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: None,
            username: None,
            dict_dropped_fields: 0,
        }])
        .expect("encode error");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_022_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_100,
                max_ts: 1_100,
                source_id: 7,
            },
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
        assert!(matches!(
            SegmentFacts::extract(&unit, &context(), &LIMIT),
            Err(BuildError::Source(SourceError::Corrupt))
        ));
    }

    #[test]
    fn extract_materializes_one_observation_per_lifecycle_row() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("extract");
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
    fn fact_file_reload_matches_forced_raw_decode() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("raw extract");
        let encoded = raw.encode(&LIMIT).expect("encode fact file");
        let index = SegmentFacts::from_bytes(
            &encoded,
            raw.identity(),
            raw.lineage(),
            &raw.catalog_descriptors(),
            &LIMIT,
        )
        .expect("admit");
        let divergences =
            semantic_divergences(&index, &raw, full_range(), LIMITS).expect("bounded comparison");
        assert_eq!(divergences, Vec::<SemanticDivergence>::new());
    }

    #[test]
    fn retained_observations_survive_fact_file_round_trip() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("raw extract");
        let encoded = raw.encode(&LIMIT).expect("encode fact file");
        let index = SegmentFacts::from_bytes(
            &encoded,
            raw.identity(),
            raw.lineage(),
            &raw.catalog_descriptors(),
            &LIMIT,
        )
        .expect("admit");
        assert_eq!(index.observations(), raw.observations());
        assert_eq!(index.coverage(), raw.coverage());
        assert_eq!(index.manifest_entries(), raw.manifest_entries());
        let index_result = index.query(full_range(), LIMITS).expect("index query");
        let raw_result = raw.query(full_range(), LIMITS).expect("raw query");
        assert_eq!(index_result.counts(), raw_result.counts());
    }

    #[test]
    fn forced_recompute_matches_derived_answer() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let derived = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("first build");
        let recomputed =
            SegmentFacts::extract(&unit, &context(), &LIMIT).expect("forced recompute");
        let divergences = semantic_divergences(&derived, &recomputed, full_range(), LIMITS)
            .expect("bounded comparison");
        assert!(divergences.is_empty());
    }

    #[test]
    fn range_slices_partition_retained_facts_without_double_counting() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("extract");

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
    fn positional_reload_requires_no_pgm_source() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("raw extract");
        let encoded = raw.encode(&LIMIT).expect("encode fact file");

        // The positional reader is handed only the fact-file bytes; it never has
        // a handle to the PGM, so a successful reload proves the sealed interior
        // is served without any PGM body read.
        let warm = SegmentFacts::from_reader(
            encoded.as_slice(),
            raw.identity(),
            raw.lineage(),
            &raw.catalog_descriptors(),
            &LIMIT,
        )
        .expect("positional reload");
        assert_eq!(warm.observations(), raw.observations());
        assert_eq!(warm.coverage(), raw.coverage());
    }

    #[test]
    fn store_namespace_changes_identity_not_counts() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let here = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("here");
        let elsewhere = SegmentFacts::extract(
            &unit,
            &SegmentContext::new(
                b"store-b".to_vec(),
                NamingContractId([0x33; 16]),
                SegmentLocator([0x44; 32]),
            )
            .expect("valid context"),
            &LIMIT,
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
    fn corrupt_fact_buffer_returns_typed_error() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("raw extract");
        let mut encoded = raw.encode(&LIMIT).expect("encode fact file");
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;
        let outcome = SegmentFacts::from_bytes(
            &encoded,
            raw.identity(),
            raw.lineage(),
            &raw.catalog_descriptors(),
            &LIMIT,
        );
        assert!(matches!(
            outcome,
            Err(CacheReadError::Corrupt | CacheReadError::Incompatible | CacheReadError::Oversized)
        ));
    }

    #[test]
    fn lifecycle_events_fold_into_lifecycle_counts() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context(), &LIMIT).expect("extract");
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
