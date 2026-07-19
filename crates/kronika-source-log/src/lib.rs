//! `PostgreSQL` log collectors.
//!
//! [`LogCollector`] discovers a PostgreSQL stderr log file, resumes from a
//! durable [`TailState`], and converts bounded records into typed source rows.
//! It never exposes raw log lines as an output contract.
//!
//! [`TailCaps`] bounds lines, bytes, elapsed read time, line length, and
//! backlog. Rotation, copy-truncate, sparse ranges, binary input, truncation,
//! budget exhaustion, and missing files are counted in [`TailGaps`] and later
//! sealed as gap rows. A batch advances durable state only after the caller
//! commits the corresponding segment window.
//!
//! The stderr parser groups normalized errors and emits typed checkpoint,
//! autovacuum, slow-query, lock-wait, lifecycle, and temporary-file events
//! under fixed per-cycle output caps. `csvlog` is recognized as unsupported;
//! it is not silently parsed with stderr rules.
#![allow(
    clippy::multiple_crate_versions,
    reason = "the workspace's PostgreSQL, nix, and arrow/parquet stacks pull duplicate transitive versions outside this crate"
)]

mod collector;
mod normalize;
mod parser;
mod state;
mod tailer;

pub use collector::{
    AutovacuumEvent, AutovacuumKind, CheckpointEvent, CheckpointPhase, DiscoveryStatus, GapReason,
    GroupedLogError, LifecycleEvent, LifecycleKind, LockWaitEvent, LockWaitKind, LogCollection,
    LogCollector, LogConfig, LogGap, SlowQueryEvent, TempFileEvent,
};
pub use normalize::ErrorCategory;
pub use parser::{LogSeverity, ParserKind};
pub use state::TailState;
pub use tailer::{TailCaps, TailGaps};

/// Type id for grouped log errors.
pub const PG_LOG_ERRORS_TYPE_ID: u32 = 1_022_001;
/// Type id for typed checkpoint log events.
pub const PG_LOG_CHECKPOINTS_TYPE_ID: u32 = 1_024_001;
/// Type id for autovacuum/autoanalyze log events.
pub const PG_LOG_AUTOVACUUM_TYPE_ID: u32 = 1_025_001;
/// Type id for slow-query top-N log events.
pub const PG_LOG_SLOW_QUERIES_TYPE_ID: u32 = 1_026_001;
/// Type id for lock-wait log events.
pub const PG_LOG_LOCK_WAITS_TYPE_ID: u32 = 1_027_001;
/// Type id for server lifecycle log events.
pub const PG_LOG_LIFECYCLE_TYPE_ID: u32 = 1_028_001;
/// Type id for log-tail degradation rows.
pub const PG_LOG_GAP_TYPE_ID: u32 = 1_029_001;
/// Type id for temporary-file log events.
pub const PG_LOG_TEMP_FILES_TYPE_ID: u32 = 1_030_001;

/// Maximum normalized error pattern length, bytes.
pub const MAX_PATTERN_BYTES: usize = 256;
/// Maximum stored text field length, bytes.
pub const MAX_TEXT_BYTES: usize = 5120;

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.get(..end).unwrap_or_default()
}

fn u32_saturating(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
