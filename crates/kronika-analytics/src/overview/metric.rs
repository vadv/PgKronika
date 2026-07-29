//! Stable metric-series metadata for canonical overview samples.
//!
//! The PGM registry describes physical columns. This module gives the subset
//! admitted to overview facts explicit factor IDs, units, reset families and
//! content-qualified series/entity identities. Unsupported columns remain
//! explicit coverage gaps instead of being guessed from their names.

use super::fact::{EntityKind, EntityRef};
use super::health::FactorId;
use super::reduce::{AlignmentId, MetricSeriesId};
use super::sha256;

const METRIC_SERIES_DOMAIN_TAG: &[u8] = b"pgk-overview-metric-series-v2";
const METRIC_ENTITY_DOMAIN_TAG: &[u8] = b"pgk-overview-metric-entity-v2";
const METRIC_ALIGNMENT_DOMAIN_TAG: &[u8] = b"pgk-overview-metric-alignment-v2";

/// Stable overview factor inventory whose source mappings are versioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricFactor {
    /// `pg_stat_database.deadlocks`.
    PgDatabaseDeadlocks,
    /// `pg_stat_database.conflicts`.
    PgDatabaseRecoveryConflicts,
    /// `pg_stat_database.checksum_failures`.
    PgDatabaseChecksumFailures,
    /// `pg_stat_database.sessions_abandoned`.
    PgDatabaseSessionsAbandoned,
    /// `pg_stat_database.sessions_fatal`.
    PgDatabaseSessionsFatal,
    /// `pg_stat_database.sessions_killed`.
    PgDatabaseSessionsKilled,
    /// Explicit `pg_stat_database` reset timestamp.
    PgStatisticsResetAt,
    /// Explicit `PostgreSQL` postmaster start timestamp.
    PgPostmasterStartTime,
    /// Current per-database connection count.
    PgDatabaseConnections,
    /// Per-database connection limit.
    PgDatabaseConnectionLimit,
    /// Age of `datfrozenxid`, transactions.
    PgDatabaseFrozenXidAge,
    /// Age of `datminmxid`, multixacts.
    PgDatabaseMinMxidAge,
    /// `PostgreSQL` recovery role.
    PgRecoveryRole,
    /// `PostgreSQL` timeline ID.
    PgTimeline,
    /// Physical replication sender state.
    PgReplicationSenderState,
    /// Physical replication replay lag.
    PgReplicationReplayLag,
    /// Replication slot state.
    PgReplicationSlotState,
    /// Complete physical-sender snapshot population boundary.
    PgReplicationSenderSnapshotPopulation,
    /// Complete replication-slot snapshot population boundary.
    PgReplicationSlotSnapshotPopulation,
    /// Filesystem total bytes.
    PgFilesystemTotalBytes,
    /// Filesystem available bytes.
    PgFilesystemAvailableBytes,
    /// Cgroup current memory bytes.
    OsCgroupMemoryCurrentBytes,
    /// Cgroup configured memory limit.
    OsCgroupMemoryMaxBytes,
    /// Cgroup memory.high events.
    OsCgroupMemoryHighEvents,
    /// Cgroup memory.max events.
    OsCgroupMemoryMaxEvents,
    /// Cgroup OOM events.
    OsCgroupOomEvents,
    /// Cgroup OOM kills.
    OsCgroupOomKills,
    /// Host OOM kills.
    OsHostOomKills,
    /// CPU pressure source family pending owner-approved policy mapping.
    CpuPressureUnsupported,
    /// PSI memory source family pending owner-approved policy mapping.
    MemoryPsiUnsupported,
    /// Disk throughput source family pending owner-approved policy mapping.
    StorageThroughputUnsupported,
    /// Blocked-session source family pending complete population mapping.
    BlockedSessionsUnsupported,
}

