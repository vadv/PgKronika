use crate::config::Config;
use crate::logging::{
    CollectionFamily, LogLevel, duration_ms, field, layout_id, log_event, section_name,
};
use crate::reset_source::{ExtensionResetSource, collect_plan_reset_metadata, plan_reset_context};
use crate::statements_source::{
    StatementsSource, StatementsSourceCache, all_statements_candidates, statement_client,
};
use kronika_source_pg::pool::ConnectionPool;
use kronika_source_pg::reset_metadata::{ResetBase, ResetExtensions};
use kronika_source_pg::store_plans::{
    StorePlansOsscRow, StorePlansRow, collect_store_plans, collect_store_plans_ossc,
    fetch_plan_text, store_plans_extversion, store_plans_is_ossc, store_plans_is_vadv,
};
use std::time::{Duration, Instant};
use tokio_postgres::Client;

/// Cached `pg_store_plans` source and read pacing.
///
/// Plans change slowly and reading their texts can be expensive, so reads run
/// on their own interval. Segments sealed between reads do not include the
/// section.
#[derive(Debug, Default)]
pub(crate) struct PlansSourceCache {
    pub(crate) selected: Option<CachedPlansSource>,
    pub(crate) next_read: Option<Instant>,
}

impl PlansSourceCache {
    /// Whether the paced `pg_store_plans` read is due at `now`.
    ///
    /// The main loop asks this before skipping a tick whose scheduler due-set
    /// is empty: the plans pace must not depend on another source being due.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.next_read.is_none_or(|due| now >= due)
    }

    pub(crate) fn next_due_in(&self, now: Instant) -> Option<Duration> {
        let delay = self.next_read?.saturating_duration_since(now);
        (!delay.is_zero()).then_some(delay)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CachedPlansSource {
    pub(crate) source: StatementsSource,
    pub(crate) extversion: String,
    pub(crate) fork: PlansFork,
}

/// Which `pg_store_plans` fork the cached source exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlansFork {
    /// vadv 2.x: `pg_store_plans(showtext)`, per-plan-shape identity.
    Vadv,
    /// ossc upstream: zero-argument view function, per-query identity.
    Ossc,
}

/// One paced read, typed by the source fork.
#[derive(Debug)]
pub(crate) enum PlansRead {
    Vadv(Vec<StorePlansRow>),
    Ossc(Vec<StorePlansOsscRow>),
}

/// One plan read and the reset state that bracketed it.
pub(crate) struct PlansSnapshot {
    pub(crate) read: PlansRead,
    pub(crate) source_total: u64,
    pub(crate) snapshot_ts: i64,
    pub(crate) reset: (ResetBase, ResetExtensions),
}

impl PlansRead {
    const fn is_empty(&self) -> bool {
        match self {
            Self::Vadv(rows) => rows.is_empty(),
            Self::Ossc(rows) => rows.is_empty(),
        }
    }

    pub(crate) const fn rows_len(&self) -> usize {
        match self {
            Self::Vadv(rows) => rows.len(),
            Self::Ossc(rows) => rows.len(),
        }
    }

