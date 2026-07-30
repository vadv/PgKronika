use crate::budget::{PoolBudget, PoolSource};
use crate::config::Config;
use crate::coverage::{CoverageAttempt, SourceCoverage};
use crate::logging::{
    LogLevel, field, layout_id, log_database_collection_finish, log_database_collection_retry,
    log_database_collection_skip, log_database_collection_start, log_event, section_name,
};
use crate::scheduler::{DueSet, SourceKind};
use crate::source_contracts::{user_indexes_type_id, user_tables_type_id};
use crate::statements_source::{StatementsSourceCache, collect_statements_cached};
use kronika_source_pg::incident_gauges::{FreezeHorizonRow, collect_freeze_horizons};
use kronika_source_pg::pool::{AdaptiveTimeout, ConnectionPool, DEFAULT_MAX_DATABASES};
use kronika_source_pg::statements::{StatementsRow, StatementsVersion};
use kronika_source_pg::user_indexes::{UserIndexesRow, UserIndexesVersion, collect_user_indexes};
use kronika_source_pg::user_tables::{UserTablesRow, UserTablesVersion, collect_user_tables};
use std::time::Instant;

/// Carry pool connection failures and the database cap into the same attempt
/// as the successful per-database reads.
fn record_pool_gaps(pool: &ConnectionPool, coverage: &mut SourceCoverage) {
    let covered_target = pool
        .expected()
        .iter()
        .take(DEFAULT_MAX_DATABASES)
        .all(|expected| pool.per_db().iter().any(|db| db.datname == *expected));
    if !covered_target {
        coverage.record_other_failure();
    }
    if pool.expected().len() > DEFAULT_MAX_DATABASES {
        coverage.record_collector_loss();
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
    cycle_ts_us: i64,
) -> (
    Vec<(String, UserTablesVersion, Vec<UserTablesRow>)>,
    CoverageAttempt,
) {
    let mut user_tables = Vec::new();
    let mut coverage = SourceCoverage::new_attempt();
    record_pool_gaps(pool, &mut coverage);
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
            match collect_user_tables(db.client(), major, config.max_tables, cycle_ts_us).await {
                Ok((version, rows, source_total)) => {
                    coverage.record_success(rows.len(), source_total);
                    // The total rides in the same statement as the rows, so
                    // it describes exactly the population they were cut from.
                    // Every selection axis is an unfiltered ORDER BY, so an
                    // empty read means an empty source: a zero total is exact.
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
                    coverage.record_other_failure();
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
                        coverage.record_timeout();
                        "statement_timeout"
                    } else if is_sqlstate(&err, "42501") {
                        coverage.record_permission_failure();
                        "permission_denied"
                    } else {
                        coverage.record_other_failure();
                        "query_failed"
                    };
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
    (
        user_tables,
        CoverageAttempt {
            ts: cycle_ts_us,
            section_type_id: type_id,
            coverage,
        },
    )
}

/// Collect the two-axis freeze candidates from every database.
async fn collect_freeze_horizons_all(
    pool: &ConnectionPool,
    config: &Config,
) -> Vec<FreezeHorizonRow> {
    const MAX_FREEZE_CANDIDATES_PER_AXIS: i64 = 128;
    let mut collected = Vec::new();
    for db in pool.per_db() {
        let started = Instant::now();
        log_database_collection_start(1_031_001, &db.datname);
        match collect_freeze_horizons(
            db.client(),
            config.max_tables.min(MAX_FREEZE_CANDIDATES_PER_AXIS),
        )
        .await
        {
            Ok((mut rows, source_total)) => {
                log_database_collection_finish(
                    1_031_001,
                    &db.datname,
                    rows.len(),
                    source_total,
                    started.elapsed(),
                );
                collected.append(&mut rows);
            }
            Err(err) => {
                let reason = if is_sqlstate(&err, "57014") {
                    "statement_timeout"
                } else if is_sqlstate(&err, "42501") {
                    "permission_denied"
                } else {
                    "query_failed"
                };
                log_database_collection_skip(
                    1_031_001,
                    &db.datname,
                    reason,
                    &err,
                    started.elapsed(),
                );
            }
        }
    }
    collected
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
    cycle_ts_us: i64,
) -> (
    Vec<(String, UserIndexesVersion, Vec<UserIndexesRow>)>,
    CoverageAttempt,
) {
    let mut user_indexes = Vec::new();
    let mut coverage = SourceCoverage::new_attempt();
    record_pool_gaps(pool, &mut coverage);
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
            match collect_user_indexes(db.client(), major, config.max_indexes, cycle_ts_us).await {
                Ok((version, rows, source_total)) => {
                    coverage.record_success(rows.len(), source_total);
                    // Same-statement total; an empty read means an empty
                    // source, so the zero total is exact.
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
                    coverage.record_other_failure();
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
                        coverage.record_timeout();
                        "statement_timeout"
                    } else if is_sqlstate(&err, "42501") {
                        coverage.record_permission_failure();
                        "permission_denied"
                    } else {
                        coverage.record_other_failure();
                        "query_failed"
                    };
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
    (
        user_indexes,
        CoverageAttempt {
            ts: cycle_ts_us,
            section_type_id: type_id,
            coverage,
        },
    )
}

/// What the sized pool sources produced this cycle.
pub(crate) struct PoolReads {
    pub(crate) statements: Option<(StatementsVersion, Vec<StatementsRow>, u64)>,
    pub(crate) user_tables: Vec<(String, UserTablesVersion, Vec<UserTablesRow>)>,
    pub(crate) freeze_horizons: Vec<FreezeHorizonRow>,
    pub(crate) tables_attempt: CoverageAttempt,
    pub(crate) user_indexes: Vec<(String, UserIndexesVersion, Vec<UserIndexesRow>)>,
    pub(crate) indexes_attempt: CoverageAttempt,
    pub(crate) deferred: Vec<SourceKind>,
}

/// Read the due sized sources under the cycle budget, in priority order:
/// statements first, indexes last. Under pressure, the most expensive source is
/// deferred first.
#[allow(
    clippy::too_many_arguments,
    reason = "pool reads share the server layout, cycle timestamp, scheduler state, and one mutable budget"
)]
pub(crate) async fn read_pool_sources(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    cycle_ts_us: i64,
    statements_cache: &mut StatementsSourceCache,
    due: &DueSet,
    pool_budget: &mut PoolBudget,
    cycle_start: Instant,
) -> PoolReads {
    let mut deferred = Vec::new();
    let statements = if due.has(SourceKind::Statements) {
        if pool_budget.admit(PoolSource::Statements, cycle_start.elapsed(), due.forced()) {
            collect_statements_cached(pool, major, config, statements_cache).await
        } else {
            deferred.push(SourceKind::Statements);
            None
        }
    } else {
        None
    };
    let tables_type_id = user_tables_type_id(major);
    let (user_tables, freeze_horizons, tables_attempt) = if due.has(SourceKind::UserTables) {
        if pool_budget.admit(PoolSource::UserTables, cycle_start.elapsed(), due.forced()) {
            let (tables, coverage) =
                collect_user_tables_all(pool, major, config, cycle_ts_us).await;
            let freeze = collect_freeze_horizons_all(pool, config).await;
            (tables, freeze, coverage)
        } else {
            deferred.push(SourceKind::UserTables);
            (
                Vec::new(),
                Vec::new(),
                CoverageAttempt::not_attempted(cycle_ts_us, tables_type_id),
            )
        }
    } else {
        (
            Vec::new(),
            Vec::new(),
            CoverageAttempt::not_attempted(cycle_ts_us, tables_type_id),
        )
    };
    let indexes_type_id = user_indexes_type_id(major);
    let (user_indexes, indexes_attempt) = if due.has(SourceKind::UserIndexes) {
        if pool_budget.admit(PoolSource::UserIndexes, cycle_start.elapsed(), due.forced()) {
            collect_user_indexes_all(pool, major, config, cycle_ts_us).await
        } else {
            deferred.push(SourceKind::UserIndexes);
            (
                Vec::new(),
                CoverageAttempt::not_attempted(cycle_ts_us, indexes_type_id),
            )
        }
    } else {
        (
            Vec::new(),
            CoverageAttempt::not_attempted(cycle_ts_us, indexes_type_id),
        )
    };
    PoolReads {
        statements,
        user_tables,
        freeze_horizons,
        tables_attempt,
        user_indexes,
        indexes_attempt,
        deferred,
    }
}

/// Whether a tokio-postgres error carries the given SQLSTATE code.
fn is_sqlstate(err: &tokio_postgres::Error, code: &str) -> bool {
    err.code().is_some_and(|state| state.code() == code)
}
