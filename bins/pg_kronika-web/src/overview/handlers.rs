//! Axum handlers for the three timeline projections.
//!
//! Each first-page request validates and admits a descriptor plan before
//! response-level work. Rendering then uses the selected immutable fact view;
//! counts, notable selection, and coverage remain in `kronika-analytics`.

use std::collections::{BTreeMap, BinaryHeap};
use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::overview::{
    CountError, CountLimits, CountResource, CoverageSpan, EventCounts,
    EventFact as CanonicalEventFact, EventObservation, EventPayload as CanonicalEventPayload,
    NotableClass, NotablePolicy, OracleError, OracleLimits, OracleResource, PhysicalCountSemantics,
    RetainedExactness, Severity, SourceCompleteness,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::api_error::{ApiError, QueryParameter};
use crate::api_response::HealthResponse;
use crate::overview::cache::{CacheKey, Endpoint, ResponseKey};
use crate::overview::cursor::{CursorError, EventsCursor};
use crate::overview::dto::{
    CoverageSpanDto, EventDigestDto, EventFact, EventFactPosition, EventFactProjection,
    EventsResponseDto, FreshnessDto, JointCountDto, LifecycleDigestDto, LossDto, NotablePreviewDto,
    OverviewResponseDto, SignalCountDto, SqlstateCountDto, TailPendingDto, TimelineMetaDto,
    category_name, severity_name, sqlstate_text,
};
use crate::overview::loader::FactLoadFailure;
use crate::overview::selection::{SelectedSealedPlan, SelectionError};
use crate::overview::view::{CanonicalFactQueryError, IndexView, TimelineMetadata, TimelineStatus};
use crate::params::QueryParams;
use crate::{AppState, TimelineFlightRole};

/// Absolute query span for the overview endpoint: 31 days.
const MAX_OVERVIEW_SPAN_US: i64 = 31 * 24 * 3_600 * 1_000_000;

/// Response schema version echoed into every timeline response.
const RESPONSE_SCHEMA_VERSION: u32 = kronika_analytics::overview::RESPONSE_SCHEMA_VERSION;

/// Health policy version bound into response cache keys.
const HEALTH_POLICY_VERSION: u32 = kronika_analytics::overview::HEALTH_POLICY_VERSION;

/// Top-N sparse dimensions kept in the digest projection.
const DIGEST_TOP_N: usize = 16;

const OVERVIEW_PARAMS: &[QueryParameter] = &[QueryParameter::From, QueryParameter::To];

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
const MAX_FACTOR_COVERAGE_RECORDS: usize = 65_536;

/// A validated overview request range.
#[derive(Debug, Clone, Copy)]
struct OverviewRequest {
    range: CoverageSpan,
    from_us: i64,
    to_us: i64,
}

/// `GET /v1/timeline/overview?from_us=..&to_us=..`.
#[utoipa::path(
    get,
    path = "/v1/timeline/overview",
    tag = "timeline",
    params(
        ("from" = i64, Query),
        ("to" = i64, Query),
    ),
    responses(
        (status = 200, description = "Timeline overview", body = OverviewResponseDto),
        (status = 400, description = "Invalid query", body = ApiError),
        (status = 401, description = "Authentication required", body = ApiError),
        (status = 413, description = "Timeline query limit exceeded", body = ApiError),
        (status = 503, description = "Cold-build capacity unavailable", body = ApiError),
        (status = 500, description = "Timeline read or render failed", body = ApiError),
    )
)]
pub(crate) async fn overview(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), OVERVIEW_PARAMS) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let request = match validate(&params) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let (snapshot, view) = state.overview_request_view();
    let plan = match select_plan(&state, view, request.range) {
        Ok(plan) => plan,
        Err(error) => return error.into_response(),
    };
    let key = overview_key(plan.fact_set_id(), request);
    serve(
        state,
        key,
        TimelineViewSource::Selected { snapshot, plan },
        move |loaded| render_overview(loaded, request),
    )
    .await
}

enum TimelineViewSource {
    Selected {
        snapshot: Arc<kronika_reader::LocalDirSnapshot>,
        plan: SelectedSealedPlan,
    },
    Loaded(Arc<IndexView>),
}

