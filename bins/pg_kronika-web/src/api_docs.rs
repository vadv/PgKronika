//! One registry for documented HTTP routes and their generated `OpenAPI` document.

use axum::Router;
use utoipa::openapi::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::AppState;

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "PgKronika Web API",
        version = "1.0.0",
        description = "Bounded local access to retained PostgreSQL telemetry and derived analysis."
    ),
    tags(
        (name = "core", description = "Process and wire-format metadata."),
        (name = "sections", description = "Registry catalogs, retained rows, and counter diffs."),
        (name = "analytics", description = "Bounded anomaly and incident analysis."),
        (name = "timeline", description = "Typed event, health, and overview projections."),
        (name = "ui", description = "Stable indexed projections consumed by the embedded UI.")
    )
)]
struct ApiDoc;

fn configured() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(<ApiDoc as utoipa::OpenApi>::openapi())
        .routes(routes!(crate::handlers::v1::version))
        .routes(routes!(crate::handlers::v1::sections))
        .routes(routes!(crate::handlers::v1::segments))
        .routes(routes!(crate::handlers::v1::section_data))
        .routes(routes!(crate::handlers::v1::sections_batch))
        .routes(routes!(crate::handlers::v1::section_diff))
        .routes(routes!(crate::handlers::v1::sections_batch_diff))
        .routes(routes!(crate::overview::handlers::overview))
        .routes(routes!(crate::overview::handlers::events))
        .routes(routes!(crate::overview::handlers::health))
        .routes(routes!(crate::ui::handlers::heatmap))
        .routes(routes!(crate::ui::handlers::catalog))
        .routes(routes!(crate::ui::handlers::summary))
        .routes(routes!(crate::handlers::anomalies::anomalies))
        .routes(routes!(crate::handlers::incidents::incidents))
}

pub(crate) fn router_and_document() -> (Router<AppState>, OpenApi) {
    configured().split_for_parts()
}

pub(crate) fn document() -> OpenApi {
    let (_, document) = router_and_document();
    document
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::document;

    const OPERATIONS: &[(&str, &str, &str)] = &[
        ("GET", "/v1/version", "version"),
        ("GET", "/v1/sections", "sections"),
        ("GET", "/v1/segments", "segments"),
        ("GET", "/v1/section/{name}", "section_data"),
        ("GET", "/v1/sections/batch", "sections_batch"),
        ("GET", "/v1/section/{name}/diff", "section_diff"),
        ("GET", "/v1/sections/batch/diff", "sections_batch_diff"),
        ("GET", "/v1/timeline/overview", "overview"),
        ("GET", "/v1/timeline/events", "events"),
        ("GET", "/v1/timeline/health", "health"),
        ("GET", "/v1/timeline/heatmap", "heatmap"),
        ("GET", "/v1/ui/catalog", "catalog"),
        ("GET", "/v1/views/summary", "summary"),
        ("GET", "/v1/anomalies", "anomalies"),
        ("GET", "/v1/incidents", "incidents"),
    ];

    #[test]
    fn every_operation_has_a_json_success_schema() {
        let document = serde_json::to_value(document()).expect("serialize OpenAPI");
        let paths = document["paths"].as_object().expect("paths object");
        let observed = paths
            .values()
            .flat_map(|item| {
                item.as_object()
                    .expect("path item")
                    .values()
                    .filter_map(|operation| operation.get("operationId"))
            })
            .count();
        assert_eq!(observed, OPERATIONS.len(), "documented operation count");

        for (method, path, operation_id) in OPERATIONS {
            let operation = &document["paths"][path][method.to_ascii_lowercase()];
            assert_eq!(
                operation["operationId"],
                Value::String((*operation_id).to_owned()),
                "{method} {path} operationId"
            );
            let schema =
                &operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"];
            assert!(
                schema
                    .as_str()
                    .is_some_and(|reference| reference.starts_with("#/components/schemas/")),
                "{method} {path} must reference a named JSON 200 schema"
            );
        }
    }

    #[test]
    fn operations_have_one_domain_tag_and_only_explicit_responses() {
        let document = serde_json::to_value(document()).expect("serialize OpenAPI");
        let expected_tags = [
            ("version", "core"),
            ("sections", "sections"),
            ("segments", "sections"),
            ("section_data", "sections"),
            ("sections_batch", "sections"),
            ("section_diff", "sections"),
            ("sections_batch_diff", "sections"),
            ("overview", "timeline"),
            ("events", "timeline"),
            ("health", "timeline"),
            ("heatmap", "ui"),
            ("catalog", "ui"),
            ("summary", "ui"),
            ("anomalies", "analytics"),
            ("incidents", "analytics"),
        ];

        for (_, path, operation_id) in OPERATIONS {
            let operation = &document["paths"][path]["get"];
            let expected_tag = expected_tags
                .iter()
                .find_map(|(candidate, tag)| (*candidate == *operation_id).then_some(*tag))
                .expect("operation tag fixture");
            assert_eq!(
                operation["tags"],
                serde_json::json!([expected_tag]),
                "{operation_id} domain tag"
            );
            assert!(
                operation["responses"].get("default").is_none(),
                "{operation_id} must enumerate real response statuses"
            );
            for (status, response) in operation["responses"]
                .as_object()
                .expect("responses object")
            {
                if status == "304" {
                    assert!(
                        response.get("content").is_none(),
                        "304 must not advertise a response body"
                    );
                } else if status != "200" {
                    assert_eq!(
                        response["content"]["application/json"]["schema"]["$ref"],
                        "#/components/schemas/ApiError",
                        "{operation_id} {status} error schema"
                    );
                }
            }
        }
    }

    #[test]
    fn overlapping_wire_variants_use_non_exclusive_schema_composition() {
        let document = serde_json::to_value(document()).expect("serialize OpenAPI");
        let schemas = document["components"]["schemas"]
            .as_object()
            .expect("component schemas");

        for name in ["ApiValue", "DiffScalar", "EventPayload"] {
            assert!(
                schemas[name].get("anyOf").is_some(),
                "{name} wire variants overlap and must use anyOf"
            );
            assert!(
                schemas[name].get("oneOf").is_none(),
                "{name} must not reject values matching multiple numeric or object variants"
            );
        }
        assert_eq!(
            schemas["OverviewResponseDto"]["properties"]["coverage"]["type"], "array",
            "overview coverage is an array of factor records"
        );
        assert_eq!(
            schemas["OverviewResponseDto"]["properties"]["health_summary"]["$ref"],
            "#/components/schemas/HealthSummaryResponse"
        );
        assert_eq!(
            schemas["EventsResponseDto"]["properties"]["coverage"]["items"]["$ref"],
            "#/components/schemas/HealthFactorCoverageResponse"
        );
        let incident_evidence = schemas["IncidentEvidenceResponse"]["anyOf"]
            .as_array()
            .expect("incident evidence variants");
        for expected_type in ["string", "object"] {
            assert!(
                incident_evidence
                    .iter()
                    .any(|variant| variant["type"] == expected_type),
                "incident evidence must accept {expected_type}"
            );
        }
    }
}
