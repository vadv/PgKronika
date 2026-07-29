# Passive Instance Metadata Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Make `instance_metadata` purely informational, remove the invented service identity completely, and ensure no incident or analytic result depends on the section.

**Architecture:** The collector keeps one exact `instance_metadata` schema with factual PostgreSQL and OS fields only. Incident preparation, entity joins, plan continuity, and overview reset extraction stop requesting the section. Existing typed identities, snapshot provenance, reset metadata, counter decreases, and gaps remain the only analytic boundaries.

**Tech Stack:** Rust 2024 workspace, generated registry codecs, Axum Web API, Python 3 repository guards, Cucumber BDD, Markdown contracts

## Global Constraints

- The current tracked tree must contain neither removed identifier.
- `instance_metadata` remains type `1_021_001` with `ts`, `hostname`, `pg_version_num`, `kernel_version`, nullable `pg_system_identifier`, `clock_ticks_per_sec`, `page_size_bytes`, `boot_id`, and `btime`.
- No legacy decoder, alias, reserved column, compatibility layout, migration reader, or replacement identifier is added.
- Every selected valid payload section remains eligible for incident analysis without `instance_metadata`.
- `instance_metadata` does not affect incident admission, keys, joins, plan continuity, metric reset epochs, coverage, or quality.
- Resource bounds, typed entity identities, snapshot provenance, reset metadata, negative-counter reset detection, and gap detection remain enforced.
- Manual edits use `apply_patch`.
- Final verification includes repository guards, qualification validators, formatting, strict clippy, workspace tests, and dependency checks.

---

### Task 1: Remove The Service Identity From The Stored Schema

**Files:**
- Modify: `scripts/validate-single-root-terminology.py`
- Modify: `scripts/test_validate_single_root_terminology.py`
- Modify: `crates/kronika-registry/src/codec/instance_metadata.rs`
- Modify: `bins/pg_kronika-collector/src/config.rs`
- Modify: `bins/pg_kronika-collector/src/service_sections.rs`
- Modify: `bins/pg_kronika-collector/src/buffering.rs`
- Modify: `bins/pg_kronika-collector/src/segments.rs`
- Modify: `crates/kronika-writer/src/buffer.rs`
- Modify: `crates/kronika-writer/src/recovery.rs`
- Modify: `crates/kronika-reader/src/overview/qualification_fixture.rs`
- Modify: `crates/kronika-bdd/features/service_metadata.feature`

**Interfaces:**
- Produces: `InstanceMetadata` with exactly nine factual fields
- Removes: the dedicated collector configuration input and dictionary value
- Extends: `forbidden_terms() -> tuple[str, ...]` with the two assembled tokens

- [x] **Step 1: Extend the guard regression**

Add both assembled tokens to the guard test constants and `RETIRED_TERMS`.
Keep every spelling split across string fragments so the tests do not violate
their own invariant.

- [x] **Step 2: Run the guard tests to verify RED**

Run:

```bash
python3 -B scripts/test_validate_single_root_terminology.py
```

Expected: the guard-source test fails because the validator does not yet reject
the new tokens.

- [x] **Step 3: Extend the validator**

Add the same two fragment-built strings to `forbidden_terms()`. Do not special
case paths or introduce an allow-list.

- [x] **Step 4: Remove the schema and collector field**

Delete the field from `InstanceMetadata`, `Config`, `InstanceFacts`, buffering,
recovery fixtures, qualification fixtures, and collector test defaults. Remove
the environment read and hostname fallback. Keep all remaining field types,
column classes, order, collection schedule, and error handling unchanged.

- [x] **Step 5: Update codec and collector tests**

Make the codec contract-shape test assert the exact remaining column list.
Remove BDD scenarios and steps that set or inspect the deleted override while
retaining hostname, PostgreSQL, kernel, boot, clock-tick, and page-size
assertions.

- [x] **Step 6: Run focused checks**

