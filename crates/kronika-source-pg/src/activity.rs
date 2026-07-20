//! Collects `pg_stat_activity` rows for types `1_001_001`, `1_001_002`, and
//! `1_001_003`.
//!
//! Collection returns owned raw rows. The collector interns strings when it
//! writes the segment dictionary, so this crate does not depend on the writer.

use kronika_registry::pg_stat_activity::{PgStatActivityV1, PgStatActivityV2, PgStatActivityV3};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Source-side all-or-nothing ceiling for one activity snapshot.
pub const MAX_ACTIVITY_ROWS: usize = 4_096;
const ACTIVITY_FETCH_ROWS: i64 = 4_097;

/// Add the collector marker required by the SQL-transparency rule.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/activity.rs */ ",
            $sql,
        )
    };
}

/// The `pg_stat_activity` layout selected by the server major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityVersion {
    /// PG 10-12: type `1_001_001` (no `leader_pid`, no `query_id`).
    V1,
    /// PG 13: type `1_001_002` (adds `leader_pid`).
    V2,
    /// PG 14-18: type `1_001_003` (adds `query_id`).
    V3,
}

/// Select the `pg_stat_activity` schema variant for a server major version.
///
/// `leader_pid` arrived in PG13 and `query_id` in PG14; below 13 is V1, 13 is
/// V2, 14 and above is V3.
#[must_use]
pub const fn activity_version(major: u32) -> ActivityVersion {
    if major >= 14 {
        ActivityVersion::V3
    } else if major == 13 {
        ActivityVersion::V2
    } else {
        ActivityVersion::V1
    }
}

/// SQL for one `pg_stat_activity` schema variant.
///
/// Each query carries the kronika marker and selects only the columns that
/// version stores. Timestamps come back as unix microseconds,
/// `backend_xid`/`backend_xmin` as `age()` in transactions, and `ts` as one
/// `statement_timestamp()` for the whole snapshot.
#[must_use]
pub const fn activity_query(version: ActivityVersion) -> &'static str {
    match version {
        ActivityVersion::V1 => marked!(
            "SELECT pid, datname::text AS datname, usename::text AS usename, \
             coalesce(application_name, '') AS application_name, \
             coalesce(host(client_addr), '') AS client_addr, \
             backend_type, state, wait_event_type, wait_event, query, \
             age(backend_xid)::int8 AS backend_xid_age, \
             age(backend_xmin)::int8 AS backend_xmin_age, \
             (extract(epoch from backend_start) * 1e6)::int8 AS backend_start_us, \
             (extract(epoch from xact_start) * 1e6)::int8 AS xact_start_us, \
             (extract(epoch from query_start) * 1e6)::int8 AS query_start_us, \
             (extract(epoch from state_change) * 1e6)::int8 AS state_change_us, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
             pg_has_role('pg_read_all_stats', 'member') AS full_visibility \
             FROM pg_stat_activity"
        ),
        ActivityVersion::V2 => marked!(
            "SELECT pid, leader_pid, datname::text AS datname, usename::text AS usename, \
             coalesce(application_name, '') AS application_name, \
             coalesce(host(client_addr), '') AS client_addr, \
             backend_type, state, wait_event_type, wait_event, query, \
             age(backend_xid)::int8 AS backend_xid_age, \
             age(backend_xmin)::int8 AS backend_xmin_age, \
             (extract(epoch from backend_start) * 1e6)::int8 AS backend_start_us, \
             (extract(epoch from xact_start) * 1e6)::int8 AS xact_start_us, \
             (extract(epoch from query_start) * 1e6)::int8 AS query_start_us, \
             (extract(epoch from state_change) * 1e6)::int8 AS state_change_us, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
             pg_has_role('pg_read_all_stats', 'member') AS full_visibility \
             FROM pg_stat_activity"
        ),
        ActivityVersion::V3 => marked!(
            "SELECT pid, leader_pid, datname::text AS datname, usename::text AS usename, \
             coalesce(application_name, '') AS application_name, \
             coalesce(host(client_addr), '') AS client_addr, \
             backend_type, state, wait_event_type, wait_event, query, query_id, \
             age(backend_xid)::int8 AS backend_xid_age, \
             age(backend_xmin)::int8 AS backend_xmin_age, \
             (extract(epoch from backend_start) * 1e6)::int8 AS backend_start_us, \
             (extract(epoch from xact_start) * 1e6)::int8 AS xact_start_us, \
             (extract(epoch from query_start) * 1e6)::int8 AS query_start_us, \
             (extract(epoch from state_change) * 1e6)::int8 AS state_change_us, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
             pg_has_role('pg_read_all_stats', 'member') AS full_visibility \
             FROM pg_stat_activity"
        ),
    }
}

