# PgKronika

[Русская версия](README.ru.md)

PgKronika records diagnostic history for a PostgreSQL instance in local,
immutable PGM segment files. A collector reads PostgreSQL statistics, Linux
`/proc` and cgroup data, and PostgreSQL stderr logs by default when they are
discoverable and readable. A separate web process serves the recorded rows,
counter diffs, source-scoped timeline digests, health lines, notable events,
anomaly episodes, and incident clusters through a local UI and JSON API.

The project is under active development. The collector, local segment store,
reader, offline dump command, storage rotation, and web API are implemented.
PostgreSQL-facing flows are covered by PostgreSQL 15–18 BDD tests. Packaging,
remote archival, MCP, alerting, and root-cause diagnosis are not implemented.

## Data path

```text
PostgreSQL 15–18       Linux /proc, /sys, cgroups       stderr log
        \                     |                           /
         kronika-source-pg / kronika-source-os / kronika-source-log
                              |
                 kronika-registry + kronika-derive
                              |
          kronika-writer -> active.parts -> YYYY/MM/DD/*.pgm
                              |
                 kronika-store -> kronika-reader
                              |
             kronika-analytics -> pg_kronika-web
              diff, anomaly, health     JSON/UI, timeline, incidents

        data root / PGM / active.parts -> pg_kronika-dump -> JSON
```

The collector runs on the database host and opens no network listener. It
writes synchronized frames to the root-level `active.parts` journal and seals
them into self-contained `.pgm` files. The web process reads both sealed files
and valid live journal parts from the same data root. It never connects to
PostgreSQL.

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

Build the three implemented user-facing binaries:

```sh
rustup target add x86_64-unknown-linux-gnu --toolchain 1.96.0
make build TARGET=x86_64-unknown-linux-gnu
mkdir -p var/segments
```

Start the collector. Replace the DSN with a PostgreSQL 15–18 role and database
available on your host. `KRONIKA_SEGMENT_MAX_BYTES=0` seals every collection
window, which makes the first result available immediately.

```sh
KRONIKA_PG_DSN='host=127.0.0.1 port=5432 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR="$PWD/var/segments" \
KRONIKA_LOG_STATE_PATH="$PWD/var/pg_log_tail.state" \
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

List the available sections:

```sh
curl -sS http://127.0.0.1:8688/v1/sections
```

The embedded UI is at `http://127.0.0.1:8688/`, interactive Swagger UI at
`http://127.0.0.1:8688/swagger-ui/`, and the generated OpenAPI document at
`http://127.0.0.1:8688/openapi.json`. API errors are JSON objects with `code`
and `params`. The web server has no TLS. If `KRONIKA_WEB_BASIC_AUTH` is unset,
the UI, Swagger UI, OpenAPI document, and `/v1/*` are open; `/healthz`,
`/readyz`, and `/metrics` remain public even when Basic Auth is enabled.

[`bins/pg_kronika-web/src/api_docs.rs`](bins/pg_kronika-web/src/api_docs.rs)
is the single registry that connects documented handlers to both Axum and
OpenAPI. Rust response DTOs define the success schemas and every operation
lists its reachable HTTP statuses. The generated, tool-neutral view starts at
[`openapi/openapi.yaml`](bins/pg_kronika-web/openapi/openapi.yaml); paths and
schemas are split into `core`, `sections`, `analytics`, `timeline`, and `ui`
YAML files with standard relative `$ref` values. Refresh and structurally
round-trip-check the tree with:

```sh
make openapi
```

CI runs the same command and rejects a diff in the committed tree. Rust
contract tests also require one named success schema and one domain tag for
all 15 operations. `make openapi-bundle` writes an uncommitted single-file
bundle to `target/pg-kronika-openapi.yaml` when a downstream tool needs one.

With the demo already running, execute one deterministic, schema-validated
request per operation and require lightweight evidence of retained data:

```sh
make demo-api-smoke
```

The local-only smoke uses pinned Schemathesis through `uvx`; it is deliberately
not a CI job. Override the target with `DEMO_API_URL` and, when Basic Auth is
enabled, set `PG_KRONIKA_SMOKE_AUTH=user:password`.

## Workspace map

All packages are internal and share one version. Nothing is published to
crates.io.

