# kronika-reader

[Русская версия](README.ru.md)

`kronika-reader` verifies and decodes local PGM units, builds a snapshot over
sealed files and live journal parts, and exposes bounded logical queries used by
`pg_kronika-web`.

## Units and snapshots

`PgmUnit<R: ReadAt>` is the common decode path for a sealed `File` and an
in-memory active part. It reads catalog entries in chunks of at most 64 KiB,
validates format version, CRC, and bounds, and retains the decoded `Catalog`
without a second raw catalog copy. Section bodies are read on demand, checked
against their CRC, and only then passed to the registry codec. `Segment` is the
sealed-file convenience wrapper.

`kronika-store::LocalDir` scans `active.parts` first and then lists sealed units;
those operations do not capture one atomic combined view. `LocalDirSnapshot`
returns the observed sealed units first, followed by live parts. It suppresses
all live parts for one `SegmentId` only when their catalogs, aggregated with
the same section relocation used by finalization, exactly match the sealed unit
for that `SegmentId`. It never suppresses only a matching prefix. Overlapping
time ranges, a match for only one part of a multi-part journal, or equal content
under another `SegmentId` do not prove identity. Store warnings and journal
damage remain available to callers.

Sealed discovery retains an exact `FileIdentity` plus an `Arc<CatalogSummary>`
for each PGM, not its complete entry table. The store checks the opened file
identity before and after deriving the summary. Opening a selected unit repeats
the identity check around the lazy full-catalog read and compares the resulting
summary with the pinned value. Cloned snapshots share the sealed-unit collection,
its summaries, and the sealed descriptor baseline through `Arc`; they do not
duplicate section bodies.

An `active.parts` scan is accepted only when the journal identity remains
unchanged across the attempt. The reader makes at most two scan attempts, and
makes the second only after the device, inode, length, or filesystem timestamps
changed. Stable corruption remains an error. A valid durable journal-v1 reset
marker is checked together with the old frames and treated as one complete,
logically empty journal state. PgKronika has not had a public release: journal
v1 is the first and only journal format, with no migration path.

A writer may finalize or reset the journal after a snapshot captured a part
reference. Opening that reference yields `ReadError::StaleSnapshot`. Query
helpers refresh a bounded number of times and surface a gap if the unit remains
unstable.

`LiveBuilder`, `LiveView`, and finalization reconciliation provide bounded
overview fold and handoff primitives. The production web refresh owner retains
one builder, folds only newly completed journal parts, and reconciles a live
generation with its exact finalized descriptor before publishing an immutable
timeline view. Ordinary logical-section requests continue to query
`LocalDirSnapshot`.

## Logical queries

`logical_section(name)` combines registered layout versions with that name.
Section queries:

1. select the units that overlap the requested time range;
2. decode only matching entries and dictionary sections;
3. union version columns and resolve strings;
4. order rows by the registry sort key;
5. return coverage gaps and an opaque next cursor.

`section` and `sections` use a row limit plus the hard 10,000,000-cell
materialization ceiling. `section_with_limits` and `sections_with_limits` let
an adapter spend a smaller request-wide cell budget. Exceeding it returns
`QueryError::ResultTooLarge` before retaining another row.

`QueryLimits::with_work_limits(QueryWorkLimits::new(...))` adds aggregate
request ceilings. `max_units` counts units inspected after time filtering,
`max_catalog_read_bytes` counts stored bytes admitted to open
candidate catalogs, and `max_dictionary_read_bytes` counts stored
dictionary-body bytes admitted after catalog confirmation. The defaults are
500,000 units, 64 MiB of catalog reads, and 64 MiB of dictionary reads. Before
admitting work over a ceiling, the query returns
`QueryError::WorkLimitExceeded { resource, limit, observed }`; a stale-open
retry is charged again.

The cursor pins the last returned key and query contract. A malformed or
cross-source cursor is rejected rather than treated as an offset.

The compact summary's 512-bit Bloom filter can rule out a section type with
non-zero rows without opening that PGM. It has no false negatives, but may
produce false positives. Every positive candidate is therefore opened and
confirmed against the real catalog before it contributes data. The
`source_summaries` path performs this work under typed `units`, `rows`, and
`bytes` limits and returns `LimitExceeded` without a partial result when a limit
is exhausted.

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

`SourceDescriptor` is the validated canonical catalog-layout digest;
`section_body_id` and `dictionary_context_id` derive typed content identities
from exact retained values.
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
large allocations. PgKronika owns the whole data directory. The journal and
owner locks are root-level objects; each sealed segment and its same-stem
sidecar are siblings in the UTC day derived from `SegmentId`:

```text
/data/active.parts
/data/YYYY/MM/DD/N.pgm
/data/YYYY/MM/DD/N.ovf
```

`FactStore` receives a verified `SegmentAddress` and resolves `N.ovf` through
the owned calendar tree; request strings cannot select another path. The
PGKOVF header stores the `FactKey`,
`SegmentLineageId`, canonical `SourceDescriptor`, source metadata, and the schema,
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

`GcConfig` bounds each traversal of the owned calendar tree. The defaults allow
100,000 visited entries, require two distinct authoritative GC generations and
120 seconds since the first absent observation, and retain recognized
publication temporary files for 600 seconds. An unavailable live set, a scan
error, or an entry-cap hit authorizes no deletion and does not advance grace.
GC admits a same-day, same-stem `.ovf` only after validating its PGKOVF header
against the corresponding live descriptor. It never follows symlinks or
removes PGM sources, `active.parts`, or the owner lock. A sidecar unlink also
takes the publication gate and checks the opened inode and device again.

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
