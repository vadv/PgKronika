# pg_stat_user_tables (1_003) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the first database-local (class B) metric — per-table statistics from `pg_stat_user_tables` joined with `pg_statio_user_tables` and `pg_class`, collected from every database via the connection pool.

**Architecture:** Three schema versions (`1_003_001..003`) following the registry's monotonic-add discipline. Candidate selection blends reftool's volume top-N (activity ∪ size ∪ bloat) with a threshold "danger" branch built from PostgreSQL's own autovacuum trigger formulas plus table-level wraparound. The daemon wires `pool.refresh()`, iterates `pool.per_db()`, and applies the existing `AdaptiveTimeout` to the heavy query.

**Tech Stack:** Rust, tokio-postgres, arrow/parquet codec (`#[derive(Section)]`), kronika-writer interner.

**Design source:** `docs/superpowers/specs/2026-06-29-pg-stat-user-tables-design.md`. Two refinements over the spec, locked here: timestamp columns use `Option<Ts>` (unix microseconds, house convention) rather than epoch seconds; a `datid` (u32) column is added for a stable numeric sort key and a join key to `pg_stat_database`.

## Global Constraints

- PostgreSQL major range PG10-18. Live BDD covers PG15-18; PG10-13 covered by golden codec tests.
- reftool is the floor: never collect *less* than reftool's `tables.rs`. Above-floor additions are allowed and intended.
- NULL means "never"/"not applicable"; never coalesce such cases to 0. Genuine zero counters stay 0.
- Every SQL literal wrapped in the file's `marked!` macro (SQL-transparency rule).
- Column class is load-bearing: `c` = counter (monotonic, rate-able), `g` = gauge (point-in-time value), `l` = label, `t` = `ts` (must be named `ts`).
- Code and doc-comments in English (matches existing source). Commit messages in Russian. No `Co-Authored-By` line.
- After each task: `cargo fmt` and `cargo clippy -- --deny warnings` must pass.
- Reference implementation to mirror for structure: `crates/kronika-registry/src/codec/pg_stat_database.rs` and `crates/kronika-source-pg/src/database.rs` (both versioned, per-database, intern `datname`).

---

## File Structure

- Create `crates/kronika-registry/src/codec/pg_stat_user_tables.rs` — three `#[derive(Section)]` structs + contract tests.
- Modify `crates/kronika-registry/src/codec.rs` — `pub mod pg_stat_user_tables;`.
- Modify `crates/kronika-registry/src/lib.rs` — add to the `pub use codec::{...}` list and to `registry()`.
- Create `crates/kronika-source-pg/src/user_tables.rs` — version/query/Row/to_vN/collect + unit tests.
- Modify `crates/kronika-source-pg/src/lib.rs` — `pub mod user_tables;`.
- Modify `bins/pg_kronika-collector/src/main.rs` — `snapshot_and_seal` takes `&ConnectionPool`; daemon calls `refresh()`; per-db loop with adaptive timeout; `push_user_tables`.
- Create `crates/kronika-bdd/features/user_tables.feature` + step in the bdd binary.
- Modify `docs/type-registry/postgresql.md` and `docs/type-registry/postgresql-collection.md`.

---

## Task 1: Registry codec — three layout structs

**Files:**
- Create: `crates/kronika-registry/src/codec/pg_stat_user_tables.rs`
- Modify: `crates/kronika-registry/src/codec.rs` (add `pub mod pg_stat_user_tables;` alongside the other `pub mod` lines)
- Modify: `crates/kronika-registry/src/lib.rs:32-36` (add `pg_stat_user_tables` to the `pub use codec::{...}` list) and `crates/kronika-registry/src/lib.rs:67-86` (add three `CONTRACT`s to `registry()`)

**Interfaces:**
- Produces: `PgStatUserTablesV1` (`1_003_001`), `PgStatUserTablesV2` (`1_003_002`), `PgStatUserTablesV3` (`1_003_003`), each implementing `Section`.

- [ ] **Step 1: Write the failing contract tests**

In `crates/kronika-registry/src/codec/pg_stat_user_tables.rs`, add a `#[cfg(test)] mod tests` mirroring `pg_stat_database.rs:439-693`. Key assertions:

```rust
#[test]
fn v3_contract_shape() {
    let c = PgStatUserTablesV3::CONTRACT;
    assert_eq!(c.type_id.get(), 1_003_003);
    assert_eq!(c.columns.len(), 46);
    assert_eq!(c.sort_key, ["datid", "relid", "ts"]);
    assert_eq!(c.column("ts").map(|col| col.nullable), Some(false));
    assert_eq!(c.column("relid").map(|col| col.nullable), Some(false));
    assert_eq!(c.column("datname").map(|col| col.nullable), Some(false));
    assert_eq!(c.column("idx_scan").map(|col| col.nullable), Some(true));
    assert_eq!(c.column("toast_bytes").map(|col| col.nullable), Some(true));
    assert_eq!(c.column("last_vacuum").map(|col| col.nullable), Some(true));
    assert!(c.column("n_tup_newpage_upd").is_some());
    assert!(c.column("last_seq_scan").is_some());
    assert_eq!(lint(&[c]), Ok(()));
}
#[test]
fn v2_drops_pg16_columns() {
    let c = PgStatUserTablesV2::CONTRACT;
    assert_eq!(c.type_id.get(), 1_003_002);
    assert_eq!(c.columns.len(), 43);
    assert!(c.column("n_ins_since_vacuum").is_some());
    assert!(c.column("n_tup_newpage_upd").is_none());
    assert!(c.column("last_seq_scan").is_none());
    assert_eq!(lint(&[c]), Ok(()));
}
#[test]
fn v1_is_base_layout() {
    let c = PgStatUserTablesV1::CONTRACT;
    assert_eq!(c.type_id.get(), 1_003_001);
    assert_eq!(c.columns.len(), 42);
    assert!(c.column("n_ins_since_vacuum").is_none());
    assert_eq!(lint(&[c]), Ok(()));
}
```

