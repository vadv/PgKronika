//! Source-aware catalog for the stable web UI projections.

use std::collections::BTreeSet;

use kronika_analytics::web_projection::{WebView, web_view_by_name};
use kronika_registry::registry;
use serde::Serialize;
use utoipa::ToSchema;

/// Catalog schema revision.
const CATALOG_REVISION: u16 = 1;

/// Availability of one input, metric, or column for a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Availability {
    /// Every required input has a supported observed type.
    Available,
    /// An optional extension, privilege, join input, or OS facility is absent.
    Gated,
    /// The projection is known but the collector does not write its input.
    NotCollected,
    /// The source carries a type outside the supported projection contract.
    #[allow(
        dead_code,
        reason = "reserved wire state for a source type newer than the local projection"
    )]
    UnsupportedType,
}

/// UI ownership scope of one view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    /// Rows belong to one database.
    Database,
    /// Rows belong to the host.
    Host,
    /// Rows belong to the `PostgreSQL` instance.
    Instance,
}

/// Public scalar shape of one projected column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ValueType {
    /// Signed integer.
    I64,
    /// Unsigned integer.
    U64,
    /// Floating-point value.
    F64,
    /// Boolean.
    Bool,
    /// UTF-8 text.
    Text,
    /// UTC timestamp in unix microseconds.
    Timestamp,
}

/// One alternative group of physical inputs for a view.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct InputSpec {
    /// Stable input code referenced by metrics and columns.
    pub code: &'static str,
    /// Logical registry sections accepted by this input.
    pub logical_sections: Vec<&'static str>,
    /// Supported physical type IDs for the logical sections.
    pub type_ids: Vec<u32>,
    /// Source-specific availability.
    pub availability: Availability,
}

/// A proven or conditional relationship between two inputs.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct JoinSpec {
    /// Left input code.
    pub left: &'static str,
    /// Right input code.
    pub right: &'static str,
    /// Equality fields, in comparison order.
    pub fields: Vec<&'static str>,
    /// Declared join cardinality.
    pub cardinality: &'static str,
    /// Evidence required before the join is accepted.
    pub provenance: &'static str,
}

/// One heatmap or spark metric.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct MetricSpec {
    /// Stable metric code inside its view.
    pub code: &'static str,
    /// Independently changeable projection revision.
    pub revision: u16,
    /// Public unit code.
    pub unit: &'static str,
    /// Bucket aggregation across segments.
    pub aggregation: &'static str,
    /// Normative formula over projected inputs.
    pub formula: &'static str,
    /// Input groups required by the formula.
    pub requires: Vec<&'static str>,
    /// Source-specific availability.
    pub availability: Availability,
}

/// One table or detail projection column.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct ColumnSpec {
    /// Stable column code.
    pub code: &'static str,
    /// Public value shape.
    #[serde(rename = "type")]
    pub value_type: ValueType,
    /// Raw source field when this is a direct projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
    /// Formula when this is a derived projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula: Option<&'static str>,
    /// Whether frame omits the value and detail loads it.
    pub lazy: bool,
    /// Input groups required by this column.
    pub requires: Vec<&'static str>,
    /// Source-specific availability.
    pub availability: Availability,
}

/// Default ordering of one preset.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct SortSpec {
    /// Column code.
    pub column: &'static str,
    /// `asc` or `desc`.
    pub order: &'static str,
}

/// Named ordered subset of view columns.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct PresetSpec {
    /// Stable preset code.
    pub code: &'static str,
    /// Columns returned by the preset, in display order.
    pub columns: Vec<&'static str>,
    /// Default sort.
    pub sort: SortSpec,
}

/// One stable UI projection.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct ViewSpec {
    /// Numeric OVF logical ID.
    pub view_code: u16,
    /// Public URL and JSON code.
    pub code: &'static str,
    /// Independently changeable view revision.
    pub view_revision: u16,
    /// Row ownership scope.
    pub scope: Scope,
    /// Typed identity encoding revision.
    pub identity_revision: u16,
    /// Source-specific view availability.
    pub availability: Availability,
    /// Physical input families.
    pub inputs: Vec<InputSpec>,
    /// Proven joins between inputs.
    pub joins: Vec<JoinSpec>,
    /// Heatmap and spark metrics.
    pub metrics: Vec<MetricSpec>,
    /// Frame and detail columns.
    pub columns: Vec<ColumnSpec>,
    /// Named frame presets.
    pub presets: Vec<PresetSpec>,
    /// Metric used by the canonical row spark.
    pub canonical_metric: &'static str,
}