Run:

```bash
python3 -B scripts/test_validate_single_root_terminology.py
cargo test -p kronika-registry -p kronika-writer -p pg_kronika-collector --lib --target aarch64-apple-darwin
```

Expected: the guard tests and focused schema/collector suites pass.

### Task 2: Make Incidents Independent Of Instance Metadata

**Files:**
- Modify: `bins/pg_kronika-web/src/incident_input.rs`
- Modify: `bins/pg_kronika-web/src/handlers/incidents.rs`
- Modify: `bins/pg_kronika-web/src/incident/engine.rs`
- Modify: `bins/pg_kronika-web/src/incident/entity_join.rs`
- Modify: `bins/pg_kronika-web/src/incident/active/activity.rs`
- Modify: `bins/pg_kronika-web/src/incident/model.rs`
- Modify: `bins/pg_kronika-web/src/tests/incidents.rs`
- Modify: `bins/pg_kronika-web/src/tests/problems.rs`
- Modify: `crates/kronika-bdd/src/harness/web.rs`
- Modify: `docs/superpowers/specs/2026-07-16-kronika-incident-lenses-design.md`
- Modify: `docs/superpowers/specs/2026-07-17-kronika-incident-implementation.md`

**Interfaces:**
- Produces: `IncidentKeyV2::new(start_us, end_us, members, max_bytes)`
- Produces: `EntityJoinIndex::new(relation_limit)` and `matches(&EntityJoinKey)`
- Removes: service-identity fields from `PreparedInput`, `IncidentConfig`, and `EvalContext`
- Removes: missing/conflicting service-identity input and response states

- [x] **Step 1: Write the failing route regression**

Change the incident fixture writer so it emits no `instance_metadata`. Add a
test that writes two valid payload segments, requests one range covering both,
and asserts that the expected incident and findings use both segments.

- [x] **Step 2: Run the route test to verify RED**

Run:

```bash
cargo test -p pg_kronika-web --lib tests::incidents --target aarch64-apple-darwin
```

Expected: the route returns the current missing-identity data-quality response
instead of incidents.

- [x] **Step 3: Remove incident input admission**

Remove `instance_metadata` from the requested logical sections. Delete the
service-identity loader, byte charging performed only for that value, prepared-input
field, error variants, handler branches, and problem-registry entries.

- [x] **Step 4: Replace the incident key**

Rename the incident key type, change the version byte, and encode
only the incident bounds and sorted `EpisodeRefV1` members. Preserve checked
length arithmetic and both per-key and aggregate key-byte limits.

- [x] **Step 5: Remove service scope from joins**

Delete the service-scope wrapper. Construct `EntityJoinIndex` from only its relation limit;
match only the complete `EntityJoinKey`. Update activity/lock helpers and tests
so exact typed identity and shared-snapshot mismatches still fail while an
unrelated service identity no longer exists in the interface.

- [x] **Step 6: Simplify engine configuration**

Remove the label from production/test constructors and `EvalContext`. Keep
clock relation, clustering, work admission, output caps, ordering, and evidence
limits unchanged.

- [x] **Step 7: Run focused incident checks**

Run:

```bash
cargo test -p pg_kronika-web --lib incident:: --target aarch64-apple-darwin
cargo test -p pg_kronika-web --lib tests::incidents --target aarch64-apple-darwin
cargo test -p pg_kronika-web --lib tests::problems --target aarch64-apple-darwin
cargo test -p kronika-bdd --lib --target aarch64-apple-darwin
```

Expected: incidents work without metadata; typed/snapshot join tests and exact
problem/OpenAPI tests pass.

### Task 3: Remove Instance Metadata From Plan Continuity

