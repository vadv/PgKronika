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

use std::collections::{BTreeMap, BTreeSet as DictionaryIdSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

use kronika_analytics::overview::{
    Applicability, BoundaryQuality, CounterSample, Coverage, CoverageSpan, CoverageState,
    EventFact, EventObservation, FactorCoverage, FactorId, GaugeSample, MetricSeriesDescriptor,
    MetricSeriesId, MetricUnit, ObservationId, ObservationPayload, ObservationProvenance,
    OracleError, OracleLimits, OracleResult, PeriodQuality, PhysicalCountSemantics,
    PopulationTotalQuality, RawOracle, RetainedExactness, SegmentIdentity, SourceCompleteness,
    SourcePopulation, query_bounded,
};
use kronika_format::ReadAt;

use crate::unit::PgmUnit;
use crate::{PgmBodyReadStats, ReadError};

use super::block::{
    BlockKind, CounterSamplesBlock, EntityStateRecord, EntityStatesBlock, GaugeSamplesBlock,
    LossCoverageBlock, ResetMarker, ResetMarkersBlock, SourceManifestBlock, StringTableBlock,
};
use super::container::{
    BlockContent, CacheReadError, FactFile, FactFileReader, FactReadStats, HeaderIdentity,
    validate_block_descriptor, validate_observation_provenance, verify_manifest_identity,
};
use super::descriptors::{CatalogEntryDescriptor, ManifestEntryDescriptor};
use super::event_extract::{
    DictionaryFingerprint, EventExtraction, TIMESTAMP_FALLBACK_GAP_REASON, extract_events,
    fingerprint_dictionary,
};
use super::event_facts::EventFactsBlock;
use super::limits::Bounds;
use super::metric_extract::{
    MetricExtraction, cadence_covered_duration, extract_metrics, observed_cadence,
};
use super::observations::EventObservationsBlock;

/// Filesystem address of one sealed PGM and its sibling overview sidecar.
#[derive(Debug, Clone)]
pub struct SegmentContext {
    pgm_file_name: OsString,
    sidecar_file_name: OsString,
}

/// Invalid direct-child PGM filename for a sibling sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentContextError {
    /// The value is not one direct-child filename ending in `.pgm`.
    InvalidPgmFileName,
}

impl std::fmt::Display for SegmentContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPgmFileName => {
                f.write_str("PGM filename must be one direct child ending in .pgm")
            }
        }
    }
}

impl std::error::Error for SegmentContextError {}

impl SegmentContext {
    /// Derives the sibling `.ovf` name from one direct-child `.pgm` name.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentContextError`] unless the value is a safe direct-child
    /// filename with a non-empty stem and exact `.pgm` extension.
    pub fn new(pgm_file_name: impl Into<OsString>) -> Result<Self, SegmentContextError> {
        let pgm_file_name = pgm_file_name.into();
        let bytes = pgm_file_name.as_bytes();
        if bytes.len() <= 4
            || !bytes.ends_with(b".pgm")
            || bytes.contains(&b'/')
            || bytes.contains(&0)
            || bytes == b"."
            || bytes == b".."
        {
            return Err(SegmentContextError::InvalidPgmFileName);
        }
        let mut sidecar = bytes[..bytes.len() - 4].to_vec();
        sidecar.extend_from_slice(b".ovf");
        Ok(Self {
            pgm_file_name,
            sidecar_file_name: OsString::from_vec(sidecar),
        })
    }

    /// Direct-child PGM filename.
    #[must_use]
    pub fn pgm_file_name(&self) -> &OsStr {
        &self.pgm_file_name
    }