/// Complete source-aware catalog response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct ProjectionCatalog {
    /// Catalog schema revision.
    pub revision: u16,
    views: Vec<ViewSpec>,
}

impl ProjectionCatalog {
    /// Build the stable catalog with availability derived from observed PGM types.
    #[must_use]
    pub(crate) fn for_type_ids(observed: &BTreeSet<u32>) -> Self {
        let mut views = vec![
            activity_view(),
            statements_view(),
            plans_view(),
            tables_view(),
            indexes_view(),
            vacuum_view(),
            processes_view(),
            locks_view(),
            events_view(),
        ];
        for view in &mut views {
            apply_availability(view, observed);
        }
        Self {
            revision: CATALOG_REVISION,
            views,
        }
    }

    /// Stable view list in ascending `view_code` order.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn views(&self) -> &[ViewSpec] {
        &self.views
    }

    /// Find one metric by public view and metric codes.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn metric(&self, view: &str, metric: &str) -> Option<&MetricSpec> {
        self.views
            .iter()
            .find(|candidate| candidate.code == view)?
            .metrics
            .iter()
            .find(|candidate| candidate.code == metric)
    }
}

fn apply_availability(view: &mut ViewSpec, observed: &BTreeSet<u32>) {
    for input in &mut view.inputs {
        input.availability = if input
            .type_ids
            .iter()
            .any(|type_id| observed.contains(type_id))
        {
            Availability::Available
        } else {
            Availability::Gated
        };
    }
    let input_available = |code: &str| {
        view.inputs
            .iter()
            .any(|input| input.code == code && input.availability == Availability::Available)
    };
    for metric in &mut view.metrics {
        metric.availability = availability_for(&metric.requires, &input_available);
    }
    for column in &mut view.columns {
        if column.availability != Availability::NotCollected {
            column.availability = availability_for(&column.requires, &input_available);
        }
    }
    view.availability = view
        .metrics
        .iter()
        .find(|metric| metric.code == view.canonical_metric)
        .map_or(Availability::Gated, |metric| metric.availability);
}

fn availability_for(required: &[&str], input_available: &impl Fn(&str) -> bool) -> Availability {
    if required.iter().all(|code| input_available(code)) {
        Availability::Available
    } else {
        Availability::Gated
    }
}

fn input(code: &'static str, logical_sections: &[&'static str]) -> InputSpec {
    let logical_sections = logical_sections.to_vec();
    let mut type_ids: Vec<u32> = registry()
        .iter()
        .filter(|contract| logical_sections.contains(&contract.name))
        .map(|contract| contract.type_id.get())
        .collect();
    type_ids.sort_unstable();
    type_ids.dedup();
    InputSpec {
        code,
        logical_sections,
        type_ids,
        availability: Availability::Gated,
    }
}

fn projection(name: &str) -> &'static WebView {
    web_view_by_name(name).expect("catalog view must exist in the shared projection registry")
}

fn projection_inputs(view: &WebView) -> Vec<InputSpec> {
    view.inputs
        .iter()
        .map(|input_spec| input(input_spec.code, input_spec.sections))
        .collect()
}

fn projection_metrics(view: &WebView) -> Vec<MetricSpec> {
    view.metrics
        .iter()
        .map(|metric| MetricSpec {
            code: metric.name,
            revision: metric.revision,
            unit: metric.unit.as_str(),
            aggregation: metric.aggregation.as_str(),
            formula: metric.formula.as_str(),
            requires: metric.requires.to_vec(),
            availability: Availability::Gated,
        })
        .collect()
}

fn raw_column(
    code: &'static str,
    value_type: ValueType,
    source: &'static str,
    lazy: bool,
    requires: &[&'static str],
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: Some(source),
        formula: None,
        lazy,
        requires: requires.to_vec(),
        availability: Availability::Gated,
    }
}

