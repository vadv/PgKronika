# PR11 Plans Visual Convergence — Implementation Plan

> **For Codex:** Execute this plan task by task with TDD and verification
> checkpoints. Do not manufacture a baseline regression or A/B tree diff from
> the current single-plan evidence.

**Goal:** Ship the approved Plans investigation experience as one dense
1920×1080 workspace with row-coupled plan time/calls evidence, bounded observed
change records and explicit collector-fork attribution.

**Architecture:** Add a dedicated `PlansWorkspace` that owns temporal data and
change-timeline evidence, generalize the existing virtualized temporal matrix
for plan identity, preserve lazy universal Entity Detail for plan text, and
remove Plans from the shared detached analytical center.

**Stack:** React 19, TypeScript, TanStack Query, Vitest/Testing Library, existing
Rust Axum UI contracts, Puppeteer shell verifier.

---

## Task 1: Lock the Plans workspace contract in failing frontend tests

**Files:**

- Create: `web/src/components/PlansWorkspace.test.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/TableView.test.tsx`
- Modify: `web/src/components/TemporalBucketRow.test.tsx`

1. Add a component test requiring a 96-bucket Plans request, truthful
   regression-evidence copy, both fork attribution labels and a row-coupled
   plan matrix.
2. Add Changes coverage requiring no more than three first→last-observed
   records and non-continuous observation language.
3. Change the App test to require a dedicated Plans workspace and no detached
   Plans analytical center.
4. Add matrix tests for plan identity, time/calls semantics and 96 buckets.
5. Run the focused Vitest files and record the expected RED result.

## Task 2: Generalize the row-coupled temporal matrix for Plans

**Files:**

- Modify: `web/src/components/TableView.tsx`
- Modify: `web/src/components/TemporalBucketRow.tsx`
- Modify: `web/src/components/StatementsTimeMatrix.css`
- Modify: `web/src/components/TableView.test.tsx`
- Modify: `web/src/components/TemporalBucketRow.test.tsx`

1. Extend the temporal-matrix presentation discriminator with `plans`.
2. Keep Statements and Activity behavior and selectors unchanged.
3. Add sticky `planid + queryid` identity and Plans-specific evidence labels.
4. Render exact entity-series time/calls buckets without a continuous line.
5. Run the focused component tests until green.

## Task 3: Build PlansWorkspace and bounded change evidence

**Files:**

- Create: `web/src/components/PlansWorkspace.tsx`
- Create: `web/src/components/PlansWorkspace.css`
- Modify: `web/src/main.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/components/PlansWorkspace.test.tsx`

1. Fetch the 96-bucket Plans heatmap with the selected time/calls metric.
2. Build the compact lens, attribution and temporal-quality evidence strip.
3. Fetch the existing `change_timeline` frame only for Regression evidence or
   Changes and render at most three observed envelopes.
4. Feed the generalized plan time matrix and keep loading/error/empty states
   honest.
5. Run PlansWorkspace and accessibility-focused tests until green.

## Task 4: Integrate truthful Plans lenses and remove detached evidence

**Files:**

- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/WorkloadEvidencePanel.tsx`
- Modify: `web/src/components/WorkloadEvidencePanel.test.tsx`

1. Add Regression evidence as the first available Plans lens, backed by the
   existing `regression` preset and explicitly bounded copy.
2. Route Plans to `PlansWorkspace` on desktop and mobile.
3. Exclude Plans from the shared detached analytical center.
4. Remove only the obsolete Plans branch from `WorkloadEvidencePanel`.
5. Preserve the visible gated Compare control and its reason.
6. Run App and workload tests until green.

## Task 5: Make the deterministic demo representative

**Files:**

- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/catalog.fixture.json`
- Modify: `web/scripts/verify-shell.mjs`

1. Populate representative plan ids, query ids, calls, mean time, rows,
   buffers, first/last observed calls and partial attribution cases.
2. Update the verifier to require no root scroll, no detached Plans center, at
   least 18 visible rows, 96 buckets per temporal row, metric/lens switching,
   bounded change records and gated Compare.
3. Capture Regression evidence, Buffers and Changes at 1920×1080.

## Task 6: Verify, visually compare and ship

**Files:**

- Modify: `design-qa.md`

1. Run Prettier, ESLint, TypeScript, frontend coverage and relevant Rust tests.
2. Run full repository checks required by PR CI, including clippy, dependency
   rules, shell verifier and bundle budget.
3. Open the reference and prototype screenshots together at the same viewport;
   fix every P0/P1/P2 mismatch and repeat the capture.
4. Verify the rendered app in the browser, including Regression→Buffers→Changes
   switching, time/calls switching, row selection and Plan Detail.
5. Update `design-qa.md` to `final result: passed` only after comparison.
6. Commit, push, open PR11, wait for green CI and merge to `main`.
