# PR12 — OS Visual Convergence

## Outcome

Replace the detached process heatmap, small host-pressure card and generic
process table with one dense OS investigation workspace. An operator must be
able to answer which host pressure was observed, which processes dominated the
selected interval, when each process was active in the selected metric, and
where evidence is incomplete or independently scoped.

The baseline viewport is 1920×1080. Health Line remains the only upper chart.
The OS workspace owns the remaining height and keeps overflow inside the ranked
matrix.

## Visual target

The selected reference is:

- `output/playwright/pgkronika-os-pressure-refined.png`

The implementation must be compared with that reference at the same
1920×1080 viewport before merge. PR12 keeps the reference's information order — combined
PostgreSQL + OS Health, host pressure context, prepared resource lenses, dense
entity ranking and an evidence inspector — but implements it with PgKronika's
existing design tokens and only the evidence published by the current API.

The reference's synthetic memory-pressure lane, per-device saturation, network
telemetry, filesystem forecast, causal mechanism text and correlation score are
not copied. The current public contract publishes two host spine series
(`load_per_cpu` and `psi_io_some`) and process CPU/I/O heatmaps. Unsupported
resource families remain visibly gated rather than rendered from demo-only
assumptions.

## Information architecture

The existing screen header and prepared-lens toolbar stay in place. The body
becomes one `OsWorkspace` with:

1. a compact host evidence rail containing the active lens, exact metric
   semantics, host identity, the two retained host signals and quality limits;
2. one viewport-owned process matrix where process identity, point aggregates
   and 96-bucket interval evidence share each row;
3. universal Entity Detail as the persistent inspector when an operator selects
   a process.

There is no detached OS heatmap or permanent right-side mechanism card. The
row-coupled matrix provides visual temporal association without calling it
correlation or causality.

## Prepared lenses

- **Pressure** combines host load demand and I/O PSI context with process CPU
  ranking. Load is not CPU utilization and PSI is not utilization.
- **CPU** ranks by interval-derived process CPU and exposes RSS, threads,
  storage-accounted I/O and lazy command context.
- **Memory** ranks the current process snapshot by RSS. The temporal selector
  still says CPU or I/O because the heatmap API does not publish RSS history.
- **Storage I/O** defaults the temporal metric to combined process I/O and
  ranks storage-accounted read/write rates plus block-delay evidence.
- **Cgroups** keeps exact same-snapshot `(pid, starttime, ts)` mapping visible
  and never sums host and cgroup values.
- **Processes** is the general impact ranking and keeps process lifetime in
  Entity History.
- **Data quality** emphasizes missing inputs, gaps, caps and selection bias.
- **Network** and **Filesystems** remain visible but unavailable with their
  existing reasons.

Every available lens remains URL-addressable. Switching to Storage I/O selects
the `io` temporal metric; the other prepared lenses select `cpu` unless the
operator explicitly changes the metric in that lens.

## Host evidence rail

The rail requests the existing bounded 24-bucket spine for the selected range.
It publishes:

- latest retained `load_per_cpu` as runnable demand with a gauge badge;
- maximum retained `psi_io_some` as stalled-time percentage with a gauge badge;
- logical CPU count and kernel version from UI context;
- scope text stating that host, process and cgroup values are not summed;
- quality status, snapshots, gaps, gated sources and `resource_limited` caps.

The two host series may use compact bucket strips, but their units remain
independent. They share time geometry and cursor with Health Line; they do not
share a numeric scale with each other or with process rows. Loading, error and
absence are separate states. A failed spine request is never presented as a
zero-pressure host.

## Process matrix grammar

- Sticky identity combines PID and process type. Process Entity Detail may keep
  `starttime` for rate-delta continuity and history, while Activity navigation
  links every retained entity with the same PID.
- Aggregate columns follow the selected catalog preset and remain sortable.
- Every rendered row contains exactly 96 temporal buckets on desktop and 48 on
  mobile.
- Temporal metric is explicitly **CPU** or **I/O**. CPU is derived from positive
  tick deltas using host HZ; I/O is storage-accounted read + write bytes per
  second.
- A coloured bucket means retained interval evidence exists for that exact
  process entity. A blank bucket is missing or no positive delta; it does not
  prove the process was absent.
- Point RSS, threads, command and cgroup values are not stretched into history.
- Rows stay virtualized, horizontally navigable and independently scrollable
  up to the 4,096-row process collection cap.
- Filtered frame counts and the unfiltered top heatmap population are labelled
  as independent scopes instead of an impossible retained/matched ratio.

## Detail and relation boundaries

Selecting a process opens the existing reusable Entity Detail with point,
history, relations and raw projection views. Activity and OS-process histories
are linked by PID, including across retained collection gaps. `starttime` is
used only for process-rate delta continuity and never hides the navigation
relationship.

The matrix does not add a separate evidence inspector filled with inferred
mechanisms. Findings, events and relationships remain independently sourced and
retain their own provenance in Detail and Signals.

## Shared Health and Events convergence

The persistent 60 px Health line aggregates event facts into 48 bounded
density buckets. One interval renders one bar whose height is occurrence count
and whose colour is the strongest event family in that interval. It never
stacks one glyph per fact. The right summary exposes the total occurrence count
for the selected window.

The Events screen owns range evidence instead of stretching a point-in-time
frame row over the remaining viewport. Its investigation body contains:

- the existing 96-bucket family heatmap and six newest Signals;
- a scrollable timeline of up to 200 typed event facts from the selected range;
- occurrence and retained-fact totals;
- ranked event-family density and collection-quality fields;
- client-side prepared-lens and typed-filter application, with typed input
  taking priority.

At 1920×1080 the screen has no empty table canvas while range facts exist.

## States and accessibility

- Host-spine and process-heatmap loading, error, empty and partial states are
  explicit and independently retryable.
- On heatmap failure, rows remain usable but announce request failure rather
  than “no retained series”.
- Lens, metric, row and retry controls have visible focus and keyboard support.
- Matrix row count, sticky header and sticky identity remain correct during
  vertical and horizontal navigation.
- Reduced motion disables decorative transitions without hiding state.

## Acceptance

- At 1920×1080 Health Line, OS context, host evidence rail and at least 18
  process rows are visible without root scroll.
- OS has no detached analytical-center region.
- Pressure, CPU, Memory, Storage I/O, Cgroups, Processes and Data quality are
  materially distinct and URL-addressable.
- Every rendered process temporal row contains exactly 96 desktop buckets.
- CPU/I/O switching changes the exact row-coupled heatmap request and label.
- Storage I/O defaults to `io`; other lenses default to `cpu`.
- Host load and PSI semantics, host/process/cgroup scope separation and
  `resource_limited` caps remain visible.
- Filtered frame and global heatmap populations cannot form a misleading ratio.
- Network and Filesystems remain visibly unavailable.
- Process selection opens real Entity Detail.
- Frontend and Rust tests, formatting, lint, clippy, visual verifier and bundle
  budget pass.