    pub(crate) const fn type_id(&self) -> u32 {
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
pub(crate) fn plans_reread_delay(rows_empty: bool, interval: Duration) -> Duration {
    if rows_empty {
        interval.min(Duration::from_secs(30))
    } else {
        interval
    }
}

enum SnapshotAttempt {
    Collected(Box<PlansSnapshot>),
    QueryFailed(tokio_postgres::Error),
    ProvenanceUnavailable(anyhow::Error),
    ProvenanceChanged,
}

#[allow(
    clippy::too_many_arguments,
    reason = "a verified plan read coordinates the pool, server version, node identity, reset context, and fork"
)]
async fn collect_verified_plans(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    statements_cache: &StatementsSourceCache,
    client: &Client,
    source: &StatementsSource,
    extversion: &str,
    fork: PlansFork,
) -> SnapshotAttempt {
    let statements = statements_cache
        .selected
        .as_ref()
        .map(|cached| ExtensionResetSource {
            client: statement_client(pool, &cached.source),
            version: &cached.extversion,
            label: cached.source.label(),
        });
    let plans = ExtensionResetSource {
        client: Some(client),
        version: extversion,
        label: source.label(),
    };
    let before =
        match collect_plan_reset_metadata(pool.main(), major, statements.as_ref(), &plans).await {
            Ok(reset) => reset,
            Err(error) => return SnapshotAttempt::ProvenanceUnavailable(error),
        };
    let snapshot_ts = before.0.ts;
    let (read, source_total) =
        match collect_plans_for_fork(client, major, config, fork, snapshot_ts).await {
            Ok(read) => read,
            Err(error) => return SnapshotAttempt::QueryFailed(error),
        };
    let after =
        match collect_plan_reset_metadata(pool.main(), major, statements.as_ref(), &plans).await {
            Ok(reset) => reset,
            Err(error) => return SnapshotAttempt::ProvenanceUnavailable(error),
        };
    match store_plans_extversion(client).await {
        Ok(Some(observed)) if observed == extversion => {}
        Ok(_) => return SnapshotAttempt::ProvenanceChanged,
        Err(error) => {
            return SnapshotAttempt::ProvenanceUnavailable(
                anyhow::Error::new(error).context("recheck pg_store_plans extension version"),
            );
        }
    }
    if plan_reset_context(&before) != plan_reset_context(&after) {
        return SnapshotAttempt::ProvenanceChanged;
    }
    SnapshotAttempt::Collected(Box::new(PlansSnapshot {
        read,
        source_total,
        snapshot_ts,
        reset: before,
    }))
}

fn log_provenance_skip(type_id: u32, source: &str, reason: &'static str, error: Option<&str>) {
    let mut fields = vec![
        field("collection", section_name(type_id)),
        field("type_id", type_id),
        field("layout_id", layout_id(type_id)),
        field("source", source),
        field("reason", reason),
    ];
    if let Some(error) = error {
        fields.push(field("error", error));
    }
    log_event(LogLevel::Warn, "collection_skip", &fields);
}

