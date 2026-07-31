use axum::response::IntoResponse as _;

use super::*;
use crate::api_error::{ApiError, ExpectedValue, LimitResource, QueryConstraint, QueryParameter};
use crate::reason::{ApiReason, MaterializationResource, ReasonKind};

#[test]
fn v5_query_names_and_constraints_are_closed_wire_values() {
    assert_eq!(
        serde_json::to_value(QueryParameter::Columns).expect("query parameter JSON"),
        "columns"
    );
    assert_eq!(
        serde_json::to_value(QueryParameter::Include).expect("query parameter JSON"),
        "include"
    );
    assert_eq!(
        serde_json::to_value(ExpectedValue::EntityToken).expect("expected value JSON"),
        "entity_token"
    );
    assert_eq!(
        serde_json::to_value(ExpectedValue::ProjectionColumnList).expect("expected value JSON"),
        "projection_column_list"
    );
    assert_eq!(
        serde_json::to_value(QueryConstraint::PointOrHistory).expect("constraint JSON"),
        "point_or_history"
    );
    assert_eq!(
        serde_json::to_value(QueryConstraint::HistorySupported).expect("constraint JSON"),
        "history_supported"
    );
    assert_eq!(
        serde_json::to_value(QueryConstraint::PresetOrColumns).expect("constraint JSON"),
        "preset_or_columns"
    );
}

#[tokio::test]
async fn selected_segment_shape_limit_is_400_without_changing_other_limit_statuses() {
    let selected = capture_json(
        ApiError::query_shape_limit_exceeded(LimitResource::SelectedSegments, 1_024, None)
            .into_response(),
    )
    .await;
    assert_api_error(
        &selected.body,
        StatusCode::BAD_REQUEST,
        "query_limit_exceeded",
        serde_json::json!({
            "resource": "selected_segments",
            "limit": 1_024,
        }),
    );

    let existing = capture_json(
        ApiError::query_limit_exceeded(LimitResource::Rows, 10, Some(11)).into_response(),
    )
    .await;
    assert_eq!(existing.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn routing_method_and_query_shape_use_the_closed_registry() {
    let (_dir, route) = fixture_captured("/v1/unknown", &[]).await;
    assert_eq!(route.status, StatusCode::NOT_FOUND);
    assert_api_error(
        &route.body,
        route.status,
        "route_not_found",
        serde_json::json!({}),
    );

    let (_dir, method) = fixture_request_captured(Method::POST, "/v1/version", &[]).await;
    assert_eq!(method.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_api_error(
        &method.body,
        method.status,
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
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_api_error(
        &unknown.body,
        unknown.status,
        "unknown_query_parameter",
        serde_json::json!({ "parameter": "locale" }),
    );

    let (_dir, malformed_path) = fixture_captured("/v1/section/%FF?from=0&to=1", &[]).await;
    assert_eq!(malformed_path.status, StatusCode::NOT_FOUND);
    assert_api_error(
        &malformed_path.body,
        malformed_path.status,
        "unknown_section",
        serde_json::json!({ "section": "invalid" }),
    );

    for malformed in ["%", "%0", "%GG", "%FF", "source=%FF"] {
        let uri = format!("/v1/version?{malformed}");
        let (_dir, response) = fixture_captured(&uri, &[]).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_api_error(
            &response.body,
            response.status,
            "invalid_query_parameter",
            serde_json::json!({
                "parameter": "query",
                "expected": "url_encoded_query",
            }),
        );
    }
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
