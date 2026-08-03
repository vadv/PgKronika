# Statements Visual Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Statements the first production screen that visibly matches the approved forensic design: one dense ranked time matrix where identity, impact metrics, and a 96-bucket heatmap stay aligned row by row.

**Architecture:** Keep the existing bounded frame and heatmap APIs. A new Statements workspace fetches `top=64` heatmap rows and passes them into the existing virtualized table through an optional time-matrix column; only rendered frame rows receive bucket DOM, so a 1,000-row answer remains bounded. The Statements route drops its detached analytical-center heatmap, while every other route keeps its current evidence geometry.

**Tech Stack:** React 19, TypeScript, TanStack Query, Vitest/Testing Library, Vite, Puppeteer shell verifier, Rust UI catalog.

## Global Constraints

- The approved visual target is `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-statements-overview.png` at 1920×1080.
- 1920×1080 at DPR 1 and 100% zoom is the primary contract; the document root must not scroll.
- Statements keeps exactly 96 temporal buckets on desktop and a horizontally scrollable 48-bucket fallback on mobile.
- Heatmap remains raw visual evidence and must not be replaced by a correlation score or causal claim.
- Heatmap requests stay bounded to `top=64`; frame pagination and row virtualization remain active for approximately 1,000 statements.
- Query text remains server-capped, nullable, detail-only, excluded from bounded frame search, and absent from share state.
- A missing entity series is rendered as unavailable evidence, never as a zero-valued series.
- Existing relation quality, provenance, reset/gap, partial, loading, empty, and error semantics remain visible.
- No new frontend runtime dependency is added.
- Frontend commands run under Node 22; Rust checks use toolchain 1.96.0 and target `aarch64-apple-darwin` on this workstation.

---

### Task 1: Lightweight temporal row primitive

**Files:**
- Create: `web/src/components/TemporalBucketRow.tsx`
- Create: `web/src/components/TemporalBucketRow.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: `HeatmapRow`, `heatColor`, decimal-string microsecond timestamps.
- Produces: `bucketPosition(timeUs, fromUs, toUs, bucketCount): number | null` and `TemporalBucketRow`.

- [ ] **Step 1: Write failing unit tests for bucket alignment and missing evidence**

```tsx
expect(bucketPosition("150", "100", "200", 10)).toBe(5);
expect(bucketPosition("99", "100", "200", 10)).toBeNull();

