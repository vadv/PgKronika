//! Thin axum handler for `GET /v1/timeline/overview`.
//!
//! The handler validates the request, assembles the atomic index view off the
//! async runtime, queries the requested range, and serializes a compact event
//! and health summary. It orchestrates only: counts, notable selection, and
//! coverage come from `kronika-analytics`.

use std::collections::{BTreeMap, BinaryHeap};
use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::overview::{
    CountError, CountLimits, CountResource, CoverageSpan, EventCounts, EventObservation,
    NotableClass, NotablePolicy, OracleError, OracleLimits, OracleResource, PhysicalCountSemantics,
    RetainedExactness, Severity, SourceCompleteness, SourceScopeId,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::overview::cache::{CacheKey, Endpoint, ResponseKey};
use crate::overview::cursor::{CursorError, EventsCursor};
use crate::overview::dto::{
    CoverageSpanDto, EventDigestDto, EventFact, EventFactPosition, EventFactProjection,
    EventsResponseDto, JointCountDto, LifecycleDigestDto, NotablePreviewDto, OverviewResponseDto,
    SignalCountDto, SourceFreshnessDto, SourceLossDto, SqlstateCountDto, TimelineMetaDto,
    category_name, severity_name, sqlstate_text,
};
use crate::overview::view::{IndexView, SourceMetadata};
use crate::params::QueryParams;
use crate::problem::{ApiProblem, QueryParameter};
use crate::{AppState, TimelineFlightRole};

/// Absolute query span for the overview endpoint: 31 days.
const MAX_OVERVIEW_SPAN_US: i64 = 31 * 24 * 3_600 * 1_000_000;

/// Response schema version echoed into every timeline response.
const RESPONSE_SCHEMA_VERSION: u32 = kronika_analytics::overview::RESPONSE_SCHEMA_VERSION;

/// Health policy version bound into response cache keys.
const HEALTH_POLICY_VERSION: u32 = kronika_analytics::overview::HEALTH_POLICY_VERSION;

/// Top-N sparse dimensions kept in the digest projection.
const DIGEST_TOP_N: usize = 16;

const OVERVIEW_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
];

const QUERY_LIMITS: OracleLimits = OracleLimits {
    max_observations: 1_048_576,
    max_coverage_spans: 262_144,
    count_limits: CountLimits {
        max_input_entries: 1_048_576,
        max_joint_keys: 65_536,
        max_signal_keys: 1_024,
    },
};

/// Maximum logical bytes cloned into one timeline oracle result.
const QUERY_MATERIALIZED_BYTES: usize = 64 * 1024 * 1024;

/// A validated overview request range.
#[derive(Debug, Clone, Copy)]
struct OverviewRequest {
    range: CoverageSpan,
    source: u64,
    from_us: i64,
    to_us: i64,
}

/// `GET /v1/timeline/overview?from_us=..&to_us=..`.
pub(crate) async fn overview(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), OVERVIEW_PARAMS) {
        Ok(params) => params,
        Err(problem) => return problem.into_response(),
    };
    let request = match validate(&params) {
        Ok(request) => request,
        Err(problem) => return problem.into_response(),
    };
    let view = state.overview_view();
    let key = overview_key(&view, request);
    serve(state, key, move || render_overview(&view, request)).await
}

/// Serves a cached body or renders and caches one.
///
/// A cache hit returns the retained bytes without spawning a blocking task or
/// touching the analytic semaphore (§14.2). A miss renders off the async
/// runtime, caches the serialized body, and returns it.
async fn serve<R, T>(state: AppState, key: ResponseKey, render: R) -> Response
where
    R: FnOnce() -> Result<T, ApiProblem> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let cache_key = CacheKey::new(key.clone());
    if let Some(bytes) = cache_key
        .as_ref()
        .and_then(|cache_key| state.response_cache.get(cache_key))
    {
        return json_bytes_response(bytes);
    }
    let flight = match state.timeline_flight(&key) {
        TimelineFlightRole::Follower(flight) => flight,
        TimelineFlightRole::Leader(flight) => {
            if let Some(bytes) = cache_key
                .as_ref()
                .and_then(|cache_key| state.response_cache.get(cache_key))
            {
                state.finish_timeline_flight(&key, &flight, Ok(bytes));
            } else {
                let Ok(permit) = state.try_acquire_analytic() else {
                    metrics::counter!("kronika_web_timeline_capacity_rejections_total")
                        .increment(1);
                    state.finish_timeline_flight(
                        &key,
                        &flight,
                        Err(ApiProblem::analytic_capacity_unavailable()),
                    );
                    return match flight.wait().await {
                        Ok(bytes) => json_bytes_response(bytes),
                        Err(problem) => problem.into_response(),
                    };
                };
                let worker_state = state.clone();
                let worker_key = key.clone();
                let worker_flight = Arc::clone(&flight);
                tokio::spawn(async move {
                    let cache = worker_state.response_cache.clone();
                    let render_cache_key = cache_key;
                    let rendered =
                        tokio::task::spawn_blocking(move || -> Result<Arc<[u8]>, ApiProblem> {
                            let _permit = permit;
                            let value = render()?;
                            let bytes: Arc<[u8]> = serde_json::to_vec(&value)
                                .map_err(|_error| ApiProblem::internal_error())?
                                .into();
                            if let Some(render_cache_key) = render_cache_key {
                                cache.insert(render_cache_key, Arc::clone(&bytes));
                            }
                            Ok(bytes)
                        })
                        .await
                        .unwrap_or_else(|_join| Err(ApiProblem::internal_error()));
                    worker_state.finish_timeline_flight(&worker_key, &worker_flight, rendered);
                });
            }
            flight
        }
    };
    match flight.wait().await {
        Ok(bytes) => json_bytes_response(bytes),
        Err(problem) => problem.into_response(),
    }
}

fn json_bytes_response(bytes: Arc<[u8]>) -> Response {
    let body = axum::body::Body::from(bytes::Bytes::from_owner(bytes));
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn overview_key(view: &IndexView, request: OverviewRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Overview,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id: view.fact_set_id(),
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: None,
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: source_filter(&[request.source]),
        page: None,
    }
}

