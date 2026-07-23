//! Lossless chunked fold of completed active parts into a live overview view.
//!
//! [`LiveBuilder`] is the single mutable writer: it folds each completed,
//! CRC-valid active part into live facts exactly once, keyed by [`PartId`] so a
//! re-delivered part is a no-op. Folded facts live as immutable chunks; a
//! [`LiveView`] snapshot clones the chunk list — shared pointers, not copied
//! observations — so a torn or pending tail never moves the folded watermark and
//! publication does not duplicate the growing record set.
//!
//! A [`LiveView`] answers the same bounded query contract as a sealed segment:
//! it merges its chunks into one canonical observation set and unioned coverage.
//! Counts fold from observation content, so a live view over parts of a stream
//! reports the same counts and coverage envelope as the unsplit source.

use std::collections::BTreeSet;
use std::sync::Arc;

use kronika_analytics::overview::{
    Coverage, CoverageSpan, MemoryOracle, OracleError, OracleLimits, OracleResult, RawOracle,
};
use kronika_format::ReadAt;

use crate::refresh::{JournalGenerationId, PartDescriptor, PartId};
use crate::unit::PgmUnit;

use super::facts::{BuildError, SegmentContext, SegmentFacts};
use super::limits::Bounds;
use super::publish::{FactLoad, FactStore};

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
    /// A folded offset or watermark exceeded the checked integer range.
    Overflow,
}

impl std::fmt::Display for LiveFoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GenerationMismatch => f.write_str("part belongs to a different generation"),
            Self::Build(error) => write!(f, "completed part extraction failed: {error}"),
            Self::Overflow => f.write_str("folded offset or watermark overflow"),
        }
    }
}

impl std::error::Error for LiveFoldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::GenerationMismatch | Self::Overflow => None,
        }
    }
}

/// The single mutable writer that folds completed parts into live facts.
#[derive(Debug, Clone)]
pub struct LiveBuilder {
    store_namespace: Vec<u8>,
    state: LiveState,
    generation: JournalGenerationId,
    folded_part_ids: BTreeSet<PartId>,
    chunks: Vec<Arc<SegmentFacts>>,
    watermark_us: Option<i64>,
    folded_through_offset: u64,
}

impl LiveBuilder {
    /// Creates an empty builder scoped to a store namespace.
    #[must_use]
    pub fn new(store_namespace: impl Into<Vec<u8>>) -> Self {
        Self {
            store_namespace: store_namespace.into(),
            state: LiveState::Empty,
            generation: JournalGenerationId(0),
            folded_part_ids: BTreeSet::new(),
            chunks: Vec::new(),
            watermark_us: None,
            folded_through_offset: 0,
        }
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
        bounds: &Bounds,
    ) -> Result<FoldEffect, LiveFoldError> {
        if part.part_id.generation != self.generation {
            return Err(LiveFoldError::GenerationMismatch);
        }
        if self.folded_part_ids.contains(&part.part_id) {
            return Ok(FoldEffect::Duplicate);
        }
        let end_offset = part
            .part_id
            .frame_offset
            .checked_add(part.part_id.body_len)
            .ok_or(LiveFoldError::Overflow)?;
        let discriminator = part_discriminator(&part.part_id);
        let facts = SegmentFacts::fold_live(
            unit,
            &self.store_namespace,
            self.generation.0,
            &discriminator,
            bounds,
        )
        .map_err(|error| {
            self.state = LiveState::Incomplete;
            LiveFoldError::Build(error)
        })?;

        self.chunks.push(Arc::new(facts));
        self.folded_part_ids.insert(part.part_id);
        self.watermark_us = Some(
            self.watermark_us
                .map_or(part.max_ts, |current| current.max(part.max_ts)),
        );
        self.folded_through_offset = self.folded_through_offset.max(end_offset);
        if self.state != LiveState::Incomplete {
            self.state = LiveState::Current;
        }
        Ok(FoldEffect::Folded)
    }

    /// Publishes an immutable snapshot of the folded view.
    ///
    /// The chunk list is cloned as shared pointers, so publication reuses every
    /// unchanged chunk instead of copying the growing observation set.
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

    /// Merged coverage across every chunk as a union of half-open spans.
    #[must_use]
    pub fn coverage(&self) -> Coverage {
        let mut coverage = Coverage::empty();
        for chunk in &self.chunks {
            coverage = coverage.union(chunk.coverage());
        }
        coverage
    }

    fn merged_oracle(&self) -> Result<MemoryOracle, OracleError> {
        let mut observations = Vec::new();
        for chunk in &self.chunks {
            observations.extend_from_slice(chunk.observations());
        }
        MemoryOracle::new(observations, self.coverage())
    }
}

