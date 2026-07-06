//! `PostgreSQL` log-domain sections.
//!
//! The log-domain layout stores grouped stderr errors, selected typed stderr
//! events, and explicit degradation signals from the bounded tailer.

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
    /// `DETAIL:` continuation payload when `PostgreSQL` emitted one.
    #[column(l)]
    pub detail: Option<StrId>,
    /// `HINT:` continuation payload when `PostgreSQL` emitted one.
    #[column(l)]
    pub hint: Option<StrId>,
    /// `CONTEXT:` continuation payload when `PostgreSQL` emitted one.
    #[column(l)]
    pub context: Option<StrId>,
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

/// Type `1_024_001`: typed checkpoint LOG events.
///
/// One row represents one checkpoint LOG record in the collection window.
/// Nullable numeric fields mean the field is not present in that checkpoint
/// message shape, not that `PostgreSQL` reported zero.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_024_001,
    name = "pg_log_checkpoints",
    semantics = event_stream,
    sort_key("ts", "phase")
)]
pub struct PgLogCheckpointV1 {
    /// Log record time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Phase: `0` starting, `1` complete, `2` too frequent.
    #[column(l)]
    pub phase: u8,
    /// Starting reason or warning text.
    #[column(l)]
    pub reason: Option<StrId>,
    /// Too-frequent checkpoint interval, seconds.
    #[column(g)]
    pub seconds_apart: Option<i64>,
    /// Buffers written by checkpoint.
    #[column(g)]
    pub buffers_written: Option<i64>,
    /// Write phase time, ms.
    #[column(g)]
    pub write_ms: Option<f64>,
    /// Sync phase time, ms.
    #[column(g)]
    pub sync_ms: Option<f64>,
    /// Total checkpoint time, ms.
    #[column(g)]
    pub total_ms: Option<f64>,
    /// WAL distance, kB.
    #[column(g)]
    pub distance_kb: Option<i64>,
    /// Estimated WAL distance, kB.
    #[column(g)]
    pub estimate_kb: Option<i64>,
    /// WAL files added.
    #[column(g)]
    pub wal_added: Option<i64>,
    /// WAL files removed.
    #[column(g)]
    pub wal_removed: Option<i64>,
    /// WAL files recycled.
    #[column(g)]
    pub wal_recycled: Option<i64>,
    /// Files synced.
    #[column(g)]
    pub sync_files: Option<i64>,
    /// Longest individual file sync, ms.
    #[column(g)]
    pub longest_sync_ms: Option<f64>,
    /// Average file sync, ms.
    #[column(g)]
    pub average_sync_ms: Option<f64>,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}

/// Type `1_025_001`: autovacuum and autoanalyze LOG events.
///
/// One row represents one bounded stderr autovacuum/autoanalyze report. Numeric
/// fields stay nullable because `PostgreSQL` emits different shapes for vacuum
/// and analyze records, and older releases may omit selected metrics.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_025_001,
    name = "pg_log_autovacuum",
    semantics = event_stream,
    sort_key("ts", "kind", "relation")
)]
pub struct PgLogAutovacuumV1 {
    /// Log record time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Kind: `0` vacuum, `1` analyze.
    #[column(l)]
    pub kind: u8,
    /// Qualified relation name reported by `PostgreSQL`.
    #[column(l)]
    pub relation: Option<StrId>,
    /// Index scans from a vacuum report.
    #[column(g)]
    pub index_scans: Option<i64>,
    /// Heap pages removed by vacuum.
    #[column(g)]
    pub pages_removed: Option<i64>,
    /// Heap pages remaining after vacuum.
    #[column(g)]
    pub pages_remaining: Option<i64>,
    /// Tuples removed by vacuum.
    #[column(g)]
    pub tuples_removed: Option<i64>,
    /// Tuples remaining after vacuum.
    #[column(g)]
    pub tuples_remaining: Option<i64>,
    /// Dead tuples not yet removable.
    #[column(g)]
    pub tuples_dead_not_removable: Option<i64>,
    /// Elapsed runtime, ms.
    #[column(g)]
    pub elapsed_ms: Option<f64>,
    /// Buffer cache hits.
    #[column(g)]
    pub buffer_hits: Option<i64>,
    /// Buffer cache misses.
    #[column(g)]
    pub buffer_misses: Option<i64>,
    /// Buffers dirtied.
    #[column(g)]
    pub buffer_dirtied: Option<i64>,
    /// Average read rate, MB/s.
    #[column(g)]
    pub avg_read_rate_mbs: Option<f64>,
    /// Average write rate, MB/s.
    #[column(g)]
    pub avg_write_rate_mbs: Option<f64>,
    /// User CPU time, ms.
    #[column(g)]
    pub cpu_user_ms: Option<f64>,
    /// System CPU time, ms.
    #[column(g)]
    pub cpu_system_ms: Option<f64>,
    /// WAL records generated.
    #[column(g)]
    pub wal_records: Option<i64>,
    /// WAL full-page images generated.
    #[column(g)]
    pub wal_fpi: Option<i64>,
    /// WAL bytes generated.
    #[column(g)]
    pub wal_bytes: Option<i64>,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}

