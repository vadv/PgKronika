//! Built-in metric identities and provisional absolute-threshold policies.

mod cgroup;
mod cpu;
mod memory;
mod postgres_activity;
mod postgres_io;
mod postgres_replication;
mod postgres_statements;
mod postgres_tables;
mod pressure;
mod storage;

use super::{
    AgePolicy, Boundary, Classified, Comparison, Direction, FractionPolicy, FreeCapacityPolicy,
    MetricInput, Policy, RatioWithFloorPolicy, ScalarPolicy, WarningLimitPolicy, ZeroDisposition,
};

/// Stable identity of one built-in absolute-threshold metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MetricId {
    /// Total CPU percentage used by the `PostgreSQL` process tree.
    OsProcessCpuPercent,
    /// One-minute load average divided by logical CPU count.
    OsLoadAvg1PerCore,
    /// Host idle CPU percentage.
    OsCpuIdlePercent,
    /// Host I/O-wait CPU percentage.
    OsCpuIoWaitPercent,
    /// Host stolen CPU percentage.
    OsCpuStealPercent,
    /// Processes blocked on I/O.
    OsLoadProcsBlocked,
    /// `PostgreSQL` backend count divided by logical CPU count.
    PgActivityBackendLoadPerCore,
    /// Host memory used percentage.
    OsMemoryUsedPercent,
    /// `PostgreSQL` virtual-memory growth in kibibytes.
    OsProcessVirtualGrowthKib,
    /// `PostgreSQL` resident-memory growth in kibibytes.
    OsProcessResidentGrowthKib,
    /// `PostgreSQL` virtual swap size in kibibytes.
    OsProcessVirtualSwapKib,
    /// Host swap used in kibibytes.
    OsMemorySwapUsedKib,
    /// Host swap-in operations per second.
    OsVmstatSwapInPerSecond,
    /// Host swap-out operations per second.
    OsVmstatSwapOutPerSecond,
    /// `PostgreSQL` major page-fault count delta.
    OsProcessMajorFaultsDelta,
    /// `PostgreSQL` resident-set size in kibibytes.
    OsProcessRssKib,
    /// CPU `some` pressure-stall percentage.
    OsPsiCpuSomePercent,
    /// Memory `some` pressure-stall percentage.
    OsPsiMemorySomePercent,
    /// I/O `some` pressure-stall percentage.
    OsPsiIoSomePercent,
    /// cgroup CPU quota used percentage.
    OsCgroupCpuUsedPercent,
    /// cgroup throttled CPU time delta in milliseconds.
    OsCgroupCpuThrottledMillisecondsDelta,
    /// cgroup CPU throttle-event count delta.
    OsCgroupCpuThrottleEventsDelta,
    /// cgroup anonymous-memory percentage.
    OsCgroupMemoryAnonPercent,
    /// cgroup memory-headroom percentage.
    OsCgroupMemoryHeadroomPercent,
    /// cgroup out-of-memory kill count delta.
    OsCgroupMemoryOomKillsDelta,
    /// Maximum disk utilization percentage.
    OsDiskUtilPercent,
    /// Maximum disk request latency in milliseconds.
    OsDiskMaxAwaitMilliseconds,
    /// Disk read request latency in milliseconds.
    OsDiskReadAwaitMilliseconds,
    /// Disk write request latency in milliseconds.
    OsDiskWriteAwaitMilliseconds,
    /// Filesystem capacity available by relative and absolute amount.
    OsFilesystemFreeCapacity,
    /// Process block-I/O delay delta in seconds.
    OsProcessBlockDelaySecondsDelta,
    /// Disk blocks read per second.
    OsDiskBlocksReadPerSecond,
    /// Network interface errors per second.
    OsNetworkErrorsPerSecond,
    /// Network interface drops per second.
    OsNetworkDropsPerSecond,
    /// Dead-tuple fraction gated by the absolute dead-tuple count.
    PgTablesDeadTuplePercent,
    /// Dead-tuple count.
    PgTablesDeadTuples,
    /// Sequential scan percentage.
    PgTablesSequentialScanPercent,
    /// Tuples modified since the last analyze.
    PgTablesModifiedSinceAnalyze,
    /// Tuples inserted since the last vacuum.
    PgTablesInsertedSinceVacuum,
    /// Applicable table age since the last autovacuum.
    PgTablesAutovacuumAgeSeconds,
    /// Applicable table age since the last autoanalyze.
    PgTablesAutoanalyzeAgeSeconds,
    /// Temporary bytes written per second.
    PgTablesTempBytesPerSecond,
    /// Duration of an applicable non-idle query.
    PgActivityQueryDurationSeconds,
    /// Duration of a transaction.
    PgActivityTransactionDurationSeconds,
    /// Client backend count divided by `max_connections`.
    PgActivityClientBackendCapacity,
    /// Sessions currently idle in a transaction.
    PgActivityIdleInTransactionSessions,
    /// Sessions waiting on another session.
    PgActivityBlockedSessions,
    /// Queries exceeding the adapter's long-query definition.
    PgActivityLongQueries,
    /// Transactions exceeding the adapter's long-transaction definition.
    PgActivityLongTransactions,
    /// Rolled-back transactions as a percentage of completed transactions.
    PgDatabaseRollbackPercent,
    /// Reset-aware deadlock count delta.
    PgDatabaseDeadlocksDelta,
    /// Database buffer-cache hit percentage.
    PgDatabaseCacheHitPercent,
    /// Database I/O cache hit percentage.
    PgDatabaseIoCacheHitPercent,
    /// Effective database cache hit percentage.
    PgDatabaseEffectiveCacheHitPercent,
    /// Completed checkpoints per minute.
    PgCheckpointerCheckpointsPerMinute,
    /// Reset-aware checkpoint write-time delta in milliseconds.
    PgCheckpointerWriteTimeMillisecondsDelta,
    /// Backend-written buffers per second.
    PgBgwriterBuffersBackendPerSecond,
    /// Reset-aware `maxwritten_clean` count delta.
    PgBgwriterMaxwrittenCleanDelta,
    /// Client backend evictions per second.
    PgBgwriterClientEvictionsPerSecond,
    /// Statement execution time per returned row.
    PgStatementsMillisecondsPerRow,
    /// Mean statement execution time.
    PgStatementsMeanTimeMilliseconds,
    /// Percentage of statement execution time.
    PgStatementsTimePercent,
    /// Planning time as a percentage of total statement time.
    PgStatementsPlanTimePercent,
    /// Distinct plan count for one statement.
    PgStatementsPlanCount,
    /// Physical replication replay lag.
    PgReplicationReplayLagSeconds,
    /// Reset-aware recovery conflict count delta.
    PgDatabaseRecoveryConflictsDelta,
    /// Dead tuple count has crossed its effective vacuum threshold.
    PgTablesVacuumThresholdExceeded,
    /// Modified tuple count has crossed its effective analyze threshold.
    PgTablesAnalyzeThresholdExceeded,
    /// Inserted tuple count has crossed its effective insert-vacuum threshold.
    PgTablesInsertVacuumThresholdExceeded,
}

