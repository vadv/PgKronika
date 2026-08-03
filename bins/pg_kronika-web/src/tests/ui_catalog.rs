use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use kronika_analytics::MetricId;
use kronika_analytics::web_projection::web_view_by_name;
use kronika_registry::registry;
use serde_json::json;
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
fn metric_semantics_publish_revision_four_with_stable_numeric_ids() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    assert_eq!(catalog.revision, 4);

    let actual = catalog
        .views()
        .iter()
        .map(|view| {
            let projection = web_view_by_name(view.code).expect("catalog projection");
            (
                projection.code,
                projection.revision,
                projection
                    .metrics
                    .iter()
                    .map(|metric| (metric.code, metric.revision))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            (1, 2, vec![(1, 1), (2, 2), (3, 1), (4, 1)]),
            (2, 3, vec![(1, 3), (2, 2), (3, 2), (4, 2)]),
            (3, 2, vec![(1, 2), (2, 2)]),
            (4, 2, vec![(1, 1), (2, 1), (3, 1)]),
            (5, 2, vec![(1, 1), (2, 1)]),
            (6, 2, vec![(1, 2)]),
            (7, 2, vec![(1, 2), (2, 1)]),
            (8, 2, vec![(1, 2)]),
            (9, 2, vec![(1, 1)]),
        ]
    );
}

#[test]
fn catalog_serializes_relation_quality_without_promoting_pid_only_evidence() {
    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");
    let activity_joins = serialized_view(&catalog, "activity")["joins"]
        .as_array()
        .expect("activity joins");
    let activity_join = activity_joins
        .iter()
        .find(|join| join["right"] == "process")
        .expect("activity to process join");
    let replication_join = activity_joins
        .iter()
        .find(|join| join["right"] == "replication_replicas")
        .expect("activity to replication join");
    let vacuum_join = serialized_view(&catalog, "vacuum")["joins"]
        .as_array()
        .expect("vacuum joins")
        .first()
        .expect("vacuum to tables join");
    let process_cgroup_join = serialized_view(&catalog, "processes")["joins"]
        .as_array()
        .expect("process joins")
        .first()
        .expect("process to cgroup join");

    assert_eq!(activity_join["kind"], "best_effort");
    assert_eq!(activity_join["fields"], json!(["pid", "ts"]));
    assert_eq!(activity_join["provenance"], "same_snapshot_pid_only");
    assert_eq!(replication_join["kind"], "temporal");
    assert_eq!(vacuum_join["kind"], "temporal");
    assert_eq!(process_cgroup_join["kind"], "exact");
}

#[test]
fn workload_views_publish_distinct_prepared_lenses_and_fork_provenance() {
    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");
    let activity = serialized_view(&catalog, "activity");
    let plans = serialized_view(&catalog, "plans");

    assert_eq!(activity["capabilities"]["related"], true);
    for preset in [
        "overview",
        "waits_locks",
        "duration",
        "cpu",
        "disk_io",
        "replication",
        "sampling",
    ] {
        assert_serialized_preset(&catalog, "activity", preset);
    }
    assert_serialized_preset(&catalog, "plans", "change_timeline");

    let plan_joins = plans["joins"].as_array().expect("plan joins");
    assert!(plan_joins.iter().any(|join| {
        join["kind"] == "best_effort"
            && join["provenance"] == "ossc_queryid_dbid_userid_attribution"
            && join["fields"] == json!(["queryid", "dbid", "userid"])
    }));
    assert!(plan_joins.iter().any(|join| {
        join["kind"] == "best_effort"
            && join["provenance"] == "vadv_queryid_stat_statements_dbid_userid_attribution"
            && join["fields"] == json!(["queryid_stat_statements", "dbid", "userid"])
    }));
}