fn validate(params: &QueryParams) -> Result<OverviewRequest, ApiProblem> {
    let source = parse_single_source(params)?;
    let from_us = crate::params::parse_i64(params, QueryParameter::From)?;
    let to_us = crate::params::parse_i64(params, QueryParameter::To)?;
    let Some(range) = CoverageSpan::new(from_us, to_us) else {
        return Err(ApiProblem::invalid_query_constraint(
            crate::problem::QueryConstraint::FromBeforeTo,
        ));
    };
    if to_us.saturating_sub(from_us) > MAX_OVERVIEW_SPAN_US {
        return Err(ApiProblem::query_limit_exceeded(
            crate::problem::LimitResource::QuerySpanUs,
            u64::try_from(MAX_OVERVIEW_SPAN_US).unwrap_or(u64::MAX),
            None,
        ));
    }
    Ok(OverviewRequest {
        range,
        source,
        from_us,
        to_us,
    })
}

fn render_overview(
    view: &IndexView,
    request: OverviewRequest,
) -> Result<OverviewResponseDto, ApiProblem> {
    let result = view
        .query_range(&[request.source], request.range, QUERY_LIMITS)
        .map_err(oracle_problem)?;

    let policy = NotablePolicy::v1();
    let observations = result.observations();
    let digest = event_digest(result.counts(), observations)?;
    let notable_preview = notable_preview_dto(&policy, observations, request)?;
    let meta = timeline_meta(view, request, &[request.source], None)?;
    let covered_duration_us = result.coverage().covered_duration_in(request.range);
    let (health_summary, coverage) =
        crate::overview::health::overview_health_summary(observations, request.range);

    Ok(OverviewResponseDto {
        meta,
        event_digest: digest,
        notable_preview,
        health_summary,
        coverage,
        retained_coverage_duration_us: covered_duration_us,
    })
}

const HEALTH_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Step,
];

/// A validated `/health` request.
#[derive(Debug, Clone, Copy)]
struct HealthRequest {
    range: CoverageSpan,
    source: u64,
    from_us: i64,
    to_us: i64,
    effective_step_us: u64,
}

/// `GET /v1/timeline/health?from=..&to=..&step=..`.
pub(crate) async fn health(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), HEALTH_PARAMS) {
        Ok(params) => params,
        Err(problem) => return problem.into_response(),
    };
    let request = match validate_health(&params) {
        Ok(request) => request,
        Err(problem) => return problem.into_response(),
    };
    let view = state.overview_view();
    let key = health_key(&view, request);
    serve(state, key, move || render_health(&view, request)).await
}

fn validate_health(params: &QueryParams) -> Result<HealthRequest, ApiProblem> {
    let source = parse_single_source(params)?;
    let from_us = crate::params::parse_i64(params, QueryParameter::From)?;
    let to_us = crate::params::parse_i64(params, QueryParameter::To)?;
    let Some(range) = CoverageSpan::new(from_us, to_us) else {
        return Err(ApiProblem::invalid_query_constraint(
            crate::problem::QueryConstraint::FromBeforeTo,
        ));
    };
    if to_us.saturating_sub(from_us) > MAX_OVERVIEW_SPAN_US {
        return Err(ApiProblem::query_limit_exceeded(
            crate::problem::LimitResource::QuerySpanUs,
            u64::try_from(MAX_OVERVIEW_SPAN_US).unwrap_or(u64::MAX),
            None,
        ));
    }
    let requested_step = parse_optional_u64(params, QueryParameter::Step)?;
    let effective_step_us =
        crate::overview::health::effective_step_us(from_us, to_us, requested_step);
    Ok(HealthRequest {
        range,
        source,
        from_us,
        to_us,
        effective_step_us,
    })
}

fn parse_optional_u64(
    params: &QueryParams,
    parameter: QueryParameter,
) -> Result<Option<u64>, ApiProblem> {
    params.get(parameter).map_or(Ok(None), |value| {
        value.parse::<u64>().map(Some).map_err(|_error| {
            ApiProblem::invalid_query_parameter(parameter, crate::problem::ExpectedValue::Uint64)
        })
    })
}

fn health_key(view: &IndexView, request: HealthRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Health,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id: view.fact_set_id(),
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: Some(request.effective_step_us),
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: source_filter(&[request.source]),
        page: None,
    }
}

fn render_health(view: &IndexView, request: HealthRequest) -> Result<Value, ApiProblem> {
    let result = view
        .query_range(&[request.source], request.range, QUERY_LIMITS)
        .map_err(oracle_problem)?;
    let line = crate::overview::health::compute_health(
        result.observations(),
        request.range,
        request.effective_step_us,
    )
    .ok_or_else(ApiProblem::internal_error)?;

    let meta = timeline_meta(
        view,
        OverviewRequest {
            range: request.range,
            source: request.source,
            from_us: request.from_us,
            to_us: request.to_us,
        },
        &[request.source],
        Some(request.effective_step_us),
    )?;
    Ok(json!({
        "meta": meta,
        "health_policy_version": line.policy_version,
        "factor_set_ids": line.factor_set_ids,
        "points": line.points,
        "coverage": line.coverage,
    }))
}

const EVENTS_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Limit,
    QueryParameter::Cursor,
    QueryParameter::MinSeverity,
    QueryParameter::Kind,
];

/// Default and maximum `/events` page size.
const EVENTS_DEFAULT_LIMIT: usize = 100;
const EVENTS_MAX_LIMIT: usize = 1_000;

/// A validated `/events` request.
#[derive(Debug, Clone)]
struct EventsRequest {
    range: CoverageSpan,
    sources: Vec<u64>,
    from_us: i64,
    to_us: i64,
    limit: usize,
    cursor: Option<String>,
    min_severity: Option<Severity>,
    kind: Option<Box<str>>,
}