async fn serve<R, T>(
    state: AppState,
    key: ResponseKey,
    source: TimelineViewSource,
    render: R,
) -> Response
where
    R: FnOnce(&Arc<IndexView>) -> Result<T, ApiError> + Send + 'static,
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
                let worker_state = state.clone();
                let worker_key = key.clone();
                let worker_flight = Arc::clone(&flight);
                tokio::spawn(async move {
                    let loaded = match source {
                        TimelineViewSource::Selected { snapshot, plan } => worker_state
                            .load_overview_selection(snapshot, &plan)
                            .await
                            .map_err(fact_load_error),
                        TimelineViewSource::Loaded(view) => Ok(view),
                    };
                    let loaded = match loaded {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            worker_state.finish_timeline_flight(
                                &worker_key,
                                &worker_flight,
                                Err(error),
                            );
                            return;
                        }
                    };
                    let cacheable = loaded.fact_set_id() == worker_key.fact_set_id;
                    let permit = match worker_state.acquire_analytic().await {
                        Ok(permit) => permit,
                        Err(error) => {
                            metrics::counter!("kronika_web_timeline_capacity_rejections_total")
                                .increment(1);
                            worker_state.finish_timeline_flight(
                                &worker_key,
                                &worker_flight,
                                Err(error),
                            );
                            return;
                        }
                    };
                    let cache = worker_state.response_cache.clone();
                    let render_cache_key = cacheable.then_some(cache_key).flatten();
                    let rendered =
                        tokio::task::spawn_blocking(move || -> Result<Arc<[u8]>, ApiError> {
                            let _permit = permit;
                            let value = render(&loaded)?;
                            let bytes: Arc<[u8]> = serde_json::to_vec(&value)
                                .map_err(|_error| ApiError::internal_error())?
                                .into();
                            if let Some(render_cache_key) = render_cache_key {
                                cache.insert(render_cache_key, Arc::clone(&bytes));
                            }
                            Ok(bytes)
                        })
                        .await
                        .unwrap_or_else(|_join| Err(ApiError::internal_error()));
                    worker_state.finish_timeline_flight(&worker_key, &worker_flight, rendered);
                });
            }
            flight
        }
    };
    match flight.wait().await {
        Ok(bytes) => json_bytes_response(bytes),
        Err(error) => error.into_response(),
    }
}

fn json_bytes_response(bytes: Arc<[u8]>) -> Response {
    let body = axum::body::Body::from(bytes::Bytes::from_owner(bytes));
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn select_plan(
    state: &AppState,
    view: Arc<crate::overview::view::DescriptorView>,
    range: CoverageSpan,
) -> Result<SelectedSealedPlan, ApiError> {
    state
        .select_overview(view, range)
        .map_err(|error| match error {
            SelectionError::LimitExceeded { limit } => {
                metrics::counter!(
                    "kronika_web_timeline_query_limit_rejections_total",
                    "resource" => "selected_segments"
                )
                .increment(1);
                ApiError::query_shape_limit_exceeded(
                    crate::api_error::LimitResource::SelectedSegments,
                    crate::api_error::count_u64(limit),
                    None,
                )
            }
            SelectionError::InvalidLimit => ApiError::internal_error(),
        })
}

fn fact_load_error(error: FactLoadFailure) -> ApiError {
    match error {
        FactLoadFailure::ColdBuildOverloaded {
            retry_after_seconds,
            reason,
        } => {
            metrics::counter!(
                "kronika_web_overview_cold_work_rejections_total",
                "reason" => reason
            )
            .increment(1);
            ApiError::cold_build_overloaded(retry_after_seconds)
        }
        FactLoadFailure::Source(_error) => ApiError::store_read_failed(),
        FactLoadFailure::WorkerFailed | FactLoadFailure::IdentityMismatch => {
            ApiError::internal_error()
        }
    }
}

const fn overview_key(fact_set_id: [u8; 32], request: OverviewRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Overview,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id,
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: None,
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: String::new(),
        page: None,
    }
}

fn validate(params: &QueryParams) -> Result<OverviewRequest, ApiError> {
    let from_us = crate::params::parse_i64(params, QueryParameter::From)?;
    let to_us = crate::params::parse_i64(params, QueryParameter::To)?;
    let Some(range) = CoverageSpan::new(from_us, to_us) else {
        return Err(ApiError::invalid_query_constraint(
            crate::api_error::QueryConstraint::FromBeforeTo,
        ));
    };
    if to_us.saturating_sub(from_us) > MAX_OVERVIEW_SPAN_US {
        return Err(ApiError::query_limit_exceeded(
            crate::api_error::LimitResource::QuerySpanUs,
            u64::try_from(MAX_OVERVIEW_SPAN_US).unwrap_or(u64::MAX),
            None,
        ));
    }
    Ok(OverviewRequest {
        range,
        from_us,
        to_us,
    })
}

