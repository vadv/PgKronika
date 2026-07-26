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

The `active.parts` journal and finished `.pgm` files are direct children of
`KRONIKA_OUT_DIR`. For example:

```text
$KRONIKA_OUT_DIR/
|-- active.parts
|-- <first_timestamp>.pgm
`-- <next_first_timestamp>.pgm
```

There are no separate files for string values. While a segment is open,
dictionary bodies live in self-contained PGM parts inside `active.parts`.
Sealing decodes those parts, normalizes their dictionaries, and writes one
compact PGM.

The format crate owns the PGM framing, journal-frame bytes, end catalog,
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

The collector creates a fresh interner and row buffers for each non-empty
collection cycle. The registry sorts each snapshot section by its contract
key and encodes it. The writer adds dictionary sections, builds a
self-contained PGM part, wraps it in a 16-byte journal frame, appends it to
`active.parts`, and synchronizes the append.

The collector keeps appending windows until age, size, forced rotation,
journal pressure, or compact-output admission closes the segment. Sealing:

1. validates and admits every recorded part before publication work;
2. decodes each data section and creates a canonically sorted run;
3. merges runs with fixed fan-in 32, spilling bounded intermediate bodies
   beside the destination;
4. validates and deduplicates dictionary records by `str_id`;
5. writes exactly one compact body for every present `type_id`, followed by
   one end catalog and tail index;
6. synchronizes the sibling temporary file, publishes without replacing a
   different destination, and synchronizes the parent directory;
7. lets the collector reset `active.parts` only after publication succeeds.

Seal failure leaves the synchronized journal available for another attempt.
The writer reports source, spill, output, write, and admitted-memory totals for
release qualification and operator diagnostics.

### Windows, parts, sections, and bodies

The format description uses four different levels:

- a **collection window** is one non-empty collector cycle;
- a **PGM part** is a self-contained record of one window, with its own PGM magic,
  bodies, catalog, and tail index;
- a **section** is one catalog entry: `type_id`, offset, length, row count, and
  CRC32C;
- a **section body** is the byte range addressed by a catalog entry. The
  writer places one self-contained Parquet file in that range.

While the segment is open, every part is wrapped in its own journal frame:

```text
active.parts
|
+-- journal frame #1
|   `-- PGM part for window #1
|       |-- PGM marker
|       |-- data and dictionary bodies
|       |-- part catalog
|       `-- tail index
|
`-- journal frame #2
    `-- PGM part for window #2
        |-- PGM marker
        |-- data and dictionary bodies
        |-- part catalog
        `-- tail index
```

Within each part, the writer places non-empty data bodies in ascending
`type_id` order, followed by `dict.strings` and then `dict.blobs`. An absent
dictionary section occupies no space. Readers locate every body through the
catalog.

Completing the segment coalesces the parts:

```text
active.parts                         <first_timestamp>.pgm

frame [part for window #1]             PGM marker
frame [part for window #2] -- seal() -> one body per present type_id
...                                   one normalized dict.strings body, if any
                                      one normalized dict.blobs body, if any
                                      one end catalog
                                      one tail index
```

The finished PGM has no explicit window-boundary markers. Rows from every
admitted window are in canonical registry order, and the catalog describes
section types rather than collection windows.

## Sealed PGM layout

All integers are little-endian. In canonical output from the current writer,
fields are packed without alignment padding. For `N` catalog entries and `B`
total bytes in all section bodies:

```text
offset  size          contents
0       4             PGM magic
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

The 52 fixed bytes are the leading marker, catalog metadata, and tail index.
There is no outer compression layer. Current PGM bodies are packed without
gaps, and `type_id` values are strictly increasing.

### Catalog entry: 32 bytes

| Offset | Field | Type | Meaning |
| ---: | --- | --- | --- |
| 0 | `type_id` | `u32` | Section schema registered by `kronika-registry`. |
| 4 | `flags` | `u32` | Reserved; current writers store zero. |
| 8 | `offset` | `u64` | Absolute body offset from the first byte of the file. |
| 16 | `len` | `u64` | Body length in bytes. |
| 24 | `rows` | `u32` | Number of logical rows recorded for the body. |
| 28 | `crc32c` | `u32` | CRC32C of this section body. |

Each present `type_id` appears exactly once. Catalog entries and their packed
bodies are ordered by ascending `type_id`.

### Catalog metadata: 40 bytes

| Offset | Field | Type | Meaning |
| ---: | --- | --- | --- |
| 0 | `min_ts` | `i64` | Earliest section timestamp, Unix microseconds. |
| 8 | `max_ts` | `i64` | Latest section timestamp, Unix microseconds. |
| 16 | `source_id` | `u64` | Source identifier; zero means unspecified. |
| 24 | `entry_count` | `u32` | Number of 32-byte entries before this block. |
| 28 | `format_version` | `u32` | Internal catalog contract value; writers store `1`. |
| 32 | `crc32c` | `u32` | CRC32C of entries and metadata with this field zeroed. |
| 36 | `reserved` | `u32` | Reserved; current writers store zero. |

The collector copies `KRONIKA_SOURCE_ID` into this field and does not require a
matching `dict.strings` record. Readers treat `source_id` as an opaque
identifier, not a resolvable string reference.

### Tail index: 8 bytes

| Offset | Field | Type | Meaning |
| ---: | --- | --- | --- |
| 0 | `catalog_len` | `u32` | Entries plus the 40-byte metadata block; excludes the tail itself. |
| 4 | `magic` | 4 bytes | Opaque PGM marker. |

A reader therefore starts at the end, not at the first section:

1. read the final eight bytes;
2. use `catalog_len` to find the catalog start;
3. decode and CRC-check the catalog;
4. read a selected body by its absolute `offset` and `len`;
5. verify the body CRC before handing the bytes to the registry codec.

### Example: a snapshot and two dictionaries in one PGM

The following simplified PGM for PostgreSQL 15-18 contains one
`pg_stat_activity` body, one dictionary body for short string values, and one
dictionary body for large values. Let their sizes be `S`, `T`, and `L`, with
`B = S + T + L`.

```text
offset

0       +----------------------------------------------------------+
        | PGM magic                                                | 4 bytes
4       +----------------------------------------------------------+
        | body #0: pg_stat_activity, type_id=1_001_003             | S bytes
        | Parquet: ts | pid | datname=H | query=Q | ...            |
4+S     +----------------------------------------------------------+
        | body #1: dict.strings, type_id=3_001_001                 | T bytes
        | Parquet: str_id | bytes                                  |
4+S+T   +----------------------------------------------------------+
        | body #2: dict.blobs, type_id=3_002_001                   | L bytes
        | Parquet: str_id | stored_bytes | full_len | ...          |
4+B     +----------------------------------------------------------+
        | catalog entry #0: type=1_001_003, offset=4, len=S        | 32 bytes
36+B    | catalog entry #1: type=3_001_001, offset=4+S, len=T      | 32 bytes
68+B    | catalog entry #2: type=3_002_001, offset=4+S+T, len=L    | 32 bytes
100+B   +----------------------------------------------------------+
        | catalog metadata                                         | 40 bytes
140+B   +----------------------------------------------------------+
        | catalog_len=136 | PGM magic                              | 8 bytes
