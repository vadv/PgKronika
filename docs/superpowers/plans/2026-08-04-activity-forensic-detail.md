# Activity Forensic Detail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the approved full-canvas Activity forensic detail with aligned PostgreSQL/OS history and bounded investigative continuations.

**Architecture:** Add a focused `ActivityDetail` component that owns one Activity point request and one capped entity-history request. Route selected desktop Activity rows to it from `App`, keep `ActivityWorkspace` mounted but hidden, and reuse the existing URL patching/navigation contracts for process, Statements, and waits continuations.

**Tech Stack:** React 19, TypeScript, TanStack Query, react-i18next, Vitest/Testing Library, CSS design tokens, Puppeteer shell verifier.

## Global Constraints

- 1920×1080 root with no root scroll; detail occupies y=136…1056.
- Keep the 60 px Health Line and 24 px status bar.
- History is capped at 96 snapshots, eight columns, and six hours.
- Relations are investigative links, not proof or causal claims.
- No raw entity tokens, endpoint paths, provenance methods, gap/gated counters, or proof copy in the primary detail.
- Mobile keeps the existing generic dock.

---

### Task 1: Lock the Activity detail contract

**Files:**
- Create: `web/src/components/ActivityDetail.test.tsx`
- Modify: `web/src/App.test.tsx`

**Interfaces:**
- Consumes: `ActivityDetailProps`, Activity `ViewSpec`, point/history API fixtures.
- Produces: red tests for `data-testid="activity-detail"`, four temporal lanes, three analysis columns, bounded history, continuation callbacks, and retained overview routing.

- [ ] **Step 1: Write the component tests** with an Activity point containing PID/database/user/application/state/wait/query/queryid plus two related process candidates, and a history response containing categorical state/wait plus numeric duration/resource rows.
- [ ] **Step 2: Assert the desired behavior**: four lanes, local null cells, every process card, Statements and waits callbacks, no raw token/provenance/proof copy, and `limit=96` with the exact eight history columns.
- [ ] **Step 3: Write the App routing test**: desktop Activity `dock=row` mounts the full-canvas detail, suppresses the generic dock, hides-but-preserves the Activity overview, and closes with Escape.
- [ ] **Step 4: Run** `npm test -- src/components/ActivityDetail.test.tsx src/App.test.tsx` and confirm failure because `ActivityDetail` and the inline route do not exist.

### Task 2: Implement the bounded Activity forensic canvas

**Files:**
- Create: `web/src/components/ActivityDetail.tsx`
- Create: `web/src/components/ActivityDetail.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: `useEntityPoint`, `useEntityHistory`, `ViewSpec`, `onClose`, `onOpenEntity`, `onFindStatements`, `onOpenWaits`.
- Produces: `ActivityDetail(props: ActivityDetailProps)`.

- [ ] **Step 1: Add the component shell** with the compact identity/state strip, local point loading/error states, and close control.
- [ ] **Step 2: Add the shared temporal geometry**: categorical Activity observation cells and three dual-series normalized SVG lanes using the capped history response.
- [ ] **Step 3: Add the three analysis columns** for PostgreSQL observation, current/first OS matrix plus command, and investigation continuations.
- [ ] **Step 4: Add EN/RU copy** using operator language: observed, not observed, continue investigation, related process, find in Statements, open waits & locks.
- [ ] **Step 5: Run** `npm test -- src/components/ActivityDetail.test.tsx` and confirm the component tests pass.

### Task 3: Route selected desktop Activity into the detail

**Files:**
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`

**Interfaces:**
- Consumes: existing `patch(Partial<UiState>)`, `inlineEntityDetail`, `ActivityWorkspace` props.
- Produces: `inlineActivityDetail` and retained `activity-overview-preserved` wrapper.

- [ ] **Step 1: Mount `ActivityDetail`** immediately below `HealthLine` when desktop Activity has `dock=row` and an entity.
- [ ] **Step 2: Wire continuations**: returned process entity/snapshot, Statements `queryid=<value>` filter, and Activity `waits_locks` `pid=<value>` filter.
- [ ] **Step 3: Preserve the overview** by keeping `ActivityWorkspace` mounted inside a wrapper that becomes `display:none` while detail is visible.
- [ ] **Step 4: Run** `npm test -- src/components/ActivityDetail.test.tsx src/App.test.tsx` and confirm all route and component tests pass.

### Task 4: Add deterministic demo and 1920×1080 verification

**Files:**
- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/verify-shell.mjs`

**Interfaces:**
- Consumes: Activity entity point/history demo routes and current shell verifier.
- Produces: `web/demo/shots/forensic-activity-detail-1920x1080.png` and machine assertions for geometry, data bounds, navigation, forbidden copy, and overview restoration.

- [ ] **Step 1: Shape Activity history** so categorical observations and resource traces visibly change without inventing a process relation the point response did not return.
- [ ] **Step 2: Verify exact geometry**: root 1920×1080, `scrollY=0`, detail y=136…1056, four lanes, three analysis columns, no generic dock, no visible overview.
- [ ] **Step 3: Verify request bounds and actions**: 96 samples, eight columns, six-hour cap, process open, Statements query filter, waits PID filter, close restores overview.
- [ ] **Step 4: Run** `npm run verify:shell` and inspect the generated Activity detail screenshot against the approved reference at the same viewport.

### Task 5: Ship PR207 green

**Files:**
- Create: `docs/superpowers/specs/2026-08-04-activity-detail-design-qa.md`
- Modify: `bins/pg_kronika-web/static.tar.gz`

**Interfaces:**
- Consumes: approved reference screenshot, prototype screenshot, test/CI output.
- Produces: QA record, deterministic production bundle, PR207.

- [ ] **Step 1: Run the single Impeccable detector pass** on changed Activity UI targets and resolve mechanical findings.
- [ ] **Step 2: Run frontend gates**: design tokens, typecheck/build, lint, Prettier, full Vitest coverage, production npm audit, and shell verifier.
- [ ] **Step 3: Rebuild and check the deterministic bundle** with `make web-frontend` and `make web-bundle-budget`.
- [ ] **Step 4: Run Rust/package gates**: `cargo fmt --all -- --check`, native `pg_kronika-web` tests, and clippy with warnings denied.
- [ ] **Step 5: Document and publish** the combined reference/prototype comparison and DBA/SRE/frontend/memory review, commit, push, open PR207, fix CI, and squash-merge only when every required check is green.
