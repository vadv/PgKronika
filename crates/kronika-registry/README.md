# kronika-registry

[Русская версия](README.ru.md)

`kronika-registry` gives PgKronika's collection, writing, and reading paths a
shared data contract. A PGM catalog entry contains an offset, length, row
count, CRC32C, and other fields, but its only schema identifier is the numeric
`type_id`. The registry uses that number to define:

- the names and physical representation of the section body's columns;
- which values are cumulative counters, gauges, labels, or snapshot time;
- how rows are sorted and which labels identify one time series;
- how layouts from different PostgreSQL versions form one logical source;
- how typed rows are encoded as Parquet and decoded again.

It is not a process, configuration file, or table inside a PGM. The registry is
a static list of `TypeContract` values compiled into PgKronika's libraries and
binaries. The full contract is not serialized into the PGM, so a writer and a
reader must agree on what a `type_id` means.
The Parquet body contains physical column names and types, but it does not
declare their roles, `identity`, `semantics`, or collection gates. The decoder
uses that embedded Parquet schema for validation, not as a replacement for the
registry.

Runtime controls are listed under
[What can be configured](#what-can-be-configured). The registry itself has no
runtime configuration.

## Place in the data path

```text
PostgreSQL / Linux / cgroup / PostgreSQL log
        |
        | source chooses a struct for the source version
        | string and binary values are interned: bytes -> StrId
        v
typed rows from kronika-registry
        |
        | Section::encode
        | exact Arrow schema -> sort -> Parquet + Zstd-3
        v
section body
        |
        | kronika-writer adds a catalog entry and dictionaries
        v
active.parts / completed PGM
        |
        | catalog: type_id, offset, len, rows, CRC32C
        | CRC32C -> contract lookup -> schema check -> decode
        v
kronika-reader
        |
        | StrId resolution, layout union, time series, diffs
        v
pg_kronika-web / kronika-analytics
```

[`kronika-format`](../kronika-format/README.md) owns the outer file layout,
catalog, `StrId`, and dictionary-placement rules. The registry reserves the
dictionary `type_id` values and owns the meaning and codec of a typed section
body. [`kronika-writer`](../kronika-writer/README.md) encodes dictionary
bodies and writes parts and completed segments.
[`kronika-reader`](../kronika-reader/README.md) decodes dictionaries and
builds logical queries.

### Three meanings of "section"

| Term | Meaning |
| --- | --- |
| Physical PGM section | A catalog entry and the byte range it points to. For a registered type, the body is a self-contained Parquet file. |
| Section type | One `type_id` and its codec. Typed sections also have a `TypeContract`; dictionaries are the exception and have separate ids and codecs without a `registry()` entry. For example, `1_005_003` describes the PostgreSQL 14–17 `pg_stat_database` layout. |
| Logical section | The union of every registered layout with the same `name`. For example, four `pg_stat_database` layouts form one source when queried. |

The `Section` trait is implemented for a Rust struct type that describes one
row, not for each row instance. The sealed trait and internal derive macro
prevent a downstream crate from silently registering an arbitrary schema.

## What a contract contains

`registry()` returns the static contract list. One `TypeContract` contains:

| Field | Practical effect |
| --- | --- |
| `type_id` | Selects the exact body schema and codec. It is assigned in `#[section(id = ...)]`, not derived from a name, OID, or hash. |
| `name` | Groups layouts into one logical section at read time. |
| `semantics` | Defines which rows enter a body and the condition under which that body is emitted. |
| `columns` | Fixes each column's order, physical type, role, nullability, and optional collection gate. |
| `sort_key` | Defines canonical row order before encoding. |
| `identity` | Defines the labels of one time series for diffing. If empty, the reader uses `sort_key` without `ts`. |
| `deprecated` | Marks a layout no longer written while retaining its contract for old data. Every currently registered contract has `false`. |

The contract does not contain SQL, a schedule, top-N limits, timeouts, segment
rotation, or a counter reset source. Those decisions belong to the source,
collector, writer, and reader. Collection gates are properties of individual
columns, not a query schedule.

### Column roles

The registry has exactly four `ColumnClass` variants:

| Role | Stored value | Reader behavior |
| --- | --- | --- |
| `Timestamp` | Required `ts: Ts`, snapshot time in Unix microseconds. | Places rows on the time axis. |
| `Label` | An entity id or attribute such as an OID, PID, name, type, or flag. | May participate in the series key; being a `Label` does not itself make a column identity. |
| `Cumulative` | A cumulative counter such as `xact_commit`. | Compares adjacent samples of one series and computes a delta and per-second rate. |
| `Gauge` | An instantaneous value such as `numbackends`, XID age, or size. | Returns raw samples without subtracting adjacent points. |

`identity` is not a fifth column role. It is a separate list of `Label` column
names. There is likewise no `Constant` column class.

Physical types include narrow signed and unsigned integers, `f32`, `f64`,
`bool`, `Ts`, `StrId`, and `ColumnType::ListI32`. The last maps to Arrow
`List<Int32>` and a Rust `Vec<i32>` field. A codec uses the narrowest suitable
type: a PID is stored as `i32`, not `i64`. `Option<T>` makes a physical `NULL`
legal; a `NULL` in a required column is a decode error.

## Reading a `type_id`

Code writes an id as `C_SSS_VVV`:

```text
1_005_003
| |   |
| |   `-- VVV: storage-layout version, 003
| `------ SSS: type slot within the class, 005
`-------- C: section class, 1
```

On disk this is an ordinary `u32` with value `1005003`; the underscores exist
only to make the number readable. The parts mean:

- `C`: section class;
- `SSS`: allocated type slot within the class;
- `VVV`: version of the physical schema and its meaning.

`SSS` is not the PGM `source_id` of a PostgreSQL instance. `VVV` is not a
PostgreSQL version or a PGM container version. Source code maps a concrete
source version to a layout version.

| Class | Meaning | Current use |
| ---: | --- | --- |
| `1` | `Snapshot` | Every typed contract currently returned by `registry()`. This currently includes the PostgreSQL log event sections. |
| `2` | `Event` | Known by `TypeId`, but no current registered contract uses it. |
| `3` | `Dictionary` | `dict.strings = 3_001_001` and `dict.blobs = 3_002_001`; encoded separately and not included in `registry()`. |
| `10` | `Chart` | Reserved by `TypeId`; no current registered contract uses it. |

The `type_id` class and a contract's `semantics` are separate properties. The
current linter does not prove that they correspond, so an enum class does not
by itself imply that a source or codec exists.

### IDs that serve different purposes

| Identifier | What it distinguishes |
| --- | --- |
| `type_id` | A section-body layout, such as one version of `pg_stat_activity`. |
| PGM `source_id` | An opaque `u64` copied from `KRONIKA_SOURCE_ID` into segment metadata. The registry does not interpret it. |
| `StrId` | A text or binary value held by PGM dictionary sections. |
| `query_id`, `plan_id`, OID, PID | PostgreSQL or OS data inside a row; these values do not select the codec. |

## Example: a PostgreSQL upgrade and `pg_stat_activity`

`pg_stat_activity` changed across PostgreSQL versions. One `type_id` cannot
describe every variant without guessing:

| PostgreSQL | Rust struct | `type_id` | Difference |
| --- | --- | ---: | --- |
| 10–12 | `PgStatActivityV1` | `1_001_001` | No `leader_pid` or `query_id`. |
| 13 | `PgStatActivityV2` | `1_001_002` | Adds `leader_pid`. |
| 14–18 | `PgStatActivityV3` | `1_001_003` | Adds `query_id`. |

The source learns the server major version when it connects and selects the
matching SQL and struct. `pg_stat_statements` is different: its layout is
selected from the extension version in `pg_extension`, not from the
PostgreSQL major version.

All three activity contracts have:

```text
name      = "pg_stat_activity"
semantics = SnapshotFull
sort_key  = ["ts", "pid"]
identity  = []             # the reader uses pid from sort_key
```

A `kronika-reader` build containing all three contracts finds their `type_id`
values by `name`, verifies that the sort key agrees, and builds a column union.
Columns remain in first-appearance order: the complete V1 schema comes first,
then V2's `leader_pid`, then V3's `query_id`. Missing values become `NULL` when
an older body is read:

```text
body 1_001_001: ts | pid | datname | ... | state_change
body 1_001_002: ts | pid | leader_pid | datname | ... | state_change
body 1_001_003: ts | pid | leader_pid | datname | ... | query | query_id | ... | state_change
                                     |
                                     v
logical pg_stat_activity:
ts | pid | datname | ... | state_change | leader_pid? | query_id?
```

Here `?` means that the logical column allows `NULL`, including because an
older layout did not contain that column.

A stored section remains readable while the reader retains its `type_id`
contract. An unknown `type_id` does not become self-describing: the full
contract is absent from PGM, and a direct `decode_any` call returns
`UnknownType`.

Even adding a nullable column requires a new `type_id`. The decoder checks the
exact Arrow column count, order, types, and nullability; it does not infer
compatibility from Parquet contents.

### Where string values are stored

`datname`, `usename`, `query`, and other string columns do not repeat their
text in every `pg_stat_activity` row. The typed body stores a `StrId`; the
bytes live in PGM dictionary bodies:

The diagram below shows the column relationship, not a character-for-character
Parquet dump. On disk, `ts` is an `i64` and each `StrId` is a `u64`;
`S_database` and `S_query` stand for concrete numeric values shared by both
bodies.

```text
pg_stat_activity body, type_id = 1_001_003
+----------------+------+--------------------+------------------+
| ts             | pid  | datname            | query            |
+----------------+------+--------------------+------------------+
| 12:00:00       | 8241 | StrId(S_database)  | StrId(S_query)   |
+----------------+------+--------------------+------------------+

dict.strings body, type_id = 3_001_001
+--------------------+------------------------------------------+
| str_id             | bytes                                    |
+--------------------+------------------------------------------+
| S_database         | "orders"                                 |
| S_query            | "select * from orders where id = $1"     |
+--------------------+------------------------------------------+
```

Short values live in `dict.strings`; large or truncated values may live in
`dict.blobs`. See
[Where string values physically live](../kronika-format/README.md#where-string-values-physically-live)
for the exact layout and deduplication rules.

## Example: deriving `tx/s` from `pg_stat_database`

For PostgreSQL 14–17 the source writes type `1_005_003`. One row describes one
database:

```text
name      = "pg_stat_database"
sort_key  = ["datid", "ts"]
identity  = ["datid"]
```

Suppose two snapshots for `datid = 16384` are 60 seconds apart:

| Time | `datid` | `xact_commit` (`Cumulative`) | `numbackends` (`Gauge`) | `blk_read_time` (`Cumulative`) |
| --- | ---: | ---: | ---: | ---: |
| `12:00:00` | `16384` | `10,000` | `4` | `800 ms` |
| `12:01:00` | `16384` | `10,120` | `7` | `860 ms` |

The reader first groups both rows by `identity = ["datid"]`, then interprets each
column according to its role:

```text
xact_commit:
    delta = 10,120 - 10,000 = 120 transactions
    rate  = 120 / 60 = 2 transactions/s

numbackends:
    series values = 4, 7
    no 7 - 4 delta is computed because this is a Gauge

blk_read_time with admissible track_io_timing readings:
    delta = 860 - 800 = 60 ms
    rate  = 60 / 60 = 1 ms/s
```

The first cumulative point has no preceding sample and yields `FirstPoint`.
If the next `xact_commit` falls from `10,120` to `20`, the reader yields
`Reset`, not `-168.3 tx/s`. An interval with no readable segment or part yields
`Gap`. `collection_coverage` and `snapshot_coverage` are separate signals and
do not create a `Gap` by themselves. A delta and rate require exactly two
adjacent valid samples.

PostgreSQL reports zero `blk_read_time` while `track_io_timing` is disabled.
To keep that zero from looking like measured timing, the contract gates the
column with `reset_metadata.track_io_timing`. The reader checks the gate over
the interval between snapshots using discrete readings:

- the latest reading at the interval start is `true`, and no `false` or `NULL`
  reading occurs through the interval end: the diff is retained;
- the initial state is unknown, or any reading is `false` or `NULL`: the result
  is `NotCollected`.

A GUC change between readings is unobservable. `reset_metadata` also records
the collector session's setting; interpreting aggregate counters assumes that
contributing sessions use the same setting.

A gate does not disable SQL or change the stored source value. It controls the
interpretation of a decoded diff. On PostgreSQL 18, `PgStatIoV2` uses
`track_wal_io_timing` for `read_time`, `write_time`, `extend_time`, and
`fsync_time` in rows where `object = "wal"`. `writeback_time` and other rows
continue to use `track_io_timing`.

### `NULL`, zero, and no measurement

| State | Meaning |
| --- | --- |
| `NULL` in a nullable column | The source supplied no value for that row. For example, `datname` is absent from the shared `datid = 0` row. |
| Numeric `0` | A real stored value. A zero counter delta means no increase, not missing data. |
| `NotCollected` | A derived reader reason: the metric was not known to be collected over the whole interval. It is not a Parquet cell. |
| `FirstPoint`, `Reset`, `Gap` | Other reasons why a cumulative counter has no valid diff. |

Anomaly detection happens later. `kronika-analytics` receives calculated rates
for `Cumulative` columns and raw values for `Gauge` columns. The formulas,
minimum sample counts, and an `xact_commit` example are in
[`kronika-analytics`](../kronika-analytics/README.md#how-pgkronika-detects-an-anomaly).

## Collection semantics

`Semantics` describes what one section body means. It neither starts collection
nor defines its interval.

| `Semantics` | Source promise | Current examples |
| --- | --- | --- |
| `SnapshotFull` | A regular collection writes the complete result returned by the source module. This does not prove that the underlying PostgreSQL or OS view was read without limits. | Most `pg_stat_*`, OS, and cgroup sections. |
| `ConditionalFull` | A full snapshot is emitted only when a collection condition holds. An absent body does not by itself mean an empty source. | `pg_locks`, `pg_stat_progress_vacuum`, and the vacuum observation. |
| `EventStream` | The body contains events observed during the segment interval. | Typed PostgreSQL log events. |
| `Changed` | Changed cumulative-counter rows plus an `is_baseline` row are emitted. | Supported by the `Semantics` enum and linter, but no current registered contract uses it. |
| `OnChange` | A snapshot is emitted on change or after a periodic refresh. | `pg_settings`, `os_mountinfo`, and `os_topology`. |

Collection semantics do not prove that a particular snapshot is complete.
Top-N selection, timeouts, insufficient privileges, and collector-side loss
are separate signals. If the source emits no completeness provenance, it
cannot be inferred from `semantics`.

## Snapshot completeness

A row count is not proof of completeness. The registry defines two provenance
sections:

| Logical section | What it reports |
| --- | --- |
| `collection_coverage` (`1_023_001`) | For `source_type_id`: known lower bound `total`, its `unknown_total` inexactness flag, written rows `collected`, limit `max_n`, axis `order_by`, boundary `cutoff_value`, and reason `0` top-N, `1` timeout, `2` insufficient privileges, or `3` other. |
| `snapshot_coverage` (`1_038_001`) | For an attempted multi-row snapshot: read state, visibility, observed and durably written row counts, plus `collector_pid` and `collector_started_at`. |

An older segment without `snapshot_coverage` has unknown coverage, not
automatically complete coverage. A provenance section neither restores omitted
rows nor changes every query response automatically. It makes completeness
facts available; each consumer must explicitly use them when shaping a result.

## Encoding and validation

The internal `#[derive(Section)]` macro produces from one named-field Rust
struct:

- its `TypeContract`;
- its exact Arrow schema;
- its Parquet encoder and decoder;
- its `ts` range calculation.

Before writing, the codec checks the row count, builds Arrow arrays, sorts the
`RecordBatch` by `sort_key`, and writes a self-contained Parquet file with
Zstd level 3. Arrow metadata is omitted and `created_by` is left empty.

On read, `decode_any`, `decode_rows`, and `Section::decode` accept a
`VerifiedSection`, meaning the body CRC32C has already matched its catalog
entry. Before reading column data, the codec checks the body size, Parquet
metadata, row-group count, and claimed row count. The decoder then requires an
exact schema match and rejects a `NULL` in a required column.

Hard limits are compiled into the codec:

| Limit | Value |
| --- | ---: |
| Rows in one body | `65,536` |
| Size of one body | `8 MiB` |
| Parquet row groups | `16` |
| `ListI32` values in one row | `4,096` |
| `ListI32` values in one body | `262,144` |

An exceeded limit, unknown `type_id`, schema mismatch, bad CRC, or Parquet
failure returns `CodecError`. The codec never truncates automatically. A source
module must reduce its output before encoding. The current collector writes
`collection_coverage` when tables, indexes, statements, or plans are
truncated; other source limits do not produce that section automatically. An
encoding failure rejects the current collection window; the collector records
the failure and continues work on the next cycle.

`lint` checks duplicate `type_id` values, key columns, the `ts`, identity, and
`Changed` requirements, and valid `eps_abs` values. `lint_references` checks
gate references, their Boolean type, row selectors, and consistency across
layouts. The linter does not currently enforce a relationship between a
`type_id` class and `semantics`.

## What can be configured

`kronika-registry` has no CLI, environment variables, or configuration file.
An operator cannot change `type_id` values, schemas, column roles, keys,
Zstd-3, or codec limits at runtime.

| Task | Where it is configured |
| --- | --- |
| Sampling intervals for `pg_stat_activity`, `pg_stat_database`, OS, and other sources | `*_INTERVAL_S` in [`pg_kronika-collector`](../../bins/pg_kronika-collector/README.md#scheduling). |
| Top-N limits for tables, indexes, statements, and plans | `KRONIKA_PG_MAX_TABLES`, `KRONIKA_PG_MAX_INDEXES`, `KRONIKA_PG_MAX_STATEMENTS`, and `KRONIKA_PG_MAX_PLANS` in the collector's [cardinality and storage guards](../../bins/pg_kronika-collector/README.md#cardinality-and-storage-guards). |
| Timeouts, excluded databases, and cycle budget | `KRONIKA_PG_STATEMENT_TIMEOUT_MS`, `KRONIKA_PG_LOCK_TIMEOUT_MS`, `KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS`, `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS`, `KRONIKA_PG_EXCLUDE_DATABASES`, and `KRONIKA_CYCLE_DB_BUDGET_MS` under the collector's [connection and query guards](../../bins/pg_kronika-collector/README.md#connection-and-query-guards). |
| Open-segment rotation and journal cap | `KRONIKA_SEGMENT_MAX_BYTES` is a threshold on raw `active.parts` bytes after an append; `KRONIKA_SEGMENT_MAX_AGE_S` sets the age; `KRONIKA_JOURNAL_MAX_BYTES` is the hard journal cap. See the collector's [cardinality and storage guards](../../bins/pg_kronika-collector/README.md#cardinality-and-storage-guards). |
| Anomaly window, step, and threshold | [`pg_kronika-web` query parameters](../kronika-analytics/README.md#anomalies-and-incidents). |

`BytesPool` is a bounded pool of reusable input buffers and does not cache
decompressed Arrow arrays. Its `buffer_limit` controls whether a returned
buffer is retained, not whether a checked-out buffer may temporarily grow.
Current PgKronika components do not expose its configuration to operators.

## Changing the registry

A section-type change affects data compatibility, so adding a Rust field is
not sufficient:

1. Decide whether the logical source keeps the same meaning. A new entity gets
   a new `SSS`; a new layout of the same entity increments `VVV`.
2. Never reuse an existing `type_id`. Even a new nullable column requires a
   new layout version.
3. Add the struct and annotations under `src/codec/`, then include its contract
   in `registry()` in `src/lib.rs`.
4. Make the source select the new layout explicitly from the PostgreSQL,
   extension, or other source version.
5. Preserve the type and role of common columns and the `sort_key` if layouts
   with one `name` must be unioned. Changing the set of `identity` columns
   changes the series key and can split old and new rows; reordering the same
   names does not.
6. Add round-trip, schema, limit, and lint tests. Update cross-section tests for
   a collection gate.
7. Update the id allocation table and documentation in the same change.

The executable schema is in [`src/lib.rs`](src/lib.rs),
[`src/contract.rs`](src/contract.rs), and [`src/codec/`](src/codec/). Identifier
allocation and the type inventory are in
[`docs/type-registry.md`](../../docs/type-registry.md), with additional
collection notes in
[`docs/type-registry/semantics.md`](../../docs/type-registry/semantics.md).
Those documents also discuss reserved and proposed layouts; an entry in a
design note does not mean that the current `registry()` already has a codec.