Add a `v3_row(ts, datid, relid)` builder and `v3_roundtrip` / `v3_encode_sorts_by_datid_relid_ts` tests mirroring `pg_stat_database.rs:518-531` (use `crate::assert_roundtrips`). Builder sets nullable columns to a mix of `Some`/`None` (e.g. `idx_scan: None`, `toast_bytes: None`, `last_vacuum: Some(Ts(ts - 10))`) to exercise the null path.

- [ ] **Step 2: Run tests, verify they fail to compile** (structs not defined)

Run: `cargo test -p kronika-registry pg_stat_user_tables`
Expected: compile error, `PgStatUserTablesV3 not found`.

- [ ] **Step 3: Write the three structs**

Header doc-comment (English) explaining the catalog growth: `n_ins_since_vacuum` arrives in PG13; `n_tup_newpage_upd`, `last_seq_scan`, `last_idx_scan` in PG16. Then the V3 struct (the superset):

```rust
use crate::{Section, StrId, Ts};

/// Type `1_003_003`: `pg_stat_user_tables` on PG 16-18 (V2 plus
/// `n_tup_newpage_upd` and the `last_seq_scan`/`last_idx_scan` timestamps).
///
/// One row per selected table per database. `idx_*` columns are `None` when the
/// table has no indexes; `toast_*` columns are `None` when it has no TOAST
/// relation; `last_*` timestamps are `None` when the event never happened.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_003_003,
    name = "pg_stat_user_tables",
    semantics = snapshot_full,
    sort_key("datid", "relid", "ts")
)]
pub struct PgStatUserTablesV3 {
    /// Snapshot time, unix microseconds (per-database `statement_timestamp()`).
    #[column(t)]
    pub ts: Ts,
    /// Database oid of the connection that produced this row.
    #[column(l)]
    pub datid: u32,
    /// Database name of the connection.
    #[column(l)]
    pub datname: StrId,
    /// Table oid.
    #[column(l)]
    pub relid: u32,
    /// Schema name.
    #[column(l)]
    pub schemaname: StrId,
    /// Table name.
    #[column(l)]
    pub relname: StrId,
    /// Tablespace name; `pg_default` when the table uses the default tablespace.
    #[column(l)]
    pub tablespace: StrId,
    /// Sequential scans.
    #[column(c)]
    pub seq_scan: i64,
    /// Live rows fetched by sequential scans.
    #[column(c)]
    pub seq_tup_read: i64,
    /// Index scans; `None` when the table has no indexes.
    #[column(c)]
    pub idx_scan: Option<i64>,
    /// Live rows fetched by index scans; `None` when the table has no indexes.
    #[column(c)]
    pub idx_tup_fetch: Option<i64>,
    /// Rows inserted.
    #[column(c)]
    pub n_tup_ins: i64,
    /// Rows updated (including HOT).
    #[column(c)]
    pub n_tup_upd: i64,
    /// Rows deleted.
    #[column(c)]
    pub n_tup_del: i64,
    /// Rows HOT-updated.
    #[column(c)]
    pub n_tup_hot_upd: i64,
    /// Rows updated to a new page (PG16+).
    #[column(c)]
    pub n_tup_newpage_upd: i64,
    /// Estimated live rows.
    #[column(g)]
    pub n_live_tup: i64,
    /// Estimated dead rows.
    #[column(g)]
    pub n_dead_tup: i64,
    /// Rows modified since the last analyze.
    #[column(g)]
    pub n_mod_since_analyze: i64,
    /// Rows inserted since the last vacuum (PG13+).
    #[column(g)]
    pub n_ins_since_vacuum: i64,
    /// Manual vacuums.
    #[column(c)]
    pub vacuum_count: i64,
    /// Autovacuums.
    #[column(c)]
    pub autovacuum_count: i64,
    /// Manual analyzes.
    #[column(c)]
    pub analyze_count: i64,
    /// Autoanalyzes.
    #[column(c)]
    pub autoanalyze_count: i64,
    /// Last manual vacuum; `None` if never.
    #[column(g)]
    pub last_vacuum: Option<Ts>,
    /// Last autovacuum; `None` if never.
    #[column(g)]
    pub last_autovacuum: Option<Ts>,
    /// Last manual analyze; `None` if never.
    #[column(g)]
    pub last_analyze: Option<Ts>,
    /// Last autoanalyze; `None` if never.
    #[column(g)]
    pub last_autoanalyze: Option<Ts>,
    /// Last sequential scan (PG16+); `None` if never.
    #[column(g)]
    pub last_seq_scan: Option<Ts>,
    /// Last index scan (PG16+); `None` if never.
    #[column(g)]
    pub last_idx_scan: Option<Ts>,
    /// Main-fork size in bytes (`pg_relation_size`).
    #[column(g)]
    pub size_bytes: i64,
    /// TOAST table + its indexes size in bytes; `None` when no TOAST relation.
    #[column(g)]
    pub toast_bytes: Option<i64>,
    /// TOAST live tuples; `None` when no TOAST relation.
    #[column(g)]
    pub toast_n_live_tup: Option<i64>,
    /// TOAST dead tuples; `None` when no TOAST relation.
    #[column(g)]
    pub toast_n_dead_tup: Option<i64>,
    /// Last TOAST autovacuum; `None` when no TOAST relation or never.
    #[column(g)]
    pub toast_last_autovacuum: Option<Ts>,
    /// Age of `relfrozenxid` in transactions (wraparound proximity).
    #[column(g)]
    pub xid_age: i64,
    /// Age of `relminmxid` in multixacts (multixact wraparound proximity).
    #[column(g)]
    pub mxid_age: i64,
    /// Planner row estimate (`pg_class.reltuples`); `-1` means never analyzed (PG14+).
    #[column(g)]
    pub reltuples: i64,
    /// Heap blocks read from disk.
    #[column(c)]
    pub heap_blks_read: i64,
    /// Heap buffer hits.
    #[column(c)]
    pub heap_blks_hit: i64,
    /// Index blocks read; `None` when the table has no indexes.
    #[column(c)]
    pub idx_blks_read: Option<i64>,
    /// Index buffer hits; `None` when the table has no indexes.
    #[column(c)]
    pub idx_blks_hit: Option<i64>,
    /// TOAST blocks read; `None` when no TOAST relation.
    #[column(c)]
    pub toast_blks_read: Option<i64>,
    /// TOAST buffer hits; `None` when no TOAST relation.
    #[column(c)]
    pub toast_blks_hit: Option<i64>,
    /// TOAST-index blocks read; `None` when no TOAST relation.
    #[column(c)]
    pub tidx_blks_read: Option<i64>,
    /// TOAST-index buffer hits; `None` when no TOAST relation.
    #[column(c)]
    pub tidx_blks_hit: Option<i64>,
}
```

