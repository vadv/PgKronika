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
$KRONIKA_OUT_DIR/active.parts
        |
        | seal()
        v
$KRONIKA_OUT_DIR/YYYY/MM/DD/N.pgm
        |
        v
kronika-store / kronika-reader
        |
        v
pg_kronika-web
```

The journal is always `$KRONIKA_OUT_DIR/active.parts`. Finished PGM files use
the strict UTC calendar tree:

```text
$KRONIKA_OUT_DIR/
|-- active.parts
`-- YYYY/
    `-- MM/
        `-- DD/
            `-- N.pgm
```

`N` is the decimal [`SegmentId`](../kronika-layout/src/time.rs): Unix
microseconds of the first collection window successfully appended to the
segment. `YYYY/MM/DD` is the UTC day derived from that id.

There are no separate files for string values. While a segment is open,
dictionary bodies live in the PGM parts inside `active.parts`. On completion,
the writer copies those bodies unchanged into the finished `.pgm`.

The format crate owns the `PGM1` and `PGMP` byte layouts, the end catalog,
CRC32C checksums, `StrId`, and the bounded dictionary model. It deliberately
does not own:

- PostgreSQL or Linux semantics and section schemas: those belong to
  [`kronika-registry`](../kronika-registry/README.md);
- Parquet encoding, buffering, journal I/O, and sealing: those belong to
  [`kronika-writer`](../kronika-writer/README.md);
- collection intervals, rotation, and source limits: those belong to
  [`pg_kronika-collector`](../../bins/pg_kronika-collector/README.md);
- data-directory paths, strict discovery, and ownership:
  [`kronika-layout`](../kronika-layout/);
- typed queries, HTTP, retention, encryption, or remote storage.

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

### Windows, parts, sections, and bodies

The format description uses four different levels:

- a **collection window** is one non-empty collector cycle;
- a **PGM part** is a self-contained record of one window, with its own `PGM1`,
  bodies, catalog, and tail index;
- a **section** is one catalog entry: `type_id`, offset, length, row count, and
  CRC32C;
- a **section body** is the byte range addressed by a catalog entry. The
  current writer places one self-contained Parquet file in that range.

While the segment is open, every part is wrapped in its own journal frame:

```text
active.parts
|
+-- PGMP frame #1
|   `-- PGM part for window #1
|       |-- "PGM1"
|       |-- data section bodies
|       |-- dict.strings / dict.blobs bodies
|       |-- part catalog
|       `-- tail index
|
`-- PGMP frame #2
    `-- PGM part for window #2
        |-- "PGM1"
        |-- data section bodies
        |-- dict.strings / dict.blobs bodies
        |-- part catalog
        `-- tail index
```

Within each part, the current writer places non-empty data bodies in ascending
`type_id` order, followed by `dict.strings` and then `dict.blobs`. An absent
dictionary section occupies no space. This is current writer order, not a fixed
format offset; readers locate every body through the catalog.

Completing the segment removes the individual part framing:

```text
active.parts                         YYYY/MM/DD/N.pgm

PGMP [part for window #1]             "PGM1"
PGMP [part for window #2] -- seal() -> bodies from window #1, including its dictionaries
...                                  bodies from window #2, including its dictionaries
                                     ...
                                     one shared end catalog
                                     one tail index
```

The finished PGM has no explicit window-boundary markers. A body's originating
window is not recorded; it can only be inferred from order and contents. The
catalog describes sections, not window numbers.

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
| 16 | `source_id` | `u64` | Source identifier; zero means unspecified. |
| 24 | `entry_count` | `u32` | Number of 32-byte entries before this block. |
| 28 | `format_version` | `u32` | Container layout version; current writers store `1`. |
| 32 | `crc32c` | `u32` | CRC32C of entries and metadata with this field zeroed. |
| 36 | `reserved` | `u32` | Reserved; current writers store zero. |

The low-level `Catalog` and `PartMeta` APIs historically document `source_id`
as the `StrId` of `{cluster_id}/{pg_system_identifier}`. The current collector
instead copies an arbitrary `KRONIKA_SOURCE_ID` into the field and does not
guarantee a matching `dict.strings` record. Readers of current collector output
must therefore treat `source_id` as an opaque identifier, not a resolvable
string reference.

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

### Example: a snapshot and two dictionaries in one PGM

