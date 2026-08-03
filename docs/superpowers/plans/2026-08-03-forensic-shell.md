# Forensic UI PR 3: Shared Forensic Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the source-tab dashboard chrome with a compact, keyboard-operable forensic shell whose navigation, time geometry, 60 px Health line, data-state language, and 1920×1080 viewport contract are shared by every later analytical screen.

**Architecture:** Keep API view codes stable, but introduce a frontend navigation model that groups them into Workload, Data, Host, and Events and maps the Host entry to the existing process-backed OS surface until PR 7 composes the full OS screen. Move hash/live-time logic behind one `TimeGeometryProvider`: cursor, selected range, hover bucket, brush draft, baseline, Live/Replay mode, and all mutations have one owner, while int64 microseconds remain decimal strings and all arithmetic uses `BigInt`. Refactor the current Spine presentation into a fixed-height Health line; its backend contracts and server verdicts stay unchanged. Build reusable semantic-state and provenance primitives, then make the desktop shell an explicit viewport grid whose document never scrolls at 1920×1080 while the ranked matrix remains independently scrollable.

**Tech Stack:** React 19, TypeScript 5.9, TanStack Query, i18next, Vitest/Testing Library, Vite, Puppeteer demo harness, existing Axum/OpenAPI timeline endpoints.

## Global Constraints

- This is stacked PR 3 and must remain based on `codex/pr02-metric-semantics`; do not implement Statements lenses, global search results, entity-detail anatomy, Activity/Plans workflows, or OS/Data/Event analytical screens from PRs 4–8.
- The approved contract is `docs/superpowers/specs/2026-08-02-pgkronika-forensic-ui-system-design.md`: 44 px Global context, 32 px primary navigation, exactly 60 px Health line, 68–76 px screen header, and a 24 px status strip.
- At 1920×1080 and 100% zoom the page root must have no vertical overflow. Dense evidence content scrolls inside its bounded matrix region; primary context, navigation, Health line, screen header, and at least 16 rows remain visible.
- Health line is the only persistent aggregate combining PostgreSQL and OS. Do not add KPI cards or extra top charts, and do not remove the analytical heatmap from the screen body.
- One time geometry owns cursor, range, hover bucket, brush, baseline, gaps, and Live/Replay. No component may call `Date.now()` independently to derive an API range after this PR.
- int64 microsecond timestamps stay signed decimal strings at all React and URL boundaries. Use `BigInt` for every timestamp/range calculation and convert to `Number` only for bounded SVG pixel geometry.
- Brush movement is immediate and local; committing a brush updates the selected replay range once, after pointer/keyboard completion. A simple click moves the cursor without fabricating a range.
- Preserve the current URL compatibility (`view`, `at`, `span`, `baseline`, filters and dock). Allow a committed brush span within 1 second–24 hours while keeping 15m/1h/6h/24h as prepared controls. Free-text `q` remains transient.
- API `severity`, `status`, `reason`, `coverage`, and rule revisions are rendered as received. The client must not infer root cause, causal linkage, or silently turn `null`, gap, partial, gated, reset, unsupported, or top-N truncation into zero/complete.
- Severity and quality are never encoded by color alone. Every meaningful state has text or an accessible label, and healthy states stay visually quiet.
- Tooltip content opens from hover/focus after the existing delay. Provenance is an explicit button/popover with `aria-expanded`, Escape/click-outside close, focus restoration, and no hover-only facts.
- Keyboard contract: `/` is reserved for PR 5 search; number keys select available top-level destinations; arrows move the shared cursor; Shift+arrows jump one hour; Space toggles Live/Replay; Shift+click sets/clears baseline; Enter opens the selected entity when available; Escape closes the topmost layer.
- Respect `prefers-reduced-motion`: disable pulse/animated transitions without hiding loading or selection state. Focus rings must remain visible in both themes.
- Every code change starts with a failing focused test, then the smallest passing implementation, focused verification, and a task commit. Keep EN/RU localization parity in the same task.
- Do not change Rust/OpenAPI contracts in this PR unless an existing frontend contract cannot represent a server-provided value; any such change requires a focused contract test and explicit justification in the ledger.
- Run frontend gates with Node 22. Rebuild `bins/pg_kronika-web/static.tar.gz` deterministically at the end and reject macOS `._*`/xattr archive entries.

