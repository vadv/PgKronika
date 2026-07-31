# Web UI v5 API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Реализовать полный backend-only Web API proposal v5, добавить пять
недостающих URL в runtime router и Swagger и расширить четыре существующих
контракта без frontend-кода.

**Architecture:** Новые UI consumers используют существующие PGM/OVF reader
границы. `timeline/spine` получает отдельную внутреннюю host-series projection
в уже адресуемом `EntitySeries` OVF, context и entity читают только выбранные
PGM, data quality работает по descriptors/summary и сохранённому producer
status, storage использует bounded `kronika-layout` scan. Rust DTO с
`ToSchema` остаются единственным источником OpenAPI.

**Tech Stack:** Rust 1.96, Axum, utoipa/utoipa-axum, serde, PgKronika
PGM/OVF reader, `kronika-layout`, Cargo tests, generated multifile OpenAPI.

## Global Constraints

- Реализовать только read-only backend; frontend и опасные write operations не
  добавлять.
- Не добавлять `/v1/sources`, query-параметр `source`, source-aware cursor,
  cache key или deep-link.
- Текущие 16 `/v1` операций расширить ровно пятью `GET`: `/v1/ui/context`,
  `/v1/timeline/spine`, `/v1/data/quality`,
  `/v1/entity/{view}/{entity}`, `/v1/storage`.
- Неизвестные и повторные query-параметры отклонять до storage I/O.
- `null` не подменяет наблюдённый ноль и для отсутствующего факта имеет
  машинную причину.
- Context и entity point читают не больше одного PGM выбранного снимка.
- Spine читает только адресуемые OVF host-series blocks и 0 raw PGM.
- Data quality читает descriptors, summary/status metadata и 0 section bodies.
- Entity history ограничен 6 часами, 32 PGM, 2 000 snapshots и 2 МиБ JSON.
- Локальные исполняемые Rust-проверки на macOS используют
  `--target aarch64-apple-darwin`; default musl target проверяется CI.
- Каждый behavior change выполняется RED-GREEN-REFACTOR.
- После полной проверки запушить `feat/web-ui-v5-api` и создать PR на ревью.

---

### Task 1: Общий wire-контракт и snapshot helpers

**Files:**
- Modify: `bins/pg_kronika-web/src/api_error.rs`
- Modify: `bins/pg_kronika-web/src/params.rs`
- Create: `bins/pg_kronika-web/src/ui/snapshot.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/api_errors.rs`
- Test: `bins/pg_kronika-web/src/tests/ui_frame.rs`

**Interfaces:**
- Produces: `QueryParameter::{Columns,Include}`,
  `ExpectedValue::{EntityToken,ProjectionColumnList}`,
  `QueryConstraint::{PointOrHistory,HistorySupported,PresetOrColumns}`.
- Produces:
  `snapshot::resolve_snapshot_at(snapshot, at_us) -> Result<Option<ResolvedSnapshotAt>, WebIndexReadError>`.
- Produces:
  `snapshot::resolve_view_snapshot(snapshot, view, at_us) -> Result<ResolvedViewSnapshot, WebIndexReadError>`.
- Consumes: existing `UiSummaryBlock`, `SnapshotNeighbors`, `SegmentDescriptor`.

- [x] **Step 1: Write failing closed-registry and snapshot-selection tests**

```rust
#[test]
fn v5_query_names_and_constraints_are_closed_wire_values() {
    assert_eq!(serde_json::to_value(QueryParameter::Columns).unwrap(), "columns");
    assert_eq!(serde_json::to_value(ExpectedValue::EntityToken).unwrap(), "entity_token");
    assert_eq!(
        serde_json::to_value(QueryConstraint::PointOrHistory).unwrap(),
        "point_or_history"
    );
}

#[test]
fn snapshot_resolver_returns_the_latest_view_snapshot_at_or_before_at() {
    let resolved = resolve_view_snapshot(&fixture_snapshot(), events_view(), 1_550).unwrap();
    assert_eq!(resolved.neighbors.unwrap().current, 1_500);
}

#[test]
fn snapshot_at_resolver_is_independent_of_public_view_coverage() {
    let resolved = resolve_snapshot_at(&fixture_snapshot(), 1_550)
        .unwrap()
        .expect("snapshot");
    assert_eq!(resolved.timestamp_us, 1_500);
}
```

