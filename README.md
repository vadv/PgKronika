# PgKronika

[Русская версия](README.ru.md)

PgKronika records diagnostic history for a PostgreSQL instance in local,
immutable PGM segment files. A collector reads PostgreSQL statistics, Linux
`/proc` and cgroup data, and optionally PostgreSQL stderr logs. A separate web
process serves the recorded rows, counter diffs, anomaly episodes, and incident
clusters through a local UI and JSON API.

The project is under active development. The collector, local segment store,
reader, and web API are implemented and covered by PostgreSQL 15–18 BDD tests.
Packaging, retention management, remote archival, `pg_kronika-dump`, MCP,
alerting, and root-cause diagnosis are not implemented.

## Data path

```text
PostgreSQL 15–18       Linux /proc, /sys, cgroups       stderr log
        \                     |                           /
         kronika-source-pg / kronika-source-os / kronika-source-log
                              |
                 kronika-registry + kronika-derive
                              |
                kronika-writer -> active.parts -> *.pgm
                              |
                 kronika-store -> kronika-reader
                              |
             kronika-analytics -> pg_kronika-web
                    diff, anomaly       JSON/UI, incidents
```

The collector runs on the database host and opens no network listener. It
writes an append-only `active.parts` journal and seals its frames into
self-contained `.pgm` files. The web process reads both sealed files and valid
live journal parts from the same directory. It never connects to PostgreSQL.

PgKronika is useful when an operator needs detailed, high-cardinality evidence
close to the database: sessions, statements, plans, relation statistics,
replication, OS pressure, process and cgroup counters, or typed log events. It
does not replace a metrics alerting system or infer a cause from correlation.

## Support and prerequisites

| Area | Current contract |
| --- | --- |
| PostgreSQL | Majors 15, 16, 17, and 18 are exercised by the BDD matrix. |
| Platform | Linux. CI and the BDD image currently exercise `x86_64`. |
| Rust | Rust 1.96.0, pinned in [`rust-toolchain.toml`](rust-toolchain.toml). |
| Development build | GNU target; the command below matches the target used by CI. |
| Release build | The repository default is `x86_64-unknown-linux-musl` and requires `musl-gcc` plus the Rust target. There is no published release bundle yet. |
| PostgreSQL access | A login role with `CONNECT` on the databases to inspect. Grant `pg_monitor` (or equivalent narrower privileges) to expose other sessions' statistics. |
| Extensions | Optional. `pg_stat_statements` and either supported `pg_store_plans` fork are discovered where installed; missing extensions do not prevent core collection. |

The collector enumerates connectable, non-template databases and keeps at most
20 per-database connections. Its DSN selects the initial connection, not a
single-database collection mode. Full privilege and extension behavior is in
the [PostgreSQL collection contract](docs/type-registry/postgresql-collection.md).

## Build and run the shortest path

The following path produces a segment, opens it with the web process, and
returns the first useful API result. Run it from the repository root on Linux.

Build the two implemented user-facing binaries:

```sh
rustup target add x86_64-unknown-linux-gnu --toolchain 1.96.0
cargo +1.96.0 build --locked --target x86_64-unknown-linux-gnu \
  -p pg_kronika-collector -p pg_kronika-web
mkdir -p var/segments
```

Start the collector. Replace the DSN with a PostgreSQL 15–18 role and database
available on your host. `KRONIKA_SEGMENT_MAX_BYTES=0` seals every collection
window, which makes the first result available immediately.

```sh
KRONIKA_PG_DSN='host=127.0.0.1 port=5432 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR="$PWD/var/segments" \
KRONIKA_SEGMENT_MAX_BYTES=0 \
./target/x86_64-unknown-linux-gnu/debug/pg_kronika-collector
```

The process prints `ready`, then `sealed <path> reason=tick` after a successful
window. Collection failures are logged to stderr and do not stop later cycles.

In another shell, serve that directory on loopback:

```sh
KRONIKA_WEB_DIR="$PWD/var/segments" \
KRONIKA_WEB_ADDR=127.0.0.1:8688 \
./target/x86_64-unknown-linux-gnu/debug/pg_kronika-web
```

List the recorded sources and available sections:

```sh
curl -sS http://127.0.0.1:8688/v1/sources
curl -sS http://127.0.0.1:8688/v1/sections
```

The embedded UI is at `http://127.0.0.1:8688/`. The web server has no TLS. If
`KRONIKA_WEB_BASIC_AUTH` is unset, the UI and `/v1/*` are open; `/healthz`,
`/readyz`, and `/metrics` remain public even when Basic Auth is enabled.

## Workspace map

All packages are internal and share one version. Nothing is published to
crates.io.

