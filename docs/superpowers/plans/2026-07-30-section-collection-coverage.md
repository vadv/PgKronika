# Exact Section Collection Coverage Implementation Plan

**Goal:** Return factual `collected/source_total` status for statements, plans, tables, and indexes with one PostgreSQL source pass and bounded OVF-only web reads.

**Architecture:** Each PostgreSQL query materializes its source once and calculates an exact window count over that materialization. The collector persists one canonical snapshot marker per attempted view read, the reader projects those markers into the sole `UiSummary` revision 1 layout, and `/v1/views/summary` reads only that bounded block.

**Tech Stack:** Rust 1.96, Tokio PostgreSQL, typed PGM registry sections, PGKOVF binary blocks, Axum, Serde, Utoipa/OpenAPI.

## Global Constraints

- Public `source_total` is an exact factual count or `null`; no estimate, lower bound, or quality score is exposed.
- PostgreSQL source SRFs or statistics views occur once per collection query.
- `tables` and `indexes` counts aggregate all configured databases under one `cycle_ts_us`.
- `/v1/views/summary` does not read PGM bodies, dictionaries, raw coverage sections, entity series, or source rows.
- Existing PGM coverage types remain unchanged.
- `UiSummary` revision 1 changes in place because the format is not deployed.
- No legacy decoder, format migration, or compatibility revision is added.
- Frontend number formatting is outside this implementation.

---

### Task 1: Single-pass PostgreSQL source queries

**Files:**
- Modify: `crates/kronika-source-pg/src/statements.rs`
- Modify: `crates/kronika-source-pg/src/store_plans.rs`
- Modify: `crates/kronika-source-pg/src/user_tables.rs`
- Modify: `crates/kronika-source-pg/src/user_indexes.rs`

**Interfaces:**
- Produces: `collect_user_tables(client, major, max_tables, cycle_ts_us)`
- Produces: `collect_user_indexes(client, major, max_indexes, cycle_ts_us)`
- Preserves: statements and plans return `(rows, exact_source_total)`

- [ ] **Step 1: Write failing SQL contract tests**

For every supported layout, assert that the emitted SQL has one source CTE,
uses `count(*) OVER ()::int8 AS source_total`, contains no scalar count over
the PostgreSQL source, and makes all candidate/final reads through `source`.
PostgreSQL 12+ must use `source AS MATERIALIZED (...)`; PostgreSQL 10/11 must
use `source AS (...)` without the unsupported keyword. Statements additionally
require exactly one
`pg_stat_statements(false)`, two bounded ordinal candidate axes, a final
ordinal-only join, and `NULL::text AS query`. For tables and indexes also
assert `$2::int8 AS ts_us`.

```rust
fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

if server_major >= 12 {
    assert!(query.contains("source AS MATERIALIZED ("));
} else {
    assert!(query.contains("source AS ("));
    assert!(!query.contains("AS MATERIALIZED"));
}
assert!(query.contains("count(*) OVER ()::int8 AS source_total"));
assert!(!query.contains("(SELECT count(*) FROM pg_stat_user_tables)"));
assert_eq!(occurrences(query, "FROM pg_stat_user_tables"), 1);
assert!(query.contains("$2::int8 AS ts_us"));
```

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p kronika-source-pg query
```

Expected: failures show repeated source references, scalar count subqueries, and
missing `$2` timestamps in the current SQL.

- [ ] **Step 3: Implement one materialized source per family**

Use this PostgreSQL 12+ shape for tables and apply the same source/candidate
structure to the other families while preserving each layout's existing selected
columns and joins. On PostgreSQL 10/11, emit `source AS (...)`; those releases
materialize CTEs implicitly:

```sql
WITH source AS MATERIALIZED (
  SELECT t.*, count(*) OVER ()::int8 AS source_total
  FROM pg_stat_user_tables t
),
candidates AS (
  (SELECT relid FROM source ORDER BY seq_tup_read DESC LIMIT $1)
  UNION
  (SELECT relid FROM source ORDER BY n_tup_ins + n_tup_upd + n_tup_del DESC LIMIT $1)
)
SELECT s.relid, $2::int8 AS ts_us, s.source_total
FROM source s
JOIN candidates c ON c.relid = s.relid
```

Statements use `pg_stat_statements(false) WITH ORDINALITY`: each axis selects
only `source_ordinal` under its own `LIMIT`, their `UNION` has at most `2N`
rows, and the final query joins only by ordinal. Layouts 1.9 and newer include
`toplevel` in identity and tie-break ordering. The source projection excludes
query text and the final `query` field is `NULL`.

The five source bodies are exactly `pg_stat_statements(false)`,
`pg_store_plans(false)`, zero-argument `pg_store_plans()`,
`pg_stat_user_tables`, and `pg_stat_user_indexes`. Empty successful results
continue to infer exact total zero. For OSSC, outer top-N and a `NULL` plan
projection bound transfer and collector allocations, not the extension reading
the full plan file and materializing the result in the server backend.

- [ ] **Step 4: Pass the cycle timestamp through table/index collectors**

Change both functions to bind `[&max_n, &cycle_ts_us]`, update rustdoc for `$1/$2`,
and keep the decoded row timestamp sourced from `ts_us`.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p kronika-source-pg
git add crates/kronika-source-pg/src
git commit -m "perf(pg): count collection coverage in one source pass"
```

