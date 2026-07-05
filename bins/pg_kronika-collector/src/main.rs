//! Collects `PostgreSQL` stats and writes sealed PGM segments.
//!
//! The daemon runs on the database host. Each collection tick reads the due
//! `PostgreSQL` sources and appends one journal part when rows exist. The open
//! segment seals on SIGUSR2, on the raw journal byte cap, or on its max age.
//! The journal is reset only after a successful seal.
//!
//! Environment:
//! - `KRONIKA_PG_DSN`: libpq connection string (key=value or URI) for the target
//!   server;
//! - `KRONIKA_OUT_DIR`: directory that receives sealed segments;
//! - `KRONIKA_LOG_LEVEL`: collector stderr log level, one of `error`, `warn`,
//!   `info`, `debug`, or `trace` (default `info`; invalid values warn and use
//!   `info`);
//! - `KRONIKA_SOURCE_ID`: `u64` stamped into every sealed segment to identify
//!   the source (default `0`). Sealing refuses to mix two *non-zero* source ids
//!   in one segment, so distinct collectors sharing a `KRONIKA_OUT_DIR` must use
//!   distinct non-zero ids. The default `0` is exempt from that check: two
//!   collectors left at `0` writing to the same directory merge their data
//!   silently. Set a unique non-zero id per source in any multi-collector setup;
//! - `KRONIKA_PG_STATEMENT_TIMEOUT_MS`: statement timeout in ms, default 15000;
//! - `KRONIKA_PG_LOCK_TIMEOUT_MS`: lock timeout in ms, default 1000 (must be
//!   below the statement timeout, else it never fires; validated at startup);
//! - `KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS`: idle-in-transaction timeout in ms,
//!   default 10000;
//! - `KRONIKA_PG_EXCLUDE_DATABASES`: semicolon-separated list of databases to skip;
//! - `KRONIKA_PG_MAX_TABLES`: per-axis top-N row count for the `pg_stat_user_tables`
//!   candidate selection, default 500. Each of the six mechanical axes (read
//!   activity, write volume, size, dead tuples, transaction-id age, multixact age)
//!   contributes up to this many rows before the union;
//! - `KRONIKA_PG_MAX_INDEXES`: per-axis top-N row count for the
//!   `pg_stat_user_indexes` candidate selection, default 500. Each mechanical axis
//!   (scans, tuples read, size; plus scan recency on PG16+) contributes up to this
//!   many rows before the union;
//! - `KRONIKA_PG_MAX_STATEMENTS`: per-axis top-N row count for the
//!   `pg_stat_statements` candidate selection, default 500. Each axis (total
//!   execution time, calls) contributes up to this many rows before the union;
//! - `KRONIKA_PG_MAX_PLANS`: top-N row count by total time for the
//!   `pg_store_plans` (vadv fork) read, default 500;
//! - `KRONIKA_PG_PLANS_INTERVAL_S`: minimum interval between `pg_store_plans`
//!   reads, default 300; `0` reads on every snapshot. Section `1_004_001` is
//!   written only into the segment whose snapshot performed a read;
//! - `KRONIKA_PG_MAX_PLAN_TEXT`: per-plan text truncation in bytes, default
//!   32768;
//! - `KRONIKA_PG_PLAN_TEXT_BUDGET`: total plan-text bytes fetched per read,
//!   default 8388608; rows past the budget are written with a NULL plan;
//! - `KRONIKA_PG_POOL_REFRESH_SECS`: minimum interval between connection-pool
//!   refreshes (per-database connection reconciliation), default 600;
//! - `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS`: cap for the adaptive `statement_timeout`
//!   of the heavy per-table size query, default 60000. A `57014` (query canceled)
//!   widens the timeout and retries the same database until this cap;
//! - `KRONIKA_PG_MAX_LOCK_ROWS`: lock-wait graph guard, default 1000. The
//!   section is skipped when no waits exist or when waiters, edges, or nodes
//!   exceed this value;
//! - `KRONIKA_NODE_SELF_ID`: stable node id sealed into `instance_metadata`
//!   (`1_021_001`); defaults to the hostname;
//! - `KRONIKA_INTERVAL_S`: regular wake cap of the internal timer, default 5;
//!   positive source or trigger intervals can wake sooner; `0` disables the
//!   timer, leaving collection to SIGUSR2 only. A signal is a forced tick: it
//!   reads every source regardless of intervals;
//! - per-source read intervals, seconds (a source is read on the first tick
//!   whose elapsed time since its last read reaches the interval):
//!   `KRONIKA_PG_ACTIVITY_INTERVAL_S` (5), `KRONIKA_PG_DATABASE_INTERVAL_S`
//!   (10), `KRONIKA_PG_BGWRITER_INTERVAL_S` (10), `KRONIKA_PG_WAL_INTERVAL_S`
//!   (10), `KRONIKA_PG_IO_INTERVAL_S` (10), `KRONIKA_PG_ARCHIVER_INTERVAL_S`
//!   (30), `KRONIKA_PG_PREPARED_INTERVAL_S` (30),
//!   `KRONIKA_PG_PROGRESS_VACUUM_INTERVAL_S` (10),
//!   `KRONIKA_PG_STATEMENTS_INTERVAL_S` (30), `KRONIKA_PG_TABLES_INTERVAL_S`
//!   (30), `KRONIKA_PG_INDEXES_INTERVAL_S` (60),
//!   `KRONIKA_PG_REPLICATION_INTERVAL_S` (30),
//!   `KRONIKA_PG_RESET_METADATA_INTERVAL_S` (30),
//!   `KRONIKA_INSTANCE_INTERVAL_S` (60), `KRONIKA_PG_SETTINGS_INTERVAL_S`
//!   (3600), `KRONIKA_OS_CORE_INTERVAL_S` (10),
//!   `KRONIKA_OS_MOUNTTOPO_INTERVAL_S` (60),
//!   `KRONIKA_OS_PROCESS_INTERVAL_S` (5),
//!   `KRONIKA_OS_PROCESS_STATUS_INTERVAL_S` (30),
//!   `KRONIKA_OS_CGROUP_INTERVAL_S` (10),
//!   `KRONIKA_OS_CGROUP_MAPPING_INTERVAL_S` (30),
//!   `KRONIKA_PG_LOG_INTERVAL_S` (5). The lock-wait graph has no
//!   interval: it runs when the freshest activity snapshot shows a backend
//!   waiting on a heavyweight lock. The `pg_store_plans` pace stays on
//!   `KRONIKA_PG_PLANS_INTERVAL_S`;
//! - `PostgreSQL` log source: `KRONIKA_PG_LOG_ENABLED` (default `0`, or enabled
//!   when `KRONIKA_LOG_PATH` is set), `KRONIKA_LOG_PATH` direct stderr fixture
//!   or sidecar path, `KRONIKA_LOG_ROOT` for relative `pg_current_logfile()`
//!   paths, `KRONIKA_LOG_FORMAT` (`stderr` only in this scope; `csvlog` emits
//!   `pg_log_gap`), `KRONIKA_LOG_STATE_PATH`, and
//!   `KRONIKA_LOG_START_AT_BEGINNING` for deterministic fixtures;
//! - OS source caps: `KRONIKA_OS_MAX_DISKS` (256),
//!   `KRONIKA_OS_MAX_PROCS` (4096), `KRONIKA_OS_MAX_CGROUPS` (1024),
//!   `KRONIKA_OS_MAX_CGROUP_IO_ROWS` (4096), `KRONIKA_OS_CGROUP_MAX_DEPTH` (8);
//! - trigger fast intervals and thresholds:
//!   `KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S` (1),
//!   `KRONIKA_PG_ASH_ACTIVE_THRESHOLD` (20),
//!   `KRONIKA_PG_REPLICATION_FAST_INTERVAL_S` (10),
//!   `KRONIKA_PG_REPL_LAG_TRIGGER_S` (10),
//!   `KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES` (1073741824). A fast interval at
//!   or above the source's base interval disables that trigger; `0` means every
//!   timer wake while the trigger stays hot;
//! - `KRONIKA_SEGMENT_MAX_BYTES`: seal the segment once the journal holds
//!   this many raw bytes before segment packing, default 67108864 (64 MiB);
//!   `0` seals on every tick. A SIGUSR2 tick always seals immediately;
//! - `KRONIKA_SEGMENT_MAX_AGE_S`: seal an open segment at this age even when
//!   it stays under the byte cap, default 900 seconds, analogous to
//!   `archive_timeout` for WAL.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

mod budget;
mod config;
mod logging;
mod scheduler;
mod statements_source;

use anyhow::{Context, Result};
use budget::{PoolBudget, PoolSource};
use config::{Config, env_u64, validate_replication_detail_bounds, validate_settings_row_count};
#[cfg(test)]
use config::{
    validate_cardinality, validate_heavy_cap, validate_max_lock_rows, validate_max_plans,
    validate_plan_text_limits,
};
use kronika_format::DictLimits;
use kronika_registry::collection_coverage::CollectionCoverageV1;
use kronika_registry::instance_metadata::InstanceMetadata;
use kronika_registry::os_cgroup_cpu::OsCgroupCpu;
use kronika_registry::os_cgroup_io::OsCgroupIo;
use kronika_registry::os_cgroup_mapping::OsCgroupMapping;
use kronika_registry::os_cgroup_memory::OsCgroupMemory;
use kronika_registry::os_cgroup_pids::OsCgroupPids;
use kronika_registry::os_cpu::OsCpu;
use kronika_registry::os_diskstats::OsDiskstats;
use kronika_registry::os_loadavg::OsLoadavg;
use kronika_registry::os_meminfo::OsMeminfo;
use kronika_registry::os_mountinfo::OsMountinfo;
use kronika_registry::os_netdev::OsNetdev;
use kronika_registry::os_netstat::OsNetstat;
use kronika_registry::os_process::OsProcess;
use kronika_registry::os_process_status::OsProcessStatus;
use kronika_registry::os_psi::OsPsi;
use kronika_registry::os_snmp::OsSnmp;
use kronika_registry::os_stat::OsStat;
use kronika_registry::os_topology::OsTopology;
use kronika_registry::os_vmstat::OsVmstat;
use kronika_registry::pg_log::{PgLogErrorV1, PgLogGapV1};
use kronika_registry::{StrId, Ts};
use kronika_source_log::{
    DiscoveryStatus as LogDiscoveryStatus, GroupedLogError, LogCollection, LogCollector, LogGap,
    MAX_PATTERN_BYTES, MAX_TEXT_BYTES, PG_LOG_ERRORS_TYPE_ID, PG_LOG_GAP_TYPE_ID,
};
use kronika_source_os::proc::cpuinfo;
use kronika_source_os::proc::loadavg::parse_loadavg;
use kronika_source_os::proc::meminfo::parse_meminfo;
use kronika_source_os::proc::pressure::parse_pressure;
use kronika_source_os::proc::process::{ProcessError, process_facts, read_process};
use kronika_source_os::proc::stat::{parse_cpu, parse_stat_misc};
use kronika_source_os::proc::vmstat::parse_vmstat;
use kronika_source_os::proc::{diskstats, net_dev, net_netstat, net_snmp};
use kronika_source_os::{
    MountEntry, OsInstanceFacts, OsScope, ProcFs, SysFs, cgroup, collect_os_instance_facts,
    container_device_set, detect_container, mount_row, net_scope, parse_dev_pair, parse_mountinfo,
    statvfs,
};
use kronika_source_pg::archiver::{ArchiverRow, collect_archiver, to_archiver};
use kronika_source_pg::database::{self, DatabaseRow, DatabaseVersion, collect_database};
use kronika_source_pg::instance_metadata::{
    PgInstanceFacts, collect_pg_instance_facts, pg_system_identifier,
};
use kronika_source_pg::io::{self, IoRow, IoVersion, collect_io};
use kronika_source_pg::locks::{
    LocksRow, LocksVersion, collect_locks, locks_version, to_v1 as locks_to_v1,
    to_v2 as locks_to_v2,
};
use kronika_source_pg::pool::{AdaptiveTimeout, ConnectionPool, DEFAULT_MAX_DATABASES};
use kronika_source_pg::prepared_xacts::{
    PreparedXactsRow, collect_prepared_xacts, to_prepared_xacts,
};
use kronika_source_pg::progress_vacuum::{
    ProgressVacuumRow, collect_progress_vacuum, to_progress_vacuum,
};
use kronika_source_pg::replication_details::{
    ReplicaRow, SlotRow, collect_replication_detail_bounds, collect_replication_replicas,
    collect_replication_slots, to_replicas_v1, to_slots_v1,
};
use kronika_source_pg::replication_instance::{
    ReplicationInstanceRow, collect_replication_instance, to_replication_instance,
};
use kronika_source_pg::reset_metadata::{
    ResetBase, ResetExtensions, collect_reset_base, statements_reset_at, store_plans_reset_at,
    to_reset_metadata,
};
use kronika_source_pg::settings::{SettingsRow, collect_settings, to_settings_v1};
use kronika_source_pg::statements::{self, StatementsRow, StatementsVersion};
use kronika_source_pg::store_plans::{
    self, StorePlansOsscRow, StorePlansRow, collect_store_plans, collect_store_plans_ossc,
    fetch_plan_text, store_plans_extversion, store_plans_is_ossc, store_plans_is_vadv,
};
use kronika_source_pg::user_indexes::{
    self, UserIndexesRow, UserIndexesVersion, collect_user_indexes,
};
use kronika_source_pg::user_tables::{self, UserTablesRow, UserTablesVersion, collect_user_tables};
use kronika_source_pg::wal::{WalSnapshot, collect_wal};
use kronika_source_pg::{
    ActivityRow, ActivityVersion, collect_activity, collect_bgwriter_checkpointer, to_v1, to_v2,
    to_v3,
};
use kronika_writer::{
    FlushedPart, Interner, Journal, JournalConfig, JournalError, SectionBuffers, dict, seal,
};
use logging::{
    CollectionFamily, LogLevel, duration_ms, field, layout_id, log_collection_failure,
    log_collection_finish, log_collection_start, log_database_collection_finish,
    log_database_collection_retry, log_database_collection_skip, log_database_collection_start,
    log_event, log_flush_summary, log_journal_append, log_source_deferred, section_name,
    summary_rows,
};
#[cfg(test)]
use scheduler::Intervals;
use scheduler::{DueSet, Scheduler, SourceKind};
#[cfg(test)]
use statements_source::{CachedStatementsSource, MissingStatementsSource};
use statements_source::{
    StatementsSource, StatementsSourceCache, all_statements_candidates, collect_statements_cached,
    statement_client, statements_type_id,
};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::signal::unix::{SignalKind, signal};
use tokio_postgres::Client;

fn timer_sleep_delay(
    now: Instant,
    tick_secs: u64,
    segment_max_age_secs: u64,
    sched: &Scheduler,
    plans_cache: &PlansSourceCache,
    segment: &SegmentState,
) -> Option<Duration> {
    if tick_secs == 0 {
        return None;
    }
    let mut delay = Duration::from_secs(tick_secs);
    if let Some(next_due) = sched.next_elapsed_due_in(now) {
        delay = delay.min(next_due);
    }
    if let Some(next_plans) = plans_cache.next_due_in(now) {
        delay = delay.min(next_plans);
    }
    if let Some(next_age) = segment.time_until_age(now, Duration::from_secs(segment_max_age_secs)) {
        delay = delay.min(next_age);
    }
    Some(delay)
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    std::fs::create_dir_all(&config.out_dir).context("create the output directory")?;

    // The journal lives next to the sealed segments so windows survive a
    // restart; recovered windows are sealed right away, before the server
    // connection — shipping already-collected data must not wait for it.
    let (mut journal, recovered) =
        open_collector_journal(&config.out_dir, config.journal_max_bytes)?;
    if let Some(dest) = recovered {
        announce(&format!("sealed {} reason=recovered", dest.display()));
    }

    let mut pool = ConnectionPool::connect(
        &config.dsn,
        &format!("pg_kronika-collector/{}", env!("CARGO_PKG_VERSION")),
        config.session,
        config.exclude_databases.clone(),
    )
    .await
    .context("connect pool")?;

    let mut sigusr2 = signal(SignalKind::user_defined2()).context("install the SIGUSR2 handler")?;
    let mut sigterm = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;
    let mut statements_cache = StatementsSourceCache::default();
    let mut plans_cache = PlansSourceCache::default();
    let mut log_collector =
        LogCollector::new(config.log.clone()).context("initialize PostgreSQL log collector")?;
    let mut sched = Scheduler::new(config.intervals);
    let mut segment = SegmentState::default();
    let mut pool_budget = PoolBudget::new(Duration::from_millis(config.cycle_db_budget_ms));
    // With the timer disabled collection is signal-driven only.
    let mut first_timer_tick = config.tick_secs > 0;

    announce("ready");

    loop {
        let sleep = if first_timer_tick {
            first_timer_tick = false;
            Some(Duration::ZERO)
        } else {
            timer_sleep_delay(
                Instant::now(),
                config.tick_secs,
                config.segment_max_age_secs,
                &sched,
                &plans_cache,
                &segment,
            )
        };
        let forced = tokio::select! {
            Some(()) = sigusr2.recv() => true,
            () = async {
                match sleep {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => std::future::pending::<()>().await,
                }
            } => false,
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        };
        let due = sched.plan(Instant::now(), forced);
        // The age valve runs on every tick, before collection: a tick whose
        // sources fail or return no rows must still close an expired segment.
        let age = Duration::from_secs(config.segment_max_age_secs);
        if segment.age_expired(Instant::now(), age) {
            match seal_open_segment(&mut journal, &config, &mut segment, "age") {
                Ok(dest) => {
                    sched.mark_segment_opened();
                    announce(&format!("sealed {} reason=age", dest.display()));
                }
                Err(err) => log_event(
                    LogLevel::Error,
                    "segment_seal_failure",
                    &[field("reason", "age"), field("error", format!("{err:#}"))],
                ),
            }
        }
        // The plans pace lives outside the scheduler; a tick with only the
        // plans read due still runs.
        if due.is_empty() && !plans_cache.is_due(Instant::now()) {
            continue;
        }
        run_collection_cycle(
            &mut pool,
            &mut journal,
            &config,
            &mut statements_cache,
            &mut plans_cache,
            &mut log_collector,
            &due,
            &mut segment,
            &mut sched,
            &mut pool_budget,
        )
        .await;
    }
    Ok(())
}

/// One collection cycle: reconnect and refresh the pool, append a collection
/// window, and seal the segment when rotation is due. Failures log and leave
/// the daemon running.
#[allow(
    clippy::too_many_arguments,
    reason = "the collection cycle wires every piece of daemon state together once"
)]
async fn run_collection_cycle(
    pool: &mut ConnectionPool,
    journal: &mut Journal,
    config: &Config,
    statements_cache: &mut StatementsSourceCache,
    plans_cache: &mut PlansSourceCache,
    log_collector: &mut LogCollector,
    due: &DueSet,
    segment: &mut SegmentState,
    sched: &mut Scheduler,
    pool_budget: &mut PoolBudget,
) {
    if let Err(err) = pool.ensure_main().await {
        log_event(
            LogLevel::Error,
            "pool_reconnect_failure",
            &[field("source", "main"), field("error", format!("{err:#}"))],
        );
        if due.has(SourceKind::PgLog) {
            match run_log_only_cycle(log_collector, journal, config, due, segment).await {
                Ok(sealed) => {
                    for (dest, reason) in sealed {
                        sched.mark_segment_opened();
                        announce(&format!("sealed {} reason={reason}", dest.display()));
                    }
                }
                Err(err) => log_event(
                    LogLevel::Error,
                    "pg_log_only_failure",
                    &[field("error", format!("{err:#}"))],
                ),
            }
        }
        return;
    }
    if let Err(err) = pool
        .refresh(
            Duration::from_secs(config.pool_refresh_secs),
            DEFAULT_MAX_DATABASES,
        )
        .await
    {
        log_event(
            LogLevel::Warn,
            "pool_refresh_failure",
            &[field("error", format!("{err:#}"))],
        );
    }
    for db in pool.uncovered() {
        log_event(
            LogLevel::Warn,
            "database_uncovered",
            &[field("database", db), field("reason", "pool_limit")],
        );
    }
    let major = pool.server_major();
    match snapshot_and_seal(
        pool,
        major,
        journal,
        config,
        statements_cache,
        plans_cache,
        log_collector,
        due,
        segment,
        pool_budget,
    )
    .await
    {
        Ok(outcome) => {
            apply_trigger_pace(
                sched,
                SourceKind::Activity,
                outcome.activity_hot,
                config.activity_fast_interval_s,
            );
            apply_trigger_pace(
                sched,
                SourceKind::Replication,
                outcome.replication_hot,
                config.replication_fast_interval_s,
            );
            for kind in outcome.deferred {
                // The budget pushed the source to the next tick, not dropped it.
                sched.defer(kind);
                log_source_deferred(kind, major);
            }
            for (dest, reason) in outcome.sealed {
                // A fresh segment must be self-contained: the service sections
                // (instance, reset, settings) come due on its first window.
                sched.mark_segment_opened();
                announce(&format!("sealed {} reason={reason}", dest.display()));
            }
        }
        Err(err) => log_event(
            LogLevel::Error,
            "snapshot_failure",
            &[field("error", format!("{err:#}"))],
        ),
    }
}

