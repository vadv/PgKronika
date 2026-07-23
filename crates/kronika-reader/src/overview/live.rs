//! Bounded folding of completed journal parts into immutable overview views.

use std::collections::BTreeSet;
use std::sync::Arc;

use kronika_analytics::overview::{
    Coverage, CoverageSpan, OracleError, OracleLimits, OracleResult, RawOracle, query_bounded,
};
use kronika_format::ReadAt;

use crate::refresh::{
    JournalGenerationId, PartDescriptor, PartId, catalog_digest as refresh_catalog_digest,
};
use crate::unit::PgmUnit;

use super::facts::{BuildError, MAX_STORE_NAMESPACE_BYTES, SegmentContext, SegmentFacts};
use super::limits::Bounds;
use super::publish::{FactLoad, FactStore, PersistError};

/// Live builder state, per the live-view state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    /// The journal is proven empty; only sealed segments answer queries.
    Empty,
    /// A restart or reset has not yet folded every completed part to the
    /// watermark.
    Warming,
    /// Every completed part up to the watermark is folded exactly once.
    Current,
    /// Append continuity or identity was not proven; the prior view is stale.
    NeedsRebuild,
    /// A hard cap, unsupported or corrupt completed input, or overflow made the
    /// fold lossy; promotion is forbidden.
    Incomplete,
}

/// Whether a fold added a new chunk or recognized a re-delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldEffect {
    /// The part was folded for the first time in this generation.
    Folded,
    /// The part's key was already folded; the builder is unchanged.
    Duplicate,
}

/// Why a completed part could not be folded into the live view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveFoldError {
    /// The part belongs to a different generation than the builder.
    ///
    /// The caller must apply the generation reset before folding it.
    GenerationMismatch,
    /// Extraction of the completed part failed; the view is now [`Incomplete`].
    ///
    /// [`Incomplete`]: LiveState::Incomplete
    Build(BuildError),
    /// The descriptor does not identify the supplied PGM part.
    DescriptorMismatch,
    /// The part overlaps or precedes an already folded part.
    NonMonotonePart,
    /// The aggregate live view exceeded its configured hard bound.
    LimitExceeded,
    /// The builder must be reset before it can accept another part.
    InvalidState,
    /// A folded offset or watermark exceeded the checked integer range.
    Overflow,
}

impl std::fmt::Display for LiveFoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GenerationMismatch => f.write_str("part belongs to a different generation"),
            Self::Build(error) => write!(f, "completed part extraction failed: {error}"),
            Self::DescriptorMismatch => {
                f.write_str("part descriptor does not match the supplied PGM unit")
            }
            Self::NonMonotonePart => f.write_str("part position overlaps or precedes folded state"),
            Self::LimitExceeded => f.write_str("live view safety limit exceeded"),
            Self::InvalidState => f.write_str("live builder requires a reset"),
            Self::Overflow => f.write_str("folded offset or watermark overflow"),
        }
    }
}

impl std::error::Error for LiveFoldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::GenerationMismatch
            | Self::DescriptorMismatch
            | Self::NonMonotonePart
            | Self::LimitExceeded
            | Self::InvalidState
            | Self::Overflow => None,
        }
    }
}

/// Invalid immutable configuration for a live builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveConfigError {
    /// A stable store namespace is required for source identity.
    EmptyStoreNamespace,
    /// The store namespace exceeds the identity-input bound.
    StoreNamespaceTooLong,
    /// A bound exceeds the format's absolute limits.
    InvalidBounds,
}

impl std::fmt::Display for LiveConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyStoreNamespace => f.write_str("store namespace must not be empty"),
            Self::StoreNamespaceTooLong => f.write_str("store namespace exceeds 4096 bytes"),
            Self::InvalidBounds => f.write_str("live bounds exceed absolute limits"),
        }
    }
}

impl std::error::Error for LiveConfigError {}

#[derive(Debug, Clone, Copy, Default)]
struct LiveUsage {
    parts: u64,
    manifest_entries: u64,
    observations: u64,
    coverage_spans: u64,
    retained_text_bytes: u64,
}

impl LiveUsage {
    fn checked_add(self, facts: &SegmentFacts) -> Result<Self, LiveFoldError> {
        let manifest_entries = u64::try_from(facts.manifest_entries().len())
            .map_err(|_error| LiveFoldError::Overflow)?;
        let observations =
            u64::try_from(facts.observations().len()).map_err(|_error| LiveFoldError::Overflow)?;
        let coverage_spans = u64::try_from(facts.coverage().spans().len())
            .map_err(|_error| LiveFoldError::Overflow)?;
        Ok(Self {
            parts: self.parts.checked_add(1).ok_or(LiveFoldError::Overflow)?,
            manifest_entries: self
                .manifest_entries
                .checked_add(manifest_entries)
                .ok_or(LiveFoldError::Overflow)?,
            observations: self
                .observations
                .checked_add(observations)
                .ok_or(LiveFoldError::Overflow)?,
            coverage_spans: self
                .coverage_spans
                .checked_add(coverage_spans)
                .ok_or(LiveFoldError::Overflow)?,
            retained_text_bytes: self
                .retained_text_bytes
                .checked_add(facts.retained_text_bytes())
                .ok_or(LiveFoldError::Overflow)?,
        })
    }

    fn is_within(self, bounds: &Bounds) -> bool {
        self.parts <= u64::from(bounds.directory_entries)
            && self.manifest_entries <= u64::from(bounds.directory_entries)
            && self.observations <= bounds.items_per_block
            && self.coverage_spans <= bounds.coverage_spans
            && self.retained_text_bytes <= bounds.string_table_bytes
    }
}

/// The single mutable writer that folds completed parts into live facts.
#[derive(Debug, Clone)]
pub struct LiveBuilder {
    store_namespace: Vec<u8>,
    bounds: Bounds,
    state: LiveState,
    generation: JournalGenerationId,
    folded_part_ids: BTreeSet<PartId>,
    chunks: Vec<Arc<SegmentFacts>>,
    usage: LiveUsage,
    source_id: Option<u64>,
    watermark_us: Option<i64>,
    folded_through_offset: u64,
}

