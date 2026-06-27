# kronika-bdd

[Русская версия](README.ru.md)

`kronika-bdd` is the BDD runner for PostgreSQL integration scenarios. PostgreSQL
15, 16, and 17 are provided by Nix, booted in parallel, and queried through
`tokio-postgres`. It checks the infrastructure itself, runs the `source-pg`
collector live against every version, and drives the `pg_kronika-collector`
binary end to end into a sealed segment.

## What It Runs

`features/smoke.feature` proves the infrastructure itself:

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

`features/collector.feature` runs the `source-pg` collector against the live
matrix:

```gherkin
Scenario: every version yields a plausible bgwriter/checkpointer snapshot
  Given the PostgreSQL matrix is booted
  Then every version reports plausible bgwriter/checkpointer stats
```

For each version it calls `collect_bgwriter_checkpointer` (registry type
`1_006_001`) and checks that:

- the row carries the timestamp the caller passed;
- counters are non-negative and `bgwriter_stats_reset` is a real instant no
  later than collection;
- the filled and `NULL` columns match the version — PG17+ fills the restartpoint
  and checkpointer-reset columns and drops `buffers_backend`, while earlier
  versions do the reverse.

This is the live guard on the collector's version dispatch: a query that no
longer matches a server's catalog fails here, not in production.

`features/collector.feature` also drives the collector binary end to end:

```gherkin
Scenario: every version seals a readable segment with section 1_006_001
  Given the PostgreSQL matrix is booted
  Then every version is collected into a sealed segment with section 1_006_001
```

For each version the runner spawns `pg_kronika-collector` (path from
`KRONIKA_COLLECTOR_BIN`) against the cluster, waits for its `ready` line, sends
`SIGUSR2`, and reads back the `sealed <path>` it prints. It then opens that
segment with `kronika-reader` and asserts section `1_006_001` decodes to exactly
the one snapshot row, with the segment's time range pinned to that snapshot. This
exercises the whole write/read loop — collect, seal, read — in one check.

## Quick Local Check

This runs only the host-safe unit tests. It does not start PostgreSQL:

```sh
cargo test -p kronika-bdd
```

Use this for parser and harness-code changes. It is not the full matrix test.

## Full Local Run With Docker

This is the same path CI uses, but written safely for a developer checkout. It
does not require Nix on the host; Nix runs inside a pinned `nixos/nix` image.

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
third runs the matrix.

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
- `server_version` mismatch: the process answered, but not as the expected
  PostgreSQL major version.
- `collect type 1_006_001 ...` or `postgres NN: ...` from the collector
  scenario: the query did not match the server's catalog, or the snapshot was
  implausible. The message names the column or version dispatch that disagreed.