/// Apply one source's trigger verdict to its pace, logging only transitions.
fn apply_trigger_pace(
    sched: &mut Scheduler,
    kind: SourceKind,
    hot: Option<bool>,
    fast_interval_s: u64,
) {
    match hot {
        Some(true) => {
            if sched.accelerate(kind, fast_interval_s) {
                eprintln!("pg_kronika-collector: pace: {kind:?} accelerated to {fast_interval_s}s");
            }
        }
        Some(false) if sched.relax(kind) => {
            eprintln!("pg_kronika-collector: pace: {kind:?} back to its base interval");
        }
        _ => {}
    }
}

/// What one collection cycle produced: segments sealed on this tick, the
/// sized sources the budget pushed to the next tick, and the trigger verdicts
/// of the sources that were read (`None` when a source was not read).
#[derive(Debug, Default)]
struct CycleOutcome {
    sealed: Vec<(PathBuf, &'static str)>,
    deferred: Vec<SourceKind>,
    activity_hot: Option<bool>,
    replication_hot: Option<bool>,
}

/// Counters accumulated while collecting one top-N source, for `1_023_001`.
#[derive(Debug, Default, Clone, Copy)]
struct SourceCoverage {
    /// Known lower bound for source rows.
    total: u64,
    /// Rows collected.
    collected: u64,
    /// At least one count failed, so `total` is not exact.
    unknown_total: bool,
    /// Databases skipped after the adaptive timeout hit its cap.
    timeouts: u32,
    /// Databases skipped on a privilege failure (SQLSTATE 42501).
    permission_skips: u32,
    /// Databases skipped for any other error.
    other_skips: u32,
}

impl SourceCoverage {
    /// The `1_023_001` reason code: a timeout outranks a privilege failure,
    /// which outranks other skips; plain top-N selection is the default.
    const fn reason(&self) -> u8 {
        if self.timeouts > 0 {
            1
        } else if self.permission_skips > 0 {
            2
        } else if self.other_skips > 0 || self.unknown_total {
            3
        } else {
            0
        }
    }

    /// Whether any source rows are missing from the section.
    const fn truncated(&self) -> bool {
        self.total > self.collected
            || self.unknown_total
            || self.timeouts > 0
            || self.permission_skips > 0
            || self.other_skips > 0
    }
}

/// One pending `1_023_001` row.
#[derive(Debug, Clone, Copy)]
struct CoverageRecord {
    source_type_id: u32,
    coverage: SourceCoverage,
    max_n: u32,
    order_by: &'static str,
    cutoff_value: Option<f64>,
}

/// The `1_013` layout collected on this server major.
pub(crate) const fn user_tables_type_id(major: u32) -> u32 {
    match major {
        0..=12 => 1_013_001,
        13..=15 => 1_013_002,
        16..=17 => 1_013_003,
        _ => 1_013_004,
    }
}

/// The `1_001` layout collected on this server major.
const fn activity_type_id(version: ActivityVersion) -> u32 {
    match version {
        ActivityVersion::V1 => 1_001_001,
        ActivityVersion::V2 => 1_001_002,
        ActivityVersion::V3 => 1_001_003,
    }
}

/// The `1_005` layout collected on this server major.
const fn database_type_id(version: DatabaseVersion) -> u32 {
    match version {
        DatabaseVersion::V1 => 1_005_001,
        DatabaseVersion::V2 => 1_005_002,
        DatabaseVersion::V3 => 1_005_003,
        DatabaseVersion::V4 => 1_005_004,
    }
}

/// The `1_014` layout collected on this server major.
pub(crate) const fn user_indexes_type_id(major: u32) -> u32 {
    if major >= 16 { 1_014_002 } else { 1_014_001 }
}

/// The `1_014` selection axes on this server major: `last_idx_scan` exists
/// only from PG16, so older majors must not claim it in coverage.
const fn user_indexes_order_by(major: u32) -> &'static str {
    if major >= 16 {
        "idx_scan|idx_tup_read|relpages|last_idx_scan"
    } else {
        "idx_scan|idx_tup_read|relpages"
    }
}

/// The `1_007` layout returned by `pg_stat_wal`.
const fn wal_type_id(wal: &WalSnapshot) -> u32 {
    match wal {
        WalSnapshot::V1(_) => 1_007_001,
        WalSnapshot::V2(_) => 1_007_002,
    }
}

/// The `1_009` layout collected on this server major.
const fn io_type_id(version: IoVersion) -> u32 {
    match version {
        IoVersion::V1 => 1_009_001,
        IoVersion::V2 => 1_009_002,
    }
}

/// The `1_011` layout collected on this server major.
const fn locks_type_id(version: LocksVersion) -> u32 {
    match version {
        LocksVersion::V1 => 1_011_001,
        LocksVersion::V2 => 1_011_002,
    }
}

/// Collect `pg_stat_user_tables` from every pool database, returning owned rows.
///
/// All awaits finish here so the caller can intern without holding the `!Send`
/// `Interner` across an await. The heavy size query runs under an adaptive
/// `statement_timeout`: SQLSTATE `57014` widens it and retries the same database
/// until the cap; any other error logs and skips that database so one bad
/// database does not lose the whole segment.
#[allow(
    clippy::too_many_lines,
    reason = "per-database retry, coverage, and diagnostic logging stay together for one source"
)]
async fn collect_user_tables_all(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
) -> (
    Vec<(String, UserTablesVersion, Vec<UserTablesRow>)>,
    SourceCoverage,
) {
    let mut user_tables = Vec::new();
    let mut coverage = SourceCoverage::default();
    let mut heavy = AdaptiveTimeout::new(15_000, config.heavy_timeout_cap_ms);
    let type_id = user_tables_type_id(major);
    for db in pool.per_db() {
        loop {
            let started = Instant::now();
            log_database_collection_start(type_id, &db.datname);
            // The heavy size functions can be slow, so this query runs under a
            // wider statement_timeout. SET persists on the connection: it stays
            // in effect until the next database's SET overwrites it.
            if let Err(err) = db
                .client()
                .batch_execute(&format!("SET statement_timeout = {}", heavy.current_ms()))
                .await
            {
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", "database"),
                        field("database", &db.datname),
                        field("reason", "set_statement_timeout_failed"),
                        field("error", &err),
                    ],
                );
            }
            match collect_user_tables(db.client(), major, config.max_tables).await {
                Ok((version, rows, source_total)) => {
                    coverage.collected += rows.len() as u64;
                    // The total rides in the same statement as the rows, so
                    // it describes exactly the population they were cut from.
                    // Every selection axis is an unfiltered ORDER BY, so an
                    // empty read means an empty source: a zero total is exact.
                    coverage.total += source_total;
                    log_database_collection_finish(
                        type_id,
                        &db.datname,
                        rows.len(),
                        source_total,
                        started.elapsed(),
                    );
                    user_tables.push((db.datname.clone(), version, rows));
                    break;
                }
                Err(err) if is_sqlstate(&err, "57014") && !heavy.at_cap() => {
                    let old_timeout_ms = heavy.current_ms();
                    heavy.grow(); // statement_timeout hit; retry this database wider
                    log_database_collection_retry(
                        type_id,
                        &db.datname,
                        old_timeout_ms,
                        heavy.current_ms(),
                        &err,
                        started.elapsed(),
                    );
                }
                Err(err) if is_sqlstate(&err, "55P03") => {
                    // lock_not_available: another session holds a conflicting lock.
                    // Label it distinctly so contention is not read as a query bug.
                    coverage.other_skips += 1;
                    log_database_collection_skip(
                        type_id,
                        &db.datname,
                        "lock_not_available",
                        &err,
                        started.elapsed(),
                    );
                    break;
                }
                Err(err) => {
                    let reason = if is_sqlstate(&err, "57014") {
                        coverage.timeouts += 1;
                        "statement_timeout"
                    } else if is_sqlstate(&err, "42501") {
                        coverage.permission_skips += 1;
                        "permission_denied"
                    } else {
                        coverage.other_skips += 1;
                        "query_failed"
                    };
                    coverage.unknown_total = true;
                    log_database_collection_skip(
                        type_id,
                        &db.datname,
                        reason,
                        &err,
                        started.elapsed(),
                    );
                    break;
                }
            }
        }
    }
    (user_tables, coverage)
}

/// Collect `pg_stat_user_indexes` from every pool database, returning owned rows.
///
/// Mirrors [`collect_user_tables_all`]: all awaits finish here so the caller can
/// intern without holding the `!Send` `Interner` across an await. The size query
/// runs under an adaptive `statement_timeout`: SQLSTATE `57014` widens it and
/// retries the same database until the cap; any other error logs and skips that
/// database so one bad database does not lose the whole segment.
#[allow(
    clippy::too_many_lines,
    reason = "per-database retry, coverage, and diagnostic logging stay together for one source"
)]
async fn collect_user_indexes_all(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
) -> (
    Vec<(String, UserIndexesVersion, Vec<UserIndexesRow>)>,
    SourceCoverage,
) {
    let mut user_indexes = Vec::new();
    let mut coverage = SourceCoverage::default();
    let mut heavy = AdaptiveTimeout::new(15_000, config.heavy_timeout_cap_ms);
    let type_id = user_indexes_type_id(major);
    for db in pool.per_db() {
        loop {
            let started = Instant::now();
            log_database_collection_start(type_id, &db.datname);
            // pg_relation_size over many indexes can be slow, so this query runs
            // under a wider statement_timeout. SET persists on the connection: it
            // stays in effect until the next database's SET overwrites it.
            if let Err(err) = db
                .client()
                .batch_execute(&format!("SET statement_timeout = {}", heavy.current_ms()))
                .await
            {
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", "database"),
                        field("database", &db.datname),
                        field("reason", "set_statement_timeout_failed"),
                        field("error", &err),
                    ],
                );
            }
            match collect_user_indexes(db.client(), major, config.max_indexes).await {
                Ok((version, rows, source_total)) => {
                    coverage.collected += rows.len() as u64;
                    // Same-statement total; an empty read means an empty
                    // source, so the zero total is exact.
                    coverage.total += source_total;
                    log_database_collection_finish(
                        type_id,
                        &db.datname,
                        rows.len(),
                        source_total,
                        started.elapsed(),
                    );
                    user_indexes.push((db.datname.clone(), version, rows));
                    break;
                }
                Err(err) if is_sqlstate(&err, "57014") && !heavy.at_cap() => {
                    let old_timeout_ms = heavy.current_ms();
                    heavy.grow(); // statement_timeout hit; retry this database wider
                    log_database_collection_retry(
                        type_id,
                        &db.datname,
                        old_timeout_ms,
                        heavy.current_ms(),
                        &err,
                        started.elapsed(),
                    );
                }
                Err(err) if is_sqlstate(&err, "55P03") => {
                    // lock_not_available: another session holds a conflicting lock.
                    // Label it distinctly so contention is not read as a query bug.
                    coverage.other_skips += 1;
                    log_database_collection_skip(
                        type_id,
                        &db.datname,
                        "lock_not_available",
                        &err,
                        started.elapsed(),
                    );
                    break;
                }
                Err(err) => {
                    let reason = if is_sqlstate(&err, "57014") {
                        coverage.timeouts += 1;
                        "statement_timeout"
                    } else if is_sqlstate(&err, "42501") {
                        coverage.permission_skips += 1;
                        "permission_denied"
                    } else {
                        coverage.other_skips += 1;
                        "query_failed"
                    };
                    coverage.unknown_total = true;
                    log_database_collection_skip(
                        type_id,
                        &db.datname,
                        reason,
                        &err,
                        started.elapsed(),
                    );
                    break;
                }
            }
        }
    }
    (user_indexes, coverage)
}

/// Cached `pg_store_plans` source and read pacing.
///
/// Plans change slowly and reading their texts can be expensive, so reads run
/// on their own interval. Segments sealed between reads do not include the
/// section.
#[derive(Debug, Default)]
struct PlansSourceCache {
    selected: Option<CachedPlansSource>,
    next_read: Option<Instant>,
}

impl PlansSourceCache {
    /// Whether the paced `pg_store_plans` read is due at `now`.
    ///
    /// The main loop asks this before skipping a tick whose scheduler due-set
    /// is empty: the plans pace must not depend on another source being due.
    fn is_due(&self, now: Instant) -> bool {
        self.next_read.is_none_or(|due| now >= due)
    }

    fn next_due_in(&self, now: Instant) -> Option<Duration> {
        let delay = self.next_read?.saturating_duration_since(now);
        (!delay.is_zero()).then_some(delay)
    }
}

#[derive(Debug, Clone)]
struct CachedPlansSource {
    source: StatementsSource,
    extversion: String,
    fork: PlansFork,
}

/// Which `pg_store_plans` fork the cached source exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlansFork {
    /// vadv 2.x: `pg_store_plans(showtext)`, per-plan-shape identity.
    Vadv,
    /// ossc upstream: zero-argument view function, per-query identity.
    Ossc,
}

/// One paced read, typed by the source fork.
#[derive(Debug)]
enum PlansRead {
    Vadv(Vec<StorePlansRow>),
    Ossc(Vec<StorePlansOsscRow>),
}

impl PlansRead {
    const fn is_empty(&self) -> bool {
        match self {
            Self::Vadv(rows) => rows.is_empty(),
            Self::Ossc(rows) => rows.is_empty(),
        }
    }

    const fn rows_len(&self) -> usize {
        match self {
            Self::Vadv(rows) => rows.len(),
            Self::Ossc(rows) => rows.len(),
        }
    }

    const fn type_id(&self) -> u32 {
        match self {
            Self::Vadv(_) => 1_004_001,
            Self::Ossc(_) => 1_003_001,
        }
    }
}

impl PlansFork {
    const fn type_id(self) -> u32 {
        match self {
            Self::Vadv => 1_004_001,
            Self::Ossc => 1_003_001,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Vadv => "vadv",
            Self::Ossc => "ossc",
        }
    }
}

/// Delay until the next `pg_store_plans` read.
///
/// An empty result means plans have not accumulated yet; retry sooner than the
/// full interval so the first plans are not delayed by up to `interval`.
fn plans_reread_delay(rows_empty: bool, interval: Duration) -> Duration {
    if rows_empty {
        interval.min(Duration::from_secs(30))
    } else {
        interval
    }
}

/// One read attempt through the cached source; any failure invalidates it so
/// the caller can decide when to rediscover.
#[allow(
    clippy::too_many_lines,
    reason = "cached source validation and rediscovery diagnostics are one state transition"
)]
async fn try_cached_plans_read(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut PlansSourceCache,
    now: Instant,
) -> Option<(PlansRead, u64)> {
    if let Some(cached) = cache.selected.clone() {
        let label = cached.source.label();
        let started = Instant::now();
        let type_id = cached.fork.type_id();
        if let Some(client) = statement_client(pool, &cached.source) {
            match store_plans_extversion(client).await {
                Ok(Some(extversion)) if extversion == cached.extversion => {
                    match collect_plans_for_fork(client, config, cached.fork).await {
                        Ok(read) => {
                            cache.next_read = Some(
                                now + plans_reread_delay(read.0.is_empty(), config.plans_interval),
                            );
                            log_event(
                                LogLevel::Debug,
                                "collection_finish",
                                &[
                                    field("collection", section_name(type_id)),
                                    field("type_id", type_id),
                                    field("layout_id", layout_id(type_id)),
                                    field("source", &label),
                                    field("cached_source", true),
                                    field("fork", cached.fork.as_str()),
                                    field("rows", read.0.rows_len()),
                                    field("source_total", read.1),
                                    field("elapsed_ms", duration_ms(started.elapsed())),
                                ],
                            );
                            return Some(read);
                        }
                        Err(err) => {
                            log_event(
                                LogLevel::Warn,
                                "collection_probe_failure",
                                &[
                                    field("collection", section_name(type_id)),
                                    field("type_id", type_id),
                                    field("layout_id", layout_id(type_id)),
                                    field("source", &label),
                                    field("cached_source", true),
                                    field("fork", cached.fork.as_str()),
                                    field("reason", "query_failed"),
                                    field("error", &err),
                                    field("elapsed_ms", duration_ms(started.elapsed())),
                                ],
                            );
                            cache.selected = None;
                        }
                    }
                }
                Ok(Some(extversion)) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", &label),
                            field("cached_source", true),
                            field("fork", cached.fork.as_str()),
                            field("reason", "extension_version_changed"),
                            field("old_extversion", &cached.extversion),
                            field("new_extversion", extversion),
                        ],
                    );
                    cache.selected = None;
                }
                Ok(None) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", &label),
                            field("cached_source", true),
                            field("fork", cached.fork.as_str()),
                            field("reason", "extension_missing"),
                        ],
                    );
                    cache.selected = None;
                }
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", &label),
                            field("cached_source", true),
                            field("fork", cached.fork.as_str()),
                            field("reason", "probe_failed"),
                            field("error", &err),
                            field("elapsed_ms", duration_ms(started.elapsed())),
                        ],
                    );
                    cache.selected = None;
                }
            }
        } else {
            log_event(
                LogLevel::Warn,
                "collection_probe_failure",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", &label),
                    field("cached_source", true),
                    field("fork", cached.fork.as_str()),
                    field("reason", "source_unavailable"),
                ],
            );
            cache.selected = None;
        }
    }

    None
}

