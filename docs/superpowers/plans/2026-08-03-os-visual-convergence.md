# PR12 OS Visual Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the approved OS pressure experience as one dense 1920×1080
workspace with honest host context, row-coupled process CPU/I/O evidence and
bounded process populations.

**Architecture:** Add a dedicated `OsWorkspace` that owns the bounded host
spine and process heatmap, generalize the existing virtualized temporal matrix
for process identity, route OS away from the detached analytical center, and
keep reusable Entity Detail as the process inspector. Do not add backend fields
or fabricate unsupported resource series.

**Tech Stack:** React 19, TypeScript, TanStack Query, Vitest/Testing Library,
existing Rust Axum UI contracts, Puppeteer shell verifier.

## Global Constraints

- Baseline viewport is exactly 1920×1080 CSS px at DPR 1 and 100% zoom.
- Health Line remains the only upper chart and exactly 60 px high.
- Host, process and cgroup values are separate scopes and are never summed.
- `load_per_cpu` is runnable demand; `psi_io_some` is stalled time. Neither is
  utilization.
- Process CPU and I/O heatmaps contain interval-derived evidence only.
- RSS, threads, command and cgroup are point values and never become synthetic
  history.
- The process collection cap is 4,096 rows; caps remain visible as
  `resource_limited` evidence.
- Network and Filesystems stay unavailable until collected.
- Local executable Rust checks on macOS use `--target aarch64-apple-darwin`.

---

### Task 1: Lock the OS workspace contract in failing frontend tests

**Files:**

- Create: `web/src/components/OsWorkspace.test.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/TableView.test.tsx`
- Modify: `web/src/components/TemporalBucketRow.test.tsx`

**Interfaces:**

- Consumes: existing `ViewSpec`, `/v1/timeline/spine`,
  `/v1/timeline/heatmap`, and `/v1/frame/processes` fixtures.
- Produces: executable expectations for `OsWorkspace`, process matrix identity,
  error semantics and metric switching.

- [ ] **Step 1: Write the failing workspace test**

  Require one 24-bucket host-spine request, one 96-bucket process heatmap,
  visible host scope/quality evidence, and one row-coupled matrix:

  ```tsx
  expect(await screen.findByTestId("os-workspace")).toBeDefined();
  expect(screen.queryByTestId("infrastructure-analytical-center")).toBeNull();
  expect(screen.getAllByTestId("process-interval-row")[0]).toHaveAttribute(
    "data-mode",
    "process_intervals",
  );
  ```

- [ ] **Step 2: Write failure and scope tests**

  Assert a host-spine error has `role="alert"`, a process heatmap error is
  passed into row aria labels, and filtered frame/global heatmap counts use
  independent copy.

- [ ] **Step 3: Extend App and matrix tests**

  Require OS to own a dedicated workspace on desktop/mobile, no detached
  center, sticky PID/type identity and exact 96-bucket rows.

- [ ] **Step 4: Run focused tests and confirm RED**

  Run:

  ```bash
  fnm exec --using=22.14.0 npx vitest run \
    src/components/OsWorkspace.test.tsx \
    src/components/TableView.test.tsx \
    src/components/TemporalBucketRow.test.tsx \
    src/App.test.tsx
  ```

  Expected: fail because `OsWorkspace`, `process_intervals` and the dedicated
  App route do not exist.

- [ ] **Step 5: Commit the red contract**

  ```bash
  git add web/src/components/OsWorkspace.test.tsx web/src/App.test.tsx \
    web/src/components/TableView.test.tsx \
    web/src/components/TemporalBucketRow.test.tsx
  git commit -m "test(web): lock OS visual convergence"
  ```

### Task 2: Generalize the temporal matrix for process evidence

**Files:**

- Modify: `web/src/components/TableView.tsx`
- Modify: `web/src/components/TemporalBucketRow.tsx`
- Modify: `web/src/components/TableView.test.tsx`
- Modify: `web/src/components/TemporalBucketRow.test.tsx`

**Interfaces:**

- Consumes: `TimeMatrixColumn` and `HeatmapResponse`.
- Produces: `kind: "processes"` and
  `evidenceMode: "process_intervals"` presentation contracts.