| Package | Responsibility |
| --- | --- |
| [`kronika-format`](crates/kronika-format/) | PGM framing, catalog, CRC32C, dictionaries, and journal frame validation. |
| [`kronika-layout`](crates/kronika-layout/) | Typed `YYYY/MM/DD` data-root grammar, segment addresses, strict bounded discovery, and mutation ownership. |
| [`kronika-derive`](crates/kronika-derive/) | Internal `Section` derive that generates registry contracts and Parquet codecs. |
| [`kronika-registry`](crates/kronika-registry/) | Stable type ids, schemas, column semantics, gates, codecs, and registry linting. |
| [`kronika-writer`](crates/kronika-writer/) | Bounded section buffers, string interning, `active.parts`, and sealing. |
| [`kronika-store`](crates/kronika-store/) | Read-only scan of a local segment directory and live journal. |
| [`kronika-reader`](crates/kronika-reader/) | Verified section decode, snapshots, pagination, logical sections, gauges, diffs, and durable-first overview facts. |
| [`kronika-analytics`](crates/kronika-analytics/) | Source-independent counter, anomaly, notable-event, count, and health policy kernels. |
| [`kronika-source-pg`](crates/kronika-source-pg/) | PostgreSQL queries and version-specific mapping into registry rows. |
| [`kronika-source-os`](crates/kronika-source-os/) | Bounded Linux `/proc`, `/sys`, filesystem, process, and cgroup readers. |
| [`kronika-source-log`](crates/kronika-source-log/) | Bounded stderr tailing, normalization, typed events, and gap reporting. |
| [`pg_kronika-collector`](bins/pg_kronika-collector/) | Collection lifecycle, pacing, budgets, coverage, journaling, and rotation. |
| [`pg_kronika-web`](bins/pg_kronika-web/) | Local UI, JSON API, auth, readiness, bounded source-scoped timelines, anomalies, incident clustering, and diagnostic findings. |
| [`pg_kronika-dump`](bins/pg_kronika-dump/) | Read-only JSON inventory of a data root, exact PGM size/row inspection, and bounded damaged-journal forensics. |
| [`kronika-bdd`](crates/kronika-bdd/) | Docker/Nix integration runner for the PostgreSQL 15–18 matrix. |
| [`xtask`](xtask/) | Dependency-boundary check used by CI. |
| `pg_kronika-archiver` | Placeholder that prints an error and exits with status 2. |

The current dependency boundaries and data flow are described in
[`docs/architecture.md`](docs/architecture.md). CI enforces binary-to-crate
allow lists with `cargo run -p xtask -- check-deps`.

## Contracts that affect operators

- **Format and integrity.** PGM format version 1 uses little-endian framing,
  per-section CRC32C, and a CRC-protected end catalog. CRC detects accidental
  corruption; it is not authentication. Unknown or malformed data is reported
  or skipped with typed diagnostics rather than interpreted as valid rows.
- **Data-root layout.** `KRONIKA_OUT_DIR` and `KRONIKA_WEB_DIR` name the same
  PgKronika-owned root:

  ```text
  /data/active.parts
  /data/.pgkronika-quarantine-v1/
  /data/.pgkronika-writer.owner.lock
  /data/.pgkronika-overview.owner.lock
  /data/YYYY/MM/DD/N.pgm
  /data/YYYY/MM/DD/N.ovf
  ```

  `N` is `SegmentId`: Unix microseconds of the first collection window
  successfully appended to the segment. Its UTC day determines `YYYY/MM/DD`;
  a segment crossing midnight stays in that starting-day directory. Queries
  use the PGM catalog's `min_ts` and `max_ts`, not the path. Each owner-lock
  acquisition synchronizes the lock inode and data root; a retry after a
  failed initial root `fsync` does not treat `EEXIST` as proof of durability.
  Only one collector may write to a data root. Unknown files, directories, and
  symbolic links are never followed or interpreted. The writer tries to move
  each opaque entry into the bounded quarantine namespace without overwrite;
  a local rename failure is reported but does not hide valid segments or stop
  collection. This is the first supported layout in the unreleased project.
- **Durability.** `active.parts` journal version 1 uses magic `PGKJNL1\0` and a
  checksummed header that stores the active `SegmentId`. The first frame and
  id are synchronized together before append returns. On localized damage the
  exact journal inode is quarantined; bounded recovery accepts only complete
  frames whose frame, PGM, catalog, and section CRCs verify, then uses the
  normal append/seal path. Discarded bytes are accounted, a fresh generation
  starts, and collection continues even when evidence movement degrades
  locally. Invalid PGM files are quarantined whole and excluded while valid
  segments remain readable. Sealing writes and synchronizes a same-day
  temporary file, then publishes without overwriting an existing segment. At
  startup the writer lock holder quarantines recognized stale PGM temporaries;
  OVF temporaries remain overview-owned. For sealed segments, the
  timeline index checks admitted sibling
  fact files with matching lineage
  before the bounded process-local fallback. Only a recoverable publication
  failure can place the same immutable facts in that fallback.
- **Derived fact sidecars.** `KRONIKA_WEB_DIR` is one PgKronika-owned data
  root. Every sealed `YYYY/MM/DD/N.pgm` has at most one same-day `N.ovf`
  sibling. OVF uses directory addresses `(block_kind, logical_id)`: every file
  has one `UiSummary`, while each present UI view has an independent
  `EntitySeries(view_code)`. A reader can fetch one view without reading the
  other views. Entity series use Zstd level 1 only when compression saves more
  than 64 bytes; CRC32C covers stored bytes and decompression must produce the
  exact bounded decoded length. Missing buckets remain distinct from observed
  zeroes. One independently constructed `FactStore` or process holds the
  overview mutation lease; clones share it. Garbage collection scans the tree
  with fixed bounds, refuses to sweep after an unavailable, incomplete, or
  capped scan, and never deletes PGM source files or follows symlinks. Optional
  logical-byte and file-count ceilings apply only to recognized derived
  files. Write backoff never suppresses reads of valid sidecars.