148+B   +----------------------------------------------------------+
```

For three bodies, `catalog_len = 3 * 32 + 40 = 136`, so the complete file is
`B + 148` bytes. If the segment has no large values, the `dict.blobs` body and
its catalog entry are absent.

In current PgKronika files, a data or dictionary body is normally a
self-contained Parquet file, so it has its own `PAR1 ... PAR1` framing. The PGM
container treats those bytes as opaque; it does not parse column metadata. The
Parquet rows shown above are decoded contents, not literal plain text in the
file: internal encoding and Zstd mean that text bytes need not occur verbatim
in the PGM.

The repository also contains a byte-exact
[`minimal.pgm`](tests/fixtures/minimal.pgm) fixture. It is 88 bytes:
PGM magic, one four-byte body `01 02 03 04`, one catalog entry, metadata, and the
tail. [`tests/fixture.rs`](tests/fixture.rs) records every offset and verifies
that the encoder reproduces the fixture byte for byte.

## Compact section bodies

`kronika-format` treats bodies as opaque bytes. The production writer
uses this physical profile for every final data and dictionary body:

| Property | Current value |
| --- | --- |
| Rows | At most 65,536 |
| Encoded body | At most 8 MiB |
| Row groups | Exactly one |
| Data pages | Exactly one per column chunk |
| Value encoding | `PLAIN`; Parquet dictionary encoding disabled |
| Compression | Zstandard level 6 |
| Statistics and page indexes | Disabled |
| Arrow schema metadata | Omitted |
| `created_by` | Empty |

Data rows use the registry's complete canonical order: declared sort-key
columns first, then every remaining physical column as a deterministic
tie-break. Equal physical rows retain multiplicity. Dictionary rows are
strictly ordered by `str_id`.

Writer admission proves row, page, encoded-body, reader-work, memory, input,
spill, and output bounds before publication. The reader validates container,
schema, row, byte, decoded-memory, and CRC invariants; writer-specific Parquet
choices are checked by writer tests, the all-layout oracle, lifecycle BDD, and
release qualification.

There is no whole-file compression pass. Each Parquet body, the PGM catalog,
and journal framing remain independently addressable, so a reader can locate
and verify one body without decompressing unrelated sections.

## Where string values physically live

A snapshot section does not store the original text in every record. A
`StrId` column stores only a `u64` equal to `xxh3_64(original_bytes)`. Zero
means "no value". The number is not a file offset: the original bytes must be
found under the same id in separate dictionary Parquet bodies.

The current writer emits two dictionary section types:

| `type_id` | Section | Columns after decoding |
| ---: | --- | --- |
| `3_001_001` | `dict.strings` | `str_id`, complete `bytes` |
| `3_002_001` | `dict.blobs` | `str_id`, `stored_bytes`, `full_len`, `truncated`, optional `full_sha256` |

Both sections are **inside the PGM**, alongside snapshot sections.
`dict.blobs` is not a separate file, PostgreSQL TOAST, or external object
storage.

### The short string `postgres`

For the bytes `postgres`,
`H = xxh3_64(b"postgres") = 0x0939566173e67ada`. If two PostgreSQL server
processes in one window are connected to this database, the data occupies one
physical file as follows. Parquet contents are shown after decoding; unrelated
bodies are omitted:

```text
$KRONIKA_OUT_DIR/<first_timestamp>.pgm  (one file)

[ PGM magic ]
[ pg_stat_activity Parquet body, type_id=1_001_003
  after decoding:
  pid | datname
  101 | H
  102 | H ]
[ dict.strings Parquet body, type_id=3_001_001
  after decoding:
  str_id | bytes
  H      | b"postgres" ]
[ end catalog
  offset,len -> entire pg_stat_activity body
  offset,len -> entire dict.strings body ]
[ tail index ]
```

The snapshot section therefore contains two logical `StrId` values represented
as `u64`, while the dictionary contains one record with the `postgres` bytes.
One level deeper, their placement is:

```text
PGM catalog entry: type_id=3_001_001, offset=X, len=T
                            |
                            v
byte range [X, X+T): self-contained Parquet body
|-- "PAR1"
|-- row group #0
|   |-- str_id column chunk: encodes H
|   `-- bytes column chunk: encodes b"postgres"
|-- Parquet metadata and metadata length
`-- "PAR1"
```

The bytes belong to the `bytes` column chunk of the `dict.strings` body. They
are PLAIN-encoded in its single Zstandard-6 data page. The
`pg_stat_activity` body keeps only `H`. The PGM catalog stores the offset and
length of the entire dictionary body, not of one value; finer offsets belong
to Parquet metadata. Resolving `H` therefore requires decoding the dictionary
body.

The "after decoding" blocks show logical values, not on-disk bytes. The exact
cost of the two references need not be 16 bytes, and encoding and compression
mean that `postgres` need not occur as a plain byte sequence in the file.

Reading follows the end catalog:

```text
(1) tail index -> end catalog

(2) catalog -> pg_stat_activity body
            -> decode Parquet
            -> datname=H

(3) catalog + H -> dict.strings and/or dict.blobs body
                -> decode record with str_id=H
                -> b"postgres"
                -> datname="postgres"
