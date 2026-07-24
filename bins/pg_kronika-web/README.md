# pg_kronika-web

[Русская версия](README.ru.md)

`pg_kronika-web` serves a local PGM directory through an embedded UI, JSON API,
and Prometheus endpoint. It opens sealed segments and valid `active.parts`
frames through `LocalDirSnapshot`, maintains a source-scoped timeline index,
refreshes the published store view every second, and never connects to
PostgreSQL. One retained writer folds journal deltas, promotes exactly matched
sealed segments, and atomically publishes immutable timeline views.

## Configuration

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_WEB_DIR` | required | Directory containing `.pgm` files and optional `active.parts`. |
| `KRONIKA_WEB_ADDR` | required | Listen address in `host:port` form. |
| `KRONIKA_WEB_BASIC_AUTH` | unset | `user:password`; when absent, UI and `/v1/*` are open. |
| `KRONIKA_WEB_STALE_AFTER_S` | `10` | `/readyz` returns `503` when the last successful refresh is older than this. |
| `KRONIKA_WEB_LOG` | `info` | `tracing-subscriber` filter directive. |
| `KRONIKA_WEB_OVERVIEW_CACHE_DIR` | `<KRONIKA_WEB_DIR>/.pgkronika-overview-cache` | Durable per-segment timeline fact cache. |
| `KRONIKA_WEB_OVERVIEW_NAMESPACE` | canonical store-path bytes | Stable store/deployment identity used in timeline fact keys. |
| `KRONIKA_WEB_OVERVIEW_FALLBACK_SEGMENT_HOURS` | `24` | Total admitted segment-hours retained after recoverable durable-publication failures. |
| `KRONIKA_WEB_OVERVIEW_FALLBACK_BYTES` | `67108864` | Canonical fact-byte budget for the process-local fallback. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_BYTES` | `67108864` | Logical-byte budget for the serialized overview/health response cache. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_ENTRIES` | `4096` | Serialized overview/health response-cache entry budget. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_VIEWS` | `64` | Maximum event views pinned for cursor continuation. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_BYTES` | `536870912` | Logical-byte budget for cursor-pinned event views. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_TTL_S` | `300` | Cursor and pinned-view lifetime in seconds. |

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

Timeline resource policy defaults and constraints are:

| Resource | Default | Constraint or ceiling |
| --- | ---: | ---: |
| Recoverable durable-publication fallback | 24 segment-hours, 64 MiB | 744 hours, 256 MiB |
| Serialized overview/health response cache | 4,096 entries, 64 MiB logical charge | Both configured budgets are nonzero and fit `usize`. |
| Cursor-pinned event views | 64 views, 512 MiB logical charge, 300 s TTL | All budgets are nonzero; count and bytes fit `usize`. |
| Timeline query range | — | 31 days |
| Materialized timeline query | — | 64 MiB cloned-observation charge; 1,048,576 observations/count inputs, 262,144 clipped coverage spans, 65,536 joint keys, 1,024 signal keys |
| Events page | 100 items | 1,000 items |
| Notable preview | 100 items | Fixed by notable policy v1 |
| Health line | — | 2,000 points |

All seven numeric `KRONIKA_WEB_OVERVIEW_*` budget variables accept nonzero
unsigned decimal integers. Byte, entry, and view budgets must fit the
platform's `usize`. The fallback additionally rejects values above 744
segment-hours or 268435456 bytes. Invalid values stop startup before the
listener binds.

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
| `GET /v1/timeline/overview` | exactly one `source`, `from`, `to` | Returns a source-scoped event digest, bounded notable preview, health summary, coverage, freshness, completeness, exactness, count semantics, and known loss. |
| `GET /v1/timeline/events` | one or more repeatable `source`, `from`, `to`; optional `limit`, `cursor`, `min_severity`, `kind` | Returns a stable page of typed notable event facts and an opaque cursor when more events remain. |
| `GET /v1/timeline/health` | exactly one `source`, `from`, `to`; optional integer-microsecond `step` | Returns at most 2,000 policy-evaluated health points plus coverage and the effective step. |
| `GET /v1/anomalies` | `source`, `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `limit`, `section` | Finds intervals where counter rates or gauge values changed unusually during the selected period. It returns the affected series, metric, interval, direction, and peak statistics; ranks episodes by `abs(peak.m)`; and reports per-section evaluation counts plus any skipped sections. |
| `GET /v1/incidents` | `source`, `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section` | Groups anomaly episodes that are close in time into incident candidates. It returns findings and machine-readable evidence where the inputs support them, plus coverage, data quality, catalog state, and skipped work. |
| `GET /` | none | Opens the embedded browser UI over the same local snapshot. |

`source` is the unsigned id returned by `/v1/sources`. `from` and `to` are
signed Unix timestamps in microseconds. Duration parameters accept `250ms`,
`90s`, `15m`, `2h`, or bare seconds. Row endpoints return 1,000 rows by default
and clamp `limit` to 10,000. Treat a cursor as opaque and pass it back unchanged
on the next request.

Timeline `from`/`to` ranges are half-open and limited to 31 days. Overview and
health reject missing or repeated `source`; events canonicalizes a repeatable
source set by sorting and deduplicating it. Timeline health `step` is an integer
number of microseconds and is raised when necessary to keep the result within
2,000 points. Events pages default to 100 facts and never exceed 1,000. An
invalid or query-mismatched event cursor returns `400`. Expired and post-restart
cursors return `410` with `code=cursor_expired`; an evicted or otherwise absent
pinned view returns `410` with `code=view_gone`. Registry capacity failure returns
`503` with `code=cursor_capacity_unavailable` and no `Retry-After`.

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
- Timeline facts remain isolated by source. The overview preview and event
  pages use the same typed `EventFact` projection: semantic `event_id`,
  provenance-bound `event_instance_id`, source and time fields, notable and
  evidence classes, quality flags, a typed payload, supporting evidence, and
  attached loss. Pagination order is exactly `(sort_ts_us, event_id,
  event_instance_id)`.
- Event counts use checked arithmetic. Severity and category totals,
  SQLSTATE top/other/missing buckets, and joint top/other buckets independently
  reconcile to retained error occurrences; retained groups and physical
  observation rows are separate counts. Retained exactness, source
  completeness, physical-count semantics, freshness, and known loss remain
  independent response fields.
- Durable lineage-qualified fact files are always consulted before the bounded
  process-local fallback. Only a recoverable publication failure may populate
  that fallback. Exact overview/health response caching is bounded by entry
  count and bytes. Event cursors pin an exact immutable view in a count-, byte-,
  and TTL-bounded registry and bind the canonical source set, query, policy,
  and last sort position with a process-local OS-random authentication key.
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
- Cross-section lock evidence requires an explicit producer-stored shared
  observation token and an exact `(snapshot timestamp, PID, backend_start)`
  match. Equal timestamps do not prove the relation. The current activity and
  lock collectors use separate statements, so `cross_section_entity_join`
  remains unavailable until the producer stores that token.
- Product-owned incomplete-result explanations use the closed
  `{ "kind": "...", "params": { ... } }` reason schema. Lens ids, enum values,
  formulas, units, and evidence remain stable machine data; incident catalogs
  contain no localized title or question.
- Only one anomaly, incident, or uncached timeline request runs at a time. Equal
  timeline misses share one single-flight build; cache hits do not consume
  the slot. Another distinct heavy request receives `503` with
  `code=analytic_capacity_unavailable` and `Retry-After: 1`; it is not queued.

Store scan warnings and damaged journal regions remain available to the reader
and affect gaps/completeness. They are never converted to successful rows.

## Timeline metrics

`/metrics` exposes timeline publication gauges/counters
`kronika_web_overview_durable_hits_total`,
`kronika_web_overview_fallback_hits_total`,
`kronika_web_overview_rebuilt_total`,
`kronika_web_overview_promotions_total`,
`kronika_web_overview_persistence_failures_total`,
`kronika_web_overview_sealed_failures_total`,
`kronika_web_store_view_generation`,
`kronika_web_overview_view_generation`,
`kronika_web_overview_data_through_us`, and
`kronika_web_overview_refresh_errors_total`. Cursor pressure is visible through
`kronika_web_timeline_cursor_views`, `kronika_web_timeline_cursor_bytes`, and
`kronika_web_timeline_cursor_pins_total`,
`kronika_web_timeline_cursor_resolves_total`,
`kronika_web_timeline_cursor_evictions_total`,
`kronika_web_timeline_cursor_expired_total`, and
`kronika_web_timeline_cursor_capacity_rejections_total`. Response-cache and
single-flight activity use
`kronika_web_timeline_response_cache_{hits,misses,evictions}_total`,
`kronika_web_timeline_response_cache_{entries,bytes}`, and
`kronika_web_timeline_singleflight_{leaders,joins}_total`. HTTP request labels
use fixed matched route templates rather than raw URIs.

## Shutdown and failure behavior

`SIGTERM` and `SIGINT` start graceful HTTP shutdown. The refresh task reports
scan errors and keeps the last published view; `/readyz` becomes stale once the
configured threshold is exceeded. A successful store scan followed by a
timeline-build failure publishes the fresh metadata together with the last
usable timeline and never exposes a partially built timeline. A bad
environment, initial store/overview failure, or unavailable OS entropy for
cursor authentication exits before binding the listener.

The binary has no CLI flags and does not implement MCP, remote stores,
retention, or alert delivery.
