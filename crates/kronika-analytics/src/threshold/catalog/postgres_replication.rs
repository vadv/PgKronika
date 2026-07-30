//! `PostgreSQL` replication and recovery-conflict policies.

use super::{
    CatalogEntry, Comparison, Direction, MetricId, Unit, ZeroDisposition, boundary, scalar_entry,
};

pub(super) const PG_REPLICATION_REPLAY_LAG_SECONDS: CatalogEntry = scalar_entry(
    MetricId::PgReplicationReplayLagSeconds,
    Unit::Seconds,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 10.0)),
    Some(boundary(Comparison::Above, 60.0)),
    ZeroDisposition::Classify,
);

pub(super) const PG_DATABASE_RECOVERY_CONFLICTS_DELTA: CatalogEntry = scalar_entry(
    MetricId::PgDatabaseRecoveryConflictsDelta,
    Unit::Count,
    Direction::HigherIsWorse,
    Some(boundary(Comparison::Above, 0.0)),
    None,
    ZeroDisposition::Inactive,
);
