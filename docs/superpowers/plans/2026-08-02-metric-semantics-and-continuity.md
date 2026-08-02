# Forensic UI PR 2: Metric Semantics and Counter Continuity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every PR 2 metric honest about counter continuity, units, PostgreSQL provenance, and version-specific meanings before the visual shell consumes the catalog.

**Architecture:** Keep stable metric identity and normative formulas in `kronika-analytics`, while the web frame evaluator applies the same continuity verdict to every row delta. Exact extension-reset markers and instance metadata are read as bounded auxiliary inputs; unavailable denominators or conversion metadata produce `null`, never fabricated zeroes or clamped values. Public catalog codes are renamed where the old name claimed evidence the collector does not have.

**Tech Stack:** Rust workspace (`kronika-analytics`, `pg_kronika-web`, `kronika-registry`), Axum JSON DTOs, React/TypeScript localization catalog, repository OpenAPI/demo generators.

## Global Constraints

- This is stacked PR 2 and must remain based on `codex/pr01-semantic-contracts`; do not absorb shell or screen implementation from later roadmap PRs.
- Preserve numeric view codes and metric codes. Bump only affected view/metric revisions and bump the public catalog revision from `3` to `4`.
- Counter deltas are valid only with a predecessor inside `max_rate_gap_us`, no recorded gap, and a non-reset counter family; formulas that divide additionally require every denominator to be finite and non-zero.
- `pg_stat_statements` and `pg_store_plans` use their exact `reset_metadata` markers; do not apply the coarse database reset marker to table or index rows.
- Linux process CPU and block-delay counters are ticks. Divide their positive delta by `clock_ticks_per_sec * elapsed_seconds`; missing or non-positive HZ yields `null` and multi-core values may exceed `1.0`.
- PostgreSQL `*_blks_read` means reads into PostgreSQL buffers and does not prove physical disk I/O because the operating-system page cache may satisfy the read.
- A plan timestamp sourced from `first_call`/`last_call` must use those public names. Lock age is provable only from `waitstart`; granted locks without `waitstart` have `null` age.
- Do not coalesce PG10–16 vacuum dead-tuple count with PG17+ dead-item-ID count or byte capacity. Zero `heap_blks_total` yields `null` progress.
- Every code change starts with a failing focused test, then the smallest passing implementation, then focused tests and a task commit.
- Review every diff for bounded memory: auxiliary metadata reads stay within the existing sealed query limits, no unbounded collection or response field is added, and the frame row cap remains unchanged.
- Comments explain invariant, provenance, or safety reasoning; do not narrate syntax or retain stale claims.
- If a public contract is documented in a crate README, update both English `README.md` and Russian `README.ru.md` mirrors in the same task.
- Run Rust commands for the host as `--target aarch64-apple-darwin`; repository default musl linkage is not the local verification target.

---

### Task 1: One continuity verdict with exact extension resets

**Files:**
- Modify: `crates/kronika-analytics/src/web_projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`

**Interfaces:**
- Consumes: `ProjectionInput::{current,previous,gaps,predecessor_ts_us,snapshot_ts_us}` and the `reset_metadata` fields `pg_stat_statements_reset_at`, `pg_store_plans_reset_at`.
- Produces: private `ContinuityVerdict::{Continuous,FirstPoint,Gap,Reset}` and `continuity_for(view, input) -> ContinuityVerdict`; `delta` and delta-sum helpers consume the verdict instead of a boolean. The effective reset row is the latest bounded service-section sample at-or-before the selected snapshot in the same PGM.

- [ ] **Step 1: Write failing continuity tests**

Add tests that make the reason observable:

```rust
#[test]
fn exact_statement_reset_invalidates_increasing_counters() {
    // predecessor at 10, current at 20, reset metadata sampled at 18
    // with its exact extension reset marker at 15;
    // calls and total_exec_time both increase across the samples.
    // Assert mean is null and its classification reason is "reset".
}

#[test]
fn exact_plan_reset_invalidates_increasing_counters() {
    // Same interval and marker shape using pg_store_plans_reset_at.
    // Assert calls and mean are null even though raw counters increased.
}

#[test]
fn reset_before_the_predecessor_does_not_break_the_interval() {
    // reset marker at 5, predecessor at 10, current at 20.
    // Assert the normal positive delta is returned.
}
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web exact_statement_reset_invalidates_increasing_counters
cargo test --target aarch64-apple-darwin -p pg_kronika-web exact_plan_reset_invalidates_increasing_counters
```

