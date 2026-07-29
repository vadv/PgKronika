use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use kronika_registry::registry;
use tower::ServiceExt;

use crate::ui::catalog::{Availability, ProjectionCatalog};
use crate::{app, tests::test_metrics_handle};

use super::{assert_problem, serve, serve_captured, state_for_dir, write_bgwriter_segment};

fn first_type_id(section: &str) -> u32 {
    registry()
        .iter()
        .find(|contract| contract.name == section)
        .map_or_else(
            || panic!("fixture section {section} is registered"),
            |contract| contract.type_id.get(),
        )
}

#[test]
fn catalog_exposes_all_nine_views_in_stable_code_order() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    assert_eq!(
        catalog
            .views()
            .iter()
            .map(|view| (view.view_code, view.code))
            .collect::<Vec<_>>(),
        vec![
            (1, "activity"),
            (2, "statements"),
            (3, "plans"),
            (4, "tables"),
            (5, "indexes"),
            (6, "vacuum"),
            (7, "processes"),
            (8, "locks"),
            (9, "events"),
        ]
    );
}

#[test]
fn every_preset_returns_its_sort_column() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    for view in catalog.views() {
        for preset in &view.presets {
            assert!(
                preset.columns.contains(&preset.sort.column),
                "{}.{} sorts by omitted column {}",
                view.code,
                preset.code,
                preset.sort.column
            );
        }
    }
}

#[test]
fn statements_metrics_publish_explicit_formulas_and_units() {
    let observed = BTreeSet::from([first_type_id("pg_stat_statements")]);
    let catalog = ProjectionCatalog::for_type_ids(&observed);
    let statements = catalog
        .views()
        .iter()
        .find(|view| view.code == "statements")
        .expect("statements view");
    assert_eq!(
        statements
            .metrics
            .iter()
            .map(|metric| {
                (
                    metric.code,
                    metric.formula,
                    metric.unit,
                    metric.availability,
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "time",
                "sum(positive_delta(total_exec_time))",
                "us",
                Availability::Available,
            ),
            (
                "calls",
                "sum(positive_delta(calls))",
                "count",
                Availability::Available,
            ),
            (
                "io",
                "sum(positive_delta(shared_blks_read + local_blks_read))",
                "blocks",
                Availability::Available,
            ),
            (
                "temp",
                "sum(positive_delta(temp_blks_written))",
                "blocks",
                Availability::Available,
            ),
        ]
    );
}

#[test]
fn statements_hit_percentage_uses_window_deltas() {
    let observed = BTreeSet::from([first_type_id("pg_stat_statements")]);
    let catalog = ProjectionCatalog::for_type_ids(&observed);
    let hit_pct = catalog
        .views()
        .iter()
        .find(|view| view.code == "statements")
        .and_then(|view| view.columns.iter().find(|column| column.code == "hit_pct"))
        .expect("statements.hit_pct");

    assert_eq!(
        hit_pct.formula,
        Some(
            "100 * positive_delta(shared_blks_hit) / max(positive_delta(shared_blks_hit + shared_blks_read), 1)"
        )
    );
}

#[test]
fn activity_cpu_requires_both_activity_and_process_inputs() {
    let activity_type = first_type_id("pg_stat_activity");
    let process_type = first_type_id("os_process");

    let activity_only = ProjectionCatalog::for_type_ids(&BTreeSet::from([activity_type]));
    let cpu = activity_only
        .metric("activity", "cpu")
        .expect("activity cpu metric");
    assert_eq!(cpu.availability, Availability::Gated);

    let joined = ProjectionCatalog::for_type_ids(&BTreeSet::from([activity_type, process_type]));
    let cpu = joined
        .metric("activity", "cpu")
        .expect("activity cpu metric");
    assert_eq!(cpu.availability, Availability::Available);
}

#[test]
fn process_pss_is_not_collected_even_when_process_input_exists() {
    let observed = BTreeSet::from([first_type_id("os_process")]);
    let catalog = ProjectionCatalog::for_type_ids(&observed);
    let processes = catalog
        .views()
        .iter()
        .find(|view| view.code == "processes")
        .expect("processes view");
    let pss = processes
        .columns
        .iter()
        .find(|column| column.code == "pss")
        .expect("pss column");
    assert_eq!(pss.availability, Availability::NotCollected);
}

#[tokio::test]
async fn ui_catalog_returns_nine_views_for_the_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);

    let response = serve_captured(dir.path(), "/v1/ui/catalog", &[]).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["revision"], 1);
    assert_eq!(
        response.body["views"].as_array().map(Vec::len),
        Some(9),
        "the endpoint exposes the complete stable view catalog"
    );
    assert!(
        response.headers.contains_key(header::ETAG),
        "catalog responses carry a cache validator"
    );
}

#[tokio::test]
async fn ui_catalog_keeps_pss_not_collected() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);

    let (status, body) = serve(dir.path(), "/v1/ui/catalog").await;
    assert_eq!(status, StatusCode::OK);
    let pss = body["views"]
        .as_array()
        .expect("views")
        .iter()
        .find(|view| view["code"] == "processes")
        .and_then(|view| view["columns"].as_array())
        .and_then(|columns| columns.iter().find(|column| column["code"] == "pss"))
        .expect("processes.pss");
    assert_eq!(pss["availability"], "not_collected");
}

#[tokio::test]
async fn ui_catalog_rejects_unknown_parameters() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);

    let (status, body) = serve(dir.path(), "/v1/ui/catalog?extra=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_problem(
        &body,
        status,
        "unknown_query_parameter",
        serde_json::json!({ "parameter": "extra" }),
    );
}

#[tokio::test]
async fn ui_catalog_honors_if_none_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);
    let first = serve_captured(dir.path(), "/v1/ui/catalog", &[]).await;
    let etag = first
        .headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .expect("catalog ETag")
        .to_owned();

    let state = state_for_dir(dir.path());
    let response = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/ui/catalog")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route request");
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(
        response.headers().contains_key(header::ETAG),
        "304 repeats the validator"
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read 304 body")
        .to_bytes();
    assert!(body.is_empty(), "304 has no representation body");
}
