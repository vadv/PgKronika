//! Canonical descriptor selection for one timeline request.

use std::sync::Arc;

use kronika_analytics::overview::CoverageSpan;
use kronika_reader::{LiveState, LiveView, SealedLocator};
use sha2::{Digest, Sha256};

use super::view::{DescriptorEntry, DescriptorSource, DescriptorView};

pub(crate) const DEFAULT_MAX_SELECTED_SEGMENTS: usize = 1_024;
pub(crate) const ABSOLUTE_MAX_SELECTED_SEGMENTS: usize = 4_096;

const SELECTED_FACT_SET_DOMAIN: &[u8] = b"pgk-overview-selected-fact-set-v1";

/// Descriptor selection failed before fact access or request-level work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionError {
    InvalidLimit,
    SourcesNotCanonical,
    LimitExceeded { limit: usize },
}

/// Immutable, bounded descriptor plan for one canonical source/range request.
#[derive(Debug, Clone)]
pub(crate) struct SelectedSealedPlan {
    view: Arc<DescriptorView>,
    selected_indices: Vec<usize>,
    source_descriptors: Vec<DescriptorSource>,
    sealed_gap: bool,
    fact_set_id: [u8; 32],
}

impl SelectedSealedPlan {
    pub(crate) fn build(
        view: Arc<DescriptorView>,
        sources: &[u64],
        range: CoverageSpan,
        limit: usize,
    ) -> Result<Self, SelectionError> {
        if limit == 0 || limit > ABSOLUTE_MAX_SELECTED_SEGMENTS {
            return Err(SelectionError::InvalidLimit);
        }
        if sources.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(SelectionError::SourcesNotCanonical);
        }

        let stop_after = limit.checked_add(1).ok_or(SelectionError::InvalidLimit)?;
        let mut selected_indices = Vec::with_capacity(stop_after.min(64));
        for source in sources {
            if view.extend_selected(*source, range, stop_after, &mut selected_indices) {
                return Err(SelectionError::LimitExceeded { limit });
            }
        }

        let source_descriptors = view.sources_for(sources);
        let sealed_gap = sources
            .iter()
            .any(|source| view.unavailable_intersects(*source, range));
        let fact_set_id =
            selected_fact_set_id(&view, sources, range, &selected_indices, sealed_gap);
        Ok(Self {
            view,
            selected_indices,
            source_descriptors,
            sealed_gap,
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

    pub(crate) const fn sealed_gap(&self) -> bool {
        self.sealed_gap
    }

    pub(crate) const fn fact_set_id(&self) -> [u8; 32] {
        self.fact_set_id
    }

    pub(crate) const fn view(&self) -> &Arc<DescriptorView> {
        &self.view
    }

    pub(crate) fn source_descriptors(&self) -> &[DescriptorSource] {
        &self.source_descriptors
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
    sources: &[u64],
    range: CoverageSpan,
    indices: &[usize],
    sealed_gap: bool,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SELECTED_FACT_SET_DOMAIN);
    hasher.update(view.view_generation().to_le_bytes());
    hasher.update(range.start_us().to_le_bytes());
    hasher.update(range.end_us().to_le_bytes());
    hasher.update((sources.len() as u64).to_le_bytes());
    for source in sources {
        hasher.update(source.to_le_bytes());
    }
    hasher.update((indices.len() as u64).to_le_bytes());
    for index in indices {
        let entry = view.entry(*index);
        hasher.update(entry.descriptor().locator.as_bytes());
        hasher.update(entry.fact_build_key().fact_key().as_bytes());
        hasher.update(entry.fact_build_key().segment_lineage_id().0);
    }
    hasher.update([u8::from(sealed_gap)]);
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
