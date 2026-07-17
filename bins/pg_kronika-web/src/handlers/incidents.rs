//! `GET /v1/incidents` — clustered cross-section incidents over a period.
//!
//! Reads and scans one source's sections (like `/v1/anomalies`), clusters the
//! resulting episodes, evaluates the lens catalog (empty until the lenses land),
//! and returns the incidents with their canonical keys and members.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use kronika_reader::{LocalDirSnapshot, QueryError, logical_section};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::anomaly::ScanParams;
use crate::handlers::anomalies::scannable_sections;
use crate::handlers::metrics::data_age_seconds;
use crate::incident::{
    AnalyzeError, ClockRelation, EngineOutcome, EngineSkip, EpisodeRefV1, IdentityValue, Incident,
    IncidentConfig, LimitAxis, analyze,
};
use crate::incident_input::{
    InputError, InputLimits, InputQuality, SectionSkip, SkipReason, prepare_input,
};
use crate::params::{
    bad_request, parse_duration_us, parse_f64_non_negative, parse_i64, parse_u64,
    query_error_response,
};

/// Default window length: five minutes.
const WINDOW_DEFAULT_US: i64 = 300 * 1_000_000;
/// Default distance between window positions: one minute.
const STEP_DEFAULT_US: i64 = 60 * 1_000_000;
/// Default episode cutoff, in robust sigmas.
const THRESHOLD_DEFAULT: f64 = 3.5;
/// Default relative floor as a fraction of the reference median.
const EPS_REL_DEFAULT: f64 = 0.05;
/// Default clustering span cap: one hour.
const MAX_CLUSTER_SPAN_DEFAULT_US: i64 = 3_600 * 1_000_000;
/// `Retry-After` value, in seconds, advertised when the analyzer is busy.
const RETRY_AFTER_SECONDS: &str = "1";

/// One in-flight analysis at a time: the engine's owned state is bounded per
/// request, so serializing requests bounds the process's peak.
static INCIDENT_REQUESTS: Semaphore = Semaphore::const_new(1);

/// Validated request parameters: the scan plus the incident clustering knobs.
struct IncidentParams {
    scan: ScanParams,
    epsilon_us: i64,
    max_cluster_span_us: i64,
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
            "incident_analyzer_unavailable",
            "the incident analyzer is busy; retry shortly",
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
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    match run(&state, &params) {
        Ok(body) => body.into_response(),
        Err(error) => error.into_response(),
    }
}

fn run(
    state: &AppState,
    params: &std::collections::HashMap<String, String>,
) -> Result<Json<Value>, IncidentError> {
    let source = parse_u64(params, "source")?;
    let request = parse_incident_params(params)?;
    let sections = resolve_sections(params)?;

    let Ok(_permit) = INCIDENT_REQUESTS.try_acquire() else {
        return Err(IncidentError::busy());
    };

    let mut snap = state.snapshot.load().as_ref().clone();
    let data_age = source_data_age(&snap, source);

    let prepared = match prepare_input(
        &mut snap,
        source,
        &request.scan,
        &sections,
        &InputLimits::default_100mb(),
    ) {
        Ok(prepared) => prepared,
        Err(InputError::NoData) => {
            return Ok(Json(empty_response(source, &request.scan, data_age)));
        }
        Err(error) => return Err(input_error_response(error)),
    };

    let config = IncidentConfig::default_100mb(
        &prepared.node_self_id,
        request.epsilon_us,
        request.max_cluster_span_us,
        ClockRelation::Unknown,
    );
    // `episodes` moves into the engine; the borrowed fields stay valid.
    let outcome = analyze(prepared.episodes, &prepared.series, &[], &config)
        .map_err(analyze_error_response)?;

    Ok(Json(build_response(
        prepared.source_id,
        &request.scan,
        data_age,
        &outcome,
        &prepared.coverage_by_section,
        &prepared.quality,
        &prepared.skipped,
    )))
}

