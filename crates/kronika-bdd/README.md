# kronika-bdd

[Русская версия](README.ru.md)

`kronika-bdd` runs PostgreSQL integration scenarios. Nix provides PostgreSQL
15, 16, and 17; the runner boots them in parallel and connects through
`tokio-postgres`. The suite checks the cluster setup, calls the `source-pg`
collector against every version, and runs `pg_kronika-collector` until it writes
a sealed segment.

## What It Runs

`features/smoke.feature` checks the cluster setup:

```gherkin
Scenario: every version is reachable
  Given the PostgreSQL matrix is booted
  Then every version answers a version query
```

For each PostgreSQL major version, the runner:

- creates a temporary data directory;
- runs `initdb` with trust auth, C locale, and no sync;
- starts `postgres` on a free loopback port;
- waits until the server accepts TCP connections;
- runs `SHOW server_version`;
- checks that the reported version matches the expected major version.

`features/collector.feature` calls the `source-pg` collector against the
running PostgreSQL versions:

```gherkin
Scenario: every version yields a valid bgwriter/checkpointer snapshot
  Given the PostgreSQL matrix is booted
  Then every version reports valid bgwriter/checkpointer stats
```

For each version it calls `collect_bgwriter_checkpointer` (registry type
`1_006_001`) and checks that:

- the row's `ts` is the server's `clock_timestamp()`, near the harness clock;
- counters are non-negative and `bgwriter_stats_reset` is before that `ts`;
- the filled and `NULL` columns match the version: PG17+ fills
  `restartpoints_*` and `checkpointer_stats_reset`, but leaves
  `buffers_backend` empty; earlier versions do the reverse.

This catches a stale query or wrong version branch in the BDD suite.

The same feature also starts the collector binary:

```gherkin
Scenario: every version seals a readable segment with section 1_006_001
  Given the PostgreSQL matrix is booted
  Then every version is collected into a sealed segment with section 1_006_001
```

For each version the runner spawns `pg_kronika-collector` (path from
`KRONIKA_COLLECTOR_BIN`) against the cluster, waits for its `ready` line, sends
`SIGUSR2`, and reads back the `sealed <path>` it prints. It then opens that
segment with `kronika-reader`, decodes section `1_006_001` typed, and asserts the
one row's `ts` equals the segment range and its PG17/pre-17 columns survived the
round-trip.

## Quick Local Check

This runs only unit tests. It does not start PostgreSQL:

```sh
cargo test -p kronika-bdd
```

Use this for `KRONIKA_PG_MATRIX` parsing and runner code. It is not the full
PostgreSQL run.

## Full Local Run With Docker

This path needs Docker with Buildx. Nix stays inside Docker, but it runs from a
builder image that keeps the Rust, Nix, and PostgreSQL dependency store.

From the repository root:

```sh
export BDD_IMAGE_PREFIX=ghcr.io/vadv/pgkronika
export BDD_BUILDER_PULL=1

./scripts/bdd-image.sh build-builder
./scripts/bdd-image.sh build-runtime
./scripts/bdd-image.sh run
```

`build-builder` pulls `pgkronika-bdd-builder` when the dependency key already
exists. Otherwise it builds the builder locally, using the registry BuildKit
cache when available. `build-runtime` uses that builder to create `image.tar`,
loads `pgkronika-bdd:latest` into Docker, and leaves the tarball in the working
tree.

The first builder build after a dependency change is still expensive. The point
of the builder image is to pay that cost once per dependency key and reuse it
for later source-only changes.

To publish the builder image from a machine that is allowed to push:

```sh
export BDD_BUILDER_PUSH=1
./scripts/bdd-image.sh build-builder
```

`BDD_CACHE_FROM` and `BDD_CACHE_TO` can still be set for a separate BuildKit
registry cache, but the default path relies on the builder image itself. That
avoids publishing the same large Nix store twice.

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
- `server_version` mismatch: the process answered, but not with the expected
  PostgreSQL major version.
- `collect type 1_006_001 ...` or `postgres NN: ...` from the collector
  scenario: the query did not match the server's catalog, or the snapshot was
  rejected by the checks. The message names the column or version branch.
