# Web Index Producer And Consumers Implementation Plan

**Статус: PARTIAL.**

- **Уже реализовано:** shared projection registry, bounded builder,
  cold/live/restart wiring, selective OVF reads, summary/heatmap routes,
  OpenAPI и endpoint tests.
- **Осталось:** финальные `resource_limited` states вместо whole-build abort;
  stable `corrupt_ovf`/`resource_limited`/`response_too_large` и OpenAPI;
  producer acceptance с real statements series logical ID 2; hand-checked
  multi-segment local-top-K merge с lower/truth/upper, `unseen_upper`,
  exact/approx и missing ≠ zero; structural PGM-body-read/512-KiB
  qualification и BDD/qualification для catalog, summary и heatmap.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Наполнять `UiSummary` и `EntitySeries` при построении OVF и обслуживать `GET /v1/views/summary` и `GET /v1/timeline/heatmap` без чтения PGM.

**Architecture:** Стабильные коды view, metric, unit, формулы и требования к входам живут в dependency-free реестре `kronika-analytics`. Reader исполняет этот реестр один раз при cold/live extraction, хранит готовые web-блоки в `SegmentFacts` и публикует их в OVF. Web открывает sidecar по уже выбранному descriptor, читает только directory и адресованный блок, а затем объединяет summary или локальные top-K в ограниченный ответ.

**Tech Stack:** Rust, `kronika-analytics`, `kronika-registry`, `kronika-reader`, Axum, Serde, существующий OVF codec и `LocalDirSnapshot`.

## Global Constraints

- Поддерживать ровно девять view с `view_code` `1..=9`.
- Не вводить режим совместимости или отдельное имя версии формата.
- Холодный `heatmap` читает только `EntitySeries(view_code)` пересекающихся сегментов и `0` байт PGM.
- `summary` читает только `UiSummary` выбранных сегментов и `0` байт PGM.
- Диапазон `heatmap` не больше 24 часов; `buckets=1..=256`, `top=1..=64`.
- Missing bucket, observed zero, gated и resource limit остаются различимы.
- Локальный top-64 точен; объединение диапазона публикует доказуемые `lower`, `upper`, `unseen_upper` и `ranking.exact`.
- Все накопители ограничены существующими `Bounds`; неполнота публикуется явно.
- Локальные Rust-проверки используют `--target aarch64-apple-darwin`.

---

### Task 1: Shared Projection Registry

**Files:**
- Create: `crates/kronika-analytics/src/web_projection.rs`
- Modify: `crates/kronika-analytics/src/lib.rs`
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`

**Interfaces:**
- Produces: `web_views() -> &'static [WebView]`.
- Produces: `web_view_by_code(u16) -> Option<&'static WebView>`.
- Produces: `web_view_by_name(&str) -> Option<&'static WebView>`.
- `WebView` carries stable code/revision/identity/input sections and `&'static [WebMetric]`.
- `WebMetric` carries stable numeric and public codes, revision, unit, aggregation, formula and canonical flag.

- [ ] **Step 1: Write failing registry tests**

Add unit tests that hand-check the ordered view list, uniqueness of numeric and public codes, exactly one canonical metric per view, and the statements metric formulas.

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p kronika-analytics web_projection --target aarch64-apple-darwin
```

Expected: compilation fails because `web_projection` does not exist.

- [ ] **Step 3: Implement the static registry**

Use static slices and closed enums for aggregation/formula. Keep the module free of registry, reader, HTTP and serialization dependencies.

- [ ] **Step 4: Make the catalog consume the registry**

Map the shared view/metric metadata into the existing source-aware DTO. Keep columns, joins and presets in web until their consumers exist.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
cargo test -p kronika-analytics web_projection --target aarch64-apple-darwin
cargo test -p pg_kronika-web --lib ui_catalog --target aarch64-apple-darwin
```

Expected: both suites pass.

### Task 2: Bounded Web Index Builder