render(
  <TemporalBucketRow
    row={null}
    bucketCount={96}
    gridFromUs="100"
    gridToUs="200"
    cursorUs="150"
    baselineUs={null}
    metricLabel="total time"
  />,
);
expect(screen.getAllByTestId("time-matrix-bucket")).toHaveLength(96);
expect(screen.getByTestId("temporal-row").dataset.evidence).toBe("unavailable");
expect(screen.getByTestId("time-matrix-cursor").style.left).toBe("50%");
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cd web && PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH npx vitest run src/components/TemporalBucketRow.test.tsx --coverage.enabled=false`

Expected: FAIL because `TemporalBucketRow.tsx` and `bucketPosition` do not exist.

- [ ] **Step 3: Implement exact bucket rendering with one row-level selection target**

```tsx
export function bucketPosition(
  timeUs: string,
  fromUs: string,
  toUs: string,
  bucketCount: number,
): number | null {
  const time = BigInt(timeUs);
  const from = BigInt(fromUs);
  const to = BigInt(toUs);
  if (bucketCount <= 0 || to <= from || time < from || time > to) return null;
  return Math.min(
    bucketCount - 1,
    Number(((time - from) * BigInt(bucketCount)) / (to - from)),
  );
}
```

Render exactly `bucketCount` non-focusable cells, preserve `null` cells, expose one localized row summary to assistive technology, use one overlay marker for cursor and baseline, and include bucket time/value in the cell tooltip without adding 96 keyboard stops.

- [ ] **Step 4: Add localized missing-series and bucket tooltip copy**

```json
"statements.matrix.seriesUnavailable": "series not retained for this statement",
"statements.matrix.bucketValue": "{{time}} · {{metric}}: {{value}}"
```

Add the Russian equivalents with identical interpolation variables.

- [ ] **Step 5: Run the focused test and verify GREEN**

Run: `cd web && PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH npx vitest run src/components/TemporalBucketRow.test.tsx --coverage.enabled=false`

Expected: PASS with no warnings.

- [ ] **Step 6: Commit the primitive**

```bash
git add web/src/components/TemporalBucketRow.tsx web/src/components/TemporalBucketRow.test.tsx web/src/i18n/en.json web/src/i18n/ru.json
git commit -m "feat(web): add temporal matrix row primitive"
```

### Task 2: Integrate heatmap evidence into virtualized frame rows

**Files:**
- Modify: `web/src/components/TableView.tsx`
- Modify: `web/src/components/TableView.test.tsx`
- Create: `web/src/components/StatementsTimeMatrix.css`
- Modify: `web/src/main.tsx`

**Interfaces:**
- Consumes: `HeatmapResponse` and the Task 1 primitive.
- Produces: optional `timeMatrix: TimeMatrixColumn | null` on `TableViewProps`.

```ts
export interface TimeMatrixColumn {
  data: HeatmapResponse | undefined;
  pending: boolean;
  error: boolean;
  metricLabel: string;
  cursorUs: string | null;
  baselineUs: string | null;
  onRetry: () => void;
}
```

- [ ] **Step 1: Write failing table tests for row coupling and bounded DOM**

Create a frame with 1,000 logical rows and 36 virtualized rendered rows plus a heatmap response containing two matching entities. Assert:

```tsx
expect(screen.getAllByTestId("temporal-row").length).toBeLessThanOrEqual(40);
expect(screen.getByRole("row", { name: /stmt-a/ }).querySelector('[data-evidence="available"]')).not.toBeNull();
expect(screen.getByRole("row", { name: /stmt-c/ }).querySelector('[data-evidence="unavailable"]')).not.toBeNull();
expect(document.querySelectorAll('[data-testid="time-matrix-bucket"]').length).toBeLessThanOrEqual(40 * 96);
```

Also assert that tables without `timeMatrix` retain the existing spark column.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cd web && PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH npx vitest run src/components/TableView.test.tsx --coverage.enabled=false`

Expected: FAIL because `TableViewProps.timeMatrix` is not defined and no temporal column is rendered.

- [ ] **Step 3: Add the optional time-matrix column**

Index `timeMatrix.data.rows` by exact `row.entity`. In time-matrix mode:

- combine `queryid`, `database`, and `user` into one 272 px sticky identity cell;
- keep the lens-owned numeric columns in their declared order;
- replace the spark column with one flexible temporal column;
- use a 34 px row height in virtualization calculations;
- render missing entity series as unavailable evidence;
- keep selection, arrow-key row navigation, pagination, loading, retry, and cursor-expiry behavior unchanged.

- [ ] **Step 4: Add the Statements matrix layout styles**

```css
.statements-time-matrix {
  table-layout: fixed;
  min-width: 1420px;
}

.statements-time-matrix__identity {
  width: 272px;
}

.statements-time-matrix__timeline {
  width: 50%;
  min-width: 620px;
}
```

Use the existing restrained dark palette, 4 px rhythm, semantic colors, and focus ring. Do not add gradients, glass effects, decorative cards, or shadow stacks.

- [ ] **Step 5: Run table tests and verify GREEN**

Run: `cd web && PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH npx vitest run src/components/TableView.test.tsx src/components/TemporalBucketRow.test.tsx --coverage.enabled=false`

Expected: PASS with bounded rendered rows and bucket cells.

- [ ] **Step 6: Commit the integrated matrix**

```bash
git add web/src/components/TableView.tsx web/src/components/TableView.test.tsx web/src/components/StatementsTimeMatrix.css web/src/main.tsx
git commit -m "feat(web): align statement rows with temporal evidence"
```

