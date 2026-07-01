# pg_locks wait-tree (1_011) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** collect the PostgreSQL lock-wait blocking graph (type_id `1_011`) as raw
data — one row per backend in a blocking component, with a `blocked_by` edge array
— so the read side can reconstruct and visualize the wait tree.

**Architecture:** class-A metric collected from `pool.main()`, `conditional_full`
(section written only when lock waits exist). Two schema versions split on the
PG14 `waitstart` column. A cheap `EXISTS` precheck gates a cycle-safe recursive
CTE over `pg_blocking_pids`. Requires a new codec column kind — `list<i32>`
(Arrow `List<Int32>`) — for the `blocked_by` array.

**Tech Stack:** Rust (edition 2024, MSRV 1.96), tokio-postgres, arrow/parquet via
the `kronika-registry` codec + `#[derive(Section)]` (crate `kronika-derive`),
cucumber BDD.

**Design source:** `docs/superpowers/specs/2026-07-01-pg-locks-wait-tree-design.md`.

## Global Constraints

- PostgreSQL majors PG10-18. Live BDD covers PG14-18 (`1_011_002`); PG10-13
  (`1_011_001`) covered by golden codec tests.
- Recorder principle: raw data only. No thresholds, health verdicts, cycle/deadlock
  interpretation. `depth`/`root_pid` are structural conveniences, not verdicts.
- Column class is load-bearing: `l` = label, `g` = gauge, `t` = `ts` (must be named
  `ts`, non-nullable `Ts`). This metric has no counters (`c`).
- NULL means "not applicable" (non-waiting root has no awaited lock; xid age NULL
  without an assigned xid). Never coalesce such cases to 0.
- Every SQL literal wrapped in the file's `marked!` macro.
- Code and doc-comments in English; commit messages and registry docs in Russian.
  No `Co-Authored-By` line.
- After each task: `export PATH="$HOME/.cargo/bin:$PATH"`, then
  `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `cargo run -p xtask -- check-deps` — all green.
- Branch: `feat/pg-locks-wait-tree` (already created, spec committed).

---

## File Structure

- Modify `crates/kronika-derive/src/lib.rs` — teach `#[derive(Section)]` a
  `Vec<i32>` field → `ListI32` column.
- Modify `crates/kronika-registry/src/contract.rs` — `ColumnType::ListI32`
  variant; `build_arrow_schema` mapping lives in codec.rs.
- Modify `crates/kronika-registry/src/codec.rs` — `List<Int32>` in schema build,
  `write_list_i32` / `read_list_i32` helpers, re-exports.
- Modify `crates/kronika-registry/src/lib.rs` — re-export the new helpers + the
  two `pg_locks` contracts; register both in `registry()`.
- Create `crates/kronika-registry/src/codec/pg_locks.rs` — `PgLocksV1`
  (`1_011_001`) and `PgLocksV2` (`1_011_002`) `#[derive(Section)]` structs +
  contract/roundtrip tests.
- Modify `crates/kronika-registry/src/codec.rs` — `pub mod pg_locks;`.
- Create `crates/kronika-source-pg/src/locks.rs` — version enum, two-stage query,
  `LocksRow`, `to_v1`/`to_v2`, `lock_waits_exist`, `collect_locks` + unit tests.
- Modify `crates/kronika-source-pg/src/lib.rs` — `pub mod locks;`.
- Modify `bins/pg_kronika-collector/src/main.rs` — `KRONIKA_PG_MAX_LOCK_ROWS`,
  cardinality validation, `collect_locks` in `snapshot_and_seal`, `push_locks`.
- Modify `crates/kronika-bdd/src/main.rs` + `features/*.feature` — a live blocking
  scenario + a negative case; type-id consts + decode.
- Modify `docs/type-registry/postgresql.md` + `postgresql-collection.md`.

---

## Task 1: Codec `list<i32>` column support

Adds a new column kind backed by `Vec<i32>`, stored as an Arrow `List<Int32>` (a
non-null list; an empty vector is an empty list, not NULL). Reusable by future
array-shaped metrics.

