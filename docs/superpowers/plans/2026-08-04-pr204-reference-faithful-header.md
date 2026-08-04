# PR204 reference-faithful forensic chrome — implementation plan

> Execute autonomously in `/Users/vadv/Projects/PgKronika-worktrees/review-pr204` and update the existing PR204 branch after verification.

**Goal:** Align PR204's Health score, grouped navigation, and global chips with the approved Superdesign reference while retaining its useful title and relationship-glyph improvements.

**Tech stack:** React 19, TypeScript 5.9, Vite 7, Vitest/Testing Library, tokenized inline styles, Rust-packaged static frontend.

## Task 1 — Lock visible behavior with failing tests

**Files**

- Modify: `web/src/components/Spine.test.tsx`
- Modify: `web/src/components/PrimaryNavigation.test.tsx`
- Modify: `web/src/components/Header.test.tsx`

1. Assert the available Health score reads `N/100`.
2. Assert group ordinals are inline, unbadged text while later groups retain separators.
3. Assert shared header controls use the compact reference radius.
4. Run the focused tests and confirm failures describe the current divergence.

## Task 2 — Implement the reference-faithful chrome

**Files**

- Modify: `web/src/components/Spine.tsx`
- Modify: `web/src/components/PrimaryNavigation.tsx`
- Modify: `web/src/design/ui.ts`

1. Render the score as compact `N/100`, preserving the current score state, delta, tooltip, and accessible label.
2. Remove the navigation ordinal circle and replace off-grid spacing with existing tokens.
3. Restore `var(--radius-sm)` and grid-aligned dense padding for shared chips and buttons.
4. Preserve `PageHeader` and the enlarged Activity relationship glyph from PR204.
5. Run focused tests and the design-token gate.

## Task 3 — Rebuild and qualify

**Files**

- Rebuild: `bins/pg_kronika-web/static.tar.gz`
- Add: final audit screenshots under ignored `output/audit-pr204/`

1. Run `make web-frontend` to refresh the packaged frontend.
2. Run `make web-frontend-check`, `npm --prefix web run verify:shell`, and `make web-bundle-budget`.
3. Capture implementation and reference at 1920×1080 in the chosen in-app browser, compare them together, and fix material mismatches.
4. Repeat the overflow check at 1440×900.

## Task 4 — Review, push, and merge

1. Run `git diff --check` and review the final scope.
2. Commit the approved correction, including the rebuilt bundle.
3. Request an independent code/design review; resolve Critical and Important findings.
4. Push to `claude/visual-density-pass`, wait for all PR204 checks, and fix any failure.
5. Merge PR204 when green and verify the new `main` state.

## Task 5 — Continue the next UX/UI stack

1. Start from the merged `main` in a new isolated worktree.
2. Audit Tables, Indexes, and Vacuum against the approved forensic system at 1920×1080.
3. Implement the next large visual slice around prepared lenses, dense evidence tables, and reusable Entity Detail before considering backend expansion.
