//! Active diagnostic lenses over typed counter and gauge evidence.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::sink::FindingSink;
use super::evidence::{
    ConfidenceCap, DirectEvidence, Evidence, FindingDraft, FindingScope, GaugeEntity,
    GaugeEvidence, GaugeRatio, GaugeUnit, Role, ThresholdKind,
};
use super::gauge_contracts::{
    CgroupMemoryLens, FreezeHorizonLens, PhysicalReplicationLens, RunningVacuumLens,
    SlotRetentionLens, StorageCapacityLens,
};
use super::lens::Lens;
use super::model::IdentityValue;
use super::os_lenses::{BlockDeviceLens, HostCpuLens, ProcessIoWhoLens};
use super::query_plan::{PlanChurnLens, QueryWorkLens};
use super::series::SeriesSet;
use super::typed::{ActivityBackend, GaugeObjective, TypedInputs};
#[cfg(test)]
use super::typed::{ActivitySnapshot, SnapshotCompleteness};

const PG_STAT_DATABASE: &str = "pg_stat_database";
const PG_STAT_WAL: &str = "pg_stat_wal";
const CHECKPOINTER: &str = "pg_stat_bgwriter + pg_stat_checkpointer";
const PG_STAT_IO: &str = "pg_stat_io";
const PG_STAT_USER_TABLES: &str = "pg_stat_user_tables";
const PG_STAT_ARCHIVER: &str = "pg_stat_archiver";
const PG_STAT_ACTIVITY: &str = "pg_stat_activity";
const PG_LOCKS: &str = "pg_locks";
const OS_NETDEV: &str = "os_netdev";
const OS_CGROUP_CPU: &str = "os_cgroup_cpu";
const OS_MEMINFO: &str = "os_meminfo";

/// `PG-CACHE-010` (`shared_buffer_misses`): shared-buffer miss pressure. Reports
/// an elevated `sum(d(blks_read)) / sum(d(blks_read) + d(blks_hit))` over the
/// incident as an amplifier. Direction needs clock provenance, so the lens never
/// leads; without direct evidence its confidence stays capped at medium.
pub(crate) struct SharedBufferMissesLens;

impl SharedBufferMissesLens {
    const ID: &'static str = "PG-CACHE-010";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "blks_read",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "blks_hit",
        },
    ];
    /// A regular signal needs at least three valid pairs (data-quality policy).
    const MIN_INTERVALS: usize = 3;
    /// Only an unusually cold cache is worth an incident finding.
    const MISS_THRESHOLD: f64 = 0.2;
}

impl Lens for SharedBufferMissesLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_DATABASE || member.column != "blks_read" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_DATABASE,
                &member.identity,
                "blks_read",
                "blks_hit",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            let total = sums.sum_a + sums.sum_b;
            if total <= 0.0 || sums.sum_a / total < Self::MISS_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-WAL-009` (`wal_amplification`): WAL write amplification. Reports an
/// elevated `sum(d(wal_fpi)) / sum(d(wal_records))` (full-page images per WAL
/// record) as an amplifier. Direction needs clock provenance, so the lens never
/// leads; without direct evidence its confidence stays capped at medium.
pub(crate) struct WalAmplificationLens;

impl WalAmplificationLens {
    const ID: &'static str = "PG-WAL-009";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_WAL,
            column: "wal_fpi",
        },
        SectionColumn {
            section: PG_STAT_WAL,
            column: "wal_records",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// Half the WAL records carrying a full-page image is heavy amplification.
    const FPI_THRESHOLD: f64 = 0.5;
}

impl Lens for WalAmplificationLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_WAL || member.column != "wal_fpi" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_WAL,
                &member.identity,
                "wal_fpi",
                "wal_records",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            if sums.sum_b <= 0.0 || sums.sum_a / sums.sum_b < Self::FPI_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-TEMP-003` (`temp_spill`): spill into temporary files. Reports an amplifier
/// when both `temp_bytes` and `temp_files` advanced over the incident, the honest
/// signature of query work spilling to disk. Counter evidence caps at medium.
pub(crate) struct TempSpillLens;

impl TempSpillLens {
    const ID: &'static str = "PG-TEMP-003";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "temp_bytes",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "temp_files",
        },
    ];
    const MIN_INTERVALS: usize = 3;
}

impl Lens for TempSpillLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_DATABASE || member.column != "temp_bytes" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_DATABASE,
                &member.identity,
                "temp_bytes",
                "temp_files",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            if sums.sum_a <= 0.0 || sums.sum_b <= 0.0 {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Counter],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-CHKPT-008` (`requested_checkpoints`): checkpoints forced by demand rather
/// than by the timer. Reports an elevated
/// `sum(d(checkpoints_req)) / sum(d(checkpoints_req + checkpoints_timed))` as an
/// amplifier. Ratio evidence caps at medium.
pub(crate) struct RequestedCheckpointsLens;

impl RequestedCheckpointsLens {
    const ID: &'static str = "PG-CHKPT-008";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: CHECKPOINTER,
            column: "checkpoints_req",
        },
        SectionColumn {
            section: CHECKPOINTER,
            column: "checkpoints_timed",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// More requested than timed checkpoints inverts the healthy ratio.
    const REQUESTED_THRESHOLD: f64 = 0.5;
}

impl Lens for RequestedCheckpointsLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != CHECKPOINTER || member.column != "checkpoints_req" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                CHECKPOINTER,
                &member.identity,
                "checkpoints_req",
                "checkpoints_timed",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            let total = sums.sum_a + sums.sum_b;
            if total <= 0.0 || sums.sum_a / total < Self::REQUESTED_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-IO-011` (`backend_io_latency`): slow reads inside `PostgreSQL`. Reports an
/// elevated `sum(d(read_time)) / sum(d(reads))` (milliseconds per read) as an
/// amplifier. `read_time` needs `track_io_timing`; ratio evidence caps at medium.
pub(crate) struct BackendIoLatencyLens;

impl BackendIoLatencyLens {
    const ID: &'static str = "PG-IO-011";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_IO,
            column: "read_time",
        },
        SectionColumn {
            section: PG_STAT_IO,
            column: "reads",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// One millisecond per read is slow for the buffered-read path.
    const LATENCY_MS_THRESHOLD: f64 = 1.0;
}

impl Lens for BackendIoLatencyLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_IO || member.column != "read_time" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_IO,
                &member.identity,
                "read_time",
                "reads",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            if sums.sum_b <= 0.0 || sums.sum_a / sums.sum_b < Self::LATENCY_MS_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-HOT-007` (`hot_update_failure`): updates that miss the HOT path. Reports an
/// elevated non-HOT fraction
/// `sum(d(n_tup_upd - n_tup_hot_upd)) / sum(d(n_tup_upd))` as an amplifier of
/// index and WAL work. Ratio evidence caps at medium.
pub(crate) struct HotUpdateFailureLens;

impl HotUpdateFailureLens {
    const ID: &'static str = "PG-HOT-007";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "n_tup_hot_upd",
        },
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "n_tup_upd",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// A majority of updates missing HOT is an index-write amplifier.
    const NON_HOT_THRESHOLD: f64 = 0.5;
}

impl Lens for HotUpdateFailureLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_USER_TABLES || member.column != "n_tup_upd" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_USER_TABLES,
                &member.identity,
                "n_tup_hot_upd",
                "n_tup_upd",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            // `sum_a` (HOT) never exceeds `sum_b` (all updates), so the fraction
            // stays in `[0, 1]`.
            if sums.sum_b <= 0.0 || (sums.sum_b - sums.sum_a) / sums.sum_b < Self::NON_HOT_THRESHOLD
            {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-ARCH-017` (`wal_archiving_failure`): the archiver rejecting WAL segments.
/// Reports a coincident finding when `failed_count` advanced during the incident,
/// summed over the intervals it shares with the `archived_count` beside it.
/// Counter evidence caps at medium.
pub(crate) struct WalArchivingFailureLens;

