# pg_kronika-collector

[Русская версия](README.ru.md)

`pg_kronika-collector` is the only process that connects to PostgreSQL and
writes PGM data. It reads due PostgreSQL, Linux, cgroup, and PostgreSQL
stderr-log sources, appends one bounded collection window to `active.parts`,
and seals the journal into `YYYY/MM/DD/<segment_id>.pgm` when a rotation
condition fires.

The daemon prints `ready` and `sealed ...` state changes to stdout. Structured
logfmt diagnostics go to stderr. A failed collection cycle is logged and
retried. Configuration and initial-connection errors stop startup; localized
tree, PGM, journal-recovery, or quarantine damage is reported and excluded
while valid storage and future collection continue.

## Required configuration

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_PG_DSN` | required | `tokio-postgres` URI or `key=value` connection string. |
| `KRONIKA_OUT_DIR` | required | PgKronika-owned data root containing `active.parts`, owner locks, and the `YYYY/MM/DD` segment tree. |
| `KRONIKA_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, or `trace`; an invalid value falls back to `info`. |

The output directory is created if absent. File modes follow the process umask.
Segments may contain SQL, plans, process arguments, and log text; restrict this
directory accordingly.

## Data root and segment identity

The supported local layout is fixed:

```text
KRONIKA_OUT_DIR/
├── active.parts
├── .pgkronika-quarantine-v1/
├── .pgkronika-writer.owner.lock
├── .pgkronika-overview.owner.lock
└── YYYY/
    └── MM/
        └── DD/
            ├── N.pgm
            └── N.ovf
```

`N` is the `SegmentId`: the Unix timestamp in microseconds of the first
collection window successfully appended to the segment. `YYYY/MM/DD` is the
UTC day derived from that id. A segment open across midnight stays in its
starting day's directory; readers use the PGM catalog's `min_ts` and `max_ts`
for time-range queries.

The collector writes `active.parts`, the writer lock, and PGM files. The web
process may add the overview lock and derived `N.ovf` siblings. One collector
holds `.pgkronika-writer.owner.lock` for the lifetime of the process, so two
collectors cannot write the same data root. Use a separate `KRONIKA_OUT_DIR`
for each collector. After acquiring that lock, startup quarantines recognized
stale PGM publication temporaries. OVF and overview-probe temporaries remain
under the overview owner's control.

`active.parts` uses journal version 1 with magic `PGKJNL1\0`. Its checksummed
header stores the active `SegmentId`; the first id and frame are synchronized
together. A zero-length file is initialized in place. For other localized
format damage, startup first preserves the exact original inode without
overwrite. A bounded physical scan publishes only complete frames with valid
frame, PGM, catalog, and section CRCs under the trusted header `SegmentId`;
bytes that cannot be admitted are counted. A fresh canonical or fallback
journal generation then accepts new windows, so manual deletion is not
required. After successful PGM publication, the active generation becomes a
synchronized valid empty version-1 journal.

Canonical data has a closed grammar. Root-level `.pgm` or `.ovf` files,
symbolic links, unknown entries, malformed dates, and misplaced segment ids
are never followed or interpreted. The writer attempts a bounded,
collision-safe atomic quarantine rename; otherwise the entry is ignored with
a typed diagnostic. An invalid PGM is quarantined whole and excluded. Neither
case blocks valid segments or startup. The quarantine directory is opaque to
normal scans and is never removed automatically.

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
| `KRONIKA_JOURNAL_MAX_BYTES` | `1073741824` | Physical journal cap, including the reset marker; accepted range is 36 bytes–1 GiB, and reaching it triggers an early seal. |
| `KRONIKA_RETENTION` | unset | Storage rotation target for the whole data root: a byte budget (at least `2 × KRONIKA_SEGMENT_MAX_BYTES`), `auto` (= `auto:80`), or `auto:<P>` with `P` in `1..=99` as the partition used fraction. Unset disables rotation. |

Invalid startup limits fail before collection when they would exceed a section
or dictionary contract. OS cap parse errors degrade to the documented default
and emit a warning.

## Scheduling

`KRONIKA_INTERVAL_S` is the timer tick (`5` seconds). Set it to `0` for
signal-driven collection only; with rotation enabled the process still wakes
every 60 seconds, but that wake runs rotation only and never starts
collection. Each source has its own base interval:

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

Every actual `pg_store_plans` read also writes a coordinated reset-metadata
row at the plan snapshot's exact timestamp. The collector samples reset state
before and after the read and drops the plan snapshot if that state changes or
cannot be read.

