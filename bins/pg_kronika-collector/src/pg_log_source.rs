use crate::activity_dict_limits;
use crate::buffer_row;
use crate::config::Config;
use crate::logging::{
    LogLevel, duration_ms, field, log_collection_finish, log_collection_start, log_count_degraded,
    log_event,
};
use crate::scheduler::DueSet;
use crate::segments::{SegmentState, append_window_and_maybe_seal, encode_window};
use anyhow::{Context, Result};
use kronika_registry::pg_log::{
    PgLogAutovacuumV1, PgLogCheckpointV1, PgLogErrorV1, PgLogGapV1, PgLogLifecycleV1,
    PgLogLockWaitV1, PgLogSlowQueryV1, PgLogTempFileV1,
};
use kronika_registry::{StrId, Ts};
use kronika_source_log::{
    AutovacuumEvent, CheckpointEvent, DiscoveryStatus as LogDiscoveryStatus, GroupedLogError,
    LifecycleEvent, LockWaitEvent, LogCollection, LogCollector, LogGap, MAX_PATTERN_BYTES,
    MAX_TEXT_BYTES, PG_LOG_AUTOVACUUM_TYPE_ID, PG_LOG_CHECKPOINTS_TYPE_ID, PG_LOG_ERRORS_TYPE_ID,
    PG_LOG_GAP_TYPE_ID, PG_LOG_LIFECYCLE_TYPE_ID, PG_LOG_LOCK_WAITS_TYPE_ID,
    PG_LOG_SLOW_QUERIES_TYPE_ID, PG_LOG_TEMP_FILES_TYPE_ID, SlowQueryEvent, TempFileEvent,
};
use kronika_writer::{Interner, Journal, SectionBuffers};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio_postgres::Client;

