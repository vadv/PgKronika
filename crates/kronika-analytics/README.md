# kronika-analytics

[Русская версия](README.ru.md)

`kronika-analytics` is PgKronika's shared computation kernel. It is not a
separate process or standalone product. `pg_kronika-collector` obtains the
source data, `kronika-writer` records it in PGM, `kronika-reader` decodes rows
and builds series and facts, `kronika-analytics` applies common rules, and
`pg_kronika-web` assembles the results into HTTP and UI views.

The analytics kernel is responsible for:

- cumulative-counter differences and interval-derived rates;
- fixed-size classification against provisional absolute resource thresholds;
- robust anomaly scores and contiguous anomaly episodes;
- call-normalized categorical-distribution and per-operation work comparisons;
- checked counts, coverage, notable-event selection, and health evaluation for
  the timeline;
- one bounded query contract for `overview` facts.

Both `kronika-reader` and `pg_kronika-web` use these deterministic rules. The
web layer owns request windows, budgets, incident clustering, and JSON/UI
serialization. Analytics deliberately contains no PGM decoding, filesystem or
network I/O, PostgreSQL access, HTTP handling, redaction, or diagnosis.

Observation identities do not depend on a filesystem path:

- a sealed PGM produces rebuild-stable content-derived lineage from its exact
  content descriptor and first catalog descriptor;
- a live view uses its journal generation and first-part descriptor and reports
  `IdentityQuality::Approximate` until a sealed handoff is proven.

## Place in the data flow

```text
pg_kronika-collector -> kronika-writer -> active.parts / N.pgm
        -> kronika-reader decodes rows, marks gaps and resets,
           and publishes the same-stem sibling N.ovf

adjacent counter samples -> kronika-analytics::diff_pair -> DiffPoint
        -> kronika-reader assembles SeriesDiff
        -> pg_kronika-web builds diff responses and anomaly input

SeriesDiff + gauge values -> pg_kronika-web defines windows
        -> kronika-analytics::score_window / episodes
        -> pg_kronika-web ranks episodes and builds incidents

plan call deltas and buffer deltas -> pg_kronika-web proves continuity
        -> kronika-analytics::compare_distributions / compare_per_unit
        -> pg_kronika-web adds typed, bounded plan evidence

fixed-size metric operands -> kronika-analytics::threshold::classify
        -> explainable absolute-threshold verdict
        -> no HTTP or UI adapter is connected yet

typed observations -> SegmentFacts / LiveView in kronika-reader
        -> IndexView in pg_kronika-web
        -> kronika-analytics::overview rules
        -> pg_kronika-web builds timeline JSON/UI
```

This shared I/O-free layer keeps `kronika-reader` and `pg_kronika-web` from
implementing the same rules differently. In particular, it keeps missing data
and resets distinct from measured zero and prevents handlers from changing
scoring and aggregation formulas independently.

## How analytics supports PgKronika features

PgKronika records timestamped PostgreSQL, Linux, and cgroup snapshots, typed
stderr events, and collection coverage. These records are evidence, not
ready-made UI metrics. A cumulative total needs two compatible samples, a
missing interval must not look like zero activity, and a burst of log rows
needs explicit counting and ranking rules.

| PgKronika feature | Input from `kronika-reader` | What the analytics kernel does | User-visible result |
| --- | --- | --- | --- |
| `GET /v1/section/{name}/diff`, `GET /v1/sections/batch/diff` | Decoded rows and gaps; the reader groups cumulative columns by registry identity. | `diff_pair` computes each valid adjacent delta and per-second rate. The reader adds `FirstPoint` and `Gap`; web applies collection gates as `NotCollected`. A decrease remains `Reset`, and measured zero remains a value. | Each point has `delta`, `rate`, and `dt_micros`, or an explicit `nodata` reason. |
| `GET /v1/anomalies`, input to `GET /v1/incidents` | Rate series for cumulative columns and value series for gauges. | Scores retrospective current/reference windows and groups adjacent above-threshold positions into episodes. | Anomaly intervals with series, metric, direction, and peak score. Web then clusters these episodes and runs incident lenses. |
| Stored-plan evidence in `GET /v1/anomalies` | Call counts by plan and buffer-work totals from continuity-proven `pg_store_plans` intervals. | Compares call-normalized plan shares and work per call using explicit sample and effect gates. | Stable, typed plan-mixture and same-plan buffer signals. Web owns fork applicability, reset/gap/version boundaries, work limits, completeness, and JSON. |
| `GET /v1/timeline/overview`, `/events`, `/health` | Typed log observations with provenance, occurrence counts, and coverage. | Folds checked counts, selects a bounded notable preview, and evaluates health only from eligible evidence. | Event digest and pages, coverage and loss metadata, and health points that remain `Unknown` when required evidence is missing. |