/// Type `1_026_001`: slow-query LOG top-N.
///
/// One row represents a normalized SQL pattern from `duration: ... statement:`
/// stderr LOG records in the collection window. The collector keeps only the
/// bounded top-N by max duration and reports `parser_drop` when more patterns
/// were read.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_026_001,
    name = "pg_log_slow_queries",
    semantics = event_stream,
    sort_key("pattern", "ts")
)]
pub struct PgLogSlowQueryV1 {
    /// Timestamp of the max-duration sample, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Normalized SQL pattern.
    #[column(l)]
    pub pattern: Option<StrId>,
    /// Bounded SQL sample for the max-duration occurrence.
    #[column(l)]
    pub sample: Option<StrId>,
    /// Occurrences of this pattern in the collection window.
    #[column(g)]
    pub count: u32,
    /// Largest duration for the pattern, ms.
    #[column(g)]
    pub max_duration_ms: f64,
    /// Sum of durations for this pattern, ms.
    #[column(g)]
    pub total_duration_ms: f64,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}

/// Type `1_027_001`: lock-wait LOG events.
///
/// One row represents a `log_lock_waits` message. `waiting` and `acquired`
/// records are separate rows because `PostgreSQL` emits them as separate LOG
/// lines and may attach different continuation fields.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_027_001,
    name = "pg_log_lock_waits",
    semantics = event_stream,
    sort_key("ts", "kind", "pid")
)]
pub struct PgLogLockWaitV1 {
    /// Log record time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Kind: `0` still waiting, `1` acquired.
    #[column(l)]
    pub kind: u8,
    /// Waiting backend process id.
    #[column(l)]
    pub pid: Option<i32>,
    /// Lock mode, e.g. `ShareLock`.
    #[column(l)]
    pub lock_mode: Option<StrId>,
    /// Lock target, e.g. `transaction 12345`.
    #[column(l)]
    pub lock_target: Option<StrId>,
    /// Wait duration reported by `PostgreSQL`, ms.
    #[column(g)]
    pub duration_ms: Option<f64>,
    /// `DETAIL:` continuation payload.
    #[column(l)]
    pub detail: Option<StrId>,
    /// `CONTEXT:` continuation payload.
    #[column(l)]
    pub context: Option<StrId>,
    /// SQL from a following `STATEMENT:` line.
    #[column(l)]
    pub statement: Option<StrId>,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}

/// Type `1_028_001`: server lifecycle LOG events.
///
/// Crash/OOM lifecycle messages are also retained in `pg_log_errors` for the
/// compatibility error timeline; shutdown and ready messages live only here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_028_001,
    name = "pg_log_lifecycle",
    semantics = event_stream,
    sort_key("ts", "kind")
)]
pub struct PgLogLifecycleV1 {
    /// Log record time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Kind: `0` crash, `1` shutdown, `2` ready.
    #[column(l)]
    pub kind: u8,
    /// Crashed process id when present.
    #[column(l)]
    pub pid: Option<i32>,
    /// Crash signal number when present.
    #[column(l)]
    pub signal: Option<i32>,
    /// Shutdown mode: `fast`, `smart`, or `immediate`.
    #[column(l)]
    pub shutdown_mode: Option<StrId>,
    /// Bounded lifecycle message.
    #[column(l)]
    pub message: Option<StrId>,
    /// SQL extracted from a following crash `DETAIL:` line.
    #[column(l)]
    pub query_detail: Option<StrId>,
    /// Text fields dropped because dictionary interning failed.
    #[column(g)]
    pub dict_dropped_fields: u8,
}

