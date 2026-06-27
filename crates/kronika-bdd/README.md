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

- the row carries the timestamp the caller passed;
- counters are non-negative and `bgwriter_stats_reset` is before collection;
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
segment with `kronika-reader` and asserts section `1_006_001` decodes to exactly
one snapshot row. The segment time range must match that row.

## Quick Local Check

This runs only unit tests. It does not start PostgreSQL:

```sh
cargo test -p kronika-bdd
```

Use this for `KRONIKA_PG_MATRIX` parsing and runner code. It is not the full
PostgreSQL run.

## Full Local Run With Docker

This follows the CI path without writing into the checkout. It does not require
Nix on the host; Nix runs inside a pinned `nixos/nix` image.

From the repository root:

```sh
export NIX_BUILD_IMAGE='docker.io/nixos/nix:2.31.2@sha256:29fc5fe207f159ceb0143c25c19c774062fee02ce5eda118f3067547b3054894'

docker run --rm \
  -v "$PWD":/work:ro \
  -e NIX_CONFIG='experimental-features = nix-command flakes' \
  "$NIX_BUILD_IMAGE" \
  sh -ceu '
    mkdir -p /tmp/src
    tar --exclude=.git --exclude=target --exclude=result --exclude=.direnv \
      -C /work -cf - . | tar -C /tmp/src -xf -
    cd /tmp/src
    nix build .#image --out-link /tmp/img
    /tmp/img
  ' > image.tar

docker load -i image.tar
docker run --rm pgkronika-bdd:latest
```

The first command builds the image tarball. The second loads it into Docker. The
third runs the PostgreSQL 15, 16, and 17 checks.

`image.tar` is only a local artifact; remove it when it is no longer needed.

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

- `bdd image` builds the Nix image once;
- `bdd matrix` runs the already built image.

For same-repository runs, `bdd image` pushes the image to GHCR under a
content-hash tag. If the tag already exists, the job skips the expensive build.
For fork pull requests, the image tarball is uploaded as a short-lived artifact
instead of being pushed to GHCR.

The content hash includes the flake files, Cargo lockfile, workspace manifests,
Rust toolchain pin, and BDD source/features. A change to any of those inputs
gets a new image tag.

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
