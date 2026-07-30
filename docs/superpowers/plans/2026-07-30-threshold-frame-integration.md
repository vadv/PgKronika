# Threshold Frame Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the first bounded HTTP consumer of the 69-policy Class 1 catalog through `GET /v1/frame/{view}`, with 14 proven per-cell numeric bindings and exact serialized classifications.

**Architecture:** Keep threshold policy and comparison semantics in `kronika-analytics`. Add an exhaustive web-owned binding manifest, locate exact snapshots through `UiSummary`, project at most two PGM files into bounded frame rows, prepare typed `MetricInput` values, and serialize `Classified` through Rust DTOs and generated OpenAPI.

**Tech Stack:** Rust 2024, Axum, `kronika-reader`, `kronika-analytics`, Utoipa, existing OVF/PGM fixtures, generated multi-file OpenAPI, no new dependencies.

## Global Constraints

- The endpoint serves all nine existing `WebView` projections.
- Exactly 14 `MetricId` values are bound to frame columns; the other 55 have an explicit deferred reason.
- A frame reads at most one current PGM and one proven predecessor PGM.
- Spark reads only `UiSummary` and `EntitySeries` for the selected view and spans at most 24 hours.
- `limit` defaults to 100 and is restricted to `1..=200`.
- Serialized JSON is limited to 1 MiB and is truncated only at a row boundary.
- Raw query strings remain limited to 8192 bytes; `q` is limited to 256 decoded UTF-8 bytes and cursor to 512 bytes.
- Missing, gated, reset, gap, invalid and inapplicable inputs never become numeric zero.
- The server returns `Classified`; clients do not repeat thresholds or comparison operators.
- No categorical classifier, production frontend, collector field, relation reloptions, context/detail/storage endpoint, cache or Class 2 change is part of this plan.
- Use native `--target aarch64-apple-darwin` for local execution on macOS when the repository default Linux-musl target cannot run.

---

### Task 1: Exact Snapshot Neighbors

**Files:**
- Modify: `crates/kronika-reader/src/overview/web_index/summary.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Modify: `crates/kronika-reader/src/overview/mod.rs`
- Modify: `crates/kronika-reader/src/lib.rs`

**Interfaces:**
- Consumes: existing sorted `UiSummaryBlock::snapshot_times` and per-view presence mask.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotNeighbors {
    pub previous: Option<i64>,
    pub current: i64,
    pub next: Option<i64>,
}

impl UiSummaryBlock {
    pub fn snapshot_neighbors(
        &self,
        view_code: u16,
        at_us: i64,
    ) -> Option<SnapshotNeighbors>;
}
```

- [ ] **Step 1: Write failing unit tests**

Add tests beside the existing `snapshot_at` tests:

```rust
#[test]
fn neighbors_skip_timestamps_where_the_view_is_absent() {
    let block = summary_with_presence(
        &[10, 20, 30, 40],
        1,
        &[true, false, true, true],
    );

    assert_eq!(
        block.snapshot_neighbors(1, 35),
        Some(SnapshotNeighbors {
            previous: Some(10),
            current: 30,
            next: Some(40),
        })
    );
    assert_eq!(block.snapshot_neighbors(1, 9), None);
}
```

Also assert:

- `at_us` equal to the first present timestamp selects it;
- no previous/next yields `None` only for that field;
- an unknown `view_code` returns `None`;
- a view with no present snapshots returns `None`.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p kronika-reader \
  overview::web_index::summary::tests::neighbors
```

Expected: compilation fails because `SnapshotNeighbors` and
`snapshot_neighbors` do not exist.

- [ ] **Step 3: Implement binary-search lookup**

Reuse the existing per-view presence API. Select the greatest present
timestamp `<= at_us`, then scan presence bits backward and forward inside the
same bounded `snapshot_times` slice. Do not allocate a filtered timestamp
vector.

- [ ] **Step 4: Re-export and run focused tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p kronika-reader \
  overview::web_index::summary
```

Expected: all summary tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-reader/src/overview/web_index/summary.rs \
  crates/kronika-reader/src/overview/web_index/mod.rs \
  crates/kronika-reader/src/overview/mod.rs \
  crates/kronika-reader/src/lib.rs
