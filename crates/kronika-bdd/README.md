# kronika-bdd

[Русская версия](README.ru.md)

`kronika-bdd` runs PostgreSQL integration scenarios. Nix provides the
PostgreSQL matrix (15 through 18); the runner boots it once per run, and every
scenario opens its own uniquely named database on one cluster, drives sessions
through `tokio-postgres`, snapshots `pg_kronika-collector` into a sealed
segment, and asserts the recorded rows.

## What It Runs

Every scenario follows `docs/bdd-testing-guide.md`: the setup SQL is visible in
the `.feature` as docstrings, the expected values are concrete and tied to that
setup, and PostgreSQL itself is the oracle.

`features/smoke.feature` is the one matrix-wide scenario:

```gherkin
Scenario: every booted major reports a matching server_version_num
  Given the PostgreSQL matrix is booted
  Then each cluster's declared major matches the result of:
    """
    SELECT current_setting('server_version_num')::int / 10000
    """
```

The metric features (activity, archiver, bgwriter/checkpointer, database, I/O,
locks, prepared transactions, progress vacuum, statements, user tables and
indexes, WAL, replication instance, connection pool) share one step vocabulary:

- `Given a fresh database on PostgreSQL NN` — the isolated per-scenario
  database;
- `Given a database seeded with:` and `Given a second database seeded with:` —
  docstring SQL setup;
- `Given session "X" runs:` / `… runs and holds its transaction open:` /
  `… runs and blocks:` — named sessions whose backend pids resolve `[X]`
  placeholders in expectation tables;
- `When the collector snapshots the segment` — runs `pg_kronika-collector`
  (path from `KRONIKA_COLLECTOR_BIN`) until it seals a segment;
- `Then section 1_XXX_YYY has exactly one row:` / `has a row for session "X":`
  / `has a row with <col> = <val>:` — expectation tables against the decoded
  section;
- `Then section 1_XXX_YYY <column> matches the <kind> oracle:` — an
  independent SQL read compared per kind: `exact`, `transformed`, `subset`,
  `floor` (lower bound), `ceiling` (upper bound);
- `Given the window floor for section 1_XXX_YYY <column> is captured as:` with
  `Then section 1_XXX_YYY <column> is between the captured floor and:` — check
  that a monotonically advancing counter lies between oracle reads taken before
  and after the snapshot;
- `Then section 1_XXX_YYY is absent from the segment` — the guard for
  layout-split metrics.

A step phrase that matches no registered step fails the run
(`fail_on_skipped`), and every failed assertion dumps the decoded section
table, the oracle values, `server.log`, and the collector's stderr.

## Running Tests

Use [../../docs/testing.md](../../docs/testing.md) for local and CI commands,
Docker/Buildx prerequisites, and the BDD image cache model.

Runner-only unit tests do not start PostgreSQL:

```sh
cargo test -p kronika-bdd
```

The usual tagged Docker/Nix run from the repository root is:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` is required and is passed to Cucumber as `--tags`. `DEBUG=1` enables
verbose Cucumber output and passes `DEBUG` into the container.

## Useful Failures

- `KRONIKA_PG_MATRIX is not set`: the runner was started outside the Nix image
  without PostgreSQL binary paths.
- `postgres ... not ready`: the server failed to start or did not accept TCP
  connections within 30 seconds. The error includes `server.log`.
- Smoke mismatch: the cluster answered, but `server_version_num / 10000` is
  not the major the matrix declared.
- assertion failures print the decoded section table, the oracle values, and
  both `server.log` and the collector's stderr; the message names the section
  and column.