impl EventsRequest {
    /// The canonical string form of the response filters, for the cache key.
    fn filters(&self) -> String {
        let severity = self.min_severity.map_or_else(
            || "none".to_owned(),
            |value| format!("some:{}", severity_name(value)),
        );
        let kind = self.kind.as_deref().map_or_else(
            || "none".to_owned(),
            |value| format!("some:{}:{value}", value.len()),
        );
        format!(
            "{};limit={};min_severity={severity};kind={kind}",
            source_filter(&self.sources),
            self.limit
        )
    }
}

/// `GET /v1/timeline/events?from=..&to=..&limit=..&cursor=..`.
pub(crate) async fn events(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse_with_repeatable(
        raw.as_deref(),
        EVENTS_PARAMS,
        &[QueryParameter::Source],
    ) {
        Ok(params) => params,
        Err(problem) => return problem.into_response(),
    };
    let request = match validate_events(&params) {
        Ok(request) => request,
        Err(problem) => return problem.into_response(),
    };
    let policy = NotablePolicy::v1();
    let filters = request.filters();
    let query_hash = events_query_hash_bytes(&policy, request.from_us, request.to_us, &filters);
    let source_set_hash = source_set_hash(&request.sources);
    let now_secs = cursor_now_secs();
    state.cursor_registry().prune(now_secs);
    let (view, start_after) = match request.cursor.as_deref() {
        Some(token) => {
            let cursor = match EventsCursor::decode(
                token,
                state.cursor_registry(),
                query_hash,
                source_set_hash,
                now_secs,
            ) {
                Ok(cursor) => cursor,
                Err(error) => return cursor_problem(error).into_response(),
            };
            let view = match state.cursor_registry().resolve(
                cursor.lease.fact_set_id,
                source_set_hash,
                now_secs,
            ) {
                Ok(view) => view,
                Err(error) => return cursor_problem(error).into_response(),
            };
            (view, Some(cursor))
        }
        None => (state.overview_view(), None),
    };
    let key = events_key(&view, &request);
    let cursor_state = state.clone();
    serve(state, key, move || {
        render_events(
            &view,
            &request,
            start_after,
            query_hash,
            source_set_hash,
            &cursor_state,
        )
    })
    .await
}

fn events_key(view: &IndexView, request: &EventsRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Events,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id: view.fact_set_id(),
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: None,
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: request.filters(),
        page: request.cursor.clone(),
    }
}

fn validate_events(params: &QueryParams) -> Result<EventsRequest, ApiProblem> {
    let sources = parse_sources(params)?;
    let from_us = crate::params::parse_i64(params, QueryParameter::From)?;
    let to_us = crate::params::parse_i64(params, QueryParameter::To)?;
    let Some(range) = CoverageSpan::new(from_us, to_us) else {
        return Err(ApiProblem::invalid_query_constraint(
            crate::problem::QueryConstraint::FromBeforeTo,
        ));
    };
    if to_us.saturating_sub(from_us) > MAX_OVERVIEW_SPAN_US {
        return Err(ApiProblem::query_limit_exceeded(
            crate::problem::LimitResource::QuerySpanUs,
            u64::try_from(MAX_OVERVIEW_SPAN_US).unwrap_or(u64::MAX),
            None,
        ));
    }
    let limit = params
        .get(QueryParameter::Limit)
        .map_or(Ok(EVENTS_DEFAULT_LIMIT), |raw| {
            raw.parse::<usize>().map_err(|_error| {
                ApiProblem::invalid_query_parameter(
                    QueryParameter::Limit,
                    crate::problem::ExpectedValue::PositiveInteger,
                )
            })
        })?;
    if limit == 0 {
        return Err(ApiProblem::invalid_query_parameter(
            QueryParameter::Limit,
            crate::problem::ExpectedValue::PositiveInteger,
        ));
    }
    if limit > EVENTS_MAX_LIMIT {
        return Err(ApiProblem::query_limit_exceeded(
            crate::problem::LimitResource::Rows,
            u64::try_from(EVENTS_MAX_LIMIT).unwrap_or(u64::MAX),
            Some(u64::try_from(limit).unwrap_or(u64::MAX)),
        ));
    }
    let cursor = params.get(QueryParameter::Cursor).map(str::to_owned);
    let min_severity = parse_min_severity(params)?;
    let kind = params.get(QueryParameter::Kind).map(Box::from);
    Ok(EventsRequest {
        range,
        sources,
        from_us,
        to_us,
        limit,
        cursor,
        min_severity,
        kind,
    })
}