fn render_overview(
    view: &IndexView,
    request: OverviewRequest,
) -> Result<OverviewResponseDto, ApiError> {
    let result = view
        .query_range(request.range, QUERY_LIMITS, QUERY_MATERIALIZED_BYTES)
        .map_err(oracle_error)?;

    let policy = NotablePolicy::v1();
    let observations = result.observations();
    let digest = event_digest(result.counts(), observations)?;
    let notable_preview = notable_preview_dto(&policy, observations, request)?;
    let meta = timeline_meta(view, request, None)?;
    let covered_duration_us = result.coverage().covered_duration_in(request.range);
    let (health_summary, _policy_coverage) =
        crate::overview::health::overview_health_summary(observations, request.range);
    let coverage = Value::Array(
        view.query_factor_coverage(request.range, MAX_FACTOR_COVERAGE_RECORDS)
            .map_err(canonical_fact_error)?
            .iter()
            .map(crate::overview::health::factor_coverage_json)
            .collect(),
    );

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
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Step,
];

/// A validated `/health` request.
#[derive(Debug, Clone, Copy)]
struct HealthRequest {
    range: CoverageSpan,
    from_us: i64,
    to_us: i64,
    effective_step_us: u64,
}

/// `GET /v1/timeline/health?from=..&to=..&step=..`.
#[utoipa::path(
    get,
    path = "/v1/timeline/health",
    tag = "timeline",
    params(
        ("from" = i64, Query),
        ("to" = i64, Query),
        ("step" = Option<u64>, Query),
    ),
    responses(
        (status = 200, description = "Timeline health line", body = HealthResponse),
        (status = 400, description = "Invalid query", body = ApiError),
        (status = 401, description = "Authentication required", body = ApiError),
        (status = 413, description = "Timeline query limit exceeded", body = ApiError),
        (status = 503, description = "Cold-build capacity unavailable", body = ApiError),
        (status = 500, description = "Timeline read or render failed", body = ApiError),
    )
)]
pub(crate) async fn health(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), HEALTH_PARAMS) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let request = match validate_health(&params) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let (snapshot, view) = state.overview_request_view();
    let plan = match select_plan(&state, view, request.range) {
        Ok(plan) => plan,
        Err(error) => return error.into_response(),
    };
    let key = health_key(plan.fact_set_id(), request);
    serve(
        state,
        key,
        TimelineViewSource::Selected { snapshot, plan },
        move |loaded| render_health(loaded, request),
    )
    .await
}

fn validate_health(params: &QueryParams) -> Result<HealthRequest, ApiError> {
    let from_us = crate::params::parse_i64(params, QueryParameter::From)?;
    let to_us = crate::params::parse_i64(params, QueryParameter::To)?;
    let Some(range) = CoverageSpan::new(from_us, to_us) else {
        return Err(ApiError::invalid_query_constraint(
            crate::api_error::QueryConstraint::FromBeforeTo,
        ));
    };
    if to_us.saturating_sub(from_us) > MAX_OVERVIEW_SPAN_US {
        return Err(ApiError::query_limit_exceeded(
            crate::api_error::LimitResource::QuerySpanUs,
            u64::try_from(MAX_OVERVIEW_SPAN_US).unwrap_or(u64::MAX),
            None,
        ));
    }
    let requested_step = parse_optional_u64(params, QueryParameter::Step)?;
    let effective_step_us =
        crate::overview::health::effective_step_us(from_us, to_us, requested_step);
    Ok(HealthRequest {
        range,
        from_us,
        to_us,
        effective_step_us,
    })
}

fn parse_optional_u64(
    params: &QueryParams,
    parameter: QueryParameter,
) -> Result<Option<u64>, ApiError> {
    params.get(parameter).map_or(Ok(None), |value| {
        value.parse::<u64>().map(Some).map_err(|_error| {
            ApiError::invalid_query_parameter(parameter, crate::api_error::ExpectedValue::Uint64)
        })
    })
}

const fn health_key(fact_set_id: [u8; 32], request: HealthRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Health,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id,
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: Some(request.effective_step_us),
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: String::new(),
        page: None,
    }
}

