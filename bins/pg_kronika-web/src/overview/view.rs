//! Atomic index view: one ordered sealed set and one live generation.
//!
//! An [`IndexView`] is the immutable snapshot a single request reads. It binds
//! an ordered set of sealed segment facts to exactly one live generation, so a
//! request never mixes a new sealed set with a stale live view.
//!
//! The view precomputes its coverage envelope and fact-set identity when it is
//! built, so a merged query neither re-clones the chunk vector nor recomputes
//! the live envelope per query. The refresh cycle is the single writer: it
//! publishes each fresh view into an `ArcSwap`, and requests read it lock-free.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kronika_analytics::overview::{
    Coverage, CoverageSpan, OracleError, OracleLimits, OracleResult, PhysicalCountSemantics,
    RawOracle, RetainedExactness, SourceCompleteness, SourceScopeId, query_bounded,
    query_bounded_materialized,
};
use kronika_reader::{FactKey, FileKind, LiveState, LiveView, SegmentDescriptor, SegmentFacts};
use sha2::{Digest, Sha256};

/// Domain separator for the response/cache fact-set identity.
const FACT_SET_ID_DOMAIN: &[u8] = b"pgk-overview-fact-set-id-v1";

/// The source-completeness status of an index view for the wire contract.
///
/// The status reports the completeness of the selected retained contract, not
/// of the physical `PostgreSQL` log, which the collector cannot prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceStatus {
    /// The live generation is folded through its watermark; sealed set intact.
    CompleteForContract,
    /// The live state is incomplete under a hard bound; sealed set still served.
    Partial,
    /// A restart or full rescan has not yet folded the journal tail.
    Warming,
    /// Append continuity or identity could not be proven for the live journal.
    Gap,
}

impl SourceStatus {
    /// The stable wire code of this status.
    pub(crate) const fn wire_code(self) -> &'static str {
        match self {
            Self::CompleteForContract => "complete_for_contract",
            Self::Partial => "partial",
            Self::Warming => "warming",
            Self::Gap => "gap",
        }
    }

    /// Derives a status from a published live state.
    const fn from_live_state(state: LiveState) -> Self {
        match state {
            LiveState::Empty | LiveState::Current => Self::CompleteForContract,
            LiveState::Warming => Self::Warming,
            LiveState::NeedsRebuild => Self::Gap,
            LiveState::Incomplete => Self::Partial,
        }
    }
}

/// Selected-source metadata retained independently from event presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceMetadata {
    pub(crate) source_id: u64,
    pub(crate) source_scope_id: Option<SourceScopeId>,
    pub(crate) data_through_us: Option<i64>,
    pub(crate) covered: Coverage,
    pub(crate) known_gaps: Coverage,
    pub(crate) source_completeness: SourceCompleteness,
    pub(crate) retained_exactness: RetainedExactness,
    pub(crate) physical_count: PhysicalCountSemantics,
    pub(crate) dropped_lower_bound: Option<u64>,
}

/// Metadata aggregation can fail rather than publishing an invented count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataError {
    /// A proven dropped-record lower bound exceeded `u64`.
    CountOverflow,
    /// The selected metadata exceeded its explicit coverage/gap span budget.
    SpanLimitExceeded,
    /// One numeric source resolved to contradictory scope identities.
    SourceScopeConflict,
}

#[derive(Debug)]
struct SourceAccumulator {
    source_scope_id: Option<SourceScopeId>,
    data_through_us: Option<i64>,
    covered: Vec<CoverageSpan>,
    known_gaps: Vec<CoverageSpan>,
    source_completeness: Option<SourceCompleteness>,
    retained_exactness: Option<RetainedExactness>,
    physical_count: Option<PhysicalCountSemantics>,
    dropped_lower_bound: Option<u64>,
    dropped_count_unavailable: bool,
}

/// One sealed segment bound into an index view.
#[derive(Debug, Clone)]
pub(crate) struct SealedEntry {
    descriptor: SegmentDescriptor,
    facts: Arc<SegmentFacts>,
    fact_key: FactKey,
}

