//! Structured stderr logging for the collector.
//!
//! Events render as one logfmt line per call: a fixed `pg_kronika-collector`
//! prefix, then `level` and `action`, then caller fields. `KRONIKA_LOG_LEVEL`
//! (read once) gates output; the stdout segment announcements stay in `main`.
//! The domain helpers below name the collection, its `type_id`, and its layout
//! consistently so operators can filter by any of them.

use std::fmt::{Display, Write as _};
use std::sync::OnceLock;
use std::time::Duration;

use kronika_writer::FlushSummary;

use crate::scheduler::SourceKind;
use crate::source_contracts::{user_indexes_type_id, user_tables_type_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

pub(crate) struct LogField<'a> {
    key: &'static str,
    value: LogValue<'a>,
}

pub(crate) enum LogValue<'a> {
    Str(&'a str),
    Display(&'a dyn Display),
    Owned(String),
    Bool(bool),
    I32(i32),
    I64(i64),
    U32(u32),
    U64(u64),
    U128(u128),
    Usize(usize),
}

pub(crate) trait IntoLogValue<'a> {
    fn into_log_value(self) -> LogValue<'a>;
}

impl<'a, T: Display> IntoLogValue<'a> for &'a T {
    fn into_log_value(self) -> LogValue<'a> {
        LogValue::Display(self)
    }
}

impl<'a> IntoLogValue<'a> for &'a str {
    fn into_log_value(self) -> LogValue<'a> {
        LogValue::Str(self)
    }
}

impl<'a> IntoLogValue<'a> for &'a dyn Display {
    fn into_log_value(self) -> LogValue<'a> {
        LogValue::Display(self)
    }
}

impl IntoLogValue<'static> for String {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::Owned(self)
    }
}

impl IntoLogValue<'static> for std::path::Display<'_> {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::Owned(self.to_string())
    }
}

impl IntoLogValue<'static> for bool {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::Bool(self)
    }
}

impl IntoLogValue<'static> for i32 {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::I32(self)
    }
}

impl IntoLogValue<'static> for i64 {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::I64(self)
    }
}

impl IntoLogValue<'static> for u32 {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::U32(self)
    }
}

impl IntoLogValue<'static> for u64 {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::U64(self)
    }
}

impl IntoLogValue<'static> for u128 {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::U128(self)
    }
}

impl IntoLogValue<'static> for usize {
    fn into_log_value(self) -> LogValue<'static> {
        LogValue::Usize(self)
    }
}

fn current_log_level() -> LogLevel {
    static LOG_LEVEL: OnceLock<LogLevel> = OnceLock::new();
    *LOG_LEVEL.get_or_init(|| {
        let Ok(value) = std::env::var("KRONIKA_LOG_LEVEL") else {
            return LogLevel::Info;
        };
        let value = value.trim();
        let Some(level) = parse_log_level_value(value) else {
            emit_log_event(
                LogLevel::Warn,
                "invalid_log_level",
                &[
                    field("env", "KRONIKA_LOG_LEVEL"),
                    field("value", value),
                    field("fallback", LogLevel::Info.as_str()),
                ],
            );
            return LogLevel::Info;
        };
        level
    })
}

fn parse_log_level_value(value: &str) -> Option<LogLevel> {
    let value = value.trim();
    let value = value.to_ascii_lowercase();
    LogLevel::parse(&value)
}

fn log_enabled(level: LogLevel) -> bool {
    level.rank() <= current_log_level().rank()
}

pub(crate) fn log_event(level: LogLevel, action: &'static str, fields: &[LogField<'_>]) {
    if log_enabled(level) {
        emit_log_event(level, action, fields);
    }
}

fn emit_log_event(level: LogLevel, action: &'static str, fields: &[LogField<'_>]) {
    let line = render_log_line(level, action, fields);
    eprintln!("{line}");
}

fn render_log_line(level: LogLevel, action: &'static str, fields: &[LogField<'_>]) -> String {
    let mut line = String::from("pg_kronika-collector");
    push_log_field(&mut line, "level", level.as_str());
    push_log_field(&mut line, "action", action);
    for field in fields {
        push_log_field_value(&mut line, field.key, &field.value);
    }
    line
}