### Task 2: Canonical collector coverage and shared database timestamp

**Files:**
- Modify: `bins/pg_kronika-collector/src/coverage.rs`
- Modify: `bins/pg_kronika-collector/src/pool_sources.rs`
- Modify: `bins/pg_kronika-collector/src/main.rs`
- Modify: `bins/pg_kronika-collector/src/tests/coverage.rs`

**Interfaces:**
- Consumes: four exact totals from Task 1
- Produces: `CoverageAttempt { ts, section_type_id, coverage }`
- Produces: `SourceCoverage::snapshot_marker()`
- Produces: `PoolReads::{tables_attempt,indexes_attempt}`

- [ ] **Step 1: Write failing state-machine tests**

Cover complete, top-N, permission-only, timeout, other failure, and `u32` overflow.
The expected state priority is loss, read failure, permission, source limit,
complete:

```rust
assert_eq!(complete.read_state(), (0, 0));
assert_eq!(top_n.read_state(), (1, 0));
assert_eq!(permission.read_state(), (2, 1));
assert_eq!(timeout.read_state(), (3, 2));
assert_eq!(overflow.read_state(), (4, 2));
assert_eq!(overflow.exact_total(), None);
```

Also assert that an attempted empty source emits `collected=0`, `source_total=0`
instead of being indistinguishable from a source that was not due.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p pg_kronika-collector coverage
```

Expected: `SourceCoverage` has no attempt marker, canonical state mapping, or
overflow-aware exact total.

- [ ] **Step 3: Implement the collector state machine**

Add an `attempted: bool` bit and checked accumulation methods. Keep the PGM
`source_total` field as the known lower bound for failure records, while
`CollectionCoverageV1.unknown_total` determines whether it is exact.

```rust
pub(crate) struct CoverageAttempt {
    pub(crate) ts: i64,
    pub(crate) section_type_id: u32,
    pub(crate) coverage: SourceCoverage,
}