fn bounded_activity_query(version: ActivityVersion) -> String {
    format!("{} ORDER BY pid LIMIT $1", activity_query(version))
}

/// Raw `pg_stat_activity` row before string interning.
///
/// Strings are owned; the caller interns them into the segment dictionary.
/// Columns not selected by a version-specific query are `None`.
#[derive(Debug, Clone)]
pub struct ActivityRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Backend process id.
    pub pid: i32,
    /// Parallel-group leader pid.
    pub leader_pid: Option<i32>,
    /// Database name.
    pub datname: Option<String>,
    /// Role name.
    pub usename: Option<String>,
    /// Application name (empty string when unset).
    pub application_name: String,
    /// Client host as text (empty string for a local connection).
    pub client_addr: String,
    /// Backend type.
    pub backend_type: String,
    /// Backend state.
    pub state: Option<String>,
    /// Wait-event class.
    pub wait_event_type: Option<String>,
    /// Wait-event name.
    pub wait_event: Option<String>,
    /// Current query text.
    pub query: Option<String>,
    /// Query id.
    pub query_id: Option<i64>,
    /// Age of the backend's xid in transactions.
    pub backend_xid_age: Option<i64>,
    /// Age of the backend's xmin horizon.
    pub backend_xmin_age: Option<i64>,
    /// Backend start time, unix microseconds.
    pub backend_start: i64,
    /// Current transaction start, unix microseconds.
    pub xact_start: Option<i64>,
    /// Current query start, unix microseconds.
    pub query_start: Option<i64>,
    /// Last state change, unix microseconds.
    pub state_change: Option<i64>,
}

/// One bounded activity read and the provenance needed by snapshot-wide
/// consumers. Rows are empty when `truncated` is true.
#[derive(Debug, Clone)]
pub struct ActivityRead {
    /// Durable activity layout selected for the server major version.
    pub version: ActivityVersion,
    /// Parsed rows, empty when the source-side bound was exceeded.
    pub rows: Vec<ActivityRow>,
    /// Rows returned by the bounded read, including its possible guard row.
    pub source_rows: usize,
    /// Whether the guard row proved that the snapshot exceeded the bound.
    pub truncated: bool,
    /// Whether activity fields for all sessions were visible to the collector.
    pub full_visibility: bool,
}

/// Intern an optional string, preserving `None`.
fn opt<E>(
    intern: &mut impl FnMut(&[u8]) -> Result<StrId, E>,
    value: Option<&str>,
) -> Result<Option<StrId>, E> {
    match value {
        Some(s) => Ok(Some(intern(s.as_bytes())?)),
        None => Ok(None),
    }
}