fn render_health(view: &IndexView, request: HealthRequest) -> Result<Value, ApiError> {
    let result = view
        .query_range(request.range, QUERY_LIMITS, QUERY_MATERIALIZED_BYTES)
        .map_err(oracle_error)?;
    let line = crate::overview::health::compute_health(
        result.observations(),
        request.range,
        request.effective_step_us,
    )
    .ok_or_else(ApiError::internal_error)?;
    let coverage = view
        .query_factor_coverage(request.range, MAX_FACTOR_COVERAGE_RECORDS)
        .map_err(canonical_fact_error)?
        .iter()
        .map(crate::overview::health::factor_coverage_json)
        .collect::<Vec<_>>();

    let meta = timeline_meta(
        view,
        OverviewRequest {
            range: request.range,
            from_us: request.from_us,
            to_us: request.to_us,
        },
        Some(request.effective_step_us),
    )?;
    Ok(json!({
        "meta": meta,
        "health_policy_version": line.policy_version,
        "factor_set_ids": line.factor_set_ids,
        "points": line.points,
        "coverage": coverage,
    }))
}

const EVENTS_PARAMS: &[QueryParameter] = &[
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
        format!("limit={};min_severity={severity};kind={kind}", self.limit)
    }
}

/// `GET /v1/timeline/events?from=..&to=..&limit=..&cursor=..`.
#[utoipa::path(
    get,
    path = "/v1/timeline/events",
    tag = "timeline",
    params(
        ("from" = i64, Query),
        ("to" = i64, Query),
        ("limit" = Option<usize>, Query),
        ("cursor" = Option<String>, Query),
        ("min_severity" = Option<String>, Query, example = "warning"),
        ("kind" = Option<String>, Query, example = "pg.database.deadlock_delta"),
    ),
    responses(
        (status = 200, description = "Paginated notable timeline events", body = EventsResponseDto),
        (status = 400, description = "Invalid query or cursor", body = ApiError),
        (status = 401, description = "Authentication required", body = ApiError),
        (status = 410, description = "Cursor expired or retained view is gone", body = ApiError),
        (status = 413, description = "Timeline query limit exceeded", body = ApiError),
        (status = 503, description = "Cursor or cold-build capacity unavailable", body = ApiError),
        (status = 500, description = "Timeline read or render failed", body = ApiError),
    )
)]
pub(crate) async fn events(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), EVENTS_PARAMS) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let request = match validate_events(&params) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let policy = NotablePolicy::v1();
    let filters = request.filters();
    let query_hash = events_query_hash_bytes(&policy, request.from_us, request.to_us, &filters);
    let now_secs = cursor_now_secs();
    state.cursor_registry().prune(now_secs);
    let (view_source, start_after, fact_set_id) = if let Some(token) = request.cursor.as_deref() {
        let cursor =
            match EventsCursor::decode(token, state.cursor_registry(), query_hash, now_secs) {
                Ok(cursor) => cursor,
                Err(error) => return cursor_error(error).into_response(),
            };
        let view = match state
            .cursor_registry()
            .resolve(cursor.lease.fact_set_id, now_secs)
        {
            Ok(view) => view,
            Err(error) => return cursor_error(error).into_response(),
        };
        let fact_set_id = view.fact_set_id();
        (TimelineViewSource::Loaded(view), Some(cursor), fact_set_id)
    } else {
        let (snapshot, view) = state.overview_request_view();
        let plan = match select_plan(&state, view, request.range) {
            Ok(plan) => plan,
            Err(error) => return error.into_response(),
        };
        let fact_set_id = plan.fact_set_id();
        (
            TimelineViewSource::Selected { snapshot, plan },
            None,
            fact_set_id,
        )
    };
    let key = events_key(fact_set_id, &request);
    let cursor_state = state.clone();
    serve(state, key, view_source, move |view| {
        render_events(view, &request, start_after, query_hash, &cursor_state)
    })
    .await
}

fn events_key(fact_set_id: [u8; 32], request: &EventsRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Events,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id,
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: None,
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: request.filters(),
        page: request.cursor.clone(),
    }
}