`SegmentFacts` and `LiveView` in `kronika-reader` and `IndexView` in
`pg_kronika-web` implement `RawOracle`. One query against a pinned view keeps
its observations, counts, and coverage together; `semantic_divergences`
checks whether alternate query paths mean the same thing.

## Module map

| Module | Purpose | Main entry points |
| --- | --- | --- |
| [`diff`](src/diff/mod.rs) | Lets the reader turn consecutive cumulative PGM samples into a value, reset, or invalid interval without treating no-data as zero. | `diff_pair`, `Scalar`, `DiffPoint`, `Reason` |
| [`threshold`](src/threshold/mod.rs) | Classifies fixed-size metric operands against 69 provisional absolute-threshold policies without source or transport knowledge. | `MetricId`, `MetricInput`, `classify`, `catalog` |
| [`anomaly`](src/anomaly/mod.rs) | Scores retrospective windows, folds adjacent triggers into episodes, compares normalized category mixtures, and compares work per operation. | `ScoreParams`, `score_window`, `episodes`, `DistributionParams`, `compare_distributions`, `PerUnitParams`, `compare_per_unit` |
| [`overview::observation`](src/overview/observation.rs) | Gives reader-produced event facts validated payloads, provenance, and stable or view-scoped identities. | `EventObservation`, `SegmentIdentity`, `ObservationPayload` |
| [`overview::counts`](src/overview/counts.rs) | Aggregates timeline errors by `(severity, category, SQLSTATE)` and lifecycle events with checked arithmetic. | `EventCounts`, `LifecycleCounts`, `CountLimits` |
| [`overview::coverage`](src/overview/coverage.rs) | Preserves whether the reader actually covered a half-open time span instead of turning an absent measurement into zero. | `CoverageSpan`, `Coverage` |
| [`overview::reduce`](src/overview/reduce.rs) | Defines bounded counter, ratio, gauge, and zero-order-hold primitives for future counter/gauge health factors; current production endpoints do not call them. | `classify_series`, `CounterReduction`, `GaugeReduction`, `time_weighted_mean` |
| [`overview::notable`](src/overview/notable.rs) | Selects and deterministically ranks event rows for the web timeline and overview preview. | `NotablePolicy`, `NotableClass` |
| [`overview::health`](src/overview/health.rs) | Combines eligible covered factors into health scores and preserves `Unknown` when required evidence is absent. | `HealthPolicy`, `RequiredFactorProfile`, `downsample_worst` |
| [`overview::health_line`](src/overview/health_line.rs) | Applies the current event-only policy to the bucketed health line served by web. | `overview_health_policy`, `health_line` |
| [`overview::oracle`](src/overview/oracle.rs) | Defines one bounded result containing observations, counts, and coverage for implementations in `kronika-reader` and `pg_kronika-web`; compares alternate query paths. | `RawOracle`, `query_bounded`, `semantic_divergences` |

## Class 1 threshold catalog

`threshold::classify(MetricId, MetricInput)` applies one of 69 built-in
policies covering CPU and load, memory and swap, PSI, cgroup, storage, network,
and PostgreSQL activity, connection capacity, cache/checkpoints, table
maintenance, statements/plans, and replication. All current values are
`Calibration::Provisional`: they are explicit starting points, not thresholds
validated against representative production installations.

