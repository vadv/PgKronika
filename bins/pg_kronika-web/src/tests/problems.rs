use axum::response::IntoResponse as _;

use super::*;
use crate::problem::{
    ApiProblem, ExpectedValue, LimitResource, ProblemCode, QueryConstraint, QueryParameter,
};
use crate::reason::{ApiReason, MaterializationResource, ReasonKind};

fn problem_example(code: ProblemCode) -> (ApiProblem, serde_json::Value) {
    match code {
        ProblemCode::Unauthorized => (ApiProblem::unauthorized(), serde_json::json!({})),
        ProblemCode::RouteNotFound => (ApiProblem::route_not_found(), serde_json::json!({})),
        ProblemCode::MethodNotAllowed => (
            ApiProblem::method_not_allowed("GET, HEAD"),
            serde_json::json!({}),
        ),
        ProblemCode::MissingQueryParameter => (
            ApiProblem::missing_query_parameter(QueryParameter::From),
            serde_json::json!({ "parameter": "from" }),
        ),
        ProblemCode::InvalidQueryParameter => (
            ApiProblem::invalid_query_parameter(QueryParameter::From, ExpectedValue::Int64),
            serde_json::json!({ "parameter": "from", "expected": "int64" }),
        ),
        ProblemCode::UnknownQueryParameter => (
            ApiProblem::unknown_query_parameter("unexpected"),
            serde_json::json!({ "parameter": "unexpected" }),
        ),
        ProblemCode::DuplicateQueryParameter => (
            ApiProblem::duplicate_query_parameter(QueryParameter::From),
            serde_json::json!({ "parameter": "from" }),
        ),
        ProblemCode::InvalidQueryConstraint => (
            ApiProblem::invalid_query_constraint(QueryConstraint::FromBeforeTo),
            serde_json::json!({ "constraint": "from_before_to" }),
        ),
        ProblemCode::UnknownSection => (
            ApiProblem::unknown_section("unknown_section"),
            serde_json::json!({ "section": "unknown_section" }),
        ),
        ProblemCode::InvalidCursor => (ApiProblem::invalid_cursor(), serde_json::json!({})),
        ProblemCode::CursorQueryMismatch => {
            (ApiProblem::cursor_query_mismatch(), serde_json::json!({}))
        }
        ProblemCode::CursorExpired => (ApiProblem::cursor_expired(), serde_json::json!({})),
        ProblemCode::ViewGone => (ApiProblem::view_gone(), serde_json::json!({})),
        ProblemCode::QueryLimitExceeded => (
            ApiProblem::query_limit_exceeded(LimitResource::Rows, 10, Some(11)),
            serde_json::json!({ "resource": "rows", "limit": 10, "observed": 11 }),
        ),
        ProblemCode::CursorCapacityUnavailable => (
            ApiProblem::cursor_capacity_unavailable(),
            serde_json::json!({}),
        ),
        ProblemCode::AnalyticCapacityUnavailable => (
            ApiProblem::analytic_capacity_unavailable(),
            serde_json::json!({ "retry_after_seconds": 1 }),
        ),
        ProblemCode::ColdBuildOverloaded => (
            ApiProblem::cold_build_overloaded(1),
            serde_json::json!({ "retry_after_seconds": 1 }),
        ),
        ProblemCode::StoreReadFailed => (ApiProblem::store_read_failed(), serde_json::json!({})),
        ProblemCode::InternalError => (ApiProblem::internal_error(), serde_json::json!({})),
    }
}