fn validate_events(params: &QueryParams) -> Result<EventsRequest, ApiError> {
    let from_us = crate::params::parse_i64(params, QueryParameter::From)?;
    let to_us = crate::params::parse_i64(params, QueryParameter::To)?;
    let Some(range) = CoverageSpan::new(from_us, to_us) else {
        return Err(ApiError::invalid_query_constraint(
            crate::api_error::QueryConstraint::FromBeforeTo,
        ));
    };
    if to_us.saturating_sub(from_us) > MAX_OVERVIEW_SPAN_US {
        return Err(ApiError::query_limit_exceeded(
            crate::api_error::LimitResource::QuerySpanUs,
            u64::try_from(MAX_OVERVIEW_SPAN_US).unwrap_or(u64::MAX),
            None,
        ));
    }
    let limit = params
        .get(QueryParameter::Limit)
        .map_or(Ok(EVENTS_DEFAULT_LIMIT), |raw| {
            raw.parse::<usize>().map_err(|_error| {
                ApiError::invalid_query_parameter(
                    QueryParameter::Limit,
                    crate::api_error::ExpectedValue::PositiveInteger,
                )
            })
        })?;
    if limit == 0 {
        return Err(ApiError::invalid_query_parameter(
            QueryParameter::Limit,
            crate::api_error::ExpectedValue::PositiveInteger,
        ));
    }
    if limit > EVENTS_MAX_LIMIT {
        return Err(ApiError::query_limit_exceeded(
            crate::api_error::LimitResource::Rows,
            u64::try_from(EVENTS_MAX_LIMIT).unwrap_or(u64::MAX),
            Some(u64::try_from(limit).unwrap_or(u64::MAX)),
        ));
    }
    let cursor = params.get(QueryParameter::Cursor).map(str::to_owned);
    let min_severity = parse_min_severity(params)?;
    let kind = params.get(QueryParameter::Kind).map(Box::from);
    Ok(EventsRequest {
        range,
        from_us,
        to_us,
        limit,
        cursor,
        min_severity,
        kind,
    })
}

