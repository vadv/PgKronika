# kronika-format

[Русская версия](README.ru.md)

`kronika-format` defines the binary contract that lets PgKronika finish writing
a segment in one process and open it later in another. It is not a standalone
converter or a generic analytics format.

Inside PgKronika, the crate connects the durable write and read paths:

```text
PostgreSQL / Linux / cgroup / PostgreSQL log
        |
        v
pg_kronika-collector collects one window
        |
        v
kronika-registry encodes typed rows as section bodies
        |
        v
kronika-writer builds a self-contained PGM part
        |
        v
active.parts --seal--> <first_timestamp>.pgm
                              |
                              v
                kronika-store / kronika-reader
                              |
                              v
                       pg_kronika-web
```

The format crate owns the `PGM1` and `PGMP` byte layouts, the end catalog,
CRC32C checksums, `StrId`, and the bounded dictionary model. It deliberately
does not own:

- PostgreSQL or Linux semantics and section schemas: those belong to
  [`kronika-registry`](../kronika-registry/README.md);
- Parquet encoding, buffering, journal I/O, and sealing: those belong to
  [`kronika-writer`](../kronika-writer/README.md);
- collection intervals, rotation, and source limits: those belong to
  [`pg_kronika-collector`](../../bins/pg_kronika-collector/README.md);
- filesystem discovery, typed queries, HTTP, retention, encryption, or remote
  storage.

## From a collection window to a segment

The current collector creates a fresh interner and row buffers for each
non-empty collection cycle. The registry sorts each snapshot section by its
contract key and encodes it. The writer adds dictionary sections, builds a
self-contained PGM part, wraps it in a `PGMP` frame, appends it to
`active.parts`, and calls `sync_data`.

The collector keeps appending windows until a size, age, forced-rotation, or
journal-cap condition closes the segment. Sealing then:

1. validates the catalogs of the recorded parts;
2. writes one leading `PGM1`;
3. copies every section body in journal and catalog order;
4. rewrites their absolute offsets into one end catalog;
5. synchronizes a sibling temporary file and publishes it without overwriting
   an existing destination;
6. resets `active.parts` only after publication succeeds.

This distinction matters for file size: the current `seal` path does **not**
merge, deduplicate, or re-encode Parquet bodies. It removes the per-part
framing and builds one catalog around the original bodies.

## Sealed PGM v1 layout

All integers are little-endian. In canonical output from the current writer,
fields are packed without alignment padding. For `N` catalog entries and `B`
total bytes in all section bodies:

```text
offset  size          contents
0       4             "PGM1"
4       B             section body 0, section body 1, ... in catalog order
4+B     32*N          catalog entries
...     40            catalog metadata
...     8             tail index
```

The exact file size is:

```text
pgm_bytes = B + 32*N + 52
catalog_len = 32*N + 40
```

The 52 fixed bytes are the leading magic, catalog metadata, and tail index.
There is no outer compression layer. These equations describe canonical writer
output; the low-level validator does not require an arbitrary v1 input to be
packed without unused gaps or overlapping section ranges.

### Catalog entry: 32 bytes

| Offset | Field | Type | Meaning |
| ---: | --- | --- | --- |
| 0 | `type_id` | `u32` | Section schema registered by `kronika-registry`. |
| 4 | `flags` | `u32` | Reserved; current writers store zero. |
| 8 | `offset` | `u64` | Absolute body offset from the first byte of the file. |
| 16 | `len` | `u64` | Body length in bytes. |
| 24 | `rows` | `u32` | Number of logical rows recorded for the body. |
| 28 | `crc32c` | `u32` | CRC32C of this section body. |

The same `type_id` can appear many times. In current collector output this
usually means that consecutive collection windows each emitted a body for the
same logical section. For chart types, repeated entries can instead describe
different entities. Catalog order is significant and is preserved.

### Catalog metadata: 40 bytes

| Offset | Field | Type | Meaning |
| ---: | --- | --- | --- |
| 0 | `min_ts` | `i64` | Earliest section timestamp, Unix microseconds. |
| 8 | `max_ts` | `i64` | Latest section timestamp, Unix microseconds. |
| 16 | `source_id` | `u64` | `StrId` of `{cluster_id}/{pg_system_identifier}`; zero means unspecified. |
| 24 | `entry_count` | `u32` | Number of 32-byte entries before this block. |
| 28 | `format_version` | `u32` | Container layout version; current writers store `1`. |
| 32 | `crc32c` | `u32` | CRC32C of entries and metadata with this field zeroed. |
| 36 | `reserved` | `u32` | Reserved; current writers store zero. |

