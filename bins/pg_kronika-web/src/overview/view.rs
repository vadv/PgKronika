//! Immutable descriptor authority and request-scoped timeline facts.
//!
//! Refresh publishes a [`DescriptorView`] with catalog-derived sealed
//! identities and one live generation. After descriptor admission, an
//! [`IndexView`] binds only the selected sealed facts to that same generation.
//! A request therefore cannot mix publication generations or traverse
//! unselected sealed fact bodies.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Arc;

use kronika_analytics::overview::{
    CounterSample, Coverage, CoverageSpan, EventFact as CanonicalEventFact, FactId, FactorCoverage,
    GaugeSample, MetricFactor, MetricSeriesDescriptor, MetricSeriesId, OracleError, OracleLimits,
    OracleResult, PhysicalCountSemantics, RawOracle, RetainedExactness, SourceCompleteness,
    query_bounded, query_bounded_materialized,
};
use kronika_reader::{
    EntityStateRecord, FactBuildKey, FactKey, FileKind, LiveState, LiveView, SealedLocator,
    SegmentDescriptor, SegmentFacts,
};
#[cfg(test)]
use sha2::{Digest, Sha256};

use super::admission::ColdWorkWeight;

/// A canonical metric/fact query exceeded a bound or found contradictory data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalFactQueryError {
    LimitExceeded,
    ContradictoryFacts,
}

/// Domain separator for the response/cache fact-set identity.
#[cfg(test)]
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
}

#[derive(Debug)]
struct SourceAccumulator {
    data_through_us: Option<i64>,
    covered: Vec<CoverageSpan>,
    known_gaps: Vec<CoverageSpan>,
    source_completeness: Option<SourceCompleteness>,
    retained_exactness: Option<RetainedExactness>,
    physical_count: Option<PhysicalCountSemantics>,
    dropped_lower_bound: Option<u64>,
    dropped_count_unavailable: bool,
}

/// Descriptor metadata admitted at refresh without loading a fact body.
#[derive(Debug, Clone)]
pub(crate) struct DescriptorEntry {
    descriptor: SegmentDescriptor,
    fact_build_key: FactBuildKey,
    cold_weight: ColdWorkWeight,
}

impl DescriptorEntry {
    pub(crate) const fn new(
        descriptor: SegmentDescriptor,
        fact_build_key: FactBuildKey,
        cold_weight: ColdWorkWeight,
    ) -> Self {
        Self {
            descriptor,
            fact_build_key,
            cold_weight,
        }
    }

    pub(crate) const fn descriptor(&self) -> &SegmentDescriptor {
        &self.descriptor
    }

    pub(crate) const fn fact_build_key(&self) -> FactBuildKey {
        self.fact_build_key
    }

    pub(crate) const fn cold_weight(&self) -> ColdWorkWeight {
        self.cold_weight
    }
}

/// Descriptor-derived source identity and freshness without a fact body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DescriptorSource {
    source_id: u64,
    data_through_us: Option<i64>,
}

/// One explicit source interval omitted from a selected fact view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourceGap {
    source_id: u64,
    span: CoverageSpan,
}

impl SourceGap {
    pub(crate) const fn new(source_id: u64, span: CoverageSpan) -> Self {
        Self { source_id, span }
    }

    pub(crate) const fn source_id(self) -> u64 {
        self.source_id
    }

    pub(crate) const fn span(self) -> CoverageSpan {
        self.span
    }
}

impl DescriptorSource {
    const fn unknown(source_id: u64) -> Self {
        Self {
            source_id,
            data_through_us: None,
        }
    }

    pub(crate) const fn source_id(self) -> u64 {
        self.source_id
    }

    pub(crate) const fn data_through_us(self) -> Option<i64> {
        self.data_through_us
    }
}

/// One bounded opportunity to re-key the immediately preceding live view.
#[derive(Debug, Clone)]
pub(crate) struct PromotionCandidate {
    live: Arc<LiveView>,
    locators: BTreeSet<SealedLocator>,
}

impl PromotionCandidate {
    pub(crate) fn new(live: Arc<LiveView>, locators: BTreeSet<SealedLocator>) -> Option<Self> {
        (!locators.is_empty()).then_some(Self { live, locators })
    }

    fn for_locator(&self, locator: SealedLocator) -> Option<Arc<LiveView>> {
        self.locators
            .contains(&locator)
            .then(|| Arc::clone(&self.live))
    }
}

#[derive(Debug, Clone)]
struct IntervalIndex {
    range: Range<usize>,
    subtree_max_ts: Vec<i64>,
}

impl IntervalIndex {
    fn build<T>(values: &[T], range: Range<usize>, span: impl Fn(&T) -> (i64, i64) + Copy) -> Self {
        let mut subtree_max_ts = vec![i64::MIN; range.len()];
        build_interval_max(
            &values[range.clone()],
            0,
            range.len(),
            &mut subtree_max_ts,
            span,
        );
        Self {
            range,
            subtree_max_ts,
        }
    }

