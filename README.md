# PgKronika

[Русская версия](README.ru.md)

PgKronika is an experimental PostgreSQL observability system based on immutable
local segment files, not on a central time-series database.

The collector runs on the database host. It reads PostgreSQL, OS, cgroup, and
log data, then writes self-contained PGM segment files. A separate web process
opens those segments through a UI, MCP, and JSON API. Remote archival, when
enabled, is handled by a separate archiver so cloud credentials do not need to
be present in the collector.

The project is still early. The current work is focused on the file format,
writer state, registry, and test infrastructure. There is no usable monitoring
product yet.

## Goals

- Keep recent diagnostic history on or near the database host.
- Make each segment readable on its own: schemas, offsets, dictionaries,
  events, and precomputed chart points are stored with the data.
- Keep high-cardinality PostgreSQL detail without forcing it into
  Prometheus metrics.
- Support offline incident analysis by copying segment files and opening them
  locally.
- Keep collection, reading, archiving, and format diagnostics in separate
  binaries.

## Architecture

PgKronika is planned as four statically linked binaries:

| Binary | Role |
| --- | --- |
| `pg_kronika-collector` | Collects PostgreSQL, OS, cgroup, and log data; writes local PGM segments. |
| `pg_kronika-web` | Serves the web UI, MCP, and JSON API over segment stores. |
| `pg_kronika-archiver` | Uploads completed local segments when remote archival is enabled. |
| `pg_kronika-dump` | Inspects, verifies, extracts, and compares segment files. |

The storage format is PGM: an immutable segment with snapshot sections,
dictionaries, events, chart data, and an end catalog. Segments are meant to be
copied, verified, and opened without contacting the original database host.

## Accepted Decisions

- A section `type_id` names the exact schema stored in the section. If a
  PostgreSQL major version changes a view's columns or moves counters to another
  view, PgKronika allocates a new schema variant (`C_SSS_VVV`) instead of using
  nullable version columns. `NULL` means a runtime-absent value, not "absent in
  this PostgreSQL version".
- The collector reads the PostgreSQL major version once from the connection
  handshake and dispatches to the collector whose codec matches that version's
  schema. Current splits are `1_006_001` for `pg_stat_bgwriter` on PG 15-16,
  `1_006_002` for `pg_stat_checkpointer` plus the slimmed `pg_stat_bgwriter` on
  PG 17+, `1_020_001` for reset context on PG 15, and `1_020_002` for reset
  context on PG 16+ with `pg_stat_io`.
- Segment-level context stays in service sections. PostgreSQL and extension
  versions belong in `instance_metadata`; reset timestamps belong in
  `reset_metadata`; settings and GUCs belong in the settings family once it is
  added. Stats sections do not repeat that context in every row.
- Crate READMEs and rustdoc next to the implementation are the contract
  documentation. The `docs/` directory is design history while the code is still
  changing.

## Repository Layout

```text
bins/      command binaries
crates/    internal Rust crates
docs/      historical design notes
xtask/     workspace maintenance commands
```

Current behavior is documented next to the code: crate README files, rustdoc,
and tests. The `docs/` directory is kept as design history while the
implementation is still moving.

## Development

Install the Rust toolchain selected by
[`rust-toolchain.toml`](rust-toolchain.toml), then run:

```sh
cargo check --workspace
cargo run -p xtask -- check-deps
```

`check-deps` enforces architectural dependency rules between binaries and
internal crates. For example, storage-backend code must not become reachable
from the collector, and PostgreSQL source code must not become reachable from
the web process.

The PostgreSQL BDD matrix can run locally, in GitHub Actions, and in GitLab CI.
See [crates/kronika-bdd/README.md](crates/kronika-bdd/README.md) for the quick
local test, Docker/Buildx flow, local Nix flow, and CI examples.

The implementation is moving crate by crate: format primitives, writer state,
type registry, collectors, and then serving binaries.

## License

PgKronika is licensed under the [MIT License](LICENSE).