#[tokio::test]
async fn every_problem_code_has_the_exact_body_and_headers() {
    for code in ProblemCode::ALL {
        let (problem, params) = problem_example(code);
        assert_eq!(problem.code(), code);
        let response = capture_json(problem.into_response()).await;
        assert_eq!(response.status, code.status());
        assert_eq!(response.media_type(), Some("application/problem+json"));
        assert_eq!(
            response
                .headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(response.headers.get(header::CONTENT_LANGUAGE).is_none());
        assert!(response.headers.get(header::VARY).is_none());
        assert_problem(&response.body, response.status, code.as_str(), params);

        let request_id = response
            .headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .expect("problem request id");
        assert_eq!(
            response.body["instance"],
            format!("https://pgkronika.dev/problems/occurrences/{request_id}")
        );
        assert_eq!(
            response
                .headers
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            (code == ProblemCode::Unauthorized).then_some("Basic realm=\"pg_kronika-web\"")
        );
        assert_eq!(
            response
                .headers
                .get(header::ALLOW)
                .and_then(|value| value.to_str().ok()),
            (code == ProblemCode::MethodNotAllowed).then_some("GET, HEAD")
        );
        assert_eq!(
            response
                .headers
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            matches!(
                code,
                ProblemCode::AnalyticCapacityUnavailable | ProblemCode::ColdBuildOverloaded
            )
            .then(|| response.body["params"]["retry_after_seconds"].to_string())
            .as_deref()
        );
    }
}

#[tokio::test]
async fn selected_segment_shape_limit_is_400_without_changing_other_limit_statuses() {
    let selected = capture_json(
        ApiProblem::query_shape_limit_exceeded(LimitResource::SelectedSegments, 1_024, None)
            .into_response(),
    )
    .await;
    assert_problem(
        &selected.body,
        StatusCode::BAD_REQUEST,
        "query_limit_exceeded",
        serde_json::json!({
            "resource": "selected_segments",
            "limit": 1_024,
        }),
    );
    assert_eq!(selected.media_type(), Some("application/problem+json"));
    assert_eq!(
        selected
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );

    let existing = capture_json(
        ApiProblem::query_limit_exceeded(LimitResource::Rows, 10, Some(11)).into_response(),
    )
    .await;
    assert_eq!(existing.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn accept_language_does_not_change_success_or_problem_semantics() {
    let (_dir, english) = fixture_captured("/v1/version", &[("accept-language", "en")]).await;
    let (_dir, russian) =
        fixture_captured("/v1/version", &[("accept-language", "ru-RU, ru;q=0.9")]).await;
    assert_eq!(english.body, russian.body);
    assert_eq!(english.media_type(), Some("application/json"));
    for response in [&english, &russian] {
        assert!(response.headers.get(header::CONTENT_LANGUAGE).is_none());
        assert!(response.headers.get(header::VARY).is_none());
    }

    let uri = "/v1/segments?unexpected=value&from=0&to=1";
    let (_dir, english) = fixture_captured(uri, &[("accept-language", "en")]).await;
    let (_dir, russian) = fixture_captured(uri, &[("accept-language", "ru")]).await;
    for response in [&english, &russian] {
        assert_problem(
            &response.body,
            StatusCode::BAD_REQUEST,
            "unknown_query_parameter",
            serde_json::json!({ "parameter": "unexpected" }),
        );
        assert!(response.headers.get(header::CONTENT_LANGUAGE).is_none());
        assert!(response.headers.get(header::VARY).is_none());
    }
    let mut english = english.body;
    let mut russian = russian.body;
    english
        .as_object_mut()
        .expect("problem object")
        .remove("instance");
    russian
        .as_object_mut()
        .expect("problem object")
        .remove("instance");
    assert_eq!(english, russian);
}

#[tokio::test]
async fn routing_method_and_query_shape_use_the_closed_registry() {
    let (_dir, route) = fixture_captured("/v1/unknown", &[]).await;
    assert_problem(
        &route.body,
        StatusCode::NOT_FOUND,
        "route_not_found",
        serde_json::json!({}),
    );

    let (_dir, method) = fixture_request_captured(Method::POST, "/v1/version", &[]).await;
    assert_problem(
        &method.body,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        serde_json::json!({}),
    );
    assert_eq!(
        method
            .headers
            .get(header::ALLOW)
            .and_then(|value| value.to_str().ok()),
        Some("GET, HEAD")
    );

    let (_dir, unknown) = fixture_captured("/v1/version?locale=ru", &[]).await;
    assert_problem(
        &unknown.body,
        StatusCode::BAD_REQUEST,
        "unknown_query_parameter",
        serde_json::json!({ "parameter": "locale" }),
    );

    let (_dir, duplicate) = fixture_captured("/v1/segments?unexpected=8&from=0&to=1", &[]).await;
    assert_problem(
        &duplicate.body,
        StatusCode::BAD_REQUEST,
        "unknown_query_parameter",
        serde_json::json!({ "parameter": "unexpected" }),
    );

    let (_dir, malformed_path) = fixture_captured("/v1/section/%FF?from=0&to=1", &[]).await;
    assert_problem(
        &malformed_path.body,
        StatusCode::NOT_FOUND,
        "unknown_section",
        serde_json::json!({ "section": "invalid" }),
    );

    for malformed in ["%", "%0", "%GG", "%FF", "source=%FF"] {
        let uri = format!("/v1/version?{malformed}");
        let (_dir, response) = fixture_captured(&uri, &[]).await;
        assert_problem(
            &response.body,
            StatusCode::BAD_REQUEST,
            "invalid_query_parameter",
            serde_json::json!({
                "parameter": "query",
                "expected": "url_encoded_query",
            }),
        );
    }
}

#[tokio::test]
async fn documented_v1_paths_reach_the_actual_router_with_contextual_allow() {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid OpenAPI JSON");
    for path in document["paths"]
        .as_object()
        .expect("OpenAPI paths")
        .keys()
        .filter(|path| path.starts_with("/v1"))
    {
        let concrete = path.replace("{name}", "pg_stat_archiver");
        let (_dir, get_response) = fixture_captured(&concrete, &[]).await;
        assert_ne!(
            get_response.body["code"], "route_not_found",
            "documented GET {path} must reach a registered route"
        );

        let (_dir, method_response) = fixture_request_captured(Method::POST, &concrete, &[]).await;
        assert_problem(
            &method_response.body,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            serde_json::json!({}),
        );
        assert_eq!(
            method_response
                .headers
                .get(header::ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("GET, HEAD"),
            "Allow for {path}"
        );

        let documented_query: std::collections::BTreeSet<_> =
            document["paths"][path]["get"]["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|parameter| parameter["$ref"].as_str())
                .filter_map(|reference| reference.rsplit('/').next())
                .filter_map(|name| {
                    let parameter = &document["components"]["parameters"][name];
                    (parameter["in"] == "query")
                        .then(|| parameter["name"].as_str().expect("query parameter name"))
                })
                .collect();
        for parameter in QueryParameter::ALL {
            let uri = format!("{concrete}?{}=1", parameter.as_str());
            let (_dir, response) = fixture_captured(&uri, &[]).await;
            let rejected_as_unknown = response.body["code"] == "unknown_query_parameter";
            assert_eq!(
                rejected_as_unknown,
                !documented_query.contains(parameter.as_str()),
                "router/OpenAPI query allowlist for {path}: {}",
                parameter.as_str()
            );
        }
    }
}

#[tokio::test]
async fn generated_request_ids_are_unique_and_match_the_instance_header_invariant() {
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..100 {
        let response = capture_json(ApiProblem::internal_error().into_response()).await;
        let request_id = response
            .headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .expect("server request id");
        assert!(ids.insert(request_id.to_owned()), "duplicate request id");
        assert_eq!(
            response.body["instance"],
            format!("https://pgkronika.dev/problems/occurrences/{request_id}")
        );
    }
}

#[tokio::test]
async fn correlation_is_server_generated_and_does_not_reflect_request_data() {
    let secret = "client-secret-request-id";
    let (_dir, response) = fixture_captured(
        "/v1/segments?unexpected=%2Fprivate%2Fstore&from=0&to=1",
        &[("x-request-id", secret)],
    )
    .await;
    let request_id = response
        .headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("server request id");
    assert_ne!(request_id, secret);
    let rendered = serde_json::to_string(&response.body).expect("problem JSON");
    assert!(!rendered.contains(secret));
    assert!(!rendered.contains("/private/store"));
    assert!(!rendered.contains("detail"));
    assert!(!rendered.contains("error"));
}

fn reason_example(kind: ReasonKind) -> ApiReason {
    match kind {
        ReasonKind::MaterializationLimit => {
            ApiReason::materialization_limit(MaterializationResource::Cells, 10)
        }
        ReasonKind::IncompletePage => ApiReason::incomplete_page(),
        ReasonKind::ScoringWorkBudget => ApiReason::scoring_work_budget(11, 10),
        ReasonKind::ScanBudget => ApiReason::scan_budget(11, 10),
        ReasonKind::ConflictingTimestamp => ApiReason::conflicting_timestamp(42),
        ReasonKind::IdentityByteLimit => ApiReason::identity_byte_limit(11, 10),
        ReasonKind::SeriesPointLimit => ApiReason::series_point_limit(11, 10),
        ReasonKind::TypedGaugePointLimit => ApiReason::typed_gauge_point_limit(11, 10),
        ReasonKind::SnapshotRowLimit => ApiReason::snapshot_row_limit(11, 10),
        ReasonKind::IncompleteSnapshot => ApiReason::incomplete_snapshot(),
        ReasonKind::RetentionLimit => ApiReason::retention_limit(1),
        ReasonKind::NoData => ApiReason::no_data(),
        ReasonKind::MissingNodeIdentity => ApiReason::missing_node_identity(),
        ReasonKind::ConflictingNodeIdentity => ApiReason::conflicting_node_identity(),
        ReasonKind::ProducerUnavailable => ApiReason::producer_unavailable(),
        ReasonKind::ProvenanceOrInputMissing => ApiReason::provenance_or_input_missing(),
        ReasonKind::CompleteProvenance => ApiReason::complete_provenance(0),
        ReasonKind::SectionAbsent => ApiReason::section_absent(),
        ReasonKind::CompleteCoverage => ApiReason::complete_coverage(0),
        ReasonKind::CoverageGap => ApiReason::coverage_gap(2),
        ReasonKind::EmptyIncidentWindow => ApiReason::empty_incident_window(),
        ReasonKind::InsufficientIntervalsForObservedPeriod => {
            ApiReason::insufficient_intervals_for_observed_period()
        }
        ReasonKind::IncidentWindowShorterThanObservedPeriod => {
            ApiReason::incident_window_shorter_than_observed_period()
        }
    }
}

#[test]
fn every_reason_kind_has_only_kind_and_typed_params() {
    for kind in ReasonKind::ALL {
        let reason = reason_example(kind);
        assert_eq!(reason.kind(), kind);
        let value = serde_json::to_value(reason).expect("reason JSON");
        let object = value.as_object().expect("reason object");
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["kind", "params"]);
        assert!(value["kind"].is_string());
        assert!(value["params"].is_object());
        assert!(value.get("reason").is_none());
        assert!(value.get("detail").is_none());
    }
}

#[test]
fn openapi_is_a_closed_projection_of_the_machine_registries() {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid OpenAPI JSON");
    assert_eq!(document["openapi"], "3.2.0");
    assert_eq!(
        document["jsonSchemaDialect"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_problem_schema(&document);
    assert_reason_schema(&document);
    assert_params_schemas(&document);
    assert_changed_success_schemas(&document);
    assert_timeline_contract(&document);
    assert_v1_media_and_locale_contract(&document);
    assert_local_refs_resolve(&document, &document);
}

fn assert_problem_schema(document: &serde_json::Value) {
    let problem = &document["components"]["schemas"]["Problem"];
    assert!(
        problem.get("$id").is_none(),
        "embedded schemas inherit the document base URI"
    );
    assert_eq!(problem["additionalProperties"], false);
    assert_eq!(
        problem["required"],
        serde_json::json!(["type", "status", "code", "params", "instance"])
    );
    assert_eq!(problem["properties"]["instance"]["format"], "uri");
    let documented_codes: Vec<_> = problem["properties"]["code"]["enum"]
        .as_array()
        .expect("problem code enum")
        .iter()
        .map(|value| value.as_str().expect("problem code string"))
        .collect();
    let code_registry: Vec<_> = ProblemCode::ALL
        .into_iter()
        .map(ProblemCode::as_str)
        .collect();
    assert_eq!(documented_codes, code_registry);

    let branches = problem["allOf"].as_array().expect("problem branches");
    for code in ProblemCode::ALL {
        let branch = branches
            .iter()
            .find(|branch| branch["if"]["properties"]["code"]["const"] == code.as_str())
            .expect("one branch for every problem code");
        assert_eq!(
            branch["then"]["properties"]["type"]["const"],
            code.type_uri()
        );
        if code == ProblemCode::QueryLimitExceeded {
            assert_eq!(
                branch["then"]["properties"]["status"]["enum"],
                serde_json::json!([400, 413])
            );
            let selected = &branch["then"]["allOf"][0];
            assert_eq!(
                selected["if"]["properties"]["params"]["properties"]["resource"]["const"],
                "selected_segments"
            );
            assert_eq!(
                selected["then"]["properties"]["status"]["const"],
                StatusCode::BAD_REQUEST.as_u16()
            );
            assert_eq!(
                selected["else"]["properties"]["status"]["const"],
                StatusCode::PAYLOAD_TOO_LARGE.as_u16()
            );
        } else {
            assert_eq!(
                branch["then"]["properties"]["status"]["const"],
                u64::from(code.status().as_u16())
            );
        }
        assert!(branch["then"]["properties"]["params"]["$ref"].is_string());
    }
    assert_eq!(branches.len(), ProblemCode::ALL.len());
    let application_problem = &document["components"]["responses"]["ApplicationProblem"];
    let content = application_problem["content"]
        .as_object()
        .expect("application problem content");
    assert_eq!(content.len(), 1);
    assert!(content.contains_key("application/problem+json"));
    for (response, header_name) in [
        ("UnauthorizedProblem", "WWW-Authenticate"),
        ("MethodNotAllowedProblem", "Allow"),
        ("TimelineCapacityProblem", "Retry-After"),
    ] {
        assert!(
            document["components"]["responses"][response]["headers"]
                .as_object()
                .is_some_and(|headers| headers.contains_key(header_name)),
            "{response} documents {header_name}"
        );
    }
}

fn assert_reason_schema(document: &serde_json::Value) {
    let reason = &document["components"]["schemas"]["Reason"];
    assert!(
        reason.get("$id").is_none(),
        "embedded schemas inherit the document base URI"
    );
    assert_eq!(reason["additionalProperties"], false);
    assert_eq!(reason["required"], serde_json::json!(["kind", "params"]));
    let documented_kinds: Vec<_> = reason["properties"]["kind"]["enum"]
        .as_array()
        .expect("reason kind enum")
        .iter()
        .map(|value| value.as_str().expect("reason kind string"))
        .collect();
    let reason_registry: Vec<_> = ReasonKind::ALL
        .into_iter()
        .map(|kind| {
            let value = serde_json::to_value(reason_example(kind)).expect("reason JSON");
            value["kind"]
                .as_str()
                .expect("serialized reason kind")
                .to_owned()
        })
        .collect();
    assert_eq!(documented_kinds, reason_registry);
    let reason_branches = reason["allOf"].as_array().expect("reason branches");
    for kind in &reason_registry {
        let matching: Vec<_> = reason_branches
            .iter()
            .filter(|branch| {
                let selector = &branch["if"]["properties"]["kind"];
                selector["const"].as_str() == Some(kind)
                    || selector["enum"]
                        .as_array()
                        .is_some_and(|values| values.iter().any(|value| value == kind))
            })
            .collect();
        assert_eq!(matching.len(), 1, "one typed params branch for {kind}");
        assert!(matching[0]["then"]["properties"]["params"]["$ref"].is_string());
    }
}

fn assert_params_schemas(document: &serde_json::Value) {
    assert_schema_enum_matches(
        document,
        "/components/schemas/KnownParameter/enum",
        &QueryParameter::ALL,
    );
    let any_locations = document
        .pointer("/components/schemas/AnyParameterLocation/enum")
        .and_then(serde_json::Value::as_array)
        .expect("all invalid-parameter locations");
    let expected_locations: Vec<_> = std::iter::once("query")
        .chain(QueryParameter::ALL.into_iter().map(QueryParameter::as_str))
        .map(serde_json::Value::from)
        .collect();
    assert_eq!(any_locations, &expected_locations);
    for parameter in QueryParameter::ALL {
        assert_eq!(
            QueryParameter::from_query_name(parameter.as_str()),
            Some(parameter),
            "query registry round-trip"
        );
    }
    assert_schema_enum_matches(
        document,
        "/components/schemas/InvalidParameterParams/properties/expected/enum",
        &ExpectedValue::ALL,
    );
    assert_schema_enum_matches(
        document,
        "/components/schemas/ConstraintParams/properties/constraint/enum",
        &QueryConstraint::ALL,
    );
    assert_schema_enum_matches(
        document,
        "/components/schemas/LimitParams/properties/resource/enum",
        &LimitResource::ALL,
    );
    assert_schema_enum_matches(
        document,
        "/components/schemas/MaterializationReasonParams/properties/resource/enum",
        &MaterializationResource::ALL,
    );

    for schema in [
        "EmptyParams",
        "ParameterParams",
        "InvalidParameterParams",
        "UnknownParameterParams",
        "ConstraintParams",
        "SectionParams",
        "LimitParams",
        "CapacityParams",
        "MaterializationReasonParams",
        "WorkReasonParams",
        "TimestampReasonParams",
        "ObservedLimitReasonParams",
        "DroppedReasonParams",
        "GapCountReasonParams",
    ] {
        assert_eq!(
            document["components"]["schemas"][schema]["additionalProperties"], false,
            "{schema} must reject undeclared params"
        );
    }
}

fn assert_changed_success_schemas(document: &serde_json::Value) {
    assert_changed_schemas_are_closed(document);
    assert_incident_catalog_schema(document);
    assert_changed_schema_details(document);
}

fn assert_changed_schemas_are_closed(document: &serde_json::Value) {
    for schema in [
        "Sections",
        "SectionCatalogEntry",
        "SectionColumn",
        "Segments",
        "Segment",
        "SegmentSection",
        "DiffValuePoint",
        "DiffNoDataPoint",
        "DiffSeries",
        "DiffSection",
        "DiffResponse",
        "AnomalyParameters",
        "AnomalyPeak",
        "AnomalyEpisode",
        "PlanDistributionScope",
        "PlanDistributionParameters",
        "PlanDistributionCoverage",
        "PlanShareEvidence",
        "PlanDistributionEvidence",
        "PlanDistributionSignal",
        "PlanBufferScope",
        "PlanBufferDimension",
        "PlanBufferParameters",
        "PlanBufferCoverage",
        "PlanBufferEvidence",
        "PlanBufferSignal",
        "PlanDetectorNotEvaluated",
        "PlanDetectorCounts",
        "PlanQuality",
        "PlanApplicability",
        "PlanWork",
        "PlanAnalysisTruncation",
        "PlanAnalysis",
        "AnomalyNotEvaluated",
        "AnomalySectionCounts",
        "AnomalyCoverage",
        "AnomalyTruncation",
        "AnomalyResponse",
        "SectionReason",
        "IncidentResponse",
        "IncidentCatalog",
        "IncidentCatalogCounts",
        "IncidentEvaluator",
        "ActiveIncidentLens",
        "IncidentRequirement",
        "IncidentRequirementIdentity",
        "IncidentRequirementCondition",
        "LensCapability",
        "InactiveIncidentLens",
        "IncidentLog",
        "EventCatalogEntry",
        "IncidentSkipped",
        "AnalysisReason",
        "EngineSkip",
    ] {
        assert_eq!(
            document["components"]["schemas"][schema]["additionalProperties"], false,
            "{schema} must reject undeclared presentation fields"
        );
    }
}

fn assert_incident_catalog_schema(document: &serde_json::Value) {
    for schema in [
        "ActiveIncidentLens",
        "IncidentRequirement",
        "IncidentRequirementIdentity",
        "IncidentRequirementCondition",
        "InactiveIncidentLens",
        "EventCatalogEntry",
    ] {
        let properties = document["components"]["schemas"][schema]["properties"]
            .as_object()
            .expect("catalog properties");
        for forbidden in ["title", "question", "text_locale"] {
            assert!(!properties.contains_key(forbidden), "{schema}.{forbidden}");
        }
    }
    let incident_counts = &document["components"]["schemas"]["IncidentCatalogCounts"]["properties"];
    for (field, expected) in [
        ("core_lenses", 28),
        ("event_branches", 14),
        ("evaluator_branches", 42),
        ("unique_lens_ids", 40),
        ("active_lens_ids", 40),
        ("inactive_lens_ids", 0),
        ("entity_join_requirements", 24),
    ] {
        assert_eq!(incident_counts[field]["const"], expected, "{field}");
    }
    assert_eq!(
        document["components"]["schemas"]["IncidentRequirement"]["properties"]["requirement_id"]
            ["enum"]
            .as_array()
            .map(Vec::len),
        Some(24)
    );
    assert_eq!(
        document["components"]["schemas"]["IncidentRequirement"]["properties"]["contract"]["enum"]
            .as_array()
            .map(Vec::len),
        Some(24)
    );
    let incident_log = &document["components"]["schemas"]["IncidentLog"]["properties"];
    assert_eq!(incident_log["catalog"]["minItems"], 14);
    assert_eq!(incident_log["catalog"]["maxItems"], 14);
    assert_eq!(
        incident_log["coverage"]["additionalProperties"]["enum"],
        serde_json::json!(["unknown", "gap", "not_collected"])
    );
    let condition_kinds = document["components"]["schemas"]["IncidentRequirement"]["properties"]
        ["conditions"]["allOf"]
        .as_array()
        .expect("closed condition-kind triple");
    assert_eq!(condition_kinds.len(), 3);
    for condition in condition_kinds {
        assert_eq!(condition["minContains"], 1);
        assert_eq!(condition["maxContains"], 1);
    }
    let openapi_requirement_ids: std::collections::BTreeSet<_> = document["components"]["schemas"]
        ["IncidentRequirement"]["properties"]["requirement_id"]["enum"]
        .as_array()
        .expect("requirement id enum")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect();
    let mut executable_requirement_ids = std::collections::BTreeSet::new();
    for lens in crate::incident::core_catalog() {
        if let Some(contract) = lens.entity_join_contract() {
            executable_requirement_ids.insert(format!("entity_join.{}", contract.as_str()));
        }
    }
    assert_eq!(openapi_requirement_ids, executable_requirement_ids);
}

fn assert_changed_schema_details(document: &serde_json::Value) {
    assert_eq!(
        document["components"]["schemas"]["DiffNoDataPoint"]["properties"]["nodata"]["enum"],
        serde_json::json!(["reset", "gap", "first_point", "anomaly", "not_collected"])
    );
    assert_eq!(
        document["components"]["schemas"]["PlanBufferCoverage"]["properties"]["instance_identity"]
            ["const"],
        "pg_system_identifier"
    );
    for schema in ["PlanQuality", "AnomalyCoverage"] {
        let object = &document["components"]["schemas"][schema];
        let required: std::collections::BTreeSet<_> = object["required"]
            .as_array()
            .expect("required fields")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let properties: std::collections::BTreeSet<_> = object["properties"]
            .as_object()
            .expect("schema properties")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(required, properties, "{schema} fields must all be required");
    }
}

fn assert_timeline_contract(document: &serde_json::Value) {
    assert_timeline_schema_contract(document);
    assert_timeline_endpoint_contract(document);
}

#[allow(
    clippy::too_many_lines,
    reason = "the assertions enumerate the complete public timeline schema contract"
)]
fn assert_timeline_schema_contract(document: &serde_json::Value) {
    let schemas = &document["components"]["schemas"];
    for schema in [
        "CoverageSpan",
        "TimelineFreshness",
        "TimelineLoss",
        "TimelineMeta",
        "SupportingEvidence",
        "EventLoss",
        "ErrorEventPayload",
        "LifecycleEventPayload",
        "CounterDeltaEventPayload",
        "CapacityEventPayload",
        "EventEntity",
        "EventFact",
        "SqlstateCount",
        "JointCount",
        "SignalCount",
        "LifecycleDigest",
        "EventDigest",
        "NotablePreview",
        "HealthDomainPenalty",
        "HealthFloorEvidence",
        "HealthSourcePopulation",
        "HealthFactorCoverage",
        "HealthPoint",
        "TimelineHealthSummary",
        "TimelineOverviewResponse",
        "TimelineEventsResponse",
        "TimelineHealthResponse",
    ] {
        let object = &schemas[schema];
        assert_eq!(
            object["additionalProperties"], false,
            "{schema} must reject undeclared fields"
        );
        let properties: std::collections::BTreeSet<_> = object["properties"]
            .as_object()
            .expect("timeline schema properties")
            .keys()
            .map(String::as_str)
            .collect();
        let required: std::collections::BTreeSet<_> = object["required"]
            .as_array()
            .expect("timeline schema required fields")
            .iter()
            .map(|field| field.as_str().expect("required field name"))
            .collect();
        assert_eq!(
            required, properties,
            "{schema} must require every serialized field, including nullable fields"
        );
    }

    assert_eq!(schemas["B64UrlSha256"]["pattern"], "^[A-Za-z0-9_-]{43}$");
    assert_eq!(schemas["B64Url128"]["pattern"], "^[A-Za-z0-9_-]{22}$");
    assert_eq!(
        schemas["TimelineEventsCursor"]["pattern"],
        "^[A-Za-z0-9_-]{270}$"
    );
    let cursor_description = schemas["TimelineEventsCursor"]["description"]
        .as_str()
        .expect("timeline cursor description");
    assert!(cursor_description.contains("process-local"));
    assert!(cursor_description.contains("not durable across process restarts"));
    let event_properties = &schemas["EventFact"]["properties"];
    let event_id_description = event_properties["event_id"]["description"]
        .as_str()
        .expect("event ID description");
    assert!(event_id_description.contains("excludes storage path"));
    assert!(event_id_description.contains("active-to-sealed"));
    let event_instance_description = event_properties["event_instance_id"]["description"]
        .as_str()
        .expect("event instance ID description");
    assert!(event_instance_description.contains("retained occurrences"));
    assert!(event_instance_description.contains("provenance"));
    assert_eq!(
        schemas["EventFact"]["properties"]["section_type_id"]["type"],
        serde_json::json!(["integer", "null"])
    );
    assert_eq!(
        schemas["EventFact"]["properties"]["supporting_evidence"]["minItems"],
        1
    );
    assert_eq!(
        schemas["EventFact"]["properties"]["supporting_evidence"]["maxItems"],
        3
    );
    assert_eq!(
        schemas["EventFact"]["properties"]["payload"]["$ref"],
        "#/components/schemas/EventPayload"
    );
    assert_eq!(
        schemas["NotablePreview"]["properties"]["observations"]["maxItems"],
        100
    );
    assert_eq!(
        schemas["TimelineEventsResponse"]["properties"]["events"]["maxItems"],
        1000
    );
    assert_eq!(
        schemas["TimelineHealthResponse"]["properties"]["points"]["maxItems"],
        2000
    );
    assert_eq!(
        schemas["TimelineMeta"]["properties"]["response_schema_version"]["const"],
        2
    );
    assert_eq!(
        schemas["TimelineEventsResponse"]["properties"]["notable_policy_version"]["const"],
        2
    );
    for response in [
        "TimelineOverviewResponse",
        "TimelineEventsResponse",
        "TimelineHealthResponse",
    ] {
        assert_eq!(
            schemas[response]["properties"]["coverage"]["maxItems"], 65_536,
            "{response} factor coverage bound must match the handler"
        );
        assert_eq!(
            schemas[response]["properties"]["coverage"]["items"]["$ref"],
            "#/components/schemas/HealthFactorCoverage"
        );
    }
    for (property, width) in [("by_severity", 5), ("by_category", 11)] {
        assert_eq!(
            schemas["EventDigest"]["properties"][property]["minItems"],
            width
        );
        assert_eq!(
            schemas["EventDigest"]["properties"][property]["maxItems"],
            width
        );
    }
    for property in ["by_sqlstate", "joint_top"] {
        assert_eq!(
            schemas["EventDigest"]["properties"][property]["maxItems"],
            16
        );
    }
}

fn assert_timeline_endpoint_contract(document: &serde_json::Value) {
    let parameters = &document["components"]["parameters"];
    assert!(parameters.get("timelineEventSources").is_none());
    assert!(parameters.get("timelineSource").is_none());
    assert_eq!(parameters["timelineFrom"]["schema"]["format"], "int64");
    assert_eq!(parameters["timelineTo"]["schema"]["format"], "int64");
    assert_eq!(
        parameters["timelineHealthStep"]["schema"]["type"],
        "integer"
    );
    assert_eq!(parameters["timelineEventsLimit"]["schema"]["default"], 100);
    assert_eq!(parameters["timelineEventsLimit"]["schema"]["minimum"], 1);
    assert_eq!(parameters["timelineEventsLimit"]["schema"]["maximum"], 1000);

    let paths = &document["paths"];
    assert_eq!(
        paths["/v1/timeline/overview"]["get"]["parameters"],
        serde_json::json!([
            { "$ref": "#/components/parameters/timelineFrom" },
            { "$ref": "#/components/parameters/timelineTo" }
        ])
    );
    assert_eq!(
        paths["/v1/timeline/events"]["get"]["parameters"],
        serde_json::json!([
            { "$ref": "#/components/parameters/timelineFrom" },
            { "$ref": "#/components/parameters/timelineTo" },
            { "$ref": "#/components/parameters/timelineEventsLimit" },
            { "$ref": "#/components/parameters/timelineEventsCursor" },
            { "$ref": "#/components/parameters/minSeverity" },
            { "$ref": "#/components/parameters/kind" }
        ])
    );
    assert_eq!(
        paths["/v1/timeline/health"]["get"]["parameters"],
        serde_json::json!([
            { "$ref": "#/components/parameters/timelineFrom" },
            { "$ref": "#/components/parameters/timelineTo" },
            { "$ref": "#/components/parameters/timelineHealthStep" }
        ])
    );
    assert_eq!(
        paths["/v1/timeline/events"]["get"]["responses"]["410"]["$ref"],
        "#/components/responses/ApplicationProblem"
    );
    assert_eq!(
        paths["/v1/timeline/events"]["get"]["responses"]["503"]["$ref"],
        "#/components/responses/TimelineCapacityProblem"
    );
    let events_description = paths["/v1/timeline/events"]["get"]["description"]
        .as_str()
        .expect("timeline events description");
    assert!(events_description.contains("process-local"));
    assert!(events_description.contains("cursor_expired"));
    assert!(events_description.contains("HTTP 410"));
    let cursor_description = parameters["timelineEventsCursor"]["description"]
        .as_str()
        .expect("timeline cursor parameter description");
    assert!(cursor_description.contains("previous process"));
    assert!(cursor_description.contains("cursor_expired"));
    for path in [
        "/v1/timeline/overview",
        "/v1/timeline/events",
        "/v1/timeline/health",
    ] {
        assert_eq!(
            paths[path]["get"]["responses"]["400"]["$ref"],
            "#/components/responses/TimelineRequestProblem"
        );
        assert_eq!(
            paths[path]["get"]["responses"]["503"]["$ref"],
            "#/components/responses/TimelineCapacityProblem"
        );
        let description = paths[path]["get"]["description"]
            .as_str()
            .expect("timeline description");
        assert!(description.contains("1024"));
        assert!(description.contains("4096"));
    }
}

fn assert_v1_documented_paths(document: &serde_json::Value) {
    let paths = document["paths"].as_object().expect("OpenAPI paths");
    let mut documented_paths: Vec<_> = paths.keys().map(String::as_str).collect();
    documented_paths.sort_unstable();
    assert_eq!(
        documented_paths,
        [
            "/healthz",
            "/metrics",
            "/readyz",
            "/v1/anomalies",
            "/v1/incidents",
            "/v1/section/{name}",
            "/v1/section/{name}/diff",
            "/v1/sections",
            "/v1/sections/batch",
            "/v1/sections/batch/diff",
            "/v1/segments",
            "/v1/timeline/events",
            "/v1/timeline/health",
            "/v1/timeline/heatmap",
            "/v1/timeline/overview",
            "/v1/ui/catalog",
            "/v1/version",
            "/v1/views/summary",
        ]
    );

    let expected_success_schemas = [
        ("/v1/version", "Version"),
        ("/v1/sections", "Sections"),
        ("/v1/segments", "Segments"),
        ("/v1/section/{name}", "NeutralObject"),
        ("/v1/sections/batch", "NeutralObject"),
        ("/v1/section/{name}/diff", "DiffResponse"),
        ("/v1/sections/batch/diff", "BatchDiffResponse"),
        ("/v1/timeline/overview", "TimelineOverviewResponse"),
        ("/v1/timeline/events", "TimelineEventsResponse"),
        ("/v1/timeline/health", "TimelineHealthResponse"),
        ("/v1/timeline/heatmap", "UiHeatmapResponse"),
        ("/v1/ui/catalog", "UiProjectionCatalog"),
        ("/v1/views/summary", "UiViewSummaryResponse"),
        ("/v1/anomalies", "AnomalyResponse"),
        ("/v1/incidents", "IncidentResponse"),
    ];
    for (path, schema) in expected_success_schemas {
        assert_eq!(
            document["paths"][path]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str(),
            Some(format!("#/components/schemas/{schema}").as_str()),
            "{path} success schema"
        );
    }
}

fn assert_v1_media_and_locale_contract(document: &serde_json::Value) {
    assert_v1_documented_paths(document);
    let paths = document["paths"].as_object().expect("OpenAPI paths");
    let metrics_content = document["paths"]["/metrics"]["get"]["responses"]["200"]["content"]
        .as_object()
        .expect("metrics content");
    assert_eq!(
        metrics_content
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["text/plain; version=0.0.4"]
    );

    for (path, item) in paths {
        let methods = item.as_object().expect("path item");
        assert_eq!(methods.len(), 1, "{path} exposes only GET");
        assert!(methods.contains_key("get"), "{path} exposes only GET");
        if !path.starts_with("/v1") {
            continue;
        }
        let operation = &item["get"];
        let success_content = operation["responses"]["200"]["content"]
            .as_object()
            .expect("application success content");
        assert_eq!(success_content.len(), 1, "{path} success media type");
        assert!(
            success_content.contains_key("application/json"),
            "{path} success media type"
        );
        assert_eq!(
            operation["responses"]["413"]["$ref"], "#/components/responses/ApplicationProblem",
            "{path} enforces the raw-query ceiling"
        );
        assert_eq!(
            operation["responses"]["401"]["$ref"], "#/components/responses/UnauthorizedProblem",
            "{path} authentication response"
        );
        assert_eq!(
            operation["responses"]["405"]["$ref"], "#/components/responses/MethodNotAllowedProblem",
            "{path} method response"
        );
        for (status, response) in operation["responses"]
            .as_object()
            .expect("operation responses")
        {
            if path == "/v1/ui/catalog" && status == "304" {
                assert!(
                    response["headers"]["ETag"]["schema"]["pattern"].is_string(),
                    "catalog 304 repeats its strong ETag"
                );
                assert!(
                    response.get("content").is_none(),
                    "catalog 304 carries no representation body"
                );
                continue;
            }
            if status != "200" {
                let expected = match status.as_str() {
                    "401" => "#/components/responses/UnauthorizedProblem",
                    "405" => "#/components/responses/MethodNotAllowedProblem",
                    "400"
                        if matches!(
                            path.as_str(),
                            "/v1/timeline/overview" | "/v1/timeline/events" | "/v1/timeline/health"
                        ) =>
                    {
                        "#/components/responses/TimelineRequestProblem"
                    }
                    "503"
                        if matches!(
                            path.as_str(),
                            "/v1/timeline/overview" | "/v1/timeline/events" | "/v1/timeline/health"
                        ) =>
                    {
                        "#/components/responses/TimelineCapacityProblem"
                    }
                    "503" => "#/components/responses/AnalyticCapacityProblem",
                    _ => "#/components/responses/ApplicationProblem",
                };
                assert_eq!(response["$ref"], expected, "{path} status {status}");
            }
        }
        for parameter in operation["parameters"].as_array().into_iter().flatten() {
            assert_ne!(parameter["name"], "locale");
            assert_ne!(parameter["name"], "Accept-Language");
        }
    }
}

fn assert_schema_enum_matches<T>(document: &serde_json::Value, pointer: &str, registry: &[T])
where
    T: serde::Serialize,
{
    let documented = document.pointer(pointer).expect("OpenAPI enum");
    let registered = serde_json::to_value(registry).expect("serialized registry");
    assert_eq!(documented, &registered, "OpenAPI enum at {pointer}");
}

fn assert_local_refs_resolve(document: &serde_json::Value, value: &serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                assert_local_refs_resolve(document, value);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str)
                && let Some(pointer) = reference.strip_prefix('#')
            {
                assert!(
                    document.pointer(pointer).is_some(),
                    "unresolved OpenAPI reference {reference}"
                );
            }
            for value in object.values() {
                assert_local_refs_resolve(document, value);
            }
        }
        _ => {}
    }
}