- [ ] **Step 1: Add the process discriminators**

  Extend the existing unions without changing Statements, Activity or Plans:

  ```ts
  kind?: "statements" | "activity" | "plans" | "processes";
  evidenceMode?:
    | "point_samples"
    | "interval_estimates"
    | "plan_intervals"
    | "process_intervals";
  ```

- [ ] **Step 2: Add sticky process identity**

  Group `pid` and `type` into the sticky identity cell. Keep all other preset
  columns sortable and use the server entity string as the row key.

- [ ] **Step 3: Add process interval semantics**

  Add process-specific header, row, bucket, unavailable and load-error labels.
  Pass request failure into every null row aria label.

- [ ] **Step 4: Run focused matrix tests**

  Run:

  ```bash
  fnm exec --using=22.14.0 npx vitest run \
    src/components/TableView.test.tsx \
    src/components/TemporalBucketRow.test.tsx
  ```

  Expected: all focused matrix tests pass.

- [ ] **Step 5: Commit the matrix support**

  ```bash
  git add web/src/components/TableView.tsx \
    web/src/components/TemporalBucketRow.tsx \
    web/src/components/TableView.test.tsx \
    web/src/components/TemporalBucketRow.test.tsx
  git commit -m "feat(web): add process temporal matrix"
  ```

### Task 3: Build OsWorkspace and honest host evidence

**Files:**

- Create: `web/src/components/OsWorkspace.tsx`
- Create: `web/src/components/OsWorkspace.css`
- Modify: `web/src/components/OsWorkspace.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/main.tsx`

**Interfaces:**

- Consumes: `useTimelineSpine`, `useHeatmap`, `TableView`, `ContextResponse`,
  selected preset/metric/filter/sort state.
- Produces: `OsWorkspace(props: OsWorkspaceProps)` and
  `osMetricForPreset(preset: string | null): "cpu" | "io"`.

- [ ] **Step 1: Implement the metric mapping**

  ```ts
  export function osMetricForPreset(preset: string | null): "cpu" | "io" {
    return preset === "disk_io" ? "io" : "cpu";
  }
  ```

  Test Pressure/CPU/Memory/Cgroup/Processes/Data quality → `cpu` and
  Storage I/O → `io`.

- [ ] **Step 2: Fetch bounded host and process evidence**

  Request spine with 24 buckets and heatmap with 96 desktop / 48 mobile
  buckets, `top: 64`, the exact selected range and metric.

- [ ] **Step 3: Render the host evidence rail**

  Render latest load, maximum PSI, 24-bucket compact strips, host identity,
  scope guard and complete quality counts. Keep loading/error/empty states
  distinct and add an explicit Retry action for spine failure.

- [ ] **Step 4: Render the process matrix**

  Feed `TableView` with `kind: "processes"`,
  `evidenceMode: "process_intervals"`, the exact CPU/I/O label, independent
  filtered/global population copy and existing row/detail callbacks.

- [ ] **Step 5: Add EN/RU copy and CSS**

  Use existing tokens only. Keep the rail compact, matrix flex-owned, sticky
  identity readable and reduced motion respected.

- [ ] **Step 6: Run workspace tests**

  Run:

  ```bash
  fnm exec --using=22.14.0 npx vitest run \
    src/components/OsWorkspace.test.tsx \
    src/components/TableView.test.tsx \
    src/components/TemporalBucketRow.test.tsx
  ```

  Expected: all OS and shared-matrix tests pass.

- [ ] **Step 7: Commit the workspace**

  ```bash
  git add web/src/components/OsWorkspace.tsx \
    web/src/components/OsWorkspace.css \
    web/src/components/OsWorkspace.test.tsx \
    web/src/i18n/en.json web/src/i18n/ru.json web/src/main.tsx
  git commit -m "feat(web): build dense OS investigation workspace"
  ```

### Task 4: Integrate OS and retire its detached panel

**Files:**

- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/InfrastructureEvidencePanel.tsx`
- Modify: `web/src/components/InfrastructureEvidencePanel.test.tsx`

**Interfaces:**

- Consumes: existing URL state, prepared OS lenses and Entity Detail callback.
- Produces: dedicated desktop/mobile OS routing with no detached analytical
  center; InfrastructureEvidencePanel remains for Tables/Indexes/Vacuum.

- [ ] **Step 1: Route OS to OsWorkspace**

  Exclude `processesScreen` from `infrastructureEvidenceScreen` and render
  `OsWorkspace` in both desktop and compact branches.

- [ ] **Step 2: Synchronize lens default metrics**

  Compute the OS default metric from `osMetricForPreset(effectivePreset)` while
  retaining per-lens explicit overrides in `metricByContext`.

- [ ] **Step 3: Remove only obsolete HostEvidence code**

  Delete the process branch and spine imports from
  `InfrastructureEvidencePanel`; preserve Tables, Indexes and Vacuum behavior.

- [ ] **Step 4: Run App and infrastructure tests**

  Run:

  ```bash
  fnm exec --using=22.14.0 npx vitest run \
    src/App.test.tsx \
    src/components/InfrastructureEvidencePanel.test.tsx \
    src/components/OsWorkspace.test.tsx
  ```

  Expected: dedicated OS workspace passes and the three remaining
  infrastructure panels retain their contracts.

- [ ] **Step 5: Commit integration**

  ```bash
  git add web/src/App.tsx web/src/App.test.tsx \
    web/src/components/InfrastructureEvidencePanel.tsx \
    web/src/components/InfrastructureEvidencePanel.test.tsx
  git commit -m "refactor(web): route OS through its evidence workspace"
  ```

### Task 5: Make the deterministic demo prove density and interaction

**Files:**

- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/verify-shell.mjs`

**Interfaces:**

- Consumes: existing deterministic process catalog, frame, spine, heatmap and
  entity-detail routes.
- Produces: a stable 1920×1080 browser contract for OS Pressure and Storage I/O.

- [ ] **Step 1: Expand process demo evidence**

  Keep at least 218 matched process entities, representative CPU/RSS/read/write
  values, cgroup gaps and a `resource_limited` quality marker. Keep response
  pages bounded at 200 rows.

- [ ] **Step 2: Replace the old OS analytical-center assertions**

  Require root height 1080, Health Line 60, no OS analytical center, the host
  rail inside the viewport, at least 18 visible process rows, 96 buckets per
  rendered row and independent matrix overflow.

- [ ] **Step 3: Exercise core OS controls**

  Verify Pressure→Storage I/O selects metric `io`, CPU/I/O buttons update the
  heatmap request, filtering uses independent population copy, Network and
  Filesystems remain gated, and row selection opens process Entity Detail.

- [ ] **Step 4: Run browser proof**

  Run:

  ```bash
  fnm exec --using=22.14.0 npm run verify:shell
  ```

  Expected: forensic shell PASS and fresh OS 1920×1080 screenshot.

- [ ] **Step 5: Commit demo verification**

  ```bash
  git add web/scripts/demo-stub.mjs web/scripts/verify-shell.mjs
  git commit -m "test(web): verify dense OS evidence"
  ```

### Task 6: Verify, visually compare and ship

**Files:**

- Modify: `design-qa.md`
- Modify: `bins/pg_kronika-web/static.tar.gz`

**Interfaces:**

- Consumes: completed OS workspace and approved reference screenshot.
- Produces: reviewed PR12, deterministic UI archive and green main merge.

- [ ] **Step 1: Run frontend verification**

  Run typecheck, ESLint, Prettier, design-token check, all 308+ Vitest tests,
  production audit, build and bundle budget under Node 22.

- [ ] **Step 2: Run repository verification**

  Run Rust formatting, clippy with warnings denied, full workspace tests,
  OpenAPI export/lint, frontend codegen, dependency rules, BDD image and demo
  API smoke.

- [ ] **Step 3: Compare source and implementation together**

  Open `pgkronika-os-pressure-refined.png` and the fresh OS screenshot in one
  same-input comparison at original 1920×1080 resolution. Fix every P0/P1/P2
  mismatch and repeat browser proof.

- [ ] **Step 4: Record QA and package assets**

  Update `design-qa.md` only after the comparison passes. Run
  `fnm exec --using=22.14.0 make web-frontend`, verify archive contents and the
  262,144-byte bundle budget.

- [ ] **Step 5: Request independent review**

  Review the complete range against the design spec, process semantics,
  4,096-row bound, accessibility and 1920×1080 acceptance. Resolve all P0/P1/P2.

- [ ] **Step 6: Push, create PR12 and merge green**

  Push `codex/pr12-os-visual-convergence`, create a ready PR against `main`,
  wait for every required GitHub job, squash-merge only when green, and verify
  the merge commit is the exact `origin/main` head.
