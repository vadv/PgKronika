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

## Query-plan evidence split

Plan evidence uses two complementary, non-flaky paths:

- `@plan_evidence` writes fixed upstream-compatible plan, coverage, reset, and
  instance rows through the real PGM writer, starts the packaged web process,
  and checks both stable plan signal IDs through `GET /v1/anomalies`. It does
  not depend on an optimizer choosing a particular plan.
- `@plan_collector` runs the real extensions on PostgreSQL 15-18. The upstream
  and vadv scenarios compare all ten exposed buffer counters with an
  independent live SQL oracle and require complete snapshot, reset,
  extension-version, query-id-setting, node, system-id, and server-major
  provenance in the sealed segment.

This separation proves deterministic analyzer behavior through real storage
and HTTP while independently proving the version- and fork-specific collector
contract. Neither path uses retries, timing sleeps, or optimizer-forcing
assertions.

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

Plan evidence and the live collector matrix can be selected independently:

```sh
DEBUG=1 make test-bdd TAGS=@plan_evidence
DEBUG=1 make test-bdd TAGS=@plan_collector
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