- **Storage rotation.** `KRONIKA_RETENTION` bounds the whole `KRONIKA_OUT_DIR`
  tree; unset keeps the pre-rotation unbounded growth. A `<bytes>` value is a
  fixed budget and must be at least `2 × KRONIKA_SEGMENT_MAX_BYTES`. `auto`
  (equivalently `auto:80`) or `auto:<P>` targets a used fraction of the backing
  partition, `P` in `1..=99`. The collector owns rotation: after each
  publication and on a 60-second tick it deletes the oldest replaceable data —
  writer temporaries, then orphan overviews, then quarantined evidence oldest
  first, then sealed segments oldest first, each segment taking its sibling
  `N.ovf` in the same pass — and prunes emptied day directories. Quarantined
  evidence counts toward the budget. One pass deletes against a deficit
  computed at its start and stops at the first removal error; in `auto` mode
  bytes freed by unlink count as reclaimed even while readers hold the files
  open, so held descriptors never cause deleting extra history. With
  `KRONIKA_INTERVAL_S=0` the rotation timer wakes the process for rotation
  only; collection stays signal-driven. Direct unlink is safe because readers keep open descriptors
  and the overview owner revalidates identity. The active journal and the newest
  sealed segment are never deleted; when the tree is pinned to that minimum the
  collector logs a degradation event at most once per tick and keeps collecting.
  `auto` cooperates with other data on a shared partition — foreign growth
  speeds cleanup of our history down to the minimum — but reserves no free
  space. Fixed size may exceed the budget by the active journal's growth
  between ticks and by overview sidecars published since the last enforcement
  scan; every scan re-seeds the size counter, and an hourly authoritative
  recount bounds that drift even when the incremental counter alone never
  crosses the budget. Each deletion is logged with the path, `SegmentId`, freed
  bytes, the `budget` or `auto` reason, and the current size against the
  threshold.
- **Resource bounds.** Registry sections are capped at 65,536 rows, 8 MiB of
  encoded bytes, and 16 Parquet row groups. The collector applies source,
  dictionary, cycle-time, journal, and cardinality caps. Reader queries have
  row and materialized-cell limits. Web permits one heavy anomaly, incident, or
  uncached timeline request at a time and returns `503` when that slot is busy.
  The timeline response cache has count and byte bounds. Cursor-pinned views
  have independent count, byte, and lifetime bounds.
- **Data quality.** A real unchanged counter produces a zero delta. Resets,
  missing coverage, first points, invalid time order, and disabled collection
  gates produce explicit no-data reasons. Timeline responses report retained
  exactness, source completeness, physical-count semantics, and known loss as
  separate facts. Missing data is not converted to zero or bridged by diff or
  anomaly analysis.
- **Security.** Segment files can contain SQL, plans, object names, process
  arguments, and log text. Protect the output directory and backups. The
  collector does not encrypt or redact them. Bind web to loopback or place it
  behind a TLS reverse proxy; Basic Auth alone does not provide transport
  security.

PostgreSQL log collection is enabled by default. Its durable position must be
set with `KRONIKA_LOG_STATE_PATH` outside `KRONIKA_OUT_DIR`; back it up
separately at the same consistency point when exact log-tail recovery matters.

Detailed limits and failure variants live in each crate's README and rustdoc.

## Documentation and validation

- Documentation map and source-of-truth rules: [`docs/README.md`](docs/README.md)
- Installation and first run: [Build and run the shortest path](#build-and-run-the-shortest-path)
- Collector configuration: [`pg_kronika-collector` operator guide](bins/pg_kronika-collector/README.md)
- JSON API and web configuration: [`pg_kronika-web` operator guide](bins/pg_kronika-web/README.md)
- Offline data and journal inspection: [`pg_kronika-dump` operator guide](bins/pg_kronika-dump/README.md)
- PostgreSQL connection and collection behavior: [`docs/connection-and-multidb.md`](docs/connection-and-multidb.md)
- Type and data-quality contracts: [`docs/type-registry.md`](docs/type-registry.md)
- Local and CI tests: [`docs/testing.md`](docs/testing.md)
- Overview parity evidence: [`docs/qualification/overview-parity-v1.md`](docs/qualification/overview-parity-v1.md)
- BDD conventions and runner: [`docs/bdd-testing-guide.md`](docs/bdd-testing-guide.md) and [`kronika-bdd`](crates/kronika-bdd/)
- Current architecture: [`docs/architecture.md`](docs/architecture.md)
- PGM container reference: [`kronika-format`](crates/kronika-format/)

For a documentation-only change, the repository's documented minimum gate is:

```sh
git diff --check
cargo +1.96.0 fmt --all --check
```

See [testing.md](docs/testing.md) before changing Rust code or BDD behavior.

## License

PgKronika is licensed under the [MIT License](LICENSE).
