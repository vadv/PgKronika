//! Readiness metadata for diagnostic lenses.

/// A lens that cannot run until its typed inputs reach the incident engine.
pub(crate) struct DormantLens {
    lens_id: &'static str,
    awaiting: &'static [&'static str],
}

impl DormantLens {
    pub(crate) const fn lens_id(&self) -> &'static str {
        self.lens_id
    }

    pub(crate) const fn awaiting(&self) -> &'static [&'static str] {
        self.awaiting
    }
}

const LOCK_PREREQUISITES: &[&str] = &["sampled_blocked_by_edges", "lock_snapshot_coverage"];

const DORMANT_CATALOG: &[DormantLens] = &[DormantLens {
    lens_id: "PG-LOCK-012",
    awaiting: LOCK_PREREQUISITES,
}];

/// Returns catalog entries whose required typed inputs are not available.
pub(crate) const fn dormant_catalog() -> &'static [DormantLens] {
    DORMANT_CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_lens_waits_for_exact_edge_and_snapshot_quality() {
        let catalog = dormant_catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].lens_id(), "PG-LOCK-012");
        assert_eq!(
            catalog[0].awaiting(),
            ["sampled_blocked_by_edges", "lock_snapshot_coverage"]
        );
    }

    #[test]
    fn dormant_catalog_order_is_stable() {
        let first: Vec<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        let second: Vec<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        assert_eq!(first, second);
    }
}