The six input forms are scalar, fraction, observation with a dynamic limit,
ratio with an absolute count floor, caller-gated age, and free capacity with
relative and absolute conditions. An applicable input returns
`Classified::Verdict` with its `Level`, the exact warning or critical
`Boundary` that selected it, and fixed-size `Evidence` containing the operands
and derived value. Missing input, a not-applicable rule, non-finite or
out-of-domain numbers, an invalid denominator, and an adapter input-shape
error remain distinct `NotClassifiedReason` values.

Source adapters must provide reset-aware deltas for cumulative counters.
Connection capacity is `client backend` count divided by a positive
`max_connections`, so an absolute backend count is not classified without its
server limit. Config-bound autovacuum indicators receive version- and
reloption-aware effective thresholds; a disabled or inapplicable server rule
must be passed as `MetricInput::NotApplicable`.

The catalog is a static slice and lookup is O(1). One classification is
deterministic, allocation-free, I/O-free, clock-free, and O(1); callers provide
all operands including current time for age policies.

The bounded `GET /v1/frame/{view}` adapter in `pg_kronika-web` is the first
consumer. Its exhaustive manifest binds 14 per-cell numeric policies and
defers the other 55 `MetricId` values with typed reasons. Web prepares exact
typed operands from a current snapshot and proven predecessor, then serializes
the returned `Classified`; it does not copy policy numbers, operators, zero
semantics, boundaries, or evidence rules. No binding and a bound input that
returns `NotClassifiedReason` remain different wire states. Config-bound
autovacuum policies stay deferred until relation reloptions are durably
collected. Frame reads the exact sealed descriptor selected by `UiSummary`;
planning settings come from that same PGM. Delta views admit a predecessor
only within their typed 15-minute `max_rate_gap`, otherwise the cell remains
gap/not-classified and the second PGM is not opened. When two PGMs are needed,
they share one row, cell, and owned-byte materialization ceiling. Search is
limited to the public label and selected non-lazy cells returned by the frame.
The production frontend is not part of this integration.

This Class 1 contract answers whether an observation crossed a fixed operator
policy. The separate [`anomaly`](src/anomaly/mod.rs) module implements Class 2:
a modified z-score relative to the history of the same series.

## Rules behind the views

### Diff responses: resets and no-data

`diff_pair` expects two values of the same `Scalar` variant and timestamps in
Unix microseconds. For integer counter values admitted by the reader it returns
an exact `i128` delta; the rate for both scalar variants is per second.

- A decrease returns `Reason::Reset`.
- A zero or negative time interval, or mixed scalar variants, returns
  `Reason::Anomaly`.
- An unchanged counter returns a real zero delta and rate. Zero is not missing
  data.
- `Reason::Gap`, `Reason::FirstPoint`, and `Reason::NotCollected` require
  series, coverage, or collection-gate context and are assigned by the caller.

`Scalar::Float` does not validate finiteness. A caller that requires finite
delta and rate values must reject `NaN` and infinities before calling
`diff_pair`.

### Query-plan change evidence

The plan kernels accept source-independent aggregates; they do not know about
PostgreSQL forks, query IDs, plan IDs, snapshots, or HTTP.

`compare_distributions` independently normalizes the reference and current
category counts, then computes total-variation distance:

```text
reference_share[plan] = reference_calls[plan] / reference_calls_total
current_share[plan]   = current_calls[plan] / current_calls_total
total_variation       = 0.5 * sum(abs(current_share - reference_share))
```

Duplicate category rows are summed with checked arithmetic, zero-count rows
have no effect, and category evidence is ordered by stable numeric identity.
A uniform increase in all call counts is therefore stable. The default verdict
requires 20 calls on each side and an inclusive `total_variation >= 0.20`;
zero distance never triggers, even if a caller constructs a zero threshold.
Memory is linear in the admitted distinct categories, which the adapter must
bound before calling the kernel.

`compare_per_unit` compares `work / operations` on the two sides. Its default
verdict requires 20 operations on each side, a strict increase, at least
`1.0` additional work unit per operation, and at least a `50%` relative
increase. A zero-work reference has no finite relative ratio: any positive
current rate passes the relative gate, but the absolute gate still applies.
The function allocates no memory and retains exact integer totals alongside
the derived finite rates.