- [x] **Step 2: Run RED tests**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  v5_query_names_and_constraints_are_closed_wire_values
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  snapshot_resolver_returns_the_latest_view_snapshot_at_or_before_at
```

Expected: compilation fails because the new enum variants and
`resolve_view_snapshot` do not exist.

- [x] **Step 3: Add closed values and extract the shared resolver**

```rust
pub(crate) struct ResolvedViewSnapshot {
    pub(crate) neighbors: Option<SnapshotNeighbors>,
    pub(crate) current_descriptor: Option<SegmentDescriptor>,
    pub(crate) previous_descriptor: Option<SegmentDescriptor>,
    pub(crate) current_status: Option<SnapshotStatus>,
    pub(crate) previous_status: Option<SnapshotStatus>,
    pub(crate) fallback_status: Option<SnapshotStatus>,
    pub(crate) next: Option<i64>,
}

pub(crate) struct ResolvedSnapshotAt {
    pub(crate) timestamp_us: i64,
    pub(crate) descriptor: SegmentDescriptor,
}

pub(crate) fn resolve_snapshot_at(
    snapshot: &LocalDirSnapshot,
    at_us: i64,
) -> Result<Option<ResolvedSnapshotAt>, WebIndexReadError> {
    // Select the latest descriptor at or before `at_us` independently of
    // public-view summary coverage.
}

pub(crate) fn resolve_view_snapshot(
    snapshot: &LocalDirSnapshot,
    view: &WebView,
    at_us: i64,
) -> Result<ResolvedViewSnapshot, WebIndexReadError> {
    // Preserve the existing summary scan, revision checks and global
    // previous/current/next ordering formerly local to frame.
}
```

Update frame to call the extracted helper without changing its existing
response.

- [x] **Step 4: Run GREEN and regression tests**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  snapshot_resolver_returns_the_latest_view_snapshot_at_or_before_at
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_frame
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::api_errors
```

Expected: all selected tests pass.

- [x] **Step 5: Commit**

```sh
git add bins/pg_kronika-web/src/api_error.rs \
  bins/pg_kronika-web/src/params.rs \
  bins/pg_kronika-web/src/ui/snapshot.rs \
  bins/pg_kronika-web/src/ui/mod.rs \
  bins/pg_kronika-web/src/ui/frame/projection.rs \
  bins/pg_kronika-web/src/tests/api_errors.rs \
  bins/pg_kronika-web/src/tests/ui_frame.rs
git commit -m "refactor(web): share v5 query and snapshot contracts"
```

### Task 2: OVF host-series projection и `/v1/timeline/spine`

**Files:**
- Modify: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/build.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/series.rs`
- Modify: `crates/kronika-reader/src/overview/facts.rs`
- Create: `bins/pg_kronika-web/src/ui/spine.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_spine.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`

**Interfaces:**
- Produces: `HOST_SIGNALS_VIEW_CODE = 10`,
  metrics `LOAD_PER_CPU_METRIC_CODE = 1`, `PSI_IO_SOME_METRIC_CODE = 2`.
- Produces:
  `spine(snapshot, live, SpineRequest) -> Result<SpineResponse, SpineError>`.
- Uses existing `EntitySeriesBlock` addressing; the host projection is internal
  and never appears as a tenth catalog view.

- [x] **Step 1: Write failing reader projection tests**

```rust
#[test]
fn web_index_projects_load_per_cpu_and_io_psi_into_hidden_host_series() {
    let facts = all_family_fixture_with_load_psi_and_four_cpus();
    let block = facts
        .entity_series()
        .iter()
        .find(|block| block.view_code() == HOST_SIGNALS_VIEW_CODE)
        .expect("host series");
    assert_eq!(metric_values(block, LOAD_PER_CPU_METRIC_CODE), [Some(0.5)]);
    assert_eq!(metric_values(block, PSI_IO_SOME_METRIC_CODE), [Some(34.0)]);
}
```

- [x] **Step 2: Run reader RED test**

```sh
cargo test -p kronika-reader --lib --target aarch64-apple-darwin \
  web_index_projects_load_per_cpu_and_io_psi_into_hidden_host_series
