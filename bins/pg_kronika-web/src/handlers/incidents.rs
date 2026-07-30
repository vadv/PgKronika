//! `GET /v1/incidents` clusters anomaly episodes across sections and runs the
//! active diagnostic lenses over the typed counter evidence the reader folded.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use kronika_reader::{LocalDirSnapshot, QueryError, QueryWorkResource, logical_section};
use serde_json::Value;

use crate::AppState;
use crate::anomaly::ScanParams;
use crate::api_error::{ApiError, LimitResource, QueryConstraint, QueryParameter, count_u64};
use crate::api_response::IncidentsResponse;
use crate::handlers::anomalies::scannable_sections;
use crate::handlers::metrics::data_age_seconds;
use crate::incident::{
    AnalyzeError, ClockRelation, EventConfig, EventError, EventLens, IncidentConfig, Lens,
    active_catalog, analyze, evaluate_events, event_catalog,
};
use crate::incident_input::{
    InputError, InputLimits, MaterializationKind, prepare_input, scan_position_count,
};
use crate::incident_response::{ResponseInput, build_response, no_data_response};
use crate::params::{QueryParams, parse_duration_us, parse_f64_non_negative, parse_i64};

const WINDOW_DEFAULT_US: i64 = 300 * 1_000_000;
const STEP_DEFAULT_US: i64 = 60 * 1_000_000;
const THRESHOLD_DEFAULT: f64 = 3.5;
const EPS_REL_DEFAULT: f64 = 0.05;
const MAX_CLUSTER_SPAN_DEFAULT_US: i64 = 3_600 * 1_000_000;
/// Hard public interval for bounded store scans.
const MAX_QUERY_SPAN_US: i64 = 24 * 3_600 * 1_000_000;
const INCIDENT_PARAMS: &[QueryParameter] = &[
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
    params: IncidentParams,
    sections: Vec<&'static str>,
}

/// `GET /v1/incidents?from&to` returns clustered incidents.
///
/// Optional parameters are `window`, `step`, `threshold`, `eps_rel`, `epsilon`,
/// `max_cluster_span`, and `section`. All time inputs are unix microseconds.
#[utoipa::path(
    get,
    path = "/v1/incidents",
    tag = "analytics",
    params(
        ("from" = i64, Query),
        ("to" = i64, Query),
        ("window" = Option<String>, Query),
        ("step" = Option<String>, Query),
        ("threshold" = Option<f64>, Query),
        ("eps_rel" = Option<f64>, Query),
        ("epsilon" = Option<String>, Query),
        ("max_cluster_span" = Option<String>, Query),
        ("section" = Option<String>, Query, example = "pg_stat_database"),
    ),
    responses(
        (status = 200, description = "Clustered incidents and diagnostic findings", body = IncidentsResponse),
        (status = 400, description = "Invalid or oversized query shape", body = ApiError),
        (status = 401, description = "Authentication required", body = ApiError),
        (status = 404, description = "Unknown section", body = ApiError),
        (status = 413, description = "Store or analysis limit exceeded", body = ApiError),
        (status = 503, description = "Analytic capacity unavailable", body = ApiError),
        (status = 500, description = "Store read or analysis failed", body = ApiError),
    )
)]
pub(crate) async fn incidents(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), INCIDENT_PARAMS) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let request = match validate_request(&params) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let Ok(permit) = state.try_acquire_analytic() else {
        return ApiError::analytic_capacity_unavailable().into_response();
    };

    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        run(&state, request)
    })
    .await
    {
        Ok(Ok(body)) => body.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(join) => logged_internal_error("api_analytic_worker_failed", &join).into_response(),
    }
}

fn validate_request(params: &QueryParams) -> Result<ValidatedRequest, ApiError> {
    let request = parse_incident_params(params, &InputLimits::production())?;
    let sections = resolve_sections(params)?;
    Ok(ValidatedRequest {
        params: request,
        sections,
    })
}

