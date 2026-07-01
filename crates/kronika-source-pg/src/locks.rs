//! `PostgreSQL` lock-wait tree collection (type `1_011`).
//!
//! Class A: one connection sees all backends cluster-wide. Two-stage — a cheap
//! `EXISTS` precheck gates a cycle-safe recursive CTE over `pg_blocking_pids`.
//! The collector records raw nodes plus `blocked_by` edges; the read side builds
//! and interprets the tree. No thresholds or verdicts live here — `depth` and
//! `root_pid` are structural traversal outputs.
//!
//! The layout splits on the PG14 `waitstart` column: V1 (PG10-13) has no
//! `waitstart` and guards recursion with a manual path array; V2 (PG14-18) adds
//! `waitstart` and uses the SQL `CYCLE` clause.

use kronika_registry::pg_locks::{PgLocksV1, PgLocksV2};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule): the
/// statement then shows in `pg_stat_activity` and the server log as kronika, its
/// version, and this source file.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/locks.rs */ ",
            $sql,
        )
    };
}

/// Schema version, split on the PG14 `waitstart` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocksVersion {
    /// PG10-13: no `waitstart`, manual cycle guard.
    V1,
    /// PG14-18: `waitstart`, SQL `CYCLE` clause.
    V2,
}

/// Pick the layout from the server major version.
#[must_use]
pub const fn locks_version(major: u32) -> LocksVersion {
    if major >= 14 {
        LocksVersion::V2
    } else {
        LocksVersion::V1
    }
}

/// Cheap precheck: are any backends waiting on a heavyweight lock?
///
/// A caller runs this before [`collect_locks`] to skip the recursive CTE when
/// nothing is blocked.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn lock_waits_exist(client: &Client) -> Result<bool, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock') AS waiting"
            ),
            &[],
        )
        .await?;
    Ok(row.get("waiting"))
}

/// The version-specific collection query. `$1` = max rows.
#[must_use]
pub const fn locks_query(version: LocksVersion) -> &'static str {
    match version {
        LocksVersion::V1 => locks_query_v1(),
        LocksVersion::V2 => locks_query_v2(),
    }
}

const fn locks_query_v2() -> &'static str {
    marked!(
        "WITH RECURSIVE \
         snap AS (SELECT statement_timestamp() AS ts), \
         waiters AS (SELECT a.pid, pg_blocking_pids(a.pid) AS bp \
                     FROM pg_stat_activity a WHERE a.wait_event_type = 'Lock'), \
         edges AS (SELECT w.pid AS waiter, b AS blocker \
                   FROM waiters w, unnest(w.bp) AS b), \
         roots AS (SELECT DISTINCT e.blocker AS pid FROM edges e \
                   WHERE e.blocker <> 0 AND e.blocker NOT IN (SELECT pid FROM waiters)), \
         tree AS (SELECT r.pid, 0 AS depth, r.pid AS root_pid FROM roots r \
                  UNION ALL \
                  SELECT e.waiter, t.depth + 1, t.root_pid \
                  FROM tree t JOIN edges e ON e.blocker = t.pid) \
                  CYCLE pid SET is_cycle USING path, \
         nodes AS (SELECT pid, min(depth) AS depth, \
                          (array_agg(root_pid ORDER BY depth))[1] AS root_pid \
                   FROM tree GROUP BY pid) \
         SELECT n.pid, n.depth, n.root_pid, \
           COALESCE((SELECT array_agg(DISTINCT b ORDER BY b) \
                     FROM unnest(pg_blocking_pids(n.pid)) b), ARRAY[]::int[]) AS blocked_by, \
           coalesce(a.datid, 0) AS datid, coalesce(a.datname::text, '') AS datname, a.usename::text AS usename, \
           coalesce(a.application_name, '') AS application_name, \
           coalesce(host(a.client_addr), '') AS client_addr, \
           coalesce(a.backend_type, '') AS backend_type, a.state, \
           a.wait_event_type, a.wait_event, \
           coalesce(left(a.query, 5000), '') AS query, \
           age(a.backend_xid)::int8 AS backend_xid_age, \
           age(a.backend_xmin)::int8 AS backend_xmin_age, \
           (extract(epoch FROM a.backend_start) * 1e6)::int8 AS backend_start_us, \
           (extract(epoch FROM a.xact_start) * 1e6)::int8 AS xact_start_us, \
           (extract(epoch FROM a.query_start) * 1e6)::int8 AS query_start_us, \
           (extract(epoch FROM a.state_change) * 1e6)::int8 AS state_change_us, \
           l.locktype AS lock_locktype, l.mode AS lock_mode, l.granted AS lock_granted, \
           l.relation AS lock_relation, c.relname::text AS lock_relname, \
           l.page AS lock_page, l.tuple AS lock_tuple, \
           l.transactionid::text::int8 AS lock_transactionid, l.fastpath AS lock_fastpath, \
           (l.locktype || coalesce(':' || c.relname, '')) AS lock_target, \
           (extract(epoch FROM l.waitstart) * 1e6)::int8 AS waitstart_us, \
           (extract(epoch FROM snap.ts) * 1e6)::int8 AS ts_us \
         FROM nodes n CROSS JOIN snap \
         JOIN pg_stat_activity a ON a.pid = n.pid \
         LEFT JOIN LATERAL (SELECT lk.locktype, lk.mode, lk.granted, lk.relation, lk.page, \
                                   lk.tuple, lk.transactionid, lk.fastpath, lk.waitstart \
                            FROM pg_locks lk WHERE lk.pid = n.pid AND NOT lk.granted LIMIT 1) l ON true \
         LEFT JOIN pg_class c ON c.oid = l.relation \
         ORDER BY n.root_pid, n.depth, n.pid \
         LIMIT $1"
    )
}

