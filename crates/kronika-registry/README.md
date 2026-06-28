# kronika-registry

[Русская версия](README.ru.md)

`kronika-registry` defines each section `type_id` and the codecs for section
bodies. It contains:

- the `type_id` scheme and `SectionClass`;
- the type contract: columns, column classes, sort key, collection semantics;
- the registry linter, which checks the contract invariants;
- the `Section` trait and `#[derive(Section)]`, which implements it from a
  struct (the type's contract and codec);
- shared codec code and `decode_any`, which selects the decoder by `type_id`.

## Type Ids

A `type_id` is `C_SSS_VVV` in decimal: class `C`, source `SSS` within the
class, schema variant `VVV` (starting at 001). `1_006_001` is class 1
(snapshot), source 006, variant 001; charts use the two-digit class 10, so
`10_001_001` is class 10, source 001, variant 001.

`TypeId` decomposes the digits (`class_digit`, `source`, `version`,
`section_class`). A `TypeId` is built only inside this crate, through
`#[derive(Section)]`. The validating constructor rejects an unknown class or a
zero source or version. Since the contract id is a `const`, an invalid
`#[section(id = ...)]` fails compilation. Every id in the registry is valid by
construction, and external crates cannot create one.

`SectionClass` is `Snapshot` (1), `Event` (2), `Dictionary` (3), `Chart` (10).

## Schema Exactness

A `type_id` fully and exactly characterizes the data it stores: the precise set
of columns and what they mean. The variant digits `VVV` are a *schema* version,
not a release number. When a source's schema differs across PostgreSQL major
versions — a view's columns change, or a view appears or moves, like the
checkpoint counters leaving `pg_stat_bgwriter` for `pg_stat_checkpointer` in
PG17 — each version gets a distinct `type_id`, incrementing `VVV`: `1_010_001`
for PG10, `1_010_002` for PG11–16, `1_010_003` for PG17.

A reader knows a section's full schema from its `type_id` alone, with no version
logic. Two consequences:

- A column never means "absent on this version." A version-specific column lives
  only in the `type_id`s whose version has it; a missing column is a missing
  `type_id`, not a `NULL`. `NULL` still encodes a genuinely runtime-absent value,
  such as an extension that is not installed — that is install config, not a
  version-shape difference.
- The collector maps the connected major version to the exact `type_id` and its
  exact codec. There is no in-type `if server_version` and no merged type whose
  columns are valid only on some versions.

The exact PostgreSQL and extension versions are recorded once per segment in
`instance_metadata`, never repeated as a column in every section.

## Type Contract

`TypeContract` is the registry record for one `type_id`:

- `columns` — `Column` entries in schema order: name, `ColumnType`,
  `ColumnClass`, and whether the column may be `NULL`;
- `sort_key` — column names, in order; every name must be a column;
- `semantics` — how the source is collected;
- `name`, `deprecated`.

`ColumnClass` (README of the format crate calls these the C/G/L/T classes):

- `Cumulative` — a monotonic counter; rates are deltas over time;
- `Gauge` — an instantaneous value;
- `Label` — identity or an attribute of the entity;
- `Timestamp` — `i64` unix microseconds.

`ColumnType` is the on-disk value type: the integer and float base types
(`I8`…`I64`, `U8`…`U64`, `F32`/`F64`), `Bool`, `Ts` (an `i64` timestamp), and
`StrId` (a `u64` reference into the segment string dictionary — the bytes live
there, not in the section). A struct field's Rust type sets the column type: a
`Ts` or `StrId` field maps to that type, and an `Option<T>` field is a nullable
column. `Semantics` is `SnapshotFull`, `ConditionalFull`, `EventStream`,
`Changed`, or `OnChange`.

## Registry Linter

`lint_registry` checks every known type and runs in CI through the crate's
tests. Per-id validity (class, source, version) is enforced when the `TypeId`
is constructed, so the linter covers only what construction cannot — rules that
span a contract or the whole registry:

- two contracts that share a `type_id` (ids are never reused);
- a sort-key name that is not a column;
- a `Changed` type with no `is_baseline` column;
- a `Timestamp`-class column that is not the required non-nullable `ts`.

## Snapshot Sections

A snapshot section body is a zstd-compressed Parquet file. `arrow_schema`
builds the Arrow schema from the contract columns, so codecs use the same
column order, Arrow types, and nullability as the registry.

`encode` sorts the rows by the contract's `sort_key` before writing, so adjacent
values in a column are alike and compress well. Rows that tie on the key keep an
unspecified order, and a decode returns the rows in this sorted order rather than
the order they were passed in.

The registry contract defines the schema, so the section does **not** embed a
second copy of it. The writer skips the `ARROW:schema` key-value blob that
arrow-rs writes by default and clears the Arrow-version string. That removes
a fixed ~1.1 KB per section (about a quarter of a single-row section) and
makes encoding deterministic. The file still contains the native Parquet
schema: physical types and column layout needed to read the column chunks.

Decode checks the file against the contract and returns a typed error on any
mismatch. If bytes stored under a known `type_id` do not decode, the writer
produced a bad section. The reader can skip it and report the diagnostic
instead of guessing.

## Derived Codecs

A type is a struct with `#[derive(Section)]` (`kronika-derive`). The derive
reads each field's Rust type (on-disk type and nullability), the
`#[column(class)]` attribute, and the `#[section(..)]` header. From that it
implements `Section`: the contract const and `encode`/`decode`.

Per-column encode/decode code is generated from the same struct that defines
the contract. Shared code in `encode_section` / `decode_section` handles the
Parquet framing, compression, and memory bounds. The derived code only supplies
one column builder/reader per field, so a new type goes through the same bounds
as existing types. Column types map to the narrowest Arrow type that fits
(`i32` maps to 32-bit, not 64) to keep sections small.

## Section Trait

Each generated codec is an `impl Section for T`. Generic code can use
`CONTRACT`, `encode`, `decode`, and `ts_range` without naming every registered
type. The shared roundtrip test and the writer's row buffers rely on that.

When reading a segment, only `type_id` is known. `decode_any(type_id, section)`
selects the contract through `registry()` and returns Arrow `RecordBatch`es plus
`DecodeStats`. Errors from this path keep the same `type_id`, so read metrics
can use one label for success and failure. Adding a section type means adding one
`registry()` entry. `T::decode` and `decode_any` both validate column set, order,
type, and nullability against the contract.

`decode` and `decode_any` take a `VerifiedSection`: owned `Bytes` whose CRC was
checked against the catalog. The bytes are not copied again; the Parquet reader
borrows from them. A reader can keep the segment in memory once (mmap or one
read) and pass section slices to decode. A source that reads sections one at a
time can use `decode_pooled`, which fills a `BytesPool` buffer and returns it to
the pool after the last `Bytes` reference is dropped. The pool covers only the
input buffer; decompressed pages and Arrow arrays are new allocations.
`BytesPool::stats()` reports reuse and drop counters. If
`dropped_oversize_total` grows, `buffer_limit` is below real section sizes and
the pool is no longer reusing those buffers.

The `Section` trait is public but closed to downstream impls. A private
supertrait is implemented only by this crate's derive, so `T: Section` means a
registry type. The derive macro is not exported.

## Example: family `1_006` (background writer / checkpointer)

Types are not documented one section per id in this README — the catalog of
types is the registry contracts in code. The `1_006` family is the worked
example of [Schema Exactness](#schema-exactness).

PostgreSQL 17 reorganized these views: it moved and renamed the checkpoint
counters into `pg_stat_checkpointer`, added the restartpoint counters for hot
standby, and moved `buffers_backend` / `buffers_backend_fsync` to `pg_stat_io`.
That is a different schema, so it is a different `type_id`:

- `1_006_001` (`Bgwriter`) — `pg_stat_bgwriter` on PG 15–16: the combined view,
  with `buffers_backend` / `buffers_backend_fsync` and one `stats_reset`.
- `1_006_002` (`BgwriterCheckpointer`) — `pg_stat_checkpointer` plus the slimmed
  `pg_stat_bgwriter` on PG 17+: `num_timed` / `num_requested`, the restartpoint
  counters, and a `stats_reset` for each view, with no `buffers_backend`.

Each struct has exactly its version's columns — no nullable "absent here" field
and no `if server_version` — and `encode` / `decode` convert a row slice to and
from the Parquet section body. The collector emits one or the other by major
version.

## Service Sections

Two types are mandatory in every segment that carries PostgreSQL or OS data, and
the reader interprets all other sections against them:

- `instance_metadata` (`1_021_001`) — the instance fingerprint: `pg_version_num`,
  `pg_system_identifier`, hostname, and the OS constants (`clock_ticks_per_sec`,
  `page_size_bytes`, `boot_id`, `btime`) that make OS sections self-describing.
- `reset_metadata` (`1_020_001` on PG 15, `1_020_002` on PG 16+) — the
  cross-cutting reset context: the postmaster start time and the per-view reset
  times (`pg_stat_database`, `pg_stat_wal`, `pg_stat_archiver`, and `pg_stat_io`
  on PG 16+). It carries reset timestamps only; extension versions live in
  `instance_metadata` and GUCs live in the settings family, so neither is smeared
  across stats types. `pg_stat_io` arrived in PG16, so its reset is a schema
  difference — a second `type_id`, not a nullable column.

A statistics reset is per view: `pg_stat_reset_shared('bgwriter')` resets only
that view. It can happen in the middle of a segment, so a view that exposes its
own `stats_reset` carries it per row in its own section (`stats_reset` in
`1_006_001`, the bgwriter and checkpointer resets in `1_006_002`). Then the diff
code can see the reset between two samples; `reset_metadata`, one row per
segment, would miss it.

Version differences in a view's schema are different `type_id`s, not nullable
columns (see [Schema Exactness](#schema-exactness)): `buffers_backend` is not a
PG17 `NULL`, it is simply absent from `1_006_002`. The exact source and extension
versions are recorded once per segment in `instance_metadata`, never repeated as
a column in every type.

## Memory Bounds

A snapshot section holds at most `MAX_SECTION_ROWS` rows. `encode` rejects a
larger slice before it builds any column, and rejects a finished body above
`MAX_SECTION_BYTES` — the same byte cap `decode` enforces — so a writer cannot
emit a section the reader would refuse. `decode` checks the section before
materializing rows: byte length must fit `MAX_SECTION_BYTES`, row groups must fit
`MAX_ROW_GROUPS`, and both metadata and decoded row counts must fit
`MAX_SECTION_ROWS`. `decode_pooled` checks the catalog's claimed length against
the byte cap before it reads, so a corrupt length cannot allocate first.

Encode peak memory is the caller's row slice plus one Parquet row group.
`decode` takes owned `Bytes` and does not copy the section again; the Parquet
reader borrows from those bytes. `decode_section` streams, one read batch at a
time, so its added peak is one batch; `decode_batches` (and `decode_any`)
returns the whole section as Arrow batches, so its added peak is the section's
decoded rows. Both are bounded at `MAX_SECTION_ROWS` by the same limits, far
above what the current regularly sampled single-row sources produce in one
segment; they limit writer bugs and malformed sections, not normal data.

One risk remains in Parquet decoding itself: a page can request memory according
to its own header. The decode entry points therefore take `VerifiedSection`, not
raw bytes. `VerifiedSection::verify(bytes, expected_crc, crc32c)` checks the
section CRC before Parquet sees the bytes. This catches media corruption; a
segment deliberately rebuilt with matching CRCs is outside the format's
protection model.

## Tests

- `type_id` decomposition and validation;
- the registry linter, with one test per finding;
- the `1_006` and `1_020` family codecs: each variant's exact contract shape
  (no version-`NULL` columns), exact value roundtrip, extension `NULL` remaining
  distinct from zero, Parquet file framing, and the row, byte, and row-group
  limits. The roundtrip fixture compares decoded values rather than raw Parquet
  bytes because Arrow metadata and encoding choices can change between dependency
  versions;
- the generic path: every registered type round-trips an empty section through
  `decode_any` with no per-type code; `decode_any` reports decode stats and
  rejects an unregistered `type_id`.

## Future Work

`#[derive(Section)]` can also emit a roundtrip fixture per type once several
types exist. More types are added with the next sources. Bloom filters and
row-group tuning come with the first high-cardinality types that need them.
