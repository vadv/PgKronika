# Data Maintenance Forensic Detail — Design QA

## Final result

Passed.

The selected Tables state was compared with the approved Superdesign reference
at the same 1280×720 browser viewport. The production shell was then verified
at the required 1920×1080 baseline for Tables, Indexes, and Vacuum.

## Reference comparison

Reference: `https://pgkronika-forensic-u.superdesign.cloud/`

Local comparison artifacts (generated, not committed) are retained in the
sibling `pr205-data-maintenance-fidelity-qa/` directory:

- `reference-1280x720.png`
- `prototype-final-1280x720.png`
- `comparison-final.png`

The implementation follows the reference composition:

- persistent global context, grouped navigation, and 60 px Health Line;
- compact entity/risk strip;
- three aligned, full-width temporal lanes with multiple signals and one
  related-event lane;
- three ruled analytical columns for the history matrix, maintenance state,
  and related entities;
- dense 4 px rhythm, small radii, restrained borders, and existing PgKronika
  color/type tokens.

Intentional differences are data-contract differences, not visual invention:

- the existing PgKronika application chrome remains intact;
- only related entities returned by the API are shown;
- Vacuum states whose catalog declares no history show current measurements
  and a local “history is not collected” note instead of fabricated traces.

## 1920×1080 contract

The real-browser shell verifier recorded the following for all three detail
states:

| Check | Tables | Indexes | Vacuum |
| --- | ---: | ---: | ---: |
| Root geometry | 1920×1080 | 1920×1080 | 1920×1080 |
| Root scroll | 0 | 0 | 0 |
| Detail bounds | y 136–1056 | y 136–1056 | y 136–1056 |
| Temporal lanes | 3 | 3 | 3 |
| Analysis columns | 3 | 3 | 3 |
| Generic row dock mounted | no | no | no |
| Population overview behind detail | no | no | no |

Generated shell screenshots:

- `web/demo/shots/forensic-table-detail-1920x1080.png`
- `web/demo/shots/forensic-index-detail-1920x1080.png`
- `web/demo/shots/forensic-vacuum-detail-1920x1080.png`

## Data and interaction checks

- Tables history: `limit=96`, six columns, six-hour cap.
- Indexes history: `limit=96`, four columns, six-hour cap.
- Vacuum without history capability: no history request; missing state remains
  local to the temporal field.
- Related-entity buttons preserve the recorded snapshot time and open the next
  entity in the same full-canvas investigation flow.
- Escape and the visible close button remove only `dock=row`, preserving the
  selected entity and cursor context.
- Raw entity tokens, endpoint paths, provenance methods, gap/gated counters,
  and proof language are absent from the detail surface.
- The Impeccable mechanical scan returned no findings for the new component and
  stylesheet.

## Visual findings resolved

1. Replaced the first uniform six-row sparkline draft with three composite
   temporal lanes matching the reference hierarchy.
2. Added the reference-style current / window delta / baseline matrix.
3. Added large key-stat cards and full-width related-entity cards.
4. Rebalanced the temporal field for the 1920×1080 baseline while keeping the
   reference proportion at compact desktop height.
5. Replaced heavy native scrollbars with thin internal analysis scrollbars;
   the root page remains fixed.

## Mandatory review panel

- **DevOps / packaging:** deterministic `static.tar.gz` is 189,330 bytes
  against the 262,144-byte budget; no dependency or deployment contract
  changed.
- **DBA:** selection issues one entity point query and, only when advertised by
  the catalog, one history query capped at 96 snapshots × 6 metrics × 6 hours.
  No population or SQL-text expansion is introduced.
- **SRE / failure behavior:** point failures, history failures, empty history,
  and catalog-disabled history remain local. A disabled history query no longer
  leaves an indefinite loading indicator.
- **PostgreSQL semantics:** Tables, Indexes, and Vacuum use different prepared
  field groups. The UI does not infer physical relation I/O or invent a related
  statement/index when the API returns none.
- **Frontend / Rust performance:** no Rust path changed. The largest detail
  payload is 576 scalar history values plus one bounded point response; the DOM
  contains three charts and at most six matrix rows.
- **Memory bounds:** TanStack Query holds one bounded point and one bounded
  history result per opened entity under its existing cache lifetime. No
  unbounded list, pagination accumulator, timer, or subscription was added.
- **Comment quality:** new comments explain the non-obvious SPA focus-reset
  behavior in the shell verifier. The component relies on names and types for
  ordinary behavior instead of restating code in comments.

No high-severity review findings remain.