/// Resolve the sections to scan: an explicit `section` filter (404 when
/// unknown) or every scannable section.
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
) -> Result<IncidentParams, IncidentError> {
    let from = parse_i64(params, "from")?;
    let to = parse_i64(params, "to")?;
    if from >= to {
        return Err(bad_request("`from` must be before `to`").into());
    }
    let window = parse_duration_us(params, "window", WINDOW_DEFAULT_US)?;
    let step = parse_duration_us(params, "step", STEP_DEFAULT_US)?;
    let threshold = parse_f64_non_negative(params, "threshold", THRESHOLD_DEFAULT)?;
    let eps_rel = parse_f64_non_negative(params, "eps_rel", EPS_REL_DEFAULT)?;
    let epsilon_us = parse_duration_us(params, "epsilon", step)?;
    let max_cluster_span_us =
        parse_duration_us(params, "max_cluster_span", MAX_CLUSTER_SPAN_DEFAULT_US)?;
    if from.checked_add(window).is_none_or(|first| first > to) {
        return Err(bad_request("`window` must fit inside [from, to]").into());
    }
    if epsilon_us > max_cluster_span_us {
        return Err(bad_request("`epsilon` must not exceed `max_cluster_span`").into());
    }
    Ok(IncidentParams {
        scan: ScanParams {
            from,
            to,
            window,
            step,
            threshold,
            eps_rel,
        },
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
        // `run` maps NoData to an empty 200 body before reaching here.
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
        InputError::MissingNodeIdentity => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "missing_node_identity",
            "no node identity covers the requested source and interval",
        ),
        InputError::ConflictingNodeIdentity => IncidentError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "conflicting_node_identity",
            "the interval spans more than one node identity for the source",
        ),
        InputError::Read(QueryError::UnknownSection(name)) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "registry_invariant",
            &format!("resolved section `{name}` is absent from the registry"),
        ),
        InputError::Read(query) => query_error_response(&query).into(),
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
        AnalyzeError::DuplicateLensId(id) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "duplicate_lens_id",
            &format!("lens id `{id}` is registered twice"),
        ),
        AnalyzeError::Cluster(_) => IncidentError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cluster_error",
            "clustering the episodes failed",
        ),
    }
}

/// The response envelope with no incidents, used when a source has no unit over
/// the period. Every top-level field is present so `0 incidents` is not
/// confused with an absence of data.
fn empty_response(source: u64, scan: &ScanParams, data_age: Option<u64>) -> Value {
    json!({
        "source_id": source,
        "from": scan.from,
        "to": scan.to,
        "complete": true,
        "incidents": Value::Array(Vec::new()),
        "coverage_by_section": json!({}),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(),
        "data_quality": quality_to_json(&InputQuality::default()),
        "skipped": Value::Array(Vec::new()),
    })
}

fn build_response(
    source: u64,
    scan: &ScanParams,
    data_age: Option<u64>,
    outcome: &EngineOutcome,
    coverage: &std::collections::BTreeMap<&'static str, Vec<kronika_reader::Gap>>,
    quality: &InputQuality,
    input_skipped: &[SectionSkip],
) -> Value {
    let incidents: Vec<Value> = outcome.incidents.iter().map(incident_to_json).collect();
    json!({
        "source_id": source,
        "from": scan.from,
        "to": scan.to,
        "complete": outcome.complete,
        "incidents": incidents,
        "coverage_by_section": coverage_to_json(coverage),
        "data_age_seconds": data_age.map_or(Value::Null, Value::from),
        "catalog": catalog_to_json(),
        "data_quality": quality_to_json(quality),
        "skipped": skipped_to_json(input_skipped, &outcome.skipped, outcome.span_splits),
    })
}

fn incident_to_json(incident: &Incident) -> Value {
    let members: Vec<Value> = incident.members.iter().map(member_to_json).collect();
    json!({
        "interval": { "from": incident.start_us, "to": incident.end_us },
        "incident_key": hex(incident.key.canonical_bytes()),
        "members": members,
        "findings": Value::Array(Vec::new()),
        "evaluation_complete": incident.evaluation_complete,
    })
}

fn member_to_json(member: &EpisodeRefV1) -> Value {
    let identity: Vec<Value> = member.identity.iter().map(identity_to_json).collect();
    json!({
        "logical_section": member.logical_section,
        "column": member.column,
        "identity": identity,
        "from": member.start_us,
        "to": member.end_us,
    })
}

fn identity_to_json(value: &IdentityValue) -> Value {
    match value {
        IdentityValue::I64(v) => (*v).into(),
        IdentityValue::U64(v) => (*v).into(),
        IdentityValue::Bool(v) => (*v).into(),
        IdentityValue::Text(v) => Value::String(v.clone()),
    }
}

/// The lens catalog: `applied` (empty until the lenses land) and `dormant`
/// lenses awaiting a later step. The field is always present so a consumer can
/// distinguish "no lens ran" from "the field is missing".
fn catalog_to_json() -> Value {
    json!({
        "applied": Value::Array(Vec::new()),
        "dormant": Value::Array(Vec::new()),
    })
}

fn quality_to_json(quality: &InputQuality) -> Value {
    json!({
        "non_canonical_identity": quality.non_canonical_identity,
        "non_finite_points": quality.non_finite_points,
        "first_points": quality.first_points,
        "resets": quality.resets,
        "gaps": quality.gaps,
        "not_collected": quality.not_collected,
        "anomalous_points": quality.anomalous_points,
        "invalid_gauge_points": quality.invalid_gauge_points,
        "duplicate_timestamps": quality.duplicate_timestamps,
    })
}