impl SourceCoverage {
    pub(crate) const fn read_state(self) -> (u8, u8);
    pub(crate) const fn exact_total(self) -> Option<u64>;
    pub(crate) fn snapshot_marker(self, ts: i64, section_type_id: u32)
        -> SnapshotCoverageV1;
}
```

For overflow, saturate only the wire value, force state
`collector_limit_or_loss`, set `unknown_total=true`, and never return it from
`exact_total`.

- [ ] **Step 4: Apply one cycle timestamp to every database**

Pass `main_src.ts.0` into `read_pool_sources`, then into
`collect_user_tables_all` and `collect_user_indexes_all`, and finally into every
Task 1 query. Aggregate successful counts with checked addition. Record failed
databases in the same attempt; do not publish a partial total as exact.

- [ ] **Step 5: Emit snapshot markers for tables and indexes**

Add their attempts to `completeness` whenever `attempted=true`, including
successful empty reads and partial multi-database failures. Continue writing
`CollectionCoverageV1` for every non-complete state.

- [ ] **Step 6: Verify GREEN and commit**

```bash
cargo test -p pg_kronika-collector coverage
cargo test -p pg_kronika-collector
git add bins/pg_kronika-collector/src
git commit -m "feat(collector): persist exact all-database coverage"
```

### Task 3: Preserve failed statements and plans attempts

**Files:**
- Modify: `bins/pg_kronika-collector/src/statements_source.rs`
- Modify: `bins/pg_kronika-collector/src/plans_source.rs`
- Modify: `bins/pg_kronika-collector/src/pool_sources.rs`
- Modify: `bins/pg_kronika-collector/src/coverage.rs`
- Modify: `bins/pg_kronika-collector/src/main.rs`
- Modify: `bins/pg_kronika-collector/src/tests/statements_source.rs`
- Modify: `bins/pg_kronika-collector/src/tests/coverage.rs`

**Interfaces:**
- Consumes: `CoverageAttempt` from Task 2
- Produces: one optional data payload and one optional attempt marker per due view
- Preserves: source discovery probes without a known physical type emit no marker

- [ ] **Step 1: Write failing attempt-outcome tests**

Test these pure transitions without mocking PostgreSQL:

```rust
assert_eq!(
    query_failure_attempt(ts, type_id, "42501").coverage.read_state(),
    (2, 1)
);
assert_eq!(
    query_failure_attempt(ts, type_id, "57014").coverage.read_state(),
    (3, 2)
);
assert!(unknown_layout_probe_failure().is_none());
```

For OSSC masked rows, assert exact total remains present, collected excludes masked
rows, and the final state is permission/restricted.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p pg_kronika-collector statements_source
cargo test -p pg_kronika-collector coverage
```

- [ ] **Step 3: Return typed attempt outcomes**

Keep source payloads optional, but return a `CoverageAttempt` once extension version
or plan fork identifies `section_type_id`. Successful attempts use the source
timestamp; failed statements use the cycle timestamp; failed plans use the reset
snapshot timestamp already captured before enumeration.

- [ ] **Step 4: Carry OSSC restricted visibility**

Return the masked-row count from OSSC enumeration to `plans_source`, add it to the
attempt's missing rows without changing exact total, and choose
`permission/restricted`. Persist both snapshot and collection coverage records so
older analytical readers retain the reason metadata.

- [ ] **Step 5: Verify GREEN and commit**

```bash
cargo test -p pg_kronika-collector
git add bins/pg_kronika-collector/src
git commit -m "feat(collector): retain failed source coverage attempts"
```

### Task 4: Canonically merge PGM coverage facts

**Files:**
- Modify: `crates/kronika-reader/src/overview/metric_extract.rs`

**Interfaces:**
- Consumes: matching `SnapshotCoverageV1` and `CollectionCoverageV1`
- Produces: one canonical `CoverageRecord` per `(section_type_id, ts)`

- [ ] **Step 1: Write failing merge tests**

Add cases where snapshot coverage carries the final state and collection coverage
refines exactness:

```rust
// Same collected/lower-bound values, collection says the total is unknown.
assert_eq!(merged.total_exact, false);
assert_eq!(merged.read_state, 3);

// Two different exact totals or collected values remain corruption.
assert!(matches!(conflict, Err(BuildError::Source(SourceError::Corrupt))));
```

- [ ] **Step 2: Verify RED**

```bash
cargo test -p kronika-reader snapshot_and_collection
```

Expected: the current byte-for-byte equality rule rejects a legitimate
`unknown_total` refinement.

- [ ] **Step 3: Implement semantic merge**

Require equal timestamps and collected counts. Accept an inexact lower bound not
greater than an exact total. Use snapshot state/visibility as authoritative when
present; use collection reason only for collection-only data. Contradicting
exact totals, impossible bounds, or state invariants remain corruption.

