# Data Maintenance Forensic Detail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a reference-faithful inline forensic detail for Tables, Indexes, and Vacuum while preserving the dense heatmap population browser.

**Architecture:** A focused `DataMaintenanceDetail` component owns bounded entity point/history queries and renders view-specific temporal and analytical regions. `App.tsx` chooses that inline workspace only for desktop data-maintenance row detail and leaves the universal dock unchanged for all other views.

**Tech Stack:** React 19, TypeScript, TanStack Query, Vitest, Testing Library, existing PgKronika design tokens and entity APIs.

## Global Constraints

- 1920×1080 is the baseline and must have no root-page scroll.
- The 60 px Health Line remains visible above the detail.
- History is limited to six columns, 96 snapshots, and at most six hours.
- Missing data is local and calm; do not show API tokens, gap/gated counters, or proof language in the primary detail.
- Use only existing design tokens, the 4 px spacing rhythm, and 4–6 px radii.
- Every production behavior starts with a failing test.

---

### Task 1: Define the data-maintenance detail contract

**Files:**
- Create: `web/src/components/DataMaintenanceDetail.test.tsx`
- Create: `web/src/components/DataMaintenanceDetail.tsx`

**Interfaces:**
- Consumes: `ViewSpec`, `useEntityPoint`, `useEntityHistory`, and route callbacks.
- Produces: `DataMaintenanceDetail(props)` with `view`, `entity`, `at`, `span`, `onClose`, and `onOpenEntity`.

- [ ] **Step 1: Write the failing structure and query-bounds tests**

Assert that a selected table renders `data-testid="data-maintenance-detail"`,
`data-testid="maintenance-temporal-field"`, all three analytical columns, and
that the history URL requests no more than six columns, `limit=96`, and a
six-hour-or-shorter window.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `npm test -- --run src/components/DataMaintenanceDetail.test.tsx`

Expected: FAIL because `DataMaintenanceDetail` does not exist.

- [ ] **Step 3: Implement bounded point/history loading and semantic grouping**

Use these public props:

```ts
export interface DataMaintenanceDetailProps {
  view: ViewSpec;
  entity: string;
  at: string;
  span: number;
  onClose: () => void;
  onOpenEntity: (view: string, entity: string, at: string) => void;
}
```

Choose up to six available non-lazy columns from the view-specific priority
list and call `useEntityHistory` with `limit: 96`.

- [ ] **Step 4: Run the focused test and verify GREEN**

Run: `npm test -- --run src/components/DataMaintenanceDetail.test.tsx`

Expected: PASS.

### Task 2: Recreate the approved temporal and three-column composition

**Files:**
- Modify: `web/src/components/DataMaintenanceDetail.test.tsx`
- Modify: `web/src/components/DataMaintenanceDetail.tsx`
- Create: `web/src/components/DataMaintenanceDetail.css`

**Interfaces:**
- Consumes: the bounded history response established in Task 1.
- Produces: labelled temporal lanes and view-specific analytical panels.

- [ ] **Step 1: Write failing visual-contract tests**

Assert compact entity metadata, latest values beside each lane, an event lane
for related entities, view-specific group headings, local missing text, and no
visible `/v1/`, technical entity token, `gaps`, `gated`, `proof`, or
`provenance` text.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `npm test -- --run src/components/DataMaintenanceDetail.test.tsx`

Expected: FAIL on the missing visual regions.

- [ ] **Step 3: Implement the reference composition**

Render one SVG data trace per numeric history column, a shared cursor, relation
markers, and three ruled lower columns. Keep latest numeric values as text and
use the existing formatting helpers for units and timestamps.

- [ ] **Step 4: Run the focused test and verify GREEN**

Run: `npm test -- --run src/components/DataMaintenanceDetail.test.tsx`

Expected: PASS.

### Task 3: Route infrastructure detail inline