**Files:**
- Create: `crates/kronika-reader/src/overview/web_index/build.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Modify: `crates/kronika-reader/src/overview/facts.rs`
- Test: `crates/kronika-reader/src/overview/facts.rs`

**Interfaces:**
- Produces: `WebIndexBlocks { summary: UiSummaryBlock, series: Vec<EntitySeriesBlock> }`.
- Produces: `build_web_index(unit: &PgmUnit<R>, min_ts: i64, max_ts: i64, bounds: &Bounds) -> Result<WebIndexBlocks, BuildError>`.
- `SegmentFacts` owns the populated summary and canonical series vector.

- [ ] **Step 1: Write a failing producer test**

Build the existing all-family PGM fixture, extract facts, encode OVF, and assert that `UiSummary` is non-empty and the statements `EntitySeries` block is addressable by logical ID `2`.

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p kronika-reader facts::tests::all_family_ovf_contains_populated_web_index --target aarch64-apple-darwin
```

Expected: failure because the summary is empty and no series block exists.

- [ ] **Step 3: Decode only projection inputs**

Select catalog entries by shared input section names, decode each selected body once inside the builder, resolve labels through the segment dictionary, and charge row/cell/string memory against `Bounds`.

- [ ] **Step 4: Build exact summary snapshots**

For each view, group primary-input rows by `ts`, store exact row counts and status, then build one shared sorted timestamp table and presence mask.

- [ ] **Step 5: Build local top-K series**

Use contract identity columns as canonical typed identity bytes. Evaluate declared counter/gauge/event formulas per entity and grid bucket, preserve presence independently from numeric zero, rank by exact score, retain at most 64, and quantize only retained bucket values.

- [ ] **Step 6: Integrate cold, live and restart paths**

Store blocks on `SegmentFacts`, include their resident bytes, encode them through `FactFile::build`, reload them in `from_reader_with_stats`, and rebuild them from the sealed unit during promotion so published bytes do not depend on partitioning.

- [ ] **Step 7: Verify GREEN**

Run:

```bash
cargo test -p kronika-reader all_family_ovf_contains_populated_web_index --target aarch64-apple-darwin
cargo test -p kronika-reader overview::facts --target aarch64-apple-darwin
```

Expected: producer and existing fact lifecycle tests pass.

### Task 3: Selective Sidecar Read API

**Files:**
- Modify: `crates/kronika-reader/src/snapshot.rs`
- Create: `crates/kronika-reader/src/overview/web_index/read.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Test: `crates/kronika-reader/src/snapshot/tests.rs`

**Interfaces:**
- Produces: `LocalDirSnapshot::read_ui_summary(descriptor) -> Result<(UiSummaryBlock, FactReadStats), WebIndexReadError>`.
- Produces: `LocalDirSnapshot::read_entity_series(descriptor, view_code) -> Result<(Option<EntitySeriesBlock>, FactReadStats), WebIndexReadError>`.
- Both methods derive and validate exact OVF identity from descriptor metadata and never open a PGM body.

- [ ] **Step 1: Write failing selective-read tests**

Publish a populated sidecar, select one descriptor, read summary and view `2`, and assert `FactReadStats` includes only header, directory and selected body. Assert an absent view returns `None` with no body read.

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p kronika-reader selective_web_index_read --target aarch64-apple-darwin
```

Expected: compilation fails because the snapshot methods do not exist.

- [ ] **Step 3: Implement exact sidecar opens**

Reuse `FactFileReader::open` and `read_block`; do not call `SegmentFacts::from_reader` or decode unrelated blocks.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p kronika-reader selective_web_index_read --target aarch64-apple-darwin
```

Expected: tests pass with bounded read counters.

### Task 4: OVF-Only View Summary

**Files:**
- Create: `bins/pg_kronika-web/src/ui/data.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/lib.rs`
- Modify: `bins/pg_kronika-web/src/problem.rs`
- Test: `bins/pg_kronika-web/src/tests/ui_data.rs`

**Interfaces:**
- Produces: `GET /v1/views/summary?at=<i64>`.
- Response includes all nine view codes, latest snapshot at or before `at`, exact population or `null`, status, notable flag and quality.

- [ ] **Step 1: Write failing endpoint tests**

Assert the route returns exact per-snapshot population, distinguishes gated from empty, rejects unknown query parameters, and leaves PGM body-read counters unchanged.

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p pg_kronika-web --lib ui_summary --target aarch64-apple-darwin
```

