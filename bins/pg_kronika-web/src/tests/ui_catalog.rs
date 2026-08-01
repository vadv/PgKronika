use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use kronika_analytics::MetricId;
use kronika_registry::registry;
use tower::ServiceExt;

use crate::ui::catalog::{Availability, ProjectionCatalog};
use crate::ui::thresholds::{BindingDisposition, threshold_projections};
use crate::{app, tests::test_metrics_handle};

use super::{assert_api_error, serve, serve_captured, state_for_dir, write_bgwriter_segment};

fn first_type_id(section: &str) -> u32 {
    registry()
        .iter()
        .find(|contract| contract.name == section)
        .map_or_else(
            || panic!("fixture section {section} is registered"),
            |contract| contract.type_id.get(),
        )
}

fn all_type_ids() -> BTreeSet<u32> {
    registry()
        .iter()
        .map(|contract| contract.type_id.get())
        .collect()
}

fn serialized_view<'a>(catalog: &'a serde_json::Value, code: &str) -> &'a serde_json::Value {
    catalog["views"]
        .as_array()
        .expect("catalog views")
        .iter()
        .find(|view| view["code"] == code)
        .unwrap_or_else(|| panic!("catalog view {code}"))
}

fn assert_serialized_column(catalog: &serde_json::Value, view: &str, column: &str) {
    assert!(
        serialized_view(catalog, view)["columns"]
            .as_array()
            .expect("catalog columns")
            .iter()
            .any(|candidate| candidate["code"] == column),
        "missing catalog column {view}.{column}"
    );
}