impl MetricId {
    /// Every built-in metric in canonical catalog order.
    pub const ALL: [Self; 69] = [
        Self::OsProcessCpuPercent,
        Self::OsLoadAvg1PerCore,
        Self::OsCpuIdlePercent,
        Self::OsCpuIoWaitPercent,
        Self::OsCpuStealPercent,
        Self::OsLoadProcsBlocked,
        Self::PgActivityBackendLoadPerCore,
        Self::OsMemoryUsedPercent,
        Self::OsProcessVirtualGrowthKib,
        Self::OsProcessResidentGrowthKib,
        Self::OsProcessVirtualSwapKib,
        Self::OsMemorySwapUsedKib,
        Self::OsVmstatSwapInPerSecond,
        Self::OsVmstatSwapOutPerSecond,
        Self::OsProcessMajorFaultsDelta,
        Self::OsProcessRssKib,
        Self::OsPsiCpuSomePercent,
        Self::OsPsiMemorySomePercent,
        Self::OsPsiIoSomePercent,
        Self::OsCgroupCpuUsedPercent,
        Self::OsCgroupCpuThrottledMillisecondsDelta,
        Self::OsCgroupCpuThrottleEventsDelta,
        Self::OsCgroupMemoryAnonPercent,
        Self::OsCgroupMemoryHeadroomPercent,
        Self::OsCgroupMemoryOomKillsDelta,
        Self::OsDiskUtilPercent,
        Self::OsDiskMaxAwaitMilliseconds,
        Self::OsDiskReadAwaitMilliseconds,
        Self::OsDiskWriteAwaitMilliseconds,
        Self::OsFilesystemFreeCapacity,
        Self::OsProcessBlockDelaySecondsDelta,
        Self::OsDiskBlocksReadPerSecond,
        Self::OsNetworkErrorsPerSecond,
        Self::OsNetworkDropsPerSecond,
        Self::PgTablesDeadTuplePercent,
        Self::PgTablesDeadTuples,
        Self::PgTablesSequentialScanPercent,
        Self::PgTablesModifiedSinceAnalyze,
        Self::PgTablesInsertedSinceVacuum,
        Self::PgTablesAutovacuumAgeSeconds,
        Self::PgTablesAutoanalyzeAgeSeconds,
        Self::PgTablesTempBytesPerSecond,
        Self::PgActivityQueryDurationSeconds,
        Self::PgActivityTransactionDurationSeconds,
        Self::PgActivityClientBackendCapacity,
        Self::PgActivityIdleInTransactionSessions,
        Self::PgActivityBlockedSessions,
        Self::PgActivityLongQueries,
        Self::PgActivityLongTransactions,
        Self::PgDatabaseRollbackPercent,
        Self::PgDatabaseDeadlocksDelta,
        Self::PgDatabaseCacheHitPercent,
        Self::PgDatabaseIoCacheHitPercent,
        Self::PgDatabaseEffectiveCacheHitPercent,
        Self::PgCheckpointerCheckpointsPerMinute,
        Self::PgCheckpointerWriteTimeMillisecondsDelta,
        Self::PgBgwriterBuffersBackendPerSecond,
        Self::PgBgwriterMaxwrittenCleanDelta,
        Self::PgBgwriterClientEvictionsPerSecond,
        Self::PgStatementsMillisecondsPerRow,
        Self::PgStatementsMeanTimeMilliseconds,
        Self::PgStatementsTimePercent,
        Self::PgStatementsPlanTimePercent,
        Self::PgStatementsPlanCount,
        Self::PgReplicationReplayLagSeconds,
        Self::PgDatabaseRecoveryConflictsDelta,
        Self::PgTablesVacuumThresholdExceeded,
        Self::PgTablesAnalyzeThresholdExceeded,
        Self::PgTablesInsertVacuumThresholdExceeded,
    ];

