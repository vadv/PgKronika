//! Collects `PostgreSQL` stats and writes sealed PGM segments.
//!
//! The daemon runs on the database host. A collection signal gathers the enabled
//! `PostgreSQL` sources, writes one journal part, seals `<ts>.pgm`, and resets
//! the journal after a successful seal.
//!
//! Environment:
//! - `KRONIKA_PG_DSN`: libpq connection string (key=value or URI) for the target
//!   server;
//! - `KRONIKA_OUT_DIR`: directory that receives sealed segments;
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
//! - `KRONIKA_PG_POOL_REFRESH_SECS`: minimum interval between connection-pool
//!   refreshes (per-database connection reconciliation), default 600;
//! - `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS`: cap for the adaptive `statement_timeout`
//!   of the heavy per-table size query, default 60000. A `57014` (query canceled)
//!   widens the timeout and retries the same database until this cap;
//! - `KRONIKA_PG_MAX_LOCK_ROWS`: maximum rows returned by the lock-wait tree
//!   query, default 1000. The section is skipped entirely when no waits exist.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

use anyhow::{Context, Result};
use kronika_format::DictLimits;
use kronika_registry::{MAX_SECTION_ROWS, StrId};
use kronika_source_pg::archiver::{ArchiverRow, collect_archiver, to_archiver};
use kronika_source_pg::database::{self, DatabaseRow, DatabaseVersion, collect_database};
use kronika_source_pg::io::{self, IoRow, IoVersion, collect_io};
use kronika_source_pg::locks::{
    LocksRow, LocksVersion, collect_locks, lock_waits_exist, locks_version, to_v1 as locks_to_v1,
    to_v2 as locks_to_v2,
};
use kronika_source_pg::pool::{
    AdaptiveTimeout, ConnectionPool, DEFAULT_MAX_DATABASES, SessionConfig,
};
use kronika_source_pg::prepared_xacts::{
    PreparedXactsRow, collect_prepared_xacts, to_prepared_xacts,
};
use kronika_source_pg::progress_vacuum::{
    ProgressVacuumRow, collect_progress_vacuum, to_progress_vacuum,
};
use kronika_source_pg::replication_instance::{
    ReplicationInstanceRow, collect_replication_instance, to_replication_instance,
};
use kronika_source_pg::statements::{
    self, StatementsRow, StatementsVersion, collect_statements, statements_extversion,
    statements_version,
};
use kronika_source_pg::user_indexes::{
    self, INDEX_TOPN_AXES, UserIndexesRow, UserIndexesVersion, collect_user_indexes,
};
use kronika_source_pg::user_tables::{
    self, TABLE_TOPN_AXES, UserTablesRow, UserTablesVersion, collect_user_tables,
};
use kronika_source_pg::wal::{WalSnapshot, collect_wal};
use kronika_source_pg::{
    ActivityRow, ActivityVersion, collect_activity, collect_bgwriter_checkpointer, to_v1, to_v2,
    to_v3,
};
use kronika_writer::{Interner, Journal, JournalConfig, SectionBuffers, dict, seal};
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use tokio::signal::unix::{SignalKind, signal};
use tokio_postgres::Client;

struct Config {
    dsn: String,
    out_dir: PathBuf,
    source_id: u64,
    session: SessionConfig,
    exclude_databases: HashSet<String>,
    /// Per-axis top-N row count for the `pg_stat_user_tables` candidate selection.
    max_tables: i64,
    /// Per-axis top-N row count for the `pg_stat_user_indexes` candidate selection.
    max_indexes: i64,
    /// Per-axis top-N row count for the `pg_stat_statements` candidate selection.
    max_statements: i64,
    /// Minimum interval between connection-pool refreshes, seconds.
    pool_refresh_secs: u64,
    /// Cap for the adaptive `statement_timeout` of the heavy per-table query, ms.
    heavy_timeout_cap_ms: u64,
    /// Maximum rows returned by the lock-wait tree query.
    max_lock_rows: i64,
}

