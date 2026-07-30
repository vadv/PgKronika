//! `PostgreSQL` session, activity, and transaction policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, fraction_entry,
    scalar_entry,
};

pub(super) const PG_ACTIVITY_QUERY_DURATION_SECONDS: CatalogEntry = scalar_entry(
    MetricId::PgActivityQueryDurationSeconds,
    Unit::Seconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 1.0)),
    Some(boundary(Comparison::AtLeast, 30.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_ACTIVITY_TRANSACTION_DURATION_SECONDS: CatalogEntry = scalar_entry(
    MetricId::PgActivityTransactionDurationSeconds,
    Unit::Seconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::AtLeast, 5.0)),
    Some(boundary(Comparison::AtLeast, 60.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_ACTIVITY_CLIENT_BACKEND_CAPACITY: CatalogEntry = fraction_entry(
    MetricId::PgActivityClientBackendCapacity,
    Unit::Ratio,
    boundary(Comparison::AtLeast, 0.70),
    boundary(Comparison::AtLeast, 0.90),
);

pub(super) const PG_ACTIVITY_IDLE_IN_TRANSACTION_SESSIONS: CatalogEntry = scalar_entry(
    MetricId::PgActivityIdleInTransactionSessions,
    Unit::Count,
    Direction::HigherIsWorse,
    None,
    Some(boundary(Comparison::Above, 0.0)),
    ZeroDisposition::Inactive,
);

pub(super) const PG_ACTIVITY_BLOCKED_SESSIONS: CatalogEntry = scalar_entry(
    MetricId::PgActivityBlockedSessions,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::AtLeast, 5.0)),
    ZeroDisposition::Inactive,
);

pub(super) const PG_ACTIVITY_LONG_QUERIES: CatalogEntry = scalar_entry(
    MetricId::PgActivityLongQueries,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::AtLeast, 5.0)),
    ZeroDisposition::Inactive,
);

pub(super) const PG_ACTIVITY_LONG_TRANSACTIONS: CatalogEntry = scalar_entry(
    MetricId::PgActivityLongTransactions,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    Some(boundary(Comparison::AtLeast, 3.0)),
    ZeroDisposition::Inactive,
);

pub(super) const PG_DATABASE_ROLLBACK_PERCENT: CatalogEntry = scalar_entry(
    MetricId::PgDatabaseRollbackPercent,
    Unit::Percent,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 3.0)),
    Some(boundary(Comparison::Above, 10.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_DATABASE_DEADLOCKS_DELTA: CatalogEntry = scalar_entry(
    MetricId::PgDatabaseDeadlocksDelta,
    Unit::Count,
    Direction::HigherIsWorse,
    None,
    Some(boundary(Comparison::Above, 0.0)),
    ZeroDisposition::Inactive,
);