    /// Stable diagnostic and future adapter code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OsProcessCpuPercent => "os.process.cpu_pct",
            Self::OsLoadAvg1PerCore => "os.load.avg1_per_core",
            Self::OsCpuIdlePercent => "os.cpu.idle_pct",
            Self::OsCpuIoWaitPercent => "os.cpu.iowait_pct",
            Self::OsCpuStealPercent => "os.cpu.steal_pct",
            Self::OsLoadProcsBlocked => "os.load.procs_blocked",
            Self::PgActivityBackendLoadPerCore => "pg.activity.backend_load_per_core",
            Self::OsMemoryUsedPercent => "os.memory.used_pct",
            Self::OsProcessVirtualGrowthKib => "os.process.virtual_growth_kib",
            Self::OsProcessResidentGrowthKib => "os.process.resident_growth_kib",
            Self::OsProcessVirtualSwapKib => "os.process.virtual_swap_kib",
            Self::OsMemorySwapUsedKib => "os.memory.swap_used_kib",
            Self::OsVmstatSwapInPerSecond => "os.vmstat.swap_in_per_second",
            Self::OsVmstatSwapOutPerSecond => "os.vmstat.swap_out_per_second",
            Self::OsProcessMajorFaultsDelta => "os.process.major_faults_delta",
            Self::OsProcessRssKib => "os.process.rss_kib",
            Self::OsPsiCpuSomePercent => "os.psi.cpu_some_pct",
            Self::OsPsiMemorySomePercent => "os.psi.memory_some_pct",
            Self::OsPsiIoSomePercent => "os.psi.io_some_pct",
            Self::OsCgroupCpuUsedPercent => "os.cgroup.cpu_used_pct",
            Self::OsCgroupCpuThrottledMillisecondsDelta => "os.cgroup.cpu_throttled_ms_delta",
            Self::OsCgroupCpuThrottleEventsDelta => "os.cgroup.cpu_throttle_events_delta",
            Self::OsCgroupMemoryAnonPercent => "os.cgroup.memory_anon_pct",
            Self::OsCgroupMemoryHeadroomPercent => "os.cgroup.memory_headroom_pct",
            Self::OsCgroupMemoryOomKillsDelta => "os.cgroup.memory_oom_kills_delta",
            Self::OsDiskUtilPercent => "os.disk.util_pct",
            Self::OsDiskMaxAwaitMilliseconds => "os.disk.max_await_ms",
            Self::OsDiskReadAwaitMilliseconds => "os.disk.read_await_ms",
            Self::OsDiskWriteAwaitMilliseconds => "os.disk.write_await_ms",
            Self::OsFilesystemFreeCapacity => "os.filesystem.free_capacity",
            Self::OsProcessBlockDelaySecondsDelta => "os.process.block_delay_seconds_delta",
            Self::OsDiskBlocksReadPerSecond => "os.disk.blocks_read_per_second",
            Self::OsNetworkErrorsPerSecond => "os.network.errors_per_second",
            Self::OsNetworkDropsPerSecond => "os.network.drops_per_second",
            Self::PgTablesDeadTuplePercent => "pg.tables.dead_tuple_pct",
            Self::PgTablesDeadTuples => "pg.tables.dead_tuples",
            Self::PgTablesSequentialScanPercent => "pg.tables.sequential_scan_pct",
            Self::PgTablesModifiedSinceAnalyze => "pg.tables.modified_since_analyze",
            Self::PgTablesInsertedSinceVacuum => "pg.tables.inserted_since_vacuum",
            Self::PgTablesAutovacuumAgeSeconds => "pg.tables.autovacuum_age_seconds",
            Self::PgTablesAutoanalyzeAgeSeconds => "pg.tables.autoanalyze_age_seconds",
            Self::PgTablesTempBytesPerSecond => "pg.tables.temp_bytes_per_second",
            Self::PgActivityQueryDurationSeconds => "pg.activity.query_duration_seconds",
            Self::PgActivityTransactionDurationSeconds => {
                "pg.activity.transaction_duration_seconds"
            }
            Self::PgActivityClientBackendCapacity => "pg.activity.client_backend_capacity",
            Self::PgActivityIdleInTransactionSessions => "pg.activity.idle_in_transaction_sessions",
            Self::PgActivityBlockedSessions => "pg.activity.blocked_sessions",
            Self::PgActivityLongQueries => "pg.activity.long_queries",
            Self::PgActivityLongTransactions => "pg.activity.long_transactions",
            Self::PgDatabaseRollbackPercent => "pg.database.rollback_pct",
            Self::PgDatabaseDeadlocksDelta => "pg.database.deadlocks_delta",
            Self::PgDatabaseCacheHitPercent => "pg.database.cache_hit_pct",
            Self::PgDatabaseIoCacheHitPercent => "pg.database.io_cache_hit_pct",
            Self::PgDatabaseEffectiveCacheHitPercent => "pg.database.effective_cache_hit_pct",
            Self::PgCheckpointerCheckpointsPerMinute => "pg.checkpointer.checkpoints_per_minute",
            Self::PgCheckpointerWriteTimeMillisecondsDelta => "pg.checkpointer.write_time_ms_delta",
            Self::PgBgwriterBuffersBackendPerSecond => "pg.bgwriter.buffers_backend_per_second",
            Self::PgBgwriterMaxwrittenCleanDelta => "pg.bgwriter.maxwritten_clean_delta",
            Self::PgBgwriterClientEvictionsPerSecond => "pg.bgwriter.client_evictions_per_second",
            Self::PgStatementsMillisecondsPerRow => "pg.statements.milliseconds_per_row",
            Self::PgStatementsMeanTimeMilliseconds => "pg.statements.mean_time_ms",
            Self::PgStatementsTimePercent => "pg.statements.time_pct",
            Self::PgStatementsPlanTimePercent => "pg.statements.plan_time_pct",
            Self::PgStatementsPlanCount => "pg.statements.plan_count",
            Self::PgReplicationReplayLagSeconds => "pg.replication.replay_lag_seconds",
            Self::PgDatabaseRecoveryConflictsDelta => "pg.database.recovery_conflicts_delta",
            Self::PgTablesVacuumThresholdExceeded => "pg.tables.vacuum_threshold_exceeded",
            Self::PgTablesAnalyzeThresholdExceeded => "pg.tables.analyze_threshold_exceeded",
            Self::PgTablesInsertVacuumThresholdExceeded => {
                "pg.tables.insert_vacuum_threshold_exceeded"
            }
        }
    }
}

