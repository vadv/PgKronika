# `pg_locks` wait tree (`1_011`) implementation notes

## Goal

Record the PostgreSQL lock-wait blocking graph as raw collector data. The
section stores one row per backend in a blocking component and keeps the graph
edges in `blocked_by`, a `list<i32>` column populated from
`pg_blocking_pids(pid)`.

The collector does not assign severity, detect victims, or render the tree.
Those decisions belong to the read side. `depth` and `root_pid` are traversal
fields that help readers render a simple tree; `blocked_by` carries the edge
set.

## Layout

- `1_011_001`: PG 10-13, without `waitstart`.
- `1_011_002`: PG 14-18, with trailing `waitstart`.
- Semantics: `conditional_full`.
- Sort key: `(root_pid, depth, pid)`.
- Scope: instance-wide class A collection from `pool.main()`.

Both layouts include backend context from `pg_stat_activity`, the awaited
`pg_locks` row for waiters, and transaction-age context:

- backend identity: `pid`, `datid`, `datname`, `usename`,
  `application_name`, `client_addr`, `backend_type`;
- wait and activity state: `state`, `wait_event_type`, `wait_event`, `query`;
- transaction context: `backend_xid_age`, `backend_xmin_age`, `backend_start`,
  `xact_start`, `query_start`, `state_change`;
- awaited lock context: `lock_locktype`, `lock_mode`, `lock_granted`,
  `lock_relation`, `lock_relname`, `lock_page`, `lock_tuple`,
  `lock_transactionid`, `lock_fastpath`, `lock_target`;
- PG 14+ wait timing: `waitstart`.

`blocked_by` may contain `0`, PostgreSQL's marker for a prepared-transaction
holder with no live backend row. Such a holder remains an edge endpoint but does
not get its own row in the section.

## Codec work

`blocked_by` required a new registry column type:

- `ColumnType::ListI32`;
- Arrow `List<Int32>` schema support;
- `write_list_i32` and `read_list_i32`;
- `#[derive(Section)]` support for `Vec<i32>` fields;
- codec roundtrip coverage for empty lists, multi-element lists, and `0`.

The list itself is non-nullable. An empty `Vec<i32>` represents a root node.

## Collector query

Collection has two stages:

1. Check for `wait_event_type = 'Lock'` in `pg_stat_activity`.
2. If a lock wait exists, run the version-specific recursive CTE.

The CTE starts from lock-waiting backends, reads each waiter's
`pg_blocking_pids(pid)`, walks blockers transitively to the roots, joins backend
context, and joins the first non-granted `pg_locks` row for the awaited lock.

The PG 14+ query uses SQL `CYCLE`. The PG 10-13 query uses a manual path array
guard. Both queries cap the result with `KRONIKA_PG_MAX_LOCK_ROWS` (default
`1000`), validated at startup against `MAX_SECTION_ROWS`.

## Scope

Included:

- heavyweight lock waits visible through `pg_locks`;
- relation, extend, page, tuple, object, transactionid, and advisory locks;
- row-lock waits represented by the `transactionid` pattern.

Excluded:

- `LWLock` and `BufferPin`, which are wait events in `pg_stat_activity` but not
  `pg_locks` edges;
- `SIReadLock` predicate locks, which do not block another backend directly;
- deadlock interpretation. If a snapshot catches a cycle before PostgreSQL
  resolves it, the stored `blocked_by` edges can contain that cycle.

## Files

- `crates/kronika-registry/src/codec.rs`: `ListI32` Arrow encode/decode helpers.
- `crates/kronika-derive/src/lib.rs`: `Vec<i32>` derive support.
- `crates/kronika-registry/src/codec/pg_locks.rs`: `PgLocksV1` and
  `PgLocksV2` contracts, structs, and codec tests.
- `crates/kronika-source-pg/src/locks.rs`: version selection, precheck,
  recursive queries, raw rows, and row conversion.
- `bins/pg_kronika-collector/src/main.rs`: configuration, cardinality
  validation, collection, and section buffering.
- `crates/kronika-bdd/features/pg_locks.feature` and
  `crates/kronika-bdd/src/main.rs`: live blocking scenario and negative case.
- `docs/type-registry/postgresql.md` and
  `docs/type-registry/postgresql-collection.md`: registry and collection
  contract.

## Verification

- Codec tests cover both layouts and `list<i32>` roundtrips.
- Source unit tests cover the PG 13/14 version boundary, query shape, and row
  conversion.
- Collector unit tests cover `KRONIKA_PG_MAX_LOCK_ROWS` validation and buffering.
- BDD covers a live PG 14-18 row-lock wait and the no-wait negative case.
