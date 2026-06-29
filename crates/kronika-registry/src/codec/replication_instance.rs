//! Type `1_015_001`: instance-level replication status.
//!
//! One row describes this instance's replication role, synchronous-replication
//! settings, current WAL write position on a primary, and WAL receiver/apply
//! position on a standby. It is stable across PG 10-18.

use crate::{Section, StrId, Ts};

/// Type `1_015_001`: one row of instance replication status.
///
/// Standby-only columns are `None` on a primary; `current_wal_lsn` is `None` on
/// a standby because `pg_current_wal_lsn()` cannot be called during recovery.
/// LSNs are stored as signed byte offsets from `0/0`; values above `i64::MAX`
/// saturate at `i64::MAX` before encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_015_001,
    name = "replication_instance",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct ReplicationInstance {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Whether the instance is in recovery (a standby).
    #[column(g)]
    pub is_in_recovery: bool,
    /// Current timeline id from the control file.
    #[column(g)]
    pub timeline_id: i32,
    /// `synchronous_standby_names` setting, interned.
    #[column(l)]
    pub synchronous_standby_names: StrId,
    /// `synchronous_commit` setting, interned.
    #[column(l)]
    pub synchronous_commit: StrId,
    /// WAL receiver status on a standby (`pg_stat_wal_receiver.status`).
    #[column(l)]
    pub wal_receiver_status: Option<StrId>,
    /// Upstream host this standby streams from; `None` when not a streaming
    /// standby. PG10 extracts this from bounded `conninfo`; PG11+ uses
    /// `pg_stat_wal_receiver.sender_host`.
    #[column(l)]
    pub sender_host: Option<StrId>,
    /// Upstream port this standby streams from.
    #[column(g)]
    pub sender_port: Option<i32>,
    /// Replication slot used by this WAL receiver.
    #[column(l)]
    pub slot_name: Option<StrId>,
    /// WAL sender rows on this instance whose state is `streaming`.
    #[column(g)]
    pub streaming_replicas: i32,
    /// Replay lag, seconds. `0` means receive and replay LSN are both known and
    /// equal; `None` means primary, missing receiver state, or no replay
    /// timestamp yet.
    #[column(g)]
    pub replay_lag_s: Option<i64>,
    /// Last received WAL LSN, signed byte offset; `None` on a primary or before
    /// streaming starts.
    #[column(g)]
    pub standby_receive_lsn: Option<i64>,
    /// Last replayed WAL LSN, signed byte offset; `None` on a primary.
    #[column(g)]
    pub standby_replay_lsn: Option<i64>,
    /// Commit timestamp of the last replayed transaction; `None` on a primary or
    /// before the first replay.
    #[column(g)]
    pub standby_last_replay_at: Option<Ts>,
    /// Current WAL write location, signed byte offset; `None` on a standby.
    #[column(g)]
    pub current_wal_lsn: Option<i64>,
    /// Last WAL location reported by this receiver to the upstream sender.
    #[column(g)]
    pub latest_end_lsn: Option<i64>,
    /// Time of `latest_end_lsn`; `None` when there is no WAL receiver row.
    #[column(g)]
    pub latest_end_time: Option<Ts>,
    /// Timeline of the latest received and flushed WAL.
    #[column(g)]
    pub received_tli: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::ReplicationInstance;
    use crate::{Section, StrId, Ts, VerifiedSection, lint};

    fn primary_row(ts: i64) -> ReplicationInstance {
        ReplicationInstance {
            ts: Ts(ts),
            is_in_recovery: false,
            timeline_id: 1,
            synchronous_standby_names: StrId(3),
            synchronous_commit: StrId(4),
            wal_receiver_status: None,
            sender_host: None,
            sender_port: None,
            slot_name: None,
            streaming_replicas: 2,
            replay_lag_s: None,
            standby_receive_lsn: None,
            standby_replay_lsn: None,
            standby_last_replay_at: None,
            current_wal_lsn: Some(123_456_789),
            latest_end_lsn: None,
            latest_end_time: None,
            received_tli: None,
        }
    }

    fn standby_row(ts: i64) -> ReplicationInstance {
        ReplicationInstance {
            is_in_recovery: true,
            wal_receiver_status: Some(StrId(8)),
            sender_host: Some(StrId(9)),
            sender_port: Some(5432),
            slot_name: Some(StrId(10)),
            streaming_replicas: 0,
            replay_lag_s: Some(2),
            standby_receive_lsn: Some(123_456_700),
            standby_replay_lsn: Some(123_456_600),
            standby_last_replay_at: Some(Ts(ts - 2_000_000)),
            current_wal_lsn: None,
            latest_end_lsn: Some(123_456_800),
            latest_end_time: Some(Ts(ts - 1_000_000)),
            received_tli: Some(2),
            ..primary_row(ts)
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[ReplicationInstance::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape_matches_the_source() {
        let c = ReplicationInstance::CONTRACT;
        assert_eq!(c.type_id.get(), 1_015_001);
        assert_eq!(c.columns.len(), 18);
        assert_eq!(c.sort_key, ["ts"]);
        assert_eq!(
            c.column("is_in_recovery").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(
            c.column("streaming_replicas").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(c.column("sender_host").map(|col| col.nullable), Some(true));
        assert_eq!(
            c.column("wal_receiver_status").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(c.column("slot_name").map(|col| col.nullable), Some(true));
        assert_eq!(
            c.column("current_wal_lsn").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(
            c.column("standby_replay_lsn").map(|col| col.nullable),
            Some(true)
        );
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        // One section may hold a primary sample and a standby sample.
        crate::assert_roundtrips(&[primary_row(1_000_000), standby_row(2_000_000)]);
    }

    #[test]
    fn nulls_survive_distinct_from_zero() {
        let bytes = ReplicationInstance::encode(&[primary_row(5)]).expect("encode");
        let decoded =
            ReplicationInstance::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].standby_receive_lsn, None);
        assert_eq!(decoded[0].replay_lag_s, None);
        assert_eq!(decoded[0].sender_host, None);
        assert_eq!(decoded[0].wal_receiver_status, None);
        assert_eq!(decoded[0].latest_end_lsn, None);
    }
}
