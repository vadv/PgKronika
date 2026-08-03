# PR5: Global forensic search and universal Entity Detail

## Outcome

Add a slash-command search that federates the existing bounded frame
projections and a reusable, non-reflowing Entity Detail drawer. Search and
detail must preserve the shared time cursor, expose evidence limitations, and
never serialize free-form text or SQL into the share URL.

## Binding semantics

- Search runs against server frames, never only against rows already loaded in
  the active table. Every result group keeps its own opaque continuation.
- The current server filter contract remains `field=value`, AND terms,
  case-insensitive full-value glob matching. The palette translates a bounded
  `key:value` convenience syntax only when the target column exists and is not
  lazy.
- Lazy query/plan text is excluded from global search. It may appear in Detail
  when the point endpoint returns it.
- Search reports retained frame population and provenance. It does not claim
  coverage beyond the selected snapshot/top-N source population.
- Unsupported keys remain visible with an exact reason. Client-side fuzzy
  matching must not fabricate OID/device evidence absent from the catalog.
- Share state may contain typed entity token, view, range, lens, and sort. The
  palette query and all other free-form strings remain transient.
- Related entities are navigable only from the server-provided relation list;
  relation kind and method are always visible.

## Implementation slices

1. Add pure, tested search admission and compilation: 256 UTF-8 bytes, at most
   16 terms, catalog-aware aliases, free-text glob escaping, and explicit
   unsupported-key diagnostics.
2. Add an infinite-query search group over `/v1/frame/{view}` with a 20-row
   page, dedupe, exact `matched`, cursor continuation, and stable query key.
3. Add the `/` command palette with grouped results, match reason, evidence
   source, keyboard focus, Enter drill-down, Escape close, and an explicit
   button in the global header.
4. Extend entity history client arguments and split the drawer into Summary,
   History, Relationships, and Raw evidence. Fetch history only when selected,
   cap it to the shared range and six useful non-lazy columns, and retain point
   evidence while history loads.
5. Keep the desktop drawer fixed at 520 px (clamped 480–560) and the mobile
   bottom sheet. Opening it must not change the matrix geometry.
6. Add unit/component regressions, a 1920x1080 Chromium scenario, localization,
   deterministic embedded bundle, and Rust/OpenAPI checks for unchanged wire
   compatibility.

## Acceptance gates

- `/` focuses search; arrows traverse results; Enter opens the selected entity;
  Escape closes the top layer and restores focus.
- A group with more than 20 matches loads the next server page without using
  or filtering the active table rows.
- `queryid`, `planid`, `pid`, relation/index, wait/event, database/user/app, and
  cgroup aliases target only compatible catalog views. Unsupported OID/device
  search explains why no public searchable field exists.
- Summary, History, Relationships, and Raw evidence work for Statement Detail;
  the same component anatomy accepts process/table/index/vacuum views.
- The matrix bounding box is unchanged before and after opening the 520 px
  detail drawer at 1920x1080.
- Search text and lazy SQL never appear in `location.hash` or the share URL.