    fn extend_intersections<T>(
        &self,
        values: &[T],
        query: CoverageSpan,
        stop_after: usize,
        output: &mut Vec<usize>,
        span: impl Fn(&T) -> (i64, i64) + Copy,
    ) -> bool {
        let source_values = &values[self.range.clone()];
        let before_end = source_values.partition_point(|value| span(value).0 < query.end_us());
        visit_intersections(
            source_values,
            &self.subtree_max_ts,
            0,
            source_values.len(),
            before_end,
            query.start_us(),
            self.range.start,
            stop_after,
            output,
            span,
        )
    }
}

fn build_interval_max<T>(
    values: &[T],
    start: usize,
    end: usize,
    subtree_max_ts: &mut [i64],
    span: impl Fn(&T) -> (i64, i64) + Copy,
) -> i64 {
    if start == end {
        return i64::MIN;
    }
    let middle = start + (end - start) / 2;
    let left = build_interval_max(values, start, middle, subtree_max_ts, span);
    let right = build_interval_max(values, middle + 1, end, subtree_max_ts, span);
    let maximum = span(&values[middle]).1.max(left).max(right);
    subtree_max_ts[middle] = maximum;
    maximum
}

#[allow(
    clippy::too_many_arguments,
    reason = "the interval traversal carries explicit immutable bounds and one bounded output"
)]
fn visit_intersections<T>(
    values: &[T],
    subtree_max_ts: &[i64],
    start: usize,
    end: usize,
    before_end: usize,
    from_us: i64,
    global_start: usize,
    stop_after: usize,
    output: &mut Vec<usize>,
    span: impl Fn(&T) -> (i64, i64) + Copy,
) -> bool {
    if start == end || start >= before_end || subtree_max_ts[start + (end - start) / 2] < from_us {
        return false;
    }
    let middle = start + (end - start) / 2;
    if visit_intersections(
        values,
        subtree_max_ts,
        start,
        middle,
        before_end,
        from_us,
        global_start,
        stop_after,
        output,
        span,
    ) {
        return true;
    }
    if middle < before_end && span(&values[middle]).1 >= from_us {
        output.push(global_start + middle);
        if output.len() == stop_after {
            return true;
        }
    }
    visit_intersections(
        values,
        subtree_max_ts,
        middle + 1,
        end,
        before_end,
        from_us,
        global_start,
        stop_after,
        output,
        span,
    )
}

/// Atomically published descriptor authority and live generation.
#[derive(Debug, Clone)]
pub(crate) struct DescriptorView {
    view_generation: u64,
    sealed: Vec<DescriptorEntry>,
    source_indices: BTreeMap<u64, IntervalIndex>,
    unavailable: Vec<SegmentDescriptor>,
    unavailable_source_indices: BTreeMap<u64, IntervalIndex>,
    sources: BTreeMap<u64, DescriptorSource>,
    store_data_through_us: Option<i64>,
    live: Arc<LiveView>,
    promotion: Option<PromotionCandidate>,
}

impl DescriptorView {
    pub(crate) fn new(
        view_generation: u64,
        mut sealed: Vec<DescriptorEntry>,
        mut unavailable: Vec<SegmentDescriptor>,
        live: Arc<LiveView>,
        promotion: Option<PromotionCandidate>,
    ) -> Self {
        sealed.sort_by_key(|entry| {
            let descriptor = entry.descriptor();
            (descriptor.source_id, descriptor.min_ts, descriptor.locator)
        });
        unavailable.sort_by_key(|descriptor| {
            (descriptor.source_id, descriptor.min_ts, descriptor.locator)
        });
        let sealed_source_indices = source_indices(
            &sealed,
            |entry| entry.descriptor().source_id,
            |entry| {
                let descriptor = entry.descriptor();
                (descriptor.min_ts, descriptor.max_ts)
            },
        );
        let unavailable_source_indices = source_indices(
            &unavailable,
            |descriptor| descriptor.source_id,
            |descriptor| (descriptor.min_ts, descriptor.max_ts),
        );
        let sources = descriptor_sources(&sealed, &live);
        let store_data_through_us = sources
            .values()
            .filter_map(|source| source.data_through_us)
            .max();
        Self {
            view_generation,
            sealed,
            source_indices: sealed_source_indices,
            unavailable,
            unavailable_source_indices,
            sources,
            store_data_through_us,
            live,
            promotion,
        }
    }

    pub(crate) const fn view_generation(&self) -> u64 {
        self.view_generation
    }

    pub(crate) fn extend_selected_with_halo(
        &self,
        source: u64,
        range: CoverageSpan,
        stop_after: usize,
        selected: &mut Vec<usize>,
    ) -> bool {
        let Some(index) = self.source_indices.get(&source) else {
            return false;
        };
        if index.extend_intersections(&self.sealed, range, stop_after, selected, |entry| {
            let descriptor = entry.descriptor();
            (descriptor.min_ts, descriptor.max_ts)
        }) {
            return true;
        }
        let source_entries = &self.sealed[index.range.clone()];
        let left_halo = source_entries
            .iter()
            .rposition(|entry| entry.descriptor().max_ts < range.start_us())
            .map(|offset| index.range.start + offset);
        let right_halo = source_entries
            .iter()
            .position(|entry| entry.descriptor().min_ts >= range.end_us())
            .map(|offset| index.range.start + offset);
        for halo in [left_halo, right_halo].into_iter().flatten() {
            if !selected.contains(&halo) {
                selected.push(halo);
                if selected.len() == stop_after {
                    return true;
                }
            }
        }
        selected.sort_unstable();
        selected.dedup();
        selected.len() == stop_after
    }