---

### Task 1: One URL-safe time geometry store

**Files:**
- Create: `web/src/state/timeGeometry.tsx`
- Create: `web/src/state/timeGeometry.test.tsx`
- Modify: `web/src/state/url.ts`
- Modify: `web/src/state/url.test.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`

**Interfaces:**
- Produces: `TimeGeometryProvider`, `useTimeGeometry()`, `TimeRange { fromUs, toUs }`, and actions `setCursor`, `setSpan`, `commitRange`, `setHover`, `setBrushDraft`, `setBaseline`, `toggleLive`.
- Preserves: `UiState.at === null` means Live; replay `at` is the selected cursor/range end; `span` is integer seconds in `[1, 86400]`; default prepared span remains 3600 seconds.

- [ ] **Step 1: Write failing URL and provider tests**

Cover invalid/non-decimal timestamps, 1-second and 24-hour bounds, rejection of zero/negative/over-24h spans, prepared spans, exact BigInt range endpoints, one pinned Live tick shared by consumers, committed brush entering Replay, a sub-second brush normalizing to one second, baseline preservation, hover/brush draft staying ephemeral, hashchange/back-forward adoption, and no query consumer deriving a different `toUs` during an unrelated render.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
npm --prefix web test -- --run web/src/state/url.test.ts web/src/state/timeGeometry.test.tsx web/src/App.test.tsx
```

Expected: no provider exists, arbitrary committed spans are rejected, and time behavior is still split between `App` and `Spine`.

- [ ] **Step 3: Implement the provider and migrate App**

Use a single 15-second Live tick in the provider. Derive the canonical range only as:

```text
toUs   = replay cursor or pinned live tick
fromUs = toUs - span_seconds * 1_000_000
```

Keep hover and brush draft in memory; only committed cursor/range/baseline changes update the hash. Centralize hash write/back-forward handling and expose stable callbacks so global key handlers do not re-register on every render. Replace App-local `liveAt`, `heatmapRange`, and timestamp arithmetic with provider values.

- [ ] **Step 4: Run focused tests**

```bash
npm --prefix web test -- --run web/src/state/url.test.ts web/src/state/timeGeometry.test.tsx web/src/App.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/state/timeGeometry.tsx web/src/state/timeGeometry.test.tsx web/src/state/url.ts web/src/state/url.test.ts web/src/App.tsx web/src/App.test.tsx
git commit -m "feat(web): centralize forensic time geometry"
```

### Task 2: Grouped forensic navigation and fixed shell regions

**Files:**
- Create: `web/src/navigation/model.ts`
- Create: `web/src/navigation/model.test.ts`
- Create: `web/src/components/PrimaryNavigation.tsx`
- Create: `web/src/components/PrimaryNavigation.test.tsx`
- Create: `web/src/components/ShellLayout.tsx`
- Create: `web/src/components/ShellLayout.test.tsx`
- Modify: `web/src/components/Header.tsx`
- Modify: `web/src/components/Header.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/design/tokens.css`

**Interfaces:**
- Produces frontend destinations grouped as `Workload(activity, statements, plans)`, `Data(tables, indexes, vacuum)`, `Host(OS -> processes backing view)`, and `Events(events)`.
- Keeps `processes` and `locks` reachable from links/hash and later search/detail, but removes them from permanent top-level destinations.

- [ ] **Step 1: Write failing navigation and layout tests**

Assert group order and labels, catalog availability propagation, OS selecting the stable `processes` API view, hidden Locks/Processes top-level tabs, roving tab focus with Left/Right/Home/End, number shortcuts following visible destination order, Live/Replay and prepared spans in the 32 px primary bar, Global context region height 44 px, nav height 32 px, status height 24 px, and a hash pointing to `locks` rendering an honest contextual fallback rather than an empty active tab.

- [ ] **Step 2: Run tests and verify RED**

```bash
npm --prefix web test -- --run web/src/navigation/model.test.ts web/src/components/PrimaryNavigation.test.tsx web/src/components/ShellLayout.test.tsx web/src/App.test.tsx
```

Expected: current flat `TabBar` exposes raw Processes/Locks and has no fixed region contract.

- [ ] **Step 3: Implement the grouped shell**

Move Live/Replay and prepared span controls out of the timeline into `PrimaryNavigation`. Render group labels and destinations without a permanent sidebar. Use semantic `header`, `nav`, `main`, and `footer` regions; expose stable `data-shell-region` hooks for the viewport verifier. Keep mobile incident triage behavior intact below 760 px and allow normal document flow there.

- [ ] **Step 4: Run focused tests and localization parity**

```bash
npm --prefix web test -- --run web/src/navigation/model.test.ts web/src/components/PrimaryNavigation.test.tsx web/src/components/ShellLayout.test.tsx web/src/components/Header.test.tsx web/src/App.test.tsx web/src/i18n/parity.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/navigation web/src/components/PrimaryNavigation.tsx web/src/components/PrimaryNavigation.test.tsx web/src/components/ShellLayout.tsx web/src/components/ShellLayout.test.tsx web/src/components/Header.tsx web/src/components/Header.test.tsx web/src/App.tsx web/src/App.test.tsx web/src/i18n/en.json web/src/i18n/ru.json web/src/design/tokens.css
git commit -m "feat(web): add grouped forensic navigation shell"
```

### Task 3: A real 60 px Health line with shared cursor and brush

**Files:**
- Create: `web/src/components/HealthLine.tsx`
- Create: `web/src/components/HealthLine.test.tsx`
- Modify: `web/src/components/Spine.tsx`
- Modify: `web/src/components/Spine.test.tsx`
- Modify: `web/src/components/spineHealth.ts`
- Modify: `web/src/components/spineHealth.test.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/design/tokens.css`

**Interfaces:**
- `HealthLine` consumes the Task 1 time context and existing `/v1/timeline/spine`, `/health`, `/events`, and `/incidents` hooks.
- It renders one 60 px region: quiet health score/quality summary, PG+OS signal ribbon/spark geometry, event glyphs, gap hatching, selected range, shared cursor, hover cursor, and optional baseline.

- [ ] **Step 1: Write failing geometry and interaction tests**

Cover exact 60 px outer height, no embedded mode/zoom controls, one accessible timeline slider, cursor click, Shift+click baseline toggle, pointer brush preview and single commit, minimum drag threshold distinguishing click from brush, keyboard cursor movement, shared hover updates visible to an external consumer, baseline/range lines aligned to the same SVG grid, gap hatch preserved under selection, forming Live tail, empty/error states, and server verdict/severity labels available without color.

- [ ] **Step 2: Run focused tests and verify RED**

```bash
npm --prefix web test -- --run web/src/components/HealthLine.test.tsx web/src/components/Spine.test.tsx web/src/components/spineHealth.test.ts web/src/App.test.tsx
```

Expected: the current Spine owns private hover/live geometry, contains mode/zoom controls, and is taller than the approved Health line.

- [ ] **Step 3: Refactor without changing backend semantics**

Keep query hooks and score/verdict helpers, but make `HealthLine` the public shell component. Convert pointer positions only inside the bounded SVG, use pointer capture during a brush, render the draft without network changes, and call `commitRange` once on pointer-up. Keep exact/best-effort/temporal semantics out of the aggregate: this line shows coincidence and quality, never a causal conclusion.

- [ ] **Step 4: Run focused tests**

```bash
npm --prefix web test -- --run web/src/components/HealthLine.test.tsx web/src/components/Spine.test.tsx web/src/components/spineHealth.test.ts web/src/App.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/HealthLine.tsx web/src/components/HealthLine.test.tsx web/src/components/Spine.tsx web/src/components/Spine.test.tsx web/src/components/spineHealth.ts web/src/components/spineHealth.test.ts web/src/App.tsx web/src/App.test.tsx web/src/i18n/en.json web/src/i18n/ru.json web/src/design/tokens.css
git commit -m "feat(web): turn timeline spine into health line"
```

### Task 4: Honest semantic states and reusable provenance popover

**Files:**
- Create: `web/src/components/SemanticBadge.tsx`
- Create: `web/src/components/SemanticBadge.test.tsx`
- Create: `web/src/components/ProvenancePopover.tsx`
- Create: `web/src/components/ProvenancePopover.test.tsx`
- Modify: `web/src/components/HealthLine.tsx`
- Modify: `web/src/components/HealthLine.test.tsx`
- Modify: `web/src/components/PageHeader.tsx`
- Modify: `web/src/components/PageHeader.test.tsx`
- Modify: `web/src/components/StatusBar.tsx`
- Modify: `web/src/components/StatusBar.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ru.json`
- Modify: `web/src/design/ui.ts`
- Modify: `web/src/design/tokens.css`

**Interfaces:**
- Produces semantic kinds `G | ΔC | R | S | E | EST` and data states `null | partial | gated | reset | gap | unsupported | top_n` with non-color labels and localized explanations.
- Produces `ProvenancePopover` fields: definition, value, unit, selected window/snapshot, aggregation/formula, baseline, source/producer, coverage, reset boundary, sampling caveat, verdict rule/revision, state, and reason. Optional fields are omitted, never invented.

- [ ] **Step 1: Write failing component tests**

Assert every semantic/data-state label and accessible name, `null` renders as `—` plus its reason, quiet complete/healthy presentation, popover open by click/Enter/Space, focusable trigger, outside-click and Escape close, trigger focus restoration, focus/fact retention without hover, long formula wrapping, viewport clamping, and Health score provenance showing the exact server-window inputs and local documented formula without claiming root cause.

- [ ] **Step 2: Run tests and verify RED**

```bash
npm --prefix web test -- --run web/src/components/SemanticBadge.test.tsx web/src/components/ProvenancePopover.test.tsx web/src/components/HealthLine.test.tsx web/src/components/PageHeader.test.tsx web/src/components/StatusBar.test.tsx
```

Expected: reusable semantic and persistent provenance primitives do not exist.

- [ ] **Step 3: Implement and integrate the primitives**

Use a popover only for persistent, multi-row provenance; keep the existing delayed Tooltip for short definitions. Integrate provenance into the Health score and page coverage/source affordances. Make Status strip report the active mode, selected range/cursor, baseline presence, data quality, and selection count in its fixed 24 px without duplicating full explanations.

- [ ] **Step 4: Run focused tests and parity**

```bash
npm --prefix web test -- --run web/src/components/SemanticBadge.test.tsx web/src/components/ProvenancePopover.test.tsx web/src/components/HealthLine.test.tsx web/src/components/PageHeader.test.tsx web/src/components/StatusBar.test.tsx web/src/i18n/parity.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/SemanticBadge.tsx web/src/components/SemanticBadge.test.tsx web/src/components/ProvenancePopover.tsx web/src/components/ProvenancePopover.test.tsx web/src/components/HealthLine.tsx web/src/components/HealthLine.test.tsx web/src/components/PageHeader.tsx web/src/components/PageHeader.test.tsx web/src/components/StatusBar.tsx web/src/components/StatusBar.test.tsx web/src/i18n/en.json web/src/i18n/ru.json web/src/design/ui.ts web/src/design/tokens.css
git commit -m "feat(web): expose metric states and provenance"
```

### Task 5: Enforce the 1920×1080 viewport and release gates

**Files:**
- Create: `web/scripts/verify-shell.mjs`
- Modify: `web/scripts/demo-shot.mjs`
- Modify: `web/scripts/demo-stub.mjs`
- Modify: `web/package.json`
- Modify: `Makefile`
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/components/TableView.tsx`
- Modify: `web/src/components/TableView.test.tsx`
- Modify: `web/src/design/tokens.css`
- Modify: `docs/superpowers/specs/2026-08-02-pgkronika-forensic-ui-system-design.md`
- Rebuild: `bins/pg_kronika-web/static.tar.gz`