impl RawOracle for LiveView {
    fn query(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<OracleResult, OracleError> {
        self.merged_oracle()?.query(range, limits)
    }
}

/// Outcome of reconciling a newly sealed segment against the live view.
#[derive(Debug)]
pub enum SealOutcome {
    /// The live candidate matched the sealed provenance and was promoted without
    /// reading the sealed section bodies.
    Promoted(SegmentFacts),
    /// The candidate was lossy, absent, or its provenance did not match, so the
    /// facts were rebuilt from the PGM.
    Rebuilt(FactLoad),
}

impl SealOutcome {
    /// The reconciled sealed facts, whichever path produced them.
    #[must_use]
    pub const fn facts(&self) -> &SegmentFacts {
        match self {
            Self::Promoted(facts) => facts,
            Self::Rebuilt(load) => load.facts(),
        }
    }

    /// Whether the live candidate was promoted rather than rebuilt.
    #[must_use]
    pub const fn was_promoted(&self) -> bool {
        matches!(self, Self::Promoted(_))
    }
}

/// Reconciles a newly sealed segment against the current live view.
///
/// A `Current`, lossless candidate whose provenance exactly matches the sealed
/// segment is promoted without reading the sealed section bodies. Any other case
/// — a lossy or non-current candidate, or a provenance mismatch — rebuilds the
/// facts from the PGM through the durable fact store. Promotion never depends on
/// response caps and never emits a segment whose records differ from a cold
/// rebuild.
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
    if candidate.is_current()
        && let Some(promoted) = SegmentFacts::try_promote_from_parts(
            sealed_unit,
            sealed_context,
            candidate.chunks(),
            bounds,
        )?
    {
        return Ok(SealOutcome::Promoted(promoted));
    }
    let load = store.load_or_build(sealed_unit, sealed_context, bounds)?;
    Ok(SealOutcome::Rebuilt(load))
}

/// Serializes a part key into a unique per-part live-lineage discriminator.
///
/// Two parts at different journal positions get different discriminators, so
/// identical section bodies in different parts fold into distinct observation
/// identities and neither is lost.
fn part_discriminator(part_id: &PartId) -> [u8; 28] {
    let mut bytes = [0_u8; 28];
    bytes[0..8].copy_from_slice(&part_id.generation.0.to_le_bytes());
    bytes[8..16].copy_from_slice(&part_id.frame_offset.to_le_bytes());
    bytes[16..24].copy_from_slice(&part_id.body_len.to_le_bytes());
    bytes[24..28].copy_from_slice(&part_id.catalog_digest.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{CountLimits, NamingContractId, SegmentLocator};
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};

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

    fn sealed_context() -> SegmentContext {
        SegmentContext::new(
            NAMESPACE.to_vec(),
            NamingContractId([0x51; 16]),
            SegmentLocator([0x52; 32]),
        )
        .expect("valid context")
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
            builder
                .fold_part(&descriptor, &unit, &LIMIT)
                .expect("fold part");
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
        let builder = LiveBuilder::new(NAMESPACE.to_vec());
        assert_eq!(builder.state(), LiveState::Empty);
        assert_eq!(builder.folded_part_count(), 0);
        assert_eq!(builder.watermark_us(), None);
    }

    #[test]
    fn folding_a_completed_part_advances_the_watermark_and_becomes_current() {
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
        fold_slices(&mut builder, &[&stream()]);
        assert_eq!(builder.state(), LiveState::Current);
        assert_eq!(builder.folded_part_count(), 1);
        assert_eq!(builder.watermark_us(), Some(4_000));
    }

    #[test]
    fn re_delivering_the_same_part_is_idempotent() {
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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
            builder.fold_part(&descriptor, &unit, &LIMIT).expect("fold"),
            FoldEffect::Folded
        );
        assert_eq!(
            builder
                .fold_part(&descriptor, &unit, &LIMIT)
                .expect("re-fold"),
            FoldEffect::Duplicate
        );
        assert_eq!(builder.folded_part_count(), 1, "redelivery adds no chunk");
    }

    #[test]
    fn a_part_from_a_different_generation_is_rejected() {
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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
            builder.fold_part(&descriptor, &unit, &LIMIT),
            Err(LiveFoldError::GenerationMismatch)
        );
    }

    #[test]
    fn reset_discards_folded_state_and_enters_warming() {
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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
            let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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

        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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

            let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
            fold_slices(&mut builder, &slices);
            let view = builder.publish();
            let (_dir, store) = store();
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
        }
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
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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

        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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
            let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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
                        .fold_part(&descriptor, &unit, &LIMIT)
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

            let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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

        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
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
        builder
            .fold_part(&descriptor, &unit, &LIMIT)
            .expect("fold part");

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

        let mut builder = LiveBuilder::new(NAMESPACE.to_vec());
        fold_slices(&mut builder, &slices[0..1]);
        // Fold the second part under a tight item bound so extraction fails and
        // the view becomes lossy.
        let tight = Bounds {
            items_per_block: 1,
            ..LIMIT
        };
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
        assert!(builder.fold_part(&descriptor, &unit, &tight).is_err());
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
