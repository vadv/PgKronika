# Operator-First Evidence UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild Health, entity detail, Events/Signals, Search, and collection chrome as a dense operator-first investigation surface that follows the approved PgKronika visual references at 1920×1080 and never weakens a discovered relation because snapshots are missing.

**Architecture:** Keep the existing API contracts and shared time geometry. Presentation components will consume technical quality and entity identifiers for routing and explicit diagnostics only; normal work surfaces will render human labels, observed values, local empty cells, and positive relation captions. New component-scoped CSS will replace large inline style blocks so the implemented layout can match the selected technical-minimalist reference screens without changing data ownership.

**Tech Stack:** React 19, TypeScript 5.9, TanStack Query, Vitest and Testing Library, i18next, Rust/Axum projection backend, Playwright CLI for 1920×1080 visual verification.

## Global Constraints

- Baseline viewport is 1920×1080; primary content must be visible without page scrolling.
- Use the committed technical-minimalist design system: flat dark/light surfaces, hairline dividers, 4 px rhythm, 26–30 px dense rows, 4–8 px radii, restrained accent color.
- Use the selected references `pgkronika-simplified-healthline.png`, `pgkronika-signals-synchronized-evidence.png`, `pgkronika-entity-process-detail-refined.png`, and `pgkronika-global-search-refined.png` for geometry and hierarchy.
- Do not copy the obsolete proof/quality captions visible in early references; the operator-first specification supersedes them.
- A discovered relation is visible when its key is available. Missing intermediate snapshots, `quality.status`, and relation kind never hide it.
- `starttime` constrains CPU/I/O delta calculations only. It never determines whether a PID relation is displayed.
- Normal UI must not render raw `entity`, endpoint paths, `complete`, `gaps`, `gated`, `identity_quality`, `evidence_quality`, `method`, `fields`, `exact`, `best effort`, or `point projection`.
- Technical identifiers and collection metadata remain available through explicit `Raw evidence`, copy-ID, and Data Health actions.
- Every behavior change follows red-green-refactor and keeps Russian/English localization parity.

---

### Task 1: Human heatmap and compact Health navigator

**Files:**
- Modify: `web/src/components/HeatmapStrip.test.tsx`
- Modify: `web/src/components/HeatmapStrip.tsx`
- Modify: `web/src/components/Spine.test.tsx`
- Modify: `web/src/components/Spine.tsx`
- Create: `web/src/components/Spine.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: existing `HeatmapResponse`, `SpineSeries`, `HealthPointResponse`, and shared `TimeGeometryProvider` state.
- Produces: a heatmap tooltip containing only row label, metric, value, and interval; a `Spine` navigator whose visible summary contains observed Health state and local missing-bucket copy but no window-wide gap counters or source/provenance button.

- [ ] **Step 1: Write failing heatmap tooltip tests**

Add a row-tooltip assertion that the human label remains visible while the opaque entity token and quality summary are absent:

```tsx
fireEvent.mouseEnter(screen.getByTestId("heatmap-row-label-r1"));
const tooltip = await screen.findByRole("tooltip");
expect(tooltip).toHaveTextContent("postgres / 45");
expect(tooltip).not.toHaveTextContent("AQACLQAAAAhAEHmW");
expect(tooltip).not.toHaveTextContent(/gaps|gated|partial/i);
```

Keep the existing click assertion that `onSelectEntity` receives the full opaque token.

- [ ] **Step 2: Run the heatmap test and verify RED**

Run: `cd web && npx vitest run src/components/HeatmapStrip.test.tsx`

Expected: FAIL because the tooltip still renders the entity token and quality rows.

- [ ] **Step 3: Implement the human tooltip**

Remove `tooltip.entity` and `qualityParts` from normal heatmap tooltips. Preserve `row.entity` only in the click handler. For a missing bucket render the localized `data.noSnapshotInterval` copy beside the interval; never aggregate missing buckets into a row warning.

- [ ] **Step 4: Write failing Health navigator tests**

Assert that a 95-bucket missing window renders a neutral current state and accessible local bucket descriptions without the visible terms `95 gaps`, `source`, or `provenance`:

```tsx
expect(screen.getByRole("region", { name: /Health/ })).toHaveTextContent("No snapshot");
expect(screen.getByRole("region", { name: /Health/ })).not.toHaveTextContent(/95 gaps|provenance/i);
expect(screen.queryByRole("button", { name: /provenance|source/i })).toBeNull();
```

- [ ] **Step 5: Run the Spine test and verify RED**

Run: `cd web && npx vitest run src/components/Spine.test.tsx`

Expected: FAIL because `Spine` exposes aggregate gap/source chrome.

- [ ] **Step 6: Implement the compact navigator and stylesheet**

Import `Spine.css`, add stable BEM classes, and reproduce the reference geometry: a 116–132 px summary rail, one continuous 40 px time strip, aligned cursor, thin Health line, severity/event marks, and right-aligned cursor facts. Keep selection, brushing, baseline, keyboard, live refresh, and accessible bucket names working. Replace visible aggregate collection text with:

```tsx
<span className="health-navigator__state">
  {hasCurrent ? scoreLabel : t("data.noSnapshotCurrent")}