const fn locks_query_v1() -> &'static str {
    marked!(
        "WITH RECURSIVE \
         snap AS (SELECT statement_timestamp() AS ts), \
         waiters AS (SELECT a.pid, pg_blocking_pids(a.pid) AS bp \
                     FROM pg_stat_activity a WHERE a.wait_event_type = 'Lock'), \
         edges AS (SELECT w.pid AS waiter, b AS blocker \
                   FROM waiters w, unnest(w.bp) AS b), \
         roots AS (SELECT DISTINCT e.blocker AS pid FROM edges e \
                   WHERE e.blocker <> 0 AND e.blocker NOT IN (SELECT pid FROM waiters)), \
         tree AS (SELECT r.pid, 0 AS depth, r.pid AS root_pid, ARRAY[r.pid] AS path FROM roots r \
                  UNION ALL \
                  SELECT e.waiter, t.depth + 1, t.root_pid, t.path || e.waiter AS path \
                  FROM tree t JOIN edges e ON e.blocker = t.pid \
                  WHERE NOT e.waiter = ANY(t.path)), \
         nodes AS (SELECT pid, min(depth) AS depth, \
                          (array_agg(root_pid ORDER BY depth))[1] AS root_pid \
                   FROM tree GROUP BY pid) \
         SELECT n.pid, n.depth, n.root_pid, \
           COALESCE((SELECT array_agg(DISTINCT b ORDER BY b) \
                     FROM unnest(pg_blocking_pids(n.pid)) b), ARRAY[]::int[]) AS blocked_by, \
           coalesce(a.datid, 0) AS datid, coalesce(a.datname::text, '') AS datname, a.usename::text AS usename, \
           coalesce(a.application_name, '') AS application_name, \
           coalesce(host(a.client_addr), '') AS client_addr, \
           coalesce(a.backend_type, '') AS backend_type, a.state, \
           a.wait_event_type, a.wait_event, \
           coalesce(left(a.query, 5000), '') AS query, \
           age(a.backend_xid)::int8 AS backend_xid_age, \
           age(a.backend_xmin)::int8 AS backend_xmin_age, \
           (extract(epoch FROM a.backend_start) * 1e6)::int8 AS backend_start_us, \
           (extract(epoch FROM a.xact_start) * 1e6)::int8 AS xact_start_us, \
           (extract(epoch FROM a.query_start) * 1e6)::int8 AS query_start_us, \
           (extract(epoch FROM a.state_change) * 1e6)::int8 AS state_change_us, \
           l.locktype AS lock_locktype, l.mode AS lock_mode, l.granted AS lock_granted, \
           l.relation AS lock_relation, c.relname::text AS lock_relname, \
           l.page AS lock_page, l.tuple AS lock_tuple, \
           l.transactionid::text::int8 AS lock_transactionid, l.fastpath AS lock_fastpath, \
           (l.locktype || coalesce(':' || c.relname, '')) AS lock_target, \
           (extract(epoch FROM snap.ts) * 1e6)::int8 AS ts_us \
         FROM nodes n CROSS JOIN snap \
         JOIN pg_stat_activity a ON a.pid = n.pid \
         LEFT JOIN LATERAL (SELECT lk.locktype, lk.mode, lk.granted, lk.relation, lk.page, \
                                   lk.tuple, lk.transactionid, lk.fastpath \
                            FROM pg_locks lk WHERE lk.pid = n.pid AND NOT lk.granted LIMIT 1) l ON true \
         LEFT JOIN pg_class c ON c.oid = l.relation \
         ORDER BY n.root_pid, n.depth, n.pid \
         LIMIT $1"
    )
}

