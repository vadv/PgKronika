//! Type `1_016_001`: `pg_stat_replication`, one row per connected walsender.
//!
//! Read on the primary; a standby writes no rows. LSN columns are absolute
//! byte offsets from `0/0`, saturated to `i64::MAX`, and are `None` for a
//! walsender that reports no position yet — a `pg_basebackup` connection
//! shows `state = backup` with NULL positions. Lag columns are `None` when
//! the standby reports no reply (e.g. `wal_receiver_status_interval = 0`).

use crate::{Section, StrId, Ts};

/// One row of type `1_016_001`; one `pg_stat_replication` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_016_001,
    name = "pg_stat_replication",
    semantics = snapshot_full,
    sort_key("application_name", "pid", "ts")
)]
pub struct ReplicationReplicasV1 {
    /// Collection time, unix microseconds; one value for all rows of a read.
    #[column(t)]
    pub ts: Ts,
    /// Walsender backend pid.
    #[column(l)]
    pub pid: i32,
    /// Role the walsender authenticated as; empty when unresolved.
    #[column(l)]
    pub usename: StrId,
    /// `application_name` of the standby; empty when unset.
    #[column(l)]
    pub application_name: StrId,
    /// Standby client address; `None` for a unix-socket connection.
    #[column(l)]
    pub client_addr: Option<StrId>,
    /// Walsender state (`streaming`, `catchup`, `backup`, …).
    #[column(l)]
    pub state: StrId,
    /// Synchronous state (`async`, `sync`, `potential`, `quorum`).
    #[column(l)]
    pub sync_state: StrId,
    /// Priority of this standby for synchronous replication.
    #[column(g)]
    pub sync_priority: Option<i32>,
    /// Last WAL position sent, byte offset from `0/0`.
    #[column(g)]
    pub sent_lsn: Option<i64>,
    /// Last WAL position written by the standby.
    #[column(g)]
    pub write_lsn: Option<i64>,
    /// Last WAL position flushed by the standby.
    #[column(g)]
    pub flush_lsn: Option<i64>,
    /// Last WAL position replayed by the standby.
    #[column(g)]
    pub replay_lsn: Option<i64>,
    /// Write lag, microseconds.
    #[column(g)]
    pub write_lag_us: Option<i64>,
    /// Flush lag, microseconds.
    #[column(g)]
    pub flush_lag_us: Option<i64>,
    /// Replay lag, microseconds.
    #[column(g)]
    pub replay_lag_us: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::ReplicationReplicasV1;
    use crate::{Section, StrId, Ts, lint};

    fn streaming_row() -> ReplicationReplicasV1 {
        ReplicationReplicasV1 {
            ts: Ts(1_000_000),
            pid: 4242,
            usename: StrId(1),
            application_name: StrId(2),
            client_addr: Some(StrId(3)),
            state: StrId(4),
            sync_state: StrId(5),
            sync_priority: Some(0),
            sent_lsn: Some(16_777_216),
            write_lsn: Some(16_777_000),
            flush_lsn: Some(16_776_000),
            replay_lsn: Some(16_775_000),
            write_lag_us: Some(1_500),
            flush_lag_us: Some(2_500),
            replay_lag_us: Some(9_000),
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[ReplicationReplicasV1::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = ReplicationReplicasV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_016_001);
        assert_eq!(c.columns.len(), 15);
        assert_eq!(c.sort_key, ["application_name", "pid", "ts"]);
        assert_eq!(c.column("pid").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("sent_lsn").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("client_addr").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        // A pg_basebackup walsender: no positions, no lag, no address.
        let backup_row = ReplicationReplicasV1 {
            ts: Ts(2_000_000),
            pid: 4243,
            client_addr: None,
            sync_priority: None,
            sent_lsn: None,
            write_lsn: None,
            flush_lsn: None,
            replay_lsn: None,
            write_lag_us: None,
            flush_lag_us: None,
            replay_lag_us: None,
            ..streaming_row()
        };
        crate::assert_roundtrips(&[streaming_row(), backup_row]);
    }
}
