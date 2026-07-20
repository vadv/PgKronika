//! Incident lenses backed by dedicated bounded gauge contracts.
#![allow(
    clippy::float_cmp,
    reason = "state codes and booleans are exactly representable integer values widened to f64"
)]

use std::sync::Arc;

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::sink::FindingSink;
use super::evidence::{
    ConfidenceCap, Evidence, FindingDraft, FindingScope, GaugeEntity, GaugeEvidence, GaugeRatio,
    GaugeTrendInput, GaugeUnit, GaugeValueInput, Role, ThresholdKind,
};
use super::lens::Lens;
use super::series::SeriesSet;
use super::typed::{GaugeObjective, TypedInputs};

const FREEZE: &str = "pg_freeze_horizon";
const VACUUM: &str = "pg_vacuum_observation";
const REPLICATION: &str = "pg_replication_physical";
const SLOT: &str = "pg_replication_slot_retention";
const STORAGE: &str = "pg_storage_mount";
const CGROUP: &str = "pg_process_cgroup_memory";

fn entity(section: &'static str, identity: &[super::model::IdentityValue]) -> GaugeEntity {
    GaugeEntity::new(section, Arc::from(identity))
}

fn emit(
    sink: &mut FindingSink<'_>,
    member: &super::model::EpisodeRefV1,
    evidence: Vec<Evidence>,
) -> Result<(), LimitHit> {
    sink.emit(FindingDraft::new(
        Role::Coincident,
        FindingScope::from_episode(member),
        evidence,
    ))
}

pub(crate) struct FreezeHorizonLens;

impl FreezeHorizonLens {
    const ID: &'static str = "PG-FREEZE-006";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: FREEZE,
            column: "xid_age",
        },
        SectionColumn {
            section: FREEZE,
            column: "xid_limit",
        },
        SectionColumn {
            section: FREEZE,
            column: "mxid_age",
        },
        SectionColumn {
            section: FREEZE,
            column: "mxid_limit",
        },
    ];
    const LIMIT_FRACTION: f64 = 0.9;
}

impl Lens for FreezeHorizonLens {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }
    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            let columns = match (member.logical_section, member.column) {
                (FREEZE, "xid_age") => ("xid_age", "xid_limit"),
                (FREEZE, "mxid_age") => ("mxid_age", "mxid_limit"),
                _ => continue,
            };
            let Some(window) = typed.paired_gauge_window(
                FREEZE,
                &member.identity,
                columns,
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.reduce(GaugeObjective::RatioMax) else {
                continue;
            };
            if reading.a < 0.0 || reading.b <= 0.0 || reading.a / reading.b < Self::LIMIT_FRACTION {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(columns.0, reading.a, columns.1, reading.b, GaugeUnit::Count),
                Self::LIMIT_FRACTION,
                ThresholdKind::AtLeast,
                reading.observed_at_us,
                reading.samples,
                entity(FREEZE, &member.identity),
            ) else {
                continue;
            };
            emit(sink, member, vec![Evidence::GaugeObservation(evidence)])?;
        }
        Ok(())
    }
}

pub(crate) struct RunningVacuumLens;

impl RunningVacuumLens {
    const ID: &'static str = "PG-VACUUM-005";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: VACUUM,
            column: "elapsed_us",
        },
        SectionColumn {
            section: VACUUM,
            column: "activity_present",
        },
        SectionColumn {
            section: VACUUM,
            column: "clock_valid",
        },
    ];
    const ELAPSED_FLOOR_US: f64 = 300_000_000.0;
}

