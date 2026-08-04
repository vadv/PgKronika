# Process Detail Richness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn Process Detail into the dense three-column forensic workspace shown by the published Superdesign reference, using the Linux process fields PgKronika already collects and keeping same-PID PostgreSQL relationships easy to follow.

**Architecture:** Expand the public `processes` projection with typed point gauges and reset-safe interval rates from `os_process`; do not invent client-side metrics. Keep the reusable Entity Detail shell, but give process fields an explicit identity / CPU-scheduler-memory / I/O-cache-path / execution-context layout and compact semantic badges. The deterministic demo mirrors the real public projection so 1920×1080 visual QA exercises the same contract as production.

**Tech Stack:** Rust projection catalog and frame engine, React 19 + TypeScript, CSS, Vitest/Testing Library, deterministic Node demo server and shell verifier.

## Global Constraints

- The 1920×1080 viewport is the baseline and the root must not scroll.
- Process/Activity relationships follow every observed same-PID candidate; process `starttime` protects counter continuity only and never gates relationship visibility.
- `rchar`/`wchar` are logical syscall bytes; `read_bytes`/`write_bytes` are storage-accounted bytes.
- Only `max(logical read rate - storage read rate, 0)` may be labelled an approximate cache-served read estimate; it is never called a page-cache hit ratio.
- Missing `/proc/<pid>/io` remains null, never zero, and renders calmly as unavailable.
- Production values come from the typed backend projection; no frontend causal inference or synthetic production field.
- Summary stays operator-facing; opaque entity tokens, endpoint details, and quality mechanics remain in Raw.

---

### Task 1: Publish the collected Linux process evidence

**Files:**
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`

**Interfaces:**
- Consumes: `os_process` fields `state`, `ppid`, `uid`, `euid`, `starttime`, `nice`, `prio`, `rtprio`, `policy`, `curcpu`, `utime`, `stime`, `rundelay_ns`, `blkdelay_ticks`, `nvcsw`, `nivcsw`, `minflt`, `majflt`, `vmem_kb`, `rmem_kb`, `vswap_kb`, `syscr`, `syscw`, `rchar`, `wchar`, `read_bytes`, and `write_bytes`.
- Produces: public process columns `state`, `parent_pid`, `uid`, `effective_uid`, `started_at`, `current_cpu`, `nice`, `priority`, `realtime_priority`, `scheduler_policy`, `cpu_user`, `cpu_system`, `run_delay`, `voluntary_context_switches_per_second`, `involuntary_context_switches_per_second`, `minor_faults_per_second`, `major_faults_per_second`, `virtual_memory`, `swap`, `read_syscalls_per_second`, `write_syscalls_per_second`, `logical_read_bytes_per_second`, `logical_write_bytes_per_second`, and `cache_served_read_bytes_per_second` in addition to existing columns.

- [ ] **Step 1: Write catalog tests for the expanded process contract**

Add assertions that the named columns serialize with the expected types, units, sources/formulas, and `processes` requirements. Assert the CPU, memory, disk I/O, and processes presets expose a dense but purpose-specific subset instead of every detail field.

- [ ] **Step 2: Run the catalog test and verify it fails**

Run: `cargo test -p pg_kronika-web --target aarch64-apple-darwin ui_catalog::host_and_object_views_publish_prepared_lenses_and_temporal_relations -- --exact`

Expected: FAIL because the new process columns are absent.

- [ ] **Step 3: Write projection tests for gauges, rates, null I/O, and PID reuse**

Use a current sample at `20_000_000` and predecessor at `10_000_000`, 100 clock ticks/s. Assert user/system CPU and scheduler delay use their own positive deltas, context switches/faults/syscalls/logical/physical I/O use elapsed seconds, the approximate cache-served read rate is `max(rchar_rate - read_bytes_rate, 0)`, and nullable I/O produces null. Add a different-`starttime` case asserting every derived counter field is null while point gauges remain visible.

- [ ] **Step 4: Run the projection tests and verify they fail**

Run: `cargo test -p pg_kronika-web --target aarch64-apple-darwin process_detail -- --nocapture`

Expected: FAIL because projection dispatch does not recognize the new codes.

- [ ] **Step 5: Add catalog columns and reset-safe projection formulas**

Extend `processes_view()` with raw or derived columns matching the interface above. Extend `column_requires_predecessor()` for all rate fields. In `project_processes()`, map state and scheduler policy to bounded text labels, project gauges directly, use `tick_rate_sum()` for CPU/block delay, elapsed-rate helpers for counters, nanoseconds-per-elapsed-second for run delay, and a backend helper for the non-negative cache-served estimate. Preserve the existing `(pid,starttime)` predecessor lookup.

- [ ] **Step 6: Run focused Rust tests and format/lint the touched crate**

Run: `cargo test -p pg_kronika-web --target aarch64-apple-darwin process_detail -- --nocapture`

Run: `cargo test -p pg_kronika-web --target aarch64-apple-darwin ui_catalog::host_and_object_views_publish_prepared_lenses_and_temporal_relations -- --exact`

Run: `cargo fmt --all -- --check`

Expected: PASS.

- [ ] **Step 7: Commit the backend projection**

```bash
git add bins/pg_kronika-web/src/ui/catalog.rs bins/pg_kronika-web/src/ui/frame/projection.rs bins/pg_kronika-web/src/tests/ui_catalog.rs bins/pg_kronika-web/src/tests/ui_frame.rs
git commit -m "feat(web): expose rich process evidence"
```

### Task 2: Give Process Detail a deliberate forensic composition

**Files:**
- Modify: `web/src/components/DockOverlay.test.tsx`
- Modify: `web/src/components/DockOverlay.tsx`
- Modify: `web/src/components/DockOverlay.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: the expanded public process column codes from Task 1 and the existing `EntityPointResponse.fields` array.
- Produces: deterministic `detailFieldLayout(viewCode, fields)` grouping, semantic badges `S`, `G`, `R`, `ΔC`, and `EST`, and a compact process summary matching the reference's three analytical columns.