**Files:**
- Modify: `bins/pg_kronika-web/src/plan_anomaly.rs`
- Modify: `bins/pg_kronika-web/src/tests/anomalies.rs`
- Modify: `bins/pg_kronika-web/benches/anomalies.rs`
- Modify: `crates/kronika-bdd/src/steps/plan_anomalies.rs`
- Modify: `crates/kronika-bdd/src/steps/store_plans.rs`
- Modify: `docs/superpowers/plans/2026-07-22-analysis-remaining-contracts.md`

**Interfaces:**
- Preserves: reset, extension-version, `compute_query_id`, membership,
  coverage, gap, and counter validation
- Removes: `InstanceContext`, instance lookup/conflict maps, instance-only
  `ContinuityFailure` variants, and their quality counters

- [x] **Step 1: Write the failing plan regression**

Remove `instance_metadata` from the plan-anomaly fixture and assert that the
same supported anomaly remains evaluated with complete population. Assert the
exact quality object does not contain instance-specific counters.

- [x] **Step 2: Run the plan route test to verify RED**

Run:

```bash
cargo test -p pg_kronika-web --lib tests::anomalies --target aarch64-apple-darwin
```

Expected: continuity is currently reported as metadata-unknown or the fixture
fails the old exact quality shape.

- [x] **Step 3: Remove the context consumer**

Delete `instance_metadata` from `PLAN_CONTEXT_SECTIONS`; remove parsing,
conflict/gap tracking, nearest-instance lookup, major-version checks, system
identifier checks, and the corresponding failure variants.

- [x] **Step 4: Close the new quality shape**

Remove counters produced only by the deleted checks from `QualityCounts`,
`to_json`, completeness, BDD assertions, fixtures, and benchmark setup. Do not
leave always-zero compatibility properties.

- [x] **Step 5: Run focused plan checks**

Run:

```bash
cargo test -p pg_kronika-web --lib plan_anomaly:: --target aarch64-apple-darwin
cargo test -p pg_kronika-web --lib tests::anomalies --target aarch64-apple-darwin
cargo test -p kronika-bdd --lib --target aarch64-apple-darwin
```

Expected: plan analysis works without `instance_metadata`, and the closed
quality object contains no retired counters.

### Task 4: Remove Boot Metadata From Metric Reset Semantics

**Files:**
- Modify: `crates/kronika-reader/src/overview/metric_extract.rs`
- Modify: `crates/kronika-reader/src/overview/facts.rs`
- Modify: `crates/kronika-analytics/src/overview/metric.rs`
- Modify: `crates/kronika-reader/src/overview/qualification_fixture.rs`
- Modify: `docs/superpowers/specs/2026-07-22-overview-index-timeline-api.md`

**Interfaces:**
- Removes: `instance_metadata` from metric extraction and `ResetTimeline`
- Produces: OS counter descriptors with `reset_family = None`
- Produces: stable unqualified reset epochs derived from `MetricSeriesId`
- Preserves: negative-value rejection, value-decrease resets, gaps, work
  limits, factor inventory, units, and typed entity identity

- [x] **Step 1: Write failing reset regressions**

Add tests proving cgroup and host OS counters:

```text
same series + increasing values => Valid
same series + decreasing value => Reset
known gap between increasing values => Gap
```

Construct the fixtures without `instance_metadata` and assert the descriptors
have no boot reset family or missing-reset-context loss.

- [x] **Step 2: Run focused tests to verify RED**

Run:

```bash
cargo test -p kronika-reader --lib overview::metric_extract --target aarch64-apple-darwin
```

Expected: current extraction reports missing reset context and gives every
sample a timestamp-derived epoch.

- [x] **Step 3: Remove the metadata timeline**

Delete the type from the supported-source allow-list and decoded reset
sections. Remove OS boot fields and methods from `ResetContext` and
`ResetTimeline`.

- [x] **Step 4: Emit ordinary OS counter series**

Build cgroup/host OS descriptors with `reset_family = None`. Derive the
required stored epoch deterministically from the completed `MetricSeriesId`,
without hostname, path, timestamp, metadata, global mutable state, or a magic
configuration value.

