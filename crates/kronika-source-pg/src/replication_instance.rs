//! Instance replication status collection for type `1_015_001`.
//!
//! One row describes this instance's replication role and receiver state. The
//! same shape serves a primary and a standby: `pg_current_wal_lsn()` is read only
//! outside recovery, and standby LSNs, replay lag, and WAL receiver fields are
//! filled only in recovery. String-like values come back from SQL as bytea
//! slices capped by [`REPLICATION_INSTANCE_TEXT_BYTE_LIMIT`] before the collector
//! can allocate them.

use kronika_registry::Ts;
use kronika_registry::replication_instance::ReplicationInstance;
use tokio_postgres::Client;

/// Maximum bytes fetched for one interned text field in `1_015_001`.
///
/// This cap applies in SQL before `tokio-postgres` materializes `Vec<u8>`.
pub const REPLICATION_INSTANCE_TEXT_BYTE_LIMIT: i32 = 4096;
const _: () = assert!(
    REPLICATION_INSTANCE_TEXT_BYTE_LIMIT > 0,
    "replication-instance text byte limit must be positive"
);

/// Maximum LSN byte offset that fits the `i64` schema.
///
/// `pg_lsn` is unsigned internally, while the registry stores offsets as `i64`.
/// Larger values saturate at this ceiling before the SQL cast to `int8`.
pub const LSN_BYTE_OFFSET_CEILING: i64 = i64::MAX;

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

/// One raw instance-replication row; byte fields are interned by the caller.
#[derive(Debug)]
pub struct ReplicationInstanceRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Whether the instance is in recovery (a standby).
    pub is_in_recovery: bool,
    /// Control-file timeline id.
    pub timeline_id: i32,
    /// `synchronous_standby_names` setting, byte-bounded in SQL.
    pub synchronous_standby_names: Vec<u8>,
    /// `synchronous_commit` setting, byte-bounded in SQL.
    pub synchronous_commit: Vec<u8>,
    /// WAL receiver status on a standby.
    pub wal_receiver_status: Option<Vec<u8>>,
    /// Upstream host this standby streams from; `None` off a streaming standby.
    pub sender_host: Option<Vec<u8>>,
    /// Upstream port this standby streams from.
    pub sender_port: Option<i32>,
    /// Replication slot used by this WAL receiver.
    pub slot_name: Option<Vec<u8>>,
    /// Connected WAL sender rows whose state is `streaming`.
    pub streaming_replicas: i32,
    /// Replay lag, seconds (standby only).
    pub replay_lag_s: Option<i64>,
    /// Last received WAL LSN, byte offset (standby only).
    pub standby_receive_lsn: Option<i64>,
    /// Last replayed WAL LSN, byte offset (standby only).
    pub standby_replay_lsn: Option<i64>,
    /// Last replayed transaction commit time, unix microseconds (standby only).
    pub standby_last_replay_at: Option<i64>,
    /// Current WAL write location, byte offset (primary only).
    pub current_wal_lsn: Option<i64>,
    /// Last WAL location reported by this receiver to the upstream sender.
    pub latest_end_lsn: Option<i64>,
    /// Time of `latest_end_lsn`, unix microseconds.
    pub latest_end_time: Option<i64>,
    /// Timeline of the latest received and flushed WAL.
    pub received_tli: Option<i32>,
}

/// Build a `1_015_001` row, interning byte-bounded settings and receiver labels.
///
/// # Errors
/// Returns the interner's error if a byte value cannot be interned.
pub fn to_replication_instance<E>(
    row: &ReplicationInstanceRow,
    mut intern: impl FnMut(&[u8]) -> Result<kronika_registry::StrId, E>,
) -> Result<ReplicationInstance, E> {
    Ok(ReplicationInstance {
        ts: Ts(row.ts),
        is_in_recovery: row.is_in_recovery,
        timeline_id: row.timeline_id,
        synchronous_standby_names: intern(&row.synchronous_standby_names)?,
        synchronous_commit: intern(&row.synchronous_commit)?,
        wal_receiver_status: row
            .wal_receiver_status
            .as_deref()
            .map(&mut intern)
            .transpose()?,
        sender_host: row.sender_host.as_deref().map(&mut intern).transpose()?,
        sender_port: row.sender_port,
        slot_name: row.slot_name.as_deref().map(&mut intern).transpose()?,
        streaming_replicas: row.streaming_replicas,
        replay_lag_s: row.replay_lag_s,
        standby_receive_lsn: row.standby_receive_lsn,
        standby_replay_lsn: row.standby_replay_lsn,
        standby_last_replay_at: row.standby_last_replay_at.map(Ts),
        current_wal_lsn: row.current_wal_lsn,
        latest_end_lsn: row.latest_end_lsn,
        latest_end_time: row.latest_end_time.map(Ts),
        received_tli: row.received_tli,
    })
}

