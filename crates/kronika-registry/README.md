# kronika-registry

[Русская версия](README.ru.md)

`kronika-registry` defines what each section `type_id` means and provides the
codecs for section bodies. It contains:

- the `type_id` scheme and `SectionClass`;
- the type contract: columns, column classes, sort key, collection semantics;
- the registry linter, which checks the contract invariants;
- the `Section` trait and `#[derive(Section)]`, which implements it from a
  struct (the type's contract and codec);
- shared codec code and `decode_any`, which selects the decoder by `type_id`.

## Type Ids

A `type_id` is `C_SSS_VVV` in decimal: class `C`, source `SSS` within the
class, layout version `VVV` (starting at 001). `1_006_001` is class 1
(snapshot), source 006, version 001; charts use the two-digit class 10, so
`10_001_001` is class 10, source 001, version 001.

`TypeId` decomposes the digits (`class_digit`, `source`, `version`,
`section_class`). A `TypeId` is built only inside this crate, through
`#[derive(Section)]`. The validating constructor rejects an unknown class or a
zero source or version. Since the contract id is a `const`, an invalid
`#[section(id = ...)]` fails compilation. Every id in the registry is valid by
construction, and external crates cannot create one.

`SectionClass` is `Snapshot` (1), `Event` (2), `Dictionary` (3), `Chart` (10).

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

Each generated codec is an `impl Section for T`, so generic code works over
`T: Section` (`CONTRACT`, `encode`, `decode`, and `ts_range` for the catalog
time range) instead of naming each type — a shared roundtrip test is written
once, not per type, and the writer buffers any type without a per-type case.

When reading a segment, the `type_id` is known only at read time, so the reader
cannot name `T`. `decode_any(type_id, section)` selects the contract through
`registry()` and returns a `DecodedSection`: Arrow `RecordBatch`es plus
`DecodeStats` (`type_id`, input bytes, row groups, batches, rows), which the
reader can export as RED metrics. On failure, `CodecError::Section` keeps the
same `type_id` for a failed decode and `CodecError::UnknownType` keeps it for an
id absent from the registry. `CodecError::section_type_id()` returns that label
without a separate `match` over variants, so every read outcome can be counted
under `{type_id}`. No concrete type and no per-type `match` are needed; adding a
section type means adding one `registry()` entry. Both `T::decode` and
`decode_any` validate the file schema against the contract — column set, order,
type, and nullability — so the typed and registry paths accept and reject the
same bytes.

`decode` and `decode_any` take a `VerifiedSection`: owned `Bytes` whose CRC was
checked against the catalog. The bytes are not copied again; the Parquet reader
borrows from them. A reader can keep the segment in memory once (mmap or one
read) and pass section slices to decode. A source that reads sections one at a
time can use `decode_pooled`, which fills a `BytesPool` buffer and returns it to
the pool after the last `Bytes` reference is dropped. The pool covers only the
input buffer; decompressed pages and Arrow arrays are new allocations.
`BytesPool::stats()` reports the reuse and drop counters: a climbing
`dropped_oversize_total` is the signal that `buffer_limit` sits below the section
size, so the pool has silently degraded to a per-section allocator.

The `Section` trait is public but sealed: a crate-private supertrait that only
the in-crate `#[derive(Section)]` implements, so `T: Section` always means a
registry type and no downstream crate can implement it to forge a codec for an
existing contract. The macro is not public either; the derive routes ids through
the crate-private `TypeId` constructor, an internal registry tool, not a public
extension point.

## Example: type `1_006_001`

Types are not documented one section per id in this README — the catalog of
types is the registry contracts in code. `1_006_001` is kept here as a single
worked example.

`pg_stat_bgwriter` + `pg_stat_checkpointer`, a single-row snapshot
(`SnapshotFull`, sort key `(ts)`). PostgreSQL 17 reorganized these views: it
moved and renamed the checkpoint counters into `pg_stat_checkpointer`, added the
restartpoint counters for hot standby, and removed
`buffers_backend` / `buffers_backend_fsync`. The collector reads both views and
writes one row; each field keeps a stable name and documents its
per-version source, and a counter the running server lacks is written `NULL`,
not `0` — in either direction.

The whole `1_006_001` codec is its annotated struct plus `#[derive(Section)]`;
`BgwriterCheckpointer::encode` / `decode` convert a row slice to and from the
Parquet section body.

## Service Sections

Two types are mandatory in every segment that carries PostgreSQL or OS data, and
the reader interprets all other sections against them:

- `instance_metadata` (`1_021_001`) — the instance fingerprint: `pg_version_num`,
  `pg_system_identifier`, hostname, and the OS constants (`clock_ticks_per_sec`,
  `page_size_bytes`, `boot_id`, `btime`) that make OS sections self-describing.
- `reset_metadata` (`1_020_001`) — the cross-cutting reset context: the global
  postmaster start time, reset times for views that do not yet have their own
  section, the extension versions, and the GUCs (`track_io_timing`,
  `track_wal_io_timing`, `compute_query_id`) that decide whether a column is
  present or meaningful.

A statistics reset is per view: `pg_stat_reset_shared('bgwriter')` resets only
that view. It can happen in the middle of a segment, so a view that exposes its
own `stats_reset` carries it per row in its own section
(`bgwriter_stats_reset` in `1_006_001`). Then the diff code can see the reset
between two samples; `reset_metadata`, one row per segment, would miss it.

This is why one codec spans every PostgreSQL version. Version differences are
nullable columns — `buffers_backend` is `None` on PG17+ — and their meaning
comes from these sections, not from a new `type_id` per release: `pg_version_num`
says the column was removed in PG17, and the row's `checkpointer_stats_reset`
(null before PG17) confirms it. The source version is recorded once per
segment, not repeated as a column in every type.

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

One risk remains in Parquet decoding itself: a valid-size page can request a
large buffer from its page header, which the byte limit does not bound. The
decode entry points therefore take a `VerifiedSection`, not raw bytes, and its only
constructor — `VerifiedSection::verify(bytes, expected_crc, crc32c)` — runs the
check, so unverified bytes cannot reach the parser by accident. The crc function
is passed in by the caller because the catalog checksum code is in
`kronika-format`. This catches media corruption; a segment deliberately rebuilt
with a matching CRC is outside the protection model of the format.

## Tests

- `type_id` decomposition and validation;
- the registry linter, with one test per finding;
- the `1_006_001` codec: exact value roundtrip, PG16 and PG17 rows in one
  section, `NULL` remaining distinct from zero, Parquet file framing, and the
  row, byte, and row-group limits. The roundtrip fixture compares decoded values
  rather than raw Parquet bytes because Arrow metadata and encoding choices can
  change between dependency versions;
- the generic path: every registered type round-trips an empty section through
  `decode_any` with no per-type code; `decode_any` reports decode stats and
  rejects an unregistered `type_id`.

## Future Work

`#[derive(Section)]` can also emit a roundtrip fixture per type once several
types exist. More types are added with the next sources. Bloom filters and
row-group tuning come with the first high-cardinality types that need them.