fn env_u64(key: &str, default: u64) -> Result<u64> {
    std::env::var(key).map_or_else(
        |_| Ok(default),
        |v| v.parse().with_context(|| format!("{key} is not a u64")),
    )
}

impl Config {
    fn from_env() -> Result<Self> {
        let dsn = std::env::var("KRONIKA_PG_DSN").context("KRONIKA_PG_DSN is not set")?;
        let out_dir = std::env::var("KRONIKA_OUT_DIR")
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
            eprintln!("pg_kronika: excluding databases: {exclude_databases:?}");
        }
        let max_tables = i64::try_from(env_u64("KRONIKA_PG_MAX_TABLES", 500)?)
            .context("KRONIKA_PG_MAX_TABLES exceeds i64")?;
        let max_indexes = i64::try_from(env_u64("KRONIKA_PG_MAX_INDEXES", 500)?)
            .context("KRONIKA_PG_MAX_INDEXES exceeds i64")?;
        let max_statements = i64::try_from(env_u64("KRONIKA_PG_MAX_STATEMENTS", 500)?)
            .context("KRONIKA_PG_MAX_STATEMENTS exceeds i64")?;
        let pool_refresh_secs = env_u64("KRONIKA_PG_POOL_REFRESH_SECS", 600)?;
        let heavy_timeout_cap_ms = env_u64("KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS", 60_000)?;
        let max_lock_rows = i64::try_from(env_u64("KRONIKA_PG_MAX_LOCK_ROWS", 1000)?)
            .context("KRONIKA_PG_MAX_LOCK_ROWS exceeds i64")?;
        validate_cardinality(max_tables, max_indexes)?;
        validate_heavy_cap(heavy_timeout_cap_ms)?;
        validate_max_lock_rows(max_lock_rows)?;
        Ok(Self {
            dsn,
            out_dir,
            source_id,
            session,
            exclude_databases,
            max_tables,
            max_indexes,
            max_statements,
            pool_refresh_secs,
            heavy_timeout_cap_ms,
            max_lock_rows,
        })
    }
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
fn validate_cardinality(max_tables: i64, max_indexes: i64) -> Result<()> {
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
fn validate_heavy_cap(heavy_timeout_cap_ms: u64) -> Result<()> {
    anyhow::ensure!(
        heavy_timeout_cap_ms > 0,
        "KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS must be greater than 0: a cap of 0 sets \
         statement_timeout = 0, which removes the guard on the heavy size query"
    );
    Ok(())
}

/// Reject a lock-row cap that would overflow a single section.
///
/// # Errors
/// Returns an error naming the env and the limit when the value exceeds
/// [`MAX_SECTION_ROWS`].
fn validate_max_lock_rows(max_lock_rows: i64) -> Result<()> {
    let cap = i64::try_from(MAX_SECTION_ROWS).context("MAX_SECTION_ROWS exceeds i64")?;
    anyhow::ensure!(
        max_lock_rows <= cap,
        "KRONIKA_PG_MAX_LOCK_ROWS ({max_lock_rows}) exceeds the {cap}-row section cap; \
         lower KRONIKA_PG_MAX_LOCK_ROWS"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    std::fs::create_dir_all(&config.out_dir).context("create the output directory")?;

    let mut pool = ConnectionPool::connect(
        &config.dsn,
        &format!("pg_kronika-collector/{}", env!("CARGO_PKG_VERSION")),
        config.session,
        config.exclude_databases.clone(),
    )
    .await
    .context("connect pool")?;

    // Only sealed segments leave this process.
    let journal_dir = tempfile::tempdir().context("create the journal directory")?;
    let (mut journal, _report) = Journal::open(
        &journal_dir.path().join("active.parts"),
        JournalConfig::default(),
    )
    .context("open the journal")?;

    let mut sigusr2 = signal(SignalKind::user_defined2()).context("install the SIGUSR2 handler")?;
    let mut sigterm = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;
    let mut statements_cache = StatementsSourceCache::default();

    announce("ready");

    loop {
        tokio::select! {
            Some(()) = sigusr2.recv() => {
                if let Err(err) = pool.ensure_main().await {
                    eprintln!("pg_kronika-collector: main reconnect failed: {err:#}");
                    continue;
                }
                if let Err(err) = pool
                    .refresh(
                        std::time::Duration::from_secs(config.pool_refresh_secs),
                        DEFAULT_MAX_DATABASES,
                    )
                    .await
                {
                    eprintln!("pg_kronika-collector: pool refresh failed: {err:#}");
                }
                for db in pool.uncovered() {
                    eprintln!("pg_kronika-collector: database not covered this cycle: {db}");
                }
                let major = pool.server_major();
                match snapshot_and_seal(&pool, major, &mut journal, &config, &mut statements_cache).await {
                    Ok(dest) => announce(&format!("sealed {}", dest.display())),
                    Err(err) => eprintln!("pg_kronika-collector: snapshot failed: {err:#}"),
                }
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }
    }
    Ok(())
}

/// Collect `pg_stat_user_tables` from every pool database, returning owned rows.
///
/// All awaits finish here so the caller can intern without holding the `!Send`
/// `Interner` across an await. The heavy size query runs under an adaptive
/// `statement_timeout`: SQLSTATE `57014` widens it and retries the same database
/// until the cap; any other error logs and skips that database so one bad
/// database does not lose the whole segment.
async fn collect_user_tables_all(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
) -> Vec<(String, UserTablesVersion, Vec<UserTablesRow>)> {
    let mut user_tables = Vec::new();
    let mut heavy = AdaptiveTimeout::new(15_000, config.heavy_timeout_cap_ms);
    for db in pool.per_db() {
        loop {
            // The heavy size functions can be slow, so this query runs under a
            // wider statement_timeout. SET persists on the connection: it stays
            // in effect until the next database's SET overwrites it.
            if let Err(err) = db
                .client()
                .batch_execute(&format!("SET statement_timeout = {}", heavy.current_ms()))
                .await
            {
                eprintln!(
                    "pg_kronika-collector: SET statement_timeout failed for {}: {err}; \
                     user_tables query proceeds under the previously-set timeout",
                    db.datname
                );
            }
            match collect_user_tables(db.client(), major, config.max_tables).await {
                Ok((version, rows)) => {
                    user_tables.push((db.datname.clone(), version, rows));
                    break;
                }
                Err(err) if is_sqlstate(&err, "57014") && !heavy.at_cap() => {
                    heavy.grow(); // statement_timeout hit; retry this database wider
                }
                Err(err) if is_sqlstate(&err, "55P03") => {
                    // lock_not_available: another session holds a conflicting lock.
                    // Label it distinctly so contention is not read as a query bug.
                    eprintln!(
                        "pg_kronika-collector: skip user_tables for {} (lock_not_available): {err}",
                        db.datname
                    );
                    break;
                }
                Err(err) => {
                    eprintln!(
                        "pg_kronika-collector: skip user_tables for {}: {err}",
                        db.datname
                    );
                    break;
                }
            }
        }
    }
    user_tables
}

/// Collect `pg_stat_user_indexes` from every pool database, returning owned rows.
///
/// Mirrors [`collect_user_tables_all`]: all awaits finish here so the caller can
/// intern without holding the `!Send` `Interner` across an await. The size query
/// runs under an adaptive `statement_timeout`: SQLSTATE `57014` widens it and
/// retries the same database until the cap; any other error logs and skips that
/// database so one bad database does not lose the whole segment.
async fn collect_user_indexes_all(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
) -> Vec<(String, UserIndexesVersion, Vec<UserIndexesRow>)> {
    let mut user_indexes = Vec::new();
    let mut heavy = AdaptiveTimeout::new(15_000, config.heavy_timeout_cap_ms);
    for db in pool.per_db() {
        loop {
            // pg_relation_size over many indexes can be slow, so this query runs
            // under a wider statement_timeout. SET persists on the connection: it
            // stays in effect until the next database's SET overwrites it.
            if let Err(err) = db
                .client()
                .batch_execute(&format!("SET statement_timeout = {}", heavy.current_ms()))
                .await
            {
                eprintln!(
                    "pg_kronika-collector: SET statement_timeout failed for {}: {err}; \
                     user_indexes query proceeds under the previously-set timeout",
                    db.datname
                );
            }
            match collect_user_indexes(db.client(), major, config.max_indexes).await {
                Ok((version, rows)) => {
                    user_indexes.push((db.datname.clone(), version, rows));
                    break;
                }
                Err(err) if is_sqlstate(&err, "57014") && !heavy.at_cap() => {
                    heavy.grow(); // statement_timeout hit; retry this database wider
                }
                Err(err) if is_sqlstate(&err, "55P03") => {
                    // lock_not_available: another session holds a conflicting lock.
                    // Label it distinctly so contention is not read as a query bug.
                    eprintln!(
                        "pg_kronika-collector: skip user_indexes for {} (lock_not_available): {err}",
                        db.datname
                    );
                    break;
                }
                Err(err) => {
                    eprintln!(
                        "pg_kronika-collector: skip user_indexes for {}: {err}",
                        db.datname
                    );
                    break;
                }
            }
        }
    }
    user_indexes
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StatementsSource {
    Main,
    Database(String),
}

impl StatementsSource {
    fn label(&self) -> String {
        match self {
            Self::Main => "main".to_owned(),
            Self::Database(datname) => format!("database {datname}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedStatementsSource {
    source: StatementsSource,
    extversion: String,
    version: StatementsVersion,
}

impl CachedStatementsSource {
    fn new(source: StatementsSource, extversion: String) -> Self {
        let version = statements_version(&extversion);
        Self {
            source,
            extversion,
            version,
        }
    }

    fn matches_extversion(&self, extversion: &str) -> bool {
        self.extversion == extversion && self.version == statements_version(extversion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MissingStatementsSource {
    covered_databases: Vec<String>,
    next_probe: usize,
}

impl MissingStatementsSource {
    const fn new(covered_databases: Vec<String>) -> Self {
        Self {
            covered_databases,
            next_probe: 0,
        }
    }

    fn matches_covered(&self, covered_databases: &[String]) -> bool {
        self.covered_databases == covered_databases
    }

    const fn next_per_db_probe(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let index = self.next_probe % len;
        self.next_probe = (index + 1) % len;
        Some(index)
    }
}

#[derive(Debug, Default)]
struct StatementsSourceCache {
    selected: Option<CachedStatementsSource>,
    missing: Option<MissingStatementsSource>,
}

impl StatementsSourceCache {
    fn store(&mut self, source: StatementsSource, extversion: String) -> StatementsVersion {
        let cached = CachedStatementsSource::new(source, extversion);
        let version = cached.version;
        self.selected = Some(cached);
        self.missing = None;
        version
    }

    fn invalidate(&mut self) {
        self.selected = None;
        self.missing = None;
    }

    fn mark_missing(&mut self, covered_databases: Vec<String>) {
        self.selected = None;
        self.missing = Some(MissingStatementsSource::new(covered_databases));
    }
}

struct StatementsCandidate<'a> {
    source: StatementsSource,
    client: &'a Client,
}

fn covered_statement_databases(pool: &ConnectionPool) -> Vec<String> {
    pool.per_db()
        .iter()
        .filter(|db| !db.client().is_closed())
        .map(|db| db.datname.clone())
        .collect()
}

fn statement_client<'a>(pool: &'a ConnectionPool, source: &StatementsSource) -> Option<&'a Client> {
    match source {
        StatementsSource::Main => (!pool.main().is_closed()).then_some(pool.main()),
        StatementsSource::Database(datname) => pool
            .per_db()
            .iter()
            .find(|db| db.datname == *datname && !db.client().is_closed())
            .map(kronika_source_pg::pool::DatabaseConn::client),
    }
}

fn all_statements_candidates(pool: &ConnectionPool) -> Vec<StatementsCandidate<'_>> {
    let mut candidates = Vec::with_capacity(1 + pool.per_db().len());
    if !pool.main().is_closed() {
        candidates.push(StatementsCandidate {
            source: StatementsSource::Main,
            client: pool.main(),
        });
    }
    candidates.extend(
        pool.per_db()
            .iter()
            .filter(|db| !db.client().is_closed())
            .map(|db| StatementsCandidate {
                source: StatementsSource::Database(db.datname.clone()),
                client: db.client(),
            }),
    );
    candidates
}

fn incremental_statements_candidates<'a>(
    pool: &'a ConnectionPool,
    cache: &mut StatementsSourceCache,
) -> Vec<StatementsCandidate<'a>> {
    let live = pool
        .per_db()
        .iter()
        .filter(|db| !db.client().is_closed())
        .collect::<Vec<_>>();
    let mut candidates = Vec::with_capacity(2);
    if !pool.main().is_closed() {
        candidates.push(StatementsCandidate {
            source: StatementsSource::Main,
            client: pool.main(),
        });
    }
    if let Some(index) = cache
        .missing
        .as_mut()
        .and_then(|missing| missing.next_per_db_probe(live.len()))
    {
        let db = live[index];
        candidates.push(StatementsCandidate {
            source: StatementsSource::Database(db.datname.clone()),
            client: db.client(),
        });
    }
    candidates
}

async fn collect_statements_from_candidate(
    candidate: StatementsCandidate<'_>,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>)> {
    let label = candidate.source.label();
    let extversion = match statements_extversion(candidate.client).await {
        Ok(Some(extversion)) => extversion,
        Ok(None) => return None,
        Err(err) => {
            eprintln!("pg_kronika-collector: pg_stat_statements probe failed on {label}: {err}");
            return None;
        }
    };
    let version = statements_version(&extversion);
    match collect_statements(candidate.client, version, config.max_statements).await {
        Ok(rows) => {
            let version = cache.store(candidate.source, extversion);
            Some((version, rows))
        }
        Err(err) => {
            eprintln!("pg_kronika-collector: pg_stat_statements query failed on {label}: {err}");
            None
        }
    }
}

async fn discover_statements_source(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>)> {
    for candidate in all_statements_candidates(pool) {
        if let Some(rows) = collect_statements_from_candidate(candidate, config, cache).await {
            return Some(rows);
        }
    }
    cache.mark_missing(covered_statement_databases(pool));
    eprintln!(
        "pg_kronika-collector: no usable pg_stat_statements source on main or covered per-db connections; skipping section 1_002"
    );
    None
}

async fn rediscover_missing_statements_source(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>)> {
    for candidate in incremental_statements_candidates(pool, cache) {
        if let Some(rows) = collect_statements_from_candidate(candidate, config, cache).await {
            return Some(rows);
        }
    }
    None
}

/// Collect `pg_stat_statements` from one cached source connection.
///
/// The view is instance-wide and rows identify the execution database with
/// `dbid`, so the collector queries one reachable database that has the
/// extension installed. Source discovery checks `pool.main()` first, then the
/// covered per-db pool connections; databases outside the pool cap are invisible
/// to this discovery until an explicit source-db setting exists. All awaits
/// finish here so the caller can intern without holding the `!Send` `Interner`
/// across an await.
async fn collect_statements_cached(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>)> {
    if let Some(cached) = cache.selected.clone() {
        let label = cached.source.label();
        if let Some(client) = statement_client(pool, &cached.source) {
            match statements_extversion(client).await {
                Ok(Some(extversion)) if cached.matches_extversion(&extversion) => {
                    match collect_statements(client, cached.version, config.max_statements).await {
                        Ok(rows) => return Some((cached.version, rows)),
                        Err(err) => {
                            eprintln!(
                                "pg_kronika-collector: pg_stat_statements query failed on cached {label}: {err}; rediscovering"
                            );
                            cache.invalidate();
                        }
                    }
                }
                Ok(Some(extversion)) => {
                    eprintln!(
                        "pg_kronika-collector: pg_stat_statements version changed on cached {label} ({} -> {extversion}); rediscovering",
                        cached.extversion
                    );
                    cache.invalidate();
                }
                Ok(None) => {
                    eprintln!(
                        "pg_kronika-collector: pg_stat_statements extension missing on cached {label}; rediscovering"
                    );
                    cache.invalidate();
                }
                Err(err) => {
                    eprintln!(
                        "pg_kronika-collector: pg_stat_statements probe failed on cached {label}: {err}; rediscovering"
                    );
                    cache.invalidate();
                }
            }
        } else {
            eprintln!(
                "pg_kronika-collector: cached pg_stat_statements source {label} is unavailable; rediscovering"
            );
            cache.invalidate();
        }
    }

    let covered = covered_statement_databases(pool);
    if cache
        .missing
        .as_ref()
        .is_some_and(|missing| missing.matches_covered(&covered))
    {
        return rediscover_missing_statements_source(pool, config, cache).await;
    }
    discover_statements_source(pool, config, cache).await
}

async fn snapshot_and_seal(
    pool: &ConnectionPool,
    major: u32,
    journal: &mut Journal,
    config: &Config,
    statements_cache: &mut StatementsSourceCache,
) -> Result<PathBuf> {
    let client = pool.main();
    // Run every query first: SectionBuffers and Interner are `!Send`, so they
    // must not be held across an await.
    let bgwriter = collect_bgwriter_checkpointer(client, major)
        .await
        .context("collect type 1_006_001")?;
    let ts = bgwriter.ts;
    let (activity_version, activity_rows) = collect_activity(client, major)
        .await
        .context("collect pg_stat_activity")?;
    let (database_version, database_rows) = collect_database(client, major)
        .await
        .context("collect pg_stat_database")?;
    let progress_vacuum_rows = collect_progress_vacuum(client, major)
        .await
        .context("collect pg_stat_progress_vacuum")?;
    let prepared_rows = collect_prepared_xacts(client)
        .await
        .context("collect pg_prepared_xacts")?;
    let wal = collect_wal(client, major)
        .await
        .context("collect pg_stat_wal")?;
    // pg_stat_io exists from PG16; `None` on older majors.
    let io = collect_io(client, major)
        .await
        .context("collect pg_stat_io")?;
    let archiver = collect_archiver(client)
        .await
        .context("collect pg_stat_archiver")?;
    let replication_instance_row = collect_replication_instance(client, major)
        .await
        .context("collect replication instance status")?;
    let lock_rows = match lock_waits_exist(client).await {
        Ok(true) => collect_locks(client, major, config.max_lock_rows)
            .await
            .context("collect pg_locks wait tree")?,
        Ok(false) => Vec::new(),
        Err(err) => {
            eprintln!(
                "pg_kronika-collector: lock-wait precheck failed; skipping section 1_011: {err}"
            );
            Vec::new()
        }
    };

    let user_tables = collect_user_tables_all(pool, major, config).await;
    let user_indexes = collect_user_indexes_all(pool, major, config).await;
    let statements = collect_statements_cached(pool, config, statements_cache).await;

    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    buffers
        .push(bgwriter)
        .map_err(|_row| anyhow::anyhow!("section buffer full for bgwriter"))?;
    push_activity(
        &mut buffers,
        &mut interner,
        activity_version,
        &activity_rows,
    )?;
    push_database(
        &mut buffers,
        &mut interner,
        database_version,
        &database_rows,
    )?;
    push_progress_vacuum(&mut buffers, &mut interner, &progress_vacuum_rows)?;
    push_prepared_xacts(&mut buffers, &mut interner, &prepared_rows)?;
    // pg_stat_wal has one all-numeric row; PG10-13 produce no row.
    match wal {
        Some(WalSnapshot::V1(row)) => buffer_row(&mut buffers, row)?,
        Some(WalSnapshot::V2(row)) => buffer_row(&mut buffers, row)?,
        None => {}
    }
    if let Some((io_version, io_rows)) = &io {
        push_io(&mut buffers, &mut interner, *io_version, io_rows)?;
    }
    push_archiver(&mut buffers, &mut interner, &archiver)?;
    push_replication_instance(&mut buffers, &mut interner, &replication_instance_row)?;
    push_user_tables(&mut buffers, &mut interner, &user_tables)?;
    push_user_indexes(&mut buffers, &mut interner, &user_indexes)?;
    if let Some((version, rows)) = &statements {
        push_statements(&mut buffers, &mut interner, *version, rows)?;
    }
    if !lock_rows.is_empty() {
        push_locks(
            &mut buffers,
            &mut interner,
            locks_version(major),
            &lock_rows,
        )?;
    }

    let dict_sections = dict::encode(interner.window()).context("encode the segment dictionary")?;
    let part = buffers
        .flush(&dict_sections, config.source_id)
        .context("encode the collection window")?
        .context("a buffered row must yield a part")?;
    journal
        .append(&part)
        .context("append the part to the journal")?;

    let dest = config.out_dir.join(format!("{}.pgm", ts.0));
    seal(journal, &dest).context("seal the segment")?;
    // Leave active.parts intact if seal() fails.
    journal.reset().context("reset the journal after seal")?;
    Ok(dest)
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

/// Whether a tokio-postgres error carries the given SQLSTATE code.
fn is_sqlstate(err: &tokio_postgres::Error, code: &str) -> bool {
    err.code().is_some_and(|state| state.code() == code)
}

/// Buffer one typed snapshot row, mapping a full buffer to an error.
fn buffer_row<S: kronika_registry::Section + 'static>(
    buffers: &mut SectionBuffers,
    row: S,
) -> Result<()> {
    buffers
        .push(row)
        .map_err(|_row| anyhow::anyhow!("section buffer is full"))
}

fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .ok();
}

#[cfg(test)]
mod tests {
    use super::{
        CachedStatementsSource, MissingStatementsSource, StatementsSource, StatementsSourceCache,
        activity_dict_limits, push_activity, push_archiver, push_database, push_io, push_locks,
        push_prepared_xacts, push_progress_vacuum, push_replication_instance, push_statements,
        push_user_indexes, push_user_tables, validate_cardinality, validate_heavy_cap,
        validate_max_lock_rows,
    };
    use kronika_registry::MAX_SECTION_ROWS;
    use kronika_source_pg::archiver::ArchiverRow;
    use kronika_source_pg::database::{DatabaseRow, DatabaseVersion};
    use kronika_source_pg::io::{IoRow, IoVersion};
    use kronika_source_pg::locks::{LocksRow, LocksVersion};
    use kronika_source_pg::prepared_xacts::PreparedXactsRow;
    use kronika_source_pg::progress_vacuum::ProgressVacuumRow;
    use kronika_source_pg::replication_instance::ReplicationInstanceRow;
    use kronika_source_pg::statements::{StatementsRow, StatementsVersion};
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
            lock_relation: None,
            lock_relname: None,
            lock_page: None,
            lock_tuple: None,
            lock_transactionid: None,
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
}
