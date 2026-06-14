//! Type `1_020_001`: `reset_metadata`, the per-segment counter-reset context
//! (README.md, "Service Sections").
//!
//! Mandatory in every segment. This is not a chart metric; it is context for
//! counter diffs. It lets the reader separate a real counter reset
//! (`PostgreSQL` restart, `pg_stat_reset()`) from data loss, and records which
//! extensions and GUCs were present so version-dependent columns and `NULL`s
//! remain interpretable.
//!
//! A view that exposes its own `stats_reset` carries it in its own section, per
//! row, so a mid-segment reset is visible there (see `bgwriter_stats_reset` in
//! type `1_006_001`). This section holds only the cross-cutting context: the
//! global postmaster restart, reset times for views without their own section
//! yet, extension versions, and the GUCs that change how columns are read.

use crate::{Section, StrId, Ts};

/// One row of type `1_020_001`; one row per segment. A reset timestamp is `None`
/// when the source view, field, or extension is unavailable in this instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_020_001,
    name = "reset_metadata",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct ResetMetadata {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Postmaster start time; a change means `PostgreSQL` restarted, so
    /// cumulative counters restarted with it.
    #[column(g)]
    pub postmaster_start_time: Ts,
    /// Max `stats_reset` across `pg_stat_database`; a coarse database-level
    /// reset marker.
    #[column(g)]
    pub pg_stat_database_reset_max_at: Ts,
    /// `pg_stat_statements` reset; `None` if the extension or its info view is
    /// absent.
    #[column(g)]
    pub pg_stat_statements_reset_at: Option<Ts>,
    /// `pg_store_plans` reset; `None` if unsupported.
    #[column(g)]
    pub pg_store_plans_reset_at: Option<Ts>,
    /// `pg_stat_wal` reset; `None` before `pg_stat_wal` existed.
    #[column(g)]
    pub pg_stat_wal_reset_at: Option<Ts>,
    /// `pg_stat_archiver` reset.
    #[column(g)]
    pub pg_stat_archiver_reset_at: Ts,
    /// `pg_stat_io` reset; `None` before PG16.
    #[column(g)]
    pub pg_stat_io_reset_at: Option<Ts>,
    /// `pg_stat_statements` extension version; `None` if not installed.
    #[column(l)]
    pub ext_pg_stat_statements_version: Option<StrId>,
    /// `pg_store_plans` extension version; `None` if not installed.
    #[column(l)]
    pub ext_pg_store_plans_version: Option<StrId>,
    /// `compute_query_id` GUC value; if `off`, `query_id` is not a reliable key.
    #[column(l)]
    pub compute_query_id: Option<StrId>,
    /// `track_io_timing` GUC; if `false`, `blk_*_time` stay zero and do not mean
    /// fast IO; `None` if the GUC is unavailable.
    #[column(l)]
    pub track_io_timing: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::ResetMetadata;
    use crate::{Section, StrId, Ts, lint};

    fn pg17_row() -> ResetMetadata {
        ResetMetadata {
            ts: Ts(2_000_000),
            postmaster_start_time: Ts(1_700_000_000_000_000),
            pg_stat_database_reset_max_at: Ts(1_700_000_500_000_000),
            pg_stat_statements_reset_at: Some(Ts(1_700_000_400_000_000)),
            pg_store_plans_reset_at: None,
            pg_stat_wal_reset_at: Some(Ts(1_700_000_200_000_000)),
            pg_stat_archiver_reset_at: Ts(1_700_000_100_000_000),
            pg_stat_io_reset_at: Some(Ts(1_700_000_600_000_000)),
            ext_pg_stat_statements_version: Some(StrId(10)),
            ext_pg_store_plans_version: None,
            compute_query_id: Some(StrId(11)),
            track_io_timing: Some(true),
        }
    }

    fn pg15_row() -> ResetMetadata {
        ResetMetadata {
            // Pre-PG16 and no extensions: io reset absent, ext versions and GUCs
            // unknown.
            pg_stat_io_reset_at: None,
            ext_pg_stat_statements_version: None,
            compute_query_id: None,
            track_io_timing: None,
            ..pg17_row()
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[ResetMetadata::CONTRACT]), Ok(()));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        crate::assert_roundtrips(&[pg17_row(), pg15_row()]);
    }
}