**Interfaces:**
- Adds `npm --prefix web run verify:shell` and `make web-shell-check`.
- The verifier runs the deterministic demo at 1920×1080/100%, asserts region heights, `document.documentElement.scrollHeight <= 1080`, Health line visibility, independently scrollable matrix overflow, at least 16 visible table rows, and keyboard reachability of navigation, Health line, matrix, and status context; it emits one diagnostic screenshot on failure and one approved-size screenshot on success.

- [ ] **Step 1: Write the failing shell verifier and DOM contracts**

Add stable region/row test IDs and make the verifier print measured dimensions and offending selectors. Add unit tests that desktop uses bounded overflow while the <=760 px mobile layout keeps normal document scrolling. Run it against the current layout and record the RED measurements in the SDD ledger.

- [ ] **Step 2: Run unit and browser checks and verify RED**

```bash
npm --prefix web test -- --run web/src/App.test.tsx web/src/components/TableView.test.tsx
npm --prefix web run verify:shell
```

Expected: the desktop document exceeds 1080 px or required shell regions do not have their contracted heights.

- [ ] **Step 3: Bound the desktop analytical area**

Use a desktop CSS grid/flex contract with `min-height: 0` at every nested boundary. Keep Global context, navigation, Health line, page header, heatmap/analytical center, matrix, and status visible; put overflow only on the ranked matrix body. Disable pulse/transitions under `prefers-reduced-motion`, preserve sticky table header/identity column, and do not shrink the Health line or table rows below approved sizes to force a pass. Update the design spec only with the implemented shell verification command and any clarified mapping from Host/OS to the current backing view.

