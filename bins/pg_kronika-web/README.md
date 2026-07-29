# pg_kronika-web

[Русская версия](README.ru.md)

`pg_kronika-web` serves a local PgKronika data root through an embedded UI,
JSON API, and Prometheus endpoint. It opens sealed segments from the
`YYYY/MM/DD` tree and valid root-level `active.parts` frames through
`LocalDirSnapshot`, maintains a source-scoped timeline index, refreshes the
published store view every second, and never connects to PostgreSQL. One
retained writer folds journal deltas, promotes exactly matched
sealed segments, and atomically publishes immutable descriptor and live
views. Sealed fact bodies are loaded only for admitted timeline requests.

## Configuration

| Variable | Default | Meaning |
| --- | ---: | --- |
| `KRONIKA_WEB_DIR` | required | PgKronika-owned data root containing `active.parts`, owner locks, and same-day `YYYY/MM/DD/N.pgm` plus optional `N.ovf`. |
| `KRONIKA_WEB_ADDR` | required | Listen address in `host:port` form. |
| `KRONIKA_WEB_BASIC_AUTH` | unset | `user:password`; when absent, UI and `/v1/*` are open. |
| `KRONIKA_WEB_STALE_AFTER_S` | `10` | `/readyz` returns `503` when the last successful refresh is older than this. |
| `KRONIKA_WEB_LOG` | `info` | `tracing-subscriber` filter directive. |
| `KRONIKA_WEB_OVERVIEW_FALLBACK_SEGMENT_HOURS` | `24` | Total admitted segment-hours retained after recoverable durable-publication failures. |
| `KRONIKA_WEB_OVERVIEW_FALLBACK_BYTES` | `67108864` | Canonical fact-byte budget for the process-local fallback. |
| `KRONIKA_WEB_OVERVIEW_GC_MAX_ENTRIES` | `100000` | Maximum entries in one complete data-directory inventory; reaching the bound forbids a sweep. |
| `KRONIKA_WEB_OVERVIEW_GC_GRACE_GENERATIONS` | `2` | Distinct authoritative GC generations required before a non-live final is eligible. |
| `KRONIKA_WEB_OVERVIEW_GC_WALL_GRACE_S` | `120` | Minimum seconds since the first authoritative non-live observation. |
| `KRONIKA_WEB_OVERVIEW_GC_ARTIFACT_GRACE_S` | `600` | Minimum age of a recognized publication temporary file before cleanup. |
| `KRONIKA_WEB_OVERVIEW_CACHE_MAX_LOGICAL_BYTES` | unset | Optional logical-`st_size` ceiling for recognized derived sidecars and publication artifacts. |
| `KRONIKA_WEB_OVERVIEW_CACHE_MAX_FILES` | unset | Optional file-count ceiling for recognized derived sidecars and publication artifacts. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_BYTES` | `67108864` | Logical-byte budget for the serialized overview/health response cache. |
| `KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_ENTRIES` | `4096` | Serialized overview/health response-cache entry budget. |
| `KRONIKA_WEB_OVERVIEW_DECODED_CACHE_BYTES` | `268435456` | Logical resident-byte budget for decoded sealed facts retained in memory. |
| `KRONIKA_WEB_OVERVIEW_DECODED_CACHE_ENTRIES` | `4096` | Entry budget for decoded sealed facts retained in memory. |
| `KRONIKA_WEB_OVERVIEW_SOURCE_SCRUB_INTERVAL_S` | `60` | Seconds between streaming CRC scrubs; each due scrub checks one sealed section. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_VIEWS` | `64` | Maximum event views pinned for cursor continuation. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_MAX_BYTES` | `536870912` | Logical-byte budget for cursor-pinned event views. |
| `KRONIKA_WEB_OVERVIEW_CURSOR_TTL_S` | `300` | Cursor and pinned-view lifetime in seconds. |
| `KRONIKA_WEB_OVERVIEW_MAX_SELECTED_SEGMENTS` | `1024` | Effective sealed-segment admission limit for one timeline request; accepted range `1..=4096`. |
| `KRONIKA_WEB_OVERVIEW_COLD_MAX_WORKERS` | `4` | Maximum active sealed-fact workers. |
| `KRONIKA_WEB_OVERVIEW_COLD_MAX_QUEUE` | `64` | Maximum queued exact sealed-fact builds. |
| `KRONIKA_WEB_OVERVIEW_COLD_PER_REQUEST_PARALLELISM` | `4` | Maximum cold loads started concurrently by one request. |
| `KRONIKA_WEB_OVERVIEW_COLD_WAIT_TIMEOUT_MS` | `5000` | Maximum FIFO wait before a cold build is rejected. |
| `KRONIKA_WEB_OVERVIEW_COLD_RETRY_AFTER_S` | `1` | `Retry-After` value for cold-build overload responses. |
| `KRONIKA_WEB_OVERVIEW_COLD_PGM_BYTES` | `1073741824` | Aggregate source-PGM byte capacity for active cold work. |
| `KRONIKA_WEB_OVERVIEW_COLD_DECODED_BYTES` | `1073741824` | Aggregate decoded working-set byte capacity for active cold work. |
| `KRONIKA_WEB_OVERVIEW_COLD_CPU_ROWS` | `2097152` | Aggregate source-row CPU charge for active cold work. |
| `KRONIKA_WEB_OVERVIEW_COLD_FILE_DESCRIPTORS` | `16` | Aggregate file-descriptor reservation for active cold work. |
| `KRONIKA_WEB_OVERVIEW_COLD_READ_BYTES` | `1073741824` | Aggregate PGM and sidecar read capacity for active cold work. |
| `KRONIKA_WEB_OVERVIEW_COLD_WRITE_BYTES` | `1073741824` | Aggregate sidecar write capacity for active cold work. |
| `KRONIKA_WEB_OVERVIEW_COLD_PUBLICATIONS` | `4` | Aggregate durable-publication capacity for active cold work. |

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
| Data-directory GC inventory | 100,000 entries | 1,000,000 entries |
| Non-live final grace | 2 distinct authoritative GC generations and 120 s | Both values must be nonzero; generation grace must be at least 2 |
| Recognized publication-artifact grace | 600 s | Must be nonzero |
| Derived sidecar admission | No byte or file ceiling by default | Optional nonzero logical-byte and file-count ceilings |
| Serialized overview/health response cache | 4,096 entries, 64 MiB logical charge | Both configured budgets are nonzero and fit `usize`. |
| Decoded sealed facts in memory | 4,096 entries, 256 MiB logical resident charge | Exact hits bypass source-build admission; both configured budgets are nonzero and fit `usize`. |
| Streaming source scrub | One sealed section every 60 s | Interval is nonzero; CRC failure removes the source from the usable descriptor set. |
| Cursor-pinned event views | 64 views, 512 MiB logical charge, 300 s TTL | All budgets are nonzero; count and bytes fit `usize`. |
| Selected sealed segments per timeline request | 1,024 | Configurable from 1 through the absolute v1 ceiling of 4,096 |
| Cold sealed-fact scheduler | 4 workers, FIFO queue 64, per-request parallelism 4, wait 5 s | All limits are nonzero; queue rejection, overweight work, and timeout return an operator-configured retry hint. |
| Cold weighted capacity | 1 GiB each PGM/decoded/read/write, 2,097,152 rows, 16 file descriptors, 4 publications | Values are process-wide aggregate ceilings; byte and row charges round up to fixed scheduler quanta. |
| Timeline query range | — | 31 days |
| Materialized timeline query | — | 64 MiB cloned-observation charge; 1,048,576 observations/count inputs, 262,144 clipped coverage spans, 65,536 joint keys, 1,024 signal keys |
| Anomaly projection | 50 results per evidence class | 10,000 window positions; 50,000,000 charged timeline-unit/position pairs; 10,000 generic episodes and 10,000 plan signals independently; 262,144 rows, 10,000,000 cells, and 64 MiB owned variable-width payload per section page |
| Events page | 100 items | 1,000 items |
| Notable preview | 100 items | Fixed by notable policy v1 |
| Health line | — | 2,000 points |

Numeric `KRONIKA_WEB_OVERVIEW_*` policy variables accept unsigned decimal integers.
Required budgets, intervals, queue sizes, and weighted capacities must be
nonzero; either derived-sidecar ceiling may remain unset. Byte, entry, queue,
and view budgets that become process sizes must fit the platform's `usize`.
The fallback additionally rejects values above 744 segment-hours or 268435456
bytes. The selected-segment limit must be in `1..=4096`. Invalid values stop
startup before the listener binds.

## Sibling fact sidecars

`KRONIKA_WEB_DIR` is one exclusively PgKronika-owned data root and requires no
additional storage address or identifier. A sealed segment and its derived
facts have the same stem and UTC day:

```text
/data/active.parts
/data/.pgkronika-overview.owner.lock
/data/YYYY/MM/DD/N.pgm
/data/YYYY/MM/DD/N.ovf
```

`N` is the `SegmentId`, the Unix timestamp in microseconds of the first
collection window successfully appended to the source segment. The UTC day is
derived from that id, not from the query range or file timestamps. A segment
that crosses midnight remains in its starting-day directory.

The first `FactStore` to acquire the root-level
`.pgkronika-overview.owner.lock` holds the only OVF mutation right for the data
root during its lifetime. Other processes using the same root may read
admitted sidecars; publication and GC report contention, and newly built facts
remain in the bounded process-local fallback.

Every inventory is a strict bounded traversal. Root-level PGM/OVF files,
symbolic links, unknown entries, malformed dates, and a segment stored under
the wrong UTC day fail the inventory instead of producing a partial view. This
is the first supported layout in the unreleased project.

The web writer requests GC after every 60 successful timeline publications.
The generation grace advances only on distinct, complete, authoritative GC
scans, not on ordinary refreshes. With the defaults, deletion also requires
120 seconds since the first scan that found a final non-live; the wall grace
can therefore require a later scan. Any unavailable sealed source, scan error,
or entry-cap hit authorizes no deletion and does not advance grace. GC scans
the owned tree directly with a hard entry bound. It admits a same-stem
`.ovf` only after validating the PGKOVF header against its PGM, never follows
symlinks, and never removes PGM, `active.parts`, or the owner lock.

The optional derived-file ceilings count recognized sidecar and publication
artifact sizes and entries;
they are not free-space limits or physical filesystem quotas. If a complete
scan cannot admit a publication without exceeding a configured ceiling, the
response still uses the bounded in-memory fallback. `ENOSPC` and configured
quota failures receive at most one authoritative GC pass and one publication
retry.

Durable reads continue while writes are backed off. A refresh with no new fact
build still runs the single due recovery probe. Permission and read-only
failures wait five minutes before the first probe. Capacity and transient I/O
use per-store jittered exponential delay capped at five minutes. Permanent
path, sidecar-state, identity, and unclassified I/O failures are reported
without arming global backoff.

## Endpoints

For an unfamiliar store, start with `/v1/ui/catalog`, `/v1/views/summary`,
`/v1/sections`, and `/v1/segments`. They show what data exists before you
request rows or run an analysis.

| Endpoint | Parameters | What the operator gets |
| --- | --- | --- |
| `GET /healthz` | none | Confirms that the HTTP process is running. |
| `GET /readyz` | none | Tells a health checker whether the directory snapshot was refreshed recently and reports its age. |
| `GET /metrics` | none | Exposes Prometheus metrics for reader errors, data age, HTTP requests, RSS, and open file descriptors. |
| `GET /v1/version` | none | Identifies the JSON API version and the PGM format version served by this build. |
| `GET /v1/ui/catalog` | optional `If-None-Match` header | Returns the nine stable UI views with inputs, joins, metric formulas, columns, presets, and availability. It reads PGM catalog metadata only, returns a strong ETag, and answers a matching validator with `304`. |
| `GET /v1/views/summary` | `at` | Returns the latest exact population, status, and notable state at or before the cursor for all nine UI views. Sealed data reads only the shared `UiSummary` OVF block; a current active tail is merged from memory. |
| `GET /v1/timeline/heatmap` | `view`, `metric`, `from`, `to`; optional `buckets`, `top` | Merges the selected view's local top-K series into a bounded heatmap with score bounds and an exact-ranking proof. The half-open range is limited to 24 hours, `buckets` to `1..=256`, `top` to `1..=64`, and the serialized response to 512 KiB. Sealed requests read only `EntitySeries(view)` and never a PGM body. |
| `GET /v1/sections` | none | Shows which logical datasets can be queried and gives each dataset's semantics, sort key, and union of registered columns. |
| `GET /v1/segments` | `from`, `to` | Shows which segments overlap the requested period and how many rows each section contains. It reads catalog metadata, not section bodies. |
| `GET /v1/section/{name}` | `from`, `to`; optional `limit`, `cursor` | Returns the selected dataset as time-ordered rows. The response also names unreadable or missing intervals in `gaps` and supplies `next_cursor` when more rows remain. |
| `GET /v1/sections/batch` | `from`, `to`, comma-separated `names`; optional `limit` | Returns the same row pages for several datasets, keyed by section name, after one pass over the overlapping segments. |
| `GET /v1/section/{name}/diff` | `from`, `to` | Turns cumulative counters into per-identity changes and per-second rates. Each point contains `delta`, `rate`, and `dt_micros`, or a `nodata` reason when no honest rate can be computed. |
| `GET /v1/sections/batch/diff` | `from`, `to`, comma-separated `names` | Returns the same counter-change view for several datasets, keyed by section name, after one segment pass. |
| `GET /v1/timeline/overview` | `from`, `to` | Returns an event digest, bounded notable preview, health summary, coverage, freshness, completeness, exactness, count semantics, and known loss. |
| `GET /v1/timeline/events` | `from`, `to`; optional `limit`, `cursor`, `min_severity`, `kind` | Returns a stable page of typed notable event facts and an opaque cursor when more events remain. |
| `GET /v1/timeline/health` | `from`, `to`; optional integer-microsecond `step` | Returns at most 2,000 policy-evaluated health points plus coverage and the effective step. |
| `GET /v1/anomalies` | `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `limit`, `section` | Finds unusual rate or gauge intervals and, for stored plans, call-normalized plan-mixture changes and same-plan buffer-work increases. It returns ranked `episodes`, ranked `plan_signals`, per-section evaluation counts, plan applicability and quality, coverage, truncation, and skipped work. |
| `GET /v1/incidents` | `from`, `to`; optional `window`, `step`, `threshold`, `eps_rel`, `epsilon`, `max_cluster_span`, `section` | Groups anomaly episodes that are close in time into incident candidates. It returns findings and machine-readable evidence where the inputs support them, plus coverage, data quality, catalog state, and skipped work. |
| `GET /` | none | Opens the embedded browser UI over the same local snapshot. |

