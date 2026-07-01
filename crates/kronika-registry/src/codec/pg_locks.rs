//! Type `1_011_001` / `1_011_002`: `pg_locks` wait tree.
//!
//! Each row represents one backend that participates in a blocking component.
//! The section is node-centric: every backend in a blocking chain has one row,
//! and the directed edges are carried in the `blocked_by` list column
//! (`pg_blocking_pids(pid)` deduplicated). Roots have an empty `blocked_by`.
//! `depth` is the distance from a root in the blocking component (`min(depth)`
//! shortest path); a convenience scalar, `blocked_by` is authoritative.
//! `root_pid` identifies which root anchors this node's blocking component.
//!
//! The section splits into two layout versions because `waitstart` was added to
//! `pg_locks` in PG 14. `PgLocksV2` (PG 14-18) includes `waitstart`;
//! `PgLocksV1` (PG 10-13) is byte-identical minus that trailing field.

use crate::{Section, StrId, Ts};

/// Type `1_011_002`: `pg_locks` wait tree on PG 14-18 (`PgLocksV1` plus `waitstart`).
///
/// One row per backend in a blocking component; `blocked_by` holds the deduped
/// `pg_blocking_pids` edges (`0` = prepared-xact holder).
#[derive(Debug, Clone, PartialEq, Eq, Section)]
#[section(
    id = 1_011_002,
    name = "pg_locks",
    semantics = conditional_full,
    sort_key("root_pid", "depth", "pid")
)]
pub struct PgLocksV2 {
    /// Snapshot time, unix microseconds (server `statement_timestamp()`).
    #[column(t)]
    pub ts: Ts,
    /// Backend process id.
    #[column(l)]
    pub pid: i32,
    /// Deduped `pg_blocking_pids(pid)`; empty for roots; may contain `0`.
    #[column(l)]
    pub blocked_by: Vec<i32>,
    /// Distance from a root in the blocking component (`min(depth)` shortest
    /// path); a convenience scalar, `blocked_by` is authoritative.
    #[column(g)]
    pub depth: i32,
    /// A root of this node's blocking component.
    #[column(l)]
    pub root_pid: i32,
    /// Database oid of the backend.
    #[column(l)]
    pub datid: u32,
    /// Database name of the backend.
    #[column(l)]
    pub datname: StrId,
    /// Login role; `None` for some background backends.
    #[column(l)]
    pub usename: Option<StrId>,
    /// `application_name`.
    #[column(l)]
    pub application_name: StrId,
    /// Client address as text; empty = local.
    #[column(l)]
    pub client_addr: StrId,
    /// `backend_type`.
    #[column(l)]
    pub backend_type: StrId,
    /// Session state; `None` for some background backends.
    #[column(l)]
    pub state: Option<StrId>,
    /// Wait event type; `None` for non-waiting roots.
    #[column(l)]
    pub wait_event_type: Option<StrId>,
    /// Wait event name.
    #[column(l)]
    pub wait_event: Option<StrId>,
    /// Current query (dictionary, truncated in SQL).
    #[column(l)]
    pub query: StrId,
    /// `age(backend_xid)`; `None` without an assigned xid.
    #[column(g)]
    pub backend_xid_age: Option<i64>,
    /// `age(backend_xmin)`; vacuum-horizon hold.
    #[column(g)]
    pub backend_xmin_age: Option<i64>,
    /// Backend start, unix microseconds.
    #[column(g)]
    pub backend_start: Option<Ts>,
    /// Transaction start; `None` outside a transaction.
    #[column(g)]
    pub xact_start: Option<Ts>,
    /// Current statement start.
    #[column(g)]
    pub query_start: Option<Ts>,
    /// Last state change.
    #[column(g)]
    pub state_change: Option<Ts>,
    /// Awaited lock type; `None` for non-waiting roots.
    #[column(l)]
    pub lock_locktype: Option<StrId>,
    /// Awaited lock mode.
    #[column(l)]
    pub lock_mode: Option<StrId>,
    /// Whether the awaited lock is granted; always false for the awaited row,
    /// recorded for completeness.
    #[column(l)]
    pub lock_granted: Option<bool>,
    /// Relation oid of the awaited lock (relation/page/tuple/extend).
    #[column(l)]
    pub lock_relation: Option<u32>,
    /// Relation name, resolved only for the connected database.
    #[column(l)]
    pub lock_relname: Option<StrId>,
    /// Page number of a page/tuple lock target.
    #[column(g)]
    pub lock_page: Option<i32>,
    /// Tuple offset of a tuple lock target.
    #[column(g)]
    pub lock_tuple: Option<i16>,
    /// Transaction id being awaited (row-lock pattern), raw xid.
    #[column(l)]
    pub lock_transactionid: Option<i64>,
    /// Whether the awaited lock was taken via the fast path.
    #[column(l)]
    pub lock_fastpath: Option<bool>,
    /// Human-readable target (rpglot-style), best effort.
    #[column(l)]
    pub lock_target: Option<StrId>,
    /// Lock-wait start (PG14+); nullable even while waiting.
    #[column(g)]
    pub waitstart: Option<Ts>,
}