#[test]
fn host_and_object_views_publish_prepared_lenses_and_temporal_relations() {
    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");

    for (view, presets) in [
        (
            "processes",
            &[
                "pressure",
                "cpu",
                "memory",
                "disk_io",
                "cgroup",
                "processes",
                "data_quality",
            ][..],
        ),
        (
            "tables",
            &[
                "health",
                "vacuum_risk",
                "io",
                "scan_pattern",
                "size",
                "xid_mxid",
            ][..],
        ),
        (
            "indexes",
            &["usage", "io", "size", "unused", "table_context"][..],
        ),
        ("vacuum", &["progress", "phase", "dead_items"][..]),
    ] {
        for preset in presets {
            assert_serialized_preset(&catalog, view, preset);
        }
    }

    for view in ["tables", "indexes", "vacuum"] {
        assert_eq!(
            serialized_view(&catalog, view)["capabilities"]["related"],
            true,
            "{view} exposes bounded same-snapshot relations"
        );
    }

    for (view, left, right, provenance) in [
        (
            "tables",
            "tables",
            "vacuum",
            "same_snapshot_database_relation_oid",
        ),
        (
            "indexes",
            "indexes",
            "tables",
            "same_snapshot_database_relation_oid",
        ),
        (
            "vacuum",
            "vacuum",
            "tables",
            "same_snapshot_database_relation_oid",
        ),
    ] {
        let join = serialized_view(&catalog, view)["joins"]
            .as_array()
            .expect("joins")
            .iter()
            .find(|join| join["left"] == left && join["right"] == right)
            .unwrap_or_else(|| panic!("missing {view}: {left} -> {right}"));
        assert_eq!(join["kind"], "temporal");
        assert_eq!(join["fields"], json!(["datid", "relid", "ts"]));
        assert_eq!(join["provenance"], provenance);
    }
}

