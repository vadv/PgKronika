//! `GET /v1/incidents` clusters anomaly episodes across sections and runs the
//! active diagnostic lenses over the typed counter evidence the reader folded.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use kronika_reader::{LocalDirSnapshot, QueryError, logical_section};
use serde::Deserialize;
use serde_json::{Value, json};

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
use crate::params::{bad_request, parse_duration_us, parse_f64_non_negative, parse_i64, parse_u64};

const WINDOW_DEFAULT_US: i64 = 300 * 1_000_000;
const STEP_DEFAULT_US: i64 = 60 * 1_000_000;
const THRESHOLD_DEFAULT: f64 = 3.5;
const EPS_REL_DEFAULT: f64 = 0.05;
const MAX_CLUSTER_SPAN_DEFAULT_US: i64 = 3_600 * 1_000_000;
/// Hard public interval for bounded store scans.
const MAX_QUERY_SPAN_US: i64 = 24 * 3_600 * 1_000_000;
const RETRY_AFTER_SECONDS: &str = "1";

struct IncidentParams {
    scan: ScanParams,
    epsilon_us: i64,
    max_cluster_span_us: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IncidentQuery {
    source: Option<String>,
    from: Option<String>,
    to: Option<String>,
    window: Option<String>,
    step: Option<String>,
    threshold: Option<String>,
    eps_rel: Option<String>,
    epsilon: Option<String>,
    max_cluster_span: Option<String>,
    section: Option<String>,
}

impl IncidentQuery {
    fn into_params(self) -> std::collections::HashMap<String, String> {
        let mut params = std::collections::HashMap::new();
        for (name, value) in [
            ("source", self.source),
            ("from", self.from),
            ("to", self.to),
            ("window", self.window),
            ("step", self.step),
            ("threshold", self.threshold),
            ("eps_rel", self.eps_rel),
            ("epsilon", self.epsilon),
            ("max_cluster_span", self.max_cluster_span),
            ("section", self.section),
        ] {
            if let Some(value) = value {
                params.insert(name.to_owned(), value);
            }
        }
        params
    }
}

struct ValidatedRequest {
    source: u64,
    params: IncidentParams,
    sections: Vec<&'static str>,
}

/// A handler failure as an HTTP status and a `{ error, detail }` body. The
/// busy path additionally advertises `Retry-After`.
struct IncidentError {
    status: StatusCode,
    body: Json<Value>,
    retry_after: bool,
}

impl IncidentError {
    fn new(status: StatusCode, code: &'static str, detail: &str) -> Self {
        Self {
            status,
            body: Json(json!({ "error": code, "detail": detail })),
            retry_after: false,
        }
    }

    fn busy() -> Self {
        let mut error = Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "analytic_capacity_unavailable",
            "the analytic worker is busy; retry shortly",
        );
        error.retry_after = true;
        error
    }
}

impl IntoResponse for IncidentError {
    fn into_response(self) -> Response {
        if self.retry_after {
            (
                self.status,
                [(header::RETRY_AFTER, RETRY_AFTER_SECONDS)],
                self.body,
            )
                .into_response()
        } else {
            (self.status, self.body).into_response()
        }
    }
}

impl From<(StatusCode, Json<Value>)> for IncidentError {
    fn from((status, body): (StatusCode, Json<Value>)) -> Self {
        Self {
            status,
            body,
            retry_after: false,
        }
    }
}

/// `GET /v1/incidents?source&from&to` returns clustered incidents.
///
/// Optional parameters are `window`, `step`, `threshold`, `eps_rel`, `epsilon`,
/// `max_cluster_span`, and `section`. All time inputs are unix microseconds.
pub(crate) async fn incidents(
    State(state): State<AppState>,
    query: Result<Query<IncidentQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(_rejection) => {
            return IncidentError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "query parameters must be known and may appear only once",
            )
            .into_response();
        }
    };
    let request = match validate_request(&query.into_params()) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let Ok(permit) = state.try_acquire_analytic() else {
        return IncidentError::busy().into_response();
    };

    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        run(&state, request)
    })
    .await
    {
        Ok(Ok(body)) => body.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_join) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "analytic_worker_failed",
            "the analytic worker failed",
        )
        .into_response(),
    }
}