git commit -m "feat(reader): expose exact UI snapshot neighbors"
```

---

### Task 2: Exhaustive Threshold Binding Manifest

**Files:**
- Modify: `crates/kronika-analytics/src/web_projection.rs`
- Modify: `crates/kronika-analytics/tests/web_projection.rs`
- Create: `bins/pg_kronika-web/src/ui/thresholds.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`

**Interfaces:**
- Consumes: `kronika_analytics::MetricId::ALL`, `MetricId::as_str()`, existing
  `ProjectionCatalog` views and columns.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperandKind {
    ActivityQueryDuration,
    ActivityTransactionDuration,
    StatementMillisecondsPerRow,
    StatementMeanMilliseconds,
    StatementTimePercent,
    StatementPlanTimePercent,
    TableDeadTupleRatio,
    TableDeadTuples,
    TableSequentialScanPercent,
    TableModifiedSinceAnalyze,
    TableInsertedSinceVacuum,
    TableAutovacuumAge,
    TableAutoanalyzeAge,
    ProcessRssKib,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredBindingReason {
    AggregateNotCell,
    MissingView,
    MissingCollectedOperand,
    IncompatibleUnit,
    NoStableCellMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingDisposition {
    Bound {
        view: &'static str,
        column: &'static str,
        operand: OperandKind,
    },
    Deferred(DeferredBindingReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThresholdProjection {
    pub metric_id: MetricId,
    pub disposition: BindingDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThresholdBinding {
    pub metric_id: MetricId,
    pub view: &'static str,
    pub column: &'static str,
    pub operand: OperandKind,
}

pub(crate) const fn threshold_projections() -> &'static [ThresholdProjection; 69];
pub(crate) fn binding_for(view: &str, column: &str) -> Option<ThresholdBinding>;
```

- [ ] **Step 1: Add failing exhaustive-contract tests**

Assert:

```rust
let manifest = threshold_projections();
assert_eq!(manifest.len(), MetricId::ALL.len());
assert_eq!(
    manifest.iter().map(|entry| entry.metric_id).collect::<Vec<_>>(),
    MetricId::ALL,
);
assert_eq!(
    manifest
        .iter()
        .filter(|entry| matches!(entry.disposition, BindingDisposition::Bound { .. }))
        .count(),
    14,
);
```

For every `Bound`, look up the view and column in `ProjectionCatalog` and fail
with the metric string when either is missing. Assert `(view, column)` pairs are
unique.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_catalog::threshold_manifest
```

Expected: compilation fails because `ui::thresholds` does not exist.

- [ ] **Step 3: Add the 69-entry manifest**

Bind exactly these pairs:

```text
activity.query_duration_us       -> PgActivityQueryDurationSeconds
activity.transaction_duration_us -> PgActivityTransactionDurationSeconds
statements.ms_per_row            -> PgStatementsMillisecondsPerRow
statements.mean                  -> PgStatementsMeanTimeMilliseconds
statements.time_pct              -> PgStatementsTimePercent
statements.plan_time_pct         -> PgStatementsPlanTimePercent
tables.dead_pct                  -> PgTablesDeadTuplePercent
tables.dead_tuples               -> PgTablesDeadTuples
tables.seq_scan_pct              -> PgTablesSequentialScanPercent
tables.modified_since_analyze    -> PgTablesModifiedSinceAnalyze
tables.inserted_since_vacuum     -> PgTablesInsertedSinceVacuum
tables.autovacuum_age_seconds    -> PgTablesAutovacuumAgeSeconds
tables.autoanalyze_age_seconds   -> PgTablesAutoanalyzeAgeSeconds
processes.rss                    -> OsProcessRssKib
```

List all other `MetricId` values explicitly with one
`DeferredBindingReason`. Do not generate a blanket default arm.

- [ ] **Step 4: Extend column metadata**

Add to `ColumnSpec`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub unit: Option<&'static str>,
#[serde(skip_serializing_if = "Option::is_none")]
pub threshold_metric: Option<&'static str>,
```

Change `raw_column`, `derived_column` and `unavailable_column` to accept the
unit explicitly. Use `None` for labels/text and these exact units for bound
columns:

```text
query_duration_us, transaction_duration_us -> us
ms_per_row, mean                           -> ms
time_pct, plan_time_pct, dead_pct,
seq_scan_pct                               -> percent
dead_tuples, modified_since_analyze,
inserted_since_vacuum                      -> count
autovacuum_age_seconds,
autoanalyze_age_seconds                    -> seconds
rss                                        -> kib
```

