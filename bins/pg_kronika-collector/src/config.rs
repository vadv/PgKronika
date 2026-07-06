use anyhow::{Context, Result};
use kronika_registry::MAX_SECTION_ROWS;
use kronika_source_log::{LogConfig, ParserKind as LogParserKind};
use kronika_source_pg::pool::{DEFAULT_MAX_DATABASES, SessionConfig};
use kronika_source_pg::replication_details::ReplicationDetailBounds;
use kronika_source_pg::user_indexes::INDEX_TOPN_AXES;
use kronika_source_pg::user_tables::TABLE_TOPN_AXES;
use kronika_writer::DEFAULT_MAX_JOURNAL_LEN;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::logging::{LogLevel, field, log_event};
use crate::scheduler::Intervals;
use crate::source_contracts::activity_dict_limits;

pub(crate) struct Config {
    pub(crate) dsn: String,
    pub(crate) out_dir: PathBuf,
    pub(crate) source_id: u64,
    pub(crate) session: SessionConfig,
    pub(crate) exclude_databases: HashSet<String>,
    /// Per-axis top-N row count for the `pg_stat_user_tables` candidate selection.
    pub(crate) max_tables: i64,
    /// Per-axis top-N row count for the `pg_stat_user_indexes` candidate selection.
    pub(crate) max_indexes: i64,
    /// Per-axis top-N row count for the `pg_stat_statements` candidate selection.
    pub(crate) max_statements: i64,
    /// Top-N row count by total time for the `pg_store_plans` read.
    pub(crate) max_plans: i64,
    /// Minimum interval between `pg_store_plans` reads.
    pub(crate) plans_interval: Duration,
    /// Per-plan text truncation, bytes.
    pub(crate) max_plan_text: i32,
    /// Total plan-text bytes fetched per read.
    pub(crate) plan_text_budget: u64,
    /// Minimum interval between connection-pool refreshes, seconds.
    pub(crate) pool_refresh_secs: u64,
    /// Cap for the adaptive `statement_timeout` of the heavy per-table query, ms.
    pub(crate) heavy_timeout_cap_ms: u64,
    /// Maximum lock-wait waiters, edges, and nodes accepted for one section.
    pub(crate) max_lock_rows: i64,
    /// Node id sealed into `instance_metadata`; `None` falls back to hostname.
    pub(crate) node_self_id: Option<String>,
    /// Base tick of the internal timer, seconds; `0` disables the timer and
    /// leaves collection to signals only.
    pub(crate) tick_secs: u64,
    /// Per-source read intervals.
    pub(crate) intervals: Intervals,
    /// `PostgreSQL` log source configuration.
    pub(crate) log: LogConfig,
    /// Seal the segment when the journal holds at least this many raw bytes;
    /// `0` seals on every tick (the pre-rotation behavior).
    pub(crate) segment_max_bytes: u64,
    /// Seal an open segment at this age even if the byte cap was not reached.
    pub(crate) segment_max_age_secs: u64,
    /// Hard cap of the on-disk journal file; reaching it seals the open
    /// segment early instead of failing the append.
    pub(crate) journal_max_bytes: u64,
    /// Ceiling for one cycle's database time before the sized pool sources
    /// (statements, tables, indexes) are deferred; `0` disables the budget.
    pub(crate) cycle_db_budget_ms: u64,
    /// Activity pace while its trigger fires; at or above the base interval
    /// the trigger is disabled.
    pub(crate) activity_fast_interval_s: u64,
    /// Active client backends that trip the activity trigger.
    pub(crate) ash_active_threshold: usize,
    /// Replication pace while its trigger fires; at or above the base
    /// interval the trigger is disabled.
    pub(crate) replication_fast_interval_s: u64,
    /// Replay lag that trips the replication trigger, seconds.
    pub(crate) repl_lag_trigger_s: i64,
    /// Slot-retained WAL that trips the replication trigger, bytes.
    pub(crate) slot_retained_trigger_bytes: i64,
}

pub(crate) fn env_u64(key: &str, default: u64) -> Result<u64> {
    std::env::var(key).map_or_else(
        |_| Ok(default),
        |v| v.parse().with_context(|| format!("{key} is not a u64")),
    )
}

fn env_bool(key: &str, default: bool) -> Result<bool> {
    std::env::var(key).map_or_else(
        |_| Ok(default),
        |v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => anyhow::bail!("{key} must be one of 1/0, true/false, yes/no, on/off"),
        },
    )
}

