# Activity Forensic Detail — Approved Design

## Goal

Turn a selected `pg_stat_activity` observation into the same dense,
full-canvas forensic workspace as the approved Superdesign and PR206
Statements detail. The operator must see the PostgreSQL observation, its
nearby history, OS measurements available for the process, and every bounded
investigative continuation without treating any link as a proof claim.

## Pinned constraints

- 1920×1080 is the baseline. The root page never scrolls; the detail owns the
  canvas between the 60 px Health Line and the 24 px status bar.
- Keep the PgKronika shell and the approved dark, ruled, dense visual system.
- The Activity population remains mounted but hidden while detail is open so
  filters, pagination, virtualization, and scroll position survive close.
- Missing observations are local, calm, and explicit. They never suppress
  other collected evidence.
- Related processes are best-effort investigation links. The UI does not show
  provenance methods, confidence/proof language, gap counters, gated counters,
  endpoints, or raw entity tokens in the primary surface.
- History is capped at 96 snapshots and six hours.

## Chosen composition

### Entity strip

One 40 px strip contains Activity, PID, database, role, application, snapshot,
current state/wait, data availability, and close. Long values truncate with a
native title; routing tokens never appear.

### Temporal field

Four aligned lanes share the same horizontal time geometry:

1. **Activity observations** — categorical state/wait cells. Missing samples
   remain empty rather than being interpolated.
2. **Query / transaction age** — query and transaction duration traces.
3. **CPU / memory** — CPU and RSS traces from the Activity projection's
   process enrichment when available.
4. **Disk I/O** — process read and write throughput traces.

Each numeric trace is independently normalized inside its lane, matching the
approved reference behavior. Current values remain visible in the fixed label
column. A final investigation lane exposes related process candidates and a
Statements continuation when a query id was observed.

### Analysis field

Three ruled columns fill the remaining height:

1. **PostgreSQL observation** — state, wait, backend type, query/xact age,
   query id, and bounded SQL text.
2. **PostgreSQL + OS snapshot** — current / first observed value matrix for
   durations, CPU, RSS, threads, reads, and writes, plus bounded process command.
3. **Continue investigation** — every returned process candidate, a filtered
   Statements continuation for the observed query id, and a filtered
   waits/locks continuation for the PID.

The continuations guide the operator through recorded history. They make no
claim that a query caused an OS measurement or that a reused PID is the same
process outside the returned observation relationship.

## Data flow and bounds

`ActivityDetail` issues one point request with `include=related` and, when the
catalog advertises history, one history request with these eight columns:

`state,wait_event,query_duration_us,transaction_duration_us,cpu,rss,read_bytes_per_second,write_bytes_per_second`

The request uses `limit=96`, `to=at`, and
`from=max(at-min(span,21600s), at-21600s)`. No frame pagination, process
population request, timer, or subscription is added. Existing TanStack Query
cache behavior is reused.

## Interactions

- Escape or the visible close button removes only `dock=row`.
- A process card opens the exact returned process entity at its returned
  snapshot.
- “Find in Statements” switches to Statements with a `queryid=<value>` filter
  at the same cursor and no selected row.
- “Open waits & locks” returns to Activity's prepared waits/locks lens with a
  `pid=<value>` filter at the same cursor.
- Mobile keeps the existing generic dock until a separate responsive detail
  design is approved.

## Failure semantics

- Point loading/error owns the detail body but not the shell or close action.
- History loading/error is a local note over the temporal field; current point
  values remain usable.
- An absent process candidate shows no process card; Activity history and SQL
  remain intact.
- Null state/wait/history cells render as not observed, never zero.

## Acceptance

- Exact 1920×1080 root, `scrollY=0`, detail bounds y=136…1056.
- Four temporal lanes, one related lane, three analysis columns.
- At most 96 history samples × eight columns.
- Closing detail restores the already mounted Activity overview.
- No generic dock, raw token, endpoint, provenance method, gap/gated counter,
  proof wording, or causal assertion in the visible detail.
- Keyboard close and all three continuation types work.
