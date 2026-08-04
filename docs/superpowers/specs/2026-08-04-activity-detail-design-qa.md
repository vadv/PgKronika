# Activity Forensic Detail — Design QA

## Final result

Passed.

The selected Activity state was compared with the approved Superdesign
reference in one visual input at the same 1280×720 in-app-browser viewport.
The production shell was then verified at the required 1920×1080 baseline.

## Reference comparison

Reference: `https://pgkronika-forensic-u.superdesign.cloud/`

The implementation keeps the approved composition and adapts it to a selected
`pg_stat_activity` observation:

- persistent PgKronika shell and 60 px Health Line;
- compact Activity identity strip with PID, database, role, application,
  snapshot, current state, and wait;
- one categorical state/wait history plus three aligned numeric lanes for
  query and transaction age, CPU and RSS, and process read/write throughput;
- one calm continuation lane for recorded processes and Statements;
- three ruled analytical columns for PostgreSQL facts, a PostgreSQL + OS
  snapshot comparison, and related evidence;
- dense 4 px rhythm, restrained borders, and the existing PgKronika signal
  palette.

The colored Activity observation cells are intentionally denser than a single
health trace. They preserve the visual relationship between changing backend
state, waits, PostgreSQL age, and OS pressure without turning temporal
proximity into a causal claim. Every process candidate returned by the entity
response stays inspectable.

## 1920×1080 contract

The real-browser verifier recorded:

| Check | Result |
| --- | ---: |
| Root geometry | 1920×1080 |
| Root scroll | 0 |
| Detail bounds | y 136–1056 |
| Temporal lanes | 4 |
| Numeric traces | 5 |
| Activity observation cells | 12 |
| Lock-wait cells | 2 |
| Analysis columns | 3 |
| Related process candidates | 1 |
| Generic row dock mounted | no |
| Population overview visible behind detail | no |

Generated shell screenshot:

- `web/demo/shots/forensic-activity-detail-1920x1080.png`

## Data and interaction checks

- The Activity history request is capped at `limit=96`, eight exact columns,
  and six hours.
- The point request asks for related observations once; the normal surface
  does not expose relation methods or raw routing tokens.
- Process, Statements, and waits/locks continuations preserve the selected
  forensic time and install the appropriate server filter.
- Escape and the visible close control remove only `dock=row`, returning to
  the already mounted Activity population.
- Missing values stay local as `not observed`; missing history never suppresses
  point facts or related observations that were captured.
- Raw entity tokens, endpoint paths, collection counters, and causal-certainty
  language are absent from the primary detail surface.
- The single Impeccable mechanical scan returned no findings for the changed
  UI targets.

## Mandatory review panel

- **DevOps / packaging:** deterministic `static.tar.gz` is 198,655 bytes
  against the 262,144-byte budget. The production dependency audit reports no
  vulnerabilities.
- **DBA:** state and wait history is aligned with query/xact age and process
  CPU, memory, and I/O. SQL, backend identity, and query ID remain visible for
  continuing into Statements or waits/locks.
- **SRE / failure behavior:** point, history, and relation failures stay local.
  A missing OS sample does not erase the Activity observation or SQL that was
  captured.
- **PostgreSQL semantics:** Activity remains a snapshot-oriented source; the
  history is a sequence of observed snapshots and does not claim to contain
  short queries that ran between samples.
- **Frontend performance:** one selected row adds one point response and one
  bounded 96×8 history response. The population workspace remains mounted but
  hidden, so returning does not reload the investigator's table.
- **Memory bounds:** the selected detail renders a small fixed fact matrix,
  four bounded lanes, and all returned process candidates; no timer,
  accumulator, or unbounded subscription was added.
- **Automated verification:** 56 frontend test files and 352 tests pass, along
  with typecheck, lint, format, design-token validation, production build, and
  the real-browser shell verifier.

No high-severity review findings remain.
