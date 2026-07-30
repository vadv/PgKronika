//! CPU and load policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, fraction_entry,
    scalar_entry,
};

pub(super) const OS_PROCESS_CPU_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsProcessCpuPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 50.0)),
    Some(boundary(Comparison::AtLeast, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_LOAD_AVG1_PER_CORE: CatalogEntry = fraction_entry(
    MetricId::OsLoadAvg1PerCore,
    Unit::Ratio,
    boundary(Comparison::Above, 1.0),
    boundary(Comparison::Above, 2.0),
);

pub(super) const OS_CPU_IDLE_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsCpuIdlePercent,
    Unit::Percent,
    Direction::LowerIsWorse,
    Some(boundary(Comparison::Below, 30.0)),
    Some(boundary(Comparison::Below, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_CPU_IOWAIT_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsCpuIoWaitPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 5.0)),
    Some(boundary(Comparison::Above, 15.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_CPU_STEAL_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsCpuStealPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 3.0)),
    Some(boundary(Comparison::Above, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_LOAD_PROCS_BLOCKED: CatalogEntry = scalar_entry(
    MetricId::OsLoadProcsBlocked,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::Above, 4.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_ACTIVITY_BACKEND_LOAD_PER_CORE: CatalogEntry = fraction_entry(
    MetricId::PgActivityBackendLoadPerCore,
    Unit::Ratio,
    boundary(Comparison::AtLeast, 0.25),
    boundary(Comparison::AtLeast, 0.5),
);