fn derived_column(
    code: &'static str,
    value_type: ValueType,
    formula: &'static str,
    requires: &[&'static str],
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: None,
        formula: Some(formula),
        lazy: false,
        requires: requires.to_vec(),
        availability: Availability::Gated,
    }
}

const fn unavailable_column(
    code: &'static str,
    value_type: ValueType,
    source: &'static str,
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: Some(source),
        formula: None,
        lazy: false,
        requires: Vec::new(),
        availability: Availability::NotCollected,
    }
}

fn preset(
    code: &'static str,
    columns: &[&'static str],
    sort_column: &'static str,
    order: &'static str,
) -> PresetSpec {
    PresetSpec {
        code,
        columns: columns.to_vec(),
        sort: SortSpec {
            column: sort_column,
            order,
        },
    }
}

fn view(
    projection: &'static WebView,
    scope: Scope,
    joins: Vec<JoinSpec>,
    columns: Vec<ColumnSpec>,
    presets: Vec<PresetSpec>,
) -> ViewSpec {
    let canonical_metric = projection
        .metrics
        .iter()
        .find(|metric| metric.canonical)
        .expect("shared projection has one canonical metric")
        .name;
    ViewSpec {
        view_code: projection.code,
        code: projection.name,
        view_revision: projection.revision,
        scope,
        identity_revision: projection.identity_revision,
        availability: Availability::Gated,
        inputs: projection_inputs(projection),
        joins,
        metrics: projection_metrics(projection),
        columns,
        presets,
        canonical_metric,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the view is a declarative projection registry kept together for review"
)]
fn activity_view() -> ViewSpec {
    let projection = projection("activity");
    let joins = vec![JoinSpec {
        left: "activity",
        right: "process",
        fields: vec!["pid", "backend_start=starttime"],
        cardinality: "zero_or_one",
        provenance: "pid_and_process_start_match",
    }];
    let columns = vec![
        raw_column("pid", ValueType::I64, "activity.pid", false, &["activity"]),
        raw_column(
            "user",
            ValueType::Text,
            "activity.usename",
            false,
            &["activity"],
        ),
        raw_column(
            "database",
            ValueType::Text,
            "activity.datname",
            false,
            &["activity"],
        ),
        raw_column(
            "application",
            ValueType::Text,
            "activity.application_name",
            false,
            &["activity"],
        ),
        raw_column(
            "state",
            ValueType::Text,
            "activity.state",
            false,
            &["activity"],
        ),
        derived_column(
            "wait_event",
            ValueType::Text,
            "join_non_null(wait_event_type, ':', wait_event)",
            &["activity"],
        ),
        raw_column(
            "query",
            ValueType::Text,
            "activity.query",
            true,
            &["activity"],
        ),
        derived_column(
            "query_duration_us",
            ValueType::F64,
            "snapshot_ts - query_start",
            &["activity"],
        ),
        derived_column(
            "transaction_duration_us",
            ValueType::F64,
            "snapshot_ts - xact_start",
            &["activity"],
        ),
        derived_column(
            "cpu",
            ValueType::F64,
            "positive_delta(utime + stime) / elapsed",
            &["activity", "process"],
        ),
    ];
    let presets = vec![
        preset(
            "sessions",
            &[
                "pid",
                "user",
                "database",
                "application",
                "state",
                "query",
                "query_duration_us",
            ],
            "query_duration_us",
            "desc",
        ),
        preset(
            "waits",
            &[
                "pid",
                "user",
                "database",
                "wait_event",
                "query",
                "query_duration_us",
            ],
            "query_duration_us",
            "desc",
        ),
        preset(
            "resources",
            &["pid", "user", "database", "cpu", "query"],
            "cpu",
            "desc",
        ),
        preset(
            "transaction_age",
            &[
                "pid",
                "user",
                "database",
                "state",
                "transaction_duration_us",
            ],
            "transaction_duration_us",
            "desc",
        ),
    ];
    view(projection, Scope::Database, joins, columns, presets)
}

#[allow(
    clippy::too_many_lines,
    reason = "the view is a declarative projection registry kept together for review"
)]
fn statements_view() -> ViewSpec {
    let projection = projection("statements");
    let columns = vec![
        raw_column(
            "queryid",
            ValueType::I64,
            "statements.queryid",
            false,
            &["statements"],
        ),
        raw_column(
            "query",
            ValueType::Text,
            "statements.query",
            true,
            &["statements"],
        ),
        derived_column(
            "calls",
            ValueType::F64,
            "positive_delta(calls)",
            &["statements"],
        ),
        derived_column(
            "total",
            ValueType::F64,
            "positive_delta(total_exec_time)",
            &["statements"],
        ),
        derived_column(
            "mean",
            ValueType::F64,
            "positive_delta(total_exec_time) / max(positive_delta(calls), 1)",
            &["statements"],
        ),
        derived_column(
            "rows",
            ValueType::F64,
            "positive_delta(rows)",
            &["statements"],
        ),
        derived_column(
            "hit_pct",
            ValueType::F64,
            "100 * positive_delta(shared_blks_hit) / max(positive_delta(shared_blks_hit + shared_blks_read), 1)",
            &["statements"],
        ),
        derived_column(
            "blks_read",
            ValueType::F64,
            "positive_delta(shared_blks_read + local_blks_read)",
            &["statements"],
        ),
        derived_column(
            "temp_written",
            ValueType::F64,
            "positive_delta(temp_blks_written)",
            &["statements"],
        ),
        derived_column(
            "wal_bytes",
            ValueType::F64,
            "positive_delta(wal_bytes)",
            &["statements"],
        ),
    ];
    let presets = vec![
        preset(
            "time",
            &["queryid", "query", "calls", "total", "mean", "rows"],
            "total",
            "desc",
        ),
        preset(
            "io",
            &["queryid", "query", "calls", "hit_pct", "blks_read"],
            "blks_read",
            "desc",
        ),
        preset(
            "temp",
            &["queryid", "query", "calls", "temp_written"],
            "temp_written",
            "desc",
        ),
        preset(
            "wal",
            &["queryid", "query", "calls", "wal_bytes", "total"],
            "wal_bytes",
            "desc",
        ),
    ];
    view(projection, Scope::Database, Vec::new(), columns, presets)
}

