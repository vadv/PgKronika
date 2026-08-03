# Statements visual convergence — design QA

## Comparison target

- Source visual truth: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-statements-overview.png`
- Implementation: `/Users/vadv/Projects/PgKronika-worktrees/pr08-events-signals-polish/web/demo/shots/forensic-shell-1920x1080.png`
- Viewport and density: both images are 1920 × 1080 pixels, 1920 × 1080 CSS px, DPR 1. No density normalization was required.
- State: dark theme, Statements workload screen, one-hour replay range, ranked statements by time, first statement selected, 96 temporal buckets.
- Browser evidence: the production build was rendered by the shell verifier at the stated viewport. Root geometry was exactly 1920 × 1080 with no page scroll; the matrix owned its overflow. The verifier also exercised filtering, five 200-row continuations, keyboard row navigation, detail/history/relations, alternate statement metrics, and the 1440 × 900 compact layout. Browser console and page-error diagnostics were checked; none were reported.

## Full-view comparison evidence

The source and implementation were opened together at original resolution. The implementation preserves the source's key visual model: a single PG + OS health line above a dense ranked statement/time matrix, with time evidence coupled to each entity row. The matrix occupies the remaining viewport instead of competing with a detached chart. Its 272 px identity band, compact numeric columns, and 96-bucket evidence band keep the same left-to-right investigation flow as the source.

The implementation deliberately carries PgKronika's existing navigation, replay controls, provenance, data-quality semantics, and entity-detail contract instead of copying decorative source chrome. Dynamic demo values and query identities therefore differ from the mock, but their hierarchy and density are equivalent.

The native-resolution full-view comparison was sufficient for the dense row region: both captures expose readable column labels, counters, bucket cells, selection, and state colors at 1:1 pixels. No additional crop was needed. Geometry assertions provide the focused matrix evidence: 28 fully visible 27 px rows, 39 virtualized DOM rows for 1,000 loaded statements, 96 cells per rendered row, and a temporal band wider than 45% of the matrix.

## Required fidelity surfaces

- Fonts and typography: compact UI and monospace data roles are consistent with the incumbent design system. The two-line statement identity stays inside a true 27 px row; large counters use stable `k/M/B/T` widths while their exact values remain available in the cell tooltip. Labels do not wrap into adjacent controls at either tested viewport.
- Spacing and layout rhythm: the 44 px global header, 32 px navigation, 60 px Health Line, 72 px Statements context, 31 px matrix controls, and viewport-owned matrix form a clear vertical rhythm. Root content does not scroll at 1920 × 1080 or 1440 × 900.
- Colors and tokens: the implementation uses the existing semantic tokens. Blue communicates retained temporal intensity, amber/red mark pressure, green/amber/red remain status colors, missing series have a distinct unavailable treatment, and selection is readable without masking the heatmap.
- Image quality and asset fidelity: neither target depends on photographic or illustrated assets. Visible evidence is native UI data, text, and tokenized chart rendering; no placeholder image, emoji, custom logo, or fake illustration substitutes an asset from the target.
- Copy and content: `Ranked history`, explicit lens/metric controls, `heatmap (96 buckets)`, retained/missing-series counts, sample quality, and provenance make the screen explain both what is ranked and how trustworthy the temporal evidence is. SQL text remains detail-only by product constraint rather than being duplicated across 1,000 dense rows.
- Icons and controls: the incumbent icon set and semantic controls remain aligned and keyboard reachable. Filter, metric, lens, replay, selection, search, and detail affordances are functional.
- Accessibility and resilience: row selection has keyboard support, temporal cells are not 96 extra tab stops per row, missing evidence is not encoded as zero, reduced-motion behavior is verified, and persistent controls remain in the viewport.

## Comparison history

1. Initial comparison — blocked
   - [P1] The implementation used a detached analytical heatmap above the table, so the visual relation between a statement and its history was indirect.
   - [P1] The first row implementation measured 43.78 px because the two-line identity inherited loose line height, leaving too few statements visible.
   - [P2] Full-width `calls` and `rows` counters clipped in the dense numeric band.
   - [P2] The browser check used a single search-latency sample and could fail on an isolated GC pause.
2. Fixes made
   - Replaced the detached Statements analytical center with one row-coupled ranked time matrix.
   - Added exact 96-bucket temporal rows, unavailable-series semantics, and cursor/baseline markers.
   - Set actual statement row geometry to 34 px and kept rendering bounded by virtualization.
   - Added compact high-volume counters with exact raw values in tooltips.
   - Kept the 100 ms sustained search contract but measured the warmed median of five samples.
3. Post-fix comparison — passed
   - Same-state 1920 × 1080 comparison shows the Health Line and statement/time relationship above the fold with no detached chart.
   - Browser geometry initially confirmed 22 full rows, 96 buckets per rendered row, 1,000 loaded statements, and no root overflow.
   - The compact 1440 × 900 check retains the same hierarchy and confines horizontal overflow to the matrix.
4. Independent finish review — fix
   - The reviewer requested durable product/direction records, an explicit combined PostgreSQL + OS Health identity and current verdict, Statements-first workload navigation, and source-level matrix density.
   - `PRODUCT.md` and the production-surviving five-block direction contract now record the durable product and surface decisions.
   - The top evidence line now says `Health · PostgreSQL + OS`, shows the current verdict separately from the window score, and Workload navigation orders Statements, Activity, Plans.
   - Matrix rows were reduced to a readable 27 px with 28 fully visible rows; DOM rendering remains bounded at 39 rows.
5. Reviewer verdict pass — ship
   - The reviewer scored all five material fixes resolved and found no visual regression in the regenerated 1920 × 1080 and 1440 × 900 captures.

## Findings

No actionable P0, P1, or P2 visual mismatch remains. The source's inline normalized fingerprint is intentionally represented by a compact statement identity in the matrix and full SQL in Entity Detail; this preserves response bounds and SQL-visibility semantics rather than being an unfinished visual omission.

## Open questions

None for this Statements convergence slice.

## Implementation checklist

- [x] One PG + OS Health Line remains visible.
- [x] Ranked statement and temporal evidence share the same row.
- [x] 1,000 statements stay server-paged and DOM-virtualized.
- [x] Search, lenses, metrics, selection, and Entity Detail remain functional.
- [x] 1920 × 1080 and 1440 × 900 browser contracts pass.
- [x] Missing evidence, sample quality, and provenance are explicit.

## Follow-up polish

The same ranked time-matrix grammar can now be applied screen-by-screen to Activity and Plans where row-level temporal evidence is available; snapshot-only evidence must retain its point-in-time wording.

final result: passed

---

# Plans visual convergence — design QA

## Comparison target

- Source visual truth: `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-plans-regression-refined.png`
- Browser-rendered implementation: `/Users/vadv/Projects/PgKronika-worktrees/pr11-plans-visual-convergence/web/demo/shots/forensic-plans-1920x1080.png`
- Viewport and density: both images are 1920 × 1080 pixels, rendered at
  1920 × 1080 CSS px and DPR 1.
- State: dark theme, one-hour replay range, Plans Regression evidence lens,
  1,000 retained plan rows, 64 retained temporal series, 96 buckets per
  rendered plan row and three bounded first/last-observed records.
- Browser evidence: the production bundle was rendered by the deterministic
  shell verifier. Root geometry is exactly 1920 × 1080 with no page scroll;
  the plan matrix owns its vertical overflow. The verifier also exercised the
  Changes lens, fork provenance, Compare gating, temporal request geometry and
  the existing Activity, infrastructure, Events, search and detail flows.

## Full-view comparison evidence

The source and implementation were opened together at original resolution in
one comparison input. Both keep the combined PostgreSQL + OS Health Line as the
only upper chart, place Plans investigation context immediately below it, and
make plan identity, cost and temporal position scannable in one horizontal
flow.

The implementation intentionally translates the source's three-row incident
story into the user-required dense population view. The source's plan-mix,
mean-latency and buffer charts become exact time/calls cells beside each of the
1,000 ranked plans; 36 virtualized plan rows are visible at once. This keeps
the heatmap as an at-a-glance relationship surface without separating it from
the plan entity it describes.

The source's A/B Plan Tree Diff and investigation finding are intentionally not
rendered. The current server does not publish two bounded plan trees, a stable
pairing contract, or a typed diff. Compare remains visible and gated with that
reason. Likewise, Regression evidence says it is ranked by current interval
mean and that temporal coincidence is not a computed before/after baseline.
These are evidence-bound differences, not unfinished visual regions.

## Required fidelity surfaces

- Fonts and typography: the incumbent interface/monospace pairing preserves
  the operational-console character of the source. Plan and query ids use
  compact tokens with full values in the identity tooltip. Mean time now keeps
  the source unit correctly as milliseconds per call.
- Spacing and layout rhythm: 44 px global context, 32 px navigation, 60 px
  Health Line, 72 px screen context, 38 px Plans evidence, 76 px observed-plan
  rail and the viewport-owned matrix form a dense but legible hierarchy. The
  root does not scroll.
- Colors and visual tokens: blue expresses temporal intensity; amber and red
  remain evidence pressure; cyan marks identifiers and fork attribution.
  Missing series are distinct from zero and the selected/verdict tint does not
  obscure the time lane.
- Copy and content: `Regression evidence`, `first / last observed only`, both
  fork-specific attribution methods, retained/matched coverage and the
  explicit no-baseline statement bound the operator's inference. Long boundary
  copy remains available in the title when compact layout ellipsizes it.
- Controls and accessibility: lens, metric, plan row and observed-envelope
  controls are keyboard reachable. The 96 individual evidence cells do not
  become tab stops. Compare is visibly disabled instead of disappearing.
- Density and resilience: the first 200 rows are server-paged, the DOM stays
  virtualized, 36 temporal rows are rendered, each contains exactly 96 cells,
  and heatmap failure leaves the ranked plan frame usable.

## Comparison history

1. Initial implementation comparison — fix.
   - The old screen separated a generic heatmap from a generic plan table and
     confined fork/change evidence to a small unrelated right card.
   - The deterministic demo contained only ten plan rows and did not populate
     the Buffers lens.
2. Convergence fixes.
   - Replaced the detached analytical center with one dedicated Plans
     workspace and row-coupled temporal matrix.
   - Added bounded first/last-observed records, current-interval regression
     boundary, OSSC/vadv provenance, exact time/calls controls and a visible
     gated Compare lens.
   - Raised the representative population to 1,000 plans and populated calls,
     rows, shared hits/reads and partial evidence.
   - Corrected the public plan mean unit from unspecified/mislabeled µs to the
     collected `pg_store_plans.total_time` millisecond unit and bumped the Plans
     view revision.
3. Final same-state comparison — passed.
   - The 1920 × 1080 implementation shows Health Line, all Plans controls,
     bounded plan observations and 36 ranked plan/time rows without root
     scrolling.
   - The shell verifier confirms 200 loaded rows, independent matrix scroll,
     96 buckets per row, no detached center, at most three change records,
     both fork attributions, default Regression evidence and gated Compare.

## Findings

No actionable P0, P1 or P2 visual mismatch remains. The omitted A/B diff,
plan-node blame and synchronized buffer history require new typed backend
contracts; presenting them now would make the UI more similar but less true.

## Open questions

None for this Plans convergence slice. A later backend stack may publish a
typed two-tree comparison and stable baseline pairing; that should activate the
existing Compare affordance rather than introducing a second screen.

## Implementation checklist

- [x] Health Line is the only upper chart.
- [x] Plan identity and temporal evidence share one row.
- [x] 1,000 plans remain paged and DOM-virtualized.
- [x] Time/calls, lenses, filtering, selection and Plan Detail remain
  functional.
- [x] First/last observations do not claim continuous execution.
- [x] OSSC and vadv attribution semantics remain distinct.
- [x] 1920 × 1080 geometry, browser console and key interactions pass.

final result: passed

---

# Activity visual convergence — design QA

## Comparison target

- Source visual truth:
  - `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-activity-overview.png`
  - `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-activity-cpu.png`
  - `/Users/vadv/Projects/PgKronika/output/playwright/pgkronika-activity-waits-refined.png`
- Browser-rendered implementation:
  - `/Users/vadv/Projects/PgKronika-worktrees/pr08-events-signals-polish/web/demo/shots/forensic-activity-1920x1080.png`
  - `/Users/vadv/Projects/PgKronika-worktrees/pr08-events-signals-polish/web/demo/shots/forensic-activity-cpu-1920x1080.png`
  - `/Users/vadv/Projects/PgKronika-worktrees/pr08-events-signals-polish/web/demo/shots/forensic-activity-waits-1920x1080.png`
- Viewport and density: every source and implementation image is 1920 × 1080 pixels, rendered at 1920 × 1080 CSS px and DPR 1. No density normalization was required.
- State: dark theme, one-hour replay range, Activity Overview / CPU / Waits & Locks lenses, 36 retained activity rows, 96 temporal buckets per row. Active fraction is observation-derived; CPU, I/O, and wait values are interval-derived from consecutive observations.
- Browser evidence: the production bundle was rendered by the deterministic shell verifier and separately exercised in the in-app browser. Root geometry is exactly 1920 × 1080 with no page scroll. The primary interactions tested were Activity lens switching, automatic lens-to-metric synchronization, row selection, entity summary/relationships, Activity → related OS process drill-down, and Waits & Locks edge inspection. Console errors and page errors were checked by the verifier; none were reported.

## Full-view comparison evidence

The three source/implementation pairs were opened together at original resolution. The implementation preserves the target's investigation order: combined PostgreSQL + OS Health Line, Activity lens and point-snapshot context, PG identity/state, an explicit PID relationship boundary, OS process evidence where available, and dense rows filling the remaining viewport.

The implementation intentionally adds the user-required temporal relationship surface that the Overview/CPU source captures do not yet show: every Activity row carries 96 aligned evidence cells. The surface names observation-derived cells as samples and CPU/I/O/wait cells as derived intervals, while keeping missing evidence distinct from zero. This is a product-correct extension, not design drift.

The implementation deliberately does not reproduce two analytically unsafe parts of the visual sources. CPU does not invent user/system split, run-queue delay, or context-switch values absent from the public contract; it renders total process CPU, RSS, threads, command, and an explicitly interval-derived CPU lane. Waits & Locks does not turn sparse observations into a causal Gantt or confidence score. It presents independently bounded lock edges as `edge_only` above interval-attributed wait lanes. These differences preserve PgKronika's evidence semantics and the user's explicit requirement that visual coincidence must not become asserted correlation.

## Focused-region comparison evidence

The Activity context and first rows were compared at 1:1 pixels using matched 1920 px-wide crops:

- `/tmp/pgkronika-designqa-pr10/source-overview-focus.png`
- `/tmp/pgkronika-designqa-pr10/implementation-overview-focus.png`
- `/tmp/pgkronika-designqa-pr10/source-waits-focus.png`
- `/tmp/pgkronika-designqa-pr10/implementation-waits-focus.png`

The focused comparison confirms that the two-level column grouping is readable once per contiguous evidence group, PID identity remains the scan anchor, `best_effort · same_snapshot_pid_only` is visible above the matrix, lock evidence is visually separated from sampled wait history, duration verdict tinting does not obscure values, and the 96-bucket lanes remain individually distinguishable.

## Required fidelity surfaces

- Fonts and typography: the existing UI/monospace pairing matches the source's operations-console character. Uppercase evidence headers, compact labels, numeric alignment, 34 px two-line Activity identities, ellipsis, and optical weights remain readable without row-height expansion. Full values and provenance remain available through titles and Entity Detail.
- Spacing and layout rhythm: the 44 px global header, 32 px navigation, 60 px Health Line, 72 px screen context, 38 px snapshot evidence strip, optional 72 px lock-edge strip, and viewport-owned matrix produce a stable hierarchy. Overview exposes 33 rendered rows and Waits exposes more than 18 at 1920 × 1080; the root never scrolls.
- Colors and visual tokens: blue represents temporal evidence intensity, red/amber verdicts remain semantic, green labels distinguish OS evidence, and cyan labels distinguish the PID relation boundary. Missing process evidence renders as null rather than a zero or fabricated join.
- Image quality and asset fidelity: neither source uses photographic, illustrative, logo, or bespoke image assets. All visible data marks are native text, table, and chart rendering; no placeholder imagery, emoji, CSS illustration, or fake SVG replaces a source asset.
- Copy and content: `point snapshot`, the short-query sampling caveat, `observed samples`, `derived intervals`, `best_effort`, `same_snapshot_pid_only`, and `edge only · point snapshot` explain what the operator can and cannot infer. Cell tooltips state whether a value is an observation or is derived from consecutive observations. English and Russian strings are supplied for the surface.
- Icons and controls: lens and metric controls retain the incumbent compact control language. Active, disabled, focus, and selected states are distinct; unavailable Memory and XID/Horizon lenses remain visible with reasons instead of disappearing.
- Accessibility and resilience: Activity rows are keyboard selectable, 96 temporal cells do not become 96 tab stops, control groups have accessible labels, missing series have explicit unavailable semantics, reduced motion is honored, and persistent shell controls remain above the fold.

## Comparison history

1. First same-state Activity comparison — passed.
   - No actionable P0, P1, or P2 visual mismatch was found across Overview, CPU, or Waits & Locks.
   - Before capture, browser verification caught and fixed three functional/visual convergence defects: CPU lens retained the active-fraction metric, repeated column-group labels created header noise, and related-process navigation retained an invalid Activity preset.
   - Post-fix evidence shows one label per evidence group, CPU-selected temporal lanes and requests, and an Activity → OS process detail transition with view-scoped parameters cleared.
2. Independent review and post-review comparison — passed.
   - Expanded list-valued `blocked_by` evidence into individual waiter → blocker edges, discarded root rows, preserved blocker PID `0` as the prepared-transaction holder marker, deduplicated edges, and retained a three-edge visual bound. The strip follows frame cursors until it finds three edges or exhausts the result, so leading root nodes cannot produce a false empty state.
   - The fresh Waits capture shows a regular blocker, a prepared-transaction blocker, and a third independent edge without overflow.
   - Preset-scoped metric state now follows URL history, cross-view relations clear incompatible filters, and the Activity grouped table reports both header rows to assistive technology.
   - Active-fraction lanes remain labeled as observed samples; CPU, I/O, and wait lanes now say `derived intervals` and disclose consecutive-observation provenance in each cell tooltip.
   - Failed lock continuations stop automatic pagination and expose an explicit retry instead of entering a request loop; Activity toolbar and metric descriptions use the same point-versus-interval vocabulary as the matrix.
   - The three fresh source/implementation pairs were reopened together at original 1920 × 1080 resolution. No actionable P0, P1, or P2 mismatch was introduced.

## Findings

No actionable P0, P1, or P2 mismatch remains. The richer CPU and Waits analytics visible in the conceptual source are intentionally bounded by currently truthful public evidence, while the implementation adds the required row-coupled point-sample matrix and explicit uncertainty model.

## Open questions

None for this Activity slice. Future backend contracts may add truthful CPU user/system, run-queue, context-switch, or time-weighted wait-class evidence; those should become new prepared columns/lenses rather than inferred UI values.

## Implementation checklist

- [x] Activity is one combined PG + process-evidence + temporal matrix.
- [x] Overview, CPU, and Disk lenses select a relevant temporal metric.
- [x] Waits & Locks keeps exact edges separate from interval-attributed wait history.
- [x] Unique same-snapshot PID is the only process attribution path; ambiguous joins remain null.
- [x] Activity → related process opens a real Entity Detail with invalid Activity lens state cleared.
- [x] 1920 × 1080 root, 96 buckets, row density, browser console, and key interactions pass.

## Follow-up polish

- [P3] The Overview relation cells ellipsize `best effort` in the most column-heavy layout. The full relationship class is already visible in the evidence strip and cell title; a later density pass can trade 12–16 px from the command column to expose it inline.
- [P3] The screen-context sampling note truncates after the central matrix has claimed space. Its full meaning is duplicated in the point-snapshot strip; a later tooltip pass can make the toolbar copy independently discoverable.

final result: passed
