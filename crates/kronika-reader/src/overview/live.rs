//! Bounded folding of completed journal parts into immutable overview views.

use std::collections::BTreeSet;
use std::sync::Arc;

use kronika_analytics::overview::{
    Coverage, CoverageSpan, OracleError, OracleLimits, OracleResult, OracleSourceError, RawOracle,
    query_bounded,
};
use kronika_format::{DamageKind, DamageRegion, FRAME_HEADER_LEN, ReadAt};

use crate::refresh::{
    ByteRange, JournalGenerationId, PartDescriptor, PartId, RefreshDelta,
    catalog_digest as refresh_catalog_digest,
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
    /// The refresh does not continue the builder's pinned view generation.
    ViewGenerationMismatch,
    /// The refresh's complete descriptor set is inconsistent with its
    /// transition or validated byte watermark.
    RefreshMismatch,
    /// The refresh did not provide one authoritative, damage-safe active view.
    IncompleteRefresh,
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
            Self::ViewGenerationMismatch => {
                f.write_str("refresh does not continue the pinned view generation")
            }
            Self::RefreshMismatch => {
                f.write_str("refresh descriptors do not match its journal watermark")
            }
            Self::IncompleteRefresh => {
                f.write_str("refresh did not produce an authoritative active view")
            }
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
            | Self::ViewGenerationMismatch
            | Self::RefreshMismatch
            | Self::IncompleteRefresh
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
    known_gap_spans: u64,
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
        let known_gap_spans = u64::try_from(facts.loss_coverage().known_gaps().spans().len())
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
            known_gap_spans: self
                .known_gap_spans
                .checked_add(known_gap_spans)
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
            && self.known_gap_spans <= bounds.coverage_spans
            && self.retained_text_bytes <= bounds.string_table_bytes
    }
}

#[derive(Debug, Clone)]
struct PendingRefresh {
    new_view_generation: u64,
    new_valid_len: u64,
    current_parts: Vec<PartDescriptor>,
    current_parts_complete: bool,
    tail_pending: Option<ByteRange>,
    damages: Vec<DamageRegion>,
}

/// The single mutable writer that folds completed parts into live facts.
#[derive(Debug, Clone)]
pub struct LiveBuilder {
    store_namespace: Vec<u8>,
    bounds: Bounds,
    state: LiveState,
    baseline_pinned: bool,
    rebaseline_prepared: bool,
    view_generation: u64,
    generation: JournalGenerationId,
    folded_part_ids: BTreeSet<PartId>,
    folded_parts: Vec<PartDescriptor>,
    chunks: Vec<Arc<SegmentFacts>>,
    usage: LiveUsage,
    source_id: Option<u64>,
    watermark_us: Option<i64>,
    folded_through_offset: u64,
    completed_tail_pending: Option<ByteRange>,
    pending_refresh: Option<PendingRefresh>,
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
            state: LiveState::Warming,
            baseline_pinned: false,
            rebaseline_prepared: false,
            view_generation: 0,
            generation: JournalGenerationId(0),
            folded_part_ids: BTreeSet::new(),
            folded_parts: Vec::new(),
            chunks: Vec::new(),
            usage: LiveUsage::default(),
            source_id: None,
            watermark_us: None,
            folded_through_offset: 0,
            completed_tail_pending: None,
            pending_refresh: None,
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