fn validate_request(
    params: &std::collections::HashMap<String, String>,
) -> Result<ValidatedRequest, IncidentError> {
    let source = parse_u64(params, "source")?;
    let request = parse_incident_params(params, &InputLimits::production())?;
    let sections = resolve_sections(params)?;
    Ok(ValidatedRequest {
        source,
        params: request,
        sections,
    })
}

fn run(state: &AppState, request: ValidatedRequest) -> Result<Json<Value>, IncidentError> {
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
            )));
        }
        Err(InputError::ConflictingNodeIdentity) => {
            return Ok(Json(identity_response(
                source,
                &request.scan,
                data_age,
                "conflicting_node_identity",
            )));
        }
        Err(error) => return Err(input_error_response(error)),
    };

    let config = IncidentConfig::production(
        &prepared.node_self_id,
        request.epsilon_us,
        request.max_cluster_span_us,
        // One collector stamps every section of a cycle with the same server
        // clock, so capture-time order is comparable across signals.
        ClockRelation::SameDomain,
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

fn resolve_sections(
    params: &std::collections::HashMap<String, String>,
) -> Result<Vec<&'static str>, IncidentError> {
    match params.get("section") {
        Some(name) => {
            let logical = logical_section(name).ok_or_else(|| {
                IncidentError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_section",
                    &format!("no section named `{name}`"),
                )
            })?;
            Ok(vec![logical.name])
        }
        None => Ok(scannable_sections()),
    }
}

fn parse_incident_params(
    params: &std::collections::HashMap<String, String>,
    limits: &InputLimits,
) -> Result<IncidentParams, IncidentError> {
    let from = parse_i64(params, "from")?;
    let to = parse_i64(params, "to")?;
    if from >= to {
        return Err(bad_request("`from` must be before `to`").into());
    }
    let span = to
        .checked_sub(from)
        .ok_or_else(|| IncidentError::from(bad_request("the query interval overflows")))?;
    if span > MAX_QUERY_SPAN_US {
        return Err(IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "query_span_too_large",
            "the query interval exceeds the 24-hour ceiling",
        ));
    }
    let window = parse_duration_us(params, "window", WINDOW_DEFAULT_US)?;
    let step = parse_duration_us(params, "step", STEP_DEFAULT_US)?;
    let threshold = parse_f64_non_negative(params, "threshold", THRESHOLD_DEFAULT)?;
    let eps_rel = parse_f64_non_negative(params, "eps_rel", EPS_REL_DEFAULT)?;
    let epsilon_us = parse_duration_us(params, "epsilon", step)?;
    let max_cluster_span_us = parse_duration_us(
        params,
        "max_cluster_span",
        MAX_CLUSTER_SPAN_DEFAULT_US.min(span),
    )?;
    if from.checked_add(window).is_none_or(|first| first > to) {
        return Err(bad_request("`window` must fit inside [from, to]").into());
    }
    if epsilon_us > max_cluster_span_us {
        return Err(bad_request("`epsilon` must not exceed `max_cluster_span`").into());
    }
    if max_cluster_span_us > span {
        return Err(bad_request("`max_cluster_span` must not exceed the query interval").into());
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
        .ok_or_else(|| IncidentError::from(bad_request("the scan arithmetic is invalid")))?;
    if positions > limits.position_limit() {
        return Err(IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_positions",
            "the scan exceeds the window-position ceiling",
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
fn input_error_response(error: InputError) -> IncidentError {
    match error {
        InputError::NoData => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no_data",
            "the source has no unit over the requested period",
        ),
        InputError::UnknownSection(name) => IncidentError::new(
            StatusCode::NOT_FOUND,
            "unknown_section",
            &format!("no section named `{name}`"),
        ),
        InputError::InvalidScan => IncidentError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the period, window, or step does not define a finite scan",
        ),
        InputError::PositionLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_positions",
            &format!(
                "the scan would materialize {observed} window positions; the ceiling is {limit}"
            ),
        ),
        InputError::UnitLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_units",
            &format!("{observed} store units overlap the request; the ceiling is {limit}"),
        ),
        InputError::SectionLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_sections",
            &format!("{observed} sections requested; the ceiling is {limit}"),
        ),
        InputError::MaterializationLimit { limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "result_too_large",
            &format!("the request exceeds the {limit}-cell materialization ceiling; narrow it"),
        ),
        InputError::IdentityByteLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "identity_too_large",
            &format!("row identities need {observed} bytes; the ceiling is {limit}"),
        ),
        InputError::SeriesLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_points",
            &format!("the scan retains {observed} series points; the ceiling is {limit}"),
        ),
        InputError::MissingNodeIdentity | InputError::ConflictingNodeIdentity => {
            IncidentError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "identity_mapping_invariant",
                "identity quality was not mapped to a partial response",
            )
        }
        InputError::Read(error) => read_error_response(error),
        InputError::UnknownColumn { section, column } => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "registry_invariant",
            &format!("scanned column `{column}` is absent from section `{section}`"),
        ),
        InputError::DuplicateSeries { section, column } => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "duplicate_series",
            &format!("section `{section}` column `{column}` was ingested twice"),
        ),
        InputError::InvalidSeries {
            section,
            column,
            error,
        } => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_series",
            &format!(
                "section `{section}` column `{column}` folded into an invalid series: {error:?}"
            ),
        ),
    }
}

