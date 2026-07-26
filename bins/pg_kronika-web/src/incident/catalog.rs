//! Bounded accounting for the registered incident evaluator catalog.

use std::collections::BTreeSet;

use super::lenses::LensMetadata;
use super::{core_catalog, event_catalog_metadata, inactive_catalog};

/// Exact scopes published with the incident catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IncidentCatalogCounts {
    pub core_lenses: usize,
    pub event_branches: usize,
    pub evaluator_branches: usize,
    pub unique_lens_ids: usize,
    pub active_lens_ids: usize,
    pub inactive_lens_ids: usize,
    pub entity_join_requirements: usize,
}

/// The unique stable IDs registered by either evaluator family.
pub(crate) fn registered_lens_ids() -> Vec<&'static str> {
    core_catalog()
        .iter()
        .map(LensMetadata::lens_id)
        .chain(event_catalog_metadata().iter().map(|branch| branch.lens_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Derive every count from the executable catalogs and neutral metadata.
pub(crate) fn catalog_counts() -> IncidentCatalogCounts {
    let core_lenses = core_catalog().len();
    let event_branches = event_catalog_metadata().len();
    let unique_lens_ids = registered_lens_ids().len();
    let entity_join_requirements = core_catalog()
        .iter()
        .filter(|lens| lens.entity_join_contract().is_some())
        .count();
    IncidentCatalogCounts {
        core_lenses,
        event_branches,
        evaluator_branches: core_lenses.saturating_add(event_branches),
        unique_lens_ids,
        active_lens_ids: unique_lens_ids,
        inactive_lens_ids: inactive_catalog().len(),
        entity_join_requirements,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::event_catalog_ids;

    #[test]
    fn executable_catalogs_define_the_normative_counts() {
        let counts = catalog_counts();
        assert_eq!(counts.core_lenses, 28);
        assert_eq!(counts.event_branches, 14);
        assert_eq!(counts.evaluator_branches, 42);
        assert_eq!(counts.unique_lens_ids, 40);
        assert_eq!(counts.active_lens_ids, 40);
        assert_eq!(counts.inactive_lens_ids, 0);
        assert_eq!(counts.entity_join_requirements, 24);
    }

    #[test]
    fn event_metadata_and_evaluator_catalog_stay_aligned() {
        assert_eq!(
            event_catalog_metadata()
                .iter()
                .map(|entry| entry.lens_id)
                .collect::<Vec<_>>(),
            event_catalog_ids()
        );
    }
}