    /// Snapshot generation consumed by the last completed refresh.
    #[must_use]
    pub const fn view_generation(&self) -> u64 {
        self.view_generation
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
    /// This prepares an explicit caller-directed rebaseline. The next
    /// [`begin_refresh`](Self::begin_refresh) must carry this generation and
    /// re-deliver its complete current descriptor set; direct part folding
    /// remains unavailable.
    pub fn reset_to(&mut self, generation: JournalGenerationId) {
        self.clear_folded(generation);
        self.pending_refresh = None;
        self.baseline_pinned = true;
        self.rebaseline_prepared = true;
        self.state = LiveState::Warming;
    }

    /// Starts consumption of one complete snapshot refresh.
    ///
    /// The builder copies the refresh's authoritative descriptor set and
    /// completion evidence before accepting any PGM body. Until
    /// [`complete_refresh`](Self::complete_refresh) succeeds, published views
    /// remain unavailable for queries and promotion.
    ///
    /// # Errors
    ///
    /// Returns [`LiveFoldError`] when another refresh is pending, the view or
    /// journal generation does not continue this builder, or the descriptor
    /// sequence contradicts the validated journal watermark.
    pub fn begin_refresh(&mut self, delta: &RefreshDelta) -> Result<(), LiveFoldError> {
        let bootstrap = self.prepare_refresh_baseline(delta)?;

        if let Err(error) = validate_descriptor_sequence(
            &delta.journal.current_parts,
            delta.journal.generation_id,
            delta.journal.new_valid_len,
        ) {
            self.state = LiveState::Incomplete;
            return Err(error);
        }

        if bootstrap {
            if delta.journal.completed_parts != delta.journal.current_parts {
                self.state = LiveState::NeedsRebuild;
                return Err(LiveFoldError::RefreshMismatch);
            }
        } else if self.rebaseline_prepared {
            if delta.journal.generation_id != self.generation
                || delta.journal.completed_parts != delta.journal.current_parts
            {
                self.state = LiveState::NeedsRebuild;
                return Err(LiveFoldError::RefreshMismatch);
            }
        } else if delta.journal.transition.preserves_generation() {
            if matches!(self.state, LiveState::NeedsRebuild | LiveState::Incomplete) {
                return Err(LiveFoldError::InvalidState);
            }
            if delta.journal.generation_id != self.generation {
                self.state = LiveState::NeedsRebuild;
                return Err(LiveFoldError::GenerationMismatch);
            }
            if delta.journal.current_parts_complete
                && (delta.journal.previous_valid_len != self.folded_through_offset
                    || !delta.journal.current_parts.starts_with(&self.folded_parts)
                    || delta.journal.completed_parts
                        != delta.journal.current_parts[self.folded_parts.len()..])
            {
                self.state = LiveState::NeedsRebuild;
                return Err(LiveFoldError::RefreshMismatch);
            }
        } else {
            let Some(next_generation) = self.generation.0.checked_add(1) else {
                self.state = LiveState::Incomplete;
                return Err(LiveFoldError::Overflow);
            };
            if delta.journal.generation_id != JournalGenerationId(next_generation) {
                self.state = LiveState::NeedsRebuild;
                return Err(LiveFoldError::GenerationMismatch);
            }
            if delta.journal.current_parts_complete
                && delta.journal.completed_parts != delta.journal.current_parts
            {
                self.state = LiveState::NeedsRebuild;
                return Err(LiveFoldError::RefreshMismatch);
            }
            self.clear_folded(delta.journal.generation_id);
        }

        self.rebaseline_prepared = false;
        self.pending_refresh = Some(PendingRefresh {
            new_view_generation: delta.new_view_generation,
            new_valid_len: delta.journal.new_valid_len,
            current_parts: delta.journal.current_parts.clone(),
            current_parts_complete: delta.journal.current_parts_complete,
            tail_pending: delta.journal.tail_pending,
            damages: delta.journal.damages.clone(),
        });
        self.state = LiveState::Warming;
        Ok(())
    }

    fn prepare_refresh_baseline(&mut self, delta: &RefreshDelta) -> Result<bool, LiveFoldError> {
        if self.pending_refresh.is_some() {
            return Err(LiveFoldError::InvalidState);
        }
        let bootstrap = delta.journal.bootstrap;
        if bootstrap && self.baseline_pinned {
            self.state = LiveState::NeedsRebuild;
            return Err(LiveFoldError::RefreshMismatch);
        }
        if !bootstrap && !self.baseline_pinned {
            self.state = LiveState::NeedsRebuild;
            return Err(LiveFoldError::RefreshMismatch);
        }
        if !bootstrap && delta.previous_view_generation != self.view_generation {
            self.state = LiveState::NeedsRebuild;
            return Err(LiveFoldError::ViewGenerationMismatch);
        }
        if self.refresh_changes_view(delta) && !delta.view_changed {
            self.state = LiveState::NeedsRebuild;
            return Err(LiveFoldError::RefreshMismatch);
        }
        if let Err(error) = validate_view_generation(
            delta.previous_view_generation,
            delta.new_view_generation,
            delta.view_changed,
        ) {
            self.state = match error {
                LiveFoldError::Overflow => LiveState::Incomplete,
                _ => LiveState::NeedsRebuild,
            };
            return Err(error);
        }
        if bootstrap {
            self.view_generation = delta.previous_view_generation;
            self.clear_folded(delta.journal.generation_id);
            self.baseline_pinned = true;
        }
        Ok(bootstrap)
    }

    fn refresh_changes_view(&self, delta: &RefreshDelta) -> bool {
        !delta.sealed_added.is_empty()
            || !delta.sealed_removed.is_empty()
            || !delta.journal.completed_parts.is_empty()
            || !delta.journal.transition.preserves_generation()
            || delta.journal.current_parts != self.folded_parts
            || delta.journal.new_valid_len != self.folded_through_offset
            || delta.journal.tail_pending != self.completed_tail_pending
    }

    /// Completes the pending refresh as one immutable availability boundary.
    ///
    /// A complete refresh must account for the exact ordered active descriptor
    /// set and end at the validated journal watermark. A single torn tail that
    /// starts at that watermark is valid pending input; middle or quarantined
    /// damage is not.
    ///
    /// # Errors
    ///
    /// Returns [`LiveFoldError::InvalidState`] without a matching
    /// [`begin_refresh`](Self::begin_refresh), or
    /// [`LiveFoldError::IncompleteRefresh`] when discovery or folding was not
    /// complete.
    pub fn complete_refresh(&mut self) -> Result<(), LiveFoldError> {
        let pending = self
            .pending_refresh
            .take()
            .ok_or(LiveFoldError::InvalidState)?;
        self.view_generation = pending.new_view_generation;
        if self.state != LiveState::Warming
            || !pending.current_parts_complete
            || self.folded_parts != pending.current_parts
            || self.folded_through_offset != pending.new_valid_len
            || (pending.current_parts.is_empty()
                && (pending.tail_pending.is_some() || !pending.damages.is_empty()))
            || !completion_damage_is_valid(
                pending.new_valid_len,
                pending.tail_pending,
                &pending.damages,
            )
        {
            self.state = LiveState::Incomplete;
            return Err(LiveFoldError::IncompleteRefresh);
        }

        self.state = if self.folded_parts.is_empty() {
            LiveState::Empty
        } else {
            LiveState::Current
        };
        self.completed_tail_pending = pending.tail_pending;
        Ok(())
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
        if self.state != LiveState::Warming || self.pending_refresh.is_none() {
            return Err(LiveFoldError::InvalidState);
        }
        if part.part_id.generation != self.generation {
            self.invalidate_pending(LiveState::NeedsRebuild);
            return Err(LiveFoldError::GenerationMismatch);
        }
        if self.folded_part_ids.contains(&part.part_id)
            && let Some(existing) = self
                .folded_parts
                .iter()
                .find(|descriptor| descriptor.part_id == part.part_id)
        {
            if existing == part {
                return Ok(FoldEffect::Duplicate);
            }
            self.invalidate_pending(LiveState::Incomplete);
            return Err(LiveFoldError::DescriptorMismatch);
        }
        if self
            .pending_refresh
            .as_ref()
            .and_then(|pending| pending.current_parts.get(self.folded_parts.len()))
            != Some(part)
        {
            self.invalidate_pending(LiveState::Incomplete);
            return Err(LiveFoldError::DescriptorMismatch);
        }
        let catalog = unit.catalog();
        if part.source_id != catalog.source_id
            || part.min_ts != catalog.min_ts
            || part.max_ts != catalog.max_ts
            || part.part_id.body_len != unit.source_file_len()
            || part.part_id.catalog_digest != refresh_catalog_digest(catalog)
        {
            self.invalidate_pending(LiveState::Incomplete);
            return Err(LiveFoldError::DescriptorMismatch);
        }
        if part.source_id != 0
            && self
                .source_id
                .is_some_and(|source_id| source_id != part.source_id)
        {
            self.invalidate_pending(LiveState::Incomplete);
            return Err(LiveFoldError::DescriptorMismatch);
        }
        let end_offset = part
            .part_id
            .frame_offset
            .checked_add(part.part_id.body_len)
            .ok_or_else(|| {
                self.invalidate_pending(LiveState::Incomplete);
                LiveFoldError::Overflow
            })?;
        if !self.chunks.is_empty() && part.part_id.frame_offset < self.folded_through_offset {
            self.invalidate_pending(LiveState::Incomplete);
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
        .map_err(LiveFoldError::Build);
        let facts = match facts {
            Ok(facts) => facts,
            Err(error) => {
                self.invalidate_pending(LiveState::Incomplete);
                return Err(error);
            }
        };
        let usage = match self.usage.checked_add(&facts) {
            Ok(usage) => usage,
            Err(error) => {
                self.invalidate_pending(LiveState::Incomplete);
                return Err(error);
            }
        };
        if !usage.is_within(&self.bounds) {
            self.invalidate_pending(LiveState::Incomplete);
            return Err(LiveFoldError::LimitExceeded);
        }

        self.chunks.push(Arc::new(facts));
        self.folded_part_ids.insert(part.part_id);
        self.folded_parts.push(*part);
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
        Ok(FoldEffect::Folded)
    }

    fn clear_folded(&mut self, generation: JournalGenerationId) {
        self.generation = generation;
        self.folded_part_ids.clear();
        self.folded_parts.clear();
        self.chunks.clear();
        self.usage = LiveUsage::default();
        self.source_id = None;
        self.watermark_us = None;
        self.folded_through_offset = 0;
        self.completed_tail_pending = None;
    }

    fn invalidate_pending(&mut self, state: LiveState) {
        self.pending_refresh = None;
        self.state = state;
    }

    /// Returns an immutable candidate snapshot of the folded state.
    ///
    /// The candidate copies at most the configured part count of shared
    /// pointers; observation payloads remain shared. Callers must still inspect
    /// its state, and queries enforce the same availability gate.
    #[must_use]
    pub fn publish(&self) -> LiveView {
        LiveView {
            view_generation: self.view_generation,
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

fn validate_view_generation(
    previous: u64,
    new: u64,
    changes_view: bool,
) -> Result<(), LiveFoldError> {
    if !changes_view {
        return (new == previous)
            .then_some(())
            .ok_or(LiveFoldError::ViewGenerationMismatch);
    }
    let next = previous.checked_add(1).ok_or(LiveFoldError::Overflow)?;
    (new == next)
        .then_some(())
        .ok_or(LiveFoldError::ViewGenerationMismatch)
}

fn validate_descriptor_sequence(
    parts: &[PartDescriptor],
    generation: JournalGenerationId,
    valid_len: u64,
) -> Result<(), LiveFoldError> {
    if parts.is_empty() {
        return if valid_len == 0 {
            Ok(())
        } else {
            Err(LiveFoldError::RefreshMismatch)
        };
    }

    let frame_header_len =
        u64::try_from(FRAME_HEADER_LEN).map_err(|_error| LiveFoldError::Overflow)?;
    let mut expected_body_offset = frame_header_len;
    let mut seen = BTreeSet::new();
    let mut last_end = 0_u64;
    for part in parts {
        if part.part_id.generation != generation {
            return Err(LiveFoldError::GenerationMismatch);
        }
        if !seen.insert(part.part_id) || part.part_id.frame_offset != expected_body_offset {
            return Err(LiveFoldError::RefreshMismatch);
        }
        last_end = part
            .part_id
            .frame_offset
            .checked_add(part.part_id.body_len)
            .ok_or(LiveFoldError::Overflow)?;
        expected_body_offset = last_end
            .checked_add(frame_header_len)
            .ok_or(LiveFoldError::Overflow)?;
    }
    if last_end != valid_len {
        return Err(LiveFoldError::RefreshMismatch);
    }
    Ok(())
}

fn completion_damage_is_valid(
    valid_len: u64,
    tail_pending: Option<ByteRange>,
    damages: &[DamageRegion],
) -> bool {
    match (tail_pending, damages) {
        (None, []) => true,
        (Some(tail), [damage]) => {
            let Ok(damage_from) = u64::try_from(damage.from) else {
                return false;
            };
            tail.start == valid_len
                && tail.end > tail.start
                && damage_from == valid_len
                && damage.kind == DamageKind::TornTail
        }
        _ => false,
    }
}

/// An immutable snapshot of the live view at one publication.
#[derive(Debug, Clone)]
pub struct LiveView {
    view_generation: u64,
    generation: JournalGenerationId,
    state: LiveState,
    watermark_us: Option<i64>,
    folded_through_offset: u64,
    chunks: Vec<Arc<SegmentFacts>>,
}

impl LiveView {
    /// Snapshot generation captured at this boundary.
    #[must_use]
    pub const fn view_generation(&self) -> u64 {
        self.view_generation
    }

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

    /// Checked logical resident charge for this view and its fact chunks.
    ///
    /// The charge includes the view, reserved chunk slots, `Arc` counters, and
    /// each referenced fact set. It returns `None` if a platform-sized total
    /// cannot be represented.
    #[must_use]
    pub fn resident_bytes(&self) -> Option<usize> {
        const ARC_COUNTER_BYTES: usize = 2 * size_of::<usize>();

        let chunk_slots = self
            .chunks
            .capacity()
            .checked_mul(size_of::<Arc<SegmentFacts>>())?;
        self.chunks.iter().try_fold(
            size_of::<Self>().checked_add(chunk_slots)?,
            |total, facts| {
                total
                    .checked_add(ARC_COUNTER_BYTES)?
                    .checked_add(facts.resident_bytes()?)
            },
        )
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
        if !matches!(self.state, LiveState::Empty | LiveState::Current) {
            return Err(OracleError::Source(OracleSourceError::SnapshotUnavailable));
        }
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
        facts: Arc<SegmentFacts>,
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
    pub fn facts(&self) -> &SegmentFacts {
        match self {
            Self::Promoted { facts, .. } => facts.as_ref(),
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
        let (facts, persist_error) = store.admit_publish_or_fallback(&promoted, bounds)?;
        return Ok(SealOutcome::Promoted {
            facts,
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
    use crate::refresh::{PartTransition, part_id};

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

    type EncodedSections = Vec<(u32, u32, Vec<u8>)>;

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
    ) -> (Vec<u8>, EncodedSections) {
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

    fn dictionary_only_part() -> (Vec<u8>, EncodedSections) {
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
        seal_sections_for_source(sections, min_ts, max_ts, 7)
    }

    fn seal_sections_for_source(
        sections: &[(u32, u32, Vec<u8>)],
        min_ts: i64,
        max_ts: i64,
        source_id: u64,
    ) -> Vec<u8> {
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
                source_id,
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

    fn descriptor(
        generation: JournalGenerationId,
        frame_offset: u64,
        bytes: &[u8],
    ) -> PartDescriptor {
        let unit = PgmUnit::open(bytes).expect("open descriptor part");
        PartDescriptor {
            part_id: part_id(
                generation,
                frame_offset,
                u64::try_from(bytes.len()).expect("len fits"),
                unit.catalog(),
            ),
            source_id: unit.catalog().source_id,
            min_ts: unit.catalog().min_ts,
            max_ts: unit.catalog().max_ts,
        }
    }

    fn complete_delta(
        builder: &LiveBuilder,
        current_parts: Vec<PartDescriptor>,
        transition: PartTransition,
    ) -> RefreshDelta {
        let generation_id = if transition.preserves_generation() {
            builder.generation()
        } else {
            JournalGenerationId(
                builder
                    .generation()
                    .0
                    .checked_add(1)
                    .expect("test generation fits"),
            )
        };
        let current_parts: Vec<_> = current_parts
            .into_iter()
            .map(|mut part| {
                part.part_id.generation = generation_id;
                part
            })
            .collect();
        let completed_parts = if transition.preserves_generation() {
            current_parts[builder.folded_parts.len()..].to_vec()
        } else {
            current_parts.clone()
        };
        let new_valid_len = current_parts
            .last()
            .map_or(0, |part| part.part_id.frame_offset + part.part_id.body_len);
        RefreshDelta {
            previous_view_generation: builder.view_generation(),
            new_view_generation: builder.view_generation() + 1,
            view_changed: true,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: crate::refresh::JournalDelta {
                generation_id,
                bootstrap: !builder.baseline_pinned,
                previous_valid_len: builder.folded_through_offset(),
                new_valid_len,
                completed_parts,
                current_parts,
                current_parts_complete: true,
                transition,
                tail_pending: None,
                damages: Vec::new(),
            },
        }
    }

    fn descriptors_for_bytes(builder: &LiveBuilder, bytes: &[&[u8]]) -> Vec<PartDescriptor> {
        let mut frame_offset = u64::try_from(FRAME_HEADER_LEN).expect("header length fits");
        bytes
            .iter()
            .map(|part_bytes| {
                let part = descriptor(builder.generation(), frame_offset, part_bytes);
                frame_offset = part
                    .part_id
                    .frame_offset
                    .checked_add(part.part_id.body_len)
                    .and_then(|end| {
                        end.checked_add(
                            u64::try_from(FRAME_HEADER_LEN).expect("header length fits"),
                        )
                    })
                    .expect("test journal length fits");
                part
            })
            .collect()
    }

    fn fold_bytes(builder: &mut LiveBuilder, bytes: &[&[u8]]) {
        let descriptors = descriptors_for_bytes(builder, bytes);
        let delta = complete_delta(builder, descriptors.clone(), PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
        for (part_bytes, descriptor) in bytes.iter().zip(&descriptors) {
            let unit = PgmUnit::open(*part_bytes).expect("open part");
            builder.fold_part(descriptor, &unit).expect("fold part");
        }
        builder.complete_refresh().expect("complete refresh");
    }

    fn fold_slices(builder: &mut LiveBuilder, slices: &[&[PgLogLifecycleV1]]) {
        let bytes: Vec<_> = slices.iter().map(|rows| lifecycle_part(rows)).collect();
        let slices: Vec<_> = bytes.iter().map(Vec::as_slice).collect();
        fold_bytes(builder, &slices);
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
    fn a_fresh_builder_is_unavailable_until_an_authoritative_delta() {
        let builder = live_builder();
        assert_eq!(builder.state(), LiveState::Warming);
        assert_eq!(builder.folded_part_count(), 0);
        assert_eq!(builder.watermark_us(), None);
        assert_eq!(
            builder.publish().query(full_span(), LIMITS),
            Err(OracleError::Source(OracleSourceError::SnapshotUnavailable))
        );
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
    fn a_part_fold_remains_unavailable_until_the_whole_refresh_completes() {
        let mut builder = live_builder();
        let bytes = lifecycle_part(&stream());
        let parts = descriptors_for_bytes(&builder, &[bytes.as_slice()]);
        let delta = complete_delta(&builder, parts.clone(), PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        builder.fold_part(&parts[0], &unit).expect("fold part");

        let candidate = builder.publish();
        assert_eq!(candidate.state(), LiveState::Warming);
        assert!(!candidate.is_current());
        assert_eq!(
            candidate.query(full_span(), LIMITS),
            Err(OracleError::Source(OracleSourceError::SnapshotUnavailable))
        );

        builder.complete_refresh().expect("complete refresh");
        assert_eq!(builder.state(), LiveState::Current);
        builder
            .publish()
            .query(full_span(), LIMITS)
            .expect("completed query");
    }

    #[test]
    fn completion_rejects_a_missing_descriptor_from_the_current_set() {
        let rows = stream();
        let bytes = [lifecycle_part(&rows[..3]), lifecycle_part(&rows[3..])];
        let slices = [bytes[0].as_slice(), bytes[1].as_slice()];
        let mut builder = live_builder();
        let parts = descriptors_for_bytes(&builder, &slices);
        let delta = complete_delta(&builder, parts.clone(), PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
        let unit = PgmUnit::open(bytes[0].as_slice()).expect("open first part");
        builder
            .fold_part(&parts[0], &unit)
            .expect("fold first part");

        assert_eq!(
            builder.complete_refresh(),
            Err(LiveFoldError::IncompleteRefresh)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
        assert_eq!(
            builder.publish().query(full_span(), LIMITS),
            Err(OracleError::Source(OracleSourceError::SnapshotUnavailable))
        );
    }

    #[test]
    fn a_fresh_builder_accepts_a_truthful_bootstrap_watermark() {
        let mut builder = live_builder();
        let bytes = lifecycle_part(&stream());
        let parts = descriptors_for_bytes(&builder, &[bytes.as_slice()]);
        let mut delta = complete_delta(&builder, parts.clone(), PartTransition::Append);
        delta.journal.bootstrap = true;
        delta.journal.previous_valid_len = delta.journal.new_valid_len;
        builder.begin_refresh(&delta).expect("begin bootstrap");
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        builder.fold_part(&parts[0], &unit).expect("fold bootstrap");
        builder.complete_refresh().expect("complete bootstrap");

        assert_eq!(builder.state(), LiveState::Current);
        assert_eq!(builder.view_generation(), delta.new_view_generation);
    }

    #[test]
    fn a_fresh_builder_pins_nonzero_bootstrap_generations() {
        let mut builder = live_builder();
        let bytes = lifecycle_part(&stream());
        let mut parts = descriptors_for_bytes(&builder, &[bytes.as_slice()]);
        for part in &mut parts {
            part.part_id.generation = JournalGenerationId(41);
        }
        let mut delta = complete_delta(&builder, parts.clone(), PartTransition::Append);
        delta.previous_view_generation = 17;
        delta.new_view_generation = 18;
        delta.journal.generation_id = JournalGenerationId(41);
        delta.journal.completed_parts.clone_from(&parts);
        delta.journal.current_parts.clone_from(&parts);

        builder.begin_refresh(&delta).expect("begin bootstrap");
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        builder.fold_part(&parts[0], &unit).expect("fold part");
        builder.complete_refresh().expect("complete bootstrap");

        assert_eq!(builder.view_generation(), 18);
        assert_eq!(builder.generation(), JournalGenerationId(41));
        assert_eq!(builder.state(), LiveState::Current);
    }

    #[test]
    fn a_bootstrap_delta_is_rejected_after_the_initial_boundary() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        let mut delta = complete_delta(
            &builder,
            builder.folded_parts.clone(),
            PartTransition::Append,
        );
        delta.journal.bootstrap = true;

        assert_eq!(
            builder.begin_refresh(&delta),
            Err(LiveFoldError::RefreshMismatch)
        );
        assert_eq!(builder.state(), LiveState::NeedsRebuild);
    }

    #[test]
    fn an_unchanged_boundary_can_retain_the_maximum_view_generation() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        builder.view_generation = u64::MAX;
        let delta = RefreshDelta {
            previous_view_generation: u64::MAX,
            new_view_generation: u64::MAX,
            view_changed: false,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: crate::refresh::JournalDelta {
                bootstrap: false,
                generation_id: builder.generation(),
                previous_valid_len: builder.folded_through_offset(),
                new_valid_len: builder.folded_through_offset(),
                completed_parts: Vec::new(),
                current_parts: builder.folded_parts.clone(),
                current_parts_complete: true,
                transition: PartTransition::Append,
                tail_pending: builder.completed_tail_pending,
                damages: Vec::new(),
            },
        };

        builder.begin_refresh(&delta).expect("begin no-op");
        builder.complete_refresh().expect("complete no-op");
        assert_eq!(builder.view_generation(), u64::MAX);
        assert_eq!(builder.state(), LiveState::Current);
    }

    #[test]
    fn producer_visible_warning_change_can_advance_an_unchanged_descriptor_view() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        let delta = RefreshDelta {
            previous_view_generation: builder.view_generation(),
            new_view_generation: builder.view_generation() + 1,
            view_changed: true,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: crate::refresh::JournalDelta {
                bootstrap: false,
                generation_id: builder.generation(),
                previous_valid_len: builder.folded_through_offset(),
                new_valid_len: builder.folded_through_offset(),
                completed_parts: Vec::new(),
                current_parts: builder.folded_parts.clone(),
                current_parts_complete: true,
                transition: PartTransition::Append,
                tail_pending: builder.completed_tail_pending,
                damages: Vec::new(),
            },
        };

        builder.begin_refresh(&delta).expect("begin warning change");
        builder.complete_refresh().expect("complete warning change");
        assert_eq!(builder.view_generation(), delta.new_view_generation);
        assert_eq!(builder.state(), LiveState::Current);
    }

    #[test]
    fn a_changed_boundary_must_advance_the_view_generation() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        let delta = complete_delta(&builder, Vec::new(), PartTransition::Reset);
        let stale = RefreshDelta {
            new_view_generation: delta.previous_view_generation,
            ..delta
        };

        assert_eq!(
            builder.begin_refresh(&stale),
            Err(LiveFoldError::ViewGenerationMismatch)
        );
        assert_eq!(builder.state(), LiveState::NeedsRebuild);
    }

    #[test]
    fn a_clean_reset_completes_as_empty() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        let delta = complete_delta(&builder, Vec::new(), PartTransition::Reset);

        builder.begin_refresh(&delta).expect("begin reset");
        assert_eq!(builder.state(), LiveState::Warming);
        builder.complete_refresh().expect("complete reset");

        assert_eq!(builder.state(), LiveState::Empty);
        assert_eq!(builder.folded_part_count(), 0);
        assert_eq!(builder.watermark_us(), None);
        let result = builder
            .publish()
            .query(full_span(), LIMITS)
            .expect("empty query");
        assert!(result.observations().is_empty());
    }

    #[test]
    fn a_validated_prefix_with_a_torn_tail_can_complete() {
        let mut builder = live_builder();
        let bytes = lifecycle_part(&stream());
        let parts = descriptors_for_bytes(&builder, &[bytes.as_slice()]);
        let mut delta = complete_delta(&builder, parts.clone(), PartTransition::Append);
        let valid_len = delta.journal.new_valid_len;
        delta.journal.tail_pending = Some(ByteRange {
            start: valid_len,
            end: valid_len + 3,
        });
        delta.journal.damages = vec![DamageRegion {
            from: usize::try_from(valid_len).expect("valid length fits"),
            kind: DamageKind::TornTail,
        }];
        builder.begin_refresh(&delta).expect("begin refresh");
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        builder.fold_part(&parts[0], &unit).expect("fold part");

        builder.complete_refresh().expect("complete valid prefix");
        assert_eq!(builder.state(), LiveState::Current);
    }

    #[test]
    fn a_torn_tail_without_a_valid_part_is_not_a_clean_empty_view() {
        let mut builder = live_builder();
        let mut delta = complete_delta(&builder, Vec::new(), PartTransition::Append);
        delta.journal.tail_pending = Some(ByteRange { start: 0, end: 3 });
        delta.journal.damages = vec![DamageRegion {
            from: 0,
            kind: DamageKind::TornTail,
        }];
        builder.begin_refresh(&delta).expect("begin refresh");

        assert_eq!(
            builder.complete_refresh(),
            Err(LiveFoldError::IncompleteRefresh)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
    }

    #[test]
    fn non_authoritative_active_discovery_never_completes() {
        let mut builder = live_builder();
        let mut delta = complete_delta(&builder, Vec::new(), PartTransition::Append);
        delta.journal.current_parts_complete = false;
        builder.begin_refresh(&delta).expect("begin refresh");

        assert_eq!(
            builder.complete_refresh(),
            Err(LiveFoldError::IncompleteRefresh)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
    }

    #[test]
    fn an_invalid_end_watermark_makes_the_prior_view_unavailable() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        let mut delta = complete_delta(
            &builder,
            builder.folded_parts.clone(),
            PartTransition::Append,
        );
        delta.journal.new_valid_len += 1;

        assert_eq!(
            builder.begin_refresh(&delta),
            Err(LiveFoldError::RefreshMismatch)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
        assert_eq!(
            builder.publish().query(full_span(), LIMITS),
            Err(OracleError::Source(OracleSourceError::SnapshotUnavailable))
        );
    }

    #[test]
    fn re_delivering_the_same_part_is_idempotent() {
        let mut builder = live_builder();
        let rows = stream();
        let bytes = lifecycle_part(&rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let descriptor = descriptor(
            builder.generation(),
            u64::try_from(FRAME_HEADER_LEN).expect("header length fits"),
            &bytes,
        );
        let delta = complete_delta(&builder, vec![descriptor], PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
        assert_eq!(
            builder.fold_part(&descriptor, &unit).expect("fold"),
            FoldEffect::Folded
        );
        assert_eq!(
            builder.fold_part(&descriptor, &unit).expect("re-fold"),
            FoldEffect::Duplicate
        );
        builder.complete_refresh().expect("complete refresh");
        assert_eq!(builder.folded_part_count(), 1, "redelivery adds no chunk");
    }

    #[test]
    fn a_part_from_a_different_generation_is_rejected() {
        let mut builder = live_builder();
        let rows = stream();
        let bytes = lifecycle_part(&rows);
        let unit = PgmUnit::open(bytes.as_slice()).expect("open part");
        let expected = descriptor(
            builder.generation(),
            u64::try_from(FRAME_HEADER_LEN).expect("header length fits"),
            &bytes,
        );
        let delta = complete_delta(&builder, vec![expected], PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
        let mut descriptor = expected;
        descriptor.part_id.generation = JournalGenerationId(99);
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
        let expected = descriptor(
            builder.generation(),
            u64::try_from(FRAME_HEADER_LEN).expect("header length fits"),
            &bytes,
        );
        let delta = complete_delta(&builder, vec![expected], PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
        let descriptor = PartDescriptor {
            source_id: unit.catalog().source_id + 1,
            ..expected
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
        let descriptor = descriptor(builder.generation(), 17, &bytes);
        let mut current = builder.folded_parts.clone();
        current.push(descriptor);
        let delta = complete_delta(&builder, current, PartTransition::Append);
        assert_eq!(
            builder.begin_refresh(&delta),
            Err(LiveFoldError::RefreshMismatch)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
    }

    #[test]
    fn reset_discards_folded_state_and_enters_warming() {
        let mut builder = live_builder();
        fold_slices(&mut builder, &[&stream()]);
        let delta = complete_delta(&builder, Vec::new(), PartTransition::Reset);
        builder.reset_to(delta.journal.generation_id);
        assert_eq!(builder.state(), LiveState::Warming);
        assert_eq!(builder.generation(), delta.journal.generation_id);
        assert_eq!(builder.folded_part_count(), 0);
        assert_eq!(builder.watermark_us(), None);
        builder.begin_refresh(&delta).expect("begin rebaseline");
        builder.complete_refresh().expect("complete rebaseline");
        assert_eq!(builder.state(), LiveState::Empty);
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
        fs::set_permissions(cache_dir.path(), fs::Permissions::from_mode(0o000))
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
        assert_eq!(store.fallback_stats().resident_entries, 1);
        let fallback = store
            .load_or_build(&sealed_unit, &context, &LIMIT)
            .expect("promoted fallback load");
        assert_eq!(fallback.origin(), super::super::FactOrigin::FallbackHit);
        assert_eq!(
            fallback.pgm_body_read_stats(),
            crate::PgmBodyReadStats::default()
        );
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
        fold_bytes(
            &mut builder,
            &[first_bytes.as_slice(), second_bytes.as_slice()],
        );

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
        fold_bytes(
            &mut builder,
            &[dictionary_bytes.as_slice(), lifecycle_bytes.as_slice()],
        );
        assert_eq!(builder.watermark_us(), Some(1_000));

        let (_cache_dir, store) = store();
        let outcome = reconcile_seal(&builder.publish(), &sealed_unit, &context, &store, &LIMIT)
            .expect("reconcile");
        assert!(outcome.was_promoted());
        assert_eq!(outcome.facts().observations(), rebuilt.observations());
    }

    #[test]
    fn source_zero_parts_do_not_cross_store_namespaces() {
        let (dictionary_bytes, sections) = dictionary_only_part();
        let sealed_bytes = seal_sections_for_source(&sections, 0, 0, 0);
        let sealed_unit = PgmUnit::open(sealed_bytes.as_slice()).expect("open sealed dictionary");
        let mut builder =
            LiveBuilder::new(b"another-store".to_vec(), LIMIT).expect("valid live builder");
        fold_bytes(&mut builder, &[dictionary_bytes.as_slice()]);

        let (_cache_dir, store) = store();
        let outcome = reconcile_seal(
            &builder.publish(),
            &sealed_unit,
            &sealed_context(),
            &store,
            &LIMIT,
        )
        .expect("reconcile");

        assert!(
            !outcome.was_promoted(),
            "source zero still carries the store namespace scope"
        );
        assert!(
            outcome.persist_error().is_none(),
            "an empty timestamp envelope remains durably admissible"
        );
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
        fold_bytes(
            &mut builder,
            &[first_bytes.as_slice(), second_bytes.as_slice()],
        );

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
            let mut live_parts = Vec::new();
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
                    live_parts.push(lifecycle_part(group));
                }
            }
            if !live_parts.is_empty() {
                let live_slices: Vec<_> = live_parts.iter().map(Vec::as_slice).collect();
                fold_bytes(&mut builder, &live_slices);
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

    fn empty_interval_gap_part(ts: i64) -> Vec<u8> {
        let gaps = [kronika_registry::pg_log::PgLogGapV1 {
            ts: Ts(ts),
            source_path: None,
            parser_kind: 0,
            reason: 1,
            dev: Some(1),
            inode: Some(2),
            offset: Some(3),
            bytes_skipped: 1,
            truncated_lines: 0,
            invalid_utf8: 0,
            binary_dropped: 0,
            rotations: 0,
            missing_files: 0,
            budget_exhaustions: 0,
            dict_dropped_fields: 0,
            parser_dropped_lines: 0,
        }];
        let gap_body = kronika_registry::pg_log::PgLogGapV1::encode(&gaps).expect("encode gap");
        build_part(
            &[SectionInput {
                type_id: 1_029_001,
                rows: 1,
                body: &gap_body,
            }],
            PartMeta {
                min_ts: i64::MAX,
                max_ts: i64::MIN,
                source_id: 7,
            },
        )
    }

    #[test]
    fn cumulative_known_gap_spans_have_an_independent_bound() {
        let tight = Bounds {
            coverage_spans: 1,
            ..LIMIT
        };
        let mut builder = LiveBuilder::new(NAMESPACE.to_vec(), tight).expect("valid live builder");
        let bytes = [
            empty_interval_gap_part(1_000),
            empty_interval_gap_part(2_000),
        ];
        let slices = [bytes[0].as_slice(), bytes[1].as_slice()];
        let parts = descriptors_for_bytes(&builder, &slices);
        let delta = complete_delta(&builder, parts.clone(), PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");

        let first = PgmUnit::open(bytes[0].as_slice()).expect("open first gap");
        assert_eq!(builder.fold_part(&parts[0], &first), Ok(FoldEffect::Folded));
        let second = PgmUnit::open(bytes[1].as_slice()).expect("open second gap");
        assert_eq!(
            builder.fold_part(&parts[1], &second),
            Err(LiveFoldError::LimitExceeded)
        );
        assert_eq!(builder.state(), LiveState::Incomplete);
    }

    #[test]
    fn a_timestamp_fallback_gap_rebuilds_instead_of_promoting() {
        let part_bytes = fallback_gap_part();
        let sealed_unit = PgmUnit::open(part_bytes.as_slice()).expect("open sealed");
        let context = sealed_context();
        let rebuilt = SegmentFacts::extract(&sealed_unit, &context, &LIMIT).expect("rebuild");

        let mut builder = live_builder();
        fold_bytes(&mut builder, &[part_bytes.as_slice()]);

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
        let frame_offset = builder
            .folded_through_offset()
            .checked_add(u64::try_from(FRAME_HEADER_LEN).expect("header length fits"))
            .expect("offset fits");
        let descriptor = descriptor(builder.generation(), frame_offset, &bytes);
        let mut current_parts = builder.folded_parts.clone();
        current_parts.push(descriptor);
        let delta = complete_delta(&builder, current_parts, PartTransition::Append);
        builder.begin_refresh(&delta).expect("begin refresh");
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