impl Config {
    #[allow(
        clippy::too_many_lines,
        reason = "environment parsing keeps the validated daemon contract in one place"
    )]
    pub(crate) fn from_env() -> Result<Self> {
        let dsn = std::env::var("KRONIKA_PG_DSN").context("KRONIKA_PG_DSN is not set")?;
        let out_dir: PathBuf = std::env::var("KRONIKA_OUT_DIR")
            .context("KRONIKA_OUT_DIR is not set")?
            .into();
        let source_id = env_u64("KRONIKA_SOURCE_ID", 0)?;
        let session = SessionConfig {
            statement_timeout_ms: env_u64("KRONIKA_PG_STATEMENT_TIMEOUT_MS", 15_000)?,
            lock_timeout_ms: env_u64("KRONIKA_PG_LOCK_TIMEOUT_MS", 1_000)?,
            idle_in_tx_timeout_ms: env_u64("KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS", 10_000)?,
        };
        session.validate().context("invalid session timeouts")?;
        let exclude_databases: HashSet<String> = std::env::var("KRONIKA_PG_EXCLUDE_DATABASES")
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !exclude_databases.is_empty() {
            log_event(
                LogLevel::Info,
                "config_exclude_databases",
                &[field("databases", format!("{exclude_databases:?}"))],
            );
        }
        let max_tables = i64::try_from(env_u64("KRONIKA_PG_MAX_TABLES", 500)?)
            .context("KRONIKA_PG_MAX_TABLES exceeds i64")?;
        let max_indexes = i64::try_from(env_u64("KRONIKA_PG_MAX_INDEXES", 500)?)
            .context("KRONIKA_PG_MAX_INDEXES exceeds i64")?;
        let max_statements = i64::try_from(env_u64("KRONIKA_PG_MAX_STATEMENTS", 500)?)
            .context("KRONIKA_PG_MAX_STATEMENTS exceeds i64")?;
        let max_plans = i64::try_from(env_u64("KRONIKA_PG_MAX_PLANS", 500)?)
            .context("KRONIKA_PG_MAX_PLANS exceeds i64")?;
        let plans_interval = Duration::from_secs(env_u64("KRONIKA_PG_PLANS_INTERVAL_S", 300)?);
        let max_plan_text = i32::try_from(env_u64("KRONIKA_PG_MAX_PLAN_TEXT", 32_768)?)
            .context("KRONIKA_PG_MAX_PLAN_TEXT exceeds i32")?;
        let plan_text_budget = env_u64("KRONIKA_PG_PLAN_TEXT_BUDGET", 8 * 1024 * 1024)?;
        let pool_refresh_secs = env_u64("KRONIKA_PG_POOL_REFRESH_SECS", 600)?;
        let heavy_timeout_cap_ms = env_u64("KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS", 60_000)?;
        let max_lock_rows = i64::try_from(env_u64("KRONIKA_PG_MAX_LOCK_ROWS", 1000)?)
            .context("KRONIKA_PG_MAX_LOCK_ROWS exceeds i64")?;
        let node_self_id = std::env::var("KRONIKA_NODE_SELF_ID")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let tick_secs = env_u64("KRONIKA_INTERVAL_S", 5)?;
        let segment_max_bytes = env_u64("KRONIKA_SEGMENT_MAX_BYTES", 64 * 1024 * 1024)?;
        let segment_max_age_secs = env_u64("KRONIKA_SEGMENT_MAX_AGE_S", 900)?;
        let journal_max_bytes =
            env_u64("KRONIKA_JOURNAL_MAX_BYTES", DEFAULT_MAX_JOURNAL_LEN as u64)?;
        if segment_max_bytes > journal_max_bytes {
            log_event(
                LogLevel::Warn,
                "config_degraded",
                &[
                    field("reason", "segment_cap_exceeds_journal_cap"),
                    field("segment_max_bytes", segment_max_bytes),
                    field("journal_max_bytes", journal_max_bytes),
                ],
            );
        }
        let cycle_db_budget_ms = env_u64("KRONIKA_CYCLE_DB_BUDGET_MS", 15_000)?;
        let activity_fast_interval_s = env_u64("KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S", 1)?;
        let ash_active_threshold = usize::try_from(env_u64("KRONIKA_PG_ASH_ACTIVE_THRESHOLD", 20)?)
            .context("KRONIKA_PG_ASH_ACTIVE_THRESHOLD exceeds usize")?;
        let replication_fast_interval_s = env_u64("KRONIKA_PG_REPLICATION_FAST_INTERVAL_S", 10)?;
        let repl_lag_trigger_s = i64::try_from(env_u64("KRONIKA_PG_REPL_LAG_TRIGGER_S", 10)?)
            .context("KRONIKA_PG_REPL_LAG_TRIGGER_S exceeds i64")?;
        let slot_retained_trigger_bytes = i64::try_from(env_u64(
            "KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES",
            1024 * 1024 * 1024,
        )?)
        .context("KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES exceeds i64")?;
        let intervals = intervals_from_env()?;
        let log = log_config_from_env(&out_dir)?;
        validate_cardinality(max_tables, max_indexes)?;
        validate_heavy_cap(heavy_timeout_cap_ms)?;
        validate_max_lock_rows(max_lock_rows)?;
        validate_max_plans(max_plans)?;
        validate_plan_text_limits(max_plan_text, plan_text_budget)?;
        Ok(Self {
            dsn,
            out_dir,
            source_id,
            session,
            exclude_databases,
            max_tables,
            max_indexes,
            max_statements,
            max_plans,
            plans_interval,
            max_plan_text,
            plan_text_budget,
            pool_refresh_secs,
            heavy_timeout_cap_ms,
            max_lock_rows,
            node_self_id,
            tick_secs,
            intervals,
            log,
            segment_max_bytes,
            segment_max_age_secs,
            journal_max_bytes,
            cycle_db_budget_ms,
            activity_fast_interval_s,
            ash_active_threshold,
            replication_fast_interval_s,
            repl_lag_trigger_s,
            slot_retained_trigger_bytes,
        })
    }
}