Both comparisons report evaluated evidence even when stable and return a typed
reason when a sample gate or checked sum fails. They establish an observed
association only. The web adapter decides whether stored snapshots form one
continuous, applicable population and must not turn either verdict into a
causal optimizer diagnosis.

### How PgKronika detects an anomaly

PgKronika does not flag every increase or decrease. It checks how far the
median in the current window has moved from the usual level of the same series,
relative to that series' normal variation.

At each scan position:

1. The current window contains values timestamped from `position - window`
   through `position`, inclusive. The current `pg_kronika-web` policy requires
   at least three values.
2. The reference contains the other values in the same uninterrupted segment.
   The current policy requires at least 20 values. The scan is retrospective,
   so the reference can include values later than the current window.
3. PgKronika computes a deviation score `m`: the median difference divided by
   a robust scale of normal variation.
4. The position is anomalous only when `abs(m) > threshold`. With the default
   `threshold=3.5`, a score of `3.5` does not trigger; `3.5001` does.

The first position that crosses a counter reset, known gap, or invalid value is
not evaluated. Later positions use only data after that boundary and also
remain not evaluated until they have at least 20 reference and three current
values.

#### How many values are required

The current `pg_kronika-web` policy fixes `min_cur=3` and `min_ref=20`.
HTTP query parameters cannot change these values.

| Result being computed | Minimum data |
| --- | --- |
| One cumulative-counter rate point | Two adjacent source samples: the previous and current values. |
| One anomaly score | Three eligible values in the current window and 20 eligible reference values, for at least 23 values from one uninterrupted series. |
| One anomaly score for `xact_commit` | At least 24 source counter samples: the first establishes the starting value and the next 23 produce 23 rate points. |
| One anomaly score for a gauge | At least 23 source measurements: three current and 20 reference values. No adjacent-value difference is computed. |

`window` specifies a time span, not a point count. A position is not evaluated
if only two eligible values fall inside its window, even when the whole request
contains plenty of data. Likewise, 19 reference values are insufficient. A
reset or gap starts a new uninterrupted segment, so values on opposite sides
of that boundary cannot be combined to meet the minimum.

#### Example: transaction-rate increase

For `pg_stat_database.xact_commit`, PgKronika first derives the commit rate. A
counter increase from `1,000,000` to `1,000,100` over one second produces
`100 transactions/s`. Detection operates on this rate, not on the raw
cumulative value. `xact_commit` and `xact_rollback` are scored separately for
each database; the detector does not add them into a total TPS series.

The example uses exactly the minimum required data: three current rate values
and 20 reference values, for 23 rate points in total. Producing them from
`xact_commit` required 24 source counter samples.

Use `window=2m`, `step=1m`, `eps_rel=0.05`, and `threshold=3.5`. At the
`12:30` position:

- current window: `119`, `120`, `121 transactions/s` at `12:28`, `12:29`,
  and `12:30`, with median `120`;
- reference: ten values of `98` and ten values of `102 transactions/s`, with
  median `100` and median absolute deviation `MAD=2`.

The scale floor is `0.05 * 100 = 5 transactions/s`. The observed reference
variation is `1.4826 * 2 = 2.9652`, so the larger value `5` is used:

```text
sigma = max(2.9652, 5) = 5 transactions/s
m = (120 - 100) / 5 = 4.0
```

Because `4.0 > 3.5`, the `12:30` position is an anomalous increase.

Every row below uses the same reference: ten values of `98` and ten values of
`102 transactions/s`. Its median is `100`, `MAD=2`, and the scale used is
`sigma=5 transactions/s`. The three values in the first column are used only
to compute the current-window median; they are not compared with each other.

| Current window, transactions/s | Current median | Reference median | `sigma` | Calculation of `m` | Result |
| --- | ---: | ---: | ---: | ---: | --- |
| `119`, `120`, `121` | `120` | `100` | `5` | `(120 - 100) / 5 = 4.0` | Anomalous increase |
| `116`, `117.5`, `119` | `117.5` | `100` | `5` | `(117.5 - 100) / 5 = 3.5` | No trigger: the score reaches but does not exceed the threshold |
| `114`, `115`, `116` | `115` | `100` | `5` | `(115 - 100) / 5 = 3.0` | No trigger |
| `79`, `80`, `81` | `80` | `100` | `5` | `(80 - 100) / 5 = -4.0` | Anomalous decrease: `abs(-4.0) > 3.5` |