fn parse_min_severity(params: &QueryParams) -> Result<Option<Severity>, ApiProblem> {
    let Some(value) = params.get(QueryParameter::MinSeverity) else {
        return Ok(None);
    };
    let severity = match value {
        "error" => Severity::Error,
        "fatal" => Severity::Fatal,
        "panic" => Severity::Panic,
        "warning" => Severity::Warning,
        "log" => Severity::Log,
        _ => {
            return Err(ApiProblem::invalid_query_parameter(
                QueryParameter::MinSeverity,
                crate::problem::ExpectedValue::Severity,
            ));
        }
    };
    Ok(Some(severity))
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded page fold, cursor lease and typed response are one atomic projection"
)]
fn render_events(
    view: &Arc<IndexView>,
    request: &EventsRequest,
    start_after: Option<EventsCursor>,
    query_hash: [u8; 32],
    source_set_hash: [u8; 32],
    state: &AppState,
) -> Result<EventsResponseDto, ApiProblem> {
    let result = view
        .query_range(&request.sources, request.range, QUERY_LIMITS)
        .map_err(oracle_problem)?;

    let policy = NotablePolicy::v1();

    let source_metadata = view
        .selected_source_metadata(
            &request.sources,
            request.range,
            QUERY_LIMITS.max_coverage_spans,
        )
        .map_err(|_error| ApiProblem::store_read_failed())?;
    let source_ids_by_scope = source_metadata
        .iter()
        .filter_map(|source| {
            source
                .source_scope_id
                .map(|scope| (scope, source.source_id))
        })
        .map(|(scope, source_id)| {
            (view.source_id_for_scope(scope) == Some(source_id))
                .then_some((scope, source_id))
                .ok_or_else(ApiProblem::store_read_failed)
        })
        .collect::<Result<BTreeMap<SourceScopeId, u64>, _>>()?;
    let observations = result.observations();
    let mut notable = BinaryHeap::with_capacity(request.limit.saturating_add(1));
    let mut omitted_by_response_filter = 0_u64;
    for observation in observations {
        let Some(class) = policy.classify(observation) else {
            continue;
        };
        if !passes_response_filter(observation, request) {
            omitted_by_response_filter = omitted_by_response_filter
                .checked_add(1)
                .ok_or_else(ApiProblem::store_read_failed)?;
            continue;
        }
        let fact_position = EventFactProjection::position(observation, class)
            .ok_or_else(ApiProblem::store_read_failed)?;
        if let Some(cursor) = start_after
            && fact_position
                <= (EventFactPosition {
                    sort_ts_us: cursor.last_ts_us,
                    event_id: cursor.last_event_id,
                    event_instance_id: cursor.last_event_instance_id,
                })
        {
            continue;
        }
        let source_id = source_ids_by_scope
            .get(&observation.source_scope_id())
            .copied()
            .ok_or_else(ApiProblem::store_read_failed)?;
        let candidate = PageCandidate {
            position: fact_position,
            observation,
            class,
            source_id,
        };
        let retained_cap = request.limit.saturating_add(1);
        if notable.len() < retained_cap {
            notable.push(candidate);
        } else if notable
            .peek()
            .is_some_and(|worst| candidate.position < worst.position)
        {
            let _ = notable.pop();
            notable.push(candidate);
        }
    }

    let notable = notable.into_sorted_vec();
    let page_len = notable.len().min(request.limit);
    let page = &notable[..page_len];
    let has_more = notable.len() > page_len;
    let events: Vec<EventFact> = page
        .iter()
        .map(|candidate| {
            EventFactProjection::project(
                candidate.observation,
                candidate.class,
                candidate.source_id,
            )
            .ok_or_else(ApiProblem::store_read_failed)
        })
        .collect::<Result<_, _>>()?;
    let next_cursor = has_more
        .then(|| page.last())
        .flatten()
        .map(|candidate| -> Result<String, CursorError> {
            let lease = start_after.map_or_else(
                || {
                    state.cursor_registry().pin(
                        Arc::clone(view),
                        source_set_hash,
                        cursor_now_secs(),
                    )
                },
                |cursor| Ok(cursor.lease),
            )?;
            Ok(EventsCursor {
                lease,
                source_set_hash,
                query_hash,
                last_ts_us: candidate.position.sort_ts_us,
                last_event_id: candidate.position.event_id,
                last_event_instance_id: candidate.position.event_instance_id,
            }
            .encode(state.cursor_registry()))
        })
        .transpose()
        .map_err(cursor_problem)?;

    let exactness = aggregate_retained_exactness(&source_metadata);
    let completeness = aggregate_source_completeness(&source_metadata);
    let physical_count = aggregate_physical_count(&source_metadata);
    let coverage = result
        .coverage()
        .spans()
        .iter()
        .map(|span| CoverageSpanDto {
            from_us: span.start_us(),
            to_us: span.end_us(),
        })
        .collect::<Vec<_>>();
    let meta = timeline_meta_with_metadata(
        view,
        OverviewRequest {
            range: request.range,
            source: request.sources[0],
            from_us: request.from_us,
            to_us: request.to_us,
        },
        &request.sources,
        None,
        &source_metadata,
    );
    Ok(EventsResponseDto {
        meta,
        notable_policy_version: policy.version(),
        events,
        next_cursor,
        omitted_by_response_filter,
        retained_exactness: retained_exactness_name(exactness),
        source_completeness: source_completeness_name(completeness),
        physical_count_semantics: physical_count_name(physical_count),
        coverage,
    })
}

struct PageCandidate<'a> {
    position: EventFactPosition,
    observation: &'a EventObservation,
    class: NotableClass,
    source_id: u64,
}

impl PartialEq for PageCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.position == other.position
    }
}

impl Eq for PageCandidate<'_> {}

impl PartialOrd for PageCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PageCandidate<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.position.cmp(&other.position)
    }
}

/// Whether an observation passes the response filters (severity, kind).
fn passes_response_filter(observation: &EventObservation, request: &EventsRequest) -> bool {
    if !kronika_analytics::overview::passes_min_severity(observation, request.min_severity) {
        return false;
    }
    request
        .kind
        .as_deref()
        .is_none_or(|kind| observation.payload().kind_code() == kind)
}

fn cursor_problem(error: CursorError) -> ApiProblem {
    match error {
        // A decode/authentication failure or a changed query is the caller's
        // error (400); a pinned generation that is gone is a 410.
        CursorError::Invalid => ApiProblem::invalid_cursor(),
        CursorError::QueryMismatch => ApiProblem::cursor_query_mismatch(),
        CursorError::ViewGone => ApiProblem::view_gone(),
        CursorError::Expired => ApiProblem::cursor_expired(),
        CursorError::CapacityUnavailable => ApiProblem::cursor_capacity_unavailable(),
    }
}

fn oracle_problem(error: OracleError) -> ApiProblem {
    let limit = match error {
        OracleError::LimitExceeded(OracleResource::MaterializedBytes) => Some((
            crate::problem::LimitResource::Bytes,
            QUERY_MATERIALIZED_BYTES,
        )),
        OracleError::LimitExceeded(OracleResource::Observations) => Some((
            crate::problem::LimitResource::Rows,
            QUERY_LIMITS.max_observations,
        )),
        OracleError::LimitExceeded(OracleResource::CoverageSpans) => Some((
            crate::problem::LimitResource::Rows,
            QUERY_LIMITS.max_coverage_spans,
        )),
        OracleError::Counts(CountError::LimitExceeded(CountResource::InputEntries)) => Some((
            crate::problem::LimitResource::Rows,
            QUERY_LIMITS.count_limits.max_input_entries,
        )),
        OracleError::Counts(CountError::LimitExceeded(CountResource::JointKeys)) => Some((
            crate::problem::LimitResource::Rows,
            QUERY_LIMITS.count_limits.max_joint_keys,
        )),
        OracleError::Counts(CountError::LimitExceeded(CountResource::SignalKeys)) => Some((
            crate::problem::LimitResource::Rows,
            QUERY_LIMITS.count_limits.max_signal_keys,
        )),
        OracleError::Counts(CountError::Overflow)
        | OracleError::Source(_)
        | OracleError::ObservationIdCollision => None,
    };
    limit.map_or_else(ApiProblem::store_read_failed, |(resource, limit)| {
        ApiProblem::query_limit_exceeded(resource, crate::problem::count_u64(limit), None)
    })
}

