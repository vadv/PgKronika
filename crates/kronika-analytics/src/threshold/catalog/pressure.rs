//! Linux pressure-stall policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, scalar_entry,
};

pub(super) const OS_PSI_CPU_SOME_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsPsiCpuSomePercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 5.0)),
    Some(boundary(Comparison::AtLeast, 25.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_PSI_MEMORY_SOME_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsPsiMemorySomePercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 5.0)),
    Some(boundary(Comparison::AtLeast, 25.0)),
    ZeroDisposition::Classify,
);

pub(super) const OS_PSI_IO_SOME_PERCENT: CatalogEntry = scalar_entry(
    MetricId::OsPsiIoSomePercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 10.0)),
    Some(boundary(Comparison::AtLeast, 40.0)),
    ZeroDisposition::Classify,
);
