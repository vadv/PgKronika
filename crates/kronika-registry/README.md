# kronika-registry

[Русская версия](README.ru.md)

`kronika-registry` defines what each section `type_id` means and provides the
codecs for section bodies. It contains:

- the `type_id` scheme and `SectionClass`;
- the type contract: columns, column classes, sort key, collection semantics;
- the registry linter, which checks the contract invariants;
- the manual codecs that encode and decode section bodies.

## Type Ids

A `type_id` is `C_SSS_VVV` in decimal: class `C`, source `SSS` within the
class, layout version `VVV` (starting at 001). `1_006_001` is class 1
(snapshot), source 006, version 001; charts use the two-digit class 10, so
`10_001_001` is class 10, source 001, version 001.

`TypeId` decomposes the digits (`class_digit`, `source`, `version`,
`section_class`). `TypeId::new` validates a raw id at runtime. Registry
contracts are `const`, so they declare their id with `TypeId::declared`; the
linter checks those declarations.

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
tests. It reports:

- a `type_id` with an unknown class, a source outside the class range, or a
  zero version;
- two contracts that share a `type_id` (ids are never reused);
- a sort-key name that is not a column;
- a `Changed` type with no `is_baseline` column;
- a `Timestamp`-class column that is not the required non-nullable `ts`.

## Snapshot Sections

A snapshot section body is a self-contained zstd-compressed Parquet file, as
required by the container format. `arrow_schema` builds the Arrow schema from
the contract columns, so codecs use the same column order, Arrow types, and
nullability as the registry.

Codecs are manual for now, one module per type. `kronika-derive` will
generate them later; until then this crate keeps both the decoder and the
temporary encoder.

## Type `1_006_001`

`pg_stat_bgwriter` + `pg_stat_checkpointer`, a single-row snapshot
(`SnapshotFull`, sort key `(ts)`). PostgreSQL 17 moved some
`pg_stat_bgwriter` counters to `pg_stat_checkpointer`; the collector reads
both views and writes one row. Columns removed from PostgreSQL 17
(`buffers_backend`, `buffers_backend_fsync`) are written as `NULL`, not `0`.

`bgwriter_checkpointer::encode` / `decode` convert a row slice to and from
the Parquet section body.

## Memory Bounds

A snapshot section holds at most `MAX_SECTION_ROWS` rows. `encode` rejects a
larger input slice. `decode` checks the section before materializing rows:
byte length must fit `MAX_SECTION_BYTES`, row groups must fit `MAX_ROW_GROUPS`,
and both metadata and decoded row counts must fit `MAX_SECTION_ROWS`.

Encode peak memory is the caller's row slice plus one Parquet row group.
Decode peak memory is one bounded copy of the section bytes plus one Arrow
read batch. The caps are far above what the current regularly sampled
single-row sources produce in one segment; they limit writer bugs and
malformed sections, not normal data.

One residual stays inside Parquet: while decoding a valid-size page it can
reserve a buffer from the page header. So decode assumes the section CRC has
already been verified against the catalog, which catches bit rot before this
point; a fully forged segment is outside what the format protects.

## Tests

- `type_id` decomposition and validation;
- the registry linter, with one test per finding;
- the `1_006_001` codec: exact value roundtrip, PG16 and PG17 rows in one
  section, `NULL` remaining distinct from zero, Parquet file framing, and the
  row, byte, and row-group caps. The golden test compares decoded values
  rather than raw Parquet bytes because Arrow metadata and encoding choices can
  change between dependency versions.

## Future Work

`kronika-derive` will generate codecs and golden tests from contracts. More
types will be added with the next sources. Bloom filters and row-group tuning
come with the first high-cardinality types that need them.