fn timeline_meta(
    view: &IndexView,
    request: OverviewRequest,
    sources: &[u64],
    effective_step_us: Option<u64>,
) -> Result<TimelineMetaDto, ApiProblem> {
    let source_metadata = view
        .selected_source_metadata(sources, request.range, QUERY_LIMITS.max_coverage_spans)
        .map_err(|_error| ApiProblem::store_read_failed())?;
    Ok(timeline_meta_with_metadata(
        view,
        request,
        sources,
        effective_step_us,
        &source_metadata,
    ))
}

fn timeline_meta_with_metadata(
    view: &IndexView,
    request: OverviewRequest,
    sources: &[u64],
    effective_step_us: Option<u64>,
    source_metadata: &[SourceMetadata],
) -> TimelineMetaDto {
    let all_available = source_metadata
        .iter()
        .all(|source| source.source_scope_id.is_some());
    let source_status = if all_available {
        view.source_status().wire_code()
    } else {
        "unavailable"
    };
    let source_freshness = source_metadata
        .iter()
        .map(|source| SourceFreshnessDto {
            source_id: source.source_id,
            source_scope_id: source
                .source_scope_id
                .map(|scope| URL_SAFE_NO_PAD.encode(scope.0)),
            data_through_us: source.data_through_us,
            source_status: if source.source_scope_id.is_some() {
                view.source_status().wire_code()
            } else {
                "unavailable"
            },
            source_completeness: source_completeness_name(source.source_completeness),
            retained_exactness: retained_exactness_name(source.retained_exactness),
            physical_count_semantics: physical_count_name(source.physical_count),
        })
        .collect();
    let loss = source_metadata
        .iter()
        .map(|source| SourceLossDto {
            source_id: source.source_id,
            known_gaps: coverage_dtos(&source.known_gaps),
            dropped_count_lower_bound: source.dropped_lower_bound,
        })
        .collect();
    let available_sources = view
        .source_ids()
        .iter()
        .copied()
        .filter(|source| sources.binary_search(source).is_ok())
        .collect();
    let data_through_us = view.data_through_us_for(sources);
    TimelineMetaDto {
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        view_generation: view.view_generation(),
        fact_set_id: URL_SAFE_NO_PAD.encode(view.fact_set_id()),
        requested_range: CoverageSpanDto {
            from_us: request.from_us,
            to_us: request.to_us,
        },
        effective_range: CoverageSpanDto {
            from_us: request.from_us,
            to_us: request.to_us,
        },
        effective_step_us,
        sources: sources.to_vec(),
        available_sources,
        data_through_us,
        store_data_through_us: view.data_through_us(),
        tail_pending: None,
        source_status,
        source_freshness,
        loss,
    }
}

fn coverage_dtos(coverage: &kronika_analytics::overview::Coverage) -> Vec<CoverageSpanDto> {
    coverage
        .spans()
        .iter()
        .map(|span| CoverageSpanDto {
            from_us: span.start_us(),
            to_us: span.end_us(),
        })
        .collect()
}

fn aggregate_source_completeness(sources: &[SourceMetadata]) -> SourceCompleteness {
    sources
        .iter()
        .fold(SourceCompleteness::Full, |total, source| {
            match (total, source.source_completeness) {
                (SourceCompleteness::Unknown, _) | (_, SourceCompleteness::Unknown) => {
                    SourceCompleteness::Unknown
                }
                (SourceCompleteness::BoundedSubset, _) | (_, SourceCompleteness::BoundedSubset) => {
                    SourceCompleteness::BoundedSubset
                }
                (SourceCompleteness::Full, SourceCompleteness::Full) => SourceCompleteness::Full,
            }
        })
}

fn aggregate_retained_exactness(sources: &[SourceMetadata]) -> RetainedExactness {
    sources
        .iter()
        .fold(RetainedExactness::Exact, |total, source| {
            match (total, source.retained_exactness) {
                (RetainedExactness::Unknown, _) | (_, RetainedExactness::Unknown) => {
                    RetainedExactness::Unknown
                }
                (RetainedExactness::LowerBound, _) | (_, RetainedExactness::LowerBound) => {
                    RetainedExactness::LowerBound
                }
                (RetainedExactness::Exact, RetainedExactness::Exact) => RetainedExactness::Exact,
            }
        })
}

fn aggregate_physical_count(sources: &[SourceMetadata]) -> PhysicalCountSemantics {
    let mut sources = sources.iter();
    let Some(first) = sources.next() else {
        return PhysicalCountSemantics::NotApplicable;
    };
    sources.fold(first.physical_count, |total, source| {
        match (total, source.physical_count) {
            (PhysicalCountSemantics::Unknown, _)
            | (_, PhysicalCountSemantics::Unknown)
            | (PhysicalCountSemantics::Exact, PhysicalCountSemantics::NotApplicable)
            | (PhysicalCountSemantics::NotApplicable, PhysicalCountSemantics::Exact) => {
                PhysicalCountSemantics::Unknown
            }
            (PhysicalCountSemantics::LowerBound, _) | (_, PhysicalCountSemantics::LowerBound) => {
                PhysicalCountSemantics::LowerBound
            }
            (PhysicalCountSemantics::Exact, PhysicalCountSemantics::Exact) => {
                PhysicalCountSemantics::Exact
            }
            (PhysicalCountSemantics::NotApplicable, PhysicalCountSemantics::NotApplicable) => {
                PhysicalCountSemantics::NotApplicable
            }
        }
    })
}

const fn source_completeness_name(completeness: SourceCompleteness) -> &'static str {
    match completeness {
        SourceCompleteness::Full => "full",
        SourceCompleteness::BoundedSubset => "bounded_subset",
        SourceCompleteness::Unknown => "unknown",
    }
}

