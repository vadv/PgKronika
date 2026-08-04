# Data Maintenance Forensic Detail Design

## Status

Approved direction carried forward from the published Superdesign reference at
`https://pgkronika-forensic-u.superdesign.cloud/` and the operator feedback in
the PgKronika UX/UI thread. This document narrows that approved system to the
Tables, Indexes, and Vacuum surfaces.

## Goal

Turn Tables, Indexes, and Vacuum from sparse ranked lists into one coherent
forensic workflow: scan a dense population, select an entity, and investigate
its time-shaped PostgreSQL evidence and nearby related entities without leaving
the Health Line or the selected time window.

## Visual Target

The selected-entity state follows the approved table-detail frame:

1. The global context, grouped navigation, and 60 px Health Line stay fixed.
2. A compact entity strip replaces the normal page header and filter toolbar.
3. A full-width temporal evidence field occupies the upper half of the working
   surface. It contains several aligned metric lanes, the current cursor, and
   related-event markers on one time geometry.
4. The lower half is divided into three ruled analytical columns:
   primary measurements, maintenance/state measurements, and related evidence.
5. The status bar remains visible at 1920×1080; the root page never scrolls.

The unselected state remains the population browser: prepared lenses, search,
the existing 96-bucket heatmap, nearby infrastructure evidence, and the ranked
table. Selection transforms the analytical body into detail; it does not open a
small generic side panel on top of the population.

## Interaction

- Clicking or pressing Enter on a Tables, Indexes, or Vacuum row opens the
  inline forensic detail and keeps `view`, `entity`, `dock=row`, cursor time,
  and lens in the URL.
- Escape and the visible close control return to the population without losing
  the selected row or time context.
- Related entities are normal buttons. Opening one changes the view/entity and
  pins the relation's recorded snapshot time.
- Missing history or a missing measurement is shown locally as “not collected”
  or “no samples in this window”. The primary screen does not surface API
  vocabulary, entity tokens, gap counters, gating counters, or proof language.
- Partial collection is a calm compact state adjacent to the affected evidence;
  it does not block relation navigation or dominate the screen.

## Data Contract

The detail consumes only existing bounded endpoints:

- point: `/v1/entity/{view}/{entity}?at=…&include=related`;
- history: `/v1/entity/{view}/{entity}?from=…&to=…&columns=…&limit=96`.

History requests use at most six available, non-lazy columns and at most 96
snapshots. They never materialize an unbounded population. The time window is
the currently selected span, capped at six hours for detail rendering.

Prepared field groups are view-specific:

- Tables: access and buffer behavior; churn, maintenance, XID/MXID; related
  vacuum/index evidence.
- Indexes: usage and buffer behavior; size and recency; owning table evidence.
- Vacuum: phase and progress; dead-item generation fields; owning table
  evidence.

The UI must not invent metrics the API does not expose. A reference section
such as “Observed statements” may appear only when an actual related entity is
returned; otherwise the boundary is stated plainly.

## Component Boundaries

- `DataMaintenanceDetail.tsx` owns bounded point/history loading, view-specific
  grouping, temporal lanes, related navigation, and detail loading/error/empty
  states.
- `DataMaintenanceDetail.css` owns the viewport-contained reference layout and
  its compact adaptation.
- `App.tsx` owns the route decision: inline detail for desktop data-maintenance
  entities; the existing reusable dock everywhere else.
- The existing `InfrastructureEvidencePanel` and `TableView` remain the
  population browser and are not duplicated inside detail.

## Accessibility

- Close and related-entity actions are keyboard reachable and visibly focused.
- Charts have human `aria-label` summaries and retain numeric latest values in
  text; color is never the only state carrier.
- The visual order, DOM order, and keyboard order are entity strip → temporal
  evidence → the three analytical columns.
- Reduced motion keeps the layout static.

## Verification

- Component tests cover view-specific groups, bounded history parameters,
  local missing states, related navigation, and the absence of raw technical
  vocabulary.
- App tests cover inline routing and suppression of the generic row overlay.
- The full frontend gate and bundle budget remain green.
- Browser QA compares the approved reference and the rendered selected-table
  state at the same desktop viewport, with 1920×1080 as the baseline.

