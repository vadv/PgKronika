# Overview M6 traceability

Base: `1a6f435ee1f9623b0d9c46cd87b51dd0eba15195` (merged PR #114).

This checklist maps the normative acceptance rows in §20.1 of
`docs/superpowers/specs/2026-07-22-overview-index-timeline-api.md` to direct
release evidence. A row becomes `IMPLEMENTED` only when its named test or
measurement is part of the exact-head artifact and the artifact validator
requires it. A passing test from another commit or Actions attempt is not
release evidence.

| ID | M6 evidence required after PR #114 | State |
| ---: | --- | --- |
| 1 | Durable restart, selected OVF blocks, zero PGM body reads and zero writes | IN PROGRESS |
| 2 | Raw/index equality for every event, counter, gauge, reset, state, coverage and factor family over full/partial and sealed/live ranges | IN PROGRESS |
| 3 | Random partition, seal transition, segment merge and boundary invariance | IN PROGRESS |
| 4 | Missing/wrong/incompatible/corrupt/oversized OVF rebuild and publication-failure fallback | IN PROGRESS |
| 5 | PGM corruption remains a typed source gap after scrub and cannot be hidden by OVF | IN PROGRESS |
| 6 | Health/notable policy changes reuse unchanged canonical facts and OVF | IN PROGRESS |
| 7 | HTTP cursor walk is exact once, stable on a pinned view and honest on expiry/mismatch | IN PROGRESS |
| 8 | Live-to-sealed identity and provenance equality, including distinct lineages | IN PROGRESS |
| 9 | Lossless live builder plus `Incomplete` promotion denial | IN PROGRESS |
| 10 | Required coverage gap produces `score=null` and `unknown`, never false green | IN PROGRESS |
| 11 | Trusted floor survives partition, seal and worst-point downsampling while score stays unknown | IN PROGRESS |
| 12 | Every factor publishes exact applicability, coverage, population and loss semantics | IN PROGRESS |
| 13 | Counter halo, actual interval, reset/gap and exact half-open range behavior | IN PROGRESS |
| 14 | Exhaustive source taxonomy, units, entity identity, reset family and unsupported-layout evidence | IN PROGRESS |
| 15 | Memory/OVF hits bypass cold admission; identical work shares one build; disjoint work stays bounded | IN PROGRESS |
| 16 | Byte+segment-hour memory-only fallback, backoff/recovery and dense-hour accounting | IN PROGRESS |
| 17 | Exact quota/GC accounting, owner lock, inode recheck and source-file preservation | IN PROGRESS |
| 18 | All nine §18 modes on one exact host/filesystem profile with raw performance and I/O evidence | IN PROGRESS |

Release synchronization also requires:

- PostgreSQL 15–18 BDD through collection, PGM, sibling OVF and all three
  timeline endpoints;
- one machine-readable artifact whose evidence manifest names exact test
  binaries, test cases, BDD scenarios, CI jobs, Git commit, run, attempt and
  artifact checksum;
- strict final validation of the same-head Actions run without retries;
- synchronized English/Russian README, qualification guide, normative
  specification, OpenAPI and CI acceptance matrix;
- explicit `owner_deferred` only for the deployment-specific budgets and
  charts that the normative specification already defers.
