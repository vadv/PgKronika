//! Cross-crate contract tests for the dependency-free web projection registry.

use std::collections::BTreeSet;

use kronika_analytics::web_projection::{web_view_by_name, web_views};
use proptest as _;
use sha2 as _;

#[test]
fn registry_has_nine_unique_ordered_views_with_one_canonical_metric_each() {
    let views = web_views();
    assert_eq!(
        views
            .iter()
            .map(|view| (view.code, view.name))
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

    let numeric_codes = views.iter().map(|view| view.code).collect::<BTreeSet<_>>();
    let public_codes = views.iter().map(|view| view.name).collect::<BTreeSet<_>>();
    assert_eq!(numeric_codes.len(), views.len());
    assert_eq!(public_codes.len(), views.len());
    assert!(views.iter().all(|view| {
        view.metrics
            .iter()
            .filter(|metric| metric.canonical)
            .count()
            == 1
    }));
}

#[test]
fn statements_metrics_keep_executable_formulas_and_wire_metadata_together() {
    let statements = web_view_by_name("statements").expect("statements view");
    assert_eq!(statements.revision, 3);
    assert_eq!(
        statements
            .inputs
            .iter()
            .map(|input| input.code)
            .collect::<Vec<_>>(),
        vec!["statements", "reset_metadata", "settings"]
    );
    assert_eq!(
        statements
            .metrics
            .iter()
            .map(|metric| {
                (
                    metric.code,
                    metric.name,
                    metric.revision,
                    metric.unit.code(),
                    metric.aggregation.as_str(),
                    metric.formula.as_str(),
                    metric.canonical,
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                1,
                "time",
                3,
                6,
                "sum",
                "sum(positive_delta(total_exec_time))",
                true,
            ),
            (2, "calls", 2, 2, "sum", "sum(positive_delta(calls))", false,),
            (
                3,
                "io",
                2,
                3,
                "sum",
                "sum(positive_delta(shared_blks_read + local_blks_read))",
                false,
            ),
            (
                4,
                "temp",
                2,
                3,
                "sum",
                "sum(positive_delta(temp_blks_written))",
                false,
            ),
        ]
    );
}

#[test]
fn events_input_names_match_the_registry_contract_names() {
    let events = web_view_by_name("events").expect("events view");
    assert_eq!(
        events.inputs[0].sections,
        [
            "pg_log_errors",
            "pg_log_checkpoints",
            "pg_log_autovacuum",
            "pg_log_slow_queries",
            "pg_log_lock_waits",
            "pg_log_lifecycle",
            "pg_log_gap",
            "pg_log_temp_files",
            "pg_log_source_status",
        ]
    );
}

#[test]
fn only_delta_views_publish_a_bounded_predecessor_gap() {
    let gaps = web_views()
        .iter()
        .map(|view| (view.name, view.max_rate_gap_us))
        .collect::<Vec<_>>();

    assert_eq!(
        gaps,
        vec![
            ("activity", Some(900_000_000)),
            ("statements", Some(900_000_000)),
            ("plans", Some(900_000_000)),
            ("tables", Some(900_000_000)),
            ("indexes", Some(900_000_000)),
            ("vacuum", None),
            ("processes", Some(900_000_000)),
            ("locks", None),
            ("events", None),
        ]
    );
}
