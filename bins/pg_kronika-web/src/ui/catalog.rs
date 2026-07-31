//! Availability-aware catalog for the stable web UI projections.

use std::collections::BTreeSet;

use kronika_analytics::web_projection::{WebView, web_view_by_name};
use kronika_registry::registry;
use serde::Serialize;
use utoipa::ToSchema;

use super::thresholds::binding_for;

/// Catalog schema revision.
const CATALOG_REVISION: u16 = 2;

/// Availability of one input, metric, or column in the served store.
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
    /// Store-observed availability.
    pub availability: Availability,
    /// Machine reason when the input is not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<&'static str>,
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
    /// Store-observed availability.
    pub availability: Availability,
    /// Machine reason when the metric is not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<&'static str>,
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
    /// Public unit code when the value has a stable numeric unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
    /// Bound numeric threshold policy, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_metric: Option<&'static str>,
    /// Whether frame omits the value and detail loads it.
    pub lazy: bool,
    /// Input groups required by this column.
    pub requires: Vec<&'static str>,
    /// Store-observed availability.
    pub availability: Availability,
    /// Machine reason when the column is not available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<&'static str>,
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

/// UI operations supported by one projection identity.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub(crate) struct ViewCapabilities {
    /// Point detail can resolve the typed entity identity.
    pub detail: bool,
    /// History can follow the identity across snapshots.
    pub history: bool,
    /// At least one proven relation can be returned.
    pub related: bool,
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
    /// Store-observed view availability.
    pub availability: Availability,
    pub capabilities: ViewCapabilities,
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

/// Complete availability-aware catalog response.
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

    /// Build the projection contract used while materializing frame and entity rows.
    ///
    /// Per-request absence is reported from the projected value itself; this
    /// catalog only keeps intrinsically unavailable columns gated.
    #[must_use]
    pub(crate) fn for_materialization() -> Self {
        let known = registry()
            .iter()
            .map(|contract| contract.type_id.get())
            .collect();
        Self::for_type_ids(&known)
    }

    /// Stable view list in ascending `view_code` order.
    #[allow(
        dead_code,
        reason = "the frame request parser is wired into the HTTP router in a later task"
    )]
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
        let available = input
            .type_ids
            .iter()
            .any(|type_id| observed.contains(type_id));
        if available {
            input.availability = Availability::Available;
            input.unavailable_reason = None;
        } else {
            input.availability = Availability::Gated;
            input.unavailable_reason = Some("missing_extension");
        }
    }
    let input_available = |code: &str| {
        view.inputs
            .iter()
            .any(|input| input.code == code && input.availability == Availability::Available)
    };
    for metric in &mut view.metrics {
        metric.availability = availability_for(&metric.requires, &input_available);
        metric.unavailable_reason =
            (metric.availability != Availability::Available).then_some("missing_extension");
    }
    for column in &mut view.columns {
        if column.availability != Availability::NotCollected
            && column.unavailable_reason != Some("missing_provenance")
        {
            column.availability = availability_for(&column.requires, &input_available);
            column.unavailable_reason =
                (column.availability != Availability::Available).then_some("missing_extension");
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
        unavailable_reason: Some("missing_extension"),
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
            unavailable_reason: Some("missing_extension"),
        })
        .collect()
}

fn raw_column(
    code: &'static str,
    value_type: ValueType,
    source: &'static str,
    lazy: bool,
    requires: &[&'static str],
    unit: Option<&'static str>,
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: Some(source),
        formula: None,
        unit,
        threshold_metric: None,
        lazy,
        requires: requires.to_vec(),
        availability: Availability::Gated,
        unavailable_reason: Some("missing_extension"),
    }
}

fn derived_column(
    code: &'static str,
    value_type: ValueType,
    formula: &'static str,
    requires: &[&'static str],
    unit: Option<&'static str>,
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: None,
        formula: Some(formula),
        unit,
        threshold_metric: None,
        lazy: false,
        requires: requires.to_vec(),
        availability: Availability::Gated,
        unavailable_reason: Some("missing_extension"),
    }
}

const fn unavailable_column(
    code: &'static str,
    value_type: ValueType,
    source: &'static str,
    unit: Option<&'static str>,
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: Some(source),
        formula: None,
        unit,
        threshold_metric: None,
        lazy: false,
        requires: Vec::new(),
        availability: Availability::NotCollected,
        unavailable_reason: Some("not_collected"),
    }
}