impl MetricFactor {
    /// Complete stable factor inventory in numeric-ID order.
    pub const ALL: [Self; 32] = [
        Self::PgDatabaseDeadlocks,
        Self::PgDatabaseRecoveryConflicts,
        Self::PgDatabaseChecksumFailures,
        Self::PgDatabaseSessionsAbandoned,
        Self::PgDatabaseSessionsFatal,
        Self::PgDatabaseSessionsKilled,
        Self::PgStatisticsResetAt,
        Self::PgPostmasterStartTime,
        Self::PgDatabaseConnections,
        Self::PgDatabaseConnectionLimit,
        Self::PgDatabaseFrozenXidAge,
        Self::PgDatabaseMinMxidAge,
        Self::PgRecoveryRole,
        Self::PgTimeline,
        Self::PgReplicationSenderState,
        Self::PgReplicationReplayLag,
        Self::PgReplicationSlotState,
        Self::PgReplicationSenderSnapshotPopulation,
        Self::PgReplicationSlotSnapshotPopulation,
        Self::PgFilesystemTotalBytes,
        Self::PgFilesystemAvailableBytes,
        Self::OsCgroupMemoryCurrentBytes,
        Self::OsCgroupMemoryMaxBytes,
        Self::OsCgroupMemoryHighEvents,
        Self::OsCgroupMemoryMaxEvents,
        Self::OsCgroupOomEvents,
        Self::OsCgroupOomKills,
        Self::OsHostOomKills,
        Self::CpuPressureUnsupported,
        Self::MemoryPsiUnsupported,
        Self::StorageThroughputUnsupported,
        Self::BlockedSessionsUnsupported,
    ];

    /// Stable factor ID used in persisted samples and health coverage.
    #[must_use]
    pub const fn id(self) -> FactorId {
        FactorId(match self {
            Self::PgDatabaseDeadlocks => 100,
            Self::PgDatabaseRecoveryConflicts => 101,
            Self::PgDatabaseChecksumFailures => 102,
            Self::PgDatabaseSessionsAbandoned => 103,
            Self::PgDatabaseSessionsFatal => 104,
            Self::PgDatabaseSessionsKilled => 105,
            Self::PgStatisticsResetAt => 106,
            Self::PgPostmasterStartTime => 107,
            Self::PgDatabaseConnections => 110,
            Self::PgDatabaseConnectionLimit => 111,
            Self::PgDatabaseFrozenXidAge => 112,
            Self::PgDatabaseMinMxidAge => 113,
            Self::PgRecoveryRole => 120,
            Self::PgTimeline => 121,
            Self::PgReplicationSenderState => 122,
            Self::PgReplicationReplayLag => 123,
            Self::PgReplicationSlotState => 124,
            Self::PgReplicationSenderSnapshotPopulation => 125,
            Self::PgReplicationSlotSnapshotPopulation => 126,
            Self::PgFilesystemTotalBytes => 130,
            Self::PgFilesystemAvailableBytes => 131,
            Self::OsCgroupMemoryCurrentBytes => 200,
            Self::OsCgroupMemoryMaxBytes => 201,
            Self::OsCgroupMemoryHighEvents => 202,
            Self::OsCgroupMemoryMaxEvents => 203,
            Self::OsCgroupOomEvents => 204,
            Self::OsCgroupOomKills => 205,
            Self::OsHostOomKills => 206,
            Self::CpuPressureUnsupported => 900,
            Self::MemoryPsiUnsupported => 901,
            Self::StorageThroughputUnsupported => 902,
            Self::BlockedSessionsUnsupported => 903,
        })
    }

    /// Finds an inventory item by stable factor ID.
    #[must_use]
    pub const fn from_id(id: FactorId) -> Option<Self> {
        match id.0 {
            100 => Some(Self::PgDatabaseDeadlocks),
            101 => Some(Self::PgDatabaseRecoveryConflicts),
            102 => Some(Self::PgDatabaseChecksumFailures),
            103 => Some(Self::PgDatabaseSessionsAbandoned),
            104 => Some(Self::PgDatabaseSessionsFatal),
            105 => Some(Self::PgDatabaseSessionsKilled),
            106 => Some(Self::PgStatisticsResetAt),
            107 => Some(Self::PgPostmasterStartTime),
            110 => Some(Self::PgDatabaseConnections),
            111 => Some(Self::PgDatabaseConnectionLimit),
            112 => Some(Self::PgDatabaseFrozenXidAge),
            113 => Some(Self::PgDatabaseMinMxidAge),
            120 => Some(Self::PgRecoveryRole),
            121 => Some(Self::PgTimeline),
            122 => Some(Self::PgReplicationSenderState),
            123 => Some(Self::PgReplicationReplayLag),
            124 => Some(Self::PgReplicationSlotState),
            125 => Some(Self::PgReplicationSenderSnapshotPopulation),
            126 => Some(Self::PgReplicationSlotSnapshotPopulation),
            130 => Some(Self::PgFilesystemTotalBytes),
            131 => Some(Self::PgFilesystemAvailableBytes),
            200 => Some(Self::OsCgroupMemoryCurrentBytes),
            201 => Some(Self::OsCgroupMemoryMaxBytes),
            202 => Some(Self::OsCgroupMemoryHighEvents),
            203 => Some(Self::OsCgroupMemoryMaxEvents),
            204 => Some(Self::OsCgroupOomEvents),
            205 => Some(Self::OsCgroupOomKills),
            206 => Some(Self::OsHostOomKills),
            900 => Some(Self::CpuPressureUnsupported),
            901 => Some(Self::MemoryPsiUnsupported),
            902 => Some(Self::StorageThroughputUnsupported),
            903 => Some(Self::BlockedSessionsUnsupported),
            _ => None,
        }
    }