/// Read the per-source intervals, falling back to the built-in defaults.
fn intervals_from_env() -> Result<Intervals> {
    let defaults = Intervals::default();
    Ok(Intervals {
        activity: env_u64("KRONIKA_PG_ACTIVITY_INTERVAL_S", defaults.activity)?,
        database: env_u64("KRONIKA_PG_DATABASE_INTERVAL_S", defaults.database)?,
        bgwriter: env_u64("KRONIKA_PG_BGWRITER_INTERVAL_S", defaults.bgwriter)?,
        wal: env_u64("KRONIKA_PG_WAL_INTERVAL_S", defaults.wal)?,
        io: env_u64("KRONIKA_PG_IO_INTERVAL_S", defaults.io)?,
        archiver: env_u64("KRONIKA_PG_ARCHIVER_INTERVAL_S", defaults.archiver)?,
        prepared_xacts: env_u64("KRONIKA_PG_PREPARED_INTERVAL_S", defaults.prepared_xacts)?,
        progress_vacuum: env_u64(
            "KRONIKA_PG_PROGRESS_VACUUM_INTERVAL_S",
            defaults.progress_vacuum,
        )?,
        statements: env_u64("KRONIKA_PG_STATEMENTS_INTERVAL_S", defaults.statements)?,
        user_tables: env_u64("KRONIKA_PG_TABLES_INTERVAL_S", defaults.user_tables)?,
        user_indexes: env_u64("KRONIKA_PG_INDEXES_INTERVAL_S", defaults.user_indexes)?,
        replication: env_u64("KRONIKA_PG_REPLICATION_INTERVAL_S", defaults.replication)?,
        reset_metadata: env_u64(
            "KRONIKA_PG_RESET_METADATA_INTERVAL_S",
            defaults.reset_metadata,
        )?,
        instance_metadata: env_u64("KRONIKA_INSTANCE_INTERVAL_S", defaults.instance_metadata)?,
        settings: env_u64("KRONIKA_PG_SETTINGS_INTERVAL_S", defaults.settings)?,
        os_core: env_u64("KRONIKA_OS_CORE_INTERVAL_S", defaults.os_core)?,
        os_mount_topo: env_u64("KRONIKA_OS_MOUNTTOPO_INTERVAL_S", defaults.os_mount_topo)?,
        os_processes: env_u64("KRONIKA_OS_PROCESS_INTERVAL_S", defaults.os_processes)?,
        os_process_status: env_u64(
            "KRONIKA_OS_PROCESS_STATUS_INTERVAL_S",
            defaults.os_process_status,
        )?,
        os_cgroup: env_u64("KRONIKA_OS_CGROUP_INTERVAL_S", defaults.os_cgroup)?,
        os_cgroup_mapping: env_u64(
            "KRONIKA_OS_CGROUP_MAPPING_INTERVAL_S",
            defaults.os_cgroup_mapping,
        )?,
        pg_log: env_u64("KRONIKA_PG_LOG_INTERVAL_S", defaults.pg_log)?,
    })
}

