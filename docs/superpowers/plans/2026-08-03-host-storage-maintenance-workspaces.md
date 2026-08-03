# PR7 — Host, storage objects, and maintenance workspaces

**Stack base:** `codex/pr06-activity-plans`  
**Branch:** `codex/pr07-host-storage-maintenance`  
**Baseline viewport:** 1920×1080

## Outcome

Turn the existing `processes`, `tables`, `indexes`, and `vacuum` projections
into four prepared forensic workspaces. Each screen keeps the shared Health line,
the time-aligned heatmap, the dense frame table, global search, and Entity Detail.
The analytical side panel adds evidence from a second bounded source only when a
server-side identity or same-snapshot rule supports it.

## Evidence boundaries

- The OS screen is host-scoped. Process and cgroup values are never summed with
  host values; cgroup caps and response limits remain visible as
  `resource_limited` evidence.
- `load_per_cpu` is runnable demand and `psi_io_some` is stall pressure. Neither
  is labelled CPU utilization or physical-disk utilization.
- PostgreSQL buffer reads are not relabelled physical disk reads.
- Table/index size is a point value. Growth requires Entity History and is not
  inferred from the current ranking.
- Index→table and vacuum→table links require the same snapshot, database OID,
  and relation OID. They are temporal associations, not causal links.
- Table→active-vacuum is the inverse of the same bounded OID match.
- Vacuum throughput/history remain unavailable until a vacuum lifetime identity
  is collected. PID + relation OID alone is not a safe cross-time identity.
- Duplication, invalid/build state, dependencies, network, and filesystem
  telemetry are shown as gated/not-collected lenses rather than fabricated.

## Tasks

### 1. Publish related-object contracts

- Add secondary inputs needed for table↔vacuum and index↔table matching.
- Publish temporal catalog joins with exact comparison fields and cardinality.
- Enable related capabilities on Tables, Indexes, and Vacuum.
- Resolve bounded Entity Detail relations at one snapshot.
- Test match, absence, ambiguity, and cross-database rejection.

### 2. Publish prepared presets

- Tables: Health, Vacuum risk, I/O, Scan pattern, Size / growth, XID / MXID.
- Indexes: Usage, I/O, Size / growth, Unused, Table context.
- Vacuum: Progress, Phase, Dead items, Wraparound context.
- OS/processes: CPU, Memory, Storage I/O, Cgroups, Processes.
- Keep unsupported lenses explicit in the frontend with a reason.

### 3. Add the evidence side panel

- OS: bounded 24-bucket host pressure readout, scope guard, and quality caps.
- Tables: bounded active-vacuum lanes linked to Vacuum Detail.
- Indexes: table-context provenance and an Entity Detail relationship affordance.
- Vacuum: point-progress/lifetime warning and table-context provenance.
- Reuse the fixed 156 px analytical center and 96 heatmap buckets.

### 4. Demo and browser proof

- Extend deterministic demo catalog and rows for every prepared lens.
- Add related-object Entity Detail responses for demo entities.
- Verify OS, Tables, Indexes, and Vacuum at 1920×1080 in Chromium.
- Assert no root scroll, bounded center height, 96 buckets, gated states, and
  clickable evidence lanes.

### 5. Ship the stack layer

- Run Rust formatting, clippy, full web tests, frontend gate, browser proof,
  bundle budget, and static/auth tests.
- Repack `static.tar.gz` twice and compare byte-for-byte.
- Open a ready PR on top of PR6 and request independent review.