fn read_error_response(error: QueryError) -> IncidentError {
    match error {
        QueryError::UnknownSection(name) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "registry_invariant",
            &format!("resolved section `{name}` is absent from the registry"),
        ),
        QueryError::ResultTooLarge { max_cells } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "result_too_large",
            &format!("the request exceeds the {max_cells}-cell reader ceiling"),
        ),
        QueryError::MaterializedBytesTooLarge { max_bytes } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "result_too_large",
            &format!("the request exceeds the {max_bytes}-byte reader ceiling"),
        ),
        QueryError::BadCursor(_) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "reader_invariant",
            "the incident reader produced an invalid internal cursor",
        ),
        QueryError::Read(_) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_read_failed",
            "the store could not be read",
        ),
    }
}

/// Map an engine failure to an HTTP response. Admission caps are `413`; a
/// registry inconsistency (duplicate lens id) is a `500`.
fn analyze_error_response(error: AnalyzeError) -> IncidentError {
    match error {
        AnalyzeError::MissingNodeIdentity => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing_node_identity",
            "the prepared input carried no node identity",
        ),
        AnalyzeError::EpisodeLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_episodes",
            &format!("{observed} episodes clustered; the ceiling is {limit}"),
        ),
        AnalyzeError::ClusterLimit { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_clusters",
            &format!("{observed} clusters formed; the ceiling is {limit}"),
        ),
        AnalyzeError::Key(key) => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "incident_key_too_large",
            &format!(
                "an incident key needs {} bytes; the ceiling is {}",
                key.observed, key.limit
            ),
        ),
        AnalyzeError::KeyBudget { observed, limit } => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "incident_keys_too_large",
            &format!("incident keys need {observed} bytes; the ceiling is {limit}"),
        ),
        AnalyzeError::DuplicateLensId(_id) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "duplicate_lens_id",
            "the lens catalog contains a duplicate id",
        ),
        AnalyzeError::Cluster(_) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cluster_error",
            "clustering the episodes failed",
        ),
    }
}

/// Map an event-pass failure to an HTTP response. A duplicate id is a static
/// catalog inconsistency, so it is a `500`.
fn event_error_response(error: EventError) -> IncidentError {
    match error {
        EventError::DuplicateLensId(_id) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "duplicate_lens_id",
            "the event lens catalog contains a duplicate id",
        ),
    }
}
