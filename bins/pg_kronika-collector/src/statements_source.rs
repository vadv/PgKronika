use crate::config::Config;
use crate::logging::{
    CollectionFamily, LogLevel, duration_ms, field, layout_id, log_event, section_name,
};
use kronika_source_pg::pool::{ConnectionPool, DatabaseConn};
use kronika_source_pg::statements::{
    StatementsRow, StatementsVersion, collect_statements, statements_extversion, statements_version,
};
use std::time::Instant;
use tokio_postgres::Client;

/// The `1_002` layout for the extension version being collected.
pub(crate) const fn statements_type_id(version: StatementsVersion) -> u32 {
    match version {
        StatementsVersion::V1 => 1_002_001,
        StatementsVersion::V2 => 1_002_002,
        StatementsVersion::V3 => 1_002_003,
        StatementsVersion::V4 => 1_002_004,
        StatementsVersion::V5 => 1_002_005,
        StatementsVersion::V6 => 1_002_006,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatementsSource {
    Main,
    Database(String),
}

impl StatementsSource {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Main => "main".to_owned(),
            Self::Database(datname) => format!("database {datname}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachedStatementsSource {
    pub(crate) source: StatementsSource,
    pub(crate) extversion: String,
    pub(crate) version: StatementsVersion,
}

impl CachedStatementsSource {
    pub(crate) fn new(source: StatementsSource, extversion: String) -> Self {
        let version = statements_version(&extversion);
        Self {
            source,
            extversion,
            version,
        }
    }

    pub(crate) fn matches_extversion(&self, extversion: &str) -> bool {
        self.extversion == extversion && self.version == statements_version(extversion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingStatementsSource {
    covered_databases: Vec<String>,
    next_probe: usize,
}

impl MissingStatementsSource {
    pub(crate) const fn new(covered_databases: Vec<String>) -> Self {
        Self {
            covered_databases,
            next_probe: 0,
        }
    }

    pub(crate) fn matches_covered(&self, covered_databases: &[String]) -> bool {
        self.covered_databases == covered_databases
    }

    pub(crate) const fn next_per_db_probe(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let index = self.next_probe % len;
        self.next_probe = (index + 1) % len;
        Some(index)
    }
}

#[derive(Debug, Default)]
pub(crate) struct StatementsSourceCache {
    pub(crate) selected: Option<CachedStatementsSource>,
    pub(crate) missing: Option<MissingStatementsSource>,
}

impl StatementsSourceCache {
    pub(crate) fn store(
        &mut self,
        source: StatementsSource,
        extversion: String,
    ) -> StatementsVersion {
        let cached = CachedStatementsSource::new(source, extversion);
        let version = cached.version;
        self.selected = Some(cached);
        self.missing = None;
        version
    }

    pub(crate) fn invalidate(&mut self) {
        self.selected = None;
        self.missing = None;
    }

    pub(crate) fn mark_missing(&mut self, covered_databases: Vec<String>) {
        self.selected = None;
        self.missing = Some(MissingStatementsSource::new(covered_databases));
    }
}

pub(crate) struct StatementsCandidate<'a> {
    pub(crate) source: StatementsSource,
    pub(crate) client: &'a Client,
}

fn covered_statement_databases(pool: &ConnectionPool) -> Vec<String> {
    pool.per_db()
        .iter()
        .filter(|db| !db.client().is_closed())
        .map(|db| db.datname.clone())
        .collect()
}

pub(crate) fn statement_client<'a>(
    pool: &'a ConnectionPool,
    source: &StatementsSource,
) -> Option<&'a Client> {
    match source {
        StatementsSource::Main => (!pool.main().is_closed()).then_some(pool.main()),
        StatementsSource::Database(datname) => pool
            .per_db()
            .iter()
            .find(|db| db.datname == *datname && !db.client().is_closed())
            .map(DatabaseConn::client),
    }
}

pub(crate) fn all_statements_candidates(pool: &ConnectionPool) -> Vec<StatementsCandidate<'_>> {
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
) -> Option<(StatementsVersion, Vec<StatementsRow>, u64)> {
    let label = candidate.source.label();
    let started = Instant::now();
    log_event(
        LogLevel::Debug,
        "collection_start",
        &[
            CollectionFamily::Statements.field(),
            field("source", &label),
        ],
    );
    let extversion = match statements_extversion(candidate.client).await {
        Ok(Some(extversion)) => extversion,
        Ok(None) => return None,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_probe_failure",
                &[
                    CollectionFamily::Statements.field(),
                    field("source", &label),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            return None;
        }
    };
    let version = statements_version(&extversion);
    match collect_statements(candidate.client, version, config.max_statements).await {
        Ok((rows, source_total)) => {
            let version = cache.store(candidate.source, extversion);
            let type_id = statements_type_id(version);
            log_event(
                LogLevel::Debug,
                "collection_finish",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", &label),
                    field("rows", rows.len()),
                    field("source_total", source_total),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            Some((version, rows, source_total))
        }
        Err(err) => {
            let type_id = statements_type_id(version);
            log_event(
                LogLevel::Warn,
                "collection_skip",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", &label),
                    field("reason", "query_failed"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            None
        }
    }
}

async fn discover_statements_source(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>, u64)> {
    for candidate in all_statements_candidates(pool) {
        if let Some(rows) = collect_statements_from_candidate(candidate, config, cache).await {
            return Some(rows);
        }
    }
    cache.mark_missing(covered_statement_databases(pool));
    log_event(
        LogLevel::Warn,
        "collection_skip",
        &[
            CollectionFamily::Statements.field(),
            field("reason", "no_usable_source"),
        ],
    );
    None
}

async fn rediscover_missing_statements_source(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>, u64)> {
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
#[allow(
    clippy::too_many_lines,
    reason = "cached source validation and rediscovery diagnostics are one state transition"
)]
pub(crate) async fn collect_statements_cached(
    pool: &ConnectionPool,
    config: &Config,
    cache: &mut StatementsSourceCache,
) -> Option<(StatementsVersion, Vec<StatementsRow>, u64)> {
    if let Some(cached) = cache.selected.clone() {
        let label = cached.source.label();
        let started = Instant::now();
        if let Some(client) = statement_client(pool, &cached.source) {
            match statements_extversion(client).await {
                Ok(Some(extversion)) if cached.matches_extversion(&extversion) => {
                    match collect_statements(client, cached.version, config.max_statements).await {
                        Ok((rows, source_total)) => {
                            let type_id = statements_type_id(cached.version);
                            log_event(
                                LogLevel::Debug,
                                "collection_finish",
                                &[
                                    field("collection", section_name(type_id)),
                                    field("type_id", type_id),
                                    field("layout_id", layout_id(type_id)),
                                    field("source", &label),
                                    field("cached_source", true),
                                    field("rows", rows.len()),
                                    field("source_total", source_total),
                                    field("elapsed_ms", duration_ms(started.elapsed())),
                                ],
                            );
                            return Some((cached.version, rows, source_total));
                        }
                        Err(err) => {
                            let type_id = statements_type_id(cached.version);
                            log_event(
                                LogLevel::Warn,
                                "collection_probe_failure",
                                &[
                                    field("collection", section_name(type_id)),
                                    field("type_id", type_id),
                                    field("layout_id", layout_id(type_id)),
                                    field("source", &label),
                                    field("cached_source", true),
                                    field("reason", "query_failed"),
                                    field("error", &err),
                                    field("elapsed_ms", duration_ms(started.elapsed())),
                                ],
                            );
                            cache.invalidate();
                        }
                    }
                }
                Ok(Some(extversion)) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            CollectionFamily::Statements.field(),
                            field("source", &label),
                            field("cached_source", true),
                            field("reason", "extension_version_changed"),
                            field("old_extversion", &cached.extversion),
                            field("new_extversion", extversion),
                        ],
                    );
                    cache.invalidate();
                }
                Ok(None) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            CollectionFamily::Statements.field(),
                            field("source", &label),
                            field("cached_source", true),
                            field("reason", "extension_missing"),
                        ],
                    );
                    cache.invalidate();
                }
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_probe_failure",
                        &[
                            CollectionFamily::Statements.field(),
                            field("source", &label),
                            field("cached_source", true),
                            field("reason", "probe_failed"),
                            field("error", &err),
                            field("elapsed_ms", duration_ms(started.elapsed())),
                        ],
                    );
                    cache.invalidate();
                }
            }
        } else {
            log_event(
                LogLevel::Warn,
                "collection_probe_failure",
                &[
                    CollectionFamily::Statements.field(),
                    field("source", &label),
                    field("cached_source", true),
                    field("reason", "source_unavailable"),
                ],
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
