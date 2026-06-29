//! Instance replication status collection for type `1_015_001`.
//!
//! One row describing this instance's place in replication. The same shape
//! serves a primary and a standby: `pg_current_wal_lsn()` is read only off
//! recovery (it errors during recovery), and the standby LSNs, lag, and upstream
//! host only during recovery. `sender_host` lives in `pg_stat_wal_receiver` as a
//! dedicated column from PG11; PG10 has only `conninfo`. Collection returns an
//! owned row; the caller interns the settings and host strings.

use kronika_registry::Ts;
use kronika_registry::replication_instance::ReplicationInstance;
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/replication_instance.rs */ ",
            $sql,
        )
    };
}

/// One raw instance-replication row; the settings and host strings are owned and
/// interned by the caller.
#[derive(Debug, Clone)]
pub struct ReplicationInstanceRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Whether the instance is in recovery (a standby).
    pub is_in_recovery: bool,
    /// Control-file timeline id.
    pub timeline_id: i32,
    /// `synchronous_standby_names` setting.
    pub synchronous_standby_names: String,
    /// `synchronous_commit` setting.
    pub synchronous_commit: String,
    /// Upstream host this standby streams from; `None` off a streaming standby.
    pub sender_host: Option<String>,
    /// Streaming replicas connected to this instance.
    pub connected_replicas: i32,
    /// Replay lag, seconds (standby only).
    pub replay_lag_s: Option<i64>,
    /// Last received WAL LSN, byte offset (standby only).
    pub standby_receive_lsn: Option<i64>,
    /// Last replayed WAL LSN, byte offset (standby only).
    pub standby_replay_lsn: Option<i64>,
    /// Last replayed transaction commit time, unix microseconds (standby only).
    pub standby_last_replay_at: Option<i64>,
    /// Current WAL insert LSN, byte offset (primary only).
    pub current_wal_lsn: Option<i64>,
}

/// Build a `1_015_001` row, interning the settings and (when present) the host.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_replication_instance<E>(
    row: &ReplicationInstanceRow,
    mut intern: impl FnMut(&[u8]) -> Result<kronika_registry::StrId, E>,
) -> Result<ReplicationInstance, E> {
    Ok(ReplicationInstance {
        ts: Ts(row.ts),
        is_in_recovery: row.is_in_recovery,
        timeline_id: row.timeline_id,
        synchronous_standby_names: intern(row.synchronous_standby_names.as_bytes())?,
        synchronous_commit: intern(row.synchronous_commit.as_bytes())?,
        sender_host: row
            .sender_host
            .as_deref()
            .map(|host| intern(host.as_bytes()))
            .transpose()?,
        connected_replicas: row.connected_replicas,
        replay_lag_s: row.replay_lag_s,
        standby_receive_lsn: row.standby_receive_lsn,
        standby_replay_lsn: row.standby_replay_lsn,
        standby_last_replay_at: row.standby_last_replay_at.map(Ts),
        current_wal_lsn: row.current_wal_lsn,
    })
}

/// The SQL for the server's major version.
///
/// `pg_stat_wal_receiver.sender_host` exists from PG11; PG10 exposes only the
/// full `conninfo`, used as the upstream-host fallback. Everything else is
/// version-stable.
#[must_use]
pub const fn replication_query(major: u32) -> &'static str {
    if major >= 11 {
        marked!(
            "SELECT pg_is_in_recovery() AS is_in_recovery, \
             (pg_control_checkpoint()).timeline_id AS timeline_id, \
             COALESCE(current_setting('synchronous_standby_names', true), '') \
             AS synchronous_standby_names, \
             COALESCE(current_setting('synchronous_commit', true), '') AS synchronous_commit, \
             (SELECT sender_host FROM pg_stat_wal_receiver LIMIT 1) AS sender_host, \
             (SELECT count(*) FROM pg_stat_replication)::int4 AS connected_replicas, \
             CASE WHEN pg_is_in_recovery() THEN CASE \
             WHEN pg_last_wal_receive_lsn() IS NOT DISTINCT FROM pg_last_wal_replay_lsn() \
             THEN 0::int8 \
             WHEN pg_last_xact_replay_timestamp() IS NULL THEN NULL::int8 \
             ELSE GREATEST(0, extract(epoch FROM \
             (statement_timestamp() - pg_last_xact_replay_timestamp())))::int8 END END \
             AS replay_lag_s, \
             CASE WHEN pg_is_in_recovery() \
             THEN pg_wal_lsn_diff(pg_last_wal_receive_lsn(), '0/0')::int8 END \
             AS standby_receive_lsn, \
             CASE WHEN pg_is_in_recovery() \
             THEN pg_wal_lsn_diff(pg_last_wal_replay_lsn(), '0/0')::int8 END \
             AS standby_replay_lsn, \
             (extract(epoch FROM \
             (CASE WHEN pg_is_in_recovery() THEN pg_last_xact_replay_timestamp() END)) \
             * 1e6)::int8 AS standby_last_replay_at_us, \
             CASE WHEN NOT pg_is_in_recovery() \
             THEN pg_wal_lsn_diff(pg_current_wal_lsn(), '0/0')::int8 END AS current_wal_lsn, \
             (extract(epoch FROM statement_timestamp()) * 1e6)::int8 AS ts_us"
        )
    } else {
        marked!(
            "SELECT pg_is_in_recovery() AS is_in_recovery, \
             (pg_control_checkpoint()).timeline_id AS timeline_id, \
             COALESCE(current_setting('synchronous_standby_names', true), '') \
             AS synchronous_standby_names, \
             COALESCE(current_setting('synchronous_commit', true), '') AS synchronous_commit, \
             (SELECT conninfo FROM pg_stat_wal_receiver LIMIT 1) AS sender_host, \
             (SELECT count(*) FROM pg_stat_replication)::int4 AS connected_replicas, \
             CASE WHEN pg_is_in_recovery() THEN CASE \
             WHEN pg_last_wal_receive_lsn() IS NOT DISTINCT FROM pg_last_wal_replay_lsn() \
             THEN 0::int8 \
             WHEN pg_last_xact_replay_timestamp() IS NULL THEN NULL::int8 \
             ELSE GREATEST(0, extract(epoch FROM \
             (statement_timestamp() - pg_last_xact_replay_timestamp())))::int8 END END \
             AS replay_lag_s, \
             CASE WHEN pg_is_in_recovery() \
             THEN pg_wal_lsn_diff(pg_last_wal_receive_lsn(), '0/0')::int8 END \
             AS standby_receive_lsn, \
             CASE WHEN pg_is_in_recovery() \
             THEN pg_wal_lsn_diff(pg_last_wal_replay_lsn(), '0/0')::int8 END \
             AS standby_replay_lsn, \
             (extract(epoch FROM \
             (CASE WHEN pg_is_in_recovery() THEN pg_last_xact_replay_timestamp() END)) \
             * 1e6)::int8 AS standby_last_replay_at_us, \
             CASE WHEN NOT pg_is_in_recovery() \
             THEN pg_wal_lsn_diff(pg_current_wal_lsn(), '0/0')::int8 END AS current_wal_lsn, \
             (extract(epoch FROM statement_timestamp()) * 1e6)::int8 AS ts_us"
        )
    }
}

