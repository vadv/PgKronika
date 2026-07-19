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

| Endpoint | Parameters | Result |
| --- | --- | --- |
| `GET /healthz` | none | Process liveness. |
| `GET /readyz` | none | Snapshot refresh readiness and age. |
| `GET /metrics` | none | Prometheus text format, including reader, data-age, request, RSS, and fd metrics. |
| `GET /v1/version` | none | API and PGM format versions. |
| `GET /v1/sources` | none | Source ids, time spans, and unit counts. |
| `GET /v1/sections` | none | Logical registry sections and union schemas. |
| `GET /v1/segments` | `source`, `from`, `to` | Overlapping segment catalogs without body decode. |
| `GET /v1/section/{name}` | `source`, `from`, `to`; optional `limit`, `cursor` | Ordered rows, gaps, and pagination cursor. |
| `GET /v1/sections/batch` | `source`, `from`, `to`, comma-separated `names`; optional `limit` | Several logical sections from one segment pass. |
| `GET /v1/section/{name}/diff` | `source`, `from`, `to` | Per-identity counter deltas/rates and no-data reasons. |
| `GET /v1/sections/batch/diff` | `source`, `from`, `to`, comma-separated `names` | Several diff results from one pass. |
| `GET /v1/anomalies` | `source`, `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `limit`, `section` | Ranked robust-score episodes with data-quality counts. |
| `GET /v1/incidents` | `source`, `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section` | Clusters related anomaly episodes. Diagnostic findings are not available. |
| `GET /` | none | Embedded UI. |

`source` is an unsigned catalog source id. `from` and `to` are signed Unix
microseconds. Duration parameters accept `250ms`, `90s`, `15m`, `2h`, or bare
seconds. Row endpoints default to 1,000 rows and clamp `limit` to 10,000. A
cursor is opaque and must be returned unchanged on the next request.

Example:

```sh
curl -u operator:change-me \
  'http://127.0.0.1:8688/v1/segments?source=1&from=0&to=9223372036854775807'
```

Errors use `{ "error": "code", "detail": "message" }` where a detail is
useful. Unknown sections return `404`; malformed parameters return `400`; a
request exceeding the materialization ceiling returns `413`.

## Query and analysis contracts

- Section reads scan only overlapping units, verify PGM and section CRC before
  decode, union registered layout versions by logical name, and sort by the
  registry key. Exact sealed/live duplicates are suppressed.
- Diff returns a measured zero only when a counter did not change. Reset, gap,
  first point, invalid timestamp/scalar, and collection-disabled intervals are
  distinct no-data reasons.
- Anomaly scoring uses source-independent robust window statistics. Missing or
  discontinuous data is counted as not evaluated; it is not replaced by zero.
- Incident requests are limited to a 24-hour span and fixed ceilings for
  units, sections, materialized cells, series points, identity bytes, scoring
  work, and episodes.
- Only one anomaly or incident request runs at a time. A concurrent heavy
  request gets `503` with `Retry-After: 1`; it is not queued.
- `/v1/incidents` currently clusters episodes. Response `complete` remains
  `false`, findings are empty, and the diagnostic lens catalog is dormant.

Store scan warnings and damaged journal regions remain available to the reader
and affect gaps/completeness. They are never converted to successful rows.

## Shutdown and failure behavior

`SIGTERM` and `SIGINT` start graceful HTTP shutdown. The refresh task reports
scan errors and keeps the last published snapshot; `/readyz` becomes stale once
the configured threshold is exceeded. A bad environment or initial store-open
error exits before binding the listener.

The binary has no CLI flags and does not implement MCP, remote stores,
retention, or alert delivery.
