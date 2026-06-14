# PgKronika

PgKronika is an experimental PostgreSQL observability system built around
immutable local segment files instead of a central time-series database.

The collector runs on the database host, reads PostgreSQL, OS, cgroup, and log
sources, and writes self-contained PGM segment files. A web process reads those
segments for humans and agents through a web UI, MCP, and JSON API. Optional S3
archiving is handled by a separate process so cloud credentials do not enter the
privileged collector.

The project is still early: the format, writer, and registry crates are being
built first. There is no usable monitoring product yet.

## Goals

- Keep recent diagnostic history close to the database host.
- Make every segment independently readable: schemas, offsets, dictionaries,
  events, and precomputed chart points are stored in the same file.
- Preserve high-cardinality PostgreSQL detail without pushing it into
  Prometheus metrics.
- Support offline incident analysis by copying segment files and opening them
  locally.
- Keep process responsibilities narrow: collection, reading, archiving, and
  format diagnostics are separate binaries.

## Architecture

PgKronika is planned as four statically linked binaries:

| Binary | Role |
| --- | --- |
| `pg_kronika-collector` | Collects PostgreSQL, OS, cgroup, and log data; writes local PGM segments. |
| `pg_kronika-web` | Serves the web UI, MCP, and JSON API over segment stores. |
| `pg_kronika-archiver` | Uploads completed local segments to S3 when that mode is enabled. |
| `pg_kronika-dump` | Inspects, verifies, extracts, and compares segment files. |

The storage format is PGM: an immutable segment, usually around 10 minutes of
data, with snapshot sections, dictionaries, events, chart data, and a tail
catalog for range reads.

## Repository Layout

```text
bins/      command binaries
crates/    internal Rust crates
docs/      design notes and archive material
xtask/     workspace maintenance commands
```

Implemented behavior is documented next to the code: crate README files,
rustdoc, and tests. The `docs/` directory is kept for design history while the
implementation is still moving.

Current design notes:

- [`docs/architecture.md`](docs/architecture.md) describes the processes,
  deployment shapes, workspace layout, and versioning rules.
- [`docs/segment-format.md`](docs/segment-format.md) records the original PGM
  container design.
- [`docs/type-registry.md`](docs/type-registry.md) records the initial registry
  design for PostgreSQL, OS, cgroup, event, dictionary, and chart sections.
- [`docs/testing.md`](docs/testing.md) describes the testing strategy.
- [`docs/plan.md`](docs/plan.md) records the implementation sequence.

Most design notes are currently in Russian.

## Development

Install the Rust toolchain selected by [`rust-toolchain.toml`](rust-toolchain.toml),
then run:

```sh
cargo check --workspace
cargo run -p xtask -- check-deps
```

`check-deps` enforces architectural dependency rules between binaries and
internal crates. For example, storage-backend code must not become reachable
from the collector, and PostgreSQL source code must not become reachable from
the web process.

Implementation is moving crate by crate: format primitives first, then writer
state, type registry, collectors, and finally the serving binaries.

## License

PgKronika is licensed under the [MIT License](LICENSE).