fn push_log_field(line: &mut String, key: &str, value: &str) {
    line.push(' ');
    line.push_str(key);
    line.push('=');
    push_log_value(line, value);
}

fn push_log_field_value(line: &mut String, key: &str, value: &LogValue<'_>) {
    let mut rendered = String::new();
    match value {
        LogValue::Str(value) => {
            push_log_field(line, key, value);
            return;
        }
        LogValue::Display(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::Owned(value) => rendered.push_str(value),
        LogValue::Bool(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::I32(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::I64(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::U32(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::U64(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::U128(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
        LogValue::Usize(value) => {
            let _ = write!(&mut rendered, "{value}");
        }
    }
    push_log_field(line, key, &rendered);
}

fn push_log_value(line: &mut String, value: &str) {
    let plain = !value.is_empty()
        && value.chars().all(|ch| {
            !ch.is_whitespace() && !ch.is_control() && ch != '=' && ch != '"' && ch != '\\'
        });
    if plain {
        line.push_str(value);
        return;
    }
    line.push('"');
    for ch in value.chars() {
        match ch {
            '"' => line.push_str("\\\""),
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            '\t' => line.push_str("\\t"),
            _ if ch.is_control() => {
                line.push_str("\\u{");
                let _ = write!(line, "{:x}", ch as u32);
                line.push('}');
            }
            _ => line.push(ch),
        }
    }
    line.push('"');
}

pub(crate) fn field<'a>(key: &'static str, value: impl IntoLogValue<'a>) -> LogField<'a> {
    LogField {
        key,
        value: value.into_log_value(),
    }
}

pub(crate) const fn duration_ms(duration: Duration) -> u128 {
    duration.as_millis()
}

pub(crate) const fn layout_id(type_id: u32) -> u32 {
    type_id % 1_000
}

pub(crate) fn section_name(type_id: u32) -> &'static str {
    kronika_registry::section_name(type_id).unwrap_or("unknown")
}

/// The three identity fields shared by every section log: registry name,
/// raw `type_id`, and its layout suffix.
pub(crate) fn section_fields(type_id: u32) -> [LogField<'static>; 3] {
    [
        field("collection", section_name(type_id)),
        field("type_id", type_id),
        field("layout_id", layout_id(type_id)),
    ]
}

/// A source family whose section only becomes a concrete `type_id` later, so
/// its early events log the family name instead.
#[derive(Clone, Copy)]
pub(crate) enum CollectionFamily {
    Activity,
    Database,
    Statements,
    StorePlans,
    Wal,
    Io,
    ReplicationDetails,
}

impl CollectionFamily {
    const fn name(self) -> &'static str {
        match self {
            Self::Activity => "pg_stat_activity",
            Self::Database => "pg_stat_database",
            Self::Statements => "pg_stat_statements",
            Self::StorePlans => "pg_store_plans",
            Self::Wal => "pg_stat_wal",
            Self::Io => "pg_stat_io",
            Self::ReplicationDetails => "replication_details",
        }
    }

    pub(crate) fn field(self) -> LogField<'static> {
        field("collection", self.name())
    }
}

const fn source_kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Activity => "activity",
        SourceKind::Database => "database",
        SourceKind::Bgwriter => "bgwriter",
        SourceKind::Wal => "wal",
        SourceKind::Io => "io",
        SourceKind::Archiver => "archiver",
        SourceKind::PreparedXacts => "prepared_xacts",
        SourceKind::ProgressVacuum => "progress_vacuum",
        SourceKind::Statements => "statements",
        SourceKind::UserTables => "user_tables",
        SourceKind::UserIndexes => "user_indexes",
        SourceKind::Replication => "replication",
        SourceKind::ResetMetadata => "reset_metadata",
        SourceKind::InstanceMetadata => "instance_metadata",
        SourceKind::Settings => "settings",
        SourceKind::OsCore => "os_core",
        SourceKind::OsMountTopo => "os_mount_topo",
        SourceKind::OsProcesses => "os_processes",
        SourceKind::OsProcessStatus => "os_process_status",
        SourceKind::OsCgroup => "os_cgroup",
        SourceKind::OsCgroupMapping => "os_cgroup_mapping",
        SourceKind::PgLog => "pg_log",
    }
}

pub(crate) fn log_source_deferred(kind: SourceKind, major: u32) {
    let mut fields = vec![
        field("source_kind", source_kind_name(kind)),
        field("reason", "cycle_db_budget"),
        field("deferred", true),
    ];
    match kind {
        SourceKind::UserTables => fields.extend(section_fields(user_tables_type_id(major))),
        SourceKind::UserIndexes => fields.extend(section_fields(user_indexes_type_id(major))),
        SourceKind::Statements => fields.push(CollectionFamily::Statements.field()),
        SourceKind::Activity
        | SourceKind::Database
        | SourceKind::Bgwriter
        | SourceKind::Wal
        | SourceKind::Io
        | SourceKind::Archiver
        | SourceKind::PreparedXacts
        | SourceKind::ProgressVacuum
        | SourceKind::Replication
        | SourceKind::ResetMetadata
        | SourceKind::InstanceMetadata
        | SourceKind::Settings
        | SourceKind::OsCore
        | SourceKind::OsMountTopo
        | SourceKind::OsProcesses
        | SourceKind::OsProcessStatus
        | SourceKind::OsCgroup
        | SourceKind::OsCgroupMapping
        | SourceKind::PgLog => {}
    }
    log_event(LogLevel::Debug, "collection_deferred", &fields);
}

pub(crate) fn log_collection_start(type_id: u32, source: &str) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Debug,
        "collection_start",
        &[collection, type_id, layout_id, field("source", source)],
    );
}