    /// Stable locale-neutral factor code.
    #[must_use]
    pub const fn wire_code(self) -> &'static str {
        match self {
            Self::PgDatabaseDeadlocks => "pg.database.deadlocks",
            Self::PgDatabaseRecoveryConflicts => "pg.database.recovery_conflicts",
            Self::PgDatabaseChecksumFailures => "pg.database.checksum_failures",
            Self::PgDatabaseSessionsAbandoned => "pg.database.sessions_abandoned",
            Self::PgDatabaseSessionsFatal => "pg.database.sessions_fatal",
            Self::PgDatabaseSessionsKilled => "pg.database.sessions_killed",
            Self::PgStatisticsResetAt => "pg.statistics.reset_at_us",
            Self::PgPostmasterStartTime => "pg.postmaster.start_time_us",
            Self::PgDatabaseConnections => "pg.database.connections",
            Self::PgDatabaseConnectionLimit => "pg.database.connection_limit",
            Self::PgDatabaseFrozenXidAge => "pg.database.frozen_xid_age",
            Self::PgDatabaseMinMxidAge => "pg.database.min_mxid_age",
            Self::PgRecoveryRole => "pg.recovery.role",
            Self::PgTimeline => "pg.timeline",
            Self::PgReplicationSenderState => "pg.replication.sender_state",
            Self::PgReplicationReplayLag => "pg.replication.replay_lag_us",
            Self::PgReplicationSlotState => "pg.replication.slot_state",
            Self::PgReplicationSenderSnapshotPopulation => {
                "pg.replication.sender_snapshot_population"
            }
            Self::PgReplicationSlotSnapshotPopulation => "pg.replication.slot_snapshot_population",
            Self::PgFilesystemTotalBytes => "pg.filesystem.total_bytes",
            Self::PgFilesystemAvailableBytes => "pg.filesystem.available_bytes",
            Self::OsCgroupMemoryCurrentBytes => "os.cgroup.memory.current_bytes",
            Self::OsCgroupMemoryMaxBytes => "os.cgroup.memory.max_bytes",
            Self::OsCgroupMemoryHighEvents => "os.cgroup.memory.high_events",
            Self::OsCgroupMemoryMaxEvents => "os.cgroup.memory.max_events",
            Self::OsCgroupOomEvents => "os.cgroup.memory.oom_events",
            Self::OsCgroupOomKills => "os.cgroup.memory.oom_kills",
            Self::OsHostOomKills => "os.host.oom_kills",
            Self::CpuPressureUnsupported => "cpu.pressure.unsupported",
            Self::MemoryPsiUnsupported => "memory.psi.unsupported",
            Self::StorageThroughputUnsupported => "storage.throughput.unsupported",
            Self::BlockedSessionsUnsupported => "pg.blocked_sessions.unsupported",
        }
    }
}

/// Unit of a canonical metric series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricUnit {
    /// Discrete occurrences.
    Count,
    /// Bytes.
    Bytes,
    /// Microseconds.
    Microseconds,
    /// Milliseconds.
    Milliseconds,
    /// Seconds.
    Seconds,
    /// Current connections.
    Connections,
    /// Transaction identifiers.
    Transactions,
    /// Multitransaction identifiers.
    Multixacts,
    /// Closed state discriminant.
    StateCode,
}

impl MetricUnit {
    /// Stable codec discriminant.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Count => 1,
            Self::Bytes => 2,
            Self::Microseconds => 3,
            Self::Milliseconds => 4,
            Self::Seconds => 5,
            Self::Connections => 6,
            Self::Transactions => 7,
            Self::Multixacts => 8,
            Self::StateCode => 9,
        }
    }

    /// Decodes a stable unit discriminant.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Count),
            2 => Some(Self::Bytes),
            3 => Some(Self::Microseconds),
            4 => Some(Self::Milliseconds),
            5 => Some(Self::Seconds),
            6 => Some(Self::Connections),
            7 => Some(Self::Transactions),
            8 => Some(Self::Multixacts),
            9 => Some(Self::StateCode),
            _ => None,
        }
    }
}

