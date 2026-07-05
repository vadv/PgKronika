//! Source discovery, parsing, aggregation, and commit boundary.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio_postgres::Client;

use crate::normalize::{ErrorCategory, classify_error, normalize_error};
use crate::parser::{LogSeverity, ParsedLine, ParserKind, parse_stderr_line};
use crate::state::TailState;
use crate::tailer::{TailCaps, TailGaps, TailLine, read_batch};
use crate::{MAX_TEXT_BYTES, truncate_utf8, u32_saturating};

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
    /// Attached SQL statement.
    pub statement: Option<String>,
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
    statement: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ErrorKey {
    pattern: String,
    severity: LogSeverity,
    sqlstate: Option<String>,
}

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
        })
    }

    /// Whether this collector is active.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Collect one bounded log batch.
    pub async fn collect(&mut self, client: Option<&Client>, ts: i64) -> LogCollection {
        if !self.config.enabled {
            return LogCollection {
                discovery_status: Some(DiscoveryStatus::Disabled),
                ..LogCollection::default()
            };
        }

        let mut result = LogCollection::default();
        let discovery = self.refresh_source(client).await;
        result.discovery_status = Some(discovery);
        if matches!(
            discovery,
            DiscoveryStatus::UnsupportedFormat | DiscoveryStatus::SourceUnavailable
        ) {
            let reason = if discovery == DiscoveryStatus::UnsupportedFormat {
                GapReason::UnsupportedFormat
            } else {
                GapReason::SourceUnavailable
            };
            result.gaps.push(self.simple_gap(ts, reason));
            return result;
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

        let Ok(batch) = read_batch(
            &source.path,
            source.parser_kind,
            self.state.as_ref(),
            self.config.start_at_beginning,
            self.config.tail_caps,
        ) else {
            result.gaps.push(LogGap {
                parser_kind: source.parser_kind,
                reason: GapReason::SourceUnavailable,
                source_path: Some(source.path),
                ts,
                ..empty_gap()
            });
            return result;
        };
        let mut parse_gaps = ParseGaps::default();
        result.errors = parse_errors(&batch.lines, ts, &mut parse_gaps);
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
        if parse_gaps.dropped_groups != 0 {
            result.gaps.push(LogGap {
                ts,
                source_path: Some(source.path),
                parser_kind: source.parser_kind,
                reason: GapReason::ParserDrop,
                parser_dropped_lines: parse_gaps.dropped_groups,
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
}

fn parse_errors(
    lines: &[TailLine],
    fallback_ts: i64,
    gaps: &mut ParseGaps,
) -> Vec<GroupedLogError> {
    let mut pending = HashMap::<ErrorKey, PendingError>::new();
    let mut last_key = None::<ErrorKey>;
    for line in lines {
        let Ok(decoded) = std::str::from_utf8(&line.bytes) else {
            gaps.invalid_utf8 = gaps.invalid_utf8.saturating_add(1);
            last_key = None;
            continue;
        };
        let decoded = decoded.strip_suffix('\r').unwrap_or(decoded);
        if decoded.starts_with([' ', '\t']) {
            if let Some(key) = &last_key
                && let Some(entry) = pending.get_mut(key)
            {
                append_statement(entry, decoded.trim());
            }
            continue;
        }
        match parse_stderr_line(decoded) {
            Some(ParsedLine::Error {
                ts,
                severity,
                sqlstate,
                message,
            }) => {
                let pattern = normalize_error(message);
                let key = ErrorKey {
                    pattern,
                    severity,
                    sqlstate: sqlstate.map(str::to_owned),
                };
                let entry = pending.entry(key.clone()).or_insert_with(|| PendingError {
                    ts: ts.unwrap_or(fallback_ts),
                    count: 0,
                    sample: truncate_utf8(message, MAX_TEXT_BYTES).to_owned(),
                    statement: None,
                });
                entry.count = entry.count.saturating_add(1);
                last_key = Some(key);
            }
            Some(ParsedLine::Statement { text }) => {
                if let Some(key) = &last_key
                    && let Some(entry) = pending.get_mut(key)
                {
                    append_statement(entry, text);
                }
            }
            None => {
                last_key = None;
            }
        }
    }
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
            statement: entry.statement,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.count.cmp(&a.count).then_with(|| {
            (a.severity, a.category, a.pattern.as_str(), a.ts).cmp(&(
                b.severity,
                b.category,
                b.pattern.as_str(),
                b.ts,
            ))
        })
    });
    if rows.len() > 32 {
        gaps.dropped_groups = u32_saturating((rows.len() - 32) as u64);
        rows.truncate(32);
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

fn append_statement(entry: &mut PendingError, text: &str) {
    if text.is_empty() {
        return;
    }
    let current = entry.statement.get_or_insert_with(String::new);
    if !current.is_empty() {
        current.push(' ');
    }
    current.push_str(text);
    if current.len() > MAX_TEXT_BYTES {
        let truncated = truncate_utf8(current, MAX_TEXT_BYTES).to_owned();
        *current = truncated;
    }
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
            reason: GapReason::ParserDrop,
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
    use super::{GapReason, LogCollector, LogConfig};
    use crate::{ErrorCategory, LogSeverity, ParserKind};

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