```

Expected: compilation fails because the host constants/projection do not exist.

- [x] **Step 3: Build the bounded hidden host block**

```rust
pub const HOST_SIGNALS_VIEW_CODE: u16 = 10;
pub const HOST_SIGNALS_VIEW_REVISION: u16 = 1;
pub const HOST_SIGNALS_IDENTITY_REVISION: u16 = 1;
pub const LOAD_PER_CPU_METRIC_CODE: u16 = 1;
pub const PSI_IO_SOME_METRIC_CODE: u16 = 2;

fn build_host_series(
    decoded: &[DecodedSection],
    grid: TimeGrid,
    bounds: &Bounds,
) -> Result<Option<EntitySeriesBlock>, BuildError> {
    // Count distinct host-scope os_topology.cpu_id per bucket, divide load1 by
    // that proven count, and retain os_psi resource=io/scope=host some_avg10.
    // Missing inputs produce MetricStatus, never an observed zero.
}
```

Append the block to `WebIndexBlocks.series`; keep `UiSummary` at nine views.

- [x] **Step 4: Run reader GREEN tests**

```sh
cargo test -p kronika-reader --lib --target aarch64-apple-darwin \
  web_index_projects_load_per_cpu_and_io_psi_into_hidden_host_series
cargo test -p kronika-reader --lib --target aarch64-apple-darwin \
  overview::web_index
```

Expected: host projection and all existing web-index tests pass.

- [x] **Step 5: Write failing HTTP spine tests**

```rust
#[tokio::test]
async fn spine_returns_aligned_host_series_without_raw_pgm_reads() {
    let (status, body) = serve_host_fixture(
        "/v1/timeline/spine?from=1000&to=2000&buckets=2"
    ).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["series"][0]["code"], "load_per_cpu");
    assert_eq!(body["series"][1]["code"], "psi_io_some");
    assert_eq!(body["series"][0]["values"].as_array().unwrap().len(), 2);
}
```

- [x] **Step 6: Run HTTP RED test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  spine_returns_aligned_host_series_without_raw_pgm_reads
```

Expected: route returns `404 route_not_found`.

- [x] **Step 7: Implement spine request, merge, DTO and handler**

```rust
pub(crate) struct SpineRequest {
    pub(crate) from_us: i64,
    pub(crate) to_us: i64,
    pub(crate) buckets: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SpineSeriesDto {
    code: &'static str,
    unit: &'static str,
    aggregation: &'static str,
    values: Vec<Option<f64>>,
    value_statuses: Vec<ValueStatusDto>,
}
```

Enforce `1..=512`, 24 hours and 256 KiB before publishing. Merge sealed and
live host blocks onto one half-open grid and register the handler through
`OpenApiRouter`.

- [x] **Step 8: Run GREEN tests and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_spine
cargo test -p kronika-reader --lib --target aarch64-apple-darwin \
  overview::web_index
git add crates/kronika-reader/src/overview/web_index \
  crates/kronika-reader/src/overview/facts.rs \
  bins/pg_kronika-web/src/ui \
  bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/tests
git commit -m "feat(web): add indexed timeline spine API"
```

### Task 3: `/v1/ui/context`

**Files:**
- Create: `bins/pg_kronika-web/src/ui/context.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_context.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`

**Interfaces:**
- Produces:
  `build_context(snapshot, at_us, ContextLimits) -> Result<ContextResponse, ContextError>`.
- Consumes: `snapshot::resolve_snapshot_at`, one `SealedQuerySession`,
  `instance_metadata`, `pg_stat_database`, `replication_instance`,
  `replication_replicas`, `os_topology`.

- [x] **Step 1: Write failing context contract tests**

```rust
#[tokio::test]
async fn context_returns_instance_databases_replication_and_cpu_from_one_snapshot() {
    let (status, body) = serve_context_fixture("/v1/ui/context?at=1500").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["snapshot_ts_us"], "1500");
    assert_eq!(body["instance"]["role"], "primary");
    assert_eq!(body["host"]["logical_cpu_count"], 4);
    assert_eq!(body["databases"][0]["name"], "orders");
}
```

- [x] **Step 2: Run RED test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  context_returns_instance_databases_replication_and_cpu_from_one_snapshot
```

Expected: route is absent.

