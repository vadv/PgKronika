//! `PostgreSQL` collector daemon.
//!
//! The main module owns process lifecycle and cycle orchestration. Source
//! discovery, paced reads, segment IO, logging, and config parsing live in
//! separate modules.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

mod budget;
mod buffering;
mod config;
mod coverage;
mod logging;
mod main_sources;
mod os_sources;
mod pg_log_source;
mod plans_source;
mod pool_sources;
mod scheduler;
mod segments;
mod service_sections;
mod source_contracts;
mod statements_source;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use budget::PoolBudget;
use buffering::{
    push_main_conn_sections, push_plans_read, push_service_sections, push_statements,
    push_user_indexes, push_user_tables,
};
use config::Config;
use coverage::{CoverageInputs, collect_coverage_records, push_coverage};
use kronika_source_log::LogCollector;
use kronika_source_os::{OsScope, ProcFs, detect_container};
use kronika_source_pg::pool::{ConnectionPool, DEFAULT_MAX_DATABASES};
use kronika_writer::{Interner, Journal, SectionBuffers};
use logging::{LogLevel, field, log_event, log_source_deferred};
use main_sources::{
    activity_needs_acceleration, collect_main_conn_sources, replication_needs_acceleration,
};
use os_sources::{collect_os_sources, push_os_sources};
use pg_log_source::{
    collect_log_batch, commit_log_collection, push_log_collection, run_log_only_cycle,
};
use plans_source::{PlansSourceCache, collect_store_plans_cached};
use pool_sources::{PoolReads, read_pool_sources};
use scheduler::{DueSet, Scheduler, SourceKind};
use segments::{
    SegmentState, append_window_and_maybe_seal, encode_window, open_collector_journal,
    seal_open_segment,
};
use service_sections::collect_service_sections;
use source_contracts::activity_dict_limits;
use statements_source::StatementsSourceCache;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::signal::unix::{SignalKind, signal};

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

    // The journal lives next to sealed segments so windows survive restarts.
    // Recovered windows are sealed before connecting to PostgreSQL.
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
    reason = "the cycle owns daemon state transitions across pool, journal, scheduler, and logs"
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

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one snapshot coordinates reads, buffering, coverage, and segment append"
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
    // Run every query first: SectionBuffers and Interner are `!Send` and must
    // not cross await points. Each source reads only when due.
    // The budget clock covers the whole cycle's database time. Sized pool
    // sources check it in priority order: statements first, indexes last. Under
    // pressure, the most expensive source is deferred first.
    let cycle_start = Instant::now();
    let main_src = collect_main_conn_sources(pool.main(), major, config, due).await?;
    // Trigger decisions come from the rows already collected; no extra queries.
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

    // OS rows intern device/interface/mount strings while being built, so the
    // window interner must exist before the procfs read.
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
        push_log_collection(
            &mut buffers,
            &mut interner,
            log_collector,
            collection,
            main_src.ts.0,
        )?;
    }

    if buffers.is_empty() {
        // Empty due sources append nothing; the main loop still closes an
        // expired segment before the next collection attempt.
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

fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .ok();
}