fn coverage_to_json(
    coverage: &std::collections::BTreeMap<&'static str, Vec<kronika_reader::Gap>>,
) -> Value {
    let object: serde_json::Map<String, Value> = coverage
        .iter()
        .map(|(&section, gaps)| {
            let gaps: Vec<Value> = gaps
                .iter()
                .map(|gap| json!({ "from": gap.from, "to": gap.to }))
                .collect();
            (section.to_owned(), Value::Array(gaps))
        })
        .collect();
    Value::Object(object)
}

/// Every reason a section or lens evaluation was dropped: input-side section
/// skips and engine-side lens skips, plus the count of clusters split by the
/// span cap.
fn skipped_to_json(
    input_skipped: &[SectionSkip],
    engine_skipped: &[EngineSkip],
    span_splits: u64,
) -> Value {
    let mut entries: Vec<Value> = input_skipped.iter().map(section_skip_to_json).collect();
    entries.extend(engine_skipped.iter().map(engine_skip_to_json));
    json!({
        "sections": entries,
        "span_splits": span_splits,
    })
}

fn section_skip_to_json(skip: &SectionSkip) -> Value {
    let reason = match skip.reason {
        SkipReason::MaterializationLimit { limit } => {
            json!({ "kind": "materialization_limit", "limit": limit })
        }
        SkipReason::IncompletePage => json!({ "kind": "incomplete_page" }),
        SkipReason::ScanBudget {
            required,
            available,
        } => json!({ "kind": "scan_budget", "required": required, "available": available }),
        SkipReason::ConflictingTimestamp { timestamp } => {
            json!({ "kind": "conflicting_timestamp", "timestamp": timestamp })
        }
        SkipReason::IdentityByteLimit { observed, limit } => {
            json!({ "kind": "identity_byte_limit", "observed": observed, "limit": limit })
        }
        SkipReason::SeriesPointLimit { observed, limit } => {
            json!({ "kind": "series_point_limit", "observed": observed, "limit": limit })
        }
    };
    json!({ "section": skip.section, "reason": reason })
}

fn engine_skip_to_json(skip: &EngineSkip) -> Value {
    json!({
        "lens_id": skip.lens_id.map_or(Value::Null, |id| Value::String(id.to_owned())),
        "axis": axis_name(skip.limit.axis),
        "observed": skip.limit.observed,
        "limit": skip.limit.limit,
    })
}

const fn axis_name(axis: LimitAxis) -> &'static str {
    match axis {
        LimitAxis::Work => "work",
        LimitAxis::LensEvaluations => "lens_evaluations",
        LimitAxis::Findings => "findings",
        LimitAxis::EvidenceRows => "evidence_rows",
    }
}

/// Lowercase hex of a byte slice, for the incident key.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_every_byte_as_two_lowercase_digits() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex(&[1]), "01");
    }

    #[test]
    fn identity_scalars_serialize_to_their_json_kinds() {
        assert_eq!(identity_to_json(&IdentityValue::I64(-3)), json!(-3));
        assert_eq!(identity_to_json(&IdentityValue::U64(7)), json!(7));
        assert_eq!(identity_to_json(&IdentityValue::Bool(true)), json!(true));
        assert_eq!(
            identity_to_json(&IdentityValue::Text("db".to_owned())),
            json!("db")
        );
    }

    #[test]
    fn axis_names_are_stable_wire_strings() {
        assert_eq!(axis_name(LimitAxis::Work), "work");
        assert_eq!(axis_name(LimitAxis::LensEvaluations), "lens_evaluations");
        assert_eq!(axis_name(LimitAxis::Findings), "findings");
        assert_eq!(axis_name(LimitAxis::EvidenceRows), "evidence_rows");
    }

    #[test]
    fn an_empty_response_carries_every_top_level_field() {
        let scan = ScanParams {
            from: 0,
            to: 10,
            window: 5,
            step: 1,
            threshold: 3.5,
            eps_rel: 0.05,
        };
        let body = empty_response(7, &scan, None);
        for field in [
            "source_id",
            "from",
            "to",
            "complete",
            "incidents",
            "coverage_by_section",
            "data_age_seconds",
            "catalog",
            "data_quality",
            "skipped",
        ] {
            assert!(body.get(field).is_some(), "missing top-level field {field}");
        }
        assert_eq!(body["incidents"], json!([]));
        assert_eq!(body["data_age_seconds"], Value::Null);
        assert!(body["catalog"].get("applied").is_some());
        assert!(body["catalog"].get("dormant").is_some());
    }
}