- [x] **Step 3: Implement typed database identity and context projection**

```rust
fn database_entity(system_identifier: Option<i64>, oid: u32) -> String {
    // base64url(version, system identifier presence/value, database OID)
}

pub(crate) fn build_context(
    snapshot: &LocalDirSnapshot,
    at_us: i64,
    limits: ContextLimits,
) -> Result<ContextResponse, ContextError> {
    // Select one descriptor/snapshot, query the five allowlisted sections in
    // one SealedQuerySession, and derive only fields proven at that timestamp.
}
```

Use `role_reason`, `pg_system_identifier_reason` and `replay_lag_reason` for
nullable facts. Cap response at 256 KiB.

- [x] **Step 4: Add admission/error tests**

```rust
#[tokio::test]
async fn context_rejects_unknown_duplicate_and_missing_at_before_io() {
    assert_bad_request("/v1/ui/context");
    assert_bad_request("/v1/ui/context?at=1&at=2");
    assert_bad_request("/v1/ui/context?at=1&source=x");
}
```

- [x] **Step 5: Run GREEN suite and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_context
git add bins/pg_kronika-web/src/ui/context.rs \
  bins/pg_kronika-web/src/ui/handlers.rs \
  bins/pg_kronika-web/src/ui/mod.rs \
  bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/tests
git commit -m "feat(web): add UI context API"
```

### Task 4: Producer status и `/v1/data/quality`

**Files:**
- Create: `crates/kronika-layout/src/producer_status.rs`
- Modify: `crates/kronika-layout/src/lib.rs`
- Modify: `crates/kronika-layout/src/root.rs`
- Modify: `bins/pg_kronika-collector/Cargo.toml`
- Create: `bins/pg_kronika-collector/src/producer_status.rs`
- Modify: `bins/pg_kronika-collector/src/main.rs`
- Create: `bins/pg_kronika-web/src/ui/quality.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_quality.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`

**Interfaces:**
- Produces: atomically replaced root-local `producer-status.json` revision 1.
- Produces:
  `read_producer_status(root) -> Result<Option<ProducerStatus>, ProducerStatusError>`.
- Produces:
  `build_data_quality(snapshot, status, from_us, to_us) -> DataQualityResponse`.

- [x] **Step 1: Write failing producer status codec/atomicity tests**

```rust
#[test]
fn producer_status_round_trips_running_and_stopped_states() {
    let running = ProducerStatus::running(42, 1000, 2000, retention());
    write_producer_status(root.path(), &running).unwrap();
    assert_eq!(read_producer_status(root.path()).unwrap(), Some(running));
}
```

- [x] **Step 2: Run RED layout/collector tests**

```sh
cargo test -p kronika-layout --lib --target aarch64-apple-darwin producer_status
cargo test -p pg_kronika-collector --lib --target aarch64-apple-darwin producer_status
```

Expected: producer status module and constructors are absent.

- [x] **Step 3: Implement bounded status file and collector lifecycle writes**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerStatus {
    pub revision: u16,
    pub state: ProducerState,
    pub collector_pid: u32,
    pub collector_started_at_us: i64,
    pub last_status_at_us: i64,
    pub retention: Option<RetentionStatus>,
}
```

Write through a same-directory temporary, `sync_all`, rename and directory
sync. Update heartbeat after a completed cycle and write `stopped` on a
graceful signal. Reserve the file in bounded root scan so it is not foreign
data.

- [x] **Step 4: Run producer GREEN tests**

```sh
cargo test -p kronika-layout --lib --target aarch64-apple-darwin producer_status
cargo test -p pg_kronika-collector --lib --target aarch64-apple-darwin producer_status
```

- [x] **Step 5: Write failing data-quality HTTP tests**

```rust
#[tokio::test]
async fn data_quality_distinguishes_late_data_gaps_and_proven_stopped_producer() {
    let (status, body) = serve_quality_fixture(
        "/v1/data/quality?from=1000&to=2000"
    ).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["producer"]["state"], "stopped");
    assert_eq!(body["coverage"]["observed_snapshots"], 2);
    assert_eq!(body["gaps"][0]["reason"], "unknown");
}
```