The following simplified PGM for PostgreSQL 14-18 contains one
`pg_stat_activity` body, one dictionary body for short string values, and one
dictionary body for large values. Let their sizes be `S`, `T`, and `L`, with
`B = S + T + L`.

```text
offset

0       +----------------------------------------------------------+
        | "PGM1"                                                   | 4 bytes
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
        | catalog_len=136 | "PGM1"                                 | 8 bytes
148+B   +----------------------------------------------------------+
```

For three bodies, `catalog_len = 3 * 32 + 40 = 136`, so the complete file is
`B + 148` bytes. If the window has no large values, the `dict.blobs` body and
its catalog entry are absent. Every non-empty window can add several bodies, so
the same `type_id` commonly appears more than once in the finished catalog.

In current PgKronika files, a data or dictionary body is normally a
self-contained Parquet file, so it has its own `PAR1 ... PAR1` framing. The PGM
container treats those bytes as opaque; it does not parse column metadata. The
Parquet rows shown above are decoded contents, not literal plain text in the
file: internal encoding and Zstd mean that text bytes need not occur verbatim
in the PGM.

The repository also contains a byte-exact
[`minimal.pgm`](tests/fixtures/minimal.pgm) fixture. It is 88 bytes:
`PGM1`, one four-byte body `01 02 03 04`, one catalog entry, metadata, and the
tail. [`tests/fixture.rs`](tests/fixture.rs) records every offset and verifies
that the encoder reproduces the fixture byte for byte.

## What section bodies contain today

`kronika-format` permits opaque bodies, but the current PgKronika writer uses
self-contained Parquet for both snapshot and dictionary sections:

- Zstd level 3 compresses column pages inside each Parquet body;
- snapshot rows are sorted by the registry contract's canonical key, while
  dictionary rows are sorted by `str_id`;
- one body is limited to 65,536 rows and 8 MiB;
- the decoder accepts no more than 16 row groups;
- the redundant Arrow schema metadata is omitted and `created_by` is cleared
  to an empty string.

Omitting the embedded Arrow schema removes a duplicate logical schema from
every body; the exact saving depends on the section contract. The native
Parquet schema remains because the decoder needs the physical column layout.

There is no whole-file Zstd pass. Each Parquet body's header and footer, the PGM
catalog, and the PGM/PGMP framing are outside a shared Zstd stream. A reader can
locate and verify one body without decompressing unrelated sections.

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
$KRONIKA_OUT_DIR/YYYY/MM/DD/N.pgm  (one file)

[ "PGM1" ]
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

The bytes belong to the `bytes` column chunk of the `dict.strings` body.
Parquet may place the value in a dictionary or data page inside that chunk;
Zstd-3 compresses the page. The `pg_stat_activity` body keeps only `H`. The PGM
catalog stores the offset and length of the entire dictionary body, not of one
value; finer offsets belong to Parquet metadata. Resolving `H` therefore
requires decoding the dictionary body.

The "after decoding" blocks show logical values, not on-disk bytes. The exact
cost of the two references need not be 16 bytes, and encoding and compression
mean that `postgres` need not occur as a plain byte sequence in the file.

Reading follows the end catalog:

```text
(1) tail index -> end catalog

(2) catalog -> pg_stat_activity body
            -> decode Parquet
            -> datname=H

(3) catalog + H -> every dict.strings and dict.blobs body
                -> decode record with str_id=H
                -> b"postgres"
                -> datname="postgres"
```

PGM has no global `StrId -> offset` index. A full dictionary read visits every
`dict.strings` and `dict.blobs` body and builds an in-memory map. The targeted
overview path also reads dictionary bodies but retains only ids requested in
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

### Where deduplication stops

One `SegmentDicts` instance detects hash collisions and keeps one copy of equal
bytes. The current collector, however, creates a new interner for every
collection cycle. Deduplication therefore applies within one window, not
across the finished PGM.

A full dictionary read combines repeated records by `str_id`: the first
`dict.strings` record remains, every later `dict.blobs` record unconditionally
replaces the current value, and later `dict.strings` records are ignored. This
path does not compare duplicate bytes. The targeted overview resolver compares
`bytes`, `full_len`, and `truncated` for selected duplicates and rejects
contradictions. It requires `full_sha256` to be present for a truncated blob but
does not compare the digest. Both paths combine records only in reader memory;
they do not remove physical copies from the PGM.