fn plans_view() -> ViewSpec {
    view(
        projection("plans"),
        Scope::Database,
        Vec::new(),
        vec![
            raw_column("planid", ValueType::I64, "plans.planid", false, &["plans"]),
            raw_column("plan", ValueType::Text, "plans.plan", true, &["plans"]),
            derived_column(
                "queryid",
                ValueType::I64,
                "coalesce(queryid, queryid_stat_statements)",
                &["plans"],
            ),
            derived_column("calls", ValueType::F64, "positive_delta(calls)", &["plans"]),
            derived_column(
                "mean",
                ValueType::F64,
                "positive_delta(total_time) / max(positive_delta(calls), 1)",
                &["plans"],
            ),
            derived_column("rows", ValueType::F64, "positive_delta(rows)", &["plans"]),
        ],
        vec![
            preset(
                "time",
                &["planid", "plan", "queryid", "calls", "mean", "rows"],
                "mean",
                "desc",
            ),
            preset(
                "io",
                &["planid", "plan", "queryid", "calls"],
                "calls",
                "desc",
            ),
            preset(
                "rows",
                &["planid", "plan", "queryid", "rows"],
                "rows",
                "desc",
            ),
            preset(
                "regression",
                &["planid", "plan", "queryid", "mean"],
                "mean",
                "desc",
            ),
        ],
    )
}

