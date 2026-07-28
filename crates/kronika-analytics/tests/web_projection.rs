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
                1,
                1,
                "sum",
                "sum(positive_delta(total_exec_time))",
                true,
            ),
            (2, "calls", 1, 2, "sum", "sum(positive_delta(calls))", false,),
            (
                3,
                "io",
                1,
                3,
                "sum",
                "sum(positive_delta(shared_blks_read + local_blks_read))",
                false,
            ),
            (
                4,
                "temp",
                1,
                3,
                "sum",
                "sum(positive_delta(temp_blks_written))",
                false,
            ),
        ]
    );
}