Expected: `404` because the route is absent.

- [ ] **Step 3: Implement descriptor selection and merge**

Select only descriptors whose range can contain a snapshot at or before `at`, scan newest first, and stop resolving each view after its latest snapshot is found.

- [ ] **Step 4: Serialize bounded DTO and stable errors**

Reject unknown parameters. Map corrupt sidecar to `corrupt_ovf`; map hard read/response bounds to `resource_limited` or `response_too_large`.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
cargo test -p pg_kronika-web --lib ui_summary --target aarch64-apple-darwin
```

Expected: endpoint tests pass and PGM body reads remain zero.

### Task 5: OVF-Only Heatmap Merge

**Files:**
- Create: `bins/pg_kronika-web/src/ui/heatmap.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/lib.rs`
- Modify: `bins/pg_kronika-web/src/problem.rs`
- Test: `bins/pg_kronika-web/src/tests/ui_data.rs`

**Interfaces:**
- Produces: `GET /v1/timeline/heatmap?view=<code>&metric=<code>&from=<i64>&to=<i64>&buckets=<1..=256>&top=<1..=64>`.
- Merge consumes `EntitySeriesBlock` only and returns bounded rows with base64url entity token, newest label, score bounds, values, ranking proof and quality.

- [ ] **Step 1: Write failing merge and route tests**

Use hand-checked blocks where the global winner appears in only one local top-K. Assert values, missing versus zero, lower/upper bounds, `unseen_upper`, exactness, range validation, unknown view/metric, and zero PGM reads.

- [ ] **Step 2: Verify RED**

Run:

```bash
cargo test -p pg_kronika-web --lib ui_heatmap --target aarch64-apple-darwin
```

Expected: compilation or route failure because the merge is absent.

- [ ] **Step 3: Implement bounded candidate union**

Union typed identities from at most the selected segment cap, aggregate retained values into the requested half-open grid, apply metric aggregation, accumulate finite score bounds from cutoff metadata, and retain at most requested `top`.

- [ ] **Step 4: Enforce request and response limits**

Reject ranges over 24 hours, invalid bucket/top counts and unknown parameters. Preflight label, row and values bytes and return `response_too_large` above 512 KiB.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
cargo test -p pg_kronika-web --lib ui_heatmap --target aarch64-apple-darwin
```

Expected: merge and route tests pass with selective OVF reads only.

### Task 6: Contract, Qualification And PR Update

**Files:**
- Modify: `openapi/pg-kronika-web.yaml`
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`
- Modify: qualification tests only where needed to assert selective-read budgets

**Interfaces:**
- Produces: documented routes, schemas, stable problem codes and structural read budgets matching executable behavior.

- [ ] **Step 1: Update OpenAPI and operator docs**

Document exact query bounds, null/zero semantics, approximation metadata and OVF-only read behavior.

- [ ] **Step 2: Run focused and workspace checks**

Run:

```bash
cargo test -p kronika-analytics --target aarch64-apple-darwin
cargo test -p kronika-reader --lib --target aarch64-apple-darwin
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin
cargo clippy -p kronika-analytics -p kronika-reader -p pg_kronika-web --all-targets --target aarch64-apple-darwin -- -D warnings
cargo fmt --check
cargo run -p xtask --target aarch64-apple-darwin -- check-deps
git diff --check
```

Expected: all commands pass.

- [ ] **Step 3: Commit and push**

Commit coherent TDD slices, push `feat/web-index`, and verify PR #135 checks.