    #[allow(
        dead_code,
        reason = "retained as a constant-space probe for callers that do not need explicit gaps"
    )]
    pub(crate) fn unavailable_intersects(&self, source: u64, range: CoverageSpan) -> bool {
        let Some(index) = self.unavailable_source_indices.get(&source) else {
            return false;
        };
        let mut one = Vec::with_capacity(1);
        index.extend_intersections(&self.unavailable, range, 1, &mut one, |descriptor| {
            (descriptor.min_ts, descriptor.max_ts)
        })
    }

    pub(crate) fn extend_unavailable_gaps(
        &self,
        source: u64,
        range: CoverageSpan,
        stop_after: usize,
        output: &mut Vec<SourceGap>,
    ) -> bool {
        let Some(index) = self.unavailable_source_indices.get(&source) else {
            return false;
        };
        let mut intersections = Vec::new();
        let stopped = index.extend_intersections(
            &self.unavailable,
            range,
            stop_after,
            &mut intersections,
            |descriptor| (descriptor.min_ts, descriptor.max_ts),
        );
        output.extend(intersections.into_iter().filter_map(|index| {
            descriptor_gap(self.unavailable[index], range).map(|span| SourceGap::new(source, span))
        }));
        output.sort_unstable();
        output.dedup();
        stopped || output.len() >= stop_after
    }

    pub(crate) fn entry(&self, index: usize) -> &DescriptorEntry {
        &self.sealed[index]
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> &[DescriptorEntry] {
        &self.sealed
    }

    pub(crate) fn sources_for(&self, requested: &[u64]) -> Vec<DescriptorSource> {
        requested
            .iter()
            .map(|source| {
                self.sources
                    .get(source)
                    .copied()
                    .unwrap_or_else(|| DescriptorSource::unknown(*source))
            })
            .collect()
    }

    pub(crate) const fn live(&self) -> &Arc<LiveView> {
        &self.live
    }

    pub(crate) const fn data_through_us(&self) -> Option<i64> {
        self.store_data_through_us
    }

    pub(crate) fn promotion_for(&self, locator: SealedLocator) -> Option<Arc<LiveView>> {
        self.promotion
            .as_ref()
            .and_then(|candidate| candidate.for_locator(locator))
    }
}

fn descriptor_gap(descriptor: SegmentDescriptor, range: CoverageSpan) -> Option<CoverageSpan> {
    let start = descriptor.min_ts.max(range.start_us());
    let end = descriptor.max_ts.saturating_add(1).min(range.end_us());
    CoverageSpan::new(start, end)
}

fn source_indices<T>(
    values: &[T],
    source_id: impl Fn(&T) -> u64,
    span: impl Fn(&T) -> (i64, i64) + Copy,
) -> BTreeMap<u64, IntervalIndex> {
    let mut indices = BTreeMap::new();
    let mut start = 0;
    while start < values.len() {
        let source = source_id(&values[start]);
        let mut end = start + 1;
        while end < values.len() && source_id(&values[end]) == source {
            end += 1;
        }
        indices.insert(source, IntervalIndex::build(values, start..end, span));
        start = end;
    }
    indices
}

fn descriptor_sources(
    sealed: &[DescriptorEntry],
    live: &LiveView,
) -> BTreeMap<u64, DescriptorSource> {
    let mut sources = BTreeMap::new();
    for entry in sealed {
        merge_descriptor_source(
            &mut sources,
            entry.descriptor().source_id,
            entry.descriptor().max_ts,
        );
    }
    if matches!(live.state(), LiveState::Empty | LiveState::Current) {
        for facts in live.chunks() {
            let identity = facts.identity();
            merge_descriptor_source(
                &mut sources,
                identity.pgm_source_id,
                identity.source_max_ts_us,
            );
        }
    }
    sources
}

fn merge_descriptor_source(
    sources: &mut BTreeMap<u64, DescriptorSource>,
    source_id: u64,
    data_through_us: i64,
) {
    let source = sources
        .entry(source_id)
        .or_insert_with(|| DescriptorSource::unknown(source_id));
    source.data_through_us = Some(
        source
            .data_through_us
            .map_or(data_through_us, |current| current.max(data_through_us)),
    );
}

/// One sealed segment bound into an index view.
#[derive(Debug, Clone)]
pub(crate) struct SealedEntry {
    descriptor: SegmentDescriptor,
    facts: Arc<SegmentFacts>,
    fact_build_key: FactBuildKey,
}

impl SealedEntry {
    /// Binds sealed facts, computing the content-addressed fact key.
    pub(crate) fn new(descriptor: SegmentDescriptor, facts: Arc<SegmentFacts>) -> Self {
        let fact_key = FactKey::for_identity(facts.identity(), FileKind::SegmentFacts);
        let fact_build_key = FactBuildKey::new(fact_key, facts.lineage().id());
        Self {
            descriptor,
            facts,
            fact_build_key,
        }
    }

    pub(crate) fn from_descriptor(
        descriptor: &DescriptorEntry,
        facts: Arc<SegmentFacts>,
    ) -> Option<Self> {
        let loaded = Self::new(*descriptor.descriptor(), facts);
        (loaded.fact_build_key == descriptor.fact_build_key()).then_some(loaded)
    }