## `active.parts` and crash recovery

`active.parts` is the root-level durable journal of an unfinished segment.
Journal version 1 starts with a checksummed 36-byte header:

```text
"PGKJNL1\0" | version: u32 | state: u8 | id_present: u8 | reserved: u16
             | segment_id: i64 | body_len: u64 | header_crc32c: u32
```

The header records an empty state or the exact `SegmentId` of the active
segment and the number of frame bytes that follow. Every frame then has a
16-byte header followed by one complete PGM part:

```text
"PGMP" | part_len: u64 | header_crc32c: u32 | PGM part
```

The journal-header checksum covers its first 32 bytes; each frame-header
checksum covers its first 12 bytes. The PGM part has its own `PGM1`, bodies,
catalog, and tail, so a frame can be validated before it is accepted. A
canonical empty journal is the 36-byte empty header, never a zero-length file.

For a clean active journal with `P` frames, `N` total section entries, and `B`
total body bytes:

```text
active_parts_bytes = 36 + B + 32*N + 68*P
sealed_pgm_bytes   = B + 32*N + 52
completion_saves   = 68*P - 16
```

This saving is only framing. With 70 windows it is 4,744 bytes; the Parquet
bodies and all `N` catalog entries still remain.

Version 1 has three absolute admission limits: one PGM part is at most 64 MiB,
one journal contains at most 1,000,000 frames, and the physical journal file is
at most 1 GiB including the temporary reset marker. Runtime configuration may
only lower these limits. The production streaming scanner keeps one bounded
PGM part body plus the returned frame references in memory. It stops at the
first damaged frame and does not search damaged bytes for another frame magic.
The resynchronizing in-memory scanner is a diagnostic API, not a recovery
policy.

`Journal::open` is fail-closed for version 1. It validates the complete header,
the recorded body length, and every frame. A zero-length or headerless file,
another version, a torn header, a length mismatch, or any torn or damaged
frame returns an error and leaves the file unchanged. The low-level scanner's
damage classifications are diagnostic data, not a repair policy for this
journal. Version 1 is the first and only supported journal format. PgKronika
has not had a public release, and there is no alternate journal format or
migration path.

On collector startup, a valid active journal is completed under its stored
`SegmentId` and published at `YYYY/MM/DD/N.pgm`. Only successful publication
allows reset. Reset first persists a commit marker. It then writes
`JournalHeader::EMPTY` and calls `sync_data` while the marker and frame body
remain, truncates the file to 36 bytes, and calls `sync_data` again. If the
process exits after the marker is durable, the next `Journal::open` validates
it and completes the reset.

The reset marker is 32 bytes and records the previous journal length,
`SegmentId`, and header checksum. The configured journal cap is a literal
physical-file limit: the writer reserves those 32 bytes before every append,
including the first frame.

## Where the bytes go

For the current implementation, most size variation comes from section
bodies, not the 52-byte container constant or 32-byte catalog entries.

### What already reduces size without losing data

| Mechanism | What is removed or compressed | Scope |
| --- | --- | --- |
| `StrId` and dictionaries | Repeated short text becomes a value in a `u64` column; the bytes are written once. | One copy per window, not per finished PGM; Parquet determines the exact column size. |
| Parquet and Zstd-3 | Pages of each column are encoded and compressed independently of other bodies. | Repetition across Parquet bodies is not used. |
| Canonical sorting | Values with nearby keys become adjacent and output is deterministic. | Compression gain depends on the data and is not guaranteed. |
| Narrow column types | For example, `pid` is stored as `i32`, not `i64`, and text labels move to dictionaries. | Each section contract fixes its column types. |
| Omitted Arrow metadata | Each body omits a second logical Arrow schema and leaves `created_by` empty. | The physical schema and Parquet footer remain. |

### What reduces size by discarding data

These are admission limits, not compression:

| Mechanism | What is discarded | Cost |
| --- | --- | --- |
| `dict.blobs` truncation | The suffix after 64 KiB. | Full text cannot be recovered; its length and SHA-256 remain. |
| `*_MAX_TABLES`, `*_MAX_INDEXES`, `*_MAX_STATEMENTS`, `*_MAX_PLANS` | Objects below the top-N cutoff. | Their observations do not enter the PGM. |
| Source intervals | Snapshots between polling times. | Time resolution decreases. |
| Plan-text budgets | Text beyond one read's limit. | Numeric plan statistics may remain without plan text. |