/// The SQL for the server's major version.
///
/// `pg_stat_wal_receiver.sender_host` and `sender_port` exist from PG11; PG10
/// exposes only `conninfo`, so collection fetches capped conninfo bytes and
/// extracts `host`/`hostaddr` and `port` locally.
#[must_use]
pub const fn replication_query(major: u32) -> &'static str {
    if major >= 11 {
        replication_query_pg11_plus()
    } else {
        replication_query_pg10()
    }
}

const fn replication_query_pg11_plus() -> &'static str {
    marked!(
        "WITH snap AS (SELECT statement_timestamp() AS ts, \
         pg_is_in_recovery() AS is_in_recovery, \
         pg_last_wal_receive_lsn() AS receive_lsn, \
         pg_last_wal_replay_lsn() AS replay_lsn, \
         pg_last_xact_replay_timestamp() AS replay_ts), \
         recv AS (SELECT status, sender_host, sender_port, slot_name, \
         latest_end_lsn, latest_end_time, received_tli \
         FROM pg_stat_wal_receiver LIMIT 1) \
         SELECT snap.is_in_recovery AS is_in_recovery, \
         (pg_control_checkpoint()).timeline_id AS timeline_id, \
         substring(convert_to(COALESCE(current_setting('synchronous_standby_names', true), ''), 'UTF8') from 1 for $1::int4) \
         AS synchronous_standby_names, \
         substring(convert_to(COALESCE(current_setting('synchronous_commit', true), ''), 'UTF8') from 1 for $1::int4) \
         AS synchronous_commit, \
         (SELECT substring(convert_to(status, 'UTF8') from 1 for $1::int4) FROM recv) \
         AS wal_receiver_status, \
         (SELECT substring(convert_to(sender_host, 'UTF8') from 1 for $1::int4) FROM recv) \
         AS sender_host, \
         (SELECT sender_port FROM recv) AS sender_port, \
         (SELECT substring(convert_to(slot_name, 'UTF8') from 1 for $1::int4) FROM recv) \
         AS slot_name, \
         NULL::bytea AS wal_receiver_conninfo, \
         (SELECT count(*) FILTER (WHERE state = 'streaming') FROM pg_stat_replication)::int4 \
         AS streaming_replicas, \
         CASE WHEN snap.is_in_recovery THEN CASE \
         WHEN snap.receive_lsn IS NOT NULL AND snap.replay_lsn IS NOT NULL \
         AND snap.receive_lsn = snap.replay_lsn THEN 0::int8 \
         WHEN snap.replay_ts IS NULL THEN NULL::int8 \
         ELSE GREATEST(0, extract(epoch FROM (snap.ts - snap.replay_ts)))::int8 END END \
         AS replay_lag_s, \
         CASE WHEN snap.is_in_recovery AND snap.receive_lsn IS NOT NULL \
         THEN LEAST(pg_wal_lsn_diff(snap.receive_lsn, '0/0'), $2::int8::numeric)::int8 END \
         AS standby_receive_lsn, \
         CASE WHEN snap.is_in_recovery AND snap.replay_lsn IS NOT NULL \
         THEN LEAST(pg_wal_lsn_diff(snap.replay_lsn, '0/0'), $2::int8::numeric)::int8 END \
         AS standby_replay_lsn, \
         (extract(epoch FROM CASE WHEN snap.is_in_recovery THEN snap.replay_ts END) * 1e6)::int8 \
         AS standby_last_replay_at_us, \
         CASE WHEN NOT snap.is_in_recovery \
         THEN LEAST(pg_wal_lsn_diff(pg_current_wal_lsn(), '0/0'), $2::int8::numeric)::int8 END \
         AS current_wal_lsn, \
         (SELECT CASE WHEN latest_end_lsn IS NOT NULL \
         THEN LEAST(pg_wal_lsn_diff(latest_end_lsn, '0/0'), $2::int8::numeric)::int8 END \
         FROM recv) AS latest_end_lsn, \
         (SELECT (extract(epoch FROM latest_end_time) * 1e6)::int8 FROM recv) \
         AS latest_end_time_us, \
         (SELECT received_tli FROM recv) AS received_tli, \
         (extract(epoch FROM snap.ts) * 1e6)::int8 AS ts_us \
         FROM snap"
    )
}