/// Collect `pg_store_plans` from one cached source connection.
///
/// The statistics are instance-wide, read from the one database where the
/// extension is installed; discovery walks `pool.main()` first, then the
/// covered per-db connections. Returns `None` between paced reads and when no
/// vadv 2.x source exists. All awaits finish here so the caller can intern
/// without holding the `!Send` `Interner` across an await.
#[allow(
    clippy::too_many_lines,
    reason = "source discovery, fork detection, and diagnostic skip reasons share one control flow"
)]
async fn collect_store_plans_cached(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut PlansSourceCache,
    force: bool,
) -> Option<(PlansRead, u64)> {
    let now = Instant::now();
    if !force && !cache.is_due(now) {
        return None;
    }

    let had_cached_source = cache.selected.is_some();
    if let Some(read) = try_cached_plans_read(pool, config, cache, now).await {
        return Some(read);
    }

    // A cached source that just failed already cost this snapshot one heavy
    // attempt; rediscovery waits for the next snapshot instead of doubling it.
    if had_cached_source && cache.selected.is_none() {
        return None;
    }

    for candidate in all_statements_candidates(pool) {
        let label = candidate.source.label();
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[
                CollectionFamily::StorePlans.field(),
                field("source", &label),
            ],
        );
        let extversion = match store_plans_extversion(candidate.client).await {
            Ok(Some(extversion)) => extversion,
            Ok(None) => continue,
            Err(err) => {
                log_event(
                    LogLevel::Warn,
                    "collection_probe_failure",
                    &[
                        CollectionFamily::StorePlans.field(),
                        field("source", &label),
                        field("reason", "probe_failed"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                continue;
            }
        };
        let fork = match store_plans_is_vadv(candidate.client).await {
            Ok(true) => PlansFork::Vadv,
            Ok(false) => match store_plans_is_ossc(candidate.client).await {
                Ok(true) => PlansFork::Ossc,
                Ok(false) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_skip",
                        &[
                            CollectionFamily::StorePlans.field(),
                            field("source", &label),
                            field("reason", "unsupported_signature"),
                            field("elapsed_ms", duration_ms(started.elapsed())),
                        ],
                    );
                    continue;
                }
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            CollectionFamily::StorePlans.field(),
                            field("source", &label),
                            field("reason", "signature_probe_failed"),
                            field("error", &err),
                            field("elapsed_ms", duration_ms(started.elapsed())),
                        ],
                    );
                    continue;
                }
            },
            Err(err) => {
                log_event(
                    LogLevel::Warn,
                    "collection_probe_failure",
                    &[
                        CollectionFamily::StorePlans.field(),
                        field("source", &label),
                        field("reason", "signature_probe_failed"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                continue;
            }
        };
        let type_id = fork.type_id();
        let supported = match fork {
            PlansFork::Vadv => extversion.starts_with("2."),
            PlansFork::Ossc => extversion.starts_with("1."),
        };
        if !supported {
            log_event(
                LogLevel::Warn,
                "collection_skip",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", &label),
                    field("fork", fork.as_str()),
                    field("reason", "unsupported_extension_version"),
                    field("extversion", extversion),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            continue;
        }
        match collect_plans_for_fork(candidate.client, config, fork).await {
            Ok(read) => {
                cache.selected = Some(CachedPlansSource {
                    source: candidate.source,
                    extversion,
                    fork,
                });
                cache.next_read =
                    Some(now + plans_reread_delay(read.0.is_empty(), config.plans_interval));
                log_event(
                    LogLevel::Debug,
                    "collection_finish",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", &label),
                        field("fork", fork.as_str()),
                        field("rows", read.0.rows_len()),
                        field("source_total", read.1),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Some(read);
            }
            Err(err) => {
                log_event(
                    LogLevel::Warn,
                    "collection_skip",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", &label),
                        field("fork", fork.as_str()),
                        field("reason", "query_failed"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
            }
        }
    }

    // No source found: keep the pacing so discovery does not rescan every snapshot.
    cache.next_read = Some(now + config.plans_interval);
    None
}

/// Wall-clock cap on the per-read plan-text phase; rows past it seal NULL.
const PLAN_TEXT_DEADLINE: Duration = Duration::from_secs(10);

/// Run the fork's collection path and wrap the rows for sealing.
async fn collect_plans_for_fork(
    client: &Client,
    config: &Config,
    fork: PlansFork,
) -> Result<(PlansRead, u64), tokio_postgres::Error> {
    match fork {
        PlansFork::Vadv => {
            let (rows, source_total) = collect_plans_with_texts(client, config).await?;
            Ok((PlansRead::Vadv(rows), source_total))
        }
        PlansFork::Ossc => {
            let (rows, source_total) = collect_ossc_plans_with_budget(client, config).await?;
            Ok((PlansRead::Ossc(rows), source_total))
        }
    }
}

/// Collect ossc rows and apply the byte budget to their inline plan texts.
///
/// A zero budget switches to the numeric-only query, so no plan text crosses
/// the network at all. With a budget, the server truncates per row by
/// characters and each text is byte-capped in Rust to
/// `min(max_plan_text, remaining budget)` before accounting; tail rows past
/// the budget seal a NULL plan. Rows whose identity the upstream masked for
/// lack of `pg_read_all_stats` are dropped and reported.
async fn collect_ossc_plans_with_budget(
    client: &Client,
    config: &Config,
) -> Result<(Vec<StorePlansOsscRow>, u64), tokio_postgres::Error> {
    let started = Instant::now();
    let text_cap = (config.plan_text_budget > 0).then_some(config.max_plan_text);
    let (mut rows, masked, source_total) =
        collect_store_plans_ossc(client, config.max_plans, text_cap).await?;
    if masked > 0 {
        log_event(
            LogLevel::Warn,
            "collection_degraded",
            &[
                field("collection", section_name(1_003_001)),
                field("type_id", 1_003_001),
                field("layout_id", layout_id(1_003_001)),
                field("fork", "ossc"),
                field("reason", "privilege_masked_rows"),
                field("skipped_rows", masked),
            ],
        );
    }
    let per_text_cap = usize::try_from(config.max_plan_text).unwrap_or(usize::MAX);
    let mut budget = config.plan_text_budget;
    let mut kept = 0_usize;
    for row in &mut rows {
        let Some(text) = row.plan.as_mut() else {
            continue;
        };
        let cap = usize::try_from(budget)
            .unwrap_or(usize::MAX)
            .min(per_text_cap);
        if cap == 0 {
            row.plan = None;
            continue;
        }
        truncate_to_boundary(text, cap);
        budget = budget.saturating_sub(u64::try_from(text.len()).unwrap_or(u64::MAX));
        kept += 1;
    }
    log_event(
        LogLevel::Debug,
        "plan_text_read_finish",
        &[
            field("collection", section_name(1_003_001)),
            field("type_id", 1_003_001),
            field("layout_id", layout_id(1_003_001)),
            field("fork", "ossc"),
            field("rows", rows.len()),
            field("plan_texts", kept),
            field("budget_bytes_left", budget),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    Ok((rows, source_total))
}

/// Enumerate top-N plan rows, then fetch texts under the per-read budget.
///
/// Every limit degrades to a NULL `plan`, never to a lost row: the byte budget,
/// the per-text cap, the wall-clock deadline, and a fetch error all stop the
/// text phase only.
async fn collect_plans_with_texts(
    client: &Client,
    config: &Config,
) -> Result<(Vec<StorePlansRow>, u64), tokio_postgres::Error> {
    let started = Instant::now();
    let (mut rows, source_total) = collect_store_plans(client, config.max_plans).await?;
    let mut budget = config.plan_text_budget;
    let mut fetched = 0_usize;
    for row in &mut rows {
        // The server-side left() cuts characters; the fetch cap and the final
        // truncate_to_boundary make the contract bytes.
        let cap = u64::try_from(config.max_plan_text).unwrap_or(0).min(budget);
        if cap == 0 {
            break;
        }
        let remaining = PLAN_TEXT_DEADLINE.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(1_004_001)),
                    field("type_id", 1_004_001),
                    field("layout_id", layout_id(1_004_001)),
                    field("fork", "vadv"),
                    field("reason", "plan_text_deadline"),
                    field("deadline_ms", duration_ms(PLAN_TEXT_DEADLINE)),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            break;
        }
        // A hard wall: one slow fetch must not stretch the snapshot past the
        // deadline while waiting on statement_timeout.
        let attempt = tokio::time::timeout(
            remaining,
            fetch_plan_text(client, row, i32::try_from(cap).unwrap_or(i32::MAX)),
        )
        .await;
        let Ok(attempt) = attempt else {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(1_004_001)),
                    field("type_id", 1_004_001),
                    field("layout_id", layout_id(1_004_001)),
                    field("fork", "vadv"),
                    field("reason", "plan_text_fetch_timeout"),
                    field("planid", row.planid),
                    field("deadline_ms", duration_ms(PLAN_TEXT_DEADLINE)),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            break;
        };
        match attempt {
            Ok(Some(mut text)) => {
                truncate_to_boundary(&mut text, usize::try_from(cap).unwrap_or(usize::MAX));
                budget = budget.saturating_sub(u64::try_from(text.len()).unwrap_or(u64::MAX));
                row.plan = Some(text);
                fetched += 1;
            }
            // The entry vanished between enumeration and this call.
            Ok(None) => {}
            Err(err) => {
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(1_004_001)),
                        field("type_id", 1_004_001),
                        field("layout_id", layout_id(1_004_001)),
                        field("fork", "vadv"),
                        field("reason", "plan_text_fetch_failed"),
                        field("planid", row.planid),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                break;
            }
        }
    }
    log_event(
        LogLevel::Debug,
        "plan_text_read_finish",
        &[
            field("collection", section_name(1_004_001)),
            field("type_id", 1_004_001),
            field("layout_id", layout_id(1_004_001)),
            field("fork", "vadv"),
            field("rows", rows.len()),
            field("plan_texts", fetched),
            field("budget_bytes_left", budget),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    Ok((rows, source_total))
}

/// Truncate a string to at most `max_bytes`, on a UTF-8 character boundary.
fn truncate_to_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
}

/// What the sized pool sources produced this cycle.
struct PoolReads {
    statements: Option<(StatementsVersion, Vec<StatementsRow>, u64)>,
    user_tables: Vec<(String, UserTablesVersion, Vec<UserTablesRow>)>,
    tables_cov: SourceCoverage,
    user_indexes: Vec<(String, UserIndexesVersion, Vec<UserIndexesRow>)>,
    indexes_cov: SourceCoverage,
    deferred: Vec<SourceKind>,
}

/// Read the due sized sources under the cycle budget, in survival order —
/// statements first, indexes last — so under pressure the most expensive
/// source is deferred first.
async fn read_pool_sources(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    statements_cache: &mut StatementsSourceCache,
    due: &DueSet,
    pool_budget: &mut PoolBudget,
    cycle_start: Instant,
) -> PoolReads {
    let mut deferred = Vec::new();
    let statements = if due.has(SourceKind::Statements) {
        if pool_budget.admit(PoolSource::Statements, cycle_start.elapsed(), due.forced()) {
            collect_statements_cached(pool, config, statements_cache).await
        } else {
            deferred.push(SourceKind::Statements);
            None
        }
    } else {
        None
    };
    let (user_tables, tables_cov) = if due.has(SourceKind::UserTables) {
        if pool_budget.admit(PoolSource::UserTables, cycle_start.elapsed(), due.forced()) {
            collect_user_tables_all(pool, major, config).await
        } else {
            deferred.push(SourceKind::UserTables);
            (Vec::new(), SourceCoverage::default())
        }
    } else {
        (Vec::new(), SourceCoverage::default())
    };
    let (user_indexes, indexes_cov) = if due.has(SourceKind::UserIndexes) {
        if pool_budget.admit(PoolSource::UserIndexes, cycle_start.elapsed(), due.forced()) {
            collect_user_indexes_all(pool, major, config).await
        } else {
            deferred.push(SourceKind::UserIndexes);
            (Vec::new(), SourceCoverage::default())
        }
    } else {
        (Vec::new(), SourceCoverage::default())
    };
    PoolReads {
        statements,
        user_tables,
        tables_cov,
        user_indexes,
        indexes_cov,
        deferred,
    }
}

/// OS procfs sections collected synchronously in the read phase.
struct OsSources {
    cpu: Vec<OsCpu>,
    stat: Option<OsStat>,
    meminfo: Option<OsMeminfo>,
    loadavg: Option<OsLoadavg>,
    vmstat: Option<OsVmstat>,
    psi: Vec<OsPsi>,
    diskstats: Vec<OsDiskstats>,
    netdev: Vec<OsNetdev>,
    snmp: Option<OsSnmp>,
    netstat: Option<OsNetstat>,
    mountinfo: Vec<OsMountinfo>,
    topology: Vec<OsTopology>,
    processes: Vec<OsProcess>,
    process_status: Vec<OsProcessStatus>,
    cgroup_mapping: Vec<OsCgroupMapping>,
    cgroup_cpu: Vec<OsCgroupCpu>,
    cgroup_memory: Vec<OsCgroupMemory>,
    cgroup_io: Vec<OsCgroupIo>,
    cgroup_pids: Vec<OsCgroupPids>,
}

impl OsSources {
    const fn empty() -> Self {
        Self {
            cpu: Vec::new(),
            stat: None,
            meminfo: None,
            loadavg: None,
            vmstat: None,
            psi: Vec::new(),
            diskstats: Vec::new(),
            netdev: Vec::new(),
            snmp: None,
            netstat: None,
            mountinfo: Vec::new(),
            topology: Vec::new(),
            processes: Vec::new(),
            process_status: Vec::new(),
            cgroup_mapping: Vec::new(),
            cgroup_cpu: Vec::new(),
            cgroup_memory: Vec::new(),
            cgroup_io: Vec::new(),
            cgroup_pids: Vec::new(),
        }
    }
}

fn read_optional_os_file(fs: &ProcFs, rel: &'static str, type_id: u32) -> Option<String> {
    match fs.read_raw(rel) {
        Ok(content) => Some(content),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", rel),
                    field("reason", &err),
                ],
            );
            None
        }
    }
}

/// Read every procfs OS section synchronously.
///
/// Counter sections (cpu, stat, meminfo, loadavg, vmstat, psi, diskstats,
/// netdev, snmp, netstat) are gated on `due.has(SourceKind::OsCore)` and are
/// never emitted on an OsMountTopo-only tick. Mountinfo is parsed on every
/// `OsCore` tick for diskstats attribution and emitted, together with topology,
/// only when `due.has(SourceKind::OsMountTopo)` is true.
/// On file read or parse failure the affected section is skipped and a
/// `collection_degraded` event is logged; zeros are never fabricated. `scope`
/// is the host scope for device-local sections; network sections carry their
/// own `net_scope`.
///
/// The `interner` is the segment's interner: device, interface, and mount
/// strings are interned here so the built rows already hold their `StrId`s.
#[allow(
    clippy::too_many_lines,
    reason = "independent procfs reads with per-source degradation logging kept adjacent"
)]
fn collect_os_sources(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    in_container: bool,
    due: &DueSet,
) -> OsSources {
    if !due.has(SourceKind::OsCore)
        && !due.has(SourceKind::OsMountTopo)
        && !due.has(SourceKind::OsProcesses)
        && !due.has(SourceKind::OsProcessStatus)
        && !due.has(SourceKind::OsCgroup)
        && !due.has(SourceKind::OsCgroupMapping)
    {
        return OsSources::empty();
    }

    let mut os = OsSources::empty();

    if due.has(SourceKind::OsCore) {
        // stat — read once, feed to both cpu and stat-misc parsers.
        let stat_started = Instant::now();
        match fs.read("stat") {
            Ok(content) => {
                // CPU rows (1_102_001)
                let cpu_type_id = 1_102_001_u32;
                match parse_cpu(&content, ts) {
                    Ok(rows) => {
                        let n = rows.len();
                        os.cpu = rows.into_iter().map(|r| r.to_section(scope)).collect();
                        log_collection_finish(cpu_type_id, "procfs", n, stat_started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(cpu_type_id)),
                                field("type_id", cpu_type_id),
                                field("layout_id", layout_id(cpu_type_id)),
                                field("source", "stat"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                }
                // Stat-misc row (1_103_001) — same content, separate parser.
                // Its own clock so the reported latency excludes the CPU parse above.
                let stat_misc_started = Instant::now();
                let stat_type_id = 1_103_001_u32;
                match parse_stat_misc(&content, ts) {
                    Ok(row) => {
                        os.stat = Some(row.to_section(scope));
                        log_collection_finish(
                            stat_type_id,
                            "procfs",
                            1,
                            stat_misc_started.elapsed(),
                        );
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(stat_type_id)),
                                field("type_id", stat_type_id),
                                field("layout_id", layout_id(stat_type_id)),
                                field("source", "stat"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                }
            }
            Err(err) => {
                let cpu_type_id = 1_102_001_u32;
                let stat_type_id = 1_103_001_u32;
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(cpu_type_id)),
                        field("type_id", cpu_type_id),
                        field("layout_id", layout_id(cpu_type_id)),
                        field("source", "stat"),
                        field("reason", &err),
                    ],
                );
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(stat_type_id)),
                        field("type_id", stat_type_id),
                        field("layout_id", layout_id(stat_type_id)),
                        field("source", "stat"),
                        field("reason", &err),
                    ],
                );
            }
        }

        // meminfo (1_104_001)
        {
            let type_id = 1_104_001_u32;
            let started = Instant::now();
            match fs.read("meminfo") {
                Ok(content) => match parse_meminfo(&content, ts) {
                    Ok(row) => {
                        os.meminfo = Some(row.to_section(scope));
                        log_collection_finish(type_id, "procfs", 1, started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "meminfo"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                },
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "meminfo"),
                            field("reason", &err),
                        ],
                    );
                }
            }
        }

        // loadavg (1_105_001)
        {
            let type_id = 1_105_001_u32;
            let started = Instant::now();
            match fs.read("loadavg") {
                Ok(content) => match parse_loadavg(&content, ts) {
                    Ok(row) => {
                        os.loadavg = Some(row.to_section(scope));
                        log_collection_finish(type_id, "procfs", 1, started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "loadavg"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                },
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "loadavg"),
                            field("reason", &err),
                        ],
                    );
                }
            }
        }

        // vmstat (1_106_001)
        {
            let type_id = 1_106_001_u32;
            let started = Instant::now();
            match fs.read("vmstat") {
                Ok(content) => match parse_vmstat(&content, ts) {
                    Ok(row) => {
                        os.vmstat = Some(row.to_section(scope));
                        log_collection_finish(type_id, "procfs", 1, started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "vmstat"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                },
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "vmstat"),
                            field("reason", &err),
                        ],
                    );
                }
            }
        }

        // PSI — cpu/memory/io as Option<String>; missing file → None (1_107_001)
        {
            let type_id = 1_107_001_u32;
            let started = Instant::now();
            let psi_cpu = read_optional_os_file(fs, "pressure/cpu", type_id);
            let psi_memory = read_optional_os_file(fs, "pressure/memory", type_id);
            let psi_io = read_optional_os_file(fs, "pressure/io", type_id);
            match parse_pressure(
                psi_cpu.as_deref(),
                psi_memory.as_deref(),
                psi_io.as_deref(),
                ts,
            ) {
                Ok(rows) => {
                    let n = rows.len();
                    if n == 0 {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "pressure/{cpu,memory,io}"),
                                field("reason", "no pressure files available"),
                            ],
                        );
                    } else {
                        os.psi = rows.into_iter().map(|r| r.to_section(scope)).collect();
                        log_collection_finish(type_id, "procfs", n, started.elapsed());
                    }
                }
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "pressure/{cpu,memory,io}"),
                            field("reason", &err.0),
                        ],
                    );
                }
            }
        }
    } // end if due.has(SourceKind::OsCore) — stat, meminfo, loadavg, vmstat, psi

    // Mountinfo is parsed whenever either OsCore or OsMountTopo is due:
    // OsCore needs it for the container device filter in diskstats;
    // OsMountTopo needs it to build the attribution section rows.
    let mounts = mountinfo_entries(fs);

    if due.has(SourceKind::OsCore) {
        // Counters: disk and network. Network sections carry the pod's
        // network-namespace scope inside a container, not the host scope.
        let net_scope_id = net_scope(fs).as_u8();
        os.diskstats = collect_diskstats(fs, interner, scope, ts, in_container, &mounts);
        os.netdev = collect_netdev(fs, interner, net_scope_id, ts);
        collect_net_singletons(fs, net_scope_id, ts, &mut os);
    }

    if due.has(SourceKind::OsMountTopo) {
        os.mountinfo = collect_mountinfo(interner, scope, ts, &mounts);
        os.topology = collect_topology(fs, &SysFs::from_env(), interner, scope, ts);
    }

    let entity_scope = os_entity_scope(in_container);
    collect_process_sections(fs, interner, entity_scope, ts, due, &mut os);
    collect_cgroup_sections(
        &SysFs::from_env(),
        interner,
        entity_scope,
        ts,
        fs,
        due,
        &mut os,
    );

    os
}

/// Read and parse `/proc/diskstats`, interning device names into rows.
///
/// Inside a container the pod's real backing devices are the only ones charged
/// to it: `/proc/diskstats` reports the whole node, so rows are filtered to the
/// mountinfo-derived device set. Over `KRONIKA_OS_MAX_DISKS` rows the lowest
/// `(major, minor)` devices are kept and the overflow is logged, not dropped
/// silently.
fn collect_diskstats(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    in_container: bool,
    mounts: &[MountEntry],
) -> Vec<OsDiskstats> {
    let type_id = 1_108_001_u32;
    let started = Instant::now();
    let Some(content) = read_optional_os_file(fs, "diskstats", type_id) else {
        return Vec::new();
    };
    let mut rows = match diskstats::parse(&content) {
        Ok(rows) => rows,
        Err(err) => {
            log_degraded(type_id, "diskstats", &err.0);
            return Vec::new();
        }
    };

    if in_container {
        let devices = container_device_set(mounts);
        rows.retain(|row| devices.contains(&(row.major, row.minor)));
    }

    apply_disk_cap(&mut rows, type_id);

    let built: Vec<OsDiskstats> = rows
        .iter()
        .filter_map(|row| {
            let device = intern_str(interner, type_id, "diskstats", &row.device)?;
            Some(row.to_section(scope, ts, device))
        })
        .collect();
    log_collection_finish(type_id, "procfs", built.len(), started.elapsed());
    built
}

/// Keep at most `KRONIKA_OS_MAX_DISKS` devices, ordered by `(major, minor)`.
///
/// When the cap trims rows, a `collection_degraded` event with `reason=disk_cap`
/// records how many devices were dropped so the gap is visible, not silent.
fn apply_disk_cap(rows: &mut Vec<diskstats::DiskstatsRow>, type_id: u32) {
    let cap = os_max_disks(type_id);
    let cap = usize::try_from(cap).unwrap_or(usize::MAX);
    let dropped = cap_disks(rows, cap);
    if dropped == 0 {
        return;
    }
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", "diskstats"),
            field("reason", "disk_cap"),
            field("dropped", dropped),
            field("cap", cap),
        ],
    );
}

fn os_max_disks(type_id: u32) -> u64 {
    match env_u64("KRONIKA_OS_MAX_DISKS", 256) {
        Ok(cap) => cap,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "KRONIKA_OS_MAX_DISKS"),
                    field("reason", &err),
                    field("cap", 256_u64),
                ],
            );
            256
        }
    }
}

/// Trim `rows` to the `cap` lowest `(major, minor)` devices in place.
///
/// Returns the number of devices dropped (`0` when already within the cap).
fn cap_disks(rows: &mut Vec<diskstats::DiskstatsRow>, cap: usize) -> usize {
    if rows.len() <= cap {
        return 0;
    }
    rows.sort_by_key(|row| (row.major, row.minor));
    let dropped = rows.len() - cap;
    rows.truncate(cap);
    dropped
}

/// Read and parse `/proc/net/dev`, interning interface names into rows.
fn collect_netdev(fs: &ProcFs, interner: &mut Interner, scope: u8, ts: i64) -> Vec<OsNetdev> {
    let type_id = 1_109_001_u32;
    let started = Instant::now();
    let Some(content) = read_optional_os_file(fs, "net/dev", type_id) else {
        return Vec::new();
    };
    let rows = match net_dev::parse(&content) {
        Ok(rows) => rows,
        Err(err) => {
            log_degraded(type_id, "net/dev", &err.0);
            return Vec::new();
        }
    };
    let built: Vec<OsNetdev> = rows
        .iter()
        .filter_map(|row| {
            let iface = intern_str(interner, type_id, "net/dev", &row.iface)?;
            Some(row.to_section(scope, ts, iface))
        })
        .collect();
    log_collection_finish(type_id, "procfs", built.len(), started.elapsed());
    built
}

/// Read the two singleton network counter files into `os`.
fn collect_net_singletons(fs: &ProcFs, scope: u8, ts: i64, os: &mut OsSources) {
    let snmp_type_id = 1_110_001_u32;
    let started = Instant::now();
    if let Some(content) = read_optional_os_file(fs, "net/snmp", snmp_type_id) {
        match net_snmp::parse(&content) {
            Ok(row) => {
                os.snmp = Some(row.to_section(scope, ts));
                log_collection_finish(snmp_type_id, "procfs", 1, started.elapsed());
            }
            Err(err) => log_degraded(snmp_type_id, "net/snmp", &err.0),
        }
    }

    let netstat_type_id = 1_111_001_u32;
    let started = Instant::now();
    if let Some(content) = read_optional_os_file(fs, "net/netstat", netstat_type_id) {
        match net_netstat::parse(&content) {
            Ok(row) => {
                os.netstat = Some(row.to_section(scope, ts));
                log_collection_finish(netstat_type_id, "procfs", 1, started.elapsed());
            }
            Err(err) => log_degraded(netstat_type_id, "net/netstat", &err.0),
        }
    }
}