- [ ] **Step 1: Write component tests for field ordering and operator copy**

Create a process point fixture containing identity, CPU, scheduler, memory, logical I/O, approximate cache-served read, physical I/O, and command fields. Assert identity renders in the compact strip; the three groups appear in CPU/scheduler/memory, I/O/cache path, and process context order; semantic badges are present with accessible descriptions; the estimate says “approximate” but never “page-cache hits”, “proof”, “confidence”, “exact match”, `gaps`, or `gated`.

- [ ] **Step 2: Run the DockOverlay test and verify it fails**

Run: `npm --prefix web test -- --run src/components/DockOverlay.test.tsx`

Expected: FAIL because process ordering and semantic badges are not implemented.

- [ ] **Step 3: Implement explicit process field layout and badges**

Replace regex-only grouping for `viewCode === "processes"` with stable code lists. Render each metric as a compact label/value row with an optional badge derived from a static column-semantics map. Keep generic regex grouping as the reusable fallback for Activity, Statements, Plans, Tables, Indexes, Vacuum, and Events.

- [ ] **Step 4: Match the published Process Detail density**

Adjust the desktop workspace CSS to use a narrow identity band, 30–32 px metric rows, aligned values, restrained borders, grouped subheads, and a visually distinct cache-path sequence. Keep native scrolling inside the panel, one-column mobile stacking below 760 px, focus-visible controls, and reduced-motion behavior.

- [ ] **Step 5: Add complete Russian and English labels/descriptions**

Add translations for every new field, group subtitle, and badge tooltip. Descriptions name the source and formula concisely; nullable physical I/O explains permissions without alarming copy.

- [ ] **Step 6: Run frontend unit, type, and lint checks**

Run: `npm --prefix web test -- --run src/components/DockOverlay.test.tsx src/components/cellFormat.test.ts`

Run: `npm --prefix web run typecheck`

Run: `npm --prefix web run lint`

Expected: PASS.

- [ ] **Step 7: Commit the process workspace**

```bash
git add web/src/components/DockOverlay.test.tsx web/src/components/DockOverlay.tsx web/src/components/DockOverlay.css web/src/i18n/en.json web/src/i18n/ru.json
git commit -m "feat(web): compose dense process detail"
```