const fn retained_exactness_name(exactness: RetainedExactness) -> &'static str {
    match exactness {
        RetainedExactness::Exact => "exact",
        RetainedExactness::LowerBound => "lower_bound",
        RetainedExactness::Unknown => "unknown",
    }
}

const fn physical_count_name(semantics: PhysicalCountSemantics) -> &'static str {
    match semantics {
        PhysicalCountSemantics::Exact => "exact",
        PhysicalCountSemantics::LowerBound => "lower_bound",
        PhysicalCountSemantics::Unknown => "unknown",
        PhysicalCountSemantics::NotApplicable => "not_applicable",
    }
}

fn event_digest(
    counts: &EventCounts,
    observations: &[EventObservation],
) -> Result<EventDigestDto, ApiProblem> {
    let projection_error = |_error| ApiProblem::store_read_failed();
    let by_severity = counts.by_severity().map_err(projection_error)?;
    let by_category = counts.by_category().map_err(projection_error)?;
    let retained_error_occurrence_count = counts.total_occurrences().map_err(projection_error)?;
    let retained_error_group_count = u64::try_from(
        observations
            .iter()
            .filter(|observation| {
                matches!(
                    observation.payload(),
                    kronika_analytics::overview::ObservationPayload::ErrorGroup(_)
                )
            })
            .count(),
    )
    .map_err(|_error| ApiProblem::store_read_failed())?;
    let retained_observation_row_count =
        u64::try_from(observations.len()).map_err(|_error| ApiProblem::store_read_failed())?;
    let (by_sqlstate, sqlstate_missing_count, sqlstate_other_count) = sqlstate_top_n(counts)?;
    let (joint_top, joint_other_count) = joint_top_n(counts)?;
    validate_digest_reconciliation(
        retained_error_occurrence_count,
        &by_severity,
        &by_category,
        sqlstate_missing_count,
        sqlstate_other_count,
        &by_sqlstate,
        joint_other_count,
        &joint_top,
    )?;
    let lifecycle = counts.lifecycle();
    Ok(EventDigestDto {
        retained_error_occurrence_count,
        retained_error_group_count,
        retained_observation_row_count,
        by_severity,
        by_category,
        by_sqlstate,
        sqlstate_missing_count,
        sqlstate_other_count,
        joint_top,
        joint_other_count,
        lifecycle: LifecycleDigestDto {
            crashes: lifecycle.crashes(),
            shutdowns: lifecycle.shutdowns(),
            ready: lifecycle.ready(),
            signals: lifecycle
                .signals()
                .iter()
                .map(|(signal, count)| SignalCountDto {
                    signal: *signal,
                    count: *count,
                })
                .collect(),
        },
        exactness: "exact",
    })
}

fn sqlstate_top_n(counts: &EventCounts) -> Result<(Vec<SqlstateCountDto>, u64, u64), ApiProblem> {
    let mut aggregate: BTreeMap<[u8; 5], u64> = BTreeMap::new();
    let mut missing = 0_u64;
    for (key, count) in counts.joint() {
        if let Some(sqlstate) = key.sqlstate {
            let slot = aggregate.entry(sqlstate.0).or_insert(0);
            *slot = slot
                .checked_add(*count)
                .ok_or_else(ApiProblem::store_read_failed)?;
        } else {
            missing = missing
                .checked_add(*count)
                .ok_or_else(ApiProblem::store_read_failed)?;
        }
    }
    let mut total = 0_u64;
    let mut top = Vec::with_capacity(DIGEST_TOP_N);
    for (code, count) in aggregate {
        total = total
            .checked_add(count)
            .ok_or_else(ApiProblem::store_read_failed)?;
        retain_top(&mut top, code, count);
    }
    let top_total = top.iter().try_fold(0_u64, |sum, (_, count)| {
        sum.checked_add(*count)
            .ok_or_else(ApiProblem::store_read_failed)
    })?;
    let other = total
        .checked_sub(top_total)
        .ok_or_else(ApiProblem::store_read_failed)?;
    Ok((
        top.into_iter()
            .map(|(code, count)| SqlstateCountDto {
                code: sqlstate_text(code),
                count,
            })
            .collect(),
        missing,
        other,
    ))
}

fn joint_top_n(counts: &EventCounts) -> Result<(Vec<JointCountDto>, u64), ApiProblem> {
    let mut total = 0_u64;
    let mut top = Vec::with_capacity(DIGEST_TOP_N);
    for (key, count) in counts.joint() {
        total = total
            .checked_add(*count)
            .ok_or_else(ApiProblem::store_read_failed)?;
        retain_top(&mut top, *key, *count);
    }
    let top_total = top.iter().try_fold(0_u64, |sum, (_, count)| {
        sum.checked_add(*count)
            .ok_or_else(ApiProblem::store_read_failed)
    })?;
    let other = total
        .checked_sub(top_total)
        .ok_or_else(ApiProblem::store_read_failed)?;
    let top = top
        .into_iter()
        .map(|(key, count)| JointCountDto {
            severity: severity_name(key.severity),
            category: category_name(key.category),
            sqlstate: key.sqlstate.map(|code| sqlstate_text(code.0)),
            count,
        })
        .collect();
    Ok((top, other))
}