- [x] **Step 5: Remove obsolete losses and expectations**

Delete `MissingResetContext` production for the affected OS factors and update
descriptor inventories, facts tests, golden blocks, and qualification fixtures
to the new series identities.

- [x] **Step 6: Run focused analytics checks**

Run:

```bash
cargo test -p kronika-analytics --lib --target aarch64-apple-darwin
cargo test -p kronika-reader --lib --target aarch64-apple-darwin
```

Expected: metric and reader suites pass with ordinary reset/gap semantics.

### Task 5: Remove Every Current-Tree Trace And Verify

**Files:**
- Modify: `docs/type-registry/postgresql.md`
- Modify: `docs/type-registry/postgresql-collection.md`
- Modify: all remaining tracked files reported by the repository guard
- Move: `docs/superpowers/specs/2026-07-29-passive-instance-metadata-design.md`
  to `docs/superpowers/implemented/specs/`
- Move: this plan to `docs/superpowers/implemented/plans/`
- Modify: `docs/superpowers/README.md`

**Interfaces:**
- Preserves: factual `instance_metadata` documentation
- Removes: every exact field/env spelling and every behavioral claim that
  metadata gates analysis

- [x] **Step 1: Normalize documentation and fixtures**

Describe `instance_metadata` only as passive facts. Remove retired field/env
prose, old incident key shapes, service-scoped joins, instance continuity gates,
boot reset-family claims, and migration-only references from active and
implemented documents.

- [x] **Step 2: Stage and run the repository guard**

Run:

```bash
git add -A
python3 -B scripts/test_validate_single_root_terminology.py
python3 -B scripts/validate-single-root-terminology.py
```

Expected: guard tests pass and the tracked tree has no matches.

- [x] **Step 3: Run qualification and repository gates**

Run:

```bash
python3 -B scripts/validate-pgm-size-reduction.py
python3 -B scripts/validate-pgm-coalesced-sections.py
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --target aarch64-apple-darwin -- -D warnings
cargo test --workspace --target aarch64-apple-darwin
cargo run -p xtask --target aarch64-apple-darwin -- check-deps
git diff --cached --check
```

On macOS, record and exclude only the already-proven Linux-only
`rename_noreplace` quarantine/recovery tests if the unfiltered workspace run
returns `Unsupported`.

Observed on macOS: the unfiltered tests reproduce `Unsupported` in exactly
these eleven Linux-only quarantine/recovery cases:

```text
root::tests::remove_quarantine_entry_frees_bytes_once_and_rechecks_identity
rotation::tests::plan_from_a_real_scan_orders_all_victim_kinds_and_skips_foreign_temporaries
tests::runtime::corrupt_existing_pgm_does_not_block_active_journal_recovery
tests::runtime::corrupt_existing_pgm_does_not_block_writer_ownership
tests::runtime::failed_recovery_seal_preserves_evidence_and_continues_empty
tests::runtime::startup_finishes_recovery_pending_evidence_after_activation
tests::runtime::startup_quarantines_a_torn_header_and_accepts_future_windows
tests::runtime::startup_quarantines_only_stale_writer_temporaries
tests::runtime::startup_recovers_a_pending_alternate_generation
tests::runtime::startup_recovers_complete_frames_despite_a_wrong_recorded_body_length
tests::runtime::startup_validation_quarantines_body_and_catalog_corruption
```

The filtered workspace suite passes. The terminology guard and its six tests,
both qualification validators, formatting, strict clippy, all four focused
Rust suites, the 77 BDD tests, the 612 Web API tests, the 450 reader tests, and
`xtask check-deps` also pass.

- [x] **Step 4: Complete project records**

Mark every plan checkbox complete, record the exact verification caveat, move
the design and plan to `implemented`, and update active/implemented counts.

- [x] **Step 5: Commit**

```bash
git commit -m "refactor: сделать instance metadata пассивной"
```