/// Type `1_030_001`: temporary-file LOG events.
///
/// One row represents a `log_temp_files` record. The file path and attached
/// statement are bounded dictionary strings; `size_bytes` is required because
/// `PostgreSQL` reports it in the LOG line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_030_001,
    name = "pg_log_temp_files",
    semantics = event_stream,
    sort_key("ts", "size_bytes")
)]
pub struct PgLogTempFileV1 {
    /// Log record time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Temporary file path.
    #[column(l)]
    pub path: Option<StrId>,
    /// Temporary file size, bytes.
    #[column(g)]
    pub size_bytes: i64,
    /// SQL from a following `STATEMENT:` line.
    #[column(l)]
    pub statement: Option<StrId>,
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
    /// `8` source unavailable, `9` dictionary full, `10` parser drop,
    /// `11` budget exhausted, `12` disabled, `13` query failed,
    /// `14` permission denied, `15` timestamp fallback.
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
    use super::{
        PgLogAutovacuumV1, PgLogCheckpointV1, PgLogErrorV1, PgLogGapV1, PgLogLifecycleV1,
        PgLogLockWaitV1, PgLogSlowQueryV1, PgLogTempFileV1,
    };
    use crate::{Section, StrId, Ts, lint};

    #[test]
    fn contracts_pass_the_linter() {
        assert_eq!(
            lint(&[
                PgLogErrorV1::CONTRACT,
                PgLogCheckpointV1::CONTRACT,
                PgLogAutovacuumV1::CONTRACT,
                PgLogSlowQueryV1::CONTRACT,
                PgLogLockWaitV1::CONTRACT,
                PgLogLifecycleV1::CONTRACT,
                PgLogGapV1::CONTRACT,
                PgLogTempFileV1::CONTRACT,
            ]),
            Ok(())
        );
    }