impl Lens for RunningVacuumLens {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }
    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != VACUUM || member.column != "elapsed_us" {
                continue;
            }
            let Some(window) = typed.gauge_snapshot_window(
                VACUUM,
                &member.identity,
                &["elapsed_us", "activity_present", "clock_valid"],
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.extreme(0, true) else {
                continue;
            };
            if reading.values[1] != 1.0
                || reading.values[2] != 1.0
                || reading.values[0] < Self::ELAPSED_FLOOR_US
            {
                continue;
            }
            let Some(evidence) = GaugeEvidence::value(GaugeValueInput {
                operand: "elapsed_us",
                value: reading.values[0],
                unit: GaugeUnit::Microseconds,
                threshold: Self::ELAPSED_FLOOR_US,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: reading.observed_at_us,
                samples: reading.samples,
                entity: entity(VACUUM, &member.identity),
            }) else {
                continue;
            };
            emit(sink, member, vec![Evidence::GaugeObservation(evidence)])?;
        }
        Ok(())
    }
}

pub(crate) struct PhysicalReplicationLens;

impl PhysicalReplicationLens {
    const ID: &'static str = "PG-REPL-015";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: REPLICATION,
            column: "current_to_sent_bytes",
        },
        SectionColumn {
            section: REPLICATION,
            column: "sent_to_write_bytes",
        },
        SectionColumn {
            section: REPLICATION,
            column: "write_to_flush_bytes",
        },
        SectionColumn {
            section: REPLICATION,
            column: "flush_to_replay_bytes",
        },
        SectionColumn {
            section: REPLICATION,
            column: "write_lag_us",
        },
        SectionColumn {
            section: REPLICATION,
            column: "flush_lag_us",
        },
        SectionColumn {
            section: REPLICATION,
            column: "replay_lag_us",
        },
        SectionColumn {
            section: REPLICATION,
            column: "scope_code",
        },
        SectionColumn {
            section: REPLICATION,
            column: "state_code",
        },
    ];
    const BYTE_FLOOR: f64 = 64.0 * 1024.0 * 1024.0;
    const TIME_FLOOR_US: f64 = 30_000_000.0;
}

impl Lens for PhysicalReplicationLens {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }
    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != REPLICATION {
                continue;
            }
            let (unit, threshold) = if member.column.ends_with("_bytes") {
                (GaugeUnit::Bytes, Self::BYTE_FLOOR)
            } else if member.column.ends_with("_lag_us") {
                (GaugeUnit::Microseconds, Self::TIME_FLOOR_US)
            } else {
                continue;
            };
            let Some(window) = typed.gauge_snapshot_window(
                REPLICATION,
                &member.identity,
                &[member.column, "scope_code", "state_code"],
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.extreme(0, true) else {
                continue;
            };
            if reading.values[0] < threshold
                || reading.values[1] != 1.0
                || !(1.0..=5.0).contains(&reading.values[2])
            {
                continue;
            }
            let Some(evidence) = GaugeEvidence::value(GaugeValueInput {
                operand: member.column,
                value: reading.values[0],
                unit,
                threshold,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: reading.observed_at_us,
                samples: reading.samples,
                entity: entity(REPLICATION, &member.identity),
            }) else {
                continue;
            };
            emit(sink, member, vec![Evidence::GaugeObservation(evidence)])?;
        }
        Ok(())
    }
}

pub(crate) struct SlotRetentionLens;

impl SlotRetentionLens {
    const ID: &'static str = "PG-SLOT-016";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: SLOT,
            column: "retained_bytes",
        },
        SectionColumn {
            section: SLOT,
            column: "safe_wal_size",
        },
        SectionColumn {
            section: SLOT,
            column: "max_slot_wal_keep_size_bytes",
        },
        SectionColumn {
            section: SLOT,
            column: "wal_status_code",
        },
    ];
    const HEADROOM_FLOOR: f64 = 0.8;
}

