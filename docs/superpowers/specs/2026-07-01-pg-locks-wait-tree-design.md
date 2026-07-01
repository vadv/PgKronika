# `pg_locks` wait tree (`1_011`) design

**Goal:** record the PostgreSQL lock-wait blocking graph as raw data. The
read-side analyzer or web reader reconstructs and visualizes the wait tree from
the stored nodes, edges, and lock context. This is an instance-wide (class A)
metric. The section preserves transitive edges, multi-blocker fan-out, and PG14+
`waitstart`.

**Non-goal (read side, out of scope):** tree layout, urgency coloring,
"kill/wait" verdicts, cycle detection, and deadlock reporting. The collector is
a recorder: it stores nodes, edges, and raw lock context; the analyzer or reader
interprets them. The target UI has four views: indented tree, node-link graph,
timeline, and table.

## Architecture

- **Two schema versions** (the only `pg_locks` column change in PG10-18 is
  `waitstart`, added in PG14):
  - `1_011_001` - PG10-13 (no `waitstart`; the read side derives wait duration
    from `state_change` or `query_start`).
  - `1_011_002` - PG14-18 (adds `waitstart`, the recorded lock-wait start).
  The `locktype` value-domain changes (`speculative token` -> `spectoken` in
  PG13, `applytransaction` in PG16) are data changes, not schema forks. They are
  values of an existing column and are recorded verbatim.
- **Section semantics: `conditional_full`** - the section is written only when a
  blocking graph exists. A cheap precheck avoids running the recursive query when
  no lock waits are present.
- **Class A** - one connection sees all backends and locks cluster-wide through
  `pg_stat_activity`, `pg_locks`, and `pg_blocking_pids`. Collection uses
  `pool.main()`; it does not iterate over databases.

## Data model - node-centric graph

`pg_blocking_pids(pid)` returns an array: a waiter can have several blockers, and
a snapshot can contain a cycle before PostgreSQL resolves a deadlock. The section
stores a directed graph, not a single-parent tree. Acyclic snapshots can be
rendered as forests or DAGs; readers should use the edge list as the graph.

- **One row per backend** in the blocking component: waiters and their transitive
  blockers up to the roots, including holders that are not themselves waiting.
- **`blocked_by`** - the full deduplicated `pg_blocking_pids(pid)` value as a
  **`list<i32>` column**. This stores the raw edges and uses registry codec
  support for array columns (see "Codec support"). The array may contain `0`, which
  means a prepared-transaction holder with no live backend.
- **`depth` and `root_pid`** - traversal-derived fields. They let the
  reader build a simple tree layout without client-side recursion. For DAG nodes,
  they describe the primary path; `blocked_by` remains the edge set.
- Because `blocked_by` is `Vec<i32>`, the row struct is `Clone` but not `Copy`
  (unlike rows for other metrics). It can derive `Eq` because it has no `f64`
  fields.

## Scope - graph contents by PostgreSQL lock semantics

**IN** (resolvable heavyweight conflicts that `pg_blocking_pids` handles):

- relation, extend, page, tuple, and object locks;
- **row-level locks through the `transactionid` pattern** - an `UPDATE` waiting
  on a row locked by another open transaction appears as a `granted = false` lock
  with `locktype = 'transactionid'` and `transactionid` equal to the holder's xid.
  This is a common blocking case. `pg_blocking_pids` resolves the edge, and
  recording `locktype`, `transactionid`, `mode`, and the holder's `backend_xid`
  lets the analyzer explain it as a row-lock wait rather than a table-lock wait.
- advisory locks; they are heavyweight locks resolved by `pg_blocking_pids`.

**OUT**:

- `LWLock` and `BufferPin` waits - these are not in `pg_locks` and have no
  resolvable `pg_locks` edge. They are already visible per backend in
  `pg_stat_activity` (metric `1_001`). A backend waiting on `LWLock` is not part
  of this lock graph.
- `SIReadLock` and predicate locks (SSI) - these never block; conflicts abort
  rather than wait. `pg_blocking_pids` ignores them by design. They are filtered.
- **Deadlocks** - periodic snapshots do not report deadlocks. PostgreSQL's
  detector usually breaks the cycle after `deadlock_timeout` (default 1 s), and a
  snapshot cannot identify the victim. Deadlocks belong in logs
  (`log_lock_waits` and the deadlock report). If a cycle is sampled before
  resolution, it appears as mutual `blocked_by` edges; the recursive query keeps
  traversal bounded.

## Columns (`1_011_002` superset; `1_011_001` = same minus `waitstart`)

Everything is a point-in-time snapshot. Column classes are label (`l`), gauge
(`g`), and timestamp (`t`); there are **no counters** (`c`).
`sort_key = (root_pid, depth, pid)`.

Node and backend context (from `pg_stat_activity`):