The sign of `m` gives the direction: positive means an increase and negative
means a decrease. Triggering uses the absolute score, so both directions are
checked. In the final row, the current median `80` is compared with the
reference median `100`:

```text
median difference = 80 - 100 = -20 transactions/s
m = -20 / 5 = -4.0
abs(m) = 4.0 > 3.5
```

The current-window median is `20 transactions/s` below its usual level, a
change four times the scale of `5 transactions/s`. The position is therefore
an anomalous decrease.

Here, anomaly means a statistically unusual change, not a diagnosis. A TPS
drop may be an expected reduction in load or a symptom of a problem. The
analytics kernel only marks the deviation; incident processing adds context
and possible explanations.

The threshold is not a fixed percentage. A noisier normal series requires a
larger change. For example, a reference containing ten values of `90` and ten
values of `110 transactions/s` has median `100` but `MAD=10`. For a current
window of `139`, `140`, `141 transactions/s`:

```text
sigma = max(1.4826 * 10, 0.05 * 100) = 14.826
m = (140 - 100) / 14.826 ≈ 2.70
```

There is no trigger despite the increase of `40 transactions/s`: the change is
not large enough relative to this series' usual variation.

#### How triggers form an episode

Adjacent positions where `abs(m)` exceeds the threshold form one episode. For
example, scores `4.0`, `5.1`, and `3.8` at `12:30`, `12:31`, and `12:32`
form one episode whose peak is `5.1`. A score of `2.2` at `12:33` closes it. A
position that is not evaluated because of a reset, gap, or insufficient data
also closes the episode. A single triggering position forms an episode whose
`start` and `end` are equal.

The exact formula is:

```text
floor = max(eps_abs, eps_rel * abs(median(reference)))
sigma = max(1.4826 * MAD(reference), floor)
m = (median(current) - median(reference)) / sigma
```

`MAD` is the reference's median absolute deviation. The score `m` expresses
the magnitude and direction of the change in robust-scale units; it is not a
probability or percentage.

### Internal overview reduction primitives

`overview::reduce` is not wired into the current production timeline
endpoints. It defines the bounded semantics intended for counter and gauge
health factors when those are connected.

`CounterReduction::rate_per_us` is measured per microsecond, unlike
`diff_pair`'s per-second rate. It divides `sum(delta)` by `sum(duration_us)`;
it does not average pair rates. `GaugeReduction::sample_mean` weights every
sample equally. Time weighting is available only through
`time_weighted_mean`, which uses zero-order hold and stops at a configured
maximum hold time or a known gap.

## What can be tuned

### Query parameters available to operators

The values below are accepted by `pg_kronika-web`. They are separate from the
internal parameters and `*Limits` described later. The sources of truth are the
[`/v1/anomalies` handler](../../bins/pg_kronika-web/src/handlers/anomalies.rs)
, the
[`/v1/incidents` handler](../../bins/pg_kronika-web/src/handlers/incidents.rs),
and
[timeline handlers](../../bins/pg_kronika-web/src/overview/handlers.rs).

`source`, `from`, and `to` are required. `from` and `to` are Unix
microseconds, with `from < to`. Anomalies and incidents scan one `source`.
`/v1/timeline/overview` and `/v1/timeline/health` also require exactly one
`source`, while `/v1/timeline/events` accepts repeated `source` parameters.
Incident ranges are limited to 24 hours and timeline ranges to 31 days.

#### Anomalies and incidents

Both scans are retrospective, not causal: the current slice is
`[position - window, position]`, and the reference contains the remaining
values in the same uninterrupted segment, including later values.