fn retain_top<K: Ord>(top: &mut Vec<(K, u64)>, key: K, count: u64) {
    let index = top.partition_point(|(existing_key, existing_count)| {
        *existing_count > count || (*existing_count == count && existing_key < &key)
    });
    if index < DIGEST_TOP_N {
        top.insert(index, (key, count));
        top.truncate(DIGEST_TOP_N);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the arguments are the independent published reconciliation axes"
)]
fn validate_digest_reconciliation(
    total: u64,
    by_severity: &[u64; 5],
    by_category: &[u64; 11],
    sqlstate_missing: u64,
    sqlstate_other: u64,
    sqlstate_top: &[SqlstateCountDto],
    joint_other: u64,
    joint_top: &[JointCountDto],
) -> Result<(), ApiProblem> {
    let sum = |values: &[u64]| {
        values.iter().try_fold(0_u64, |acc, value| {
            acc.checked_add(*value)
                .ok_or_else(ApiProblem::store_read_failed)
        })
    };
    let sqlstate_top_total = sqlstate_top.iter().try_fold(0_u64, |acc, value| {
        acc.checked_add(value.count)
            .ok_or_else(ApiProblem::store_read_failed)
    })?;
    let sqlstate_total = sqlstate_missing
        .checked_add(sqlstate_other)
        .and_then(|partial| partial.checked_add(sqlstate_top_total))
        .ok_or_else(ApiProblem::store_read_failed)?;
    let joint_total = joint_other
        .checked_add(joint_top.iter().try_fold(0_u64, |acc, value| {
            acc.checked_add(value.count)
                .ok_or_else(ApiProblem::store_read_failed)
        })?)
        .ok_or_else(ApiProblem::store_read_failed)?;
    if sum(by_severity)? != total
        || sum(by_category)? != total
        || sqlstate_total != total
        || joint_total != total
    {
        return Err(ApiProblem::store_read_failed());
    }
    Ok(())
}