const fn replication_query_pg10() -> &'static str {
    marked!(
        "WITH snap AS (SELECT statement_timestamp() AS ts, \
         pg_is_in_recovery() AS is_in_recovery, \
         pg_last_wal_receive_lsn() AS receive_lsn, \
         pg_last_wal_replay_lsn() AS replay_lsn, \
         pg_last_xact_replay_timestamp() AS replay_ts), \
         recv AS (SELECT status, conninfo, slot_name, latest_end_lsn, \
         latest_end_time, received_tli FROM pg_stat_wal_receiver LIMIT 1) \
         SELECT snap.is_in_recovery AS is_in_recovery, \
         (pg_control_checkpoint()).timeline_id AS timeline_id, \
         substring(convert_to(COALESCE(current_setting('synchronous_standby_names', true), ''), 'UTF8') from 1 for $1::int4) \
         AS synchronous_standby_names, \
         substring(convert_to(COALESCE(current_setting('synchronous_commit', true), ''), 'UTF8') from 1 for $1::int4) \
         AS synchronous_commit, \
         (SELECT substring(convert_to(status, 'UTF8') from 1 for $1::int4) FROM recv) \
         AS wal_receiver_status, \
         NULL::bytea AS sender_host, \
         NULL::int4 AS sender_port, \
         (SELECT substring(convert_to(slot_name, 'UTF8') from 1 for $1::int4) FROM recv) \
         AS slot_name, \
         (SELECT substring(convert_to(conninfo, 'UTF8') from 1 for $1::int4) FROM recv) \
         AS wal_receiver_conninfo, \
         (SELECT count(*) FILTER (WHERE state = 'streaming') FROM pg_stat_replication)::int4 \
         AS streaming_replicas, \
         CASE WHEN snap.is_in_recovery THEN CASE \
         WHEN snap.receive_lsn IS NOT NULL AND snap.replay_lsn IS NOT NULL \
         AND snap.receive_lsn = snap.replay_lsn THEN 0::int8 \
         WHEN snap.replay_ts IS NULL THEN NULL::int8 \
         ELSE GREATEST(0, extract(epoch FROM (snap.ts - snap.replay_ts)))::int8 END END \
         AS replay_lag_s, \
         CASE WHEN snap.is_in_recovery AND snap.receive_lsn IS NOT NULL \
         THEN LEAST(pg_wal_lsn_diff(snap.receive_lsn, '0/0'), $2::int8::numeric)::int8 END \
         AS standby_receive_lsn, \
         CASE WHEN snap.is_in_recovery AND snap.replay_lsn IS NOT NULL \
         THEN LEAST(pg_wal_lsn_diff(snap.replay_lsn, '0/0'), $2::int8::numeric)::int8 END \
         AS standby_replay_lsn, \
         (extract(epoch FROM CASE WHEN snap.is_in_recovery THEN snap.replay_ts END) * 1e6)::int8 \
         AS standby_last_replay_at_us, \
         CASE WHEN NOT snap.is_in_recovery \
         THEN LEAST(pg_wal_lsn_diff(pg_current_wal_lsn(), '0/0'), $2::int8::numeric)::int8 END \
         AS current_wal_lsn, \
         (SELECT CASE WHEN latest_end_lsn IS NOT NULL \
         THEN LEAST(pg_wal_lsn_diff(latest_end_lsn, '0/0'), $2::int8::numeric)::int8 END \
         FROM recv) AS latest_end_lsn, \
         (SELECT (extract(epoch FROM latest_end_time) * 1e6)::int8 FROM recv) \
         AS latest_end_time_us, \
         (SELECT received_tli FROM recv) AS received_tli, \
         (extract(epoch FROM snap.ts) * 1e6)::int8 AS ts_us \
         FROM snap"
    )
}

