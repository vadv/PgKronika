//! Active diagnostic lenses over typed counter and gauge evidence.

mod activity;
mod counters;
mod gauges;
mod locks;
mod shared;
#[cfg(test)]
mod tests;

use super::gauge_contracts::{
    CgroupMemoryLens, FreezeHorizonLens, PhysicalReplicationLens, RunningVacuumLens,
    SlotRetentionLens, StorageCapacityLens,
};
use super::lens::Lens;
use super::os_lenses::{BlockDeviceLens, HostCpuLens, ProcessIoWhoLens};
use super::query_plan::{PlanChurnLens, QueryWorkLens};

pub(crate) use activity::{
    InternalWaitConcentrationLens, SyncReplicationWaitLens, XminHorizonHoldLens,
};
pub(crate) use counters::{
    BackendIoLatencyLens, CgroupCpuThrottlingLens, HotUpdateFailureLens, NetworkErrorsLens,
    RequestedCheckpointsLens, SharedBufferMissesLens, TempSpillLens, WalAmplificationLens,
    WalArchivingFailureLens,
};
pub(crate) use gauges::{
    ConnectionSaturationLens, MemoryReclaimLens, StaleStatisticsLens, WritebackPressureLens,
};
pub(crate) use locks::LockWaitGraphLens;

#[cfg(test)]
use super::evidence::{CounterMeasurementKind, CounterOperandPurpose, Evidence, GaugeUnit, Role};
#[cfg(test)]
use super::model::IdentityValue;
#[cfg(test)]
use super::series::SeriesSet;
#[cfg(test)]
use super::typed::{ActivityBackend, ActivitySnapshot, SnapshotCompleteness, TypedInputs};
#[cfg(test)]
use shared::{
    CHECKPOINTER, OS_CGROUP_CPU, OS_MEMINFO, OS_NETDEV, PG_LOCKS, PG_STAT_ACTIVITY,
    PG_STAT_ARCHIVER, PG_STAT_DATABASE, PG_STAT_IO, PG_STAT_USER_TABLES, PG_STAT_WAL,
    idle_in_transaction, is_internal_wait, is_syncrep,
};
#[cfg(test)]
use std::sync::Arc;

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