| Parameter | `/v1/anomalies` default | `/v1/incidents` default | Effect |
| --- | --- | --- | --- |
| `window` | `1h` | `5m` | Width of the current slice compared with the reference. A larger window smooths short changes and needs more history inside the request. |
| `step` | `window / 4` | `1m` | Distance between scan positions. The anomalies default is computed after resolving `window`; the final position is always `to`. A smaller step gives finer timing but performs more scoring work. |
| `threshold` | `3.5` | `3.5` | Strict episode cutoff in robust-scale units: a position belongs to an episode only when `abs(m) > threshold`. Raising it returns fewer, stronger deviations. |
| `eps_rel` | `0.05` | `0.05` | Fraction of `abs(median(reference))` used in the scale floor. Raising it reduces `abs(m)` only when this floor exceeds both `eps_abs` and `1.4826 * MAD`; otherwise the score is unchanged. |
| `limit` | `50` | not accepted | Maximum number of episodes returned by `/v1/anomalies` after ranking by peak `abs(m)`; it does not cap scan work. It accepts a nonnegative integer and clamps values above `10,000` to `10,000`. |
| `epsilon` | not accepted | `step` (`1m` with defaults) | Maximum gap between neighboring episodes that may still be merged: `gap <= epsilon`. |
| `max_cluster_span` | not accepted | `min(1h, request span)` | Maximum cluster extent from its first start to its furthest end, inclusive of equality. |
| `section` | all eligible sections | all eligible sections | Restricts the scan to one known logical section, such as `pg_stat_database`. Without it, the endpoint scans every non-deprecated snapshot or event section with cumulative or gauge columns. |

Duration values are positive integers with an `ms`, `s`, `m`, or `h` suffix;
an integer without a suffix is interpreted as seconds. Incidents additionally
require `epsilon <= max_cluster_span <= request span`. `window` must fit
inside the request range.

For both paths, web fixes `min_ref=20`, `min_cur=3`, and `eps_abs=1e-6` for
cumulative rates and raw gauges. This overrides the internal
`ScoreParams::default().eps_abs`.

#### Timeline

| Route and parameter | Default | Effect |
| --- | --- | --- |
| `/v1/timeline/health`: `step` | computed automatically | Requested health-point width in microseconds. Web uses `max(step, ceil((to - from) / 2000), 1)`, so the response has at most 2,000 points. A smaller requested value cannot bypass that ceiling. |
| `/v1/timeline/events`: `limit` | `100` | Events per page; accepted range `1..=1000`. |
| `/v1/timeline/events`: `min_severity` | disabled | Accepts `panic`, `fatal`, `error`, `warning`, or `log`. Events below the selected level are removed; typed events without a severity still pass. |
| `/v1/timeline/events`: `kind` | disabled | Among events classified as notable by `NotablePolicy::v1`, keeps records whose `event_kind` exactly and case-sensitively matches the parameter, such as `pg.log.error_group_observed` or `pg.lifecycle.child_signal_termination`. |
| `/v1/timeline/events`: `cursor` | first page | Opaque pointer to the next page of the pinned view. It must be reused with the same sources, range, and filters. |

`/v1/timeline/overview` has no additional parameters that change analytics
rules. See the
[`pg_kronika-web` operator guide](../../bins/pg_kronika-web/README.md) for the
complete HTTP contract and request limits.

### Internal policy parameters

These Rust-level inputs are selected by `kronika-reader` and
`pg_kronika-web`. They are not independent operator settings beyond the query
parameters above.

