# Superdesign fidelity pass — implementation plan

> Execute autonomously on `codex/pr14-activity-detail-polish`. The published Superdesign reference is approved; checkpoints are verification checkpoints, not approval pauses.

**Goal:** Ship the full-bleed reference shell, a faithful joined Activity overview, and a reusable full-width Entity Detail while preserving PgKronika's inclusive linking semantics and temporal heatmaps.

**Tech stack:** React 19, TypeScript 5.9, Vite 7, TanStack Query, Vitest/Testing Library, custom CSS tokens, Rust/Axum typed UI API.

## Task 1 — Lock the visual baseline and shell geometry

**Files**

- Modify: `web/src/components/ShellLayout.tsx`
- Modify: `web/src/components/Header.tsx`
- Modify: `web/src/components/PrimaryNavigation.tsx`
- Modify: `web/src/components/Spine.tsx`
- Modify: `web/src/components/Spine.css`
- Modify: `web/src/design/tokens.css`
- Test: `web/src/components/ShellLayout.test.tsx`
- Test: `web/src/components/Header.test.tsx`
- Test: `web/src/components/PrimaryNavigation.test.tsx`
- Test: `web/src/components/HealthLine.test.tsx`

1. Add failing behavior tests for the full-bleed desktop Health region, compact reference-style context controls, grouped navigation, and preserved 44/32/60/24 viewport ownership.
2. Run the targeted tests and confirm they fail for the missing reference geometry.
3. Implement the shell/token/chrome changes without changing URL or API behavior.
4. Run targeted tests, typecheck, and lint.
5. Commit the shell slice.

## Task 2 — Build the joined Activity overview

**Files**

- Modify: `web/src/components/ActivityWorkspace.tsx`
- Modify: `web/src/components/ActivityWorkspace.css`
- Modify: `web/src/components/TableView.tsx`
- Modify: `web/src/components/Toolbar.tsx`
- Modify: `web/src/components/PageHeader.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.ts`
- Modify: `web/src/i18n/ru.ts`
- Test: `web/src/components/ActivityWorkspace.test.tsx`
- Test: `web/src/components/TableView.test.tsx`
- Test: `web/src/App.test.tsx`

1. Add failing tests for default Activity column groups, the narrow relation column, joined OS measurements, calm missing cells, state filters, and at least 18 visible rows at 1920×1080 fixture density.
2. Add a failing test that a same-PID relation remains navigable with partial/missing interval quality.
3. Implement the default snapshot table as a dedicated Activity presentation over the existing paginated frame data. Reuse current fetching/virtualization primitives where possible.
4. Preserve the existing 96-bucket matrix for temporal Activity lenses.
5. Add exact English and Russian operator copy without proof/confidence/gap jargon.
6. Run targeted tests, typecheck, lint, and the shell verifier.
7. Commit the Activity slice.

## Task 3 — Convert row detail into the reusable forensic workspace

**Files**

- Modify: `web/src/components/DockOverlay.tsx`
- Modify: `web/src/components/DockOverlay.css`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/en.ts`
- Modify: `web/src/i18n/ru.ts`
- Test: `web/src/components/DockOverlay.test.tsx`
- Test: `web/src/App.test.tsx`

1. Add failing tests for desktop row detail occupying the analytical workspace below Health while the incident dock remains narrow.
2. Add failing tests for grouped Activity/process Summary, irrelevant null-field suppression, human identity, Relationships navigation, and token isolation to Raw.
3. Implement a reusable field-group model driven by view and field codes.
4. Implement the reference-style identity strip, synchronized state lanes, three-column summary, and responsive fallback.
5. Preserve History pagination, relationship navigation, copy/deep-link behavior, and Raw payload.
6. Run targeted tests, typecheck, lint, and accessibility assertions.
7. Commit the detail slice.

## Task 4 — Align deterministic demo data and browser qualification

**Files**

- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/verify-shell.mjs`
- Modify: `web/scripts/demo-shot.mjs`
- Modify: `web/scripts/catalog.fixture.json` only through the repository generator if the public catalog changes
- Test: relevant script tests or executable verifier behavior

1. Extend the deterministic fixture only where needed to exercise the real Activity+process composition and grouped detail.
2. Add verifier assertions for full-bleed geometry, row count, independently scrolling Activity evidence, and the desktop detail workspace.
3. Run the stub and capture Activity overview and Activity detail at 1920×1080.
4. Compare each implementation image beside the corresponding published reference image; record defects in root `design-qa.md`.
5. Apply one batched material-fix pass and one confirmation capture.
6. Commit qualification changes.

## Task 5 — Mechanical design quality and full verification

1. Run Impeccable's detector once over all changed UI targets; fix mechanical findings in one batch.
2. Run `npm test -- --run`, `npm run typecheck`, `npm run lint`, `npm run format:check`, `npm run build`, and `npm run verify:shell` under `web/`.
3. Run repository frontend gates: `make web-frontend-check`, `make web-shell-check`, and `make web-bundle-budget`.
4. If backend changed, run the exact affected Rust tests first, then the complete backend test suite required by CI.
5. Rebuild `bins/pg_kronika-web/static.tar.gz` with the repository command and verify no AppleDouble entries.
6. Run the finish review against the reference and final screenshots; resolve material findings within the bounded two-round budget.

## Task 6 — PR and green merge

1. Review `git diff --check`, `git status`, commits, and changed-file scope.
2. Push `codex/pr14-activity-detail-polish` and open the next ready PR against `main` with the reference URL and exact verification evidence.
3. Wait for every required GitHub check. Diagnose and fix any failure without weakening tests or semantics.
4. Merge the green PR into `main` and verify the remote main commit/check state.
5. Keep the local preview running on `http://127.0.0.1:4173/` for handoff.
