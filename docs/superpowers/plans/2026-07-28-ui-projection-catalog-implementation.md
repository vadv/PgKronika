# UI Projection Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Реализовать нормативный source-aware каталог девяти UI-представлений и
`GET /v1/ui/catalog`, чтобы клиент не зашивал physical sections, формулы, gates,
колонки и presets.

**Architecture:** Типизированный каталог живёт в `pg_kronika-web` и отделён от
HTTP-адаптера. Статическая часть задаёт стабильные `view_code`, revision,
формулы, колонки и presets; source-aware projection накладывает availability по
наблюдаемым type IDs из PGM catalogs, не декодируя section bodies. HTTP-handler
валидирует source, сериализует bounded response и поддерживает ETag.

**Tech Stack:** Rust 2024, Axum 0.8, serde, `kronika-registry`,
`LocalDirSnapshot`, встроенный test harness.

## Global Constraints

- Каталог содержит ровно девять view из принятой web API specification.
- `view_code`, `view_revision`, `identity_revision` и metric revisions
  стабильны и ненулевые.
- Availability имеет только `available`, `gated`, `not_collected` и
  `unsupported_type`.
- PSS всегда `not_collected`, пока collector не пишет bounded `smaps_rollup`.
- Activity `cpu` и `io` доступны только при наличии activity и process inputs;
  совпадение PID не является join proof.
- Endpoint читает только PGM catalog metadata и не декодирует section bodies.
- Ответ catalog не превышает 512 КиБ.
- Неизвестные и повторяющиеся query parameters отклоняются общим parser.
- Все production changes выполняются через наблюдаемый RED-тест.
- Локальные Rust-проверки используют `--target aarch64-apple-darwin`.

---

### Task 1: Типизированный projection catalog

**Files:**
- Create: `bins/pg_kronika-web/src/ui/mod.rs`
- Create: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/lib.rs`

**Interfaces:**
- Produces: `ProjectionCatalog::for_type_ids(&BTreeSet<u32>)`.
- Produces: serializable `ViewSpec`, `MetricSpec`, `ColumnSpec`, `PresetSpec`,
  `Availability`, `Scope` and `ValueType`.
- Stable view codes: activity `1`, statements `2`, plans `3`, tables `4`,
  indexes `5`, vacuum `6`, processes `7`, locks `8`, events `9`.

- [x] **Step 1: Write pure failing catalog tests**

Add tests that assert:

```rust
#[test]
fn catalog_exposes_all_nine_views_in_stable_code_order() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    assert_eq!(
        catalog.views().iter().map(ViewSpec::code).collect::<Vec<_>>(),
        ["activity", "statements", "plans", "tables", "indexes",
         "vacuum", "processes", "locks", "events"]
    );
}

#[test]
fn statements_metrics_publish_explicit_formulas_and_units() {
    // time, calls, io and temp have literal formulas from the accepted spec.
}

#[test]
fn activity_cpu_requires_both_activity_and_process_inputs() {
    // Activity alone leaves cpu gated; both type families make it available.
}

#[test]
fn process_pss_is_not_collected_even_when_process_input_exists() {
    // The catalog never fabricates current PSS support.
}
```

- [x] **Step 2: Verify RED**

Run:

```bash
cargo test -p pg_kronika-web ui::catalog --lib \
  --target aarch64-apple-darwin
```

Expected: compile failure because module and types do not exist.

- [x] **Step 3: Implement the static catalog and availability overlay**

Keep static definitions in functions returning owned, bounded structs. Each
view declares:

```rust
ViewSpec {
    view_code,
    code,
    view_revision: 1,
    scope,
    identity_revision: 1,
    inputs,
    joins,
    metrics,
    columns,
    presets,
    canonical_metric,
}
```

Use registry type families for direct inputs. Metrics and columns carry their
own required input families; availability is `available` only when every
required family has an observed supported type. Optional extension or OS
inputs become `gated`. PSS overrides this calculation with `not_collected`.

- [x] **Step 4: Verify GREEN**

Run the focused command from Step 2. Expected: all catalog tests pass.

- [x] **Step 5: Commit the model**

```bash
git add bins/pg_kronika-web/src/ui \
  bins/pg_kronika-web/src/lib.rs \
  docs/superpowers/plans/2026-07-28-ui-projection-catalog-implementation.md