```

PGM has no per-value `StrId -> offset` index. A full dictionary read visits the
single body of each present dictionary type and builds an in-memory map. The
targeted overview path reads the same bodies but retains only ids requested in
advance.

### Large and truncated values

Under the current collector limits, placement works as follows:

| Original value | Section | Bytes retained on disk |
| --- | --- | --- |
| `postgres`, 8 bytes | `dict.strings` | All 8 bytes. |
| Plan text, 20 KiB | `dict.blobs` | All 20 KiB, `full_len=20,480`, `truncated=false`, and no SHA-256. |
| `/proc/PID/cmdline`, 80 KiB | `dict.blobs` | The first 64 KiB, `full_len=81,920`, `truncated=true`, and SHA-256 of the original 80 KiB. |

A truncated value's `StrId` is computed over all original bytes, not the stored
prefix. The discarded suffix cannot be recovered from the PGM. Plan text
defaults to `KRONIKA_PG_MAX_PLAN_TEXT=32,768`, so it can enter `dict.blobs` but
does not reach the truncation threshold under default settings. Even the
allowed 64 KiB plan maximum is byte-capped by the source before interning, so
the dictionary treats the bytes it receives as the complete value.

The reusable model and the collector intentionally use different limits:

| Limit | `DictLimits::default()` | Current collector | Effect |
| --- | ---: | ---: | --- |
| Blob threshold | 4 KiB | 4 KiB | Values at or above it use `dict.blobs`. |
| Truncation limit | 1 MiB | 64 KiB | Longer values retain exactly this prefix. |
| Stored-byte cap | 64 MiB | 16 MiB | Rejects an ordinary new value when stored dictionary bytes would exceed the cap; required hot values are exempt. |

The collector values are currently fixed in code, not environment variables.
The byte cap counts stored value bytes after truncation, not Parquet metadata.
For an ordinary new value, exceeding it returns `DictError::Full`; it is not an
absolute encoded-section limit because required hot strings remain exempt. The
current collector does not automatically complete the segment at 16 MiB:
depending on the source, the cycle fails or individual records or text fields
are omitted.

`SegmentDicts` also models a `dict.hot_strings` subset, but the current writer
does not emit a third dictionary section. Those values are written into the
ordinary `dict.strings` body; there is no `dict.hot_strings` body to find in the
file.

### Segment-wide normalization

Each collection-window interner detects collisions and avoids duplicate bytes
within its part. Seal then normalizes all parts for the segment. Equal
`str_id` records with equal bytes and metadata collapse to one row. Conflicting
bytes, truncation metadata, or placement in both dictionary sections fail the
seal before publication. The resulting PGM contains each non-zero `str_id`
exactly once and in ascending order.

## `active.parts` and crash recovery

`active.parts` is the durable journal of an unfinished segment. Each frame has
a 16-byte header followed by one complete PGM part:

```text
internal frame marker | part_len: u64 | header_crc32c: u32 | PGM part
```

The header checksum covers its first 12 bytes. The PGM part has its own
marker, bodies, catalog, and tail, so a frame can be validated before it is
accepted.

For a clean journal with `P` frames, `N` total part-catalog entries, and `B`
total part-body bytes:

```text
active_parts_bytes = B + 32*N + 68*P
```

The sealed byte count cannot be derived by subtracting frame overhead: seal
decodes and re-encodes the rows into fewer bodies and dictionary records. Its
exact size is the final `B + 32*N + 52` measured after compaction.

The default maximum accepted part is 64 MiB. The streaming recovery scanner
keeps one PGM part body of at most `max_part_len` plus a resynchronization
window in memory. The default resynchronization window is 1 MiB and is a
separate scanner argument, not a `JournalLimits` field.

Recovery classifies malformed regions instead of treating all damage alike:

| Classification | Meaning | `Journal::open` | Collector startup |
| --- | --- | --- | --- |
| `TornTail` | The final frame is incomplete, or a valid header declares a frame ending at EOF whose inner PGM fails validation. | Truncates to the last valid boundary. At most the final part is lost. | Seals valid preceding parts before connecting to PostgreSQL. |
| `Middle { resumed_at }` | A malformed region is followed by another valid frame. | Preserves the bytes and reports the damage. | Fails closed, leaving `active.parts` untouched for diagnosis. |
| `QuarantinedTail` | A malformed terminal region has no later valid frame. | Preserves the bytes and reports the damage. | Fails closed, leaving `active.parts` untouched for diagnosis. |

On collector startup, recovered parts containing timestamped data are sealed
immediately. Parts without any timestamped data are discarded by resetting the
journal without creating a PGM. A successful seal resets the whole journal,
after the destination and parent directory are synchronized. If seal fails,
the journal stays intact and collector startup fails. Non-torn damage is never
silently skipped or cleared.

## Where the bytes go

Most variation comes from Parquet bodies, not the 52 fixed container bytes.
Seal removes structural repetition without dropping admitted rows:

| Mechanism | Effect |
| --- | --- |
| Per-type coalescing | Many collection-window bodies become one body and one Parquet footer. |
| Dictionary normalization | Equal `str_id` records become one checked segment-wide record. |
| Canonical sorting | Output is deterministic and similar values remain adjacent. |
| PLAIN plus Zstandard-6 | Values avoid per-column Parquet dictionaries and use the fixed compression profile. |
| No statistics or page indexes | Metadata not used by PGM queries is absent. |
| Packed registry types | Physical integer widths and `StrId` columns follow the registered schema. |

Collection limits are separate from compaction. Blob truncation, top-N caps,
source intervals, and text budgets can omit data before it reaches the
journal; the PGM writer never presents that as a storage saving.

The maintained release qualification uses the production `Journal -> seal ->
PgmUnit` path. It requires 20 fresh-process seal samples, a spilling
fixed-fan-in fixture, exact equality after reopen, and the all-75-layout
oracle. It records actual PGM and OVF logical and allocated bytes separately,
plus wall time, CPU, peak RSS, spill bytes, process I/O, write amplification,
restart/query latency, and PGM/OVF read origins. Run and validate it through
the commands in
[`overview-parity-v1.md`](../../docs/qualification/overview-parity-v1.md).

The frozen
[physical-reduction study](../../docs/superpowers/specs/2026-07-26-pgm-size-reduction-research.md)
retains its corpus, checksums, limitations, and causal measurements. It is
research evidence, not a deployment-size promise.

## Parameters that affect file size

The format layout itself has no compression knobs. Operators control the
amount and grouping of collected data through `pg_kronika-collector`:

| Control | Default | What it changes |
| --- | ---: | --- |
| Per-source `*_INTERVAL_S` variables | 1-3600 s, depending on source | Raising an interval produces fewer snapshots, at the cost of time resolution. `KRONIKA_INTERVAL_S=5` is only the scheduler tick. |
| `KRONIKA_PG_MAX_TABLES`, `KRONIKA_PG_MAX_INDEXES`, `KRONIKA_PG_MAX_STATEMENTS`, `KRONIKA_PG_MAX_PLANS` | `500` | Lower values reduce high-cardinality rows and dictionary text but omit lower-ranked objects. |
| `KRONIKA_PG_MAX_PLAN_TEXT` | 32,768 bytes | Maximum stored text for one plan read. |
| `KRONIKA_PG_PLAN_TEXT_BUDGET` | 8 MiB | Total plan-text budget per read; zero disables plan text. |
| `KRONIKA_SEGMENT_MAX_BYTES` | 64 MiB | Seals after this many raw `active.parts` bytes; zero seals every window. It changes file granularity, not Parquet compression. |
| `KRONIKA_SEGMENT_MAX_AGE_S` | 900 s | Maximum age of an open segment. It mainly changes time span and file count. |
| `KRONIKA_JOURNAL_MAX_BYTES` | 1 GiB | Hard cap checked before every append. If an open segment exists, the collector seals it before retrying the candidate; one window larger than the cap is rejected. It is not a target PGM size. |

Changing source limits trades observability for disk use. Segment age and
rotation size change how many windows are compacted into each PGM; they do not
change the physical encoding profile. See the
[collector configuration](../../bins/pg_kronika-collector/README.md)
for every source interval and validation rule.

## Integrity and limits

CRC32C covers every section body, the catalog, and each journal-frame header. It
detects accidental corruption; it is not authentication, a signature, or
encryption.

`kronika-format` validates framing, catalog length and checksum, section
bounds, and section checksums for complete parts. Higher layers add policy:
the sealed reader requires the internal catalog contract value and caps
catalog, section, row, row-group, and decoded-work sizes. A section schema
change receives a distinct registry `type_id`.

Sources of truth:

- [`src/lib.rs`](src/lib.rs), [`src/catalog.rs`](src/catalog.rs), and
  [`src/parts.rs`](src/parts.rs) for the byte contract;
- [`src/dictionary.rs`](src/dictionary.rs) for dictionary invariants;
- [`kronika-registry`](../kronika-registry/README.md) for section schemas and
  Parquet limits;
- [`kronika-writer`](../kronika-writer/README.md) for append and seal behavior.