    /// Direct-child sibling OVF filename with the same stem.
    #[must_use]
    pub fn sidecar_file_name(&self) -> &OsStr {
        &self.sidecar_file_name
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
            | ReadError::NonCanonicalCatalog { .. }
            | ReadError::UnknownType { .. }
            | ReadError::TooManyCatalogEntries { .. }
            | ReadError::Codec(_)
            | ReadError::DictionaryConflict { .. }
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentFacts {
    identity: HeaderIdentity,
    lineage: SegmentIdentity,
    manifest_entries: Vec<ManifestEntryDescriptor>,
    observations: Vec<EventObservation>,
    event_facts: Vec<EventFact>,
    counter_samples: CounterSamplesBlock,
    gauge_samples: GaugeSamplesBlock,
    reset_markers: ResetMarkersBlock,
    entity_states: EntityStatesBlock,
    loss_coverage: LossCoverageBlock,
    retained_text_bytes: u64,
    dictionary_fingerprints: Vec<DictionaryFingerprint>,
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
    pub fn extract<R: ReadAt>(unit: &PgmUnit<R>, bounds: &Bounds) -> Result<Self, BuildError> {
        Self::extract_with_stats(unit, bounds).map(|(facts, _stats)| facts)
    }

    pub(super) fn extract_with_stats<R: ReadAt>(
        unit: &PgmUnit<R>,
        bounds: &Bounds,
    ) -> Result<(Self, PgmBodyReadStats), BuildError> {
        let (min_ts, max_ts) = (unit.catalog().min_ts, unit.catalog().max_ts);
        let (identity, lineage) = Self::provenance(unit)?;
        let mut extracted = extract_events(unit, lineage, bounds)?;
        let metrics = extract_metrics(
            unit,
            identity.pgm_source_id,
            segment_span(min_ts, max_ts)?,
            bounds,
        )?;
        apply_descriptor_replacements(&mut extracted, &metrics)?;
        let pgm_body_read_stats =
            checked_add_read_stats(extracted.pgm_body_read_stats, metrics.pgm_body_read_stats)?;
        let facts = Self::assemble(
            identity, lineage, extracted, metrics, min_ts, max_ts, bounds,
        )?;
        Ok((facts, pgm_body_read_stats))
    }

    /// Folds one completed active part into live overview facts.
    ///
    /// Live facts carry an `Approximate` lineage keyed by journal generation and
    /// a per-part discriminator, so identical sections in different parts stay
    /// distinct and no observation collides. The lineage never claims a sealed
    /// locator; a seal that matches provenance re-keys these facts to the sealed
    /// identity. Reads each supported event body once.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] for source failures, unsupported event layouts,
    /// unsafe work bounds, or checked-arithmetic overflow.
    pub fn fold_live<R: ReadAt>(
        unit: &PgmUnit<R>,
        journal_generation: u64,
        part_discriminator: &[u8],
        bounds: &Bounds,
    ) -> Result<Self, BuildError> {
        let catalog = unit.catalog();
        if catalog.entries.is_empty() {
            return Err(BuildError::Source(SourceError::UnsupportedLayout));
        }
        let lineage = SegmentIdentity::live_approximate(
            catalog.source_id,
            journal_generation,
            part_discriminator,
        );
        let identity = HeaderIdentity::from_current_contract(
            catalog.format_version,
            catalog.source_id,
            catalog.min_ts,
            catalog.max_ts,
            unit.source_file_len(),
            unit.source_descriptor(),
            lineage.id(),
        );
        let (min_ts, max_ts) = (catalog.min_ts, catalog.max_ts);
        let mut extracted = extract_events(unit, lineage, bounds)?;
        let metrics = extract_metrics(
            unit,
            identity.pgm_source_id,
            segment_span(min_ts, max_ts)?,
            bounds,
        )?;
        apply_descriptor_replacements(&mut extracted, &metrics)?;
        Self::assemble(
            identity, lineage, extracted, metrics, min_ts, max_ts, bounds,
        )
    }

    /// Promotes matching live parts without rereading sealed event bodies.
    ///
    /// The parts must be the ordered constituents of the sealed segment. Their
    /// catalogs, source identity, timestamp envelope, and referenced dictionary
    /// values must match the sealed PGM. Dictionary
    /// sections are read when rows contain references. A match re-keys retained
    /// observations to sealed provenance; a mismatch returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] when the sealed catalog is empty, a re-keyed
    /// observation is invalid, or a checked counter overflows.
    pub(super) fn try_promote_from_parts<R: ReadAt>(
        sealed_unit: &PgmUnit<R>,
        parts: &[&Self],
        bounds: &Bounds,
    ) -> Result<Option<Self>, BuildError> {
        if parts.is_empty() {
            return Ok(None);
        }
        if !bounds.is_within_absolute_limits()
            || u64::try_from(sealed_unit.catalog().entries.len())
                .map_err(|_error| BuildError::Overflow)?
                > u64::from(bounds.directory_entries)
        {
            return Err(BuildError::LimitExceeded);
        }
        if !promotion_catalogs_match(sealed_unit, parts) || parts_have_timestamp_fallback(parts) {
            return Ok(None);
        }

        let (identity, lineage) = Self::provenance(sealed_unit)?;
        if !promotion_source_matches(identity, parts) {
            return Ok(None);
        }
        let Some(dictionary_fingerprints) = promotion_dictionary(sealed_unit, parts, bounds)?
        else {
            return Ok(None);
        };
        let Some(extracted) =
            rekey_promoted_parts(parts, lineage, dictionary_fingerprints, bounds)?
        else {
            return Ok(None);
        };

        let (min_ts, max_ts) = (sealed_unit.catalog().min_ts, sealed_unit.catalog().max_ts);
        let Some(metrics) = promote_metrics(
            parts,
            identity.pgm_source_id,
            segment_span(min_ts, max_ts)?,
            bounds,
        )?
        else {
            return Ok(None);
        };
        Self::assemble(
            identity, lineage, extracted, metrics, min_ts, max_ts, bounds,
        )
        .map(Some)
    }

    /// Assembles canonical facts from an extraction and the catalog time range.
    fn assemble(
        identity: HeaderIdentity,
        lineage: SegmentIdentity,
        extracted: EventExtraction,
        metrics: MetricExtraction,
        min_ts: i64,
        max_ts: i64,
        bounds: &Bounds,
    ) -> Result<Self, BuildError> {
        let mut event_facts = extracted
            .observations
            .iter()
            .filter_map(|observation| EventFact::from_observation(observation).transpose())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_error| BuildError::Internal)?;
        event_facts.extend(metrics.event_facts.iter().cloned());
        let covered = segment_coverage(min_ts, max_ts)?;
        let counter_samples =
            CounterSamplesBlock::new_with_series(metrics.counter_series, metrics.counters, bounds)
                .map_err(block_build_error)?;
        let gauge_samples =
            GaugeSamplesBlock::new_with_series(metrics.gauge_series, metrics.gauges, bounds)
                .map_err(block_build_error)?;
        let reset_markers =
            ResetMarkersBlock::new(metrics.reset_markers, bounds).map_err(block_build_error)?;
        let entity_states =
            EntityStatesBlock::new(metrics.entity_states, bounds).map_err(block_build_error)?;
        event_facts.extend(derive_metric_event_facts(
            &counter_samples,
            &gauge_samples,
            &entity_states,
            &extracted.known_gaps,
            bounds,
        )?);
        event_facts.sort_by(EventFact::canonical_cmp);
        if has_duplicate_fact_id(&event_facts) {
            return Err(BuildError::Internal);
        }
        let loss_coverage = LossCoverageBlock::new_with_factors(
            covered,
            extracted.known_gaps,
            Applicability::Applicable,
            PeriodQuality::Unknown,
            SourceCompleteness::BoundedSubset,
            RetainedExactness::Exact,
            PhysicalCountSemantics::LowerBound,
            extracted.dropped_lower_bound,
            metrics.factor_coverage,
            bounds,
        )
        .map_err(|error| match error {
            super::block::BlockError::AboveBound => BuildError::LimitExceeded,
            _ => BuildError::Internal,
        })?;
        Ok(Self {
            identity,
            lineage,
            manifest_entries: extracted.manifest_entries,
            observations: extracted.observations,
            event_facts,
            counter_samples,
            gauge_samples,
            reset_markers,
            entity_states,
            loss_coverage,
            retained_text_bytes: extracted.retained_text_bytes,
            dictionary_fingerprints: extracted.dictionary_fingerprints,
        })
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
    ) -> Result<(HeaderIdentity, SegmentIdentity), BuildError> {
        let catalog = unit.catalog();
        let first = catalog
            .entries
            .first()
            .ok_or(BuildError::Source(SourceError::UnsupportedLayout))?;
        let lineage = SegmentIdentity::sealed(
            catalog.source_id,
            unit.source_descriptor().0,
            first.type_id,
            &CatalogEntryDescriptor::of(first).canonical_bytes(),
        );
        let identity = HeaderIdentity::from_current_contract(
            catalog.format_version,
            catalog.source_id,
            catalog.min_ts,
            catalog.max_ts,
            unit.source_file_len(),
            unit.source_descriptor(),
            lineage.id(),
        );
        Ok((identity, lineage))
    }

    /// Header identity carrying source provenance and contract versions.
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

    /// Policy-neutral canonical event facts in canonical order.
    #[must_use]
    pub fn event_facts(&self) -> &[EventFact] {
        &self.event_facts
    }

    /// Typed cumulative samples retained for reset-aware deltas.
    #[must_use]
    pub const fn counter_samples(&self) -> &CounterSamplesBlock {
        &self.counter_samples
    }

    /// Typed instantaneous samples retained for gauge cells and state inputs.
    #[must_use]
    pub const fn gauge_samples(&self) -> &GaugeSamplesBlock {
        &self.gauge_samples
    }

    /// Counter epoch boundaries retained independently of sample selection.
    #[must_use]
    pub const fn reset_markers(&self) -> &ResetMarkersBlock {
        &self.reset_markers
    }

    /// Bounded complete-population entity snapshots.
    #[must_use]
    pub const fn entity_states(&self) -> &EntityStatesBlock {
        &self.entity_states
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

    /// Retained UTF-8 payload bytes.
    #[must_use]
    pub const fn retained_text_bytes(&self) -> u64 {
        self.retained_text_bytes
    }

    /// Checked logical bytes owned by this materialized fact set.
    ///
    /// The count includes the inline value, every reserved vector slot,
    /// concrete observation payload boxes and their retained text, loss-reason
    /// storage, coverage/gap spans, and dictionary fingerprints. Returns
    /// `None` rather than saturating if a platform-sized total overflows.
    #[must_use]
    pub fn resident_bytes(&self) -> Option<usize> {
        let manifest = self
            .manifest_entries
            .capacity()
            .checked_mul(size_of::<ManifestEntryDescriptor>())?;
        let observation_slots = self
            .observations
            .capacity()
            .checked_mul(size_of::<EventObservation>())?;
        let observation_heap = self
            .observations
            .iter()
            .try_fold(0_usize, |total, observation| {
                total.checked_add(observation.resident_heap_bytes()?)
            })?;
        let event_fact_slots = self
            .event_facts
            .capacity()
            .checked_mul(size_of::<EventFact>())?;
        let event_fact_heap = self.event_facts.iter().try_fold(0_usize, |total, fact| {
            total.checked_add(fact.resident_heap_bytes()?)
        })?;
        let dictionary = self
            .dictionary_fingerprints
            .capacity()
            .checked_mul(size_of::<DictionaryFingerprint>())?;
        let counter_samples = self.counter_samples.resident_heap_bytes()?;
        let gauge_samples = self.gauge_samples.resident_heap_bytes()?;
        let resets = self.reset_markers.resident_heap_bytes()?;
        let entity_states = self.entity_states.resident_heap_bytes()?;
        let factor_coverage = self.loss_coverage.resident_factor_bytes()?;

        size_of::<Self>()
            .checked_add(manifest)?
            .checked_add(observation_slots)?
            .checked_add(observation_heap)?
            .checked_add(event_fact_slots)?
            .checked_add(event_fact_heap)?
            .checked_add(self.loss_coverage.covered().resident_heap_bytes()?)?
            .checked_add(self.loss_coverage.known_gaps().resident_heap_bytes()?)?
            .checked_add(counter_samples)?
            .checked_add(gauge_samples)?
            .checked_add(resets)?
            .checked_add(entity_states)?
            .checked_add(factor_coverage)?
            .checked_add(dictionary)
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
        let event_facts = EventFactsBlock::new(self.event_facts.clone(), &strings, bounds)?;
        FactFile::build(
            &self.identity,
            vec![
                BlockContent::SourceManifest(Box::new(manifest)),
                BlockContent::StringTable(Box::new(strings)),
                BlockContent::EventObservations(Box::new(observations)),
                BlockContent::EventFacts(Box::new(event_facts)),
                BlockContent::LossCoverage(Box::new(self.loss_coverage.clone())),
                BlockContent::GaugeSamples(Box::new(self.gauge_samples.clone())),
                BlockContent::CounterSamples(Box::new(self.counter_samples.clone())),
                BlockContent::ResetMarkers(Box::new(self.reset_markers.clone())),
                BlockContent::EntityStates(Box::new(self.entity_states.clone())),
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
        if lineage.id() != expected.segment_lineage_id {
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

        let (facts_entry, facts_body) = singleton_body(&mut fact_reader, BlockKind::EventFacts)?;
        let event_facts = EventFactsBlock::decode(&facts_body, &strings, bounds)?;
        validate_block_descriptor(&facts_entry, &event_facts)?;

        let (counter_entry, counter_body) =
            singleton_body(&mut fact_reader, BlockKind::CounterSamples)?;
        let counter_samples = CounterSamplesBlock::decode(&counter_body, bounds)?;
        validate_block_descriptor(&counter_entry, &counter_samples)?;
        validate_metric_source(counter_samples.series(), expected.pgm_source_id)?;

        let (gauge_entry, gauge_body) = singleton_body(&mut fact_reader, BlockKind::GaugeSamples)?;
        let gauge_samples = GaugeSamplesBlock::decode(&gauge_body, bounds)?;
        validate_block_descriptor(&gauge_entry, &gauge_samples)?;
        validate_metric_source(gauge_samples.series(), expected.pgm_source_id)?;

        let (reset_entry, reset_body) = singleton_body(&mut fact_reader, BlockKind::ResetMarkers)?;
        let reset_markers = ResetMarkersBlock::decode(&reset_body, bounds)?;
        validate_block_descriptor(&reset_entry, &reset_markers)?;
        validate_reset_series(&reset_markers, &counter_samples)?;

        let (states_entry, states_body) =
            singleton_body(&mut fact_reader, BlockKind::EntityStates)?;
        let entity_states = EntityStatesBlock::decode(&states_body, bounds)?;
        validate_block_descriptor(&states_entry, &entity_states)?;
        validate_state_series(&entity_states, &gauge_samples)?;
        validate_event_fact_evidence(
            &event_facts,
            &observations,
            &counter_samples,
            &gauge_samples,
            expected.pgm_source_id,
        )?;

        let coverage = merge_coverage_blocks(&mut fact_reader, bounds)?;
        let retained_text_bytes = bounds
            .decoded_block_len
            .checked_sub(text_budget)
            .ok_or(CacheReadError::Corrupt)?;
        let stats = fact_reader.stats();
        Ok((
            Self {
                identity: fact_reader.header().identity,
                lineage: *lineage,
                manifest_entries: manifest.entries().to_vec(),
                observations: observations.into_observations(),
                event_facts: event_facts.into_facts(),
                counter_samples,
                gauge_samples,
                reset_markers,
                entity_states,
                loss_coverage: coverage,
                retained_text_bytes,
                dictionary_fingerprints: Vec::new(),
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
        query_bounded(
            &self.observations,
            self.coverage().spans().iter().copied(),
            range,
            limits,
        )
    }
}

fn promotion_catalogs_match<R: ReadAt>(sealed_unit: &PgmUnit<R>, parts: &[&SegmentFacts]) -> bool {
    sealed_unit
        .catalog()
        .entries
        .iter()
        .map(CatalogEntryDescriptor::of)
        .eq(parts
            .iter()
            .flat_map(|part| &part.manifest_entries)
            .map(|entry| entry.catalog))
}

fn parts_have_timestamp_fallback(parts: &[&SegmentFacts]) -> bool {
    parts.iter().any(|part| {
        part.observations.iter().any(|observation| {
            matches!(
                observation.payload(),
                ObservationPayload::LogGap(gap) if gap.reason == TIMESTAMP_FALLBACK_GAP_REASON
            )
        })
    })
}

fn promotion_source_matches(identity: HeaderIdentity, parts: &[&SegmentFacts]) -> bool {
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    for part in parts {
        if part.identity.source_min_ts_us <= part.identity.source_max_ts_us {
            min_ts = min_ts.min(part.identity.source_min_ts_us);
            max_ts = max_ts.max(part.identity.source_max_ts_us);
        }
    }
    if min_ts > max_ts {
        (min_ts, max_ts) = (0, 0);
    }
    min_ts == identity.source_min_ts_us
        && max_ts == identity.source_max_ts_us
        && parts.iter().all(|part| {
            part.identity.source_format_version == identity.source_format_version
                && (part.identity.pgm_source_id == 0
                    || part.identity.pgm_source_id == identity.pgm_source_id)
        })
}

fn promotion_dictionary<R: ReadAt>(
    sealed_unit: &PgmUnit<R>,
    parts: &[&SegmentFacts],
    bounds: &Bounds,
) -> Result<Option<Vec<DictionaryFingerprint>>, BuildError> {
    let mut wanted = DictionaryIdSet::new();
    for fingerprint in parts.iter().flat_map(|part| &part.dictionary_fingerprints) {
        if !wanted.contains(&fingerprint.str_id)
            && u64::try_from(wanted.len()).map_err(|_error| BuildError::Overflow)?
                == bounds.items_per_block
        {
            return Err(BuildError::LimitExceeded);
        }
        wanted.insert(fingerprint.str_id);
    }
    let sealed_dictionary = sealed_unit.resolve_overview_dictionary(&wanted, bounds)?;
    if sealed_dictionary.values.len() != wanted.len() {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    let sealed = fingerprint_dictionary(&sealed_dictionary.values)?;
    let matches = parts.iter().all(|part| {
        part.dictionary_fingerprints.iter().all(|part_value| {
            sealed
                .binary_search_by_key(&part_value.str_id, |value| value.str_id)
                .ok()
                .is_some_and(|index| sealed[index].context_id == part_value.context_id)
        })
    });
    Ok(matches.then_some(sealed))
}

struct PromotionUsage {
    observations: usize,
    manifest_entries: usize,
    known_gaps: usize,
    retained_text_bytes: u64,
}

impl PromotionUsage {
    fn for_parts(parts: &[&SegmentFacts], bounds: &Bounds) -> Result<Self, BuildError> {
        let usage = Self {
            observations: sum_len(parts, |part| part.observations.len())?,
            manifest_entries: sum_len(parts, |part| part.manifest_entries.len())?,
            known_gaps: sum_len(parts, |part| part.loss_coverage.known_gaps().spans().len())?,
            retained_text_bytes: parts.iter().try_fold(0_u64, |bytes, part| {
                bytes
                    .checked_add(part.retained_text_bytes)
                    .ok_or(BuildError::Overflow)
            })?,
        };
        if u64::try_from(usage.observations).map_err(|_error| BuildError::Overflow)?
            > bounds.items_per_block
            || u64::try_from(usage.manifest_entries).map_err(|_error| BuildError::Overflow)?
                > u64::from(bounds.directory_entries)
            || u64::try_from(usage.known_gaps).map_err(|_error| BuildError::Overflow)?
                > bounds.coverage_spans
            || usage.retained_text_bytes > bounds.string_table_bytes
        {
            return Err(BuildError::LimitExceeded);
        }
        Ok(usage)
    }
}

fn sum_len(
    parts: &[&SegmentFacts],
    len: impl Fn(&SegmentFacts) -> usize,
) -> Result<usize, BuildError> {
    parts.iter().try_fold(0_usize, |count, part| {
        count.checked_add(len(part)).ok_or(BuildError::Overflow)
    })
}

fn rekey_promoted_parts(
    parts: &[&SegmentFacts],
    lineage: SegmentIdentity,
    dictionary_fingerprints: Vec<DictionaryFingerprint>,
    bounds: &Bounds,
) -> Result<Option<EventExtraction>, BuildError> {
    let usage = PromotionUsage::for_parts(parts, bounds)?;
    let mut observations = Vec::with_capacity(usage.observations);
    let mut manifest_entries = Vec::with_capacity(usage.manifest_entries);
    let mut known_gap_spans = Vec::with_capacity(usage.known_gaps);
    let mut dropped_lower_bound = 0_u64;
    let mut base_ordinal = 0_u32;
    for part in parts {
        for observation in &part.observations {
            let source = observation.provenance();
            let provenance = ObservationProvenance {
                section_body_id: source.section_body_id,
                catalog_entry_ordinal: base_ordinal
                    .checked_add(source.catalog_entry_ordinal)
                    .ok_or(BuildError::Overflow)?,
                row_ordinal: source.row_ordinal,
                dictionary_context_id: source.dictionary_context_id,
                source_locator: source.source_locator,
            };
            observations.push(
                EventObservation::new(
                    lineage,
                    observation.source_type_id(),
                    provenance,
                    observation.shape(),
                    observation.time(),
                    observation.occurrence_count(),
                    observation.payload().clone(),
                    observation.evidence_quality(),
                    observation.quality_flags(),
                    observation.loss().cloned(),
                )
                .map_err(|_error| BuildError::Internal)?,
            );
        }
        manifest_entries.extend(part.manifest_entries.iter().copied());
        known_gap_spans.extend_from_slice(part.loss_coverage.known_gaps().spans());
        dropped_lower_bound = dropped_lower_bound
            .checked_add(part.loss_coverage.dropped_lower_bound())
            .ok_or(BuildError::Overflow)?;
        base_ordinal = base_ordinal
            .checked_add(
                u32::try_from(part.manifest_entries.len())
                    .map_err(|_error| BuildError::Overflow)?,
            )
            .ok_or(BuildError::Overflow)?;
    }
    observations.sort_by(EventObservation::canonical_cmp);
    if observations
        .windows(2)
        .any(|pair| pair[0].observation_id() == pair[1].observation_id())
    {
        return Ok(None);
    }
    Ok(Some(EventExtraction {
        manifest_entries,
        observations,
        known_gaps: Coverage::from_spans(known_gap_spans),
        dropped_lower_bound,
        pgm_body_read_stats: PgmBodyReadStats::default(),
        retained_text_bytes: usage.retained_text_bytes,
        dictionary_fingerprints,
    }))
}

fn apply_descriptor_replacements(
    events: &mut EventExtraction,
    metrics: &MetricExtraction,
) -> Result<(), BuildError> {
    for (index, replacement) in &metrics.descriptor_replacements {
        let current = events
            .manifest_entries
            .get_mut(*index)
            .ok_or(BuildError::Internal)?;
        if current.catalog != replacement.catalog {
            return Err(BuildError::Internal);
        }
        *current = *replacement;
    }
    Ok(())
}

fn checked_add_read_stats(
    left: PgmBodyReadStats,
    right: PgmBodyReadStats,
) -> Result<PgmBodyReadStats, BuildError> {
    Ok(PgmBodyReadStats {
        read_calls: left
            .read_calls
            .checked_add(right.read_calls)
            .ok_or(BuildError::Overflow)?,
        stored_bytes_read: left
            .stored_bytes_read
            .checked_add(right.stored_bytes_read)
            .ok_or(BuildError::Overflow)?,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the function performs one bounded canonical pass over each metric fact family"
)]
fn derive_metric_event_facts(
    counters: &CounterSamplesBlock,
    gauges: &GaugeSamplesBlock,
    states: &EntityStatesBlock,
    known_gaps: &Coverage,
    bounds: &Bounds,
) -> Result<Vec<EventFact>, BuildError> {
    let mut facts = Vec::new();
    let mut start = 0;
    while start < counters.samples().len() {
        let series_id = counters.samples()[start].series_id();
        let end = start
            + counters.samples()[start..].partition_point(|sample| sample.series_id() == series_id);
        let descriptor = series_descriptor(counters.series(), series_id)?;
        for pair in counters.samples()[start..end].windows(2) {
            if let Some(fact) =
                EventFact::from_counter_pair(descriptor, pair[0], pair[1], known_gaps)
                    .map_err(|_error| BuildError::Internal)?
            {
                push_fact(&mut facts, fact, bounds)?;
            }
        }
        start = end;
    }

    let mut start = 0;
    while start < states.records().len() {
        let series_id = states.records()[start].series_id;
        let end = start
            + states.records()[start..].partition_point(|record| record.series_id == series_id);
        let descriptor = series_descriptor(gauges.series(), series_id)?;
        for pair in states.records()[start..end].windows(2) {
            if coverage_intersects(known_gaps, pair[0].ts_us, pair[1].ts_us) {
                continue;
            }
            if let Some(fact) = EventFact::from_state_transition(
                descriptor,
                pair[0].ts_us,
                pair[0].state_code,
                pair[1].ts_us,
                pair[1].state_code,
                pair[1].population_total,
            )
            .map_err(|_error| BuildError::Internal)?
            {
                push_fact(&mut facts, fact, bounds)?;
            }
        }
        start = end;
    }

    for descriptor in gauges.series().iter().filter(|descriptor| {
        matches!(
            kronika_analytics::overview::MetricFactor::from_id(descriptor.factor_id),
            Some(
                kronika_analytics::overview::MetricFactor::PgStatisticsResetAt
                    | kronika_analytics::overview::MetricFactor::PgPostmasterStartTime
            )
        )
    }) {
        let samples = gauge_samples_for_series(gauges.samples(), descriptor.series_id);
        for pair in samples.windows(2) {
            if coverage_intersects(known_gaps, pair[0].ts_us(), pair[1].ts_us()) {
                continue;
            }
            if let Some(fact) = EventFact::from_metadata_change(*descriptor, pair[0], pair[1])
                .map_err(|_error| BuildError::Internal)?
            {
                push_fact(&mut facts, fact, bounds)?;
            }
        }
    }

    derive_sender_disappearances(gauges, states, known_gaps, bounds, &mut facts)?;

    let mut totals = BTreeMap::new();
    let mut available_by_key = BTreeMap::new();
    for descriptor in gauges.series() {
        let Some(entity) = descriptor.entity else {
            continue;
        };
        let destination =
            match kronika_analytics::overview::MetricFactor::from_id(descriptor.factor_id) {
                Some(kronika_analytics::overview::MetricFactor::PgFilesystemTotalBytes) => {
                    &mut totals
                }
                Some(kronika_analytics::overview::MetricFactor::PgFilesystemAvailableBytes) => {
                    &mut available_by_key
                }
                _ => continue,
            };
        for sample in gauge_samples_for_series(gauges.samples(), descriptor.series_id) {
            if destination
                .insert((entity, sample.ts_us()), (*descriptor, *sample))
                .is_some()
            {
                return Err(BuildError::Internal);
            }
        }
    }
    for (key, (total_descriptor, total)) in totals {
        let Some((available_descriptor, available_sample)) = available_by_key.get(&key).copied()
        else {
            continue;
        };
        if let Some(fact) = EventFact::from_capacity_samples(
            total_descriptor,
            total,
            available_descriptor,
            available_sample,
        )
        .map_err(|_error| BuildError::Internal)?
        {
            push_fact(&mut facts, fact, bounds)?;
        }
        let previous = available_by_key
            .range((key.0, i64::MIN)..key)
            .next_back()
            .map(|(_key, (_descriptor, sample))| *sample);
        if let Some(previous) = previous
            && let Some(fact) = EventFact::from_capacity_zero_transition(
                total_descriptor,
                total,
                available_descriptor,
                previous,
                available_sample,
                known_gaps,
            )
            .map_err(|_error| BuildError::Internal)?
        {
            push_fact(&mut facts, fact, bounds)?;
        }
    }
    Ok(facts)
}

fn derive_sender_disappearances(
    gauges: &GaugeSamplesBlock,
    states: &EntityStatesBlock,
    known_gaps: &Coverage,
    bounds: &Bounds,
    facts: &mut Vec<EventFact>,
) -> Result<(), BuildError> {
    let mut boundaries =
        BTreeMap::<(u64, u32), Vec<(MetricSeriesDescriptor, EntityStateRecord)>>::new();
    let mut snapshots = BTreeMap::<
        (u64, u32, i64),
        BTreeMap<MetricSeriesId, (MetricSeriesDescriptor, EntityStateRecord)>,
    >::new();
    for record in states.records() {
        let descriptor = series_descriptor(gauges.series(), record.series_id)?;
        match kronika_analytics::overview::MetricFactor::from_id(descriptor.factor_id) {
            Some(
                kronika_analytics::overview::MetricFactor::PgReplicationSenderSnapshotPopulation,
            ) => boundaries
                .entry((descriptor.source_id, descriptor.source_type_id))
                .or_default()
                .push((descriptor, *record)),
            Some(kronika_analytics::overview::MetricFactor::PgReplicationSenderState) => {
                snapshots
                    .entry((
                        descriptor.source_id,
                        descriptor.source_type_id,
                        record.ts_us,
                    ))
                    .or_default()
                    .insert(descriptor.series_id, (descriptor, *record));
            }
            _ => {}
        }
    }
    for ((source_id, source_type), source_boundaries) in &mut boundaries {
        source_boundaries.sort_unstable_by_key(|(_descriptor, record)| record.ts_us);
        for pair in source_boundaries.windows(2) {
            let (previous_descriptor, previous) = pair[0];
            let (current_descriptor, current) = pair[1];
            if previous_descriptor.series_id != current_descriptor.series_id
                || coverage_intersects(known_gaps, previous.ts_us, current.ts_us)
            {
                continue;
            }
            let empty = BTreeMap::new();
            let previous_entities = snapshots
                .get(&(*source_id, *source_type, previous.ts_us))
                .unwrap_or(&empty);
            let current_entities = snapshots
                .get(&(*source_id, *source_type, current.ts_us))
                .unwrap_or(&empty);
            let current_sample = GaugeSample::new(
                current_descriptor.series_id,
                current.ts_us,
                f64::from(current.state_code),
            )
            .expect("u32 population is finite");
            for (series_id, (sender_descriptor, sender)) in previous_entities {
                if current_entities.contains_key(series_id) {
                    continue;
                }
                if let Some(fact) = EventFact::from_sender_disappearance(
                    *sender_descriptor,
                    sender.ts_us,
                    sender.state_code,
                    current_descriptor,
                    current_sample,
                    current.population_total,
                )
                .map_err(|_error| BuildError::Internal)?
                {
                    push_fact(facts, fact, bounds)?;
                }
            }
        }
    }
    Ok(())
}

fn series_descriptor(
    series: &[MetricSeriesDescriptor],
    id: MetricSeriesId,
) -> Result<MetricSeriesDescriptor, BuildError> {
    series
        .binary_search_by_key(&id.0, |descriptor| descriptor.series_id.0)
        .ok()
        .map(|index| series[index])
        .ok_or(BuildError::Internal)
}

fn gauge_samples_for_series(samples: &[GaugeSample], series_id: MetricSeriesId) -> &[GaugeSample] {
    let start = samples.partition_point(|sample| sample.series_id() < series_id);
    let end = start + samples[start..].partition_point(|sample| sample.series_id() == series_id);
    &samples[start..end]
}

fn coverage_intersects(coverage: &Coverage, from_us: i64, to_us: i64) -> bool {
    let index = coverage
        .spans()
        .partition_point(|span| span.end_us() <= from_us);
    coverage
        .spans()
        .get(index)
        .is_some_and(|span| span.start_us() < to_us)
}

fn push_fact(
    destination: &mut Vec<EventFact>,
    fact: EventFact,
    bounds: &Bounds,
) -> Result<(), BuildError> {
    if destination.len() as u64 == bounds.items_per_block {
        return Err(BuildError::LimitExceeded);
    }
    destination.push(fact);
    Ok(())
}

const fn block_build_error(error: super::block::BlockError) -> BuildError {
    match error {
        super::block::BlockError::AboveBound => BuildError::LimitExceeded,
        _ => BuildError::Internal,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "promotion validates and assembles the complete persisted metric contract in one pass"
)]
fn promote_metrics(
    parts: &[&SegmentFacts],
    expected_source_id: u64,
    interval: Option<CoverageSpan>,
    bounds: &Bounds,
) -> Result<Option<MetricExtraction>, BuildError> {
    let Some(interval) = interval else {
        let has_metric_data = parts.iter().any(|part| {
            !part.counter_samples.samples().is_empty()
                || !part.gauge_samples.samples().is_empty()
                || !part.reset_markers.markers().is_empty()
                || !part.entity_states.records().is_empty()
                || !part.loss_coverage.factor_coverage().is_empty()
                || part.event_facts.iter().any(|fact| {
                    matches!(
                        fact.kind(),
                        kronika_analytics::overview::EventKind::CollectorSnapshotGap
                            | kronika_analytics::overview::EventKind::CollectorSourceReadFailure
                            | kronika_analytics::overview::EventKind::CollectorVisibilityRestricted
                    )
                })
        });
        return Ok((!has_metric_data).then(|| MetricExtraction {
            descriptor_replacements: Vec::new(),
            counter_series: Vec::new(),
            counters: Vec::new(),
            gauge_series: Vec::new(),
            gauges: Vec::new(),
            reset_markers: Vec::new(),
            entity_states: Vec::new(),
            factor_coverage: Vec::new(),
            event_facts: Vec::new(),
            pgm_body_read_stats: PgmBodyReadStats::default(),
        }));
    };
    let mut counter_series = BTreeMap::new();
    let mut gauge_series = BTreeMap::new();
    let mut counters = Vec::new();
    let mut gauges = Vec::new();
    let mut entity_states = Vec::new();
    let mut event_facts = Vec::new();
    for part in parts {
        if !merge_metric_series(
            &mut counter_series,
            part.counter_samples.series(),
            expected_source_id,
        ) || !merge_metric_series(
            &mut gauge_series,
            part.gauge_samples.series(),
            expected_source_id,
        ) {
            return Ok(None);
        }
        checked_extend(
            &mut counters,
            part.counter_samples.samples(),
            bounds.items_per_block,
        )?;
        checked_extend(
            &mut gauges,
            part.gauge_samples.samples(),
            bounds.items_per_block,
        )?;
        checked_extend(
            &mut entity_states,
            part.entity_states.records(),
            bounds.items_per_block,
        )?;
        for fact in part.event_facts.iter().filter(|fact| {
            matches!(
                fact.kind(),
                kronika_analytics::overview::EventKind::CollectorSnapshotGap
                    | kronika_analytics::overview::EventKind::CollectorSourceReadFailure
                    | kronika_analytics::overview::EventKind::CollectorVisibilityRestricted
            )
        }) {
            checked_extend(
                &mut event_facts,
                std::slice::from_ref(fact),
                bounds.items_per_block,
            )?;
        }
    }
    counters.sort_unstable_by_key(|sample| {
        (
            sample.series_id().0,
            sample.alignment_id().0,
            sample.ts_us(),
        )
    });
    if counters.windows(2).any(|pair| {
        pair[0].series_id() == pair[1].series_id()
            && pair[0].alignment_id() == pair[1].alignment_id()
            && pair[0].ts_us() == pair[1].ts_us()
    }) {
        return Ok(None);
    }
    gauges.sort_unstable_by_key(|sample| (sample.series_id().0, sample.ts_us()));
    if gauges.windows(2).any(|pair| {
        pair[0].series_id() == pair[1].series_id() && pair[0].ts_us() == pair[1].ts_us()
    }) {
        return Ok(None);
    }
    entity_states.sort_unstable_by_key(|record| (record.series_id.0, record.ts_us));
    if entity_states
        .windows(2)
        .any(|pair| pair[0].series_id == pair[1].series_id && pair[0].ts_us == pair[1].ts_us)
    {
        return Ok(None);
    }
    let factor_coverage = promoted_factor_coverage(
        parts,
        &counter_series,
        &counters,
        &gauge_series,
        &gauges,
        &entity_states,
        interval,
        bounds,
    )?;
    let reset_markers = reset_markers_for_samples(&counters);
    event_facts.sort_by(EventFact::canonical_cmp);
    if has_duplicate_fact_id(&event_facts) {
        return Ok(None);
    }
    Ok(Some(MetricExtraction {
        descriptor_replacements: Vec::new(),
        counter_series: counter_series.into_values().collect(),
        counters,
        gauge_series: gauge_series.into_values().collect(),
        gauges,
        reset_markers,
        entity_states,
        factor_coverage,
        event_facts,
        pgm_body_read_stats: PgmBodyReadStats::default(),
    }))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the inputs and steps mirror the complete promoted factor-coverage contract"
)]
fn promoted_factor_coverage(
    parts: &[&SegmentFacts],
    counter_series: &BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    counters: &[CounterSample],
    gauge_series: &BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    gauges: &[GaugeSample],
    entity_states: &[EntityStateRecord],
    interval: CoverageSpan,
    bounds: &Bounds,
) -> Result<Vec<FactorCoverage>, BuildError> {
    let mut records = BTreeMap::<FactorId, Vec<&FactorCoverage>>::new();
    for part in parts {
        for coverage in part.loss_coverage.factor_coverage() {
            records
                .entry(coverage.factor_id)
                .or_default()
                .push(coverage);
        }
    }
    if records.len() as u64 > bounds.items_per_block {
        return Err(BuildError::LimitExceeded);
    }

    let mut times = BTreeMap::<FactorId, Vec<i64>>::new();
    for sample in counters {
        let descriptor = counter_series
            .get(&sample.series_id())
            .ok_or(BuildError::Internal)?;
        times
            .entry(descriptor.factor_id)
            .or_default()
            .push(sample.ts_us());
    }
    for sample in gauges {
        let descriptor = gauge_series
            .get(&sample.series_id())
            .ok_or(BuildError::Internal)?;
        times
            .entry(descriptor.factor_id)
            .or_default()
            .push(sample.ts_us());
    }
    let mut latest_population_by_source = BTreeMap::<u32, (i64, u64)>::new();
    for state in entity_states {
        let descriptor = gauge_series
            .get(&state.series_id)
            .ok_or(BuildError::Internal)?;
        let latest = latest_population_by_source
            .entry(descriptor.source_type_id)
            .or_insert((state.ts_us, state.population_total));
        if state.ts_us > latest.0 {
            *latest = (state.ts_us, state.population_total);
        }
    }

    records
        .into_iter()
        .map(|(factor_id, mut records)| {
            records.sort_unstable_by_key(|coverage| {
                (coverage.interval.start_us(), coverage.interval.end_us())
            });
            let applicability = records
                .first()
                .map(|coverage| coverage.applicability)
                .ok_or(BuildError::Internal)?;
            if records
                .iter()
                .any(|coverage| coverage.applicability != applicability)
            {
                return Err(BuildError::Internal);
            }
            if applicability == Applicability::Unsupported {
                return Ok(unsupported_promoted_coverage(factor_id, interval));
            }
            if applicability != Applicability::Applicable {
                return Err(BuildError::Internal);
            }

            let mut loss_reasons = records
                .iter()
                .flat_map(|coverage| coverage.loss_reasons.iter().copied())
                .collect::<Vec<_>>();
            loss_reasons.sort_unstable();
            loss_reasons.dedup();
            let lost_count_lower_bound = records
                .iter()
                .filter_map(|coverage| coverage.lost_count_lower_bound)
                .max()
                .filter(|lost| *lost != 0);
            let mut source_population = records
                .iter()
                .rev()
                .find_map(|coverage| coverage.source_population);
            let mut factor_sources = counter_series
                .values()
                .chain(gauge_series.values())
                .filter(|descriptor| descriptor.factor_id == factor_id)
                .map(|descriptor| descriptor.source_type_id)
                .collect::<Vec<_>>();
            factor_sources.sort_unstable();
            factor_sources.dedup();
            if let [source_type_id] = factor_sources.as_slice()
                && let Some((_ts_us, population_total)) =
                    latest_population_by_source.get(source_type_id)
            {
                source_population = Some(SourcePopulation {
                    collected: *population_total,
                    total: Some(*population_total),
                    total_quality: PopulationTotalQuality::Exact,
                });
            }
            let source_completeness = if records
                .iter()
                .all(|coverage| coverage.source_completeness == SourceCompleteness::Full)
            {
                SourceCompleteness::Full
            } else if records
                .iter()
                .all(|coverage| coverage.source_completeness == SourceCompleteness::Unknown)
            {
                SourceCompleteness::Unknown
            } else {
                SourceCompleteness::BoundedSubset
            };

            let factor_times = times.remove(&factor_id).unwrap_or_default();
            let present_samples =
                u64::try_from(factor_times.len()).map_err(|_error| BuildError::Overflow)?;
            let cadence = observed_cadence(&factor_times);
            let covered_duration_us = cadence.map_or(0, |(period, _cadence_id)| {
                cadence_covered_duration(&factor_times, interval, period)
            });
            let state = promoted_coverage_state(
                present_samples,
                cadence.is_some(),
                covered_duration_us,
                interval,
                loss_reasons.is_empty(),
            );

            Ok(FactorCoverage {
                factor_id,
                applicability,
                state,
                interval,
                expected_period_us: cadence.map(|(period, _cadence_id)| period),
                period_quality: cadence
                    .map_or(PeriodQuality::Unknown, |_| PeriodQuality::ObservedStable),
                cadence_epoch_id: cadence.map(|(_period, id)| id),
                crosses_cadence_boundary: false,
                present_samples,
                covered_duration_us,
                source_population,
                loss_reasons,
                lost_count_lower_bound,
                retained_exactness: if records
                    .iter()
                    .all(|coverage| coverage.retained_exactness == RetainedExactness::Exact)
                    && lost_count_lower_bound.is_none()
                {
                    RetainedExactness::Exact
                } else {
                    RetainedExactness::Unknown
                },
                source_completeness,
                physical_count_semantics: PhysicalCountSemantics::NotApplicable,
                boundary_quality: BoundaryQuality::Contained,
            })
        })
        .collect()
}

const fn promoted_coverage_state(
    present_samples: u64,
    has_cadence: bool,
    covered_duration_us: u64,
    interval: CoverageSpan,
    losses_empty: bool,
) -> CoverageState {
    if present_samples == 0 {
        if losses_empty {
            CoverageState::NotCollected
        } else {
            CoverageState::Gap
        }
    } else if !has_cadence {
        if losses_empty {
            CoverageState::Unknown
        } else {
            CoverageState::Gap
        }
    } else if covered_duration_us == interval.duration_us() {
        CoverageState::Complete
    } else if covered_duration_us == 0 {
        if losses_empty {
            CoverageState::Unknown
        } else {
            CoverageState::Gap
        }
    } else {
        CoverageState::Partial
    }
}

const fn unsupported_promoted_coverage(
    factor_id: FactorId,
    interval: CoverageSpan,
) -> FactorCoverage {
    FactorCoverage {
        factor_id,
        applicability: Applicability::Unsupported,
        state: CoverageState::NotCollected,
        interval,
        expected_period_us: None,
        period_quality: PeriodQuality::Unknown,
        cadence_epoch_id: None,
        crosses_cadence_boundary: false,
        present_samples: 0,
        covered_duration_us: 0,
        source_population: None,
        loss_reasons: Vec::new(),
        lost_count_lower_bound: None,
        retained_exactness: RetainedExactness::Unknown,
        source_completeness: SourceCompleteness::Unknown,
        physical_count_semantics: PhysicalCountSemantics::NotApplicable,
        boundary_quality: BoundaryQuality::Unknown,
    }
}

fn merge_metric_series(
    destination: &mut BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    incoming: &[MetricSeriesDescriptor],
    expected_source_id: u64,
) -> bool {
    incoming.iter().all(|descriptor| {
        if descriptor.source_id != expected_source_id {
            return false;
        }
        match destination.entry(descriptor.series_id) {
            std::collections::btree_map::Entry::Occupied(existing) => {
                *existing.get() == *descriptor
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(*descriptor);
                true
            }
        }
    })
}

fn checked_extend<T: Clone>(
    destination: &mut Vec<T>,
    incoming: &[T],
    bound: u64,
) -> Result<(), BuildError> {
    let next = destination
        .len()
        .checked_add(incoming.len())
        .ok_or(BuildError::Overflow)?;
    if next as u64 > bound {
        return Err(BuildError::LimitExceeded);
    }
    destination.extend_from_slice(incoming);
    Ok(())
}

fn reset_markers_for_samples(samples: &[CounterSample]) -> Vec<ResetMarker> {
    let mut markers = Vec::new();
    let mut previous = None;
    for sample in samples {
        let epoch = (sample.series_id(), sample.reset_epoch());
        if previous != Some(epoch) {
            markers.push(ResetMarker {
                series_id: sample.series_id(),
                ts_us: sample.ts_us(),
                reset_epoch: sample.reset_epoch(),
            });
            previous = Some(epoch);
        }
    }
    markers
}

fn validate_metric_source(
    series: &[MetricSeriesDescriptor],
    expected_source_id: u64,
) -> Result<(), CacheReadError> {
    if series
        .iter()
        .any(|descriptor| descriptor.source_id != expected_source_id)
    {
        return Err(CacheReadError::WrongSource);
    }
    Ok(())
}

fn validate_reset_series(
    markers: &ResetMarkersBlock,
    counters: &CounterSamplesBlock,
) -> Result<(), CacheReadError> {
    if markers.markers() != reset_markers_for_samples(counters.samples()) {
        return Err(CacheReadError::Corrupt);
    }
    Ok(())
}

fn validate_state_series(
    states: &EntityStatesBlock,
    gauges: &GaugeSamplesBlock,
) -> Result<(), CacheReadError> {
    for state in states.records() {
        let descriptor = gauges
            .series()
            .binary_search_by_key(&state.series_id.0, |descriptor| descriptor.series_id.0)
            .ok()
            .map(|index| gauges.series()[index]);
        let sample = gauges
            .samples()
            .binary_search_by_key(&(state.series_id.0, state.ts_us), |sample| {
                (sample.series_id().0, sample.ts_us())
            })
            .ok()
            .map(|index| gauges.samples()[index]);
        if descriptor.is_none_or(|descriptor| {
            descriptor.entity.is_none() || descriptor.unit != MetricUnit::StateCode
        }) || sample
            .is_none_or(|sample| sample.value().to_bits() != f64::from(state.state_code).to_bits())
        {
            return Err(CacheReadError::Corrupt);
        }
    }
    Ok(())
}

fn segment_span(min_ts_us: i64, max_ts_us: i64) -> Result<Option<CoverageSpan>, BuildError> {
    if min_ts_us > max_ts_us {
        return Ok(None);
    }
    let end = max_ts_us.checked_add(1).ok_or(BuildError::Overflow)?;
    CoverageSpan::new(min_ts_us, end)
        .map(Some)
        .ok_or(BuildError::Source(SourceError::UnsupportedLayout))
}

/// Half-open coverage of an inclusive catalog time range.
fn segment_coverage(min_ts_us: i64, max_ts_us: i64) -> Result<Coverage, BuildError> {
    if min_ts_us > max_ts_us {
        return Ok(Coverage::empty());
    }
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

fn validate_event_fact_evidence(
    facts: &EventFactsBlock,
    observations: &EventObservationsBlock,
    counters: &CounterSamplesBlock,
    gauges: &GaugeSamplesBlock,
    expected_source_id: u64,
) -> Result<(), CacheReadError> {
    let mut source_ids = BTreeMap::<ObservationId, u64>::new();
    for observation in observations.observations() {
        insert_evidence_source(
            &mut source_ids,
            observation.observation_id(),
            observation.source_id(),
        )?;
    }
    for sample in counters.samples() {
        insert_evidence_source(
            &mut source_ids,
            kronika_analytics::overview::counter_sample_observation_id(*sample),
            metric_source(counters.series(), sample.series_id())?,
        )?;
    }
    for sample in gauges.samples() {
        insert_evidence_source(
            &mut source_ids,
            kronika_analytics::overview::gauge_sample_observation_id(*sample),
            metric_source(gauges.series(), sample.series_id())?,
        )?;
    }
    for fact in facts.facts() {
        if fact.coverage().source_id != expected_source_id {
            return Err(CacheReadError::WrongSource);
        }
        if matches!(
            fact.kind(),
            kronika_analytics::overview::EventKind::CollectorSnapshotGap
                | kronika_analytics::overview::EventKind::CollectorSourceReadFailure
                | kronika_analytics::overview::EventKind::CollectorVisibilityRestricted
        ) {
            continue;
        }
        if fact
            .supporting_observation_ids()
            .iter()
            .any(|id| source_ids.get(id) != Some(&fact.coverage().source_id))
        {
            return Err(CacheReadError::Corrupt);
        }
    }
    Ok(())
}

fn insert_evidence_source(
    source_ids: &mut BTreeMap<ObservationId, u64>,
    id: ObservationId,
    source_id: u64,
) -> Result<(), CacheReadError> {
    if source_ids.insert(id, source_id).is_some() {
        return Err(CacheReadError::Corrupt);
    }
    Ok(())
}

fn metric_source(
    descriptors: &[MetricSeriesDescriptor],
    series_id: MetricSeriesId,
) -> Result<u64, CacheReadError> {
    descriptors
        .binary_search_by_key(&series_id.0, |descriptor| descriptor.series_id.0)
        .ok()
        .map(|index| descriptors[index].source_id)
        .ok_or(CacheReadError::Corrupt)
}

fn has_duplicate_fact_id(facts: &[EventFact]) -> bool {
    let mut ids = facts.iter().map(EventFact::fact_id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.windows(2).any(|pair| pair[0] == pair[1])
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
    let mut factor_coverage = Vec::new();
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
        let next_factor_count = factor_coverage
            .len()
            .checked_add(block.factor_coverage().len())
            .ok_or(CacheReadError::Oversized)?;
        if next_factor_count as u64 > bounds.items_per_block {
            return Err(CacheReadError::Oversized);
        }
        factor_coverage.extend_from_slice(block.factor_coverage());
    }
    LossCoverageBlock::new_with_factors(
        covered,
        known_gaps,
        applicability.ok_or(CacheReadError::Corrupt)?,
        period_quality.ok_or(CacheReadError::Corrupt)?,
        source_completeness.ok_or(CacheReadError::Corrupt)?,
        retained_exactness.ok_or(CacheReadError::Corrupt)?,
        physical_count.ok_or(CacheReadError::Corrupt)?,
        dropped_lower_bound,
        factor_coverage,
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
        CountLimits, CounterReduction, ErrorCategory, EventKind, EventPayload, EvidenceQuality,
        LossReason, MetricFactor, ReductionLimits, ResetFamily, SemanticDivergence, SqlState,
        TimeQuality, classify_series, semantic_divergences,
    };
    use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::incident_gauges::PgReplicationPhysicalV1;
    use kronika_registry::pg_log::{
        PgLogAutovacuumV1, PgLogCheckpointV1, PgLogErrorV1, PgLogGapV1, PgLogLifecycleV1,
        PgLogLockWaitV1, PgLogSlowQueryV1, PgLogTempFileV1,
    };
    use kronika_registry::reset_metadata::ResetMetadata;
    use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
    use kronika_registry::{Section, StrId, Ts};
    use kronika_writer::{Interner, dict};

    use super::super::limits::LIMIT;
    use super::super::qualification_fixture::{ALL_FAMILY_SCHEMA_VERSION, all_family_fixture};
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

    const REDUCTION_LIMITS: ReductionLimits = ReductionLimits {
        max_input_items: 256,
        max_gap_spans: 256,
        max_counter_pairs: 256,
        max_gauge_samples: 256,
    };

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

    fn reset_metadata(ts_us: i64, postmaster_start_us: i64) -> ResetMetadata {
        ResetMetadata {
            ts: Ts(ts_us),
            postmaster_start_time: Ts(postmaster_start_us),
            pg_stat_database_reset_max_at: None,
            pg_stat_statements_reset_at: None,
            pg_store_plans_reset_at: None,
            pg_stat_bgwriter_reset_at: None,
            pg_stat_checkpointer_reset_at: None,
            pg_stat_wal_reset_at: None,
            pg_stat_archiver_reset_at: None,
            pg_stat_io_reset_at: None,
            ext_pg_stat_statements_version: None,
            ext_pg_store_plans_version: None,
            compute_query_id: None,
            track_io_timing: None,
            track_wal_io_timing: None,
        }
    }

    fn replication_sender(ts_us: i64) -> PgReplicationPhysicalV1 {
        replication_sender_state(ts_us, 3)
    }

    fn replication_sender_state(ts_us: i64, state_code: u8) -> PgReplicationPhysicalV1 {
        PgReplicationPhysicalV1 {
            ts: Ts(ts_us),
            pid: 42,
            backend_start_key: 5,
            application_name: StrId(1),
            slot_name: StrId(2),
            slot_type: StrId(3),
            state: StrId(4),
            sync_state: StrId(5),
            scope_code: 1,
            state_code,
            current_to_sent_bytes: None,
            sent_to_write_bytes: None,
            write_to_flush_bytes: None,
            flush_to_replay_bytes: None,
            write_lag_us: None,
            flush_lag_us: None,
            replay_lag_us: None,
        }
    }

    fn sender_coverage(
        ts_us: i64,
        read_state: u8,
        source_total: u32,
        collected: u32,
    ) -> SnapshotCoverageV1 {
        SnapshotCoverageV1 {
            ts: Ts(ts_us),
            source_type_id: 1_033_001,
            collector_pid: 99,
            collector_started_at: Ts(1),
            read_state,
            visibility: 0,
            source_total,
            collected,
        }
    }

    fn replication_snapshot_pgm(
        resets: &[ResetMetadata],
        senders: &[PgReplicationPhysicalV1],
        coverage: &[SnapshotCoverageV1],
    ) -> Vec<u8> {
        let reset_body = ResetMetadata::encode(resets).expect("encode reset metadata");
        let sender_body =
            PgReplicationPhysicalV1::encode(senders).expect("encode replication senders");
        let coverage_body = SnapshotCoverageV1::encode(coverage).expect("encode snapshot coverage");
        let min_ts = resets
            .iter()
            .map(|row| row.ts.0)
            .chain(senders.iter().map(|row| row.ts.0))
            .chain(coverage.iter().map(|row| row.ts.0))
            .min()
            .expect("fixture has a timestamp");
        let max_ts = resets
            .iter()
            .map(|row| row.ts.0)
            .chain(senders.iter().map(|row| row.ts.0))
            .chain(coverage.iter().map(|row| row.ts.0))
            .max()
            .expect("fixture has a timestamp");
        build_part(
            &[
                SectionInput {
                    type_id: 1_020_001,
                    rows: row_count(resets),
                    body: &reset_body,
                },
                SectionInput {
                    type_id: 1_033_001,
                    rows: row_count(senders),
                    body: &sender_body,
                },
                SectionInput {
                    type_id: 1_038_001,
                    rows: row_count(coverage),
                    body: &coverage_body,
                },
            ],
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        )
    }

    #[test]
    fn complete_empty_sender_snapshot_is_a_boundary_and_proves_disappearance() {
        let bytes = replication_snapshot_pgm(
            &[reset_metadata(10, 1), reset_metadata(20, 1)],
            &[replication_sender(10)],
            &[sender_coverage(10, 0, 1, 1), sender_coverage(20, 0, 0, 0)],
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open replication fixture");
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");

        let boundary_series = facts
            .gauge_samples()
            .series()
            .iter()
            .find(|descriptor| {
                MetricFactor::from_id(descriptor.factor_id)
                    == Some(MetricFactor::PgReplicationSenderSnapshotPopulation)
            })
            .expect("snapshot boundary series");
        let boundaries = facts
            .entity_states()
            .records()
            .iter()
            .filter(|record| record.series_id == boundary_series.series_id)
            .collect::<Vec<_>>();
        assert_eq!(boundaries.len(), 2);
        assert_eq!(boundaries[0].state_code, 0);
        assert_eq!(boundaries[0].population_total, 1);
        assert_eq!(boundaries[1].state_code, 0);
        assert_eq!(boundaries[1].population_total, 0);

        let disappearance = facts
            .event_facts()
            .iter()
            .find(|fact| fact.kind() == EventKind::PgReplicationSenderDisappeared)
            .expect("complete empty snapshot proves sender disappearance");
        assert_eq!(disappearance.interval().start_us(), 20);
        assert!(matches!(
            disappearance.payload(),
            EventPayload::StateTransition(payload)
                if payload.previous_state == 3
                    && payload.current_state == u32::MAX
                    && payload.population_total == 0
        ));
    }

    #[test]
    fn postmaster_change_rekeys_population_boundary_and_suppresses_disappearance() {
        let bytes = replication_snapshot_pgm(
            &[reset_metadata(10, 1), reset_metadata(20, 2)],
            &[replication_sender(10)],
            &[sender_coverage(10, 0, 1, 1), sender_coverage(20, 0, 0, 0)],
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open restart fixture");
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");

        let boundary_series_count = facts
            .gauge_samples()
            .series()
            .iter()
            .filter(|descriptor| {
                MetricFactor::from_id(descriptor.factor_id)
                    == Some(MetricFactor::PgReplicationSenderSnapshotPopulation)
            })
            .count();
        assert_eq!(
            boundary_series_count, 2,
            "the postmaster epoch is part of the complete-boundary identity"
        );
        assert!(
            facts
                .event_facts()
                .iter()
                .all(|fact| fact.kind() != EventKind::PgReplicationSenderDisappeared)
        );
        assert!(
            facts
                .event_facts()
                .iter()
                .any(|fact| fact.kind() == EventKind::PgPostmasterStartChanged)
        );
    }

    #[test]
    fn collector_read_failure_is_retained_as_a_canonical_coverage_fact() {
        let bytes = replication_snapshot_pgm(
            &[reset_metadata(10, 1)],
            &[],
            &[sender_coverage(10, 3, 2, 0)],
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open collector-loss fixture");
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");
        assert!(
            facts
                .event_facts()
                .iter()
                .any(|fact| fact.kind() == EventKind::CollectorSourceReadFailure)
        );
        assert!(
            facts
                .entity_states()
                .records()
                .iter()
                .all(|record| record.population_total != 2),
            "an incomplete snapshot cannot authorize an entity-population boundary"
        );
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
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");
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
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");
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
            SegmentFacts::extract(&unit, &tight),
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
            SegmentFacts::extract(&unit, &tight),
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
        let (_facts, local) = SegmentFacts::extract_with_stats(&unit, &LIMIT).expect("extract");
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
            SegmentFacts::extract(&unit, &LIMIT),
            Err(BuildError::Source(SourceError::Corrupt))
        ));
    }

    #[test]
    fn extract_materializes_one_observation_per_lifecycle_row() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");
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
        let raw = SegmentFacts::extract(&unit, &LIMIT).expect("raw extract");
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
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end assertion inventories every canonical fact family and contract"
    )]
    fn every_populated_canonical_block_matches_forced_raw_and_restart_warm() {
        let fixture = all_family_fixture();
        assert_eq!(fixture.schema_version, ALL_FAMILY_SCHEMA_VERSION);
        assert_eq!(fixture.source_id, 7);
        assert_eq!(fixture.cadence_us, 10);
        let bytes = fixture.sealed_bytes();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open all-family PGM");
        let raw = SegmentFacts::extract(&unit, &LIMIT).expect("forced raw extract");

        assert!(!raw.observations().is_empty());
        assert!(!raw.event_facts().is_empty());
        assert!(!raw.counter_samples().series().is_empty());
        assert!(!raw.counter_samples().samples().is_empty());
        assert!(!raw.gauge_samples().series().is_empty());
        assert!(!raw.gauge_samples().samples().is_empty());
        assert!(!raw.reset_markers().markers().is_empty());
        assert!(!raw.entity_states().records().is_empty());
        assert!(!raw.loss_coverage().factor_coverage().is_empty());
        for kind in [
            EventKind::PgDatabaseDeadlockDelta,
            EventKind::PgDatabaseRecoveryConflictDelta,
            EventKind::PgDatabaseChecksumFailureDelta,
            EventKind::PgDatabaseSessionsAbandonedDelta,
            EventKind::PgDatabaseSessionsFatalDelta,
            EventKind::PgDatabaseSessionsKilledDelta,
            EventKind::PgStatisticsResetObserved,
            EventKind::PgPostmasterStartChanged,
            EventKind::PgRecoveryRoleChanged,
            EventKind::PgTimelineChanged,
            EventKind::PgReplicationSenderStateChanged,
            EventKind::PgReplicationSenderDisappeared,
            EventKind::PgReplicationSlotStateChanged,
            EventKind::PgReplicationSlotLost,
            EventKind::OsCgroupMemoryHighDelta,
            EventKind::OsCgroupMemoryMaxDelta,
            EventKind::OsCgroupOomDelta,
            EventKind::OsCgroupOomKillDelta,
            EventKind::OsHostOomKillDelta,
            EventKind::OsFilesystemCapacityObservation,
            EventKind::OsFilesystemCapacityZeroTransition,
            EventKind::CollectorSourceReadFailure,
        ] {
            assert!(
                raw.event_facts().iter().any(|fact| fact.kind() == kind),
                "fixture did not materialize {kind:?}"
            );
        }
        let mut covered_factor_ids = raw
            .loss_coverage()
            .factor_coverage()
            .iter()
            .map(|coverage| coverage.factor_id)
            .collect::<Vec<_>>();
        covered_factor_ids.sort_unstable();
        assert_eq!(
            covered_factor_ids,
            MetricFactor::ALL
                .into_iter()
                .map(MetricFactor::id)
                .collect::<Vec<_>>(),
            "qualification coverage must enumerate the complete stable factor inventory"
        );
        for coverage in raw.loss_coverage().factor_coverage() {
            let factor = MetricFactor::from_id(coverage.factor_id).expect("known factor");
            assert_eq!(
                coverage.applicability,
                if factor.id().0 >= 900 {
                    Applicability::Unsupported
                } else {
                    Applicability::Applicable
                },
                "wrong applicability for {}",
                factor.wire_code()
            );
        }

        let descriptors = raw
            .counter_samples()
            .series()
            .iter()
            .chain(raw.gauge_samples().series());
        let mut populated_factor_ids = descriptors
            .clone()
            .map(|descriptor| descriptor.factor_id)
            .collect::<Vec<_>>();
        populated_factor_ids.sort_unstable();
        populated_factor_ids.dedup();
        assert_eq!(
            populated_factor_ids,
            MetricFactor::ALL[..28]
                .iter()
                .copied()
                .map(MetricFactor::id)
                .collect::<Vec<_>>(),
            "every supported factor needs at least one canonical series"
        );
        for descriptor in descriptors {
            let factor = MetricFactor::from_id(descriptor.factor_id).expect("known factor");
            assert_eq!(descriptor.source_id, fixture.source_id);
            assert!(
                descriptor.entity.is_some(),
                "{} has no entity",
                factor.wire_code()
            );
            let (unit, reset_family) = qualification_metric_contract(factor);
            assert_eq!(
                descriptor.unit,
                unit,
                "wrong unit for {}",
                factor.wire_code()
            );
            assert_eq!(
                descriptor.reset_family,
                reset_family,
                "wrong reset family for {}",
                factor.wire_code()
            );
        }

        let encoded = raw.encode(&LIMIT).expect("encode all canonical blocks");
        let restart_warm = SegmentFacts::from_reader(
            encoded.as_slice(),
            raw.identity(),
            raw.lineage(),
            &raw.catalog_descriptors(),
            &LIMIT,
        )
        .expect("restart-warm all canonical blocks");
        assert_eq!(
            restart_warm, raw,
            "restart-warm must preserve every canonical block, descriptor, and coverage axis"
        );

        let recomputed = SegmentFacts::extract(&unit, &LIMIT).expect("forced recompute");
        assert_eq!(
            recomputed, raw,
            "forced raw recomputation must be byte-semantically stable"
        );
    }

    fn qualification_metric_contract(factor: MetricFactor) -> (MetricUnit, Option<ResetFamily>) {
        match factor {
            MetricFactor::PgDatabaseDeadlocks
            | MetricFactor::PgDatabaseRecoveryConflicts
            | MetricFactor::PgDatabaseChecksumFailures
            | MetricFactor::PgDatabaseSessionsAbandoned
            | MetricFactor::PgDatabaseSessionsFatal
            | MetricFactor::PgDatabaseSessionsKilled => {
                (MetricUnit::Count, Some(ResetFamily::PgStatDatabase))
            }
            MetricFactor::OsCgroupMemoryHighEvents
            | MetricFactor::OsCgroupMemoryMaxEvents
            | MetricFactor::OsCgroupOomEvents
            | MetricFactor::OsCgroupOomKills => (MetricUnit::Count, Some(ResetFamily::CgroupBoot)),
            MetricFactor::OsHostOomKills => (MetricUnit::Count, Some(ResetFamily::HostBoot)),
            MetricFactor::PgStatisticsResetAt
            | MetricFactor::PgPostmasterStartTime
            | MetricFactor::PgReplicationReplayLag => (MetricUnit::Microseconds, None),
            MetricFactor::PgDatabaseConnections | MetricFactor::PgDatabaseConnectionLimit => {
                (MetricUnit::Connections, None)
            }
            MetricFactor::PgDatabaseFrozenXidAge => (MetricUnit::Transactions, None),
            MetricFactor::PgDatabaseMinMxidAge => (MetricUnit::Multixacts, None),
            MetricFactor::PgRecoveryRole
            | MetricFactor::PgTimeline
            | MetricFactor::PgReplicationSenderState
            | MetricFactor::PgReplicationSlotState
            | MetricFactor::PgReplicationSenderSnapshotPopulation
            | MetricFactor::PgReplicationSlotSnapshotPopulation => (MetricUnit::StateCode, None),
            MetricFactor::PgFilesystemTotalBytes
            | MetricFactor::PgFilesystemAvailableBytes
            | MetricFactor::OsCgroupMemoryCurrentBytes
            | MetricFactor::OsCgroupMemoryMaxBytes => (MetricUnit::Bytes, None),
            MetricFactor::CpuPressureUnsupported
            | MetricFactor::MemoryPsiUnsupported
            | MetricFactor::StorageThroughputUnsupported
            | MetricFactor::BlockedSessionsUnsupported => {
                panic!("unsupported factors cannot have canonical series")
            }
        }
    }

    #[test]
    fn every_populated_block_crc_is_enforced_for_the_all_family_fixture() {
        let bytes = all_family_fixture().sealed_bytes();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open all-family PGM");
        let raw = SegmentFacts::extract(&unit, &LIMIT).expect("extract all families");
        let encoded = raw.encode(&LIMIT).expect("encode every canonical block");
        let admitted = FactFile::admit(&encoded, raw.identity(), raw.lineage(), &LIMIT)
            .expect("admit pristine fact file");

        assert_eq!(
            admitted
                .directory()
                .iter()
                .map(|entry| entry.block_kind)
                .collect::<Vec<_>>(),
            BlockKind::ALL
                .into_iter()
                .map(BlockKind::code)
                .collect::<Vec<_>>(),
            "the qualification fixture must encode every canonical block exactly once"
        );

        for entry in admitted.directory() {
            if entry.stored_len == 0 {
                assert_eq!(
                    entry.block_kind,
                    BlockKind::StringTable.code(),
                    "only the fixture's intentionally text-free string table is empty"
                );
                continue;
            }
            let mut corrupted = encoded.clone();
            let offset = usize::try_from(entry.offset).expect("fixture offset fits usize");
            corrupted[offset] ^= 0x80;
            assert!(
                matches!(
                    SegmentFacts::from_bytes(
                        &corrupted,
                        raw.identity(),
                        raw.lineage(),
                        &raw.catalog_descriptors(),
                        &LIMIT,
                    ),
                    Err(CacheReadError::Corrupt)
                ),
                "block {} accepted a stale body CRC",
                entry.block_kind
            );
        }
    }

    #[test]
    fn every_all_family_source_body_crc_failure_stays_a_source_error() {
        let pristine = all_family_fixture().sealed_bytes();
        let catalog = PgmUnit::open(pristine.as_slice())
            .expect("open pristine all-family PGM")
            .catalog()
            .entries
            .clone();
        assert!(
            catalog.len() > BlockKind::ALL.len(),
            "the source fixture must exercise repeated section layouts"
        );

        for (ordinal, entry) in catalog.iter().enumerate() {
            assert_ne!(entry.len, 0, "fixture source body must not be empty");
            let mut damaged = pristine.clone();
            let offset = usize::try_from(entry.offset).expect("fixture source offset fits usize");
            damaged[offset] ^= 0x40;
            let unit = PgmUnit::open(damaged.as_slice()).expect("catalog remains readable");
            assert!(
                matches!(
                    SegmentFacts::extract(&unit, &LIMIT),
                    Err(BuildError::Source(SourceError::Corrupt))
                ),
                "source section ordinal {ordinal} type {} was masked by derived extraction",
                entry.type_id
            );
        }
    }

    #[test]
    fn all_family_range_edges_use_half_open_ownership_and_one_left_halo() {
        let bytes = all_family_fixture().sealed_bytes();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open all-family PGM");
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract all families");

        let left_range = CoverageSpan::new(10, 20).expect("left range");
        let right_range = CoverageSpan::new(20, 41).expect("right range");
        let left = facts.query(left_range, LIMITS).expect("left query");
        let right = facts.query(right_range, LIMITS).expect("right query");
        assert_eq!(left.observations().len(), 1);
        assert_eq!(right.observations().len(), 3);
        assert_eq!(
            left.observations().len() + right.observations().len(),
            facts.observations().len(),
            "an event exactly at the split belongs only to the right range"
        );

        let deadlock_series = facts
            .counter_samples()
            .series()
            .iter()
            .find(|series| series.factor_id == MetricFactor::PgDatabaseDeadlocks.id())
            .expect("deadlock series")
            .series_id;
        let samples: Vec<_> = facts
            .counter_samples()
            .samples()
            .iter()
            .copied()
            .filter(|sample| sample.series_id() == deadlock_series)
            .collect();
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.ts_us())
                .collect::<Vec<_>>(),
            vec![10, 20, 30, 40]
        );

        let bucket = CoverageSpan::new(15, 31).expect("unaligned bucket");
        let whole_intervals = classify_series(
            None,
            &samples,
            facts.loss_coverage().known_gaps(),
            REDUCTION_LIMITS,
        )
        .expect("classify whole series");
        let whole = CounterReduction::from_intervals(&whole_intervals, bucket, REDUCTION_LIMITS)
            .expect("reduce whole series")
            .expect("two owned pairs");

        let halo = samples[0];
        let selected = &samples[1..];
        let ranged_intervals = classify_series(
            Some(halo),
            selected,
            facts.loss_coverage().known_gaps(),
            REDUCTION_LIMITS,
        )
        .expect("classify selected series with halo");
        let ranged = CounterReduction::from_intervals(&ranged_intervals, bucket, REDUCTION_LIMITS)
            .expect("reduce selected range")
            .expect("halo restores both pairs");
        assert_eq!(ranged, whole);
        assert_eq!(ranged.valid_pairs(), 2);

        let no_halo = classify_series(
            None,
            selected,
            facts.loss_coverage().known_gaps(),
            REDUCTION_LIMITS,
        )
        .expect("classify without halo");
        let no_halo = CounterReduction::from_intervals(&no_halo, bucket, REDUCTION_LIMITS)
            .expect("reduce without halo")
            .expect("later pair remains");
        assert_eq!(
            no_halo.valid_pairs(),
            1,
            "the qualification proves the predecessor is semantically required"
        );
    }

    #[test]
    fn retained_observations_survive_fact_file_round_trip() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &LIMIT).expect("raw extract");
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
        let derived = SegmentFacts::extract(&unit, &LIMIT).expect("first build");
        let recomputed = SegmentFacts::extract(&unit, &LIMIT).expect("forced recompute");
        let divergences = semantic_divergences(&derived, &recomputed, full_range(), LIMITS)
            .expect("bounded comparison");
        assert!(divergences.is_empty());
    }

    #[test]
    fn range_slices_partition_retained_facts_without_double_counting() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");

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
        let raw = SegmentFacts::extract(&unit, &LIMIT).expect("raw extract");
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
    fn repeated_extraction_has_stable_content_identity_and_counts() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let first = SegmentFacts::extract(&unit, &LIMIT).expect("first");
        let second = SegmentFacts::extract(&unit, &LIMIT).expect("second");
        assert_eq!(first.identity(), second.identity());
        assert_eq!(first.lineage(), second.lineage());
        assert_eq!(first.observations(), second.observations());
        let first_result = first.query(full_range(), LIMITS).expect("first query");
        let second_result = second.query(full_range(), LIMITS).expect("second query");
        assert_eq!(first_result.counts(), second_result.counts());
    }

    #[test]
    fn corrupt_fact_buffer_returns_typed_error() {
        let bytes = three_lifecycle_events();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let raw = SegmentFacts::extract(&unit, &LIMIT).expect("raw extract");
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
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract");
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