`from` and `to` are signed Unix timestamps in microseconds. Duration parameters
accept `250ms`, `90s`, `15m`, `2h`, or bare seconds. Row endpoints return 1,000
rows by default and clamp `limit` to 10,000. Treat a cursor as opaque and pass
it back unchanged on the next request.

The UI catalog uses the closed availability set `available`, `gated`,
`not_collected`, and `unsupported_type`. `processes.pss` remains
`not_collected` until the collector writes bounded `smaps_rollup`; Activity
CPU and I/O require both activity and process inputs. The serialized catalog
has a 512 KiB hard ceiling.

Heatmap values preserve the distinction between an absent sample (`null`) and
an observed zero (`0`). `ranking.exact` is true only when every returned lower
score bound beats all later and unseen upper bounds. Any incomplete block is
reported in the response quality and makes the proof inexact. Current
`active.parts` web-index blocks use the same merge path in memory.

Timeline `from`/`to` ranges are half-open and limited to 31 days. Timeline
health `step` is an integer number of microseconds and is raised when necessary
to keep the result within 2,000 points. Before response-cache lookup,
response-flight registration, analytic admission, or a new cursor pin, each
first-page request plans the intersecting sealed descriptors. More than the
configured effective limit returns `400` with `code=query_limit_exceeded` and
`params.resource=selected_segments`. Live journal data is not charged as a
sealed segment and remains subject to its separate bounds.
Events pages default to 100 facts and never exceed 1,000. An invalid or
query-mismatched event cursor returns `400`. Expired and post-restart cursors
return `410` with `code=cursor_expired`; an evicted or otherwise absent pinned
view returns `410` with `code=view_gone`. Registry capacity failure returns
`503` with `code=cursor_capacity_unavailable` and no `Retry-After`.