### Task 3: Make the deterministic demo exercise the real contract

**Files:**
- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/catalog.fixture.json`
- Modify: `web/scripts/verify-shell.mjs`

**Interfaces:**
- Consumes: the Task 1 catalog schema and Task 2 process detail field layout.
- Produces: a deterministic rich process entity reachable directly from the corresponding Activity PID at the 1920×1080 baseline.

- [ ] **Step 1: Add a failing shell assertion for rich process detail**

Extend the shell verifier to open a known Activity process relation, assert the `processes` deep link, and require the three analytical groups plus logical read, approximate cache-served read, physical read, CPU user/system, scheduler, memory, and command values.

- [ ] **Step 2: Run the shell verifier and verify it fails**

Run: `npm --prefix web run verify:shell`

Expected: FAIL because the demo process rows and catalog fixture expose only the old subset.

- [ ] **Step 3: Expand and align the demo fixture**

Update `catalog.fixture.json` to mirror the Rust catalog columns. Update `rowsProcesses()` so process types and PIDs match Activity rows intentionally, and include realistic deterministic values for every rich process field. Keep one deliberately unobserved Activity process to exercise calm null handling.

- [ ] **Step 4: Run shell and bundle verification**

Run: `npm --prefix web run verify:shell`

Run: `npm --prefix web run build`

Run: `npm --prefix web run check:bundle`

Expected: PASS at 1920×1080 and 1440×900, 1,000 Statements, 96 buckets, and the configured bundle budget.

- [ ] **Step 5: Commit the deterministic reference state**

```bash
git add web/scripts/demo-stub.mjs web/scripts/catalog.fixture.json web/scripts/verify-shell.mjs
git commit -m "test(web): verify rich process detail"
```

### Task 4: Visual comparison, full verification, and green integration

**Files:**
- Modify: `docs/superpowers/specs/2026-08-03-superdesign-fidelity-pass-design.md`
- Create: `docs/superpowers/plans/2026-08-03-process-detail-richness-design-qa.md`

**Interfaces:**
- Consumes: published reference `https://pgkronika-forensic-u.superdesign.cloud/` and local deterministic preview `http://127.0.0.1:4173/` at the same process-detail state.
- Produces: side-by-side 1920×1080 QA evidence, a reviewed PR, and a squash merge to `main` after all required checks pass.

- [ ] **Step 1: Capture reference and implementation at identical state and viewport**

Use the already selected in-app Browser. Capture both pages at 1920×1080 with Process Detail open, compare them in one visual input, and record concrete differences in spacing, density, alignment, typography, borders, and clipping.

- [ ] **Step 2: Fix every material visible mismatch and compare again**

Iterate only on source CSS/React/fixtures, rerun focused tests after each change, then capture the same viewport again. The result must preserve Health line, keep the root fixed, and expose the rich detail without a horizontal page scroll.

- [ ] **Step 3: Record design QA and clean the approved spec**

Write the compared URLs, viewport, state, visible checks, and screenshot paths to the QA document. Remove the existing trailing whitespace in the approved fidelity spec without changing its requirements.

- [ ] **Step 4: Run the complete project gates**

Run: `make web-frontend-check`

Run focused host-target Rust tests for `pg_kronika-web`, then rely on GitHub CI for the long workspace property suite because the local macOS environment defaults to Linux MUSL and stops the longest invariant test.

Expected: all frontend gates and focused backend tests PASS locally.

- [ ] **Step 5: Commit QA, request review, and resolve findings**

```bash
git add docs/superpowers/plans/2026-08-03-process-detail-richness-design-qa.md docs/superpowers/specs/2026-08-03-superdesign-fidelity-pass-design.md
git commit -m "docs(web): record process detail visual QA"
```

Request a code review against `origin/main`; fix all P0/P1/P2 findings and rerun affected gates.

- [ ] **Step 6: Push, open the PR, wait for every check, and merge**

Push `codex/pr15-process-detail-richness`, open a ready PR, monitor all required checks to green, squash-merge, and verify the resulting `main` commit and production asset rollout independently.
