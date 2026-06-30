# pg_stat_user_tables (1_013) Implementation Notes

`pg_stat_user_tables` is the first database-local PostgreSQL source in
Kronika. The collector reads it through the per-database connection pool, joins
the matching `pg_statio_user_tables` counters and selected `pg_class` values,
and stores one row per selected table per database.

## Files

- `crates/kronika-registry/src/codec/pg_stat_user_tables.rs` defines the four
  section layouts and codec tests.
- `crates/kronika-source-pg/src/user_tables.rs` selects the layout for the
  server major version, builds the SQL, parses raw rows, and maps them to
  registry rows.
- `bins/pg_kronika-collector/src/main.rs` refreshes the pool, walks
  `per_db()`, retries statement timeouts with `AdaptiveTimeout`, and buffers
  the typed rows after all awaits finish.
- `crates/kronika-bdd/features/user_tables.feature` checks the live matrix path
  with two seeded databases and dictionary-backed labels.
- `docs/type-registry/postgresql.md` and
  `docs/type-registry/postgresql-collection.md` describe the public type
  contract and collection behavior.

## Layout Versions

The catalog only adds columns across PostgreSQL 10-18, so the registry uses one
type id per layout:

| `type_id` | PostgreSQL versions | Added columns |
| --- | --- | --- |
| `1_013_001` | 10-12 | base layout |
| `1_013_002` | 13-15 | `n_ins_since_vacuum` |
| `1_013_003` | 16-17 | `n_tup_newpage_upd`, `last_seq_scan`, `last_idx_scan` |
| `1_013_004` | 18 | `total_vacuum_time`, `total_autovacuum_time`, `total_analyze_time`, `total_autoanalyze_time` |

All versions sort rows by `(datid, relid, ts)`. `datid` is stored with
`datname` because table OIDs are database-local and database names can be
renamed.

## Candidate Selection

The collector limits cardinality with a mechanical union of top-N candidate
sets. `KRONIKA_PG_MAX_TABLES` controls N per axis and defaults to 500.

The axes are:

- read activity: counter sum on PG 10-15, read recency on PG 16+ using
  `GREATEST(last_seq_scan, last_idx_scan)`;
- write volume: `n_tup_ins + n_tup_upd + n_tup_del`;
- relation size: `pg_class.relpages`;
- dead tuples: `n_dead_tup`;
- transaction-id age: `age(relfrozenxid)`;
- multixact age: `mxid_age(relminmxid)`.

The SQL does not apply autovacuum thresholds or attach threshold status to
rows. It records bounded source values; analysis code can evaluate thresholds
when reading the data.

## Row Semantics

`pg_statio_user_tables` is joined by `relid` so buffer counters share the same
snapshot row as `pg_stat_user_tables`. The query also adds
`age(relfrozenxid)`, `mxid_age(relminmxid)`, `reltuples`, `main_fork_bytes`, and
TOAST relation data from `pg_class` and size functions.

`main_fork_bytes` is `pg_relation_size(relid)`, not total relation size.
`toast_bytes` is `pg_total_relation_size(reltoastrelid)` and is `NULL` when the
table has no TOAST relation.

`NULL` is meaningful:

- `idx_*` is `NULL` when the table has no indexes;
- `toast_*` is `NULL` when the table has no TOAST relation;
- `last_*` is `NULL` when the event has never occurred.

Timestamp columns use Unix microseconds. PG18 `total_*_time` columns keep the
PostgreSQL `double precision` millisecond values.

## Collector Behavior

The daemon refreshes the connection pool before each snapshot. It then collects
instance-wide sources through `pool.main()` and table rows through every
covered database in `pool.per_db()`.

The table query can call expensive size functions, so it runs under a separate
adaptive `statement_timeout`: it starts at 15 seconds and doubles on SQLSTATE
`57014` until `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` (default 60000). SQLSTATE
`55P03` is logged as lock contention. Other per-database errors skip that
database without discarding the rest of the segment.

All per-database rows are owned before interning. This keeps the async
collection path separate from `SectionBuffers` and `Interner`, which are not
held across awaits.

## Verification

The implementation includes focused unit tests for layout selection, SQL terms,
null mapping, codec round trips, and collector buffering. The BDD scenario
boots the PostgreSQL matrix, seeds two databases, seals a segment, decodes the
matching `1_013_00x` section, and verifies that `datname`, `schemaname`, and
`relname` resolve through the dictionary.