impl Lens for SlotRetentionLens {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }
    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != SLOT || member.column != "retained_bytes" {
                continue;
            }
            let Some(retained) = typed.gauge_window(
                SLOT,
                "retained_bytes",
                &member.identity,
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            let retained_points = retained.inspected_points();
            let trend = retained.trend();
            let mut amount = None;
            for headroom_column in ["safe_wal_size", "max_slot_wal_keep_size_bytes"] {
                let Some(window) = typed.gauge_snapshot_window(
                    SLOT,
                    &member.identity,
                    &["retained_bytes", headroom_column, "wal_status_code"],
                    context.incident_start_us,
                    context.incident_end_us,
                ) else {
                    continue;
                };
                let inspected = window.inspected_points();
                sink.charge_points(inspected.saturating_mul(3))?;
                let Some(status) = window.value_range(2) else {
                    continue;
                };
                let Some(reading) = window.extreme(0, true) else {
                    continue;
                };
                if status.0 >= 1.0
                    && status.1 <= 2.0
                    && status.2 == retained_points
                    && reading.values[1] > 0.0
                {
                    amount = Some((reading, headroom_column == "safe_wal_size"));
                    break;
                }
            }
            let Some((reading, is_safe_headroom)) = amount else {
                continue;
            };
            let denominator = if is_safe_headroom {
                reading.values[0] + reading.values[1]
            } else {
                reading.values[1]
            };
            let denominator_name = if is_safe_headroom {
                "retained_bytes_plus_safe_wal_size"
            } else {
                "max_slot_wal_keep_size_bytes"
            };
            if denominator <= 0.0 || reading.values[0] / denominator < Self::HEADROOM_FLOOR {
                continue;
            }
            let Some(amount_evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "retained_bytes",
                    reading.values[0],
                    denominator_name,
                    denominator,
                    GaugeUnit::Bytes,
                ),
                Self::HEADROOM_FLOOR,
                ThresholdKind::AtLeast,
                reading.observed_at_us,
                reading.samples,
                entity(SLOT, &member.identity),
            ) else {
                continue;
            };
            let mut evidence = vec![Evidence::GaugeObservation(amount_evidence)];
            if let Some(trend) = trend
                && trend.last > trend.first
                && let Some(trend_evidence) = GaugeEvidence::trend(GaugeTrendInput {
                    operand: "retained_bytes",
                    first: trend.first,
                    last: trend.last,
                    operand_unit: GaugeUnit::Bytes,
                    threshold_per_second: 0.0,
                    threshold_kind: ThresholdKind::AtLeast,
                    first_at_us: trend.first_at_us,
                    last_at_us: trend.last_at_us,
                    samples: trend.samples,
                    entity: entity(SLOT, &member.identity),
                })
            {
                evidence.push(Evidence::GaugeObservation(trend_evidence));
            }
            emit(sink, member, evidence)?;
        }
        Ok(())
    }
}

pub(crate) struct StorageCapacityLens;

impl StorageCapacityLens {
    const ID: &'static str = "OS-FS-027";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: STORAGE,
            column: "available_bytes",
        },
        SectionColumn {
            section: STORAGE,
            column: "total_bytes",
        },
        SectionColumn {
            section: STORAGE,
            column: "mapping_state",
        },
    ];
    const AVAILABLE_FLOOR: f64 = 0.1;
}

impl Lens for StorageCapacityLens {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }
    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }
    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != STORAGE || member.column != "available_bytes" {
                continue;
            }
            let Some(window) = typed.gauge_snapshot_window(
                STORAGE,
                &member.identity,
                &["available_bytes", "total_bytes", "mapping_state"],
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.extreme(0, false) else {
                continue;
            };
            if reading.values[2] != 1.0
                || reading.values[1] <= 0.0
                || reading.values[0] / reading.values[1] >= Self::AVAILABLE_FLOOR
            {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "available_bytes",
                    reading.values[0],
                    "total_bytes",
                    reading.values[1],
                    GaugeUnit::Bytes,
                ),
                Self::AVAILABLE_FLOOR,
                ThresholdKind::Below,
                reading.observed_at_us,
                reading.samples,
                entity(STORAGE, &member.identity),
            ) else {
                continue;
            };
            emit(sink, member, vec![Evidence::GaugeObservation(evidence)])?;
        }
        Ok(())
    }
}

pub(crate) struct CgroupMemoryLens;

