//! Replication detail collection: `pg_stat_replication` (`1_016_001`) and
//! `pg_replication_slots` (`1_017_001`).
//!
//! Both views are cluster-wide and read from the main connection. A standby
//! usually has no walsenders, so `1_016_001` seals no rows there; slots exist
//! on both roles, but `retained_bytes` needs `pg_current_wal_lsn()` and is
//! `NULL` in recovery. LSN offsets saturate to `i64::MAX` in SQL, matching
//! the `1_015_001` collection.

use kronika_registry::replication_replicas::ReplicationReplicasV1;
use kronika_registry::replication_slots::ReplicationSlotsV1;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

use crate::replication_instance::LSN_BYTE_OFFSET_CEILING;

/// Prefix a query literal with the collector marker.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/replication_details.rs */ ",
            $sql,
        )
    };
}

/// One `pg_stat_replication` row before interning.
#[derive(Debug, Clone)]
pub struct ReplicaRow {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// Walsender backend pid.
    pub pid: i32,
    /// Role name; empty when unresolved.
    pub usename: String,
    /// Standby application name; empty when unset.
    pub application_name: String,
    /// Client address text; `None` for a unix-socket connection.
    pub client_addr: Option<String>,
    /// Walsender state.
    pub state: String,
    /// Synchronous state.
    pub sync_state: String,
    /// Synchronous priority.
    pub sync_priority: Option<i32>,
    /// Sent position, byte offset from `0/0`.
    pub sent_lsn: Option<i64>,
    /// Written position.
    pub write_lsn: Option<i64>,
    /// Flushed position.
    pub flush_lsn: Option<i64>,
    /// Replayed position.
    pub replay_lsn: Option<i64>,
    /// Write lag, microseconds.
    pub write_lag_us: Option<i64>,
    /// Flush lag, microseconds.
    pub flush_lag_us: Option<i64>,
    /// Replay lag, microseconds.
    pub replay_lag_us: Option<i64>,
}

/// Collect every `pg_stat_replication` row.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_replication_replicas(
    client: &Client,
) -> Result<Vec<ReplicaRow>, tokio_postgres::Error> {
    let rows = client
        .query(
            marked!(
                "SELECT \
                     (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                     pid, \
                     COALESCE(usename::text, '') AS usename, \
                     COALESCE(application_name, '') AS application_name, \
                     host(client_addr)::text AS client_addr, \
                     COALESCE(state, '') AS state, \
                     COALESCE(sync_state, '') AS sync_state, \
                     sync_priority, \
                     CASE WHEN sent_lsn IS NOT NULL THEN \
                         LEAST(pg_wal_lsn_diff(sent_lsn, '0/0'), $1::int8::numeric)::int8 END \
                         AS sent_lsn, \
                     CASE WHEN write_lsn IS NOT NULL THEN \
                         LEAST(pg_wal_lsn_diff(write_lsn, '0/0'), $1::int8::numeric)::int8 END \
                         AS write_lsn, \
                     CASE WHEN flush_lsn IS NOT NULL THEN \
                         LEAST(pg_wal_lsn_diff(flush_lsn, '0/0'), $1::int8::numeric)::int8 END \
                         AS flush_lsn, \
                     CASE WHEN replay_lsn IS NOT NULL THEN \
                         LEAST(pg_wal_lsn_diff(replay_lsn, '0/0'), $1::int8::numeric)::int8 END \
                         AS replay_lsn, \
                     (extract(epoch from write_lag) * 1e6)::int8 AS write_lag_us, \
                     (extract(epoch from flush_lag) * 1e6)::int8 AS flush_lag_us, \
                     (extract(epoch from replay_lag) * 1e6)::int8 AS replay_lag_us \
                 FROM pg_stat_replication \
                 ORDER BY application_name, pid"
            ),
            &[&LSN_BYTE_OFFSET_CEILING],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| ReplicaRow {
            ts: row.get("ts_us"),
            pid: row.get("pid"),
            usename: row.get("usename"),
            application_name: row.get("application_name"),
            client_addr: row.get("client_addr"),
            state: row.get("state"),
            sync_state: row.get("sync_state"),
            sync_priority: row.get("sync_priority"),
            sent_lsn: row.get("sent_lsn"),
            write_lsn: row.get("write_lsn"),
            flush_lsn: row.get("flush_lsn"),
            replay_lsn: row.get("replay_lsn"),
            write_lag_us: row.get("write_lag_us"),
            flush_lag_us: row.get("flush_lag_us"),
            replay_lag_us: row.get("replay_lag_us"),
        })
        .collect())
}