- [x] **Step 6: Run HTTP RED test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  data_quality_distinguishes_late_data_gaps_and_proven_stopped_producer
```

Expected: route is absent.

- [x] **Step 7: Implement descriptor/summary-only quality assembly**

```rust
fn aggregate_status(
    readable: bool,
    freshness: FreshnessState,
    has_gap: bool,
    coverage_partial: bool,
    integrity_degraded: bool,
) -> DataStatus {
    // unavailable > stale > partial > late > fresh
}
```

Read only descriptors, `UiSummary` and producer status. Compute
`age_us=max(0,to-data_through_us)`, never infer `stopped` from age, publish
unknown gap reasons without causal guesses, and cap the response at 512 KiB.

- [x] **Step 8: Run GREEN tests and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_quality
cargo test -p kronika-layout -p pg_kronika-collector --lib \
  --target aarch64-apple-darwin producer_status
git add crates/kronika-layout bins/pg_kronika-collector \
  bins/pg_kronika-web/src/ui bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/tests
git commit -m "feat(web): expose retained data quality"
```

### Task 5: `/v1/storage`

**Files:**
- Create: `bins/pg_kronika-web/src/ui/storage.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_storage.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`

**Interfaces:**
- Produces:
  `build_storage(root, producer_status, StorageLimits) -> Result<StorageResponse, StorageError>`.
- Consumes: `DataRoot::scan`, `DataRoot::filesystem_usage`, producer retention.

- [x] **Step 1: Write failing accounting and HTTP tests**

```rust
#[tokio::test]
async fn storage_counts_each_layout_file_once_and_reports_filesystem_headroom() {
    let (status, body) = serve_storage_fixture("/v1/storage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["used_bytes"]["pgm"], 100);
    assert_eq!(body["used_bytes"]["ovf"], 20);
    assert!(body["filesystem"]["total_bytes"].as_u64().unwrap() > 0);
}
```

- [x] **Step 2: Run RED test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  storage_counts_each_layout_file_once_and_reports_filesystem_headroom
```

Expected: route is absent.

- [x] **Step 3: Implement bounded inventory and forecast**

```rust
pub(crate) fn build_storage(
    root: &Path,
    status: Option<&ProducerStatus>,
    limits: StorageLimits,
) -> Result<StorageResponse, StorageError> {
    // Sum verified PGM, sibling OVF, active journal, quarantine and remaining
    // regular files exactly once. Derive a positive sealed-byte growth rate.
}
```

Return `full_in_days=null` with `full_in_days_reason` when the rate is missing,
non-positive or retention precedes exhaustion. Cap at 64 KiB.

- [x] **Step 4: Run GREEN tests and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_storage
git add bins/pg_kronika-web/src/ui/storage.rs \
  bins/pg_kronika-web/src/ui/handlers.rs \
  bins/pg_kronika-web/src/ui/mod.rs \
  bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/tests
git commit -m "feat(web): add bounded storage API"
```

### Task 6: `/v1/entity/{view}/{entity}` point и history

**Files:**
- Create: `bins/pg_kronika-web/src/ui/entity.rs`
- Create: `bins/pg_kronika-web/src/ui/entity/cursor.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/api_error.rs`
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_entity.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`

**Interfaces:**
- Produces: stable `EntityToken` validation against `identity_revision`.
- Produces:
  `entity_point(snapshot, request) -> Result<EntityPointResponse, EntityError>`.
- Produces:
  `entity_history(snapshot, request) -> Result<EntityHistoryResponse, EntityError>`.
- Reuses projection evaluation and database/user scoped identity; related
  links require explicit `RelationProvenanceDto`.

- [x] **Step 1: Write failing entity mode/token tests**

```rust
#[test]
fn entity_request_requires_exactly_one_point_or_history_mode() {
    assert!(EntityRequest::parse("activity", "token", Some("at=1")).is_ok());
    assert!(EntityRequest::parse(
        "activity", "token", Some("from=1&to=2&columns=pid")
    ).is_ok());
    assert_invalid_constraint("at=1&from=1&to=2&columns=pid", "point_or_history");
}
```

- [x] **Step 2: Run parser RED test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  entity_request_requires_exactly_one_point_or_history_mode
```

Expected: `EntityRequest` is absent.

- [x] **Step 3: Implement parser, opaque cursor and stable event tokens**