/// Collect the one-row instance replication status.
///
/// LSNs come back as signed byte offsets from `0/0` via `pg_wal_lsn_diff`,
/// saturated at [`LSN_BYTE_OFFSET_CEILING`]. `ts` is the snapshot's
/// `statement_timestamp()`. Replay lag is `0` only when both receive and replay
/// LSN are known and equal; unknown LSNs stay `NULL`.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_replication_instance(
    client: &Client,
    major: u32,
) -> Result<ReplicationInstanceRow, tokio_postgres::Error> {
    let row = client
        .query_one(
            replication_query(major),
            &[
                &REPLICATION_INSTANCE_TEXT_BYTE_LIMIT,
                &LSN_BYTE_OFFSET_CEILING,
            ],
        )
        .await?;
    let conninfo = row.get::<_, Option<Vec<u8>>>("wal_receiver_conninfo");
    let (pg10_sender_host, pg10_sender_port) = conninfo
        .as_deref()
        .map_or((None, None), parse_pg10_conninfo_sender);
    Ok(ReplicationInstanceRow {
        ts: row.get("ts_us"),
        is_in_recovery: row.get("is_in_recovery"),
        timeline_id: row.get("timeline_id"),
        synchronous_standby_names: row.get("synchronous_standby_names"),
        synchronous_commit: row.get("synchronous_commit"),
        wal_receiver_status: row.get("wal_receiver_status"),
        sender_host: row
            .get::<_, Option<Vec<u8>>>("sender_host")
            .or(pg10_sender_host),
        sender_port: row
            .get::<_, Option<i32>>("sender_port")
            .or(pg10_sender_port),
        slot_name: row.get("slot_name"),
        streaming_replicas: row.get("streaming_replicas"),
        replay_lag_s: row.get("replay_lag_s"),
        standby_receive_lsn: row.get("standby_receive_lsn"),
        standby_replay_lsn: row.get("standby_replay_lsn"),
        standby_last_replay_at: row.get("standby_last_replay_at_us"),
        current_wal_lsn: row.get("current_wal_lsn"),
        latest_end_lsn: row.get("latest_end_lsn"),
        latest_end_time: row.get("latest_end_time_us"),
        received_tli: row.get("received_tli"),
    })
}

fn parse_pg10_conninfo_sender(conninfo: &[u8]) -> (Option<Vec<u8>>, Option<i32>) {
    let host = conninfo_value(conninfo, b"host").or_else(|| conninfo_value(conninfo, b"hostaddr"));
    let port = conninfo_value(conninfo, b"port").and_then(|value| parse_ascii_port(&value));
    (host, port)
}

fn parse_ascii_port(value: &[u8]) -> Option<i32> {
    let value = std::str::from_utf8(value).ok()?;
    let port = value.parse::<i32>().ok()?;
    (port > 0).then_some(port)
}