impl WalArchivingFailureLens {
    const ID: &'static str = "PG-ARCH-017";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_ARCHIVER,
            column: "failed_count",
        },
        SectionColumn {
            section: PG_STAT_ARCHIVER,
            column: "archived_count",
        },
    ];
    /// One recorded failure between two real snapshots is already actionable.
    const MIN_INTERVALS: usize = 1;
    const MIN_FAILURES: f64 = 1.0;
}

impl Lens for WalArchivingFailureLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_ARCHIVER || member.column != "failed_count" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                PG_STAT_ARCHIVER,
                &member.identity,
                "failed_count",
                "archived_count",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS || sums.sum_a < Self::MIN_FAILURES {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::Counter],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `OS-NET-028` (`network_errors`): a network interface logging receive errors.
/// Reports a coincident finding when the receive error fraction
/// `sum(d(rx_errs)) / sum(d(rx_packets))` is elevated. The design confidence is
/// low, so its findings stay capped there.
pub(crate) struct NetworkErrorsLens;

impl NetworkErrorsLens {
    const ID: &'static str = "OS-NET-028";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_NETDEV,
            column: "rx_errs",
        },
        SectionColumn {
            section: OS_NETDEV,
            column: "rx_packets",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// One erroring packet in a hundred is far above a healthy interface.
    const ERROR_THRESHOLD: f64 = 0.01;
}

