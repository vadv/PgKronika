# pg_kronika-web

[Русская версия](README.ru.md)

`pg_kronika-web` serves a local PGM directory through an embedded UI, JSON API,
and Prometheus endpoint. It opens sealed segments and valid `active.parts`
frames through `LocalDirSnapshot`, refreshes the snapshot every second, and
never connects to PostgreSQL.

## Configuration

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_WEB_DIR` | required | Directory containing `.pgm` files and optional `active.parts`. |
| `KRONIKA_WEB_ADDR` | required | Listen address in `host:port` form. |
| `KRONIKA_WEB_BASIC_AUTH` | unset | `user:password`; when absent, UI and `/v1/*` are open. |
| `KRONIKA_WEB_STALE_AFTER_S` | `10` | `/readyz` returns `503` when the last successful refresh is older than this. |
| `KRONIKA_WEB_LOG` | `info` | `tracing-subscriber` filter directive. |

```sh
KRONIKA_WEB_DIR=/var/lib/pg_kronika \
KRONIKA_WEB_ADDR=127.0.0.1:8688 \
KRONIKA_WEB_BASIC_AUTH='operator:change-me' \
pg_kronika-web
```

The server has no TLS. Bind to loopback or use a TLS reverse proxy. Basic Auth
protects the embedded UI and `/v1/*`; `/healthz`, `/readyz`, and `/metrics` are
always public. Credentials are redacted from configuration errors and debug
output, but Basic Auth does not encrypt the connection.

## Endpoints

For an unfamiliar store, start with `/v1/sources`, `/v1/sections`, and
`/v1/segments`. They show what data exists before you request rows or run an
analysis.

| Endpoint | Parameters | What the operator gets |
| --- | --- | --- |
| `GET /healthz` | none | Confirms that the HTTP process is running. |
| `GET /readyz` | none | Tells a health checker whether the directory snapshot was refreshed recently and reports its age. |
| `GET /metrics` | none | Exposes Prometheus metrics for reader errors, data age, HTTP requests, RSS, and open file descriptors. |
| `GET /v1/version` | none | Identifies the JSON API version and the PGM format version served by this build. |
| `GET /v1/sources` | none | Lists the collector sources present in the store, with the earliest and latest timestamp and the segment count for each source. |
| `GET /v1/sections` | none | Shows which logical datasets can be queried and gives each dataset's semantics, sort key, and union of registered columns. |
| `GET /v1/segments` | `source`, `from`, `to` | Shows which segments overlap the requested period and how many rows each section contains. It reads catalog metadata, not section bodies. |
| `GET /v1/section/{name}` | `source`, `from`, `to`; optional `limit`, `cursor` | Returns the selected dataset as time-ordered rows. The response also names unreadable or missing intervals in `gaps` and supplies `next_cursor` when more rows remain. |
| `GET /v1/sections/batch` | `source`, `from`, `to`, comma-separated `names`; optional `limit` | Returns the same row pages for several datasets, keyed by section name, after one pass over the overlapping segments. |
| `GET /v1/section/{name}/diff` | `source`, `from`, `to` | Turns cumulative counters into per-identity changes and per-second rates. Each point contains `delta`, `rate`, and `dt_micros`, or a `nodata` reason when no honest rate can be computed. |
| `GET /v1/sections/batch/diff` | `source`, `from`, `to`, comma-separated `names` | Returns the same counter-change view for several datasets, keyed by section name, after one segment pass. |
| `GET /v1/anomalies` | `source`, `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `limit`, `section` | Finds intervals where counter rates or gauge values changed unusually during the selected period. It returns the affected series, metric, interval, direction, and peak statistics; ranks episodes by `abs(peak.m)`; and reports per-section evaluation counts plus any skipped sections. |
| `GET /v1/incidents` | `source`, `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section` | Groups anomaly episodes that are close in time into incident candidates. It returns findings and machine-readable evidence where the inputs support them, plus coverage, data quality, catalog state, and skipped work. |
| `GET /` | none | Opens the embedded browser UI over the same local snapshot. |

`source` is the unsigned id returned by `/v1/sources`. `from` and `to` are
signed Unix timestamps in microseconds. Duration parameters accept `250ms`,
`90s`, `15m`, `2h`, or bare seconds. Row endpoints return 1,000 rows by default
and clamp `limit` to 10,000. Treat a cursor as opaque and pass it back unchanged
on the next request.

Example:

```sh
curl -u operator:change-me \
  'http://127.0.0.1:8688/v1/segments?source=1&from=0&to=9223372036854775807'
```

The success/data API is locale-neutral. `Accept-Language` does not change its
representations, and `/v1` sends neither `Content-Language` nor a language
`Vary`. Raw PostgreSQL, OS, and user strings remain literal; product-owned
labels and explanations belong to the UI.

Every `/v1` application error is an RFC 9457 Problem Details response with
media type `application/problem+json` and exactly `type`, `status`, `code`,
typed `params`, and an opaque `instance`. It has no human-language `title` or
`detail`. Problem responses use `Cache-Control: no-store` and expose the same
server-generated correlation token in `instance` and `X-Request-ID`.
`WWW-Authenticate`, `Allow`, and `Retry-After` remain present where HTTP
semantics require them. Unknown sections return `404`, malformed parameters
return `400`, and enforced input or materialization ceilings return `413`.
See the [OpenAPI contract](openapi.json) and the
[normative machine API specification](../../docs/superpowers/specs/2026-07-21-i18n-machine-api-contract.md).

## Query and analysis contracts

- Row queries read only overlapping segments, verify the PGM and section CRCs
  before decoding, combine registered layout versions under one logical section
  name, and sort by the registry key. Exact duplicates between sealed segments
  and `active.parts` appear only once.
- Diff responses distinguish a measured zero from a missing result. A point
  without a valid rate carries one of the response codes `reset`, `gap`,
  `first_point`, `anomaly`, or `not_collected`; `anomaly` here means that the
  timestamps did not advance or the scalar kinds were inconsistent.
- Anomaly search compares each current window with the other usable points in
  the selected period. The strongest absolute peak score appears first.
  `sections` reports evaluated and unevaluated window positions;
  `nodata_points` is an aggregate count, so the anomaly response does not split
  it into reset, gap, and collection-disabled totals. A window position that
  crosses a timeline break is counted under `not_evaluated.discontinuity`.
  Missing data is never replaced by zero.
- Incident clustering preserves more detail about incomplete input:
  `data_quality` has separate `resets`, `gaps`, and `not_collected` counts,
  `coverage_by_section` lists gap intervals, and `skipped` explains work omitted
  by a limit. Requests are limited to 24 hours and have fixed ceilings for
  units, sections, materialized cells, series points, identity bytes, scoring
  work, and episodes.
- Product-owned incomplete-result explanations use the closed
  `{ "kind": "...", "params": { ... } }` reason schema. Lens ids, enum values,
  formulas, units, and evidence remain stable machine data; incident catalogs
  contain no localized title or question.
- Only one anomaly or incident request runs at a time. A concurrent analysis
  request receives `503` with `Retry-After: 1`; it is not queued.

Store scan warnings and damaged journal regions remain available to the reader
and affect gaps/completeness. They are never converted to successful rows.

## Shutdown and failure behavior

`SIGTERM` and `SIGINT` start graceful HTTP shutdown. The refresh task reports
scan errors and keeps the last published snapshot; `/readyz` becomes stale once
the configured threshold is exceeded. A bad environment or initial store-open
error exits before binding the listener.

The binary has no CLI flags and does not implement MCP, remote stores,
retention, or alert delivery.
