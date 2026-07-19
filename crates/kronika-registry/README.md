# kronika-registry

[Русская версия](README.ru.md)

`kronika-registry` is the authority for PGM section meaning. It assigns stable
type ids, declares schemas and data semantics, and contains the typed Parquet
codecs used by writer and reader.

## Type contract

A `TypeContract` binds one `type_id` to:

- a logical section name and deprecation state;
- ordered columns, physical types, nullability, and column classes;
- collection semantics;
- sort and series-identity keys;
- optional collection gates and per-row gate overrides.

Type ids use decimal `C_SSS_VVV`: section class, source number, and layout
version. Current classes are snapshot (`1`), event (`2`), dictionary (`3`),
and chart (`10`). A new incompatible layout receives a new version. Several
versions may share a logical name and are unioned by the reader.

`ColumnClass` drives downstream interpretation. Cumulative values are diffed;
gauges are sampled directly; identity, timestamp, constant, and label columns
have separate invariants. A nullable value is missing data, never an implicit
zero.

## Section codecs

Each registered row implements the sealed `Section` trait. The internal
`kronika-derive` proc macro generates its contract, Arrow schema, Parquet
encoder/decoder, and timestamp range from one annotated named-field struct.
Downstream crates cannot register arbitrary types.

`VerifiedSection` owns bytes whose CRC was checked by the caller. The registry
stays independent of PGM framing while preventing unverified bytes from
reaching Parquet through its normal API. Generic decode returns positional
`Row`/`Cell` values or Arrow batches without constructing a map per cell.

## Invariants and limits

`lint` validates ids, timestamp columns, sort and identity keys, type/class
compatibility, and section semantics. `lint_references` validates collection
gate targets and row-specific overrides across the full registry.

Every codec enforces:

- at most `MAX_SECTION_ROWS = 65_536` rows;
- at most `MAX_SECTION_BYTES = 8 MiB` encoded input;
- at most `MAX_ROW_GROUPS = 16` Parquet row groups.

Encode rejects an oversized row slice before building Arrow arrays. Decode
checks byte size and Parquet metadata before materializing rows. `BytesPool`
can reuse bounded input buffers; it does not cache decompressed Arrow arrays.

## Data-quality ownership

PostgreSQL and Linux facts that affect interpretation belong in contracts:
layout versions, identity, reset sources, entry epochs, and collection gates.
For example, a timing column gated by `track_io_timing` becomes
`NotCollected` in reader diff while the gate is off; it is not emitted as a
measured zero.

The registry does not query sources, choose scheduling, write segments, scan
stores, or shape HTTP responses. The registered PostgreSQL and OS types are
listed in [`../../docs/type-registry.md`](../../docs/type-registry.md); code in
[`src/lib.rs`](src/lib.rs) and `src/codec/` is canonical when design notes
disagree.
