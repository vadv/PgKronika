//! `PostgreSQL` cache, checkpointer, and background-writer policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, scalar_entry,
};

pub(super) const PG_DATABASE_CACHE_HIT_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgDatabaseCacheHitPercent,
    Unit::Percent,
    Direction::LowerIsWorse,
    Some(boundary(Comparison::Below, 99.0)),
    Some(boundary(Comparison::Below, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_DATABASE_IO_CACHE_HIT_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgDatabaseIoCacheHitPercent,
    Unit::Percent,
    Direction::LowerIsWorse,
    Some(boundary(Comparison::Below, 99.0)),
    Some(boundary(Comparison::Below, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_DATABASE_EFFECTIVE_CACHE_HIT_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgDatabaseEffectiveCacheHitPercent,
    Unit::Percent,
    Direction::LowerIsWorse,
    Some(boundary(Comparison::Below, 99.0)),
    Some(boundary(Comparison::Below, 90.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_CHECKPOINTER_CHECKPOINTS_PER_MINUTE: CatalogEntry = scalar_entry(
    MetricId::PgCheckpointerCheckpointsPerMinute,
    Unit::CountPerMinute,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 2.0)),
    None,
    ZeroDisposition::Inactive,
);

pub(super) const PG_CHECKPOINTER_WRITE_TIME_MILLISECONDS_DELTA: CatalogEntry = scalar_entry(
    MetricId::PgCheckpointerWriteTimeMillisecondsDelta,
    Unit::Milliseconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 30_000.0)),
    Some(boundary(Comparison::Above, 120_000.0)),
    ZeroDisposition::Inactive,
);

pub(super) const PG_BGWRITER_BUFFERS_BACKEND_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::PgBgwriterBuffersBackendPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    None,
    ZeroDisposition::Inactive,
);

pub(super) const PG_BGWRITER_MAXWRITTEN_CLEAN_DELTA: CatalogEntry = scalar_entry(
    MetricId::PgBgwriterMaxwrittenCleanDelta,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    None,
    ZeroDisposition::Inactive,
);

pub(super) const PG_BGWRITER_CLIENT_EVICTIONS_PER_SECOND: CatalogEntry = scalar_entry(
    MetricId::PgBgwriterClientEvictionsPerSecond,
    Unit::CountPerSecond,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::AtLeast, 10.0)),
    ZeroDisposition::Inactive,
);
