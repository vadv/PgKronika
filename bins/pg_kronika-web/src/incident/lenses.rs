//! Dormant diagnostic catalog metadata.

const MAX_DORMANT_LENSES: usize = 28;
const MAX_MISSING_PER_LENS: usize = 6;
const MAX_CATALOG_TOKEN_BYTES: usize = 40;

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum MissingCapability {
    CounterDeltas,
    GaugeSamples,
    PairedIntervals,
    SourcePeriod,
    InputCoverage,
    EntityJoin,
    TrackPlanningGate,
    StorePlansBridge,
    BlockedByEdges,
    LockSnapshotCoverage,
    ActivityRows,
    PidCgroupMapping,
    LogEvents,
}

impl MissingCapability {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CounterDeltas => "typed_counter_deltas",
            Self::GaugeSamples => "typed_gauge_samples",
            Self::PairedIntervals => "paired_interval_inputs",
            Self::SourcePeriod => "source_period_provenance",
            Self::InputCoverage => "request_input_coverage",
            Self::EntityJoin => "cross_section_entity_join",
            Self::TrackPlanningGate => "track_planning_gate",
            Self::StorePlansBridge => "store_plans_bridge",
            Self::BlockedByEdges => "sampled_blocked_by_edges",
            Self::LockSnapshotCoverage => "lock_snapshot_coverage",
            Self::ActivityRows => "sampled_activity_rows",
            Self::PidCgroupMapping => "pid_cgroup_mapping",
            Self::LogEvents => "typed_log_events",
        }
    }
}

/// A design-catalog entry with known missing capabilities.
pub(crate) struct DormantLens {
    lens_id: &'static str,
    missing: &'static [MissingCapability],
}

impl DormantLens {
    pub(crate) const fn lens_id(&self) -> &'static str {
        self.lens_id
    }

    pub(crate) const fn missing(&self) -> &'static [MissingCapability] {
        self.missing
    }
}

use MissingCapability as Missing;

const DORMANT_CATALOG: &[DormantLens] = &[
    DormantLens {
        lens_id: "PG-QRY-001",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-PLAN-002",
        missing: &[
            Missing::CounterDeltas,
            Missing::GaugeSamples,
            Missing::StorePlansBridge,
            Missing::TrackPlanningGate,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-TEMP-003",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::LogEvents,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-ANALYZE-004",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-VACUUM-005",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::LogEvents,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-FREEZE-006",
        missing: &[
            Missing::GaugeSamples,
            Missing::LogEvents,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-HOT-007",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-CHKPT-008",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::LogEvents,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-WAL-009",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-CACHE-010",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-IO-011",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-LOCK-012",
        missing: &[Missing::BlockedByEdges, Missing::LockSnapshotCoverage],
    },
    DormantLens {
        lens_id: "PG-HORIZON-013",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::ActivityRows,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-CONN-014",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::ActivityRows,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-REPL-015",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-SLOT-016",
        missing: &[
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-ARCH-017",
        missing: &[
            Missing::CounterDeltas,
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-SYNC-018",
        missing: &[
            Missing::ActivityRows,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "PG-WAIT-019",
        missing: &[
            Missing::ActivityRows,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-CPU-020",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-CGRP-021",
        missing: &[
            Missing::PidCgroupMapping,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-MEM-022",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-CGMEM-023",
        missing: &[
            Missing::PidCgroupMapping,
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-BLOCK-024",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-WB-025",
        missing: &[
            Missing::GaugeSamples,
            Missing::CounterDeltas,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-IOWHO-026",
        missing: &[
            Missing::CounterDeltas,
            Missing::PidCgroupMapping,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-FS-027",
        missing: &[
            Missing::GaugeSamples,
            Missing::EntityJoin,
            Missing::LogEvents,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
    DormantLens {
        lens_id: "OS-NET-028",
        missing: &[
            Missing::CounterDeltas,
            Missing::PairedIntervals,
            Missing::EntityJoin,
            Missing::SourcePeriod,
            Missing::InputCoverage,
        ],
    },
];

/// Returns design entries with non-exhaustive readiness requirements.
pub(crate) const fn dormant_catalog() -> &'static [DormantLens] {
    DORMANT_CATALOG
}

const fn text_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut at = 0;
    while at < left.len() {
        if left[at] != right[at] {
            return false;
        }
        at += 1;
    }
    true
}

const fn catalog_is_valid(catalog: &[DormantLens]) -> bool {
    if catalog.is_empty() || catalog.len() > MAX_DORMANT_LENSES {
        return false;
    }
    let mut lens_at = 0;
    while lens_at < catalog.len() {
        let lens = &catalog[lens_at];
        if lens.lens_id.is_empty()
            || lens.lens_id.len() > MAX_CATALOG_TOKEN_BYTES
            || lens.missing.is_empty()
            || lens.missing.len() > MAX_MISSING_PER_LENS
        {
            return false;
        }
        let mut previous_lens = 0;
        while previous_lens < lens_at {
            if text_eq(catalog[previous_lens].lens_id, lens.lens_id) {
                return false;
            }
            previous_lens += 1;
        }
        let mut capability_at = 0;
        while capability_at < lens.missing.len() {
            let capability = lens.missing[capability_at];
            if capability.as_str().len() > MAX_CATALOG_TOKEN_BYTES {
                return false;
            }
            let mut previous_capability = 0;
            while previous_capability < capability_at {
                if lens.missing[previous_capability] as u8 == capability as u8 {
                    return false;
                }
                previous_capability += 1;
            }
            capability_at += 1;
        }
        lens_at += 1;
    }
    true
}

const _: () = assert!(catalog_is_valid(DORMANT_CATALOG));

#[cfg(test)]
mod tests {
    use super::*;

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
    fn catalog_ids_match_the_contract_order() {
        let ids: Vec<_> = dormant_catalog().iter().map(DormantLens::lens_id).collect();
        assert_eq!(ids, EXPECTED_LENSES);
    }

    #[test]
    fn duplicate_ids_are_invalid() {
        let duplicate = [
            DormantLens {
                lens_id: "PG-LOCK-012",
                missing: &[Missing::BlockedByEdges],
            },
            DormantLens {
                lens_id: "PG-LOCK-012",
                missing: &[Missing::LockSnapshotCoverage],
            },
        ];
        assert!(!catalog_is_valid(&duplicate));
    }

    #[test]
    fn duplicate_capabilities_are_invalid() {
        let duplicate = [DormantLens {
            lens_id: "PG-LOCK-012",
            missing: &[Missing::BlockedByEdges, Missing::BlockedByEdges],
        }];
        assert!(!catalog_is_valid(&duplicate));
    }

    #[test]
    fn catalog_growth_requires_a_new_bound() {
        let oversized = std::array::from_fn::<_, { MAX_DORMANT_LENSES + 1 }, _>(|_| DormantLens {
            lens_id: "x",
            missing: &[Missing::InputCoverage],
        });
        assert!(!catalog_is_valid(&oversized));
    }

    #[test]
    fn lock_requirements_preserve_pr75() {
        let lock = dormant_catalog()
            .iter()
            .find(|lens| lens.lens_id() == "PG-LOCK-012")
            .expect("lock catalog entry");
        let requirements: Vec<_> = lock
            .missing()
            .iter()
            .map(|capability| capability.as_str())
            .collect();
        assert_eq!(
            requirements,
            ["sampled_blocked_by_edges", "lock_snapshot_coverage"]
        );
    }
}