impl SealedEntry {
    /// Binds sealed facts, computing the content-addressed fact key.
    pub(crate) fn new(descriptor: SegmentDescriptor, facts: Arc<SegmentFacts>) -> Self {
        let fact_key = FactKey::for_identity(facts.identity(), FileKind::SegmentFacts);
        Self {
            descriptor,
            facts,
            fact_key,
        }
    }

    /// The content-bound descriptor of the sealed segment.
    pub(crate) const fn descriptor(&self) -> &SegmentDescriptor {
        &self.descriptor
    }

    fn facts(&self) -> &SegmentFacts {
        &self.facts
    }
}

/// An atomic snapshot of ordered sealed facts and one live generation.
#[derive(Debug, Clone)]
pub(crate) struct IndexView {
    view_generation: u64,
    sealed: Vec<SealedEntry>,
    live: Arc<LiveView>,
    live_queryable: bool,
    coverage_envelope: Coverage,
    fact_set_id: [u8; 32],
    source_status: SourceStatus,
    source_ids: Vec<u64>,
}

impl IndexView {
    /// Publishes a view over an ordered sealed set and one live generation.
    ///
    /// The sealed entries are expected in canonical `(min_ts, locator)` order.
    /// The live generation is included in queries only when its state is
    /// authoritative (`Empty` or `Current`); otherwise the view serves the
    /// sealed set alone and reports the live state through [`SourceStatus`].
    ///
    /// `sealed_gap` marks that a sealed segment could not be loaded, so the
    /// status is a source gap rather than the live state alone.
    pub(crate) fn new(
        view_generation: u64,
        sealed: Vec<SealedEntry>,
        live: Arc<LiveView>,
        sealed_gap: bool,
    ) -> Self {
        let live_queryable = matches!(live.state(), LiveState::Empty | LiveState::Current);
        let coverage_envelope = Self::build_envelope(&sealed, &live, live_queryable);
        let source_status = if sealed_gap {
            SourceStatus::Gap
        } else {
            SourceStatus::from_live_state(live.state())
        };
        let fact_set_id = Self::derive_fact_set_id(view_generation, &sealed, &live);
        let source_ids = Self::collect_source_ids(&sealed, &live, live_queryable);
        Self {
            view_generation,
            sealed,
            live,
            live_queryable,
            coverage_envelope,
            fact_set_id,
            source_status,
            source_ids,
        }
    }

    /// The monotonic generation of this published view.
    pub(crate) const fn view_generation(&self) -> u64 {
        self.view_generation
    }

    /// The response/cache fact-set identity for this view (§11.2).
    pub(crate) const fn fact_set_id(&self) -> [u8; 32] {
        self.fact_set_id
    }

    /// The source-completeness status for the wire contract.
    pub(crate) const fn source_status(&self) -> SourceStatus {
        self.source_status
    }

    /// Canonical numeric PGM source IDs represented by this view.
    pub(crate) fn source_ids(&self) -> &[u64] {
        &self.source_ids
    }

    /// Maps a retained source-scope identity back to its numeric PGM source.
    pub(crate) fn source_id_for_scope(&self, scope: SourceScopeId) -> Option<u64> {
        self.queryable_facts().find_map(|facts| {
            (facts.identity().source_scope_id == scope).then_some(facts.identity().pgm_source_id)
        })
    }

    /// The precomputed union coverage of the queried sources.
    pub(crate) const fn coverage_envelope(&self) -> &Coverage {
        &self.coverage_envelope
    }

    /// The latest microsecond folded from the live generation, if any.
    pub(crate) fn data_through_us(&self) -> Option<i64> {
        self.data_through_us_for(&self.source_ids)
    }

    /// Latest selected sealed/live timestamp.
    pub(crate) fn data_through_us_for(&self, sources: &[u64]) -> Option<i64> {
        self.queryable_facts()
            .filter(|facts| source_selected(sources, facts.identity().pgm_source_id))
            .map(|facts| facts.identity().source_max_ts_us)
            .max()
    }

