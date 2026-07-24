# kronika-reader

[Русская версия](README.ru.md)

`kronika-reader` verifies and decodes local PGM units, builds a snapshot over
sealed files and live journal parts, and exposes bounded logical queries used by
`pg_kronika-web`.

## Units and snapshots

`PgmUnit<R: ReadAt>` is the common decode path for a sealed `File` and an
in-memory active part. It opens the end catalog first, validates format version
and bounds, reads section bytes on demand, checks CRC, then invokes the registry
codec. `Segment` is the sealed-file convenience wrapper.

`kronika-store::LocalDir` scans `active.parts` first and then lists sealed units;
those operations do not capture one atomic combined view. `LocalDirSnapshot`
returns the observed sealed units first, followed by live parts. A live part is
suppressed only when its catalog exactly matches a sealed unit; overlapping
time ranges do not prove identity. Store warnings and journal damage remain
available to callers.

A writer may seal or reset `active.parts` after a snapshot captured a part
reference. This yields `ReadError::StaleSnapshot`. Query helpers refresh a
bounded number of times and surface a gap if the unit remains unstable.

`LiveBuilder`, `LiveView`, and seal reconciliation provide bounded overview
fold and handoff primitives. `pg_kronika-web` does not publish that live
timeline yet; production requests still query `LocalDirSnapshot`.

## Logical queries

`logical_section(name)` combines registered layout versions with that name.
Section queries:

1. select one `source_id` and overlapping time range;
2. decode only matching entries and dictionary sections;
3. union version columns and resolve strings;
4. order rows by the registry sort key;
5. return coverage gaps and an opaque next cursor.

`section` and `sections` use a row limit plus the hard 10,000,000-cell
materialization ceiling. `section_with_limits` and `sections_with_limits` let
an adapter spend a smaller request-wide cell budget. Exceeding it returns
`QueryError::ResultTooLarge` before retaining another row.

The cursor pins the last returned key and source contract. A malformed or
cross-source cursor is rejected rather than treated as an offset.

## Gauge and counter semantics

`gauge_section` groups gauge samples by the declared identity. `diff_section`
folds cumulative columns through `kronika-analytics` using exact integer
deltas and real sample intervals.

No-data states stay typed:

- `FirstPoint` for a series start or first sample after a break;
- `Reset` when a cumulative value decreases or reset metadata advances;
- `Gap` when coverage does not span the pair;
- `NotCollected` when a declared collection gate was off or unknown;
- `Anomaly` for invalid time order or incompatible scalar input.

An unchanged measured counter yields a real zero delta and rate. Diff does not
bridge these no-data states and does not extrapolate across unsampled time.

## Overview fact files

`source_scope_id`, `SourceDescriptor`, `section_body_id`, and
`dictionary_context_id` derive typed content identities from exact PGM
metadata and retained values. `PgmUnit::read_overview_section` reads one
catalog ordinal and verifies its CRC. `PgmUnit::resolve_overview_dictionary`
reads only `dict.strings` and `dict.blobs`, retains requested IDs, and reports
stored and decoded work.

`FactFile::build` writes the canonical PGKOVF container. `FactFile::admit`
validates the complete container, including physical layout, checksums,
aggregate bounds, logical block contents, source provenance, and string
references. `FactFileReader` reads the header and directory first, then
CRC-checks only selected block bodies. `FactReadStats` exposes the resulting
read calls and byte counts.

All PGKOVF constructors and decoders enforce the absolute `LIMIT` values before
large allocations. `FactStore` loads and validates versioned per-segment fact
files. A missing or rejected candidate triggers bounded extraction from PGM;
the store then publishes the rebuilt facts under their content key. Persistence
failures remain visible alongside the freshly extracted facts.

Persistent files remain primary. If canonical encoding and full admission
succeed but publication fails for a recoverable cache/storage reason,
`FactStore` may retain the immutable `Arc<SegmentFacts>` in a process-local
fallback LRU. Its complete key combines `FactKey` with sealed lineage, and each
lookup still tries durable storage first. The default budgets are 24
segment-hours and 64 MiB of canonical fact bytes; configuration is capped at
744 segment-hours and 256 MiB. Duration rounds up to whole hours, with one hour
charged for a point, empty, or unknown interval. Entries that exceed either
budget are returned to the caller but not retained. `FallbackStats` reports
hits, misses, inserts, evictions, oversized entries, publication-failure
offers, and exact residency.

## Bounds and failures

Catalogs are capped at 64 MiB. Registry limits cap each section at 8 MiB,
65,536 rows, and 16 Parquet row groups before decoded output is accepted.
Dictionary decode follows the same row and row-group guards. Errors distinguish
I/O, framing, unsupported format, bounds, CRC/codec, storage, and staleness.

The crate owns no HTTP status mapping, remote storage, anomaly request budget,
or PostgreSQL behavior. See [`src/lib.rs`](src/lib.rs) for the canonical public
surface.
