//! Linux cgroup policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, scalar_entry,
};

pub(super) const OS_CGROUP_CPU_USED_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsCgroupCpuUsedPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 70.0)),
    Some(boundary(Comparison::AtLeast, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_CGROUP_CPU_THROTTLED_MILLISECONDS_DELTA: CatalogEntry = scalar_entry(
    MetricId::OsCgroupCpuThrottledMillisecondsDelta,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::Above, 100.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_CGROUP_CPU_THROTTLE_EVENTS_DELTA: CatalogEntry = scalar_entry(
    MetricId::OsCgroupCpuThrottleEventsDelta,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    None,
    ZeroDisposition::Inactive,
);

pub(super) const OS_CGROUP_MEMORY_ANON_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsCgroupMemoryAnonPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 70.0)),
    Some(boundary(Comparison::AtLeast, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_CGROUP_MEMORY_HEADROOM_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsCgroupMemoryHeadroomPercent,
    Unit::Percent,
    Direction::LowerIsWorse,
    Some(boundary(Comparison::Below, 20.0)),
    Some(boundary(Comparison::Below, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_CGROUP_MEMORY_OOM_KILLS_DELTA: CatalogEntry = scalar_entry(
    MetricId::OsCgroupMemoryOomKillsDelta,
    Unit::Count,
    Direction::HigherIsWorse,
    None,
    Some(boundary(Comparison::Above, 0.0)),
    ZeroDisposition::Inactive,
);