#[test]
fn events_publish_bounded_signal_lenses_without_claiming_config_evidence() {
    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");
    let events = serialized_view(&catalog, "events");

    assert_eq!(events["view_revision"], 2);
    assert_eq!(events["capabilities"]["history"], false);
    assert_eq!(events["capabilities"]["related"], false);
    assert_eq!(
        events["presets"]
            .as_array()
            .expect("event presets")
            .iter()
            .map(|preset| preset["code"].as_str().expect("preset code"))
            .collect::<Vec<_>>(),
        vec![
            "timeline",
            "errors",
            "checkpoints",
            "vacuum",
            "slow",
            "collector_health",
        ]
    );
    assert!(
        events["presets"]
            .as_array()
            .expect("event presets")
            .iter()
            .all(|preset| preset["code"] != "config_changes")
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
            &["shared_hit", "shared_read", "first_call", "last_call"],
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
                "wait_age_us",
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
fn plan_call_timestamps_keep_their_source_names() {
    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");
    let plans = serialized_view(&catalog, "plans");
    let columns = plans["columns"].as_array().expect("plan columns");

    for (code, source) in [
        (
            "first_call",
            serde_json::Value::String("plans.first_call".to_owned()),
        ),
        (
            "last_call",
            serde_json::Value::String("plans.last_call".to_owned()),
        ),
    ] {
        let column = columns
            .iter()
            .find(|column| column["code"] == code)
            .unwrap_or_else(|| panic!("plans.{code}"));
        assert_eq!(column["source"], source);
    }
    assert!(
        columns
            .iter()
            .all(|column| !matches!(column["code"].as_str(), Some("first_seen" | "last_seen"))),
        "the catalog must not rename the collected call timestamps as observations"
    );
    assert_eq!(plans["view_revision"], json!(2));

    let locks = serialized_view(&catalog, "locks");
    assert_eq!(locks["view_revision"], json!(2));
    let wait = locks["metrics"]
        .as_array()
        .expect("lock metrics")
        .iter()
        .find(|metric| metric["code"] == "wait")
        .expect("locks.wait");
    assert_eq!(wait["revision"], json!(2));
    assert_eq!(wait["formula"], "max(wait_age_us from waitstart)");
}

#[test]
fn vacuum_catalog_keeps_generation_specific_dead_work_units() {
    let projection = web_view_by_name("vacuum").expect("vacuum projection");
    assert_eq!(projection.code, 6);
    assert_eq!(projection.revision, 2);
    assert_eq!(projection.metrics[0].code, 1);
    assert_eq!(projection.metrics[0].revision, 2);

    let catalog = serde_json::to_value(ProjectionCatalog::for_type_ids(&all_type_ids()))
        .expect("serialize catalog");
    let vacuum = serialized_view(&catalog, "vacuum");
    assert_eq!(vacuum["view_code"], json!(6));
    assert_eq!(vacuum["view_revision"], json!(2));

    let metric = vacuum["metrics"]
        .as_array()
        .expect("vacuum metrics")
        .iter()
        .find(|metric| metric["code"] == "progress")
        .expect("vacuum progress metric");
    assert_eq!(metric["revision"], json!(2));
    assert_eq!(
        metric["formula"],
        "max(heap_blks_scanned / heap_blks_total)"
    );

    let columns = vacuum["columns"].as_array().expect("vacuum columns");
    for (code, source, unit) in [
        ("dead_tuples", "vacuum.num_dead_tuples", "count"),
        ("dead_item_ids", "vacuum.num_dead_item_ids", "count"),
        ("dead_tuple_bytes", "vacuum.dead_tuple_bytes", "bytes"),
    ] {
        let column = columns
            .iter()
            .find(|column| column["code"] == code)
            .unwrap_or_else(|| panic!("vacuum.{code}"));
        assert_eq!(column["type"], "i64", "vacuum.{code}");
        assert_eq!(column["source"], source, "vacuum.{code}");
        assert!(column["formula"].is_null(), "vacuum.{code}");
        assert_eq!(column["unit"], unit, "vacuum.{code}");
    }

    let progress = columns
        .iter()
        .find(|column| column["code"] == "progress")
        .expect("vacuum.progress");
    assert_eq!(progress["formula"], "heap_blks_scanned / heap_blks_total");
    assert_eq!(progress["unit"], "ratio");

    for preset in ["progress", "dead_tuples"] {
        let columns = vacuum["presets"]
            .as_array()
            .expect("vacuum presets")
            .iter()
            .find(|candidate| candidate["code"] == preset)
            .unwrap_or_else(|| panic!("vacuum preset {preset}"))["columns"]
            .as_array()
            .expect("preset columns");
        for code in ["dead_tuples", "dead_item_ids", "dead_tuple_bytes"] {
            assert!(
                columns.iter().any(|column| column == code),
                "{preset}.{code}"
            );
        }
    }
}

#[test]
fn gated_reason_distinguishes_extension_backed_from_built_in_inputs() {
    let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::new());

    let plans = catalog
        .views()
        .iter()
        .find(|view| view.code == "plans")
        .expect("plans view");
    assert_eq!(
        plans.inputs[0].unavailable_reason,
        Some("missing_extension")
    );

    let statements = catalog
        .views()
        .iter()
        .find(|view| view.code == "statements")
        .expect("statements view");
    assert_eq!(
        statements.inputs[0].unavailable_reason,
        Some("missing_extension")
    );

    let vacuum = catalog
        .views()
        .iter()
        .find(|view| view.code == "vacuum")
        .expect("vacuum view");
    assert_eq!(vacuum.inputs[0].unavailable_reason, Some("not_collected"));
    assert!(
        vacuum
            .metrics
            .iter()
            .all(|metric| metric.unavailable_reason != Some("missing_extension"))
    );
}

#[test]
fn reset_metadata_is_auxiliary_to_extension_metric_availability() {
    for (view_code, primary_section) in [
        ("statements", "pg_stat_statements"),
        ("plans", "pg_store_plans_vadv"),
    ] {
        let primary_type = first_type_id(primary_section);
        let catalog = ProjectionCatalog::for_type_ids(&BTreeSet::from([primary_type]));
        let view = catalog
            .views()
            .iter()
            .find(|view| view.code == view_code)
            .unwrap_or_else(|| panic!("{view_code} view"));
        let reset_metadata = view
            .inputs
            .iter()
            .find(|input| input.code == "reset_metadata")
            .expect("reset metadata input");

        assert_eq!(view.availability, Availability::Available, "{view_code}");
        assert_eq!(reset_metadata.availability, Availability::Gated);
        assert_eq!(reset_metadata.unavailable_reason, Some("not_collected"));
    }
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
fn statements_query_text_is_available_but_detail_only() {
    // The collector server-truncates and stores query text. The frame keeps it
    // lazy for response bounds, while entity detail can project it.
    for catalog in [
        ProjectionCatalog::for_type_ids(&all_type_ids()),
        ProjectionCatalog::for_materialization(),
    ] {
        let query = catalog
            .views()
            .iter()
            .find(|view| view.code == "statements")
            .and_then(|view| view.columns.iter().find(|column| column.code == "query"))
            .expect("statements.query");
        assert_eq!(query.availability, Availability::Available);
        assert_eq!(query.unavailable_reason, None);
        assert!(query.lazy, "query text stays a detail-only column");
    }

    let missing = ProjectionCatalog::for_type_ids(&BTreeSet::new());
    let query = missing
        .views()
        .iter()
        .find(|view| view.code == "statements")
        .and_then(|view| view.columns.iter().find(|column| column.code == "query"))
        .expect("statements.query");
    assert_eq!(query.availability, Availability::Gated);
    assert_eq!(query.unavailable_reason, Some("missing_extension"));
}

#[test]
fn statements_presets_identify_rows_by_database_and_user() {
    let catalog = ProjectionCatalog::for_type_ids(&all_type_ids());
    let statements = catalog
        .views()
        .iter()
        .find(|view| view.code == "statements")
        .expect("statements view");
    for column in ["database", "user"] {
        let spec = statements
            .columns
            .iter()
            .find(|candidate| candidate.code == column)
            .unwrap_or_else(|| panic!("statements.{column}"));
        assert!(!spec.lazy, "{column} is a frame column");
    }
    for preset in &statements.presets {
        assert!(
            !preset.columns.contains(&"query"),
            "preset {} must not promise the lazy query column",
            preset.code
        );
        for column in ["database", "user"] {
            assert!(
                preset.columns.contains(&column),
                "preset {} identifies rows by {column}",
                preset.code
            );
        }
    }
}

#[test]
fn statements_publish_only_executable_forensic_lenses() {
    let catalog = ProjectionCatalog::for_type_ids(&all_type_ids());
    let statements = catalog
        .views()
        .iter()
        .find(|view| view.code == "statements")
        .expect("statements view");

    let actual = statements
        .presets
        .iter()
        .map(|preset| (preset.code, preset.sort.column, preset.sort.order))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            ("time", "total", "desc"),
            ("latency", "mean", "desc"),
            ("io", "blks_read", "desc"),
            ("wal", "wal_bytes", "desc"),
            ("temp", "temp_written", "desc"),
            ("planning", "plan_time_pct", "desc"),
        ]
    );
    assert!(
        statements
            .presets
            .iter()
            .all(|preset| !matches!(preset.code, "regression" | "observed_samples")),
        "unproven baseline and sample relations stay out of executable presets"
    );
}