pub(crate) async fn run_log_only_cycle(
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
    push_log_collection(
        &mut buffers,
        &mut interner,
        log_collector,
        &mut collection,
        ts,
    )?;
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

pub(crate) async fn collect_log_batch(
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
    if !collection.checkpoints.is_empty() {
        log_collection_finish(
            PG_LOG_CHECKPOINTS_TYPE_ID,
            "log",
            collection.checkpoints.len(),
            started.elapsed(),
        );
    }
    if !collection.autovacuums.is_empty() {
        log_collection_finish(
            PG_LOG_AUTOVACUUM_TYPE_ID,
            "log",
            collection.autovacuums.len(),
            started.elapsed(),
        );
    }
    if !collection.slow_queries.is_empty() {
        log_collection_finish(
            PG_LOG_SLOW_QUERIES_TYPE_ID,
            "log",
            collection.slow_queries.len(),
            started.elapsed(),
        );
    }
    if !collection.lock_waits.is_empty() {
        log_collection_finish(
            PG_LOG_LOCK_WAITS_TYPE_ID,
            "log",
            collection.lock_waits.len(),
            started.elapsed(),
        );
    }
    if !collection.lifecycles.is_empty() {
        log_collection_finish(
            PG_LOG_LIFECYCLE_TYPE_ID,
            "log",
            collection.lifecycles.len(),
            started.elapsed(),
        );
    }
    if !collection.temp_files.is_empty() {
        log_collection_finish(
            PG_LOG_TEMP_FILES_TYPE_ID,
            "log",
            collection.temp_files.len(),
            started.elapsed(),
        );
    }
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
                field("checkpoint_rows", collection.checkpoints.len()),
                field("autovacuum_rows", collection.autovacuums.len()),
                field("slow_query_rows", collection.slow_queries.len()),
                field("lock_wait_rows", collection.lock_waits.len()),
                field("lifecycle_rows", collection.lifecycles.len()),
                field("temp_file_rows", collection.temp_files.len()),
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

pub(crate) fn commit_log_collection(
    log_collector: &mut LogCollector,
    collection: Option<&LogCollection>,
) {
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

pub(crate) fn push_log_collection(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    log_collector: &mut LogCollector,
    collection: &mut LogCollection,
    ts: i64,
) -> Result<()> {
    let dropped = push_log_sections(buffers, interner, collection)?;
    if dropped == 0 {
        return Ok(());
    }
    let first_new_gap = collection.gaps.len();
    log_collector.record_dictionary_drops(collection, ts, dropped);
    push_log_gaps(buffers, interner, &collection.gaps[first_new_gap..])?;
    log_count_degraded(
        PG_LOG_GAP_TYPE_ID,
        "log",
        "dictionary_full",
        usize::try_from(dropped).unwrap_or(usize::MAX),
    );
    Ok(())
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
    for checkpoint in &collection.checkpoints {
        dropped = dropped.saturating_add(push_log_checkpoint(buffers, interner, checkpoint)?);
    }
    for autovacuum in &collection.autovacuums {
        dropped = dropped.saturating_add(push_log_autovacuum(buffers, interner, autovacuum)?);
    }
    for slow_query in &collection.slow_queries {
        dropped = dropped.saturating_add(push_log_slow_query(buffers, interner, slow_query)?);
    }
    for lock_wait in &collection.lock_waits {
        dropped = dropped.saturating_add(push_log_lock_wait(buffers, interner, lock_wait)?);
    }
    for lifecycle in &collection.lifecycles {
        dropped = dropped.saturating_add(push_log_lifecycle(buffers, interner, lifecycle)?);
    }
    for temp_file in &collection.temp_files {
        dropped = dropped.saturating_add(push_log_temp_file(buffers, interner, temp_file)?);
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

fn push_log_checkpoint(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    checkpoint: &CheckpointEvent,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let reason = checkpoint
        .reason
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    buffer_row(
        buffers,
        PgLogCheckpointV1 {
            ts: Ts(checkpoint.ts),
            phase: checkpoint.phase.code(),
            reason,
            seconds_apart: checkpoint.seconds_apart,
            buffers_written: checkpoint.buffers_written,
            write_ms: checkpoint.write_ms,
            sync_ms: checkpoint.sync_ms,
            total_ms: checkpoint.total_ms,
            distance_kb: checkpoint.distance_kb,
            estimate_kb: checkpoint.estimate_kb,
            wal_added: checkpoint.wal_added,
            wal_removed: checkpoint.wal_removed,
            wal_recycled: checkpoint.wal_recycled,
            sync_files: checkpoint.sync_files,
            longest_sync_ms: checkpoint.longest_sync_ms,
            average_sync_ms: checkpoint.average_sync_ms,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}

fn push_log_autovacuum(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    autovacuum: &AutovacuumEvent,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let relation = autovacuum
        .relation
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    buffer_row(
        buffers,
        PgLogAutovacuumV1 {
            ts: Ts(autovacuum.ts),
            kind: autovacuum.kind.code(),
            relation,
            index_scans: autovacuum.index_scans,
            pages_removed: autovacuum.pages_removed,
            pages_remaining: autovacuum.pages_remaining,
            tuples_removed: autovacuum.tuples_removed,
            tuples_remaining: autovacuum.tuples_remaining,
            tuples_dead_not_removable: autovacuum.tuples_dead_not_removable,
            elapsed_ms: autovacuum.elapsed_ms,
            buffer_hits: autovacuum.buffer_hits,
            buffer_misses: autovacuum.buffer_misses,
            buffer_dirtied: autovacuum.buffer_dirtied,
            avg_read_rate_mbs: autovacuum.avg_read_rate_mbs,
            avg_write_rate_mbs: autovacuum.avg_write_rate_mbs,
            cpu_user_ms: autovacuum.cpu_user_ms,
            cpu_system_ms: autovacuum.cpu_system_ms,
            wal_records: autovacuum.wal_records,
            wal_fpi: autovacuum.wal_fpi,
            wal_bytes: autovacuum.wal_bytes,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}

fn push_log_slow_query(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    slow_query: &SlowQueryEvent,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let pattern = intern_log_text(
        interner,
        &slow_query.pattern,
        MAX_PATTERN_BYTES,
        &mut dropped,
    );
    let sample = intern_log_text(interner, &slow_query.sample, MAX_TEXT_BYTES, &mut dropped);
    buffer_row(
        buffers,
        PgLogSlowQueryV1 {
            ts: Ts(slow_query.ts),
            pattern,
            sample,
            count: slow_query.count,
            max_duration_ms: slow_query.max_duration_ms,
            total_duration_ms: slow_query.total_duration_ms,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}

fn push_log_lock_wait(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    lock_wait: &LockWaitEvent,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let lock_mode = lock_wait
        .lock_mode
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let lock_target = lock_wait
        .lock_target
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let detail = lock_wait
        .detail
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let context = lock_wait
        .context
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let statement = lock_wait
        .statement
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    buffer_row(
        buffers,
        PgLogLockWaitV1 {
            ts: Ts(lock_wait.ts),
            kind: lock_wait.kind.code(),
            pid: lock_wait.pid,
            lock_mode,
            lock_target,
            duration_ms: lock_wait.duration_ms,
            detail,
            context,
            statement,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}

fn push_log_lifecycle(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    lifecycle: &LifecycleEvent,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let shutdown_mode = lifecycle
        .shutdown_mode
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let message = intern_log_text(interner, &lifecycle.message, MAX_TEXT_BYTES, &mut dropped);
    let query_detail = lifecycle
        .query_detail
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    buffer_row(
        buffers,
        PgLogLifecycleV1 {
            ts: Ts(lifecycle.ts),
            kind: lifecycle.kind.code(),
            pid: lifecycle.pid,
            signal: lifecycle.signal,
            shutdown_mode,
            message,
            query_detail,
            dict_dropped_fields: u8::try_from(dropped).unwrap_or(u8::MAX),
        },
    )?;
    Ok(dropped)
}

fn push_log_temp_file(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    temp_file: &TempFileEvent,
) -> Result<u32> {
    let mut dropped = 0_u32;
    let path = temp_file
        .path
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    let statement = temp_file
        .statement
        .as_deref()
        .and_then(|value| intern_log_text(interner, value, MAX_TEXT_BYTES, &mut dropped));
    buffer_row(
        buffers,
        PgLogTempFileV1 {
            ts: Ts(temp_file.ts),
            path,
            size_bytes: temp_file.size_bytes,
            statement,
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