/// Unit of the value compared with a policy's warning and critical boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Percentage represented on a `0..=100` scale.
    Percent,
    /// Unitless fraction where `1.0` means 100 percent.
    Ratio,
    /// Absolute event or object count.
    Count,
    /// Binary kibibytes.
    Kibibytes,
    /// Milliseconds.
    Milliseconds,
    /// Seconds.
    Seconds,
    /// Count per second.
    CountPerSecond,
    /// Count per minute.
    CountPerMinute,
    /// Bytes per second.
    BytesPerSecond,
    /// Bytes.
    Bytes,
}

/// Operational maturity of built-in threshold values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Calibration {
    /// Starting value that still requires validation against production data.
    Provisional,
    /// Value confirmed against representative production data.
    Validated,
}

/// One built-in metric and its classification metadata.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CatalogEntry {
    /// Stable metric identity.
    pub id: MetricId,
    /// Validated input shape, zero behavior, and exact boundaries.
    pub policy: Policy,
    /// Unit of the classified scalar or derived value.
    pub unit: Unit,
    /// Operational maturity of the threshold values.
    pub calibration: Calibration,
}

const CATALOG: [CatalogEntry; 69] = [
    cpu::OS_PROCESS_CPU_PERCENT,
    cpu::OS_LOAD_AVG1_PER_CORE,
    cpu::OS_CPU_IDLE_PERCENT,
    cpu::OS_CPU_IOWAIT_PERCENT,
    cpu::OS_CPU_STEAL_PERCENT,
    cpu::OS_LOAD_PROCS_BLOCKED,
    cpu::PG_ACTIVITY_BACKEND_LOAD_PER_CORE,
    memory::OS_MEMORY_USED_PERCENT,
    memory::OS_PROCESS_VIRTUAL_GROWTH_KIB,
    memory::OS_PROCESS_RESIDENT_GROWTH_KIB,
    memory::OS_PROCESS_VIRTUAL_SWAP_KIB,
    memory::OS_MEMORY_SWAP_USED_KIB,
    memory::OS_VMSTAT_SWAP_IN_PER_SECOND,
    memory::OS_VMSTAT_SWAP_OUT_PER_SECOND,
    memory::OS_PROCESS_MAJOR_FAULTS_DELTA,
    memory::OS_PROCESS_RSS_KIB,
    pressure::OS_PSI_CPU_SOME_PERCENT,
    pressure::OS_PSI_MEMORY_SOME_PERCENT,
    pressure::OS_PSI_IO_SOME_PERCENT,
    cgroup::OS_CGROUP_CPU_USED_PERCENT,
    cgroup::OS_CGROUP_CPU_THROTTLED_MILLISECONDS_DELTA,
    cgroup::OS_CGROUP_CPU_THROTTLE_EVENTS_DELTA,
    cgroup::OS_CGROUP_MEMORY_ANON_PERCENT,
    cgroup::OS_CGROUP_MEMORY_HEADROOM_PERCENT,
    cgroup::OS_CGROUP_MEMORY_OOM_KILLS_DELTA,
    storage::OS_DISK_UTIL_PERCENT,
    storage::OS_DISK_MAX_AWAIT_MILLISECONDS,
    storage::OS_DISK_READ_AWAIT_MILLISECONDS,
    storage::OS_DISK_WRITE_AWAIT_MILLISECONDS,
    storage::OS_FILESYSTEM_FREE_CAPACITY,
    storage::OS_PROCESS_BLOCK_DELAY_SECONDS_DELTA,
    storage::OS_DISK_BLOCKS_READ_PER_SECOND,
    storage::OS_NETWORK_ERRORS_PER_SECOND,
    storage::OS_NETWORK_DROPS_PER_SECOND,
    postgres_tables::PG_TABLES_DEAD_TUPLE_PERCENT,
    postgres_tables::PG_TABLES_DEAD_TUPLES,
    postgres_tables::PG_TABLES_SEQUENTIAL_SCAN_PERCENT,
    postgres_tables::PG_TABLES_MODIFIED_SINCE_ANALYZE,
    postgres_tables::PG_TABLES_INSERTED_SINCE_VACUUM,
    postgres_tables::PG_TABLES_AUTOVACUUM_AGE_SECONDS,
    postgres_tables::PG_TABLES_AUTOANALYZE_AGE_SECONDS,
    postgres_tables::PG_TABLES_TEMP_BYTES_PER_SECOND,
    postgres_activity::PG_ACTIVITY_QUERY_DURATION_SECONDS,
    postgres_activity::PG_ACTIVITY_TRANSACTION_DURATION_SECONDS,
    postgres_activity::PG_ACTIVITY_CLIENT_BACKEND_CAPACITY,
    postgres_activity::PG_ACTIVITY_IDLE_IN_TRANSACTION_SESSIONS,
    postgres_activity::PG_ACTIVITY_BLOCKED_SESSIONS,
    postgres_activity::PG_ACTIVITY_LONG_QUERIES,
    postgres_activity::PG_ACTIVITY_LONG_TRANSACTIONS,
    postgres_activity::PG_DATABASE_ROLLBACK_PERCENT,
    postgres_activity::PG_DATABASE_DEADLOCKS_DELTA,
    postgres_io::PG_DATABASE_CACHE_HIT_PERCENT,
    postgres_io::PG_DATABASE_IO_CACHE_HIT_PERCENT,
    postgres_io::PG_DATABASE_EFFECTIVE_CACHE_HIT_PERCENT,
    postgres_io::PG_CHECKPOINTER_CHECKPOINTS_PER_MINUTE,
    postgres_io::PG_CHECKPOINTER_WRITE_TIME_MILLISECONDS_DELTA,
    postgres_io::PG_BGWRITER_BUFFERS_BACKEND_PER_SECOND,
    postgres_io::PG_BGWRITER_MAXWRITTEN_CLEAN_DELTA,
    postgres_io::PG_BGWRITER_CLIENT_EVICTIONS_PER_SECOND,
    postgres_statements::PG_STATEMENTS_MILLISECONDS_PER_ROW,
    postgres_statements::PG_STATEMENTS_MEAN_TIME_MILLISECONDS,
    postgres_statements::PG_STATEMENTS_TIME_PERCENT,
    postgres_statements::PG_STATEMENTS_PLAN_TIME_PERCENT,
    postgres_statements::PG_STATEMENTS_PLAN_COUNT,
    postgres_replication::PG_REPLICATION_REPLAY_LAG_SECONDS,
    postgres_replication::PG_DATABASE_RECOVERY_CONFLICTS_DELTA,
    postgres_tables::PG_TABLES_VACUUM_THRESHOLD_EXCEEDED,
    postgres_tables::PG_TABLES_ANALYZE_THRESHOLD_EXCEEDED,
    postgres_tables::PG_TABLES_INSERT_VACUUM_THRESHOLD_EXCEEDED,
];