Example:

```sh
curl -u operator:change-me \
  'http://127.0.0.1:8688/v1/segments?from=0&to=9223372036854775807'
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
return `400`, and existing input or materialization ceilings return `413`.
The selected sealed-segment request-shape limit is a `400`.
See the [OpenAPI contract](openapi.json) and the
[normative machine API specification](../../docs/superpowers/specs/2026-07-21-i18n-machine-api-contract.md).

## Query and analysis contracts

- Row queries read only overlapping segments, verify the PGM and section CRCs
  before decoding, combine registered layout versions under one logical section
  name, and sort by the registry key. Exact duplicates between sealed segments
  and `active.parts` appear only once.
- The overview preview and event pages use the same typed `EventFact`
  projection: semantic `event_id`, provenance-bound `event_instance_id`, time
  fields, notable and evidence classes, quality flags, a typed payload,
  supporting evidence, and attached loss. Pagination order is exactly
  `(sort_ts_us, event_id, event_instance_id)`.
- Timeline refresh publishes catalog-derived sealed descriptors and one
  bounded live generation without decoding sealed section bodies. An admitted
  request loads only the descriptors selected for its interval.
  Hits in decoded memory, a valid sibling `.ovf`, or the recoverable in-memory
  fallback bypass fact-build admission. Missing facts share work by the full
  `FactBuildKey`, survive request cancellation, and enter the configurable
  process-wide FIFO scheduler. Queue exhaustion, an overweight build, or FIFO
  timeout returns `503` with `code=cold_build_overloaded` and the configured
  `Retry-After`.
- A selected sealed segment that fails while the request is loading facts does
  not become a successful empty segment and does not fail the whole request.
  The response is `200` with that interval in `known_gaps`, reduced
  completeness, and a fact-set identity distinct from the planned complete
  input. Such a partial response is not retained under the complete response
  cache key. The background streaming CRC scrub detects silent section-body
  damage, marks the segment unavailable, and prevents an older sidecar from
  masking that segment gap.
- Event counts use checked arithmetic. Severity and category totals,
  SQLSTATE top/other/missing buckets, and joint top/other buckets independently
  reconcile to retained error occurrences; retained groups and physical
  observation rows are separate counts. Retained exactness, data
  completeness, physical-count semantics, freshness, and known loss remain
  independent response fields.
- A valid sibling sidecar is always consulted before the bounded process-local
  fallback. Only a recoverable publication failure may populate that fallback.
  Exact overview/health response caching is bounded by entry
  count and bytes. Event cursors pin an exact immutable view in a count-, byte-,
  and TTL-bounded registry and bind the query, policy,
  and last sort position with a process-local OS-random authentication key.
- Diff responses distinguish a measured zero from a missing result. A point
  without a valid rate carries one of the response codes `reset`, `gap`,
  `first_point`, `anomaly`, or `not_collected`; `anomaly` here means that the
  timestamps did not advance or the scalar kinds were inconsistent.
- Anomaly search compares each current window with the other usable points in
  the selected period. The strongest absolute peak score appears first.
  Equal-score results use section, column/dimension, identity, and interval fields as
  explicit tie-breakers, so input collection order cannot change truncation.
  `sections` reports evaluated and unevaluated window positions;
  `nodata_points` is an aggregate count, so the anomaly response does not split
  it into reset, gap, and collection-disabled totals. A window position that
  crosses a timeline break is counted under `not_evaluated.discontinuity`.
  Missing data is never replaced by zero.
- Stored-plan analysis adds two stable evidence kinds. For upstream
  `pg_store_plans`, `pg.query.plan_distribution_shift.v1` compares each
  core-query-id plan mixture using call deltas normalized into plan shares. It
  requires at least 20 calls on each side and total-variation distance of at
  least `0.20`. The vadv fork keys rows by `(dbid, userid, planid)`, so its
  `queryid_stat_statements` value is only best-effort attribution and plan
  mixture is explicitly not applicable.
- `pg.plan.buffer_work_per_call_increase.v1` works for both supported forks. It
  compares one retained plan identity at a time, normalizes cumulative buffer
  deltas by call deltas, and requires at least 20 calls on each side, at least
  `1` additional block per call, and at least a `50%` increase. Shared and
  local `hit/read/dirtied/written` and temp `read/written` remain ten separate
  dimensions. The extension exposes no temp hit or dirtied counter.
- Plan windows never bridge reset, extension-version, instance, coverage-gap,
  or observed eviction boundaries. Each plan snapshot must have reset metadata
  at its exact timestamp; an older row is not accepted as provenance. Full
  snapshot coverage is required for plan-mixture claims. A retained plan row
  from a proven top-N snapshot may
  still support its own buffer comparison, but the response remains partial
  for population completeness. Missing or paged plan/provenance pages,
  reader-gap ranges, invalid rows, conflicting coverage or metadata, missing
  system identity, unsupported versions, work limits, and evidence truncation
  stay visible in `plan_analysis.quality`, top-level `coverage`, `truncation`,
  `complete`, and `status`. Source absence is `complete` only when those inputs
  prove the absence; otherwise its plan status is `partial`.
- Top-level `coverage.plan_positions_evaluated` counts specialized detector
  positions that reached a stable/changed verdict. It participates in
  `no_data`, `insufficient_data`, and `calm` status selection independently of
  generic-series positions.
- Both plan detectors are retrospective observations: the reference is the
  rest of the same continuous selected-period segment, including later
  samples. They neither diagnose an optimizer regression nor activate the
  causal `PG-PLAN-002` incident finding by themselves. `limit` independently
  caps ranked numeric `episodes` and ranked `plan_signals`; the response
  reports separate dropped counts for both.
- Incident clustering preserves more detail about incomplete input:
  `data_quality` has separate `resets`, `gaps`, and `not_collected` counts,
  `coverage_by_section` lists gap intervals, and `skipped` explains work omitted
  by a limit. Requests are limited to 24 hours and have fixed ceilings for
  units, sections, materialized cells, series points, identity bytes, scoring
  work, and episodes.
- The incident catalog derives and publishes 28 core lenses, 14 event
  branches, 42 evaluator branches, 40 unique active lens IDs, zero inactive
  IDs, and 24 unresolved strict `EntityJoin` requirements. `evaluators`
  distinguishes registered core and event branches and reports whether each
  branch ran for this request. `registered_lens_ids` is the bounded set of 40
  stable IDs; `evaluated_lens_ids` contains only IDs admitted to actual
  evaluation.
- `catalog_available` reports catalog visibility. `diagnosis_available` is
  independent and becomes true only when at least one evaluator runs; no
  finding is required. It stays false for `no_data`, missing node identity,
  conflicting node identity, and any request in which no branch reaches
  evaluation because its input is absent or admission rejects it.
- Each active core lens publishes only its declared strict `EntityJoin`
  requirement, if any. The requirement has a locale-neutral machine ID,
  owning lens identity (`domain`, `name`, `value`), contract, activation type,
  and separate producer, provenance, and coverage conditions. All 24 remain
  `unavailable`; the response does not activate or imply an unproved relation.
  The legacy `applied`, `active_count`, `catalog_count`, and `dormant` fields
  remain compatibility fields. `applied` contains the same ID set in its
  legacy order; new clients should use the canonical IDs and `counts`.
- Cross-section lock evidence requires an explicit producer-stored shared
  observation token and an exact `(snapshot timestamp, PID, backend_start)`
  match. Equal timestamps do not prove the relation. The current activity and
  lock collectors use separate statements, so
  `entity_join.activity_lock_waiter` reports its producer, provenance, and
  coverage conditions as unavailable.
- Existing OS inputs may still identify bounded candidates, but candidate
  equality is marked `unproven`. Exact storage-device and
  process-cgroup-device relation markers remain reserved for a future request
  that satisfies the published lifetime-mapping conditions.
- Product-owned incomplete-result explanations use the closed
  `{ "kind": "...", "params": { ... } }` reason schema. Lens ids, enum values,
  formulas, units, and evidence remain stable machine data; incident catalogs
  contain no localized title or question.
- Only one anomaly, incident, or uncached timeline response
  projection runs at a time. Equal timeline response misses share one response
  flight; cache hits do not consume the slot. Another distinct heavy request
  receives `503` with `code=analytic_capacity_unavailable` and
  `Retry-After: 1`; it is not queued.

Store scan warnings and damaged journal regions remain available to the reader
and affect gaps/completeness. They are never converted to successful rows.

## Timeline metrics

`/metrics` exposes timeline publication counters
`kronika_web_overview_durable_hits_total`,
`kronika_web_overview_fallback_hits_total`,
`kronika_web_overview_rebuilt_total`,
`kronika_web_overview_promotions_total`,
`kronika_web_overview_persistence_failures_total`,
`kronika_web_overview_sealed_failures_total`; these are monotonic counters.
Fact-load counters advance when admitted requests load selected facts; the
initial publication is descriptor-only. View progress uses
`kronika_web_store_view_generation`,
`kronika_web_overview_view_generation`,
`kronika_web_overview_data_through_us`, and
`kronika_web_overview_refresh_errors_total`.

Persistence state uses
`kronika_web_overview_persist_{mode,failures,retry_after_seconds,probe_in_flight}`,
the closed one-hot gauges `kronika_web_overview_persist_reason{reason}` and
`kronika_web_overview_persist_failure_class{class}`, and
`kronika_web_overview_persist_probe_{attempts,failures,skipped}_total`.
GC publishes scan-complete, sweep-authorized, quota, pending, and scanned-entry
gauges; cache file/logical-byte/allocated-byte gauges by the closed
`kind={sidecar,temporary,lock}` label; skip counters; and
deleted-file plus unlinked logical/allocated-byte counters. “Unlinked
allocated bytes” is the opened inode's `st_blocks` charge before unlink; open
descriptors or hard links can keep those blocks allocated.

Cursor pressure is visible through
`kronika_web_timeline_cursor_views`, `kronika_web_timeline_cursor_bytes`, and
`kronika_web_timeline_cursor_pins_total`,
`kronika_web_timeline_cursor_resolves_total`,
`kronika_web_timeline_cursor_evictions_total`,
`kronika_web_timeline_cursor_expired_total`, and
`kronika_web_timeline_cursor_capacity_rejections_total`. Response-cache and
single-flight activity use
`kronika_web_timeline_response_cache_{hits,misses,evictions}_total`,
`kronika_web_timeline_response_cache_{entries,bytes}`, and
`kronika_web_timeline_singleflight_{leaders,joins}_total`. Selected-segment
policy uses `kronika_web_timeline_selected_segments_limit` and
`kronika_web_timeline_query_limit_rejections_total{resource="selected_segments"}`.
Cold fact-work overload uses
`overview_cold_reject_total{reason}` with the closed reasons
`queue_full`, `weight_exceeds_capacity`, and `timeout`;
`overview_cold_wait_seconds`, `overview_cold_queue_depth`,
`overview_cold_work_inflight{kind="workers"}`,
`overview_inflight_bytes{kind="pgm"|"decoded"}`, and `overview_open_files`
show scheduler pressure. The compatibility counter
`kronika_web_overview_cold_work_rejections_total{reason="capacity"}` advances
for HTTP overload responses.

Decoded and durable lookup work uses
`overview_fact_lookup_total{layer,result,reason}`,
`overview_fact_read_bytes`, `overview_pgm_body_read_bytes`,
`overview_pgm_sections_decoded`,
`overview_fact_build_total{result,source_type}`, and
`overview_fact_build_seconds`; writes use `overview_fact_write_bytes`. The
decoded in-memory layer publishes
`overview_cache_{entries,bytes}{class="decoded_facts"}` and
`overview_cache_evictions_total{class="decoded_facts",reason}`.

The parity-v1 operational inventory also includes the one-hot
`overview_cache_mode{mode,reason}`, `overview_persist_failures_total{reason}`,
`overview_persist_backoff_seconds`,
`overview_singleflight_{builds,waiters}`,
`overview_live_state{state,reason}`, `overview_live_folded_parts_total`,
`overview_live_data_through_us`, `overview_live_visibility_lag_seconds`,
`overview_view_generation`, `overview_cursor_views`,
`overview_cursor_view_bytes`, and `overview_cursor_expired_total{reason}`.
Correctness signals are `overview_source_failures_total{reason}`,
`overview_coverage_loss_total{source,factor,reason}`,
`overview_retained_observations_total{kind}`,
`overview_overflow_total{kind}`, `overview_raw_fallback_total{reason}`,
`overview_gc_files_total{action,reason}`, and
`overview_gc_bytes_total{action}`. Every name is registered with a metric type
and help text when the router is built; dynamic source/factor values come only
from bounded published inventories, while all other label values are closed.

Source integrity and partial-result behavior use
`overview_source_read_failures_total{outcome="partial"}`,
`overview_source_scrub_total{outcome}`,
`overview_source_scrub_bytes_total`, `overview_source_scrub_seconds`, and
`overview_source_damaged_segments`. These and the scheduler/cache label sets
are fixed; HTTP request labels use matched route templates rather than raw
URIs.

## Shutdown and failure behavior

`SIGTERM` and `SIGINT` start graceful HTTP shutdown. The refresh task reports
scan errors and keeps the last published view; `/readyz` becomes stale once the
configured threshold is exceeded. A successful store scan followed by a
timeline-build failure publishes the fresh metadata together with the last
usable timeline and never exposes a partially built timeline. A bad
environment, initial store/overview failure, or unavailable OS entropy for
cursor authentication exits before binding the listener.

## Real-process restart and recovery BDD

The PostgreSQL 15–18 BDD matrix runs
`timeline_web_lifecycle.feature` against the packaged `pg_kronika-web`
executable, not an in-process router or a reconstructed `AppState`. Each
scenario copies one sealed collector PGM into an owned temporary data
directory and starts several fresh processes over that same directory. Real
HTTP calls verify cold sibling creation, a stable inode/mtime/hash on a durable
restart hit, zero PGM body reads and section decodes, corrupt and stale-header
rebuilds, interrupted temporary-file recovery, bounded publication fallback,
process-local cursor expiry, and deterministic writer-owner contention.

Readiness comes from a post-bind process announcement, graceful exits are
asserted, and crash/contended publication uses a qualification-only Unix
socket barrier after the temporary OVF is synced and before its atomic rename.
The harness does not use sleeps or retry loops to decide these outcomes. Run
the complete lifecycle matrix with:

```sh
DEBUG=1 make test-bdd TAGS=@timeline_web_lifecycle
```

For one supported major, use a Cucumber tag expression such as:

```sh
DEBUG=1 make test-bdd TAGS='@timeline_web_lifecycle and @pg15'
```

The binary has no CLI flags and does not implement MCP, remote stores, source
segment retention, or alert delivery.