Populate `threshold_metric` after constructing every view by calling
`binding_for(view.code, column.code)`.

- [ ] **Step 5: Add missing bound columns and correct statement units**

Add the following `ColumnSpec` entries and include them in the named presets
that sort by their domain:

```text
statements.ms_per_row
statements.time_pct
statements.plan_time_pct
tables.dead_tuples
tables.seq_scan_pct
tables.modified_since_analyze
tables.inserted_since_vacuum
tables.autovacuum_age_seconds
tables.autoanalyze_age_seconds
```

Extend `STATEMENTS_INPUTS` with optional `settings -> pg_settings`.
`statements.plan_time_pct` requires both `statements` and `settings`; the
canonical statement metric continues to require only `statements`, so missing
settings gates this column without gating the whole view.

`pg_stat_statements.total_exec_time` is milliseconds. Change the shared
statement `WebMetric` unit from `Microseconds` to a new
`WebUnit::Milliseconds`, update its public spelling to `ms`, and bump the
affected statement metric revision and statement view revision from 1 to 2.
Update the golden web projection test.

- [ ] **Step 6: Run catalog tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p kronika-analytics \
  --test web_projection
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_catalog
```

Expected: all tests pass and serialized catalog columns expose only the 14
approved threshold strings.

- [ ] **Step 7: Commit**

```bash
git add crates/kronika-analytics/src/web_projection.rs \
  crates/kronika-analytics/tests/web_projection.rs \
  bins/pg_kronika-web/src/ui/thresholds.rs \
  bins/pg_kronika-web/src/ui/mod.rs \
  bins/pg_kronika-web/src/ui/catalog.rs \
  bins/pg_kronika-web/src/tests/ui_catalog.rs
git commit -m "feat(web): bind numeric thresholds to frame columns"
```

---

### Task 3: Frame Request, DTO and Cursor Contracts

**Files:**
- Create: `bins/pg_kronika-web/src/ui/frame/mod.rs`
- Create: `bins/pg_kronika-web/src/ui/frame/dto.rs`
- Create: `bins/pg_kronika-web/src/ui/frame/cursor.rs`
- Modify: `bins/pg_kronika-web/src/ui/mod.rs`
- Modify: `bins/pg_kronika-web/src/api_error.rs`
- Modify: `bins/pg_kronika-web/src/params.rs`
- Create: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `bins/pg_kronika-web/src/tests/mod.rs`

**Interfaces:**
- Consumes: projection catalog view/preset/column metadata.
- Produces:

```rust
pub(crate) const DEFAULT_FRAME_LIMIT: usize = 100;
pub(crate) const MAX_FRAME_LIMIT: usize = 200;
pub(crate) const DEFAULT_SPAN_US: i64 = 3_600_000_000;
pub(crate) const MAX_SPAN_US: i64 = 86_400_000_000;
pub(crate) const MAX_FILTER_BYTES: usize = 256;
pub(crate) const MAX_FRAME_CURSOR_BYTES: usize = 512;
pub(crate) const MAX_FRAME_RESPONSE_BYTES: usize = 1_048_576;

pub(crate) struct FrameRequest {
    pub view: &'static WebView,
    pub at_us: i64,
    pub span_us: i64,
    pub preset: &'static str,
    pub database: Option<String>,
    pub filter: Option<String>,
    pub sort: &'static str,
    pub descending: bool,
    pub limit: usize,
    pub cursor: Option<FrameCursor>,
}

impl FrameRequest {
    pub(crate) fn parse(
        view_name: &str,
        raw_query: Option<&str>,
        catalog: &ProjectionCatalog,
    ) -> Result<Self, ApiError>;
}
```

The DTO layer produces named Utoipa schemas for:

```text
FrameResponse
FrameColumnDto
FrameRowDto
FramePageDto
FrameQualityDto
CellClassificationDto
ClassifiedResultDto
NotClassifiedResultDto
BoundaryDto
EvidenceDto
SparkDto
```

- [ ] **Step 1: Write failing parameter-contract tests**

Cover:

```text
at is required
span defaults to 1h and rejects >24h
preset defaults to the first preset
sort/order default from the selected preset
limit defaults to 100 and rejects 0 or >200
q rejects decoded values over 256 bytes
cursor rejects encoded values over 512 bytes
unknown preset/sort/order fail before storage access
source is an unknown parameter
```

Test the pure parser:

```rust
FrameRequest::parse("activity", raw_query, &ProjectionCatalog::for_type_ids(&observed))
```

No storage fixture is involved, which proves parameter validation does not
depend on reader state.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame::frame_query
```