fn tables_view() -> ViewSpec {
    view(
        projection("tables"),
        Scope::Database,
        Vec::new(),
        vec![
            derived_column(
                "relation",
                ValueType::Text,
                "qualify(schemaname, relname)",
                &["tables"],
            ),
            raw_column(
                "seq_scan",
                ValueType::I64,
                "tables.seq_scan",
                false,
                &["tables"],
            ),
            raw_column(
                "idx_scan",
                ValueType::I64,
                "tables.idx_scan",
                false,
                &["tables"],
            ),
            derived_column(
                "dead_pct",
                ValueType::F64,
                "100 * n_dead_tup / max(n_live_tup + n_dead_tup, 1)",
                &["tables"],
            ),
            raw_column(
                "last_autovacuum",
                ValueType::Timestamp,
                "tables.last_autovacuum",
                false,
                &["tables"],
            ),
        ],
        vec![
            preset(
                "activity",
                &["relation", "seq_scan", "idx_scan", "dead_pct"],
                "dead_pct",
                "desc",
            ),
            preset("writes", &["relation", "dead_pct"], "dead_pct", "desc"),
            preset(
                "maintenance",
                &["relation", "dead_pct", "last_autovacuum"],
                "dead_pct",
                "desc",
            ),
            preset(
                "io",
                &["relation", "seq_scan", "idx_scan"],
                "seq_scan",
                "desc",
            ),
        ],
    )
}

fn indexes_view() -> ViewSpec {
    view(
        projection("indexes"),
        Scope::Database,
        Vec::new(),
        vec![
            raw_column(
                "index",
                ValueType::Text,
                "indexes.indexrelname",
                false,
                &["indexes"],
            ),
            raw_column(
                "table",
                ValueType::Text,
                "indexes.relname",
                false,
                &["indexes"],
            ),
            derived_column(
                "scans",
                ValueType::F64,
                "positive_delta(idx_scan)",
                &["indexes"],
            ),
            derived_column(
                "rows_per_scan",
                ValueType::F64,
                "positive_delta(idx_tup_read) / max(positive_delta(idx_scan), 1)",
                &["indexes"],
            ),
        ],
        vec![
            preset(
                "usage",
                &["index", "table", "scans", "rows_per_scan"],
                "scans",
                "desc",
            ),
            preset("unused", &["index", "table", "scans"], "scans", "asc"),
            preset("io", &["index", "table", "scans"], "scans", "desc"),
        ],
    )
}

fn vacuum_view() -> ViewSpec {
    view(
        projection("vacuum"),
        Scope::Database,
        Vec::new(),
        vec![
            raw_column("pid", ValueType::I64, "vacuum.pid", false, &["vacuum"]),
            raw_column("table", ValueType::U64, "vacuum.relid", false, &["vacuum"]),
            raw_column("phase", ValueType::Text, "vacuum.phase", false, &["vacuum"]),
            raw_column(
                "is_autovacuum",
                ValueType::Bool,
                "vacuum.is_autovacuum",
                false,
                &["vacuum"],
            ),
            derived_column(
                "progress",
                ValueType::F64,
                "heap_blks_scanned / max(heap_blks_total, 1)",
                &["vacuum"],
            ),
            derived_column(
                "dead_tuples",
                ValueType::F64,
                "coalesce(num_dead_tuples, num_dead_item_ids)",
                &["vacuum"],
            ),
        ],
        vec![
            preset(
                "progress",
                &["pid", "table", "phase", "progress", "dead_tuples"],
                "progress",
                "desc",
            ),
            preset(
                "phase",
                &["pid", "table", "phase", "progress"],
                "phase",
                "asc",
            ),
            preset(
                "dead_tuples",
                &["pid", "table", "dead_tuples", "progress"],
                "dead_tuples",
                "desc",
            ),
        ],
    )
}

