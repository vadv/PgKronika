# kronika-bdd

[Русская версия](README.ru.md)

`kronika-bdd` is the integration-test runner for collector and web behavior on
PostgreSQL 15, 16, 17, and 18. Nix supplies the server binaries and supported
`pg_store_plans` forks; Docker runs the same image locally and in GitHub
Actions.

The runner is not a production package. Host `cargo test --workspace` does not
start PostgreSQL.

## Scenario lifecycle

The PostgreSQL matrix boots once per runner process. Scenarios execute one at a
time, create an isolated database, open named `tokio-postgres` sessions, drive
the state described in the feature file, run the collector until it seals a
segment, and compare decoded rows with explicit expectations or an independent
PostgreSQL oracle. Cleanup closes sessions and removes scenario state.

Skipped Cucumber steps fail the run. A failure reports the relevant decoded
section, oracle values, collector output, and PostgreSQL logs. Matrix smoke
also checks that each binary reports the declared major through
`server_version_num`.

## Commands

Runner-only unit tests:

```sh
cargo test -p kronika-bdd
```

Full Docker/Nix matrix from the repository root:

```sh
DEBUG=1 make test-bdd
```

One tag expression:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` is validated and passed to Cucumber as `--tags`. `DEBUG=1` enables
verbose runner output. Docker daemon and Buildx are required; host Nix is not.
See [`../../docs/testing.md`](../../docs/testing.md) for cache and CI behavior.

## Runner environment

The Nix image sets:

- `KRONIKA_PG_MATRIX`, a semicolon-separated `major=bin_dir` map;
- `KRONIKA_COLLECTOR_BIN`, the collector executable;
- `KRONIKA_FEATURES`, the feature directory.

Starting the binary outside that environment normally fails with
`KRONIKA_PG_MATRIX is not set`. Use `make test-bdd` unless you are developing
the image itself.

Feature authoring and oracle rules are in
[`../../docs/bdd-testing-guide.md`](../../docs/bdd-testing-guide.md). The
current feature files and step implementations are canonical when older design
examples differ.