/// Intern the row's strings and build the sealed `1_016_001` layout.
///
/// # Errors
/// Propagates the interner error when the dictionary is full.
pub fn to_replicas_v1<E>(
    row: &ReplicaRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<ReplicationReplicasV1, E> {
    Ok(ReplicationReplicasV1 {
        ts: Ts(row.ts),
        pid: row.pid,
        usename: intern(row.usename.as_bytes())?,
        application_name: intern(row.application_name.as_bytes())?,
        client_addr: row
            .client_addr
            .as_deref()
            .map(|s| intern(s.as_bytes()))
            .transpose()?,
        state: intern(row.state.as_bytes())?,
        sync_state: intern(row.sync_state.as_bytes())?,
        sync_priority: row.sync_priority,
        sent_lsn: row.sent_lsn,
        write_lsn: row.write_lsn,
        flush_lsn: row.flush_lsn,
        replay_lsn: row.replay_lsn,
        write_lag_us: row.write_lag_us,
        flush_lag_us: row.flush_lag_us,
        replay_lag_us: row.replay_lag_us,
    })
}

/// One `pg_replication_slots` row before interning.
#[derive(Debug, Clone)]
pub struct SlotRow {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// Slot name.
    pub slot_name: String,
    /// Logical decoding plugin; `None` for a physical slot.
    pub plugin: Option<String>,
    /// `physical` or `logical`.
    pub slot_type: String,
    /// Whether a connection is currently streaming from the slot.
    pub active: bool,
    /// Oldest WAL the slot still needs.
    pub restart_lsn: Option<i64>,
    /// Position the consumer confirmed.
    pub confirmed_flush_lsn: Option<i64>,
    /// WAL held back by this slot.
    pub retained_bytes: Option<i64>,
    /// `reserved` | `extended` | `unreserved` | `lost`; `None` before PG13.
    pub wal_status: Option<String>,
}

/// The slots query; PG13 added `wal_status`.
const fn slots_query(major: u32) -> &'static str {
    if major >= 13 {
        marked!(
            "SELECT \
                 (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                 slot_name::text, \
                 plugin::text, \
                 slot_type, \
                 COALESCE(active, false) AS active, \
                 CASE WHEN restart_lsn IS NOT NULL THEN \
                     LEAST(pg_wal_lsn_diff(restart_lsn, '0/0'), $1::int8::numeric)::int8 END \
                     AS restart_lsn, \
                 CASE WHEN confirmed_flush_lsn IS NOT NULL THEN \
                     LEAST(pg_wal_lsn_diff(confirmed_flush_lsn, '0/0'), $1::int8::numeric)::int8 END \
                     AS confirmed_flush_lsn, \
                 CASE WHEN NOT pg_is_in_recovery() AND restart_lsn IS NOT NULL THEN \
                     GREATEST(0, LEAST(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn), \
                                       $1::int8::numeric))::int8 END \
                     AS retained_bytes, \
                 wal_status \
             FROM pg_replication_slots \
             ORDER BY slot_name"
        )
    } else {
        marked!(
            "SELECT \
                 (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                 slot_name::text, \
                 plugin::text, \
                 slot_type, \
                 COALESCE(active, false) AS active, \
                 CASE WHEN restart_lsn IS NOT NULL THEN \
                     LEAST(pg_wal_lsn_diff(restart_lsn, '0/0'), $1::int8::numeric)::int8 END \
                     AS restart_lsn, \
                 CASE WHEN confirmed_flush_lsn IS NOT NULL THEN \
                     LEAST(pg_wal_lsn_diff(confirmed_flush_lsn, '0/0'), $1::int8::numeric)::int8 END \
                     AS confirmed_flush_lsn, \
                 CASE WHEN NOT pg_is_in_recovery() AND restart_lsn IS NOT NULL THEN \
                     GREATEST(0, LEAST(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn), \
                                       $1::int8::numeric))::int8 END \
                     AS retained_bytes, \
                 NULL::text AS wal_status \
             FROM pg_replication_slots \
             ORDER BY slot_name"
        )
    }
}

