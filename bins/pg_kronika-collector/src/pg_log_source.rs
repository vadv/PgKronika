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
use kronika_registry::pg_log::{PgLogErrorV1, PgLogGapV1};
use kronika_registry::{StrId, Ts};
use kronika_source_log::{
    DiscoveryStatus as LogDiscoveryStatus, GroupedLogError, LogCollection, LogCollector, LogGap,
    MAX_PATTERN_BYTES, MAX_TEXT_BYTES, PG_LOG_ERRORS_TYPE_ID, PG_LOG_GAP_TYPE_ID,
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