/// Canonical built-in threshold catalog.
#[must_use]
pub const fn catalog() -> &'static [CatalogEntry] {
    &CATALOG
}

/// Look up one built-in policy in O(1) time.
#[must_use]
pub const fn catalog_entry(id: MetricId) -> &'static CatalogEntry {
    &CATALOG[id as usize]
}

/// Classify one observation with its built-in policy.
#[must_use]
pub fn classify(id: MetricId, input: MetricInput) -> Classified {
    catalog_entry(id).policy.classify(input)
}

pub(super) const fn boundary(operator: Comparison, value: f64) -> Boundary {
    Boundary { operator, value }
}

pub(super) const fn scalar_entry(
    id: MetricId,
    unit: Unit,
    direction: Direction,
    warning: Option<Boundary>,
    critical: Option<Boundary>,
    zero: ZeroDisposition,
) -> CatalogEntry {
    CatalogEntry {
        id,
        policy: Policy::Scalar(valid_scalar(direction, warning, critical, zero)),
        unit,
        calibration: Calibration::Provisional,
    }
}

pub(super) const fn fraction_entry(
    id: MetricId,
    unit: Unit,
    warning: Boundary,
    critical: Boundary,
) -> CatalogEntry {
    let scalar = valid_scalar(
        Direction::HigherIsWorse,
        Some(warning),
        Some(critical),
        ZeroDisposition::Classify,
    );
    CatalogEntry {
        id,
        policy: Policy::Fraction(FractionPolicy::new(scalar)),
        unit,
        calibration: Calibration::Provisional,
    }
}