impl CgroupMemoryLens {
    const ID: &'static str = "OS-CGMEM-023";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: CGROUP,
            column: "current_bytes",
        },
        SectionColumn {
            section: CGROUP,
            column: "max_bytes",
        },
        SectionColumn {
            section: CGROUP,
            column: "mapping_state",
        },
        SectionColumn {
            section: CGROUP,
            column: "max_unlimited",
        },
    ];
    const UTILIZATION_FLOOR: f64 = 0.9;
}

impl Lens for CgroupMemoryLens {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }
    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
    }
    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in &cluster.members {
            if member.logical_section != CGROUP || member.column != "current_bytes" {
                continue;
            }
            let Some(window) = typed.gauge_snapshot_window(
                CGROUP,
                &member.identity,
                &[
                    "current_bytes",
                    "max_bytes",
                    "mapping_state",
                    "max_unlimited",
                ],
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.extreme(0, true) else {
                continue;
            };
            if reading.values[2] != 1.0
                || reading.values[3] != 0.0
                || reading.values[1] <= 0.0
                || reading.values[0] / reading.values[1] < Self::UTILIZATION_FLOOR
            {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "current_bytes",
                    reading.values[0],
                    "max_bytes",
                    reading.values[1],
                    GaugeUnit::Bytes,
                ),
                Self::UTILIZATION_FLOOR,
                ThresholdKind::AtLeast,
                reading.observed_at_us,
                reading.samples,
                entity(CGROUP, &member.identity),
            ) else {
                continue;
            };
            emit(sink, member, vec![Evidence::GaugeObservation(evidence)])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::model::{EnrichedEpisode, EpisodeRefV1, IdentityValue};
    use crate::incident::{ClockRelation, IncidentConfig, analyze};
    use kronika_analytics::{Direction, Episode, Evaluated};

    fn identity() -> Arc<[IdentityValue]> {
        Arc::from(vec![IdentityValue::U64(7)])
    }

    fn typed(section: &'static str, columns: &[(&'static str, &[f64])]) -> TypedInputs {
        let mut typed = TypedInputs::new();
        for &(column, values) in columns {
            typed.insert_gauge(
                section,
                column,
                Arc::clone(&identity()),
                values
                    .iter()
                    .zip(1_i64..)
                    .map(|(&value, ts)| (ts, value))
                    .collect(),
            );
        }
        typed
    }

    fn run(
        lens: &dyn Lens,
        section: &'static str,
        column: &'static str,
        typed: &TypedInputs,
    ) -> Vec<usize> {
        let episode = EnrichedEpisode {
            episode: Episode {
                start: 0,
                end: 0,
                peak_ts: 0,
                peak: Evaluated {
                    m: 0.0,
                    dir: Direction::Up,
                    med_cur: 0.0,
                    med_ref: 0.0,
                    mad_ref: 1.0,
                    sigma_used: 1.0,
                    n_cur: 1,
                    n_ref: 1,
                },
            },
            reference: EpisodeRefV1 {
                logical_section: section,
                column,
                identity: identity(),
                start_us: 0,
                end_us: 10,
            },
        };
        let lenses: [&dyn Lens; 1] = [lens];
        analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            typed,
            &lenses,
            &IncidentConfig::for_test("node", 5, 10_000, ClockRelation::Unknown),
        )
        .expect("bounded evaluation")
        .incidents[0]
            .findings
            .iter()
            .map(|finding| finding.evidence().len())
            .collect()
    }

    #[test]
    fn freeze_uses_separate_xid_and_mxid_denominators() {
        let input = typed(
            FREEZE,
            &[
                ("xid_age", &[90.0]),
                ("xid_limit", &[100.0]),
                ("mxid_age", &[40.0]),
                ("mxid_limit", &[50.0]),
            ],
        );
        assert_eq!(run(&FreezeHorizonLens, FREEZE, "xid_age", &input), vec![1]);
        assert!(run(&FreezeHorizonLens, FREEZE, "mxid_age", &input).is_empty());
    }

    #[test]
    fn vacuum_requires_same_timestamp_activity_and_valid_clock() {
        let valid = typed(
            VACUUM,
            &[
                ("elapsed_us", &[300_000_000.0]),
                ("activity_present", &[1.0]),
                ("clock_valid", &[1.0]),
            ],
        );
        assert_eq!(
            run(&RunningVacuumLens, VACUUM, "elapsed_us", &valid),
            vec![1]
        );
        let missing = typed(
            VACUUM,
            &[
                ("elapsed_us", &[300_000_000.0]),
                ("activity_present", &[0.0]),
                ("clock_valid", &[1.0]),
            ],
        );
        assert!(run(&RunningVacuumLens, VACUUM, "elapsed_us", &missing).is_empty());
    }

    #[test]
    fn replication_excludes_logical_scope_and_keeps_stage_gap() {
        let physical = typed(
            REPLICATION,
            &[
                ("flush_to_replay_bytes", &[67_108_864.0]),
                ("scope_code", &[1.0]),
                ("state_code", &[3.0]),
            ],
        );
        assert_eq!(
            run(
                &PhysicalReplicationLens,
                REPLICATION,
                "flush_to_replay_bytes",
                &physical
            ),
            vec![1]
        );
        let logical = typed(
            REPLICATION,
            &[
                ("flush_to_replay_bytes", &[67_108_864.0]),
                ("scope_code", &[2.0]),
                ("state_code", &[3.0]),
            ],
        );
        assert!(
            run(
                &PhysicalReplicationLens,
                REPLICATION,
                "flush_to_replay_bytes",
                &logical
            )
            .is_empty()
        );
    }

    #[test]
    fn slot_trend_requires_two_ordered_valid_samples() {
        let input = typed(
            SLOT,
            &[
                ("retained_bytes", &[70.0, 90.0]),
                ("safe_wal_size", &[30.0, 10.0]),
                ("wal_status_code", &[1.0, 1.0]),
            ],
        );
        assert_eq!(
            run(&SlotRetentionLens, SLOT, "retained_bytes", &input),
            vec![2]
        );
        let invalid = typed(
            SLOT,
            &[
                ("retained_bytes", &[70.0, 90.0]),
                ("safe_wal_size", &[30.0, 10.0]),
                ("wal_status_code", &[1.0, 4.0]),
            ],
        );
        assert!(run(&SlotRetentionLens, SLOT, "retained_bytes", &invalid).is_empty());
    }

    #[test]
    fn storage_requires_a_proven_mount_and_positive_capacity() {
        let input = typed(
            STORAGE,
            &[
                ("available_bytes", &[9.0]),
                ("total_bytes", &[100.0]),
                ("mapping_state", &[1.0]),
            ],
        );
        assert_eq!(
            run(&StorageCapacityLens, STORAGE, "available_bytes", &input),
            vec![1]
        );
        let mismatch = typed(
            STORAGE,
            &[
                ("available_bytes", &[1.0]),
                ("total_bytes", &[100.0]),
                ("mapping_state", &[8.0]),
            ],
        );
        assert!(run(&StorageCapacityLens, STORAGE, "available_bytes", &mismatch).is_empty());
    }

    #[test]
    fn cgroup_memory_excludes_unlimited_and_unverified_rows() {
        let input = typed(
            CGROUP,
            &[
                ("current_bytes", &[90.0]),
                ("max_bytes", &[100.0]),
                ("mapping_state", &[1.0]),
                ("max_unlimited", &[0.0]),
            ],
        );
        assert_eq!(
            run(&CgroupMemoryLens, CGROUP, "current_bytes", &input),
            vec![1]
        );
        let unlimited = typed(
            CGROUP,
            &[
                ("current_bytes", &[90.0]),
                ("max_bytes", &[100.0]),
                ("mapping_state", &[1.0]),
                ("max_unlimited", &[1.0]),
            ],
        );
        assert!(run(&CgroupMemoryLens, CGROUP, "current_bytes", &unlimited).is_empty());
    }
}
