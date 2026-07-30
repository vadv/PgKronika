//! `PostgreSQL` table, vacuum, analyze, and temporary-I/O policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, age_entry, boundary,
    ratio_with_floor_entry, scalar_entry, warning_limit_entry,
};

pub(super) const PG_TABLES_DEAD_TUPLE_PERCENT: CatalogEntry = ratio_with_floor_entry(
    MetricId::PgTablesDeadTuplePercent,
    Unit::Ratio,
    boundary(Comparison::AtLeast, 0.10),
    boundary(Comparison::AtLeast, 0.20),
    boundary(Comparison::Above, 10_000.0),
);

pub(super) const PG_TABLES_DEAD_TUPLES: CatalogEntry = scalar_entry(
    MetricId::PgTablesDeadTuples,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 1_000.0)),
    Some(boundary(Comparison::AtLeast, 100_000.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_TABLES_SEQUENTIAL_SCAN_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgTablesSequentialScanPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 30.0)),
    Some(boundary(Comparison::AtLeast, 80.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_TABLES_MODIFIED_SINCE_ANALYZE: CatalogEntry = scalar_entry(
    MetricId::PgTablesModifiedSinceAnalyze,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 100_000.0)),
    Some(boundary(Comparison::AtLeast, 1_000_000.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_TABLES_INSERTED_SINCE_VACUUM: CatalogEntry = scalar_entry(
    MetricId::PgTablesInsertedSinceVacuum,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 100_000.0)),
    Some(boundary(Comparison::AtLeast, 1_000_000.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_TABLES_AUTOVACUUM_AGE_SECONDS: CatalogEntry = age_entry(
    MetricId::PgTablesAutovacuumAgeSeconds,
    boundary(Comparison::Above, 21_600.0),
    boundary(Comparison::Above, 86_400.0),
);

pub(super) const PG_TABLES_AUTOANALYZE_AGE_SECONDS: CatalogEntry = age_entry(
    MetricId::PgTablesAutoanalyzeAgeSeconds,
    boundary(Comparison::Above, 21_600.0),
    boundary(Comparison::Above, 86_400.0),
);

pub(super) const PG_TABLES_TEMP_BYTES_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::PgTablesTempBytesPerSecond,
    Unit::BytesPerSecond,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    None,
    ZeroDisposition::Inactive,
);

pub(super) const PG_TABLES_VACUUM_THRESHOLD_EXCEEDED: CatalogEntry = warning_limit_entry(
    MetricId::PgTablesVacuumThresholdExceeded,
    Unit::Count,
    Comparison::Above,
    ZeroDisposition::Classify,
);

pub(super) const PG_TABLES_ANALYZE_THRESHOLD_EXCEEDED: CatalogEntry = warning_limit_entry(
    MetricId::PgTablesAnalyzeThresholdExceeded,
    Unit::Count,
    Comparison::Above,
    ZeroDisposition::Classify,
);

pub(super) const PG_TABLES_INSERT_VACUUM_THRESHOLD_EXCEEDED: CatalogEntry = warning_limit_entry(
    MetricId::PgTablesInsertVacuumThresholdExceeded,
    Unit::Count,
    Comparison::Above,
    ZeroDisposition::Classify,
);