`PgStatUserTablesV2` (`id = 1_003_002`, 43 fields): copy V3 verbatim, then delete the three PG16 fields — `n_tup_newpage_upd`, `last_seq_scan`, `last_idx_scan`.

`PgStatUserTablesV1` (`id = 1_003_001`, 42 fields): copy V2, then delete `n_ins_since_vacuum`.

Keep the same `#[section(...)]` block (only the `id` changes) and the same per-field doc-comments.

- [ ] **Step 4: Wire the module and registry**

In `crates/kronika-registry/src/codec.rs` add `pub mod pg_stat_user_tables;`.
In `crates/kronika-registry/src/lib.rs`, add `pg_stat_user_tables` to the `pub use codec::{...}` list (the block at lines 32-36), and three lines to `registry()` (after the activity block, before the `pg_stat_database` block, to keep ascending order):

```rust
        pg_stat_user_tables::PgStatUserTablesV1::CONTRACT,
        pg_stat_user_tables::PgStatUserTablesV2::CONTRACT,
        pg_stat_user_tables::PgStatUserTablesV3::CONTRACT,
```

- [ ] **Step 5: Run tests, verify pass**

Run: `cargo test -p kronika-registry pg_stat_user_tables && cargo test -p kronika-registry the_registry_is_clean`
Expected: PASS (contract shapes, roundtrip, registry lint clean).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p kronika-registry -- --deny warnings
git add crates/kronika-registry/src/codec/pg_stat_user_tables.rs crates/kronika-registry/src/codec.rs crates/kronika-registry/src/lib.rs
git commit -m "Реестр: схема pg_stat_user_tables в трёх версиях" \
  -m "Что требовалось: типы сегмента для статистики таблиц по версиям каталога PG10-18." \
  -m "Суть: type_id 1_003_001..003 — V1 (PG10-12), V2 (+n_ins_since_vacuum, PG13-15), V3 (+n_tup_newpage_upd, last_seq_scan/last_idx_scan, PG16-18). statio-колонки и xid/mxid-age слиты в строку таблицы; NULL для «нет индексов/TOAST» и «никогда»."
```

---

## Task 2: Source module — query, parse, map

**Files:**
- Create: `crates/kronika-source-pg/src/user_tables.rs`
- Modify: `crates/kronika-source-pg/src/lib.rs` (add `pub mod user_tables;` near the other `pub mod` lines around line 33-44)

**Interfaces:**
- Consumes: `PgStatUserTablesV1/V2/V3` from Task 1.
- Produces:
  - `enum UserTablesVersion { V1, V2, V3 }`
  - `const fn user_tables_version(major: u32) -> UserTablesVersion`
  - `const fn user_tables_query(version: UserTablesVersion) -> &'static str`
  - `struct UserTablesRow` (owned superset)
  - `fn to_v1/to_v2/to_v3<E>(row: &UserTablesRow, datname: &str, intern: impl FnMut(&[u8]) -> Result<StrId, E>) -> Result<PgStatUserTables*, E>`
  - `async fn collect_user_tables(client: &Client, major: u32, max_tables: i64, wrap_fraction: f64) -> Result<(UserTablesVersion, Vec<UserTablesRow>), tokio_postgres::Error>`