Activity can accelerate to `KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S` (`1`) when
active client backends reach `KRONIKA_PG_ASH_ACTIVE_THRESHOLD` (`20`).
Replication can accelerate to `KRONIKA_PG_REPLICATION_FAST_INTERVAL_S` (`10`)
when lag reaches `KRONIKA_PG_REPL_LAG_TRIGGER_S` (`10`) or retained WAL reaches
`KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES` (`1073741824`). A fast interval at or
above its base disables that trigger.

`SIGUSR2` forces all sources and seals the resulting window. `SIGTERM` and
`SIGINT` stop the loop; any already synchronized journal frames remain for
recovery and are sealed on the next start.

## PostgreSQL log source

PostgreSQL log collection is enabled by default. Unless `KRONIKA_LOG_PATH` is
set, each discovery attempt checks that `SHOW log_destination` contains
`stderr`, then calls `pg_catalog.pg_current_logfile('stderr')`. A relative
result is resolved against `SHOW data_directory`, or against
`KRONIKA_LOG_ROOT` when that override is set.

The collector reports the outcome in `pg_log_source_status`:

| `state` | Meaning |
| --- | --- |
| `collecting` | The supported file was opened and processed. This is also the result for a readable file with no new lines. |
| `collecting_degraded` | The last known file was processed, but discovery could not be refreshed because no PostgreSQL client was available or the discovery query failed. Reading succeeded; this state alone does not prove data loss. |
| `unavailable` | No supported file could be read. `reason` distinguishes `no_current_logfile`, `unsupported_format`, `missing_file`, `permission_denied`, `read_error`, and discovery failures without a known file. |
| `disabled` | The operator explicitly set `KRONIKA_PG_LOG_ENABLED=0`. |

A status row is written on the first observation, when the state, reason,
parser, or path changes, and after the unchanged heartbeat interval. The rows
are available through `GET /v1/section/pg_log_source_status`.

The collector does not change PostgreSQL settings or file permissions. When no
committed tail position exists, the first read of a newly discovered file
starts at EOF. Set `KRONIKA_LOG_START_AT_BEGINNING=1` to read it from byte zero.

When discovery finds a different path, the collector reads the previous file
once more under the normal per-cycle limits: at most 4096 lines, 1 MiB, and
50 ms. After that result is committed, collection switches to the new file
even if the old tail is not exhausted. The first committed cycle from the new
file includes a `pg_log_gap` with `reason=rotation`; when the remaining old
tail could be measured, `bytes_skipped` is the number of unread bytes. This
keeps fresh-event delay to one bounded old-file read.

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_PG_LOG_ENABLED` | `true` | Attempt supported file-log discovery and reading; explicit `false` disables it. |
| `KRONIKA_PG_LOG_INTERVAL_S` | `5` | Attempt to read the known file. |
| `KRONIKA_LOG_DISCOVERY_INTERVAL_S` | `60` | Re-run PostgreSQL path discovery, including while no source exists. |
| `KRONIKA_PG_LOG_STATUS_INTERVAL_S` | `300` | Emit an unchanged status heartbeat; must be greater than zero. |
| `KRONIKA_LOG_PATH` | unset | Override the discovered path; does not override explicit disable. |
| `KRONIKA_LOG_ROOT` | unset | Root used for PostgreSQL log discovery. |
| `KRONIKA_LOG_FORMAT` | `stderr` | `stderr` is parsed; `csvlog` is accepted but reported as unsupported. |
| `KRONIKA_LOG_STATE_PATH` | required when enabled | Durable tail position. Must be outside `KRONIKA_OUT_DIR`. |
| `KRONIKA_LOG_START_AT_BEGINNING` | `false` | Start a newly discovered file at offset zero. |

The tailer applies fixed line, byte, time, backlog, and output caps. Rotation,
truncation, binary input, backlog skips, and exhausted budgets become typed gap
rows; the collector does not present a partial read as complete.

Because PostgreSQL log collection is enabled by default,
`KRONIKA_LOG_STATE_PATH` is normally required. It is not required when
`KRONIKA_PG_LOG_ENABLED=0`. The state file is deliberately outside the strict
data root; include it separately in a consistent backup when preserving the
exact committed log-tail position matters.

## Linux fixture overrides

`KRONIKA_PROC_ROOT`, `KRONIKA_SYS_ROOT`, and `KRONIKA_STATVFS_FIXTURE` exist for
BDD and parser fixtures. Production deployments normally leave them unset.

## Canonical run

```sh
KRONIKA_PG_DSN='host=127.0.0.1 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR=/var/lib/pg_kronika \
KRONIKA_LOG_STATE_PATH=/var/lib/pg_kronika-log.state \
pg_kronika-collector
```

The binary has no command-line flags. Unknown configuration is not discovered
from a file; the environment is the complete operator interface.
