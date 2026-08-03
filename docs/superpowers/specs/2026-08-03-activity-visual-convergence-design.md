# PR10 — Activity Visual Convergence

## Outcome

Replace the detached Activity heatmap plus generic table with one dense forensic
workspace that answers, at a glance, which PostgreSQL backends were observed,
what they were doing or waiting for, which OS process evidence can be joined,
and how trustworthy that join is.

The baseline viewport is 1920×1080. Health Line remains the only upper chart.
The Activity workspace owns the remaining height and must not make the document
scroll.

## Visual target

The selected visual references are:

- `output/playwright/pgkronika-activity-overview.png`
- `output/playwright/pgkronika-activity-cpu.png`
- `output/playwright/pgkronika-activity-waits-refined.png`
- `output/playwright/pgkronika-entity-process-detail-refined.png`

They are translated into the shipped visual system in `DESIGN.md`: matte
near-black surfaces, ruled rows, evidence blue, restrained semantic colour,
Inter for interface copy and JetBrains Mono for measured evidence. The work is
not a literal preservation of the mock's old navigation chrome.

## Information architecture

### Shared Activity context

The existing screen header and lens toolbar stay in place. The body becomes one
`ActivityWorkspace` with:

1. a compact evidence strip describing the point snapshot, process-link
   coverage and temporal sample quality;
2. an optional bounded lock-evidence strip for Waits & Locks;
3. one viewport-owned ranked matrix where Activity columns and observed temporal
   samples share each backend row.

There is no second detached Activity heatmap. The row-coupled temporal lane is
the visual correlation aid: operators can align spikes, gaps and backend state
without the UI claiming statistical or causal correlation.

### Lenses

- **Overview** combines PostgreSQL backend identity, state, wait, query age,
  query id when collected, relation quality and compact OS process evidence.
- **Waits & Locks** ranks waiters, shows bounded waiter→blocker edges and keeps
  the observed-sample lane beside each backend.
- **Duration** emphasizes query and transaction age.
- **CPU** emphasizes process CPU, RSS, threads and command evidence.
- **Disk I/O** emphasizes storage-accounted read/write rates and keeps them
  distinct from PostgreSQL buffer semantics.
- **Replication** keeps the existing sender/receiver evidence.
- **Sampling** emphasizes coverage and gaps.
- Memory and XID/Horizon remain visibly unavailable until their source
  contracts are publishable.

## Matrix grammar

- Sticky backend identity combines PID with `database / user · application`.
- PostgreSQL context, relation quality, OS metrics and observed samples are
  visually grouped in the header.
- `process_link` is rendered as `best_effort` only for one unique same-snapshot
  PID candidate. Missing and ambiguous candidates stay unavailable.
- Overview publishes `process_link`, CPU, RSS, read/s and write/s only from that
  candidate. CPU and I/O rates preserve the existing reset/continuity rules.
- Activity rows remain virtualized and independently scrollable.
- The temporal lane uses 96 buckets on desktop and 48 on mobile. A coloured
  bucket means an observed sample contributed evidence; a blank bucket is not
  interpreted as idle or absent work.
- No line connects samples. Tooltips say “observed sample” and expose missing
  evidence rather than implying continuous execution.

## Waits & Locks

The Waits & Locks lens adds a bounded evidence strip above the matrix:

- at most three waiter→blocker edges from the existing Locks frame;
- waiter PID, blocker PID, target and proven wait age when `waitstart` exists;
- explicit `point snapshot` and `edge only` provenance;
- row activation opens the existing Lock detail.

The strip is supporting evidence, not a causal graph. Query age is never
renamed to lock wait age.

## Entity detail

Selecting an Activity row continues to open universal Entity Detail. Its
Activity→Process relation must show:

- relation kind `best_effort`;
- method `same_snapshot_unique_pid`;
- fields `pid, ts`;
- the related process identity, whose own lifetime remains `(pid,starttime)`.

The UI must not equate PostgreSQL `backend_start` with Linux `starttime`.

## Data contract additions

The Activity catalog exposes already-collected evidence needed by the combined
row:

- `queryid` from `pg_stat_activity.query_id`, nullable on older PostgreSQL or
  when query-id computation is disabled;
- `rss`, `threads` and `command` projected from the unique related process;
- richer Overview, CPU and Disk I/O presets containing the explicit
  `process_link` quality column.

These additions do not broaden the join. Ambiguous or absent process candidates
produce null for every OS-derived Activity value.

## States and accessibility

- Loading, heatmap error, empty rows, partial coverage and unavailable process
  evidence remain explicit.
- Lens, metric and row controls retain visible focus and keyboard operation.
- The matrix exposes its real row count and keeps sticky headers and identity
  cells readable during both horizontal and vertical navigation.

## Acceptance

- At 1920×1080 Health Line, Activity context, evidence strip and at least 18
  ranked rows are visible without root scroll.
- Activity has no detached analytical-center region.
- Overview, CPU and Waits & Locks are materially distinct and URL-addressable.
- Every rendered temporal row contains exactly 96 buckets on desktop.
- Point-snapshot and missed-short-query caveats are visible.
- Process relation provenance remains `best_effort / same_snapshot_unique_pid`.
- Frontend and Rust tests, formatting, lint, clippy, visual verifier and bundle
  budget pass.
