//! `GET /v1/incidents` clusters anomaly episodes across sections and runs the
//! active diagnostic lenses over the typed counter evidence the reader folded.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use kronika_reader::{LocalDirSnapshot, QueryError, logical_section};
use serde_json::Value;

use crate::AppState;
use crate::anomaly::ScanParams;
use crate::handlers::anomalies::scannable_sections;
use crate::handlers::metrics::data_age_seconds;
use crate::incident::{
    AnalyzeError, ClockRelation, EventConfig, EventError, EventLens, IncidentConfig, Lens,
    active_catalog, analyze, evaluate_events, event_catalog,
};
use crate::incident_input::{InputError, InputLimits, prepare_input, scan_position_count};
use crate::incident_response::{
    ResponseInput, build_response, identity_response, no_data_response,
};
use crate::params::{QueryParams, parse_duration_us, parse_f64_non_negative, parse_i64, parse_u64};
use crate::problem::{ApiProblem, LimitResource, QueryConstraint, QueryParameter, count_u64};
use crate::reason::ApiReason;

const WINDOW_DEFAULT_US: i64 = 300 * 1_000_000;
const STEP_DEFAULT_US: i64 = 60 * 1_000_000;
const THRESHOLD_DEFAULT: f64 = 3.5;
const EPS_REL_DEFAULT: f64 = 0.05;
const MAX_CLUSTER_SPAN_DEFAULT_US: i64 = 3_600 * 1_000_000;
/// Hard public interval for bounded store scans.
const MAX_QUERY_SPAN_US: i64 = 24 * 3_600 * 1_000_000;
const INCIDENT_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Window,
    QueryParameter::Step,
    QueryParameter::Threshold,
    QueryParameter::EpsRel,
    QueryParameter::Epsilon,
    QueryParameter::MaxClusterSpan,
    QueryParameter::Section,
];

struct IncidentParams {
    scan: ScanParams,
    epsilon_us: i64,
    max_cluster_span_us: i64,
}

struct ValidatedRequest {
    source: u64,
    params: IncidentParams,
    sections: Vec<&'static str>,
}

/// `GET /v1/incidents?source&from&to` returns clustered incidents.
///
/// Optional parameters are `window`, `step`, `threshold`, `eps_rel`, `epsilon`,
/// `max_cluster_span`, and `section`. All time inputs are unix microseconds.
pub(crate) async fn incidents(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), INCIDENT_PARAMS) {
        Ok(params) => params,
        Err(problem) => return problem.into_response(),
    };
    let request = match validate_request(&params) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let Ok(permit) = state.try_acquire_analytic() else {
        return ApiProblem::analytic_capacity_unavailable().into_response();
    };

    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        run(&state, request)
    })
    .await
    {
        Ok(Ok(body)) => body.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(join) => logged_internal_problem("api_analytic_worker_failed", &join).into_response(),
    }
}

fn validate_request(params: &QueryParams) -> Result<ValidatedRequest, ApiProblem> {
    let source = parse_u64(params, QueryParameter::Source)?;
    let request = parse_incident_params(params, &InputLimits::production())?;
    let sections = resolve_sections(params)?;
    Ok(ValidatedRequest {
        source,
        params: request,
        sections,
    })
}

fn run(state: &AppState, request: ValidatedRequest) -> Result<Json<Value>, ApiProblem> {
    let ValidatedRequest {
        source,
        params: request,
        sections,
    } = request;

    let mut snap = state.snapshot.load().as_ref().clone();
    let data_age = source_data_age(&snap, source);

    let prepared = match prepare_input(
        &mut snap,
        source,
        &request.scan,
        &sections,
        &InputLimits::production(),
    ) {
        Ok(prepared) => prepared,
        Err(InputError::NoData) => {
            return Ok(Json(no_data_response(source, &request.scan, data_age)));
        }
        Err(InputError::MissingNodeIdentity) => {
            return Ok(Json(identity_response(
                source,
                &request.scan,
                data_age,
                "missing_node_identity",
                ApiReason::missing_node_identity(),
            )));
        }
        Err(InputError::ConflictingNodeIdentity) => {
            return Ok(Json(identity_response(
                source,
                &request.scan,
                data_age,
                "conflicting_node_identity",
                ApiReason::conflicting_node_identity(),
            )));
        }
        Err(error) => return Err(input_error_response(error)),
    };

    let config = IncidentConfig::production(
        &prepared.node_self_id,
        request.epsilon_us,
        request.max_cluster_span_us,
        // Product convention: timestamps are true observation times, but all
        // metric signals in one incident observation are simultaneous.
        ClockRelation::Simultaneous,
    );
    let catalog = active_catalog();
    let lenses: Vec<&dyn Lens> = catalog.iter().map(AsRef::as_ref).collect();
    let outcome = analyze(
        prepared.episodes,
        &prepared.series,
        &prepared.typed,
        &lenses,
        &config,
    )
    .map_err(analyze_error_response)?;

    let event_lens_catalog = event_catalog();
    let event_lenses: Vec<&dyn EventLens> = event_lens_catalog.iter().map(AsRef::as_ref).collect();
    let log = evaluate_events(
        &prepared.log_events,
        &event_lenses,
        &EventConfig::production(),
    )
    .map_err(event_error_response)?;

    Ok(Json(build_response(
        prepared.source_id,
        &request.scan,
        data_age,
        &outcome,
        &log,
        &ResponseInput {
            coverage: &prepared.coverage_by_section,
            quality: &prepared.quality,
            skipped: &prepared.skipped,
            capability_by_section: &prepared.capability_by_section,
        },
    )))
}

