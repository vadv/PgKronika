# pg_kronika-collector

[Русская версия](README.ru.md)

`pg_kronika-collector` is the only process that connects to PostgreSQL and
writes PGM data. It reads due PostgreSQL, Linux, cgroup, and optional stderr-log
sources, appends one bounded collection window to `active.parts`, and seals the
journal into `<first_timestamp>.pgm` when a rotation condition fires.

The daemon prints `ready` and `sealed ...` state changes to stdout. Structured
logfmt diagnostics go to stderr. A failed collection cycle is logged and
retried; a configuration, initial connection, or journal-open error stops
startup.

## Required configuration

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_PG_DSN` | required | `tokio-postgres` URI or `key=value` connection string. |
| `KRONIKA_OUT_DIR` | required | Directory containing `active.parts` and sealed `.pgm` files. |
| `KRONIKA_SOURCE_ID` | `0` | `u64` source id stored in segment catalogs. Use a distinct non-zero value when multiple collectors share a directory. |
| `KRONIKA_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, or `trace`; an invalid value falls back to `info`. |

The output directory is created if absent. File modes follow the process umask.
Segments may contain SQL, plans, process arguments, and log text; restrict this
directory accordingly.

## Connection and query guards

| Variable | Default |
| --- | ---: |
| `KRONIKA_PG_STATEMENT_TIMEOUT_MS` | `15000` |
| `KRONIKA_PG_LOCK_TIMEOUT_MS` | `1000` |
| `KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS` | `10000` |
| `KRONIKA_PG_EXCLUDE_DATABASES` | empty; semicolon-separated names |
| `KRONIKA_PG_POOL_REFRESH_SECS` | `600` |
| `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` | `60000` |
| `KRONIKA_CYCLE_DB_BUDGET_MS` | `15000`; `0` disables the cycle-time budget |

All timeouts must be non-zero, and lock timeout must be below statement
timeout. The pool opens one main connection and up to 20 per-database
connections in database-name order. It retries closed connections and records
uncovered or skipped work instead of reporting it as complete data. See
[`docs/connection-and-multidb.md`](../../docs/connection-and-multidb.md).

## Cardinality and storage guards

| Variable | Default | Contract |
| --- | ---: | --- |
| `KRONIKA_PG_MAX_TABLES` | `500` | Top N per table-selection axis and database. |
| `KRONIKA_PG_MAX_INDEXES` | `500` | Top N per index-selection axis and database. |
| `KRONIKA_PG_MAX_STATEMENTS` | `500` | Top N per statement-selection axis. |
| `KRONIKA_PG_MAX_LOCK_ROWS` | `1000` | Maximum lock waiters, edges, and nodes accepted for a section. |
| `KRONIKA_PG_MAX_PLANS` | `500` | Maximum plan rows in one read. |
| `KRONIKA_PG_MAX_PLAN_TEXT` | `32768` | Per-plan text limit; accepted range is 1–65536 bytes. |
| `KRONIKA_PG_PLAN_TEXT_BUDGET` | `8388608` | Total plan-text bytes per read; `0` disables plan text, maximum 16 MiB. |
| `KRONIKA_PG_PLANS_INTERVAL_S` | `300` | Minimum interval between `pg_store_plans` reads. |
| `KRONIKA_OS_MAX_DISKS` | `256` | Lowest `(major, minor)` disk devices retained. |
| `KRONIKA_OS_MAX_PROCS` | `4096` | Lowest numeric PIDs retained. |
| `KRONIKA_OS_MAX_CGROUPS` | `1024` | Cgroup traversal count. |
| `KRONIKA_OS_MAX_CGROUP_IO_ROWS` | `4096` | Cgroup I/O rows per pass. |
| `KRONIKA_OS_CGROUP_MAX_DEPTH` | `8` | Cgroup traversal depth. |
| `KRONIKA_SEGMENT_MAX_BYTES` | `67108864` | Seal after this many raw journal bytes; `0` seals each window. |
| `KRONIKA_SEGMENT_MAX_AGE_S` | `900` | Maximum age of an open segment. |
| `KRONIKA_JOURNAL_MAX_BYTES` | `1073741824` | Hard on-disk journal cap; reaching it triggers an early seal. |

Invalid startup limits fail before collection when they would exceed a section
or dictionary contract. OS cap parse errors degrade to the documented default
and emit a warning.

## Scheduling

`KRONIKA_INTERVAL_S` is the timer tick (`5` seconds). Set it to `0` for
signal-driven collection only. Each source has its own base interval:

