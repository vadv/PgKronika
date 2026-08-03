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