```rust
pub(crate) enum EntityMode {
    Point { at_us: i64, include_related: bool },
    History {
        from_us: i64,
        to_us: i64,
        columns: Vec<&'static str>,
        limit: usize,
        cursor: Option<EntityHistoryCursor>,
    },
}
```

Reject malformed base64url/revision before I/O. Replace frame event
section/row ordinals with token bytes containing identity revision, stable
event identity and snapshot binding.

- [x] **Step 4: Write failing point/history/related HTTP tests**

```rust
#[tokio::test]
async fn entity_point_returns_lazy_fields_and_only_proven_related_links() {
    let token = frame_entity_token().await;
    let (status, body) = serve(&format!(
        "/v1/entity/statements/{token}?at=1500&include=related"
    )).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["fields"].as_array().unwrap().iter().any(|f| f["code"] == "query"));
    assert_eq!(body["related"][0]["provenance"]["kind"], "field_equality");
}

#[tokio::test]
async fn entity_history_tiles_snapshots_without_duplicates_and_preserves_null_reasons() {
    let body = walk_entity_history_pages().await;
    assert_eq!(timestamps(&body), vec!["1000", "1500", "2000"]);
    assert_eq!(body[1]["statuses"][0], "unavailable");
    assert_eq!(body[1]["reasons"][0], "producer_gap");
}
```

- [x] **Step 5: Run HTTP RED tests**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  entity_point_returns_lazy_fields_and_only_proven_related_links
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  entity_history_tiles_snapshots_without_duplicates_and_preserves_null_reasons
```

Expected: route is absent.

- [x] **Step 6: Implement point, related provenance and bounded history**

```rust
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct EntityFieldDto {
    code: &'static str,
    value: FrameValue,
    status: &'static str,
    #[schema(required = true)]
    reason: Option<&'static str>,
}
```

Query one selected PGM for point. History enumerates summary snapshot evidence
once, reads at most 32 descriptors sequentially, keeps one decoded segment
outside caches, emits at most 2 000 snapshots/2 MiB and binds cursor to view,
entity, columns, range and last timestamp.

- [x] **Step 7: Run GREEN tests and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_entity
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_frame
git add bins/pg_kronika-web/src/ui/entity.rs \
  bins/pg_kronika-web/src/ui/entity \
  bins/pg_kronika-web/src/ui/frame/projection.rs \
  bins/pg_kronika-web/src/ui/handlers.rs \
  bins/pg_kronika-web/src/ui/mod.rs \
  bins/pg_kronika-web/src/api_error.rs \
  bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/tests
git commit -m "feat(web): add entity detail and history API"
```

### Task 7: Catalog, summary и frame v5 extensions

**Files:**
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/data.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/mod.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/query.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/dto.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_data.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`

**Interfaces:**
- Adds `ViewCapabilities`, `unavailable_reason`, v5 columns and presets.
- Adds `notable_level` and `notable_count` to summary wire DTO.
- Adds custom `columns`, opaque database token, parsed `q`,
  `cell_statuses` and categorical classifications to frame.

- [x] **Step 1: Write failing catalog and summary tests**

```rust
#[test]
fn catalog_contains_every_v5_column_preset_capability_and_reason() {
    let catalog = ProjectionCatalog::for_type_ids(&all_type_ids());
    assert_catalog_column(&catalog, "activity", "backend_type");
    assert_catalog_preset(&catalog, "activity", "replication");
    assert_catalog_column(&catalog, "tables", "xid_age");
    assert_eq!(column(&catalog, "processes", "pss").unavailable_reason, Some("not_collected"));
}

#[tokio::test]
async fn summary_returns_notable_level_and_count() {
    let body = summary_fixture().await;
    assert_eq!(events(&body)["notable_level"], "warning");
    assert_eq!(events(&body)["notable_count"], 2);
}
```

- [x] **Step 2: Run RED catalog/summary tests**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  catalog_contains_every_v5_column_preset_capability_and_reason
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  summary_returns_notable_level_and_count
```

- [x] **Step 3: Extend catalog and summary without scanning extra PGM**

```rust
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub(crate) struct ViewCapabilities {
    detail: bool,
    history: bool,
    related: bool,
}
```

