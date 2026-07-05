//! Source discovery, parsing, aggregation, and commit boundary.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio_postgres::Client;

use crate::normalize::{ErrorCategory, classify_error, normalize_error};
use crate::parser::{ContinuationKind, LogSeverity, ParsedLine, ParserKind, parse_stderr_line};
use crate::state::TailState;
use crate::tailer::{TailCaps, TailGaps, TailLine, read_batch};
use crate::{MAX_PATTERN_BYTES, MAX_TEXT_BYTES, truncate_utf8, u32_saturating};

/// Runtime configuration for the log collector.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Whether log collection is active.
    pub enabled: bool,
    /// Direct file path override.
    pub path_override: Option<PathBuf>,
    /// Root for relative `pg_current_logfile()` paths.
    pub root_override: Option<PathBuf>,
    /// Parser forced by configuration.
    pub parser_kind: ParserKind,
    /// File holding committed tail state.
    pub state_path: PathBuf,
    /// Start at byte zero when no state exists.
    pub start_at_beginning: bool,
    /// Minimum interval between PG discovery queries.
    pub discovery_interval: Duration,
    /// Tailer caps.
    pub tail_caps: TailCaps,
}

impl LogConfig {
    /// Build disabled log collection with a state file under `out_dir`.
    #[must_use]
    pub fn disabled(out_dir: &Path) -> Self {
        Self {
            enabled: false,
            path_override: None,
            root_override: None,
            parser_kind: ParserKind::Stderr,
            state_path: out_dir.join("pg_log_tail.state"),
            start_at_beginning: false,
            discovery_interval: Duration::from_mins(1),
            tail_caps: TailCaps::default(),
        }
    }
}

/// Discovery outcome for structured collector logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryStatus {
    /// Source is available.
    Available,
    /// Log destination is known but not implemented in this PR.
    UnsupportedFormat,
    /// `PostgreSQL` could not report a usable log path.
    SourceUnavailable,
    /// Discovery query failed; the previous source, if any, remains usable.
    QueryFailed,
    /// Log collection is disabled.
    Disabled,
}

/// One grouped log error before dictionary interning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedLogError {
    /// First log timestamp in the group.
    pub ts: i64,
    /// Severity.
    pub severity: LogSeverity,
    /// Error category.
    pub category: ErrorCategory,
    /// SQLSTATE when present.
    pub sqlstate: Option<String>,
    /// Normalized pattern.
    pub pattern: String,
    /// Occurrence count.
    pub count: u32,
    /// First concrete sample.
    pub sample: String,
    /// Attached `DETAIL:` payload.
    pub detail: Option<String>,
    /// Attached `HINT:` payload.
    pub hint: Option<String>,
    /// Attached `CONTEXT:` payload.
    pub context: Option<String>,
    /// Attached SQL statement.
    pub statement: Option<String>,
}

/// Phase of a checkpoint log event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointPhase {
    /// `checkpoint starting: ...`.
    Starting,
    /// `checkpoint complete: ...`.
    Complete,
    /// `checkpoints are occurring too frequently ...`.
    TooFrequent,
}

impl CheckpointPhase {
    /// Numeric code stored in `pg_log_checkpoints`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Complete => 1,
            Self::TooFrequent => 2,
        }
    }
}

/// One typed checkpoint event before dictionary interning.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointEvent {
    /// Log record time.
    pub ts: i64,
    /// Checkpoint phase.
    pub phase: CheckpointPhase,
    /// Starting reason or warning text.
    pub reason: Option<String>,
    /// Interval reported by the too-frequent checkpoint warning.
    pub seconds_apart: Option<i64>,
    /// Buffers written by the checkpoint.
    pub buffers_written: Option<i64>,
    /// Checkpoint write time, ms.
    pub write_ms: Option<f64>,
    /// Checkpoint sync time, ms.
    pub sync_ms: Option<f64>,
    /// Total checkpoint time, ms.
    pub total_ms: Option<f64>,
    /// WAL distance covered by the checkpoint, kB.
    pub distance_kb: Option<i64>,
    /// Estimated WAL distance for checkpoint scheduling, kB.
    pub estimate_kb: Option<i64>,
    /// WAL files added.
    pub wal_added: Option<i64>,
    /// WAL files removed.
    pub wal_removed: Option<i64>,
    /// WAL files recycled.
    pub wal_recycled: Option<i64>,
    /// Files synced by the checkpoint.
    pub sync_files: Option<i64>,
    /// Longest individual file sync duration, ms.
    pub longest_sync_ms: Option<f64>,
    /// Average file sync duration, ms.
    pub average_sync_ms: Option<f64>,
}

/// Slow-query aggregate for one normalized SQL pattern in a collection window.
#[derive(Debug, Clone, PartialEq)]
pub struct SlowQueryEvent {
    /// Timestamp of the max-duration sample.
    pub ts: i64,
    /// Normalized SQL pattern.
    pub pattern: String,
    /// Bounded SQL sample for the max-duration occurrence.
    pub sample: String,
    /// Occurrences of this pattern in the collection window.
    pub count: u32,
    /// Largest duration for the pattern, ms.
    pub max_duration_ms: f64,
    /// Sum of durations for this pattern, ms.
    pub total_duration_ms: f64,
}

/// Server lifecycle event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleKind {
    /// Backend/postmaster child crash.
    Crash,
    /// Postmaster shutdown request.
    Shutdown,
    /// Server is ready to accept connections.
    Ready,
}

impl LifecycleKind {
    /// Numeric code stored in `pg_log_lifecycle`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Crash => 0,
            Self::Shutdown => 1,
            Self::Ready => 2,
        }
    }
}

/// One typed server lifecycle event before dictionary interning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleEvent {
    /// Log record time.
    pub ts: i64,
    /// Lifecycle kind.
    pub kind: LifecycleKind,
    /// Crashed process id when `PostgreSQL` reports it.
    pub pid: Option<i32>,
    /// Crash signal number when `PostgreSQL` reports it.
    pub signal: Option<i32>,
    /// Shutdown mode: `fast`, `smart`, or `immediate`.
    pub shutdown_mode: Option<String>,
    /// Bounded lifecycle message.
    pub message: String,
    /// SQL extracted from a following crash `DETAIL:` line.
    pub query_detail: Option<String>,
}

/// Why `pg_log_gap` was emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapReason {
    /// Backlog exceeded the configured threshold.
    Backlog,
    /// A line exceeded the physical-line cap.
    Truncate,
    /// A line was not valid `UTF-8`.
    InvalidUtf8,
    /// A line contained NUL bytes.
    Binary,
    /// Sparse file hole skipped.
    Sparse,
    /// Rotation/copytruncate or source path switch.
    Rotation,
    /// Source file was missing.
    MissingFile,
    /// Parser kind is not implemented in this scope.
    UnsupportedFormat,
    /// No log source could be discovered.
    SourceUnavailable,
    /// Dictionary interning failed for text fields.
    DictionaryFull,
    /// Parser-level output cap dropped records.
    ParserDrop,
    /// Read budget stopped this cycle; offset is not committed past unread bytes.
    BudgetExhausted,
    /// Log collection is disabled by configuration.
    Disabled,
    /// Discovery query failed; last known source is used when available.
    QueryFailed,
    /// The source path exists but cannot be opened with collector permissions.
    PermissionDenied,
    /// stderr prefix did not expose a parseable timestamp; collection time was used.
    TimestampFallback,
}