pub(crate) fn log_database_collection_start(type_id: u32, database: &str) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Debug,
        "collection_start",
        &[
            collection,
            type_id,
            layout_id,
            field("source", "database"),
            field("database", database),
        ],
    );
}

pub(crate) fn log_collection_finish(type_id: u32, source: &str, rows: usize, elapsed: Duration) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Debug,
        "collection_finish",
        &[
            collection,
            type_id,
            layout_id,
            field("source", source),
            field("rows", rows),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
}

/// A per-database top-N read that finished, with the source population it was
/// cut from (`source_total`).
pub(crate) fn log_database_collection_finish(
    type_id: u32,
    database: &str,
    rows: usize,
    source_total: u64,
    elapsed: Duration,
) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Debug,
        "collection_finish",
        &[
            collection,
            type_id,
            layout_id,
            field("source", "database"),
            field("database", database),
            field("rows", rows),
            field("source_total", source_total),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
}

/// A per-database read the adaptive `statement_timeout` widened for a retry.
pub(crate) fn log_database_collection_retry(
    type_id: u32,
    database: &str,
    timeout_ms: u64,
    next_timeout_ms: u64,
    err: &(dyn Display + '_),
    elapsed: Duration,
) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Warn,
        "collection_retry",
        &[
            collection,
            type_id,
            layout_id,
            field("source", "database"),
            field("database", database),
            field("reason", "statement_timeout"),
            field("timeout_ms", timeout_ms),
            field("next_timeout_ms", next_timeout_ms),
            field("error", err),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
}

/// A per-database read skipped for `reason` (contention, timeout at cap,
/// permission, or query error).
pub(crate) fn log_database_collection_skip(
    type_id: u32,
    database: &str,
    reason: &'static str,
    err: &(dyn Display + '_),
    elapsed: Duration,
) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Warn,
        "collection_skip",
        &[
            collection,
            type_id,
            layout_id,
            field("source", "database"),
            field("database", database),
            field("reason", reason),
            field("error", err),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
}

pub(crate) fn log_collection_failure(
    type_id: u32,
    source: &str,
    err: &(dyn Display + '_),
    elapsed: Duration,
) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Error,
        "collection_failure",
        &[
            collection,
            type_id,
            layout_id,
            field("source", source),
            field("error", err),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
}

pub(crate) fn log_count_degraded(
    type_id: u32,
    source: &'static str,
    reason: &'static str,
    count: usize,
) {
    let [collection, type_id, layout_id] = section_fields(type_id);
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            collection,
            type_id,
            layout_id,
            field("source", source),
            field("reason", reason),
            field("count", count),
        ],
    );
}

pub(crate) fn summary_rows(summary: &FlushSummary) -> u64 {
    let mut rows = 0_u64;
    for section in &summary.sections {
        rows = rows.saturating_add(u64::from(section.rows));
    }
    rows
}

pub(crate) fn log_flush_summary(summary: &FlushSummary, source_id: u64, elapsed: Duration) {
    log_event(
        LogLevel::Debug,
        "window_encoded",
        &[
            field("source_id", source_id),
            field("sections", summary.sections.len()),
            field("section_rows", summary_rows(summary)),
            field("part_bytes", summary.part_bytes),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
    for section in &summary.sections {
        let [collection, type_id, layout_id] = section_fields(section.type_id);
        log_event(
            LogLevel::Debug,
            "section_encoded",
            &[
                collection,
                type_id,
                layout_id,
                field("section_rows", section.rows),
                field("encoded_bytes", section.body_bytes),
                field("part_bytes", summary.part_bytes),
            ],
        );
    }
}

pub(crate) fn log_journal_append(
    summary: &FlushSummary,
    part_offset: usize,
    part_len: usize,
    journal_bytes_before: usize,
    journal_bytes_after: usize,
    elapsed: Duration,
    retry_after_seal: bool,
) {
    log_event(
        LogLevel::Debug,
        "journal_append_finish",
        &[
            field("part_offset", part_offset),
            field("part_len", part_len),
            field("part_bytes", summary.part_bytes),
            field("sections", summary.sections.len()),
            field("section_rows", summary_rows(summary)),
            field("journal_bytes_before", journal_bytes_before),
            field("journal_bytes_after", journal_bytes_after),
            field("retry_after_seal", retry_after_seal),
            field("elapsed_ms", duration_ms(elapsed)),
        ],
    );
}

#[cfg(test)]
mod tests {
    use kronika_writer::{FlushSummary, SectionFlushSummary};

    use super::{
        CollectionFamily, LogLevel, field, parse_log_level_value, push_log_value, render_log_line,
        summary_rows,
    };

    #[test]
    fn logfmt_value_quotes_spaces_and_escapes_control_characters() {
        let mut out = String::new();
        push_log_value(&mut out, "pg_stat_bgwriter + pg_stat_checkpointer");
        assert_eq!(out, "\"pg_stat_bgwriter + pg_stat_checkpointer\"");

        out.clear();
        push_log_value(&mut out, "plain_value");
        assert_eq!(out, "plain_value");

        out.clear();
        push_log_value(&mut out, "a=\"b\"\n");
        assert_eq!(out, "\"a=\\\"b\\\"\\n\"");

        out.clear();
        push_log_value(&mut out, "\u{1b}\0");
        assert_eq!(out, "\"\\u{1b}\\u{0}\"");
    }

    #[test]
    fn render_log_line_uses_collection_family_and_structured_fields() {
        let line = render_log_line(
            LogLevel::Warn,
            "collection_skip",
            &[
                CollectionFamily::Statements.field(),
                field("source", "database"),
                field("database", "db one"),
                field("section_rows", 3_u32),
                field("error", "bad\u{1b}value"),
            ],
        );
        assert_eq!(
            line,
            "pg_kronika-collector level=warn action=collection_skip collection=pg_stat_statements source=database database=\"db one\" section_rows=3 error=\"bad\\u{1b}value\""
        );
    }

    #[test]
    fn log_level_parse_accepts_configured_levels() {
        assert_eq!(LogLevel::parse("error"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("warning"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("verbose"), None);
        assert_eq!(parse_log_level_value(" DEBUG "), Some(LogLevel::Debug));
        assert_eq!(parse_log_level_value("verbose"), None);
    }

    #[test]
    fn summary_rows_sums_section_rows_and_is_zero_when_empty() {
        let empty = FlushSummary {
            sections: Vec::new(),
            part_bytes: 0,
        };
        assert_eq!(summary_rows(&empty), 0);

        let populated = FlushSummary {
            sections: vec![
                SectionFlushSummary {
                    type_id: 1_006_001,
                    rows: 2,
                    body_bytes: 10,
                },
                SectionFlushSummary {
                    type_id: 1_021_001,
                    rows: 5,
                    body_bytes: 20,
                },
            ],
            part_bytes: 30,
        };
        assert_eq!(summary_rows(&populated), 7);
    }
}