impl LiveBuilder {
    /// Creates an empty builder scoped to a store namespace.
    ///
    /// # Errors
    ///
    /// Returns [`LiveConfigError`] for an empty or oversized namespace or
    /// bounds above the absolute format limits.
    pub fn new(
        store_namespace: impl Into<Vec<u8>>,
        bounds: Bounds,
    ) -> Result<Self, LiveConfigError> {
        let store_namespace = store_namespace.into();
        if store_namespace.is_empty() {
            return Err(LiveConfigError::EmptyStoreNamespace);
        }
        if store_namespace.len() > MAX_STORE_NAMESPACE_BYTES {
            return Err(LiveConfigError::StoreNamespaceTooLong);
        }
        if !bounds.is_within_absolute_limits() {
            return Err(LiveConfigError::InvalidBounds);
        }
        Ok(Self {
            store_namespace,
            bounds,
            state: LiveState::Empty,
            generation: JournalGenerationId(0),
            folded_part_ids: BTreeSet::new(),
            chunks: Vec::new(),
            usage: LiveUsage::default(),
            source_id: None,
            watermark_us: None,
            folded_through_offset: 0,
        })
    }

    /// Current builder state.
    #[must_use]
    pub const fn state(&self) -> LiveState {
        self.state
    }

    /// Generation the folded state belongs to.
    #[must_use]
    pub const fn generation(&self) -> JournalGenerationId {
        self.generation
    }

    /// Number of folded chunks (one per completed part).
    #[must_use]
    pub const fn folded_part_count(&self) -> usize {
        self.chunks.len()
    }

    /// Latest folded data timestamp, absent while empty.
    #[must_use]
    pub const fn watermark_us(&self) -> Option<i64> {
        self.watermark_us
    }

    /// Journal offset the builder has folded through.
    #[must_use]
    pub const fn folded_through_offset(&self) -> u64 {
        self.folded_through_offset
    }

    /// Discards folded state and enters a fresh generation.
    ///
    /// A refresh that cannot prove append continuity (reset, replacement, or an
    /// uncertain rewrite) calls this before re-folding the current parts, so a
    /// part from a stale generation is never mixed into the new view.
    pub fn reset_to(&mut self, generation: JournalGenerationId) {
        self.generation = generation;
        self.folded_part_ids.clear();
        self.chunks.clear();
        self.usage = LiveUsage::default();
        self.source_id = None;
        self.watermark_us = None;
        self.folded_through_offset = 0;
        self.state = LiveState::Warming;
    }

    /// Folds one completed part into the live view exactly once.
    ///
    /// A part whose [`PartId`] was already folded in this generation is a no-op
    /// that returns [`FoldEffect::Duplicate`]. Extraction failure moves the view
    /// to [`LiveState::Incomplete`], which forbids promotion.
    ///
    /// # Errors
    ///
    /// Returns [`LiveFoldError`] when the part belongs to another generation,
    /// its extraction fails, or a folded counter overflows.
    pub fn fold_part<R: ReadAt>(
        &mut self,
        part: &PartDescriptor,
        unit: &PgmUnit<R>,
    ) -> Result<FoldEffect, LiveFoldError> {
        if matches!(self.state, LiveState::NeedsRebuild | LiveState::Incomplete) {
            return Err(LiveFoldError::InvalidState);
        }
        if part.part_id.generation != self.generation {
            self.state = LiveState::NeedsRebuild;
            return Err(LiveFoldError::GenerationMismatch);
        }
        let catalog = unit.catalog();
        if part.source_id != catalog.source_id
            || part.min_ts != catalog.min_ts
            || part.max_ts != catalog.max_ts
            || part.part_id.body_len != unit.source_file_len()
            || part.part_id.catalog_digest != refresh_catalog_digest(catalog)
        {
            self.state = LiveState::Incomplete;
            return Err(LiveFoldError::DescriptorMismatch);
        }
        if part.source_id != 0
            && self
                .source_id
                .is_some_and(|source_id| source_id != part.source_id)
        {
            self.state = LiveState::Incomplete;
            return Err(LiveFoldError::DescriptorMismatch);
        }
        if self.folded_part_ids.contains(&part.part_id) {
            return Ok(FoldEffect::Duplicate);
        }
        let end_offset = part
            .part_id
            .frame_offset
            .checked_add(part.part_id.body_len)
            .ok_or_else(|| {
                self.state = LiveState::Incomplete;
                LiveFoldError::Overflow
            })?;
        if !self.chunks.is_empty() && part.part_id.frame_offset < self.folded_through_offset {
            self.state = LiveState::Incomplete;
            return Err(LiveFoldError::NonMonotonePart);
        }
        let discriminator = part_discriminator(&part.part_id);
        let facts = SegmentFacts::fold_live(
            unit,
            &self.store_namespace,
            self.generation.0,
            &discriminator,
            &self.bounds,
        )
        .map_err(|error| {
            self.state = LiveState::Incomplete;
            LiveFoldError::Build(error)
        })?;
        let usage = self.usage.checked_add(&facts).inspect_err(|_error| {
            self.state = LiveState::Incomplete;
        })?;
        if !usage.is_within(&self.bounds) {
            self.state = LiveState::Incomplete;
            return Err(LiveFoldError::LimitExceeded);
        }

        self.chunks.push(Arc::new(facts));
        self.folded_part_ids.insert(part.part_id);
        self.usage = usage;
        if part.source_id != 0 {
            self.source_id = Some(part.source_id);
        }
        if part.min_ts <= part.max_ts {
            self.watermark_us = Some(
                self.watermark_us
                    .map_or(part.max_ts, |current| current.max(part.max_ts)),
            );
        }
        self.folded_through_offset = self.folded_through_offset.max(end_offset);
        if self.state != LiveState::Incomplete {
            self.state = LiveState::Current;
        }
        Ok(FoldEffect::Folded)
    }

    /// Publishes an immutable snapshot of the folded view.
    ///
    /// Publication copies at most the configured part count of shared pointers;
    /// observation payloads remain shared.
    #[must_use]
    pub fn publish(&self) -> LiveView {
        LiveView {
            generation: self.generation,
            state: self.state,
            watermark_us: self.watermark_us,
            folded_through_offset: self.folded_through_offset,
            chunks: self.chunks.clone(),
        }
    }