impl GapReason {
    /// Numeric code stored in `pg_log_gap`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Backlog => 0,
            Self::Truncate => 1,
            Self::InvalidUtf8 => 2,
            Self::Binary => 3,
            Self::Sparse => 4,
            Self::Rotation => 5,
            Self::MissingFile => 6,
            Self::UnsupportedFormat => 7,
            Self::SourceUnavailable => 8,
            Self::DictionaryFull => 9,
            Self::ParserDrop => 10,
            Self::BudgetExhausted => 11,
            Self::Disabled => 12,
            Self::QueryFailed => 13,
            Self::PermissionDenied => 14,
            Self::TimestampFallback => 15,
        }
    }
}

/// One typed log-gap row before dictionary interning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogGap {
    /// Detection time.
    pub ts: i64,
    /// Source path when known.
    pub source_path: Option<PathBuf>,
    /// Parser kind.
    pub parser_kind: ParserKind,
    /// Gap reason.
    pub reason: GapReason,
    /// Device id when known.
    pub dev: Option<u64>,
    /// Inode when known.
    pub inode: Option<u64>,
    /// Offset after read when known.
    pub offset: Option<u64>,
    /// Bytes skipped.
    pub bytes_skipped: u64,
    /// Truncated physical lines.
    pub truncated_lines: u32,
    /// Invalid `UTF-8` lines.
    pub invalid_utf8: u32,
    /// Binary lines.
    pub binary_dropped: u32,
    /// Rotation detections.
    pub rotations: u32,
    /// Missing-file observations.
    pub missing_files: u32,
    /// Budget exhaustions.
    pub budget_exhaustions: u32,
    /// Dictionary fields dropped.
    pub dict_dropped_fields: u32,
    /// Parser-level dropped lines or groups.
    pub parser_dropped_lines: u32,
}

/// One collection result.
#[derive(Debug, Default, Clone)]
pub struct LogCollection {
    /// Grouped errors.
    pub errors: Vec<GroupedLogError>,
    /// Typed checkpoint events.
    pub checkpoints: Vec<CheckpointEvent>,
    /// Slow-query top-N rows.
    pub slow_queries: Vec<SlowQueryEvent>,
    /// Server lifecycle events.
    pub lifecycles: Vec<LifecycleEvent>,
    /// Degradation rows.
    pub gaps: Vec<LogGap>,
    /// Discovery status for logs.
    pub discovery_status: Option<DiscoveryStatus>,
    next_state: Option<TailState>,
}

/// Stateful log collector.
#[derive(Debug)]
pub struct LogCollector {
    config: LogConfig,
    state: Option<TailState>,
    source: Option<LogSource>,
    next_discovery: Option<Instant>,
    disabled_reported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogSource {
    path: PathBuf,
    parser_kind: ParserKind,
}

#[derive(Debug, Clone)]
struct PendingError {
    ts: i64,
    count: u32,
    sample: String,
    detail: Option<String>,
    hint: Option<String>,
    context: Option<String>,
    statement: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingSlowQuery {
    ts: i64,
    duration_ms: f64,
    sql: String,
}

#[derive(Debug, Clone)]
struct PendingSlowQueryGroup {
    ts: i64,
    pattern: String,
    sample: String,
    count: u32,
    max_duration_ms: f64,
    total_duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ErrorKey {
    pattern: String,
    severity: LogSeverity,
    sqlstate: Option<String>,
}

#[derive(Debug, Default)]
struct ParsedLogRecords {
    errors: Vec<GroupedLogError>,
    checkpoints: Vec<CheckpointEvent>,
    slow_queries: Vec<SlowQueryEvent>,
    lifecycles: Vec<LifecycleEvent>,
}

const MAX_ERROR_GROUPS: usize = 32;
const MAX_CHECKPOINT_EVENTS: usize = 64;
const MAX_SLOW_QUERY_GROUPS: usize = 16;
const MAX_LIFECYCLE_EVENTS: usize = 32;

impl LogCollector {
    /// Create a collector and load persisted tail state.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from reading the state file.
    pub fn new(config: LogConfig) -> io::Result<Self> {
        let state = TailState::load(&config.state_path)?;
        let source = state.as_ref().map(|state| LogSource {
            path: state.path.clone(),
            parser_kind: state.parser_kind,
        });
        Ok(Self {
            config,
            state,
            source,
            next_discovery: None,
            disabled_reported: false,
        })
    }

    /// Whether this collector is active.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Collect one bounded log batch.
    #[allow(
        clippy::too_many_lines,
        reason = "collection keeps discovery, tail gaps, and commit state in one auditable boundary"
    )]
    pub async fn collect(&mut self, client: Option<&Client>, ts: i64) -> LogCollection {
        if !self.config.enabled {
            let mut collection = LogCollection {
                discovery_status: Some(DiscoveryStatus::Disabled),
                ..LogCollection::default()
            };
            if !self.disabled_reported {
                collection
                    .gaps
                    .push(self.simple_gap(ts, GapReason::Disabled));
                self.disabled_reported = true;
            }
            return collection;
        }

        let mut result = LogCollection::default();
        let discovery = self.refresh_source(client).await;
        result.discovery_status = Some(discovery);
        if matches!(
            discovery,
            DiscoveryStatus::UnsupportedFormat
                | DiscoveryStatus::SourceUnavailable
                | DiscoveryStatus::QueryFailed
        ) {
            let reason = match discovery {
                DiscoveryStatus::UnsupportedFormat => GapReason::UnsupportedFormat,
                DiscoveryStatus::QueryFailed => GapReason::QueryFailed,
                _ => GapReason::SourceUnavailable,
            };
            result.gaps.push(self.simple_gap(ts, reason));
            if discovery != DiscoveryStatus::QueryFailed {
                return result;
            }
        }

        let Some(source) = self.source.clone() else {
            result
                .gaps
                .push(self.simple_gap(ts, GapReason::SourceUnavailable));
            return result;
        };
        if source.parser_kind != ParserKind::Stderr {
            result.gaps.push(LogGap {
                parser_kind: source.parser_kind,
                reason: GapReason::UnsupportedFormat,
                source_path: Some(source.path),
                ts,
                ..empty_gap()
            });
            return result;
        }

        let batch = match read_batch(
            &source.path,
            source.parser_kind,
            self.state.as_ref(),
            self.config.start_at_beginning,
            self.config.tail_caps,
        ) {
            Ok(batch) => batch,
            Err(err) => {
                let reason = read_error_reason(err.kind());
                result.gaps.push(LogGap {
                    parser_kind: source.parser_kind,
                    reason,
                    source_path: Some(source.path),
                    ts,
                    ..empty_gap()
                });
                return result;
            }
        };
        let mut parse_gaps = ParseGaps::default();
        let parsed = parse_stderr_records(&batch.lines, ts, &mut parse_gaps);
        result.errors = parsed.errors;
        result.checkpoints = parsed.checkpoints;
        result.slow_queries = parsed.slow_queries;
        result.lifecycles = parsed.lifecycles;
        result.gaps.extend(gaps_from_tail(
            ts,
            &source.path,
            source.parser_kind,
            batch.gaps,
            batch.next_state.as_ref(),
        ));
        if parse_gaps.invalid_utf8 != 0 {
            result.gaps.push(LogGap {
                ts,
                source_path: Some(source.path.clone()),
                parser_kind: source.parser_kind,
                reason: GapReason::InvalidUtf8,
                invalid_utf8: parse_gaps.invalid_utf8,
                parser_dropped_lines: parse_gaps.invalid_utf8,
                ..file_state_fields(batch.next_state.as_ref())
            });
        }
        let parser_drops = parse_gaps
            .dropped_groups
            .saturating_add(parse_gaps.dropped_events);
        if parser_drops != 0 {
            result.gaps.push(LogGap {
                ts,
                source_path: Some(source.path.clone()),
                parser_kind: source.parser_kind,
                reason: GapReason::ParserDrop,
                parser_dropped_lines: parser_drops,
                ..file_state_fields(batch.next_state.as_ref())
            });
        }
        if parse_gaps.timestamp_fallbacks != 0 {
            result.gaps.push(LogGap {
                ts,
                source_path: Some(source.path),
                parser_kind: source.parser_kind,
                reason: GapReason::TimestampFallback,
                ..file_state_fields(batch.next_state.as_ref())
            });
        }
        result.next_state = batch.next_state;
        result
    }