**Files:**
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`

**Interfaces:**
- Consumes: `DataMaintenanceDetailProps` from Task 1.
- Produces: desktop inline detail when `view` is Tables/Indexes/Vacuum and
  `dock=row&entity=…`; the generic overlay remains for every other case.

- [ ] **Step 1: Write failing route-ownership tests**

Assert that an infrastructure entity URL renders the inline detail, suppresses
the generic row overlay, and that closing patches only `dock: null`.

- [ ] **Step 2: Run the focused App test and verify RED**

Run: `npm test -- --run src/App.test.tsx`

Expected: FAIL because the generic overlay still owns the route.

- [ ] **Step 3: Implement the route decision**

Derive `inlineInfrastructureDetail` in `Shell`, suppress `DockOverlay` for that
case, and replace the normal screen-context/analytical-center/table body with
`DataMaintenanceDetail` immediately below `HealthLine`.

- [ ] **Step 4: Run the focused App test and verify GREEN**

Run: `npm test -- --run src/App.test.tsx`

Expected: PASS.

### Task 4: Complete bilingual copy and compact adaptation

**Files:**
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/components/DataMaintenanceDetail.test.tsx`
- Modify: `web/src/components/DataMaintenanceDetail.css`

**Interfaces:**
- Consumes: semantic regions from Task 2.
- Produces: English/Russian parity and bounded 1440×900 behavior.

- [ ] **Step 1: Add failing copy/parity assertions**

Assert that every visible label resolves in both locales and that missing data
uses the local human label rather than technical quality terminology.

- [ ] **Step 2: Run parity and focused tests and verify RED**

Run: `npm test -- --run src/i18n/parity.test.ts src/components/DataMaintenanceDetail.test.tsx`

Expected: FAIL on missing translation keys.

- [ ] **Step 3: Add exact copy and compact CSS**

Add the `maintenanceDetail.*` key family to both locale files and a compact
media/container treatment that preserves the temporal field and makes the
lower analysis region internally scrollable.

- [ ] **Step 4: Run parity and focused tests and verify GREEN**

Run: `npm test -- --run src/i18n/parity.test.ts src/components/DataMaintenanceDetail.test.tsx`

Expected: PASS.

### Task 5: Verify, package, visually compare, and review

**Files:**
- Modify: `bins/pg_kronika-web/static.tar.gz`
- Modify: `design-qa.md`

**Interfaces:**
- Consumes: the complete PR205 implementation.
- Produces: green committed assets and a passed reference comparison.

- [ ] **Step 1: Run the full frontend gate and bundle budget**

Run: `make web-frontend-check && make web-bundle-budget`

Expected: all tests pass and bundle remains within 256 KiB.

- [ ] **Step 2: Build the committed frontend archive**

Run: `make web-frontend`

Expected: `bins/pg_kronika-web/static.tar.gz` matches the fresh build.

- [ ] **Step 3: Run shell geometry and design-token verification**

Run: `cd web && npm run verify:shell`

Expected: exact 1920×1080 and compact desktop have no root scroll.

- [ ] **Step 4: Run one mechanical design scan**

Run: `node /Users/vadv/.agents/skills/impeccable/scripts/detect.mjs --json web/src/components/DataMaintenanceDetail.tsx web/src/components/DataMaintenanceDetail.css web/src/App.tsx`

Expected: no unexplained findings.

- [ ] **Step 5: Capture and compare the selected-table state**

Capture the approved reference and local selected-table implementation at the
same viewport, place both in one comparison input, fix all P0/P1/P2 findings,
and save `design-qa.md` with `final result: passed`.

- [ ] **Step 6: Run the mandatory review panel**

Review DevOps packaging, DBA load, SRE failure behavior, PostgreSQL semantics,
Rust/frontend performance, memory bounds, and comment quality. Fix every high
severity finding.

- [ ] **Step 7: Commit and open the PR**

```bash
git add docs/superpowers web/src bins/pg_kronika-web/static.tar.gz design-qa.md
git commit -m "feat(web): build data maintenance forensic detail"
git push -u origin codex/pr205-data-maintenance-fidelity
gh pr create --base main --head codex/pr205-data-maintenance-fidelity
```