    /// The folded chunks, in fold order.
    #[must_use]
    pub fn chunks(&self) -> &[Arc<SegmentFacts>] {
        &self.chunks
    }
}

/// An immutable snapshot of the live view at one publication.
#[derive(Debug, Clone)]
pub struct LiveView {
    generation: JournalGenerationId,
    state: LiveState,
    watermark_us: Option<i64>,
    folded_through_offset: u64,
    chunks: Vec<Arc<SegmentFacts>>,
}

impl LiveView {
    /// Generation this view belongs to.
    #[must_use]
    pub const fn generation(&self) -> JournalGenerationId {
        self.generation
    }

    /// State captured at publication.
    #[must_use]
    pub const fn state(&self) -> LiveState {
        self.state
    }

    /// Whether the view is a promotion-eligible `Current` view.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.state == LiveState::Current
    }

    /// Latest folded data timestamp, absent for an empty view.
    #[must_use]
    pub const fn watermark_us(&self) -> Option<i64> {
        self.watermark_us
    }

    /// Journal offset folded through at publication.
    #[must_use]
    pub const fn folded_through_offset(&self) -> u64 {
        self.folded_through_offset
    }

    /// The folded chunks backing this view, in fold order.
    #[must_use]
    pub fn chunks(&self) -> &[Arc<SegmentFacts>] {
        &self.chunks
    }

    /// Catalog coverage envelope across every folded part.
    #[must_use]
    pub fn coverage(&self) -> Coverage {
        let start = self
            .chunks
            .iter()
            .flat_map(|chunk| chunk.coverage().spans())
            .map(|span| span.start_us())
            .min();
        let end = self
            .chunks
            .iter()
            .flat_map(|chunk| chunk.coverage().spans())
            .map(|span| span.end_us())
            .max();
        start
            .zip(end)
            .and_then(|(start, end)| CoverageSpan::new(start, end))
            .map_or_else(Coverage::empty, |span| Coverage::from_spans(vec![span]))
    }
}

impl RawOracle for LiveView {
    fn query(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<OracleResult, OracleError> {
        let coverage = self.coverage();
        query_bounded(
            self.chunks.iter().flat_map(|chunk| chunk.observations()),
            coverage.spans().iter().copied(),
            range,
            limits,
        )
    }
}

/// Outcome of reconciling a newly sealed segment against the live view.
#[derive(Debug)]
pub enum SealOutcome {
    /// The live candidate matched sealed provenance and was re-keyed.
    Promoted {
        /// Canonical sealed facts.
        facts: SegmentFacts,
        /// Best-effort durable publication failure.
        persist_error: Option<PersistError>,
    },
    /// The candidate was lossy, absent, or its provenance did not match, so the
    /// facts were rebuilt from the PGM.
    Rebuilt(FactLoad),
}

impl SealOutcome {
    /// The reconciled sealed facts, whichever path produced them.
    #[must_use]
    pub const fn facts(&self) -> &SegmentFacts {
        match self {
            Self::Promoted { facts, .. } => facts,
            Self::Rebuilt(load) => load.facts(),
        }
    }

    /// Whether the live candidate was promoted rather than rebuilt.
    #[must_use]
    pub const fn was_promoted(&self) -> bool {
        matches!(self, Self::Promoted { .. })
    }