### Task 3: Build the Statements workspace around the unified matrix

**Files:**
- Create: `web/src/components/StatementsWorkspace.tsx`
- Create: `web/src/components/StatementsWorkspace.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: `useHeatmap`, `TableView`, shared time geometry, selected metric, and existing frame props.
- Produces: `StatementsWorkspace`, a Statements-only route composition.

- [ ] **Step 1: Write failing workspace tests for the approved geometry**

```tsx
expect(screen.getByTestId("statements-time-matrix")).toBeDefined();
expect(screen.queryByTestId("statements-detached-heatmap")).toBeNull();
expect(new URL(heatmapCall).searchParams.get("buckets")).toBe("96");
expect(new URL(heatmapCall).searchParams.get("top")).toBe("64");
expect(screen.getByRole("button", { name: /calls/i })).toBeDefined();
```

Verify metric changes request the matching heatmap metric without changing the active frame lens, and verify loading/error/empty heatmap states leave the ranked frame usable.

- [ ] **Step 2: Run focused workspace/App tests and verify RED**

Run: `cd web && PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH npx vitest run src/components/StatementsWorkspace.test.tsx src/App.test.tsx --coverage.enabled=false`

Expected: FAIL because the Statements route still renders a detached `HeatmapStrip`.

- [ ] **Step 3: Implement the Statements workspace**

`StatementsWorkspace` calls:

```ts
useHeatmap({
  view: "statements",
  metric,
  from,
  to,
  buckets: 96,
  top: 64,
});
```

Render one compact matrix control bar with active lens summary, metric buttons, retained/matched counts, quality/provenance access, and the shared cursor time. Pass the response into `TableView.timeMatrix`.

- [ ] **Step 4: Route Statements through the new composition**

In `App.tsx`, exclude Statements from the detached analytical-center block and render `StatementsWorkspace` in place of the generic `TableView`. Activity, Plans, OS, Tables, Indexes, Vacuum, and Events keep their existing analytical centers.

- [ ] **Step 5: Add concise localized matrix copy**

Add English and Russian keys for matrix title, retained-series count, missing-series count, metric group label, heatmap quality, and retry. Keep operator-visible words short enough for 1920×1080 and 1440×900.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run: `cd web && PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH npx vitest run src/components/StatementsWorkspace.test.tsx src/App.test.tsx src/components/TableView.test.tsx src/components/TemporalBucketRow.test.tsx --coverage.enabled=false`

Expected: PASS; the Statements request contains `buckets=96&top=64` and the detached heatmap is absent only on Statements.

- [ ] **Step 7: Commit the Statements composition**

```bash
git add web/src/components/StatementsWorkspace.tsx web/src/components/StatementsWorkspace.test.tsx web/src/App.tsx web/src/App.test.tsx web/src/i18n/en.json web/src/i18n/ru.json
git commit -m "feat(web): make statements a unified ranked time matrix"
```

### Task 4: Lock the visual contract into browser verification

**Files:**
- Modify: `web/scripts/verify-shell.mjs`
- Modify: `web/scripts/demo-shot.mjs`
- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/scripts/catalog.fixture.json`

**Interfaces:**
- Consumes: the real built SPA and coherent 1,000-statement demo dataset.
- Produces: measurable 1920×1080 and 1440×900 acceptance evidence.

- [ ] **Step 1: Add failing shell-verifier assertions**

At 1920×1080 assert:

```js
statements.detachedHeatmap === false;
statements.timeMatrixBuckets === 96;
statements.renderedRows >= 18;
statements.renderedRows <= 40;
statements.bucketCells <= 40 * 96;
statements.rootScrollY === 0;
statements.timelineWidth / statements.matrixWidth >= 0.45;
```

At 1440×900 assert that the matrix remains present, horizontal overflow is owned by the matrix rather than the document, and the Health line plus matrix controls remain visible.

- [ ] **Step 2: Run the verifier and confirm RED**

Run: `PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH make web-shell-check`

Expected: FAIL until the verifier and demo fixtures recognize the integrated matrix contract.