- [ ] **Step 1: Write failing unit tests**

Mirror `database.rs:475-618`. Include a `fake_intern` (FNV) helper and a `sample_row` builder:

```rust
#[test]
fn version_follows_catalog_changes() {
    assert_eq!(user_tables_version(10), UserTablesVersion::V1);
    assert_eq!(user_tables_version(12), UserTablesVersion::V1);
    assert_eq!(user_tables_version(13), UserTablesVersion::V2);
    assert_eq!(user_tables_version(15), UserTablesVersion::V2);
    assert_eq!(user_tables_version(16), UserTablesVersion::V3);
    assert_eq!(user_tables_version(18), UserTablesVersion::V3);
}

#[test]
fn query_has_version_specific_columns_and_marker() {
    assert!(!user_tables_query(UserTablesVersion::V1).contains("n_ins_since_vacuum"));
    assert!(user_tables_query(UserTablesVersion::V2).contains("n_ins_since_vacuum"));
    assert!(!user_tables_query(UserTablesVersion::V2).contains("n_tup_newpage_upd"));
    assert!(user_tables_query(UserTablesVersion::V3).contains("n_tup_newpage_upd"));
    assert!(user_tables_query(UserTablesVersion::V3).contains("last_seq_scan"));
    for v in [UserTablesVersion::V1, UserTablesVersion::V2, UserTablesVersion::V3] {
        let q = user_tables_query(v);
        assert!(q.contains("pg_kronika"));
        assert!(q.contains("pg_stat_user_tables"));
        assert!(q.contains("LEFT JOIN pg_statio_user_tables"));
        assert!(q.contains("relfrozenxid"));
        assert!(q.contains("autovacuum_freeze_max_age"));
    }
    // V1 omits the PG13+ insert-vacuum danger term.
    assert!(!user_tables_query(UserTablesVersion::V1).contains("autovacuum_vacuum_insert_threshold"));
    assert!(user_tables_query(UserTablesVersion::V2).contains("autovacuum_vacuum_insert_threshold"));
}

#[test]
fn to_v3_maps_nulls_interns_strings_and_injects_datname() {
    let r = to_v3(&sample_row(/*relid*/5, /*has_idx*/false, /*has_toast*/false), "appdb", fake_intern)
        .expect("infallible intern");
    assert_eq!(r.relid, 5);
    assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
    assert_eq!(r.idx_scan, None);          // no indexes
    assert_eq!(r.idx_blks_read, None);
    assert_eq!(r.toast_bytes, None);       // no TOAST
    assert_eq!(r.last_vacuum, None);       // never vacuumed in the sample
    assert_eq!(r.n_tup_newpage_upd, 0);
    assert_eq!(r.xid_age, 100_000_000);
}

#[test]
fn intern_failure_propagates() {
    fn boom(_b: &[u8]) -> Result<StrId, &'static str> { Err("full") }
    assert_eq!(to_v3(&sample_row(5, true, true), "appdb", boom), Err("full"));
}
```

`sample_row(relid, has_idx, has_toast)` builds a `UserTablesRow` with `idx_scan`/`idx_blks_read`/`idx_blks_hit` set to `None` when `!has_idx`, `toast_*` to `None` when `!has_toast`, and all `last_*` to `None`.

- [ ] **Step 2: Run, verify fail to compile**

Run: `cargo test -p kronika-source-pg user_tables`
Expected: compile error (module not found).

- [ ] **Step 3: Implement the module**

Start with the file-local `marked!` macro (copy `database.rs:16-25`, with the path `crates/kronika-source-pg/src/user_tables.rs`), the imports, the version enum and selector:

```rust
use kronika_registry::pg_stat_user_tables::{
    PgStatUserTablesV1, PgStatUserTablesV2, PgStatUserTablesV3,
};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTablesVersion { V1, V2, V3 }

#[must_use]
pub const fn user_tables_version(major: u32) -> UserTablesVersion {
    if major >= 16 { UserTablesVersion::V3 }
    else if major >= 13 { UserTablesVersion::V2 }
    else { UserTablesVersion::V1 }
}
```

`user_tables_query` returns one `marked!` literal per version. The V3 query (V2/V1 drop the noted columns/terms):

