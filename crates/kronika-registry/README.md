# kronika-registry

[Русская версия](README.ru.md)

`kronika-registry` defines what each section `type_id` means and provides the
codecs for section bodies. It contains:

- the `type_id` scheme and `SectionClass`;
- the type contract: columns, column classes, sort key, collection semantics;
- the registry linter, which checks the contract invariants;
- the `Section` trait and `#[derive(Section)]`, which implements it from a
  struct (the type's contract and codec);
- the shared codec runtime, plus `decode_any` for dispatch by `type_id`.

## Type Ids

A `type_id` is `C_SSS_VVV` in decimal: class `C`, source `SSS` within the
class, layout version `VVV` (starting at 001). `1_006_001` is class 1
(snapshot), source 006, version 001; charts use the two-digit class 10, so
`10_001_001` is class 10, source 001, version 001.

`TypeId` decomposes the digits (`class_digit`, `source`, `version`,
`section_class`). A `TypeId` is built only inside this crate, by
`#[derive(Section)]`: the validating constructor rejects an unknown class or a
zero source or version, and because a contract's id lives in a `const`, an
invalid `#[section(id = ...)]` is a compile error. Every id in the registry is
valid by construction, and no external crate can mint one.

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

`ColumnType` is the on-disk value type (`I64`, `F64`, `U64`, `Bool`, `Ts`).
`Semantics` is `SnapshotFull`, `ConditionalFull`, `EventStream`, `Changed`,
or `OnChange`.

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

The registry is the schema of truth, so the section does **not** embed a
second copy of it: the writer skips the `ARROW:schema` key-value blob that
arrow-rs writes by default and clears the Arrow-version string. That removes
a fixed ~1.1 KB per section (about a quarter of a single-row section) and
makes encoding deterministic. Only the native Parquet schema — the physical
types and column layout the decoder needs to read the column chunks — stays
in the file.

Decode treats the contract as authoritative: it imposes the contract's
columns and types and returns a typed error on any mismatch. A `type_id`
whose bytes do not decode means the writer violated the contract, so the
reader skips that section and raises a diagnostic rather than guessing.

## Generated Codecs

A type is a struct with `#[derive(Section)]` (`kronika-derive`) — the struct
is the only per-type code. The derive reads each field's Rust type (the
on-disk type and nullability) and a `#[column(class)]`, plus a
`#[section(..)]` header, and implements the `Section` trait: the contract const
and `encode`/`decode`. There is no hand-written per-column code to drift
from the contract.

The framing, compression, and every memory bound live once in the shared
codec runtime (`encode_section` / `decode_section`); the generated code only
supplies one column builder/reader per field, so a new type cannot forget a
bound. Column types map to the narrowest Arrow type that fits (`i32` → 32-bit,
not 64) to keep sections small. `kronika-derive` is exactly the crate
`architecture.md` §7 plans: struct → schema, encoder, decoder.

## Section Trait

Each generated codec is an `impl Section for T`, so generic code works over
`T: Section` (`CONTRACT`, `encode`, `decode`) instead of naming each type — a
shared roundtrip test is written once, not per type.

Reading a segment, the `type_id` is a runtime `u32`, so the reader cannot name
`T`. `decode_any(type_id, bytes)` dispatches through `registry()` and returns
the section's Arrow `RecordBatch`es, validated against the contract. It needs
no concrete type and no per-type `match`, so a new section type costs one
`registry()` entry and is decodable immediately — the property that lets the
registry grow to hundreds of types without per-type wiring.

The `Section` trait is public; the `#[derive(Section)]` macro is not. Every
section type lives in this crate (the derive routes ids through the
crate-private `TypeId` constructor), so the macro is a registry-internal tool,
not a public extension point.

## Example: type `1_006_001`

Types are not documented one section per id in this README — the catalog of
types is the registry contracts in code. `1_006_001` is kept here as a single
worked example.

`pg_stat_bgwriter` + `pg_stat_checkpointer`, a single-row snapshot
(`SnapshotFull`, sort key `(ts)`). PostgreSQL 17 moved some
`pg_stat_bgwriter` counters to `pg_stat_checkpointer`; the collector reads
both views and writes one row. Columns removed from PostgreSQL 17
(`buffers_backend`, `buffers_backend_fsync`) are written as `NULL`, not `0`.

The whole `1_006_001` codec is its annotated struct plus `#[derive(Section)]`;
`BgwriterCheckpointer::encode` / `decode` convert a row slice to and from the
Parquet section body.

## Memory Bounds

A snapshot section holds at most `MAX_SECTION_ROWS` rows. `encode` rejects a
larger input slice. `decode` checks the section before materializing rows:
byte length must fit `MAX_SECTION_BYTES`, row groups must fit `MAX_ROW_GROUPS`,
and both metadata and decoded row counts must fit `MAX_SECTION_ROWS`.

Encode peak memory is the caller's row slice plus one Parquet row group.
`decode_section` streams — one read batch at a time — so its peak is one bounded
copy of the section bytes plus one batch. `decode_batches` (and `decode_any`)
returns the whole section as Arrow batches instead, so its peak is the section
itself, still bounded at `MAX_SECTION_ROWS` by the same caps. The caps are far
above what the current regularly sampled single-row sources produce in one
segment; they limit writer bugs and malformed sections, not normal data.

One risk remains in Parquet decoding itself: a valid-size page can request a
large buffer from its page header. Callers should pass bytes to `decode` only
after verifying the section CRC against the catalog. That catches media
corruption before this point; fully forged segments are outside the protection
model of the format.

## Tests

- `type_id` decomposition and validation;
- the registry linter, with one test per finding;
- the `1_006_001` codec: exact value roundtrip, PG16 and PG17 rows in one
  section, `NULL` remaining distinct from zero, Parquet file framing, and the
  row, byte, and row-group caps. The golden test compares decoded values
  rather than raw Parquet bytes because Arrow metadata and encoding choices can
  change between dependency versions;
- the generic path: every registered type round-trips an empty section through
  `decode_any` with no per-type code, and `decode_any` rejects an unregistered
  `type_id`.

## Future Work

`#[derive(Section)]` can also emit a roundtrip golden test per type once
several types exist. More types are added with the next sources. Bloom filters
and row-group tuning come with the first high-cardinality types that need them.