    /// The content-bound descriptor of the sealed segment.
    #[cfg(test)]
    pub(crate) const fn descriptor(&self) -> &SegmentDescriptor {
        &self.descriptor
    }

    fn facts(&self) -> &SegmentFacts {
        &self.facts
    }

    /// Exact durable build identity represented by this retained segment.
    #[cfg(test)]
    pub(crate) const fn fact_build_key(&self) -> FactBuildKey {
        self.fact_build_key
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
    source_descriptors: Vec<DescriptorSource>,
    source_gaps: Vec<SourceGap>,
    source_ids: Vec<u64>,
    store_data_through_us: Option<i64>,
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
    #[cfg(test)]
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
        let source_descriptors = loaded_source_descriptors(&sealed, &live, live_queryable);
        let store_data_through_us = source_descriptors
            .iter()
            .filter_map(|source| source.data_through_us())
            .max();
        Self::new_with_id(
            view_generation,
            sealed,
            live,
            live_queryable,
            coverage_envelope,
            fact_set_id,
            source_status,
            source_descriptors,
            Vec::new(),
            store_data_through_us,
        )
    }

    pub(crate) fn from_selected(
        view: &DescriptorView,
        sealed: Vec<SealedEntry>,
        source_gaps: Vec<SourceGap>,
        fact_set_id: [u8; 32],
        source_descriptors: Vec<DescriptorSource>,
        store_data_through_us: Option<i64>,
    ) -> Self {
        let live = Arc::clone(view.live());
        let live_queryable = matches!(live.state(), LiveState::Empty | LiveState::Current);
        let coverage_envelope = Self::build_envelope(&sealed, &live, live_queryable);
        let source_status = if source_gaps.is_empty() {
            SourceStatus::from_live_state(live.state())
        } else {
            SourceStatus::Gap
        };
        Self::new_with_id(
            view.view_generation(),
            sealed,
            live,
            live_queryable,
            coverage_envelope,
            fact_set_id,
            source_status,
            source_descriptors,
            source_gaps,
            store_data_through_us,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor binds every immutable axis of one selected fact view"
    )]
    fn new_with_id(
        view_generation: u64,
        sealed: Vec<SealedEntry>,
        live: Arc<LiveView>,
        live_queryable: bool,
        coverage_envelope: Coverage,
        fact_set_id: [u8; 32],
        source_status: SourceStatus,
        source_descriptors: Vec<DescriptorSource>,
        source_gaps: Vec<SourceGap>,
        store_data_through_us: Option<i64>,
    ) -> Self {
        let source_ids = source_descriptors
            .iter()
            .map(|source| source.source_id())
            .collect();
        Self {
            view_generation,
            sealed,
            live,
            live_queryable,
            coverage_envelope,
            fact_set_id,
            source_status,
            source_descriptors,
            source_gaps,
            source_ids,
            store_data_through_us,
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

    /// The precomputed union coverage of the queried sources.
    pub(crate) const fn coverage_envelope(&self) -> &Coverage {
        &self.coverage_envelope
    }

    /// The latest microsecond folded from the live generation, if any.
    pub(crate) const fn data_through_us(&self) -> Option<i64> {
        self.store_data_through_us
    }

    /// Latest selected sealed/live timestamp.
    pub(crate) fn data_through_us_for(&self, sources: &[u64]) -> Option<i64> {
        self.source_descriptors
            .iter()
            .filter(|source| source_selected(sources, source.source_id()))
            .filter_map(|source| source.data_through_us())
            .max()
    }

    /// Incomplete active-journal byte range for the selected live source.
    pub(crate) fn live_tail_pending_for(
        &self,
        sources: &[u64],
    ) -> Option<kronika_reader::ByteRange> {
        self.live
            .source_id()
            .filter(|source| sources.binary_search(source).is_ok())
            .and_then(|_| self.live.tail_pending())
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
                let source = self
                    .source_descriptors
                    .iter()
                    .find(|source| source.source_id() == *source_id)
                    .copied()
                    .unwrap_or_else(|| DescriptorSource::unknown(*source_id));
                (
                    *source_id,
                    SourceAccumulator {
                        data_through_us: source.data_through_us(),
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
        for gap in &self.source_gaps {
            let Some(accumulator) = selected.get_mut(&gap.source_id()) else {
                continue;
            };
            if remaining_spans == 0 {
                return Err(MetadataError::SpanLimitExceeded);
            }
            accumulator.known_gaps.push(gap.span());
            remaining_spans -= 1;
            accumulator.source_completeness = Some(SourceCompleteness::BoundedSubset);
        }
        for facts in self.queryable_facts() {
            let identity = facts.identity();
            let Some(accumulator) = selected.get_mut(&identity.pgm_source_id) else {
                continue;
            };
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

    /// Queries canonical persisted facts and reset-aware metric derivations.
    ///
    /// Selected sealed facts already include the left/right descriptor halo.
    /// This method replays all selected natural samples, so a pair crossing a
    /// fact-file boundary is equivalent to the same samples in one file.
    #[allow(
        clippy::too_many_lines,
        reason = "the query performs one bounded canonical merge across persisted metric families"
    )]
    pub(crate) fn query_canonical_facts(
        &self,
        sources: &[u64],
        range: CoverageSpan,
        max_facts: usize,
        max_samples: usize,
    ) -> Result<Vec<CanonicalEventFact>, CanonicalFactQueryError> {
        let mut facts = BTreeMap::<FactId, CanonicalEventFact>::new();
        let mut counter_series =
            BTreeMap::<MetricSeriesId, (MetricSeriesDescriptor, Vec<CounterSample>)>::new();
        let mut state_series =
            BTreeMap::<MetricSeriesId, (MetricSeriesDescriptor, Vec<EntityStateRecord>)>::new();
        let mut gauge_series =
            BTreeMap::<MetricSeriesId, (MetricSeriesDescriptor, Vec<GaugeSample>)>::new();
        let mut gaps = BTreeMap::<u64, Coverage>::new();
        let mut materialized_samples = 0_usize;

        for gap in &self.source_gaps {
            gaps.entry(gap.source_id())
                .and_modify(|coverage| {
                    *coverage = coverage.union(&Coverage::from_spans(vec![gap.span()]));
                })
                .or_insert_with(|| Coverage::from_spans(vec![gap.span()]));
        }

        for segment in self
            .queryable_facts()
            .filter(|facts| source_selected(sources, facts.identity().pgm_source_id))
        {
            for fact in segment.event_facts().iter().filter(|fact| {
                fact.interval().start_us() < range.end_us()
                    && fact.interval().end_us() > range.start_us()
            }) {
                insert_canonical_fact(&mut facts, fact.clone(), max_facts)?;
            }
            gaps.entry(segment.identity().pgm_source_id)
                .and_modify(|coverage| {
                    *coverage = coverage.union(segment.loss_coverage().known_gaps());
                })
                .or_insert_with(|| segment.loss_coverage().known_gaps().clone());
            for descriptor in segment.counter_samples().series() {
                let entry = counter_series
                    .entry(descriptor.series_id)
                    .or_insert_with(|| (*descriptor, Vec::new()));
                if entry.0 != *descriptor {
                    return Err(CanonicalFactQueryError::ContradictoryFacts);
                }
            }
            for sample in segment.counter_samples().samples() {
                materialized_samples = materialized_samples
                    .checked_add(1)
                    .ok_or(CanonicalFactQueryError::LimitExceeded)?;
                if materialized_samples > max_samples {
                    return Err(CanonicalFactQueryError::LimitExceeded);
                }
                counter_series
                    .get_mut(&sample.series_id())
                    .ok_or(CanonicalFactQueryError::ContradictoryFacts)?
                    .1
                    .push(*sample);
            }
            for descriptor in segment.gauge_samples().series() {
                match gauge_series.entry(descriptor.series_id) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((*descriptor, Vec::new()));
                    }
                    std::collections::btree_map::Entry::Occupied(entry)
                        if entry.get().0 != *descriptor =>
                    {
                        return Err(CanonicalFactQueryError::ContradictoryFacts);
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {}
                }
                match state_series.entry(descriptor.series_id) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((*descriptor, Vec::new()));
                    }
                    std::collections::btree_map::Entry::Occupied(entry)
                        if entry.get().0 != *descriptor =>
                    {
                        return Err(CanonicalFactQueryError::ContradictoryFacts);
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {}
                }
            }
            for sample in segment.gauge_samples().samples() {
                materialized_samples = materialized_samples
                    .checked_add(1)
                    .ok_or(CanonicalFactQueryError::LimitExceeded)?;
                if materialized_samples > max_samples {
                    return Err(CanonicalFactQueryError::LimitExceeded);
                }
                gauge_series
                    .get_mut(&sample.series_id())
                    .ok_or(CanonicalFactQueryError::ContradictoryFacts)?
                    .1
                    .push(*sample);
            }
            for record in segment.entity_states().records() {
                materialized_samples = materialized_samples
                    .checked_add(1)
                    .ok_or(CanonicalFactQueryError::LimitExceeded)?;
                if materialized_samples > max_samples {
                    return Err(CanonicalFactQueryError::LimitExceeded);
                }
                state_series
                    .get_mut(&record.series_id)
                    .ok_or(CanonicalFactQueryError::ContradictoryFacts)?
                    .1
                    .push(*record);
            }
        }

        for (_series_id, (descriptor, mut samples)) in counter_series {
            samples.sort_unstable_by_key(|sample| {
                (
                    sample.ts_us(),
                    sample.alignment_id().0,
                    sample.value(),
                    sample.reset_epoch(),
                )
            });
            samples.dedup();
            if samples
                .windows(2)
                .any(|pair| pair[0].ts_us() == pair[1].ts_us())
                || samples.first().is_some_and(|first| {
                    samples
                        .iter()
                        .any(|sample| sample.alignment_id() != first.alignment_id())
                })
            {
                return Err(CanonicalFactQueryError::ContradictoryFacts);
            }
            let known_gaps = gaps
                .get(&descriptor.source_id)
                .cloned()
                .unwrap_or_else(Coverage::empty);
            for pair in samples.windows(2) {
                if pair[1].ts_us() < range.start_us() || pair[1].ts_us() >= range.end_us() {
                    continue;
                }
                if let Some(fact) =
                    CanonicalEventFact::from_counter_pair(descriptor, pair[0], pair[1], &known_gaps)
                        .map_err(|_error| CanonicalFactQueryError::ContradictoryFacts)?
                {
                    insert_canonical_fact(&mut facts, fact, max_facts)?;
                }
            }
        }

        for (descriptor, samples) in gauge_series.values().filter(|(descriptor, _samples)| {
            matches!(
                MetricFactor::from_id(descriptor.factor_id),
                Some(MetricFactor::PgStatisticsResetAt | MetricFactor::PgPostmasterStartTime)
            )
        }) {
            let mut samples = samples.clone();
            samples.sort_unstable_by_key(|sample| (sample.ts_us(), sample.value().to_bits()));
            samples.dedup();
            if samples
                .windows(2)
                .any(|pair| pair[0].ts_us() == pair[1].ts_us())
            {
                return Err(CanonicalFactQueryError::ContradictoryFacts);
            }
            let known_gaps = gaps
                .get(&descriptor.source_id)
                .cloned()
                .unwrap_or_else(Coverage::empty);
            for pair in samples.windows(2) {
                if pair[1].ts_us() < range.start_us()
                    || pair[1].ts_us() >= range.end_us()
                    || known_gaps.spans().iter().any(|gap| {
                        gap.start_us() < pair[1].ts_us() && gap.end_us() > pair[0].ts_us()
                    })
                {
                    continue;
                }
                if let Some(fact) =
                    CanonicalEventFact::from_metadata_change(*descriptor, pair[0], pair[1])
                        .map_err(|_error| CanonicalFactQueryError::ContradictoryFacts)?
                {
                    insert_canonical_fact(&mut facts, fact, max_facts)?;
                }
            }
        }

        derive_sender_disappearances(&state_series, &gaps, range, max_facts, &mut facts)?;

        for (_series_id, (descriptor, mut records)) in state_series {
            records.sort_unstable_by_key(|record| {
                (record.ts_us, record.state_code, record.population_total)
            });
            records.dedup();
            if records
                .windows(2)
                .any(|pair| pair[0].ts_us == pair[1].ts_us)
            {
                return Err(CanonicalFactQueryError::ContradictoryFacts);
            }
            let known_gaps = gaps
                .get(&descriptor.source_id)
                .cloned()
                .unwrap_or_else(Coverage::empty);
            for pair in records.windows(2) {
                if pair[1].ts_us < range.start_us() || pair[1].ts_us >= range.end_us() {
                    continue;
                }
                if known_gaps
                    .spans()
                    .iter()
                    .any(|gap| gap.start_us() < pair[1].ts_us && gap.end_us() > pair[0].ts_us)
                {
                    continue;
                }
                if let Some(fact) = CanonicalEventFact::from_state_transition(
                    descriptor,
                    pair[0].ts_us,
                    pair[0].state_code,
                    pair[1].ts_us,
                    pair[1].state_code,
                    pair[1].population_total,
                )
                .map_err(|_error| CanonicalFactQueryError::ContradictoryFacts)?
                {
                    insert_canonical_fact(&mut facts, fact, max_facts)?;
                }
            }
        }

        let mut capacity_total = BTreeMap::new();
        let mut capacity_available = BTreeMap::new();
        for (_series_id, (descriptor, mut samples)) in gauge_series {
            samples.sort_unstable_by_key(|sample| (sample.ts_us(), sample.value().to_bits()));
            samples.dedup();
            if samples
                .windows(2)
                .any(|pair| pair[0].ts_us() == pair[1].ts_us())
            {
                return Err(CanonicalFactQueryError::ContradictoryFacts);
            }
            let Some(entity) = descriptor.entity else {
                continue;
            };
            let destination = match MetricFactor::from_id(descriptor.factor_id) {
                Some(MetricFactor::PgFilesystemTotalBytes) => &mut capacity_total,
                Some(MetricFactor::PgFilesystemAvailableBytes) => &mut capacity_available,
                _ => continue,
            };
            for sample in samples {
                if destination
                    .insert((entity, sample.ts_us()), (descriptor, sample))
                    .is_some()
                {
                    return Err(CanonicalFactQueryError::ContradictoryFacts);
                }
            }
        }
        for (key, (total_descriptor, total)) in &capacity_total {
            let Some((available_descriptor, current_available)) =
                capacity_available.get(key).copied()
            else {
                continue;
            };
            let previous_available = capacity_available
                .range((key.0, i64::MIN)..*key)
                .next_back()
                .map(|(_key, (_descriptor, sample))| *sample);
            if let Some(previous_available) = previous_available
                && current_available.ts_us() >= range.start_us()
                && current_available.ts_us() < range.end_us()
            {
                let known_gaps = gaps
                    .get(&available_descriptor.source_id)
                    .cloned()
                    .unwrap_or_else(Coverage::empty);
                if let Some(fact) = CanonicalEventFact::from_capacity_zero_transition(
                    *total_descriptor,
                    *total,
                    available_descriptor,
                    previous_available,
                    current_available,
                    &known_gaps,
                )
                .map_err(|_error| CanonicalFactQueryError::ContradictoryFacts)?
                {
                    insert_canonical_fact(&mut facts, fact, max_facts)?;
                }
            }
        }

        let mut output = facts.into_values().collect::<Vec<_>>();
        output.sort_by(CanonicalEventFact::canonical_cmp);
        Ok(output)
    }

    /// Returns bounded factor coverage records intersecting a query interval.
    pub(crate) fn query_factor_coverage(
        &self,
        sources: &[u64],
        range: CoverageSpan,
        max_records: usize,
    ) -> Result<Vec<FactorCoverage>, CanonicalFactQueryError> {
        let mut coverage = Vec::new();
        for segment in self
            .queryable_facts()
            .filter(|facts| source_selected(sources, facts.identity().pgm_source_id))
        {
            for record in segment
                .loss_coverage()
                .factor_coverage()
                .iter()
                .filter(|record| {
                    record.interval.start_us() < range.end_us()
                        && record.interval.end_us() > range.start_us()
                })
            {
                if coverage.len() == max_records {
                    return Err(CanonicalFactQueryError::LimitExceeded);
                }
                coverage.push(record.clone());
            }
        }
        coverage.sort_unstable_by_key(|record| {
            (
                record.factor_id.0,
                record.interval.start_us(),
                record.interval.end_us(),
            )
        });
        Ok(coverage)
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
        let source_descriptors = self
            .source_descriptors
            .capacity()
            .checked_mul(size_of::<DescriptorSource>())?;
        let source_gaps = self
            .source_gaps
            .capacity()
            .checked_mul(size_of::<SourceGap>())?;

        size_of::<Self>()
            .checked_add(ARC_COUNTER_BYTES)?
            .checked_add(sealed_slots)?
            .checked_add(sealed)?
            .checked_add(ARC_COUNTER_BYTES)?
            .checked_add(self.live.resident_bytes()?)?
            .checked_add(coverage)?
            .checked_add(sources)?
            .checked_add(source_descriptors)?
            .checked_add(source_gaps)
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

    #[cfg(test)]
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
            hasher.update(entry.fact_build_key.fact_key().as_bytes());
        }
        hasher.update(live.generation().0.to_le_bytes());
        hasher.update(live.folded_through_offset().to_le_bytes());
        hasher.update(live.view_generation().to_le_bytes());
        hasher.update([live_state_tag(live.state())]);
        hasher.finalize().into()
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

#[cfg(test)]
fn loaded_source_descriptors(
    sealed: &[SealedEntry],
    live: &LiveView,
    live_queryable: bool,
) -> Vec<DescriptorSource> {
    let mut sources = BTreeMap::new();
    for entry in sealed {
        let identity = entry.facts().identity();
        merge_descriptor_source(
            &mut sources,
            identity.pgm_source_id,
            identity.source_max_ts_us,
        );
    }
    if live_queryable {
        for facts in live.chunks() {
            let identity = facts.identity();
            merge_descriptor_source(
                &mut sources,
                identity.pgm_source_id,
                identity.source_max_ts_us,
            );
        }
    }
    sources.into_values().collect()
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

fn derive_sender_disappearances(
    states: &BTreeMap<MetricSeriesId, (MetricSeriesDescriptor, Vec<EntityStateRecord>)>,
    gaps: &BTreeMap<u64, Coverage>,
    range: CoverageSpan,
    max_facts: usize,
    facts: &mut BTreeMap<FactId, CanonicalEventFact>,
) -> Result<(), CanonicalFactQueryError> {
    let mut boundaries =
        BTreeMap::<(u64, u32), Vec<(MetricSeriesDescriptor, EntityStateRecord)>>::new();
    let mut snapshots = SenderSnapshots::new();
    for (descriptor, records) in states.values() {
        for record in records {
            match MetricFactor::from_id(descriptor.factor_id) {
                Some(MetricFactor::PgReplicationSenderSnapshotPopulation) => boundaries
                    .entry((descriptor.source_id, descriptor.source_type_id))
                    .or_default()
                    .push((*descriptor, *record)),
                Some(MetricFactor::PgReplicationSenderState) => {
                    insert_sender_snapshot(&mut snapshots, *descriptor, *record)?;
                }
                _ => {}
            }
        }
    }
    for ((source_id, source_type), source_boundaries) in &mut boundaries {
        source_boundaries.sort_unstable_by_key(|(_descriptor, record)| record.ts_us);
        if source_boundaries
            .windows(2)
            .any(|pair| pair[0].1.ts_us == pair[1].1.ts_us)
        {
            return Err(CanonicalFactQueryError::ContradictoryFacts);
        }
        for pair in source_boundaries.windows(2) {
            let (previous_descriptor, previous) = pair[0];
            let (current_descriptor, current) = pair[1];
            if current.ts_us < range.start_us()
                || current.ts_us >= range.end_us()
                || previous_descriptor.series_id != current_descriptor.series_id
                || gaps
                    .get(&current_descriptor.source_id)
                    .is_some_and(|known| {
                        known.spans().iter().any(|gap| {
                            gap.start_us() < current.ts_us && gap.end_us() > previous.ts_us
                        })
                    })
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
                if let Some(fact) = CanonicalEventFact::from_sender_disappearance(
                    *sender_descriptor,
                    sender.ts_us,
                    sender.state_code,
                    current_descriptor,
                    current_sample,
                    current.population_total,
                )
                .map_err(|_error| CanonicalFactQueryError::ContradictoryFacts)?
                {
                    insert_canonical_fact(facts, fact, max_facts)?;
                }
            }
        }
    }
    Ok(())
}

type SenderSnapshots = BTreeMap<
    (u64, u32, i64),
    BTreeMap<MetricSeriesId, (MetricSeriesDescriptor, EntityStateRecord)>,
>;

fn insert_sender_snapshot(
    snapshots: &mut SenderSnapshots,
    descriptor: MetricSeriesDescriptor,
    record: EntityStateRecord,
) -> Result<(), CanonicalFactQueryError> {
    if snapshots
        .entry((
            descriptor.source_id,
            descriptor.source_type_id,
            record.ts_us,
        ))
        .or_default()
        .insert(descriptor.series_id, (descriptor, record))
        .is_some()
    {
        return Err(CanonicalFactQueryError::ContradictoryFacts);
    }
    Ok(())
}

fn insert_canonical_fact(
    facts: &mut BTreeMap<FactId, CanonicalEventFact>,
    fact: CanonicalEventFact,
    max_facts: usize,
) -> Result<(), CanonicalFactQueryError> {
    if !facts.contains_key(&fact.fact_id()) && facts.len() == max_facts {
        return Err(CanonicalFactQueryError::LimitExceeded);
    }
    match facts.entry(fact.fact_id()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(fact);
        }
        std::collections::btree_map::Entry::Occupied(entry) if entry.get() != &fact => {
            return Err(CanonicalFactQueryError::ContradictoryFacts);
        }
        std::collections::btree_map::Entry::Occupied(_) => {}
    }
    Ok(())
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

#[cfg(test)]
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
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_layout::FileIdentity;
    use kronika_reader::{
        JournalDelta, JournalGenerationId, LIMIT, LiveBuilder, PartTransition, PgmUnit,
        RefreshDelta,
    };
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_store::CatalogSummary;

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
        let locator = SealedLocator::from_segment_id(crate::test_layout::named_address(file).id);
        let summary = CatalogSummary::from_catalog(
            unit.catalog(),
            u32::try_from(unit.catalog().encoded_len()).expect("catalog length"),
        );
        let descriptor = SegmentDescriptor::from_summary(
            locator,
            FileIdentity {
                device: 1,
                inode: locator.segment_id().get().unsigned_abs(),
                len: unit.source_file_len(),
                mtime_seconds: 0,
                mtime_nanoseconds: 0,
                ctime_seconds: 0,
                ctime_nanoseconds: 0,
            },
            &summary,
        );
        let facts = SegmentFacts::extract(&unit, &LIMIT).expect("extract facts");
        SealedEntry::new(descriptor, Arc::new(facts))
    }

    fn empty_live() -> Arc<LiveView> {
        let mut builder = LiveBuilder::new(LIMIT).expect("live builder");
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
                completed_parts: Arc::from([]),
                current_parts: Arc::from([]),
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
        let builder = LiveBuilder::new(LIMIT).expect("live builder");
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

    fn descriptor_entry(entry: &SealedEntry) -> DescriptorEntry {
        DescriptorEntry::new(
            *entry.descriptor(),
            entry.fact_build_key(),
            ColdWorkWeight {
                workers: 1,
                pgm_bytes: 1,
                decoded_bytes: 1,
                cpu: 1,
                file_descriptors: 1,
                read_bytes: 1,
                write_bytes: 1,
                publications: 1,
            },
        )
    }

    #[test]
    fn descriptor_selection_reflects_half_open_range_intersection() {
        let early = sealed_entry("143000.pgm", 1_000, 2_000);
        let late = sealed_entry("143001.pgm", 5_000, 6_000);
        let view = Arc::new(DescriptorView::new(
            3,
            vec![descriptor_entry(&early), descriptor_entry(&late)],
            Vec::new(),
            empty_live(),
            None,
        ));

        assert_eq!(
            crate::overview::selection::SelectedSealedPlan::build(
                Arc::clone(&view),
                &[7],
                CoverageSpan::new(0, 10_000).expect("span"),
                4,
            )
            .expect("plan")
            .selected_count(),
            2,
            "a range covering both segments selects both"
        );
        assert_eq!(
            crate::overview::selection::SelectedSealedPlan::build(
                Arc::clone(&view),
                &[7],
                CoverageSpan::new(0, 2_500).expect("span"),
                4,
            )
            .expect("plan")
            .selected_count(),
            2,
            "the intersecting segment retains the nearest right halo"
        );
        assert_eq!(
            crate::overview::selection::SelectedSealedPlan::build(
                view,
                &[7],
                CoverageSpan::new(3_000, 4_000).expect("span"),
                4,
            )
            .expect("plan")
            .selected_count(),
            2,
            "a range between segments retains both boundary halos"
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
        assert_eq!(unknown[0].data_through_us, None);
        assert_eq!(unknown[0].source_completeness, SourceCompleteness::Unknown);
        assert_eq!(unknown[0].dropped_lower_bound, None);

        let disjoint = view
            .selected_source_metadata(&[7], CoverageSpan::new(10_000, 20_000).expect("range"), 16)
            .expect("disjoint metadata");
        assert_eq!(disjoint[0].data_through_us, Some(2_000));
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