fn parse_min_severity(params: &QueryParams) -> Result<Option<Severity>, ApiError> {
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
            return Err(ApiError::invalid_query_parameter(
                QueryParameter::MinSeverity,
                crate::api_error::ExpectedValue::Severity,
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
    state: &AppState,
) -> Result<EventsResponseDto, ApiError> {
    let result = view
        .query_range(request.range, QUERY_LIMITS, QUERY_MATERIALIZED_BYTES)
        .map_err(oracle_error)?;

    let policy = NotablePolicy::v1();

    let metadata = view
        .metadata(request.range, QUERY_LIMITS.max_coverage_spans)
        .map_err(|_error| ApiError::store_read_failed())?;
    let observations = result.observations();
    let canonical_facts = view
        .query_canonical_facts(
            request.range,
            QUERY_LIMITS.max_observations,
            QUERY_LIMITS.max_observations,
        )
        .map_err(canonical_fact_error)?;
    let mut notable = BinaryHeap::with_capacity(request.limit.saturating_add(1));
    let mut omitted_by_response_filter = 0_u64;
    for observation in observations {
        let Some(class) = policy.classify(observation) else {
            continue;
        };
        if !passes_response_filter(observation, request) {
            omitted_by_response_filter = omitted_by_response_filter
                .checked_add(1)
                .ok_or_else(ApiError::store_read_failed)?;
            continue;
        }
        let fact_position = EventFactProjection::position(observation, class)
            .ok_or_else(ApiError::store_read_failed)?;
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
        let candidate = PageCandidate {
            position: fact_position,
            source: PageCandidateSource::Observation(observation),
            class,
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
    for fact in canonical_facts.iter().filter(|fact| {
        matches!(
            fact.payload(),
            CanonicalEventPayload::CounterDelta(_)
                | CanonicalEventPayload::StateTransition(_)
                | CanonicalEventPayload::Capacity(_)
                | CanonicalEventPayload::Marker
        )
    }) {
        let Some(class) = policy.classify_fact(fact) else {
            continue;
        };
        if !passes_canonical_response_filter(fact, request) {
            omitted_by_response_filter = omitted_by_response_filter
                .checked_add(1)
                .ok_or_else(ApiError::store_read_failed)?;
            continue;
        }
        let position = EventFactProjection::canonical_position(fact);
        if let Some(cursor) = start_after
            && position
                <= (EventFactPosition {
                    sort_ts_us: cursor.last_ts_us,
                    event_id: cursor.last_event_id,
                    event_instance_id: cursor.last_event_instance_id,
                })
        {
            continue;
        }
        let candidate = PageCandidate {
            position,
            source: PageCandidateSource::Canonical(fact),
            class,
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
            match candidate.source {
                PageCandidateSource::Observation(observation) => {
                    EventFactProjection::project(observation, candidate.class)
                }
                PageCandidateSource::Canonical(fact) => {
                    EventFactProjection::project_canonical(fact, candidate.class)
                }
            }
            .ok_or_else(ApiError::store_read_failed)
        })
        .collect::<Result<_, _>>()?;
    let next_cursor = has_more
        .then(|| page.last())
        .flatten()
        .map(|candidate| -> Result<String, CursorError> {
            let lease = start_after.map_or_else(
                || {
                    state
                        .cursor_registry()
                        .pin(Arc::clone(view), cursor_now_secs())
                },
                |cursor| Ok(cursor.lease),
            )?;
            Ok(EventsCursor {
                lease,
                query_hash,
                last_ts_us: candidate.position.sort_ts_us,
                last_event_id: candidate.position.event_id,
                last_event_instance_id: candidate.position.event_instance_id,
            }
            .encode(state.cursor_registry()))
        })
        .transpose()
        .map_err(cursor_error)?;

    let exactness = metadata.retained_exactness;
    let completeness = metadata.source_completeness;
    let physical_count = metadata.physical_count;
    let coverage = view
        .query_factor_coverage(request.range, MAX_FACTOR_COVERAGE_RECORDS)
        .map_err(canonical_fact_error)?
        .iter()
        .map(crate::overview::health::factor_coverage_json)
        .collect();
    let meta = timeline_meta_with_metadata(
        view,
        OverviewRequest {
            range: request.range,
            from_us: request.from_us,
            to_us: request.to_us,
        },
        None,
        &metadata,
    );
    Ok(EventsResponseDto {
        meta,
        notable_policy_version: policy.version(),
        events,
        next_cursor,
        omitted_by_response_filter,
        retained_exactness: retained_exactness_name(exactness),
        completeness: source_completeness_name(completeness),
        physical_count_semantics: physical_count_name(physical_count),
        coverage,
    })
}

struct PageCandidate<'a> {
    position: EventFactPosition,
    source: PageCandidateSource<'a>,
    class: NotableClass,
}

#[derive(Clone, Copy)]
enum PageCandidateSource<'a> {
    Observation(&'a EventObservation),
    Canonical(&'a CanonicalEventFact),
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

fn passes_canonical_response_filter(fact: &CanonicalEventFact, request: &EventsRequest) -> bool {
    request
        .kind
        .as_deref()
        .is_none_or(|kind| fact.kind().wire_code() == kind)
}

fn cursor_error(error: CursorError) -> ApiError {
    match error {
        // A decode/authentication failure or a changed query is the caller's
        // error (400); a pinned generation that is gone is a 410.
        CursorError::Invalid => ApiError::invalid_cursor(),
        CursorError::QueryMismatch => ApiError::cursor_query_mismatch(),
        CursorError::ViewGone => ApiError::view_gone(),
        CursorError::Expired => ApiError::cursor_expired(),
        CursorError::CapacityUnavailable => ApiError::cursor_capacity_unavailable(),
    }
}

fn oracle_error(error: OracleError) -> ApiError {
    let limit = match error {
        OracleError::LimitExceeded(OracleResource::MaterializedBytes) => Some((
            crate::api_error::LimitResource::Bytes,
            QUERY_MATERIALIZED_BYTES,
        )),
        OracleError::LimitExceeded(OracleResource::Observations) => Some((
            crate::api_error::LimitResource::Rows,
            QUERY_LIMITS.max_observations,
        )),
        OracleError::LimitExceeded(OracleResource::CoverageSpans) => Some((
            crate::api_error::LimitResource::Rows,
            QUERY_LIMITS.max_coverage_spans,
        )),
        OracleError::Counts(CountError::LimitExceeded(CountResource::InputEntries)) => Some((
            crate::api_error::LimitResource::Rows,
            QUERY_LIMITS.count_limits.max_input_entries,
        )),
        OracleError::Counts(CountError::LimitExceeded(CountResource::JointKeys)) => Some((
            crate::api_error::LimitResource::Rows,
            QUERY_LIMITS.count_limits.max_joint_keys,
        )),
        OracleError::Counts(CountError::LimitExceeded(CountResource::SignalKeys)) => Some((
            crate::api_error::LimitResource::Rows,
            QUERY_LIMITS.count_limits.max_signal_keys,
        )),
        OracleError::Counts(CountError::Overflow)
        | OracleError::Source(_)
        | OracleError::ObservationIdCollision => None,
    };
    limit.map_or_else(ApiError::store_read_failed, |(resource, limit)| {
        ApiError::query_limit_exceeded(resource, crate::api_error::count_u64(limit), None)
    })
}

fn canonical_fact_error(error: CanonicalFactQueryError) -> ApiError {
    match error {
        CanonicalFactQueryError::LimitExceeded => ApiError::query_limit_exceeded(
            crate::api_error::LimitResource::Rows,
            crate::api_error::count_u64(MAX_FACTOR_COVERAGE_RECORDS),
            None,
        ),
        CanonicalFactQueryError::ContradictoryFacts => ApiError::store_read_failed(),
    }
}

fn timeline_meta(
    view: &IndexView,
    request: OverviewRequest,
    effective_step_us: Option<u64>,
) -> Result<TimelineMetaDto, ApiError> {
    let metadata = view
        .metadata(request.range, QUERY_LIMITS.max_coverage_spans)
        .map_err(|_error| ApiError::store_read_failed())?;
    Ok(timeline_meta_with_metadata(
        view,
        request,
        effective_step_us,
        &metadata,
    ))
}

fn timeline_meta_with_metadata(
    view: &IndexView,
    request: OverviewRequest,
    effective_step_us: Option<u64>,
    metadata: &TimelineMetadata,
) -> TimelineMetaDto {
    let view_status = view.status();
    let status = if matches!(view_status, TimelineStatus::Gap) || metadata.data_through_us.is_some()
    {
        view_status.wire_code()
    } else {
        "unavailable"
    };
    let freshness = FreshnessDto {
        data_through_us: metadata.data_through_us,
        status: if !metadata.known_gaps.is_empty() || metadata.data_through_us.is_some() {
            view_status.wire_code()
        } else {
            "unavailable"
        },
        completeness: source_completeness_name(metadata.source_completeness),
        retained_exactness: retained_exactness_name(metadata.retained_exactness),
        physical_count_semantics: physical_count_name(metadata.physical_count),
    };
    let loss = LossDto {
        known_gaps: coverage_dtos(&metadata.known_gaps),
        dropped_count_lower_bound: metadata.dropped_lower_bound,
    };
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
        data_through_us: metadata.data_through_us,
        store_data_through_us: view.data_through_us(),
        tail_pending: view.live_tail_pending().map(|pending| TailPendingDto {
            from_offset_bytes: pending.start,
            to_offset_bytes: pending.end,
        }),
        status,
        freshness,
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
) -> Result<EventDigestDto, ApiError> {
    let projection_error = |_error| ApiError::store_read_failed();
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
    .map_err(|_error| ApiError::store_read_failed())?;
    let retained_observation_row_count =
        u64::try_from(observations.len()).map_err(|_error| ApiError::store_read_failed())?;
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

fn sqlstate_top_n(counts: &EventCounts) -> Result<(Vec<SqlstateCountDto>, u64, u64), ApiError> {
    let mut aggregate: BTreeMap<[u8; 5], u64> = BTreeMap::new();
    let mut missing = 0_u64;
    for (key, count) in counts.joint() {
        if let Some(sqlstate) = key.sqlstate {
            let slot = aggregate.entry(sqlstate.0).or_insert(0);
            *slot = slot
                .checked_add(*count)
                .ok_or_else(ApiError::store_read_failed)?;
        } else {
            missing = missing
                .checked_add(*count)
                .ok_or_else(ApiError::store_read_failed)?;
        }
    }
    let mut total = 0_u64;
    let mut top = Vec::with_capacity(DIGEST_TOP_N);
    for (code, count) in aggregate {
        total = total
            .checked_add(count)
            .ok_or_else(ApiError::store_read_failed)?;
        retain_top(&mut top, code, count);
    }
    let top_total = top.iter().try_fold(0_u64, |sum, (_, count)| {
        sum.checked_add(*count)
            .ok_or_else(ApiError::store_read_failed)
    })?;
    let other = total
        .checked_sub(top_total)
        .ok_or_else(ApiError::store_read_failed)?;
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

fn joint_top_n(counts: &EventCounts) -> Result<(Vec<JointCountDto>, u64), ApiError> {
    let mut total = 0_u64;
    let mut top = Vec::with_capacity(DIGEST_TOP_N);
    for (key, count) in counts.joint() {
        total = total
            .checked_add(*count)
            .ok_or_else(ApiError::store_read_failed)?;
        retain_top(&mut top, *key, *count);
    }
    let top_total = top.iter().try_fold(0_u64, |sum, (_, count)| {
        sum.checked_add(*count)
            .ok_or_else(ApiError::store_read_failed)
    })?;
    let other = total
        .checked_sub(top_total)
        .ok_or_else(ApiError::store_read_failed)?;
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
) -> Result<(), ApiError> {
    let sum = |values: &[u64]| {
        values.iter().try_fold(0_u64, |acc, value| {
            acc.checked_add(*value)
                .ok_or_else(ApiError::store_read_failed)
        })
    };
    let sqlstate_top_total = sqlstate_top.iter().try_fold(0_u64, |acc, value| {
        acc.checked_add(value.count)
            .ok_or_else(ApiError::store_read_failed)
    })?;
    let sqlstate_total = sqlstate_missing
        .checked_add(sqlstate_other)
        .and_then(|partial| partial.checked_add(sqlstate_top_total))
        .ok_or_else(ApiError::store_read_failed)?;
    let joint_total = joint_other
        .checked_add(joint_top.iter().try_fold(0_u64, |acc, value| {
            acc.checked_add(value.count)
                .ok_or_else(ApiError::store_read_failed)
        })?)
        .ok_or_else(ApiError::store_read_failed)?;
    if sum(by_severity)? != total
        || sum(by_category)? != total
        || sqlstate_total != total
        || joint_total != total
    {
        return Err(ApiError::store_read_failed());
    }
    Ok(())
}

fn notable_preview_dto(
    policy: &NotablePolicy,
    observations: &[EventObservation],
    request: OverviewRequest,
) -> Result<NotablePreviewDto, ApiError> {
    let mut total = 0_u64;
    let mut selected = BinaryHeap::with_capacity(policy.response_cap());
    for observation in observations {
        let Some(class) = policy.classify(observation) else {
            continue;
        };
        total = total
            .checked_add(1)
            .ok_or_else(ApiError::store_read_failed)?;
        let candidate = PageCandidate {
            position: EventFactProjection::position(observation, class)
                .ok_or_else(ApiError::store_read_failed)?,
            source: PageCandidateSource::Observation(observation),
            class,
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
    let retained = u64::try_from(selected.len()).map_err(|_error| ApiError::store_read_failed())?;
    let observations = selected
        .into_iter()
        .map(|candidate| {
            let PageCandidateSource::Observation(observation) = candidate.source else {
                return Err(ApiError::store_read_failed());
            };
            EventFactProjection::project(observation, candidate.class)
                .ok_or_else(ApiError::store_read_failed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NotablePreviewDto {
        observations,
        omitted_count: total
            .checked_sub(retained)
            .ok_or_else(ApiError::store_read_failed)?,
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

fn cursor_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The filter string a default `/events` request produces, so the overview
/// preview's `events_query_hash` matches an unfiltered first page.
fn default_events_filters() -> String {
    format!("limit={EVENTS_DEFAULT_LIMIT};min_severity=none;kind=none")
}

fn events_query_hash(policy: &NotablePolicy, request: OverviewRequest) -> String {
    let hash = events_query_hash_bytes(
        policy,
        request.from_us,
        request.to_us,
        &default_events_filters(),
    );
    URL_SAFE_NO_PAD.encode(hash)
}

#[cfg(test)]
mod tests {
    use super::{
        DIGEST_TOP_N, EVENTS_DEFAULT_LIMIT, EventsRequest, QUERY_MATERIALIZED_BYTES,
        default_events_filters, joint_top_n, oracle_error, sqlstate_top_n,
    };
    use crate::api_error::ErrorCode;
    use kronika_analytics::overview::{
        CountLimits, CoverageSpan, ErrorCategory, EventCounts, JointErrorKey, LifecycleCounts,
        OracleError, OracleResource, Severity, SqlState,
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
            from_us: 0,
            to_us: 1,
            limit: EVENTS_DEFAULT_LIMIT,
            cursor: None,
            min_severity: None,
            kind: None,
        };
        assert_eq!(request.filters(), default_events_filters());
    }

    #[test]
    fn an_absent_kind_cannot_alias_a_literal_asterisk_filter() {
        let base = EventsRequest {
            range: CoverageSpan::new(0, 1).expect("valid range"),
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

    #[test]
    fn materialized_byte_overflow_maps_to_a_413_error() {
        let error = oracle_error(OracleError::LimitExceeded(
            OracleResource::MaterializedBytes,
        ));
        assert_eq!(error.code(), ErrorCode::QueryLimitExceeded);
        let value = serde_json::to_value(error).expect("serialize API error");
        assert_eq!(value["code"], "query_limit_exceeded");
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
