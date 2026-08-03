# PR11 — Plans Visual Convergence

## Outcome

Replace the detached Plans heatmap, generic table and small unrelated evidence
card with one dense forensic workspace. An operator must be able to see which
stored plan versions dominate the selected interval, when each version was
observed, which versions are expensive, and which attribution claims are safe
for the collector fork in use.

The baseline viewport is 1920×1080. Health Line remains the only upper chart.
The Plans workspace owns the remaining height and must not make the document
scroll.

## Visual target

The selected reference is:

- `output/playwright/pgkronika-plans-regression-refined.png`

The current shipped implementation was compared with that reference at the
same 1920×1080 viewport. PR11 keeps the reference's dense hierarchy — compact
context, plan-change evidence and a ranked plan matrix — while translating it
to the existing visual system in `DESIGN.md` and to the evidence the current
API can actually prove.

The reference's A/B tree diff and synchronized statement-buffer charts are not
copied as decorative UI. The current typed contract cannot prove either claim.
Compare therefore remains visible but gated until two plan trees and a stable
pairing contract are available.

## Information architecture

### Shared Plans context

The existing screen header and lens toolbar stay in place. The body becomes one
`PlansWorkspace` with:

1. a compact evidence strip describing the active lens, collector-fork
   attribution and temporal coverage;
2. a bounded first→last-observed evidence rail for lenses concerned with
   regression or change;
3. one viewport-owned ranked matrix where plan identity, interval aggregates
   and time/calls evidence share each plan row.

There is no second detached Plans heatmap. Row-coupled temporal lanes make
visual coincidence available without claiming that a plan caused a Health
Line change.

### Lenses

- **Regression evidence** ranks the current interval by mean time and places
  temporal evidence beside each plan. It is the default investigation lens,
  but it is not presented as a computed before/after regression.
- **Execution** emphasizes total plan time, calls and mean time.
- **Buffers** emphasizes PostgreSQL shared-buffer hit/read evidence. It does
  not rename those counters as Linux disk I/O.
- **Rows** emphasizes returned rows and calls.
- **Changes** emphasizes first and last observed calls and shows the bounded
  plan-change rail.
- **Compare** remains visible and disabled with an explicit explanation that
  the backend does not yet publish a typed two-tree diff.

All available lenses remain URL-addressable. Lens copy describes what is
ranked or observed, not an inferred causal result.

## Matrix grammar

- Sticky plan identity combines `planid` with `queryid` when that attribution
  is available for the active collector fork.
- Aggregate columns follow the selected catalog preset and remain sortable.
- Every row contains exactly 96 temporal buckets on desktop and 48 on mobile.
- The temporal metric is explicitly **time** or **calls**. The shipped Plans
  heatmap does not invent mean-time or buffer buckets that the API lacks.
- A coloured bucket means a positive interval delta was observed for that plan
  entity. A blank bucket is missing or no positive delta; it is never treated
  as proof that the plan did not exist.
- Rows remain virtualized, horizontally navigable and independently scrollable
  for populations around 1,000 plan-stat rows.
- Stored plan text stays in universal Entity Detail and is loaded lazily. It is
  not duplicated into every matrix row.

## Change evidence rail

Regression evidence and Changes show at most three records from the existing
`change_timeline` frame. Each record contains plan identity, optional query
identity, first observed call, last observed call and available calls/mean
evidence.

The rail is an observation envelope, not a continuous plan-active interval.
Its copy must say “first observed” and “last observed”; it must not imply that
the plan executed continuously between those timestamps or that adjacent
records form a proven transition.

## Fork-aware attribution

Plans retain the collector-specific `best_effort` semantics already exposed by
the server:

- **OSSC pg_store_plans:** `(dbid, userid, queryid, planid)` supplies the
  strongest available plan/query attribution.
- **vadv pg_store_plans:** `(dbid, userid, planid)` is plan identity;
  `queryid_stat_statements` is the last executing query and remains visibly
  weaker, not a guaranteed per-query attribution.

The workspace exposes those provenance labels in context and never collapses
both forks into one stronger-looking “exact” relation.

## States and accessibility

- Loading, heatmap error, empty rows, partial bucket coverage, missing query id
  and unavailable plan comparison remain explicit.
- Lens, metric, rail record and row controls retain visible focus and keyboard
  operation.
- The matrix exposes its real row count and keeps sticky headers and identity
  readable during vertical and horizontal navigation.
- Quality copy distinguishes retained entities from matched time series. A
  partial match is useful evidence, not silently rendered as complete.

## Acceptance

- At 1920×1080 Health Line, Plans context, evidence strip and at least 18
  ranked rows are visible without root scroll.
- Plans has no detached analytical-center region.
- Regression evidence, Execution, Buffers, Rows and Changes are materially
  distinct and URL-addressable.
- Every rendered temporal row contains exactly 96 buckets on desktop.
- Time/calls selection updates the row-coupled temporal lanes.
- The change rail contains at most three first→last observed records and uses
  non-continuous observation language.
- Both OSSC and vadv attribution semantics are visible.
- Compare remains visibly gated until a truthful typed A/B contract exists.
- Frontend and Rust tests, formatting, lint, clippy, visual verifier and bundle
  budget pass.