/// Type `1_011_001`: `pg_locks` wait tree on PG 10-13 (base layout, no
/// `waitstart`). Column meanings match [`PgLocksV2`] for fields present in
/// this layout.
#[derive(Debug, Clone, PartialEq, Eq, Section)]
#[section(
    id = 1_011_001,
    name = "pg_locks",
    semantics = conditional_full,
    sort_key("root_pid", "depth", "pid")
)]
pub struct PgLocksV1 {
    /// Snapshot time, unix microseconds (server `statement_timestamp()`).
    #[column(t)]
    pub ts: Ts,
    /// Backend process id.
    #[column(l)]
    pub pid: i32,
    /// Deduped `pg_blocking_pids(pid)`; empty for roots; may contain `0`.
    #[column(l)]
    pub blocked_by: Vec<i32>,
    /// Distance from a root in the blocking component (`min(depth)` shortest
    /// path); a convenience scalar, `blocked_by` is authoritative.
    #[column(g)]
    pub depth: i32,
    /// A root of this node's blocking component.
    #[column(l)]
    pub root_pid: i32,
    /// Database oid of the backend.
    #[column(l)]
    pub datid: u32,
    /// Database name of the backend.
    #[column(l)]
    pub datname: StrId,
    /// Login role; `None` for some background backends.
    #[column(l)]
    pub usename: Option<StrId>,
    /// `application_name`.
    #[column(l)]
    pub application_name: StrId,
    /// Client address as text; empty = local.
    #[column(l)]
    pub client_addr: StrId,
    /// `backend_type`.
    #[column(l)]
    pub backend_type: StrId,
    /// Session state; `None` for some background backends.
    #[column(l)]
    pub state: Option<StrId>,
    /// Wait event type; `None` for non-waiting roots.
    #[column(l)]
    pub wait_event_type: Option<StrId>,
    /// Wait event name.
    #[column(l)]
    pub wait_event: Option<StrId>,
    /// Current query (dictionary, truncated in SQL).
    #[column(l)]
    pub query: StrId,
    /// `age(backend_xid)`; `None` without an assigned xid.
    #[column(g)]
    pub backend_xid_age: Option<i64>,
    /// `age(backend_xmin)`; vacuum-horizon hold.
    #[column(g)]
    pub backend_xmin_age: Option<i64>,
    /// Backend start, unix microseconds.
    #[column(g)]
    pub backend_start: Option<Ts>,
    /// Transaction start; `None` outside a transaction.
    #[column(g)]
    pub xact_start: Option<Ts>,
    /// Current statement start.
    #[column(g)]
    pub query_start: Option<Ts>,
    /// Last state change.
    #[column(g)]
    pub state_change: Option<Ts>,
    /// Awaited lock type; `None` for non-waiting roots.
    #[column(l)]
    pub lock_locktype: Option<StrId>,
    /// Awaited lock mode.
    #[column(l)]
    pub lock_mode: Option<StrId>,
    /// Whether the awaited lock is granted; always false for the awaited row,
    /// recorded for completeness.
    #[column(l)]
    pub lock_granted: Option<bool>,
    /// Relation oid of the awaited lock (relation/page/tuple/extend).
    #[column(l)]
    pub lock_relation: Option<u32>,
    /// Relation name, resolved only for the connected database.
    #[column(l)]
    pub lock_relname: Option<StrId>,
    /// Page number of a page/tuple lock target.
    #[column(g)]
    pub lock_page: Option<i32>,
    /// Tuple offset of a tuple lock target.
    #[column(g)]
    pub lock_tuple: Option<i16>,
    /// Transaction id being awaited (row-lock pattern), raw xid.
    #[column(l)]
    pub lock_transactionid: Option<i64>,
    /// Whether the awaited lock was taken via the fast path.
    #[column(l)]
    pub lock_fastpath: Option<bool>,
    /// Human-readable target (rpglot-style), best effort.
    #[column(l)]
    pub lock_target: Option<StrId>,
}