/// One read attempt through the cached source; any failure invalidates it so
/// the caller can decide when to rediscover.
#[allow(
    clippy::too_many_lines,
    reason = "cached source validation and rediscovery diagnostics are one state transition"
)]
async fn try_cached_plans_read(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    statements_cache: &StatementsSourceCache,
    cache: &mut PlansSourceCache,
    now: Instant,
) -> Option<PlansSnapshot> {
    if let Some(cached) = cache.selected.clone() {
        let label = cached.source.label();
        let started = Instant::now();
        let type_id = cached.fork.type_id();
        if let Some(client) = statement_client(pool, &cached.source) {
            match store_plans_extversion(client).await {
                Ok(Some(extversion)) if extversion == cached.extversion => {
                    match collect_verified_plans(
                        pool,
                        major,
                        config,
                        statements_cache,
                        client,
                        &cached.source,
                        &cached.extversion,
                        cached.fork,
                    )
                    .await
                    {
                        SnapshotAttempt::Collected(snapshot) => {
                            cache.next_read = Some(
                                now + plans_reread_delay(
                                    snapshot.read.is_empty(),
                                    config.plans_interval,
                                ),
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
                                    field("rows", snapshot.read.rows_len()),
                                    field("source_total", snapshot.source_total),
                                    field("elapsed_ms", duration_ms(started.elapsed())),
                                ],
                            );
                            return Some(*snapshot);
                        }
                        SnapshotAttempt::QueryFailed(err) => {
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
                        SnapshotAttempt::ProvenanceUnavailable(err) => {
                            let error = err.to_string();
                            log_provenance_skip(
                                type_id,
                                &label,
                                "plan_provenance_unavailable",
                                Some(&error),
                            );
                            cache.next_read =
                                Some(now + plans_reread_delay(true, config.plans_interval));
                            cache.selected = None;
                        }
                        SnapshotAttempt::ProvenanceChanged => {
                            log_provenance_skip(
                                type_id,
                                &label,
                                "plan_provenance_changed_during_read",
                                None,
                            );
                            cache.next_read =
                                Some(now + plans_reread_delay(true, config.plans_interval));
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
/// supported source exists. All awaits finish here so the caller can intern
/// without holding the `!Send` `Interner` across an await.
#[allow(
    clippy::too_many_lines,
    reason = "source discovery, fork detection, and diagnostic skip reasons use one control flow"
)]
pub(crate) async fn collect_store_plans_cached(
    pool: &ConnectionPool,
    major: u32,
    config: &Config,
    statements_cache: &StatementsSourceCache,
    cache: &mut PlansSourceCache,
    force: bool,
) -> Option<PlansSnapshot> {
    let now = Instant::now();
    if !force && !cache.is_due(now) {
        return None;
    }

    let had_cached_source = cache.selected.is_some();
    if let Some(read) =
        try_cached_plans_read(pool, major, config, statements_cache, cache, now).await
    {
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
        match collect_verified_plans(
            pool,
            major,
            config,
            statements_cache,
            candidate.client,
            &candidate.source,
            &extversion,
            fork,
        )
        .await
        {
            SnapshotAttempt::Collected(snapshot) => {
                cache.selected = Some(CachedPlansSource {
                    source: candidate.source.clone(),
                    extversion,
                    fork,
                });
                cache.next_read =
                    Some(now + plans_reread_delay(snapshot.read.is_empty(), config.plans_interval));
                log_event(
                    LogLevel::Debug,
                    "collection_finish",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", &label),
                        field("fork", fork.as_str()),
                        field("rows", snapshot.read.rows_len()),
                        field("source_total", snapshot.source_total),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Some(*snapshot);
            }
            SnapshotAttempt::QueryFailed(err) => {
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
            SnapshotAttempt::ProvenanceUnavailable(err) => {
                cache.selected = Some(CachedPlansSource {
                    source: candidate.source.clone(),
                    extversion,
                    fork,
                });
                cache.next_read = Some(now + plans_reread_delay(true, config.plans_interval));
                let error = err.to_string();
                log_provenance_skip(type_id, &label, "plan_provenance_unavailable", Some(&error));
                return None;
            }
            SnapshotAttempt::ProvenanceChanged => {
                cache.selected = Some(CachedPlansSource {
                    source: candidate.source.clone(),
                    extversion,
                    fork,
                });
                cache.next_read = Some(now + plans_reread_delay(true, config.plans_interval));
                log_provenance_skip(type_id, &label, "plan_provenance_changed_during_read", None);
                return None;
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
    major: u32,
    config: &Config,
    fork: PlansFork,
    snapshot_ts: i64,
) -> Result<(PlansRead, u64), tokio_postgres::Error> {
    match fork {
        PlansFork::Vadv => {
            let (rows, source_total) =
                collect_plans_with_texts(client, major, config, snapshot_ts).await?;
            Ok((PlansRead::Vadv(rows), source_total))
        }
        PlansFork::Ossc => {
            let (rows, source_total) =
                collect_ossc_plans_with_budget(client, major, config, snapshot_ts).await?;
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
    major: u32,
    config: &Config,
    snapshot_ts: i64,
) -> Result<(Vec<StorePlansOsscRow>, u64), tokio_postgres::Error> {
    let started = Instant::now();
    let text_cap = (config.plan_text_budget > 0).then_some(config.max_plan_text);
    let (mut rows, masked, source_total) =
        collect_store_plans_ossc(client, major, config.max_plans, text_cap, snapshot_ts).await?;
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
    major: u32,
    config: &Config,
    snapshot_ts: i64,
) -> Result<(Vec<StorePlansRow>, u64), tokio_postgres::Error> {
    let started = Instant::now();
    let (mut rows, source_total) =
        collect_store_plans(client, major, config.max_plans, snapshot_ts).await?;
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
        // Hard timeout: one slow fetch must not stretch the snapshot past the
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
pub(crate) fn truncate_to_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
}