Expected: compilation fails because the frame module and route do not exist.

- [ ] **Step 3: Add query parameter variants**

Extend `QueryParameter` with:

```rust
Span => "span",
Preset => "preset",
Database => "database",
Q => "q",
Sort => "sort",
Order => "order",
```

Add `ExpectedValue::SortOrder`. Parse `limit` with a frame-specific strict
range check; do not reuse the global parser that clamps to 10,000.

- [ ] **Step 4: Implement the opaque cursor**

Use a versioned bounded binary payload and URL-safe base64 without padding:

```rust
pub(crate) struct FrameCursor {
    version: u8,
    view_code: u16,
    view_revision: u16,
    snapshot_ts_us: i64,
    query_fingerprint: [u8; 32],
    sort_key: SortKey,
    entity: Vec<u8>,
}
```

`SortKey` must encode null, signed, unsigned, finite float, boolean, timestamp
and text-prefix variants. Reject non-finite floats, entity values above the
existing identity bound, unknown versions, trailing bytes and payloads above
512 bytes. Fingerprint the normalized view, revision, at, span, preset,
database, q, sort and order with existing `sha2`.

- [ ] **Step 5: Implement exact wire conversion**

Convert analytics values without losing reason or evidence:

```rust
impl From<Classified> for ClassificationResultDto
impl From<Boundary> for BoundaryDto
impl From<Evidence> for EvidenceDto
```

Use these stable spellings:

```text
levels: inactive, ok, warning, critical
operators: above, at_least, below, at_most
statuses: classified, not_classified
reasons: missing, non_finite, out_of_domain, invalid_denominator,
         not_applicable, input_shape_mismatch
evidence kinds: scalar, fraction, limit, ratio_with_floor, age, free_capacity
```

- [ ] **Step 6: Test DTO serialization and cursor round trips**

Golden-test every `Evidence` variant, all `NotClassifiedReason` variants, an
inactive verdict without boundary and warning/critical verdicts with boundary.
Property-test cursor round trips within bounds and rejection of arbitrary byte
strings.

- [ ] **Step 7: Commit**

```bash
git add bins/pg_kronika-web/src/ui/frame \
  bins/pg_kronika-web/src/ui/mod.rs \
  bins/pg_kronika-web/src/api_error.rs \
  bins/pg_kronika-web/src/params.rs \
  bins/pg_kronika-web/src/tests/ui_frame.rs \
  bins/pg_kronika-web/src/tests/mod.rs
git commit -m "feat(web): define bounded frame contracts"
```

---

### Task 4: Exact Nine-View Projection

**Files:**
- Create: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Create: `bins/pg_kronika-web/src/ui/frame/query.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/mod.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `bins/pg_kronika-web/src/test_layout.rs`

**Interfaces:**
- Consumes: `SnapshotNeighbors`, `LocalDirSnapshot`, `LogicalSection`,
  `SectionPage`, `Value`, registry contracts and `ProjectionCatalog`.
- Produces:

```rust
pub(crate) struct ProjectedRow {
    pub entity: Vec<u8>,
    pub label: String,
    pub cells: Vec<FrameValue>,
    pub operands: RowOperands,
}

pub(crate) struct ProjectedFrame {
    pub snapshot_ts_us: i64,
    pub predecessor_ts_us: Option<i64>,
    pub neighbors: SnapshotNeighbors,
    pub rows: Vec<ProjectedRow>,
    pub quality: FrameQuality,
}

pub(crate) fn project_frame(
    snapshot: &mut LocalDirSnapshot,
    request: &FrameRequest,
    limits: FrameLimits,
) -> Result<ProjectedFrame, FrameError>;
```

- [ ] **Step 1: Add one RED fixture per view**

Build registry-backed PGM fixtures for:

```text
activity, statements, plans, tables, indexes,
vacuum, processes, locks, events
```

For each fixture, request the default preset and assert exact entity, label and
cell order from `ProjectionCatalog`. Include nullable and lazy fields.

- [ ] **Step 2: Prove predecessor behavior before implementation**

Create two-segment statement/table/process fixtures and assert:

- current gauges come from the last snapshot `<= at`;
- cumulative values use only the exact previous identity;
- a reset returns null derived value and a reset marker in `RowOperands`;
- a coverage gap returns null;
- an intermediate PGM without the view is not opened;
- instrumentation records one current and at most one predecessor PGM.

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame::frame_projection
```