### Tail index: 8 bytes

| Offset | Field | Type | Meaning |
| ---: | --- | --- | --- |
| 0 | `catalog_len` | `u32` | Entries plus the 40-byte metadata block; excludes the tail itself. |
| 4 | `magic` | 4 bytes | `"PGM1"`. |

A reader therefore starts at the end, not at the first section:

1. read the final eight bytes;
2. use `catalog_len` to find the catalog start;
3. decode and CRC-check the catalog;
4. read a selected body by its absolute `offset` and `len`;
5. verify the body CRC before handing the bytes to the registry codec.

### Concrete offset example

Suppose a segment contains two bodies of 100 and 250 bytes. Then `B=350`,
`N=2`, `catalog_len=104`, and the file occupies 466 bytes:

| Byte range | Size | Contents |
| --- | ---: | --- |
| `[0, 4)` | 4 | `"PGM1"` |
| `[4, 104)` | 100 | First section body; its catalog entry has `offset=4`, `len=100`. |
| `[104, 354)` | 250 | Second body; its entry has `offset=104`, `len=250`. |
| `[354, 418)` | 64 | Two 32-byte catalog entries. |
| `[418, 458)` | 40 | Catalog metadata. |
| `[458, 466)` | 8 | `catalog_len=104` followed by `"PGM1"`. |

In current PgKronika files, a data or dictionary body is normally a
self-contained Parquet file, so it has its own `PAR1 ... PAR1` framing. The PGM
container treats those bytes as opaque; it does not parse column metadata.

The repository also contains a byte-exact
[`minimal.pgm`](tests/fixtures/minimal.pgm) fixture. It is 88 bytes:
`PGM1`, one four-byte body `01 02 03 04`, one catalog entry, metadata, and the
tail. [`tests/fixture.rs`](tests/fixture.rs) records every offset and verifies
that the encoder reproduces the fixture byte for byte.

## What section bodies contain today

`kronika-format` permits opaque bodies, but the current PgKronika writer uses
self-contained Parquet for both snapshot and dictionary sections:

- Zstd level 3 compresses every body independently;
- snapshot rows are sorted by the registry contract's canonical key, while
  dictionary rows are sorted by `str_id`;
- one body is limited to 65,536 rows and 8 MiB;
- the decoder accepts no more than 16 row groups;
- the redundant Arrow schema metadata is omitted and `created_by` is cleared
  to an empty string.

Omitting the embedded Arrow schema removes a duplicate logical schema from
every body; the exact saving depends on the section contract. The native
Parquet schema remains because the decoder needs the physical column layout.

There is no whole-file Zstd pass. A reader can locate and verify one body
without decompressing unrelated sections.

## Strings, plans, and other large values

Snapshot rows refer to repeated text through `StrId`, which is
`xxh3_64(original_bytes)` with zero reserved for "no value". The dictionary
model detects hash collisions and inconsistent placement within one
`SegmentDicts` instance instead of silently aliasing two values.

The current writer emits two sorted Parquet section types:

| Section | Stored columns |
| --- | --- |
| `dict.strings` | `str_id`, complete `bytes` |
| `dict.blobs` | `str_id`, `stored_bytes`, `full_len`, `truncated`, optional `full_sha256` |

For a truncated value, `str_id`, `full_len`, and SHA-256 still identify the
original bytes while only a prefix is kept in the segment. The full value
cannot be reconstructed from that prefix.

`SegmentDicts` can mark a short string as required for the modeled
`dict.hot_strings` subset. Required hot strings are exempt from the stored-byte
cap. The current writer does not emit a separate hot-string section, however;
it writes only `dict.strings` and `dict.blobs`.

The reusable model and the collector intentionally use different limits:

| Limit | `DictLimits::default()` | Current collector | Effect |
| --- | ---: | ---: | --- |
| Blob threshold | 4 KiB | 4 KiB | Values at or above it use `dict.blobs`. |
| Truncation limit | 1 MiB | 64 KiB | Longer values retain exactly this prefix. |
| Stored-byte cap | 64 MiB | 16 MiB | Rejects an ordinary new value when stored dictionary bytes would exceed the cap; required hot values are exempt. |

The collector values are currently fixed in code, not environment variables.
The byte cap counts stored value bytes after truncation, not Parquet metadata.
For an ordinary new value, exceeding it returns `DictError::Full`; it is not an
absolute encoded-section limit because required hot strings remain exempt.

Most importantly, the current collector creates a new interner for every
collection cycle. Equal values are deduplicated within that window, but the
same `str_id` can be physically written again in later windows of the same
sealed segment. Collision checks are likewise window-local in the current
collector; the reader does not compare repeated string bytes across windows.