Populate all fields listed in the design gap table. Derive summary level/count
from existing notable evidence in the indexed summary; if the OVF format needs
count/level, revise `UiSummaryBlock` and its build/codec tests before changing
the HTTP DTO.

- [x] **Step 4: Write failing frame parser/DTO tests**

```rust
#[test]
fn frame_accepts_custom_columns_and_canonical_fielded_globs() {
    let request = FrameRequest::parse(
        "activity",
        Some("at=1&columns=pid,state&q=state=active%20query%3D%22select%20*%22"),
        &catalog(),
    ).unwrap();
    assert_eq!(request.columns, vec!["pid", "state"]);
}

#[tokio::test]
async fn frame_null_cells_have_aligned_machine_reasons() {
    let body = frame_fixture_with_missing_pss().await;
    assert_eq!(body["rows"][0]["cells"][0], Value::Null);
    assert_eq!(body["rows"][0]["cell_statuses"][0]["reason"], "not_collected");
}
```

- [x] **Step 5: Run frame RED tests**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  frame_accepts_custom_columns_and_canonical_fielded_globs
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  frame_null_cells_have_aligned_machine_reasons
```

- [x] **Step 6: Implement columns, database token, q AST and statuses**

```rust
pub(crate) struct FrameFilter {
    terms: Vec<FilterTerm>,
    fingerprint: [u8; 32],
}

