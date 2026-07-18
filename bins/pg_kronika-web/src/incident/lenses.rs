//! Readiness metadata for diagnostic lenses.
//!
//! The engine receives anomaly episodes and scalar series, not the typed deltas,
//! gauges, sampled edges, and cross-section joins the lens formulas need, so
//! every catalog lens is dormant and names the typed inputs it still awaits.

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

// Prerequisite capabilities named as the typed inputs a lens still needs. A
// capability is engine-facing, not a raw column: one may cover several columns
// of the same kind. `period_clock_provenance` gates every directional role;
// only the structural lock edge escapes it.
const COUNTER_DELTAS: &str = "typed_counter_deltas";
const GAUGE_VALUES: &str = "typed_gauge_values";
const PAIRED_RATIOS: &str = "paired_interval_ratios";
const CLOCK_PROVENANCE: &str = "period_clock_provenance";
const ENTITY_JOIN: &str = "cross_section_entity_join";
const TRACK_PLANNING: &str = "track_planning_gate";
const STORE_PLANS_BRIDGE: &str = "store_plans_bridge";
const BLOCKED_BY_EDGES: &str = "sampled_blocked_by_edges";
const LOCK_COVERAGE: &str = "lock_snapshot_coverage";
const WAIT_EVENTS: &str = "sampled_wait_events";
const CGROUP_MAPPING: &str = "pid_cgroup_mapping";
const LOG_EVENTS: &str = "typed_log_events";

/// The full lens catalog, dormant until each entry's typed inputs are wired.
/// Order and ids follow the lens contract (`PG-QRY-001` … `OS-NET-028`).
const DORMANT_CATALOG: &[DormantLens] = &[
    DormantLens {
        lens_id: "PG-QRY-001",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-PLAN-002",
        awaiting: &[STORE_PLANS_BRIDGE, TRACK_PLANNING, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-TEMP-003",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, LOG_EVENTS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-ANALYZE-004",
        awaiting: &[GAUGE_VALUES, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-VACUUM-005",
        awaiting: &[GAUGE_VALUES, COUNTER_DELTAS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-FREEZE-006",
        awaiting: &[GAUGE_VALUES, LOG_EVENTS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-HOT-007",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-CHKPT-008",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, LOG_EVENTS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-WAL-009",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-CACHE-010",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-IO-011",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-LOCK-012",
        awaiting: &[BLOCKED_BY_EDGES, LOCK_COVERAGE],
    },
    DormantLens {
        lens_id: "PG-HORIZON-013",
        awaiting: &[GAUGE_VALUES, COUNTER_DELTAS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-CONN-014",
        awaiting: &[GAUGE_VALUES, COUNTER_DELTAS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-REPL-015",
        awaiting: &[GAUGE_VALUES, COUNTER_DELTAS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-SLOT-016",
        awaiting: &[GAUGE_VALUES, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-ARCH-017",
        awaiting: &[COUNTER_DELTAS, GAUGE_VALUES, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-SYNC-018",
        awaiting: &[WAIT_EVENTS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "PG-WAIT-019",
        awaiting: &[WAIT_EVENTS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-CPU-020",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-CGRP-021",
        awaiting: &[CGROUP_MAPPING, COUNTER_DELTAS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-MEM-022",
        awaiting: &[GAUGE_VALUES, COUNTER_DELTAS, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-CGMEM-023",
        awaiting: &[
            CGROUP_MAPPING,
            GAUGE_VALUES,
            COUNTER_DELTAS,
            CLOCK_PROVENANCE,
        ],
    },
    DormantLens {
        lens_id: "OS-BLOCK-024",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-WB-025",
        awaiting: &[GAUGE_VALUES, COUNTER_DELTAS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-IOWHO-026",
        awaiting: &[
            COUNTER_DELTAS,
            CGROUP_MAPPING,
            ENTITY_JOIN,
            CLOCK_PROVENANCE,
        ],
    },
    DormantLens {
        lens_id: "OS-FS-027",
        awaiting: &[GAUGE_VALUES, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
    DormantLens {
        lens_id: "OS-NET-028",
        awaiting: &[COUNTER_DELTAS, PAIRED_RATIOS, ENTITY_JOIN, CLOCK_PROVENANCE],
    },
];

/// Returns the catalog entries whose required typed inputs are not available.
pub(crate) const fn dormant_catalog() -> &'static [DormantLens] {
    DORMANT_CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const EXPECTED_LENSES: [&str; 28] = [
        "PG-QRY-001",
        "PG-PLAN-002",
        "PG-TEMP-003",
        "PG-ANALYZE-004",
        "PG-VACUUM-005",
        "PG-FREEZE-006",
        "PG-HOT-007",
        "PG-CHKPT-008",
        "PG-WAL-009",
        "PG-CACHE-010",
        "PG-IO-011",
        "PG-LOCK-012",
        "PG-HORIZON-013",
        "PG-CONN-014",
        "PG-REPL-015",
        "PG-SLOT-016",
        "PG-ARCH-017",
        "PG-SYNC-018",
        "PG-WAIT-019",
        "OS-CPU-020",
        "OS-CGRP-021",
        "OS-MEM-022",
        "OS-CGMEM-023",
        "OS-BLOCK-024",
        "OS-WB-025",
        "OS-IOWHO-026",
        "OS-FS-027",
        "OS-NET-028",
    ];

    #[test]
    fn the_catalog_declares_every_documented_lens_in_contract_order() {
        let ids: Vec<&str> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        assert_eq!(ids, EXPECTED_LENSES);
    }

    #[test]
    fn lens_ids_are_unique() {
        let ids: BTreeSet<&str> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        assert_eq!(ids.len(), dormant_catalog().len());
    }

    #[test]
    fn every_dormant_lens_awaits_at_least_one_input() {
        for lens in dormant_catalog() {
            assert!(
                !lens.awaiting().is_empty(),
                "{} declares no prerequisite",
                lens.lens_id()
            );
        }
    }

    #[test]
    fn the_lock_lens_waits_only_for_edge_and_snapshot_quality() {
        let lock = dormant_catalog()
            .iter()
            .find(|lens| lens.lens_id() == "PG-LOCK-012")
            .expect("lock lens is catalogued");
        assert_eq!(
            lock.awaiting(),
            ["sampled_blocked_by_edges", "lock_snapshot_coverage"],
            "the structural lock lens does not depend on clock provenance"
        );
    }

    #[test]
    fn dormant_catalog_order_is_stable() {
        let first: Vec<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        let second: Vec<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        assert_eq!(first, second);
    }
}