## `active.parts` and crash recovery

`active.parts` is the durable journal of an unfinished segment. Each frame has
a 16-byte header followed by one complete PGM part:

```text
"PGMP" | part_len: u64 | header_crc32c: u32 | PGM part
```

The header checksum covers its first 12 bytes. The PGM part has its own
`PGM1`, bodies, catalog, and tail, so a frame can be validated before it is
accepted.

For a clean journal with `P` frames, `N` total section entries, and `B` total
body bytes:

```text
active_parts_bytes = B + 32*N + 68*P
sealed_pgm_bytes   = B + 32*N + 52
seal_saves         = 68*P - 52
```

This saving is only framing. With 70 windows it is 4,708 bytes; the Parquet
bodies and all `N` catalog entries still remain.

The default maximum accepted part is 64 MiB. The streaming recovery scanner
keeps one PGM part body of at most `max_part_len` plus a resynchronization
window in memory. The default resynchronization window is 1 MiB and is a
separate scanner argument, not a `JournalLimits` field.

Recovery classifies malformed regions instead of treating all damage alike:

| Classification | Meaning | `Journal::open` | Collector startup |
| --- | --- | --- | --- |
| `TornTail` | The final frame is incomplete, or its valid header and declared length end exactly at EOF but the inner PGM fails validation. | Truncates to the last valid boundary. At most the final part is lost. | Processes valid preceding parts under the recovery rule below. |
| `Middle { resumed_at }` | A malformed region is followed by another valid frame. | Preserves the bytes for diagnostics and reports valid parts on both sides. | If any valid parts were reported, processes them and then resets the entire journal. |
| `QuarantinedTail` | A malformed terminal region has no later valid frame. | Preserves the bytes and reports valid earlier parts. | If any valid parts were reported, processes them and then resets the entire journal. |

On collector startup, recovered parts containing timestamped data are sealed
immediately. Parts without any timestamped data are discarded by resetting the
journal without creating a PGM. A successful seal resets the whole journal,
including damaged bytes that `Journal::open` preserved. If the seal fails, the
collector logs the failure, also resets the journal, and starts collecting
fresh data. If recovery finds no valid part, no recovery PGM is created:
`TornTail` has already been truncated, while other damaged bytes remain and
new frames can be appended after them.

## Where the bytes go

For the current implementation, most size variation comes from section
bodies, not the 52-byte container constant or 32-byte catalog entries.

### Size-related behavior already implemented

- Zstd level 3 is applied separately to each Parquet body.
- Rows are written in canonical order, which makes encoded output
  deterministic; any compression benefit depends on the data.
- Repeated strings within one collection window use `StrId` dictionary rows.
- Long values can move to `dict.blobs` and be truncated with their original
  length and SHA-256 retained.
- Redundant Arrow schema metadata is not written, and `created_by` is empty.
- Source intervals, top-N limits, and plan-text budgets bound the amount of
  data admitted before encoding.

### Why logical data repeats across collection windows

Each collection window writes a separate Parquet body for every non-empty
type. A one-row snapshot still pays for a Parquet schema, column metadata, and
footer. Over a 15-minute segment, the same type can therefore have dozens of
small bodies and dozens of footers.

Dictionary sections are also emitted per window. Because the collector does
not carry one dictionary across the whole open segment, common database names,
query text, and plan text can recur in many bodies. The current seal operation
preserves both forms of repetition.

### Experimental 16.5-16.7x repack estimate

The repository contains the ignored
[`repack_estimate`](../../bins/pg_kronika-demo/tests/repack_estimate.rs) test,
which is run manually. It grouped bodies by `type_id` and, for each group that
decoded, estimated one Zstd-3 Parquet body; a parse or write failure falls back
to the original size. Within each dictionary `type_id`, it kept the first row
for every `str_id` without checking that duplicate rows had equal remaining
columns. The report says every body parsed, but the test does not assert that
condition, and its original output and PGM inputs were not committed. The
proposed transformation keeps the PGM v1 container layout.

| Captured segment | Current PGM | Estimated repack | Ratio |
| --- | ---: | ---: | ---: |
| First 15 minutes | 17.8 MiB | about 1.08 MiB | 16.5x |
| Second 15 minutes | 17.6 MiB | about 1.06 MiB | 16.7x |
| Final 7 minutes | 5.4 MiB | about 0.53 MiB | 10.3x |

The often quoted "17x" is the rounded 16.7x result, not a separate
measurement. The main causes found in that dataset were:

