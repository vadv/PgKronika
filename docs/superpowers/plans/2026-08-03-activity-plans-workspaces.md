# PR6 — Activity + Process Evidence and Plans Workspace

## Outcome

Turn Activity and Plans into two distinct workload workspaces while preserving
the shared 1920×1080 forensic shell, 60 px Health line and 96-bucket heatmap.
Activity remains a point snapshot. Process enrichment and plan attribution are
shown with their real relation quality and never promoted to exact identity.

## Server contract

1. Add explicit Activity presets for Overview, Waits & Locks, Duration, CPU,
   Disk I/O, Replication and Sampling using only existing projected fields.
   Memory and XID/Horizon remain visibly gated until those fields exist.
2. Add a Plans Change timeline preset using `first_call`/`last_call`; keep
   Compare gated until two bounded trees and a diff contract exist.
3. Publish fork-specific best-effort Plans→Statements join metadata for OSSC
   and vadv in the catalog.
4. Expose Activity→Process as a point-detail relation only when the existing
   same-snapshot PID matcher finds exactly one process candidate. The relation
   is `best_effort`, method `same_snapshot_unique_pid`, fields `pid,ts`; the
   process entity keeps its full `(pid,starttime)` identity.
5. Cover unique, ambiguous and absent process candidates in Rust tests and
   keep public fixtures/schema parity.

## Frontend

1. Provide prepared, URL-addressable lens sets per screen. Each available lens
   maps to a distinct server preset, so only one lens can be selected.
2. Split the 156 px analytical center for Activity and Plans: the heatmap keeps
   most of the width; a compact evidence panel explains the selected lens.
3. Activity panel reads the Activity projection itself for process-enriched
   metrics. Waits & Locks additionally reads the bounded Locks frame and shows
   waiter→blocker lanes. It never performs a client-side PID join.
4. Plans panel shows bounded version lanes and the catalog's fork-specific
   attribution provenance. It does not claim a statement identity or a parsed
   tree diff that the API cannot provide.
5. Activity copy explicitly says point snapshot and missed-short-query risk.
   Sampling is labelled observed coverage, never ASH or exact query cost.
6. Universal Entity Detail uses the new Activity→Process relation for a
   provenance-labelled drill-down.

## Verification

- TDD for lens configuration, evidence panels and entity relations.
- Frontend type/lint/format/design-token/full coverage gates under Node 22.
- Rust catalog/entity/frame tests on the macOS host target.
- Chromium at 1920×1080: all major regions visible, 96 heatmap buckets,
  Activity point-snapshot note, lock lanes, process drill-down, Plans timeline
  and fork provenance.
- Reproducible embedded archive and bundle budget.