| Parameter | Built-in value or valid range | Exact effect |
| --- | --- | --- |
| `ScoreParams::min_ref` | Default `20`; `ScoreParams::new` changes `0` to `1`. | Minimum reference values needed to score one anomaly scan position; fewer return `RefTooSmall`. |
| `ScoreParams::min_cur` | Default `3`; `ScoreParams::new` changes `0` to `1`. | Minimum values in that position's current window; fewer return `CurTooSmall`. |
| `ScoreParams::eps_abs` | Default `1e-9`; `ScoreParams::new` clamps it to at least `f64::MIN_POSITIVE`. | Absolute lower bound for anomaly scale `sigma`, in the units of the rate or gauge being scored. Web overrides it as described above. |
| `ScoreParams::eps_rel` | Default `0.05`; `ScoreParams::new` clamps it to at least `0.0`. | Relative lower bound for anomaly scale, as a fraction of `abs(median(ref_))`. Web passes the query value above. |
| `episodes::threshold` | No default; must be finite and at least `0.0`. | Keeps anomaly scan positions with strict `abs(m) > threshold`. A negative or non-finite value returns no episodes. |
| `DistributionParams` | Default: 20 reference calls, 20 current calls, total variation `0.20`. Counts are clamped to at least one; a non-finite effect becomes `1.0`, otherwise it is clamped to `0.0..=1.0`. | Emits `Shift` only for a nonzero total-variation distance at or above the inclusive effect gate after both count gates pass. |
| `PerUnitParams` | Default: 20 reference operations, 20 current operations, absolute increase `1.0`, relative increase `0.50`. Counts are clamped to at least one; negative effects become zero and non-finite effects become `f64::MAX`. | Emits `Increase` only when the exact cross-product comparison proves a strict rate increase and both inclusive effect gates pass. |
| `HoldModel::max_gap_us` | No default; microseconds. | Longest interval for which the internal `time_weighted_mean` primitive may carry a gauge value forward. `0` provides no positive hold coverage; a hold never crosses a known gap. |
| `NotablePolicy::response_cap` | `100` in `v1()`; `with_response_cap` accepts `1..=1000`. | Maximum ranked observation rows in the overview preview. The full retained input is still scanned; `total_notable` and `omitted_count` report pre-cap rows. |
| `FactorPenalty::new(..., penalty, ...)` | No default; finite `0.0..=1.0`. | Penalty contributed by one covered factor to a generic health cell. Invalid values are rejected. |
| `HealthPolicy::degraded_below` | No generic default; finite `0.0..=1.0`. | A known health score strictly below this value is `Degraded`; equality is `Normal`. |
| `HealthPolicy::critical_below` | No generic default; finite `0.0..=degraded_below`. | A known health score strictly below this value is `Critical`; equality is `Degraded` when the two thresholds differ. |

Use `ScoreParams::new` to apply the lower-bound normalization described above.
It does not reject positive infinity, so configuration parsing must still
require finite values. The fields are public, and a struct literal bypasses
normalization entirely.

`RequiredFactorProfile::new` also accepts a semantic `profile_id`, nonempty
required factor lists grouped by domain, optional factor/domain pairs, and
`HealthLimits`. One factor may not occur twice or belong to two roles.
`HealthPolicy::new` requires nonzero policy and reduction-semantics versions.
These identifiers select compatible semantics. Changing them is a compatibility
change, not an adjustment to score sensitivity.

For a generic health cell, every required factor must either be validly
`NotApplicable` or have strictly eligible coverage and an explicit
`FactorPenalty`. A supplied optional penalty participates too. The maximum
factor penalty is selected in each domain, then:

```text
continuous_score = product(1 - domain_penalty)
```

A trusted floor makes the state `Critical`. If `continuous_score` is known,
the floor also makes `overall_score = Some(0.0)`; if required evidence is
unknown, `overall_score` remains `None`.

`NotablePolicy::preview` counts each retained observation row once; it does not
expand an `ErrorGroup.occurrence_count`. Version 1 classifies only fixed
`PANIC`/SQLSTATE/authentication error groups and child termination or crash
records that carry a signal. A signal-less crash, generic application error,
ready event, or shutdown event is not notable. Ranking tiers are `Panic` and
`ChildSigkill`; then `OutOfMemory`, `DiskFull`, and `IntegrityError`; then
`ChildSignalTermination`; then `ConnectionSlotsExhausted`, `Deadlock`, and
`LockNotAvailable`; then `SerializationFailure` and `QueryCancelled`; and
finally `AuthenticationFailure`, `AuthorizationFailure`, and
`PermissionDenied`. Within a tier, newer observations come first, followed by
`observation_id`.

### Internal resource limits

`CountLimits`, `ReductionLimits`, `OracleLimits`, and `HealthLimits` do **not**
implement `Default`. There is no universal safe value: the caller must choose
hard ceilings from its admitted input size and memory budget. Exceeding a
ceiling returns a typed error rather than a partial reduction.