| Package | Responsibility |
| --- | --- |
| [`kronika-format`](crates/kronika-format/) | PGM framing, catalog, CRC32C, dictionaries, and journal frame validation. |
| [`kronika-derive`](crates/kronika-derive/) | Internal `Section` derive that generates registry contracts and Parquet codecs. |
| [`kronika-registry`](crates/kronika-registry/) | Stable type ids, schemas, column semantics, gates, codecs, and registry linting. |
| [`kronika-writer`](crates/kronika-writer/) | Bounded section buffers, string interning, `active.parts`, and sealing. |
| [`kronika-store`](crates/kronika-store/) | Read-only scan of a local segment directory and live journal. |
| [`kronika-reader`](crates/kronika-reader/) | Verified section decode, snapshots, pagination, logical sections, gauges, and diffs. |
| [`kronika-analytics`](crates/kronika-analytics/) | Source-independent counter, anomaly, and overview contract kernels. |
| [`kronika-source-pg`](crates/kronika-source-pg/) | PostgreSQL queries and version-specific mapping into registry rows. |
| [`kronika-source-os`](crates/kronika-source-os/) | Bounded Linux `/proc`, `/sys`, filesystem, process, and cgroup readers. |
| [`kronika-source-log`](crates/kronika-source-log/) | Bounded stderr tailing, normalization, typed events, and gap reporting. |
| [`pg_kronika-collector`](bins/pg_kronika-collector/) | Collection lifecycle, pacing, budgets, coverage, journaling, and rotation. |
| [`pg_kronika-web`](bins/pg_kronika-web/) | Local UI, JSON API, auth, readiness, bounded queries, anomalies, incident clustering, and diagnostic findings. |
| [`kronika-bdd`](crates/kronika-bdd/) | Docker/Nix integration runner for the PostgreSQL 15–18 matrix. |
| [`xtask`](xtask/) | Dependency-boundary check used by CI. |
| `pg_kronika-archiver`, `pg_kronika-dump` | Placeholders that print an error and exit with status 2. |

The current dependency boundaries and data flow are described in
[`docs/architecture.md`](docs/architecture.md). CI enforces binary-to-crate
allow lists with `cargo run -p xtask -- check-deps`.

## Contracts that affect operators

- **Format and integrity.** PGM format version 1 uses little-endian framing,
  per-section CRC32C, and a CRC-protected end catalog. CRC detects accidental
  corruption; it is not authentication. Unknown or malformed data is reported
  or skipped with typed diagnostics rather than interpreted as valid rows.
- **Durability.** A journal frame is synchronized before append returns. Sealing
  writes and synchronizes a temporary file, then publishes without overwriting
  an existing segment. A torn final journal frame is truncated on recovery;
  other damage remains visible in scan diagnostics.
- **Resource bounds.** Registry sections are capped at 65,536 rows, 8 MiB of
  encoded bytes, and 16 Parquet row groups. The collector applies source,
  dictionary, cycle-time, journal, and cardinality caps. Reader queries have
  row and materialized-cell limits. Web permits one heavy anomaly/incident
  request at a time and returns `503` when that slot is busy.
- **Data quality.** A real unchanged counter produces a zero delta. Resets,
  missing coverage, first points, invalid time order, and disabled collection
  gates produce explicit no-data reasons. They are not converted to zero and
  are not bridged by diff or anomaly analysis.
- **Security.** Segment files can contain SQL, plans, object names, process
  arguments, and log text. Protect the output directory and backups. The
  collector does not encrypt or redact them. Bind web to loopback or place it
  behind a TLS reverse proxy; Basic Auth alone does not provide transport
  security.

Detailed limits and failure variants live in each crate's README and rustdoc.

## Documentation and validation

- Installation and first run: [Build and run the shortest path](#build-and-run-the-shortest-path)
- Collector configuration: [`pg_kronika-collector` operator guide](bins/pg_kronika-collector/README.md)
- JSON API and web configuration: [`pg_kronika-web` operator guide](bins/pg_kronika-web/README.md)
- PostgreSQL connection and collection behavior: [`docs/connection-and-multidb.md`](docs/connection-and-multidb.md)
- Type and data-quality contracts: [`docs/type-registry.md`](docs/type-registry.md)
- Local and CI tests: [`docs/testing.md`](docs/testing.md)
- BDD conventions and runner: [`docs/bdd-testing-guide.md`](docs/bdd-testing-guide.md) and [`kronika-bdd`](crates/kronika-bdd/)
- Current architecture: [`docs/architecture.md`](docs/architecture.md)
- Container reference: [`kronika-format`](crates/kronika-format/) and the historical design note [`docs/segment-format.md`](docs/segment-format.md)

For a documentation-only change, the repository's documented minimum gate is:

```sh
git diff --check
cargo +1.96.0 fmt --all --check
```

See [testing.md](docs/testing.md) before changing Rust code or BDD behavior.

## License

PgKronika is licensed under the [MIT License](LICENSE).