```sql
WITH s AS (
  SELECT current_setting('autovacuum_freeze_max_age')::int8 AS afma,
         current_setting('autovacuum_multixact_freeze_max_age')::int8 AS amfma,
         current_setting('autovacuum_vacuum_threshold')::int8 AS vac_t,
         current_setting('autovacuum_vacuum_scale_factor')::float8 AS vac_sf,
         current_setting('autovacuum_vacuum_insert_threshold')::int8 AS ins_t,
         current_setting('autovacuum_vacuum_insert_scale_factor')::float8 AS ins_sf,
         current_setting('autovacuum_analyze_threshold')::int8 AS ana_t,
         current_setting('autovacuum_analyze_scale_factor')::float8 AS ana_sf
),
candidates AS (
  (SELECT relid FROM pg_stat_user_tables
     ORDER BY GREATEST(last_seq_scan, last_idx_scan) DESC NULLS LAST LIMIT $1)
  UNION
  (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid
     ORDER BY c.relpages DESC LIMIT $1)
  UNION
  (SELECT relid FROM pg_stat_user_tables ORDER BY COALESCE(n_dead_tup, 0) DESC LIMIT $1)
  UNION
  (SELECT t.relid FROM pg_stat_user_tables t
     JOIN pg_class c ON c.oid = t.relid CROSS JOIN s
     WHERE age(c.relfrozenxid)::int8 > (s.afma * $2)::int8
        OR mxid_age(c.relminmxid)::int8 > (s.amfma * $2)::int8
        OR t.n_dead_tup > s.vac_t + s.vac_sf * c.reltuples
        OR t.n_ins_since_vacuum > s.ins_t + s.ins_sf * c.reltuples
        OR t.n_mod_since_analyze > s.ana_t + s.ana_sf * c.reltuples)
)
SELECT
  (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid,
  t.relid,
  t.schemaname::text AS schemaname, t.relname::text AS relname,
  COALESCE(ts.spcname, 'pg_default')::text AS tablespace,
  t.seq_scan, t.seq_tup_read, t.idx_scan, t.idx_tup_fetch,
  t.n_tup_ins, t.n_tup_upd, t.n_tup_del, t.n_tup_hot_upd, t.n_tup_newpage_upd,
  t.n_live_tup, t.n_dead_tup, t.n_mod_since_analyze, t.n_ins_since_vacuum,
  t.vacuum_count, t.autovacuum_count, t.analyze_count, t.autoanalyze_count,
  (extract(epoch from t.last_vacuum) * 1e6)::int8 AS last_vacuum_us,
  (extract(epoch from t.last_autovacuum) * 1e6)::int8 AS last_autovacuum_us,
  (extract(epoch from t.last_analyze) * 1e6)::int8 AS last_analyze_us,
  (extract(epoch from t.last_autoanalyze) * 1e6)::int8 AS last_autoanalyze_us,
  (extract(epoch from t.last_seq_scan) * 1e6)::int8 AS last_seq_scan_us,
  (extract(epoch from t.last_idx_scan) * 1e6)::int8 AS last_idx_scan_us,
  pg_relation_size(t.relid)::int8 AS size_bytes,
  CASE WHEN cl.reltoastrelid <> 0 THEN pg_total_relation_size(cl.reltoastrelid)::int8 END AS toast_bytes,
  CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_live_tuples(cl.reltoastrelid) END AS toast_n_live_tup,
  CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_dead_tuples(cl.reltoastrelid) END AS toast_n_dead_tup,
  CASE WHEN cl.reltoastrelid <> 0 THEN (extract(epoch from pg_stat_get_last_autovacuum_time(cl.reltoastrelid)) * 1e6)::int8 END AS toast_last_autovacuum_us,
  age(cl.relfrozenxid)::int8 AS xid_age, mxid_age(cl.relminmxid)::int8 AS mxid_age, cl.reltuples::int8 AS reltuples,
  io.heap_blks_read, io.heap_blks_hit, io.idx_blks_read, io.idx_blks_hit,
  io.toast_blks_read, io.toast_blks_hit, io.tidx_blks_read, io.tidx_blks_hit,
  (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us
FROM pg_stat_user_tables t
JOIN candidates cand ON cand.relid = t.relid
LEFT JOIN pg_class cl ON cl.oid = t.relid
LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace
LEFT JOIN pg_statio_user_tables io ON io.relid = t.relid
```

**V2 query** = V3 with: the activity candidate ordered by `COALESCE(seq_scan,0)+COALESCE(idx_scan,0)+COALESCE(n_tup_ins,0)+COALESCE(n_tup_upd,0)+COALESCE(n_tup_del,0) DESC` (no `last_*_scan`); drop `t.n_tup_newpage_upd`, `last_seq_scan_us`, `last_idx_scan_us` from the SELECT. **V1 query** = V2 with: drop `t.n_ins_since_vacuum` from the SELECT and the `ins_t`/`ins_sf` columns from `s` and the insert-vacuum `OR` term from the danger branch.

`UserTablesRow` is the owned superset (numbers owned; version-absent and catalog-NULL columns are `Option`). Mirror `database.rs:144-221`:

```rust
#[derive(Debug, Clone)]
pub struct UserTablesRow {
    pub ts: i64,
    pub datid: u32,
    pub relid: u32,
    pub schemaname: String,
    pub relname: String,
    pub tablespace: String,
    pub seq_scan: i64,
    pub seq_tup_read: i64,
    pub idx_scan: Option<i64>,
    pub idx_tup_fetch: Option<i64>,
    pub n_tup_ins: i64,
    pub n_tup_upd: i64,
    pub n_tup_del: i64,
    pub n_tup_hot_upd: i64,
    pub n_tup_newpage_upd: Option<i64>, // V3
    pub n_live_tup: i64,
    pub n_dead_tup: i64,
    pub n_mod_since_analyze: i64,
    pub n_ins_since_vacuum: Option<i64>, // V2+
    pub vacuum_count: i64,
    pub autovacuum_count: i64,
    pub analyze_count: i64,
    pub autoanalyze_count: i64,
    pub last_vacuum: Option<i64>,
    pub last_autovacuum: Option<i64>,
    pub last_analyze: Option<i64>,
    pub last_autoanalyze: Option<i64>,
    pub last_seq_scan: Option<i64>,  // V3
    pub last_idx_scan: Option<i64>,  // V3
    pub size_bytes: i64,
    pub toast_bytes: Option<i64>,
    pub toast_n_live_tup: Option<i64>,
    pub toast_n_dead_tup: Option<i64>,
    pub toast_last_autovacuum: Option<i64>,
    pub xid_age: i64,
    pub mxid_age: i64,
    pub reltuples: i64,
    pub heap_blks_read: i64,
    pub heap_blks_hit: i64,
    pub idx_blks_read: Option<i64>,
    pub idx_blks_hit: Option<i64>,
    pub toast_blks_read: Option<i64>,
    pub toast_blks_hit: Option<i64>,
    pub tidx_blks_read: Option<i64>,
    pub tidx_blks_hit: Option<i64>,
}
```