Expected: compilation fails because `project_frame` does not exist.

- [ ] **Step 3: Implement typed value access**

Add local helpers that read `OutRow` by registry column name and return:

```rust
fn finite_number(row: &OutRow, name: &str) -> Result<Option<f64>, ProjectionError>;
fn timestamp(row: &OutRow, name: &str) -> Result<Option<i64>, ProjectionError>;
fn text(row: &OutRow, name: &str) -> Result<Option<&str>, ProjectionError>;
```

Do not coerce `Null`, text or non-finite float to zero. Preserve integers above
JavaScript's exact range as string-valued `FrameValue`.

- [ ] **Step 4: Implement current/predecessor reads**

Resolve `SnapshotNeighbors` from the existing OVF request view. Query only
logical sections required by the selected `WebView`, at `current` and optional
`previous`. Use a shared request-wide `QueryLimits` budget. Group predecessor
rows by the declared registry identity; never join only on PID.

For activity/process join, require:

```text
activity.pid == process.pid
activity.backend_start == process.starttime
```

An unmatched process row gates only process-derived columns.

- [ ] **Step 5: Implement the nine explicit evaluators**

Use one function per view:

```rust
fn project_activity(...)
fn project_statements(...)
fn project_plans(...)
fn project_tables(...)
fn project_indexes(...)
fn project_vacuum(...)
fn project_processes(...)
fn project_locks(...)
fn project_events(...)
```

Each evaluator must have a golden test matching every public formula in
`ProjectionCatalog`. Heavy `lazy=true` columns are omitted from `cells` even
when source data is present.

For statement `time_pct`, apply the optional database filter before computing
the denominator, then compute snapshot totals before `q`, sort or pagination.
For other statement/table percentages, compute operands before `q`, sort or
pagination. `total_exec_time` and `total_plan_time` stay in milliseconds.

- [ ] **Step 6: Implement filter, sort and pagination**

Apply in this order:

```text
exact snapshot projection
snapshot-wide denominators
database filter
q filter
stable sort by (selected cell, entity)
cursor seek
row-boundary response budget
limit
```

`matched` counts rows after filters and before cursor/limit. Null sorts last in
both directions. Text comparison uses Unicode scalar order on the stored
lossy-UTF-8 value already returned by reader. Generate `next` from the last
returned row only when another matched row exists.

- [ ] **Step 7: Run projection tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame::frame_projection
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame::frame_pagination
```

Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add bins/pg_kronika-web/src/ui/frame/projection.rs \
  bins/pg_kronika-web/src/ui/frame/query.rs \
  bins/pg_kronika-web/src/ui/frame/mod.rs \
  bins/pg_kronika-web/src/tests/ui_frame.rs \
  bins/pg_kronika-web/src/test_layout.rs
git commit -m "feat(web): project exact bounded UI frames"
```

---

### Task 5: Fourteen Typed Threshold Adapters

**Files:**
- Create: `bins/pg_kronika-web/src/ui/frame/threshold.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/mod.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`

**Interfaces:**
- Consumes: `ThresholdBinding`, `ProjectedRow`, snapshot-wide denominators,
  predecessor proof and `kronika_analytics::classify`.
- Produces:

```rust
pub(crate) struct CellClassification {
    pub column: &'static str,
    pub metric_id: MetricId,
    pub result: Classified,
}

pub(crate) fn classify_row(
    view: &str,
    columns: &[ColumnSpec],
    row: &ProjectedRow,
    context: &FrameThresholdContext,
) -> Vec<CellClassification>;
```

- [ ] **Step 1: Add a RED table test for all 14 operands**

For every `OperandKind`, test a representative classified input and every
adapter-specific non-classified path. The test must compare the complete
`MetricInput` before calling analytics:

