//! Canonical descriptor selection for one timeline request.

use std::sync::Arc;

use kronika_analytics::overview::CoverageSpan;
use kronika_reader::{LiveState, LiveView, SealedLocator, SegmentDescriptor};
use sha2::{Digest, Sha256};

use super::view::{DescriptorEntry, DescriptorFreshness, DescriptorView, OmittedRange};

pub(crate) const DEFAULT_MAX_SELECTED_SEGMENTS: usize = 1_024;
pub(crate) const ABSOLUTE_MAX_SELECTED_SEGMENTS: usize = 4_096;

const SELECTED_FACT_SET_DOMAIN: &[u8] = b"pgk-overview-selected-fact-set-v1";
const PARTIAL_FACT_SET_DOMAIN: &[u8] = b"pgk-overview-partial-fact-set-v1";

/// Descriptor selection failed before fact access or request-level work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionError {
    InvalidLimit,
    LimitExceeded { limit: usize },
}

/// Immutable, bounded descriptor plan for one root/range request.
#[derive(Debug, Clone)]
pub(crate) struct SelectedSealedPlan {
    view: Arc<DescriptorView>,
    selected_indices: Vec<usize>,
    freshness: DescriptorFreshness,
    omitted_ranges: Vec<OmittedRange>,
    range: CoverageSpan,
    fact_set_id: [u8; 32],
}

impl SelectedSealedPlan {
    pub(crate) fn build(
        view: Arc<DescriptorView>,
        range: CoverageSpan,
        limit: usize,
    ) -> Result<Self, SelectionError> {
        if limit == 0 || limit > ABSOLUTE_MAX_SELECTED_SEGMENTS {
            return Err(SelectionError::InvalidLimit);
        }
        let stop_after = limit.checked_add(1).ok_or(SelectionError::InvalidLimit)?;
        let mut selected_indices = Vec::with_capacity(stop_after.min(64));
        if view.extend_selected_with_halo(range, stop_after, &mut selected_indices) {
            return Err(SelectionError::LimitExceeded { limit });
        }

        let freshness = view.freshness();
        let mut omitted_ranges = Vec::new();
        if view.extend_unavailable_gaps(range, stop_after, &mut omitted_ranges) {
            return Err(SelectionError::LimitExceeded { limit });
        }
        let fact_set_id = selected_fact_set_id(&view, range, &selected_indices, &omitted_ranges);
        Ok(Self {
            view,
            selected_indices,
            freshness,
            omitted_ranges,
            range,
            fact_set_id,
        })
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &DescriptorEntry> {
        self.selected_indices
            .iter()
            .map(|index| self.view.entry(*index))
    }

    #[cfg(test)]
    pub(crate) const fn selected_count(&self) -> usize {
        self.selected_indices.len()
    }

    #[cfg(test)]
    pub(crate) const fn sealed_gap(&self) -> bool {
        !self.omitted_ranges.is_empty()
    }

    pub(crate) fn omitted_ranges(&self) -> &[OmittedRange] {
        &self.omitted_ranges
    }

    pub(crate) fn gap_for(&self, descriptor: &SegmentDescriptor) -> OmittedRange {
        let start = descriptor.min_ts.max(self.range.start_us());
        let end = descriptor.max_ts.saturating_add(1).min(self.range.end_us());
        let span = CoverageSpan::new(start, end).unwrap_or_else(|| {
            if descriptor.max_ts < self.range.start_us() {
                CoverageSpan::new(
                    self.range.start_us(),
                    self.range
                        .start_us()
                        .saturating_add(1)
                        .min(self.range.end_us()),
                )
                .expect("a valid request range has a left boundary interval")
            } else {
                CoverageSpan::new(
                    self.range
                        .start_us()
                        .max(self.range.end_us().saturating_sub(1)),
                    self.range.end_us(),
                )
                .expect("a valid request range has a right boundary interval")
            }
        });
        OmittedRange::new(span)
    }

    pub(crate) fn fact_set_id_with_gaps(&self, gaps: &[OmittedRange]) -> [u8; 32] {
        if gaps == self.omitted_ranges {
            return self.fact_set_id;
        }
        let mut hasher = Sha256::new();
        hasher.update(PARTIAL_FACT_SET_DOMAIN);
        hasher.update(self.fact_set_id);
        hasher.update((gaps.len() as u64).to_le_bytes());
        for gap in gaps {
            hasher.update(gap.span().start_us().to_le_bytes());
            hasher.update(gap.span().end_us().to_le_bytes());
        }
        hasher.finalize().into()
    }

    pub(crate) const fn fact_set_id(&self) -> [u8; 32] {
        self.fact_set_id
    }

    pub(crate) const fn view(&self) -> &Arc<DescriptorView> {
        &self.view
    }

    pub(crate) const fn freshness(&self) -> DescriptorFreshness {
        self.freshness
    }

    pub(crate) fn store_data_through_us(&self) -> Option<i64> {
        self.view.data_through_us()
    }

    pub(crate) fn promotion_for(&self, locator: SealedLocator) -> Option<Arc<LiveView>> {
        self.view.promotion_for(locator)
    }
}

fn selected_fact_set_id(
    view: &DescriptorView,
    range: CoverageSpan,
    indices: &[usize],
    omitted_ranges: &[OmittedRange],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SELECTED_FACT_SET_DOMAIN);
    hasher.update(view.view_generation().to_le_bytes());
    hasher.update(range.start_us().to_le_bytes());
    hasher.update(range.end_us().to_le_bytes());
    hasher.update((indices.len() as u64).to_le_bytes());
    for index in indices {
        let entry = view.entry(*index);
        hasher.update(entry.descriptor().locator.as_bytes());
        hasher.update(entry.fact_build_key().fact_key().as_bytes());
        hasher.update(entry.fact_build_key().segment_lineage_id().0);
    }
    hasher.update((omitted_ranges.len() as u64).to_le_bytes());
    for gap in omitted_ranges {
        hasher.update(gap.span().start_us().to_le_bytes());
        hasher.update(gap.span().end_us().to_le_bytes());
    }
    let live = view.live();
    hasher.update(live.generation().0.to_le_bytes());
    hasher.update(live.folded_through_offset().to_le_bytes());
    hasher.update(live.view_generation().to_le_bytes());
    hasher.update([live_state_tag(live.state())]);
    hasher.finalize().into()
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