fn run(state: &AppState, request: ValidatedRequest) -> Result<Json<Value>, ApiError> {
    let ValidatedRequest {
        params: request,
        sections,
    } = request;

    let mut snap = state.snapshot().as_ref().clone();
    let data_age = data_age(&snap);

    let prepared = match prepare_input(
        &mut snap,
        &request.scan,
        &sections,
        &InputLimits::production(),
    ) {
        Ok(prepared) => prepared,
        Err(InputError::NoData) => {
            return Ok(Json(no_data_response(&request.scan, data_age)));
        }
        Err(error) => return Err(input_error_response(error)),
    };

    let config = IncidentConfig::production(
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

fn resolve_sections(params: &QueryParams) -> Result<Vec<&'static str>, ApiError> {
    match params.get(QueryParameter::Section) {
        Some(name) => {
            let logical = logical_section(name).ok_or_else(|| ApiError::unknown_section(name))?;
            Ok(vec![logical.name])
        }
        None => Ok(scannable_sections()),
    }
}

fn parse_incident_params(
    params: &QueryParams,
    limits: &InputLimits,
) -> Result<IncidentParams, ApiError> {
    let from = parse_i64(params, QueryParameter::From)?;
    let to = parse_i64(params, QueryParameter::To)?;
    if from >= to {
        return Err(ApiError::invalid_query_constraint(
            QueryConstraint::FromBeforeTo,
        ));
    }
    let span = to
        .checked_sub(from)
        .ok_or_else(|| ApiError::invalid_query_constraint(QueryConstraint::FiniteScan))?;
    if span > MAX_QUERY_SPAN_US {
        return Err(ApiError::query_limit_exceeded(
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
        return Err(ApiError::invalid_query_constraint(
            QueryConstraint::WindowWithinInterval,
        ));
    }
    if epsilon_us > max_cluster_span_us {
        return Err(ApiError::invalid_query_constraint(
            QueryConstraint::EpsilonNotGreaterThanMaxClusterSpan,
        ));
    }
    if max_cluster_span_us > span {
        return Err(ApiError::invalid_query_constraint(
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
        .ok_or_else(|| ApiError::invalid_query_constraint(QueryConstraint::FiniteScan))?;
    if positions > limits.position_limit() {
        return Err(ApiError::query_limit_exceeded(
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

/// Seconds since the newest timestamp of any unit, or `None` when there are no
/// units.
fn data_age(snap: &LocalDirSnapshot) -> Option<u64> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let max_ts = snap.units().iter().map(|unit| unit.max_ts).max();
    data_age_seconds(now_secs, max_ts)
}

/// Map a `prepare_input` failure to an HTTP response.
///
/// Admission caps hit before any scan runs are `413`; a malformed scan is a
/// `400`; reader and registry-invariant failures are `500` — an absence of
/// incidents is never masked as a read error.
fn input_error_response(error: InputError) -> ApiError {
    match error {
        InputError::NoData => logged_internal_error("api_unmapped_no_data", &"no_data"),
        InputError::UnknownSection(name) => {
            logged_internal_error("api_registry_section_missing", &name)
        }
        InputError::InvalidScan => ApiError::invalid_query_constraint(QueryConstraint::FiniteScan),
        InputError::PositionLimit { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::WindowPositions,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::UnitLimit { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::Units,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::SectionLimit { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::Sections,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::MaterializationLimit { resource, limit } => ApiError::query_limit_exceeded(
            match resource {
                MaterializationKind::Cells => LimitResource::Cells,
                MaterializationKind::Bytes => LimitResource::Bytes,
            },
            count_u64(limit),
            None,
        ),
        InputError::SeriesLimit { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::SeriesPoints,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        InputError::Read(error) => read_error_response(error),
        InputError::UnknownColumn { section, column } => {
            logged_internal_error("api_registry_column_missing", &(section, column))
        }
        InputError::DuplicateSeries { section, column } => {
            logged_internal_error("api_duplicate_series", &(section, column))
        }
        InputError::InvalidSeries {
            section,
            column,
            error,
        } => logged_internal_error("api_invalid_series", &(section, column, error)),
    }
}

fn read_error_response(error: QueryError) -> ApiError {
    match error {
        QueryError::UnknownSection(name) => {
            logged_internal_error("api_registry_section_missing", &name)
        }
        QueryError::RowsTooLarge { max_rows } => {
            ApiError::query_limit_exceeded(LimitResource::Rows, count_u64(max_rows), None)
        }
        QueryError::ResultTooLarge { max_cells } => {
            ApiError::query_limit_exceeded(LimitResource::Cells, count_u64(max_cells), None)
        }
        QueryError::MaterializedBytesTooLarge { max_bytes } => {
            ApiError::query_limit_exceeded(LimitResource::Bytes, count_u64(max_bytes), None)
        }
        QueryError::WorkLimitExceeded {
            resource,
            limit,
            observed,
        } => {
            let resource = match resource {
                QueryWorkResource::Units => LimitResource::Units,
                QueryWorkResource::CatalogBytes | QueryWorkResource::DictionaryBytes => {
                    LimitResource::Bytes
                }
            };
            ApiError::query_limit_exceeded(resource, limit, Some(observed))
        }
        QueryError::BadCursor(message) => {
            logged_internal_error("api_reader_cursor_invariant", &message)
        }
        QueryError::Read(read) => logged_store_read_error(&read),
        QueryError::SealedDescriptor(_) => {
            tracing::error!(
                event = "api_store_descriptor_read_failed",
                "store descriptor query failed"
            );
            ApiError::store_read_failed()
        }
    }
}

/// Map an engine failure to an HTTP response. Admission caps are `413`; a
/// registry inconsistency (duplicate lens id) is a `500`.
fn analyze_error_response(error: AnalyzeError) -> ApiError {
    match error {
        AnalyzeError::EpisodeLimit { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::Episodes,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        AnalyzeError::ClusterLimit { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::Clusters,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        AnalyzeError::Key(key) => ApiError::query_limit_exceeded(
            LimitResource::IncidentKeyBytes,
            count_u64(key.limit),
            Some(count_u64(key.observed)),
        ),
        AnalyzeError::KeyBudget { observed, limit } => ApiError::query_limit_exceeded(
            LimitResource::TotalIncidentKeyBytes,
            count_u64(limit),
            Some(count_u64(observed)),
        ),
        AnalyzeError::DuplicateLensId(id) => logged_internal_error("api_duplicate_lens_id", &id),
        AnalyzeError::Cluster(error) => logged_internal_error("api_cluster_invariant", &error),
    }
}

/// Map an event-pass failure to an HTTP response. A duplicate id is a static
/// catalog inconsistency, so it is a `500`.
fn event_error_response(error: EventError) -> ApiError {
    match error {
        EventError::DuplicateLensId(id) => {
            logged_internal_error("api_duplicate_event_lens_id", &id)
        }
    }
}

fn logged_internal_error(event: &'static str, cause: &impl std::fmt::Debug) -> ApiError {
    let api_error = ApiError::internal_error();
    tracing::error!(
        event = event,
        error = ?cause,
        "internal API failure"
    );
    api_error
}

fn logged_store_read_error(cause: &impl std::fmt::Debug) -> ApiError {
    let api_error = ApiError::store_read_failed();
    tracing::error!(
        event = "api_store_read_failed",
        error = ?cause,
        "incident store read failed"
    );
    api_error
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api_error::ErrorCode;

    #[test]
    fn materialization_failures_keep_their_resource_in_the_413_error() {
        for (resource, expected) in [
            (MaterializationKind::Cells, "cells"),
            (MaterializationKind::Bytes, "bytes"),
        ] {
            let error = input_error_response(InputError::MaterializationLimit {
                resource,
                limit: 17,
            });
            assert_eq!(error.code(), ErrorCode::QueryLimitExceeded);
            assert_eq!(error.code().status(), StatusCode::PAYLOAD_TOO_LARGE);
            let body = serde_json::to_value(error).expect("API error JSON");
            assert_eq!(
                body["params"],
                serde_json::json!({ "resource": expected, "limit": 17 })
            );
        }
    }
}