fn resolve_sections(params: &QueryParams) -> Result<Vec<&'static str>, ApiProblem> {
    match params.get(QueryParameter::Section) {
        Some(name) => {
            let logical = logical_section(name).ok_or_else(|| ApiProblem::unknown_section(name))?;
            Ok(vec![logical.name])
        }
        None => Ok(scannable_sections()),
    }
}

fn parse_incident_params(
    params: &QueryParams,
    limits: &InputLimits,
) -> Result<IncidentParams, ApiProblem> {
    let from = parse_i64(params, QueryParameter::From)?;
    let to = parse_i64(params, QueryParameter::To)?;
    if from >= to {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::FromBeforeTo,
        ));
    }
    let span = to
        .checked_sub(from)
        .ok_or_else(|| ApiProblem::invalid_query_constraint(QueryConstraint::FiniteScan))?;
    if span > MAX_QUERY_SPAN_US {
        return Err(ApiProblem::query_limit_exceeded(
            LimitResource::QuerySpanUs,
            u64::try_from(MAX_QUERY_SPAN_US).unwrap_or(u64::MAX),
            u64::try_from(span).ok(),
        ));
    }
    let window = parse_duration_us(params, QueryParameter::Window, WINDOW_DEFAULT_US)?;
    let step = parse_duration_us(params, QueryParameter::Step, STEP_DEFAULT_US)?;
    let threshold = parse_f64_non_negative(params, QueryParameter::Threshold, THRESHOLD_DEFAULT)?;
    let eps_rel = parse_f64_non_negative(params, QueryParameter::EpsRel, EPS_REL_DEFAULT)?;
    let epsilon_us = parse_duration_us(params, QueryParameter::Epsilon, step)?;
    let max_cluster_span_us = parse_duration_us(
        params,
        QueryParameter::MaxClusterSpan,
        MAX_CLUSTER_SPAN_DEFAULT_US.min(span),
    )?;
    if from.checked_add(window).is_none_or(|first| first > to) {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::WindowWithinInterval,
        ));
    }
    if epsilon_us > max_cluster_span_us {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::EpsilonNotGreaterThanMaxClusterSpan,
        ));
    }
    if max_cluster_span_us > span {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::MaxClusterSpanWithinInterval,
        ));
    }
    let scan_params = ScanParams {
        from,
        to,
        window,
        step,
        threshold,
        eps_rel,
    };
    let positions = scan_position_count(&scan_params)
        .ok_or_else(|| ApiProblem::invalid_query_constraint(QueryConstraint::FiniteScan))?;
    if positions > limits.position_limit() {
        return Err(ApiProblem::query_limit_exceeded(
            LimitResource::WindowPositions,
            count_u64(limits.position_limit()),
            Some(count_u64(positions)),
        ));
    }
    Ok(IncidentParams {
        scan: scan_params,
        epsilon_us,
        max_cluster_span_us,
    })
}

/// Seconds since the newest timestamp of any unit belonging to `source`, or
/// `None` when the source has no units.
fn source_data_age(snap: &LocalDirSnapshot, source: u64) -> Option<u64> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let max_ts = snap
        .units()
        .iter()
        .filter(|unit| unit.source_id == source)
        .map(|unit| unit.max_ts)
        .max();
    data_age_seconds(now_secs, max_ts)
}