Expected: the increasing counters are incorrectly emitted because only negative deltas currently signal reset.

- [ ] **Step 3: Add bounded auxiliary reset inputs and continuity evaluation**

Add `reset_metadata` as an auxiliary `WebInput` for statements and plans, without making its absence gate their primary raw rows. Because reset metadata is collected every 30 seconds rather than at every statement/plan snapshot, read it from the current descriptor start through the selected snapshot and use the latest row at-or-before that snapshot. Compute one verdict per projected frame with this order:

```rust
enum ContinuityVerdict {
    Continuous,
    FirstPoint,
    Gap,
    Reset,
}

// Gap wins when the predecessor was rejected or storage reported a gap.
// FirstPoint applies when no predecessor exists and no gap was reported.
// Reset applies when the exact family marker is in (predecessor_ts, snapshot_ts].
// Otherwise the interval is Continuous.
```

Pass the verdict through `project_view` to every delta evaluator. `delta` returns `Gap`, `Missing`, or `Reset` from the verdict before comparing values, and still detects a counter-local reset when `current < previous`.

- [ ] **Step 4: Run focused frame and catalog tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_frame
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_catalog
```

Expected: PASS, including existing negative-reset, partial-denominator, and max-gap coverage.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-analytics/src/web_projection.rs bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/tests/ui_frame.rs bins/pg_kronika-web/src/tests/ui_catalog.rs
git commit -m "fix(web): honor exact counter reset boundaries"
```

### Task 2: Convert Linux ticks with instance HZ

**Files:**
- Modify: `crates/kronika-analytics/src/web_projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/build.rs`

**Interfaces:**
- Consumes: the latest bounded `instance_metadata.clock_ticks_per_sec` at-or-before each process snapshot in the same PGM, plus the Task 1 continuity verdict.
- Produces: `clock_ticks_per_sec(input) -> Option<f64>`, an executable HZ-aware web-index formula, and tick-rate evaluators returning CPU/block-wait seconds per wall second in both frames and heatmap/spark series.

- [ ] **Step 1: Write failing HZ and denominator tests**

Add focused cases with a 10-second interval:

```rust
#[test]
fn process_cpu_and_block_delay_are_divided_by_clock_ticks() {
    // HZ=100 metadata precedes the current process snapshot;
    // CPU tick delta=90; blkdelay tick delta=20.
    // Assert CPU=0.09 and block_delay=0.02.
}

#[test]
fn activity_cpu_uses_the_same_instance_clock() {
    // Unique same-snapshot PID association, carried-forward HZ=100,
    // CPU tick delta=90.
    // Assert best_effort link remains visible and CPU=0.09.
}

#[test]
fn tick_rates_are_null_without_a_positive_clock() {
    // Exercise missing, zero, and negative clock_ticks_per_sec.
    // Assert CPU and block_delay are null; byte rates remain computable.
}

#[test]
fn process_cpu_web_index_uses_hz_without_gating_the_canonical_metric() {
    // Build a PGM with process snapshots at 10s and 20s and one HZ=100
    // metadata row at or before both. Assert metric status Complete and 0.09.
}

#[test]
fn activity_cpu_web_index_uses_unique_same_snapshot_pid_attribution() {
    // Attribute activity samples to the unique same-timestamp process lifetime,
    // then assert the HZ-aware activity CPU series value; ambiguity yields none.
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web clock_ticks
cargo test --target aarch64-apple-darwin -p pg_kronika-web activity_cpu_uses_the_same_instance_clock
```

Expected: current CPU/block-delay values are larger by the HZ factor or incorrectly remain non-null.

- [ ] **Step 3: Add metadata requirements and tick conversion**

Add an `instance` input backed by mandatory `instance_metadata` to activity and process views. The frame query reads this slow-cadence service section from the current descriptor start through the selected snapshot and selects the latest valid row at-or-before the snapshot, matching the established context projection. Require it for CPU and block-delay metrics/columns, but not for process byte rates; the process view is correctly gated when its canonical CPU metric cannot be interpreted without HZ, while activity's non-CPU canonical metric remains available. Change normative formulas to:

```text
positive_delta(utime + stime) / (clock_ticks_per_sec * elapsed_seconds)
positive_delta(blkdelay_ticks) / (clock_ticks_per_sec * elapsed_seconds)
```

