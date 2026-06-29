//! Type `1_015_001`: instance-level replication status.
//!
//! One row describing where this instance sits in replication: whether it is in
//! recovery, its timeline, the synchronous-replication settings, the upstream
//! host on a standby, how many replicas are connected, and the standby- or
//! primary-only LSNs. Stable across PG 10-18.

use crate::{Section, StrId, Ts};

/// Type `1_015_001`: one row of instance replication status.
///
/// The standby-only columns (`replay_lag_s`, `standby_receive_lsn`,
/// `standby_replay_lsn`, `standby_last_replay_at`, `sender_host`) are `None` on a
/// primary; `current_wal_lsn` is `None` on a standby (it cannot be read during
/// recovery). LSNs are stored as byte offsets from `0/0`.
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
    /// Upstream host this standby streams from (`pg_stat_wal_receiver`); `None`
    /// when not a streaming standby. Interned.
    #[column(l)]
    pub sender_host: Option<StrId>,
    /// Streaming replicas connected to this instance (`pg_stat_replication`).
    #[column(g)]
    pub connected_replicas: i32,
    /// Replay lag, seconds; `0` once replay caught up to the received LSN.
    /// `None` on a primary, or on a standby that has received WAL but not
    /// replayed yet (no replay timestamp to measure against).
    #[column(g)]
    pub replay_lag_s: Option<i64>,
    /// Last received WAL LSN, byte offset; `None` on a primary.
    #[column(g)]
    pub standby_receive_lsn: Option<i64>,
    /// Last replayed WAL LSN, byte offset; `None` on a primary.
    #[column(g)]
    pub standby_replay_lsn: Option<i64>,
    /// Commit timestamp of the last replayed transaction; `None` on a primary or
    /// before the first replay.
    #[column(g)]
    pub standby_last_replay_at: Option<Ts>,
    /// Current WAL insert LSN, byte offset; `None` on a standby (unreadable
    /// during recovery).
    #[column(g)]
    pub current_wal_lsn: Option<i64>,
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
            sender_host: None,
            connected_replicas: 2,
            replay_lag_s: None,
            standby_receive_lsn: None,
            standby_replay_lsn: None,
            standby_last_replay_at: None,
            current_wal_lsn: Some(123_456_789),
        }
    }

    fn standby_row(ts: i64) -> ReplicationInstance {
        ReplicationInstance {
            is_in_recovery: true,
            sender_host: Some(StrId(9)),
            connected_replicas: 0,
            replay_lag_s: Some(2),
            standby_receive_lsn: Some(123_456_700),
            standby_replay_lsn: Some(123_456_600),
            standby_last_replay_at: Some(Ts(ts - 2_000_000)),
            current_wal_lsn: None,
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
        assert_eq!(c.columns.len(), 12);
        assert_eq!(c.sort_key, ["ts"]);
        assert_eq!(
            c.column("is_in_recovery").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(
            c.column("connected_replicas").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(c.column("sender_host").map(|col| col.nullable), Some(true));
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
        // A primary keeps standby LSNs and sender_host NULL, not Some(0)/Some("").
        let bytes = ReplicationInstance::encode(&[primary_row(5)]).expect("encode");
        let decoded =
            ReplicationInstance::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].standby_receive_lsn, None);
        assert_eq!(decoded[0].replay_lag_s, None);
        assert_eq!(decoded[0].sender_host, None);
    }
}