/// Map a `prepare_input` failure to an HTTP response.
///
/// Admission caps hit before any scan runs are `413`; a malformed scan is a
/// `400`; reader and registry-invariant failures are `500` — an absence of
/// incidents is never masked as a read error.
fn input_error_response(error: InputError) -> ApiProblem {
    match error {
        InputError::NoData => logged_internal_problem("api_unmapped_no_data", &"no_data"),
        InputError::UnknownSection(name) => {
            logged_internal_problem("api_registry_section_missing", &name)
        }
        InputError::InvalidScan => {
            ApiProblem::invalid_query_constraint(QueryConstraint::FiniteScan)
        }
        InputError::PositionLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::WindowPositions,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::UnitLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::Units,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::SectionLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::Sections,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::MaterializationLimit { limit } => {
            ApiProblem::query_limit_exceeded(LimitResource::Cells, count_u64(limit), None)
        }
        InputError::IdentityByteLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::IdentityBytes,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::SeriesLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::SeriesPoints,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::MissingNodeIdentity | InputError::ConflictingNodeIdentity => {
            logged_internal_problem("api_identity_mapping_invariant", &"identity")
        }
        InputError::Read(error) => read_error_response(error),
        InputError::UnknownColumn { section, column } => {
            logged_internal_problem("api_registry_column_missing", &(section, column))
        }
        InputError::DuplicateSeries { section, column } => {
            logged_internal_problem("api_duplicate_series", &(section, column))
        }
        InputError::InvalidSeries {
            section,
            column,
            error,
        } => logged_internal_problem("api_invalid_series", &(section, column, error)),
    }
}

fn read_error_response(error: QueryError) -> ApiProblem {
    match error {
        QueryError::UnknownSection(name) => {
            logged_internal_problem("api_registry_section_missing", &name)
        }
        QueryError::ResultTooLarge { max_cells } => {
            ApiProblem::query_limit_exceeded(LimitResource::Cells, count_u64(max_cells), None)
        }
        QueryError::MaterializedBytesTooLarge { max_bytes } => {
            ApiProblem::query_limit_exceeded(LimitResource::Bytes, count_u64(max_bytes), None)
        }
        QueryError::BadCursor(message) => {
            logged_internal_problem("api_reader_cursor_invariant", &message)
        }
        QueryError::Read(read) => logged_store_read_problem(&read),
    }
}

/// Map an engine failure to an HTTP response. Admission caps are `413`; a
/// registry inconsistency (duplicate lens id) is a `500`.
fn analyze_error_response(error: AnalyzeError) -> ApiProblem {
    match error {
        AnalyzeError::MissingNodeIdentity => {
            logged_internal_problem("api_engine_identity_invariant", &"missing")
        }
        AnalyzeError::EpisodeLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::Episodes,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        AnalyzeError::ClusterLimit { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::Clusters,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        AnalyzeError::Key(key) => ApiProblem::query_limit_exceeded(
            LimitResource::IncidentKeyBytes,
            count_u64(key.limit),
            Some(count_u64(key.observed)),
        ),
        AnalyzeError::KeyBudget { observed, limit } => ApiProblem::query_limit_exceeded(
            LimitResource::TotalIncidentKeyBytes,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        AnalyzeError::DuplicateLensId(id) => logged_internal_problem("api_duplicate_lens_id", &id),
        AnalyzeError::Cluster(error) => logged_internal_problem("api_cluster_invariant", &error),
    }
}

/// Map an event-pass failure to an HTTP response. A duplicate id is a static
/// catalog inconsistency, so it is a `500`.
fn event_error_response(error: EventError) -> ApiProblem {
    match error {
        EventError::DuplicateLensId(id) => {
            logged_internal_problem("api_duplicate_event_lens_id", &id)
        }
    }
}

fn logged_internal_problem(event: &'static str, error: &impl std::fmt::Debug) -> ApiProblem {
    let problem = ApiProblem::internal_error();
    tracing::error!(
        event = event,
        request_id = problem.request_id(),
        error = ?error,
        "internal API failure"
    );
    problem
}

fn logged_store_read_problem(error: &impl std::fmt::Debug) -> ApiProblem {
    let problem = ApiProblem::store_read_failed();
    tracing::error!(
        event = "api_store_read_failed",
        request_id = problem.request_id(),
        error = ?error,
        "incident store read failed"
    );
    problem
}