</span>
```

The button that opened score provenance is removed from normal Health. Detailed collection diagnostics remain reachable from the header Data Health action.

- [ ] **Step 7: Run focused tests and commit**

Run: `cd web && npx vitest run src/components/HeatmapStrip.test.tsx src/components/Spine.test.tsx src/components/HealthLine.test.tsx`

Expected: PASS.

Commit: `feat(web): make health and heatmaps operator first`

---

### Task 2: Structured entity detail and positive relations

**Files:**
- Modify: `web/src/components/DockOverlay.test.tsx`
- Modify: `web/src/components/DockOverlay.tsx`
- Create: `web/src/components/DockOverlay.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: `EntityPointResponse`, `EntityHistoryResponse`, current view columns, and `RelatedEntityDto` from the existing entity endpoint.
- Produces: a 520–560 px desktop inspector with a human header, data-first Summary, continuous History, positive Relations, and an explicit Raw evidence surface.

- [ ] **Step 1: Write failing Summary and heading tests**

Replace the old provenance-card assertions with consumer-visible behavior:

```tsx
expect(screen.getByRole("tabpanel")).toHaveTextContent("RSS");
expect(screen.getByRole("tabpanel")).not.toHaveTextContent(/complete|gaps|gated|point projection|\/v1\/entity/i);
expect(screen.getByTestId("dock-entity-heading")).not.toHaveAttribute("title", entityToken);
expect(screen.getByTestId("dock-copy-token")).toHaveAccessibleName(/technical ID/i);
```

The mutation caught is reintroducing API routing material into the normal detail surface.

- [ ] **Step 2: Run the Summary test and verify RED**

Run: `cd web && npx vitest run src/components/DockOverlay.test.tsx -t "Summary"`

Expected: FAIL on the existing provenance card and token title.

- [ ] **Step 3: Implement data-first Summary and detail geometry**

Remove the partial banner and `data-detail-provenance` block. Add `DockOverlay.css` and turn the desktop inspector into the reference hierarchy: human identity row, compact tab rail, aligned measurement sections, SQL/command blocks only when present, and continuous dividers instead of nested cards. The copy action remains icon-sized but has a descriptive accessible label; the opaque token is never a native title.

- [ ] **Step 4: Write failing History and Relations tests**

Assert History preserves received snapshots without a quality banner, and Relations use only positive labels:

```tsx
expect(screen.queryByTestId("history-quality")).toBeNull();
expect(screen.getAllByTestId("history-snapshot")).toHaveLength(3);
expect(screen.getByText("Связано по PID")).toBeVisible();
expect(screen.getByRole("tabpanel")).not.toHaveTextContent(/best_effort|exact|method|fields|pid · pid/i);
```

Also assert an activity relation remains rendered when fixture quality is partial and contains a gap.

- [ ] **Step 5: Run History/Relations tests and verify RED**

Run: `cd web && npx vitest run src/components/DockOverlay.test.tsx -t "History|Relations"`

Expected: FAIL because the current tabs render quality and provenance metadata.

- [ ] **Step 6: Implement continuous History, positive labels, and explicit Raw evidence**

Map relation semantics to these localization keys:

