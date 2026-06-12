# PgKronika

PostgreSQL monitoring with a self-contained, immutable segment format (PGM)
instead of a TSDB: a collector writes ~10-minute segment files on the database
host, optionally archives them to S3, and a single web process serves the
history to humans and AI agents (web UI, MCP, JSON API).

**Status: design phase.** There is no working code yet; the workspace is a
skeleton. Design documents live in [`docs/`](docs/) (in Russian for now,
English translation is planned as they stabilize):

- [`docs/segment-format.md`](docs/segment-format.md) — the PGM segment format
- [`docs/type-registry.md`](docs/type-registry.md) — the data type registry
- [`docs/architecture.md`](docs/architecture.md) — processes and topologies
- [`docs/testing.md`](docs/testing.md) — testing strategy
- [`docs/plan.md`](docs/plan.md) — implementation plan

## Layout

Four binaries (`bins/`) over a set of internal crates (`crates/`). Dependency
rules between them are part of the architecture and are enforced in CI:

```sh
cargo run -p xtask -- check-deps
```

## License

[MIT](LICENSE)