#[cfg(test)]
mod tests {
    use super::{PgLocksV1, PgLocksV2};
    use crate::{Section, StrId, Ts, VerifiedSection, lint};

    /// A root backend (not blocked): no waiter columns populated.
    fn v2_row(ts: i64, pid: i32, root_pid: i32) -> PgLocksV2 {
        PgLocksV2 {
            ts: Ts(ts),
            pid,
            blocked_by: vec![],
            depth: 0,
            root_pid,
            datid: 16_384,
            datname: StrId(1),
            usename: Some(StrId(2)),
            application_name: StrId(3),
            client_addr: StrId(4),
            backend_type: StrId(5),
            state: Some(StrId(6)),
            wait_event_type: None,
            wait_event: None,
            query: StrId(7),
            backend_xid_age: None,
            backend_xmin_age: None,
            backend_start: Some(Ts(ts - 60_000_000)),
            xact_start: Some(Ts(ts - 5_000_000)),
            query_start: Some(Ts(ts - 1_000_000)),
            state_change: Some(Ts(ts - 1_000_000)),
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

    #[test]
    fn v2_contract_shape() {
        let c = PgLocksV2::CONTRACT;
        assert_eq!(c.type_id.get(), 1_011_002);
        assert_eq!(c.columns.len(), 32);
        assert_eq!(c.sort_key, ["root_pid", "depth", "pid"]);
        assert_eq!(c.column("ts").map(|col| col.nullable), Some(false));
        assert_eq!(
            c.column("blocked_by").map(|col| col.ty),
            Some(crate::ColumnType::ListI32)
        );
        assert!(c.column("waitstart").is_some());
        assert_eq!(
            c.column("wait_event_type").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(
            c.column("lock_granted").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::Bool, true))
        );
        assert_eq!(
            c.column("lock_page").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::I32, true))
        );
        assert_eq!(
            c.column("lock_tuple").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::I16, true))
        );
        assert_eq!(
            c.column("lock_fastpath").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::Bool, true))
        );
        assert_eq!(lint(&[c]), Ok(()));
    }

    #[test]
    fn v1_drops_waitstart() {
        let c = PgLocksV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_011_001);
        assert_eq!(c.columns.len(), 31);
        assert!(c.column("waitstart").is_none());
        assert!(c.column("blocked_by").is_some());
        assert_eq!(
            c.column("lock_granted").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::Bool, true))
        );
        assert_eq!(
            c.column("lock_page").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::I32, true))
        );
        assert_eq!(
            c.column("lock_tuple").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::I16, true))
        );
        assert_eq!(
            c.column("lock_fastpath").map(|col| (col.ty, col.nullable)),
            Some((crate::ColumnType::Bool, true))
        );
        assert_eq!(lint(&[c]), Ok(()));
    }

    #[test]
    fn v2_roundtrip() {
        // Rows already in sort order (root_pid, depth, pid): (10,0,10), (10,1,20).
        let root = v2_row(1_000_000, 10, 10);
        let mut waiter = v2_row(1_000_000, 20, 10);
        waiter.blocked_by = vec![10, 0]; // multi-element with 0
        waiter.depth = 1;
        waiter.wait_event_type = Some(StrId(8));
        waiter.wait_event = Some(StrId(9));
        waiter.lock_locktype = Some(StrId(10));
        waiter.lock_mode = Some(StrId(11));
        waiter.lock_granted = Some(false);
        waiter.lock_relation = Some(12_345);
        waiter.lock_relname = Some(StrId(12));
        waiter.lock_page = Some(42);
        waiter.lock_tuple = Some(7);
        waiter.lock_transactionid = Some(999_999);
        waiter.lock_fastpath = Some(false);
        waiter.lock_target = Some(StrId(13));
        waiter.waitstart = Some(Ts(999_000));
        crate::assert_roundtrips(&[root, waiter]);
    }

    #[test]
    fn v2_roundtrip_empty_and_zero_blocked_by() {
        // Root has empty blocked_by; isolated has vec![0].
        let root = v2_row(2_000_000, 5, 5);
        let mut solo = v2_row(2_000_000, 7, 7);
        solo.blocked_by = vec![0];
        // (root_pid=5 < root_pid=7) so root sorts first.
        crate::assert_roundtrips(&[root, solo]);
    }

    #[test]
    fn v2_encode_sorts_by_root_depth_pid() {
        let rows = [
            v2_row(1_000_000, 30, 10), // root_pid=10, depth=0, pid=30
            v2_row(1_000_000, 10, 10), // root_pid=10, depth=0, pid=10
            v2_row(1_000_000, 5, 5),   // root_pid=5,  depth=0, pid=5
        ];
        let bytes = PgLocksV2::encode(&rows).expect("encode");
        let decoded = PgLocksV2::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(
            decoded
                .iter()
                .map(|r| (r.root_pid, r.depth, r.pid))
                .collect::<Vec<_>>(),
            [(5, 0, 5), (10, 0, 10), (10, 0, 30)]
        );
    }

    #[test]
    fn v2_nullable_awaited_lock_columns_roundtrip() {
        let with_lock = {
            let mut r = v2_row(1_000_000, 99, 99);
            r.waitstart = Some(Ts(500_000));
            r.lock_locktype = Some(StrId(20));
            r.lock_mode = Some(StrId(21));
            r.lock_granted = Some(false);
            r.lock_relation = Some(54_321);
            r.lock_page = Some(3);
            r.lock_tuple = Some(11);
            r.lock_transactionid = Some(42);
            r.lock_fastpath = Some(false);
            r
        };
        let without_lock = v2_row(1_000_000, 100, 100);

        // Sort order: root_pid 99 < 100.
        let bytes = PgLocksV2::encode(&[with_lock.clone(), without_lock.clone()]).expect("encode");
        let decoded = PgLocksV2::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0], with_lock);
        assert_eq!(decoded[1], without_lock);
        assert_eq!(decoded[0].waitstart, Some(Ts(500_000)));
        assert_eq!(decoded[1].waitstart, None);
        assert_eq!(decoded[0].lock_relation, Some(54_321));
        assert_eq!(decoded[1].lock_relation, None);
        assert_eq!(decoded[0].lock_granted, Some(false));
        assert_eq!(decoded[1].lock_granted, None);
        assert_eq!(decoded[0].lock_page, Some(3));
        assert_eq!(decoded[1].lock_page, None);
        assert_eq!(decoded[0].lock_tuple, Some(11));
        assert_eq!(decoded[1].lock_tuple, None);
        assert_eq!(decoded[0].lock_fastpath, Some(false));
        assert_eq!(decoded[1].lock_fastpath, None);
    }
}