```rust
assert_eq!(
    prepare_input(
        OperandKind::StatementMeanMilliseconds,
        &row,
        &context,
    ),
    MetricInput::Scalar(28.4),
);
```

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame::threshold_inputs
```

Expected: compilation fails because `prepare_input` does not exist.

- [ ] **Step 2: Implement activity inputs**

Use:

```rust
ActivityQueryDuration =>
    state == "active" && query_start.is_some()
        ? Scalar((snapshot_ts_us - query_start_us) as f64 / 1_000_000.0)
        : NotApplicable

ActivityTransactionDuration =>
    xact_start.is_some()
        ? Scalar((snapshot_ts_us - xact_start_us) as f64 / 1_000_000.0)
        : NotApplicable
```

Do not clamp future timestamps. Passing a negative scalar must preserve
analytics `OutOfDomain`.

- [ ] **Step 3: Implement statement inputs**

Use reset-aware non-negative deltas:

```text
ms_per_row = exec_ms_delta / rows_delta, rows_delta > 0
mean_ms = exec_ms_delta / calls_delta, calls_delta > 0
time_pct = 100 * exec_ms_delta / all_statement_exec_ms_delta, denominator > 0
plan_time_pct = 100 * plan_ms_delta / (plan_ms_delta + exec_ms_delta)
```

`plan_time_pct` is `NotApplicable` when planning fields are absent or the
same-PGM `pg_settings` value `pg_stat_statements.track_planning` is not `on`.
Zero plan time with proven tracking is a valid `Scalar(0.0)`.

- [ ] **Step 4: Implement table inputs**

Use:

```rust
MetricInput::RatioWithFloor {
    ratio: dead / (live + dead),
    count: dead,
}
MetricInput::Scalar(dead)
MetricInput::Scalar(100.0 * seq_delta / (seq_delta + idx_delta))
MetricInput::Scalar(n_mod_since_analyze)
MetricInput::Scalar(n_ins_since_vacuum)
MetricInput::Age {
    epoch_seconds: last_autovacuum_us / 1e6,
    now_seconds: snapshot_ts_us / 1e6,
    gate: n_dead_tup > 0,
}
MetricInput::Age {
    epoch_seconds: last_autoanalyze_us / 1e6,
    now_seconds: snapshot_ts_us / 1e6,
    gate: n_mod_since_analyze >= 10_000,
}
```

A missing timestamp uses `MetricInput::Missing`; a pre-PG13 missing
`n_ins_since_vacuum` uses `MetricInput::NotApplicable`. A zero scan denominator
uses `NotApplicable`.

- [ ] **Step 5: Implement process RSS input**

Map a finite non-negative `rmem_kb` directly to `MetricInput::Scalar`.
Do not bind `processes.cpu` or `processes.block_delay`: their current scheduler
tick/rate units do not match the catalog policies.

- [ ] **Step 6: Classify and attach results**

Iterate only the response preset columns. For every bound column, emit exactly
one `CellClassification`, even when the result is `NotClassified`. Call:

```rust
let result = kronika_analytics::threshold::classify(binding.metric_id, input);
```

Never reproduce warning/critical numbers in web code.

- [ ] **Step 7: Verify boundary and JSON behavior**

Add integration cases for:

```text
active query 0.5s / 1s / 30s
statement mean 9.999ms / 10ms / 100ms
dead ratio below/at floor and warning/critical percentages
future maintenance timestamp -> out_of_domain
missing predecessor -> missing
counter reset -> missing
RSS at 100 MiB and 1 GiB boundaries
```

Assert exact `metric`, `level`, `boundary.operator`, `boundary.value` and
`evidence` JSON.

- [ ] **Step 8: Commit**

```bash
git add bins/pg_kronika-web/src/ui/frame/threshold.rs \
  bins/pg_kronika-web/src/ui/frame/projection.rs \
  bins/pg_kronika-web/src/ui/frame/mod.rs \
  bins/pg_kronika-web/src/tests/ui_frame.rs
git commit -m "feat(web): classify numeric frame cells"
```

---

### Task 6: OVF Sparks and Frame Quality

**Files:**
- Create: `bins/pg_kronika-web/src/ui/frame/spark.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/mod.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/dto.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`

**Interfaces:**
- Consumes: selected view's canonical `WebMetric`, `EntitySeriesBlock`,
  `LiveView`, requested `span`.
- Produces one `SparkDto` per returned entity and a merged `FrameQualityDto`.

- [ ] **Step 1: Add failing spark tests**

Assert:

- spark reads zero PGM after frame projection;
- only the selected view's `EntitySeries` blocks are opened;
- a top-K miss yields null points and `complete=false`;
- missing OVF, gaps and active tail are reflected in quality;
- `span=24h` succeeds and `span>24h` was rejected earlier.

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame::frame_spark
```