| Type and field | Unit | What it limits |
| --- | --- | --- |
| `CountLimits::max_input_entries` | entries | Each count-constructor input before normalization, including zero-count entries. In `fold_counts`, error-group rows and signal-bearing lifecycle rows are charged separately; other observation rows are not charged. `EventCounts::merge` does not apply it. |
| `CountLimits::max_joint_keys` | distinct keys | Stored nonzero `(severity, category, SQLSTATE)` combinations. |
| `CountLimits::max_signal_keys` | distinct signals | Stored nonzero lifecycle signal numbers. |
| `ReductionLimits::max_input_items` | records | The whole supplied record slice, checked before bucket or range filtering. |
| `ReductionLimits::max_gap_spans` | spans | All supplied normalized known-gap spans, checked before range filtering. |
| `ReductionLimits::max_counter_pairs` | intervals/pairs | Counter intervals retained or selected. `classify_series` also requires its sample count to fit this ceiling. |
| `ReductionLimits::max_gauge_samples` | samples | Samples retained in one `GaugeReduction` after bucket selection. `time_weighted_mean` applies it to every supplied sample plus the optional preceding sample; `max_input_items` separately limits the full input slice. |
| `OracleLimits::max_observations` | observations | Unique in-range observations returned by one query. |
| `OracleLimits::max_coverage_spans` | spans | Clipped input coverage spans before `Coverage` merges overlaps and adjacency. |
| `OracleLimits::count_limits` | nested limits | Sparse count work performed for the same atomic result. |
| `max_materialized_bytes` argument | logical bytes | Inline `EventObservation` storage plus its boxed payload, owned text, and loss storage, charged before cloning. |
| `HealthLimits::max_profile_factors` | factors | Required and optional factors in one profile. |
| `HealthLimits::max_cell_factors` | penalties | Supplied factor penalties in one health cell, checked before duplicate validation. |
| `HealthLimits::max_coverage_entries` | records | Supplied factor-coverage records in one cell, checked before duplicate validation. |
| `HealthLimits::max_floor_evidence` | records | Supplied trusted floor records in one cell or downsample bucket, checked before deduplication. |
| `HealthLimits::max_downsample_points` | points | Fine health points scanned by `downsample_worst`. |

Input identities, timestamps, values, coverage records, and the version
constants in [`overview`](src/overview/mod.rs) are data or compatibility axes,
not tuning parameters.

## Current overview state

- `CoverageSpan` is always half-open: `[from_us, to_us)`.
- Missing coverage, a reset, disabled collection, and a measured zero remain
  different states. Count addition and merges use checked arithmetic.
- `NotablePolicy::v1` classifies individual observations and ranks them
  deterministically. It does not infer a cause. In particular, `SIGKILL`,
  an out-of-memory observation, and integrity evidence remain separate.
- `overview_health_policy()` currently has one required
  `DatabaseErrorPressure` factor that event facts do not cover. Therefore
  `health_line` currently returns `None` for both numeric scores in every
  bucket. Without trusted floor evidence the state is `Unknown`.
- Structured or derived-exact `PANIC` evidence can force `Critical` for
  availability; evidence of the same quality for `XX001` or `XX002` can force
  `Critical` for integrity. Parsed or heuristic text, child termination, and
  `53100` do not establish a floor.
- The fixed v1 overview health policy uses strict thresholds `0.8` and `0.5`
  and `OVERVIEW_HEALTH_LIMITS` of `8` profile factors, `8` cell factors, `8`
  coverage entries, `65,536` floor records, and `10,000` points.
- `MemoryOracle` is a fixture over decoded records. Production `RawOracle`
  implementations include `SegmentFacts` and `LiveView` in `kronika-reader`
  and the assembled `IndexView` in `pg_kronika-web`.

Detailed identity, payload, coverage-eligibility, ranking, and health decision
tables live in the linked module rustdoc and source files rather than in this
README.

## Implementation paths

The relevant project call sites are the
[`kronika-reader` diff fold](../kronika-reader/src/query/diff.rs), the
[`pg_kronika-web` anomaly adapter](../../bins/pg_kronika-web/src/anomaly.rs),
the
[`pg_kronika-web` timeline handlers](../../bins/pg_kronika-web/src/overview/handlers.rs),
and the [workspace architecture](../../docs/architecture.md).