/// Collect the one-row instance replication status.
///
/// LSNs come back as byte offsets from `0/0` via `pg_wal_lsn_diff`. `ts` is the
/// snapshot's `statement_timestamp()`. The lag is `0` once replay has caught up
/// to the received LSN, even if the primary has been quiet since.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_replication_instance(
    client: &Client,
    major: u32,
) -> Result<ReplicationInstanceRow, tokio_postgres::Error> {
    let row = client.query_one(replication_query(major), &[]).await?;
    Ok(ReplicationInstanceRow {
        ts: row.get("ts_us"),
        is_in_recovery: row.get("is_in_recovery"),
        timeline_id: row.get("timeline_id"),
        synchronous_standby_names: row.get("synchronous_standby_names"),
        synchronous_commit: row.get("synchronous_commit"),
        sender_host: row.get("sender_host"),
        connected_replicas: row.get("connected_replicas"),
        replay_lag_s: row.get("replay_lag_s"),
        standby_receive_lsn: row.get("standby_receive_lsn"),
        standby_replay_lsn: row.get("standby_replay_lsn"),
        standby_last_replay_at: row.get("standby_last_replay_at_us"),
        current_wal_lsn: row.get("current_wal_lsn"),
    })
}

#[cfg(test)]
mod tests {
    use super::{ReplicationInstanceRow, replication_query, to_replication_instance};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_replication_instance expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn standby_raw() -> ReplicationInstanceRow {
        ReplicationInstanceRow {
            ts: 5_000,
            is_in_recovery: true,
            timeline_id: 2,
            synchronous_standby_names: "*".to_owned(),
            synchronous_commit: "on".to_owned(),
            sender_host: Some("10.0.0.1".to_owned()),
            connected_replicas: 0,
            replay_lag_s: Some(3),
            standby_receive_lsn: Some(900),
            standby_replay_lsn: Some(880),
            standby_last_replay_at: Some(4_000),
            current_wal_lsn: None,
        }
    }

    #[test]
    fn query_uses_sender_host_from_pg11_and_conninfo_before() {
        assert!(replication_query(18).contains("sender_host FROM pg_stat_wal_receiver"));
        assert!(replication_query(11).contains("sender_host FROM pg_stat_wal_receiver"));
        assert!(replication_query(10).contains("conninfo FROM pg_stat_wal_receiver"));
        assert!(!replication_query(10).contains("sender_host FROM pg_stat_wal_receiver"));
        for major in [10, 11, 18] {
            assert!(replication_query(major).contains("connected_replicas"));
            assert!(replication_query(major).contains("pg_kronika"));
        }
    }

    #[test]
    fn to_replication_instance_interns_settings_host_and_maps_columns() {
        let out = to_replication_instance(&standby_raw(), fake_intern).expect("intern");
        assert!(out.is_in_recovery);
        assert_eq!(out.timeline_id, 2);
        assert_eq!(out.synchronous_standby_names, fake_intern(b"*").unwrap());
        assert_eq!(out.synchronous_commit, fake_intern(b"on").unwrap());
        assert_eq!(out.sender_host, Some(fake_intern(b"10.0.0.1").unwrap()));
        assert_eq!(out.connected_replicas, 0);
        assert_eq!(out.standby_last_replay_at.map(|ts| ts.0), Some(4_000));
        assert_eq!(out.current_wal_lsn, None);
    }

    #[test]
    fn primary_row_leaves_sender_host_none() {
        let mut raw = standby_raw();
        raw.is_in_recovery = false;
        raw.sender_host = None;
        let out = to_replication_instance(&raw, fake_intern).expect("intern");
        assert_eq!(out.sender_host, None);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_replication_instance(&standby_raw(), boom), Err("full"));
    }
}