```text
ts                 ts        T
pid                i32       L
blocked_by         list<i32> L   // deduped pg_blocking_pids; [] for roots; may contain 0 (prepared xact)
depth              i32       G   // distance from a root, from the CTE
root_pid           i32       L   // a root of this node's component
datid              u32       L
datname            str       L
usename            str?      L   // NULL for some background backends
application_name   str       L
client_addr        str       L   // text; empty = local
backend_type       str       L
state              str?      L   // active | idle in transaction | ...
wait_event_type    str?      L   // NULL for non-waiting roots
wait_event         str?      L
query              str       L   // dictionary, truncated on collector
backend_xid_age    i64?      G   // age(backend_xid); NULL without an assigned xid
backend_xmin_age   i64?      G   // age(backend_xmin); vacuum-horizon hold
backend_start      ts?       G
xact_start         ts?       G   // NULL outside a transaction
query_start        ts?       G
state_change       ts?       G
```

Awaited lock (from `pg_locks` where `granted = false` for this pid; all NULL for
non-waiting roots):

```text
lock_locktype      str?      L   // relation | transactionid | tuple | advisory | ...
lock_mode          str?      L   // AccessExclusiveLock | ShareLock | ...
lock_granted       bool?     L   // always false for the awaited row (recorded for completeness)
lock_relation      u32?      L   // relation OID (relation/page/tuple/extend locks)
lock_relname       str?      L   // resolved only for current_database's relations (see limitation)
lock_page          i32?      G
lock_tuple         i16?      G
lock_transactionid i64?      L   // xid being waited on (row-lock pattern); raw xid
lock_fastpath      bool?     L
lock_target        str?      L   // human-readable target, best effort
waitstart          ts?       G   // PG14+ (1_011_002 only); recorded lock-wait start; nullable even when granted = false
```

**Cross-database relname limitation:** `pg_locks` is cluster-wide, but `pg_class`
is per database. A relation lock in another database can be recorded only as
`lock_relation` (OID) plus `datid`; `lock_relname` is resolved only for relations
in the connected database and is NULL otherwise. Raw OID and `datid` are always
recorded. The reader can resolve names if it has per-database catalogs.

## Collection mechanics

Collection has two stages:

1. **Precheck (cheap):** `SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE
   wait_event_type = 'Lock')`. If there are no lock waits, no section is written.
2. **Recursive CTE (only if waits exist):** seed from backends where
   `wait_event_type = 'Lock'`; compute each waiter's `pg_blocking_pids(pid)`;
   climb transitively to the roots; gather every backend in the component; join
   `pg_stat_activity` for node context, `LEFT JOIN pg_locks` for the awaited
   `granted = false` lock, and `LEFT JOIN pg_class`/`pg_database` for available
   names. Apply the max-rows cap with `LIMIT`.

- Call `pg_blocking_pids` only for backends that are waiting on `Lock`, not for
  every backend. It takes exclusive access to the lock manager's shared state.
- Deduplicate each array; parallel queries can duplicate leader PIDs.
- **Cycle-safe:** `1_011_002` (PG14+) uses the SQL `CYCLE` clause; `1_011_001`
  (PG10-13) uses a manual `NOT pid = ANY(path)` guard.
- **PID 0** (prepared-transaction blocker) is kept as an opaque root node.
- **Volume knob:** `KRONIKA_PG_MAX_LOCK_ROWS` (default 1000) caps the section.
  Startup validation keeps it under `MAX_SECTION_ROWS` (65536).
- The query is wrapped in the file's `marked!` macro for SQL transparency.

## Codec support - `list<i32>` columns

`blocked_by` uses an Arrow `List<Int32>` column. This requires:

- a new column class or type in the `#[derive(Section)]` macro for `Vec<i32>`;
- encode (`ListArray` builder) and decode paths in `codec.rs`;
- lint acceptance for the list type;
- golden roundtrip coverage for a list column.

The same support can be reused by future array-shaped metrics.

## Testing

- **Golden codec tests** for `1_011_001` (PG10-13, outside the live matrix):
  contract shape, roundtrip behavior, null preservation, and sort order. Include
  a multi-element `blocked_by`, an empty `blocked_by`, and `0` in the array.
- **Live BDD** for `1_011_002` (PG14-18): a real blocking scenario. Session H
  runs `BEGIN; UPDATE t SET ... WHERE id = 1;` and holds the transaction open.
  Session W runs `UPDATE t SET ... WHERE id = 1;` and blocks on H's
  `transactionid`. After a snapshot, assert that the section contains W with
  `blocked_by = [H.pid]`, H as a root (`blocked_by = []`),
  `lock_locktype = 'transactionid'`, `waitstart` present, and the precheck -> CTE
  path fired. This requires two concurrent sessions with a held-open transaction
  in the BDD harness.
- **Negative live case:** no waits -> no section.

## Recorder invariant

The collector does not compute thresholds, health formulas, "is this dangerous"
answers, cycle verdicts, or deadlock verdicts. `depth`, `root_pid`, any
`is_cycle`-style interpretation, and urgency coloring belong in the analyzer or
reader. The collector records raw nodes, raw `blocked_by` edges, and raw
`pg_locks` context.