**Files:**
- Modify: `crates/kronika-registry/src/contract.rs` (add `ColumnType::ListI32`)
- Modify: `crates/kronika-registry/src/codec.rs` (schema + helpers)
- Modify: `crates/kronika-registry/src/lib.rs` (re-exports)
- Modify: `crates/kronika-derive/src/lib.rs` (type mapping + encode/decode gen)
- Test: `crates/kronika-registry/src/codec.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `ColumnType::ListI32`; `kronika_registry::write_list_i32(impl Iterator<Item = Vec<i32>>) -> ArrayRef`; `kronika_registry::read_list_i32(&RecordBatch, &'static str) -> Result<ListColumn, CodecError>` with `ListColumn::value(usize) -> Vec<i32>`. The derive maps a `Vec<i32>` field (class `l`) to this column.

- [ ] **Step 1: Add the `ColumnType::ListI32` variant + failing schema test**

In `crates/kronika-registry/src/contract.rs`, add to the `ColumnType` enum
(after `StrId`):

```rust
    /// A list of `i32` (Arrow `List<Int32>`); an empty list is not NULL.
    ListI32,
```

In `crates/kronika-registry/src/codec.rs` `#[cfg(test)]` module add:

```rust
#[test]
fn list_i32_roundtrips() {
    use arrow_array::RecordBatch;
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    let arr = write_list_i32(vec![vec![1, 2, 3], vec![], vec![0, 7]].into_iter());
    let field = Field::new(
        "blocked_by",
        DataType::List(Arc::new(Field::new("item", DataType::Int32, true))),
        false,
    );
    let batch = RecordBatch::try_new(Arc::new(Schema::new(vec![field])), vec![arr]).unwrap();
    let col = read_list_i32(&batch, "blocked_by").unwrap();
    assert_eq!(col.value(0), vec![1, 2, 3]);
    assert_eq!(col.value(1), Vec::<i32>::new());
    assert_eq!(col.value(2), vec![0, 7]);
}
```

- [ ] **Step 2: Run the test, verify it fails to compile**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p kronika-registry list_i32_roundtrips`
Expected: compile error — `write_list_i32` / `read_list_i32` not found.

- [ ] **Step 3: Implement the encode/decode helpers**

In `crates/kronika-registry/src/codec.rs`, add (near `write_required`):

```rust
use arrow_array::{ListArray, builder::{Int32Builder, ListBuilder}};

/// Build an Arrow `List<Int32>` column, one list per row (empty vec = empty list).
#[must_use]
pub fn write_list_i32(rows: impl Iterator<Item = Vec<i32>>) -> ArrayRef {
    let mut b = ListBuilder::new(Int32Builder::new());
    for row in rows {
        for v in row {
            b.values().append_value(v);
        }
        b.append(true);
    }
    Arc::new(b.finish())
}

/// A decoded `List<Int32>` column.
pub struct ListColumn<'a> {
    array: &'a ListArray,
}

impl ListColumn<'_> {
    /// The list at row `i` as an owned `Vec<i32>`.
    #[must_use]
    pub fn value(&self, i: usize) -> Vec<i32> {
        let values = self.array.value(i);
        let ints = values
            .as_any()
            .downcast_ref::<PrimitiveArray<arrow_array::types::Int32Type>>()
            .expect("list child is Int32");
        (0..ints.len()).map(|j| ints.value(j)).collect()
    }
}

/// Borrow a `List<Int32>` column by name.
pub fn read_list_i32<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> Result<ListColumn<'a>, CodecError> {
    let column = batch
        .column_by_name(name)
        .ok_or(CodecError::MissingColumn { name })?;
    let array = column
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or(CodecError::ColumnType { name })?;
    Ok(ListColumn { array })
}
```

In `build_arrow_schema` add the match arm:

```rust
                ColumnType::ListI32 => DataType::List(Arc::new(Field::new(
                    "item",
                    DataType::Int32,
                    true,
                ))),
```

In `crates/kronika-registry/src/lib.rs`, add `write_list_i32, read_list_i32,
ListColumn` to the `pub use codec::{...}` list.

- [ ] **Step 4: Run the helper test, verify it passes**

Run: `cargo test -p kronika-registry list_i32_roundtrips`
Expected: PASS. (Remove the throwaway last line of the test once green.)

- [ ] **Step 5: Teach the derive macro the `Vec<i32>` field**

In `crates/kronika-derive/src/lib.rs`:

`unwrap_option`/`type_ident` extract a single ident; `Vec<i32>` is not a bare
ident. Add a dedicated detector. In `parse_column`, after `let (inner, nullable)
= unwrap_option(&field.ty);`, branch:

```rust
    if is_vec_i32(inner) {
        return Ok(ColumnDef {
            name: field_ident.to_string(),
            field: field_ident,
            column_type: Ident::new("ListI32", proc_macro2::Span::call_site()),
            column_class: column_class(&class_attr.parse_args()?)?,
            arrow_type: None,
            wrapper: None,
            nullable: false, // a list is never NULL; empty vec = empty list
        });
    }
