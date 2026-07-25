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
fold and handoff primitives. The production web refresh owner retains one
builder, folds only newly completed journal parts, and reconciles a live
generation with its exact sealed descriptor before publishing an immutable
timeline view. Ordinary logical-section requests continue to query
`LocalDirSnapshot`.

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

`SourceDescriptor`, `section_body_id`, and `dictionary_context_id` derive
typed content identities from exact PGM metadata and retained values.
`PgmUnit::read_overview_section` reads one catalog ordinal and verifies its
CRC. `PgmUnit::resolve_overview_dictionary` reads only `dict.strings` and
`dict.blobs`, retains requested IDs, and reports stored and decoded work.

`FactFile::build` writes the canonical PGKOVF container. `FactFile::admit`
validates the complete container, including physical layout, checksums,
aggregate bounds, logical block contents, source provenance, and string
references. `FactFileReader` reads the header and directory first, then
CRC-checks only selected block bodies. `FactReadStats` exposes the resulting
read calls and byte counts.

All PGKOVF constructors and decoders enforce the absolute `LIMIT` values before
large allocations. PgKronika owns the whole data directory and uses one
same-stem sidecar for each sealed segment:

```text
/data/active.parts
/data/1721916000000000.pgm
/data/1721916000000000.ovf
```

`FactStore` derives the sidecar name only by replacing the exact `.pgm`
extension with `.ovf`. The PGKOVF header stores the `FactKey`,
`SegmentLineageId`, exact `SourceDescriptor`, source metadata, and the schema,
extractor, registry, and source-format versions. Every read validates those
fields against the selected PGM. A missing, stale, incompatible, corrupt, or
mismatched sidecar triggers bounded extraction from that PGM. The replacement
is written to a same-directory process-unique temporary file, synchronized,
and atomically renamed over the same sidecar path. The PGM path does not
participate in `FactKey` or lineage.

The persistent sidecar remains the first lookup. If canonical encoding and
full admission succeed but publication fails for a recoverable storage reason,
`FactStore` may retain the immutable `Arc<SegmentFacts>` in a process-local
fallback LRU. Its complete key combines `FactKey` with
`SegmentLineageId`. The default budgets are 24 segment-hours and 64 MiB of
canonical fact bytes; configuration is capped at 744 segment-hours and
256 MiB. Duration rounds up to whole hours, with one hour charged for a point,
empty, or unknown interval. Entries that exceed either budget are returned to
the caller but not retained. `FallbackStats` reports hits, misses, inserts,
evictions, oversized entries, publication-failure offers, and exact residency.

`FactBuildKey` is the exact pair `(FactKey, SegmentLineageId)`. It qualifies
single-flight, cold-work admission, and the process-local fallback; it is not a
filename or directory key. Before any mutation, `FactStore` takes
`.pgkronika-overview.owner.lock` in the data directory. Clones share that
lease. Another independently constructed store or process may read valid
sidecars but cannot publish or collect them while the owner is alive.

`GcConfig` bounds each direct scan of the data directory. The defaults allow
100,000 entries, require two distinct authoritative GC generations and
120 seconds since the first absent observation, and retain recognized
publication temporary files for 600 seconds. An unavailable live set, a scan
error, or an entry-cap hit authorizes no deletion and does not advance grace.
GC admits a same-stem `.ovf` only after validating its PGKOVF header against
the corresponding live descriptor. It never follows symlinks or removes PGM
sources, `active.parts`, or the owner lock. A sidecar unlink also takes the
publication gate and checks the opened inode and device again.

Logical-byte and file-count ceilings are optional and disabled by default.
When configured, admission counts only recognized sidecars, publication
temporary files, and the owner lock from one complete scan. The byte ceiling
uses logical `st_size`, not free blocks or a physical-filesystem quota. If the
complete derived-file inventory cannot admit a publication, the write returns
`QuotaExceeded`; source files are not counted or evicted.

All sidecar write APIs share one `PersistMode` state machine. Read-only,
permission, capacity, stale-filesystem, and selected transient I/O failures
arm bounded backoff and may populate the fallback. Per-key or root contention
may use the fallback for that call but does not disable the store. Invalid
facts, unsafe paths, invalid sidecar state, and unclassified I/O remain
permanent typed failures and do not arm global backoff. `ENOSPC` and quota
failures may run one GC pass from the last complete authoritative mark and
retry the publication once. `FactStore::probe_persistence` reserves one due
probe, writes and synchronizes a sentinel, removes it, and clears backoff only
after success. Sidecar reads remain first and do not reset write state.

Permission and read-only probes start at the five-minute cap. Capacity and
transient failures use per-store jittered exponential delay, also capped at
five minutes. `PersistModeSnapshot` reports the typed reason, consecutive
failure count, remaining delay, and reservation state.

The web layer combines concurrent builds by `FactBuildKey` and applies
weighted process-wide admission before extraction or publication.

## Bounds and failures

Catalogs are capped at 64 MiB. Registry limits cap each section at 8 MiB,
65,536 rows, and 16 Parquet row groups before decoded output is accepted.
Dictionary decode follows the same row and row-group guards. Errors distinguish
I/O, framing, unsupported format, bounds, CRC/codec, storage, and staleness.

The crate owns no HTTP status mapping, remote storage, anomaly request budget,
or PostgreSQL behavior. See [`src/lib.rs`](src/lib.rs) for the canonical public
surface.