    /// Persist the batch state after the caller has safely handled its output.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors from saving the state file.
    pub fn commit(&mut self, collection: &LogCollection) -> io::Result<()> {
        let Some(state) = &collection.next_state else {
            return Ok(());
        };
        state.save(&self.config.state_path)?;
        self.state = Some(state.clone());
        self.source = Some(LogSource {
            path: state.path.clone(),
            parser_kind: state.parser_kind,
        });
        Ok(())
    }

    /// Add a dictionary-full gap to a collection before commit.
    pub fn record_dictionary_drops(&self, collection: &mut LogCollection, ts: i64, dropped: u32) {
        if dropped == 0 {
            return;
        }
        let source = self.source.as_ref();
        collection.gaps.push(LogGap {
            ts,
            source_path: source.map(|source| source.path.clone()),
            parser_kind: source.map_or(ParserKind::Unknown, |source| source.parser_kind),
            reason: GapReason::DictionaryFull,
            dict_dropped_fields: dropped,
            ..file_state_fields(collection.next_state.as_ref().or(self.state.as_ref()))
        });
    }

    async fn refresh_source(&mut self, client: Option<&Client>) -> DiscoveryStatus {
        if let Some(path) = &self.config.path_override {
            self.source = Some(LogSource {
                path: path.clone(),
                parser_kind: self.config.parser_kind,
            });
            return DiscoveryStatus::Available;
        }
        let now = Instant::now();
        if self.next_discovery.is_some_and(|deadline| now < deadline) && self.source.is_some() {
            return DiscoveryStatus::Available;
        }
        self.next_discovery = Some(now + self.config.discovery_interval);
        let Some(client) = client else {
            return if self.source.is_some() {
                DiscoveryStatus::QueryFailed
            } else {
                DiscoveryStatus::SourceUnavailable
            };
        };
        match discover(client, self.config.root_override.as_deref()).await {
            Ok(Some(source)) => {
                self.source = Some(source);
                DiscoveryStatus::Available
            }
            Ok(None) => DiscoveryStatus::UnsupportedFormat,
            Err(DiscoveryError::Unavailable | DiscoveryError::Query) => {
                if self.source.is_some() {
                    DiscoveryStatus::QueryFailed
                } else {
                    DiscoveryStatus::SourceUnavailable
                }
            }
        }
    }