const fn unavailable_column_with_reason(
    code: &'static str,
    value_type: ValueType,
    formula: &'static str,
    reason: &'static str,
) -> ColumnSpec {
    ColumnSpec {
        code,
        value_type,
        source: None,
        formula: Some(formula),
        unit: None,
        threshold_metric: None,
        lazy: false,
        requires: Vec::new(),
        availability: Availability::Gated,
        unavailable_reason: Some(reason),
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
    mut columns: Vec<ColumnSpec>,
    presets: Vec<PresetSpec>,
) -> ViewSpec {
    let canonical_metric = projection
        .metrics
        .iter()
        .find(|metric| metric.canonical)
        .expect("shared projection has one canonical metric")
        .name;
    for column in &mut columns {
        column.threshold_metric =
            binding_for(projection.name, column.code).map(|binding| binding.metric_id.as_str());
    }
    ViewSpec {
        view_code: projection.code,
        code: projection.name,
        view_revision: projection.revision,
        scope,
        identity_revision: projection.identity_revision,
        availability: Availability::Gated,
        capabilities: ViewCapabilities {
            detail: true,
            history: matches!(
                projection.name.as_bytes(),
                b"activity" | b"statements" | b"plans" | b"tables" | b"indexes" | b"processes"
            ),
            related: projection.name.as_bytes() == b"statements",
        },
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
    let joins = vec![
        JoinSpec {
            left: "activity",
            right: "process",
            fields: vec!["pid", "backend_start=starttime"],
            cardinality: "zero_or_one",
            provenance: "pid_and_process_start_match",
        },
        JoinSpec {
            left: "activity",
            right: "replication_replicas",
            fields: vec!["pid", "ts"],
            cardinality: "zero_or_one",
            provenance: "same_snapshot_walsender_pid",
        },
    ];
    let columns = vec![
        raw_column(
            "pid",
            ValueType::I64,
            "activity.pid",
            false,
            &["activity"],
            None,
        ),
        raw_column(
            "user",
            ValueType::Text,
            "activity.usename",
            false,
            &["activity"],
            None,
        ),
        raw_column(
            "database",
            ValueType::Text,
            "activity.datname",
            false,
            &["activity"],
            None,
        ),
        raw_column(
            "application",
            ValueType::Text,
            "activity.application_name",
            false,
            &["activity"],
            None,
        ),
        raw_column(
            "backend_type",
            ValueType::Text,
            "activity.backend_type",
            false,
            &["activity"],
            None,
        ),
        raw_column(
            "state",
            ValueType::Text,
            "activity.state",
            false,
            &["activity"],
            None,
        ),
        derived_column(
            "wait_event",
            ValueType::Text,
            "join_non_null(wait_event_type, ':', wait_event)",
            &["activity"],
            None,
        ),
        raw_column(
            "query",
            ValueType::Text,
            "activity.query",
            true,
            &["activity"],
            None,
        ),
        derived_column(
            "query_duration_us",
            ValueType::F64,
            "snapshot_ts - query_start",
            &["activity"],
            Some("us"),
        ),
        derived_column(
            "transaction_duration_us",
            ValueType::F64,
            "snapshot_ts - xact_start",
            &["activity"],
            Some("us"),
        ),
        derived_column(
            "cpu",
            ValueType::F64,
            "positive_delta(utime + stime) / elapsed",
            &["activity", "process"],
            None,
        ),
        raw_column(
            "replication_state",
            ValueType::Text,
            "replication_replicas.state",
            false,
            &["activity", "replication_replicas"],
            None,
        ),
        raw_column(
            "sync_state",
            ValueType::Text,
            "replication_replicas.sync_state",
            false,
            &["activity", "replication_replicas"],
            None,
        ),
        raw_column(
            "replay_lag_us",
            ValueType::I64,
            "replication_replicas.replay_lag_us",
            false,
            &["activity", "replication_replicas"],
            Some("us"),
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
        preset(
            "replication",
            &[
                "pid",
                "user",
                "application",
                "backend_type",
                "replication_state",
                "sync_state",
                "replay_lag_us",
            ],
            "replay_lag_us",
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
            None,
        ),
        raw_column(
            "query",
            ValueType::Text,
            "statements.query",
            true,
            &["statements"],
            None,
        ),
        derived_column(
            "calls",
            ValueType::F64,
            "positive_delta(calls)",
            &["statements"],
            None,
        ),
        derived_column(
            "total",
            ValueType::F64,
            "positive_delta(total_exec_time)",
            &["statements"],
            None,
        ),
        derived_column(
            "ms_per_row",
            ValueType::F64,
            "positive_delta(total_exec_time) / positive_delta(rows)",
            &["statements"],
            Some("ms"),
        ),
        derived_column(
            "mean",
            ValueType::F64,
            "positive_delta(total_exec_time) / positive_delta(calls)",
            &["statements"],
            Some("ms"),
        ),
        derived_column(
            "time_pct",
            ValueType::F64,
            "100 * positive_delta(total_exec_time) / sum_after_database_filter(positive_delta(total_exec_time))",
            &["statements"],
            Some("percent"),
        ),
        derived_column(
            "plan_time_pct",
            ValueType::F64,
            "100 * positive_delta(total_plan_time) / (positive_delta(total_plan_time) + positive_delta(total_exec_time)) when pg_stat_statements.track_planning = 'on'",
            &["statements", "settings"],
            Some("percent"),
        ),
        derived_column(
            "rows",
            ValueType::F64,
            "positive_delta(rows)",
            &["statements"],
            None,
        ),
        derived_column(
            "hit_pct",
            ValueType::F64,
            "100 * positive_delta(shared_blks_hit) / max(positive_delta(shared_blks_hit + shared_blks_read), 1)",
            &["statements"],
            None,
        ),
        derived_column(
            "blks_read",
            ValueType::F64,
            "positive_delta(shared_blks_read + local_blks_read)",
            &["statements"],
            None,
        ),
        derived_column(
            "temp_written",
            ValueType::F64,
            "positive_delta(temp_blks_written)",
            &["statements"],
            None,
        ),
        derived_column(
            "wal_bytes",
            ValueType::F64,
            "positive_delta(wal_bytes)",
            &["statements"],
            None,
        ),
    ];
    let presets = vec![
        preset(
            "time",
            &[
                "queryid",
                "query",
                "calls",
                "total",
                "mean",
                "ms_per_row",
                "time_pct",
                "plan_time_pct",
                "rows",
            ],
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

#[allow(
    clippy::too_many_lines,
    reason = "the view is a declarative projection registry kept together for review"
)]
fn plans_view() -> ViewSpec {
    view(
        projection("plans"),
        Scope::Database,
        Vec::new(),
        vec![
            raw_column(
                "planid",
                ValueType::I64,
                "plans.planid",
                false,
                &["plans"],
                None,
            ),
            raw_column(
                "plan",
                ValueType::Text,
                "plans.plan",
                true,
                &["plans"],
                None,
            ),
            derived_column(
                "queryid",
                ValueType::I64,
                "coalesce(queryid, queryid_stat_statements)",
                &["plans"],
                None,
            ),
            derived_column(
                "calls",
                ValueType::F64,
                "positive_delta(calls)",
                &["plans"],
                None,
            ),
            derived_column(
                "mean",
                ValueType::F64,
                "positive_delta(total_time) / max(positive_delta(calls), 1)",
                &["plans"],
                None,
            ),
            derived_column(
                "rows",
                ValueType::F64,
                "positive_delta(rows)",
                &["plans"],
                None,
            ),
            derived_column(
                "shared_hit",
                ValueType::F64,
                "positive_delta(shared_blks_hit)",
                &["plans"],
                Some("blocks"),
            ),
            derived_column(
                "shared_read",
                ValueType::F64,
                "positive_delta(shared_blks_read)",
                &["plans"],
                Some("blocks"),
            ),
            raw_column(
                "first_seen",
                ValueType::Timestamp,
                "plans.first_call",
                false,
                &["plans"],
                None,
            ),
            raw_column(
                "last_seen",
                ValueType::Timestamp,
                "plans.last_call",
                false,
                &["plans"],
                None,
            ),
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
                &[
                    "planid",
                    "plan",
                    "queryid",
                    "calls",
                    "shared_hit",
                    "shared_read",
                ],
                "shared_read",
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

#[allow(
    clippy::too_many_lines,
    reason = "the view is a declarative projection registry kept together for review"
)]
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
                None,
            ),
            raw_column(
                "size",
                ValueType::I64,
                "tables.main_fork_bytes",
                false,
                &["tables"],
                Some("bytes"),
            ),
            raw_column(
                "seq_scan",
                ValueType::I64,
                "tables.seq_scan",
                false,
                &["tables"],
                None,
            ),
            raw_column(
                "idx_scan",
                ValueType::I64,
                "tables.idx_scan",
                false,
                &["tables"],
                None,
            ),
            derived_column(
                "dead_pct",
                ValueType::F64,
                "100 * n_dead_tup / max(n_live_tup + n_dead_tup, 1)",
                &["tables"],
                Some("percent"),
            ),
            derived_column(
                "io_hit_pct",
                ValueType::F64,
                "100 * positive_delta(heap_blks_hit + idx_blks_hit) / max(positive_delta(heap_blks_hit + idx_blks_hit + heap_blks_read + idx_blks_read), 1)",
                &["tables"],
                Some("percent"),
            ),
            raw_column(
                "xid_age",
                ValueType::I64,
                "tables.xid_age",
                false,
                &["tables"],
                Some("transactions"),
            ),
            raw_column(
                "mxid_age",
                ValueType::I64,
                "tables.mxid_age",
                false,
                &["tables"],
                Some("multixacts"),
            ),
            raw_column(
                "dead_tuples",
                ValueType::I64,
                "tables.n_dead_tup",
                false,
                &["tables"],
                Some("count"),
            ),
            derived_column(
                "seq_scan_pct",
                ValueType::F64,
                "100 * positive_delta(seq_scan) / (positive_delta(seq_scan) + positive_delta(idx_scan))",
                &["tables"],
                Some("percent"),
            ),
            raw_column(
                "modified_since_analyze",
                ValueType::I64,
                "tables.n_mod_since_analyze",
                false,
                &["tables"],
                Some("count"),
            ),
            raw_column(
                "inserted_since_vacuum",
                ValueType::I64,
                "tables.n_ins_since_vacuum",
                false,
                &["tables"],
                Some("count"),
            ),
            raw_column(
                "last_autovacuum",
                ValueType::Timestamp,
                "tables.last_autovacuum",
                false,
                &["tables"],
                None,
            ),
            derived_column(
                "autovacuum_age_seconds",
                ValueType::F64,
                "(snapshot_ts - last_autovacuum) / 1000000",
                &["tables"],
                Some("seconds"),
            ),
            derived_column(
                "autoanalyze_age_seconds",
                ValueType::F64,
                "(snapshot_ts - last_autoanalyze) / 1000000",
                &["tables"],
                Some("seconds"),
            ),
        ],
        vec![
            preset(
                "activity",
                &[
                    "relation",
                    "seq_scan",
                    "idx_scan",
                    "seq_scan_pct",
                    "dead_pct",
                    "dead_tuples",
                ],
                "dead_pct",
                "desc",
            ),
            preset(
                "writes",
                &[
                    "relation",
                    "modified_since_analyze",
                    "inserted_since_vacuum",
                    "dead_pct",
                ],
                "inserted_since_vacuum",
                "desc",
            ),
            preset(
                "maintenance",
                &[
                    "relation",
                    "dead_pct",
                    "dead_tuples",
                    "modified_since_analyze",
                    "inserted_since_vacuum",
                    "last_autovacuum",
                    "autovacuum_age_seconds",
                    "autoanalyze_age_seconds",
                ],
                "autovacuum_age_seconds",
                "desc",
            ),
            preset(
                "io",
                &[
                    "relation",
                    "seq_scan",
                    "idx_scan",
                    "seq_scan_pct",
                    "io_hit_pct",
                ],
                "io_hit_pct",
                "desc",
            ),
            preset(
                "size",
                &["relation", "size", "dead_pct", "xid_age", "mxid_age"],
                "size",
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
                None,
            ),
            raw_column(
                "table",
                ValueType::Text,
                "indexes.relname",
                false,
                &["indexes"],
                None,
            ),
            raw_column(
                "size",
                ValueType::I64,
                "indexes.main_fork_bytes",
                false,
                &["indexes"],
                Some("bytes"),
            ),
            derived_column(
                "scans",
                ValueType::F64,
                "positive_delta(idx_scan)",
                &["indexes"],
                None,
            ),
            derived_column(
                "rows_per_scan",
                ValueType::F64,
                "positive_delta(idx_tup_read) / max(positive_delta(idx_scan), 1)",
                &["indexes"],
                None,
            ),
            derived_column(
                "io_hit_pct",
                ValueType::F64,
                "100 * positive_delta(idx_blks_hit) / max(positive_delta(idx_blks_hit + idx_blks_read), 1)",
                &["indexes"],
                Some("percent"),
            ),
            raw_column(
                "last_idx_scan",
                ValueType::Timestamp,
                "indexes.last_idx_scan",
                false,
                &["indexes"],
                None,
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
            preset(
                "size",
                &["index", "table", "size", "scans", "last_idx_scan"],
                "size",
                "desc",
            ),
            preset(
                "io",
                &["index", "table", "scans", "io_hit_pct"],
                "io_hit_pct",
                "desc",
            ),
        ],
    )
}

fn vacuum_view() -> ViewSpec {
    view(
        projection("vacuum"),
        Scope::Database,
        vec![JoinSpec {
            left: "vacuum",
            right: "tables",
            fields: vec!["datid", "relid", "ts"],
            cardinality: "zero_or_one",
            provenance: "same_snapshot_database_relation_oid",
        }],
        vec![
            raw_column(
                "pid",
                ValueType::I64,
                "vacuum.pid",
                false,
                &["vacuum"],
                None,
            ),
            raw_column(
                "table",
                ValueType::U64,
                "vacuum.relid",
                false,
                &["vacuum"],
                None,
            ),
            derived_column(
                "relation",
                ValueType::Text,
                "qualify(tables.schemaname, tables.relname) by same (datid, relid, ts)",
                &["vacuum", "tables"],
                None,
            ),
            raw_column(
                "phase",
                ValueType::Text,
                "vacuum.phase",
                false,
                &["vacuum"],
                None,
            ),
            raw_column(
                "is_autovacuum",
                ValueType::Bool,
                "vacuum.is_autovacuum",
                false,
                &["vacuum"],
                None,
            ),
            derived_column(
                "progress",
                ValueType::F64,
                "heap_blks_scanned / max(heap_blks_total, 1)",
                &["vacuum"],
                None,
            ),
            derived_column(
                "dead_tuples",
                ValueType::F64,
                "coalesce(num_dead_tuples, num_dead_item_ids)",
                &["vacuum"],
                None,
            ),
            unavailable_column_with_reason(
                "elapsed",
                ValueType::F64,
                "snapshot_ts - proven_vacuum_start",
                "missing_provenance",
            ),
        ],
        vec![
            preset(
                "progress",
                &[
                    "pid",
                    "relation",
                    "phase",
                    "progress",
                    "dead_tuples",
                    "elapsed",
                ],
                "progress",
                "desc",
            ),
            preset(
                "phase",
                &["pid", "relation", "phase", "progress", "elapsed"],
                "phase",
                "asc",
            ),
            preset(
                "dead_tuples",
                &["pid", "relation", "dead_tuples", "progress", "elapsed"],
                "dead_tuples",
                "desc",
            ),
        ],
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the view is a declarative projection registry kept together for review"
)]
fn processes_view() -> ViewSpec {
    let mut columns = vec![
        raw_column(
            "pid",
            ValueType::I64,
            "process.pid",
            false,
            &["process"],
            None,
        ),
        raw_column(
            "type",
            ValueType::Text,
            "process.comm",
            false,
            &["process"],
            None,
        ),
        derived_column(
            "cpu",
            ValueType::F64,
            "positive_delta(utime + stime) / elapsed",
            &["process"],
            None,
        ),
        raw_column(
            "rss",
            ValueType::I64,
            "process.rmem_kb",
            false,
            &["process"],
            Some("kib"),
        ),
        raw_column(
            "threads",
            ValueType::U64,
            "process.num_threads",
            false,
            &["process"],
            Some("count"),
        ),
        unavailable_column("pss", ValueType::I64, "smaps_rollup.pss_kb", None),
        derived_column(
            "read_bytes_per_second",
            ValueType::F64,
            "positive_delta(read_bytes) / elapsed",
            &["process"],
            None,
        ),
        derived_column(
            "write_bytes_per_second",
            ValueType::F64,
            "positive_delta(write_bytes) / elapsed",
            &["process"],
            None,
        ),
        derived_column(
            "block_delay",
            ValueType::F64,
            "positive_delta(blkdelay_ticks) / elapsed",
            &["process"],
            None,
        ),
        raw_column(
            "command",
            ValueType::Text,
            "process.cmdline",
            true,
            &["process"],
            None,
        ),
        raw_column(
            "cgroup",
            ValueType::Text,
            "cgroup_mapping.cgroup_path",
            false,
            &["process", "cgroup_mapping"],
            None,
        ),
    ];
    columns.shrink_to_fit();
    view(
        projection("processes"),
        Scope::Host,
        vec![JoinSpec {
            left: "process",
            right: "cgroup_mapping",
            fields: vec!["pid", "starttime", "ts"],
            cardinality: "zero_or_one",
            provenance: "same_snapshot_pid_and_process_start",
        }],
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
            preset(
                "cgroup",
                &["pid", "type", "cgroup", "cpu", "rss", "command"],
                "cpu",
                "desc",
            ),
            preset(
                "threads",
                &["pid", "type", "threads", "cpu", "rss", "command"],
                "threads",
                "desc",
            ),
        ],
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the view is a declarative projection registry kept together for review"
)]
fn locks_view() -> ViewSpec {
    view(
        projection("locks"),
        Scope::Database,
        Vec::new(),
        vec![
            raw_column("pid", ValueType::I64, "locks.pid", false, &["locks"], None),
            raw_column(
                "depth",
                ValueType::I64,
                "locks.depth",
                false,
                &["locks"],
                None,
            ),
            raw_column(
                "root_pid",
                ValueType::I64,
                "locks.root_pid",
                false,
                &["locks"],
                None,
            ),
            raw_column(
                "blocked_by",
                ValueType::Text,
                "locks.blocked_by",
                false,
                &["locks"],
                None,
            ),
            derived_column(
                "user_application",
                ValueType::Text,
                "join_non_null(usename, ' / ', application_name)",
                &["locks"],
                None,
            ),
            derived_column(
                "lock",
                ValueType::Text,
                "join_non_null(wait_event_type, ':', wait_event)",
                &["locks"],
                None,
            ),
            raw_column(
                "granted",
                ValueType::Bool,
                "locks.lock_granted",
                false,
                &["locks"],
                None,
            ),
            raw_column(
                "lock_mode",
                ValueType::Text,
                "locks.lock_mode",
                false,
                &["locks"],
                None,
            ),
            raw_column(
                "lock_type",
                ValueType::Text,
                "locks.lock_locktype",
                false,
                &["locks"],
                None,
            ),
            raw_column(
                "target",
                ValueType::Text,
                "locks.lock_relname",
                false,
                &["locks"],
                None,
            ),
            derived_column(
                "wait_or_hold_us",
                ValueType::F64,
                "proven_wait_or_hold_duration_us",
                &["locks"],
                None,
            ),
            raw_column(
                "query",
                ValueType::Text,
                "locks.query",
                true,
                &["locks"],
                None,
            ),
        ],
        vec![
            preset(
                "tree",
                &[
                    "pid",
                    "depth",
                    "root_pid",
                    "blocked_by",
                    "user_application",
                    "lock",
                    "granted",
                    "lock_mode",
                    "lock_type",
                    "target",
                    "wait_or_hold_us",
                    "query",
                ],
                "wait_or_hold_us",
                "desc",
            ),
            preset(
                "blockers",
                &[
                    "pid",
                    "root_pid",
                    "blocked_by",
                    "user_application",
                    "granted",
                    "lock_mode",
                    "lock_type",
                    "target",
                    "wait_or_hold_us",
                ],
                "wait_or_hold_us",
                "desc",
            ),
            preset(
                "waiters",
                &[
                    "pid",
                    "depth",
                    "root_pid",
                    "blocked_by",
                    "user_application",
                    "lock",
                    "granted",
                    "lock_mode",
                    "lock_type",
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
                None,
            ),
            raw_column(
                "severity",
                ValueType::U64,
                "events.severity",
                false,
                &["events"],
                None,
            ),
            raw_column(
                "severity_code",
                ValueType::Text,
                "stable_severity_code(events.severity)",
                false,
                &["events"],
                None,
            ),
            raw_column(
                "type",
                ValueType::U64,
                "events.category",
                false,
                &["events"],
                None,
            ),
            raw_column(
                "category_code",
                ValueType::Text,
                "stable_category_code(events.category, events.kind)",
                false,
                &["events"],
                None,
            ),
            raw_column(
                "duration",
                ValueType::F64,
                "events.duration_us",
                false,
                &["events"],
                None,
            ),
            raw_column(
                "message",
                ValueType::Text,
                "events.message",
                true,
                &["events"],
                None,
            ),
            raw_column(
                "detail",
                ValueType::Text,
                "events.typed_detail",
                true,
                &["events"],
                None,
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
