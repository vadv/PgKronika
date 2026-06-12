# kronika-format

[Русская версия](README.ru.md)

`kronika-format` contains the byte-level primitives for the PGM container:

- file magic `PGM1`;
- catalog entries;
- catalog metadata;
- tail index;
- CRC32C;
- `str_id` and the per-segment dictionary model;
- `active.parts` journal frame validation and scanning.

It does not know how section bodies are encoded. Parquet sections, events,
charts, storage backends, and I/O are handled by other crates. This README
describes the container subset implemented by this crate.

## Current Contents

The crate currently exposes:

- `MAGIC` and `FORMAT_VERSION`;
- `ENTRY_LEN`, `META_LEN`, and `TAIL_INDEX_LEN` size constants;
- `Entry`, one 32-byte catalog row;
- `Catalog`, the decoded end catalog and metadata;
- `TailIndex`, the final 8-byte pointer to the catalog;
- `DecodeError`, typed catalog and tail-index decode errors;
- `crc32c`, the checksum used by the container;
- `StrId`, the interned string id (`xxh3_64`);
- `SegmentDicts` with `DictLimits`, `BlobEntry`, `Resolved`, `DictStats`,
  and the `DictError` / `InvalidLimits` error types;
- `FrameHeader`, `JournalLimits`, `PartRef`, `ScanReport`, and the
  `active.parts` validation and scan errors.

Later implementation steps add HOT block headers and dictionary section
encoding.

## File Layout

```text
segment.pgm
┌─────────────────────────────────────────┐
│ MAGIC "PGM1"                 4 B        │
│ section bodies               ...        │  <- opaque to this crate
│ catalog entries              32 B each  │
│ catalog metadata             40 B       │
│ tail index                   8 B        │
└─────────────────────────────────────────┘
```

All integer fields are little-endian. `offset` and `len` in a catalog entry are
absolute offsets from the beginning of the segment file to the section body.

## Catalog Entry

Each catalog entry is exactly 32 bytes.

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `type_id` | `u32` | section type from the registry |
| 4 | `flags` | `u32` | reserved, written as zero |
| 8 | `offset` | `u64` | absolute offset of the section body |
| 16 | `len` | `u64` | section body length in bytes |
| 24 | `rows` | `u32` | rows or records in the section |
| 28 | `crc32c` | `u32` | CRC32C of the section body |

`type_id` may repeat. Repeated entries are parts of one logical section in
catalog order. The exception is chart sections (class 10): there repeated
entries describe different entities, and splitting one chart into parts is
forbidden. `Catalog` preserves the on-disk order.

## Catalog Metadata

The metadata block is exactly 40 bytes and follows all entries.

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `min_ts` | `i64` | minimum segment timestamp, unix microseconds |
| 8 | `max_ts` | `i64` | maximum segment timestamp, unix microseconds |
| 16 | `source_id` | `u64` | `str_id` of `{cluster_id}/{pg_system_identifier}`, or `0` |
| 24 | `entry_count` | `u32` | number of catalog entries |
| 28 | `format_version` | `u32` | container version |
| 32 | `crc32c` | `u32` | CRC32C over entries and metadata with this field zeroed |
| 36 | `reserved` | `u32` | reserved, written as zero |

`format_version` versions the container layout only. Data schemas evolve through
new `type_id` values in the registry.

## Tail Index

The tail index is the final 8 bytes of the segment.

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `catalog_len` | `u32` | length of entries plus metadata |
| 4 | `magic` | 4 bytes | ASCII `PGM1` |

Opening a segment starts from these 8 bytes.

## Encode Path

`Catalog::encode` produces everything that follows the last section body in a
segment file: entries, metadata with the CRC patched in, and the 8-byte tail
index, as one buffer. It panics if the encoded catalog does not fit `u32`;
that is treated as a writer bug, not a recoverable error.

`Catalog::encoded_len` returns the length of entries plus metadata — the
`catalog_len` value stored in the tail index. `TailIndex::encode` produces the
final 8 bytes on its own.

## Decode Path

1. Read the last 8 bytes and call `TailIndex::decode`.
2. Use `catalog_len` to read the bytes immediately before the tail index.
3. Call `Catalog::decode`.
4. Read section bodies using `offset` and `len` from the decoded entries.

`Catalog::decode` checks:

- byte length: `entry_count * 32 + 40`;
- stored `entry_count` against the length-derived count;
- catalog CRC32C before parsing entries.

Decode errors include the observed values where that helps debugging: bad tail
magic, invalid catalog length, entry-count mismatch, and CRC mismatch.

## CRC32C

All container checksums use CRC32C, the Castagnoli polynomial
(`CRC_32_ISCSI`). The catalog checksum is computed over catalog entries and
metadata with the metadata `crc32c` field set to zero.

## String Ids and Dictionaries

Every text value of a segment — SQL, plans, object names, `cmdline`, event
payloads, chart series names — is referenced by `str_id = xxh3_64(bytes)`
and stored once in the segment dictionaries. Zero is reserved as the
"no value" sentinel: an input that hashes to zero is treated as a collision
and never enters a dictionary. `StrId` only represents non-zero ids:
`StrId::from_raw(0)` returns `None`, and `StrId::get()` returns the raw
`u64` written on disk.

