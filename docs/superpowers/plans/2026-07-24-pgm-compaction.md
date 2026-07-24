# PGM compaction: measured headroom and the repack experiment

Status: research note with reproducible tooling; no production changes yet.

## Question

A one-hour saturated demo-stand run produces ~71 MiB of sealed `.pgm`
(17.8 MiB per 15-minute segment; `demo-data/report.json` from the stand run).
The reftool comparator stores ~10 MiB/hour compressed on its own demo load.
Where does the difference come from, and how much of it is recoverable
without changing what PGM stores?

## Findings

Per-type byte accounting over the stand's three sealed segments
(`segment_breakdown` test) shows the size is dominated by container
overhead, not payload:

1. **Per-window section granularity.** Every type flushes as its own Parquet
   section per ~13-second window. A Parquet footer scales with the column
   count, not the row count, so one-row snapshot sections (meminfo, vmstat,
   PSI, bgwriter, WAL, cgroup, ...) cost 2.4–6.8 KiB each while carrying tens
   of payload bytes. The lock-tree type spent 1.9 MiB on 458 rows spread over
   211 mostly-empty sections.
2. **Dictionary re-emission.** `dict.strings` held 71 468 rows per segment
   with only ~3 200 unique `str_id` values: every window re-writes the
   strings it references, ~22 copies of each entry per segment. `str_id`
   values are stable across windows, so duplicates are exact.
3. **Control experiment.** External `zstd -19` over a finished `.pgm`
   compresses it to 9–10% of its size — the redundancy lives in the
   container (repeated footers, repeated dictionary rows, repeated
   near-identical windows), not in the data.

## Repack experiment

`repack_estimate` (ignored test in `bins/pg_kronika-demo/tests/`) rebuilds
each segment with exactly two changes and no format changes: all Parquet
parts of one type merge into a single body (same schema, same ZSTD-3, one
row group), and dictionary parts deduplicate by `str_id`. Every section of
every stand segment parsed and repacked; nothing was skipped.

| Segment | Before | After | Ratio |
| --- | --- | --- | --- |
| 15-min #1 | 17.8 MiB | ~1.08 MiB | 16.5x |
| 15-min #2 | 17.6 MiB | ~1.06 MiB | 16.7x |
| 7-min tail | 5.4 MiB | ~0.53 MiB | 10.3x |

Largest per-type wins: plan texts 169x (471 parts of footers around a
handful of rows), one-row OS snapshots 44–55x, ASH 21x, processes and
statements ~10x.

Projected steady rate after compaction: **~4.3 MiB/hour** on the saturated
top-500 profile — under half of the reftool comparator rate that anchors
the §18 benchmark gates of the overview spec.

## Implementation candidates

Both keep the write path (journal windows, crash recovery) untouched:

1. **Compact at seal.** `seal_open_segment` already rewrites the journal
   into the final `.pgm`; merging parts per type and deduplicating the
   dictionary is a transformation of that rewrite. Readers need no changes —
   multi-part types simply arrive as one part. Open point: merged rows must
   be re-sorted by the type's canonical sort key (window-major order today).
2. **Offline compactor.** A post-seal pass (archiver stage or a standalone
   subcommand) rewrites sealed segments in place. No writer changes at all,
   but hot segments stay fat until compaction runs, and identity/CRC of the
   sealed file changes after the fact.

Option 1 is the structural fix; option 2 is a lower-risk stopgap that can
reuse the same merge code.

## Reproducing

```
make demo-run                              # or reuse demo-data/segments
KRONIKA_BREAKDOWN_DIR=demo-data/segments \
  cargo test -p pg_kronika-demo --test segment_breakdown -- --nocapture --ignored
KRONIKA_REPACK_DIR=demo-data/segments \
  cargo test -p pg_kronika-demo --test repack_estimate -- --nocapture --ignored
```