fn notable_preview_dto(
    policy: &NotablePolicy,
    observations: &[EventObservation],
    request: OverviewRequest,
) -> Result<NotablePreviewDto, ApiProblem> {
    let mut total = 0_u64;
    let mut selected = BinaryHeap::with_capacity(policy.response_cap());
    for observation in observations {
        let Some(class) = policy.classify(observation) else {
            continue;
        };
        total = total
            .checked_add(1)
            .ok_or_else(ApiProblem::store_read_failed)?;
        let candidate = PageCandidate {
            position: EventFactProjection::position(observation, class)
                .ok_or_else(ApiProblem::store_read_failed)?,
            observation,
            class,
            source_id: request.source,
        };
        if selected.len() < policy.response_cap() {
            selected.push(candidate);
        } else if selected
            .peek()
            .is_some_and(|worst| candidate.position < worst.position)
        {
            let _ = selected.pop();
            selected.push(candidate);
        }
    }
    let selected = selected.into_sorted_vec();
    let retained =
        u64::try_from(selected.len()).map_err(|_error| ApiProblem::store_read_failed())?;
    let observations = selected
        .into_iter()
        .map(|candidate| {
            EventFactProjection::project(
                candidate.observation,
                candidate.class,
                candidate.source_id,
            )
            .ok_or_else(ApiProblem::store_read_failed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NotablePreviewDto {
        observations,
        omitted_count: total
            .checked_sub(retained)
            .ok_or_else(ApiProblem::store_read_failed)?,
        events_query_hash: events_query_hash(policy, request),
    })
}

fn events_query_hash_bytes(
    policy: &NotablePolicy,
    from_us: i64,
    to_us: i64,
    filters: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pgk-overview-events-query-v1");
    hasher.update(from_us.to_le_bytes());
    hasher.update(to_us.to_le_bytes());
    hasher.update(policy.version().to_le_bytes());
    hasher.update(RESPONSE_SCHEMA_VERSION.to_le_bytes());
    hasher.update(kronika_analytics::overview::REDACTION_POLICY_VERSION.to_le_bytes());
    hasher.update(filters.as_bytes());
    hasher.finalize().into()
}

fn source_set_hash(sources: &[u64]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pgk-overview-source-set-v1");
    hasher.update(
        u64::try_from(sources.len())
            .expect("query parameter bound fits u64")
            .to_le_bytes(),
    );
    for source in sources {
        hasher.update(source.to_le_bytes());
    }
    hasher.finalize().into()
}

fn cursor_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The filter string a default `/events` request produces, so the overview
/// preview's `events_query_hash` matches an unfiltered first page.
fn default_events_filters(source: u64) -> String {
    format!("source={source};limit={EVENTS_DEFAULT_LIMIT};min_severity=none;kind=none")
}

fn events_query_hash(policy: &NotablePolicy, request: OverviewRequest) -> String {
    let hash = events_query_hash_bytes(
        policy,
        request.from_us,
        request.to_us,
        &default_events_filters(request.source),
    );
    URL_SAFE_NO_PAD.encode(hash)
}

impl IndexView {
    /// Queries the range through the merged oracle.
    fn query_range(
        &self,
        sources: &[u64],
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<kronika_analytics::overview::OracleResult, OracleError> {
        self.query_sources(sources, range, limits, QUERY_MATERIALIZED_BYTES)
    }
}

fn parse_single_source(params: &QueryParams) -> Result<u64, ApiProblem> {
    let sources = parse_sources(params)?;
    if sources.len() != 1 {
        return Err(ApiProblem::invalid_query_parameter(
            QueryParameter::Source,
            crate::problem::ExpectedValue::Uint64,
        ));
    }
    Ok(sources[0])
}

fn parse_sources(params: &QueryParams) -> Result<Vec<u64>, ApiProblem> {
    let values = params.values(QueryParameter::Source);
    if values.is_empty() {
        return Err(ApiProblem::missing_query_parameter(QueryParameter::Source));
    }
    let mut sources = values
        .iter()
        .map(|value| {
            value.parse::<u64>().map_err(|_error| {
                ApiProblem::invalid_query_parameter(
                    QueryParameter::Source,
                    crate::problem::ExpectedValue::Uint64,
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    sources.sort_unstable();
    sources.dedup();
    Ok(sources)
}

fn source_filter(sources: &[u64]) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("source=");
    for (index, source) in sources.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        let _ = write!(out, "{source}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        DIGEST_TOP_N, EVENTS_DEFAULT_LIMIT, EventsRequest, QUERY_MATERIALIZED_BYTES,
        aggregate_physical_count, default_events_filters, joint_top_n, oracle_problem,
        sqlstate_top_n,
    };
    use crate::overview::view::SourceMetadata;
    use crate::problem::ProblemCode;
    use kronika_analytics::overview::{
        CountLimits, Coverage, CoverageSpan, ErrorCategory, EventCounts, JointErrorKey,
        LifecycleCounts, OracleError, OracleResource, PhysicalCountSemantics, RetainedExactness,
        Severity, SourceCompleteness, SqlState,
    };

    const LIMITS: CountLimits = CountLimits {
        max_input_entries: 4096,
        max_joint_keys: 4096,
        max_signal_keys: 64,
    };

    #[test]
    fn the_default_events_filter_string_matches_an_unfiltered_request() {
        // The overview preview's events_query_hash uses `default_events_filters`;
        // it must equal the filter string a default `/events` request produces,
        // or a first-page cursor would not validate against the preview hint.
        let request = EventsRequest {
            range: CoverageSpan::new(0, 1).expect("valid range"),
            sources: vec![7],
            from_us: 0,
            to_us: 1,
            limit: EVENTS_DEFAULT_LIMIT,
            cursor: None,
            min_severity: None,
            kind: None,
        };
        assert_eq!(request.filters(), default_events_filters(7));
    }

    #[test]
    fn an_absent_kind_cannot_alias_a_literal_asterisk_filter() {
        let base = EventsRequest {
            range: CoverageSpan::new(0, 1).expect("valid range"),
            sources: vec![7],
            from_us: 0,
            to_us: 1,
            limit: EVENTS_DEFAULT_LIMIT,
            cursor: None,
            min_severity: None,
            kind: None,
        };
        let literal = EventsRequest {
            kind: Some(Box::from("*")),
            ..base.clone()
        };
        assert_ne!(base.filters(), literal.filters());
    }

    fn source_with_physical_count(physical_count: PhysicalCountSemantics) -> SourceMetadata {
        SourceMetadata {
            source_id: 7,
            source_scope_id: None,
            data_through_us: None,
            covered: Coverage::empty(),
            known_gaps: Coverage::empty(),
            source_completeness: SourceCompleteness::Unknown,
            retained_exactness: RetainedExactness::Unknown,
            physical_count,
            dropped_lower_bound: None,
        }
    }

    #[test]
    fn physical_count_reduction_has_no_sentinel_alias() {
        let exact = source_with_physical_count(PhysicalCountSemantics::Exact);
        let not_applicable = source_with_physical_count(PhysicalCountSemantics::NotApplicable);
        assert_eq!(
            aggregate_physical_count(std::slice::from_ref(&exact)),
            PhysicalCountSemantics::Exact
        );
        assert_eq!(
            aggregate_physical_count(std::slice::from_ref(&not_applicable)),
            PhysicalCountSemantics::NotApplicable
        );
        assert_eq!(
            aggregate_physical_count(&[exact, not_applicable]),
            PhysicalCountSemantics::Unknown
        );
    }

    #[test]
    fn materialized_byte_overflow_maps_to_a_typed_413_problem() {
        let problem = oracle_problem(OracleError::LimitExceeded(
            OracleResource::MaterializedBytes,
        ));
        assert_eq!(problem.code(), ProblemCode::QueryLimitExceeded);
        let value = serde_json::to_value(problem).expect("serialize problem");
        assert_eq!(value["status"], 413);
        assert_eq!(value["params"]["resource"], "bytes");
        assert_eq!(
            value["params"]["limit"],
            u64::try_from(QUERY_MATERIALIZED_BYTES).expect("fixed bound fits u64")
        );
        assert!(value["params"].get("observed").is_none());
    }

    fn sqlstate(index: usize) -> SqlState {
        let mut code = [b'0'; 5];
        for (slot, digit) in code.iter_mut().rev().zip(numeral_digits(index)) {
            *slot = digit;
        }
        SqlState(code)
    }

    fn numeral_digits(mut value: usize) -> Vec<u8> {
        let mut digits = Vec::new();
        loop {
            digits.push(b'0' + u8::try_from(value % 10).expect("digit fits"));
            value /= 10;
            if value == 0 {
                break;
            }
        }
        digits
    }

    /// Builds `n` distinct-sqlstate keys whose counts descend `n, n-1, .. 1`.
    fn descending_counts(n: usize) -> EventCounts {
        let entries: Vec<(JointErrorKey, u64)> = (0..n)
            .map(|index| {
                let key = JointErrorKey {
                    severity: Severity::Error,
                    category: ErrorCategory::Other,
                    sqlstate: Some(sqlstate(index)),
                };
                (key, u64::try_from(n - index).expect("count fits"))
            })
            .collect();
        EventCounts::from_joint(entries, LifecycleCounts::default(), LIMITS)
            .expect("bounded counts")
    }

    #[test]
    fn sqlstate_projection_keeps_the_top_n_and_buckets_the_remainder() {
        let counts = descending_counts(20);
        let (top, missing, other) = sqlstate_top_n(&counts).expect("projection");
        assert_eq!(top.len(), DIGEST_TOP_N, "the sparse dimension is capped");
        assert_eq!(top[0].count, 20, "the highest count ranks first");
        assert_eq!(missing, 0);
        // Ranks 16..20 fall into the other bucket: counts 4 + 3 + 2 + 1.
        assert_eq!(other, 10, "dropped counts are summed, not discarded");
    }

    #[test]
    fn joint_projection_ranks_by_count_then_code() {
        let counts = descending_counts(3);
        let (joint, other) = joint_top_n(&counts).expect("projection");
        assert_eq!(joint.len(), 3);
        assert_eq!(joint[0].count, 3);
        assert_eq!(joint[2].count, 1);
        assert_eq!(other, 0);
    }

    #[test]
    fn a_count_tie_breaks_on_ascending_code() {
        let low = JointErrorKey {
            severity: Severity::Error,
            category: ErrorCategory::Other,
            sqlstate: Some(SqlState(*b"00001")),
        };
        let high = JointErrorKey {
            severity: Severity::Error,
            category: ErrorCategory::Other,
            sqlstate: Some(SqlState(*b"00002")),
        };
        let counts =
            EventCounts::from_joint([(high, 5), (low, 5)], LifecycleCounts::default(), LIMITS)
                .expect("bounded counts");
        let (top, _missing, _other) = sqlstate_top_n(&counts).expect("projection");
        assert_eq!(top[0].code, "00001", "equal counts order by ascending code");
    }
}