fn log_config_from_env(out_dir: &Path) -> Result<LogConfig> {
    let path_override = std::env::var("KRONIKA_LOG_PATH")
        .ok()
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let enabled = env_bool("KRONIKA_PG_LOG_ENABLED", path_override.is_some())?;
    let root_override = std::env::var("KRONIKA_LOG_ROOT")
        .ok()
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let parser_kind = match std::env::var("KRONIKA_LOG_FORMAT")
        .unwrap_or_else(|_| "stderr".to_owned())
        .trim()
    {
        "stderr" => LogParserKind::Stderr,
        "csvlog" => LogParserKind::Csvlog,
        "unknown" => LogParserKind::Unknown,
        other => anyhow::bail!("KRONIKA_LOG_FORMAT must be stderr or csvlog, got {other:?}"),
    };
    let state_path = std::env::var("KRONIKA_LOG_STATE_PATH")
        .map_or_else(|_| out_dir.join("pg_log_tail.state"), PathBuf::from);
    Ok(LogConfig {
        enabled,
        path_override,
        root_override,
        parser_kind,
        state_path,
        start_at_beginning: env_bool("KRONIKA_LOG_START_AT_BEGINNING", false)?,
        discovery_interval: Duration::from_secs(env_u64("KRONIKA_LOG_DISCOVERY_INTERVAL_S", 60)?),
        tail_caps: kronika_source_log::TailCaps::default(),
    })
}

/// Reject per-axis top-N counts that could overflow a single section.
///
/// Worst case, every covered database contributes `axes * top_n` rows to one
/// section. That product must stay under [`MAX_SECTION_ROWS`], or the sealed
/// section is rejected at encode time and the whole segment is lost. Bounding the
/// env here turns a mid-run failure into a clear startup error.
///
/// # Errors
/// Returns an error naming the env and the limit when either product overflows.
pub(crate) fn validate_cardinality(max_tables: i64, max_indexes: i64) -> Result<()> {
    let cap = i64::try_from(MAX_SECTION_ROWS).context("MAX_SECTION_ROWS exceeds i64")?;
    let databases =
        i64::try_from(DEFAULT_MAX_DATABASES).context("DEFAULT_MAX_DATABASES exceeds i64")?;
    check_section_bound(
        "KRONIKA_PG_MAX_TABLES",
        databases,
        TABLE_TOPN_AXES,
        max_tables,
        cap,
    )?;
    check_section_bound(
        "KRONIKA_PG_MAX_INDEXES",
        databases,
        INDEX_TOPN_AXES,
        max_indexes,
        cap,
    )
}

/// Fail unless `databases * axes * top_n <= cap`.
///
/// # Errors
/// Returns an error naming `env`, the worst-case row count, and `cap`.
fn check_section_bound(env: &str, databases: i64, axes: i64, top_n: i64, cap: i64) -> Result<()> {
    let worst_case = databases
        .checked_mul(axes)
        .and_then(|rows| rows.checked_mul(top_n))
        .with_context(|| format!("{env}: worst-case section row count overflows i64"))?;
    anyhow::ensure!(
        worst_case <= cap,
        "{env} is too high: {databases} databases * {axes} axes * {top_n} = {worst_case} rows \
         exceeds the {cap}-row section cap; lower {env}"
    );
    Ok(())
}

/// Reject a heavy-query timeout cap of zero.
///
/// A cap of 0 makes the adaptive loop set `statement_timeout = 0`, which disables
/// the timeout entirely, so a runaway size query has no guard.
///
/// # Errors
/// Returns an error naming the env when the cap is zero.
pub(crate) fn validate_heavy_cap(heavy_timeout_cap_ms: u64) -> Result<()> {
    anyhow::ensure!(
        heavy_timeout_cap_ms > 0,
        "KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS must be greater than 0: a cap of 0 sets \
         statement_timeout = 0, which removes the guard on the heavy size query"
    );
    Ok(())
}

/// Reject plan-text limits the segment dictionary cannot absorb.
///
/// The dictionary truncates one entry at 64 KiB and caps the whole window at
/// 16 MiB; env values past those bounds would fail the snapshot during
/// interning instead of at startup.
///
/// # Errors
/// Returns an error naming the env and the bound when out of range.
pub(crate) fn validate_plan_text_limits(max_plan_text: i32, plan_text_budget: u64) -> Result<()> {
    anyhow::ensure!(
        max_plan_text > 0 && max_plan_text <= 64 * 1024,
        "KRONIKA_PG_MAX_PLAN_TEXT must be in 1..=65536, got {max_plan_text}"
    );
    anyhow::ensure!(
        plan_text_budget <= 16 * 1024 * 1024,
        "KRONIKA_PG_PLAN_TEXT_BUDGET must not exceed 16777216, got {plan_text_budget}"
    );
    Ok(())
}