    /// Returns sorted, range-clipped metadata for every requested source.
    ///
    /// An unknown source remains explicit with unknown quality instead of
    /// becoming an exact empty result.
    #[allow(
        clippy::too_many_lines,
        reason = "all independent quality axes must be folded in the same bounded fact pass"
    )]
    pub(crate) fn selected_source_metadata(
        &self,
        sources: &[u64],
        range: CoverageSpan,
        max_spans: usize,
    ) -> Result<Vec<SourceMetadata>, MetadataError> {
        let mut selected = sources
            .iter()
            .map(|source_id| {
                (
                    *source_id,
                    SourceAccumulator {
                        source_scope_id: None,
                        data_through_us: None,
                        covered: Vec::new(),
                        known_gaps: Vec::new(),
                        source_completeness: None,
                        retained_exactness: None,
                        physical_count: None,
                        dropped_lower_bound: None,
                        dropped_count_unavailable: false,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut remaining_spans = max_spans;
        for facts in self.queryable_facts() {
            let identity = facts.identity();
            let Some(accumulator) = selected.get_mut(&identity.pgm_source_id) else {
                continue;
            };
            match accumulator.source_scope_id {
                Some(scope) if scope != identity.source_scope_id => {
                    return Err(MetadataError::SourceScopeConflict);
                }
                None => accumulator.source_scope_id = Some(identity.source_scope_id),
                Some(_) => {}
            }
            accumulator.data_through_us = Some(
                accumulator
                    .data_through_us
                    .map_or(identity.source_max_ts_us, |current| {
                        current.max(identity.source_max_ts_us)
                    }),
            );
            if identity.source_max_ts_us < range.start_us()
                || identity.source_min_ts_us >= range.end_us()
            {
                continue;
            }
            let loss = facts.loss_coverage();
            extend_clipped(
                &mut accumulator.covered,
                loss.covered(),
                range,
                &mut remaining_spans,
            )?;
            extend_clipped(
                &mut accumulator.known_gaps,
                loss.known_gaps(),
                range,
                &mut remaining_spans,
            )?;
            accumulator.source_completeness = Some(accumulator.source_completeness.map_or_else(
                || loss.source_completeness(),
                |current| merge_source_completeness(current, loss.source_completeness()),
            ));
            accumulator.retained_exactness = Some(accumulator.retained_exactness.map_or_else(
                || loss.retained_exactness(),
                |current| merge_retained_exactness(current, loss.retained_exactness()),
            ));
            accumulator.physical_count = Some(accumulator.physical_count.map_or_else(
                || loss.physical_count(),
                |current| merge_physical_count(current, loss.physical_count()),
            ));
            let fact_fully_selected = identity.source_min_ts_us >= range.start_us()
                && identity.source_max_ts_us < range.end_us();
            if loss.dropped_lower_bound() != 0 && !fact_fully_selected {
                accumulator.dropped_count_unavailable = true;
                accumulator.dropped_lower_bound = None;
            } else if !accumulator.dropped_count_unavailable {
                accumulator.dropped_lower_bound = Some(
                    accumulator
                        .dropped_lower_bound
                        .unwrap_or(0)
                        .checked_add(loss.dropped_lower_bound())
                        .ok_or(MetadataError::CountOverflow)?,
                );
            }
        }
        Ok(selected
            .into_iter()
            .map(|(source_id, accumulator)| SourceMetadata {
                source_id,
                source_scope_id: accumulator.source_scope_id,
                data_through_us: accumulator.data_through_us,
                covered: Coverage::from_spans(accumulator.covered),
                known_gaps: Coverage::from_spans(accumulator.known_gaps),
                source_completeness: accumulator
                    .source_completeness
                    .unwrap_or(SourceCompleteness::Unknown),
                retained_exactness: accumulator
                    .retained_exactness
                    .unwrap_or(RetainedExactness::Unknown),
                physical_count: accumulator
                    .physical_count
                    .unwrap_or(PhysicalCountSemantics::NotApplicable),
                dropped_lower_bound: accumulator.dropped_lower_bound,
            })
            .collect())
    }

    /// Queries only the canonical selected source set.
    pub(crate) fn query_sources(
        &self,
        sources: &[u64],
        range: CoverageSpan,
        limits: OracleLimits,
        max_materialized_bytes: usize,
    ) -> Result<OracleResult, OracleError> {
        let sealed_observations = self
            .sealed
            .iter()
            .filter(|entry| source_selected(sources, entry.descriptor.source_id))
            .flat_map(|entry| entry.facts().observations());
        let sealed_spans = self
            .sealed
            .iter()
            .filter(|entry| source_selected(sources, entry.descriptor.source_id))
            .flat_map(|entry| entry.facts().coverage().spans().iter().copied());
        if self.live_queryable {
            let live_observations = self
                .live
                .chunks()
                .iter()
                .filter(|facts| source_selected(sources, facts.identity().pgm_source_id))
                .flat_map(|facts| facts.observations());
            let live_spans = self
                .live
                .chunks()
                .iter()
                .filter(|facts| source_selected(sources, facts.identity().pgm_source_id))
                .flat_map(|facts| facts.coverage().spans().iter().copied());
            query_bounded_materialized(
                sealed_observations.chain(live_observations),
                sealed_spans.chain(live_spans),
                range,
                limits,
                max_materialized_bytes,
            )
        } else {
            query_bounded_materialized(
                sealed_observations,
                sealed_spans,
                range,
                limits,
                max_materialized_bytes,
            )
        }
    }

    /// Checked logical resident charge retained while a cursor pins this view.
    ///
    /// The charge includes reserved container slots, `Arc` counters, sealed and
    /// live fact allocations, coverage, and source IDs. It returns `None`
    /// instead of saturating if a platform-sized total cannot be represented.
    pub(crate) fn resident_bytes(&self) -> Option<usize> {
        const ARC_COUNTER_BYTES: usize = 2 * size_of::<usize>();

        let sealed_slots = self
            .sealed
            .capacity()
            .checked_mul(size_of::<SealedEntry>())?;
        let sealed = self.sealed.iter().try_fold(0_usize, |total, entry| {
            total
                .checked_add(ARC_COUNTER_BYTES)?
                .checked_add(entry.facts().resident_bytes()?)
        })?;
        let coverage = self.coverage_envelope().resident_heap_bytes()?;
        let sources = self.source_ids.capacity().checked_mul(size_of::<u64>())?;

        size_of::<Self>()
            .checked_add(ARC_COUNTER_BYTES)?
            .checked_add(sealed_slots)?
            .checked_add(sealed)?
            .checked_add(ARC_COUNTER_BYTES)?
            .checked_add(self.live.resident_bytes()?)?
            .checked_add(coverage)?
            .checked_add(sources)
    }

    fn build_envelope(sealed: &[SealedEntry], live: &LiveView, live_queryable: bool) -> Coverage {
        let mut envelope = Coverage::empty();
        for entry in sealed {
            envelope = envelope.union(entry.facts.coverage());
        }
        if live_queryable {
            envelope = envelope.union(&live.coverage());
        }
        envelope
    }

    fn derive_fact_set_id(
        view_generation: u64,
        sealed: &[SealedEntry],
        live: &LiveView,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(FACT_SET_ID_DOMAIN);
        hasher.update(view_generation.to_le_bytes());
        let sealed_count = u64::try_from(sealed.len()).unwrap_or(u64::MAX);
        hasher.update(sealed_count.to_le_bytes());
        for entry in sealed {
            hasher.update(entry.descriptor.locator.as_bytes());
            hasher.update(entry.descriptor.source_id.to_le_bytes());
            hasher.update(entry.fact_key.as_bytes());
        }
        hasher.update(live.generation().0.to_le_bytes());
        hasher.update(live.folded_through_offset().to_le_bytes());
        hasher.update(live.view_generation().to_le_bytes());
        hasher.update([live_state_tag(live.state())]);
        hasher.finalize().into()
    }

    fn collect_source_ids(
        sealed: &[SealedEntry],
        live: &LiveView,
        live_queryable: bool,
    ) -> Vec<u64> {
        sealed
            .iter()
            .map(|entry| entry.descriptor.source_id)
            .chain(
                live_queryable
                    .then_some(live.chunks().iter())
                    .into_iter()
                    .flatten()
                    .map(|facts| facts.identity().pgm_source_id),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn queryable_facts(&self) -> impl Iterator<Item = &SegmentFacts> {
        self.sealed.iter().map(SealedEntry::facts).chain(
            self.live_queryable
                .then_some(self.live.chunks().iter())
                .into_iter()
                .flatten()
                .map(AsRef::as_ref),
        )
    }
}

impl RawOracle for IndexView {
    fn query(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<OracleResult, OracleError> {
        let sealed_observations = self
            .sealed
            .iter()
            .flat_map(|entry| entry.facts.observations());
        let spans = self.coverage_envelope.spans().iter().copied();
        if self.live_queryable {
            let live_observations = self
                .live
                .chunks()
                .iter()
                .flat_map(|chunk| chunk.observations());
            query_bounded(
                sealed_observations.chain(live_observations),
                spans,
                range,
                limits,
            )
        } else {
            query_bounded(sealed_observations, spans, range, limits)
        }
    }
}

fn source_selected(sources: &[u64], source: u64) -> bool {
    sources.binary_search(&source).is_ok()
}

const fn merge_source_completeness(
    left: SourceCompleteness,
    right: SourceCompleteness,
) -> SourceCompleteness {
    match (left, right) {
        (SourceCompleteness::Unknown, _) | (_, SourceCompleteness::Unknown) => {
            SourceCompleteness::Unknown
        }
        (SourceCompleteness::BoundedSubset, _) | (_, SourceCompleteness::BoundedSubset) => {
            SourceCompleteness::BoundedSubset
        }
        (SourceCompleteness::Full, SourceCompleteness::Full) => SourceCompleteness::Full,
    }
}

const fn merge_retained_exactness(
    left: RetainedExactness,
    right: RetainedExactness,
) -> RetainedExactness {
    match (left, right) {
        (RetainedExactness::Unknown, _) | (_, RetainedExactness::Unknown) => {
            RetainedExactness::Unknown
        }
        (RetainedExactness::LowerBound, _) | (_, RetainedExactness::LowerBound) => {
            RetainedExactness::LowerBound
        }
        (RetainedExactness::Exact, RetainedExactness::Exact) => RetainedExactness::Exact,
    }
}

const fn merge_physical_count(
    left: PhysicalCountSemantics,
    right: PhysicalCountSemantics,
) -> PhysicalCountSemantics {
    match (left, right) {
        (PhysicalCountSemantics::Unknown, _)
        | (_, PhysicalCountSemantics::Unknown)
        | (PhysicalCountSemantics::Exact, PhysicalCountSemantics::NotApplicable)
        | (PhysicalCountSemantics::NotApplicable, PhysicalCountSemantics::Exact) => {
            PhysicalCountSemantics::Unknown
        }
        (PhysicalCountSemantics::LowerBound, _) | (_, PhysicalCountSemantics::LowerBound) => {
            PhysicalCountSemantics::LowerBound
        }
        (PhysicalCountSemantics::Exact, PhysicalCountSemantics::Exact) => {
            PhysicalCountSemantics::Exact
        }
        (PhysicalCountSemantics::NotApplicable, PhysicalCountSemantics::NotApplicable) => {
            PhysicalCountSemantics::NotApplicable
        }
    }
}

fn extend_clipped(
    output: &mut Vec<CoverageSpan>,
    coverage: &Coverage,
    range: CoverageSpan,
    remaining_spans: &mut usize,
) -> Result<(), MetadataError> {
    for span in coverage.spans() {
        let Some(clipped) = CoverageSpan::new(
            span.start_us().max(range.start_us()),
            span.end_us().min(range.end_us()),
        ) else {
            continue;
        };
        if *remaining_spans == 0 {
            return Err(MetadataError::SpanLimitExceeded);
        }
        output.push(clipped);
        *remaining_spans -= 1;
    }
    Ok(())
}

const fn live_state_tag(state: LiveState) -> u8 {
    match state {
        LiveState::Empty => 0,
        LiveState::Warming => 1,
        LiveState::Current => 2,
        LiveState::NeedsRebuild => 3,
        LiveState::Incomplete => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kronika_analytics::overview::{CountLimits, OracleSourceError};
    use kronika_analytics::overview::{NamingContractId, SegmentLocator};
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_reader::{
        JournalDelta, JournalGenerationId, LIMIT, LiveBuilder, PartTransition, PgmUnit,
        RefreshDelta, SealedLocator, SegmentContext,
    };
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    const LIMITS: OracleLimits = OracleLimits {
        max_observations: 4096,
        max_coverage_spans: 4096,
        count_limits: CountLimits {
            max_input_entries: 65_536,
            max_joint_keys: 65_536,
            max_signal_keys: 1_024,
        },
    };

    fn full_span() -> CoverageSpan {
        CoverageSpan::new(i64::MIN + 1, i64::MAX).expect("valid full span")
    }

    fn sealed_bytes(min_ts: i64, max_ts: i64) -> Vec<u8> {
        sealed_bytes_for_source(min_ts, max_ts, 7)
    }

    fn sealed_bytes_for_source(min_ts: i64, max_ts: i64, source_id: u64) -> Vec<u8> {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id,
            },
        )
    }

    fn sealed_entry(file: &str, min_ts: i64, max_ts: i64) -> SealedEntry {
        let bytes = sealed_bytes(min_ts, max_ts);
        sealed_entry_from_bytes(file, &bytes)
    }

    fn sealed_entry_for_source(
        file: &str,
        min_ts: i64,
        max_ts: i64,
        source_id: u64,
    ) -> SealedEntry {
        let bytes = sealed_bytes_for_source(min_ts, max_ts, source_id);
        sealed_entry_from_bytes(file, &bytes)
    }

    fn sealed_entry_from_bytes(file: &str, bytes: &[u8]) -> SealedEntry {
        let unit = PgmUnit::open(bytes).expect("open sealed unit");
        let locator = SealedLocator::from_file_name_bytes(file.as_bytes());
        let descriptor = SegmentDescriptor::from_catalog(locator, unit.catalog());
        let context = SegmentContext::new(
            b"deployment".to_vec(),
            NamingContractId([9; 16]),
            SegmentLocator(*locator.as_bytes()),
        )
        .expect("valid segment context");
        let facts = SegmentFacts::extract(&unit, &context, &LIMIT).expect("extract facts");
        SealedEntry::new(descriptor, Arc::new(facts))
    }

    fn empty_live() -> Arc<LiveView> {
        let mut builder = LiveBuilder::new(b"deployment".to_vec(), LIMIT).expect("live builder");
        let delta = RefreshDelta {
            previous_view_generation: 0,
            new_view_generation: 1,
            view_changed: true,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: JournalDelta {
                bootstrap: true,
                generation_id: JournalGenerationId(1),
                previous_valid_len: 0,
                new_valid_len: 0,
                completed_parts: Vec::new(),
                current_parts: Vec::new(),
                current_parts_complete: true,
                transition: PartTransition::Append,
                tail_pending: None,
                damages: Vec::new(),
            },
        };
        builder.begin_refresh(&delta).expect("begin empty refresh");
        builder.complete_refresh().expect("complete empty refresh");
        Arc::new(builder.publish())
    }

    fn warming_live() -> Arc<LiveView> {
        let builder = LiveBuilder::new(b"deployment".to_vec(), LIMIT).expect("live builder");
        Arc::new(builder.publish())
    }

    #[test]
    fn empty_view_has_no_observations_and_a_stable_identity() {
        let live = empty_live();
        let view = IndexView::new(1, Vec::new(), live, false);
        let result = view.query(full_span(), LIMITS).expect("query empty view");
        assert!(result.observations().is_empty());
        assert_eq!(view.source_status(), SourceStatus::CompleteForContract);
        let again = IndexView::new(1, Vec::new(), empty_live(), false);
        assert_eq!(
            view.fact_set_id(),
            again.fact_set_id(),
            "identical inputs derive an identical fact-set id"
        );
    }

    #[test]
    fn sealed_facts_are_queryable_and_bound_the_envelope() {
        let entry = sealed_entry("143000.pgm", 1_000, 2_000);
        let view = IndexView::new(2, vec![entry], empty_live(), false);
        // The bgwriter fixture retains no events, but the facts and their
        // coverage are real: the envelope is non-empty and the query succeeds.
        let result = view.query(full_span(), LIMITS).expect("query sealed view");
        assert!(result.observations().is_empty());
        assert!(
            !view.coverage_envelope().is_empty(),
            "sealed coverage bounds the view"
        );
    }

    #[test]
    fn a_generation_change_rekeys_the_fact_set_id() {
        let one = IndexView::new(1, Vec::new(), empty_live(), false);
        let two = IndexView::new(2, Vec::new(), empty_live(), false);
        assert_ne!(
            one.fact_set_id(),
            two.fact_set_id(),
            "a new view generation must re-key the fact set"
        );
    }

    #[test]
    fn a_warming_live_view_is_excluded_and_reported() {
        let view = IndexView::new(1, Vec::new(), warming_live(), false);
        assert_eq!(view.source_status(), SourceStatus::Warming);
        assert_eq!(view.data_through_us(), None);
        // A warming live view is unqueryable on its own; the merged view still
        // answers (over the empty sealed set) rather than propagating the error.
        let result = view
            .query(full_span(), LIMITS)
            .expect("merged query ignores warming live");
        assert!(result.observations().is_empty());
        // A fresh live builder is unqueryable directly, proving the merge gates
        // it out rather than surfacing an error.
        assert_eq!(
            warming_live().query(full_span(), LIMITS),
            Err(OracleError::Source(OracleSourceError::SnapshotUnavailable))
        );
    }

    #[test]
    fn selected_metadata_is_source_and_range_scoped() {
        let view = IndexView::new(
            3,
            vec![
                sealed_entry_for_source("source-7.pgm", 1_000, 2_000, 7),
                sealed_entry_for_source("source-8.pgm", 3_000, 4_000, 8),
            ],
            empty_live(),
            false,
        );
        let range = CoverageSpan::new(900, 2_100).expect("range");
        let selected = view
            .selected_source_metadata(&[7], range, 16)
            .expect("selected metadata");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].source_id, 7);
        assert!(selected[0].source_scope_id.is_some());
        assert_eq!(selected[0].data_through_us, Some(2_000));
        assert!(!selected[0].covered.is_empty());
        assert_eq!(
            selected[0].source_completeness,
            SourceCompleteness::BoundedSubset
        );
        assert_eq!(selected[0].retained_exactness, RetainedExactness::Exact);
        assert_eq!(
            selected[0].physical_count,
            PhysicalCountSemantics::LowerBound
        );
    }

    #[test]
    fn unknown_and_disjoint_sources_do_not_invent_exact_zero_loss() {
        let view = IndexView::new(
            3,
            vec![sealed_entry("source-7.pgm", 1_000, 2_000)],
            empty_live(),
            false,
        );
        let unknown = view
            .selected_source_metadata(&[999], CoverageSpan::new(0, 10_000).expect("range"), 16)
            .expect("unknown metadata");
        assert_eq!(unknown[0].source_scope_id, None);
        assert_eq!(unknown[0].source_completeness, SourceCompleteness::Unknown);
        assert_eq!(unknown[0].dropped_lower_bound, None);

        let disjoint = view
            .selected_source_metadata(&[7], CoverageSpan::new(10_000, 20_000).expect("range"), 16)
            .expect("disjoint metadata");
        assert!(disjoint[0].source_scope_id.is_some());
        assert_eq!(disjoint[0].retained_exactness, RetainedExactness::Unknown);
        assert_eq!(disjoint[0].dropped_lower_bound, None);
    }

    #[test]
    fn selected_metadata_enforces_its_span_budget_before_retaining() {
        let view = IndexView::new(
            3,
            vec![sealed_entry("source-7.pgm", 1_000, 2_000)],
            empty_live(),
            false,
        );
        assert_eq!(
            view.selected_source_metadata(&[7], CoverageSpan::new(0, 10_000).expect("range"), 0,),
            Err(MetadataError::SpanLimitExceeded)
        );
    }
}