git commit -m "feat(web): add UI projection catalog"
```

### Task 2: Source-aware HTTP endpoint and ETag

**Files:**
- Create: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/lib.rs`
- Modify: `bins/pg_kronika-web/src/problem.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_catalog.rs`

**Interfaces:**
- Consumes: `ProjectionCatalog::for_type_ids`.
- Produces: `GET /v1/ui/catalog?source=<u64>`.
- Produces: `unknown_source` RFC 9457 problem.
- Produces: strong ETag over canonical serialized catalog bytes.

- [x] **Step 1: Write failing endpoint tests**

Use real `AppState`, router and PGM fixtures:

```rust
#[tokio::test]
async fn ui_catalog_returns_nine_views_for_a_known_source() {}

#[tokio::test]
async fn ui_catalog_keeps_pss_not_collected() {}

#[tokio::test]
async fn ui_catalog_unknown_source_is_not_found() {}

#[tokio::test]
async fn ui_catalog_rejects_unknown_parameters() {}

#[tokio::test]
async fn ui_catalog_honors_if_none_match() {}
```

The first test asserts the route, source validation, stable view ordering and
response revision. The ETag test performs one real request, reuses its response
header and expects `304 Not Modified` with an empty body.

- [x] **Step 2: Verify RED**

Run:

```bash
cargo test -p pg_kronika-web ui_catalog --lib \
  --target aarch64-apple-darwin
```

Expected: `404` because the route is not registered.

- [x] **Step 3: Implement bounded source type discovery**

In the blocking worker:

1. clone the published snapshot;
2. select units whose `source_id` matches;
3. return `unknown_source` when none match;
4. read catalog entries only, collecting non-empty known type IDs in a
   `BTreeSet<u32>`;
5. stop once every input family in the catalog has been observed;
6. map catalog read failure to the existing `store_read_failed` problem.

No section body may be decoded by this path.

- [x] **Step 4: Implement response and ETag**

Serialize one canonical body:

```json
{
  "revision": 1,
  "views": []
}
```

Reject a serialized body above 512 KiB before constructing the response.
Compute the quoted strong ETag from SHA-256 of the body. Match
`If-None-Match` exactly; a match returns `304` and the same ETag.

- [x] **Step 5: Verify GREEN**

Run the focused endpoint command from Step 2, then:

```bash
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin
cargo clippy -p pg_kronika-web --all-targets \
  --target aarch64-apple-darwin -- -D warnings
```

- [x] **Step 6: Commit the endpoint**

```bash
git add bins/pg_kronika-web/src/ui \
  bins/pg_kronika-web/src/lib.rs \
  bins/pg_kronika-web/src/problem.rs \
  bins/pg_kronika-web/src/tests
git commit -m "feat(web): expose source-aware UI catalog"
```

### Task 3: Contract and API artifact

**Files:**
- Modify: `bins/pg_kronika-web/openapi.json`
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`

**Interfaces:**
- Consumes: the exact JSON and problem contracts from Task 2.
- Produces: documented `GET /v1/ui/catalog`.

- [x] **Step 1: Add the route to OpenAPI and README mirrors**

Document the required `source`, ETag/`304`, stable availability values,
`unknown_source`, and the 512 KiB response limit. Keep English and Russian
README structure synchronized.

- [x] **Step 2: Validate the API artifact**

Run:

```bash
python3 -m json.tool bins/pg_kronika-web/openapi.json >/dev/null
cargo fmt --all -- --check
cargo test -p pg_kronika-web ui_catalog --lib \
  --target aarch64-apple-darwin
cargo run -p xtask --target aarch64-apple-darwin -- check-deps
git diff --check
```

- [x] **Step 3: Commit and push**

```bash
git add bins/pg_kronika-web/openapi.json \
  bins/pg_kronika-web/README.md \
  bins/pg_kronika-web/README.ru.md
git commit -m "docs(web): document UI projection catalog"
git push
```

After this plan is green, continue with a separate implementation plan for the
populated web-index producer and the OVF-only `views/summary` and
`timeline/heatmap` consumers.