Expected: frame rows have no spark values.

- [ ] **Step 3: Reuse heatmap merge semantics**

Extract the entity lookup and grid merge helpers from `ui/heatmap.rs` only
when sharing them removes duplication without changing heatmap JSON. Preserve
the selected canonical metric's unit, missing buckets and `MetricStatus`.

- [ ] **Step 4: Merge quality without hiding degradation**

Combine:

```text
UiSummary snapshot status
PGM coverage gaps
gated/unavailable columns
predecessor reset/gap reasons
EntitySeries completeness
active tail
reader resource limits
```

Do not convert a partial spark into an HTTP failure.

- [ ] **Step 5: Run heatmap and frame tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_data
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame
```

Expected: existing summary/heatmap behavior remains unchanged and all frame
tests pass.

- [ ] **Step 6: Commit**

```bash
git add bins/pg_kronika-web/src/ui/frame/spark.rs \
  bins/pg_kronika-web/src/ui/frame/mod.rs \
  bins/pg_kronika-web/src/ui/frame/dto.rs \
  bins/pg_kronika-web/src/ui/heatmap.rs \
  bins/pg_kronika-web/src/tests/ui_frame.rs
git commit -m "feat(web): add bounded frame sparks and quality"
```

---

### Task 7: HTTP Route and Generated OpenAPI

**Files:**
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/api_docs.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: generated `bins/pg_kronika-web/openapi/openapi.yaml`
- Modify: generated `bins/pg_kronika-web/openapi/paths/ui.yaml`
- Modify: generated `bins/pg_kronika-web/openapi/schemas/ui.yaml`
- Modify: generated schemas in `bins/pg_kronika-web/openapi/schemas/common.yaml` only if DTO ownership requires it

**Interfaces:**
- Consumes: `FrameRequest`, `build_frame`, DTOs and existing `AppState`.
- Produces:

```rust
#[utoipa::path(
    get,
    path = "/v1/frame/{view}",
    tag = "ui",
    params(
        ("view" = String, Path),
        ("at" = i64, Query),
        ("span" = Option<String>, Query),
        ("preset" = Option<String>, Query),
        ("database" = Option<String>, Query),
        ("q" = Option<String>, Query),
        ("sort" = Option<String>, Query),
        ("order" = Option<String>, Query),
        ("limit" = Option<usize>, Query),
        ("cursor" = Option<String>, Query),
    ),
    responses(
        (status = 200, body = FrameResponse),
        (status = 400, body = ApiError),
        (status = 401, body = ApiError),
        (status = 410, body = ApiError),
        (status = 413, body = ApiError),
        (status = 500, body = ApiError),
    )
)]
pub(crate) async fn frame(...) -> Result<Response<Body>, ApiError>;
```

- [ ] **Step 1: Add route/OpenAPI RED tests**

Extend `api_docs::tests::OPERATIONS` with:

```rust
("GET", "/v1/frame/{view}", "frame")
```

Assert the route has only the six explicit response statuses above and tag
`ui`. Assert the generated success schema references `FrameResponse`.

- [ ] **Step 2: Implement handler orchestration**

Parse and validate outside `spawn_blocking`. Inside `spawn_blocking`, clone the
request snapshot and live descriptor view, call `build_frame`, serialize to a
bounded buffer, and map:

```text
bad view/preset/sort/order/cursor -> 400
expired exact snapshot cursor     -> 410
row/response/read work limit      -> 413
store corruption/read error       -> 500
worker panic/cancel                -> 500
```

Log only stable event names, limits and counts; do not log query text, filter
contents, entity tokens or filesystem paths.

- [ ] **Step 3: Register the route**

Add `.routes(routes!(crate::ui::handlers::frame))` to `api_docs::configured`.
Do not add a second manual Axum route.

- [ ] **Step 4: Generate and round-trip OpenAPI**

Run:

```bash
make openapi
make openapi-bundle
git diff --check
```

Run `make openapi` again and verify it leaves no new diff.

- [ ] **Step 5: Run HTTP integration tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  tests::ui_frame
cargo test --target aarch64-apple-darwin -p pg_kronika-web \
  api_docs::tests
```