    fn simple_gap(&self, ts: i64, reason: GapReason) -> LogGap {
        let source = self.source.as_ref();
        LogGap {
            ts,
            source_path: source.map(|source| source.path.clone()),
            parser_kind: source.map_or(self.config.parser_kind, |source| source.parser_kind),
            reason,
            ..file_state_fields(self.state.as_ref())
        }
    }
}

fn read_error_reason(kind: io::ErrorKind) -> GapReason {
    if kind == io::ErrorKind::PermissionDenied {
        GapReason::PermissionDenied
    } else {
        GapReason::SourceUnavailable
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoveryError {
    Query,
    Unavailable,
}

async fn discover(
    client: &Client,
    root: Option<&Path>,
) -> Result<Option<LogSource>, DiscoveryError> {
    let destination = show(client, "log_destination").await?;
    if !destination.split(',').any(|value| value.trim() == "stderr") {
        return Ok(None);
    }
    let Some(path) = current_logfile(client).await? else {
        return Err(DiscoveryError::Unavailable);
    };
    let full = if PathBuf::from(&path).is_absolute() {
        PathBuf::from(path)
    } else if let Some(root) = root {
        root.join(path)
    } else {
        let data_directory = show(client, "data_directory").await?;
        PathBuf::from(data_directory).join(path)
    };
    Ok(Some(LogSource {
        path: full,
        parser_kind: ParserKind::Stderr,
    }))
}

async fn show(client: &Client, name: &str) -> Result<String, DiscoveryError> {
    let query = format!("/* pg_kronika:log */ SHOW {name}");
    client
        .query_one(query.as_str(), &[])
        .await
        .map_err(|_err| DiscoveryError::Query)
        .map(|row| row.get(0))
}

async fn current_logfile(client: &Client) -> Result<Option<String>, DiscoveryError> {
    client
        .query_one(
            "/* pg_kronika:log */ SELECT pg_current_logfile('stderr')",
            &[],
        )
        .await
        .map_err(|_err| DiscoveryError::Query)
        .map(|row| {
            row.get::<_, Option<String>>(0)
                .filter(|value| !value.is_empty())
        })
}

#[derive(Debug, Default)]
struct ParseGaps {
    invalid_utf8: u32,
    dropped_groups: u32,
    dropped_events: u32,
    timestamp_fallbacks: u32,
}

#[allow(
    clippy::too_many_lines,
    reason = "stderr event routing, continuation state, and cap accounting stay auditable together"
)]
fn parse_stderr_records(
    lines: &[TailLine],
    fallback_ts: i64,
    gaps: &mut ParseGaps,
) -> ParsedLogRecords {
    let mut records = ParsedLogRecords::default();
    let mut pending = HashMap::<ErrorKey, PendingError>::new();
    let mut slow_groups = HashMap::<String, PendingSlowQueryGroup>::new();
    let mut pending_slow = None::<PendingSlowQuery>;
    let mut last_key = None::<ErrorKey>;
    let mut last_continuation = None::<ContinuationKind>;
    let mut last_lifecycle = None::<usize>;
    let mut lifecycle_detail_active = false;
    for line in lines {
        let Ok(decoded) = std::str::from_utf8(&line.bytes) else {
            flush_pending_slow_query(&mut pending_slow, &mut slow_groups);
            gaps.invalid_utf8 = gaps.invalid_utf8.saturating_add(1);
            last_key = None;
            last_continuation = None;
            last_lifecycle = None;
            lifecycle_detail_active = false;
            continue;
        };
        let decoded = decoded.strip_suffix('\r').unwrap_or(decoded);
        if decoded.starts_with([' ', '\t']) {
            let text = decoded.trim();
            if let Some(slow) = pending_slow.as_mut() {
                append_string_capped(&mut slow.sql, text, MAX_TEXT_BYTES);
                continue;
            }
            if let Some(kind) = last_continuation {
                append_to_last_continuation(&mut pending, last_key.as_ref(), kind, text);
            }
            if lifecycle_detail_active {
                append_lifecycle_detail_continuation(&mut records.lifecycles, last_lifecycle, text);
            }
            continue;
        }
        flush_pending_slow_query(&mut pending_slow, &mut slow_groups);
        match parse_stderr_line(decoded) {
            Some(ParsedLine::Error {
                ts,
                severity,
                sqlstate,
                message,
            }) => {
                let used_ts = ts.unwrap_or(fallback_ts);
                let mut routed = false;
                let mut lifecycle_for_continuation = None;
                if severity == LogSeverity::Log {
                    if let Some(event) = parse_checkpoint_event(message, used_ts) {
                        routed = true;
                        push_checkpoint_event(&mut records.checkpoints, event, gaps);
                    } else if let Some(slow) = parse_slow_query_event(message, used_ts) {
                        routed = true;
                        pending_slow = Some(slow);
                    } else if let Some(event) = parse_lifecycle_event(message, used_ts) {
                        routed = true;
                        lifecycle_for_continuation =
                            push_lifecycle_event(&mut records.lifecycles, event, gaps);
                    }
                }

                let pattern = normalize_error(message);
                let category = classify_error(&pattern, severity);
                if severity == LogSeverity::Log && !is_relevant_log_event(&pattern, category) {
                    if routed && ts.is_none() {
                        gaps.timestamp_fallbacks = gaps.timestamp_fallbacks.saturating_add(1);
                    }
                    last_key = None;
                    last_continuation = None;
                    last_lifecycle = lifecycle_for_continuation;
                    lifecycle_detail_active = false;
                    continue;
                }
                if ts.is_none() {
                    gaps.timestamp_fallbacks = gaps.timestamp_fallbacks.saturating_add(1);
                }
                let key = ErrorKey {
                    pattern,
                    severity,
                    sqlstate: sqlstate.map(str::to_owned),
                };
                let entry = pending.entry(key.clone()).or_insert_with(|| PendingError {
                    ts: used_ts,
                    count: 0,
                    sample: truncate_utf8(message, MAX_TEXT_BYTES).to_owned(),
                    detail: None,
                    hint: None,
                    context: None,
                    statement: None,
                });
                entry.count = entry.count.saturating_add(1);
                last_key = Some(key);
                last_continuation = None;
                last_lifecycle = lifecycle_for_continuation;
                lifecycle_detail_active = false;
            }
            Some(ParsedLine::Continuation { kind, text }) => {
                if let Some(slow) = pending_slow.as_mut() {
                    append_string_capped(&mut slow.sql, text, MAX_TEXT_BYTES);
                    continue;
                }
                last_continuation =
                    append_to_last_continuation(&mut pending, last_key.as_ref(), kind, text)
                        .then_some(kind);
                lifecycle_detail_active = kind == ContinuationKind::Detail
                    && apply_lifecycle_detail(&mut records.lifecycles, last_lifecycle, text);
            }
            None => {
                last_key = None;
                last_continuation = None;
                last_lifecycle = None;
                lifecycle_detail_active = false;
            }
        }
    }
    flush_pending_slow_query(&mut pending_slow, &mut slow_groups);
    records.errors = error_rows_from_pending(pending, gaps);
    records.slow_queries = slow_rows_from_groups(slow_groups, gaps);
    records
}

fn error_rows_from_pending(
    pending: HashMap<ErrorKey, PendingError>,
    gaps: &mut ParseGaps,
) -> Vec<GroupedLogError> {
    let mut rows: Vec<_> = pending
        .into_iter()
        .map(|(key, entry)| GroupedLogError {
            ts: entry.ts,
            severity: key.severity,
            category: classify_error(&key.pattern, key.severity),
            sqlstate: key.sqlstate,
            pattern: key.pattern,
            count: entry.count,
            sample: entry.sample,
            detail: entry.detail,
            hint: entry.hint,
            context: entry.context,
            statement: entry.statement,
        })
        .collect();
    rows.sort_by(|a, b| {
        retention_priority(a.severity)
            .cmp(&retention_priority(b.severity))
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| {
                (a.severity, a.category, a.pattern.as_str(), a.ts).cmp(&(
                    b.severity,
                    b.category,
                    b.pattern.as_str(),
                    b.ts,
                ))
            })
    });
    if rows.len() > MAX_ERROR_GROUPS {
        gaps.dropped_groups = u32_saturating((rows.len() - MAX_ERROR_GROUPS) as u64);
        rows.truncate(MAX_ERROR_GROUPS);
    }
    rows.sort_by(|a, b| {
        (a.severity, a.category, a.pattern.as_str(), a.ts).cmp(&(
            b.severity,
            b.category,
            b.pattern.as_str(),
            b.ts,
        ))
    });
    rows
}

fn slow_rows_from_groups(
    groups: HashMap<String, PendingSlowQueryGroup>,
    gaps: &mut ParseGaps,
) -> Vec<SlowQueryEvent> {
    let mut rows: Vec<_> = groups
        .into_values()
        .map(|entry| SlowQueryEvent {
            ts: entry.ts,
            pattern: entry.pattern,
            sample: entry.sample,
            count: entry.count,
            max_duration_ms: entry.max_duration_ms,
            total_duration_ms: entry.total_duration_ms,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.max_duration_ms
            .total_cmp(&a.max_duration_ms)
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| a.pattern.cmp(&b.pattern))
            .then_with(|| a.ts.cmp(&b.ts))
    });
    if rows.len() > MAX_SLOW_QUERY_GROUPS {
        gaps.dropped_events = gaps
            .dropped_events
            .saturating_add(u32_saturating((rows.len() - MAX_SLOW_QUERY_GROUPS) as u64));
        rows.truncate(MAX_SLOW_QUERY_GROUPS);
    }
    rows.sort_by(|a, b| (a.pattern.as_str(), a.ts).cmp(&(b.pattern.as_str(), b.ts)));
    rows
}

fn flush_pending_slow_query(
    pending: &mut Option<PendingSlowQuery>,
    groups: &mut HashMap<String, PendingSlowQueryGroup>,
) {
    let Some(query) = pending.take() else {
        return;
    };
    let pattern = normalize_slow_sql(&query.sql);
    let entry = groups
        .entry(pattern.clone())
        .or_insert_with(|| PendingSlowQueryGroup {
            ts: query.ts,
            pattern,
            sample: query.sql.clone(),
            count: 0,
            max_duration_ms: query.duration_ms,
            total_duration_ms: 0.0,
        });
    entry.count = entry.count.saturating_add(1);
    entry.total_duration_ms += query.duration_ms;
    if query.duration_ms > entry.max_duration_ms {
        entry.ts = query.ts;
        entry.sample = query.sql;
        entry.max_duration_ms = query.duration_ms;
    }
}

fn normalize_slow_sql(sql: &str) -> String {
    let normalized = normalize_error(sql);
    let normalized = replace_numeric_literals(&normalized);
    truncate_utf8(&normalized, MAX_PATTERN_BYTES).to_owned()
}

fn replace_numeric_literals(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if !bytes[idx].is_ascii_digit() {
            let Some(ch) = value.get(idx..).and_then(|tail| tail.chars().next()) else {
                break;
            };
            out.push(ch);
            idx += ch.len_utf8();
            continue;
        }
        let start = idx;
        while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b'.') {
            idx += 1;
        }
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = idx >= bytes.len() || !is_ident_byte(bytes[idx]);
        if before_ok && after_ok {
            out.push_str("...");
        } else {
            out.push_str(value.get(start..idx).unwrap_or_default());
        }
    }
    out
}

const fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn push_checkpoint_event(
    rows: &mut Vec<CheckpointEvent>,
    event: CheckpointEvent,
    gaps: &mut ParseGaps,
) {
    if rows.len() >= MAX_CHECKPOINT_EVENTS {
        gaps.dropped_events = gaps.dropped_events.saturating_add(1);
        return;
    }
    rows.push(event);
}