- [ ] **Step 3: Update deterministic demo fixtures and screenshots**

Make the top 64 heatmap entities use exact statement frame entity tokens, include partial/gap examples, and keep 1,000 frame rows. Capture dark-theme Statements screenshots at 1920×1080 and 1440×900.

- [ ] **Step 4: Run the verifier and confirm GREEN**

Run: `PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH make web-shell-check`

Expected: PASS for Statements and all previously verified screens.

- [ ] **Step 5: Commit browser acceptance**

```bash
git add web/scripts/verify-shell.mjs web/scripts/demo-shot.mjs web/scripts/demo-stub.mjs web/scripts/catalog.fixture.json
git commit -m "test(web): qualify statements visual geometry"
```

### Task 5: Design QA, production verification, and PR

**Files:**
- Create: `design-qa.md`
- Modify: only files implicated by P0/P1/P2 comparison findings.
- Modify: `bins/pg_kronika-web/static/*` via the deterministic build pipeline.

**Interfaces:**
- Consumes: approved 1920×1080 reference plus fresh 1920×1080 and 1440×900 implementation screenshots.
- Produces: `design-qa.md` with `final result: passed`, deterministic embedded assets, a clean branch, and a reviewable PR.

- [ ] **Step 1: Run the complete frontend quality gate**

Run: `PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH make web-frontend-check`

Expected: typecheck, lint, formatting, localization parity, design-token checks, and all Vitest suites pass.

- [ ] **Step 2: Capture and compare the same viewport/state**

Open `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-statements-overview.png` and the fresh 1920×1080 implementation screenshot together. Record geometry, hierarchy, typography, control, identity, heatmap, cursor, selection, and overflow findings in `design-qa.md`.

- [ ] **Step 3: Fix all P0/P1/P2 findings in one bounded batch**

Run the relevant focused tests before each production change, confirm RED, implement the smallest correction, and confirm GREEN. Recapture 1920×1080 and 1440×900 once after the batch.

- [ ] **Step 4: Complete blocking design QA**

`design-qa.md` must end with:

```md
final result: passed
```

Any remaining P3 note must be visually non-blocking and explicitly listed.

- [ ] **Step 5: Run Rust and deterministic archive checks**

```bash
cargo +1.96.0 fmt --all --check
cargo +1.96.0 clippy -q -p pg_kronika-web --all-targets --target aarch64-apple-darwin -- -D warnings
cargo +1.96.0 test -q -p pg_kronika-web --target aarch64-apple-darwin
PATH=/Users/vadv/.npm/_npx/52027bd8fc0022aa/node_modules/.bin:$PATH make web-shell-check
docker run --rm -v "$PWD":/work -w /work node:22-bookworm tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner '--exclude=*.map' -czf bins/pg_kronika-web/static.tar.gz -C bins/pg_kronika-web/static .
make web-bundle-budget
```

Expected: all commands pass, the archive is deterministic, and the bundle remains within 262,144 bytes.

- [ ] **Step 6: Run the final design detector once**

Run: `node /Users/vadv/.agents/skills/impeccable/scripts/detect.mjs --json web/src/App.tsx web/src/components/StatementsWorkspace.tsx web/src/components/TableView.tsx web/src/components/TemporalBucketRow.tsx web/src/components/StatementsTimeMatrix.css`

Expected: no unresolved mechanical findings. Do not run the detector a second time.

- [ ] **Step 7: Commit production assets and QA evidence**

```bash
git add design-qa.md bins/pg_kronika-web/static web/src web/scripts
git commit -m "build(web): embed statements visual convergence"
```

- [ ] **Step 8: Push and open PR9**

```bash
git push -u origin codex/pr09-statements-visual-convergence
gh pr create --repo vadv/PgKronika --base main --head codex/pr09-statements-visual-convergence --title "web: converge Statements with the forensic design" --body-file /tmp/pgkronika-pr09-body.md
```

Expected: the PR is mergeable and all required GitHub checks are green before merge.