pub(super) const fn warning_limit_entry(
    id: MetricId,
    unit: Unit,
    operator: Comparison,
    zero: ZeroDisposition,
) -> CatalogEntry {
    CatalogEntry {
        id,
        policy: Policy::WarningLimit(valid_warning_limit(operator, zero)),
        unit,
        calibration: Calibration::Provisional,
    }
}

pub(super) const fn ratio_with_floor_entry(
    id: MetricId,
    unit: Unit,
    warning: Boundary,
    critical: Boundary,
    floor: Boundary,
) -> CatalogEntry {
    let ratio = valid_scalar(
        Direction::HigherIsWorse,
        Some(warning),
        Some(critical),
        ZeroDisposition::Classify,
    );
    CatalogEntry {
        id,
        policy: Policy::RatioWithFloor(valid_ratio_with_floor(ratio, floor)),
        unit,
        calibration: Calibration::Provisional,
    }
}

pub(super) const fn age_entry(id: MetricId, warning: Boundary, critical: Boundary) -> CatalogEntry {
    let age = valid_scalar(
        Direction::HigherIsWorse,
        Some(warning),
        Some(critical),
        ZeroDisposition::Classify,
    );
    CatalogEntry {
        id,
        policy: Policy::AgeGated(valid_age(age)),
        unit: Unit::Seconds,
        calibration: Calibration::Provisional,
    }
}