impl Lens for NetworkErrorsLens {
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
            if member.logical_section != OS_NETDEV || member.column != "rx_errs" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                OS_NETDEV,
                &member.identity,
                "rx_errs",
                "rx_packets",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            if sums.sum_b <= 0.0 || sums.sum_a / sums.sum_b < Self::ERROR_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `OS-CGRP-021` (`cgroup_cpu_throttling`): a cgroup denied the CPU it asked for.
/// Reports an amplifier when the throttle fraction
/// `sum(d(throttled_usec)) / sum(d(throttled_usec + usage_usec))` is elevated. It
/// measures throttling itself, not whether host CPU was spare (a cross-section
/// question); ratio evidence caps at medium.
pub(crate) struct CgroupCpuThrottlingLens;

impl CgroupCpuThrottlingLens {
    const ID: &'static str = "OS-CGRP-021";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_CGROUP_CPU,
            column: "throttled_usec",
        },
        SectionColumn {
            section: OS_CGROUP_CPU,
            column: "usage_usec",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// A tenth of the demanded CPU time lost to throttling bites latency.
    const THROTTLE_THRESHOLD: f64 = 0.1;
}

impl Lens for CgroupCpuThrottlingLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != OS_CGROUP_CPU || member.column != "throttled_usec" {
                continue;
            }
            let Some(sums) = typed.paired_delta_sums(
                OS_CGROUP_CPU,
                &member.identity,
                "throttled_usec",
                "usage_usec",
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            if sums.intervals < Self::MIN_INTERVALS {
                continue;
            }
            let total = sums.sum_a + sums.sum_b;
            if total <= 0.0 || sums.sum_a / total < Self::THROTTLE_THRESHOLD {
                continue;
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::Ratio],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-ANALYZE-004`: observed modifications relative to the planner row
/// estimate. This does not assert that a plan changed.
pub(crate) struct StaleStatisticsLens;

impl StaleStatisticsLens {
    const ID: &'static str = "PG-ANALYZE-004";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "n_mod_since_analyze",
        },
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "reltuples",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const MODIFIED_FRACTION_FLOOR: f64 = 0.2;
}

impl Lens for StaleStatisticsLens {
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
            if member.logical_section != PG_STAT_USER_TABLES
                || member.column != "n_mod_since_analyze"
            {
                continue;
            }
            let Some(window) = typed.paired_gauge_window(
                PG_STAT_USER_TABLES,
                &member.identity,
                ("n_mod_since_analyze", "reltuples"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(pair) = window.reduce(GaugeObjective::RatioAbsOneMax) else {
                continue;
            };
            if pair.samples < Self::MIN_SAMPLES {
                continue;
            }
            let denominator = pair.b.abs().max(1.0);
            if pair.a < 0.0 || pair.a / denominator < Self::MODIFIED_FRACTION_FLOOR {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(pair.a, denominator, GaugeUnit::Count),
                Self::MODIFIED_FRACTION_FLOOR,
                ThresholdKind::AtLeast,
                pair.observed_at_us,
                pair.samples,
                GaugeEntity::new(PG_STAT_USER_TABLES, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-CONN-014`: observed occupancy of one database's `datconnlimit`.
pub(crate) struct ConnectionSaturationLens;

impl ConnectionSaturationLens {
    const ID: &'static str = "PG-CONN-014";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "numbackends",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "datconnlimit",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const SATURATION_FLOOR: f64 = 0.8;
}

impl Lens for ConnectionSaturationLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
            if member.logical_section != PG_STAT_DATABASE || member.column != "numbackends" {
                continue;
            }
            let Some(window) = typed.paired_gauge_window(
                PG_STAT_DATABASE,
                &member.identity,
                ("numbackends", "datconnlimit"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(pair) = window.reduce(GaugeObjective::RatioMax) else {
                continue;
            };
            if pair.samples < Self::MIN_SAMPLES {
                continue;
            }
            if pair.a < 0.0 || pair.b <= 0.0 || pair.a / pair.b < Self::SATURATION_FLOOR {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(pair.a, pair.b, GaugeUnit::Count),
                Self::SATURATION_FLOOR,
                ThresholdKind::AtLeast,
                pair.observed_at_us,
                pair.samples,
                GaugeEntity::new(PG_STAT_DATABASE, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `OS-MEM-022`: an observed low `MemAvailable / MemTotal` sample.
pub(crate) struct MemoryReclaimLens;

impl MemoryReclaimLens {
    const ID: &'static str = "OS-MEM-022";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_MEMINFO,
            column: "mem_available",
        },
        SectionColumn {
            section: OS_MEMINFO,
            column: "mem_total",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const AVAILABLE_FRACTION_FLOOR: f64 = 0.05;
}

impl Lens for MemoryReclaimLens {
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
            if member.logical_section != OS_MEMINFO || member.column != "mem_available" {
                continue;
            }
            let Some(window) = typed.paired_gauge_window(
                OS_MEMINFO,
                &member.identity,
                ("mem_available", "mem_total"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(pair) = window.reduce(GaugeObjective::RatioMin) else {
                continue;
            };
            if pair.samples < Self::MIN_SAMPLES {
                continue;
            }
            if pair.b <= 0.0 || pair.a / pair.b >= Self::AVAILABLE_FRACTION_FLOOR {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(pair.a, pair.b, GaugeUnit::Kibibytes),
                Self::AVAILABLE_FRACTION_FLOOR,
                ThresholdKind::Below,
                pair.observed_at_us,
                pair.samples,
                GaugeEntity::new(OS_MEMINFO, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `OS-WB-025`: an observed `(Dirty + Writeback) / MemTotal` crossing.
pub(crate) struct WritebackPressureLens;

impl WritebackPressureLens {
    const ID: &'static str = "OS-WB-025";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_MEMINFO,
            column: "dirty",
        },
        SectionColumn {
            section: OS_MEMINFO,
            column: "writeback",
        },
        SectionColumn {
            section: OS_MEMINFO,
            column: "mem_total",
        },
    ];
    const MIN_SAMPLES: usize = 1;
    const DIRTY_FRACTION_FLOOR: f64 = 0.1;
}

impl Lens for WritebackPressureLens {
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
            if member.logical_section != OS_MEMINFO || member.column != "dirty" {
                continue;
            }
            let Some(window) = typed.triple_gauge_window(
                OS_MEMINFO,
                &member.identity,
                ("dirty", "writeback", "mem_total"),
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            sink.charge_points(window.inspected_points())?;
            let Some(reading) = window.sum_ratio_max() else {
                continue;
            };
            if reading.samples < Self::MIN_SAMPLES {
                continue;
            }
            let numerator = reading.a + reading.b;
            if reading.denominator <= 0.0
                || numerator / reading.denominator < Self::DIRTY_FRACTION_FLOOR
            {
                continue;
            }
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(numerator, reading.denominator, GaugeUnit::Kibibytes),
                Self::DIRTY_FRACTION_FLOOR,
                ThresholdKind::AtLeast,
                reading.observed_at_us,
                reading.samples,
                GaugeEntity::new(OS_MEMINFO, Arc::clone(&member.identity)),
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-HORIZON-013` (`xmin_horizon_hold`): an observed old transaction with an
/// assigned xmin capable of holding the vacuum horizon. Prepared transactions,
/// slots, and standby feedback are independent alternatives outside this row
/// observation. Unseen snapshots prevent a global-cause or persistence claim.
pub(crate) struct XminHorizonHoldLens;

impl XminHorizonHoldLens {
    const ID: &'static str = "PG-HORIZON-013";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_ACTIVITY,
            column: "backend_xmin_age",
        },
        SectionColumn {
            section: PG_STAT_ACTIVITY,
            column: "xact_start",
        },
    ];
    /// Below this xmin age the hold is ordinary transaction churn, not a horizon
    /// a vacuum is waiting on.
    const MIN_XMIN_AGE: i64 = 1_000_000;
    const MIN_XMIN_AGE_F64: f64 = 1_000_000.0;
    /// A running (non-idle) transaction must be at least this old, in
    /// microseconds, to count as a horizon-holding long transaction (5 minutes).
    const MIN_LONG_XACT_US: i64 = 300_000_000;
    const MIN_LONG_XACT_US_F64: f64 = 300_000_000.0;

    fn holds_horizon(backend: &ActivityBackend) -> bool {
        backend
            .xmin_age
            .is_some_and(|age| age >= Self::MIN_XMIN_AGE)
            && (idle_in_transaction(backend.state.as_deref())
                || backend
                    .xact_age_us
                    .is_some_and(|age| age >= Self::MIN_LONG_XACT_US))
    }
}

impl Lens for XminHorizonHoldLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
        let Some(member) = activity_member(cluster) else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        sink.charge_points(activity_backends_examined(typed, start, end))?;
        let candidate = typed
            .activity_window(start, end)
            .flat_map(|snapshot| {
                snapshot.backends.iter().filter_map(move |backend| {
                    (backend.pid > 0 && backend.backend_start > 0 && Self::holds_horizon(backend))
                        .then_some((snapshot.ts, backend))
                })
            })
            .max_by_key(|(_, backend)| backend.xmin_age.unwrap_or_default());
        if let Some((observed_at_us, backend)) = candidate {
            let identity: Arc<[IdentityValue]> = Arc::from(vec![
                IdentityValue::I64(backend.pid),
                IdentityValue::I64(backend.backend_start),
            ]);
            let Some(xmin_age) = backend.xmin_age.and_then(exact_i64_as_f64) else {
                return Ok(());
            };
            let Some(xmin_evidence) = GaugeEvidence::value(
                xmin_age,
                GaugeUnit::Count,
                Self::MIN_XMIN_AGE_F64,
                ThresholdKind::AtLeast,
                observed_at_us,
                1,
                GaugeEntity::new(PG_STAT_ACTIVITY, Arc::clone(&identity)),
            ) else {
                return Ok(());
            };
            let mut evidence = vec![Evidence::GaugeObservation(xmin_evidence)];
            if let Some(xact_age_us) = backend.xact_age_us.and_then(exact_i64_as_f64)
                && let Some(xact_evidence) = GaugeEvidence::value(
                    xact_age_us,
                    GaugeUnit::Microseconds,
                    Self::MIN_LONG_XACT_US_F64,
                    ThresholdKind::AtLeast,
                    observed_at_us,
                    1,
                    GaugeEntity::new(PG_STAT_ACTIVITY, identity),
                )
            {
                evidence.push(Evidence::GaugeObservation(xact_evidence));
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                evidence,
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-SYNC-018` (`sync_replication_wait`): the same backend session sampled on
/// `wait_event='SyncRep'` in three consecutive snapshots. This is a positive
/// sampled observation, not elapsed wait duration or a standby root cause.
pub(crate) struct SyncReplicationWaitLens;

impl SyncReplicationWaitLens {
    const ID: &'static str = "PG-SYNC-018";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_STAT_ACTIVITY,
        column: "wait_event",
    }];
    /// Require the same session in three consecutive stored snapshots. This is
    /// sampled persistence, not elapsed wait duration.
    const MIN_CONSECUTIVE_SAMPLES: usize = 3;
}

impl Lens for SyncReplicationWaitLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
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
        let Some(member) = activity_member(cluster) else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        sink.charge_points(activity_backends_examined(typed, start, end))?;
        let mut runs: BTreeMap<(i64, i64), (usize, usize, i64)> = BTreeMap::new();
        for (snapshot_index, snapshot) in typed.activity_window(start, end).enumerate() {
            for backend in snapshot.backends.iter().filter(|backend| {
                backend.pid > 0
                    && backend.backend_start > 0
                    && is_syncrep(backend.wait_event.as_deref())
            }) {
                let entry = runs.entry((backend.pid, backend.backend_start)).or_insert((
                    snapshot_index,
                    0,
                    snapshot.ts,
                ));
                entry.1 = if entry.1 > 0 && entry.0.checked_add(1) == Some(snapshot_index) {
                    entry.1.saturating_add(1)
                } else {
                    1
                };
                entry.0 = snapshot_index;
                entry.2 = snapshot.ts;
            }
        }
        let persistent = runs
            .into_iter()
            .filter(|(_, (_, samples, _))| *samples >= Self::MIN_CONSECUTIVE_SAMPLES)
            .max_by_key(|(_, (_, samples, _))| *samples);
        if let Some(((pid, backend_start), (_, samples, observed_at_us))) = persistent {
            let Some(samples_value) = u32::try_from(samples).ok().map(f64::from) else {
                return Ok(());
            };
            let identity: Arc<[IdentityValue]> = Arc::from(vec![
                IdentityValue::I64(pid),
                IdentityValue::I64(backend_start),
            ]);
            let Some(evidence) = GaugeEvidence::value(
                samples_value,
                GaugeUnit::Count,
                3.0,
                ThresholdKind::AtLeast,
                observed_at_us,
                samples,
                GaugeEntity::new(PG_STAT_ACTIVITY, identity),
            ) else {
                return Ok(());
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-WAIT-019` (`internal_wait_concentration`): active backends piling onto
/// internal waits. Reports each bounded wait class separately only when at
/// least three snapshots carry explicit complete-visibility provenance and the
/// class accounts for at least half of the visible active-backend denominator.
pub(crate) struct InternalWaitConcentrationLens;

impl InternalWaitConcentrationLens {
    const ID: &'static str = "PG-WAIT-019";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_STAT_ACTIVITY,
        column: "wait_event_type",
    }];
    /// A fraction over too few active backends is noise; require a floor so the
    /// concentration is meaningful.
    const MIN_ACTIVE: usize = 3;

    const MIN_SNAPSHOTS: usize = 3;
    const WAIT_FRACTION: f64 = 0.5;
}

impl Lens for InternalWaitConcentrationLens {
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
        let Some(member) = activity_member(cluster) else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        sink.charge_points(activity_backends_examined(typed, start, end))?;
        let mut snapshots = 0_usize;
        let mut active_total = 0_usize;
        let mut class_totals = [0_usize; 3];
        let mut observed_at_us = start;
        for snapshot in typed
            .activity_window(start, end)
            .filter(|snapshot| snapshot.completeness.denominator_usable())
        {
            let active = snapshot
                .backends
                .iter()
                .filter(|backend| backend.state.as_deref() == Some("active"))
                .count();
            if active < Self::MIN_ACTIVE {
                continue;
            }
            snapshots = snapshots.saturating_add(1);
            active_total = active_total.saturating_add(active);
            observed_at_us = snapshot.ts;
            for backend in snapshot
                .backends
                .iter()
                .filter(|backend| backend.state.as_deref() == Some("active"))
            {
                match backend.wait_event_type.as_deref() {
                    Some("LWLock") => class_totals[0] = class_totals[0].saturating_add(1),
                    Some("BufferPin") => class_totals[1] = class_totals[1].saturating_add(1),
                    Some("IO") => class_totals[2] = class_totals[2].saturating_add(1),
                    _ => {}
                }
            }
        }
        let concentrated = class_totals
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, count)| {
                snapshots >= Self::MIN_SNAPSHOTS
                    && active_total > 0
                    && count.saturating_mul(2) >= active_total
            })
            .max_by_key(|(_, count)| *count);
        if let Some((class, waiting)) = concentrated {
            let class = match class {
                0 => "LWLock",
                1 => "BufferPin",
                _ => "IO",
            };
            let identity: Arc<[IdentityValue]> =
                Arc::from(vec![IdentityValue::Text(class.to_owned())]);
            let (Some(waiting), Some(active_total)) = (
                u32::try_from(waiting).ok().map(f64::from),
                u32::try_from(active_total).ok().map(f64::from),
            ) else {
                return Ok(());
            };
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(waiting, active_total, GaugeUnit::Count),
                Self::WAIT_FRACTION,
                ThresholdKind::AtLeast,
                observed_at_us,
                snapshots,
                GaugeEntity::new(PG_STAT_ACTIVITY, identity),
            ) else {
                return Ok(());
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
                None,
            ))?;
        }
        Ok(())
    }
}

/// `PG-LOCK-012` (`lock_wait_graph`): a sampled lock wait-for graph. Reports a
/// lead when a `pg_locks` snapshot in the incident window carries a `blocked_by`
/// edge — direct evidence of a process that prevented a waiter from acquiring
/// the requested lock. It may name a queue predecessor instead of a lock holder.
/// This is the only snapshot lens with structural direction and high confidence.
pub(crate) struct LockWaitGraphLens;

impl LockWaitGraphLens {
    const ID: &'static str = "PG-LOCK-012";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_LOCKS,
        column: "blocked_by",
    }];
}

impl Lens for LockWaitGraphLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
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
        let Some(_member) = cluster
            .members
            .iter()
            .find(|member| member.logical_section == PG_LOCKS)
        else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        let snapshots: Vec<_> = typed.lock_window(start, end).collect();
        let examined: usize = snapshots.iter().map(|snapshot| snapshot.edges.len()).sum();
        sink.charge_points(examined)?;
        for snapshot in snapshots {
            for edge in &snapshot.edges {
                let identity: Arc<[IdentityValue]> = Arc::from(vec![
                    IdentityValue::I64(snapshot.ts),
                    IdentityValue::I64(edge.waiter_pid),
                    IdentityValue::I64(edge.blocker_pid),
                ]);
                sink.emit(FindingDraft::new(
                    Role::Lead,
                    FindingScope::from_parts(PG_LOCKS, "blocked_by", identity),
                    vec![Evidence::Direct(DirectEvidence::sampled_lock_edge())],
                    None,
                ))?;
            }
        }
        Ok(())
    }
}

/// The first `pg_stat_activity` episode of a cluster, or `None` when the section
/// is not represented. The activity lenses report once per incident, scoped to
/// this episode, rather than once per matching member.
fn activity_member(cluster: &Cluster) -> Option<&super::model::EpisodeRefV1> {
    cluster
        .members
        .iter()
        .find(|member| member.logical_section == PG_STAT_ACTIVITY)
}

/// Total backends the activity lenses scan over the incident window, charged as
/// work before analysis.
fn activity_backends_examined(typed: &TypedInputs, start: i64, end: i64) -> usize {
    typed
        .activity_window(start, end)
        .map(|snapshot| snapshot.backends.len())
        .sum()
}

/// Whether a session state is an open but idle transaction, which pins the
/// vacuum horizon without doing work.
fn idle_in_transaction(state: Option<&str>) -> bool {
    matches!(
        state,
        Some("idle in transaction" | "idle in transaction (aborted)")
    )
}

/// Whether a wait event is the synchronous-replication commit wait.
fn is_syncrep(wait_event: Option<&str>) -> bool {
    wait_event == Some("SyncRep")
}

fn exact_i64_as_f64(value: i64) -> Option<f64> {
    const MAX_EXACT_INTEGER: i64 = 1_i64 << 53;
    if !(-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&value) {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "the preceding bound proves exact IEEE-754 integer representation"
    )]
    Some(value as f64)
}

/// Whether a wait-event class is an internal (non-client) contention point.
fn is_internal_wait(wait_event_type: Option<&str>) -> bool {
    matches!(wait_event_type, Some("LWLock" | "BufferPin" | "IO"))
}

/// The lenses whose typed inputs are wired, applied to every request.
pub(crate) fn active_catalog() -> Vec<Box<dyn Lens>> {
    vec![
        Box::new(SharedBufferMissesLens),
        Box::new(WalAmplificationLens),
        Box::new(TempSpillLens),
        Box::new(RequestedCheckpointsLens),
        Box::new(BackendIoLatencyLens),
        Box::new(HotUpdateFailureLens),
        Box::new(WalArchivingFailureLens),
        Box::new(NetworkErrorsLens),
        Box::new(CgroupCpuThrottlingLens),
        Box::new(StaleStatisticsLens),
        Box::new(ConnectionSaturationLens),
        Box::new(MemoryReclaimLens),
        Box::new(WritebackPressureLens),
        Box::new(RunningVacuumLens),
        Box::new(FreezeHorizonLens),
        Box::new(PhysicalReplicationLens),
        Box::new(SlotRetentionLens),
        Box::new(CgroupMemoryLens),
        Box::new(StorageCapacityLens),
        Box::new(QueryWorkLens),
        Box::new(PlanChurnLens),
        Box::new(HostCpuLens),
        Box::new(BlockDeviceLens),
        Box::new(ProcessIoWhoLens),
        Box::new(XminHorizonHoldLens),
        Box::new(SyncReplicationWaitLens),
        Box::new(InternalWaitConcentrationLens),
        Box::new(LockWaitGraphLens),
    ]
}

/// The ids of the active lenses, in catalog order.
pub(crate) fn active_catalog_ids() -> Vec<&'static str> {
    active_catalog().iter().map(|lens| lens.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::super::evidence::Confidence;
    use super::*;
    use crate::incident::model::{EnrichedEpisode, EpisodeRefV1};
    use crate::incident::{ClockRelation, IncidentConfig, LockEdge, LockSnapshot, analyze};
    use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Scalar};

    fn id() -> Arc<[IdentityValue]> {
        Arc::from(vec![IdentityValue::U64(5)])
    }

    fn episode_window(start_us: i64, end_us: i64) -> EnrichedEpisode {
        EnrichedEpisode {
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
                    sigma_used: 1.4826,
                    n_cur: 0,
                    n_ref: 0,
                },
            },
            reference: EpisodeRefV1 {
                logical_section: PG_STAT_DATABASE,
                column: "blks_read",
                identity: id(),
                start_us,
                end_us,
            },
        }
    }

    // One-second interval, so the recovered delta equals `delta`.
    fn point(delta: f64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Int(0),
            rate: delta,
            dt_micros: 1_000_000,
        }
    }

    fn typed(read: &[f64], hit: &[f64]) -> TypedInputs {
        let mut typed = TypedInputs::new();
        let read_points = read.iter().zip(0_i64..).map(|(&d, ts)| (ts, point(d)));
        let hit_points = hit.iter().zip(0_i64..).map(|(&d, ts)| (ts, point(d)));
        typed.insert_counter(PG_STAT_DATABASE, "blks_read", id(), read_points.collect());
        typed.insert_counter(PG_STAT_DATABASE, "blks_hit", id(), hit_points.collect());
        typed
    }

    fn run(typed: &TypedInputs) -> Vec<(Role, Confidence)> {
        run_window(typed, 0, 10)
    }

    fn run_window(typed: &TypedInputs, start_us: i64, end_us: i64) -> Vec<(Role, Confidence)> {
        let lens = SharedBufferMissesLens;
        let lenses: [&dyn Lens; 1] = [&lens];
        let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
        let outcome = analyze(
            vec![episode_window(start_us, end_us)],
            &SeriesSet::for_test(0),
            typed,
            &lenses,
            &config,
        )
        .expect("valid analysis");
        outcome.incidents[0]
            .findings
            .iter()
            .map(|finding| (finding.role(), finding.confidence()))
            .collect()
    }

    #[test]
    fn a_cold_cache_over_enough_intervals_reports_a_medium_amplifier() {
        // miss ratio 80/(80+20) = 0.8, three valid intervals.
        let findings = run(&typed(&[30.0, 30.0, 20.0], &[5.0, 5.0, 10.0]));
        assert_eq!(findings, vec![(Role::Amplifier, Confidence::MEDIUM)]);
    }

    #[test]
    fn counter_evidence_outside_the_incident_window_does_not_report() {
        let typed = typed(
            &[80.0, 80.0, 80.0, 1.0, 1.0, 1.0],
            &[1.0, 1.0, 1.0, 99.0, 99.0, 99.0],
        );
        assert!(
            run_window(&typed, 3, 5).is_empty(),
            "the cold-cache intervals end before the incident window"
        );
    }

    #[test]
    fn a_warm_cache_reports_nothing() {
        // miss ratio 3/(3+297) = 0.01, below the threshold.
        assert!(run(&typed(&[1.0, 1.0, 1.0], &[99.0, 99.0, 99.0])).is_empty());
    }

    #[test]
    fn a_ratio_below_the_threshold_reports_nothing() {
        // miss ratio 57/(57+243) = 0.19, just under the 0.2 floor.
        assert!(run(&typed(&[19.0, 19.0, 19.0], &[81.0, 81.0, 81.0])).is_empty());
    }

    #[test]
    fn too_few_valid_intervals_report_nothing() {
        // A cold cache but only two intervals: below the data-quality minimum.
        assert!(run(&typed(&[50.0, 50.0], &[1.0, 1.0])).is_empty());
    }

    #[test]
    fn an_empty_input_reports_nothing() {
        assert!(run(&TypedInputs::new()).is_empty());
    }

    #[test]
    fn the_active_catalog_lists_every_wired_lens_once() {
        let ids = active_catalog_ids();
        assert_eq!(
            ids,
            vec![
                "PG-CACHE-010",
                "PG-WAL-009",
                "PG-TEMP-003",
                "PG-CHKPT-008",
                "PG-IO-011",
                "PG-HOT-007",
                "PG-ARCH-017",
                "OS-NET-028",
                "OS-CGRP-021",
                "PG-ANALYZE-004",
                "PG-CONN-014",
                "OS-MEM-022",
                "OS-WB-025",
                "PG-VACUUM-005",
                "PG-FREEZE-006",
                "PG-REPL-015",
                "PG-SLOT-016",
                "OS-CGMEM-023",
                "OS-FS-027",
                "PG-QRY-001",
                "PG-PLAN-002",
                "OS-CPU-020",
                "OS-BLOCK-024",
                "OS-IOWHO-026",
                "PG-HORIZON-013",
                "PG-SYNC-018",
                "PG-WAIT-019",
                "PG-LOCK-012",
            ]
        );
        let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "active ids are unique");
        assert_eq!(active_catalog().len(), ids.len());
    }

    // One-second interval; the recovered delta equals `delta`.
    fn pair(
        section: &'static str,
        column_a: &'static str,
        a: &[f64],
        column_b: &'static str,
        b: &[f64],
    ) -> TypedInputs {
        let points = |deltas: &[f64]| -> Vec<(i64, DiffPoint)> {
            deltas
                .iter()
                .zip(0_i64..)
                .map(|(&d, ts)| (ts, point(d)))
                .collect()
        };
        let mut typed = TypedInputs::new();
        typed.insert_counter(section, column_a, id(), points(a));
        typed.insert_counter(section, column_b, id(), points(b));
        typed
    }

    fn run_lens(
        lens: &dyn Lens,
        section: &'static str,
        column: &'static str,
        typed: &TypedInputs,
    ) -> Vec<(Role, Confidence)> {
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
                    sigma_used: 1.4826,
                    n_cur: 0,
                    n_ref: 0,
                },
            },
            reference: EpisodeRefV1 {
                logical_section: section,
                column,
                identity: id(),
                start_us: 0,
                end_us: 10,
            },
        };
        let lenses: [&dyn Lens; 1] = [lens];
        let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
        let outcome = analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            typed,
            &lenses,
            &config,
        )
        .expect("valid analysis");
        outcome.incidents[0]
            .findings
            .iter()
            .map(|finding| (finding.role(), finding.confidence()))
            .collect()
    }

    #[test]
    fn wal_amplification_reports_a_medium_amplifier_above_the_fpi_floor() {
        // 6 FPIs over 10 records = 0.6, three intervals.
        let typed = pair(
            PG_STAT_WAL,
            "wal_fpi",
            &[2.0, 2.0, 2.0],
            "wal_records",
            &[4.0, 3.0, 3.0],
        );
        assert_eq!(
            run_lens(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn wal_amplification_below_the_floor_reports_nothing() {
        // 3 FPIs over 30 records = 0.1, below 0.5.
        let typed = pair(
            PG_STAT_WAL,
            "wal_fpi",
            &[1.0, 1.0, 1.0],
            "wal_records",
            &[10.0, 10.0, 10.0],
        );
        assert!(run_lens(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed).is_empty());
    }

    #[test]
    fn wal_amplification_with_too_few_intervals_reports_nothing() {
        let typed = pair(
            PG_STAT_WAL,
            "wal_fpi",
            &[9.0, 9.0],
            "wal_records",
            &[10.0, 10.0],
        );
        assert!(run_lens(&WalAmplificationLens, PG_STAT_WAL, "wal_fpi", &typed).is_empty());
    }

    #[test]
    fn wal_amplification_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &WalAmplificationLens,
                PG_STAT_WAL,
                "wal_fpi",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn temp_spill_reports_a_medium_amplifier_when_both_counters_advance() {
        let typed = pair(
            PG_STAT_DATABASE,
            "temp_bytes",
            &[8_192.0; 3],
            "temp_files",
            &[1.0, 1.0, 2.0],
        );
        assert_eq!(
            run_lens(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn temp_spill_without_file_growth_reports_nothing() {
        // Bytes advanced but no temp file was created over the incident.
        let typed = pair(
            PG_STAT_DATABASE,
            "temp_bytes",
            &[8_192.0; 3],
            "temp_files",
            &[0.0, 0.0, 0.0],
        );
        assert!(run_lens(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed).is_empty());
    }

    #[test]
    fn temp_spill_with_too_few_intervals_reports_nothing() {
        let typed = pair(
            PG_STAT_DATABASE,
            "temp_bytes",
            &[8_192.0, 8_192.0],
            "temp_files",
            &[1.0, 1.0],
        );
        assert!(run_lens(&TempSpillLens, PG_STAT_DATABASE, "temp_bytes", &typed).is_empty());
    }

    #[test]
    fn temp_spill_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &TempSpillLens,
                PG_STAT_DATABASE,
                "temp_bytes",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn requested_checkpoints_reports_a_medium_amplifier_above_the_floor() {
        // 9 requested vs 3 timed = 0.75, three intervals.
        let typed = pair(
            CHECKPOINTER,
            "checkpoints_req",
            &[3.0, 3.0, 3.0],
            "checkpoints_timed",
            &[1.0, 1.0, 1.0],
        );
        assert_eq!(
            run_lens(
                &RequestedCheckpointsLens,
                CHECKPOINTER,
                "checkpoints_req",
                &typed
            ),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn requested_checkpoints_below_the_floor_reports_nothing() {
        // 3 requested vs 12 timed = 0.2, below 0.5.
        let typed = pair(
            CHECKPOINTER,
            "checkpoints_req",
            &[1.0, 1.0, 1.0],
            "checkpoints_timed",
            &[4.0, 4.0, 4.0],
        );
        assert!(
            run_lens(
                &RequestedCheckpointsLens,
                CHECKPOINTER,
                "checkpoints_req",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn requested_checkpoints_with_too_few_intervals_reports_nothing() {
        let typed = pair(
            CHECKPOINTER,
            "checkpoints_req",
            &[9.0, 9.0],
            "checkpoints_timed",
            &[1.0, 1.0],
        );
        assert!(
            run_lens(
                &RequestedCheckpointsLens,
                CHECKPOINTER,
                "checkpoints_req",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn requested_checkpoints_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &RequestedCheckpointsLens,
                CHECKPOINTER,
                "checkpoints_req",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn backend_io_latency_reports_a_medium_amplifier_above_the_floor() {
        // 30 ms over 10 reads = 3 ms/read, three intervals.
        let typed = pair(
            PG_STAT_IO,
            "read_time",
            &[10.0, 10.0, 10.0],
            "reads",
            &[3.0, 3.0, 4.0],
        );
        assert_eq!(
            run_lens(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn backend_io_latency_below_the_floor_reports_nothing() {
        // 3 ms over 30 reads = 0.1 ms/read, below 1 ms.
        let typed = pair(
            PG_STAT_IO,
            "read_time",
            &[1.0, 1.0, 1.0],
            "reads",
            &[10.0, 10.0, 10.0],
        );
        assert!(run_lens(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed).is_empty());
    }

    #[test]
    fn backend_io_latency_with_too_few_intervals_reports_nothing() {
        let typed = pair(PG_STAT_IO, "read_time", &[10.0, 10.0], "reads", &[1.0, 1.0]);
        assert!(run_lens(&BackendIoLatencyLens, PG_STAT_IO, "read_time", &typed).is_empty());
    }

    #[test]
    fn backend_io_latency_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &BackendIoLatencyLens,
                PG_STAT_IO,
                "read_time",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn hot_update_failure_reports_a_medium_amplifier_above_the_floor() {
        // 3 HOT of 12 updates = 75% non-HOT, three intervals.
        let typed = pair(
            PG_STAT_USER_TABLES,
            "n_tup_hot_upd",
            &[1.0, 1.0, 1.0],
            "n_tup_upd",
            &[4.0, 4.0, 4.0],
        );
        assert_eq!(
            run_lens(
                &HotUpdateFailureLens,
                PG_STAT_USER_TABLES,
                "n_tup_upd",
                &typed
            ),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn hot_update_failure_when_hot_dominates_reports_nothing() {
        // 9 HOT of 10 updates = 10% non-HOT, below 50%.
        let typed = pair(
            PG_STAT_USER_TABLES,
            "n_tup_hot_upd",
            &[3.0, 3.0, 3.0],
            "n_tup_upd",
            &[3.0, 3.0, 4.0],
        );
        assert!(
            run_lens(
                &HotUpdateFailureLens,
                PG_STAT_USER_TABLES,
                "n_tup_upd",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn hot_update_failure_with_too_few_intervals_reports_nothing() {
        let typed = pair(
            PG_STAT_USER_TABLES,
            "n_tup_hot_upd",
            &[0.0, 0.0],
            "n_tup_upd",
            &[5.0, 5.0],
        );
        assert!(
            run_lens(
                &HotUpdateFailureLens,
                PG_STAT_USER_TABLES,
                "n_tup_upd",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn hot_update_failure_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &HotUpdateFailureLens,
                PG_STAT_USER_TABLES,
                "n_tup_upd",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn wal_archiving_failure_reports_a_medium_coincident_on_any_failure() {
        // One failure recorded in a single usable interval.
        let typed = pair(
            PG_STAT_ARCHIVER,
            "failed_count",
            &[1.0],
            "archived_count",
            &[4.0],
        );
        assert_eq!(
            run_lens(
                &WalArchivingFailureLens,
                PG_STAT_ARCHIVER,
                "failed_count",
                &typed
            ),
            vec![(Role::Coincident, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn wal_archiving_failure_without_failures_reports_nothing() {
        let typed = pair(
            PG_STAT_ARCHIVER,
            "failed_count",
            &[0.0, 0.0],
            "archived_count",
            &[5.0, 5.0],
        );
        assert!(
            run_lens(
                &WalArchivingFailureLens,
                PG_STAT_ARCHIVER,
                "failed_count",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn wal_archiving_failure_with_no_usable_interval_reports_nothing() {
        let mut typed = TypedInputs::new();
        typed.insert_counter(PG_STAT_ARCHIVER, "failed_count", id(), Vec::new());
        typed.insert_counter(PG_STAT_ARCHIVER, "archived_count", id(), Vec::new());
        assert!(
            run_lens(
                &WalArchivingFailureLens,
                PG_STAT_ARCHIVER,
                "failed_count",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn wal_archiving_failure_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &WalArchivingFailureLens,
                PG_STAT_ARCHIVER,
                "failed_count",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn network_errors_reports_a_low_coincident_above_the_floor() {
        // 2 errors over 100 packets = 2%, three intervals; capped at low.
        let typed = pair(
            OS_NETDEV,
            "rx_errs",
            &[1.0, 1.0, 0.0],
            "rx_packets",
            &[30.0, 30.0, 40.0],
        );
        assert_eq!(
            run_lens(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed),
            vec![(Role::Coincident, Confidence::LOW)]
        );
    }

    #[test]
    fn network_errors_below_the_floor_reports_nothing() {
        // 3 errors over 30000 packets = 0.01%, below 1%.
        let typed = pair(
            OS_NETDEV,
            "rx_errs",
            &[1.0, 1.0, 1.0],
            "rx_packets",
            &[10_000.0, 10_000.0, 10_000.0],
        );
        assert!(run_lens(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed).is_empty());
    }

    #[test]
    fn network_errors_with_too_few_intervals_reports_nothing() {
        let typed = pair(
            OS_NETDEV,
            "rx_errs",
            &[5.0, 5.0],
            "rx_packets",
            &[10.0, 10.0],
        );
        assert!(run_lens(&NetworkErrorsLens, OS_NETDEV, "rx_errs", &typed).is_empty());
    }

    #[test]
    fn network_errors_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &NetworkErrorsLens,
                OS_NETDEV,
                "rx_errs",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn cgroup_throttling_reports_a_medium_amplifier_above_the_floor() {
        // 300 of 1000 us throttled = 0.3, three intervals.
        let typed = pair(
            OS_CGROUP_CPU,
            "throttled_usec",
            &[100.0, 100.0, 100.0],
            "usage_usec",
            &[200.0, 200.0, 300.0],
        );
        assert_eq!(
            run_lens(
                &CgroupCpuThrottlingLens,
                OS_CGROUP_CPU,
                "throttled_usec",
                &typed
            ),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn cgroup_throttling_below_the_floor_reports_nothing() {
        // 30 of 1030 us throttled = 3%, below 10%.
        let typed = pair(
            OS_CGROUP_CPU,
            "throttled_usec",
            &[10.0, 10.0, 10.0],
            "usage_usec",
            &[330.0, 330.0, 340.0],
        );
        assert!(
            run_lens(
                &CgroupCpuThrottlingLens,
                OS_CGROUP_CPU,
                "throttled_usec",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn cgroup_throttling_with_too_few_intervals_reports_nothing() {
        let typed = pair(
            OS_CGROUP_CPU,
            "throttled_usec",
            &[100.0, 100.0],
            "usage_usec",
            &[100.0, 100.0],
        );
        assert!(
            run_lens(
                &CgroupCpuThrottlingLens,
                OS_CGROUP_CPU,
                "throttled_usec",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn cgroup_throttling_on_empty_input_reports_nothing() {
        assert!(
            run_lens(
                &CgroupCpuThrottlingLens,
                OS_CGROUP_CPU,
                "throttled_usec",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    // Gauge readings at ts 0.. within the run_lens incident window `[0, 10]`.
    fn gauges(section: &'static str, columns: &[(&'static str, &[f64])]) -> TypedInputs {
        let mut typed = TypedInputs::new();
        for &(name, values) in columns {
            let points = values
                .iter()
                .zip(0_i64..)
                .map(|(&value, ts)| (ts, value))
                .collect();
            typed.insert_gauge(section, name, id(), points);
        }
        typed
    }

    #[test]
    fn stale_statistics_uses_reltuples_and_reports_only_the_observation() {
        let typed = gauges(
            PG_STAT_USER_TABLES,
            &[("n_mod_since_analyze", &[250.0]), ("reltuples", &[1_000.0])],
        );
        assert_eq!(
            run_lens(
                &StaleStatisticsLens,
                PG_STAT_USER_TABLES,
                "n_mod_since_analyze",
                &typed,
            ),
            vec![(Role::Coincident, Confidence::LOW)]
        );
    }

    #[test]
    fn stale_statistics_below_the_ratio_reports_nothing() {
        let typed = gauges(
            PG_STAT_USER_TABLES,
            &[("n_mod_since_analyze", &[199.0]), ("reltuples", &[1_000.0])],
        );
        assert!(
            run_lens(
                &StaleStatisticsLens,
                PG_STAT_USER_TABLES,
                "n_mod_since_analyze",
                &typed,
            )
            .is_empty()
        );
    }

    #[test]
    fn stale_statistics_uses_absolute_estimate_and_includes_equality() {
        let typed = gauges(
            PG_STAT_USER_TABLES,
            &[
                ("n_mod_since_analyze", &[200.0]),
                ("reltuples", &[-1_000.0]),
            ],
        );
        assert_eq!(
            run_lens(
                &StaleStatisticsLens,
                PG_STAT_USER_TABLES,
                "n_mod_since_analyze",
                &typed,
            ),
            vec![(Role::Coincident, Confidence::LOW)]
        );
    }

    #[test]
    fn per_database_connection_limit_includes_the_threshold_boundary() {
        let typed = gauges(
            PG_STAT_DATABASE,
            &[("numbackends", &[80.0]), ("datconnlimit", &[100.0])],
        );
        assert_eq!(
            run_lens(
                &ConnectionSaturationLens,
                PG_STAT_DATABASE,
                "numbackends",
                &typed,
            ),
            vec![(Role::Coincident, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn nonpositive_database_connection_limits_are_not_denominators() {
        for limit in [-2.0, -1.0, 0.0] {
            let typed = gauges(
                PG_STAT_DATABASE,
                &[("numbackends", &[80.0]), ("datconnlimit", &[limit])],
            );
            assert!(
                run_lens(
                    &ConnectionSaturationLens,
                    PG_STAT_DATABASE,
                    "numbackends",
                    &typed,
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn low_host_available_memory_is_a_low_confidence_observation() {
        let typed = gauges(
            OS_MEMINFO,
            &[("mem_available", &[4.0]), ("mem_total", &[100.0])],
        );
        assert_eq!(
            run_lens(&MemoryReclaimLens, OS_MEMINFO, "mem_available", &typed,),
            vec![(Role::Coincident, Confidence::LOW)]
        );
    }

    #[test]
    fn available_memory_equal_to_the_floor_does_not_cross_below_it() {
        let typed = gauges(
            OS_MEMINFO,
            &[("mem_available", &[5.0]), ("mem_total", &[100.0])],
        );
        assert!(run_lens(&MemoryReclaimLens, OS_MEMINFO, "mem_available", &typed,).is_empty());
    }

    #[test]
    fn zero_host_memory_total_is_not_a_denominator() {
        let typed = gauges(
            OS_MEMINFO,
            &[("mem_available", &[0.0]), ("mem_total", &[0.0])],
        );
        assert!(run_lens(&MemoryReclaimLens, OS_MEMINFO, "mem_available", &typed,).is_empty());
    }

    #[test]
    fn writeback_ratio_uses_dirty_plus_writeback_at_one_timestamp() {
        let typed = gauges(
            OS_MEMINFO,
            &[
                ("dirty", &[6.0]),
                ("writeback", &[4.0]),
                ("mem_total", &[100.0]),
            ],
        );
        assert_eq!(
            run_lens(&WritebackPressureLens, OS_MEMINFO, "dirty", &typed,),
            vec![(Role::Coincident, Confidence::LOW)]
        );
    }

    #[test]
    fn active_and_dormant_lenses_are_accounted_once() {
        let active = active_catalog_ids();
        assert_eq!(active.len(), 28);
        assert_eq!(crate::incident::core_catalog().len(), active.len());
        assert!(crate::incident::dormant_catalog().is_empty());
        let unique: std::collections::BTreeSet<_> = active.iter().copied().collect();
        assert_eq!(unique.len(), active.len());
    }

    #[test]
    fn gauge_window_work_is_admitted_before_reduction() {
        let typed = gauges(
            OS_MEMINFO,
            &[
                ("mem_available", &[4.0, 4.0, 4.0]),
                ("mem_total", &[100.0, 100.0, 100.0]),
            ],
        );
        let mut episode = episode_window(0, 10);
        episode.reference.logical_section = OS_MEMINFO;
        episode.reference.column = "mem_available";
        let lens = MemoryReclaimLens;
        let lenses: [&dyn Lens; 1] = [&lens];
        let config =
            IncidentConfig::for_test_with_work_limit("node", 5, 1_000, ClockRelation::Unknown, 5);
        let outcome = analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            &typed,
            &lenses,
            &config,
        )
        .expect("valid analysis");
        assert!(!outcome.complete);
        assert!(outcome.incidents[0].findings.is_empty());
        assert_eq!(outcome.skipped[0].limit.axis, super::super::LimitAxis::Work);
        assert_eq!(outcome.skipped[0].limit.observed, 8);
        assert_eq!(outcome.skipped[0].limit.limit, 5);
    }

    fn base_backend() -> ActivityBackend {
        ActivityBackend {
            pid: 1,
            backend_start: 1,
            xid_age: None,
            xmin_age: None,
            state: None,
            wait_event_type: None,
            wait_event: None,
            xact_age_us: None,
        }
    }

    // A snapshot at ts=5 sits inside the run_lens episode window [0, 10].
    fn activity_typed(backends: Vec<ActivityBackend>) -> TypedInputs {
        let mut typed = TypedInputs::new();
        typed.insert_activity_snapshot(ActivitySnapshot {
            ts: 5,
            backends,
            completeness: SnapshotCompleteness::Complete,
        });
        typed
    }

    fn repeated_activity_typed(backends: &[ActivityBackend]) -> TypedInputs {
        let mut typed = TypedInputs::new();
        for ts in [3, 5, 7] {
            typed.insert_activity_snapshot(ActivitySnapshot {
                ts,
                backends: backends.to_owned(),
                completeness: SnapshotCompleteness::Complete,
            });
        }
        typed
    }

    fn run_activity(lens: &dyn Lens, typed: &TypedInputs) -> Vec<(Role, Confidence)> {
        run_lens(lens, PG_STAT_ACTIVITY, "backend_xmin_age", typed)
    }

    #[test]
    fn xmin_hold_reports_a_medium_amplifier_for_an_old_idle_transaction() {
        let typed = activity_typed(vec![ActivityBackend {
            xmin_age: Some(2_000_000),
            state: Some("idle in transaction".into()),
            ..base_backend()
        }]);
        assert_eq!(
            run_activity(&XminHorizonHoldLens, &typed),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn xmin_hold_reports_for_an_old_long_running_transaction() {
        let typed = activity_typed(vec![ActivityBackend {
            xmin_age: Some(2_000_000),
            state: Some("active".into()),
            xact_age_us: Some(400_000_000),
            ..base_backend()
        }]);
        assert_eq!(
            run_activity(&XminHorizonHoldLens, &typed),
            vec![(Role::Amplifier, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn xmin_hold_ignores_a_fresh_horizon() {
        // Idle in transaction but a young xmin: ordinary churn, not a hold.
        let typed = activity_typed(vec![ActivityBackend {
            xmin_age: Some(100),
            state: Some("idle in transaction".into()),
            ..base_backend()
        }]);
        assert!(run_activity(&XminHorizonHoldLens, &typed).is_empty());
    }

    #[test]
    fn xmin_hold_ignores_an_active_short_transaction() {
        // Old xmin, but a running query that just started holds no horizon yet.
        let typed = activity_typed(vec![ActivityBackend {
            xmin_age: Some(2_000_000),
            state: Some("active".into()),
            xact_age_us: Some(1_000),
            ..base_backend()
        }]);
        assert!(run_activity(&XminHorizonHoldLens, &typed).is_empty());
    }

    #[test]
    fn xmin_hold_ignores_a_backend_without_an_assigned_xmin() {
        let typed = activity_typed(vec![ActivityBackend {
            xmin_age: None,
            state: Some("idle in transaction".into()),
            ..base_backend()
        }]);
        assert!(run_activity(&XminHorizonHoldLens, &typed).is_empty());
    }

    #[test]
    fn xmin_hold_on_empty_input_reports_nothing() {
        assert!(run_activity(&XminHorizonHoldLens, &TypedInputs::new()).is_empty());
    }

    #[test]
    fn sync_replication_reports_a_medium_coincident_on_a_syncrep_wait() {
        let typed = repeated_activity_typed(&[ActivityBackend {
            wait_event: Some("SyncRep".into()),
            ..base_backend()
        }]);
        assert_eq!(
            run_lens(
                &SyncReplicationWaitLens,
                PG_STAT_ACTIVITY,
                "wait_event",
                &typed
            ),
            vec![(Role::Coincident, Confidence::MEDIUM)]
        );
    }

    #[test]
    fn sync_replication_ignores_other_waits() {
        let typed = activity_typed(vec![ActivityBackend {
            wait_event: Some("ClientRead".into()),
            ..base_backend()
        }]);
        assert!(
            run_lens(
                &SyncReplicationWaitLens,
                PG_STAT_ACTIVITY,
                "wait_event",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn sync_replication_requires_the_same_session_in_three_consecutive_samples() {
        let mut typed = TypedInputs::new();
        for (ts, backend_start) in [(3, 1), (5, 2), (7, 1)] {
            typed.insert_activity_snapshot(ActivitySnapshot {
                ts,
                backends: vec![ActivityBackend {
                    pid: 1,
                    backend_start,
                    wait_event: Some("SyncRep".into()),
                    ..base_backend()
                }],
                completeness: SnapshotCompleteness::Restricted,
            });
        }
        assert!(
            run_lens(
                &SyncReplicationWaitLens,
                PG_STAT_ACTIVITY,
                "wait_event",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn internal_wait_reports_a_low_coincident_when_active_backends_concentrate() {
        // Three active backends, two on LWLock: 2*2 >= 3.
        let lwlock = || ActivityBackend {
            state: Some("active".into()),
            wait_event_type: Some("LWLock".into()),
            ..base_backend()
        };
        let running = ActivityBackend {
            state: Some("active".into()),
            ..base_backend()
        };
        let typed = repeated_activity_typed(&[lwlock(), lwlock(), running]);
        assert_eq!(
            run_lens(
                &InternalWaitConcentrationLens,
                PG_STAT_ACTIVITY,
                "wait_event_type",
                &typed
            ),
            vec![(Role::Coincident, Confidence::LOW)]
        );
    }

    #[test]
    fn internal_wait_needs_a_floor_of_active_backends() {
        // Two active backends both on LWLock is concentrated but below the floor.
        let lwlock = || ActivityBackend {
            state: Some("active".into()),
            wait_event_type: Some("LWLock".into()),
            ..base_backend()
        };
        let typed = activity_typed(vec![lwlock(), lwlock()]);
        assert!(
            run_lens(
                &InternalWaitConcentrationLens,
                PG_STAT_ACTIVITY,
                "wait_event_type",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn internal_wait_ignores_a_low_fraction() {
        // Four active backends, one on LWLock: 2*1 < 4.
        let lwlock = ActivityBackend {
            state: Some("active".into()),
            wait_event_type: Some("LWLock".into()),
            ..base_backend()
        };
        let running = || ActivityBackend {
            state: Some("active".into()),
            ..base_backend()
        };
        let typed = activity_typed(vec![lwlock, running(), running(), running()]);
        assert!(
            run_lens(
                &InternalWaitConcentrationLens,
                PG_STAT_ACTIVITY,
                "wait_event_type",
                &typed
            )
            .is_empty()
        );
    }

    #[test]
    fn internal_wait_withholds_ratio_without_complete_snapshot_markers() {
        let backends = vec![
            ActivityBackend {
                state: Some("active".into()),
                wait_event_type: Some("LWLock".into()),
                ..base_backend()
            },
            ActivityBackend {
                state: Some("active".into()),
                wait_event_type: Some("LWLock".into()),
                ..base_backend()
            },
            ActivityBackend {
                state: Some("active".into()),
                ..base_backend()
            },
        ];
        let mut typed = TypedInputs::new();
        for ts in [3, 5, 7] {
            typed.insert_activity_snapshot(ActivitySnapshot {
                ts,
                backends: backends.clone(),
                completeness: SnapshotCompleteness::Unknown,
            });
        }
        assert!(
            run_lens(
                &InternalWaitConcentrationLens,
                PG_STAT_ACTIVITY,
                "wait_event_type",
                &typed
            )
            .is_empty()
        );
    }

    fn lock_typed(edges: Vec<LockEdge>) -> TypedInputs {
        let mut typed = TypedInputs::new();
        typed.insert_lock_snapshot(LockSnapshot { ts: 5, edges });
        typed
    }

    #[test]
    fn lock_wait_graph_leads_at_high_confidence_on_a_sampled_edge() {
        let typed = lock_typed(vec![LockEdge {
            waiter_pid: 20,
            blocker_pid: 10,
        }]);
        let findings = run_lens(&LockWaitGraphLens, PG_LOCKS, "blocked_by", &typed);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].0, Role::Lead, "the lock edge proves direction");
        assert_eq!(
            findings[0].1.label(),
            "high",
            "direct edge evidence reaches high confidence"
        );
    }

    #[test]
    fn lock_wait_graph_without_edges_reports_nothing() {
        // A pg_locks episode with no sampled edge is the honest boundary: the
        // lens is active but has no direct evidence to stand on.
        assert!(
            run_lens(
                &LockWaitGraphLens,
                PG_LOCKS,
                "blocked_by",
                &lock_typed(vec![])
            )
            .is_empty()
        );
        assert!(
            run_lens(
                &LockWaitGraphLens,
                PG_LOCKS,
                "blocked_by",
                &TypedInputs::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn a_snapshot_lens_ignores_a_cluster_without_its_section() {
        let typed = activity_typed(vec![ActivityBackend {
            xmin_age: Some(2_000_000),
            state: Some("idle in transaction".into()),
            ..base_backend()
        }]);
        assert!(run_lens(&XminHorizonHoldLens, PG_STAT_DATABASE, "blks_read", &typed).is_empty());
    }

    #[test]
    fn wait_classifiers_cover_their_event_sets() {
        assert!(is_syncrep(Some("SyncRep")));
        assert!(!is_syncrep(Some("ClientRead")));
        assert!(!is_syncrep(None));

        for internal in ["LWLock", "BufferPin", "IO"] {
            assert!(is_internal_wait(Some(internal)), "{internal} is internal");
        }
        assert!(!is_internal_wait(Some("Client")), "client wait is external");
        assert!(!is_internal_wait(None));

        for idle in ["idle in transaction", "idle in transaction (aborted)"] {
            assert!(idle_in_transaction(Some(idle)), "{idle} pins the horizon");
        }
        assert!(!idle_in_transaction(Some("active")));
        assert!(!idle_in_transaction(None));
    }
}
