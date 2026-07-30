# Complete Numeric Threshold Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the typed Class 1 catalog from 42 to 69 numeric policies, including `max_connections` capacity and effective-autovacuum-threshold inputs.

**Architecture:** Keep the fixed-size `MetricInput` and `Policy` model unchanged. Split new catalog constants by PostgreSQL domain, register them in one canonical `MetricId`/`CATALOG` order, and let adapters supply reset-aware deltas and effective configuration limits.

**Tech Stack:** Rust 2024, existing `kronika-analytics` threshold module, built-in tests, no new dependencies.

## Global Constraints

- Classification remains deterministic O(1), allocation-free, without I/O or clock access.
- The catalog contains exactly 69 stable, unique entries in `MetricId::ALL` order.
- `client_backends / max_connections` uses `Fraction`; `5 / 35` is `Ok`, `70 / 100` is `Warning`, and `90 / 100` is `Critical`.
- Effective autovacuum thresholds are prepared by the adapter; disabled rules use `NotApplicable`.
- Cumulative PostgreSQL counters reach the catalog only as reset-aware deltas.
- All built-in thresholds remain `Calibration::Provisional`.
- English and Russian crate READMEs remain synchronized and state that no web consumer exists.

---

### Task 1: Golden Contract for 69 Policies

**Files:**
- Modify: `crates/kronika-analytics/tests/threshold_catalog.rs`

**Interfaces:**
- Consumes: existing `MetricId`, `CatalogEntry`, `Policy`, `MetricInput`, and catalog lookup API.
- Produces: an independent 69-entry golden table and boundary behavior tests.

- [ ] **Step 1: Extend the golden catalog before production code**

Add 27 expected entries after the current PostgreSQL table entries:

```rust
fraction_entry(
    MetricId::PgActivityClientBackendCapacity,
    boundary(Comparison::AtLeast, 0.70),
    boundary(Comparison::AtLeast, 0.90),
)
```

Use `scalar_entry` for the 23 remaining research policies. Add a warning-only
fraction fixture for:

```rust
MetricId::PgTablesVacuumThresholdRatio
MetricId::PgTablesAnalyzeThresholdRatio
MetricId::PgTablesInsertVacuumThresholdRatio
```

Each config-bound entry uses `Direction::HigherIsWorse`,
`warning = Some(Above(1.0))`, `critical = None`,
`ZeroDisposition::Classify`, and `Unit::Ratio`.

- [ ] **Step 2: Add behavior tests**

Add tests that assert:

```rust
assert_eq!(
    level(classify(
        MetricId::PgActivityClientBackendCapacity,
        MetricInput::Fraction { numerator: 5.0, denominator: 35.0 },
    )),
    Level::Ok,
);
assert_eq!(
    level(classify(
        MetricId::PgActivityClientBackendCapacity,
        MetricInput::Fraction { numerator: 70.0, denominator: 100.0 },
    )),
    Level::Warning,
);
assert_eq!(
    level(classify(
        MetricId::PgActivityClientBackendCapacity,
        MetricInput::Fraction { numerator: 90.0, denominator: 100.0 },
    )),
    Level::Critical,
);
```

Also cover invalid `max_connections`, autovacuum ratio equality at `1.0`,
client evictions at `0`, epsilon, `10`, and representative strict/inclusive
boundaries from every new domain.

- [ ] **Step 3: Run the contract test and verify RED**

Run:

```bash
cargo test -p kronika-analytics --test threshold_catalog
```

Expected: compilation fails because the new `MetricId` variants do not exist.

### Task 2: PostgreSQL Catalog Modules

**Files:**
- Create: `crates/kronika-analytics/src/threshold/catalog/postgres_activity.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/postgres_io.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/postgres_statements.rs`
- Create: `crates/kronika-analytics/src/threshold/catalog/postgres_replication.rs`
- Modify: `crates/kronika-analytics/src/threshold/catalog/postgres_tables.rs`
- Modify: `crates/kronika-analytics/src/threshold/catalog/mod.rs`