Bump activity CPU and process CPU metric revisions to `2`; bump activity and process view revisions to `2`. Return `null` for absent/non-finite/non-positive HZ or elapsed time, and do not clamp the result to one core.

Represent tick rates as an executable formula variant distinct from ordinary byte-per-second rates. Extend the bounded reader web-index evaluator so process CPU consumes process rows plus the carried-forward HZ timeline, and activity CPU additionally uses the existing unique same-snapshot PID attribution without bridging different `(pid,starttime)` lifetimes. Do not leave either CPU metric gated merely because it has metadata/attribution inputs.

- [ ] **Step 4: Run focused tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_frame
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_catalog
cargo test --target aarch64-apple-darwin -p kronika-analytics web_projection
cargo test --target aarch64-apple-darwin -p kronika-reader web_index
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-analytics/src/web_projection.rs crates/kronika-reader/src/overview/web_index/build.rs bins/pg_kronika-web/src/ui/catalog.rs bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/tests/ui_frame.rs bins/pg_kronika-web/src/tests/ui_catalog.rs
git commit -m "fix(web): convert process ticks with instance hz"
```

### Task 3: Rename claims to match plan and lock provenance

**Files:**
- Modify: `crates/kronika-analytics/src/web_projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify if references exist: `web/src/**/*.{ts,tsx}`

**Interfaces:**
- Consumes: raw plan fields `first_call`, `last_call`; raw lock field `waitstart` and `lock_granted`.
- Produces: public columns `first_call`, `last_call`, and `wait_age_us`; lock metric expression `max(wait_age_us from waitstart)`.

- [ ] **Step 1: Write failing public-contract tests**

```rust
#[test]
fn plan_call_timestamps_keep_their_source_names() {
    // Catalog contains first_call and last_call and excludes first_seen/last_seen.
    // Projection returns the raw timestamp values under the new codes.
}

#[test]
fn lock_wait_age_requires_waitstart() {
    // A waiting lock with waitstart returns snapshot_ts-waitstart.
    // A granted lock with only xact_start/query_start returns null.
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web plan_call_timestamps_keep_their_source_names
cargo test --target aarch64-apple-darwin -p pg_kronika-web lock_wait_age_requires_waitstart
```

Expected: catalog codes are old and lock projection fabricates a hold duration.

- [ ] **Step 3: Rename codes, formulas, presets, and bilingual copy**

Replace all public `first_seen`/`last_seen` usages with `first_call`/`last_call`. Replace `wait_or_hold_us` with `wait_age_us`, compute it only from `waitstart`, and remove every claim that it measures lock hold time. Bump plan and lock view revisions to `2`, lock metric revision to `2`, and keep numeric codes stable.

Update EN/RU labels and descriptions. Also make all statement/plan/table/index buffer-read copy explicit that `*_blks_read` is a PostgreSQL-buffer read which may be served from the OS page cache; do not call it physical disk I/O. Describe `/proc/<pid>/io` byte rates as storage-accounted process I/O.

- [ ] **Step 4: Verify localized keys and focused tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_catalog
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_frame
npm --prefix web test -- --runInBand
```

Expected: Rust tests PASS; frontend tests PASS with no missing EN/RU keys. If the repository test runner rejects `--runInBand`, run its existing `npm --prefix web test` command unchanged and record that exact command.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-analytics/src/web_projection.rs bins/pg_kronika-web/src/ui/catalog.rs bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/tests/ui_catalog.rs bins/pg_kronika-web/src/tests/ui_frame.rs web/src
git commit -m "fix(web): align labels with collected evidence"
```

### Task 4: Separate PostgreSQL vacuum generations

**Files:**
- Modify: `crates/kronika-analytics/src/web_projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: `heap_blks_scanned`, `heap_blks_total`, PG10–16 `num_dead_tuples`, and PG17+ `num_dead_item_ids`/`dead_tuple_bytes`.
- Produces: direct `progress = scanned / total`, plus nullable public `dead_tuples`, `dead_item_ids`, and `dead_tuple_bytes` columns with distinct units and descriptions.

- [ ] **Step 1: Write failing version-layout tests**

```rust
#[test]
fn vacuum_progress_divides_by_total_not_total_plus_scanned() {
    // scanned=25,total=100 => 0.25; total=0 => null.
}