| Source | Variable | Default seconds |
| --- | --- | ---: |
| Activity | `KRONIKA_PG_ACTIVITY_INTERVAL_S` | 5 |
| Database | `KRONIKA_PG_DATABASE_INTERVAL_S` | 10 |
| Bgwriter/checkpointer | `KRONIKA_PG_BGWRITER_INTERVAL_S` | 10 |
| WAL | `KRONIKA_PG_WAL_INTERVAL_S` | 10 |
| PostgreSQL I/O | `KRONIKA_PG_IO_INTERVAL_S` | 10 |
| Archiver statistics | `KRONIKA_PG_ARCHIVER_INTERVAL_S` | 30 |
| Prepared transactions | `KRONIKA_PG_PREPARED_INTERVAL_S` | 30 |
| Vacuum progress | `KRONIKA_PG_PROGRESS_VACUUM_INTERVAL_S` | 10 |
| Statements | `KRONIKA_PG_STATEMENTS_INTERVAL_S` | 30 |
| User tables | `KRONIKA_PG_TABLES_INTERVAL_S` | 30 |
| User indexes | `KRONIKA_PG_INDEXES_INTERVAL_S` | 60 |
| Replication | `KRONIKA_PG_REPLICATION_INTERVAL_S` | 30 |
| Reset metadata | `KRONIKA_PG_RESET_METADATA_INTERVAL_S` | 30 |
| Instance metadata | `KRONIKA_INSTANCE_INTERVAL_S` | 60 |
| PostgreSQL settings | `KRONIKA_PG_SETTINGS_INTERVAL_S` | 3600 |
| Core OS | `KRONIKA_OS_CORE_INTERVAL_S` | 10 |
| Mount/topology | `KRONIKA_OS_MOUNTTOPO_INTERVAL_S` | 60 |
| Processes | `KRONIKA_OS_PROCESS_INTERVAL_S` | 5 |
| Process status | `KRONIKA_OS_PROCESS_STATUS_INTERVAL_S` | 30 |
| Cgroup | `KRONIKA_OS_CGROUP_INTERVAL_S` | 10 |
| Cgroup mapping | `KRONIKA_OS_CGROUP_MAPPING_INTERVAL_S` | 30 |
| PostgreSQL log | `KRONIKA_PG_LOG_INTERVAL_S` | 5 |

Activity can accelerate to `KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S` (`1`) when
active client backends reach `KRONIKA_PG_ASH_ACTIVE_THRESHOLD` (`20`).
Replication can accelerate to `KRONIKA_PG_REPLICATION_FAST_INTERVAL_S` (`10`)
when lag reaches `KRONIKA_PG_REPL_LAG_TRIGGER_S` (`10`) or retained WAL reaches
`KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES` (`1073741824`). A fast interval at or
above its base disables that trigger.

`SIGUSR2` forces all sources and seals the resulting window. `SIGTERM` and
`SIGINT` stop the loop; any already synchronized journal frames remain for
recovery and are sealed on the next start.

## Optional PostgreSQL log source

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_PG_LOG_ENABLED` | true only when `KRONIKA_LOG_PATH` is set | Enable log collection. |
| `KRONIKA_LOG_PATH` | unset | Explicit current log file. |
| `KRONIKA_LOG_ROOT` | unset | Root used for PostgreSQL log discovery. |
| `KRONIKA_LOG_FORMAT` | `stderr` | `stderr` is parsed; `csvlog` is accepted but reported as unsupported. |
| `KRONIKA_LOG_STATE_PATH` | `<out>/pg_log_tail.state` | Durable tail position. |
| `KRONIKA_LOG_START_AT_BEGINNING` | `false` | Start a newly discovered file at offset zero. |
| `KRONIKA_LOG_DISCOVERY_INTERVAL_S` | `60` | Rediscovery interval. |

The tailer applies fixed line, byte, time, backlog, and output caps. Rotation,
truncation, binary input, backlog skips, and exhausted budgets become typed gap
rows; the collector does not present a partial read as complete.

## Linux fixture overrides

`KRONIKA_PROC_ROOT`, `KRONIKA_SYS_ROOT`, and `KRONIKA_STATVFS_FIXTURE` exist for
BDD and parser fixtures. Production deployments normally leave them unset.

## Canonical run

```sh
KRONIKA_PG_DSN='host=127.0.0.1 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR=/var/lib/pg_kronika \
KRONIKA_SOURCE_ID=1 \
pg_kronika-collector
```

The binary has no command-line flags. Unknown configuration is not discovered
from a file; the environment is the complete operator interface.