`SegmentDicts` models the three dictionaries of one segment:

- `dict.strings` — values shorter than `blob_threshold` (default 4 KiB);
- `dict.blobs` — larger values, plus values the registry forces into blobs
  regardless of size (e.g. query plans, via `intern_blob`);
- `dict.hot_strings` — a duplicate cache for frequently needed short strings,
  always a subset of
  `dict.strings`.

Placement is decided by the set of requirements accumulated per value,
never by call order: interning the same values with the same requirements
in any order yields identical dictionaries. A value required both hot and
in blobs is a typed `PlacementConflict` error. A soft hot request
(`intern_hot_best_effort`, for event labels) returns the id plus a flag that
says whether the value was added to `dict.hot_strings`.

A value longer than `truncate_limit` (default 1 MiB) keeps only a prefix
of exactly that length; `str_id`, `full_len`, and `full_sha256` are always
computed over the full original value. Deduplication and collision checks
for truncated entries therefore compare `(full_len, full_sha256)` rather
than the stored prefix.

A `str_id` collision inside one segment is unrecoverable by design: the
writer aborts the segment (`DictError::Collision`). Both default limits
are starting values of open format questions, so they are parameters of
`DictLimits`, not constants baked into the logic.

Encoding dictionaries into on-disk section bytes is left for the typed
section codecs.

## Parts Journal

`active.parts` is an append-only journal of `PGMP` frames. Each frame contains
a 16-byte header followed by one mini-PGM part.

```text
frame
┌─────────────────────────────────────────┐
│ magic "PGMP"                 4 B        │
│ part_len                     8 B        │
│ header_crc32c                4 B        │
│ part bytes                   part_len   │
└─────────────────────────────────────────┘
```

`FrameHeader::decode` validates the frame magic and header CRC. `validate_part`
checks that the part is a self-contained mini-PGM container: segment magic, tail
index, catalog CRC, section bounds, and section CRCs.

`scan_journal` walks a journal buffer and returns:

- valid parts in journal order;
- damaged regions and their classification;
- the valid prefix length.

An incomplete final frame means the writer was interrupted before the last
frame was fully written. It is safe to truncate the file to `valid_len` and
continue. Middle damage means a later valid frame was found; parts before and
after the damaged region are kept. Final damage means the scanner found damage
at the end and no later valid frame.

## Open Questions

Three sizes are starting values, not settled format decisions. Code keeps them
as configuration, not fixed format constants:

- **`blob_threshold`, default 4 KiB** — the boundary between
  `dict.strings` and `dict.blobs`. It trades read behavior: everything in
  `dict.strings` is loaded eagerly when a segment is opened, while blobs
  are fetched on demand. A threshold set too high makes the eagerly loaded
  dictionary too large; too low pushes common labels into the lazy path and
  adds round trips.
- **`truncate_limit`, default 1 MiB** — the size above which only a
  prefix of a value is stored (with `full_sha256` keeping the identity of
  the original). It limits how much space a single giant plan or query text
  can take in a segment. Too low loses diagnostic detail; too high lets one
  value dominate a segment.
- **`max_part_len`, default 64 MiB** — the largest part accepted by the
  `active.parts` scanner. A frame that claims a larger part is treated as
  damaged.

Resynchronization after a damaged frame deliberately has no search window:
the scanner first tries the boundary implied by a sane frame header, then
searches to the end of the buffer, so frames appended after a damaged
region are always rediscovered, however large the region is.

These numbers should be settled by measuring real segments and journals:
dictionary sizes, catalog sizes, truncation frequency, and part sizes. Until
then they are constructor parameters of `DictLimits` and `JournalLimits`, with
defaults as starting values.

## Tests

- `tests/fixture.rs` decodes and re-encodes a byte-exact minimal segment fixture.
  The fixture catches drift between the implementation and the documented byte
  layout.
- `tests/property.rs` checks encode/decode roundtrips and single-byte corruption
  detection for generated catalogs.
- `tests/dict_property.rs` checks the dictionary contract on random data:
  every issued id resolves, the dictionaries stay disjoint, hot is a subset
  of strings, re-interning is idempotent, and placement is independent of
  call order.
- `tests/parts_property.rs` checks `active.parts` recovery: truncation keeps
  the fully written prefix, and single-byte corruption is reported instead of
  making a part disappear without a damaged-region record.

## Crate Boundaries

This crate contains only the byte layouts shared by writers and readers. Today
that means the end catalog, tail index, CRC32C, `str_id`, the dictionary model,
and `active.parts` frame validation. Later steps add dictionary section
encoding and HOT block headers here.

Code that needs collector state, domain knowledge, or external I/O stays above
this layer:

- `kronika-writer` manages the string interner, buffers, merge, and segment
  completion logic;
- `kronika-reader` opens segments, reads sections, and manages caches;
- `kronika-registry` defines `type_id` meaning and section schemas;
- `kronika-derive` and `kronika-writer` handle Parquet schemas and encoding;
- `kronika-store*` handles local, HTTP, and S3 storage access;
- `kronika-source-*` collects data from PostgreSQL, OS, cgroup, and logs.