/// Collect every `pg_replication_slots` row.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_replication_slots(
    client: &Client,
    major: u32,
) -> Result<Vec<SlotRow>, tokio_postgres::Error> {
    let rows = client
        .query(slots_query(major), &[&LSN_BYTE_OFFSET_CEILING])
        .await?;
    Ok(rows
        .iter()
        .map(|row| SlotRow {
            ts: row.get("ts_us"),
            slot_name: row.get("slot_name"),
            plugin: row.get("plugin"),
            slot_type: row.get("slot_type"),
            active: row.get("active"),
            restart_lsn: row.get("restart_lsn"),
            confirmed_flush_lsn: row.get("confirmed_flush_lsn"),
            retained_bytes: row.get("retained_bytes"),
            wal_status: row.get("wal_status"),
        })
        .collect())
}

/// Intern the row's strings and build the sealed `1_017_001` layout.
///
/// # Errors
/// Propagates the interner error when the dictionary is full.
pub fn to_slots_v1<E>(
    row: &SlotRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<ReplicationSlotsV1, E> {
    Ok(ReplicationSlotsV1 {
        ts: Ts(row.ts),
        slot_name: intern(row.slot_name.as_bytes())?,
        plugin: row
            .plugin
            .as_deref()
            .map(|s| intern(s.as_bytes()))
            .transpose()?,
        slot_type: intern(row.slot_type.as_bytes())?,
        active: row.active,
        restart_lsn: row.restart_lsn,
        confirmed_flush_lsn: row.confirmed_flush_lsn,
        retained_bytes: row.retained_bytes,
        wal_status: row
            .wal_status
            .as_deref()
            .map(|s| intern(s.as_bytes()))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::{ReplicaRow, SlotRow, slots_query, to_replicas_v1, to_slots_v1};
    use kronika_registry::StrId;

    #[test]
    fn slots_query_gates_wal_status_by_major() {
        assert!(slots_query(12).contains("NULL::text AS wal_status"));
        assert!(!slots_query(13).contains("NULL::text AS wal_status"));
        assert!(slots_query(13).contains("wal_status"));
    }

    #[test]
    fn to_replicas_v1_interns_labels_and_keeps_nulls() {
        let row = ReplicaRow {
            ts: 7,
            pid: 100,
            usename: "postgres".to_owned(),
            application_name: "walreceiver".to_owned(),
            client_addr: None,
            state: "backup".to_owned(),
            sync_state: "async".to_owned(),
            sync_priority: Some(0),
            sent_lsn: None,
            write_lsn: None,
            flush_lsn: None,
            replay_lsn: None,
            write_lag_us: None,
            flush_lag_us: None,
            replay_lag_us: None,
        };
        let mut next = 0_u64;
        let sealed = to_replicas_v1::<()>(&row, |_| {
            next += 1;
            Ok(StrId(next))
        })
        .expect("interner never fails here");
        assert_eq!(sealed.pid, 100);
        assert_eq!(sealed.usename, StrId(1));
        assert_eq!(sealed.client_addr, None);
        assert_eq!(sealed.sent_lsn, None);
        assert_eq!(sealed.sync_state, StrId(4));
    }

    #[test]
    fn to_slots_v1_interns_labels_and_keeps_nulls() {
        let row = SlotRow {
            ts: 7,
            slot_name: "standby_slot".to_owned(),
            plugin: None,
            slot_type: "physical".to_owned(),
            active: false,
            restart_lsn: None,
            confirmed_flush_lsn: None,
            retained_bytes: None,
            wal_status: None,
        };
        let mut next = 0_u64;
        let sealed = to_slots_v1::<()>(&row, |_| {
            next += 1;
            Ok(StrId(next))
        })
        .expect("interner never fails here");
        assert_eq!(sealed.slot_name, StrId(1));
        assert_eq!(sealed.plugin, None);
        assert_eq!(sealed.slot_type, StrId(2));
        assert!(!sealed.active);
        assert_eq!(sealed.wal_status, None);
    }
}
