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

use std::sync::Arc;

use kronika_analytics::overview::{
    Coverage, CoverageSpan, OracleError, OracleLimits, OracleResult, RawOracle, query_bounded,
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
        Self {
            view_generation,
            sealed,
            live,
            live_queryable,
            coverage_envelope,
            fact_set_id,
            source_status,
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

    /// The precomputed union coverage of the queried sources.
    pub(crate) const fn coverage_envelope(&self) -> &Coverage {
        &self.coverage_envelope
    }

    /// The latest microsecond folded from the live generation, if any.
    pub(crate) fn data_through_us(&self) -> Option<i64> {
        if self.live_queryable {
            self.live.watermark_us()
        } else {
            None
        }
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
            hasher.update(entry.fact_key.as_bytes());
        }
        hasher.update(live.generation().0.to_le_bytes());
        hasher.update(live.folded_through_offset().to_le_bytes());
        hasher.update(live.view_generation().to_le_bytes());
        hasher.update([live_state_tag(live.state())]);
        hasher.finalize().into()
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
                source_id: 7,
            },
        )
    }

    fn sealed_entry(file: &str, min_ts: i64, max_ts: i64) -> SealedEntry {
        let bytes = sealed_bytes(min_ts, max_ts);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open sealed unit");
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
}