/// Reset domain used to classify adjacent cumulative samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResetFamily {
    /// Counter is reset with `pg_stat_database`/postmaster context.
    PgStatDatabase,
    /// Counter is reset by a host boot.
    HostBoot,
    /// Counter is reset by a cgroup/host boot identity change.
    CgroupBoot,
}

impl ResetFamily {
    /// Stable codec discriminant.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::PgStatDatabase => 1,
            Self::HostBoot => 2,
            Self::CgroupBoot => 3,
        }
    }

    /// Decodes a stable reset-family discriminant.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::PgStatDatabase),
            2 => Some(Self::HostBoot),
            3 => Some(Self::CgroupBoot),
            _ => None,
        }
    }
}

/// Canonical metadata shared by samples of one series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricSeriesDescriptor {
    /// Stable series identity.
    pub series_id: MetricSeriesId,
    /// Stable factor identity.
    pub factor_id: FactorId,
    /// Physical PGM section type.
    pub section_type_id: u32,
    /// Proven unit.
    pub unit: MetricUnit,
    /// Stable entity, when one is proven.
    pub entity: Option<EntityRef>,
    /// Reset family for a cumulative series.
    pub reset_family: Option<ResetFamily>,
}

impl MetricSeriesDescriptor {
    /// Builds a descriptor and derives its content-qualified series identity.
    #[must_use]
    pub fn new(
        factor: MetricFactor,
        section_type_id: u32,
        unit: MetricUnit,
        entity: Option<EntityRef>,
        reset_family: Option<ResetFamily>,
        series_discriminator: &[u8],
    ) -> Self {
        let factor_id = factor.id();
        let digest = sha256::digest_parts(&[
            METRIC_SERIES_DOMAIN_TAG,
            &section_type_id.to_le_bytes(),
            &factor_id.0.to_le_bytes(),
            series_discriminator,
        ]);
        let mut series_id = [0_u8; 16];
        series_id.copy_from_slice(&digest[..16]);
        Self {
            series_id: MetricSeriesId(series_id),
            factor_id,
            section_type_id,
            unit,
            entity,
            reset_family,
        }
    }
}

/// Derives a content-qualified entity reference.
#[must_use]
pub fn derive_entity(kind: EntityKind, source_identity: &[u8]) -> EntityRef {
    let digest = sha256::digest_parts(&[METRIC_ENTITY_DOMAIN_TAG, &[kind.code()], source_identity]);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    EntityRef { kind, id }
}

/// Derives alignment shared by compatible samples of one entity.
#[must_use]
pub fn derive_alignment(entity: Option<EntityRef>) -> AlignmentId {
    let entity_kind = entity.map_or(0, |value| value.kind.code());
    let entity_id = entity.map_or([0_u8; 16], |value| value.id);
    let digest = sha256::digest_parts(&[METRIC_ALIGNMENT_DOMAIN_TAG, &[entity_kind], &entity_id]);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    AlignmentId(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_codes_and_units_round_trip() {
        for factor in MetricFactor::ALL {
            assert_eq!(MetricFactor::from_id(factor.id()), Some(factor));
            assert!(!factor.wire_code().is_empty());
        }
        assert!(
            MetricFactor::ALL
                .windows(2)
                .all(|pair| pair[0].id() < pair[1].id()),
            "the qualification inventory must stay in canonical ID order"
        );
        for unit in [MetricUnit::Count, MetricUnit::Bytes, MetricUnit::StateCode] {
            assert_eq!(MetricUnit::from_code(unit.code()), Some(unit));
        }
    }

    #[test]
    fn series_and_entity_identity_use_proven_dimensions() {
        let entity = derive_entity(EntityKind::Database, &42_u32.to_le_bytes());

        let series_a = MetricSeriesDescriptor::new(
            MetricFactor::PgDatabaseDeadlocks,
            1_005_004,
            MetricUnit::Count,
            Some(entity),
            Some(ResetFamily::PgStatDatabase),
            &42_u32.to_le_bytes(),
        );
        let series_b = MetricSeriesDescriptor::new(
            MetricFactor::PgDatabaseDeadlocks,
            1_005_005,
            MetricUnit::Count,
            Some(entity),
            Some(ResetFamily::PgStatDatabase),
            &42_u32.to_le_bytes(),
        );
        assert_ne!(series_a.series_id, series_b.series_id);
        assert_eq!(
            derive_alignment(Some(entity)),
            derive_alignment(Some(entity))
        );
    }
}
