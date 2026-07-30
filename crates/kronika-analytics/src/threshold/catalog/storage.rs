//! Disk, filesystem, and network policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary,
    free_capacity_entry, scalar_entry,
};

pub(super) const OS_DISK_UTIL_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsDiskUtilPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 60.0)),
    Some(boundary(Comparison::AtLeast, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_DISK_MAX_AWAIT_MILLISECONDS: CatalogEntry = scalar_entry(
    MetricId::OsDiskMaxAwaitMilliseconds,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 2.0)),
    Some(boundary(Comparison::AtLeast, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_DISK_READ_AWAIT_MILLISECONDS: CatalogEntry = scalar_entry(
    MetricId::OsDiskReadAwaitMilliseconds,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 2.0)),
    Some(boundary(Comparison::AtLeast, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_DISK_WRITE_AWAIT_MILLISECONDS: CatalogEntry = scalar_entry(
    MetricId::OsDiskWriteAwaitMilliseconds,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 2.0)),
    Some(boundary(Comparison::AtLeast, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_FILESYSTEM_FREE_CAPACITY: CatalogEntry = free_capacity_entry(
    MetricId::OsFilesystemFreeCapacity,
    boundary(Comparison::Below, 0.20),
    boundary(Comparison::Below, 0.10),
    boundary(Comparison::Below, 16_106_127_360.0),
);

pub(super) const OS_PROCESS_BLOCK_DELAY_SECONDS_DELTA: CatalogEntry = scalar_entry(
    MetricId::OsProcessBlockDelaySecondsDelta,
    Unit::Seconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 10.0)),
    Some(boundary(Comparison::Above, 50.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_DISK_BLOCKS_READ_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::OsDiskBlocksReadPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    None,
    ZeroDisposition::Inactive,
);

pub(super) const OS_NETWORK_ERRORS_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::OsNetworkErrorsPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::Above, 10.0)),
    ZeroDisposition::Inactive,
);

pub(super) const OS_NETWORK_DROPS_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::OsNetworkDropsPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::Above, 10.0)),
    ZeroDisposition::Inactive,
);