```ts
const RELATION_LABEL_KEYS = {
  activity_process: "dock.relation.pid",
  statement_plan: "dock.relation.query",
  table_index: "dock.relation.table",
  table_vacuum: "dock.relation.table",
  index_table: "dock.relation.index",
} as const;
```

Fall back to `dock.relation.object` and use `dock.relation.nearTime` only for explicit temporal relations. Do not inspect `provenance.kind` to decide visibility or tone. Move endpoint, entity token, snapshot timestamp, quality object, and relation metadata into `Raw evidence`; add a visible copy-ID action there.

- [ ] **Step 7: Run focused tests and commit**

Run: `cd web && npx vitest run src/components/DockOverlay.test.tsx src/api/entity.test.ts`

Expected: PASS.

Commit: `feat(web): rebuild entity detail around operator evidence`

---

### Task 3: Synchronized Signals and event investigation surface

**Files:**
- Modify: `web/src/components/EventsSignalPanel.test.tsx`
- Modify: `web/src/components/EventsSignalPanel.tsx`
- Modify: `web/src/components/EventsWorkspace.test.ts`
- Modify: `web/src/components/EventsWorkspace.tsx`
- Modify: `web/src/components/EventsWorkspace.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: `TimelineEventsResponse`, selected shared time range, event family grouping, and `onInvestigate(view, atUs, eventInstance)`.
- Produces: compact synchronized signal lanes and an event table whose columns are time, event, object/source, and occurrence count—never collection-quality codes.

- [ ] **Step 1: Write failing signal-lane tests**

Assert user-facing names and useful counts remain while quality codes disappear from text, tooltip, and accessible name:

```tsx
const lane = screen.getAllByTestId("event-signal-lane")[0];
expect(lane).toHaveTextContent("Deadlock");
expect(lane).toHaveTextContent("×3");
expect(lane).not.toHaveAccessibleName(/content_derived|derived_exact|quality/i);
expect(lane).not.toHaveAttribute("title", expect.stringMatching(/exact|quality/i));
expect(screen.getByTestId("event-signals-summary")).toHaveTextContent("8 signals");
```

- [ ] **Step 2: Run signal tests and verify RED**

Run: `cd web && npx vitest run src/components/EventsSignalPanel.test.tsx`

Expected: FAIL because the existing summary and lane labels expose quality codes.

- [ ] **Step 3: Implement the synchronized signal strip**

Match the selected Signals reference: 3–5 dense horizontal lanes sharing the global interval, a strong event label, human object/source, occurrence count, and a single investigate action. Keep missing data as a neutral empty lane. Remove `qualityText` from markup, titles, and aria labels. Retain technical event fields only in the entity/event Raw evidence path.

- [ ] **Step 4: Write failing Events workspace tests**

Change the expected column contract from Quality to Occurrences and assert the old footer diagnostic grid is absent:

```tsx
expect(screen.getByText("Occurrences")).toBeVisible();
expect(screen.queryByText("Identity/evidence quality")).toBeNull();
expect(screen.queryByTestId("event-quality-summary")).toBeNull();
expect(screen.getAllByTestId("event-range-row")[0]).toHaveTextContent("×3");
```

- [ ] **Step 5: Run Events workspace tests and verify RED**

Run: `cd web && npx vitest run src/components/EventsWorkspace.test.ts`

Expected: FAIL because the current table and footer render quality/completeness.

- [ ] **Step 6: Implement the dense event workspace**

Use continuous flat surfaces, 30–34 px rows, a narrow severity rail on the specific event, and a right-side family distribution. Replace the quality column with occurrence count and source/object information. Remove the collection-quality footer. Keep event family counts, filtering, selected time, retry/error, and investigation routing.

- [ ] **Step 7: Run focused tests and commit**

Run: `cd web && npx vitest run src/components/EventsSignalPanel.test.tsx src/components/EventsWorkspace.test.ts src/components/HeatmapStrip.test.tsx`

Expected: PASS.

Commit: `feat(web): synchronize signals and event evidence`

---

### Task 4: Search results that lead directly to evidence

**Files:**
- Modify: `web/src/components/ForensicSearch.test.tsx`
- Modify: `web/src/components/ForensicSearch.tsx`
- Create: `web/src/components/ForensicSearch.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: existing grouped search responses, query grammar, view/entity routing, and selected range/snapshot mode.
- Produces: a centered command workspace matching the selected Search reference, with dense result groups, human match context, and no aggregate `status/gaps/gated/unavailable/limited` line.

