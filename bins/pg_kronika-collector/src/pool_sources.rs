use crate::budget::{PoolBudget, PoolSource};
use crate::config::Config;
use crate::coverage::SourceCoverage;
use crate::logging::{
    LogLevel, field, layout_id, log_database_collection_finish, log_database_collection_retry,
    log_database_collection_skip, log_database_collection_start, log_event, section_name,
};
use crate::scheduler::{DueSet, SourceKind};
use crate::source_contracts::{user_indexes_type_id, user_tables_type_id};
use crate::statements_source::{StatementsSourceCache, collect_statements_cached};
use kronika_source_pg::pool::{AdaptiveTimeout, ConnectionPool};
use kronika_source_pg::statements::{StatementsRow, StatementsVersion};
use kronika_source_pg::user_indexes::{UserIndexesRow, UserIndexesVersion, collect_user_indexes};
use kronika_source_pg::user_tables::{UserTablesRow, UserTablesVersion, collect_user_tables};
use std::time::Instant;

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

/// What the sized pool sources produced this cycle.
pub(crate) struct PoolReads {
    pub(crate) statements: Option<(StatementsVersion, Vec<StatementsRow>, u64)>,
    pub(crate) user_tables: Vec<(String, UserTablesVersion, Vec<UserTablesRow>)>,
    pub(crate) tables_cov: SourceCoverage,
    pub(crate) user_indexes: Vec<(String, UserIndexesVersion, Vec<UserIndexesRow>)>,
    pub(crate) indexes_cov: SourceCoverage,
    pub(crate) deferred: Vec<SourceKind>,
}

/// Read the due sized sources under the cycle budget, in survival order —
/// statements first, indexes last — so under pressure the most expensive
/// source is deferred first.
pub(crate) async fn read_pool_sources(
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

/// Whether a tokio-postgres error carries the given SQLSTATE code.
fn is_sqlstate(err: &tokio_postgres::Error, code: &str) -> bool {
    err.code().is_some_and(|state| state.code() == code)
}
