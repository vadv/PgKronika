//! Memory, swap, and page-fault policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, scalar_entry,
};

pub(super) const OS_MEMORY_USED_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsMemoryUsedPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 70.0)),
    Some(boundary(Comparison::AtLeast, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_PROCESS_VIRTUAL_GROWTH_KIB: CatalogEntry = scalar_entry(
    MetricId::OsProcessVirtualGrowthKib,
    Unit::Kibibytes,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 102_400.0)),
    Some(boundary(Comparison::Above, 1_048_576.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_PROCESS_RESIDENT_GROWTH_KIB: CatalogEntry = scalar_entry(
    MetricId::OsProcessResidentGrowthKib,
    Unit::Kibibytes,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 102_400.0)),
    Some(boundary(Comparison::Above, 1_048_576.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_PROCESS_VIRTUAL_SWAP_KIB: CatalogEntry = scalar_entry(
    MetricId::OsProcessVirtualSwapKib,
    Unit::Kibibytes,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::Above, 102_400.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_MEMORY_SWAP_USED_KIB: CatalogEntry = scalar_entry(
    MetricId::OsMemorySwapUsedKib,
    Unit::Kibibytes,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::Above, 1_048_576.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_VMSTAT_SWAP_IN_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::OsVmstatSwapInPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    None,
    Some(boundary(Comparison::Above, 0.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_VMSTAT_SWAP_OUT_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::OsVmstatSwapOutPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    None,
    Some(boundary(Comparison::Above, 0.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_PROCESS_MAJOR_FAULTS_DELTA: CatalogEntry = scalar_entry(
    MetricId::OsProcessMajorFaultsDelta,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 100.0)),
    Some(boundary(Comparison::Above, 10_000.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_PROCESS_RSS_KIB: CatalogEntry = scalar_entry(
    MetricId::OsProcessRssKib,
    Unit::Kibibytes,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 1_048_576.0)),
    Some(boundary(Comparison::Above, 4_194_304.0)),
    ZeroDisposition::Classify,
);
