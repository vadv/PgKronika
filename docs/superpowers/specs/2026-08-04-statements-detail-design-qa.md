# Statements Forensic Detail — Design QA

## Final result

Passed.

The selected Statements state was compared with the approved Superdesign
reference at the same 1280×720 browser viewport. The production shell was then
verified at the required 1920×1080 baseline with a thousand loaded
`pg_stat_statements` rows kept behind the selected detail.

## Reference comparison

Reference: `https://pgkronika-forensic-u.superdesign.cloud/`

Local comparison artifacts (generated, not committed) are retained in the
sibling `pr206-statements-detail-fidelity-qa/` directory:

- `reference-1280.png`
- `prototype-1280.png`
- `reference-prototype-stacked.png`

The implementation follows the approved composition:

- persistent PgKronika shell and 60 px Health Line;
- compact statement identity strip with database, role, snapshot, and impact;
- four aligned temporal lanes for execution impact, latency, buffers, and
  write pressure;
- one calm related-workload lane;
- three ruled analytical columns for impact facts, window comparison, and
  related plans/activity;
- dense 4 px rhythm, restrained borders, and the existing PgKronika type and
  signal palette.

Intentional differences are statement-domain differences, not a new visual
language: the reference's table maintenance fields become calls, mean time,
buffer reads, hit rate, WAL, and temp pressure. Only observations returned by
the API are shown; missing activity/process samples stay a local factual state.

## 1920×1080 contract

The real-browser verifier recorded:

| Check | Result |
| --- | ---: |
| Root geometry | 1920×1080 |
| Root scroll | 0 |
| Detail bounds | y 136–1056 |
| Temporal lanes | 4 |
| Signal traces | 6 |
| Analysis columns | 3 |
| Generic row dock mounted | no |
| Population overview visible behind detail | no |

Generated shell screenshot:

- `web/demo/shots/forensic-statement-detail-1920x1080.png`

## Data and interaction checks

- A Statements history request is capped at `limit=96`, six columns, and six
  hours.
- The overview retains all five 200-row pages and its virtual scroll state;
  closing the detail returns to the already loaded 1,000-row population.
- Escape and the visible close button remove only `dock=row`, preserving the
  selected statement and cursor context.
- The Plans control opens related recorded plan evidence without asserting a
  causal conclusion.
- Raw entity tokens, endpoint paths, provenance methods, gap/gated counters,
  and proof language are absent from the primary detail surface.
- The Impeccable mechanical scan returned no findings for the changed UI
  targets.

## Mandatory review panel

- **DevOps / packaging:** deterministic `static.tar.gz` is 194,169 bytes
  against the 262,144-byte budget. The production dependency audit reports no
  vulnerabilities.
- **DBA:** the four lanes expose the fields needed to understand workload
  impact without inventing causality: total time/calls, mean/row latency,
  reads/hit rate, and WAL/temp. The selected statement keeps its database,
  role, and snapshot context visible.
- **SRE / failure behavior:** point, history, and related-evidence absences stay
  local. A missing Activity/OS sample does not suppress the statement, plan,
  or history that was observed.
- **PostgreSQL semantics:** catalog units now publish hit rate as percent,
  block counters as blocks, and WAL as bytes. The frontend formats those units
  instead of exposing raw scalar values.
- **Frontend performance:** the 1,000-row Statements overview remains
  virtualized and hidden while detail is open; the detail mounts six bounded
  traces and a small fact matrix, not a second population table.
- **Memory bounds:** one selected statement adds one point response and one
  bounded 96×6 history response under the existing query cache lifetime. No
  new unbounded accumulator, timer, or subscription was added.
- **Comment quality:** comments are limited to the non-obvious retained-overview
  behavior and browser verifier state transitions; ordinary component behavior
  is expressed through names and types.

No high-severity review findings remain.