fn assert_serialized_preset(catalog: &serde_json::Value, view: &str, preset: &str) {
    assert!(
        serialized_view(catalog, view)["presets"]
            .as_array()
            .expect("catalog presets")
            .iter()
            .any(|candidate| candidate["code"] == preset),
        "missing catalog preset {view}.{preset}"
    );
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
fn catalog_contains_every_v5_column_preset_capability_and_reason() {
    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");

    for (view, columns) in [
        ("activity", &["backend_type"][..]),
        (
            "plans",
            &["shared_hit", "shared_read", "first_seen", "last_seen"],
        ),
        ("tables", &["size", "io_hit_pct", "xid_age", "mxid_age"]),
        ("indexes", &["size", "io_hit_pct", "last_idx_scan"]),
        ("vacuum", &["relation", "elapsed"]),
        ("processes", &["threads"]),
        (
            "locks",
            &[
                "depth",
                "root_pid",
                "blocked_by",
                "granted",
                "lock_mode",
                "lock_type",
                "wait_or_hold_us",
            ],
        ),
        ("events", &["severity_code", "category_code", "detail"]),
    ] {
        for column in columns {
            assert_serialized_column(&catalog, view, column);
        }
    }
    for (view, preset) in [
        ("activity", "replication"),
        ("tables", "size"),
        ("indexes", "size"),
    ] {
        assert_serialized_preset(&catalog, view, preset);
    }

    for view in catalog["views"].as_array().expect("catalog views") {
        assert_eq!(view["capabilities"]["detail"], true, "{view}");
        assert!(view["capabilities"]["history"].is_boolean(), "{view}");
        assert!(view["capabilities"]["related"].is_boolean(), "{view}");
    }

    assert!(
        serialized_view(&catalog, "processes")["columns"]
            .as_array()
            .expect("process columns")
            .iter()
            .all(|column| column["code"] != "pss"),
        "pss stays out of the catalog until it is actually collected"
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
fn threshold_manifest_is_exhaustive_unique_and_bound_to_catalog_columns() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    let manifest = threshold_projections();

    assert_eq!(manifest.len(), MetricId::ALL.len());
    assert_eq!(
        manifest
            .iter()
            .map(|entry| entry.metric_id)
            .collect::<Vec<_>>(),
        MetricId::ALL
    );
    assert_eq!(
        manifest
            .iter()
            .filter(|entry| matches!(entry.disposition, BindingDisposition::Bound { .. }))
            .count(),
        14
    );
    assert_eq!(
        manifest
            .iter()
            .filter(|entry| matches!(entry.disposition, BindingDisposition::Deferred(_)))
            .count(),
        55
    );

    let mut bound_columns = BTreeSet::new();
    for entry in manifest {
        let BindingDisposition::Bound { view, column, .. } = entry.disposition else {
            continue;
        };
        assert!(
            bound_columns.insert((view, column)),
            "duplicate threshold binding for {view}.{column}"
        );
        let view_spec = catalog
            .views()
            .iter()
            .find(|candidate| candidate.code == view)
            .unwrap_or_else(|| panic!("{} binds unknown view {view}", entry.metric_id.as_str()));
        assert!(
            view_spec
                .columns
                .iter()
                .any(|candidate| candidate.code == column),
            "{} binds unknown column {view}.{column}",
            entry.metric_id.as_str()
        );
    }
}

#[test]
fn threshold_catalog_metadata_exposes_only_the_fourteen_bound_columns() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    let actual = catalog
        .views()
        .iter()
        .flat_map(|view| {
            view.columns.iter().filter_map(move |column| {
                column
                    .threshold_metric
                    .map(|metric| (view.code, column.code, metric, column.unit))
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            (
                "activity",
                "query_duration_us",
                "pg.activity.query_duration_seconds",
                Some("us"),
            ),
            (
                "activity",
                "transaction_duration_us",
                "pg.activity.transaction_duration_seconds",
                Some("us"),
            ),
            (
                "statements",
                "ms_per_row",
                "pg.statements.milliseconds_per_row",
                Some("ms"),
            ),
            (
                "statements",
                "mean",
                "pg.statements.mean_time_ms",
                Some("ms"),
            ),
            (
                "statements",
                "time_pct",
                "pg.statements.time_pct",
                Some("percent"),
            ),
            (
                "statements",
                "plan_time_pct",
                "pg.statements.plan_time_pct",
                Some("percent"),
            ),
            (
                "tables",
                "dead_pct",
                "pg.tables.dead_tuple_pct",
                Some("percent"),
            ),
            (
                "tables",
                "dead_tuples",
                "pg.tables.dead_tuples",
                Some("count"),
            ),
            (
                "tables",
                "seq_scan_pct",
                "pg.tables.sequential_scan_pct",
                Some("percent"),
            ),
            (
                "tables",
                "modified_since_analyze",
                "pg.tables.modified_since_analyze",
                Some("count"),
            ),
            (
                "tables",
                "inserted_since_vacuum",
                "pg.tables.inserted_since_vacuum",
                Some("count"),
            ),
            (
                "tables",
                "autovacuum_age_seconds",
                "pg.tables.autovacuum_age_seconds",
                Some("seconds"),
            ),
            (
                "tables",
                "autoanalyze_age_seconds",
                "pg.tables.autoanalyze_age_seconds",
                Some("seconds"),
            ),
            ("processes", "rss", "os.process.rss_kib", Some("kib"),),
        ]
    );
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
                "ms",
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
fn process_pss_is_absent_even_when_process_input_exists() {
    let observed = BTreeSet::from([first_type_id("os_process")]);
    let catalog = ProjectionCatalog::for_type_ids(&observed);
    let processes = catalog
        .views()
        .iter()
        .find(|view| view.code == "processes")
        .expect("processes view");
    assert!(processes.columns.iter().all(|column| column.code != "pss"));
}

#[test]
fn materialization_catalog_enables_known_inputs_but_not_intrinsic_gaps() {
    let catalog = ProjectionCatalog::for_materialization();
    let processes = catalog
        .views()
        .iter()
        .find(|view| view.code == "processes")
        .expect("processes view");

    assert!(
        processes
            .inputs
            .iter()
            .all(|input| input.availability == Availability::Available)
    );
    assert!(processes.columns.iter().all(|column| column.code != "pss"));
}

#[tokio::test]
async fn ui_catalog_returns_nine_views_for_the_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);

    let response = serve_captured(dir.path(), "/v1/ui/catalog", &[]).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["revision"], 2);
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
async fn ui_catalog_omits_pss_until_collected() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);

    let (status, body) = serve(dir.path(), "/v1/ui/catalog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["views"]
            .as_array()
            .expect("views")
            .iter()
            .find(|view| view["code"] == "processes")
            .and_then(|view| view["columns"].as_array())
            .expect("columns")
            .iter()
            .all(|column| column["code"] != "pss")
    );
}

#[tokio::test]
async fn ui_catalog_rejects_unknown_parameters() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "known.pgm", 1_000, 2_000);

    let (status, body) = serve(dir.path(), "/v1/ui/catalog?extra=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_api_error(
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
