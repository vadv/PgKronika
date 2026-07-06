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

Test commands are documented in [docs/testing.md](docs/testing.md). It covers
the local Rust gate, full and tagged BDD runs through Docker/Buildx, the BDD
image cache, and the GitHub Actions/GitLab CI paths.

The implementation is moving crate by crate: format primitives, writer state,
type registry, collectors, and then serving binaries.

## License

PgKronika is licensed under the [MIT License](LICENSE).