fn processes_view() -> ViewSpec {
    let mut columns = vec![
        raw_column("pid", ValueType::I64, "process.pid", false, &["process"]),
        raw_column("type", ValueType::Text, "process.comm", false, &["process"]),
        derived_column(
            "cpu",
            ValueType::F64,
            "positive_delta(utime + stime) / elapsed",
            &["process"],
        ),
        raw_column(
            "rss",
            ValueType::I64,
            "process.rmem_kb",
            false,
            &["process"],
        ),
        unavailable_column("pss", ValueType::I64, "smaps_rollup.pss_kb"),
        derived_column(
            "read_bytes_per_second",
            ValueType::F64,
            "positive_delta(read_bytes) / elapsed",
            &["process"],
        ),
        derived_column(
            "write_bytes_per_second",
            ValueType::F64,
            "positive_delta(write_bytes) / elapsed",
            &["process"],
        ),
        derived_column(
            "block_delay",
            ValueType::F64,
            "positive_delta(blkdelay_ticks) / elapsed",
            &["process"],
        ),
        raw_column(
            "command",
            ValueType::Text,
            "process.cmdline",
            true,
            &["process"],
        ),
    ];
    columns.shrink_to_fit();
    view(
        projection("processes"),
        Scope::Host,
        Vec::new(),
        columns,
        vec![
            preset(
                "cpu",
                &["pid", "type", "cpu", "rss", "command"],
                "cpu",
                "desc",
            ),
            preset(
                "memory",
                &["pid", "type", "rss", "pss", "command"],
                "rss",
                "desc",
            ),
            preset(
                "disk_io",
                &[
                    "pid",
                    "type",
                    "read_bytes_per_second",
                    "write_bytes_per_second",
                    "block_delay",
                    "command",
                ],
                "read_bytes_per_second",
                "desc",
            ),
        ],
    )
}

fn locks_view() -> ViewSpec {
    view(
        projection("locks"),
        Scope::Database,
        Vec::new(),
        vec![
            raw_column("pid", ValueType::I64, "locks.pid", false, &["locks"]),
            derived_column(
                "user_application",
                ValueType::Text,
                "join_non_null(usename, ' / ', application_name)",
                &["locks"],
            ),
            derived_column(
                "lock",
                ValueType::Text,
                "join_non_null(wait_event_type, ':', wait_event)",
                &["locks"],
            ),
            raw_column(
                "target",
                ValueType::Text,
                "locks.lock_relname",
                false,
                &["locks"],
            ),
            derived_column(
                "wait_or_hold_us",
                ValueType::F64,
                "proven_wait_or_hold_duration_us",
                &["locks"],
            ),
            raw_column("query", ValueType::Text, "locks.query", true, &["locks"]),
        ],
        vec![
            preset(
                "tree",
                &[
                    "pid",
                    "user_application",
                    "lock",
                    "target",
                    "wait_or_hold_us",
                    "query",
                ],
                "wait_or_hold_us",
                "desc",
            ),
            preset(
                "blockers",
                &["pid", "user_application", "target", "wait_or_hold_us"],
                "wait_or_hold_us",
                "desc",
            ),
            preset(
                "waiters",
                &[
                    "pid",
                    "user_application",
                    "lock",
                    "target",
                    "query",
                    "wait_or_hold_us",
                ],
                "wait_or_hold_us",
                "desc",
            ),
        ],
    )
}

fn events_view() -> ViewSpec {
    view(
        projection("events"),
        Scope::Instance,
        Vec::new(),
        vec![
            raw_column(
                "time",
                ValueType::Timestamp,
                "events.ts",
                false,
                &["events"],
            ),
            raw_column(
                "severity",
                ValueType::U64,
                "events.severity",
                false,
                &["events"],
            ),
            raw_column(
                "type",
                ValueType::U64,
                "events.category",
                false,
                &["events"],
            ),
            raw_column(
                "duration",
                ValueType::F64,
                "events.duration_us",
                false,
                &["events"],
            ),
            raw_column(
                "message",
                ValueType::Text,
                "events.message",
                true,
                &["events"],
            ),
        ],
        vec![
            preset(
                "errors",
                &["time", "severity", "type", "message"],
                "time",
                "desc",
            ),
            preset(
                "checkpoints",
                &["time", "type", "duration", "message"],
                "time",
                "desc",
            ),
            preset(
                "vacuum",
                &["time", "type", "duration", "message"],
                "time",
                "desc",
            ),
            preset(
                "slow",
                &["time", "type", "duration", "message"],
                "duration",
                "desc",
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_availability_does_not_depend_on_input_order() {
        let process_type = registry()
            .iter()
            .find(|contract| contract.name == "os_process")
            .expect("os_process contract")
            .type_id
            .get();
        let mut activity = activity_view();
        activity.inputs.swap(0, 1);

        apply_availability(&mut activity, &BTreeSet::from([process_type]));

        assert_eq!(activity.availability, Availability::Gated);
    }
}