/// Read and parse `/proc/self/mountinfo`, resolving `major == 0` subvolume
/// devices via `/sys`.
fn mountinfo_entries(fs: &ProcFs) -> Vec<MountEntry> {
    let type_id = 1_112_001_u32;
    let Some(content) = read_optional_os_file(fs, "self/mountinfo", type_id) else {
        return Vec::new();
    };
    let mut entries = parse_mountinfo(&content);
    resolve_major_zero(&SysFs::from_env(), &mut entries);
    entries
}

/// Recover the real `(major, minor)` of `major == 0` subvolume mounts (btrfs,
/// ZFS) whose source is a `/dev/` node, by reading `class/block/<name>/dev`.
/// Entries that cannot be resolved keep `major == 0` and are dropped by
/// `device_map`/`container_device_set` downstream.
fn resolve_major_zero(sys: &SysFs, entries: &mut [MountEntry]) {
    for entry in entries.iter_mut().filter(|e| e.major == 0) {
        let Some(name) = entry.source.strip_prefix("/dev/") else {
            continue;
        };
        let rel = format!("class/block/{name}/dev");
        if let Ok(content) = sys.read(&rel)
            && let Some((major, minor)) = parse_dev_pair(&content)
        {
            entry.major = major;
            entry.minor = minor;
        }
    }
}

/// Build one `os_mountinfo` row per parsed mount entry.
///
/// Mount point, fstype, and source strings are interned here. Filesystem
/// capacity is nullable because `statvfs` can fail for pseudo-filesystems or
/// mounts that vanish during collection.
fn collect_mountinfo(
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    entries: &[MountEntry],
) -> Vec<OsMountinfo> {
    let type_id = 1_112_001_u32;
    let started = Instant::now();
    if entries.is_empty() {
        return Vec::new();
    }

    let mut rows = Vec::new();
    for entry in entries {
        let (Some(mount_point), Some(fstype), Some(source)) = (
            intern_str(interner, type_id, "self/mountinfo", &entry.mount_point),
            intern_str(interner, type_id, "self/mountinfo", &entry.fstype),
            intern_str(interner, type_id, "self/mountinfo", &entry.source),
        ) else {
            continue;
        };
        let space = statvfs(&entry.mount_point);
        rows.push(mount_row(
            entry,
            space,
            scope,
            ts,
            mount_point,
            fstype,
            source,
        ));
    }
    log_collection_finish(type_id, "procfs", rows.len(), started.elapsed());
    rows
}

/// Read `/proc/cpuinfo` and build one `os_topology` row per logical CPU.
///
/// On read or parse failure the section is skipped and a `collection_degraded`
/// event is logged; zeros are never fabricated.
fn collect_topology(
    fs: &ProcFs,
    sys: &SysFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
) -> Vec<OsTopology> {
    let type_id = 1_113_001_u32;
    let started = Instant::now();
    let Some(content) = read_optional_os_file(fs, "cpuinfo", type_id) else {
        return Vec::new();
    };
    let mut rows = match cpuinfo::parse(&content) {
        Ok(rows) => rows,
        Err(err) => {
            log_degraded(type_id, "cpuinfo", &err.0);
            return Vec::new();
        }
    };
    for row in &mut rows {
        row.mhz_max = cpu_max_mhz(sys, row.cpu_id);
    }
    let built: Vec<OsTopology> = rows
        .iter()
        .filter_map(|row| {
            let model_name_id = intern_str(interner, type_id, "cpuinfo", &row.model_name)?;
            Some(row.to_section(scope, ts, model_name_id))
        })
        .collect();
    log_collection_finish(type_id, "procfs", built.len(), started.elapsed());
    built
}

const fn os_entity_scope(in_container: bool) -> u8 {
    if in_container {
        OsScope::Container.as_u8()
    } else {
        OsScope::Host.as_u8()
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "process collection wires independent procfs files and degradation counters together"
)]
fn collect_process_sections(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    due: &DueSet,
    os: &mut OsSources,
) {
    let hot_due = due.has(SourceKind::OsProcesses);
    let status_due = due.has(SourceKind::OsProcessStatus);
    let mapping_due = due.has(SourceKind::OsCgroupMapping);
    if !hot_due && !status_due && !mapping_due {
        return;
    }

    let hot_type_id = 1_100_001_u32;
    let status_type_id = 1_101_001_u32;
    let mapping_type_id = 1_200_001_u32;
    let started = Instant::now();
    let facts = match process_facts(fs) {
        Ok(facts) => facts,
        Err(err) => {
            for type_id in [hot_type_id, status_type_id, mapping_type_id] {
                if (type_id == hot_type_id && hot_due)
                    || (type_id == status_type_id && status_due)
                    || (type_id == mapping_type_id && mapping_due)
                {
                    log_degraded(type_id, "process", &err);
                }
            }
            return;
        }
    };
    let max_procs = usize::try_from(os_max_procs(hot_type_id)).unwrap_or(usize::MAX);
    let capped = match fs.pid_dirs_capped(max_procs) {
        Ok(capped) => capped,
        Err(err) => {
            for type_id in [hot_type_id, status_type_id, mapping_type_id] {
                if (type_id == hot_type_id && hot_due)
                    || (type_id == status_type_id && status_due)
                    || (type_id == mapping_type_id && mapping_due)
                {
                    log_degraded(type_id, "process", &err);
                }
            }
            return;
        }
    };
    if capped.dropped > 0 {
        for type_id in [hot_type_id, status_type_id, mapping_type_id] {
            if (type_id == hot_type_id && hot_due)
                || (type_id == status_type_id && status_due)
                || (type_id == mapping_type_id && mapping_due)
            {
                log_cap_degraded(type_id, "process", "process_cap", capped.dropped, max_procs);
            }
        }
    }

    let mut skipped = 0_usize;
    let mut io_nulls = 0_usize;
    let mut mapping_nulls = 0_usize;
    for pid in capped.pids {
        let read = match read_process(fs, pid, facts, ts) {
            Ok(read) => read,
            Err(ProcessError::Gone(_)) => continue,
            Err(_) => {
                skipped = skipped.saturating_add(1);
                continue;
            }
        };
        if hot_due {
            if read.hot.io.is_none() {
                io_nulls = io_nulls.saturating_add(1);
            }
            let Some(comm) = intern_str(interner, hot_type_id, "process", &read.hot.comm) else {
                continue;
            };
            let cmdline = read
                .hot
                .cmdline
                .as_deref()
                .and_then(|value| intern_str(interner, hot_type_id, "process", value));
            os.processes
                .push(kronika_source_os::proc::process::to_hot_section(
                    &read.hot, scope, comm, cmdline,
                ));
        }
        if status_due {
            os.process_status
                .push(kronika_source_os::proc::process::to_status_section(
                    &read.status,
                    scope,
                ));
        }
        if mapping_due {
            if let Some(mapping) = read.cgroup {
                if let Some(cgroup_path) = intern_str(
                    interner,
                    mapping_type_id,
                    "process/cgroup",
                    &mapping.cgroup_path,
                ) {
                    os.cgroup_mapping.push(OsCgroupMapping {
                        ts: Ts(mapping.ts),
                        pid: mapping.pid,
                        starttime: Ts(mapping.starttime),
                        cgroup_path,
                        scope,
                    });
                }
            } else {
                mapping_nulls = mapping_nulls.saturating_add(1);
            }
        }
    }

    if skipped > 0 {
        for type_id in [hot_type_id, status_type_id, mapping_type_id] {
            if (type_id == hot_type_id && hot_due)
                || (type_id == status_type_id && status_due)
                || (type_id == mapping_type_id && mapping_due)
            {
                log_count_degraded(type_id, "process", "process_skipped", skipped);
            }
        }
    }
    if hot_due && io_nulls > 0 {
        log_count_degraded(
            hot_type_id,
            "process/io",
            "process_io_unavailable",
            io_nulls,
        );
    }
    if mapping_due && mapping_nulls > 0 {
        log_count_degraded(
            mapping_type_id,
            "process/cgroup",
            "process_cgroup_unavailable",
            mapping_nulls,
        );
    }
    if hot_due {
        log_collection_finish(hot_type_id, "procfs", os.processes.len(), started.elapsed());
    }
    if status_due {
        log_collection_finish(
            status_type_id,
            "procfs",
            os.process_status.len(),
            started.elapsed(),
        );
    }
    if mapping_due {
        log_collection_finish(
            mapping_type_id,
            "procfs",
            os.cgroup_mapping.len(),
            started.elapsed(),
        );
    }
}

fn collect_cgroup_sections(
    sys: &SysFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    fs: &ProcFs,
    due: &DueSet,
    os: &mut OsSources,
) {
    if !due.has(SourceKind::OsCgroup) {
        return;
    }

    let cpu_type_id = 1_201_001_u32;
    let memory_type_id = 1_202_001_u32;
    let io_type_id = 1_203_001_u32;
    let pids_type_id = 1_204_001_u32;
    let started = Instant::now();
    let clock_ticks = process_facts(fs).map_or_else(
        |err| {
            log_degraded(cpu_type_id, "cgroup", &err);
            0
        },
        |facts| facts.clock_ticks_per_sec,
    );
    let max_cgroups = usize::try_from(os_max_cgroups(cpu_type_id)).unwrap_or(usize::MAX);
    let max_io_rows = usize::try_from(os_max_cgroup_io_rows(io_type_id)).unwrap_or(usize::MAX);
    let max_depth = usize::try_from(os_cgroup_max_depth(cpu_type_id)).unwrap_or(usize::MAX);
    let rows = cgroup::collect(sys, ts, clock_ticks, max_cgroups, max_io_rows, max_depth);
    if rows.dropped_cgroups > 0 {
        for type_id in [cpu_type_id, memory_type_id, io_type_id, pids_type_id] {
            log_cap_degraded(
                type_id,
                "cgroup",
                "cgroup_cap",
                rows.dropped_cgroups,
                max_cgroups,
            );
        }
    }
    if rows.dropped_io_rows > 0 {
        log_cap_degraded(
            io_type_id,
            "cgroup/io",
            "cgroup_io_cap",
            rows.dropped_io_rows,
            max_io_rows,
        );
    }

    for row in &rows.cpu {
        if let Some(cgroup_path) = intern_str(interner, cpu_type_id, "cgroup/cpu", &row.cgroup_path)
        {
            os.cgroup_cpu
                .push(cgroup::to_cpu_section(row, scope, cgroup_path));
        }
    }
    for row in &rows.memory {
        if let Some(cgroup_path) =
            intern_str(interner, memory_type_id, "cgroup/memory", &row.cgroup_path)
        {
            os.cgroup_memory
                .push(cgroup::to_memory_section(row, scope, cgroup_path));
        }
    }
    for row in &rows.io {
        if let Some(cgroup_path) = intern_str(interner, io_type_id, "cgroup/io", &row.cgroup_path) {
            os.cgroup_io
                .push(cgroup::to_io_section(row, scope, cgroup_path));
        }
    }
    for row in &rows.pids {
        if let Some(cgroup_path) =
            intern_str(interner, pids_type_id, "cgroup/pids", &row.cgroup_path)
        {
            os.cgroup_pids
                .push(cgroup::to_pids_section(row, scope, cgroup_path));
        }
    }
    log_collection_finish(
        cpu_type_id,
        "cgroup",
        os.cgroup_cpu.len(),
        started.elapsed(),
    );
    log_collection_finish(
        memory_type_id,
        "cgroup",
        os.cgroup_memory.len(),
        started.elapsed(),
    );
    log_collection_finish(io_type_id, "cgroup", os.cgroup_io.len(), started.elapsed());
    log_collection_finish(
        pids_type_id,
        "cgroup",
        os.cgroup_pids.len(),
        started.elapsed(),
    );
}

fn cpu_max_mhz(sys: &SysFs, cpu_id: i32) -> Option<f64> {
    let rel = format!("devices/system/cpu/cpu{cpu_id}/cpufreq/cpuinfo_max_freq");
    let khz = sys.read(&rel).ok()?.parse::<f64>().ok()?;
    (khz.is_finite() && khz >= 0.0).then_some(khz / 1000.0)
}

/// Intern one OS string, logging degradation and returning `None` on failure so
/// the caller skips only the affected row.
fn intern_str(
    interner: &mut Interner,
    type_id: u32,
    source: &'static str,
    value: &str,
) -> Option<StrId> {
    match interner.intern(value.as_bytes()) {
        Ok(id) => Some(StrId(id.get())),
        Err(err) => {
            log_degraded(type_id, source, &err);
            None
        }
    }
}

/// Emit a `collection_degraded` event with the section identity and reason.
fn log_degraded(type_id: u32, source: &'static str, reason: &dyn std::fmt::Display) {
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", source),
            field("reason", reason),
        ],
    );
}

fn log_count_degraded(type_id: u32, source: &'static str, reason: &'static str, count: usize) {
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", source),
            field("reason", reason),
            field("count", count),
        ],
    );
}

fn log_cap_degraded(
    type_id: u32,
    source: &'static str,
    reason: &'static str,
    dropped: usize,
    cap: usize,
) {
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", source),
            field("reason", reason),
            field("dropped", dropped),
            field("cap", cap),
        ],
    );
}

fn os_max_procs(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_MAX_PROCS", 4096)
}

fn os_max_cgroups(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_MAX_CGROUPS", 1024)
}

fn os_max_cgroup_io_rows(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_MAX_CGROUP_IO_ROWS", 4096)
}

fn os_cgroup_max_depth(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_CGROUP_MAX_DEPTH", 8)
}

fn os_cap_from_env(type_id: u32, key: &'static str, default: u64) -> u64 {
    match env_u64(key, default) {
        Ok(cap) => cap,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", key),
                    field("reason", &err),
                    field("cap", default),
                ],
            );
            default
        }
    }
}

