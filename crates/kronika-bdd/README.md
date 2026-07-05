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

## Quick Local Check

This runs only unit tests. It does not start PostgreSQL:

```sh
cargo test -p kronika-bdd
```

Use this for `KRONIKA_PG_MATRIX` parsing and runner code. It is not the full
PostgreSQL run.

## Full Local Run With Docker

This path needs Docker with Buildx. Nix runs inside Docker. The builder image
stores the Rust and PostgreSQL dependency build.

From the repository root:

```sh
export BDD_IMAGE_PREFIX=ghcr.io/vadv/pgkronika
export BDD_BUILDER_PULL=1

./scripts/bdd-image.sh build-builder
./scripts/bdd-image.sh build-runtime
./scripts/bdd-image.sh run
```

`build-builder` pulls `pgkronika-bdd-builder` when the dependency key exists in
the registry. Otherwise it builds the image locally. `build-runtime` uses that
builder to create `image.tar`, loads `pgkronika-bdd:latest` into Docker, and
leaves the tarball in the working tree.

The first builder build after a dependency change is still expensive. Later
source-only changes reuse the same builder image.

For a tagged local BDD run, use the repository Makefile target:

```sh
DEBUG=1 make test-bdd TAGS=@pg_log
```

`TAGS` is required and is passed to Cucumber as `--tags`. The target uses the
same Docker/Buildx image path as the full run and uses a transient runtime
tarball under `/tmp` unless `BDD_OUTPUT_TAR` is set. When `BDD_RUNTIME_IMAGE` is
not set, the target tags the local image by platform and BDD runtime input hash,
so changes to Makefile, helper scripts, or README files do not rebuild the BDD
image. The unfiltered full run remains `./scripts/bdd-image.sh run`.

To publish the builder image from a machine that is allowed to push:

```sh
export BDD_BUILDER_PUSH=1
./scripts/bdd-image.sh build-builder
```

`BDD_CACHE_FROM` and `BDD_CACHE_TO` can still point to a separate BuildKit
registry cache. The default path relies on the builder image itself and avoids a
second upload of the same Nix store.

## Full Local Run With Local Nix

If Nix is already installed on the host:

```sh
nix build .#image --out-link result-bdd-image
./result-bdd-image | docker load
docker run --rm pgkronika-bdd:latest
```

Remove `result-bdd-image` when done.

## CI Path

The GitHub Actions workflow has two BDD jobs:

- `bdd image` builds or pulls the BDD builder, then builds the runtime image;
- `bdd matrix` runs the already built image.

For same-repository runs, the builder image is stored in GHCR. The builder tag is
based on the dependency key and platform, so edits in `src/` do not rebuild the
Rust/PostgreSQL dependency layer. The final runtime image is still tagged by
content; if that exact image already exists, the job skips the build before
cleaning disk space.

Fork pull requests do not push to GHCR. They build the builder locally and pass
the runtime image to `bdd matrix` as a short-lived artifact.

The same script works in GitLab CI. A minimal job looks like this:

```yaml
bdd:
  image: docker:29
  services:
    - docker:29-dind
  variables:
    DOCKER_TLS_CERTDIR: ""
    BDD_IMAGE_PREFIX: "$CI_REGISTRY_IMAGE"
    BDD_PLATFORM: linux/amd64
  before_script:
    - docker login -u "$CI_REGISTRY_USER" -p "$CI_REGISTRY_PASSWORD" "$CI_REGISTRY"
    - docker buildx create --use
  script:
    - platform_slug=$(./scripts/bdd-image.sh platform-slug)
    - export BDD_BUILDER_PULL=1 BDD_BUILDER_PUSH=1
    - export BDD_RUNTIME_IMAGE="${BDD_IMAGE_PREFIX}/pgkronika-bdd:${platform_slug}-sha-$(./scripts/bdd-image.sh image-key | cut -c1-16)"
    - ./scripts/bdd-image.sh build-builder
    - BDD_RUNTIME_PUSH=1 ./scripts/bdd-image.sh build-runtime
    - ./scripts/bdd-image.sh run "$BDD_RUNTIME_IMAGE"
```

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