fn conninfo_value(conninfo: &[u8], target_key: &[u8]) -> Option<Vec<u8>> {
    let mut idx = 0;
    let mut found = None;
    while idx < conninfo.len() {
        while idx < conninfo.len() && conninfo[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let key_start = idx;
        while idx < conninfo.len() && conninfo[idx] != b'=' && !conninfo[idx].is_ascii_whitespace()
        {
            idx += 1;
        }
        let key = &conninfo[key_start..idx];
        while idx < conninfo.len() && conninfo[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= conninfo.len() || conninfo[idx] != b'=' {
            while idx < conninfo.len() && !conninfo[idx].is_ascii_whitespace() {
                idx += 1;
            }
            continue;
        }
        idx += 1;
        while idx < conninfo.len() && conninfo[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let value = if idx < conninfo.len() && conninfo[idx] == b'\'' {
            idx += 1;
            let mut value = Vec::new();
            while idx < conninfo.len() {
                match conninfo[idx] {
                    b'\'' => {
                        idx += 1;
                        break;
                    }
                    b'\\' if idx + 1 < conninfo.len() => {
                        idx += 1;
                        value.push(conninfo[idx]);
                        idx += 1;
                    }
                    byte => {
                        value.push(byte);
                        idx += 1;
                    }
                }
            }
            value
        } else {
            let value_start = idx;
            while idx < conninfo.len() && !conninfo[idx].is_ascii_whitespace() {
                idx += 1;
            }
            conninfo[value_start..idx].to_vec()
        };
        if key == target_key && !value.is_empty() {
            found = Some(value);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{
        LSN_BYTE_OFFSET_CEILING, ReplicationInstanceRow, conninfo_value,
        parse_pg10_conninfo_sender, replication_query, to_replication_instance,
    };
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
            synchronous_standby_names: b"*".to_vec(),
            synchronous_commit: b"on".to_vec(),
            wal_receiver_status: Some(b"streaming".to_vec()),
            sender_host: Some(b"10.0.0.1".to_vec()),
            sender_port: Some(5432),
            slot_name: Some(b"standby_a".to_vec()),
            streaming_replicas: 0,
            replay_lag_s: Some(3),
            standby_receive_lsn: Some(900),
            standby_replay_lsn: Some(880),
            standby_last_replay_at: Some(4_000),
            current_wal_lsn: None,
            latest_end_lsn: Some(910),
            latest_end_time: Some(4_500),
            received_tli: Some(2),
        }
    }

    #[test]
    fn query_uses_bytea_caps_and_versioned_sender_source() {
        assert_eq!(LSN_BYTE_OFFSET_CEILING, i64::MAX);
        assert!(replication_query(18).contains("sender_host"));
        assert!(replication_query(18).contains("sender_port"));
        assert!(replication_query(18).contains("substring(convert_to(sender_host"));
        assert!(replication_query(10).contains("conninfo"));
        assert!(!replication_query(10).contains("substring(convert_to(sender_host"));
        for major in [10, 11, 18] {
            let sql = replication_query(major);
            assert!(sql.contains("$1::int4"), "text fields are byte-capped");
            assert!(sql.contains("$2::int8::numeric"), "LSN offsets are capped");
            assert!(sql.contains("state = 'streaming'"));
            assert!(sql.contains("streaming_replicas"));
            assert!(sql.contains("pg_kronika"));
        }
    }

    #[test]
    fn replay_lag_zero_requires_known_receive_and_replay_lsn() {
        let sql = replication_query(18);
        assert!(sql.contains("snap.receive_lsn IS NOT NULL"));
        assert!(sql.contains("snap.replay_lsn IS NOT NULL"));
        assert!(sql.contains("snap.receive_lsn = snap.replay_lsn"));
        assert!(!sql.contains("IS NOT DISTINCT FROM"));
    }

    #[test]
    fn pg10_conninfo_sender_parser_extracts_host_and_port() {
        let (host, port) =
            parse_pg10_conninfo_sender(b"user=repl host=primary.local port=5433 sslmode=prefer");
        assert_eq!(host.as_deref(), Some(b"primary.local".as_slice()));
        assert_eq!(port, Some(5433));
    }

    #[test]
    fn pg10_conninfo_sender_parser_handles_quoted_values() {
        let conninfo = b"user=repl host='/var/lib/postgresql socket' port='5432'";
        let (host, port) = parse_pg10_conninfo_sender(conninfo);
        assert_eq!(
            host.as_deref(),
            Some(b"/var/lib/postgresql socket".as_slice())
        );
        assert_eq!(port, Some(5432));
    }

    #[test]
    fn pg10_conninfo_sender_parser_falls_back_to_hostaddr() {
        let (host, port) = parse_pg10_conninfo_sender(b"user=repl hostaddr=192.0.2.10");
        assert_eq!(host.as_deref(), Some(b"192.0.2.10".as_slice()));
        assert_eq!(port, None);
    }

    #[test]
    fn conninfo_parser_uses_the_last_repeated_key() {
        assert_eq!(
            conninfo_value(b"host=old host=new", b"host").as_deref(),
            Some(b"new".as_slice())
        );
    }

    #[test]
    fn to_replication_instance_interns_settings_receiver_labels_and_maps_columns() {
        let out = to_replication_instance(&standby_raw(), fake_intern).expect("intern");
        assert!(out.is_in_recovery);
        assert_eq!(out.timeline_id, 2);
        assert_eq!(out.synchronous_standby_names, fake_intern(b"*").unwrap());
        assert_eq!(out.synchronous_commit, fake_intern(b"on").unwrap());
        assert_eq!(
            out.wal_receiver_status,
            Some(fake_intern(b"streaming").unwrap())
        );
        assert_eq!(out.sender_host, Some(fake_intern(b"10.0.0.1").unwrap()));
        assert_eq!(out.sender_port, Some(5432));
        assert_eq!(out.slot_name, Some(fake_intern(b"standby_a").unwrap()));
        assert_eq!(out.streaming_replicas, 0);
        assert_eq!(out.standby_last_replay_at.map(|ts| ts.0), Some(4_000));
        assert_eq!(out.latest_end_time.map(|ts| ts.0), Some(4_500));
        assert_eq!(out.received_tli, Some(2));
        assert_eq!(out.current_wal_lsn, None);
    }

    #[test]
    fn primary_row_leaves_receiver_fields_none() {
        let mut raw = standby_raw();
        raw.is_in_recovery = false;
        raw.wal_receiver_status = None;
        raw.sender_host = None;
        raw.sender_port = None;
        raw.slot_name = None;
        raw.latest_end_lsn = None;
        raw.latest_end_time = None;
        raw.received_tli = None;
        let out = to_replication_instance(&raw, fake_intern).expect("intern");
        assert_eq!(out.sender_host, None);
        assert_eq!(out.wal_receiver_status, None);
        assert_eq!(out.slot_name, None);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_replication_instance(&standby_raw(), boom), Err("full"));
    }
}