Expected: route, schemas, parameter errors, auth and response limits pass.

- [ ] **Step 6: Commit**

```bash
git add bins/pg_kronika-web/src/ui/handlers.rs \
  bins/pg_kronika-web/src/api_docs.rs \
  bins/pg_kronika-web/src/tests/ui_frame.rs \
  bins/pg_kronika-web/openapi
git commit -m "feat(web): expose classified UI frames"
```

---

### Task 8: Qualification, Documentation and Full Verification

**Files:**
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`
- Modify: `crates/kronika-analytics/README.md`
- Modify: `crates/kronika-analytics/README.ru.md`
- Modify: `docs/superpowers/specs/2026-07-28-web-ui-api-design.md`
- Modify: `docs/superpowers/specs/2026-07-29-absolute-threshold-catalog-design.md`
- Modify: `docs/superpowers/specs/2026-07-30-threshold-frame-integration-design.md`
- Modify: `bins/pg_kronika-web/src/qualification.rs`
- Modify: `bins/pg_kronika-web/src/test_layout.rs`

**Interfaces:**
- Consumes: Tasks 1-7.
- Produces: synchronized operator documentation, measured bounds and a verified
  implementation branch.

- [ ] **Step 1: Add the structural read-budget qualification**

Instrument a fixture with:

```text
96 normal segments
1,440 early-sealed segments
one view absent from intermediate segments
one current snapshot
one predecessor in an older segment
more than 200 matching rows
EntitySeries top-K misses
```

Assert frame opens at most two PGM files regardless of segment count, reads
only the selected view's OVF series, and never exceeds 1 MiB serialized JSON.

- [ ] **Step 2: Update English and Russian contracts**

Document:

- `/v1/frame/{view}` parameters and limits;
- exact distinction between no binding and `not_classified`;
- 14 bound policies and deferred manifest;
- no production frontend;
- no config-bound autovacuum classification until relation reloptions are
  durably collected.

Keep English and Russian facts synchronized.

- [ ] **Step 3: Run focused gates**

```bash
cargo fmt --all --check
cargo test --target aarch64-apple-darwin -p kronika-reader
cargo test --target aarch64-apple-darwin -p kronika-analytics
cargo test --target aarch64-apple-darwin -p pg_kronika-web
cargo clippy --target aarch64-apple-darwin \
  -p kronika-reader -p kronika-analytics -p pg_kronika-web \
  --all-targets -- -D warnings
```

- [ ] **Step 4: Run workspace gates**

```bash
cargo fmt --all --check
cargo clippy --target aarch64-apple-darwin \
  --workspace --all-targets --all-features -- -D warnings
cargo test --target aarch64-apple-darwin --workspace
cargo run --target aarch64-apple-darwin -p xtask -- check-deps
make openapi
git diff --check
```

Record any pre-existing platform failure verbatim and prove it reproduces
outside this diff before excluding it.

- [ ] **Step 5: Review memory and information exposure**

Verify:

- all request-controlled collections have documented hard caps;
- response truncation occurs only at row boundaries;
- no query text, filter content, entity token or path is logged;
- lazy query/plan/message fields are absent from frame;
- classification remains O(1) per bound cell;
- web code contains no copied warning/critical boundary numbers.

- [ ] **Step 6: Commit documentation and qualification**

```bash
git add bins/pg_kronika-web/README.md \
  bins/pg_kronika-web/README.ru.md \
  crates/kronika-analytics/README.md \
  crates/kronika-analytics/README.ru.md \
  docs/superpowers/specs \
  bins/pg_kronika-web/src/qualification.rs \
  bins/pg_kronika-web/src/test_layout.rs
git commit -m "docs(web): document classified frame contract"
```

- [ ] **Step 7: Create the separate PR**

Start from updated `main`, use a new branch such as:

```bash
git switch main
git pull --ff-only
git switch -c feat/threshold-frame-integration
```

Push only after all gates pass:

```bash
git push -u origin feat/threshold-frame-integration
```

The PR body must link
`docs/superpowers/specs/2026-07-30-threshold-frame-integration-design.md`,
state `14 bound / 55 deferred`, list exact resource budgets and say explicitly
that categorical rules and production frontend are out of scope.