    #[test]
    fn error_contract_shape() {
        let c = PgLogErrorV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_022_001);
        assert_eq!(c.columns.len(), 14);
        assert_eq!(c.sort_key, ["severity", "category", "pattern", "ts"]);
        assert_eq!(c.column("pattern").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("count").map(|col| col.nullable), Some(false));
    }

    #[test]
    fn checkpoint_contract_shape() {
        let c = PgLogCheckpointV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_024_001);
        assert_eq!(c.columns.len(), 17);
        assert_eq!(c.sort_key, ["ts", "phase"]);
        assert_eq!(c.column("reason").map(|col| col.nullable), Some(true));
        assert_eq!(
            c.column("buffers_written").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(
            c.column("dict_dropped_fields").map(|col| col.nullable),
            Some(false)
        );
    }

    #[test]
    fn autovacuum_contract_shape() {
        let c = PgLogAutovacuumV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_025_001);
        assert_eq!(c.columns.len(), 21);
        assert_eq!(c.sort_key, ["ts", "kind", "relation"]);
        assert_eq!(c.column("relation").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("kind").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("elapsed_ms").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn slow_query_contract_shape() {
        let c = PgLogSlowQueryV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_026_001);
        assert_eq!(c.columns.len(), 7);
        assert_eq!(c.sort_key, ["pattern", "ts"]);
        assert_eq!(c.column("pattern").map(|col| col.nullable), Some(true));
        assert_eq!(
            c.column("max_duration_ms").map(|col| col.nullable),
            Some(false)
        );
    }

    #[test]
    fn lock_wait_contract_shape() {
        let c = PgLogLockWaitV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_027_001);
        assert_eq!(c.columns.len(), 10);
        assert_eq!(c.sort_key, ["ts", "kind", "pid"]);
        assert_eq!(c.column("lock_mode").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("duration_ms").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn lifecycle_contract_shape() {
        let c = PgLogLifecycleV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_028_001);
        assert_eq!(c.columns.len(), 8);
        assert_eq!(c.sort_key, ["ts", "kind"]);
        assert_eq!(c.column("pid").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("message").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn temp_file_contract_shape() {
        let c = PgLogTempFileV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_030_001);
        assert_eq!(c.columns.len(), 5);
        assert_eq!(c.sort_key, ["ts", "size_bytes"]);
        assert_eq!(c.column("path").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("size_bytes").map(|col| col.nullable), Some(false));
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
                detail: Some(StrId(4)),
                hint: Some(StrId(5)),
                context: Some(StrId(6)),
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
                detail: None,
                hint: None,
                context: None,
                statement: None,
                database: None,
                username: None,
                dict_dropped_fields: 2,
            },
        ]);
    }

    #[test]
    fn checkpoint_roundtrip_preserves_nullable_metrics() {
        crate::assert_roundtrips(&[
            PgLogCheckpointV1 {
                ts: Ts(30),
                phase: 0,
                reason: Some(StrId(10)),
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
                dict_dropped_fields: 0,
            },
            PgLogCheckpointV1 {
                ts: Ts(31),
                phase: 1,
                reason: None,
                seconds_apart: None,
                buffers_written: Some(123),
                write_ms: Some(1200.0),
                sync_ms: Some(300.0),
                total_ms: Some(1800.0),
                distance_kb: Some(4096),
                estimate_kb: Some(8192),
                wal_added: Some(0),
                wal_removed: Some(1),
                wal_recycled: Some(2),
                sync_files: Some(5),
                longest_sync_ms: Some(40.0),
                average_sync_ms: Some(8.0),
                dict_dropped_fields: 0,
            },
        ]);
    }

    #[test]
    fn autovacuum_roundtrip_preserves_nullable_metrics() {
        crate::assert_roundtrips(&[
            PgLogAutovacuumV1 {
                ts: Ts(35),
                kind: 0,
                relation: Some(StrId(12)),
                index_scans: Some(1),
                pages_removed: Some(10),
                pages_remaining: Some(20),
                tuples_removed: Some(30),
                tuples_remaining: Some(40),
                tuples_dead_not_removable: Some(5),
                elapsed_ms: Some(5670.0),
                buffer_hits: Some(100),
                buffer_misses: Some(2),
                buffer_dirtied: Some(3),
                avg_read_rate_mbs: Some(1.5),
                avg_write_rate_mbs: Some(2.5),
                cpu_user_ms: Some(120.0),
                cpu_system_ms: Some(340.0),
                wal_records: Some(15),
                wal_fpi: Some(2),
                wal_bytes: Some(4096),
                dict_dropped_fields: 0,
            },
            PgLogAutovacuumV1 {
                ts: Ts(36),
                kind: 1,
                relation: None,
                index_scans: None,
                pages_removed: None,
                pages_remaining: None,
                tuples_removed: None,
                tuples_remaining: None,
                tuples_dead_not_removable: None,
                elapsed_ms: Some(10.0),
                buffer_hits: None,
                buffer_misses: None,
                buffer_dirtied: None,
                avg_read_rate_mbs: None,
                avg_write_rate_mbs: None,
                cpu_user_ms: None,
                cpu_system_ms: None,
                wal_records: None,
                wal_fpi: None,
                wal_bytes: None,
                dict_dropped_fields: 1,
            },
        ]);
    }

    #[test]
    fn slow_query_roundtrip_preserves_topn_metrics() {
        crate::assert_roundtrips(&[PgLogSlowQueryV1 {
            ts: Ts(40),
            pattern: Some(StrId(20)),
            sample: Some(StrId(21)),
            count: 3,
            max_duration_ms: 1234.5,
            total_duration_ms: 2000.0,
            dict_dropped_fields: 0,
        }]);
    }

    #[test]
    fn lock_wait_roundtrip_preserves_continuations() {
        crate::assert_roundtrips(&[PgLogLockWaitV1 {
            ts: Ts(45),
            kind: 0,
            pid: Some(70),
            lock_mode: Some(StrId(22)),
            lock_target: Some(StrId(23)),
            duration_ms: Some(30009.004),
            detail: Some(StrId(24)),
            context: None,
            statement: Some(StrId(25)),
            dict_dropped_fields: 0,
        }]);
    }

    #[test]
    fn lifecycle_roundtrip_preserves_optional_crash_fields() {
        crate::assert_roundtrips(&[
            PgLogLifecycleV1 {
                ts: Ts(50),
                kind: 0,
                pid: Some(4242),
                signal: Some(9),
                shutdown_mode: None,
                message: Some(StrId(30)),
                query_detail: Some(StrId(31)),
                dict_dropped_fields: 0,
            },
            PgLogLifecycleV1 {
                ts: Ts(51),
                kind: 1,
                pid: None,
                signal: None,
                shutdown_mode: Some(StrId(32)),
                message: Some(StrId(33)),
                query_detail: None,
                dict_dropped_fields: 0,
            },
        ]);
    }

    #[test]
    fn temp_file_roundtrip_preserves_statement() {
        crate::assert_roundtrips(&[PgLogTempFileV1 {
            ts: Ts(60),
            path: Some(StrId(40)),
            size_bytes: 200_204_288,
            statement: Some(StrId(41)),
            dict_dropped_fields: 0,
        }]);
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