```

Add the helper:

```rust
/// True for a `Vec<i32>` field type.
fn is_vec_i32(ty: &syn::Type) -> bool {
    let syn::Type::Path(p) = ty else { return false };
    let Some(seg) = p.path.segments.last() else { return false };
    if seg.ident != "Vec" { return false }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else { return false };
    matches!(args.args.first(), Some(syn::GenericArgument::Type(inner)) if type_ident(inner).map(|i| i == "i32").unwrap_or(false))
}
```

In `build_encode`, add a leading match on `column_type == "ListI32"`:

```rust
        if c.column_type == "ListI32" {
            let field = &c.field;
            return quote! {
                ::kronika_registry::write_list_i32(rows.iter().map(|r| r.#field.clone()))
            };
        }
```

(Restructure the `map(...)` closure body so the `ListI32` case returns before the
existing `match (&c.arrow_type, c.nullable)`.)

In `build_decode`, in the `bindings` map, add:

```rust
        if c.column_type == "ListI32" {
            let name = &c.name;
            return quote! { let #col = ::kronika_registry::read_list_i32(#batch, #name)?; };
        }
```

and in the `cells` map:

```rust
        if c.column_type == "ListI32" {
            let field = &c.field;
            return quote! { #field: #col.value(#idx) };
        }
```

- [ ] **Step 6: Add a derive-level roundtrip test**

In `crates/kronika-registry/src/codec.rs` `#[cfg(test)]`, define a tiny section
that uses the derive and roundtrips (proves macro + codec agree). Reuse
`crate::assert_roundtrips` if present; otherwise encode+decode inline:

```rust
#[test]
fn derive_list_i32_section_roundtrips() {
    use crate::{Section, Ts};
    #[derive(Debug, Clone, PartialEq, Eq, crate::Section)]
    #[section(id = 9_999_001, name = "list_probe", semantics = snapshot_full, sort_key("ts"))]
    struct Probe {
        #[column(t)] ts: Ts,
        #[column(l)] edges: Vec<i32>,
    }
    let rows = vec![
        Probe { ts: Ts(10), edges: vec![1, 2] },
        Probe { ts: Ts(20), edges: vec![] },
    ];
    let section = Probe::encode(&rows).unwrap();
    let verified = crate::test_verify(section); // or the crate's verify helper
    let back = Probe::decode(verified).unwrap();
    assert_eq!(back, rows);
}
```

If a `test_verify`/encode helper name differs, mirror an existing codec roundtrip
test in the same file.

- [ ] **Step 7: Run full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p kronika-registry && cargo run -p xtask -- check-deps`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/kronika-registry crates/kronika-derive
git commit -m "codec: поддержка колонки list<i32> (Arrow List<Int32>)" \
  -m "Новый вид колонки Vec<i32> → List<Int32> в #[derive(Section)] + encode/decode. Нужен для blocked_by в дереве ожиданий (1_011); переиспользуем в будущих array-метриках."
```

---

## Task 2: Codec structs `1_011_001` / `1_011_002`

**Files:**
- Create: `crates/kronika-registry/src/codec/pg_locks.rs`
- Modify: `crates/kronika-registry/src/codec.rs` (`pub mod pg_locks;`)
- Modify: `crates/kronika-registry/src/lib.rs` (re-export + `registry()`)

**Interfaces:**
- Produces: `PgLocksV1` (`1_011_001`), `PgLocksV2` (`1_011_002`), both `Section`.
  `PgLocksV2` = `PgLocksV1` + a trailing `waitstart: Option<Ts>` gauge.

- [ ] **Step 1: Write the failing contract tests**

Create `crates/kronika-registry/src/codec/pg_locks.rs` with a `#[cfg(test)] mod
tests` mirroring another codec file's contract tests. Assertions:

```rust
#[test]
fn v2_contract_shape() {
    let c = PgLocksV2::CONTRACT;
    assert_eq!(c.type_id.get(), 1_011_002);
    assert_eq!(c.columns.len(), 27);
    assert_eq!(c.sort_key, ["root_pid", "depth", "pid"]);
    assert_eq!(c.column("ts").map(|col| col.nullable), Some(false));
    assert_eq!(c.column("blocked_by").map(|col| col.ty), Some(crate::ColumnType::ListI32));
    assert!(c.column("waitstart").is_some());
    assert_eq!(c.column("wait_event_type").map(|col| col.nullable), Some(true));
    assert_eq!(lint(&[c]), Ok(()));
}

#[test]
fn v1_drops_waitstart() {
    let c = PgLocksV1::CONTRACT;
    assert_eq!(c.type_id.get(), 1_011_001);
    assert_eq!(c.columns.len(), 26);
    assert!(c.column("waitstart").is_none());
    assert!(c.column("blocked_by").is_some());
    assert_eq!(lint(&[c]), Ok(()));
}
```

Add a `v2_row(ts, pid, root_pid)` builder and a `v2_roundtrip` /
`v2_encode_sorts_by_root_depth_pid` test mirroring
`pg_stat_user_indexes.rs`/`pg_stat_database.rs` roundtrip tests, exercising a
multi-element `blocked_by`, an empty one, and `0` in the array, plus `Some`/`None`
on the awaited-lock columns.

- [ ] **Step 2: Run, verify it fails to compile**

Run: `cargo test -p kronika-registry pg_locks`
Expected: compile error — `PgLocksV2` not found.

- [ ] **Step 3: Write the two structs**

Header doc-comment (English) explaining node-centric graph + the PG14 `waitstart`
split. Then `PgLocksV2` (the superset). Note: `Vec<i32>` makes the struct
`Clone` but not `Copy`.

```rust
use crate::{Section, StrId, Ts};

/// Type `1_011_002`: `pg_locks` wait tree on PG14-18 (`PgLocksV1` plus
/// `waitstart`). One row per backend in a blocking component; `blocked_by`
/// holds the deduped `pg_blocking_pids` edges (`0` = prepared-xact holder).
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
    /// Distance from a root along the primary path.
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
    /// Relation oid of the awaited lock (relation/page/tuple/extend).
    #[column(l)]
    pub lock_relation: Option<u32>,
    /// Relation name, resolved only for the connected database.
    #[column(l)]
    pub lock_relname: Option<StrId>,
    /// Transaction id being awaited (row-lock pattern), raw xid.
    #[column(l)]
    pub lock_transactionid: Option<i64>,
    /// Human-readable target (rpglot-style), best effort.
    #[column(l)]
    pub lock_target: Option<StrId>,
    /// Lock-wait start (PG14+); nullable even while waiting.
    #[column(g)]
    pub waitstart: Option<Ts>,
}
```

`PgLocksV1` is byte-identical minus the final `waitstart` field, with
`id = 1_011_001`. (Both structs live in this file; the reader sees them together,
so the deletion is explicit, not a cross-reference.)

Confirm the column count: V2 has 27 columns as written above; V1 has 26.
Adjust the `assert_eq!` counts in Step 1 if you add/remove a field.

- [ ] **Step 4: Register the contracts**

In `crates/kronika-registry/src/codec.rs` add `pub mod pg_locks;`.
In `crates/kronika-registry/src/lib.rs` add `pg_locks::{PgLocksV1, PgLocksV2}` to
the `pub use codec::{...}` list, and add both `CONTRACT`s to `registry()`.

- [ ] **Step 5: Run tests, verify pass**

Run: `cargo test -p kronika-registry pg_locks && cargo test -p kronika-registry the_registry_is_clean`
Expected: PASS (new type ids registered, lint clean, roundtrips pass).

- [ ] **Step 6: Full gate + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p kronika-registry
git add crates/kronika-registry
git commit -m "codec: структуры дерева ожиданий 1_011_001/1_011_002" \
  -m "Node-центричные строки pg_locks: blocked_by (list<i32>), depth/root_pid, контекст backend, ожидаемый лок. V2 (PG14-18) = V1 (PG10-13) + waitstart. conditional_full, sort(root_pid,depth,pid)."
```

---

## Task 3: Source SQL + collection (`crates/kronika-source-pg/src/locks.rs`)

**Files:**
- Create: `crates/kronika-source-pg/src/locks.rs`
- Modify: `crates/kronika-source-pg/src/lib.rs` (`pub mod locks;`)

**Interfaces:**
- Consumes: `PgLocksV1`/`PgLocksV2` from Task 2; `StrId`, `Ts`.
- Produces: `LocksVersion` (`V1`/`V2`), `locks_version(major: u32) -> LocksVersion`,
  `lock_waits_exist(&Client) -> Result<bool, Error>`,
  `collect_locks(&Client, major: u32, max_rows: i64) -> Result<Vec<LocksRow>, Error>`,
  `to_v1`/`to_v2(&LocksRow, intern) -> Result<PgLocksVN, E>`.

- [ ] **Step 1: Write pure-function unit tests first**

In `crates/kronika-source-pg/src/locks.rs` `#[cfg(test)]`:

```rust
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
fn to_v2_maps_nulls_and_edges() {
    let mut ids = std::collections::HashMap::new();
    let mut intern = |b: &[u8]| -> Result<StrId, std::convert::Infallible> {
        let n = ids.len() as u64 + 1;
        Ok(StrId(*ids.entry(b.to_vec()).or_insert(n)))
    };
    let row = sample_root_row(); // helper: a root, blocked_by empty, awaited-lock None
    let v = to_v2(&row, &mut intern).unwrap();
    assert_eq!(v.blocked_by, Vec::<i32>::new());
    assert_eq!(v.lock_locktype, None);
    assert_eq!(v.wait_event_type, None);
}
```

- [ ] **Step 2: Run, verify fail to compile**

Run: `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p kronika-source-pg locks`
Expected: compile error.

- [ ] **Step 3: Write the marker macro, version enum, and precheck**

```rust
//! PostgreSQL lock-wait tree collection (type 1_011).
//!
//! Class A: one connection sees all backends cluster-wide. Two-stage — a cheap
//! `EXISTS` precheck gates a cycle-safe recursive CTE over `pg_blocking_pids`.
//! The collector records raw nodes + `blocked_by` edges; the read side builds and
//! interprets the tree.

use tokio_postgres::Client;
use kronika_registry::{StrId, Ts};

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

/// Pick the layout from the server major.
#[must_use]
pub const fn locks_version(major: u32) -> LocksVersion {
    if major >= 14 { LocksVersion::V2 } else { LocksVersion::V1 }
}

/// Cheap precheck: are any backends waiting on a heavyweight lock?
///
/// # Errors
/// Propagates the query error.
pub async fn lock_waits_exist(client: &Client) -> Result<bool, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event_type = 'Lock') AS waiting"),
            &[],
        )
        .await?;
    Ok(row.get("waiting"))
}
```

- [ ] **Step 4: Write the two recursive-CTE queries**

`root_pid`/`depth` come from a recursion seeded at the roots (blockers that are
not themselves lock-waiters), descending to waiters. `blocked_by` is the deduped
`pg_blocking_pids` per node. PID `0` (prepared-xact blocker) appears inside
`blocked_by` arrays but is not a node row (no `pg_stat_activity` row). `$1` =
max rows.

```rust
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
           a.datid, a.datname::text AS datname, a.usename::text AS usename, \
           a.application_name AS application_name, host(a.client_addr) AS client_addr, \
           a.backend_type, a.state, a.wait_event_type, a.wait_event, \
           left(a.query, 5000) AS query, \
           age(a.backend_xid)::int8 AS backend_xid_age, \
           age(a.backend_xmin)::int8 AS backend_xmin_age, \
           (extract(epoch FROM a.backend_start) * 1e6)::int8 AS backend_start_us, \
           (extract(epoch FROM a.xact_start) * 1e6)::int8 AS xact_start_us, \
           (extract(epoch FROM a.query_start) * 1e6)::int8 AS query_start_us, \
           (extract(epoch FROM a.state_change) * 1e6)::int8 AS state_change_us, \
           l.locktype AS lock_locktype, l.mode AS lock_mode, l.relation AS lock_relation, \
           c.relname::text AS lock_relname, l.transactionid::text::int8 AS lock_transactionid, \
           (l.locktype || coalesce(':' || c.relname, '')) AS lock_target, \
           (extract(epoch FROM l.waitstart) * 1e6)::int8 AS waitstart_us, \
           (extract(epoch FROM snap.ts) * 1e6)::int8 AS ts_us \
         FROM nodes n CROSS JOIN snap \
         JOIN pg_stat_activity a ON a.pid = n.pid \
         LEFT JOIN LATERAL (SELECT lk.locktype, lk.mode, lk.relation, lk.transactionid, lk.waitstart \
                            FROM pg_locks lk WHERE lk.pid = n.pid AND NOT lk.granted LIMIT 1) l ON true \
         LEFT JOIN pg_class c ON c.oid = l.relation \
         ORDER BY n.root_pid, n.depth, n.pid \
         LIMIT $1"
    )
}
```

`locks_query_v1()` is the same string with two edits: replace the recursion's
`CYCLE pid SET is_cycle USING path,` line with a manual guard — add
`, ARRAY[r.pid] AS path` / `, t.path || e.waiter AS path` to the two `tree`
branches and `WHERE NOT e.waiter = ANY(t.path)` on the recursive branch — and
drop the `waitstart` line + its `lock_target`/`l.waitstart` reference
(`waitstart_us` becomes `NULL::int8 AS waitstart_us` so the row shape is stable;
`to_v1` ignores it).

> Note: the exact CTE will be validated by the live BDD (Task 5). If PG rejects a
> construct, adjust here and re-run BDD; the column set and marker are pinned by
> the unit tests above.

- [ ] **Step 5: Write `LocksRow`, `to_v1`/`to_v2`, and `collect_locks`**

```rust
/// Raw row from the collection query (pre-interning).
#[derive(Debug, Clone)]
pub struct LocksRow {
    pub ts: i64,
    pub pid: i32,
    pub blocked_by: Vec<i32>,
    pub depth: i32,
    pub root_pid: i32,
    pub datid: u32,
    pub datname: String,
    pub usename: Option<String>,
    pub application_name: String,
    pub client_addr: String,
    pub backend_type: String,
    pub state: Option<String>,
    pub wait_event_type: Option<String>,
    pub wait_event: Option<String>,
    pub query: String,
    pub backend_xid_age: Option<i64>,
    pub backend_xmin_age: Option<i64>,
    pub backend_start: Option<i64>,
    pub xact_start: Option<i64>,
    pub query_start: Option<i64>,
    pub state_change: Option<i64>,
    pub lock_locktype: Option<String>,
    pub lock_mode: Option<String>,
    pub lock_relation: Option<u32>,
    pub lock_relname: Option<String>,
    pub lock_transactionid: Option<i64>,
    pub lock_target: Option<String>,
    pub waitstart: Option<i64>,
}
```

`to_v2` maps every field, interning the string fields and wrapping timestamps in
`Ts` (`Option<i64>` → `Option<Ts>` via `.map(Ts)`), `blocked_by` cloned through.
`to_v1` is identical minus `waitstart`. Mirror the intern/`.transpose()` pattern
from `replication_instance.rs::to_replication_instance` (nullable strings via
`.as_deref().map(&mut intern).transpose()?`).

`collect_locks` runs the versioned query with `&[&max_rows]` and maps each
`tokio_postgres::Row` to a `LocksRow` with `row.get(...)`:

```rust
/// Collect the lock-wait tree. Caller runs `lock_waits_exist` first.
///
/// # Errors
/// Propagates the query error.
pub async fn collect_locks(
    client: &Client,
    major: u32,
    max_rows: i64,
) -> Result<Vec<LocksRow>, tokio_postgres::Error> {
    let rows = client.query(locks_query(locks_version(major)), &[&max_rows]).await?;
    Ok(rows.iter().map(row_from_pg).collect())
}
```

with a `fn row_from_pg(row: &tokio_postgres::Row) -> LocksRow` reading every
aliased column (`ts: row.get("ts_us")`, `blocked_by: row.get("blocked_by")`,
`backend_start: row.get("backend_start_us")`, …). tokio-postgres decodes SQL
`int[]` into `Vec<i32>` directly.

- [ ] **Step 6: Wire the module, run tests + gate**

Add `pub mod locks;` to `crates/kronika-source-pg/src/lib.rs`.
Run: `cargo test -p kronika-source-pg locks && cargo clippy -p kronika-source-pg --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/kronika-source-pg
git commit -m "source: сбор дерева ожиданий pg_locks (1_011)" \
  -m "Двухступенчатый сбор: EXISTS-предчек + cycle-safe рекурсивный CTE по pg_blocking_pids (PG10-13 manual path-guard / PG14-18 CYCLE + waitstart). blocked_by как int[], age(backend_xid/xmin), best-effort relname. Class A."
```

---

## Task 4: Collector wiring (`bins/pg_kronika-collector/src/main.rs`)

**Files:**
- Modify: `bins/pg_kronika-collector/src/main.rs`

**Interfaces:**
- Consumes: `collect_locks`, `lock_waits_exist`, `locks_version`, `to_v1`/`to_v2`,
  `LocksRow`, `LocksVersion` from Task 3.

- [ ] **Step 1: Add the env knob + cardinality validation (with test)**

Add `max_lock_rows: i64` to `Config`; parse in `from_env`:

```rust
let max_lock_rows = i64::try_from(env_u64("KRONIKA_PG_MAX_LOCK_ROWS", 1000)?)
    .context("KRONIKA_PG_MAX_LOCK_ROWS exceeds i64")?;
```

Add a startup check that `max_lock_rows` cannot overflow the section
(`kronika_registry::MAX_SECTION_ROWS`), with a unit test:

```rust
#[test]
fn max_lock_rows_within_section_cap() {
    assert!(1000 <= i64::try_from(kronika_registry::MAX_SECTION_ROWS).unwrap());
}
```

Extend the module-doc env list with `KRONIKA_PG_MAX_LOCK_ROWS`.

- [ ] **Step 2: Collect in `snapshot_and_seal` (class A, conditional)**

After the other `pool.main()` collections, before building buffers:

```rust
    let lock_rows = if lock_waits_exist(client).await.unwrap_or(false) {
        collect_locks(client, major, config.max_lock_rows)
            .await
            .context("collect pg_locks wait tree")?
    } else {
        Vec::new()
    };
```

- [ ] **Step 3: Add `push_locks` and call it**

```rust
fn push_locks(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: LocksVersion,
    rows: &[LocksRow],
) -> Result<()> {
    let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    for row in rows {
        match version {
            LocksVersion::V1 => buffer_row(buffers, to_v1(row, &mut intern)?)?,
            LocksVersion::V2 => buffer_row(buffers, to_v2(row, &mut intern)?)?,
        }
    }
    Ok(())
}
```

Call it after the other `push_*` (only when non-empty keeps the section out on no
waits — `conditional_full`):

```rust
    if !lock_rows.is_empty() {
        push_locks(&mut buffers, &mut interner, locks_version(major), &lock_rows)?;
    }
```

Add the `use kronika_source_pg::locks::{...}` import.

- [ ] **Step 4: Gate + a push unit test**

Add a test that `push_locks` on a one-root row seals a `1_011_00N` section (mirror
`push_prepared_xacts`'s test if present, else assert `buffers` gains the section).
Run: `cargo test -p pg_kronika-collector && cargo clippy -p pg_kronika-collector --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bins/pg_kronika-collector
git commit -m "collector: секция дерева ожиданий 1_011 (class A, conditional_full)" \
  -m "Сбор из pool.main() под EXISTS-предчеком: нет ожиданий → секции нет. KRONIKA_PG_MAX_LOCK_ROWS (дефолт 1000) + валидация против MAX_SECTION_ROWS."
```

---

## Task 5: BDD (live PG14-18) + golden (PG10-13) + negative

**Files:**
- Modify: `crates/kronika-bdd/features/*.feature` (new scenario)
- Modify: `crates/kronika-bdd/src/main.rs` (steps, consts, decode)

**Interfaces:**
- Consumes: `PgLocksV1`/`PgLocksV2` decode; `Cluster::connect` (two `Conn`s).

- [ ] **Step 1: Add a `.feature` scenario**

```gherkin
Scenario: A blocking chain is recorded as a wait tree
  Given a booted PostgreSQL cluster
  When session H holds a row lock and session W blocks on it
  Then the sealed segment has section 1_011 with W blocked_by H

Scenario: No lock waits writes no wait-tree section
  Given a booted PostgreSQL cluster
  Then the sealed segment has no section 1_011
```

- [ ] **Step 2: Implement the blocking step with two concurrent connections**

`Cluster::connect()` returns an independent `Conn` (own driver task), so two live
connections coexist. Hold H's transaction open across the snapshot:

```rust
const PG_LOCKS_V1_TYPE_ID: u32 = 1_011_001;
const PG_LOCKS_V2_TYPE_ID: u32 = 1_011_002;

#[when("session H holds a row lock and session W blocks on it")]
async fn open_block(world: &mut BddWorld) -> anyhow::Result<()> {
    for db in &world.clusters {
        let holder = db.connect().await?;
        holder.client().batch_execute(
            "CREATE TABLE IF NOT EXISTS kronika_lock_t(id int primary key, v int); \
             INSERT INTO kronika_lock_t VALUES (1,1) ON CONFLICT (id) DO NOTHING;").await?;
        holder.client().batch_execute("BEGIN; UPDATE kronika_lock_t SET v=v+1 WHERE id=1;").await?;
        // waiter: spawn a blocking UPDATE that will not return until holder commits
        let waiter = db.connect().await?;
        let w_client = waiter.client();
        // fire-and-hold: run on a task so it stays blocked while we snapshot
        let handle = tokio::spawn(async move {
            let _ = w_client.execute("UPDATE kronika_lock_t SET v=v+1 WHERE id=1", &[]).await;
        });
        // wait until the waiter is actually blocked on a Lock
        wait_until_lock_wait(&holder).await?;
        world.lock_holders.push((holder, waiter, handle)); // keep alive across snapshot
    }
    Ok(())
}
```

Add a `wait_until_lock_wait(conn)` poll on `SELECT count(*) FROM pg_stat_activity
WHERE wait_event_type='Lock'` with a short timeout, and a `lock_holders` field on
`BddWorld` to keep the connections + task alive until after the snapshot. The
`Drop` on `Conn` aborts the driver; drop them in the `Then` after asserting.

- [ ] **Step 3: Assert the sealed section**

Mirror `decode_sealed_row`, decoding by version:

```rust
#[then("the sealed segment has section 1_011 with W blocked_by H")]
async fn assert_wait_tree(world: &mut BddWorld) -> anyhow::Result<()> {
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        let type_id = if db.major() >= 14 { PG_LOCKS_V2_TYPE_ID } else { PG_LOCKS_V1_TYPE_ID };
        let rows = decode_locks_section(&segment, type_id)?;
        let waiter = rows.iter().find(|r| !r.blocked_by.is_empty())
            .context("no waiter row")?;
        anyhow::ensure!(rows.iter().any(|r| r.pid == waiter.blocked_by[0] && r.blocked_by.is_empty()),
            "blocker (root) not present");
        anyhow::ensure!(waiter.lock_locktype.is_some(), "waiter has no awaited lock");
    }
    Ok(())
}
```

`decode_locks_section` reads the entry for `type_id`, verifies CRC, and calls
`PgLocksV2::decode` / `PgLocksV1::decode` (returns owned rows with `blocked_by:
Vec<i32>`). Because these structs are not `Copy`, iterate by reference.

- [ ] **Step 4: Golden codec test for `1_011_001` (PG10-13 path)**

In `crates/kronika-registry` (or the golden test module used by other PG10-13
metrics), add an encode→bytes→decode golden for `PgLocksV1` covering a root row
(empty `blocked_by`), a waiter (`blocked_by=[pid]`), and a `0` blocker. This is
the PG10-13 coverage (outside the live matrix).

- [ ] **Step 5: Run BDD build + unit gate**

Run: `cargo test -p kronika-bdd` (host: compiles + non-live unit steps) and
`cargo test -p kronika-registry pg_locks`.
Live matrix runs in CI (`KRONIKA_PG_MATRIX`). Expected: host build+unit PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/kronika-bdd crates/kronika-registry
git commit -m "bdd/golden: сценарий блокировки 1_011 + golden PG10-13" \
  -m "Live-сценарий: две сессии, висящая транзакция, UPDATE-блокировка на transactionid → секция 1_011 с W blocked_by H, H корень. Negative: нет ожиданий → нет секции. Golden PG10-13 для 1_011_001."
```

---

## Task 6: Registry docs

**Files:**
- Modify: `docs/type-registry/postgresql.md`
- Modify: `docs/type-registry/postgresql-collection.md`

- [ ] **Step 1: Update `postgresql.md`**

Change the `1_011_001` summary-table row to two rows (`1_011_001` PG10-13 /
`1_011_002` PG14-18), semantics `conditional_full`, sort `(root_pid, depth, pid)`.
Replace/extend the `1_011` type section: node-centric graph, `blocked_by`
(list<i32>), IN/OUT scope, the two-version split, the cross-db relname limitation,
and the recorder note (tree/verdicts are read-side). Russian, concrete.

- [ ] **Step 2: Update `postgresql-collection.md`**

Rewrite the `1_011` collection note: two-stage (EXISTS precheck → cycle-safe
recursive CTE), `pg_blocking_pids` called only for `Lock`-waiters, dedup, PID 0
handling, `KRONIKA_PG_MAX_LOCK_ROWS`, class A from `pool.main()`.

- [ ] **Step 3: Commit**

```bash
git add docs/type-registry
git commit -m "docs: реестр для дерева ожиданий 1_011" \
  -m "Две раскладки 1_011_001/002, node-центричный граф blocked_by, scope IN/OUT, cross-db relname, механика двухступенчатого cycle-safe сбора."
```

---

## Final: full-workspace gate

- [ ] Run `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo run -p xtask -- check-deps` — all green.
- [ ] Push branch, open PR (base `main`), watch CI (live matrix validates the CTE + the blocking scenario on PG14-18).

## Self-review notes (spec coverage)

- Two versions / waitstart boundary → Tasks 2, 3. Value-domain locktype changes
  recorded verbatim (raw `l.locktype` text) → Task 3 SQL. conditional_full →
  Tasks 2, 4. class A → Task 4. blocked_by list<i32> → Task 1. IN/OUT scope
  (heavyweight + transactionid + advisory; LWLock/SIRead excluded) → Task 3
  (seed is `wait_event_type='Lock'`; `pg_blocking_pids` excludes predicate;
  awaited lock via `pg_locks NOT granted`). Cross-db relname → Task 3
  (`LEFT JOIN pg_class`, NULL off-db). xid/xmin age, waitstart → Tasks 2, 3.
  Cycle-safe (CYCLE / manual guard) → Task 3. Deadlocks out of scope → no task,
  documented in Task 6. Cardinality validation → Task 4. Testing → Task 5.