    /// Best-effort persistence failure, if the cache was unavailable.
    #[must_use]
    pub const fn persist_error(&self) -> Option<PersistError> {
        match self {
            Self::Promoted { persist_error, .. } => *persist_error,
            Self::Rebuilt(load) => load.persist_error(),
        }
    }
}

/// Reconciles a newly sealed segment against the current live view.
///
/// A current candidate is promoted only when its ordered catalogs, source
/// identity, timestamp envelope, and referenced dictionary values match the
/// sealed segment. Promotion reads dictionary bodies when references exist, but
/// does not read event bodies. Any mismatch rebuilds from the sealed PGM.
///
/// # Errors
///
/// Returns [`BuildError`] when a promotion re-key fails or the rebuild's source
/// extraction fails.
pub fn reconcile_seal<R: ReadAt>(
    candidate: &LiveView,
    sealed_unit: &PgmUnit<R>,
    sealed_context: &SegmentContext,
    store: &FactStore,
    bounds: &Bounds,
) -> Result<SealOutcome, BuildError> {
    let parts: Vec<_> = candidate.chunks().iter().map(Arc::as_ref).collect();
    if candidate.is_current()
        && let Some(promoted) =
            SegmentFacts::try_promote_from_parts(sealed_unit, sealed_context, &parts, bounds)?
    {
        let persist_error = store.publish(&promoted, bounds).err();
        return Ok(SealOutcome::Promoted {
            facts: promoted,
            persist_error,
        });
    }
    let load = store.load_or_build(sealed_unit, sealed_context, bounds)?;
    Ok(SealOutcome::Rebuilt(load))
}

/// Serializes a part key into a unique per-part live-lineage discriminator.
///
/// Two parts at different journal positions get different discriminators, so
/// identical section bodies in different parts fold into distinct observation
/// identities and neither is lost.
fn part_discriminator(part_id: &PartId) -> [u8; 56] {
    let mut bytes = [0_u8; 56];
    bytes[0..8].copy_from_slice(&part_id.generation.0.to_le_bytes());
    bytes[8..16].copy_from_slice(&part_id.frame_offset.to_le_bytes());
    bytes[16..24].copy_from_slice(&part_id.body_len.to_le_bytes());
    bytes[24..56].copy_from_slice(part_id.catalog_digest.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use kronika_analytics::overview::{
        CountLimits, MemoryOracle, NamingContractId, SegmentLocator,
    };
    use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::{PgLogErrorV1, PgLogLifecycleV1};
    use kronika_registry::{Section, StrId, Ts};
    use kronika_writer::{Interner, dict};

    use super::super::SourceError;
    use super::super::limits::LIMIT;
    use super::*;
    use crate::refresh::part_id;

    const LIMITS: OracleLimits = OracleLimits {
        max_observations: 4_096,
        max_coverage_spans: 4_096,
        count_limits: CountLimits {
            max_input_entries: 4_096,
            max_joint_keys: 4_096,
            max_signal_keys: 4_096,
        },
    };

    const NAMESPACE: &[u8] = b"live-store";

    fn row(ts: i64, kind: u8, pid: Option<i32>, signal: Option<i32>) -> PgLogLifecycleV1 {
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

    fn lifecycle_part(rows: &[PgLogLifecycleV1]) -> Vec<u8> {
        let min_ts = rows
            .iter()
            .map(|row| row.ts.0)
            .min()
            .expect("non-empty part");
        let max_ts = rows
            .iter()
            .map(|row| row.ts.0)
            .max()
            .expect("non-empty part");
        let body = PgLogLifecycleV1::encode(rows).expect("encode lifecycle");
        build_part(
            &[SectionInput {
                type_id: 1_028_001,
                rows: u32::try_from(rows.len()).expect("row count fits"),
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        )
    }

    fn error_part(
        value: &[u8],
        limits: DictLimits,
        force_blob: bool,
        ts: i64,
    ) -> (Vec<u8>, Vec<(u32, u32, Vec<u8>)>) {
        let mut interner = Interner::new(limits);
        let id = if force_blob {
            interner.intern_blob(value)
        } else {
            interner.intern(value)
        }
        .expect("intern pattern");
        let error_body = PgLogErrorV1::encode(&[PgLogErrorV1 {
            ts: Ts(ts),
            severity: 0,
            category: 0,
            sqlstate: None,
            pattern: Some(StrId(id.get())),
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
        let mut sections = vec![(1_022_001, 1, error_body)];
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
        let bytes = build_part(
            &inputs,
            PartMeta {
                min_ts: ts,
                max_ts: ts,
                source_id: 7,
            },
        );
        (bytes, sections)
    }

    fn dictionary_only_part() -> (Vec<u8>, Vec<(u32, u32, Vec<u8>)>) {
        let limits = DictLimits::new(64, 1_024).expect("dictionary limits");
        let mut interner = Interner::new(limits);
        interner
            .intern(b"unreferenced value")
            .expect("intern value");
        let sections: Vec<_> = dict::encode(interner.window())
            .expect("encode dictionary")
            .into_iter()
            .map(|section| (section.type_id, section.rows, section.body))
            .collect();
        let inputs: Vec<_> = sections
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        let bytes = build_part(
            &inputs,
            PartMeta {
                min_ts: i64::MAX,
                max_ts: i64::MIN,
                source_id: 0,
            },
        );
        (bytes, sections)
    }

    fn seal_sections(sections: &[(u32, u32, Vec<u8>)], min_ts: i64, max_ts: i64) -> Vec<u8> {
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
                min_ts,
                max_ts,
                source_id: 7,
            },
        )
    }

    fn sealed_context() -> SegmentContext {
        SegmentContext::new(
            NAMESPACE.to_vec(),
            NamingContractId([0x51; 16]),
            SegmentLocator([0x52; 32]),
        )
        .expect("valid context")
    }

    fn live_builder() -> LiveBuilder {
        LiveBuilder::new(NAMESPACE.to_vec(), LIMIT).expect("valid live builder")
    }

    fn raw_oracle(rows: &[PgLogLifecycleV1]) -> SegmentFacts {
        let bytes = lifecycle_part(rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open unsplit");
        SegmentFacts::extract(&unit, &sealed_context(), &LIMIT).expect("extract unsplit")
    }

    fn fold_slices(builder: &mut LiveBuilder, slices: &[&[PgLogLifecycleV1]]) {
        for (index, rows) in slices.iter().enumerate() {
            let bytes = lifecycle_part(rows);
            let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
            let offset = (u64::try_from(index).expect("index fits") + 1) * 4_096;
            let key = part_id(
                builder.generation(),
                offset,
                u64::try_from(bytes.len()).expect("len fits"),
                unit.catalog(),
            );
            let descriptor = PartDescriptor {
                part_id: key,
                source_id: unit.catalog().source_id,
                min_ts: unit.catalog().min_ts,
                max_ts: unit.catalog().max_ts,
            };
            builder.fold_part(&descriptor, &unit).expect("fold part");
        }
    }

    fn envelope(coverage: &Coverage) -> Option<(i64, i64)> {
        let spans = coverage.spans();
        let first = spans.first()?;
        let last = spans.last()?;
        Some((first.start_us(), last.end_us()))
    }

    fn stream() -> Vec<PgLogLifecycleV1> {
        vec![
            row(1_000, 2, None, None),
            row(1_500, 1, None, None),
            row(2_000, 0, Some(11), Some(9)),
            row(2_500, 0, Some(12), None),
            row(3_000, 2, None, None),
            row(3_500, 0, Some(13), Some(6)),
            row(4_000, 1, None, None),
        ]
    }

    #[test]
    fn a_fresh_builder_is_empty() {
        let builder = live_builder();
        assert_eq!(builder.state(), LiveState::Empty);
        assert_eq!(builder.folded_part_count(), 0);
        assert_eq!(builder.watermark_us(), None);
    }

    #[test]
    fn builder_configuration_is_bounded() {
        assert!(matches!(
            LiveBuilder::new(Vec::new(), LIMIT),
            Err(LiveConfigError::EmptyStoreNamespace)
        ));
        assert!(matches!(
            LiveBuilder::new(vec![b'x'; MAX_STORE_NAMESPACE_BYTES + 1], LIMIT),
            Err(LiveConfigError::StoreNamespaceTooLong)
        ));
        let invalid = Bounds {
            items_per_block: LIMIT.items_per_block + 1,
            ..LIMIT
        };
        assert!(matches!(
            LiveBuilder::new(NAMESPACE.to_vec(), invalid),
            Err(LiveConfigError::InvalidBounds)
        ));
    }

    #[test]
    fn folding_a_completed_part_advances_the_watermark_and_becomes_current() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        assert_eq!(builder.state(), LiveState::Current);
        assert_eq!(builder.folded_part_count(), 1);
        assert_eq!(builder.watermark_us(), Some(4_000));
    }

    #[test]
    fn re_delivering_the_same_part_is_idempotent() {
        let mut builder = live_builder();
        let rows = stream();
        let bytes = lifecycle_part(&rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let key = part_id(
            builder.generation(),
            4_096,
            u64::try_from(bytes.len()).expect("len fits"),
            unit.catalog(),
        );
        let descriptor = PartDescriptor {
            part_id: key,
            source_id: unit.catalog().source_id,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        };
        assert_eq!(
            builder.fold_part(&descriptor, &unit).expect("fold"),
            FoldEffect::Folded
        );
        assert_eq!(
            builder.fold_part(&descriptor, &unit).expect("re-fold"),
            FoldEffect::Duplicate
        );
        assert_eq!(builder.folded_part_count(), 1, "redelivery adds no chunk");
    }

    #[test]
    fn a_part_from_a_different_generation_is_rejected() {
        let mut builder = live_builder();
        let rows = stream();
        let bytes = lifecycle_part(&rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let key = part_id(
            JournalGenerationId(99),
            4_096,
            u64::try_from(bytes.len()).expect("len fits"),
            unit.catalog(),
        );
        let descriptor = PartDescriptor {
            part_id: key,
            source_id: unit.catalog().source_id,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        };
        assert_eq!(
            builder.fold_part(&descriptor, &unit),
            Err(LiveFoldError::GenerationMismatch)
        );
        assert_eq!(builder.state(), LiveState::NeedsRebuild);
        assert_eq!(
            builder.fold_part(&descriptor, &unit),
            Err(LiveFoldError::InvalidState)
        );
    }

    #[test]
    fn a_descriptor_mismatch_makes_the_view_incomplete() {
        let rows = stream();
        let bytes = lifecycle_part(&rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let mut builder = live_builder();
        let descriptor = PartDescriptor {
            part_id: part_id(
                builder.generation(),
                4_096,
                u64::try_from(bytes.len()).expect("len fits"),
                unit.catalog(),
            ),
            source_id: unit.catalog().source_id + 1,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        };

        assert_eq!(
            builder.fold_part(&descriptor, &unit),
            Err(LiveFoldError::DescriptorMismatch)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
    }

    #[test]
    fn overlapping_parts_are_rejected() {
        let rows = stream();
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&rows[..3]]);

        let bytes = lifecycle_part(&rows[3..]);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let descriptor = PartDescriptor {
            part_id: part_id(
                builder.generation(),
                4_097,
                u64::try_from(bytes.len()).expect("len fits"),
                unit.catalog(),
            ),
            source_id: unit.catalog().source_id,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        };
        assert_eq!(
            builder.fold_part(&descriptor, &unit),
            Err(LiveFoldError::NonMonotonePart)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
    }

    #[test]
    fn reset_discards_folded_state_and_enters_warming() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        builder.reset_to(JournalGenerationId(4));
        assert_eq!(builder.state(), LiveState::Warming);
        assert_eq!(builder.generation(), JournalGenerationId(4));
        assert_eq!(builder.folded_part_count(), 0);
        assert_eq!(builder.watermark_us(), None);
    }

    #[test]
    fn a_stream_split_into_parts_reports_the_unsplit_counts_and_coverage_envelope() {
        let rows = stream();
        let raw = raw_oracle(&rows);
        let raw_result = raw.query(full_span(), LIMITS).expect("raw query");

        for split in [1_usize, 2, 3, 7] {
            let chunk = rows.len().div_ceil(split);
            let slices: Vec<&[PgLogLifecycleV1]> = rows.chunks(chunk).collect();
            let mut builder = live_builder();
            fold_slices(&mut builder, &slices);
            let view = builder.publish();
            let live_result = view.query(full_span(), LIMITS).expect("live query");

            assert_eq!(
                live_result.counts(),
                raw_result.counts(),
                "counts are identical regardless of the {split}-way split"
            );
            assert_eq!(
                live_result.observations().len(),
                raw_result.observations().len(),
                "no observation is dropped or duplicated at {split} parts"
            );
            assert_eq!(
                envelope(&view.coverage()),
                envelope(raw.coverage()),
                "the coverage envelope matches the unsplit source"
            );
        }
    }

    #[test]
    fn duplicate_rows_in_separate_parts_are_both_retained() {
        let duplicate = row(2_000, 0, Some(11), Some(9));
        let rows = vec![duplicate, duplicate];
        let raw = raw_oracle(&rows);
        let raw_result = raw.query(full_span(), LIMITS).expect("raw query");
        assert_eq!(raw_result.observations().len(), 2);

        let mut builder = live_builder();
        fold_slices(&mut builder, &[&rows[0..1], &rows[1..2]]);
        let live_result = builder
            .publish()
            .query(full_span(), LIMITS)
            .expect("live query");
        assert_eq!(
            live_result.observations().len(),
            2,
            "two identical rows in separate parts stay distinct"
        );
        assert_eq!(live_result.counts(), raw_result.counts());
    }

    fn full_span() -> CoverageSpan {
        CoverageSpan::new(0, 1_000_000).expect("valid range")
    }

    // ---- seal handoff (promotion) tests ----

    fn sealed_from_slices(slices: &[&[PgLogLifecycleV1]]) -> Vec<u8> {
        let bodies: Vec<Vec<u8>> = slices
            .iter()
            .map(|rows| PgLogLifecycleV1::encode(rows).expect("encode section"))
            .collect();
        let inputs: Vec<SectionInput<'_>> = slices
            .iter()
            .zip(&bodies)
            .map(|(rows, body)| SectionInput {
                type_id: 1_028_001,
                rows: u32::try_from(rows.len()).expect("row count fits"),
                body,
            })
            .collect();
        let min_ts = slices
            .iter()
            .flat_map(|rows| rows.iter())
            .map(|row| row.ts.0)
            .min()
            .expect("non-empty seal");
        let max_ts = slices
            .iter()
            .flat_map(|rows| rows.iter())
            .map(|row| row.ts.0)
            .max()
            .expect("non-empty seal");
        build_part(
            &inputs,
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        )
    }

    fn store() -> (tempfile::TempDir, FactStore) {
        let directory = tempfile::TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        (directory, store)
    }

    #[test]
    fn promotion_of_matching_parts_equals_a_cold_sealed_rebuild() {
        let rows = stream();
        for split in [1_usize, 2, 3, 7] {
            let chunk = rows.len().div_ceil(split);
            let slices: Vec<&[PgLogLifecycleV1]> = rows.chunks(chunk).collect();
            let sealed_bytes = sealed_from_slices(&slices);
            let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
            let context = sealed_context();
            let rebuilt =
                SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("cold rebuild");

            let mut builder = live_builder();
            fold_slices(&mut builder, &slices);
            let view = builder.publish();
            let (_cache_dir, store) = store();
            let outcome = reconcile_seal(&view, &sealed_unit, &context, &store, &LIMIT)
                .expect("reconcile seal");

            assert!(
                outcome.was_promoted(),
                "matching provenance promotes at {split}"
            );
            assert_eq!(
                outcome.facts().observations(),
                rebuilt.observations(),
                "promoted observations and IDs equal the cold rebuild at {split}"
            );
            assert_eq!(
                outcome.facts().coverage(),
                rebuilt.coverage(),
                "promoted coverage equals the cold rebuild at {split}"
            );
            assert_eq!(outcome.persist_error(), None);
            let cached = store
                .read(&sealed_unit, &context, &LIMIT)
                .expect("promoted facts were published");
            assert_eq!(cached.observations(), rebuilt.observations());
        }
    }

    #[test]
    fn promotion_survives_an_unwritable_cache() {
        let rows = stream();
        let slices: Vec<&[PgLogLifecycleV1]> = rows.chunks(2).collect();
        let sealed_bytes = sealed_from_slices(&slices);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("rebuild");
        let mut builder = live_builder();
        fold_slices(&mut builder, &slices);

        let (cache_dir, store) = store();
        let original_mode = fs::metadata(cache_dir.path())
            .expect("cache metadata")
            .permissions()
            .mode();
        fs::set_permissions(cache_dir.path(), fs::Permissions::from_mode(0))
            .expect("make cache read-only");
        let outcome = reconcile_seal(&builder.publish(), &sealed_unit, &context, &store, &LIMIT);
        fs::set_permissions(
            cache_dir.path(),
            fs::Permissions::from_mode(original_mode & 0o7777),
        )
        .expect("restore cache permissions");
        let outcome = outcome.expect("promotion remains available");

        assert!(outcome.was_promoted());
        assert_eq!(
            outcome.persist_error(),
            Some(PersistError::PermissionDenied)
        );
        assert_eq!(outcome.facts().observations(), rebuilt.observations());
    }

    #[test]
    fn equivalent_cross_part_dictionary_placement_promotes() {
        let value = b"same normalized pattern";
        let limits = DictLimits::new(64, 1_024).expect("dictionary limits");
        let (first_bytes, first_sections) = error_part(value, limits, false, 1_000);
        let (second_bytes, second_sections) = error_part(value, limits, true, 2_000);
        let mut sealed_sections = first_sections;
        sealed_sections.extend(second_sections);
        let sealed_bytes = seal_sections(&sealed_sections, 1_000, 2_000);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("cold rebuild");

        let mut builder = live_builder();
        let mut offset = 4_096_u64;
        for bytes in [&first_bytes, &second_bytes] {
            let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
            let body_len = u64::try_from(bytes.len()).expect("part length fits");
            let descriptor = PartDescriptor {
                part_id: part_id(builder.generation(), offset, body_len, unit.catalog()),
                source_id: unit.catalog().source_id,
                min_ts: unit.catalog().min_ts,
                max_ts: unit.catalog().max_ts,
            };
            builder
                .fold_part(&descriptor, &unit)
                .expect("fold dictionary part");
            offset = offset
                .checked_add(body_len)
                .and_then(|value| value.checked_add(4_096))
                .expect("offset fits");
        }

        let (_cache_dir, store) = store();
        let outcome = reconcile_seal(&builder.publish(), &sealed_unit, &context, &store, &LIMIT)
            .expect("reconcile");
        assert!(outcome.was_promoted());
        assert_eq!(outcome.facts().observations(), rebuilt.observations());
    }

    #[test]
    fn source_zero_dictionary_part_promotes_with_timestamped_parts() {
        let (dictionary_bytes, mut sealed_sections) = dictionary_only_part();
        let lifecycle_rows = [row(1_000, 2, None, None)];
        let lifecycle_body = PgLogLifecycleV1::encode(&lifecycle_rows).expect("encode lifecycle");
        sealed_sections.push((1_028_001, 1, lifecycle_body));
        let lifecycle_bytes = lifecycle_part(&lifecycle_rows);
        let sealed_bytes = seal_sections(&sealed_sections, 1_000, 1_000);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("cold rebuild");

        let mut builder = live_builder();
        let mut offset = 4_096_u64;
        for bytes in [&dictionary_bytes, &lifecycle_bytes] {
            let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
            let body_len = u64::try_from(bytes.len()).expect("part length fits");
            let descriptor = PartDescriptor {
                part_id: part_id(builder.generation(), offset, body_len, unit.catalog()),
                source_id: unit.catalog().source_id,
                min_ts: unit.catalog().min_ts,
                max_ts: unit.catalog().max_ts,
            };
            builder.fold_part(&descriptor, &unit).expect("fold part");
            offset = offset
                .checked_add(body_len)
                .and_then(|value| value.checked_add(4_096))
                .expect("offset fits");
        }
        assert_eq!(builder.watermark_us(), Some(1_000));

        let (_cache_dir, store) = store();
        let outcome = reconcile_seal(&builder.publish(), &sealed_unit, &context, &store, &LIMIT)
            .expect("reconcile");
        assert!(outcome.was_promoted());
        assert_eq!(outcome.facts().observations(), rebuilt.observations());
    }

    #[test]
    fn truncated_cross_part_dictionary_conflict_is_not_promoted() {
        let value = b"a pattern longer than the first truncation limit";
        let truncated_limits = DictLimits::new(1, 8).expect("truncated limits");
        let full_limits = DictLimits::new(1, 1_024).expect("full limits");
        let (first_bytes, first_sections) = error_part(value, truncated_limits, false, 1_000);
        let (second_bytes, second_sections) = error_part(value, full_limits, false, 2_000);
        let mut sealed_sections = first_sections;
        sealed_sections.extend(second_sections);
        let sealed_bytes = seal_sections(&sealed_sections, 1_000, 2_000);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");

        let mut builder = live_builder();
        let mut offset = 4_096_u64;
        for bytes in [&first_bytes, &second_bytes] {
            let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
            let body_len = u64::try_from(bytes.len()).expect("part length fits");
            let descriptor = PartDescriptor {
                part_id: part_id(builder.generation(), offset, body_len, unit.catalog()),
                source_id: unit.catalog().source_id,
                min_ts: unit.catalog().min_ts,
                max_ts: unit.catalog().max_ts,
            };
            builder
                .fold_part(&descriptor, &unit)
                .expect("fold dictionary part");
            offset = offset
                .checked_add(body_len)
                .and_then(|value| value.checked_add(4_096))
                .expect("offset fits");
        }

        let (_cache_dir, store) = store();
        assert!(matches!(
            reconcile_seal(
                &builder.publish(),
                &sealed_unit,
                &sealed_context(),
                &store,
                &LIMIT,
            ),
            Err(BuildError::Source(SourceError::Corrupt))
        ));
    }

    #[test]
    fn a_promoted_segment_answers_the_unsplit_counts() {
        let rows = stream();
        let raw = raw_oracle(&rows);
        let raw_result = raw.query(full_span(), LIMITS).expect("raw query");

        let slices: Vec<&[PgLogLifecycleV1]> = rows.chunks(2).collect();
        let sealed_bytes = sealed_from_slices(&slices);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let mut builder = live_builder();
        fold_slices(&mut builder, &slices);
        let view = builder.publish();
        let (_dir, store) = store();
        let promoted =
            reconcile_seal(&view, &sealed_unit, &context, &store, &LIMIT).expect("reconcile seal");
        assert!(promoted.was_promoted());
        let promoted_result = promoted
            .facts()
            .query(full_span(), LIMITS)
            .expect("promoted query");
        assert_eq!(promoted_result.counts(), raw_result.counts());
        assert_eq!(
            promoted_result.observations().len(),
            raw_result.observations().len()
        );
    }

    #[test]
    fn a_provenance_mismatch_falls_back_to_rebuild() {
        let rows = stream();
        // The sealed segment carries all three sections, but the candidate folded
        // only the first two parts, so the catalog concatenation cannot match.
        let all: Vec<&[PgLogLifecycleV1]> = vec![&rows[0..3], &rows[3..5], &rows[5..7]];
        let sealed_bytes = sealed_from_slices(&all);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("rebuild");

        let mut builder = live_builder();
        fold_slices(&mut builder, &[&rows[0..3], &rows[3..5]]);
        let view = builder.publish();
        let (_dir, store) = store();
        let outcome =
            reconcile_seal(&view, &sealed_unit, &context, &store, &LIMIT).expect("reconcile seal");

        assert!(
            !outcome.was_promoted(),
            "a mismatch must rebuild, not promote"
        );
        assert_eq!(outcome.facts().observations(), rebuilt.observations());
    }

    // ---- partition/seal metamorphic suite (§17.3) ----

    /// Small deterministic generator so the metamorphic sweep is reproducible
    /// without a randomness dependency.
    struct Lcg(u64);

    impl Lcg {
        const fn new(seed: u64) -> Self {
            Self(seed ^ 0x9E37_79B9_7F4A_7C15)
        }

        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % bound as u64).expect("bound fits usize")
        }
    }

    /// A canonical stream that mixes lifecycle sub-kinds, signals, and repeated
    /// timestamps so the sweep exercises duplicate-timestamp retention.
    fn long_stream(len: usize) -> Vec<PgLogLifecycleV1> {
        (0..len)
            .map(|index| {
                let ts = 1_000 + i64::try_from(index / 2).expect("timestamp fits") * 10;
                let kind = u8::try_from(index % 3).expect("kind fits");
                if kind == 0 {
                    let signal = (index % 2 == 0)
                        .then(|| i32::try_from(index % 4).expect("signal fits") + 2);
                    let pid = i32::try_from(index % 5).expect("pid fits") + 1;
                    row(ts, 0, Some(pid), signal)
                } else {
                    row(ts, kind, None, None)
                }
            })
            .collect()
    }

    fn sealed_facts_with_locator(rows: &[PgLogLifecycleV1], locator: u8) -> SegmentFacts {
        let bytes = lifecycle_part(rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open sealed group");
        let context = SegmentContext::new(
            NAMESPACE.to_vec(),
            NamingContractId([0x51; 16]),
            SegmentLocator([locator; 32]),
        )
        .expect("valid context");
        SegmentFacts::extract(&unit, &context, &LIMIT).expect("extract sealed group")
    }

    /// Random contiguous group boundaries covering `0..len`.
    fn boundaries(rng: &mut Lcg, len: usize, groups: usize) -> Vec<usize> {
        let mut cuts: Vec<usize> = (0..groups.saturating_sub(1))
            .map(|_| 1 + rng.below(len.saturating_sub(1).max(1)))
            .collect();
        cuts.push(0);
        cuts.push(len);
        cuts.sort_unstable();
        cuts.dedup();
        cuts
    }

    #[test]
    fn random_sealed_and_live_partitions_match_the_unsplit_source() {
        let rows = long_stream(24);
        let raw = raw_oracle(&rows);
        let raw_result = raw.query(full_span(), LIMITS).expect("raw query");

        for seed in 0..256_u64 {
            let mut rng = Lcg::new(seed);
            let groups = 2 + rng.below(6);
            let cuts = boundaries(&mut rng, rows.len(), groups);

            let mut merged = Vec::new();
            let mut coverage = Coverage::empty();
            let mut builder = live_builder();
            let mut live_offset = 0_u64;
            for (index, window) in cuts.windows(2).enumerate() {
                let group = &rows[window[0]..window[1]];
                if group.is_empty() {
                    continue;
                }
                if rng.below(2) == 0 {
                    let facts =
                        sealed_facts_with_locator(group, u8::try_from(index + 1).unwrap_or(1));
                    merged.extend_from_slice(facts.observations());
                    coverage = coverage.union(facts.coverage());
                } else {
                    live_offset += 4_096;
                    let bytes = lifecycle_part(group);
                    let unit = PgmUnit::open(bytes.as_slice()).expect("open live part");
                    let key = part_id(
                        builder.generation(),
                        live_offset,
                        u64::try_from(bytes.len()).expect("len fits"),
                        unit.catalog(),
                    );
                    let descriptor = PartDescriptor {
                        part_id: key,
                        source_id: unit.catalog().source_id,
                        min_ts: unit.catalog().min_ts,
                        max_ts: unit.catalog().max_ts,
                    };
                    builder
                        .fold_part(&descriptor, &unit)
                        .expect("fold live part");
                }
            }
            for chunk in builder.publish().chunks() {
                merged.extend_from_slice(chunk.observations());
                coverage = coverage.union(chunk.coverage());
            }

            let oracle = MemoryOracle::new(merged, coverage).expect("no id collision");
            let result = oracle.query(full_span(), LIMITS).expect("merged query");
            assert_eq!(
                result.counts(),
                raw_result.counts(),
                "counts diverged at seed {seed}"
            );
            assert_eq!(
                result.observations().len(),
                raw_result.observations().len(),
                "observation set changed at seed {seed}"
            );
            assert_eq!(
                envelope(result.coverage()),
                envelope(raw_result.coverage()),
                "coverage envelope changed at seed {seed}"
            );
        }
    }

    #[test]
    fn random_part_groupings_promote_to_the_cold_rebuild() {
        let rows = long_stream(20);
        for seed in 0..256_u64 {
            let mut rng = Lcg::new(seed);
            let groups = 1 + rng.below(rows.len());
            let cuts = boundaries(&mut rng, rows.len(), groups);
            let slices: Vec<&[PgLogLifecycleV1]> = cuts
                .windows(2)
                .map(|window| &rows[window[0]..window[1]])
                .filter(|slice| !slice.is_empty())
                .collect();

            let sealed_bytes = sealed_from_slices(&slices);
            let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
            let context = sealed_context();
            let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("rebuild");

            let mut builder = live_builder();
            fold_slices(&mut builder, &slices);
            let view = builder.publish();
            let (_dir, store) = store();
            let outcome =
                reconcile_seal(&view, &sealed_unit, &context, &store, &LIMIT).expect("reconcile");

            assert!(outcome.was_promoted(), "seed {seed} should promote");
            assert_eq!(
                outcome.facts().observations(),
                rebuilt.observations(),
                "promoted IDs diverged from the rebuild at seed {seed}"
            );
            assert_eq!(outcome.facts().coverage(), rebuilt.coverage());
        }
    }

    fn fallback_gap_part() -> Vec<u8> {
        let lifecycle = [row(2_000, 0, Some(11), Some(9))];
        let gaps = [kronika_registry::pg_log::PgLogGapV1 {
            ts: Ts(1_500),
            source_path: None,
            parser_kind: 0,
            reason: 15,
            dev: Some(1),
            inode: Some(2),
            offset: Some(3),
            bytes_skipped: 4,
            truncated_lines: 0,
            invalid_utf8: 0,
            binary_dropped: 0,
            rotations: 0,
            missing_files: 0,
            budget_exhaustions: 0,
            dict_dropped_fields: 0,
            parser_dropped_lines: 0,
        }];
        let lifecycle_body = PgLogLifecycleV1::encode(&lifecycle).expect("encode lifecycle");
        let gap_body = kronika_registry::pg_log::PgLogGapV1::encode(&gaps).expect("encode gap");
        build_part(
            &[
                SectionInput {
                    type_id: 1_028_001,
                    rows: 1,
                    body: &lifecycle_body,
                },
                SectionInput {
                    type_id: 1_029_001,
                    rows: 1,
                    body: &gap_body,
                },
            ],
            PartMeta {
                min_ts: 1_500,
                max_ts: 2_000,
                source_id: 7,
            },
        )
    }

    #[test]
    fn a_timestamp_fallback_gap_rebuilds_instead_of_promoting() {
        let part_bytes = fallback_gap_part();
        let sealed_unit = PgmUnit::open(part_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("rebuild");

        let mut builder = live_builder();
        let unit = PgmUnit::open(part_bytes.as_slice()).expect("open part");
        let key = part_id(
            builder.generation(),
            4_096,
            u64::try_from(part_bytes.len()).expect("len fits"),
            unit.catalog(),
        );
        let descriptor = PartDescriptor {
            part_id: key,
            source_id: unit.catalog().source_id,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        };
        builder.fold_part(&descriptor, &unit).expect("fold part");

        let (_dir, store) = store();
        let outcome = reconcile_seal(&builder.publish(), &sealed_unit, &context, &store, &LIMIT)
            .expect("reconcile seal");
        assert!(
            !outcome.was_promoted(),
            "a segment-wide timestamp fallback conservatively rebuilds"
        );
        assert_eq!(outcome.facts().observations(), rebuilt.observations());
    }

    #[test]
    fn an_incomplete_candidate_is_never_promoted() {
        let rows = stream();
        let slices: Vec<&[PgLogLifecycleV1]> = vec![&rows[0..3], &rows[3..7]];
        let sealed_bytes = sealed_from_slices(&slices);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();

        let tight = Bounds {
            items_per_block: 4,
            ..LIMIT
        };
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec(), tight).expect("valid live builder");
        fold_slices(&mut builder, &slices[0..1]);
        let bytes = lifecycle_part(slices[1]);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let key = part_id(
            builder.generation(),
            9_999,
            u64::try_from(bytes.len()).expect("len fits"),
            unit.catalog(),
        );
        let descriptor = PartDescriptor {
            part_id: key,
            source_id: unit.catalog().source_id,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        };
        assert_eq!(
            builder.fold_part(&descriptor, &unit),
            Err(LiveFoldError::LimitExceeded)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);

        let view = builder.publish();
        let (_dir, store) = store();
        let outcome =
            reconcile_seal(&view, &sealed_unit, &context, &store, &LIMIT).expect("reconcile seal");
        assert!(
            !outcome.was_promoted(),
            "a lossy candidate never becomes a sealed candidate"
        );
    }
}