pub(crate) enum FilterTerm {
    Any(Glob),
    Field { column: &'static str, value: TypedFilter },
}
```

Limit columns to 32 unique codes and filter to 16 terms/256 decoded bytes.
Canonicalize the AST for cursor fingerprints. Filter database-scoped views by
decoded context token, not display name. Emit one `CellStatusDto` per cell and
keep observed zero `available`.

- [x] **Step 7: Run GREEN tests and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_catalog
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_data
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::ui_frame
git add bins/pg_kronika-web/src/ui bins/pg_kronika-web/src/tests
git commit -m "feat(web): complete v5 catalog summary and frame"
```

### Task 8: Incident focus/counters/provenance extensions

**Files:**
- Modify: `bins/pg_kronika-web/src/api_response.rs`
- Modify: `bins/pg_kronika-web/src/incident_response.rs`
- Modify: `bins/pg_kronika-web/src/handlers/incidents.rs`
- Modify: `bins/pg_kronika-web/src/tests/incidents.rs`

**Interfaces:**
- Adds incident `peak_ts_us`, `level`, `category_code`, `summary_code`,
  `finding_count`, `coincident_count`, `relations`.
- Adds finding `confidence_cap`, `slug`.
- Relation kind `proven` always includes stored join provenance.

- [ ] **Step 1: Write failing incident response test**

```rust
#[tokio::test]
async fn incidents_publish_focus_metadata_and_only_proven_relations() {
    let body = incidents_with_statement_plan_join().await;
    let incident = &body["incidents"][0];
    assert_eq!(incident["level"], "warning");
    assert!(incident["peak_ts_us"].is_string());
    assert_eq!(incident["finding_count"], incident["findings"].as_array().unwrap().len());
    assert_eq!(incident["relations"][0]["kind"], "proven");
    assert_eq!(
        incident["relations"][0]["provenance"]["contract"],
        "statement_plan"
    );
}
```

- [ ] **Step 2: Run RED test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  incidents_publish_focus_metadata_and_only_proven_relations
```

- [ ] **Step 3: Extend typed response assembly**

```rust
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentRelationResponse {
    pub(crate) from_finding: usize,
    pub(crate) to_finding: usize,
    pub(crate) kind: String,
    pub(crate) provenance: IncidentRelationProvenanceResponse,
}
```

Derive `peak_ts_us` from bounded members, level from a revisioned deterministic
server policy, counts from emitted findings, and relations only from
`entity_join` evidence. Do not infer severity from confidence or links from
time coincidence.

- [ ] **Step 4: Run GREEN tests and commit**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  tests::incidents
git add bins/pg_kronika-web/src/api_response.rs \
  bins/pg_kronika-web/src/incident_response.rs \
  bins/pg_kronika-web/src/handlers/incidents.rs \
  bins/pg_kronika-web/src/tests/incidents.rs
git commit -m "feat(web): extend incident focus metadata"
```

### Task 9: OpenAPI, docs, smoke, qualification и PR

**Files:**
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Modify: `bins/pg_kronika-web/src/openapi_export.rs`
- Modify: `bins/pg_kronika-web/openapi/openapi.yaml`
- Modify: generated files under `bins/pg_kronika-web/openapi/paths/`
- Modify: generated files under `bins/pg_kronika-web/openapi/schemas/`
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`
- Modify: `.github/workflows/demo-api-smoke.yml`
- Modify: `bins/pg_kronika-web/src/qualification.rs`

**Interfaces:**
- Runtime router, `/openapi.json` and generated multifile tree expose the same
  21 operations.
- Demo smoke calls each new route against a populated stand.

- [ ] **Step 1: Write failing exact-operation OpenAPI tests**

```rust
const OPERATIONS: &[(&str, &str, &str)] = &[
    // Existing 16 entries remain unchanged.
    ("GET", "/v1/ui/context", "context"),
    ("GET", "/v1/timeline/spine", "spine"),
    ("GET", "/v1/data/quality", "data_quality"),
    ("GET", "/v1/entity/{view}/{entity}", "entity"),
    ("GET", "/v1/storage", "storage"),
];

#[test]
fn v5_document_has_twenty_one_source_free_operations() {
    let document = serde_json::to_value(document()).unwrap();
    assert_eq!(operation_count(&document), 21);
    assert!(document["paths"].get("/v1/sources").is_none());
    assert!(!document.to_string().contains("unknown_source"));
}
```

- [ ] **Step 2: Run RED OpenAPI test**

```sh
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin \
  v5_document_has_twenty_one_source_free_operations
```

Expected: count/schema/tag/response assertions identify any remaining route
registration or DTO gaps.

- [ ] **Step 3: Complete utoipa registration and regenerate**

```sh
CARGO_BUILD_TARGET=aarch64-apple-darwin make openapi
CARGO_BUILD_TARGET=aarch64-apple-darwin make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
```

The first run updates the committed tree; after staging that output, a repeated
generation must be deterministic.

- [ ] **Step 4: Extend smoke and structural qualification**

```yaml
- name: Smoke v5 UI API
  run: |
    curl --fail "$BASE/v1/ui/context?at=$AT"
    curl --fail "$BASE/v1/timeline/spine?from=$FROM&to=$TO&buckets=96"
    curl --fail "$BASE/v1/data/quality?from=$FROM&to=$TO"
    curl --fail "$BASE/v1/storage"
```

Entity smoke first takes one token from frame and calls both point and bounded
history. Qualification asserts spine 0 raw PGM at N=96/N=1440, entity history
at 32 PGM/2 MiB, data quality at 512 KiB and storage at layout bounds.

- [ ] **Step 5: Run full verification**

```sh
cargo fmt --all -- --check
cargo test --workspace --target aarch64-apple-darwin
cargo clippy --workspace --all-targets --all-features \
  --target aarch64-apple-darwin -- -D warnings
cargo run -p xtask --target aarch64-apple-darwin -- check-deps
CARGO_BUILD_TARGET=aarch64-apple-darwin make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
git diff --check
```

Expected: every command exits 0.

- [ ] **Step 6: Update docs and commit generated contract**

```sh
git add bins/pg_kronika-web/openapi \
  bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/openapi_export.rs \
  bins/pg_kronika-web/src/qualification.rs \
  bins/pg_kronika-web/README.md \
  bins/pg_kronika-web/README.ru.md \
  .github/workflows/demo-api-smoke.yml
git commit -m "docs(web): publish complete v5 OpenAPI"
```

- [ ] **Step 7: Push and create the review PR**

Before creation, read the repository PR template and use its sections. Then:

```sh
git push -u origin feat/web-ui-v5-api
```

Create a PR to `main` titled `feat(web): complete proposal v5 API` with:

```markdown
## Summary
- add the five missing read-only Web UI v5 endpoints
- complete catalog, summary, frame, and incident response contracts
- publish the 21-operation source-free Swagger/OpenAPI tree

## Verification
- `cargo test --workspace --target aarch64-apple-darwin`
- `cargo clippy --workspace --all-targets --all-features --target aarch64-apple-darwin -- -D warnings`
- `cargo run -p xtask --target aarch64-apple-darwin -- check-deps`
- deterministic `make openapi`
```