fn push_lifecycle_event(
    rows: &mut Vec<LifecycleEvent>,
    event: LifecycleEvent,
    gaps: &mut ParseGaps,
) -> Option<usize> {
    if rows.len() >= MAX_LIFECYCLE_EVENTS {
        gaps.dropped_events = gaps.dropped_events.saturating_add(1);
        return None;
    }
    rows.push(event);
    Some(rows.len() - 1)
}

const CHECKPOINT_STARTING: &[&str] = &["checkpoint starting:", "начата контрольная точка:"];
const CHECKPOINT_COMPLETE: &[&str] = &["checkpoint complete:", "контрольная точка завершена:"];
const CHECKPOINT_TOO_FREQUENT: &[&str] = &[
    "checkpoints are occurring too frequently",
    "контрольные точки происходят слишком часто",
];
const DURATION_PREFIXES: &[(&str, &str)] = &[
    ("duration: ", " ms  statement: "),
    ("продолжительность: ", " мс  оператор: "),
];
const SERVER_CRASH_PREFIXES: &[&str] = &["server process (PID ", "серверный процесс (PID "];
const SERVER_SHUTDOWN_PREFIXES: &[(&str, &str)] = &[
    ("received fast shutdown request", "fast"),
    ("received smart shutdown request", "smart"),
    ("received immediate shutdown request", "immediate"),
    ("получен запрос на быстрое выключение", "fast"),
    ("получен запрос на умное выключение", "smart"),
    ("получен запрос на немедленное выключение", "immediate"),
];
const SERVER_READY_PREFIXES: &[&str] = &[
    "database system is ready to accept connections",
    "система БД готова принимать подключения",
];

fn parse_checkpoint_event(message: &str, ts: i64) -> Option<CheckpointEvent> {
    for prefix in CHECKPOINT_STARTING {
        if let Some(reason) = message.strip_prefix(prefix) {
            return Some(CheckpointEvent {
                ts,
                phase: CheckpointPhase::Starting,
                reason: non_empty_text(reason.trim()),
                seconds_apart: None,
                buffers_written: None,
                write_ms: None,
                sync_ms: None,
                total_ms: None,
                distance_kb: None,
                estimate_kb: None,
                wal_added: None,
                wal_removed: None,
                wal_recycled: None,
                sync_files: None,
                longest_sync_ms: None,
                average_sync_ms: None,
            });
        }
    }
    if CHECKPOINT_COMPLETE
        .iter()
        .any(|prefix| message.starts_with(prefix))
    {
        let (wal_added, wal_removed, wal_recycled) =
            parse_wal_file_counts(message).unwrap_or((None, None, None));
        return Some(CheckpointEvent {
            ts,
            phase: CheckpointPhase::Complete,
            reason: None,
            seconds_apart: None,
            buffers_written: extract_i64_after(message, "wrote ")
                .or_else(|| extract_i64_after(message, "записано буферов: ")),
            write_ms: extract_seconds_as_ms(message, "write=")
                .or_else(|| extract_seconds_as_ms(message, "запись=")),
            sync_ms: extract_seconds_as_ms(message, "sync=")
                .or_else(|| extract_seconds_as_ms(message, "синхронизация=")),
            total_ms: extract_seconds_as_ms(message, "total=")
                .or_else(|| extract_seconds_as_ms(message, "всего=")),
            distance_kb: extract_i64_after(message, "distance=")
                .or_else(|| extract_i64_after(message, "расстояние=")),
            estimate_kb: extract_i64_after(message, "estimate=")
                .or_else(|| extract_i64_after(message, "ожидалось=")),
            wal_added,
            wal_removed,
            wal_recycled,
            sync_files: extract_i64_after(message, "sync files=")
                .or_else(|| extract_i64_after(message, "синхронизировано файлов: ")),
            longest_sync_ms: extract_seconds_as_ms(message, "longest=")
                .or_else(|| extract_seconds_as_ms(message, "самый долгий: ")),
            average_sync_ms: extract_seconds_as_ms(message, "average=")
                .or_else(|| extract_seconds_as_ms(message, "средний: ")),
        });
    }
    if CHECKPOINT_TOO_FREQUENT
        .iter()
        .any(|prefix| message.starts_with(prefix))
    {
        return Some(CheckpointEvent {
            ts,
            phase: CheckpointPhase::TooFrequent,
            reason: Some(truncate_utf8(message, MAX_TEXT_BYTES).to_owned()),
            seconds_apart: extract_parenthesized_i64(message),
            buffers_written: None,
            write_ms: None,
            sync_ms: None,
            total_ms: None,
            distance_kb: None,
            estimate_kb: None,
            wal_added: None,
            wal_removed: None,
            wal_recycled: None,
            sync_files: None,
            longest_sync_ms: None,
            average_sync_ms: None,
        });
    }
    None
}

fn parse_slow_query_event(message: &str, ts: i64) -> Option<PendingSlowQuery> {
    for &(duration_prefix, statement_marker) in DURATION_PREFIXES {
        let Some(rest) = message.strip_prefix(duration_prefix) else {
            continue;
        };
        let Some(statement_pos) = rest.find(statement_marker) else {
            continue;
        };
        let duration_ms = rest.get(..statement_pos)?.parse::<f64>().ok()?;
        if !duration_ms.is_finite() || duration_ms < 0.0 {
            return None;
        }
        let sql_start = statement_pos + statement_marker.len();
        return Some(PendingSlowQuery {
            ts,
            duration_ms,
            sql: truncate_utf8(rest.get(sql_start..)?.trim(), MAX_TEXT_BYTES).to_owned(),
        });
    }
    None
}

fn parse_lifecycle_event(message: &str, ts: i64) -> Option<LifecycleEvent> {
    if SERVER_CRASH_PREFIXES
        .iter()
        .any(|prefix| message.starts_with(prefix))
    {
        return Some(LifecycleEvent {
            ts,
            kind: LifecycleKind::Crash,
            pid: parse_crash_pid(message),
            signal: parse_crash_signal(message),
            shutdown_mode: None,
            message: truncate_utf8(message, MAX_TEXT_BYTES).to_owned(),
            query_detail: None,
        });
    }
    for &(prefix, mode) in SERVER_SHUTDOWN_PREFIXES {
        if message.starts_with(prefix) {
            return Some(LifecycleEvent {
                ts,
                kind: LifecycleKind::Shutdown,
                pid: None,
                signal: None,
                shutdown_mode: Some(mode.to_owned()),
                message: truncate_utf8(message, MAX_TEXT_BYTES).to_owned(),
                query_detail: None,
            });
        }
    }
    if SERVER_READY_PREFIXES
        .iter()
        .any(|prefix| message.starts_with(prefix))
    {
        return Some(LifecycleEvent {
            ts,
            kind: LifecycleKind::Ready,
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: truncate_utf8(message, MAX_TEXT_BYTES).to_owned(),
            query_detail: None,
        });
    }
    None
}

fn apply_lifecycle_detail(rows: &mut [LifecycleEvent], index: Option<usize>, text: &str) -> bool {
    let Some(index) = index else {
        return false;
    };
    let Some(sql) = extract_crash_detail_sql(text) else {
        return false;
    };
    let Some(row) = rows.get_mut(index) else {
        return false;
    };
    append_option_text_capped(&mut row.query_detail, &sql, MAX_TEXT_BYTES);
    true
}

