# Manual Demo API Smoke Workflow Design

## Status

Approved on 2026-07-29.

## Goal

Provide an on-demand GitHub Actions workflow that builds the selected
PgKronika revision, starts an isolated demo stand on the hosted runner, waits
for ordinary segment rotation, and runs the existing evidence-bearing
Schemathesis smoke against all documented web API operations.

The workflow is diagnostic evidence for a selected revision. It is not a
required pull-request check and must never run automatically for `push` or
`pull_request`.

## Decisions

- Use a dedicated `.github/workflows/demo-api-smoke.yml`.
- Expose only the `workflow_dispatch` trigger.
- Test the branch or commit selected in the standard GitHub Actions workflow
  selector.
- Build and run the demo on the GitHub runner instead of calling an external
  environment.
- Keep the ordinary collector settings. In particular,
  `KRONIKA_SEGMENT_MAX_AGE_S` remains at its 900-second default.
- Wait at most 1200 seconds for retained timestamps to span at least 900
  seconds. Early segments closed by size or row limits do not make timeline
  evidence ready.
- Keep local `make demo-api-smoke` immediate by default. CI opts into waiting
  with `DEMO_API_WAIT_SECONDS=1200`.
- Run at most one manual demo smoke at a time.
- Retain diagnostics from successful and failed runs.

## Alternatives

### Add `workflow_dispatch` to the main CI workflow

This would make the existing CI workflow manually runnable, but a dispatch
would either run unrelated jobs or require event conditions across the whole
workflow. It also makes the expensive diagnostic smoke less visible as a
separate operation.

### Use an external demo URL

This starts faster, but the result is no longer bound to the selected source
revision. It also introduces networking, availability, authentication, and
secret-management dependencies.

### Add a reusable workflow and a dispatch wrapper

There is currently one caller. Splitting the implementation into two workflow
files would add indirection without reuse.

## Workflow

The manual job runs on `ubuntu-latest` with a 60-minute job timeout. The
20-minute limit applies only to retained-data discovery; image resolution,
source compilation, stand startup, smoke execution, graceful shutdown, and
artifact upload need separate time.

The steps are:

1. Check out the exact revision selected for the workflow dispatch.
2. Calculate the dependency-keyed BDD builder image name using
   `scripts/bdd-image.sh`.
3. Pull the exact builder from GHCR, or build and publish it when the key is
   missing. This follows the existing `bdd-matrix` cache contract.
4. Install `uv`, which supplies `uvx` for the pinned Schemathesis invocation.
5. Run `make demo-build`.
6. Run `make demo-up` with ordinary demo and collector settings.
7. Run `make demo-api-smoke` against `http://127.0.0.1:18081` with
   `DEMO_API_WAIT_SECONDS=1200`.
8. Always stop the demo gracefully, collect logs and the generated report, and
   upload an explicit diagnostic-file allowlist as an artifact.

The workflow requests `contents: read` and `packages: write`. Package write is
needed only when an exact dependency builder is absent; the demo runtime image
is local to the runner and is never published.

## Retained-Data Waiting

`scripts/demo-api-smoke.py` remains responsible for determining whether the
API has a useful real-data fixture. A new non-negative integer environment
variable, `DEMO_API_WAIT_SECONDS`, controls a bounded polling phase before the
existing preflight:

- absent or `0`: preserve the current immediate behavior;
- positive: query `/v1/segments` until its earliest and latest timestamps span
  at least 900 seconds;
- invalid or negative: fail immediately with a configuration error;
- deadline reached: fail with an error that reports the wait duration.

Polling uses the same base URL, authentication header, and all-time range as
the regular preflight. The interval is fixed at ten seconds. Once a segment is
visible, the existing preflight revalidates health, readiness, Swagger UI,
runtime OpenAPI operation count, usable time range, scorable section, and
catalog projection before invoking Schemathesis.

Only an absent or shorter-than-900-second retained range is retried. Broken
health endpoints, invalid JSON, an incorrect OpenAPI operation count, and
other contract failures remain immediate failures instead of being hidden
behind a 20-minute retry window.

## Evidence and Failure Handling

The Schemathesis output is written to both the Actions log and
`demo-data/api-smoke.log`. The existing `demo-down` path gracefully stops the
stand, seals the tail, and writes:

- `demo-data/stand.log`;
- `demo-data/container-live.log`;
- `demo-data/collector.log`;
- `demo-data/web.log`;
- `demo-data/report.json`, when shutdown reaches measurement.

Cleanup and artifact upload use `if: always()` so diagnostics survive build,
startup, wait, contract, or smoke failures. The artifact action receives only
these files and `api-smoke.log`; it never traverses root-owned `pgdata`,
tablespaces, segments, or other stand state. Missing optional files do not
hide the original failure.

## Testing

Python unit tests cover:

- the default zero wait;
- rejection of invalid and negative wait values;
- immediate success when retained data already exists;
- polling until data appears;
- timeout when data stays absent.

A repository test also checks the workflow's key safety properties: it has a
manual trigger, has no automatic trigger, applies the 1200-second wait, runs
the build/up/smoke lifecycle, and contains unconditional cleanup and artifact
upload.

The existing live `make demo-api-smoke` remains the end-to-end contract test.
The new GitHub workflow is itself the final hosted-runner validation and must
be invoked manually after the workflow file reaches the default branch.
GitHub does not dispatch a new `workflow_dispatch` workflow while its file
exists only on a feature branch.

## Documentation

The English and Russian web README files will describe both modes:

- local validation against an already running demo remains
  `make demo-api-smoke`;
- GitHub Actions provides `Demo API smoke`, manually selectable by revision,
  using ordinary rollover and a 20-minute retained-data deadline.
