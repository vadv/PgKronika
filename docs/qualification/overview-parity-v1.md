# Overview parity-v1 qualification

The overview qualification artifact is generated from source data at one exact
Git head. It is evidence for the contract in
`docs/superpowers/specs/2026-07-22-overview-index-timeline-api.md`; it is not a
portable performance promise.

## Dense-hour fixture

`overview-dense-hour-v1` contains exactly 720 `pg_stat_database` snapshots at a
five-second cadence, one reset-context row, and complete source-population
coverage for every snapshot. Production extraction creates the canonical
counter, gauge, reset, coverage, and event-fact blocks. The runner records:

- source, fact-file, and decoded block bytes;
- logical resident and single-pin bytes;
- fixed metric bytes separately from variable event/string bytes;
- exact retained series, sample, reset, state, coverage, and fact counts;
- fixed metric bytes per retained sample without claiming a universal budget.

Disk and resident limits are deployment inputs:

```text
OVERVIEW_DENSE_DISK_BUDGET_BYTES
OVERVIEW_DENSE_RESIDENT_BUDGET_BYTES
```

An artifact without both inputs remains a candidate. The validator refuses a
final PASS when either budget is absent or exceeded.

## Modes and coldness

The runner records all nine required modes: `derived-cold`, `restart-warm`,
`process-hot`, `range-cold/facts-warm`, `live`, `concurrent-identical`,
`concurrent-disjoint`, `memory-only`, and `oracle-profile`.

“Cold” in this runner means a newly constructed process-level reader or cache
state. It does not evict the host page cache, and the artifact says
`storage_cold=false`. A storage-cold result requires a separately controlled
host/filesystem procedure and must not replace or relabel this measurement.

## Local candidate

```bash
cargo run --release -p kronika-reader --example overview_qualification -- \
  --output target/qualification/overview.json
python3 scripts/validate-overview-qualification.py \
  target/qualification/overview.json
```

The CI job uploads the raw JSON, structural validation JSON, and SHA-256 files.
For final validation, use the artifact from the exact release head and run:

```bash
python3 scripts/validate-overview-qualification.py \
  overview.json --exact-head GIT_SHA --final
```

Final parity evidence additionally requires every referenced test, BDD job,
coverage job, and the qualification job to come from the same Actions run
attempt and exact head. Mixed-run or dirty-tree evidence is invalid.