fn append_lifecycle_detail_continuation(
    rows: &mut [LifecycleEvent],
    index: Option<usize>,
    text: &str,
) -> bool {
    let Some(row) = index.and_then(|index| rows.get_mut(index)) else {
        return false;
    };
    append_option_text_capped(&mut row.query_detail, text, MAX_TEXT_BYTES);
    true
}

fn extract_crash_detail_sql(text: &str) -> Option<String> {
    for marker in ["was running: ", "выполнял действие: "] {
        if let Some(pos) = text.find(marker) {
            return Some(
                truncate_utf8(text.get(pos + marker.len()..)?.trim(), MAX_TEXT_BYTES).to_owned(),
            );
        }
    }
    (text.contains("was running") || text.contains("выполнял действие")).then(String::new)
}

fn parse_crash_pid(message: &str) -> Option<i32> {
    let start = message.find("(PID ")? + "(PID ".len();
    parse_i32_prefix(message.get(start..)?)
}

fn parse_crash_signal(message: &str) -> Option<i32> {
    for marker in ["signal ", "сигналом "] {
        if let Some(pos) = message.find(marker) {
            return parse_i32_prefix(message.get(pos + marker.len()..)?);
        }
    }
    None
}

fn parse_i32_prefix(value: &str) -> Option<i32> {
    let end = value
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(value.len());
    value.get(..end)?.parse().ok()
}

fn parse_wal_file_counts(message: &str) -> Option<(Option<i64>, Option<i64>, Option<i64>)> {
    if let Some(pos) = message.find(" WAL file") {
        let added = extract_trailing_i64(message.get(..pos)?);
        let removed = extract_i64_after(message, "added, ");
        let recycled = extract_i64_after(message, "removed, ");
        return Some((added, removed, recycled));
    }
    if message.contains("добавлено файлов WAL:") {
        let added = extract_i64_after(message, "добавлено файлов WAL: ");
        let removed = extract_i64_after(message, "удалено: ");
        let recycled = extract_i64_after(message, "переработано: ");
        return Some((added, removed, recycled));
    }
    None
}

fn extract_parenthesized_i64(message: &str) -> Option<i64> {
    let start = message.find('(')? + 1;
    let rest = message.get(start..)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

fn extract_trailing_i64(text: &str) -> Option<i64> {
    let trimmed = text.trim_end();
    let end = trimmed.len();
    let start = trimmed
        .rfind(|c: char| !c.is_ascii_digit() && c != '-')
        .map_or(0, |pos| pos + 1);
    if start >= end {
        return None;
    }
    trimmed.get(start..end)?.parse().ok()
}

fn extract_i64_after(text: &str, marker: &str) -> Option<i64> {
    let start = text.find(marker)? + marker.len();
    let rest = text.get(start..)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

fn extract_f64_after(text: &str, marker: &str) -> Option<f64> {
    let start = text.find(marker)? + marker.len();
    let rest = text.get(start..)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(rest.len());
    let value = rest.get(..end)?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn extract_seconds_as_ms(text: &str, marker: &str) -> Option<f64> {
    extract_f64_after(text, marker).map(|seconds| seconds * 1000.0)
}

fn non_empty_text(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| truncate_utf8(value, MAX_TEXT_BYTES).to_owned())
}

fn append_to_last_continuation(
    pending: &mut HashMap<ErrorKey, PendingError>,
    last_key: Option<&ErrorKey>,
    kind: ContinuationKind,
    text: &str,
) -> bool {
    let Some(key) = last_key else {
        return false;
    };
    let Some(entry) = pending.get_mut(key) else {
        return false;
    };
    append_continuation(entry, kind, text);
    true
}

const fn retention_priority(severity: LogSeverity) -> u8 {
    match severity {
        LogSeverity::Panic | LogSeverity::Fatal => 0,
        LogSeverity::Error | LogSeverity::Warning | LogSeverity::Log => 1,
    }
}

fn is_relevant_log_event(pattern: &str, category: ErrorCategory) -> bool {
    let lower = pattern.to_ascii_lowercase();
    match category {
        ErrorCategory::Resource => {
            lower.contains("terminated by signal") && lower.contains(": killed")
        }
        ErrorCategory::System => {
            lower.contains("crash")
                || (lower.contains("server process")
                    && (lower.contains("terminated by signal")
                        || lower.contains("exited with exit code")))
                || lower.contains("all server processes terminated")
        }
        _ => false,
    }
}

fn append_continuation(entry: &mut PendingError, kind: ContinuationKind, text: &str) {
    let target = match kind {
        ContinuationKind::Detail => &mut entry.detail,
        ContinuationKind::Hint => &mut entry.hint,
        ContinuationKind::Context => &mut entry.context,
        ContinuationKind::Statement => &mut entry.statement,
    };
    append_text(target, text);
}

fn append_text(target: &mut Option<String>, text: &str) {
    append_option_text_capped(target, text, MAX_TEXT_BYTES);
}

fn append_option_text_capped(target: &mut Option<String>, text: &str, max_bytes: usize) {
    if text.is_empty() || max_bytes == 0 {
        return;
    }
    let current = target.get_or_insert_with(String::new);
    append_string_capped(current, text, max_bytes);
}

fn append_string_capped(target: &mut String, text: &str, max_bytes: usize) {
    if text.is_empty() || target.len() >= max_bytes {
        return;
    }
    if !target.is_empty() {
        if target.len().saturating_add(1) >= max_bytes {
            return;
        }
        target.push(' ');
    }
    if target.len() >= max_bytes {
        return;
    }
    let remaining = max_bytes - target.len();
    target.push_str(truncate_utf8(text, remaining));
}

fn gaps_from_tail(
    ts: i64,
    source_path: &Path,
    parser_kind: ParserKind,
    gaps: TailGaps,
    state: Option<&TailState>,
) -> Vec<LogGap> {
    let mut rows = Vec::new();
    if gaps.backlog_bytes_skipped != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::Backlog,
            bytes_skipped: gaps.backlog_bytes_skipped,
            ..file_state_fields(state)
        });
    }
    if gaps.sparse_bytes_skipped != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::Sparse,
            bytes_skipped: gaps.sparse_bytes_skipped,
            ..file_state_fields(state)
        });
    }
    if gaps.truncated_lines != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::Truncate,
            truncated_lines: gaps.truncated_lines,
            ..file_state_fields(state)
        });
    }
    if gaps.binary_lines_dropped != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::Binary,
            binary_dropped: gaps.binary_lines_dropped,
            ..file_state_fields(state)
        });
    }
    if gaps.rotations != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::Rotation,
            rotations: gaps.rotations,
            ..file_state_fields(state)
        });
    }
    if gaps.missing_files != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::MissingFile,
            missing_files: gaps.missing_files,
            ..file_state_fields(state)
        });
    }
    if gaps.budget_exhaustions != 0 {
        rows.push(LogGap {
            ts,
            source_path: Some(source_path.to_owned()),
            parser_kind,
            reason: GapReason::BudgetExhausted,
            budget_exhaustions: gaps.budget_exhaustions,
            ..file_state_fields(state)
        });
    }
    rows
}

fn file_state_fields(state: Option<&TailState>) -> LogGap {
    LogGap {
        source_path: None,
        parser_kind: state.map_or(ParserKind::Unknown, |state| state.parser_kind),
        reason: GapReason::SourceUnavailable,
        ts: 0,
        dev: state.map(|state| state.dev),
        inode: state.map(|state| state.inode),
        offset: state.map(|state| state.offset),
        bytes_skipped: 0,
        truncated_lines: 0,
        invalid_utf8: 0,
        binary_dropped: 0,
        rotations: 0,
        missing_files: 0,
        budget_exhaustions: 0,
        dict_dropped_fields: 0,
        parser_dropped_lines: 0,
    }
}

