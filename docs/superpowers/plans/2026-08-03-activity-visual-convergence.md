# PR10 Activity Visual Convergence — Implementation Plan

> **For Codex:** Execute this plan task by task with TDD and verification
> checkpoints. Do not weaken Activity→Process relation semantics to achieve the
> layout.

**Goal:** Ship the approved Activity Overview, CPU and Waits & Locks experience
as one dense 1920×1080 forensic workspace with row-coupled observed samples and
conservative OS-process enrichment.

**Architecture:** Add a dedicated `ActivityWorkspace` that owns temporal data
and bounded lock evidence, generalize the existing virtualized temporal matrix
without changing Statements semantics, and publish only the extra Activity
columns that can be projected through the existing unique same-snapshot process
candidate.

**Stack:** React 19, TypeScript, TanStack Query, Vitest/Testing Library, Rust
Axum projection/catalog code, Puppeteer shell verifier.

---

## Task 1: Lock the Activity workspace contract in failing frontend tests

**Files:**

- Create: `web/src/components/ActivityWorkspace.test.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/TemporalBucketRow.test.tsx`

1. Add a component test requiring point-snapshot copy, 96-bucket Activity
   heatmap request, process-link provenance, and a row-coupled point-sample
   matrix.
2. Add a Waits & Locks case requiring the bounded lock strip and edge-only
   provenance.
3. Change the App test to require no detached Activity analytical center and a
   dedicated Activity workspace.
4. Add a TemporalBucketRow test proving point-sample semantics and no continuous
   line.
5. Run the focused Vitest files and record the expected RED result.

## Task 2: Publish combined Activity row evidence through the server contract

**Files:**

- Modify: `bins/pg_kronika-web/src/ui/catalog.rs`
- Modify: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Modify: `bins/pg_kronika-web/src/tests/ui_frame.rs`

1. Add failing catalog tests for nullable `queryid`, process-derived `rss`,
   `threads`, `command`, and the enriched presets.
2. Add failing projection tests proving one unique process candidate populates
   these values while ambiguous and absent candidates leave them null.
3. Implement the catalog columns and projection using the existing
   `activity_process_candidate` only.
4. Run the focused Rust tests on `aarch64-apple-darwin` and make them green.

## Task 3: Generalize the row-coupled temporal matrix

**Files:**

- Modify: `web/src/components/TableView.tsx`
- Modify: `web/src/components/TemporalBucketRow.tsx`
- Modify: `web/src/components/StatementsTimeMatrix.css`
- Modify: `web/src/components/TableView.test.tsx`
- Modify: `web/src/components/TemporalBucketRow.test.tsx`

1. Introduce a discriminated temporal-matrix presentation for Statements and
   Activity: identity column codes, labels, test ids, row height and evidence
   mode.
2. Preserve all Statements tests and CSS selectors.
3. Add Activity grouping and sticky `PID + database/user/application` identity.
4. Render point-sample buckets with visible gaps and Activity-specific tooltip
   copy.
5. Run the focused component tests until green.

## Task 4: Build ActivityWorkspace and Waits & Locks evidence

**Files:**

- Create: `web/src/components/ActivityWorkspace.tsx`
- Create: `web/src/components/ActivityWorkspace.css`
- Modify: `web/src/main.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/components/ActivityWorkspace.test.tsx`

1. Fetch the 96-bucket Activity heatmap with the selected Activity metric.
2. Build a compact evidence strip for snapshot semantics, temporal quality and
   unique-PID relation provenance.
3. Build a bounded Waits & Locks strip from the Locks tree frame only when that
   lens is active.
4. Feed the generalized time matrix and keep loading/error/empty states honest.
5. Run ActivityWorkspace and accessibility-focused tests until green.

## Task 5: Integrate the workspace and remove the detached Activity panel

**Files:**

- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/WorkloadEvidencePanel.tsx`
- Modify: `web/src/components/WorkloadEvidencePanel.test.tsx`

1. Route Activity to `ActivityWorkspace` on desktop and mobile.
2. Exclude Activity from the shared detached analytical center; leave Plans and
   later workspaces unchanged.
3. Remove only the obsolete Activity branch from WorkloadEvidencePanel.
4. Keep URL lens/metric/search/sort/selection behavior intact.
5. Run App and workload tests until green.

## Task 6: Make the deterministic demo representative

**Files:**

- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/catalog.fixture.json`
- Modify: `web/scripts/verify-shell.mjs`

1. Populate representative Activity query ids, relation quality, CPU, RSS,
   threads, I/O and commands, including missing/ambiguous evidence cases.
2. Update the verifier to require no root scroll, no detached Activity center,
   at least 18 visible rows, 96 buckets per temporal row, lens switching,
   bounded lock edges and Process Detail provenance.
3. Capture Overview, CPU and Waits & Locks at 1920×1080.

## Task 7: Verify, visually compare and ship

**Files:**

- Modify: `design-qa.md`

1. Run Prettier, ESLint, TypeScript, frontend coverage and focused Rust tests.
2. Run full repository checks required by PR CI, including clippy, dependency
   rules, shell verifier and bundle budget.
3. Open the reference and prototype screenshots together at the same viewport;
   fix every P0/P1/P2 mismatch and repeat the capture.
4. Verify the rendered app in the in-app browser, including Overview→CPU→Waits
   switching, row selection and Process Detail.
5. Update `design-qa.md` to `final result: passed` only after the comparison.
6. Commit, push, open PR10, wait for green CI and merge to `main`.