- [ ] **Step 1: Write failing search presentation tests**

For a fixture with `partial`, gaps, gated sources, and retained matches, assert the matches still render while internal status words do not:

```tsx
expect(screen.getByText("PID 45")).toBeVisible();
expect(screen.getByRole("dialog")).not.toHaveTextContent(/partial|gaps|gated|resource_limited|unavailable_revision/i);
```

For an unavailable entire group assert only the localized message `No data for this source in the selected period` appears.

- [ ] **Step 2: Run search tests and verify RED**

Run: `cd web && npx vitest run src/components/ForensicSearch.test.tsx`

Expected: FAIL because status summaries expose internal collection codes.

- [ ] **Step 3: Implement the command-workspace layout**

Import `ForensicSearch.css`, preserve query syntax and keyboard behavior, and match the reference hierarchy: 28–32 px query bar, compact scope controls, group headers with counts, 36–54 px results, human match reason, and direct Open/Compare actions. Remove `coverageStatus`, `noMatchCoverage`, and visible technical totals from rendered output. The API fields may remain in data types.

- [ ] **Step 4: Run focused tests and commit**

Run: `cd web && npx vitest run src/components/ForensicSearch.test.tsx src/search/compile.test.ts src/search/group.test.tsx`

Expected: PASS.

Commit: `feat(web): turn search into an evidence command workspace`

---

### Task 5: Quiet collection chrome across Header, Activity, and OS