const fn empty_gap() -> LogGap {
    LogGap {
        ts: 0,
        source_path: None,
        parser_kind: ParserKind::Unknown,
        reason: GapReason::SourceUnavailable,
        dev: None,
        inode: None,
        offset: None,
        bytes_skipped: 0,
        truncated_lines: 0,
        invalid_utf8: 0,
        binary_dropped: 0,
        rotations: 0,
        missing_files: 0,
        budget_exhaustions: 0,
        dict_dropped_fields: 0,
        parser_dropped_lines: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CheckpointPhase, GapReason, LifecycleKind, LogCollector, LogConfig, read_error_reason,
    };
    use crate::{ErrorCategory, LogSeverity, ParserKind};
    use std::fmt::Write as _;
    use std::io;

    fn fixture_config(path: std::path::PathBuf, state_path: std::path::PathBuf) -> LogConfig {
        LogConfig {
            enabled: true,
            path_override: Some(path),
            root_override: None,
            parser_kind: ParserKind::Stderr,
            state_path,
            start_at_beginning: true,
            discovery_interval: std::time::Duration::from_mins(1),
            tail_caps: crate::TailCaps::default(),
        }
    }

    #[tokio::test]
    async fn collects_grouped_errors_from_stderr_fixture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:00:00 UTC [1]: ERROR:  relation \"a\" does not exist\n\
             2026-07-05 12:00:01 UTC [1]: STATEMENT:  select * from a\n\
             2026-07-05 12:00:02 UTC [1]: ERROR:  relation \"b\" does not exist\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");
        let batch = collector.collect(None, 1).await;
        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.errors[0].count, 2);
        assert_eq!(batch.errors[0].severity, LogSeverity::Error);
        assert_eq!(batch.errors[0].category, ErrorCategory::Syntax);
        assert_eq!(
            batch.errors[0].statement.as_deref(),
            Some("select * from a")
        );
    }

    #[tokio::test]
    async fn preserves_deadlock_diagnostics_as_typed_continuations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:00:00 UTC [1]: ERROR:  deadlock detected\n\
             2026-07-05 12:00:00 UTC [1]: DETAIL:  Process 111 waits for ShareLock on transaction 10; blocked by process 222.\n\
             \tProcess 222 waits for ShareLock on transaction 11; blocked by process 111.\n\
             2026-07-05 12:00:00 UTC [1]: HINT:  See server log for query details.\n\
             2026-07-05 12:00:00 UTC [1]: CONTEXT:  while updating tuple (0,1) in relation \"deadlock_probe\"\n\
             2026-07-05 12:00:00 UTC [1]: STATEMENT:  UPDATE deadlock_probe SET id = id WHERE id = 1\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert_eq!(batch.errors.len(), 1);
        let row = &batch.errors[0];
        assert_eq!(row.pattern, "deadlock detected");
        assert_eq!(row.category, ErrorCategory::Lock);
        assert_eq!(
            row.detail.as_deref(),
            Some(
                "Process 111 waits for ShareLock on transaction 10; blocked by process 222. \
                 Process 222 waits for ShareLock on transaction 11; blocked by process 111."
            )
        );
        assert_eq!(
            row.hint.as_deref(),
            Some("See server log for query details.")
        );
        assert_eq!(
            row.context.as_deref(),
            Some("while updating tuple (0,1) in relation \"deadlock_probe\"")
        );
        assert_eq!(
            row.statement.as_deref(),
            Some("UPDATE deadlock_probe SET id = id WHERE id = 1")
        );
    }

    #[tokio::test]
    async fn collects_oom_kill_and_crash_log_events_without_dumping_ordinary_logs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:00:00 UTC [1]: LOG:  checkpoint starting: immediate force wait\n\
             2026-07-05 12:00:01 UTC [2]: LOG:  server process (PID 4242) was terminated by signal 9: Killed\n\
             2026-07-05 12:00:02 UTC [3]: LOG:  server process (PID 4243) was terminated by signal 11: Segmentation fault\n\
             2026-07-05 12:00:03 UTC [4]: WARNING:  terminating connection because of crash of another server process\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert_eq!(batch.errors.len(), 3);
        let oom = batch
            .errors
            .iter()
            .find(|row| row.pattern == "server process (...) was terminated by signal ...: Killed")
            .expect("oom kill row");
        assert_eq!(oom.severity, LogSeverity::Log);
        assert_eq!(oom.category, ErrorCategory::Resource);
        let segfault = batch
            .errors
            .iter()
            .find(|row| {
                row.pattern
                    == "server process (...) was terminated by signal ...: Segmentation fault"
            })
            .expect("segfault row");
        assert_eq!(segfault.severity, LogSeverity::Log);
        assert_eq!(segfault.category, ErrorCategory::System);
        let crash_warning = batch
            .errors
            .iter()
            .find(|row| {
                row.pattern == "terminating connection because of crash of another server process"
            })
            .expect("crash warning row");
        assert_eq!(crash_warning.severity, LogSeverity::Warning);
        assert_eq!(crash_warning.category, ErrorCategory::System);
        assert_eq!(batch.checkpoints.len(), 1);
        assert_eq!(batch.checkpoints[0].phase, CheckpointPhase::Starting);
        assert_eq!(
            batch.checkpoints[0].reason.as_deref(),
            Some("immediate force wait")
        );
        assert_eq!(batch.lifecycles.len(), 2);
    }

    #[tokio::test]
    async fn collects_checkpoint_events_with_nullable_metrics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:03:00 UTC [1]: LOG:  checkpoint starting: time\n\
             2026-07-05 12:03:01 UTC [1]: LOG:  checkpoint complete: wrote 128 buffers (0.2%); 0 WAL file(s) added, 1 removed, 2 recycled; write=1.234 s, sync=0.056 s, total=1.500 s; sync files=7, longest=0.040 s, average=0.008 s; distance=4096 kB, estimate=8192 kB\n\
             2026-07-05 12:03:02 UTC [1]: LOG:  checkpoints are occurring too frequently (3 seconds apart)\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert!(batch.errors.is_empty());
        assert_eq!(batch.checkpoints.len(), 3);
        assert_eq!(batch.checkpoints[0].phase, CheckpointPhase::Starting);
        assert_eq!(batch.checkpoints[0].reason.as_deref(), Some("time"));
        let complete = batch
            .checkpoints
            .iter()
            .find(|event| event.phase == CheckpointPhase::Complete)
            .expect("complete checkpoint");
        assert_eq!(complete.buffers_written, Some(128));
        assert_eq!(complete.write_ms, Some(1234.0));
        assert_eq!(complete.sync_ms, Some(56.0));
        assert_eq!(complete.total_ms, Some(1500.0));
        assert_eq!(complete.wal_added, Some(0));
        assert_eq!(complete.wal_removed, Some(1));
        assert_eq!(complete.wal_recycled, Some(2));
        assert_eq!(complete.sync_files, Some(7));
        let too_frequent = batch
            .checkpoints
            .iter()
            .find(|event| event.phase == CheckpointPhase::TooFrequent)
            .expect("too frequent checkpoint");
        assert_eq!(too_frequent.seconds_apart, Some(3));
    }

    #[tokio::test]
    async fn collects_slow_query_topn_and_ignores_ordinary_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:04:00 UTC [1]: LOG:  listening on IPv4 address \"127.0.0.1\", port 5432\n\
             2026-07-05 12:04:01 UTC [1]: LOG:  duration: 1500.250 ms  statement: SELECT *\n\
             \tFROM slow_table WHERE id = 42\n\
             2026-07-05 12:04:02 UTC [1]: LOG:  duration: 500.000 ms  statement: SELECT * FROM slow_table WHERE id = 99\n\
             2026-07-05 12:04:03 UTC [1]: LOG:  duration: 10.000 ms\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert!(batch.errors.is_empty());
        assert!(batch.checkpoints.is_empty());
        assert!(batch.lifecycles.is_empty());
        assert_eq!(batch.slow_queries.len(), 1);
        let slow = &batch.slow_queries[0];
        assert_eq!(slow.count, 2);
        assert_close(slow.max_duration_ms, 1500.250);
        assert_close(slow.total_duration_ms, 2000.250);
        assert_eq!(slow.sample, "SELECT * FROM slow_table WHERE id = 42");
        assert!(slow.sample.len() <= crate::MAX_TEXT_BYTES);
    }

    #[tokio::test]
    async fn slow_query_topn_overflow_emits_parser_drop_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        let mut content = String::new();
        for idx in 0..18 {
            writeln!(
                &mut content,
                "2026-07-05 12:04:{idx:02} UTC [1]: LOG:  duration: {}.000 ms  statement: SELECT * FROM slow_table_{idx}",
                idx + 1
            )
            .expect("format fixture line");
        }
        std::fs::write(&log, content).expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert_eq!(batch.slow_queries.len(), 16);
        let parser_drop = batch
            .gaps
            .iter()
            .find(|gap| gap.reason == GapReason::ParserDrop)
            .expect("parser drop gap");
        assert_eq!(parser_drop.parser_dropped_lines, 2);
    }

    #[tokio::test]
    async fn collects_lifecycle_events_with_crash_detail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:05:00 UTC [1]: LOG:  server process (PID 4242) was terminated by signal 9: Killed\n\
             2026-07-05 12:05:00 UTC [1]: DETAIL:  Failed process was running: SELECT pg_sleep(10)\n\
             \tFROM lifecycle_probe\n\
             2026-07-05 12:05:01 UTC [1]: LOG:  received fast shutdown request\n\
             2026-07-05 12:05:02 UTC [1]: LOG:  database system is ready to accept connections\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.lifecycles.len(), 3);
        let crash = batch
            .lifecycles
            .iter()
            .find(|event| event.kind == LifecycleKind::Crash)
            .expect("crash lifecycle");
        assert_eq!(crash.pid, Some(4242));
        assert_eq!(crash.signal, Some(9));
        assert_eq!(
            crash.query_detail.as_deref(),
            Some("SELECT pg_sleep(10) FROM lifecycle_probe")
        );
        let shutdown = batch
            .lifecycles
            .iter()
            .find(|event| event.kind == LifecycleKind::Shutdown)
            .expect("shutdown lifecycle");
        assert_eq!(shutdown.shutdown_mode.as_deref(), Some("fast"));
        assert!(
            batch
                .lifecycles
                .iter()
                .any(|event| event.kind == LifecycleKind::Ready)
        );
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_001,
            "expected {actual} to be close to {expected}"
        );
    }

    #[tokio::test]
    async fn emits_gap_for_invalid_utf8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(&log, b"2026 bad \xff\n").expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");
        let batch = collector.collect(None, 1).await;
        assert!(batch.errors.is_empty());
        assert!(
            batch
                .gaps
                .iter()
                .any(|gap| gap.reason == GapReason::InvalidUtf8)
        );
    }

    #[tokio::test]
    async fn budget_exhaustion_is_backpressure_not_parser_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(
            &log,
            "2026-07-05 12:00:00 UTC [1]: ERROR:  relation \"a\" does not exist\n\
             2026-07-05 12:00:01 UTC [1]: ERROR:  relation \"b\" does not exist\n",
        )
        .expect("write");
        let mut config = fixture_config(log, dir.path().join("state"));
        config.tail_caps.max_lines = 1;
        let mut collector = LogCollector::new(config).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert!(
            batch.gaps.iter().any(|gap| {
                gap.reason == GapReason::BudgetExhausted && gap.budget_exhaustions > 0
            })
        );
        assert!(
            !batch
                .gaps
                .iter()
                .any(|gap| gap.reason == GapReason::ParserDrop)
        );
    }

    #[tokio::test]
    async fn disabled_collection_emits_explicit_gap_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = LogConfig::disabled(dir.path());
        config.state_path = dir.path().join("state");
        let mut collector = LogCollector::new(config).expect("collector");

        let first = collector.collect(None, 1).await;
        let second = collector.collect(None, 2).await;

        assert!(
            first
                .gaps
                .iter()
                .any(|gap| gap.reason == GapReason::Disabled)
        );
        assert!(second.gaps.is_empty());
    }

    #[tokio::test]
    async fn timestamp_fallback_emits_explicit_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        std::fs::write(&log, "ERROR:  division by zero\n").expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.errors[0].ts, 1);
        assert!(
            batch
                .gaps
                .iter()
                .any(|gap| gap.reason == GapReason::TimestampFallback)
        );
    }

    #[tokio::test]
    async fn fatal_rows_are_retained_before_lower_severity_overflow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        let mut text = String::new();
        for idx in 0..32 {
            writeln!(
                &mut text,
                "2026-07-05 12:00:{idx:02} UTC [1]: ERROR:  probe{idx} failure observed"
            )
            .expect("format error row");
            writeln!(
                &mut text,
                "2026-07-05 12:01:{idx:02} UTC [1]: ERROR:  probe{idx} failure observed"
            )
            .expect("format error row");
        }
        text.push_str("2026-07-05 12:02:00 UTC [1]: FATAL:  terminating connection due to administrator command\n");
        std::fs::write(&log, text).expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log, dir.path().join("state"))).expect("collector");

        let batch = collector.collect(None, 1).await;

        assert_eq!(batch.errors.len(), 32);
        assert!(
            batch
                .errors
                .iter()
                .any(|row| row.severity == LogSeverity::Fatal)
        );
        assert!(
            batch
                .gaps
                .iter()
                .any(|gap| gap.reason == GapReason::ParserDrop && gap.parser_dropped_lines == 1)
        );
    }

    #[test]
    fn permission_denied_is_a_distinct_gap_reason() {
        assert_eq!(
            read_error_reason(io::ErrorKind::PermissionDenied),
            GapReason::PermissionDenied
        );
        assert_eq!(
            read_error_reason(io::ErrorKind::NotFound),
            GapReason::SourceUnavailable
        );
    }

    #[tokio::test]
    async fn commit_persists_offset_after_collection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("postgresql.log");
        let state = dir.path().join("state");
        std::fs::write(
            &log,
            "2026-07-05 12:00:00 UTC [1]: ERROR:  division by zero\n",
        )
        .expect("write");
        let mut collector =
            LogCollector::new(fixture_config(log.clone(), state.clone())).expect("collector");
        let batch = collector.collect(None, 1).await;
        collector.commit(&batch).expect("commit");
        let mut collector = LogCollector::new(fixture_config(log, state)).expect("collector");
        let second = collector.collect(None, 2).await;
        assert!(second.errors.is_empty());
    }
}