- one-row Parquet sections occupying 2.4-6.8 KiB each;
- 458 lock-tree rows spread across 211 mostly empty sections and occupying
  1.9 MiB;
- 71,468 dictionary rows but only about 3,200 unique `str_id` values, or
  an average of roughly 22 rows per unique value;
- finished PGM files shrinking to 9-10% under an external `zstd -19` control
  experiment, confirming substantial cross-body repetition.

These numbers show optimization headroom, not current production compression:

- production `seal` still copies bodies without merging or deduplicating them;
- the test estimated body sizes and did not write and round-trip a replacement
  production PGM;
- the experimental writer allowed a row group of 1,000,000 rows and did not
  enforce the production limits of 65,536 rows and 8 MiB per section, so a
  cap-compatible set of reader-valid v1 sections was not proven;
- the source run lasted 37 minutes, not one hour; the note's 71 MiB/hour is
  `17.8 MiB * 4`, while all three files totalled 40.8 MiB in 37 minutes
  (about 66.2 MiB/hour);
- the projected 4.3 MiB/hour after repack was also an extrapolation;
- PostgreSQL log collection was disabled and `cross_source_join` failed in
  that run; both paths were fixed later, so the measurement must be repeated
  on a current complete dataset;
- the original PGM files were not committed, and ignored tests do not run this
  measurement in normal CI.

The exact methodology, commands, and remaining correctness work are in
[`2026-07-24-pgm-compaction.md`](../../docs/superpowers/plans/2026-07-24-pgm-compaction.md).
Implementing compaction requires canonical re-sorting, dictionary validation,
cap-aware section splitting, a semantic round trip, and new end-to-end
measurements; it is not only a compression-level change.

## Parameters that affect file size

The format layout itself has no compression knobs. Operators control the
amount and grouping of collected data through `pg_kronika-collector`:

| Control | Default | What it changes |
| --- | ---: | --- |
| Per-source `*_INTERVAL_S` variables | 1-3600 s, depending on source | Raising an interval produces fewer snapshots and Parquet footers, at the cost of time resolution. `KRONIKA_INTERVAL_S=5` is only the scheduler tick. |
| `KRONIKA_PG_MAX_TABLES`, `KRONIKA_PG_MAX_INDEXES`, `KRONIKA_PG_MAX_STATEMENTS`, `KRONIKA_PG_MAX_PLANS` | `500` | Lower values reduce high-cardinality rows and dictionary text but omit lower-ranked objects. |
| `KRONIKA_PG_MAX_PLAN_TEXT` | 32,768 bytes | Maximum stored text for one plan read. |
| `KRONIKA_PG_PLAN_TEXT_BUDGET` | 8 MiB | Total plan-text budget per read; zero disables plan text. |
| `KRONIKA_SEGMENT_MAX_BYTES` | 64 MiB | Seals after this many raw `active.parts` bytes; zero seals every window. It changes file granularity, not Parquet compression. |
| `KRONIKA_SEGMENT_MAX_AGE_S` | 900 s | Maximum age of an open segment. It mainly changes time span and file count. |
| `KRONIKA_JOURNAL_MAX_BYTES` | 1 GiB | Before every append except the first frame, exceeding this threshold causes an early seal. It is not a target PGM size. |

Changing source limits trades observability for disk use. Changing only segment
age or rotation size does not remove repeated per-window footers or dictionary
rows in the current writer. See the
[collector configuration](../../bins/pg_kronika-collector/README.md)
for every source interval and validation rule.

## Integrity, limits, and compatibility

CRC32C covers every section body, the catalog, and each `PGMP` header. It
detects accidental corruption; it is not authentication, a signature, or
encryption.

`kronika-format` validates framing, catalog length and checksum, section
bounds, and section checksums for complete parts. Higher layers add policy:
the current sealed reader accepts container version 1 and caps catalog,
section, row, and row-group sizes. An incompatible section schema receives a
new `type_id`; changing the PGM framing requires a new container version.

Sources of truth:

- [`src/lib.rs`](src/lib.rs), [`src/catalog.rs`](src/catalog.rs), and
  [`src/parts.rs`](src/parts.rs) for the byte contract;
- [`src/dictionary.rs`](src/dictionary.rs) for dictionary invariants;
- [`kronika-registry`](../kronika-registry/README.md) for section schemas and
  Parquet limits;
- [`kronika-writer`](../kronika-writer/README.md) for append and seal behavior;
- [`docs/segment-format.md`](../../docs/segment-format.md) only as a historical
  design note. Its proposed hot zones, charts, and compaction are not the
  current v1 implementation.
