# Forensic UI PR 1: Semantic Relation Contracts Implementation Plan

> **For Codex:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan.

**Goal:** Make every UI relationship expose honest, closed link-quality semantics and remove the invalid exact comparison between PostgreSQL `backend_start` and Linux process `starttime`.

**Architecture:** The UI catalog owns one serialized `RelationKind` enum shared by catalog joins and entity-detail provenance. Activity may enrich a backend with one same-snapshot PID candidate, but process deltas may continue only through the candidate's exact stored `(pid, starttime)` lifetime. Statement-to-plan links remain attribution evidence and carry fork-specific methods instead of pretending that field equality proves identity.

**Tech Stack:** Rust 2024, Axum, Serde, Utoipa/OpenAPI 3.1, TypeScript 5.9 generated API types, Vitest, Cargo tests.

## Global Constraints

- The wire vocabulary is exactly `exact`, `lifetime`, `temporal`, `best_effort`, and `unavailable`.
- `backend_start` and OS `starttime` must never be compared for equality or described as the same clock.
- Same-snapshot PID-only enrichment is `best_effort`, never `exact` or `lifetime`.
- A process counter delta is valid only when the current and previous OS rows share both `pid` and `starttime`.
- Statement→Plan is attribution, not identity. OSSC and vadv rows expose different method codes.
- Public contract changes update both README languages, generated multi-file OpenAPI, and `web/src/api/schema.d.ts` in the same PR.
- Frame and entity reads retain their existing row, cell, byte, query-string, cursor, and response caps. No new collection may grow beyond caller-owned bounded input.
- Comments explain provenance invariants or API contracts; comments that narrate the next line are removed.

### Task 1: Add the closed relation-quality vocabulary to the catalog

**Files:**

- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`

**Step 1: Write failing behavior tests**

Add a catalog test that serializes the activity, vacuum, and process joins and checks hand-written literals:

```rust
assert_eq!(activity_join["kind"], "best_effort");
assert_eq!(activity_join["fields"], json!(["pid", "ts"]));
assert_eq!(activity_join["provenance"], "same_snapshot_pid_only");
assert_eq!(process_cgroup_join["kind"], "exact");
```

The test catches a regression that promotes a PID-only or time-only association to exact evidence.

**Step 2: Run the focused test and observe RED**

Run: `cargo test -p pg_kronika-web ui_catalog -- --nocapture`

Expected: the new assertions fail because `JoinSpec` has no `kind` and Activity still advertises `backend_start=starttime`.

**Step 3: Implement the minimal catalog contract**

Add a public-to-the-crate serialized schema enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelationKind {
    Exact,
    Lifetime,
    Temporal,
    BestEffort,
    Unavailable,
}
```

Add `kind: RelationKind` to `JoinSpec`, bump `CATALOG_REVISION`, and classify the four current joins:

- Activity→Process: `BestEffort`, fields `pid` and `ts`, method `same_snapshot_pid_only`.
- Activity→Replication: `Temporal`, existing fields and method.
- Vacuum→Tables: `Temporal`, existing fields and method.
- Process→Cgroup: `Exact`, because both rows originate from the same bounded `read_process` result; keep the existing fields and method.

Do not introduce an unused exact token to make current data look stronger.

**Step 4: Run GREEN**

Run: `cargo test -p pg_kronika-web ui_catalog -- --nocapture`

Expected: all catalog tests pass.

**Step 5: Commit**

```bash
git add bins/pg_kronika-web/src/ui/catalog.rs bins/pg_kronika-web/src/tests/ui_catalog.rs
git commit -m "feat(web): добавить качество связей UI"
```

### Task 2: Make Activity process enrichment explicitly best-effort and lifetime-safe

**Files:**

- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`

**Step 1: Write failing behavior tests**

Add fixtures where PostgreSQL `backend_start` deliberately differs from OS `starttime`.

Cover these behaviors:

1. One OS row with the same snapshot and PID yields `process_link = "best_effort"` and exposes candidate CPU/I/O operands.
2. Two OS rows with the same snapshot and PID yield `process_link = null` and no OS enrichment.
3. A previous OS row with the same PID but a different `starttime` never contributes to a counter delta.

Expected values must be hand-derived literals. The tests catch false exactness, PID-reuse bridging, and ambiguous candidate selection.

**Step 2: Run the focused tests and observe RED**

Run: `cargo test -p pg_kronika-web ui_frame -- --nocapture`

Expected: the candidate test fails because projection requires `backend_start == starttime`; the new column is absent.

**Step 3: Implement candidate lookup and exact continuation**

Add a non-lazy text column named `process_link` to Activity. Its formula is `best_effort same-snapshot PID association` and it requires the process input.

Replace `activity_process` with two bounded helpers:

- candidate lookup returns a process only when exactly one current row matches `ts` and `pid`;
- predecessor lookup accepts the selected current process and matches the previous row by `pid` and `starttime`.

Do not materialize another collection. Scan the already bounded section page and stop after detecting a second candidate.

Project `process_link` as `best_effort` only when a unique candidate exists. Otherwise return `null` for the link and OS-derived Activity cells.

**Step 4: Run GREEN**

Run: `cargo test -p pg_kronika-web ui_frame -- --nocapture`

Expected: all UI frame tests pass, including the three new lifetime-safety cases.

**Step 5: Commit**

```bash
git add bins/pg_kronika-web/src/ui/catalog.rs bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/tests/ui_frame.rs
git commit -m "fix(web): не выдавать PID-связь за точную"
```

### Task 3: Expose fork-specific Statement→Plan attribution provenance

**Files:**

- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/ui/entity.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_entity.rs`