#[test]
fn vacuum_dead_work_fields_are_not_coalesced() {
    // PG16-shaped row exposes dead_tuples only.
    // PG17-shaped row exposes dead_item_ids and dead_tuple_bytes only.
}
```

Update the catalog test to require the three separate columns and exact formulas/units.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web vacuum_progress_divides_by_total_not_total_plus_scanned
cargo test --target aarch64-apple-darwin -p pg_kronika-web vacuum_dead_work_fields_are_not_coalesced
```

Expected: progress is `0.2` and the PG17 item count currently appears as dead tuples.

- [ ] **Step 3: Implement direct ratio and separate columns**

Use a dedicated finite division helper for `scanned / total`; it returns null for a zero, missing, or non-finite denominator. Change the normative metric formula to `max(heap_blks_scanned / heap_blks_total)` and bump vacuum metric and view revisions to `2`. Make each version-specific column a direct raw projection with no fallback between units, include all three in progress/dead-work presets, and add precise EN/RU descriptions.

- [ ] **Step 4: Run focused tests**

Run:

```bash
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_frame
cargo test --target aarch64-apple-darwin -p pg_kronika-web ui_catalog
cargo test --target aarch64-apple-darwin -p kronika-analytics web_projection
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-analytics/src/web_projection.rs bins/pg_kronika-web/src/ui/catalog.rs bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/tests/ui_catalog.rs bins/pg_kronika-web/src/tests/ui_frame.rs web/src/i18n/en.json web/src/i18n/ru.json
git commit -m "fix(web): separate vacuum progress semantics"
```

### Task 5: Synchronize public artifacts and run release gates

**Files:**
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `docs/api/openapi.json` or the repository's canonical generated OpenAPI path
- Modify: `web/src/demo/fixture.json` or the repository's canonical generated demo fixture path
- Modify if contract wording is present: `crates/kronika-analytics/README.md`
- Modify if contract wording is present: `crates/kronika-analytics/README.ru.md`
- Modify if contract wording is present: `bins/pg_kronika-web/README.md`
- Modify if contract wording is present: `bins/pg_kronika-web/README.ru.md`

**Interfaces:**
- Consumes: Tasks 1–4 public view/metric revisions, column codes, formulas, availability, and localized text.
- Produces: catalog revision `4`, synchronized generated artifacts, and a clean fully verified PR branch.

- [ ] **Step 1: Add/adjust artifact freshness assertions before regeneration**

Ensure catalog tests assert:

```rust
assert_eq!(catalog.revision, 4);
// Affected view revisions are 2, affected metric revisions are 2,
// numeric view and metric codes are unchanged.
```

Run the focused catalog test and verify RED while the revision is still `3`.

- [ ] **Step 2: Bump revision and regenerate with repository commands**

Discover the checked-in generator commands from `xtask`, package scripts, or contributor docs, run those commands, and do not hand-edit generated JSON. Update both README language mirrors only where they state a changed contract.

- [ ] **Step 3: Review memory and comment quality**

Inspect `git diff codex/pr01-semantic-contracts...HEAD` and confirm:

```text
- reset_metadata and instance_metadata share existing bounded section queries;
- no new unbounded rows, maps, strings, or response payloads;
- no per-row scan was introduced where one per-frame lookup suffices;
- comments describe reset precedence, provenance, or safety only;
- no comment or i18n string still claims physical disk, first observation, or lock hold age.
```

- [ ] **Step 4: Run all required gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --target aarch64-apple-darwin --workspace --all-targets --all-features -- -D warnings
cargo test --target aarch64-apple-darwin --workspace
cargo xtask check-deps
```

Then run the repository's OpenAPI freshness command and frontend Node 22 suite. If system Node is not 22, resolve the Node 22 executable with `npx -y node@22 -p 'process.execPath'` and invoke the installed npm CLI through it. Expected: every gate PASS; existing explicitly ignored tests remain ignored.

- [ ] **Step 5: Commit**

```bash
git add bins/pg_kronika-web/src/ui/catalog.rs docs web crates/kronika-analytics/README.md crates/kronika-analytics/README.ru.md bins/pg_kronika-web/README.md bins/pg_kronika-web/README.ru.md
git commit -m "chore(web): publish metric semantics revision four"
```

Use pathspecs only for files that actually changed; do not create empty documentation changes.
