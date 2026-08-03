# PR8: Events, Signals, and production polish

Date: 2026-08-03
Base: `main` at `9687a197`
Branch: `codex/pr08-events-signals-polish`

## Outcome

Complete the eight-screen forensic workflow with an Events workspace that keeps
the shared Health line and heatmap, exposes bounded typed timeline facts as
Signals, and makes every claimed drill-down disclose its identity and evidence
quality. Finish the stack with keyboard, screen-reader, localization, bundle,
and 1920×1080 / 1440×900 regression checks.

## Semantic contract

- The Health line remains the only aggregate PostgreSQL + OS health summary.
- The Events heatmap shows all retained event candidates in the selected range;
  it is visual correlation evidence, never a causal score.
- A prepared Event lens filters the signal panel and ranked table to one event
  family. It does not rewrite or narrow the Health line.
- `/v1/timeline/events` stays bounded by the server limit and cursor contract.
  The side panel requests at most 50 facts and renders at most 6 lanes.
- `EventFact.entity.id` is content-derived timeline identity, not a UI frame
  entity token. It may route to an investigation screen by entity kind, but it
  must not open Entity Detail as if exact row identity were proven.
- Event drill-down always shows `identity_quality`, `evidence_quality`, loss,
  occurrence count, and supporting-evidence count. No equality in time is
  promoted to causality.
- Config changes remain visibly gated until a typed config-change source is
  published. Collector health is limited to collector/gap/source-status facts.

## Prepared lenses

| UI lens          | Catalog preset     | Signal/table family                                        |
| ---------------- | ------------------ | ---------------------------------------------------------- |
| Timeline         | `timeline`         | all retained event kinds                                   |
| Errors           | `errors`           | event kinds containing `error`                             |
| Checkpoints      | `checkpoints`      | `pg.checkpoint.*`                                          |
| Autovacuum       | `vacuum`           | `pg.maintenance.*`                                         |
| Slow queries     | `slow`             | `pg.query.slow_*`                                          |
| Collector health | `collector_health` | `collector.*` facts, including collector-owned gaps/status |
| Config changes   | unavailable        | gated: no typed source                                     |

The frame query derives a machine filter from the selected prepared lens only
while the user's transient search field is empty. A user-entered filter remains
authoritative and share URLs continue to exclude it.

## Implementation

### 1. Catalog and bounded event projections

- Add `timeline` and `collector_health` Events presets; retain existing stable
  presets and lazy message/detail behavior.
- Bump the Events view revision because the published preset set changes.
- Test preset order, columns, default cell count, availability, and sort columns.

### 2. Events Signals panel

- Add `EventsSignalPanel` over the selected `/v1/timeline/events` range.
- Render a compact summary, maximum 6 chronological event lanes, explicit
  quality badges, occurrence counts, and loss/provenance disclosure.
- Route entity kinds to the nearest investigation screen without passing the
  opaque event entity id as a frame entity token.
- Provide loading, empty, partial, and error states inside the fixed 156 px
  analytical center.

### 3. Events workspace integration

- Add prepared Event lenses, default `timeline`, 96-bucket heatmap, and the
  two-column analytical center.
- Apply honest machine filters to the ranked frame for family lenses.
- Keep Config changes gated with a localized reason.
- Add English/Russian labels, evidence notes, quality descriptions, and parity.

### 4. Accessibility and interaction audit

- Audit changed shell/components against the pinned Web Interface Guidelines.
- Add a visible-on-focus skip link to the main analytical content.
- Give search controls stable `name`, `autoComplete="off"`, and
  `spellCheck={false}`; preserve explicit labels and paste behavior.
- Verify semantic buttons, accessible event names, focus visibility, reduced
  motion, modal/drawer containment, and no icon-only unlabeled controls.
- Ensure dynamic Event results use a polite live region without announcing the
  entire table on every fetch.

### 5. Production regression and budgets

- Extend deterministic demo facts with error, checkpoint, maintenance, slow,
  lifecycle, and collector-health families plus exact/derived/loss examples.
- Chromium: verify all eight top-level screens share time geometry and Health
  line; Events must fit at 1920×1080 and 1440×900 with document `scrollY=0`.
- Verify keyboard-only path: skip link, navigation, lens selector, signal lane,
  matrix row, Entity Detail or investigation target, status strip.
- Keep 1,000 Statements virtualized, Events response bounded, and embedded
  archive under 256 KiB; capture approved screenshots.

## Risk-to-evidence matrix

| Risk                                             | Boundary             | Oracle                                                                        |
| ------------------------------------------------ | -------------------- | ----------------------------------------------------------------------------- |
| Lens claims filtering but table shows all facts  | App integration      | frame URL contains the expected machine filter; user filter overrides it      |
| Unbounded timeline rendering                     | component + browser  | request limit is 50; rendered lanes never exceed 6                            |
| Opaque event identity is misused as row identity | component            | investigation callback receives a view and no frame entity token              |
| Missing/partial evidence looks exact             | component            | quality/loss/identity fields stay visible for exact, partial, and empty cases |
| Events displace heatmap                          | App + Chromium       | 96 heatmap buckets and Signals panel coexist in 156 px center                 |
| Keyboard/screen-reader regression                | component + Chromium | semantic roles, labels, live region, tab order, skip-link focus               |
| Viewport overflow                                | Chromium             | root height equals viewport and `scrollY=0` at 1920×1080 and 1440×900         |
| Localization drift                               | parity test          | English and Russian key sets remain identical                                 |
| Performance/bundle regression                    | existing gates       | 1,000-row virtualization/input latency and 256 KiB archive budget pass        |

## Verification order

1. Focused catalog and `EventsSignalPanel` tests.
2. Focused App integration and localization parity tests.
3. `make web-frontend-check` under Node 22.
4. Production build and Chromium shell verifier at both viewports.
5. Rust format, `pg_kronika-web` tests, and clippy on
   `aarch64-apple-darwin`.
6. Deterministic embedded archive comparison, bundle budget, and static-serving
   tests.
7. Independent correctness and accessibility review before PR publication.