- [ ] **Step 4: Verify GREEN and commit**

```bash
cargo test -p kronika-reader metric_extract
git add crates/kronika-reader/src/overview/metric_extract.rs
git commit -m "fix(reader): merge complementary coverage facts"
```

### Task 5: `UiSummary` collection codec

**Files:**
- Modify: `crates/kronika-reader/src/overview/web_index/summary.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Modify: `crates/kronika-reader/src/overview/mod.rs`
- Modify: `bins/pg_kronika-dump/tests/dump.rs`

**Interfaces:**
- Produces: `CollectionStatus`
- Produces: `CollectionReadState`
- Produces: `CollectionVisibility`
- Produces: `ViewSummary::new_with_collection(view_code, view_revision, status,
  snapshot_presence, notable_presence, populations, collection_presence,
  collections, bounds) -> Result<ViewSummary, BlockError>`
- Produces: `UiSummaryBlock::collection_state_at(view_code, at_us)`
- Produces: value getters `collected()`, `source_total()`, `read_state()`, and
  `visibility()`

- [ ] **Step 1: Write failing revision and invariant tests**

```rust
let status = CollectionStatus::new(
    500,
    Some(4_800),
    CollectionReadState::SourceLimit,
    CollectionVisibility::Full,
)?;
assert_eq!(decoded.collection_state_at(1, 100), Some((100, status)));
assert!(decode_old_layout_fixture().is_err());
assert!(CollectionStatus::new(
    500, Some(400), CollectionReadState::SourceLimit,
    CollectionVisibility::Full
).is_err());
```

Include malformed masks, invalid enum codes, complete `N != M`, source-limit
`N >= M`, non-null totals for read failure/loss, and decoded byte bounds.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p kronika-reader web_index::summary
```

- [ ] **Step 3: Implement typed collection status**

```rust
pub struct CollectionStatus {
    collected: u64,
    source_total: Option<u64>,
    read_state: CollectionReadState,
    visibility: CollectionVisibility,
}
```

Revision 1 stores a per-view collection presence mask and one compact record per set
bit: collected varint, nullable exact total, state code, visibility code. Preserve
`ViewSummary::new` as the no-collection convenience constructor for existing
callers; `new_with_collection` performs all cross-field validation.

- [ ] **Step 4: Keep one wire layout**

Encode and decode only the new revision 1 layout. Reject the previous undeployed
layout and every other revision. Do not add migration or compatibility code.

- [ ] **Step 5: Include new buffers in memory accounting**

Add collection mask capacity and `CollectionStatus` vector capacity to
`resident_heap_bytes`; retain the existing decoded-byte cap as the wire bound.

- [ ] **Step 6: Verify GREEN and commit**

```bash
cargo test -p kronika-reader web_index::summary
cargo test -p pg_kronika-dump --test dump
git add crates/kronika-reader bins/pg_kronika-dump/tests/dump.rs
git commit -m "feat(ovf): store collection status in ui summary revision 1"
```

### Task 6: Project coverage into the bounded UI summary

**Files:**
- Modify: `crates/kronika-reader/src/overview/web_index/build.rs`
- Modify: `crates/kronika-reader/src/overview/proptests.rs`
- Modify: `crates/kronika-reader/src/overview/qualification_fixture.rs`

**Interfaces:**
- Consumes: Task 5 collection types
- Produces: collection status mapped from physical section to the view's first `WebInput`
- Preserves: `population == collection.collected` for the same view/timestamp

- [ ] **Step 1: Write failing projection tests**