/// Raw row from the collection query (pre-interning).
#[derive(Debug, Clone)]
pub struct LocksRow {
    /// Snapshot time, unix microseconds (server `statement_timestamp()`).
    pub ts: i64,
    /// Backend process id.
    pub pid: i32,
    /// Deduped `pg_blocking_pids(pid)`; empty for roots; may contain `0`.
    pub blocked_by: Vec<i32>,
    /// Distance from a root in the blocking component (`min(depth)`); a
    /// convenience scalar for the primary path, `blocked_by` stays authoritative.
    pub depth: i32,
    /// A root of this node's blocking component.
    pub root_pid: i32,
    /// Database oid of the backend.
    pub datid: u32,
    /// Database name of the backend; empty for backends with no attached db.
    pub datname: String,
    /// Login role; `None` for some background backends.
    pub usename: Option<String>,
    /// `application_name`; empty when unset.
    pub application_name: String,
    /// Client address as text; empty = local.
    pub client_addr: String,
    /// `backend_type`.
    pub backend_type: String,
    /// Session state; `None` for some background backends.
    pub state: Option<String>,
    /// Wait event type; `None` for non-waiting roots.
    pub wait_event_type: Option<String>,
    /// Wait event name.
    pub wait_event: Option<String>,
    /// Current query, truncated in SQL.
    pub query: String,
    /// `age(backend_xid)`; `None` without an assigned xid.
    pub backend_xid_age: Option<i64>,
    /// `age(backend_xmin)`; vacuum-horizon hold.
    pub backend_xmin_age: Option<i64>,
    /// Backend start, unix microseconds.
    pub backend_start: Option<i64>,
    /// Transaction start; `None` outside a transaction.
    pub xact_start: Option<i64>,
    /// Current statement start.
    pub query_start: Option<i64>,
    /// Last state change.
    pub state_change: Option<i64>,
    /// Awaited lock type; `None` for non-waiting roots.
    pub lock_locktype: Option<String>,
    /// Awaited lock mode.
    pub lock_mode: Option<String>,
    /// Whether the awaited lock is granted; always false for the awaited row,
    /// recorded for completeness.
    pub lock_granted: Option<bool>,
    /// Relation oid of the awaited lock.
    pub lock_relation: Option<u32>,
    /// Relation name, resolved only for the connected database.
    pub lock_relname: Option<String>,
    /// Page number of a page/tuple lock target.
    pub lock_page: Option<i32>,
    /// Tuple offset of a tuple lock target.
    pub lock_tuple: Option<i16>,
    /// Transaction id being awaited (row-lock pattern), raw xid.
    pub lock_transactionid: Option<i64>,
    /// Whether the awaited lock was taken via the fast path.
    pub lock_fastpath: Option<bool>,
    /// Human-readable target, best effort.
    pub lock_target: Option<String>,
    /// Lock-wait start (PG14+); `None` on V1 or while not yet timed.
    pub waitstart: Option<i64>,
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

/// Build a `1_011_002` row, interning strings and wrapping timestamps in [`Ts`].
///
/// Maps every field of [`PgLocksV2`]: `blocked_by` passes through, string fields
/// go through `intern`, and `Option<i64>` timestamps become `Option<Ts>`.
///
/// # Errors
/// Returns the interner's error if any string cannot be interned.
pub fn to_v2<E>(
    row: &LocksRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgLocksV2, E> {
    Ok(PgLocksV2 {
        ts: Ts(row.ts),
        pid: row.pid,
        blocked_by: row.blocked_by.clone(),
        depth: row.depth,
        root_pid: row.root_pid,
        datid: row.datid,
        datname: intern(row.datname.as_bytes())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        application_name: intern(row.application_name.as_bytes())?,
        client_addr: intern(row.client_addr.as_bytes())?,
        backend_type: intern(row.backend_type.as_bytes())?,
        state: opt(&mut intern, row.state.as_deref())?,
        wait_event_type: opt(&mut intern, row.wait_event_type.as_deref())?,
        wait_event: opt(&mut intern, row.wait_event.as_deref())?,
        query: intern(row.query.as_bytes())?,
        backend_xid_age: row.backend_xid_age,
        backend_xmin_age: row.backend_xmin_age,
        backend_start: row.backend_start.map(Ts),
        xact_start: row.xact_start.map(Ts),
        query_start: row.query_start.map(Ts),
        state_change: row.state_change.map(Ts),
        lock_locktype: opt(&mut intern, row.lock_locktype.as_deref())?,
        lock_mode: opt(&mut intern, row.lock_mode.as_deref())?,
        lock_granted: row.lock_granted,
        lock_relation: row.lock_relation,
        lock_relname: opt(&mut intern, row.lock_relname.as_deref())?,
        lock_page: row.lock_page,
        lock_tuple: row.lock_tuple,
        lock_transactionid: row.lock_transactionid,
        lock_fastpath: row.lock_fastpath,
        lock_target: opt(&mut intern, row.lock_target.as_deref())?,
        waitstart: row.waitstart.map(Ts),
    })
}

/// Build a `1_011_001` row: identical to [`to_v2`] minus `waitstart`.
///
/// # Errors
/// Returns the interner's error if any string cannot be interned.
pub fn to_v1<E>(
    row: &LocksRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgLocksV1, E> {
    Ok(PgLocksV1 {
        ts: Ts(row.ts),
        pid: row.pid,
        blocked_by: row.blocked_by.clone(),
        depth: row.depth,
        root_pid: row.root_pid,
        datid: row.datid,
        datname: intern(row.datname.as_bytes())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        application_name: intern(row.application_name.as_bytes())?,
        client_addr: intern(row.client_addr.as_bytes())?,
        backend_type: intern(row.backend_type.as_bytes())?,
        state: opt(&mut intern, row.state.as_deref())?,
        wait_event_type: opt(&mut intern, row.wait_event_type.as_deref())?,
        wait_event: opt(&mut intern, row.wait_event.as_deref())?,
        query: intern(row.query.as_bytes())?,
        backend_xid_age: row.backend_xid_age,
        backend_xmin_age: row.backend_xmin_age,
        backend_start: row.backend_start.map(Ts),
        xact_start: row.xact_start.map(Ts),
        query_start: row.query_start.map(Ts),
        state_change: row.state_change.map(Ts),
        lock_locktype: opt(&mut intern, row.lock_locktype.as_deref())?,
        lock_mode: opt(&mut intern, row.lock_mode.as_deref())?,
        lock_granted: row.lock_granted,
        lock_relation: row.lock_relation,
        lock_relname: opt(&mut intern, row.lock_relname.as_deref())?,
        lock_page: row.lock_page,
        lock_tuple: row.lock_tuple,
        lock_transactionid: row.lock_transactionid,
        lock_fastpath: row.lock_fastpath,
        lock_target: opt(&mut intern, row.lock_target.as_deref())?,
    })
}

/// Read a raw row from a result row. Every column but `waitstart_us` is shared;
/// V1 has no `waitstart_us`, so it stays `None`.
fn row_from_pg(row: &tokio_postgres::Row, version: LocksVersion) -> LocksRow {
    LocksRow {
        ts: row.get("ts_us"),
        pid: row.get("pid"),
        blocked_by: row.get("blocked_by"),
        depth: row.get("depth"),
        root_pid: row.get("root_pid"),
        datid: row.get("datid"),
        datname: row.get("datname"),
        usename: row.get("usename"),
        application_name: row.get("application_name"),
        client_addr: row.get("client_addr"),
        backend_type: row.get("backend_type"),
        state: row.get("state"),
        wait_event_type: row.get("wait_event_type"),
        wait_event: row.get("wait_event"),
        query: row.get("query"),
        backend_xid_age: row.get("backend_xid_age"),
        backend_xmin_age: row.get("backend_xmin_age"),
        backend_start: row.get("backend_start_us"),
        xact_start: row.get("xact_start_us"),
        query_start: row.get("query_start_us"),
        state_change: row.get("state_change_us"),
        lock_locktype: row.get("lock_locktype"),
        lock_mode: row.get("lock_mode"),
        lock_granted: row.get("lock_granted"),
        lock_relation: row.get("lock_relation"),
        lock_relname: row.get("lock_relname"),
        lock_page: row.get("lock_page"),
        lock_tuple: row.get("lock_tuple"),
        lock_transactionid: row.get("lock_transactionid"),
        lock_fastpath: row.get("lock_fastpath"),
        lock_target: row.get("lock_target"),
        waitstart: match version {
            LocksVersion::V1 => None,
            LocksVersion::V2 => row.get("waitstart_us"),
        },
    }
}

/// Collect the lock-wait tree. Caller runs [`lock_waits_exist`] first.
///
/// The versioned recursive CTE gathers the blocking component: one row per
/// backend, `blocked_by` edges deduped, capped at `max_rows`.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_locks(
    client: &Client,
    major: u32,
    max_rows: i64,
) -> Result<Vec<LocksRow>, tokio_postgres::Error> {
    let version = locks_version(major);
    let rows = client.query(locks_query(version), &[&max_rows]).await?;
    Ok(rows.iter().map(|row| row_from_pg(row, version)).collect())
}

#[cfg(test)]
mod tests {
    use super::{LocksRow, LocksVersion, locks_query, locks_version, to_v1, to_v2};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    /// A root backend: `blocked_by` empty, no awaited-lock columns.
    fn sample_root_row() -> LocksRow {
        LocksRow {
            ts: 1_000_000,
            pid: 10,
            blocked_by: Vec::new(),
            depth: 0,
            root_pid: 10,
            datid: 16_384,
            datname: "app".to_owned(),
            usename: Some("postgres".to_owned()),
            application_name: "psql".to_owned(),
            client_addr: String::new(),
            backend_type: "client backend".to_owned(),
            state: Some("active".to_owned()),
            wait_event_type: None,
            wait_event: None,
            query: "select 1".to_owned(),
            backend_xid_age: None,
            backend_xmin_age: None,
            backend_start: Some(940_000),
            xact_start: Some(995_000),
            query_start: Some(999_000),
            state_change: Some(999_000),
            lock_locktype: None,
            lock_mode: None,
            lock_granted: None,
            lock_relation: None,
            lock_relname: None,
            lock_page: None,
            lock_tuple: None,
            lock_transactionid: None,
            lock_fastpath: None,
            lock_target: None,
            waitstart: None,
        }
    }

    /// A waiter node with every nullable field populated.
    fn sample_waiter_row() -> LocksRow {
        LocksRow {
            pid: 20,
            blocked_by: vec![0, 10],
            depth: 1,
            wait_event_type: Some("Lock".to_owned()),
            wait_event: Some("relation".to_owned()),
            lock_locktype: Some("relation".to_owned()),
            lock_mode: Some("AccessExclusiveLock".to_owned()),
            lock_granted: Some(false),
            lock_relation: Some(12_345),
            lock_relname: Some("orders".to_owned()),
            lock_page: Some(42),
            lock_tuple: Some(7),
            lock_transactionid: Some(999_999),
            lock_fastpath: Some(false),
            lock_target: Some("relation:orders".to_owned()),
            waitstart: Some(998_000),
            backend_xid_age: Some(7),
            backend_xmin_age: Some(9),
            ..sample_root_row()
        }
    }

    #[test]
    fn version_follows_waitstart_boundary() {
        assert_eq!(locks_version(13), LocksVersion::V1);
        assert_eq!(locks_version(14), LocksVersion::V2);
        assert_eq!(locks_version(18), LocksVersion::V2);
    }

    #[test]
    fn v1_query_uses_manual_cycle_guard_no_waitstart() {
        let q = locks_query(LocksVersion::V1);
        assert!(q.contains("pg_blocking_pids"));
        assert!(q.contains("= ANY(")); // manual path cycle guard
        assert!(!q.contains("waitstart"));
        assert!(q.contains("pg_kronika:")); // marker present
    }

    #[test]
    fn v2_query_has_waitstart_and_cycle_clause() {
        let q = locks_query(LocksVersion::V2);
        assert!(q.contains("waitstart"));
        assert!(q.contains("CYCLE")); // SQL CYCLE clause
        assert!(q.contains("pg_blocking_pids"));
    }

    #[test]
    fn both_queries_take_the_max_rows_bind() {
        assert!(locks_query(LocksVersion::V1).contains("LIMIT $1"));
        assert!(locks_query(LocksVersion::V2).contains("LIMIT $1"));
    }

    /// Deterministic stand-in for the segment interner (assigns ids in order).
    fn counting_intern() -> impl FnMut(&[u8]) -> Result<StrId, Infallible> + use<> {
        let mut ids = std::collections::HashMap::new();
        move |b: &[u8]| {
            let n = ids.len() as u64 + 1;
            Ok(StrId(*ids.entry(b.to_vec()).or_insert(n)))
        }
    }

    #[test]
    fn to_v2_maps_nulls_and_edges() {
        let row = sample_root_row(); // a root, blocked_by empty, awaited-lock None
        let v = to_v2(&row, counting_intern()).unwrap();
        assert_eq!(v.blocked_by, Vec::<i32>::new());
        assert_eq!(v.lock_locktype, None);
        assert_eq!(v.wait_event_type, None);
        assert_eq!(v.waitstart, None);
        assert_eq!(v.pid, 10);
        assert_eq!(v.root_pid, 10);
    }

    #[test]
    fn to_v2_maps_every_populated_field() {
        let mut intern = counting_intern();
        let v = to_v2(&sample_waiter_row(), &mut intern).unwrap();
        assert_eq!(v.blocked_by, vec![0, 10]);
        assert_eq!(v.depth, 1);
        assert_eq!(v.wait_event_type, Some(intern(b"Lock").unwrap()));
        assert_eq!(v.lock_relation, Some(12_345));
        assert_eq!(v.lock_granted, Some(false));
        assert_eq!(v.lock_page, Some(42));
        assert_eq!(v.lock_tuple, Some(7));
        assert_eq!(v.lock_transactionid, Some(999_999));
        assert_eq!(v.lock_fastpath, Some(false));
        assert_eq!(v.waitstart.map(|t| t.0), Some(998_000));
        assert_eq!(v.backend_xid_age, Some(7));
    }

    #[test]
    fn to_v1_matches_v2_without_waitstart() {
        let row = sample_waiter_row();
        let v1 = to_v1(&row, counting_intern()).unwrap();
        let v2 = to_v2(&row, counting_intern()).unwrap();
        assert_eq!(v1.pid, v2.pid);
        assert_eq!(v1.blocked_by, v2.blocked_by);
        assert_eq!(v1.lock_target, v2.lock_target);
        assert_eq!(v1.lock_transactionid, v2.lock_transactionid);
        assert_eq!(v1.lock_granted, v2.lock_granted);
        assert_eq!(v1.lock_page, v2.lock_page);
        assert_eq!(v1.lock_tuple, v2.lock_tuple);
        assert_eq!(v1.lock_fastpath, v2.lock_fastpath);
        assert_eq!(v1.depth, v2.depth);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_v2(&sample_root_row(), boom), Err("full"));
    }
}
