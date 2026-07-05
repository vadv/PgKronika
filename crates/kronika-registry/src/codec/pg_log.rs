//! `PostgreSQL` log-domain sections.
//!
//! The first log-domain layout stores grouped stderr errors and explicit
//! degradation signals from the bounded tailer. Other typed log events use
//! separate future sections.

use crate::{Section, StrId, Ts};

/// Type `1_022_001`: grouped `PostgreSQL` log errors.
///
/// One row represents one `(severity, category, pattern)` group in the
/// collection window. Text fields are nullable because log collection must keep
/// the row when the segment dictionary is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_022_001,
    name = "pg_log_errors",
    semantics = event_stream,
    sort_key("severity", "category", "pattern", "ts")
)]
pub struct PgLogErrorV1 {
    /// Log record time, unix microseconds; collection time when stderr has no parsed timestamp.
    #[column(t)]
    pub ts: Ts,
    /// Severity: `0` error, `1` fatal, `2` panic, `3` warning,
    /// `4` selected crash/OOM lifecycle log.
    #[column(l)]
    pub severity: u8,
    /// Error category: `0` lock through `10` other, matching the source taxonomy.
    #[column(l)]
    pub category: u8,
    /// SQLSTATE when present in csvlog or stderr message prefix.
    #[column(l)]
    pub sqlstate: Option<StrId>,
    /// Normalized grouping pattern.
    #[column(l)]
    pub pattern: Option<StrId>,
    /// Occurrences of this group in the collection window.
    #[column(g)]
    pub count: u32,
    /// First concrete message sample in the group.
    #[column(l)]
    pub sample: Option<StrId>,
    /// SQL from a following `STATEMENT:` line when available.
    #[column(l)]
    pub statement: Option<StrId>,
    /// Database name from csvlog; `NULL` for the first stderr scope.
    #[column(l)]
    pub database: Option<StrId>,
    /// User name from csvlog; `NULL` for the first stderr scope.
    #[column(l)]
    pub username: Option<StrId>,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}

/// Type `1_029_001`: `PostgreSQL` log-tail degradation signal.
///
/// A row means the collector may have skipped or degraded bytes while keeping
/// memory bounded. Absence of this section means the implemented log source saw
/// no tailer/parser degradation in the segment window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_029_001,
    name = "pg_log_gap",
    semantics = event_stream,
    sort_key("ts", "reason")
)]
pub struct PgLogGapV1 {
    /// Detection time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Source log path when known.
    #[column(l)]
    pub source_path: Option<StrId>,
    /// Parser kind: `0` stderr, `1` csvlog, `2` unknown.
    #[column(l)]
    pub parser_kind: u8,
    /// Reason: `0` backlog, `1` truncate, `2` invalid UTF-8, `3` binary,
    /// `4` sparse, `5` rotation, `6` missing file, `7` unsupported format,
    /// `8` source unavailable, `9` dictionary full, `10` parser drop.
    #[column(l)]
    pub reason: u8,
    /// File device id when known.
    #[column(l)]
    pub dev: Option<u64>,
    /// File inode when known.
    #[column(l)]
    pub inode: Option<u64>,
    /// Tail offset after the degraded read when known.
    #[column(g)]
    pub offset: Option<u64>,
    /// Bytes skipped by backlog or sparse-hole handling.
    #[column(g)]
    pub bytes_skipped: u64,
    /// Physical lines truncated to the bounded prefix.
    #[column(g)]
    pub truncated_lines: u32,
    /// Lines dropped because they were not valid UTF-8.
    #[column(g)]
    pub invalid_utf8: u32,
    /// Lines dropped because they contained NUL bytes.
    #[column(g)]
    pub binary_dropped: u32,
    /// Rotation/copytruncate detections that may have skipped old bytes.
    #[column(g)]
    pub rotations: u32,
    /// Missing-file observations.
    #[column(g)]
    pub missing_files: u32,
    /// Read cycles stopped by a line/byte/time budget.
    #[column(g)]
    pub budget_exhaustions: u32,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u32,
    /// Complete lines skipped by parser-level validation.
    #[column(g)]
    pub parser_dropped_lines: u32,
}

#[cfg(test)]
mod tests {
    use super::{PgLogErrorV1, PgLogGapV1};
    use crate::{Section, StrId, Ts, lint};

    #[test]
    fn contracts_pass_the_linter() {
        assert_eq!(
            lint(&[PgLogErrorV1::CONTRACT, PgLogGapV1::CONTRACT]),
            Ok(())
        );
    }

    #[test]
    fn error_contract_shape() {
        let c = PgLogErrorV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_022_001);
        assert_eq!(c.columns.len(), 11);
        assert_eq!(c.sort_key, ["severity", "category", "pattern", "ts"]);
        assert_eq!(c.column("pattern").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("count").map(|col| col.nullable), Some(false));
    }

    #[test]
    fn gap_contract_shape() {
        let c = PgLogGapV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_029_001);
        assert_eq!(c.columns.len(), 16);
        assert_eq!(c.sort_key, ["ts", "reason"]);
        assert_eq!(c.column("dev").map(|col| col.nullable), Some(true));
        assert_eq!(
            c.column("bytes_skipped").map(|col| col.nullable),
            Some(false)
        );
    }

    #[test]
    fn error_roundtrip_preserves_nullable_text() {
        crate::assert_roundtrips(&[
            PgLogErrorV1 {
                ts: Ts(10),
                severity: 0,
                category: 6,
                sqlstate: Some(StrId(1)),
                pattern: Some(StrId(2)),
                count: 3,
                sample: Some(StrId(3)),
                statement: None,
                database: None,
                username: None,
                dict_dropped_fields: 0,
            },
            PgLogErrorV1 {
                ts: Ts(11),
                severity: 1,
                category: 9,
                sqlstate: None,
                pattern: None,
                count: 1,
                sample: None,
                statement: None,
                database: None,
                username: None,
                dict_dropped_fields: 2,
            },
        ]);
    }

    #[test]
    fn gap_roundtrip_preserves_optional_file_identity() {
        crate::assert_roundtrips(&[
            PgLogGapV1 {
                ts: Ts(20),
                source_path: Some(StrId(7)),
                parser_kind: 0,
                reason: 0,
                dev: Some(10),
                inode: Some(20),
                offset: Some(30),
                bytes_skipped: 1024,
                truncated_lines: 0,
                invalid_utf8: 0,
                binary_dropped: 0,
                rotations: 0,
                missing_files: 0,
                budget_exhaustions: 1,
                dict_dropped_fields: 0,
                parser_dropped_lines: 0,
            },
            PgLogGapV1 {
                ts: Ts(21),
                source_path: None,
                parser_kind: 2,
                reason: 8,
                dev: None,
                inode: None,
                offset: None,
                bytes_skipped: 0,
                truncated_lines: 0,
                invalid_utf8: 0,
                binary_dropped: 0,
                rotations: 0,
                missing_files: 1,
                budget_exhaustions: 0,
                dict_dropped_fields: 0,
                parser_dropped_lines: 0,
            },
        ]);
    }
}