Build real PGM fixtures containing source rows plus snapshot/collection coverage.
Assert statements, both plan forks, tables, and indexes map to the intended view.
Also cover complete zero rows, exact top-N, unknown total after one database failure,
overflow/loss, revision conflicts, and duplicate facts.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p kronika-reader collection
```

- [ ] **Step 3: Decode only the two small coverage sections in addition to view inputs**

Add `snapshot_coverage` and `collection_coverage` to the build-time needed section
set. Resolve each coverage row's `section_type_id` through the registry and match
that physical section name against the first `WebInput` of each target view.

- [ ] **Step 4: Canonicalize coverage before building views**

Apply the same semantic rules as Task 4. An exact total is emitted only when proven;
failure/loss maps to `source_total=None`. If coverage supplies an attempted timestamp
with no source rows, insert population `collected` (including zero) so the attempted
snapshot is addressable. If rows exist, require their population to equal collected.

- [ ] **Step 5: Prove web-index read isolation**

Extend the qualification fixture so a published summary contains collection status.
Assert subsequent `read_ui_summary` changes only OVF `FactReadStats`; it must not
change `PgmBodyReadStats` or request a dictionary/entity-series block.

- [ ] **Step 6: Verify GREEN and commit**

```bash
cargo test -p kronika-reader
git add crates/kronika-reader/src/overview
git commit -m "feat(reader): project exact coverage into ui summaries"
```

### Task 7: Web API and generated OpenAPI

**Files:**
- Modify: `bins/pg_kronika-web/src/ui/data.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_data.rs`
- Generate: `bins/pg_kronika-web/openapi/openapi.yaml`
- Generate: `bins/pg_kronika-web/openapi/paths/ui.yaml`
- Generate: `bins/pg_kronika-web/openapi/schemas/ui.yaml`

**Interfaces:**
- Consumes: `UiSummaryBlock::collection_state_at`
- Produces: nullable `ViewSummaryItem.collection`
- Produces: exact JSON fields `collected`, `source_total`, `read_state`, `visibility`

- [ ] **Step 1: Write failing endpoint tests**

Publish a real OVF from a PGM fixture and assert:

```json
{
  "collection": {
    "collected": 500,
    "source_total": 4800,
    "read_state": "source_limit",
    "visibility": "full"
  }
}
```

Add complete, permission/restricted, read-failure/null, and absent-coverage/null cases.
Retain assertions for existing `population`, `status`, and `notable`.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p pg_kronika-web ui_summary
```

- [ ] **Step 3: Add DTOs and string mappings**

```rust
#[derive(Debug, Serialize, ToSchema)]
struct CollectionStatusDto {
    collected: u64,
    #[schema(required = true)]
    source_total: Option<u64>,
    read_state: &'static str,
    visibility: &'static str,
}
```

Add `#[schema(required = true)] collection: Option<CollectionStatusDto>` to each
view. Resolve it from the same summary timestamp as population. Do not expose
`total_quality`, lower-bound fields, or estimates.

- [ ] **Step 4: Regenerate and verify OpenAPI**

```bash
make openapi
cargo test -p pg_kronika-web
git diff --check
```

- [ ] **Step 5: Commit**

```bash
git add bins/pg_kronika-web/src bins/pg_kronika-web/openapi
git commit -m "feat(web): expose exact view collection coverage"
```

### Task 8: Cross-workspace verification

**Files:**
- Modify only files required by a failing verification; do not broaden scope.

- [ ] **Step 1: Format and inspect generated changes**

```bash
cargo fmt --all
git diff --check
git status --short
```

- [ ] **Step 2: Run affected lint and test suites**

```bash
cargo clippy -p kronika-source-pg -p pg_kronika-collector -p kronika-reader -p pg_kronika-web --all-targets
cargo test -p kronika-source-pg
cargo test -p pg_kronika-collector
cargo test -p kronika-reader
cargo test -p pg_kronika-web
```

- [ ] **Step 3: Run repository CI equivalents**

```bash
cargo fmt --all --check
make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
cargo clippy --workspace --all-targets
cargo test --workspace
```

- [ ] **Step 4: Inspect PostgreSQL load invariants**

Review every emitted SQL string and confirm one occurrence of its source SRF/view,
no scalar source count, and no query added solely for web coverage. Confirm the web
handler reaches only directory metadata and `UiSummary` reads.

- [ ] **Step 5: Commit verification-only fixes, if any**

```bash
git add -u
git commit -m "test: qualify exact collection coverage"
```

Do not create an empty commit when verification requires no fixes.
