# pg_stat_user_tables (1_013) Design

## Purpose

`pg_stat_user_tables` records table-level PostgreSQL statistics per database.
Unlike instance-wide views, it must be queried through a connection to each
database. Kronika stores these rows as class B, database-local data.

The source combines:

- `pg_stat_user_tables` for table access, write, vacuum, and analyze counters;
- `pg_statio_user_tables` for shared-buffer read and hit counters;
- `pg_class` for relation size inputs, row estimates, TOAST relation links, and
  wraparound age signals.

The collector records source values only. It does not decide whether a table
has crossed an autovacuum or wraparound threshold.

## Type IDs

The layout follows PostgreSQL catalog growth:

| Layout | PostgreSQL versions | `type_id` | Columns |
| --- | --- | --- | --- |
| V1 | 10-12 | `1_013_001` | base table, statio, and `pg_class` values |
| V2 | 13-15 | `1_013_002` | V1 plus `n_ins_since_vacuum` |
| V3 | 16-17 | `1_013_003` | V2 plus `n_tup_newpage_upd`, `last_seq_scan`, `last_idx_scan` |
| V4 | 18 | `1_013_004` | V3 plus four cumulative vacuum/analyze timing counters |

All layouts use `snapshot_full` semantics and sort by `(datid, relid, ts)`.

## Candidate Selection

Large clusters can have many user tables, so the collector does not read every
row. It builds a `UNION` of top-N sets and then reads the selected relids. N is
`KRONIKA_PG_MAX_TABLES` and defaults to 500 per axis.

The axes are intentionally mechanical:

- read activity: on PG 10-15, the sum of scan and write counters used by the
  older catalog; on PG 16+, `GREATEST(last_seq_scan, last_idx_scan)` for read
  recency;
- write volume: `n_tup_ins + n_tup_upd + n_tup_del`;
- relation size: `pg_class.relpages`;
- dead tuples: `n_dead_tup`;
- transaction-id age: `age(relfrozenxid)`;
- multixact age: `mxid_age(relminmxid)`.

No autovacuum GUCs are read in this query. Thresholds, per-table reloptions,
and alert decisions belong to analysis code that consumes the segment.

## Columns

V4 is the superset. Older layouts drop the columns that did not exist in their
PostgreSQL major range.

```text
ts                              ts    T
datid                           u32   L
datname                         str   L
relid                           u32   L
schemaname                      str   L
relname                         str   L
tablespace                      str   L
seq_scan                        i64   C
seq_tup_read                    i64   C
idx_scan                        i64?  C
idx_tup_fetch                   i64?  C
n_tup_ins                       i64   C
n_tup_upd                       i64   C
n_tup_del                       i64   C
n_tup_hot_upd                   i64   C
n_tup_newpage_upd               i64   C
n_live_tup                      i64   G
n_dead_tup                      i64   G
n_mod_since_analyze             i64   G
n_ins_since_vacuum              i64   G
vacuum_count                    i64   C
autovacuum_count                i64   C
analyze_count                   i64   C
autoanalyze_count               i64   C
last_vacuum                     ts?   G
last_autovacuum                 ts?   G
last_analyze                    ts?   G
last_autoanalyze                ts?   G
last_seq_scan                   ts?   G
last_idx_scan                   ts?   G
total_vacuum_time               f64   C
total_autovacuum_time           f64   C
total_analyze_time              f64   C
total_autoanalyze_time          f64   C
main_fork_bytes                 i64   G
toast_bytes                     i64?  G
toast_n_live_tup                i64?  G
toast_n_dead_tup                i64?  G
toast_last_autovacuum           ts?   G
xid_age                         i64   G
mxid_age                        i64   G
reltuples                       i64   G
heap_blks_read                  i64   C
heap_blks_hit                   i64   C
idx_blks_read                   i64?  C
idx_blks_hit                    i64?  C
toast_blks_read                 i64?  C
toast_blks_hit                  i64?  C
tidx_blks_read                  i64?  C
tidx_blks_hit                   i64?  C
```

`main_fork_bytes` comes from `pg_relation_size(relid)`. It is not total
relation size. `toast_bytes` is total size for the TOAST relation and its
indexes when a TOAST relation exists.

Timestamp columns are stored as Unix microseconds, except the PG18
`total_*_time` counters, which preserve PostgreSQL's millisecond `f64` values.

## Null Semantics

The collector preserves catalog meaning instead of replacing `NULL` with zero:

- `idx_*` is `NULL` when the table has no indexes;
- `toast_*` is `NULL` when the table has no TOAST relation;
- `last_*` is `NULL` when the event has never occurred.

Real zero counters stay zero.

## Collector Integration

The daemon uses the connection pool in two ways:

1. `pool.main()` collects instance-wide sources.
2. `pool.per_db()` collects `pg_stat_user_tables` rows from each covered
   database.

Before a snapshot, the daemon refreshes the pool at the
`KRONIKA_PG_POOL_REFRESH_SECS` cadence. The table query runs under an adaptive
timeout capped by `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS`.

SQLSTATE handling is local to the database being collected:

- `57014` grows the adaptive timeout and retries the same database until the
  cap;
- `55P03` is logged as lock contention and skips that database for the current
  snapshot;
- other query errors skip that database and leave the rest of the segment
  intact.

Rows are collected into owned structs before string interning. The segment
dictionary then stores `datname`, `schemaname`, `relname`, and `tablespace`.

## Tests

Unit tests cover version selection, SQL terms, conversion from raw rows to each
layout, null preservation, and codec round trips. Collector tests verify that
`push_user_tables` writes the expected section type and interns strings. The BDD
matrix boots PostgreSQL 15-18, seeds two databases, seals a segment, decodes the
matching `1_013_00x` section, and checks dictionary resolution for the database,
schema, and table names.