pub(super) const fn free_capacity_entry(
    id: MetricId,
    warning: Boundary,
    critical: Boundary,
    absolute_ceiling_bytes: Boundary,
) -> CatalogEntry {
    let available_fraction = valid_scalar(
        Direction::LowerIsWorse,
        Some(warning),
        Some(critical),
        ZeroDisposition::Classify,
    );
    CatalogEntry {
        id,
        policy: Policy::FreeCapacity(valid_free_capacity(
            available_fraction,
            absolute_ceiling_bytes,
        )),
        unit: Unit::Bytes,
        calibration: Calibration::Provisional,
    }
}

#[expect(
    clippy::panic,
    reason = "an invalid built-in policy must fail constant evaluation"
)]
const fn valid_scalar(
    direction: Direction,
    warning: Option<Boundary>,
    critical: Option<Boundary>,
    zero: ZeroDisposition,
) -> ScalarPolicy {
    match ScalarPolicy::new(direction, warning, critical, zero) {
        Ok(policy) => policy,
        Err(_) => panic!("invalid built-in scalar policy"),
    }
}

#[expect(
    clippy::panic,
    reason = "an invalid built-in policy must fail constant evaluation"
)]
const fn valid_warning_limit(operator: Comparison, zero: ZeroDisposition) -> WarningLimitPolicy {
    match WarningLimitPolicy::new(operator, zero) {
        Ok(policy) => policy,
        Err(_) => panic!("invalid built-in warning-limit policy"),
    }
}

#[expect(
    clippy::panic,
    reason = "an invalid built-in policy must fail constant evaluation"
)]
const fn valid_ratio_with_floor(ratio: ScalarPolicy, floor: Boundary) -> RatioWithFloorPolicy {
    match RatioWithFloorPolicy::new(ratio, floor) {
        Ok(policy) => policy,
        Err(_) => panic!("invalid built-in ratio floor"),
    }
}

#[expect(
    clippy::panic,
    reason = "an invalid built-in policy must fail constant evaluation"
)]
const fn valid_age(age: ScalarPolicy) -> AgePolicy {
    match AgePolicy::new(age) {
        Ok(policy) => policy,
        Err(_) => panic!("invalid built-in age policy"),
    }
}

#[expect(
    clippy::panic,
    reason = "an invalid built-in policy must fail constant evaluation"
)]
const fn valid_free_capacity(
    available_fraction: ScalarPolicy,
    absolute_ceiling_bytes: Boundary,
) -> FreeCapacityPolicy {
    match FreeCapacityPolicy::new(available_fraction, absolute_ceiling_bytes) {
        Ok(policy) => policy,
        Err(_) => panic!("invalid built-in free-capacity policy"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Calibration, MetricId, catalog, catalog_entry};

    #[test]
    fn enum_order_indexes_the_canonical_catalog() {
        assert_eq!(catalog().len(), MetricId::ALL.len());
        for (index, id) in MetricId::ALL.iter().copied().enumerate() {
            assert_eq!(catalog()[index].id, id);
            assert_eq!(catalog_entry(id), &catalog()[index]);
            assert_eq!(catalog()[index].calibration, Calibration::Provisional);
        }
    }
}
