//! Thin axum handler for `GET /v1/timeline/overview`.
//!
//! The handler validates the request, assembles the atomic index view off the
//! async runtime, queries the requested range, and serializes a compact event
//! and health summary. It orchestrates only: counts, notable selection, and
//! coverage come from `kronika-analytics`.

use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::overview::{
    CountLimits, CoverageSpan, ErrorCategory, EventCounts, EventObservation, JointErrorKey,
    NotableClass, NotablePolicy, ObservationId, OracleLimits, RankedNotable, Severity,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::overview::cache::{Endpoint, ResponseKey};
use crate::overview::cursor::{CursorError, EventsCursor};
use crate::overview::view::IndexView;
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
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Step,
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

/// A validated overview request range.
#[derive(Debug, Clone, Copy)]
struct OverviewRequest {
    range: CoverageSpan,
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
async fn serve<R>(state: AppState, key: ResponseKey, render: R) -> Response
where
    R: FnOnce() -> Result<Value, ApiProblem> + Send + 'static,
{
    if let Some(bytes) = state.response_cache.get(&key) {
        return json_bytes_response(bytes);
    }
    let flight = match state.timeline_flight(&key) {
        TimelineFlightRole::Follower(flight) => flight,
        TimelineFlightRole::Leader(flight) => {
            if let Some(bytes) = state.response_cache.get(&key) {
                state.finish_timeline_flight(&key, &flight, Ok(bytes));
            } else {
                let Ok(permit) = state.try_acquire_analytic() else {
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
                    let render_key = worker_key.clone();
                    let rendered =
                        tokio::task::spawn_blocking(move || -> Result<Arc<[u8]>, ApiProblem> {
                            let _permit = permit;
                            let value = render()?;
                            let bytes: Arc<[u8]> = serde_json::to_vec(&value)
                                .map_err(|_error| ApiProblem::internal_error())?
                                .into();
                            cache.insert(render_key, Arc::clone(&bytes));
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

const fn overview_key(view: &IndexView, request: OverviewRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Overview,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id: view.fact_set_id(),
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: None,
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: String::new(),
        page: None,
    }
}

fn validate(params: &QueryParams) -> Result<OverviewRequest, ApiProblem> {
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
        from_us,
        to_us,
    })
}

fn render_overview(view: &IndexView, request: OverviewRequest) -> Result<Value, ApiProblem> {
    let result = view
        .query_range(request.range, QUERY_LIMITS)
        .map_err(|_error| ApiProblem::store_read_failed())?;

    let policy = NotablePolicy::v1();
    let observations = result.observations();
    let preview = policy.preview(observations);

    let digest = event_digest(result.counts(), observations);
    let notable_preview = notable_preview_json(&policy, &preview, observations, request);
    let meta = timeline_meta(view, request);
    let covered_duration_us = view.coverage_envelope().covered_duration_in(request.range);
    let (health_summary, coverage) =
        crate::overview::health::overview_health_summary(observations, request.range);

    Ok(json!({
        "meta": meta,
        "event_digest": digest,
        "notable_preview": notable_preview,
        "health_summary": health_summary,
        "coverage": coverage,
        "retained_coverage_duration_us": covered_duration_us,
    }))
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

const fn health_key(view: &IndexView, request: HealthRequest) -> ResponseKey {
    ResponseKey {
        endpoint: Endpoint::Health,
        response_schema_version: RESPONSE_SCHEMA_VERSION,
        fact_set_id: view.fact_set_id(),
        from_us: request.from_us,
        to_us: request.to_us,
        step_us: Some(request.effective_step_us),
        notable_policy_version: NotablePolicy::v1().version(),
        health_policy_version: HEALTH_POLICY_VERSION,
        filters: String::new(),
        page: None,
    }
}

fn render_health(view: &IndexView, request: HealthRequest) -> Result<Value, ApiProblem> {
    let result = view
        .query_range(request.range, QUERY_LIMITS)
        .map_err(|_error| ApiProblem::store_read_failed())?;
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
            from_us: request.from_us,
            to_us: request.to_us,
        },
    );
    Ok(json!({
        "meta": meta,
        "health_policy_version": line.policy_version,
        "factor_set_ids": line.factor_set_ids,
        "points": line.points,
        "coverage": line.coverage,
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
        let severity = self.min_severity.map_or("*", severity_name);
        let kind = self.kind.as_deref().unwrap_or("*");
        format!("limit={};min_severity={severity};kind={kind}", self.limit)
    }
}

/// `GET /v1/timeline/events?from=..&to=..&limit=..&cursor=..`.
pub(crate) async fn events(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), EVENTS_PARAMS) {
        Ok(params) => params,
        Err(problem) => return problem.into_response(),
    };
    let request = match validate_events(&params) {
        Ok(request) => request,
        Err(problem) => return problem.into_response(),
    };
    let view = state.overview_view();
    let key = events_key(&view, &request);
    serve(state, key, move || render_events(&view, &request)).await
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
    let limit = crate::params::parse_limit_default(params, EVENTS_DEFAULT_LIMIT)?
        .clamp(1, EVENTS_MAX_LIMIT);
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

fn render_events(view: &IndexView, request: &EventsRequest) -> Result<Value, ApiProblem> {
    let result = view
        .query_range(request.range, QUERY_LIMITS)
        .map_err(|_error| ApiProblem::store_read_failed())?;

    let policy = NotablePolicy::v1();
    let filters = request.filters();
    let query_hash = events_query_hash_bytes(&policy, request.from_us, request.to_us, &filters);
    let start_after = match request.cursor.as_deref() {
        Some(token) => Some(
            EventsCursor::decode(token, query_hash, view.view_generation())
                .map_err(cursor_problem)?,
        ),
        None => None,
    };

    let observations = result.observations();
    let mut notable: Vec<(&EventObservation, NotableClass)> = Vec::new();
    let mut omitted_by_response_filter = 0_u64;
    for observation in observations {
        let Some(class) = policy.classify(observation) else {
            continue;
        };
        if !passes_response_filter(observation, request) {
            omitted_by_response_filter = omitted_by_response_filter.saturating_add(1);
            continue;
        }
        if let Some(position) = start_after {
            let key = (observation.time().sort_ts_us, observation.observation_id());
            if key <= (position.last_ts_us, ObservationId(position.last_event_id)) {
                continue;
            }
        }
        notable.push((observation, class));
    }

    let page_len = notable.len().min(request.limit);
    let page = &notable[..page_len];
    let has_more = notable.len() > page_len;
    let events: Vec<Value> = page
        .iter()
        .map(|(observation, class)| event_view(observation, *class))
        .collect();
    let next_cursor = has_more
        .then(|| page.last())
        .flatten()
        .map(|(observation, _class)| {
            EventsCursor {
                view_generation: view.view_generation(),
                query_hash,
                last_ts_us: observation.time().sort_ts_us,
                last_event_id: observation.observation_id().0,
            }
            .encode()
        });

    let exactness = retained_exactness(observations);
    let meta = timeline_meta(
        view,
        OverviewRequest {
            range: request.range,
            from_us: request.from_us,
            to_us: request.to_us,
        },
    );
    Ok(json!({
        "meta": meta,
        "notable_policy_version": policy.version(),
        "events": events,
        "next_cursor": next_cursor,
        "omitted_by_response_filter": omitted_by_response_filter,
        "retained_exactness": exactness,
        "coverage": Value::Array(Vec::new()),
    }))
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
    }
}

fn timeline_meta(view: &IndexView, request: OverviewRequest) -> Value {
    json!({
        "response_schema_version": RESPONSE_SCHEMA_VERSION,
        "view_generation": view.view_generation(),
        "fact_set_id": URL_SAFE_NO_PAD.encode(view.fact_set_id()),
        "requested_range": { "from_us": request.from_us, "to_us": request.to_us },
        "effective_range": { "from_us": request.from_us, "to_us": request.to_us },
        "effective_step_us": Value::Null,
        "data_through_us": view.data_through_us(),
        "tail_pending": Value::Null,
        "source_status": view.source_status().wire_code(),
        "loss": Value::Array(Vec::new()),
    })
}

/// The wire exactness of a retained result: lower-bound if any observation
/// carries upstream loss, otherwise retained-exact.
fn retained_exactness(observations: &[EventObservation]) -> &'static str {
    if observations.iter().any(|o| o.loss().is_some()) {
        "lower_bound"
    } else {
        "retained_exact"
    }
}

fn event_digest(counts: &EventCounts, observations: &[EventObservation]) -> Value {
    let by_severity = counts.by_severity().unwrap_or([0; 5]);
    let by_category = counts.by_category().unwrap_or([0; 11]);
    // Count overflow is unreachable (a u64 cannot hold 2^64 occurrences); the
    // sentinel never appears in practice.
    let retained_occurrence_count = counts.total_occurrences().unwrap_or(u64::MAX);
    let (by_sqlstate, sqlstate_other_count) = sqlstate_top_n(counts);
    let joint_top = joint_top_n(counts);
    let exactness = retained_exactness(observations);
    let lifecycle = counts.lifecycle();
    json!({
        "retained_occurrence_count": retained_occurrence_count,
        "retained_observation_count": observations.len(),
        "by_severity": by_severity,
        "by_category": by_category,
        "by_sqlstate": by_sqlstate,
        "sqlstate_other_count": sqlstate_other_count,
        "joint_top": joint_top,
        "lifecycle": {
            "crashes": lifecycle.crashes(),
            "shutdowns": lifecycle.shutdowns(),
            "ready": lifecycle.ready(),
            "signals": lifecycle.signals().iter()
                .map(|(signal, count)| json!({ "signal": signal, "count": count }))
                .collect::<Vec<_>>(),
        },
        "exactness": exactness,
    })
}

fn sqlstate_top_n(counts: &EventCounts) -> (Vec<Value>, u64) {
    let mut aggregate: std::collections::BTreeMap<[u8; 5], u64> = std::collections::BTreeMap::new();
    for (key, count) in counts.joint() {
        if let Some(sqlstate) = key.sqlstate {
            let slot = aggregate.entry(sqlstate.0).or_insert(0);
            *slot = slot.saturating_add(*count);
        }
    }
    let mut ranked: Vec<([u8; 5], u64)> = aggregate.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    let mut other = 0_u64;
    let top: Vec<Value> = ranked
        .iter()
        .enumerate()
        .filter_map(|(rank, (code, count))| {
            if rank < DIGEST_TOP_N {
                Some(json!({ "code": sqlstate_text(*code), "count": count }))
            } else {
                other = other.saturating_add(*count);
                None
            }
        })
        .collect();
    (top, other)
}

fn joint_top_n(counts: &EventCounts) -> Vec<Value> {
    let mut ranked: Vec<(JointErrorKey, u64)> = counts.joint().to_vec();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    ranked
        .iter()
        .take(DIGEST_TOP_N)
        .map(|(key, count)| {
            json!({
                "severity": severity_name(key.severity),
                "category": category_name(key.category),
                "sqlstate": key.sqlstate.map(|code| sqlstate_text(code.0)),
                "count": count,
            })
        })
        .collect()
}

fn notable_preview_json(
    policy: &NotablePolicy,
    preview: &kronika_analytics::overview::NotablePreview,
    observations: &[EventObservation],
    request: OverviewRequest,
) -> Value {
    let items: Vec<Value> = preview
        .ranked()
        .iter()
        .filter_map(|ranked| observation_view(observations, *ranked))
        .collect();
    json!({
        "observations": items,
        "omitted_count": preview.omitted_count(),
        "events_query_hash": events_query_hash(policy, request),
    })
}

fn observation_view(observations: &[EventObservation], ranked: RankedNotable) -> Option<Value> {
    let observation = observations.get(ranked.index())?;
    Some(event_view(observation, ranked.class()))
}

/// The machine-neutral wire view of one notable observation (§15.4).
fn event_view(observation: &EventObservation, class: NotableClass) -> Value {
    let time = observation.time();
    json!({
        "event_id": URL_SAFE_NO_PAD.encode(observation.observation_id().0),
        "identity_quality": identity_quality_name(observation.identity_quality()),
        "sort_ts_us": time.sort_ts_us,
        "occurred_at_us": time.occurred_at_us,
        "occurrence_count": observation.occurrence_count(),
        "event_kind": observation.payload().kind_code(),
        "notable_class": class.wire_code(),
        "evidence_quality": evidence_quality_name(observation.evidence_quality()),
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
    hasher.update(filters.as_bytes());
    hasher.finalize().into()
}

/// The filter string a default `/events` request produces, so the overview
/// preview's `events_query_hash` matches an unfiltered first page.
fn default_events_filters() -> String {
    format!("limit={EVENTS_DEFAULT_LIMIT};min_severity=*;kind=*")
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

impl IndexView {
    /// Queries the range through the merged oracle.
    fn query_range(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<kronika_analytics::overview::OracleResult, kronika_analytics::overview::OracleError>
    {
        use kronika_analytics::overview::RawOracle;
        self.query(range, limits)
    }
}

const fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Fatal => "fatal",
        Severity::Panic => "panic",
        Severity::Warning => "warning",
        Severity::Log => "log",
    }
}

const fn category_name(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Lock => "lock",
        ErrorCategory::Constraint => "constraint",
        ErrorCategory::Serialization => "serialization",
        ErrorCategory::Timeout => "timeout",
        ErrorCategory::Connection => "connection",
        ErrorCategory::Auth => "auth",
        ErrorCategory::Syntax => "syntax",
        ErrorCategory::Resource => "resource",
        ErrorCategory::DataCorruption => "data_corruption",
        ErrorCategory::System => "system",
        ErrorCategory::Other => "other",
    }
}

const fn identity_quality_name(
    quality: kronika_analytics::overview::IdentityQuality,
) -> &'static str {
    use kronika_analytics::overview::IdentityQuality;
    match quality {
        IdentityQuality::SourceExact => "source_exact",
        IdentityQuality::ContentDerived => "content_derived",
        IdentityQuality::Approximate => "approximate",
    }
}

const fn evidence_quality_name(
    quality: kronika_analytics::overview::EvidenceQuality,
) -> &'static str {
    use kronika_analytics::overview::EvidenceQuality;
    match quality {
        EvidenceQuality::Structured => "structured",
        EvidenceQuality::Parsed => "parsed",
        EvidenceQuality::Heuristic => "heuristic",
        EvidenceQuality::DerivedExact => "derived_exact",
    }
}

fn sqlstate_text(code: [u8; 5]) -> String {
    String::from_utf8_lossy(&code).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        DIGEST_TOP_N, EVENTS_DEFAULT_LIMIT, EventsRequest, default_events_filters, joint_top_n,
        sqlstate_top_n,
    };
    use kronika_analytics::overview::{
        CountLimits, CoverageSpan, ErrorCategory, EventCounts, JointErrorKey, LifecycleCounts,
        Severity, SqlState,
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
        let (top, other) = sqlstate_top_n(&counts);
        assert_eq!(top.len(), DIGEST_TOP_N, "the sparse dimension is capped");
        assert_eq!(top[0]["count"], 20, "the highest count ranks first");
        // Ranks 16..20 fall into the other bucket: counts 4 + 3 + 2 + 1.
        assert_eq!(other, 10, "dropped counts are summed, not discarded");
    }

    #[test]
    fn joint_projection_ranks_by_count_then_code() {
        let counts = descending_counts(3);
        let joint = joint_top_n(&counts);
        assert_eq!(joint.len(), 3);
        assert_eq!(joint[0]["count"], 3);
        assert_eq!(joint[2]["count"], 1);
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
        let (top, _other) = sqlstate_top_n(&counts);
        assert_eq!(
            top[0]["code"], "00001",
            "equal counts order by ascending code"
        );
    }
}