`row_from_pg(row, version)` reads by name; version gates the three PG16 columns and `n_ins_since_vacuum` (mirror `database.rs:405-458` `.then(|| row.get(...))` pattern). All `*_us` columns read as `Option<i64>`; `idx_*`, `toast_*`, statio `idx/toast/tidx` read as `Option<i64>` directly from the query.

`to_v3` maps `UserTablesRow` → `PgStatUserTablesV3`, interning `datname` (passed in), `schemaname`, `relname`, `tablespace`; mapping `Option<i64>` micros → `Option<Ts>` with `.map(Ts)`; using `.unwrap_or(0)` only for the version-present-but-Option-typed counters (`n_tup_newpage_upd`, `n_ins_since_vacuum`). Catalog-nullable columns (`idx_scan`, `toast_*`, statio `idx/toast/tidx`) stay `Option`. Example head:

```rust
pub fn to_v3<E>(
    row: &UserTablesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserTablesV3, E> {
    Ok(PgStatUserTablesV3 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        seq_scan: row.seq_scan,
        seq_tup_read: row.seq_tup_read,
        idx_scan: row.idx_scan,
        idx_tup_fetch: row.idx_tup_fetch,
        n_tup_ins: row.n_tup_ins,
        n_tup_upd: row.n_tup_upd,
        n_tup_del: row.n_tup_del,
        n_tup_hot_upd: row.n_tup_hot_upd,
        n_tup_newpage_upd: row.n_tup_newpage_upd.unwrap_or(0),
        n_live_tup: row.n_live_tup,
        n_dead_tup: row.n_dead_tup,
        n_mod_since_analyze: row.n_mod_since_analyze,
        n_ins_since_vacuum: row.n_ins_since_vacuum.unwrap_or(0),
        vacuum_count: row.vacuum_count,
        autovacuum_count: row.autovacuum_count,
        analyze_count: row.analyze_count,
        autoanalyze_count: row.autoanalyze_count,
        last_vacuum: row.last_vacuum.map(Ts),
        last_autovacuum: row.last_autovacuum.map(Ts),
        last_analyze: row.last_analyze.map(Ts),
        last_autoanalyze: row.last_autoanalyze.map(Ts),
        last_seq_scan: row.last_seq_scan.map(Ts),
        last_idx_scan: row.last_idx_scan.map(Ts),
        size_bytes: row.size_bytes,
        toast_bytes: row.toast_bytes,
        toast_n_live_tup: row.toast_n_live_tup,
        toast_n_dead_tup: row.toast_n_dead_tup,
        toast_last_autovacuum: row.toast_last_autovacuum.map(Ts),
        xid_age: row.xid_age,
        mxid_age: row.mxid_age,
        reltuples: row.reltuples,
        heap_blks_read: row.heap_blks_read,
        heap_blks_hit: row.heap_blks_hit,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
        toast_blks_read: row.toast_blks_read,
        toast_blks_hit: row.toast_blks_hit,
        tidx_blks_read: row.tidx_blks_read,
        tidx_blks_hit: row.tidx_blks_hit,
    })
}
```

`to_v2` = `to_v3` minus the three PG16 fields. `to_v1` = `to_v2` minus `n_ins_since_vacuum`.

```rust
pub async fn collect_user_tables(
    client: &Client,
    major: u32,
    max_tables: i64,
    wrap_fraction: f64,
) -> Result<(UserTablesVersion, Vec<UserTablesRow>), tokio_postgres::Error> {
    let version = user_tables_version(major);
    let rows = client
        .query(user_tables_query(version), &[&max_tables, &wrap_fraction])
        .await?;
    let parsed = rows.iter().map(|row| row_from_pg(row, version)).collect();
    Ok((version, parsed))
}
```

- [ ] **Step 4: Run unit tests, verify pass**

Run: `cargo test -p kronika-source-pg user_tables`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p kronika-source-pg -- --deny warnings
git add crates/kronika-source-pg/src/user_tables.rs crates/kronika-source-pg/src/lib.rs
git commit -m "Сбор pg_stat_user_tables по версиям PG10-18" \
  -m "Что требовалось: запрос и разбор статистики таблиц с двухстратегийным отбором кандидатов." \
  -m "Суть: top-N по объёму (активность/размер/bloat) плюс пороговая danger-ветка по формулам autovacuum и wraparound; statio слит через LEFT JOIN; чистые to_vN с инъекцией datname и NULL-семантикой, golden-тестируемые."
