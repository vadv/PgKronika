//! Type `1_020_001`: reset and environment context for a segment.

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
    /// Postmaster start time. A change marks a `PostgreSQL` restart, not a stats
    /// reset by itself: cumulative stats survive a clean shutdown.
    #[column(g)]
    pub postmaster_start_time: Ts,
    /// Max `stats_reset` across `pg_stat_database`; a coarse database-level
    /// reset marker. `None` until any database-level reset happened: on
    /// PG15+ a fresh cluster reports `NULL` for every database.
    #[column(g)]
    pub pg_stat_database_reset_max_at: Option<Ts>,
    /// `pg_stat_statements` reset; `None` if the extension or its info view is
    /// absent.
    #[column(g)]
    pub pg_stat_statements_reset_at: Option<Ts>,
    /// `pg_store_plans` reset; `None` if unsupported.
    #[column(g)]
    pub pg_store_plans_reset_at: Option<Ts>,
    /// `pg_stat_bgwriter` reset; `None` if the server returns no reset time.
    #[column(g)]
    pub pg_stat_bgwriter_reset_at: Option<Ts>,
    /// `pg_stat_checkpointer` reset; `None` before PG17.
    #[column(g)]
    pub pg_stat_checkpointer_reset_at: Option<Ts>,
    /// `pg_stat_wal` reset; `None` before `pg_stat_wal` existed.
    #[column(g)]
    pub pg_stat_wal_reset_at: Option<Ts>,
    /// `pg_stat_archiver` reset; `None` if the server returns no reset time.
    #[column(g)]
    pub pg_stat_archiver_reset_at: Option<Ts>,
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
    /// `track_io_timing` in the collector session. Interpreting aggregate
    /// timings assumes contributing sessions use the same setting.
    #[column(l)]
    pub track_io_timing: Option<bool>,
    /// `track_wal_io_timing` in the collector session, under the same uniform
    /// session-setting assumption as `track_io_timing`.
    #[column(l)]
    pub track_wal_io_timing: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::ResetMetadata;
    use crate::{Section, StrId, Ts, lint};

    fn pg17_row() -> ResetMetadata {
        ResetMetadata {
            ts: Ts(2_000_000),
            postmaster_start_time: Ts(1_700_000_000_000_000),
            pg_stat_database_reset_max_at: Some(Ts(1_700_000_500_000_000)),
            pg_stat_statements_reset_at: Some(Ts(1_700_000_400_000_000)),
            pg_store_plans_reset_at: None,
            pg_stat_bgwriter_reset_at: Some(Ts(1_700_000_300_000_000)),
            pg_stat_checkpointer_reset_at: Some(Ts(1_700_000_300_000_000)),
            pg_stat_wal_reset_at: Some(Ts(1_700_000_200_000_000)),
            pg_stat_archiver_reset_at: Some(Ts(1_700_000_100_000_000)),
            pg_stat_io_reset_at: Some(Ts(1_700_000_600_000_000)),
            ext_pg_stat_statements_version: Some(StrId(10)),
            ext_pg_store_plans_version: None,
            compute_query_id: Some(StrId(11)),
            track_io_timing: Some(true),
            track_wal_io_timing: Some(false),
        }
    }

    fn pg15_row() -> ResetMetadata {
        ResetMetadata {
            // Keep sort order defined.
            ts: Ts(1_000_000),
            // A fresh cluster: no database-level reset yet, pre-PG16 views,
            // no extensions, no checkpointer view.
            pg_stat_database_reset_max_at: None,
            pg_stat_checkpointer_reset_at: None,
            pg_stat_archiver_reset_at: None,
            pg_stat_io_reset_at: None,
            ext_pg_stat_statements_version: None,
            compute_query_id: None,
            track_io_timing: None,
            track_wal_io_timing: None,
            ..pg17_row()
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[ResetMetadata::CONTRACT]), Ok(()));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        // Compare in the order returned by `encode`'s sort key.
        crate::assert_roundtrips(&[pg15_row(), pg17_row()]);
    }
}