**Files:**
- Modify: `web/src/components/Header.test.tsx`
- Modify: `web/src/components/Header.tsx`
- Modify: `web/src/components/StatusBar.test.tsx`
- Modify: `web/src/components/StatusBar.tsx`
- Modify: `web/src/components/ActivityWorkspace.test.tsx`
- Modify: `web/src/components/ActivityWorkspace.tsx`
- Modify: `web/src/components/ActivityWorkspace.css`
- Modify: `web/src/components/OsWorkspace.test.tsx`
- Modify: `web/src/components/OsWorkspace.tsx`
- Modify: `web/src/components/OsWorkspace.css`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`

**Interfaces:**
- Consumes: existing `FrameQuality`, header Data Health action, process/activity projections, and OS host/process separation.
- Produces: quiet normal chrome; explicit Data Health remains the only detailed collection diagnostic surface.

- [ ] **Step 1: Write failing Header and StatusBar tests**

Assert a response containing gaps or gated optional inputs does not produce an amber normal-state chip or footer quality code. Assert a screen that cannot render at all still exposes a warning and the Data Health action remains keyboard reachable.

```tsx
expect(screen.getByRole("button", { name: /Data/ })).toHaveAttribute("data-tone", "neutral");
expect(screen.getByRole("contentinfo")).not.toHaveTextContent(/quality: partial|gaps|gated/i);
```

- [ ] **Step 2: Run chrome tests and verify RED**

Run: `cd web && npx vitest run src/components/Header.test.tsx src/components/StatusBar.test.tsx`

Expected: FAIL because normal chrome summarizes partial/gated quality.

- [ ] **Step 3: Implement quiet normal chrome**

Derive header tone from screen renderability, not raw gap count. Keep the existing Data Health popover trigger and explicit diagnostics. Replace footer quality text with current mode, cursor time, range, baseline, and selection only.

- [ ] **Step 4: Write failing Activity and OS tests**

Assert the Activity/OS workspaces preserve real rows and host signals but remove visible provenance, coverage, `partial`, `gaps`, `gated`, and scope-debug sentences. Assert local missing cells remain em dashes with `No snapshot for this interval` accessible text.

- [ ] **Step 5: Run Activity/OS tests and verify RED**

Run: `cd web && npx vitest run src/components/ActivityWorkspace.test.tsx src/components/OsWorkspace.test.tsx`

Expected: FAIL on the existing quality and scope-debug copy.

- [ ] **Step 6: Implement dense operator controls and local absence**

Keep Activity/OS prepared lenses and their tables. Remove diagnostic prose from primary controls, tighten vertical chrome, align metric controls to the reference 28 px rail, and present host signal values as measurement rows rather than cards. Keep process/activity PID links and row selection unchanged.

- [ ] **Step 7: Run focused tests and commit**

Run: `cd web && npx vitest run src/components/Header.test.tsx src/components/StatusBar.test.tsx src/components/ActivityWorkspace.test.tsx src/components/OsWorkspace.test.tsx src/components/DataHealthPopover.test.tsx`

Expected: PASS.

Commit: `feat(web): quiet collection diagnostics in work surfaces`

---

### Task 6: Backend relation guard, full verification, and visual convergence

**Files:**
- Modify if the existing contract is insufficient: `bins/pg_kronika-web/src/tests/ui_frame.rs`
- Modify if the existing contract is insufficient: `bins/pg_kronika-web/src/ui/frame/projection.rs`
- Modify: `web/src/i18n/parity.test.ts`
- Create during QA: `design-qa.md`

**Interfaces:**
- Consumes: activity/process rows from current and prior retained snapshots, existing `activity_process_relations`, built Vite UI, and selected visual references.
- Produces: a regression guard proving PID relations survive missing intervals, a verified production build, and a design-QA report with `final result: passed`.

- [ ] **Step 1: Run the existing backend PID relation guards**

Run: `cargo test --target aarch64-apple-darwin -p pg_kronika-web activity_keeps_the_pid_link -- --nocapture`

Expected: the same-PID current, ambiguous, and collection-gap relation tests PASS. If the multiple-missing-interval fixture is not represented, continue with Step 2; otherwise do not change backend production code.

- [ ] **Step 2: Add a failing multiple-missing-interval backend test only if needed**

The fixture contains Activity PID 7 at `20_000_000`, no current OS process row, missing intermediate process snapshots, and the nearest retained process PID 7. The assertion is:

```rust
assert!(relations.iter().any(|relation| {
    relation.view == "processes" && relation.method == "pid"
}));
```

Run the focused test and verify RED before changing `projection.rs`. Implement only the bounded scan needed to return found same-PID candidates; do not use `starttime`, relation kind, or quality to suppress the link. Re-run to GREEN.

- [ ] **Step 3: Run full automated frontend verification**

Run:

```bash
cd web
npm run format:check
npm run lint
npm run typecheck
npm run test
npm run build
```

Expected: all commands exit 0, localization parity passes, and coverage thresholds remain satisfied.

- [ ] **Step 4: Run Rust verification on the working macOS target**

Run:

```bash
cargo fmt --check
cargo test --target aarch64-apple-darwin -p pg_kronika-web
```

Expected: both commands exit 0. Do not use the repository default musl target for local completion because the current host linker lacks its mimalloc/zstd symbols.

- [ ] **Step 5: Start the application and capture the required states**

Use the repository demo/real-data workflow, set the browser viewport to 1920×1080, and capture at least:

- OS / Disk I/O with Health navigator;
- Events with synchronized Signals;
- Activity or Process entity Summary;
- entity Relations;
- global Search with results.

Save captures under `output/playwright/` and keep them uncommitted.

- [ ] **Step 6: Run the visual detector once**

Run:

```bash
node /Users/vadv/.agents/skills/impeccable/scripts/detect.mjs --json \
  web/src/components/Spine.tsx \
  web/src/components/Spine.css \
  web/src/components/DockOverlay.tsx \
  web/src/components/DockOverlay.css \
  web/src/components/EventsWorkspace.tsx \
  web/src/components/EventsWorkspace.css \
  web/src/components/ForensicSearch.tsx \
  web/src/components/ForensicSearch.css
```

Fix actionable findings without changing the approved visual direction.

- [ ] **Step 7: Perform blocking design QA**

Open each reference beside the matching 1920×1080 implementation capture. Record geometry, hierarchy, typography, density, color, overflow, state, and interaction differences in `design-qa.md`. Fix every P0/P1/P2, recapture, and repeat until the report ends with:

```text
final result: passed
```

- [ ] **Step 8: Commit final convergence and QA**

Commit production/test changes and `design-qa.md` with:

`test(web): verify operator-first visual convergence`