#[test]
fn activity_cpu_requires_both_activity_and_process_inputs() {
    let activity_type = first_type_id("pg_stat_activity");
    let process_type = first_type_id("os_process");
    let instance_type = first_type_id("instance_metadata");

    let activity_only = ProjectionCatalog::for_type_ids(&BTreeSet::from([activity_type]));
    let cpu = activity_only
        .metric("activity", "cpu")
        .expect("activity cpu metric");
    assert_eq!(cpu.availability, Availability::Gated);

    let joined = ProjectionCatalog::for_type_ids(&BTreeSet::from([activity_type, process_type]));
    let cpu = joined
        .metric("activity", "cpu")
        .expect("activity cpu metric");
    assert_eq!(cpu.availability, Availability::Gated);

    let joined = ProjectionCatalog::for_type_ids(&BTreeSet::from([
        activity_type,
        process_type,
        instance_type,
    ]));
    let cpu = joined
        .metric("activity", "cpu")
        .expect("activity cpu metric");
    assert_eq!(cpu.availability, Availability::Available);
}

#[test]
fn process_cpu_requires_instance_metadata_but_activity_stays_available() {
    let activity_type = first_type_id("pg_stat_activity");
    let process_type = first_type_id("os_process");
    let instance_type = first_type_id("instance_metadata");

    let without_instance =
        ProjectionCatalog::for_type_ids(&BTreeSet::from([activity_type, process_type]));
    let processes = without_instance
        .views()
        .iter()
        .find(|view| view.code == "processes")
        .expect("processes view");
    assert_eq!(processes.availability, Availability::Gated);
    assert_eq!(
        without_instance
            .metric("processes", "cpu")
            .expect("process cpu metric")
            .availability,
        Availability::Gated
    );
    assert_eq!(
        without_instance
            .views()
            .iter()
            .find(|view| view.code == "activity")
            .expect("activity view")
            .availability,
        Availability::Available
    );

    let with_instance =
        ProjectionCatalog::for_type_ids(&BTreeSet::from([process_type, instance_type]));
    assert_eq!(
        with_instance
            .metric("processes", "cpu")
            .expect("process cpu metric")
            .availability,
        Availability::Available
    );
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
    assert_eq!(response.body["revision"], 4);
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