/// Buffer every collected OS section into the snapshot window.
///
/// Rows are pre-built with their string ids already interned, so this only
/// moves them into the buffers.
///
/// # Errors
/// Returns an error if a section buffer is full.
fn push_os_sources(buffers: &mut SectionBuffers, os: &OsSources) -> Result<()> {
    for row in &os.cpu {
        buffer_row(buffers, *row)?;
    }
    if let Some(row) = os.stat {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.meminfo {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.loadavg {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.vmstat {
        buffer_row(buffers, row)?;
    }
    for row in &os.psi {
        buffer_row(buffers, *row)?;
    }
    for row in &os.diskstats {
        buffer_row(buffers, *row)?;
    }
    for row in &os.netdev {
        buffer_row(buffers, *row)?;
    }
    if let Some(row) = os.snmp {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.netstat {
        buffer_row(buffers, row)?;
    }
    for row in &os.mountinfo {
        buffer_row(buffers, *row)?;
    }
    for row in &os.topology {
        buffer_row(buffers, *row)?;
    }
    for row in &os.processes {
        buffer_row(buffers, *row)?;
    }
    for row in &os.process_status {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_mapping {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_cpu {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_memory {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_io {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_pids {
        buffer_row(buffers, *row)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the snapshot wires every piece of daemon state together once"
)]
async fn snapshot_and_seal(
    pool: &ConnectionPool,
    major: u32,
    journal: &mut Journal,
    config: &Config,
    statements_cache: &mut StatementsSourceCache,
    plans_cache: &mut PlansSourceCache,
    log_collector: &mut LogCollector,
    due: &DueSet,
    segment: &mut SegmentState,
    pool_budget: &mut PoolBudget,
) -> Result<CycleOutcome> {
    // Run every query first: SectionBuffers and Interner are `!Send`, so they
    // must not be held across an await. Each source reads only when due.
    // The budget clock covers the whole cycle's database time; the sized pool
    // sources check it in survival order — statements first, indexes last —
    // so under pressure the most expensive source is deferred first.
    let cycle_start = Instant::now();
    let main_src = collect_main_conn_sources(pool.main(), major, config, due).await?;
    // Trigger verdicts come from the rows already collected — no extra queries.
    let activity_hot = main_src
        .activity
        .as_ref()
        .map(|(_, rows)| activity_needs_acceleration(rows, config.ash_active_threshold));
    let replication_hot = main_src
        .replication
        .as_ref()
        .map(|(instance, replicas, slots)| {
            replication_needs_acceleration(
                instance,
                replicas,
                slots,
                config.repl_lag_trigger_s,
                config.slot_retained_trigger_bytes,
            )
        });
    let PoolReads {
        statements,
        user_tables,
        tables_cov,
        user_indexes,
        indexes_cov,
        deferred,
    } = read_pool_sources(
        pool,
        major,
        config,
        statements_cache,
        due,
        pool_budget,
        cycle_start,
    )
    .await;
    let store_plans_rows =
        collect_store_plans_cached(pool, config, plans_cache, due.forced()).await;
    let coverage = collect_coverage_records(
        major,
        config,
        &CoverageInputs {
            tables: tables_cov,
            indexes: indexes_cov,
            statements: &statements,
            plans: &store_plans_rows,
        },
    );
    // The extension caches stay warm across ticks: reset metadata reads the
    // info views through the same connections those sections use.
    let service =
        collect_service_sections(pool, major, config, statements_cache, plans_cache, due).await?;
    let mut log_collection = if due.has(SourceKind::PgLog) {
        Some(collect_log_batch(log_collector, Some(pool.main()), main_src.ts.0).await)
    } else {
        None
    };

    // OS procfs read — synchronous, safe before SectionBuffers are built.
    // The interner is created here, before the OS read, because the Wave 2
    // sections (diskstats, netdev, mountinfo) intern device/interface/mount
    // strings while building their rows. The same interner then serves the PG
    // pushes below — one interner per window.
    let fs = ProcFs::from_env();
    let in_container = detect_container(&fs);
    log_event(
        LogLevel::Debug,
        "os_core_read",
        &[field("container", in_container)],
    );
    let scope = OsScope::Host.as_u8();
    let mut interner = Interner::new(activity_dict_limits());
    let os = collect_os_sources(&fs, &mut interner, scope, main_src.ts.0, in_container, due);

    let mut buffers = SectionBuffers::new();
    push_main_conn_sections(&mut buffers, &mut interner, major, &main_src)?;
    push_user_tables(&mut buffers, &mut interner, &user_tables)?;
    push_user_indexes(&mut buffers, &mut interner, &user_indexes)?;
    if let Some((version, rows, _total)) = &statements {
        push_statements(&mut buffers, &mut interner, *version, rows)?;
    }
    push_plans_read(
        &mut buffers,
        &mut interner,
        store_plans_rows.as_ref().map(|(read, _total)| read),
    )?;
    push_service_sections(&mut buffers, &mut interner, &service)?;
    push_os_sources(&mut buffers, &os)?;
    push_coverage(&mut buffers, &mut interner, main_src.ts.0, &coverage)?;
    if let Some(collection) = log_collection.as_mut() {
        let dropped = push_log_sections(&mut buffers, &mut interner, collection)?;
        if dropped != 0 {
            let first_new_gap = collection.gaps.len();
            log_collector.record_dictionary_drops(collection, main_src.ts.0, dropped);
            push_log_gaps(
                &mut buffers,
                &mut interner,
                &collection.gaps[first_new_gap..],
            )?;
            log_count_degraded(
                PG_LOG_GAP_TYPE_ID,
                "log",
                "dictionary_full",
                usize::try_from(dropped).unwrap_or(usize::MAX),
            );
        }
    }

    if buffers.is_empty() {
        // Every due source turned out empty (e.g. no vacuum in progress);
        // an empty tick appends nothing. The age valve in the main loop
        // still closes an expired segment on such ticks.
        commit_log_collection(log_collector, log_collection.as_ref());
        return Ok(CycleOutcome {
            sealed: Vec::new(),
            deferred,
            activity_hot,
            replication_hot,
        });
    }
    let flushed = encode_window(buffers, &interner, config)?;
    let sealed = append_window_and_maybe_seal(
        journal,
        config,
        segment,
        main_src.ts.0,
        due.forced(),
        &flushed,
    )
    .context("append the collection window")?;
    commit_log_collection(log_collector, log_collection.as_ref());
    Ok(CycleOutcome {
        sealed,
        deferred,
        activity_hot,
        replication_hot,
    })
}

async fn run_log_only_cycle(
    log_collector: &mut LogCollector,
    journal: &mut Journal,
    config: &Config,
    due: &DueSet,
    segment: &mut SegmentState,
) -> Result<Vec<(PathBuf, &'static str)>> {
    let ts = system_ts_us();
    let mut collection = collect_log_batch(log_collector, None, ts).await;
    let mut interner = Interner::new(activity_dict_limits());
    let mut buffers = SectionBuffers::new();
    let dropped = push_log_sections(&mut buffers, &mut interner, &collection)?;
    if dropped != 0 {
        let first_new_gap = collection.gaps.len();
        log_collector.record_dictionary_drops(&mut collection, ts, dropped);
        push_log_gaps(
            &mut buffers,
            &mut interner,
            &collection.gaps[first_new_gap..],
        )?;
        log_count_degraded(
            PG_LOG_GAP_TYPE_ID,
            "log",
            "dictionary_full",
            usize::try_from(dropped).unwrap_or(usize::MAX),
        );
    }
    if buffers.is_empty() {
        commit_log_collection(log_collector, Some(&collection));
        return Ok(Vec::new());
    }
    let flushed = encode_window(buffers, &interner, config)?;
    let sealed = append_window_and_maybe_seal(journal, config, segment, ts, due.forced(), &flushed)
        .context("append the log-only collection window")?;
    commit_log_collection(log_collector, Some(&collection));
    Ok(sealed)
}

async fn collect_log_batch(
    log_collector: &mut LogCollector,
    client: Option<&Client>,
    ts: i64,
) -> LogCollection {
    log_collection_start(PG_LOG_ERRORS_TYPE_ID, "log");
    let started = Instant::now();
    let collection = log_collector.collect(client, ts).await;
    log_collection_finish(
        PG_LOG_ERRORS_TYPE_ID,
        "log",
        collection.errors.len(),
        started.elapsed(),
    );
    if !collection.gaps.is_empty() {
        log_collection_finish(
            PG_LOG_GAP_TYPE_ID,
            "log",
            collection.gaps.len(),
            started.elapsed(),
        );
    }
    if let Some(status) = collection.discovery_status {
        log_event(
            LogLevel::Debug,
            "pg_log_discovery",
            &[
                field("status", discovery_status_name(status)),
                field("error_rows", collection.errors.len()),
                field("gap_rows", collection.gaps.len()),
                field("elapsed_ms", duration_ms(started.elapsed())),
            ],
        );
    }
    collection
}

const fn discovery_status_name(status: LogDiscoveryStatus) -> &'static str {
    match status {
        LogDiscoveryStatus::Available => "available",
        LogDiscoveryStatus::UnsupportedFormat => "unsupported_format",
        LogDiscoveryStatus::SourceUnavailable => "source_unavailable",
        LogDiscoveryStatus::QueryFailed => "query_failed",
        LogDiscoveryStatus::Disabled => "disabled",
    }
}

fn commit_log_collection(log_collector: &mut LogCollector, collection: Option<&LogCollection>) {
    let Some(collection) = collection else {
        return;
    };
    if let Err(err) = log_collector.commit(collection) {
        log_event(
            LogLevel::Error,
            "pg_log_state_commit_failure",
            &[field("error", &err)],
        );
    }
}

fn push_log_sections(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    collection: &LogCollection,
) -> Result<u32> {
    let mut dropped = 0_u32;
    for error in &collection.errors {
        dropped = dropped.saturating_add(push_log_error(buffers, interner, error)?);
    }
    dropped = dropped.saturating_add(push_log_gaps(buffers, interner, &collection.gaps)?);
    Ok(dropped)
}

fn push_log_error(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    error: &GroupedLogError,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let sqlstate = error
        .sqlstate
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let pattern = intern_log_text(interner, &error.pattern, MAX_PATTERN_BYTES, &mut dropped);
    let sample = intern_log_text(interner, &error.sample, MAX_TEXT_BYTES, &mut dropped);
    let detail = error
        .detail
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let hint = error
        .hint
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let context = error
        .context
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let statement = error
        .statement
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    buffer_row(
        buffers,
        PgLogErrorV1 {
            ts: Ts(error.ts),
            severity: error.severity.code(),
            category: error.category.code(),
            sqlstate,
            pattern,
            count: error.count,
            sample,
            detail,
            hint,
            context,
            statement,
            database: None,
            username: None,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}

fn push_log_gaps(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    gaps: &[LogGap],
) -> Result<u32> {
    let mut total_dropped = 0_u32;
    for gap in gaps {
        let mut dropped = 0_u32;
        let source_path = gap.source_path.as_ref().and_then(|path| {
            let value = path.to_string_lossy();
            intern_log_text(interner, &value, MAX_TEXT_BYTES, &mut dropped)
        });
        buffer_row(
            buffers,
            PgLogGapV1 {
                ts: Ts(gap.ts),
                source_path,
                parser_kind: gap.parser_kind.code(),
                reason: gap.reason.code(),
                dev: gap.dev,
                inode: gap.inode,
                offset: gap.offset,
                bytes_skipped: gap.bytes_skipped,
                truncated_lines: gap.truncated_lines,
                invalid_utf8: gap.invalid_utf8,
                binary_dropped: gap.binary_dropped,
                rotations: gap.rotations,
                missing_files: gap.missing_files,
                budget_exhaustions: gap.budget_exhaustions,
                dict_dropped_fields: gap.dict_dropped_fields.saturating_add(dropped),
                parser_dropped_lines: gap.parser_dropped_lines,
            },
        )?;
        total_dropped = total_dropped.saturating_add(dropped);
    }
    Ok(total_dropped)
}

fn intern_log_text(
    interner: &mut Interner,
    value: &str,
    max_bytes: usize,
    dropped: &mut u32,
) -> Option<StrId> {
    let value = truncate_log_text(value, max_bytes);
    interner.intern(value.as_bytes()).map_or_else(
        |_err| {
            *dropped = dropped.saturating_add(1);
            None
        },
        |id| Some(StrId(id.get())),
    )
}

fn truncate_log_text(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    if max_bytes == 0 {
        return "";
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.get(..end).unwrap_or_default()
}

fn append_window_and_maybe_seal(
    journal: &mut Journal,
    config: &Config,
    segment: &mut SegmentState,
    ts: i64,
    forced: bool,
    flushed: &FlushedPart,
) -> Result<Vec<(PathBuf, &'static str)>> {
    let mut sealed = Vec::new();
    let append_started = Instant::now();
    let journal_bytes_before = journal.bytes();
    match journal.append(&flushed.body) {
        Ok(part_ref) => log_journal_append(
            &flushed.summary,
            part_ref.offset,
            part_ref.len,
            journal_bytes_before,
            journal.bytes(),
            append_started.elapsed(),
            false,
        ),
        Err(JournalError::Full { len, max }) if segment.first_ts.is_some() => {
            log_event(
                LogLevel::Warn,
                "journal_full",
                &[
                    field("journal_bytes", len),
                    field("journal_max_bytes", max),
                    field("part_bytes", flushed.summary.part_bytes),
                    field("sections", flushed.summary.sections.len()),
                    field("section_rows", summary_rows(&flushed.summary)),
                ],
            );
            sealed.push((
                seal_open_segment(journal, config, segment, "journal-full")?,
                "journal-full",
            ));
            let retry_started = Instant::now();
            let journal_bytes_before = journal.bytes();
            let part_ref = journal
                .append(&flushed.body)
                .context("append the window after an early seal")?;
            log_journal_append(
                &flushed.summary,
                part_ref.offset,
                part_ref.len,
                journal_bytes_before,
                journal.bytes(),
                retry_started.elapsed(),
                true,
            );
        }
        Err(other) => {
            log_event(
                LogLevel::Error,
                "journal_append_failure",
                &[
                    field("part_bytes", flushed.summary.part_bytes),
                    field("sections", flushed.summary.sections.len()),
                    field("section_rows", summary_rows(&flushed.summary)),
                    field("journal_bytes_before", journal_bytes_before),
                    field("error", &other),
                    field("elapsed_ms", duration_ms(append_started.elapsed())),
                ],
            );
            return Err(anyhow::Error::new(other).context("append the part to the journal"));
        }
    }
    let now = Instant::now();
    segment.on_window_appended(ts, now);
    let age = Duration::from_secs(config.segment_max_age_secs);
    if let Some(reason) = seal_reason(
        forced,
        journal.bytes(),
        config.segment_max_bytes,
        segment.age_expired(now, age),
    ) {
        sealed.push((seal_open_segment(journal, config, segment, reason)?, reason));
    }
    Ok(sealed)
}

fn system_ts_us() -> i64 {
    let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    let micros = duration
        .as_secs()
        .saturating_mul(1_000_000)
        .saturating_add(u64::from(duration.subsec_micros()));
    i64::try_from(micros).unwrap_or(i64::MAX)
}

/// Everything one tick reads from the main connection, gated by `due`.
struct MainConnSources {
    ts: Ts,
    bgwriter: Option<kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer>,
    activity: Option<(ActivityVersion, Vec<ActivityRow>)>,
    database: Option<(DatabaseVersion, Vec<DatabaseRow>)>,
    progress_vacuum_rows: Vec<ProgressVacuumRow>,
    prepared_rows: Vec<PreparedXactsRow>,
    wal: Option<WalSnapshot>,
    io: Option<(IoVersion, Vec<IoRow>)>,
    archiver: Option<ArchiverRow>,
    replication: Option<(ReplicationInstanceRow, Vec<ReplicaRow>, Vec<SlotRow>)>,
    lock_rows: Vec<LocksRow>,
}

/// Read the due main-connection sources.
#[allow(
    clippy::too_many_lines,
    reason = "main-connection collection keeps each source's query, row count, and failure context adjacent"
)]
async fn collect_main_conn_sources(
    client: &Client,
    major: u32,
    config: &Config,
    due: &DueSet,
) -> Result<MainConnSources> {
    let ts = kronika_source_pg::snapshot_ts(client)
        .await
        .context("read the snapshot timestamp")?;
    let bgwriter = if due.has(SourceKind::Bgwriter) {
        let type_id = 1_006_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_bgwriter_checkpointer(client, major).await {
            Ok(row) => {
                log_collection_finish(type_id, "main", 1, started.elapsed());
                Some(row)
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_stat_bgwriter + pg_stat_checkpointer");
            }
        }
    } else {
        None
    };
    let activity = if due.has(SourceKind::Activity) {
        let started = Instant::now();
        let source = "main";
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Activity.field(), field("source", source)],
        );
        match collect_activity(client, major).await {
            Ok((version, rows)) => {
                let type_id = activity_type_id(version);
                log_collection_finish(type_id, source, rows.len(), started.elapsed());
                Some((version, rows))
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Activity.field(),
                        field("source", source),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_activity");
            }
        }
    } else {
        None
    };
    let database = if due.has(SourceKind::Database) {
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Database.field(), field("source", "main")],
        );
        match collect_database(client, major).await {
            Ok((version, rows)) => {
                let type_id = database_type_id(version);
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                Some((version, rows))
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Database.field(),
                        field("source", "main"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_database");
            }
        }
    } else {
        None
    };
    let progress_vacuum_rows = if due.has(SourceKind::ProgressVacuum) {
        let type_id = 1_012_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_progress_vacuum(client, major).await {
            Ok(rows) => {
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                rows
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_stat_progress_vacuum");
            }
        }
    } else {
        Vec::new()
    };
    let prepared_rows = if due.has(SourceKind::PreparedXacts) {
        let type_id = 1_010_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_prepared_xacts(client).await {
            Ok(rows) => {
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                rows
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_prepared_xacts");
            }
        }
    } else {
        Vec::new()
    };
    let wal = if due.has(SourceKind::Wal) {
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Wal.field(), field("source", "main")],
        );
        match collect_wal(client, major).await {
            Ok(Some(wal)) => {
                log_collection_finish(wal_type_id(&wal), "main", 1, started.elapsed());
                Some(wal)
            }
            Ok(None) => {
                log_event(
                    LogLevel::Debug,
                    "collection_skip",
                    &[
                        CollectionFamily::Wal.field(),
                        field("source", "main"),
                        field("server_major", major),
                        field("reason", "unsupported_server_major"),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                None
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Wal.field(),
                        field("source", "main"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_wal");
            }
        }
    } else {
        None
    };
    let io = if due.has(SourceKind::Io) {
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Io.field(), field("source", "main")],
        );
        match collect_io(client, major).await {
            Ok(Some((version, rows))) => {
                log_collection_finish(io_type_id(version), "main", rows.len(), started.elapsed());
                Some((version, rows))
            }
            Ok(None) => {
                log_event(
                    LogLevel::Debug,
                    "collection_skip",
                    &[
                        CollectionFamily::Io.field(),
                        field("source", "main"),
                        field("server_major", major),
                        field("reason", "unsupported_server_major"),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                None
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Io.field(),
                        field("source", "main"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_io");
            }
        }
    } else {
        None
    };
    let archiver = if due.has(SourceKind::Archiver) {
        let type_id = 1_008_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_archiver(client).await {
            Ok(row) => {
                log_collection_finish(type_id, "main", 1, started.elapsed());
                Some(row)
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_stat_archiver");
            }
        }
    } else {
        None
    };
    let replication = if due.has(SourceKind::Replication) {
        let started = Instant::now();
        log_collection_start(1_015_001, "main");
        let instance_row = match collect_replication_instance(client, major).await {
            Ok(row) => {
                log_collection_finish(1_015_001, "main", 1, started.elapsed());
                row
            }
            Err(err) => {
                log_collection_failure(1_015_001, "main", &err, started.elapsed());
                return Err(err).context("collect replication instance status");
            }
        };
        log_collection_start(1_016_001, "main");
        log_collection_start(1_017_001, "main");
        let details_started = Instant::now();
        let (replica_rows, slot_rows) = collect_replication_details(client, major).await?;
        log_collection_finish(
            1_016_001,
            "main",
            replica_rows.len(),
            details_started.elapsed(),
        );
        log_collection_finish(
            1_017_001,
            "main",
            slot_rows.len(),
            details_started.elapsed(),
        );
        Some((instance_row, replica_rows, slot_rows))
    } else {
        None
    };
    // The lock-wait graph has no interval of its own: the freshest activity
    // snapshot already says whether any backend waits on a heavyweight lock.
    let lock_rows = match &activity {
        Some((_, rows)) if activity_has_lock_waiters(rows) => {
            collect_lock_rows(client, major, config.max_lock_rows).await
        }
        _ => {
            let type_id = locks_type_id(locks_version(major));
            log_event(
                LogLevel::Debug,
                "collection_skip",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "main"),
                    field("reason", "no_lock_waiters"),
                ],
            );
            Vec::new()
        }
    };
    Ok(MainConnSources {
        ts,
        bgwriter,
        activity,
        database,
        progress_vacuum_rows,
        prepared_rows,
        wal,
        io,
        archiver,
        replication,
        lock_rows,
    })
}

/// Buffer the main-connection sections that were read this tick.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_main_conn_sections(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    major: u32,
    src: &MainConnSources,
) -> Result<()> {
    if let Some(bgwriter) = src.bgwriter {
        buffer_row(buffers, bgwriter)?;
    }
    if let Some((activity_version, activity_rows)) = &src.activity {
        push_activity(buffers, interner, *activity_version, activity_rows)?;
    }
    if let Some((database_version, database_rows)) = &src.database {
        push_database(buffers, interner, *database_version, database_rows)?;
    }
    push_progress_vacuum(buffers, interner, &src.progress_vacuum_rows)?;
    push_prepared_xacts(buffers, interner, &src.prepared_rows)?;
    push_wal(buffers, src.wal)?;
    if let Some((io_version, io_rows)) = &src.io {
        push_io(buffers, interner, *io_version, io_rows)?;
    }
    if let Some(archiver) = &src.archiver {
        push_archiver(buffers, interner, archiver)?;
    }
    if let Some((instance_row, replica_rows, slot_rows)) = &src.replication {
        push_replication_instance(buffers, interner, instance_row)?;
        push_replication_details(buffers, interner, replica_rows, slot_rows)?;
    }
    if !src.lock_rows.is_empty() {
        push_locks(buffers, interner, locks_version(major), &src.lock_rows)?;
    }
    Ok(())
}

/// Whether the activity snapshot shows a backend waiting on a heavyweight
/// lock — the free precheck for the lock-wait graph.
fn activity_has_lock_waiters(rows: &[ActivityRow]) -> bool {
    rows.iter()
        .any(|row| row.wait_event_type.as_deref() == Some("Lock"))
}

/// Whether the activity snapshot justifies the accelerated pace: a backend
/// waits on a heavyweight lock, or active client backends reach `threshold`.
fn activity_needs_acceleration(rows: &[ActivityRow], threshold: usize) -> bool {
    if activity_has_lock_waiters(rows) {
        return true;
    }
    let active_clients = rows
        .iter()
        .filter(|row| {
            row.backend_type == "client backend" && row.state.as_deref() == Some("active")
        })
        .count();
    active_clients >= threshold
}

/// Whether the replication snapshot justifies the accelerated pace: a replica
/// or this standby replays behind `lag_trigger_s`, or a slot retains at least
/// `retained_trigger_bytes` of WAL.
fn replication_needs_acceleration(
    instance: &ReplicationInstanceRow,
    replicas: &[ReplicaRow],
    slots: &[SlotRow],
    lag_trigger_s: i64,
    retained_trigger_bytes: i64,
) -> bool {
    if instance
        .replay_lag_s
        .is_some_and(|lag| lag >= lag_trigger_s)
    {
        return true;
    }
    let replica_lag_floor_us = lag_trigger_s.saturating_mul(1_000_000);
    if replicas.iter().any(|replica| {
        replica
            .replay_lag_us
            .is_some_and(|lag| lag >= replica_lag_floor_us)
    }) {
        return true;
    }
    slots.iter().any(|slot| {
        slot.retained_bytes
            .is_some_and(|bytes| bytes >= retained_trigger_bytes)
    })
}

/// The open (not yet sealed) segment: its file name comes from the first
/// window's timestamp, its age from the moment that window was appended.
#[derive(Debug, Default, Clone, Copy)]
struct SegmentState {
    first_ts: Option<i64>,
    opened_at: Option<Instant>,
}

impl SegmentState {
    /// Register the appended window; the first one opens the segment.
    const fn on_window_appended(&mut self, ts: i64, now: Instant) {
        if self.first_ts.is_none() {
            self.first_ts = Some(ts);
            self.opened_at = Some(now);
        }
    }

    /// Whether the open segment has reached `max_age`.
    fn age_expired(&self, now: Instant, max_age: Duration) -> bool {
        self.opened_at
            .is_some_and(|opened| now.duration_since(opened) >= max_age)
    }

    fn time_until_age(&self, now: Instant, max_age: Duration) -> Option<Duration> {
        Some(max_age.saturating_sub(now.saturating_duration_since(self.opened_at?)))
    }
}

/// Why the open segment must seal now, or `None` to keep collecting.
///
/// Forced ticks seal immediately, `max_bytes = 0` keeps the legacy one-tick
/// segment mode, and otherwise the raw journal size or segment age closes the
/// segment.
const fn seal_reason(
    forced: bool,
    journal_bytes: usize,
    max_bytes: u64,
    age_expired: bool,
) -> Option<&'static str> {
    if forced {
        Some("forced")
    } else if max_bytes == 0 {
        Some("tick")
    } else if journal_bytes as u64 >= max_bytes {
        Some("size")
    } else if age_expired {
        Some("age")
    } else {
        None
    }
}

/// Encode the buffered window into one journal-ready part.
fn encode_window(
    mut buffers: SectionBuffers,
    interner: &Interner,
    config: &Config,
) -> Result<FlushedPart> {
    let started = Instant::now();
    let dict_sections = dict::encode(interner.window()).context("encode the segment dictionary")?;
    let flushed = buffers
        .flush_with_summary(&dict_sections, config.source_id)
        .context("encode the collection window")?
        .context("a buffered row must yield a part")?;
    log_flush_summary(&flushed.summary, config.source_id, started.elapsed());
    Ok(flushed)
}

/// Seal the open segment into `<first_ts>.pgm` and reset the journal.
fn seal_open_segment(
    journal: &mut Journal,
    config: &Config,
    segment: &mut SegmentState,
    reason: &'static str,
) -> Result<PathBuf> {
    let first_ts = segment
        .first_ts
        .context("sealing an open segment requires an appended window")?;
    let dest = config.out_dir.join(format!("{first_ts}.pgm"));
    let journal_bytes = journal.bytes();
    let journal_parts = journal.parts().len();
    let started = Instant::now();
    let summary = seal(journal, &dest).context("seal the segment")?;
    log_event(
        LogLevel::Info,
        "segment_seal_finish",
        &[
            field("segment_path", dest.display()),
            field("segment_id", first_ts),
            field("source_id", config.source_id),
            field("reason", reason),
            field("sections", summary.sections),
            field("segment_bytes", summary.bytes),
            field("journal_bytes", journal_bytes),
            field("journal_parts", journal_parts),
            field("min_ts", summary.min_ts),
            field("max_ts", summary.max_ts),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    // Leave active.parts intact if seal() fails.
    journal.reset().context("reset the journal after seal")?;
    *segment = SegmentState::default();
    Ok(dest)
}

/// Open the journal under the output directory and seal windows a previous
/// process left behind, so a restart loses no collected data.
fn open_collector_journal(
    out_dir: &Path,
    journal_max_bytes: u64,
) -> Result<(Journal, Option<PathBuf>)> {
    let journal_config = JournalConfig {
        max_journal_len: usize::try_from(journal_max_bytes)
            .context("KRONIKA_JOURNAL_MAX_BYTES exceeds usize")?,
        ..JournalConfig::default()
    };
    let (mut journal, report) =
        Journal::open(&out_dir.join("active.parts"), journal_config).context("open the journal")?;
    if report.has_media_damage() {
        log_event(
            LogLevel::Warn,
            "journal_recovery_damaged",
            &[
                field("damaged_regions", report.damages.len()),
                field("truncated_torn_tail", report.truncated_torn_tail),
            ],
        );
    }
    if journal.parts().is_empty() {
        return Ok((journal, None));
    }
    match seal_recovered_journal(&mut journal, out_dir) {
        Ok(dest) => Ok((journal, dest)),
        // A journal this binary cannot re-read (e.g. written by an
        // incompatible version) must not stop the daemon: drop it and start
        // collecting fresh.
        Err(err) => {
            log_event(
                LogLevel::Error,
                "journal_recovery_seal_failure",
                &[
                    field("journal_bytes", journal.bytes()),
                    field("journal_parts", journal.parts().len()),
                    field("error", format!("{err:#}")),
                ],
            );
            journal
                .reset()
                .context("reset the journal after a failed recovery seal")?;
            Ok((journal, None))
        }
    }
}

/// Seal recovered windows under the earliest data timestamp they carry.
///
/// Parts without a data timestamp hold no rows (a dictionary needs a data
/// section to be referenced from), so a journal made only of those is reset
/// without producing a segment.
fn seal_recovered_journal(journal: &mut Journal, out_dir: &Path) -> Result<Option<PathBuf>> {
    let mut first_ts: Option<i64> = None;
    for part in journal.parts().to_vec() {
        let body = journal.read_part(part).context("read a recovered part")?;
        let catalog = kronika_format::validate_part(&body).context("validate a recovered part")?;
        if catalog.min_ts != i64::MAX {
            first_ts = Some(first_ts.map_or(catalog.min_ts, |ts| ts.min(catalog.min_ts)));
        }
    }
    let Some(first_ts) = first_ts else {
        log_event(
            LogLevel::Info,
            "journal_recovery_empty",
            &[
                field("journal_bytes", journal.bytes()),
                field("journal_parts", journal.parts().len()),
                field("reason", "no_timestamped_sections"),
            ],
        );
        journal
            .reset()
            .context("reset a recovered journal with no data windows")?;
        return Ok(None);
    };
    let dest = out_dir.join(format!("{first_ts}.pgm"));
    let journal_bytes = journal.bytes();
    let journal_parts = journal.parts().len();
    let started = Instant::now();
    let summary = seal(journal, &dest).context("seal the recovered segment")?;
    log_event(
        LogLevel::Info,
        "segment_seal_finish",
        &[
            field("segment_path", dest.display()),
            field("segment_id", first_ts),
            field("reason", "recovered"),
            field("sections", summary.sections),
            field("segment_bytes", summary.bytes),
            field("journal_bytes", journal_bytes),
            field("journal_parts", journal_parts),
            field("min_ts", summary.min_ts),
            field("max_ts", summary.max_ts),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    journal
        .reset()
        .context("reset the journal after the recovery seal")?;
    Ok(Some(dest))
}

/// Buffer the `pg_stat_wal` singleton; PG10-13 produce no row.
///
/// # Errors
/// Returns an error when the section buffer is full.
fn push_wal(buffers: &mut SectionBuffers, wal: Option<WalSnapshot>) -> Result<()> {
    match wal {
        Some(WalSnapshot::V1(row)) => buffer_row(buffers, row),
        Some(WalSnapshot::V2(row)) => buffer_row(buffers, row),
        None => Ok(()),
    }
}

/// Buffer the paced `pg_store_plans` read under its fork's section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or the
/// section buffer is full.
fn push_plans_read(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    read: Option<&PlansRead>,
) -> Result<()> {
    match read {
        Some(PlansRead::Vadv(rows)) => push_store_plans(buffers, interner, rows),
        Some(PlansRead::Ossc(rows)) => push_store_plans_ossc(buffers, interner, rows),
        None => Ok(()),
    }
}

/// Inputs needed to assemble coverage for this snapshot's top-N reads.
struct CoverageInputs<'a> {
    tables: SourceCoverage,
    indexes: SourceCoverage,
    statements: &'a Option<(StatementsVersion, Vec<StatementsRow>, u64)>,
    plans: &'a Option<(PlansRead, u64)>,
}

/// Assemble the `1_023_001` rows for every truncated top-N source.
fn collect_coverage_records(
    major: u32,
    config: &Config,
    inputs: &CoverageInputs<'_>,
) -> Vec<CoverageRecord> {
    let mut records = Vec::new();
    if inputs.tables.truncated() {
        records.push(CoverageRecord {
            source_type_id: user_tables_type_id(major),
            coverage: inputs.tables,
            max_n: u32::try_from(config.max_tables).unwrap_or(u32::MAX),
            order_by: "reads|writes|relpages|n_dead_tup|xid_age|mxid_age",
            cutoff_value: None,
        });
    }
    if inputs.indexes.truncated() {
        records.push(CoverageRecord {
            source_type_id: user_indexes_type_id(major),
            coverage: inputs.indexes,
            max_n: u32::try_from(config.max_indexes).unwrap_or(u32::MAX),
            order_by: user_indexes_order_by(major),
            cutoff_value: None,
        });
    }
    if let Some(record) = statements_coverage(config, inputs) {
        records.push(record);
    }
    if let Some(record) = plans_coverage(config, inputs) {
        records.push(record);
    }
    records
}

/// Coverage for the collected `pg_stat_statements` read, if it was truncated.
///
/// The total rides in the same statement as the collected rows, so it
/// describes exactly the population they were cut from.
fn statements_coverage(config: &Config, inputs: &CoverageInputs<'_>) -> Option<CoverageRecord> {
    let (version, rows, source_total) = inputs.statements.as_ref()?;
    let coverage = SourceCoverage {
        total: *source_total,
        collected: rows.len() as u64,
        unknown_total: false,
        timeouts: 0,
        permission_skips: 0,
        other_skips: 0,
    };
    coverage.truncated().then(|| CoverageRecord {
        source_type_id: statements_type_id(*version),
        coverage,
        max_n: u32::try_from(config.max_statements).unwrap_or(u32::MAX),
        order_by: "total_exec_time|calls",
        cutoff_value: None,
    })
}

/// Coverage for the collected `pg_store_plans` read, if it was truncated.
///
/// The single selection axis makes the boundary meaningful: `cutoff_value`
/// is the smallest `total_time` that still made it into the section. The
/// total rides in the enumeration statement itself.
fn plans_coverage(config: &Config, inputs: &CoverageInputs<'_>) -> Option<CoverageRecord> {
    let (read, source_total) = inputs.plans.as_ref()?;
    let (collected, cutoff_value) = match read {
        PlansRead::Vadv(rows) => (
            rows.len() as u64,
            min_total_time(rows.iter().map(|r| r.total_time)),
        ),
        PlansRead::Ossc(rows) => (
            rows.len() as u64,
            min_total_time(rows.iter().map(|r| r.total_time)),
        ),
    };
    let coverage = SourceCoverage {
        total: *source_total,
        collected,
        unknown_total: false,
        timeouts: 0,
        permission_skips: 0,
        other_skips: 0,
    };
    coverage.truncated().then(|| CoverageRecord {
        source_type_id: read.type_id(),
        coverage,
        max_n: u32::try_from(config.max_plans).unwrap_or(u32::MAX),
        order_by: "total_time",
        cutoff_value,
    })
}

/// The smallest selection metric among the collected rows; `None` when empty.
fn min_total_time(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.fold(None, |acc, v| {
        Some(acc.map_or(v, |a: f64| if v < a { v } else { a }))
    })
}

/// Buffer one `1_023_001` row per truncated source.
///
/// # Errors
/// Returns an error if `order_by` cannot be interned (dictionary full) or the
/// section buffer is full.
fn push_coverage(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    ts: i64,
    records: &[CoverageRecord],
) -> Result<()> {
    for record in records {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        let row = CollectionCoverageV1 {
            ts: Ts(ts),
            source_type_id: record.source_type_id,
            total: u32::try_from(record.coverage.total).unwrap_or(u32::MAX),
            unknown_total: record.coverage.unknown_total,
            collected: u32::try_from(record.coverage.collected).unwrap_or(u32::MAX),
            max_n: record.max_n,
            order_by: intern(record.order_by.as_bytes())?,
            cutoff_value: record.cutoff_value,
            reason: record.coverage.reason(),
        };
        buffer_row(buffers, row)?;
    }
    Ok(())
}

/// Service rows gated by their scheduler intervals.
struct ServiceSections {
    reset: Option<(ResetBase, ResetExtensions)>,
    instance: Option<InstanceFacts>,
    settings: Vec<SettingsRow>,
}

/// Collect the due service sections.
async fn collect_service_sections(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    statements_cache: &StatementsSourceCache,
    plans_cache: &PlansSourceCache,
    due: &DueSet,
) -> Result<ServiceSections> {
    let reset = if due.has(SourceKind::ResetMetadata) {
        Some(collect_reset_metadata_all(pool, major, statements_cache, plans_cache).await?)
    } else {
        None
    };
    let instance = if due.has(SourceKind::InstanceMetadata) {
        Some(collect_instance_facts(pool.main(), config).await?)
    } else {
        None
    };
    let settings = if due.has(SourceKind::Settings) {
        let type_id = 1_019_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        let settings = match collect_settings(pool.main()).await {
            Ok(settings) => {
                log_collection_finish(type_id, "main", settings.len(), started.elapsed());
                settings
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_settings");
            }
        };
        validate_settings_row_count(settings.len())?;
        settings
    } else {
        Vec::new()
    };
    Ok(ServiceSections {
        reset,
        instance,
        settings,
    })
}

/// Buffer the service sections collected for this tick.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_service_sections(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    service: &ServiceSections,
) -> Result<()> {
    if let Some((reset_base, reset_ext)) = &service.reset {
        push_reset_metadata(buffers, interner, reset_base, reset_ext)?;
    }
    if let Some(instance) = &service.instance {
        push_instance_metadata(buffers, interner, instance)?;
    }
    push_settings(buffers, interner, &service.settings)
}

/// Assemble `reset_metadata`: the base from the main connection plus the
/// extension info views read through the discovered statements and plans
/// sources. An info-view failure degrades that one timestamp to `NULL`.
async fn collect_reset_metadata_all(
    pool: &ConnectionPool,
    major: u32,
    statements_cache: &StatementsSourceCache,
    plans_cache: &PlansSourceCache,
) -> Result<(ResetBase, ResetExtensions)> {
    let type_id = 1_020_001;
    let started = Instant::now();
    log_collection_start(type_id, "main");
    let base = match collect_reset_base(pool.main(), major).await {
        Ok(base) => {
            log_collection_finish(type_id, "main", 1, started.elapsed());
            base
        }
        Err(err) => {
            log_collection_failure(type_id, "main", &err, started.elapsed());
            return Err(err).context("collect reset metadata");
        }
    };
    let mut ext = ResetExtensions::default();
    if let Some(cached) = &statements_cache.selected {
        ext.statements_version = Some(cached.extversion.clone());
        if let Some(client) = statement_client(pool, &cached.source) {
            match statements_reset_at(client).await {
                Ok(reset) => ext.statements_reset_at = reset,
                Err(err) => log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", cached.source.label()),
                        field("reason", "pg_stat_statements_info_failed"),
                        field("error", &err),
                    ],
                ),
            }
        }
    }
    if let Some(cached) = &plans_cache.selected {
        ext.store_plans_version = Some(cached.extversion.clone());
        if let Some(client) = statement_client(pool, &cached.source) {
            match store_plans_reset_at(client).await {
                Ok(reset) => ext.store_plans_reset_at = reset,
                Err(err) => log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", cached.source.label()),
                        field("reason", "pg_store_plans_info_failed"),
                        field("error", &err),
                    ],
                ),
            }
        }
    }
    Ok((base, ext))
}

/// Fields written to `instance_metadata`, joined from `PostgreSQL` and the host.
#[derive(Debug)]
struct InstanceFacts {
    pg: PgInstanceFacts,
    /// `None` when `pg_control_system()` is not executable under this role.
    system_identifier: Option<i64>,
    os: OsInstanceFacts,
    node_self_id: String,
}

/// Collect the instance fingerprint; only the system identifier may degrade.
async fn collect_instance_facts(client: &Client, config: &Config) -> Result<InstanceFacts> {
    let type_id = 1_021_001;
    let started = Instant::now();
    log_collection_start(type_id, "main");
    let pg = match collect_pg_instance_facts(client).await {
        Ok(pg) => pg,
        Err(err) => {
            log_collection_failure(type_id, "main", &err, started.elapsed());
            return Err(err).context("collect instance metadata");
        }
    };
    let system_identifier = match pg_system_identifier(client).await {
        Ok(id) => Some(id),
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "main"),
                    field("reason", "pg_control_system_unavailable"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            None
        }
    };
    let os = collect_os_instance_facts().context("collect OS instance facts")?;
    let node_self_id = config
        .node_self_id
        .clone()
        .unwrap_or_else(|| os.hostname.clone());
    let facts = InstanceFacts {
        pg,
        system_identifier,
        os,
        node_self_id,
    };
    log_collection_finish(type_id, "main", 1, started.elapsed());
    Ok(facts)
}

/// Collect lock-wait rows or degrade by skipping only section `1_011`.
async fn collect_lock_rows(client: &Client, major: u32, max_lock_rows: i64) -> Vec<LocksRow> {
    let type_id = locks_type_id(locks_version(major));
    let started = Instant::now();
    log_collection_start(type_id, "main");
    match collect_locks(client, major, max_lock_rows).await {
        Ok(snapshot) => {
            if let Some(skipped) = snapshot.skipped {
                log_event(
                    LogLevel::Warn,
                    "collection_skip",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", "main"),
                        field("reason", "lock_graph_too_large"),
                        field("max_rows", skipped.max_rows),
                        field("waiters", skipped.waiters),
                        field("edges", skipped.edges),
                        field("nodes", skipped.nodes),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                Vec::new()
            } else {
                log_collection_finish(type_id, "main", snapshot.rows.len(), started.elapsed());
                snapshot.rows
            }
        }
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_skip",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "main"),
                    field("reason", "query_failed"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            Vec::new()
        }
    }
}

/// Limits for interned activity strings.
///
/// Query text can dominate the dictionary. Long values spill to `dict.blobs`,
/// truncate after 64 KiB, and the dictionary is capped at 16 MiB.
fn activity_dict_limits() -> DictLimits {
    DictLimits::new(4096, 64 * 1024)
        .and_then(|limits| limits.with_max_total_bytes(16 * 1024 * 1024))
        .expect("static activity dictionary limits satisfy 0 < blob <= truncate <= total")
}

/// Intern each row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_activity(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: ActivityVersion,
    rows: &[ActivityRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            ActivityVersion::V1 => buffer_row(buffers, to_v1(row, &mut intern)?)?,
            ActivityVersion::V2 => buffer_row(buffers, to_v2(row, &mut intern)?)?,
            ActivityVersion::V3 => buffer_row(buffers, to_v3(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each row's `datname` and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if `datname` cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_database(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: DatabaseVersion,
    rows: &[DatabaseRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            DatabaseVersion::V1 => buffer_row(buffers, database::to_v1(row, &mut intern)?)?,
            DatabaseVersion::V2 => buffer_row(buffers, database::to_v2(row, &mut intern)?)?,
            DatabaseVersion::V3 => buffer_row(buffers, database::to_v3(row, &mut intern)?)?,
            DatabaseVersion::V4 => buffer_row(buffers, database::to_v4(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each table row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_user_tables(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    collected: &[(String, UserTablesVersion, Vec<UserTablesRow>)],
) -> Result<()> {
    for (datname, version, rows) in collected {
        for row in rows {
            let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
            match version {
                UserTablesVersion::V1 => {
                    buffer_row(buffers, user_tables::to_v1(row, datname, &mut intern)?)?;
                }
                UserTablesVersion::V2 => {
                    buffer_row(buffers, user_tables::to_v2(row, datname, &mut intern)?)?;
                }
                UserTablesVersion::V3 => {
                    buffer_row(buffers, user_tables::to_v3(row, datname, &mut intern)?)?;
                }
                UserTablesVersion::V4 => {
                    buffer_row(buffers, user_tables::to_v4(row, datname, &mut intern)?)?;
                }
            }
        }
    }
    Ok(())
}

/// Intern each index row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_user_indexes(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    collected: &[(String, UserIndexesVersion, Vec<UserIndexesRow>)],
) -> Result<()> {
    for (datname, version, rows) in collected {
        for row in rows {
            let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
            match version {
                UserIndexesVersion::V1 => {
                    buffer_row(buffers, user_indexes::to_v1(row, datname, &mut intern)?)?;
                }
                UserIndexesVersion::V2 => {
                    buffer_row(buffers, user_indexes::to_v2(row, datname, &mut intern)?)?;
                }
            }
        }
    }
    Ok(())
}

/// Intern each statement row's strings and buffer it as the version's section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_statements(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: StatementsVersion,
    rows: &[StatementsRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            StatementsVersion::V1 => buffer_row(buffers, statements::to_v1(row, &mut intern)?)?,
            StatementsVersion::V2 => buffer_row(buffers, statements::to_v2(row, &mut intern)?)?,
            StatementsVersion::V3 => buffer_row(buffers, statements::to_v3(row, &mut intern)?)?,
            StatementsVersion::V4 => buffer_row(buffers, statements::to_v4(row, &mut intern)?)?,
            StatementsVersion::V5 => buffer_row(buffers, statements::to_v5(row, &mut intern)?)?,
            StatementsVersion::V6 => buffer_row(buffers, statements::to_v6(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern the two settings strings and buffer the instance replication row.
///
/// # Errors
/// Returns an error if a setting cannot be interned (dictionary full) or the
/// section buffer is full.
fn push_replication_instance(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    row: &ReplicationInstanceRow,
) -> Result<()> {
    let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    buffer_row(buffers, to_replication_instance(row, &mut intern)?)
}

/// Intern each row's labels and buffer it as the progress-vacuum section.
///
/// # Errors
/// Returns an error if a label cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_progress_vacuum(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[ProgressVacuumRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_progress_vacuum(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern each row's `datname` and buffer it as the prepared-xacts section.
///
/// # Errors
/// Returns an error if `datname` cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_prepared_xacts(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[PreparedXactsRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_prepared_xacts(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern WAL file names and buffer the singleton `pg_stat_archiver` row.
///
/// # Errors
/// Returns an error if a WAL name cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_archiver(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    row: &ArchiverRow,
) -> Result<()> {
    let intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    buffer_row(buffers, to_archiver(row, intern)?)
}

/// Intern each row's label strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a label cannot be interned (dictionary full) or a section
/// buffer is full.
fn push_io(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: IoVersion,
    rows: &[IoRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            IoVersion::V1 => buffer_row(buffers, io::to_v1(row, &mut intern)?)?,
            IoVersion::V2 => buffer_row(buffers, io::to_v2(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each row's strings and buffer it as the version's lock-wait section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_locks(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: LocksVersion,
    rows: &[LocksRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            LocksVersion::V1 => buffer_row(buffers, locks_to_v1(row, &mut intern)?)?,
            LocksVersion::V2 => buffer_row(buffers, locks_to_v2(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern the label strings and buffer the singleton `reset_metadata` row.
///
/// # Errors
/// Returns an error if a label cannot be interned (dictionary full) or the
/// section buffer is full.
fn push_reset_metadata(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    base: &ResetBase,
    ext: &ResetExtensions,
) -> Result<()> {
    let intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    buffer_row(buffers, to_reset_metadata(base, ext, intern)?)
}

/// Intern the identity strings and buffer the singleton `instance_metadata`
/// row.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or the
/// section buffer is full.
fn push_instance_metadata(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    facts: &InstanceFacts,
) -> Result<()> {
    let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    let row = InstanceMetadata {
        ts: Ts(facts.pg.ts),
        hostname: intern(facts.os.hostname.as_bytes())?,
        node_self_id: intern(facts.node_self_id.as_bytes())?,
        pg_version_num: facts.pg.pg_version_num,
        kernel_version: intern(facts.os.kernel_version.as_bytes())?,
        pg_system_identifier: facts.system_identifier,
        clock_ticks_per_sec: facts.os.clock_ticks_per_sec,
        page_size_bytes: facts.os.page_size_bytes,
        boot_id: intern(facts.os.boot_id.as_bytes())?,
        btime: Ts(facts.os.btime),
    };
    buffer_row(buffers, row)
}

/// Collect the walsender and slot detail rows from the main connection.
async fn collect_replication_details(
    client: &Client,
    major: u32,
) -> Result<(Vec<ReplicaRow>, Vec<SlotRow>)> {
    let started = Instant::now();
    let bounds = match collect_replication_detail_bounds(client).await {
        Ok(bounds) => bounds,
        Err(err) => {
            log_event(
                LogLevel::Error,
                "collection_failure",
                &[
                    CollectionFamily::ReplicationDetails.field(),
                    field("source", "main"),
                    field("reason", "bounds_query_failed"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            return Err(err).context("collect replication detail row bounds");
        }
    };
    if let Err(err) = validate_replication_detail_bounds(bounds) {
        log_event(
            LogLevel::Error,
            "collection_failure",
            &[
                CollectionFamily::ReplicationDetails.field(),
                field("source", "main"),
                field("reason", "bounds_validation_failed"),
                field("error", format!("{err:#}")),
                field("elapsed_ms", duration_ms(started.elapsed())),
            ],
        );
        return Err(err);
    }
    let replicas = match collect_replication_replicas(client).await {
        Ok(rows) => rows,
        Err(err) => {
            log_collection_failure(1_016_001, "main", &err, started.elapsed());
            return Err(err).context("collect pg_stat_replication");
        }
    };
    let slots = match collect_replication_slots(client, major).await {
        Ok(rows) => rows,
        Err(err) => {
            log_collection_failure(1_017_001, "main", &err, started.elapsed());
            return Err(err).context("collect pg_replication_slots");
        }
    };
    Ok((replicas, slots))
}

/// Intern and buffer the `pg_stat_replication` and `pg_replication_slots`
/// rows.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_replication_details(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    replicas: &[ReplicaRow],
    slots: &[SlotRow],
) -> Result<()> {
    for row in replicas {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_replicas_v1(row, &mut intern)?)?;
    }
    for row in slots {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_slots_v1(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern each row's strings and buffer it as the `pg_settings` section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or the
/// section buffer is full.
fn push_settings(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[SettingsRow],
) -> Result<()> {
    validate_settings_row_count(rows.len())?;
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_settings_v1(row, &mut intern)?)?;
    }
    Ok(())
}

/// Whether a tokio-postgres error carries the given SQLSTATE code.
fn is_sqlstate(err: &tokio_postgres::Error, code: &str) -> bool {
    err.code().is_some_and(|state| state.code() == code)
}

/// Buffer one typed snapshot row, mapping a full buffer to an error.
fn buffer_row<S: kronika_registry::Section + 'static>(
    buffers: &mut SectionBuffers,
    row: S,
) -> Result<()> {
    let type_id = S::CONTRACT.type_id.get();
    buffers.push(row).map_err(|_row| {
        anyhow::anyhow!(
            "section buffer is full: collection={} type_id={} layout_id={}",
            section_name(type_id),
            type_id,
            layout_id(type_id)
        )
    })
}

fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .ok();
}

/// Intern each plan row's strings and buffer it as section `1_004_001`.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_store_plans(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[StorePlansRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, store_plans::to_vadv_v1(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern each ossc plan row's strings and buffer it as section `1_003_001`.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_store_plans_ossc(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[StorePlansOsscRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, store_plans::to_ossc_v1(row, &mut intern)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DueSet, Intervals, MountEntry, PlansSourceCache, Scheduler, SegmentState, SourceCoverage,
        SourceKind, StatementsVersion, SysFs, cap_disks, collect_mountinfo, collect_os_sources,
        cpu_max_mhz, diskstats, min_total_time, resolve_major_zero, seal_reason,
        statements_type_id, timer_sleep_delay, user_indexes_type_id, user_tables_type_id,
    };
    use kronika_source_os::ProcFs;

    fn disk_row(major: i32, minor: i32) -> diskstats::DiskstatsRow {
        let line = format!("{major} {minor} dev{minor} 1 0 8 2 3 0 24 4 0 6 6\n");
        diskstats::parse(&line)
            .expect("valid diskstats line")
            .remove(0)
    }

    fn mount_entry(major: i32, minor: i32, source: &str) -> MountEntry {
        MountEntry {
            major,
            minor,
            mount_point: "/data".to_owned(),
            fstype: "btrfs".to_owned(),
            source: source.to_owned(),
            is_k8s_infra: false,
        }
    }

    #[test]
    fn cap_disks_keeps_lowest_devices_and_reports_drop() {
        let mut rows = vec![disk_row(8, 5), disk_row(8, 0), disk_row(259, 0)];
        let dropped = cap_disks(&mut rows, 2);
        assert_eq!(dropped, 1);
        // Kept devices are the two lowest (major, minor) pairs.
        assert_eq!(
            rows.iter().map(|r| (r.major, r.minor)).collect::<Vec<_>>(),
            vec![(8, 0), (8, 5)]
        );
    }

    #[test]
    fn cap_disks_is_a_noop_within_the_cap() {
        let mut rows = vec![disk_row(8, 0), disk_row(8, 1)];
        assert_eq!(cap_disks(&mut rows, 2), 0);
        assert_eq!(cap_disks(&mut rows, 5), 0);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn resolve_major_zero_rewrites_dev_backed_subvolumes() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("class/block/nvme0n1p2")).expect("mkdir");
        std::fs::write(dir.path().join("class/block/nvme0n1p2/dev"), "259:2\n").expect("write");
        let sys = SysFs::new(dir.path().to_path_buf());

        let mut entries = vec![
            mount_entry(0, 42, "/dev/nvme0n1p2"), // resolvable btrfs subvolume
            mount_entry(0, 43, "tmpfs"),          // no /dev/ source: unchanged
            mount_entry(8, 1, "/dev/sda1"),       // already real: unchanged
        ];
        resolve_major_zero(&sys, &mut entries);

        assert_eq!((entries[0].major, entries[0].minor), (259, 2));
        assert_eq!((entries[1].major, entries[1].minor), (0, 43));
        assert_eq!((entries[2].major, entries[2].minor), (8, 1));
    }

    #[test]
    fn resolve_major_zero_leaves_entry_when_sysfs_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = SysFs::new(dir.path().to_path_buf());
        let mut entries = vec![mount_entry(0, 42, "/dev/nvme0n1p2")];
        resolve_major_zero(&sys, &mut entries);
        // Unresolvable major==0 stays 0 and is dropped downstream by device_map.
        assert_eq!((entries[0].major, entries[0].minor), (0, 42));
    }

    #[test]
    fn collect_mountinfo_emits_every_mount_entry() {
        let entries = vec![
            MountEntry {
                major: 8,
                minor: 1,
                mount_point: "/data".to_owned(),
                fstype: "ext4".to_owned(),
                source: "/dev/sda1".to_owned(),
                is_k8s_infra: false,
            },
            MountEntry {
                major: 8,
                minor: 1,
                mount_point: "/data/pg wal".to_owned(),
                fstype: "ext4".to_owned(),
                source: "/dev/sda1".to_owned(),
                is_k8s_infra: false,
            },
        ];
        let mut interner = Interner::new(activity_dict_limits());
        let rows = collect_mountinfo(&mut interner, 0, 1_000_000, &entries);

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().map(|r| (r.major, r.minor)).collect::<Vec<_>>(),
            vec![(8, 1), (8, 1)]
        );
        assert_ne!(rows[0].mount_point, rows[1].mount_point);
    }

    #[test]
    fn cpu_max_mhz_reads_sysfs_khz() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rel = "devices/system/cpu/cpu0/cpufreq";
        std::fs::create_dir_all(dir.path().join(rel)).expect("mkdir");
        std::fs::write(dir.path().join(rel).join("cpuinfo_max_freq"), "3600000\n").expect("write");
        let sys = SysFs::new(dir.path().to_path_buf());

        assert_eq!(cpu_max_mhz(&sys, 0), Some(3600.0));
        assert_eq!(cpu_max_mhz(&sys, 1), None);
    }

    #[test]
    fn segment_seals_on_force_zero_cap_size_or_age() {
        assert_eq!(
            seal_reason(true, 0, u64::MAX, false),
            Some("forced"),
            "force always seals"
        );
        assert_eq!(
            seal_reason(false, 1, 0, false),
            Some("tick"),
            "zero cap seals every tick"
        );
        assert_eq!(
            seal_reason(false, 64, 64, false),
            Some("size"),
            "size cap reached"
        );
        assert_eq!(seal_reason(false, 63, 64, false), None, "under the cap");
        assert_eq!(
            seal_reason(false, 1, u64::MAX, true),
            Some("age"),
            "age cap reached"
        );
        assert_eq!(
            seal_reason(true, 64, 64, true),
            Some("forced"),
            "the forced reason outranks size and age"
        );
    }

    #[test]
    fn segment_state_opens_on_the_first_window_only() {
        use std::time::{Duration, Instant};

        let mut segment = SegmentState::default();
        let now = Instant::now();
        assert!(!segment.age_expired(now, Duration::from_secs(1)));
        segment.on_window_appended(100, now);
        segment.on_window_appended(200, now + Duration::from_secs(5));
        assert_eq!(
            segment.first_ts,
            Some(100),
            "the first window names the file"
        );
        assert!(segment.age_expired(now + Duration::from_secs(5), Duration::from_secs(5)));
        assert!(!segment.age_expired(now + Duration::from_secs(4), Duration::from_secs(5)));
    }

    #[test]
    fn plans_cache_is_due_without_a_deadline_and_after_it() {
        use std::time::{Duration, Instant};

        let mut cache = PlansSourceCache::default();
        let now = Instant::now();
        assert!(cache.is_due(now), "a fresh cache reads immediately");
        cache.next_read = Some(now + Duration::from_mins(5));
        assert!(!cache.is_due(now), "before the deadline nothing is due");
        assert!(
            cache.is_due(now + Duration::from_mins(5)),
            "the deadline itself is due"
        );
    }

    #[test]
    fn timer_sleep_uses_source_deadline_before_regular_tick() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let intervals = Intervals {
            activity: 1,
            ..Intervals::default()
        };
        let mut sched = Scheduler::new(intervals);
        sched.plan(start, false);

        assert_eq!(
            timer_sleep_delay(
                start,
                5,
                900,
                &sched,
                &PlansSourceCache::default(),
                &SegmentState::default()
            ),
            Some(Duration::from_secs(1)),
            "a 1s source interval is not capped by a 5s regular wake"
        );
    }

    #[test]
    fn timer_sleep_uses_accelerated_deadline_before_regular_tick() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let mut sched = Scheduler::new(Intervals::default());
        sched.plan(start, false);
        assert!(sched.accelerate(SourceKind::Activity, 1));

        assert_eq!(
            timer_sleep_delay(
                start,
                5,
                900,
                &sched,
                &PlansSourceCache::default(),
                &SegmentState::default()
            ),
            Some(Duration::from_secs(1)),
            "default activity fast pace can wake before the 5s regular timer"
        );
    }

    #[test]
    fn timer_sleep_keeps_zero_interval_on_regular_wakes() {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let intervals = Intervals {
            activity: 0,
            ..Intervals::default()
        };
        let mut sched = Scheduler::new(intervals);
        sched.plan(start, false);

        assert_eq!(
            timer_sleep_delay(
                start,
                5,
                900,
                &sched,
                &PlansSourceCache::default(),
                &SegmentState::default()
            ),
            Some(Duration::from_secs(5)),
            "zero means every timer wake, not an immediate busy loop"
        );
    }

    #[test]
    fn coverage_reason_ranks_timeouts_over_other_skips() {
        let top_n = SourceCoverage {
            total: 100,
            collected: 50,
            unknown_total: false,
            timeouts: 0,
            permission_skips: 0,
            other_skips: 0,
        };
        assert_eq!(top_n.reason(), 0);
        assert_eq!(
            SourceCoverage {
                other_skips: 2,
                ..top_n
            }
            .reason(),
            3
        );
        assert_eq!(
            SourceCoverage {
                unknown_total: true,
                ..top_n
            }
            .reason(),
            3
        );
        assert_eq!(
            SourceCoverage {
                permission_skips: 1,
                other_skips: 2,
                ..top_n
            }
            .reason(),
            2
        );
        assert_eq!(
            SourceCoverage {
                timeouts: 1,
                permission_skips: 1,
                other_skips: 2,
                ..top_n
            }
            .reason(),
            1
        );
    }

    #[test]
    fn coverage_truncated_needs_missing_rows_or_skips() {
        let complete = SourceCoverage {
            total: 50,
            collected: 50,
            unknown_total: false,
            timeouts: 0,
            permission_skips: 0,
            other_skips: 0,
        };
        assert!(!complete.truncated());
        assert!(
            SourceCoverage {
                total: 51,
                ..complete
            }
            .truncated()
        );
        assert!(
            SourceCoverage {
                timeouts: 1,
                ..complete
            }
            .truncated()
        );
        assert!(
            SourceCoverage {
                unknown_total: true,
                ..complete
            }
            .truncated()
        );
        assert!(
            SourceCoverage {
                other_skips: 1,
                ..complete
            }
            .truncated()
        );
        assert!(
            !SourceCoverage {
                total: 40,
                ..complete
            }
            .truncated()
        );
    }

    #[test]
    fn min_total_time_finds_the_boundary() {
        assert_eq!(min_total_time([3.0, 1.5, 2.0].into_iter()), Some(1.5));
        assert_eq!(min_total_time(std::iter::empty()), None);
    }

    #[test]
    fn type_id_maps_cover_the_supported_majors() {
        assert_eq!(user_tables_type_id(12), 1_013_001);
        assert_eq!(user_tables_type_id(13), 1_013_002);
        assert_eq!(user_tables_type_id(16), 1_013_003);
        assert_eq!(user_tables_type_id(18), 1_013_004);
        assert_eq!(user_indexes_type_id(15), 1_014_001);
        assert_eq!(user_indexes_type_id(16), 1_014_002);
        assert_eq!(statements_type_id(StatementsVersion::V6), 1_002_006);
    }

    use super::{
        CachedStatementsSource, MissingStatementsSource, StatementsSource, StatementsSourceCache,
        activity_dict_limits, activity_needs_acceleration, open_collector_journal, push_activity,
        push_archiver, push_database, push_io, push_locks, push_prepared_xacts,
        push_progress_vacuum, push_replication_instance, push_statements, push_user_indexes,
        push_user_tables, replication_needs_acceleration, validate_cardinality, validate_heavy_cap,
        validate_max_lock_rows, validate_replication_detail_bounds, validate_settings_row_count,
    };
    use kronika_registry::MAX_SECTION_ROWS;
    use kronika_source_pg::archiver::ArchiverRow;
    use kronika_source_pg::database::{DatabaseRow, DatabaseVersion};
    use kronika_source_pg::io::{IoRow, IoVersion};
    use kronika_source_pg::locks::{LocksRow, LocksVersion};
    use kronika_source_pg::prepared_xacts::PreparedXactsRow;
    use kronika_source_pg::progress_vacuum::ProgressVacuumRow;
    use kronika_source_pg::replication_details::{ReplicaRow, ReplicationDetailBounds, SlotRow};
    use kronika_source_pg::replication_instance::ReplicationInstanceRow;
    use kronika_source_pg::statements::StatementsRow;
    use kronika_source_pg::user_indexes::{UserIndexesRow, UserIndexesVersion};
    use kronika_source_pg::user_tables::{UserTablesRow, UserTablesVersion};
    use kronika_source_pg::{ActivityRow, ActivityVersion};
    use kronika_writer::{Interner, SectionBuffers, dict};

    #[test]
    fn cardinality_validation_passes_at_defaults() {
        assert!(validate_cardinality(500, 500).is_ok());
    }

    #[test]
    fn cardinality_validation_rejects_overflowing_max_indexes() {
        // 20 databases * 4 index axes * 820 = 65600 > 65536.
        let err = validate_cardinality(500, 820).expect_err("820 indexes must overflow");
        assert!(err.to_string().contains("KRONIKA_PG_MAX_INDEXES"));
    }

    #[test]
    fn cardinality_validation_rejects_overflowing_max_tables() {
        // 20 databases * 6 table axes * 547 = 65640 > 65536.
        let err = validate_cardinality(547, 500).expect_err("547 tables must overflow");
        assert!(err.to_string().contains("KRONIKA_PG_MAX_TABLES"));
    }

    #[test]
    fn heavy_cap_validation_rejects_zero() {
        let err = validate_heavy_cap(0).expect_err("a zero heavy cap must be rejected");
        assert!(err.to_string().contains("KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS"));
    }

    #[test]
    fn heavy_cap_validation_accepts_positive() {
        assert!(validate_heavy_cap(60_000).is_ok());
    }

    fn client_row(pid: i32) -> ActivityRow {
        ActivityRow {
            ts: 1_000,
            pid,
            leader_pid: None,
            datname: Some("appdb".to_owned()),
            usename: Some("alice".to_owned()),
            application_name: "psql".to_owned(),
            client_addr: String::new(),
            backend_type: "client backend".to_owned(),
            state: Some("active".to_owned()),
            wait_event_type: None,
            wait_event: None,
            query: Some("select 1".to_owned()),
            query_id: Some(42),
            backend_xid_age: None,
            backend_xmin_age: Some(7),
            backend_start: 100,
            xact_start: Some(500),
            query_start: Some(800),
            state_change: Some(900),
        }
    }

    #[test]
    fn push_activity_buffers_rows_and_interns_their_strings() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_activity(
            &mut buffers,
            &mut interner,
            ActivityVersion::V3,
            &[client_row(1), client_row(2)],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The buffered rows use dictionary ids, and the part carries the V3
        // activity section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "strings reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_001_003),
            "the part carries the pg_stat_activity section"
        );
    }

    /// One encoded collection window holding a single activity row.
    fn activity_window() -> Vec<u8> {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_activity(
            &mut buffers,
            &mut interner,
            ActivityVersion::V3,
            &[client_row(7)],
        )
        .expect("push interns and buffers");
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part")
    }

    #[test]
    fn startup_seals_windows_a_dead_process_left_in_the_journal() {
        use kronika_writer::{Journal, JournalConfig};

        let dir = tempfile::tempdir().expect("tempdir");
        {
            let (mut journal, _report) =
                Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                    .expect("open the journal");
            journal.append(&activity_window()).expect("append");
            // Dropping without seal is the crash: the file stays behind.
        }

        let (journal, recovered) =
            open_collector_journal(dir.path(), 1 << 30).expect("reopen the journal");
        let dest = recovered.expect("leftover windows must become a segment");
        // client_row stamps ts = 1_000, which names the recovered file.
        assert_eq!(dest, dir.path().join("1000.pgm"));
        assert!(dest.exists(), "the recovered segment is on disk");
        assert!(journal.parts().is_empty(), "the journal restarts empty");
    }

    #[test]
    fn startup_with_an_empty_journal_recovers_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (journal, recovered) =
            open_collector_journal(dir.path(), 1 << 30).expect("open the journal");
        assert!(recovered.is_none());
        assert!(journal.parts().is_empty());
    }

    fn db_row(datid: u32) -> DatabaseRow {
        DatabaseRow {
            ts: 1_000,
            datid,
            datname: if datid == 0 {
                None
            } else {
                Some("appdb".to_owned())
            },
            numbackends: if datid == 0 { Some(0) } else { Some(4) },
            xact_commit: 100,
            xact_rollback: 2,
            blks_read: 4_000,
            blks_hit: 90_000,
            tup_returned: 500,
            tup_fetched: 400,
            tup_inserted: 50,
            tup_updated: 30,
            tup_deleted: 10,
            conflicts: 0,
            temp_files: 1,
            temp_bytes: 8_192,
            deadlocks: 0,
            blk_read_time: 12.5,
            blk_write_time: 3.0,
            stats_reset: Some(1_500),
            checksum_failures: Some(0),
            checksum_last_failure: None,
            session_time: Some(1_000.0),
            active_time: Some(250.0),
            idle_in_transaction_time: Some(50.0),
            sessions: Some(7),
            sessions_abandoned: Some(1),
            sessions_fatal: Some(0),
            sessions_killed: Some(0),
            parallel_workers_to_launch: Some(9),
            parallel_workers_launched: Some(8),
            frozen_xid_age: if datid == 0 { None } else { Some(150_000_000) },
            min_mxid_age: if datid == 0 { None } else { Some(5_000_000) },
            datconnlimit: if datid == 0 { None } else { Some(-1) },
            datallowconn: if datid == 0 { None } else { Some(true) },
            datistemplate: if datid == 0 { None } else { Some(false) },
        }
    }

    #[test]
    fn push_database_buffers_rows_and_interns_datname() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_database(
            &mut buffers,
            &mut interner,
            DatabaseVersion::V4,
            &[db_row(0), db_row(1)],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The non-shared row's datname should be interned, and the part should
        // contain the V4 database section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "datname was interned");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_005_004),
            "the part carries the pg_stat_database section"
        );
    }

    fn ut_row(relid: u32) -> UserTablesRow {
        UserTablesRow {
            ts: 1_000,
            datid: 5,
            relid,
            schemaname: "public".to_owned(),
            relname: "accounts".to_owned(),
            tablespace: "pg_default".to_owned(),
            seq_scan: 10,
            seq_tup_read: 1_000,
            idx_scan: Some(7),
            idx_tup_fetch: Some(700),
            n_tup_ins: 50,
            n_tup_upd: 30,
            n_tup_del: 10,
            n_tup_hot_upd: 5,
            n_tup_newpage_upd: Some(0),
            n_live_tup: 900,
            n_dead_tup: 40,
            n_mod_since_analyze: 70,
            n_ins_since_vacuum: Some(20),
            vacuum_count: 1,
            autovacuum_count: 3,
            analyze_count: 1,
            autoanalyze_count: 2,
            last_vacuum: None,
            last_autovacuum: None,
            last_analyze: None,
            last_autoanalyze: None,
            last_seq_scan: None,
            last_idx_scan: None,
            total_vacuum_time: None,
            total_autovacuum_time: None,
            total_analyze_time: None,
            total_autoanalyze_time: None,
            main_fork_bytes: 8_192,
            toast_bytes: None,
            toast_n_live_tup: None,
            toast_n_dead_tup: None,
            toast_last_autovacuum: None,
            xid_age: 100_000_000,
            mxid_age: 5_000_000,
            reltuples: 900,
            heap_blks_read: 400,
            heap_blks_hit: 90_000,
            idx_blks_read: Some(40),
            idx_blks_hit: Some(9_000),
            toast_blks_read: None,
            toast_blks_hit: None,
            tidx_blks_read: None,
            tidx_blks_hit: None,
        }
    }

    #[test]
    fn push_user_tables_buffers_rows_and_interns_strings() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_user_tables(
            &mut buffers,
            &mut interner,
            &[(
                "appdb".to_owned(),
                UserTablesVersion::V3,
                vec![ut_row(16_384), ut_row(16_385)],
            )],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The buffered rows use dictionary ids, and the part carries the V3
        // user-tables section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "strings reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_013_003),
            "the part carries the pg_stat_user_tables section"
        );
    }

    fn ui_row(indexrelid: u32) -> UserIndexesRow {
        UserIndexesRow {
            ts: 1_000,
            datid: 5,
            indexrelid,
            relid: indexrelid - 1,
            schemaname: "public".to_owned(),
            relname: "accounts".to_owned(),
            indexrelname: "accounts_pkey".to_owned(),
            tablespace: "pg_default".to_owned(),
            idx_scan: 120,
            idx_tup_read: 3_400,
            idx_tup_fetch: 3_000,
            main_fork_bytes: 16_384,
            last_idx_scan: Some(900),
            indisunique: true,
            indisprimary: true,
            indisvalid: true,
            indisexclusion: false,
            indisready: true,
            amname: "btree".to_owned(),
            indexdef: "CREATE UNIQUE INDEX accounts_pkey ON public.accounts USING btree (id)"
                .to_owned(),
            idx_blks_read: 40,
            idx_blks_hit: 9_000,
        }
    }

    #[test]
    fn push_user_indexes_buffers_rows_and_interns_strings() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_user_indexes(
            &mut buffers,
            &mut interner,
            &[(
                "appdb".to_owned(),
                UserIndexesVersion::V2,
                vec![ui_row(16_385), ui_row(16_387)],
            )],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The buffered rows use dictionary ids, and the part carries the V2
        // user-indexes section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "strings reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_014_002),
            "the part carries the pg_stat_user_indexes section"
        );
    }

    fn statements_row(queryid: i64) -> StatementsRow {
        StatementsRow {
            ts: 1_000,
            queryid: Some(queryid),
            userid: 10,
            dbid: 5,
            toplevel: Some(true),
            datname: Some("appdb".to_owned()),
            usename: Some("alice".to_owned()),
            query: Some("select 1".to_owned()),
            calls: 100,
            rows: 5_000,
            plans: Some(90),
            total_time: 1_234.5,
            total_plan_time: Some(12.5),
            min_time: 0.5,
            max_time: 40.0,
            mean_time: 12.3,
            stddev_time: 3.1,
            min_plan_time: Some(0.1),
            max_plan_time: Some(1.0),
            mean_plan_time: Some(0.2),
            stddev_plan_time: Some(0.05),
            shared_blks_hit: 90_000,
            shared_blks_read: 4_000,
            shared_blks_dirtied: 50,
            shared_blks_written: 30,
            local_blks_hit: 0,
            local_blks_read: 0,
            local_blks_dirtied: 0,
            local_blks_written: 0,
            temp_blks_read: 0,
            temp_blks_written: 0,
            blk_read_time: 12.5,
            blk_write_time: 3.0,
            local_blk_read_time: Some(1.0),
            local_blk_write_time: Some(0.5),
            temp_blk_read_time: Some(2.0),
            temp_blk_write_time: Some(1.5),
            wal_records: Some(42),
            wal_fpi: Some(3),
            wal_bytes: Some(8_192),
            wal_buffers_full: Some(1),
            jit_functions: Some(0),
            jit_generation_time: Some(0.0),
            jit_inlining_count: Some(0),
            jit_inlining_time: Some(0.0),
            jit_optimization_count: Some(0),
            jit_optimization_time: Some(0.0),
            jit_emission_count: Some(0),
            jit_emission_time: Some(0.0),
            jit_deform_count: Some(0),
            jit_deform_time: Some(0.0),
            parallel_workers_to_launch: Some(4),
            parallel_workers_launched: Some(3),
            stats_since: Some(500),
            minmax_stats_since: Some(800),
        }
    }

    #[test]
    fn truncate_to_boundary_respects_utf8_and_short_inputs() {
        let mut short = "plan".to_owned();
        super::truncate_to_boundary(&mut short, 10);
        assert_eq!(short, "plan", "short inputs stay whole");

        let mut exact = "план".to_owned(); // 8 bytes, 4 chars
        super::truncate_to_boundary(&mut exact, 5);
        assert_eq!(exact, "пл", "the cut lands on a character boundary");
        assert!(exact.len() <= 5);

        let mut zero = "план".to_owned();
        super::truncate_to_boundary(&mut zero, 0);
        assert_eq!(zero, "", "a zero cap empties the text");
    }

    #[test]
    fn plan_text_limits_guard_matches_dictionary_bounds() {
        assert!(super::validate_plan_text_limits(32_768, 8 * 1024 * 1024).is_ok());
        assert!(super::validate_plan_text_limits(64 * 1024, 0).is_ok());
        assert!(
            super::validate_plan_text_limits(0, 1).is_err(),
            "a zero per-text cap is rejected"
        );
        assert!(
            super::validate_plan_text_limits(64 * 1024 + 1, 1).is_err(),
            "a per-text cap past the 64 KiB dictionary truncation is rejected"
        );
        assert!(
            super::validate_plan_text_limits(1, 16 * 1024 * 1024 + 1).is_err(),
            "a budget past the 16 MiB dictionary cap is rejected"
        );
    }

    #[test]
    fn max_plans_guard_accepts_range_and_rejects_extremes() {
        assert!(super::validate_max_plans(1).is_ok());
        assert!(super::validate_max_plans(500).is_ok());
        assert!(super::validate_max_plans(0).is_err(), "zero is rejected");
        let cap = i64::try_from(MAX_SECTION_ROWS).expect("cap fits i64");
        assert!(super::validate_max_plans(cap).is_ok());
        assert!(
            super::validate_max_plans(cap + 1).is_err(),
            "a value above MAX_SECTION_ROWS is rejected"
        );
    }

    #[test]
    fn settings_row_guard_rejects_section_overflow() {
        assert!(validate_settings_row_count(MAX_SECTION_ROWS).is_ok());
        let err = validate_settings_row_count(MAX_SECTION_ROWS + 1)
            .expect_err("pg_settings must not exceed one section");
        assert!(err.to_string().contains("pg_settings"));
    }

    #[test]
    fn plans_reread_delay_shortens_only_empty_reads() {
        use std::time::Duration;
        let interval = Duration::from_mins(5);
        assert_eq!(super::plans_reread_delay(false, interval), interval);
        assert_eq!(
            super::plans_reread_delay(true, interval),
            Duration::from_secs(30)
        );
        let short = Duration::from_secs(10);
        assert_eq!(
            super::plans_reread_delay(true, short),
            short,
            "the retry delay never exceeds the interval"
        );
    }

    #[test]
    fn push_statements_buffers_rows_and_interns_strings() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_statements(
            &mut buffers,
            &mut interner,
            StatementsVersion::V6,
            &[statements_row(777), statements_row(888)],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The buffered rows use dictionary ids, and the part carries the V6
        // statements section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "strings reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_002_006),
            "the part carries the pg_stat_statements section"
        );
    }

    #[test]
    fn cached_statements_source_tracks_extversion_and_layout() {
        let cached = CachedStatementsSource::new(
            StatementsSource::Database("metrics".to_owned()),
            "1.11".to_owned(),
        );
        assert_eq!(cached.version, StatementsVersion::V5);
        assert!(cached.matches_extversion("1.11"));
        assert!(!cached.matches_extversion("1.12"));
        assert!(!cached.matches_extversion("1.10"));
    }

    #[test]
    fn missing_statements_source_rotates_per_db_probes() {
        let mut missing =
            MissingStatementsSource::new(vec!["app".to_owned(), "metrics".to_owned()]);
        assert!(missing.matches_covered(&["app".to_owned(), "metrics".to_owned()]));
        assert!(!missing.matches_covered(&["app".to_owned()]));
        assert_eq!(missing.next_per_db_probe(2), Some(0));
        assert_eq!(missing.next_per_db_probe(2), Some(1));
        assert_eq!(missing.next_per_db_probe(2), Some(0));
        assert_eq!(missing.next_per_db_probe(0), None);
    }

    #[test]
    fn statements_source_cache_replaces_missing_with_selected_source() {
        let mut cache = StatementsSourceCache::default();
        cache.mark_missing(vec!["app".to_owned()]);
        assert!(cache.selected.is_none());
        assert!(cache.missing.is_some());

        let version = cache.store(StatementsSource::Main, "1.12".to_owned());
        assert_eq!(version, StatementsVersion::V6);
        assert!(cache.selected.is_some());
        assert!(cache.missing.is_none());

        cache.invalidate();
        assert!(cache.selected.is_none());
        assert!(cache.missing.is_none());
    }

    fn io_row(object: &str) -> IoRow {
        IoRow {
            ts: 1_000,
            backend_type: "client backend".to_owned(),
            object: object.to_owned(),
            context: "normal".to_owned(),
            reads: Some(100),
            read_bytes: Some(819_200),
            read_time: Some(12.5),
            writes: Some(50),
            write_bytes: Some(409_600),
            write_time: Some(3.0),
            writebacks: Some(0),
            writeback_time: None,
            extends: Some(7),
            extend_bytes: Some(57_344),
            extend_time: None,
            op_bytes: Some(8192),
            hits: Some(9000),
            evictions: Some(2),
            reuses: None,
            fsyncs: Some(1),
            fsync_time: None,
            stats_reset: Some(500),
        }
    }

    fn archiver_row() -> ArchiverRow {
        ArchiverRow {
            ts: 1_000,
            archived_count: 3,
            last_archived_wal: Some("00000001000000000000000A".to_owned()),
            last_archived_time: Some(900),
            failed_count: 1,
            last_failed_wal: Some("00000001000000000000000B".to_owned()),
            last_failed_time: Some(950),
            stats_reset: None,
        }
    }

    fn prepared_row() -> PreparedXactsRow {
        PreparedXactsRow {
            ts: 1_000,
            datname: "appdb".to_owned(),
            prepared_count: 1,
            max_age_us: 50_000,
            max_xid_age_tx: 4,
        }
    }

    fn progress_vacuum_row(phase: &str) -> ProgressVacuumRow {
        ProgressVacuumRow {
            ts: 1_000,
            pid: 42,
            datid: 16_385,
            datname: "appdb".to_owned(),
            relid: 16_384,
            is_autovacuum: true,
            phase: phase.to_owned(),
            heap_blks_total: 10_000,
            heap_blks_scanned: 4_200,
            heap_blks_vacuumed: 4_000,
            index_vacuum_count: 1,
            max_dead_tuples: Some(291_271),
            num_dead_tuples: Some(120_000),
            max_dead_tuple_bytes: None,
            dead_tuple_bytes: None,
            num_dead_item_ids: None,
            indexes_total: None,
            indexes_processed: None,
            delay_time: None,
        }
    }

    fn replication_instance_row() -> ReplicationInstanceRow {
        ReplicationInstanceRow {
            ts: 1_000,
            is_in_recovery: true,
            timeline_id: 2,
            synchronous_standby_names: b"*".to_vec(),
            synchronous_commit: b"remote_apply".to_vec(),
            wal_receiver_status: Some(b"streaming".to_vec()),
            sender_host: Some(b"primary.local".to_vec()),
            sender_port: Some(5432),
            slot_name: Some(b"standby_a".to_vec()),
            streaming_replicas: 0,
            replay_lag_s: Some(1),
            standby_receive_lsn: Some(1_024),
            standby_replay_lsn: Some(1_024),
            standby_last_replay_at: Some(900),
            current_wal_lsn: None,
            latest_end_lsn: Some(1_024),
            latest_end_time: Some(950),
            received_tli: Some(2),
        }
    }

    fn trigger_replica_row(replay_lag_us: Option<i64>) -> ReplicaRow {
        ReplicaRow {
            ts: 1_000,
            pid: 9,
            usename: "repl".to_owned(),
            application_name: "walreceiver".to_owned(),
            client_addr: None,
            state: "streaming".to_owned(),
            sync_state: "async".to_owned(),
            sync_priority: Some(0),
            sent_lsn: Some(2_048),
            write_lsn: Some(2_048),
            flush_lsn: Some(2_048),
            replay_lsn: Some(1_024),
            write_lag_us: None,
            flush_lag_us: None,
            replay_lag_us,
        }
    }

    fn trigger_slot_row(retained_bytes: Option<i64>) -> SlotRow {
        SlotRow {
            ts: 1_000,
            slot_name: "standby_a".to_owned(),
            plugin: None,
            slot_type: "physical".to_owned(),
            active: true,
            restart_lsn: Some(1_024),
            confirmed_flush_lsn: None,
            retained_bytes,
            wal_status: Some("reserved".to_owned()),
        }
    }

    #[test]
    fn activity_accelerates_on_lock_waiters_or_active_pressure() {
        let mut waiter = client_row(1);
        waiter.wait_event_type = Some("Lock".to_owned());
        assert!(activity_needs_acceleration(&[waiter], 100));

        let busy: Vec<_> = (0..3).map(client_row).collect();
        assert!(activity_needs_acceleration(&busy, 3));
        assert!(
            !activity_needs_acceleration(&busy, 4),
            "below the threshold, no lock waiters: base pace"
        );

        let mut walsender = client_row(5);
        walsender.backend_type = "walsender".to_owned();
        assert!(
            !activity_needs_acceleration(&[walsender], 1),
            "only client backends count toward the threshold"
        );
    }

    #[test]
    fn replication_accelerates_on_lag_or_retained_wal() {
        let gib = 1_024 * 1_024 * 1_024;
        let calm_instance = ReplicationInstanceRow {
            replay_lag_s: None,
            ..replication_instance_row()
        };

        assert!(
            !replication_needs_acceleration(&calm_instance, &[], &[], 10, gib),
            "nothing lags, nothing is retained"
        );
        assert!(
            replication_needs_acceleration(&replication_instance_row(), &[], &[], 1, gib),
            "this standby replays behind the trigger"
        );
        assert!(
            replication_needs_acceleration(
                &calm_instance,
                &[trigger_replica_row(Some(11_000_000))],
                &[],
                10,
                gib
            ),
            "a replica replays behind the trigger"
        );
        assert!(
            !replication_needs_acceleration(
                &calm_instance,
                &[trigger_replica_row(Some(9_000_000))],
                &[],
                10,
                gib
            ),
            "a replica under the trigger stays at base pace"
        );
        assert!(
            replication_needs_acceleration(
                &calm_instance,
                &[],
                &[trigger_slot_row(Some(gib))],
                10,
                gib
            ),
            "a slot retains enough WAL to trip the trigger"
        );
    }

    #[test]
    fn push_progress_vacuum_buffers_rows_and_interns_labels() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_progress_vacuum(
            &mut buffers,
            &mut interner,
            &[progress_vacuum_row("scanning heap")],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "row was buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "labels reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_012_001),
            "the part carries the pg_stat_progress_vacuum section"
        );
    }

    #[test]
    fn push_prepared_xacts_buffers_rows_and_interns_datname() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_prepared_xacts(&mut buffers, &mut interner, &[prepared_row()])
            .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "row was buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "datname reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_010_001),
            "the part carries the pg_prepared_xacts section"
        );
    }

    #[test]
    fn push_archiver_buffers_row_and_interns_wal_names() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_archiver(&mut buffers, &mut interner, &archiver_row())
            .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "row was buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(
            !dict_sections.is_empty(),
            "wal names reached the dictionary"
        );
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_008_001),
            "the part carries the pg_stat_archiver section"
        );
    }

    #[test]
    fn push_io_buffers_rows_and_interns_labels() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_io(
            &mut buffers,
            &mut interner,
            IoVersion::V2,
            &[io_row("relation"), io_row("wal")],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "labels reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_009_002),
            "the part carries the pg_stat_io section"
        );
    }

    #[test]
    fn push_replication_instance_buffers_row_and_interns_labels() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_replication_instance(&mut buffers, &mut interner, &replication_instance_row())
            .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "row was buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(
            !dict_sections.is_empty(),
            "replication labels reached the dictionary"
        );
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_015_001),
            "the part carries the replication_instance section"
        );
    }

    #[test]
    fn max_lock_rows_within_section_cap() {
        assert!(1000 <= i64::try_from(MAX_SECTION_ROWS).unwrap());
    }

    #[test]
    fn max_lock_rows_validation_rejects_overflow() {
        let cap = i64::try_from(MAX_SECTION_ROWS).unwrap();
        let err = validate_max_lock_rows(cap + 1).expect_err("value above cap must be rejected");
        assert!(err.to_string().contains("KRONIKA_PG_MAX_LOCK_ROWS"));
    }

    #[test]
    fn max_lock_rows_validation_rejects_zero() {
        let err = validate_max_lock_rows(0).expect_err("zero disables the graph guard");
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn replication_detail_bounds_accept_defaults() {
        let bounds = ReplicationDetailBounds {
            max_wal_senders: 10,
            max_replication_slots: 10,
        };
        assert!(validate_replication_detail_bounds(bounds).is_ok());
    }

    #[test]
    fn replication_detail_bounds_reject_section_overflow() {
        let cap = i64::try_from(MAX_SECTION_ROWS).unwrap();
        let bounds = ReplicationDetailBounds {
            max_wal_senders: cap + 1,
            max_replication_slots: 10,
        };
        let err = validate_replication_detail_bounds(bounds)
            .expect_err("max_wal_senders above the section cap must be rejected");
        assert!(err.to_string().contains("max_wal_senders"));
    }

    #[test]
    fn replication_detail_bounds_reject_negative_guc() {
        let bounds = ReplicationDetailBounds {
            max_wal_senders: 10,
            max_replication_slots: -1,
        };
        let err = validate_replication_detail_bounds(bounds)
            .expect_err("negative max_replication_slots must be rejected");
        assert!(err.to_string().contains("non-negative"));
    }

    #[test]
    fn replication_detail_bounds_reject_dictionary_overflow() {
        let bounds = ReplicationDetailBounds {
            max_wal_senders: 60_000,
            max_replication_slots: 40_000,
        };
        let err = validate_replication_detail_bounds(bounds)
            .expect_err("combined replication labels must fit the dictionary cap");
        assert!(err.to_string().contains("dictionary bytes"));
    }

    fn locks_root_row() -> LocksRow {
        LocksRow {
            ts: 1_000_000,
            pid: 10,
            blocked_by: Vec::new(),
            depth: 0,
            root_pid: 10,
            datid: 16_384,
            datname: "app".to_owned(),
            usename: Some("postgres".to_owned()),
            application_name: "psql".to_owned(),
            client_addr: String::new(),
            backend_type: "client backend".to_owned(),
            state: Some("active".to_owned()),
            wait_event_type: None,
            wait_event: None,
            query: "select 1".to_owned(),
            backend_xid_age: None,
            backend_xmin_age: None,
            backend_start: Some(940_000),
            xact_start: Some(995_000),
            query_start: Some(999_000),
            state_change: Some(999_000),
            lock_locktype: None,
            lock_mode: None,
            lock_granted: None,
            lock_database: None,
            lock_relation: None,
            lock_relname: None,
            lock_page: None,
            lock_tuple: None,
            lock_virtualxid: None,
            lock_transactionid: None,
            lock_classid: None,
            lock_objid: None,
            lock_objsubid: None,
            lock_fastpath: None,
            lock_target: None,
            waitstart: None,
        }
    }

    #[test]
    fn push_locks_buffers_v2_row_into_1_011_002_section() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_locks(
            &mut buffers,
            &mut interner,
            LocksVersion::V2,
            &[locks_root_row()],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "row was buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_011_002),
            "the part carries the pg_locks wait-tree V2 section"
        );
    }

    #[test]
    fn push_locks_buffers_v1_row_into_1_011_001_section() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_locks(
            &mut buffers,
            &mut interner,
            LocksVersion::V1,
            &[locks_root_row()],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "row was buffered");

        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_011_001),
            "the part carries the pg_locks wait-tree V1 section"
        );
    }

    // Verify that diskstats rows are not emitted on an OsMountTopo-only tick.
    #[test]
    fn collect_os_sources_no_diskstats_on_mount_topo_only_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let proc_root = dir.path();

        // diskstats: one device (8:1)
        let diskstats_line = "8 1 sda1 1 0 8 2 3 0 24 4 0 6 6\n";
        std::fs::write(proc_root.join("diskstats"), diskstats_line).expect("write diskstats");

        // self/mountinfo: sda1 mounted at /data
        std::fs::create_dir_all(proc_root.join("self")).expect("mkdir self");
        let mountinfo_line = "30 25 8:1 / /data rw - ext4 /dev/sda1 rw\n";
        std::fs::write(proc_root.join("self/mountinfo"), mountinfo_line).expect("write mountinfo");

        let fs = ProcFs::new(proc_root.to_path_buf());
        let mut interner = Interner::new(activity_dict_limits());
        let due = DueSet::for_test(vec![SourceKind::OsMountTopo]);

        let os = collect_os_sources(&fs, &mut interner, 0, 0, false, &due);

        assert!(
            os.diskstats.is_empty(),
            "diskstats must not be emitted on an OsMountTopo-only tick"
        );
        assert!(
            !os.mountinfo.is_empty(),
            "mountinfo rows must still be built"
        );
    }
}