Segment age and size limits only control file grouping. They change PGM count
and a small amount of framing overhead, but do not merge Parquet bodies or
remove repeated dictionary records.

### Why logical data repeats across collection windows

Each collection window writes a separate Parquet body for every non-empty
type. A one-row snapshot still pays for a Parquet schema, column metadata, and
footer. Over a 15-minute segment, the same type can therefore have dozens of
small bodies and dozens of footers.

Dictionary sections are also emitted per window. For `postgres`, the logical
body contents after decoding look like:

```text
before completing the segment:

active.parts
|-- PGMP [part #1:
|          pg_stat_activity body #1: pid=101, H; pid=102, H
|          dict.strings #1: H -> b"postgres"]
`-- PGMP [part #2:
           pg_stat_activity body #2: pid=103, H
           dict.strings #2: H -> b"postgres"]

after completing the segment (`seal`):

"PGM1"
|-- activity body #1
|-- dict.strings body #1: H -> b"postgres"
|-- activity body #2
|-- dict.strings body #2: H -> b"postgres"
|-- shared catalog
`-- tail index

3 StrId references | 1 unique H | 2 dictionary records on disk
```

The current segment-completion operation (`seal`) keeps both dictionary
records and both self-contained `pg_stat_activity` bodies. The researched
in-place implementation instead looks like:

```text
PGM header (single current internal identity)
|-- one pg_stat_activity body
|-- one dict.strings body: H -> b"postgres"
|-- shared catalog
`-- tail index

3 StrId references | 1 unique H | 1 physical dictionary record
```

This cannot be implemented by concatenating bytes. It must decode Parquet,
merge bodies with the same `type_id`, verify equal dictionary records, sort
records by the canonical key, seal before a merged body would exceed a
production limit, and encode again. Two bodies with the same `type_id` are not
allowed in the finished PGM. This behavior is not part of the current `seal`.

### Additional approaches that are not implemented

| Approach | Source of the saving | Constraint |
| --- | --- | --- |
| Replace the current seal internals in place | Bodies with the same `type_id` merge and dictionary records deduplicate across windows. | Requires decode and re-encode, collision checks, canonical sorting, early seal before a limit, and a complete write/read verification cycle. |
| One interner for the open segment | A repeated value is not emitted into the next window's dictionary. | Current PGM parts are self-contained. If a later window refers to an earlier dictionary, isolated frame reads and crash recovery need a new contract. |
| Higher Parquet Zstd level | Pages inside one body may become smaller. | Costs more CPU and still cannot use repetition across bodies. Level 3 is currently fixed in code. |
| Outer compression for the whole PGM | One stream can see repeated dictionaries, footers, and similar windows. | Direct body access by `offset` is lost; this would replace PGM access semantics and is outside this research. |

The first two approaches remove structural repetition. Raising the Zstd level
only changes local compression and is not a substitute for section coalescing
and dictionary deduplication.

### Validated physical-reduction research

The preliminary estimator is superseded. The current
[physical PGM reduction research](../../docs/superpowers/specs/2026-07-26-pgm-size-reduction-research.md)
writes complete reader-valid candidates, verifies exact canonical Arrow and
dictionary equality, covers every registered contract, and records fault,
resource, I/O, and separate PGM-plus-OVF evidence.

Three natural full 15-minute files produced candidates of 549,761, 524,989,
and 522,016 bytes, for reductions of 35.343x, 37.091x, and 37.029x.
Candidate size has an empirical nearest-rank p50 of 524,989 bytes and
p95/worst of 549,761 bytes. A separate 62.52-second tail reduced 6.016x and is
not part of that distribution. The full-segment sample contains only three
files; it does not support an hourly retention projection.

The recommendation replaces the existing PGM internals in place while keeping
the canonical `YYYY/MM/DD/N.pgm` address. Because PgKronika has not shipped a
release, the proposed format would become the sole writer and reader contract.
Production behavior is unchanged by this research documentation.

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
| `KRONIKA_JOURNAL_MAX_BYTES` | 1 GiB | Hard physical `active.parts` limit, including the reserved 32-byte reset marker. Every frame must fit; exhaustion causes an early seal. It is not a target PGM size. |

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
- [`kronika-writer`](../kronika-writer/README.md) for append and seal behavior.
