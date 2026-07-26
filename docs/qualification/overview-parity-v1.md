# Overview parity-v1 qualification

The overview qualification artifact is generated from source data at one exact
Git commit. It is evidence for the contract in
`docs/superpowers/specs/2026-07-22-overview-index-timeline-api.md`; it is not a
portable performance promise.

## Dense-hour fixture

`overview-dense-hour-v2` contains exactly 720 `pg_stat_database` snapshots at a
five-second cadence, one reset-context row, and complete source-population
coverage for every snapshot. Production extraction creates the canonical
counter, gauge, reset, coverage, and event-fact blocks. The runner records:

- source, fact-file, and decoded block bytes;
- logical resident and single-pin bytes;
- fixed metric bytes separately from variable event/string bytes;
- exact retained series, sample, reset, state, coverage, and fact counts;
- fixed metric bytes per retained sample without claiming a universal budget.

Disk and resident limits are owner-approved deployment inputs:

```text
OVERVIEW_DENSE_DISK_BUDGET_BYTES
OVERVIEW_DENSE_RESIDENT_BUDGET_BYTES
```

When both values are absent, the artifact records `owner_deferred`: exact
sizing, I/O, and performance gates still run, but the artifact makes no
deployment-budget claim. Supplying only one value is invalid. When both values
are configured, final validation requires both measured working sets to fit.

## Modes and coldness

The artifact records the storage model as one PgKronika-owned data directory.
The active journal and each sealed source/fact pair are siblings:

```text
active.parts
dense-hour.pgm
dense-hour.ovf
```

It also records the exact PGM and OVF names and verifies that their stems
match.

The runner records all nine required modes: `derived-cold`, `restart-warm`,
`process-hot`, `range-cold/facts-warm`, `live`, `concurrent-identical`,
`concurrent-disjoint`, `memory-only`, and `oracle-profile`.

`derived-cold` starts each iteration with a new owned data directory containing
only `dense-hour.pgm`, then times the production build path through canonical
admission and atomic publication of the sibling `dense-hour.ovf`.
`restart-warm` seeds one valid sibling OVF before measurement, then constructs a
new fact store for each iteration so no process-local fallback or decoded
in-memory entry survives. The runner preserves these mode data directories next
to its output artifact as supporting evidence.

The nine mode results retain the complete production endpoint measurements.
For HTTP modes, the production router and metrics recorder exist before the
request timer starts; the measured request still includes fact loading,
projection, response serialization, and body collection. Because HTTP and JSON
dominate these endpoint results, the artifact also records the separate
`compact sealed facts read + bucket` profile required by §18.4.6. The unchanged
25%/25%/50% p95 gates apply to that compact profile, while both profiles and all
raw samples remain in the artifact.

“Cold” in this runner means newly constructed process-level reader and
in-memory state. It does not evict the host page cache, and the artifact says
`storage_cold=false`. A storage-cold result requires a separately controlled
host/filesystem procedure and must not replace or relabel this measurement.

## PostgreSQL and process-lifecycle BDD

The exact evidence manifest names eight scenarios: one source-scoped timeline
scenario and one real `pg_kronika-web` process-lifecycle scenario for each
PostgreSQL major from 15 through 18. Run the lifecycle set with:

```bash
DEBUG=1 make test-bdd TAGS=@timeline_web_lifecycle
DEBUG=1 make test-bdd TAGS='@timeline_web_lifecycle and @pg15'
```

The first command covers PostgreSQL 15–18; the second is a targeted PostgreSQL
15 diagnostic. Every lifecycle scenario launches the actual `pg_kronika-web`
executable over an isolated owned data directory, sends HTTP requests to the
overview, events, health, cursor, and Prometheus endpoints, and starts new
processes for restart assertions. Explicit post-bind readiness, graceful
shutdown or asserted process death, and a publication barrier replace timing
sleeps and retry loops.

The scenarios prove creation and validation of the same-stem `N.ovf`, durable
reuse without PGM body reads or rewrites, atomic recovery from corrupt and
every stale identity class, rejection of interrupted temporary publication,
bounded fallback followed by durable recovery, cancellation recovery,
process-local cursor expiry, source preservation, and deterministic
second-owner contention. The artifact validator accepts only the eight exact
feature/scenario coordinates in the qualification traceability table.

## Local candidate

```bash
cargo run --release --manifest-path bins/pg_kronika-web/Cargo.toml \
  --example overview_parity_qualification --features qualification -- \
  --output target/qualification/overview.raw.json
python3 scripts/validate-overview-qualification.py \
  target/qualification/overview.raw.json
```

The CI job runs final structural, I/O, and performance validation, then uploads
the raw JSON, validation JSON, and SHA-256 files. To verify a preserved
artifact from the exact release head, run:

```bash
python3 scripts/validate-overview-qualification.py \
  overview.final.json --exact-head GIT_SHA --final
```

Final parity evidence additionally requires every referenced test, BDD job,
coverage job, and the qualification job to come from the same Actions run
attempt and exact head. Mixed-run or dirty-tree evidence is invalid. An
owner-deferred deployment budget is not a size approval; configured budgets
remain mandatory for a deployment that uses them.