**Step 1: Write failing entity tests**

Extend the statement detail fixture so one OSSC plan and one vadv plan link to the statement. Assert literal provenance objects:

```json
{
  "kind": "best_effort",
  "method": "ossc_queryid_dbid_userid_attribution",
  "fields": ["queryid", "dbid", "userid"]
}
```

```json
{
  "kind": "best_effort",
  "method": "vadv_queryid_stat_statements_dbid_userid_attribution",
  "fields": ["queryid_stat_statements", "dbid", "userid"]
}
```

The tests catch accidental collapse of fork-specific attribution and accidental promotion to exact identity.

**Step 2: Run the focused test and observe RED**

Run: `cargo test -p pg_kronika-web entity_point_returns_lazy_fields_and_only_proven_related_links -- --nocapture`

Expected: the response still emits `field_equality` and has no method.

**Step 3: Implement typed provenance**

Extend `ProjectedRelation` with `kind: RelationKind` and `method: &'static str`. Set both fields when plan rows are admitted. Extend `RelationProvenanceDto` to serialize `kind`, `method`, and `fields`.

Keep entity tokens and deduplication source-specific. Do not claim that `queryid` establishes a shared entity identity across extensions.

**Step 4: Run GREEN**

Run: `cargo test -p pg_kronika-web ui_entity -- --nocapture`

Expected: all entity tests pass.

**Step 5: Commit**

```bash
git add bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/ui/entity.rs bins/pg_kronika-web/src/tests/ui_entity.rs
git commit -m "feat(web): раскрыть provenance связей с планами"
```

### Task 4: Synchronize documentation, OpenAPI, and frontend types

**Files:**

- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Regenerate: `bins/pg_kronika-web/openapi/**`
- Regenerate: `web/src/api/schema.d.ts`

**Step 1: Document the contract in English and Russian**

Add the closed five-value relation vocabulary. State explicitly that Activity OS metrics use same-snapshot PID-only best-effort enrichment, while deltas stay within the selected `(pid, starttime)` lifetime. State that Statement→Plan is fork-specific attribution, not cross-extension identity.

Add localized labels and descriptions for `process_link` and the five relation kinds. Keep API identifiers untranslated.

**Step 2: Regenerate API artifacts**

Run:

```bash
make openapi
cd web && npm ci && npm run codegen
```

Expected: `RelationKind` is a closed enum in OpenAPI and TypeScript; `JoinSpec` requires `kind`; `RelationProvenanceDto` requires `kind`, `method`, and `fields`.

**Step 3: Run focused integration gates**

Run:

```bash
cargo test -p pg_kronika-web ui_ -- --nocapture
make web-frontend-check
```

Expected: Rust UI tests and frontend type, lint, format, and unit-test gates pass.

**Step 4: Commit**

```bash
git add bins/pg_kronika-web/README.md bins/pg_kronika-web/README.ru.md bins/pg_kronika-web/openapi web/src
git commit -m "docs(web): описать качество forensic-связей"
```

### Task 5: Run full gates and review the complete PR

**Files:**

- Review only: the full branch diff from `main`.

**Step 1: Run repository gates**

Run:

```bash
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
cargo test --workspace
cargo run -p xtask -- check-deps
make openapi
git diff --exit-code -- bins/pg_kronika-web/openapi
make web-frontend-check
```

**Step 2: Perform independent review passes**

Review the complete diff for:

- PostgreSQL semantics: no equality claim between `backend_start` and `starttime`; no queryid identity claim across extensions.
- Linux semantics: PID reuse cannot bridge process counters; same-snapshot PID remains best effort.
- Memory bounds: candidate detection scans caller-owned bounded rows without cloning or building an unbounded index.
- API compatibility: catalog revision changes; OpenAPI and TypeScript are synchronized.
- Tests: every new test catches a named production regression and uses independent expected literals.
- Comments and language: comments carry invariants; README mirrors agree.
- Focus: no visual redesign or metric-formula correction leaks into PR 1.

**Step 3: Fix every blocker and rerun affected gates**

Do not open the PR while any Critical or Important review finding remains.

**Step 4: Commit review fixes if needed**

```bash
git add <reviewed-files>
git commit -m "fix(web): учесть замечания ревью связей"
```

**Step 5: Push and open PR 1 against `main`**

The PR description must include the user-visible behavior, API migration note, test evidence, memory-bound analysis, and explicit out-of-scope list for PR 2.