- [ ] **Step 4: Run the complete frontend and release verification**

```bash
npm --prefix web run verify:shell
make web-frontend-check
make web-bundle-budget
npm --prefix web run build
make web-frontend
tar -tzf bins/pg_kronika-web/static.tar.gz
git status --short
```

Expected: 1920×1080 assertions PASS; typecheck, ESLint, Prettier, all Vitest tests and coverage PASS; bundle budget PASS; archive contains only deterministic web assets and no `._*`; generated files are clean except the intended archive update.

- [ ] **Step 5: Run branch review and commit**

Request a whole-branch review against `codex/pr02-metric-semantics`, fix every Critical/Important finding with focused regressions, rerun Step 4, then commit:

```bash
git add web/scripts/verify-shell.mjs web/scripts/demo-shot.mjs web/scripts/demo-stub.mjs web/package.json Makefile web/src/App.tsx web/src/App.test.tsx web/src/components/TableView.tsx web/src/components/TableView.test.tsx web/src/design/tokens.css docs/superpowers/specs/2026-08-02-pgkronika-forensic-ui-system-design.md bins/pg_kronika-web/static.tar.gz
git commit -m "test(web): enforce forensic shell viewport"
```

## PR 3 Acceptance Gate

- [ ] Branch is a direct descendant of `codex/pr02-metric-semantics` and contains no PR 4–8 feature work.
- [ ] Global context, grouped navigation, exactly 60 px Health line, screen context, heatmap/analytical center, matrix, and 24 px status strip are visible at 1920×1080/100% with no root vertical scroll.
- [ ] At least 16 evidence rows are visible and the matrix scrolls independently.
- [ ] Workload/Data/Host/Events replace raw flat tabs; Processes/Locks are not permanent destinations but remain deep-linkable.
- [ ] Cursor, range, hover, brush, baseline, Live/Replay, gaps, Health line, and heatmap consume one time geometry.
- [ ] Health line remains the only persistent PG+OS aggregate and does not claim causality.
- [ ] `null`, partial, gated, reset, gap, unsupported, and top-N truncation are visually and accessibly distinct.
- [ ] Provenance is keyboard-operable and can expose source, window, aggregation, unit, coverage, reset/sampling/rule details, and exact state reason when those facts exist.
- [ ] Keyboard navigation and focus states pass in both themes; reduced-motion mode has no pulse/animated transitions.
- [ ] Full frontend test/coverage/lint/type/format, shell browser verification, deterministic archive, and bundle budget gates pass.