```

---

## Task 3: Collector integration — refresh, per-db loop, adaptive timeout

**Files:**
- Modify: `bins/pg_kronika-collector/src/main.rs`

**Interfaces:**
- Consumes: `collect_user_tables`, `UserTablesRow`, `UserTablesVersion`, `user_tables::{to_v1,to_v2,to_v3}` (Task 2); `ConnectionPool::{refresh, per_db, main, server_major}`, `AdaptiveTimeout`, `DatabaseConn` (pool).

- [ ] **Step 1: Write the failing collector unit test**

Add `push_user_tables` and a test mirroring `push_database` (`main.rs:510-539`): build two `UserTablesRow`, push as `UserTablesVersion::V3`, flush, assert the part carries `type_id == 1_003_003`. Add a `ut_row(relid)` builder in the test module.

- [ ] **Step 2: Run, verify fail** — `cargo test -p pg_kronika-collector push_user_tables` → compile error.

- [ ] **Step 3a: Add imports and `push_user_tables`**

Add to the `use kronika_source_pg::...` block:

```rust
use kronika_source_pg::user_tables::{
    self, UserTablesRow, UserTablesVersion, collect_user_tables,
};
use kronika_source_pg::pool::AdaptiveTimeout;
```

Add the buffering helper (mirror `push_database`, but `datname` is carried alongside each version+rows group):

```rust
/// Intern each table row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned or a section buffer is full.
fn push_user_tables(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    collected: &[(String, UserTablesVersion, Vec<UserTablesRow>)],
) -> Result<()> {
    for (datname, version, rows) in collected {
        for row in rows {
            let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
            match version {
                UserTablesVersion::V1 => buffer_row(buffers, user_tables::to_v1(row, datname, &mut intern)?)?,
                UserTablesVersion::V2 => buffer_row(buffers, user_tables::to_v2(row, datname, &mut intern)?)?,
                UserTablesVersion::V3 => buffer_row(buffers, user_tables::to_v3(row, datname, &mut intern)?)?,
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 3b: Change `snapshot_and_seal` to take the pool, add the per-db collection**

Change the signature `client: &Client` → `pool: &ConnectionPool`, and add `let client = pool.main();` as the first line so the existing class-A collectors are unchanged. Before building `buffers`, collect per-db (all awaits first, then intern — `Interner` is `!Send`):

```rust
// database-local (class B): collect every database before interning. Peak
// memory is bounded by top-N (KRONIKA_PG_MAX_TABLES) across the pool.
let mut user_tables: Vec<(String, UserTablesVersion, Vec<UserTablesRow>)> = Vec::new();
let mut heavy = AdaptiveTimeout::new(15_000, heavy_timeout_cap_ms);
for db in pool.per_db() {
    loop {
        // Heavy size functions can be slow; widen statement_timeout for this query only.
        db.client()
            .batch_execute(&format!("SET statement_timeout = {}", heavy.current_ms()))
            .await
            .ok();
        match collect_user_tables(db.client(), major, max_tables, wrap_fraction).await {
            Ok((version, rows)) => {
                user_tables.push((db.datname.clone(), version, rows));
                break;
            }
            Err(err) if is_sqlstate(&err, "57014") && !heavy.at_cap() => {
                heavy.grow(); // statement_timeout hit; retry this database wider
            }
            Err(err) => {
                eprintln!(
                    "pg_kronika-collector: skip user_tables for {}: {err}",
                    db.datname
                );
                break;
            }
        }
    }
}
```

Add a SQLSTATE helper:

```rust
/// Whether a tokio-postgres error carries the given SQLSTATE code.
fn is_sqlstate(err: &tokio_postgres::Error, code: &str) -> bool {
    err.code().is_some_and(|state| state.code() == code)
}
```

After the existing `push_*` calls, add `push_user_tables(&mut buffers, &mut interner, &user_tables)?;`.

Thread the three knobs (`max_tables`, `wrap_fraction`, `heavy_timeout_cap_ms`) into `snapshot_and_seal` as parameters, read from env in `Config::from_env` with `env_u64`/a new `env_f64`:
- `KRONIKA_PG_MAX_TABLES` (default 500) → `i64`
- `KRONIKA_PG_WRAPAROUND_WARN_FRACTION` (default 0.8) → `f64`
- `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` (default 60000) → `u64`

- [ ] **Step 3c: Call `refresh()` in the daemon loop**

In `main()` after `pool.ensure_main().await` (`main.rs:135`), add:

```rust
if let Err(err) = pool
    .refresh(std::time::Duration::from_secs(pool_refresh_secs), max_databases)
    .await
{
    eprintln!("pg_kronika-collector: pool refresh failed: {err:#}");
}
for db in pool.uncovered() {
    eprintln!("pg_kronika-collector: database not covered this cycle: {db}");
}
```

Read `KRONIKA_PG_POOL_REFRESH_SECS` (default 600) and use `kronika_source_pg::pool::DEFAULT_MAX_DATABASES` for `max_databases`. Update the `snapshot_and_seal(pool.main(), ...)` call site to `snapshot_and_seal(&pool, ...)` with the new knob args.

- [ ] **Step 4: Run tests + build** — `cargo test -p pg_kronika-collector` and `cargo build -p pg_kronika-collector`. Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p pg_kronika-collector -- --deny warnings
git add bins/pg_kronika-collector/src/main.rs
git commit -m "Демон: сбор database-local таблиц по всем базам" \
  -m "Что требовалось: первый per-database потребитель пула — собрать pg_stat_user_tables со всех баз." \
  -m "Суть: демон обновляет пул и идёт по per_db; тяжёлый запрос под адаптивным statement_timeout (57014 — расширить и повторить, иначе пропустить базу); сбор по одной базе с последующим интернингом."
```

---

## Task 4: BDD — multi-database and danger-branch coverage

**Files:**
- Create: `crates/kronika-bdd/features/user_tables.feature`
- Modify: the bdd binary (add the `then` step; mirror the existing per-database feature step that uses `pool.per_db()`)

**Interfaces:**
- Consumes: `collect_user_tables` against live PG15-18 from the test matrix.

- [ ] **Step 1: Write the feature**

```gherkin
Feature: pg_stat_user_tables collection
  Scenario: tables are collected per database with datname separation
    Given a cluster with databases "kronika_a" and "kronika_b", each holding a seeded table
    When the collector snapshots pg_stat_user_tables across the pool
    Then the segment carries 1_003 rows from both "kronika_a" and "kronika_b"
    And each row resolves its datname, schemaname and relname through the dictionary

  Scenario: a table near xid wraparound is selected despite no activity
    Given a database with an idle table whose relfrozenxid age exceeds 80% of autovacuum_freeze_max_age
    When the collector snapshots pg_stat_user_tables
    Then that table appears in the 1_003 rows
```

- [ ] **Step 2: Implement the step**

In the bdd binary, add a step that connects a pool to the matrix cluster, `refresh()`es, iterates `per_db()`, runs `collect_user_tables`, builds a segment (reuse the existing collector-path helper or `SectionBuffers`), then `Segment::open` → `catalog().entries.find(type_id == 1_003_00x)` → `VerifiedSection::verify` → `PgStatUserTablesV*::decode` → assert rows from both databases and `segment.dictionary().resolve(str_id.0)` yields the names. For scenario 2, seed an aged table (the matrix helper can `SET` a low `autovacuum_freeze_max_age` on a database or consume xids); assert the relid is present.

- [ ] **Step 3: Run BDD** — `cargo test -p kronika-bdd` (or the project's BDD runner over `KRONIKA_PG_MATRIX`). Expected: PASS on PG15-18.

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy -p kronika-bdd -- --deny warnings
git add crates/kronika-bdd/
git commit -m "BDD: pg_stat_user_tables по нескольким базам и danger-выборка" \
  -m "Что требовалось: подтвердить разделение по datname и попадание опасной таблицы в выборку." \
  -m "Суть: сценарии проверяют строки 1_003 из двух баз с резолвом строк через словарь и отбор простаивающей таблицы у порога wraparound."
```

---

## Task 5: Documentation

**Files:**
- Modify: `docs/type-registry/postgresql.md` (summary table rows for `1_003_001..003` + a type section describing the schema, the two candidate strategies, and the version split)
- Modify: `docs/type-registry/postgresql-collection.md` (collection notes: per-db iteration, candidate SQL, adaptive timeout, env knobs)
- Modify: `bins/pg_kronika-collector/src/main.rs` module doc (`//!`) — add the four new env vars (`KRONIKA_PG_MAX_TABLES`, `KRONIKA_PG_WRAPAROUND_WARN_FRACTION`, `KRONIKA_PG_POOL_REFRESH_SECS`, `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS`)

- [ ] **Step 1: Update the registry docs** following the existing `pg_stat_database` section format (summary row per type_id, then a prose section). State the danger-branch is above the reftool floor.

- [ ] **Step 2: Verify the registry doc-vs-code test** (if present, e.g. a test that cross-checks `docs/type-registry/postgresql.md` against `registry()`): `cargo test -p kronika-registry`. Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add docs/type-registry/ bins/pg_kronika-collector/src/main.rs
git commit -m "Документация: тип pg_stat_user_tables и env коллектора" \
  -m "Что требовалось: описать схему 1_003, стратегию отбора и новые переменные окружения." \
  -m "Суть: реестр и заметки сбора описывают двухстратегийный отбор (top-N плюс пороги autovacuum/wraparound) и параметры KRONIKA_PG_MAX_TABLES, WRAPAROUND_WARN_FRACTION, POOL_REFRESH_SECS, HEAVY_TIMEOUT_CAP_MS."
```

---

## Self-Review

**Spec coverage:** schema/versions → Task 1; SQL candidate selection + danger branch + statio merge → Task 2; per-db iteration + refresh + adaptive timeout + incremental-vs-collect-all → Task 3; multi-db + wraparound BDD → Task 4; docs + env → Task 5. All spec sections map to a task.

**Placeholder scan:** SQL and structs are spelled out for V3; V2/V1 are explicit named deltas over V3 (the live reference `pg_stat_database.rs` shows all four versions fully). No "TBD"/"handle errors"/"add validation".

**Type consistency:** `to_v3(row, datname, intern)` signature matches its call in `push_user_tables`; `collect_user_tables(client, major, max_tables, wrap_fraction)` matches the Task 3 loop; `UserTablesVersion` variants identical across tasks; contract column counts (46/43/42) match the struct field counts.

**Known follow-ups (out of scope, noted in spec §10):** per-table reloptions precision; bounded-parallel per-db collection; `candidate_reason` column; danger truncation marker at the 65536-row codec cap.