/// Build a `1_001_003` row, interning strings through `intern`.
///
/// # Errors
/// Returns the interner's error if any string cannot be interned.
pub fn to_v3<E>(
    row: &ActivityRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatActivityV3, E> {
    Ok(PgStatActivityV3 {
        ts: Ts(row.ts),
        pid: row.pid,
        leader_pid: row.leader_pid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        application_name: intern(row.application_name.as_bytes())?,
        client_addr: intern(row.client_addr.as_bytes())?,
        backend_type: intern(row.backend_type.as_bytes())?,
        state: opt(&mut intern, row.state.as_deref())?,
        wait_event_type: opt(&mut intern, row.wait_event_type.as_deref())?,
        wait_event: opt(&mut intern, row.wait_event.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        query_id: row.query_id,
        backend_xid_age: row.backend_xid_age,
        backend_xmin_age: row.backend_xmin_age,
        backend_start: Ts(row.backend_start),
        xact_start: row.xact_start.map(Ts),
        query_start: row.query_start.map(Ts),
        state_change: row.state_change.map(Ts),
    })
}

/// Build a `1_001_002` row (PG13 layout, no `query_id`).
///
/// # Errors
/// Returns the interner's error if any string cannot be interned.
pub fn to_v2<E>(
    row: &ActivityRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatActivityV2, E> {
    Ok(PgStatActivityV2 {
        ts: Ts(row.ts),
        pid: row.pid,
        leader_pid: row.leader_pid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        application_name: intern(row.application_name.as_bytes())?,
        client_addr: intern(row.client_addr.as_bytes())?,
        backend_type: intern(row.backend_type.as_bytes())?,
        state: opt(&mut intern, row.state.as_deref())?,
        wait_event_type: opt(&mut intern, row.wait_event_type.as_deref())?,
        wait_event: opt(&mut intern, row.wait_event.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        backend_xid_age: row.backend_xid_age,
        backend_xmin_age: row.backend_xmin_age,
        backend_start: Ts(row.backend_start),
        xact_start: row.xact_start.map(Ts),
        query_start: row.query_start.map(Ts),
        state_change: row.state_change.map(Ts),
    })
}

/// Build a `1_001_001` row (PG10-12 layout, no `leader_pid`, no `query_id`).
///
/// # Errors
/// Returns the interner's error if any string cannot be interned.
pub fn to_v1<E>(
    row: &ActivityRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatActivityV1, E> {
    Ok(PgStatActivityV1 {
        ts: Ts(row.ts),
        pid: row.pid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        application_name: intern(row.application_name.as_bytes())?,
        client_addr: intern(row.client_addr.as_bytes())?,
        backend_type: intern(row.backend_type.as_bytes())?,
        state: opt(&mut intern, row.state.as_deref())?,
        wait_event_type: opt(&mut intern, row.wait_event_type.as_deref())?,
        wait_event: opt(&mut intern, row.wait_event.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        backend_xid_age: row.backend_xid_age,
        backend_xmin_age: row.backend_xmin_age,
        backend_start: Ts(row.backend_start),
        xact_start: row.xact_start.map(Ts),
        query_start: row.query_start.map(Ts),
        state_change: row.state_change.map(Ts),
    })
}

/// Read a raw row from a result row using the version's column set.
fn row_from_pg(row: &tokio_postgres::Row, version: ActivityVersion) -> ActivityRow {
    ActivityRow {
        ts: row.get("ts_us"),
        pid: row.get("pid"),
        leader_pid: match version {
            ActivityVersion::V1 => None,
            ActivityVersion::V2 | ActivityVersion::V3 => row.get("leader_pid"),
        },
        datname: row.get("datname"),
        usename: row.get("usename"),
        application_name: row.get("application_name"),
        client_addr: row.get("client_addr"),
        backend_type: row.get("backend_type"),
        state: row.get("state"),
        wait_event_type: row.get("wait_event_type"),
        wait_event: row.get("wait_event"),
        query: row.get("query"),
        query_id: match version {
            ActivityVersion::V1 | ActivityVersion::V2 => None,
            ActivityVersion::V3 => row.get("query_id"),
        },
        backend_xid_age: row.get("backend_xid_age"),
        backend_xmin_age: row.get("backend_xmin_age"),
        backend_start: row.get("backend_start_us"),
        xact_start: row.get("xact_start_us"),
        query_start: row.get("query_start_us"),
        state_change: row.get("state_change_us"),
    }
}

/// Collect a bounded `pg_stat_activity` snapshot.
///
/// The source fetches at most one row beyond the ceiling. If that row exists,
/// it returns no rows and sets the truncation flag; consumers must treat the
/// snapshot as unavailable.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_activity(
    client: &Client,
    major: u32,
) -> Result<ActivityRead, tokio_postgres::Error> {
    let version = activity_version(major);
    let query = bounded_activity_query(version);
    let rows = client.query(&query, &[&ACTIVITY_FETCH_ROWS]).await?;
    let source_rows = rows.len();
    let full_visibility = rows
        .first()
        .is_some_and(|row| row.get::<_, bool>("full_visibility"));
    if rows.len() > MAX_ACTIVITY_ROWS {
        return Ok(ActivityRead {
            version,
            rows: Vec::new(),
            source_rows,
            truncated: true,
            full_visibility,
        });
    }
    let parsed = rows.iter().map(|row| row_from_pg(row, version)).collect();
    Ok(ActivityRead {
        version,
        rows: parsed,
        source_rows,
        truncated: false,
        full_visibility,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ACTIVITY_FETCH_ROWS, ActivityRow, ActivityVersion, MAX_ACTIVITY_ROWS, activity_query,
        activity_version, bounded_activity_query, to_v1, to_v2, to_v3,
    };
    use kronika_registry::StrId;
    use std::convert::Infallible;

    /// Deterministic stand-in for the segment interner (FNV-1a, forced nonzero).
    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_v* expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn sample_row() -> ActivityRow {
        ActivityRow {
            ts: 2_000,
            pid: 42,
            leader_pid: Some(7),
            datname: Some("app".to_owned()),
            usename: Some("alice".to_owned()),
            application_name: "psql".to_owned(),
            client_addr: String::new(),
            backend_type: "client backend".to_owned(),
            state: Some("active".to_owned()),
            wait_event_type: None,
            wait_event: None,
            query: Some("select 1".to_owned()),
            query_id: Some(123),
            backend_xid_age: Some(5),
            backend_xmin_age: Some(9),
            backend_start: 1_000,
            xact_start: Some(1_500),
            query_start: Some(1_800),
            state_change: Some(1_900),
        }
    }

    #[test]
    fn version_follows_catalog_changes() {
        assert_eq!(activity_version(10), ActivityVersion::V1);
        assert_eq!(activity_version(12), ActivityVersion::V1);
        assert_eq!(activity_version(13), ActivityVersion::V2);
        assert_eq!(activity_version(14), ActivityVersion::V3);
        assert_eq!(activity_version(18), ActivityVersion::V3);
    }

    #[test]
    fn query_includes_version_specific_columns() {
        assert!(!activity_query(ActivityVersion::V1).contains("leader_pid"));
        assert!(!activity_query(ActivityVersion::V1).contains("query_id"));
        assert!(activity_query(ActivityVersion::V2).contains("leader_pid"));
        assert!(!activity_query(ActivityVersion::V2).contains("query_id"));
        assert!(activity_query(ActivityVersion::V3).contains("leader_pid"));
        assert!(activity_query(ActivityVersion::V3).contains("query_id"));
        for v in [
            ActivityVersion::V1,
            ActivityVersion::V2,
            ActivityVersion::V3,
        ] {
            assert!(activity_query(v).contains("pg_stat_activity"));
            assert!(activity_query(v).contains("pg_kronika"));
        }
    }

    #[test]
    fn collector_query_has_a_deterministic_source_side_guard_row() {
        let query = bounded_activity_query(ActivityVersion::V3);
        assert!(query.ends_with(" FROM pg_stat_activity ORDER BY pid LIMIT $1"));
        assert_eq!(
            ACTIVITY_FETCH_ROWS,
            i64::try_from(MAX_ACTIVITY_ROWS).unwrap() + 1
        );
    }

    #[test]
    fn query_records_visibility_without_exposing_activity_text() {
        for version in [
            ActivityVersion::V1,
            ActivityVersion::V2,
            ActivityVersion::V3,
        ] {
            let query = activity_query(version);
            assert!(query.contains("pg_has_role('pg_read_all_stats', 'member')"));
        }
    }

    #[test]
    fn to_v3_maps_every_column_and_interns_strings() {
        let r = to_v3(&sample_row(), fake_intern).expect("infallible intern");
        assert_eq!(r.ts.0, 2_000);
        assert_eq!(r.pid, 42);
        assert_eq!(r.leader_pid, Some(7));
        assert_eq!(r.datname, Some(fake_intern(b"app").unwrap()));
        assert_eq!(r.application_name, fake_intern(b"psql").unwrap());
        assert_eq!(r.client_addr, fake_intern(b"").unwrap());
        assert_eq!(r.wait_event_type, None);
        assert_eq!(r.query, Some(fake_intern(b"select 1").unwrap()));
        assert_eq!(r.query_id, Some(123));
        assert_eq!(r.backend_xmin_age, Some(9));
        assert_eq!(r.xact_start.map(|t| t.0), Some(1_500));
    }

    #[test]
    fn to_v2_keeps_leader_pid() {
        let r = to_v2(&sample_row(), fake_intern).expect("intern");
        assert_eq!(r.leader_pid, Some(7));
        assert_eq!(r.datname, Some(fake_intern(b"app").unwrap()));
        assert_eq!(r.backend_xmin_age, Some(9));
    }

    #[test]
    fn to_v1_maps_the_base_layout() {
        let r = to_v1(&sample_row(), fake_intern).expect("intern");
        assert_eq!(r.datname, Some(fake_intern(b"app").unwrap()));
        assert_eq!(r.ts.0, 2_000);
        assert_eq!(r.pid, 42);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_v3(&sample_row(), boom), Err("full"));
    }
}