/// Reject a `pg_store_plans` top-N that could overflow a single section.
///
/// # Errors
/// Returns an error naming the env and the limit when out of range.
pub(crate) fn validate_max_plans(max_plans: i64) -> Result<()> {
    let cap = i64::try_from(MAX_SECTION_ROWS).context("MAX_SECTION_ROWS exceeds i64")?;
    anyhow::ensure!(
        max_plans > 0 && max_plans <= cap,
        "KRONIKA_PG_MAX_PLANS must be in 1..={cap}, got {max_plans}"
    );
    Ok(())
}

/// Reject a lock-row cap that would overflow a single section.
///
/// # Errors
/// Returns an error naming the env and the limit when the value exceeds
/// [`MAX_SECTION_ROWS`].
pub(crate) fn validate_max_lock_rows(max_lock_rows: i64) -> Result<()> {
    let cap = i64::try_from(MAX_SECTION_ROWS).context("MAX_SECTION_ROWS exceeds i64")?;
    anyhow::ensure!(
        max_lock_rows > 0,
        "KRONIKA_PG_MAX_LOCK_ROWS must be greater than 0"
    );
    anyhow::ensure!(
        max_lock_rows <= cap,
        "KRONIKA_PG_MAX_LOCK_ROWS ({max_lock_rows}) exceeds the {cap}-row section cap; \
         lower KRONIKA_PG_MAX_LOCK_ROWS"
    );
    Ok(())
}

/// Reject a `pg_settings` snapshot that cannot fit into one section.
///
/// `PostgreSQL`'s `GUC` set is expected to be small and bounded by server code
/// plus loaded extensions, but the collector still checks the hard PGM section
/// cap before any `pg_settings` strings are interned.
///
/// # Errors
/// Returns an error naming the section and the row cap when out of range.
pub(crate) fn validate_settings_row_count(rows: usize) -> Result<()> {
    anyhow::ensure!(
        rows <= MAX_SECTION_ROWS,
        "pg_settings returned {rows} rows, exceeding the {MAX_SECTION_ROWS}-row section cap"
    );
    Ok(())
}

const REPLICA_DICT_BYTES_PER_ROW: i64 = 224;
const SLOT_DICT_BYTES_PER_ROW: i64 = 152;

/// Reject replication-detail GUC bounds that can overflow section or dictionary limits.
///
/// The strings in both views are bounded by `PostgreSQL` `name`, `inet` text, and
/// fixed status labels. These constants overestimate the unique bytes per row,
/// so a high GUC fails before the collector materializes the view output.
///
/// # Errors
/// Returns an error naming the GUC or dictionary cap that would be exceeded.
pub(crate) fn validate_replication_detail_bounds(bounds: ReplicationDetailBounds) -> Result<()> {
    let section_cap = i64::try_from(MAX_SECTION_ROWS).context("MAX_SECTION_ROWS exceeds i64")?;
    check_replication_detail_section_bound("max_wal_senders", bounds.max_wal_senders, section_cap)?;
    check_replication_detail_section_bound(
        "max_replication_slots",
        bounds.max_replication_slots,
        section_cap,
    )?;

    let dict_cap = i64::try_from(activity_dict_limits().max_total_bytes())
        .context("activity dictionary byte cap exceeds i64")?;
    let replica_bytes = bounds
        .max_wal_senders
        .checked_mul(REPLICA_DICT_BYTES_PER_ROW)
        .context("max_wal_senders dictionary estimate overflows i64")?;
    let slot_bytes = bounds
        .max_replication_slots
        .checked_mul(SLOT_DICT_BYTES_PER_ROW)
        .context("max_replication_slots dictionary estimate overflows i64")?;
    let worst_case = replica_bytes
        .checked_add(slot_bytes)
        .context("replication detail dictionary estimate overflows i64")?;
    anyhow::ensure!(
        worst_case <= dict_cap,
        "replication detail labels can require {worst_case} dictionary bytes from \
         max_wal_senders={} and max_replication_slots={}, exceeding the {dict_cap}-byte cap",
        bounds.max_wal_senders,
        bounds.max_replication_slots
    );
    Ok(())
}

fn check_replication_detail_section_bound(guc: &str, value: i64, cap: i64) -> Result<()> {
    anyhow::ensure!(value >= 0, "{guc} must be non-negative, got {value}");
    anyhow::ensure!(
        value <= cap,
        "{guc} ({value}) exceeds the {cap}-row section cap; lower {guc}"
    );
    Ok(())
}