**Interfaces:**
- Consumes: `scalar_entry`, `fraction_entry`, `boundary`, `MetricId`, `Unit`, and exact thresholds from the design.
- Produces: 69 indexed `CatalogEntry` values addressable by `MetricId`.

- [ ] **Step 1: Add the 27 `MetricId` variants and stable string codes**

Append the variants in the same order as the design: activity/database,
cache/bgwriter/checkpointer, statements, replication, then three autovacuum
ratios. Extend `MetricId::ALL` to `[Self; 69]` and keep `#[repr(u8)]`.

- [ ] **Step 2: Add domain constants**

Each module contains only `pub(super) const CatalogEntry` declarations.
Use exact `Comparison` operators from the design. Event/rate/delta policies
use `ZeroDisposition::Inactive`; gauges and percentages use
`ZeroDisposition::Classify`.

For `client_evictions_per_second`, use:

```rust
warning = Some(boundary(Comparison::Above, 0.0))
critical = Some(boundary(Comparison::AtLeast, 10.0))
zero = ZeroDisposition::Inactive
```

- [ ] **Step 3: Add warning-only fraction construction**

Add a crate-private const helper that builds `Policy::Fraction` with
`warning = Some(boundary)`, `critical = None`, and a caller-supplied
`ZeroDisposition`. Use it only for the three effective autovacuum threshold
ratios.

- [ ] **Step 4: Register the constants**

Extend `CATALOG` to `[CatalogEntry; 69]` in exact `MetricId::ALL` order.
`catalog_entry(id)` must remain direct array indexing.

- [ ] **Step 5: Run catalog and analytics tests**

Run:

```bash
cargo test -p kronika-analytics --test threshold_catalog
cargo test -p kronika-analytics
```

Expected: all tests pass.

### Task 3: Public Contract Documentation

**Files:**
- Modify: `crates/kronika-analytics/README.md`
- Modify: `crates/kronika-analytics/README.ru.md`
- Modify: `docs/superpowers/specs/2026-07-29-absolute-threshold-catalog-design.md`

**Interfaces:**
- Consumes: the implemented 69-entry catalog.
- Produces: synchronized English/Russian crate contracts and an updated phase status.

- [ ] **Step 1: Update catalog counts and domains**

Replace the 42-policy wording with 69 policies. Document numeric PostgreSQL
activity, cache/bgwriter, statements/plans, replication, connection capacity,
and config-bound autovacuum indicators.

- [ ] **Step 2: Document adapter responsibilities**

State that adapters supply:

- `client backend` count and positive `max_connections`;
- reset-aware counter deltas;
- version/reloption-aware effective autovacuum thresholds;
- `NotApplicable` when a rule is disabled or lacks a valid scope.

- [ ] **Step 3: Preserve the no-web-consumer limitation**

Keep both README mirrors explicit that HTTP, OpenAPI, and UI do not yet expose
threshold verdicts.

### Task 4: Verification and Branch Update

**Files:**
- Review: all files changed since `origin/docs/anomaly-highlight-research`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: verified commits and an updated PR branch.

- [ ] **Step 1: Run formatting and focused gates**

```bash
cargo fmt --all --check
cargo clippy -p kronika-analytics --all-targets -- -D warnings
cargo test -p kronika-analytics
```

- [ ] **Step 2: Run workspace gates**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```

Record platform-specific failures verbatim; do not report them as feature
failures without confirming causality.

- [ ] **Step 3: Review the diff**

Check correctness, boundary operators, stable ordering, comments, and memory
bounds. The classification path must only use fixed-size enums and scalar
locals; no input-sized allocation may be introduced.

- [ ] **Step 4: Commit and update the rebased PR branch**

Commit focused code/docs changes, then push with:

```bash
git push --force-with-lease origin feat/absolute-threshold-catalog
```

Use `--force-with-lease` because the branch was rebased onto the force-updated
PR #140 base.
