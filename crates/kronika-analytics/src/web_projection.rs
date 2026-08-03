//! Stable, source-independent web projection registry.
//!
//! The registry keeps wire identity and executable metric operands together.
//! It deliberately has no PGM registry, reader, storage, or HTTP dependencies.

/// How values from time buckets contribute to an entity's range score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAggregation {
    /// Add observed bucket values.
    Sum,
    /// Take the largest observed bucket value.
    Max,
}

impl WebAggregation {
    /// Public catalog spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Max => "max",
        }
    }
}

/// Stable unit used by an indexed metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebUnit {
    /// Microseconds.
    Microseconds,
    /// Milliseconds.
    Milliseconds,
    /// Dimensionless count.
    Count,
    /// `PostgreSQL` blocks.
    Blocks,
    /// Dimensionless ratio.
    Ratio,
    /// Bytes per second.
    BytesPerSecond,
    /// Percent.
    Percent,
}

impl WebUnit {
    /// Numeric OVF unit code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Microseconds => 1,
            Self::Milliseconds => 6,
            Self::Count => 2,
            Self::Blocks => 3,
            Self::Ratio => 4,
            Self::BytesPerSecond => 5,
            Self::Percent => 7,
        }
    }

    /// Public catalog spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microseconds => "us",
            Self::Milliseconds => "ms",
            Self::Count => "count",
            Self::Blocks => "blocks",
            Self::Ratio => "ratio",
            Self::BytesPerSecond => "bytes_per_second",
            Self::Percent => "percent",
        }
    }
}

/// Executable metric formula with its normative catalog expression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WebFormula {
    /// Sum positive deltas of the named cumulative fields.
    PositiveDeltaSum {
        /// Alternative cumulative field sets, newest compatible layout first.
        field_sets: &'static [&'static [&'static str]],
        /// Multiplier applied after differencing.
        scale: f64,
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Divide a positive cumulative delta by elapsed wall time.
    PositiveDeltaRate {
        /// Alternative cumulative field sets, newest compatible layout first.
        field_sets: &'static [&'static [&'static str]],
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Divide a positive tick-counter delta by instance HZ and elapsed wall time.
    PositiveDeltaTickRate {
        /// Alternative cumulative field sets, newest compatible layout first.
        field_sets: &'static [&'static [&'static str]],
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Sum observed wait duration inferred from consecutive activity samples.
    ActivityWait {
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Fraction of observed activity samples whose state is active.
    ActiveFraction {
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Maximum non-negative ratio of two instantaneous fields.
    GaugeRatio {
        /// Numerator field.
        numerator: &'static str,
        /// Denominator field.
        denominator: &'static str,
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Maximum observed lock wait age measured from `waitstart`.
    LockDuration {
        /// Normative catalog expression.
        expression: &'static str,
    },
    /// Count retained event rows.
    EventCount {
        /// Normative catalog expression.
        expression: &'static str,
    },
}

impl WebFormula {
    /// Normative formula exposed by the projection catalog.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PositiveDeltaSum { expression, .. }
            | Self::PositiveDeltaRate { expression, .. }
            | Self::PositiveDeltaTickRate { expression, .. }
            | Self::ActivityWait { expression }
            | Self::ActiveFraction { expression }
            | Self::GaugeRatio { expression, .. }
            | Self::LockDuration { expression }
            | Self::EventCount { expression } => expression,
        }
    }
}

/// One physical input family required by a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebInput {
    /// Stable input code referenced by metrics and columns.
    pub code: &'static str,
    /// Registry section names accepted by this input.
    pub sections: &'static [&'static str],
}

/// One heatmap and spark metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WebMetric {
    /// Numeric code inside the view-addressed OVF block.
    pub code: u16,
    /// Public URL and JSON code.
    pub name: &'static str,
    /// Independently changeable projection revision.
    pub revision: u16,
    /// Stable unit.
    pub unit: WebUnit,
    /// Range aggregation.
    pub aggregation: WebAggregation,
    /// Executable formula.
    pub formula: WebFormula,
    /// Input groups required by the formula.
    pub requires: &'static [&'static str],
    /// Whether this metric drives the canonical row spark.
    pub canonical: bool,
}

/// One stable UI projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WebView {
    /// Numeric OVF logical ID.
    pub code: u16,
    /// Public URL and JSON code.
    pub name: &'static str,
    /// Independently changeable view revision.
    pub revision: u16,
    /// Typed identity encoding revision.
    pub identity_revision: u16,
    /// Largest elapsed interval that can prove a cumulative-counter delta.
    pub max_rate_gap_us: Option<i64>,
    /// Physical input families.
    pub inputs: &'static [WebInput],
    /// Heatmap and spark metrics.
    pub metrics: &'static [WebMetric],
}

const DELTA_MAX_RATE_GAP_US: i64 = 15 * 60 * 1_000_000;

const ACTIVITY_INPUTS: &[WebInput] = &[
    WebInput {
        code: "activity",
        sections: &["pg_stat_activity"],
    },
    WebInput {
        code: "process",
        sections: &["os_process"],
    },
    WebInput {
        code: "instance",
        sections: &["instance_metadata"],
    },
    WebInput {
        code: "replication_replicas",
        sections: &["pg_stat_replication"],
    },
];
const STATEMENTS_INPUTS: &[WebInput] = &[
    WebInput {
        code: "statements",
        sections: &["pg_stat_statements"],
    },
    WebInput {
        code: "reset_metadata",
        sections: &["reset_metadata"],
    },
    WebInput {
        code: "settings",
        sections: &["pg_settings"],
    },
];
const PLANS_INPUTS: &[WebInput] = &[
    WebInput {
        code: "plans",
        sections: &["pg_store_plans_ossc", "pg_store_plans_vadv"],
    },
    WebInput {
        code: "reset_metadata",
        sections: &["reset_metadata"],
    },
];
const TABLES_INPUTS: &[WebInput] = &[
    WebInput {
        code: "tables",
        sections: &["pg_stat_user_tables"],
    },
    WebInput {
        code: "vacuum",
        sections: &["pg_stat_progress_vacuum"],
    },
];
const INDEXES_INPUTS: &[WebInput] = &[
    WebInput {
        code: "indexes",
        sections: &["pg_stat_user_indexes"],
    },
    WebInput {
        code: "tables",
        sections: &["pg_stat_user_tables"],
    },
];
const VACUUM_INPUTS: &[WebInput] = &[
    WebInput {
        code: "vacuum",
        sections: &["pg_stat_progress_vacuum"],
    },
    WebInput {
        code: "tables",
        sections: &["pg_stat_user_tables"],
    },
];
const PROCESS_INPUTS: &[WebInput] = &[
    WebInput {
        code: "process",
        sections: &["os_process"],
    },
    WebInput {
        code: "instance",
        sections: &["instance_metadata"],
    },
    WebInput {
        code: "cgroup_mapping",
        sections: &["os_cgroup_mapping"],
    },
];
const LOCK_INPUTS: &[WebInput] = &[WebInput {
    code: "locks",
    sections: &["pg_locks"],
}];
const EVENT_INPUTS: &[WebInput] = &[WebInput {
    code: "events",
    sections: &[
        "pg_log_errors",
        "pg_log_checkpoints",
        "pg_log_autovacuum",
        "pg_log_slow_queries",
        "pg_log_lock_waits",
        "pg_log_lifecycle",
        "pg_log_gap",
        "pg_log_temp_files",
        "pg_log_source_status",
    ],
}];

const ACTIVITY_METRICS: &[WebMetric] = &[
    WebMetric {
        code: 1,
        name: "wait",
        revision: 1,
        unit: WebUnit::Microseconds,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::ActivityWait {
            expression: "sum(observed_wait_duration_us)",
        },
        requires: &["activity"],
        canonical: false,
    },
    WebMetric {
        code: 2,
        name: "cpu",
        revision: 2,
        unit: WebUnit::Ratio,
        aggregation: WebAggregation::Max,
        formula: WebFormula::PositiveDeltaTickRate {
            field_sets: &[&["utime", "stime"]],
            expression: "positive_delta(utime + stime) / (clock_ticks_per_sec * elapsed_seconds)",
        },
        requires: &["activity", "process", "instance"],
        canonical: false,
    },
    WebMetric {
        code: 3,
        name: "io",
        revision: 1,
        unit: WebUnit::BytesPerSecond,
        aggregation: WebAggregation::Max,
        formula: WebFormula::PositiveDeltaRate {
            field_sets: &[&["read_bytes", "write_bytes"]],
            expression: "positive_delta(read_bytes + write_bytes) / elapsed",
        },
        requires: &["activity", "process"],
        canonical: false,
    },
    WebMetric {
        code: 4,
        name: "active_fraction",
        revision: 1,
        unit: WebUnit::Ratio,
        aggregation: WebAggregation::Max,
        formula: WebFormula::ActiveFraction {
            expression: "active_samples / observed_samples",
        },
        requires: &["activity"],
        canonical: true,
    },
];

const STATEMENT_METRICS: &[WebMetric] = &[
    WebMetric {
        code: 1,
        name: "time",
        revision: 3,
        unit: WebUnit::Milliseconds,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["total_exec_time"], &["total_time"]],
            scale: 1.0,
            expression: "sum(positive_delta(total_exec_time))",
        },
        requires: &["statements"],
        canonical: true,
    },
    WebMetric {
        code: 2,
        name: "calls",
        revision: 2,
        unit: WebUnit::Count,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["calls"]],
            scale: 1.0,
            expression: "sum(positive_delta(calls))",
        },
        requires: &["statements"],
        canonical: false,
    },
    WebMetric {
        code: 3,
        name: "io",
        revision: 2,
        unit: WebUnit::Blocks,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["shared_blks_read", "local_blks_read"]],
            scale: 1.0,
            expression: "sum(positive_delta(shared_blks_read + local_blks_read))",
        },
        requires: &["statements"],
        canonical: false,
    },
    WebMetric {
        code: 4,
        name: "temp",
        revision: 2,
        unit: WebUnit::Blocks,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["temp_blks_written"]],
            scale: 1.0,
            expression: "sum(positive_delta(temp_blks_written))",
        },
        requires: &["statements"],
        canonical: false,
    },
];

const PLAN_METRICS: &[WebMetric] = &[
    WebMetric {
        code: 1,
        name: "time",
        revision: 2,
        unit: WebUnit::Microseconds,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["total_time"]],
            scale: 1_000.0,
            expression: "sum(positive_delta(total_time)) * 1000",
        },
        requires: &["plans"],
        canonical: true,
    },
    WebMetric {
        code: 2,
        name: "calls",
        revision: 2,
        unit: WebUnit::Count,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["calls"]],
            scale: 1.0,
            expression: "sum(positive_delta(calls))",
        },
        requires: &["plans"],
        canonical: false,
    },
];

const TABLE_METRICS: &[WebMetric] = &[
    WebMetric {
        code: 1,
        name: "io",
        revision: 1,
        unit: WebUnit::Blocks,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&[
                "heap_blks_read",
                "idx_blks_read",
                "toast_blks_read",
                "tidx_blks_read",
            ]],
            scale: 1.0,
            expression: "sum(positive_delta(heap_blks_read + idx_blks_read + toast_blks_read + tidx_blks_read))",
        },
        requires: &["tables"],
        canonical: false,
    },
    WebMetric {
        code: 2,
        name: "writes",
        revision: 1,
        unit: WebUnit::Count,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["n_tup_ins", "n_tup_upd", "n_tup_del"]],
            scale: 1.0,
            expression: "sum(positive_delta(n_tup_ins + n_tup_upd + n_tup_del))",
        },
        requires: &["tables"],
        canonical: true,
    },
    WebMetric {
        code: 3,
        name: "dead",
        revision: 1,
        unit: WebUnit::Ratio,
        aggregation: WebAggregation::Max,
        formula: WebFormula::GaugeRatio {
            numerator: "n_dead_tup",
            denominator: "n_live_tup",
            expression: "max(n_dead_tup / max(n_live_tup + n_dead_tup, 1))",
        },
        requires: &["tables"],
        canonical: false,
    },
];

const INDEX_METRICS: &[WebMetric] = &[
    WebMetric {
        code: 1,
        name: "io",
        revision: 1,
        unit: WebUnit::Blocks,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["idx_blks_read"]],
            scale: 1.0,
            expression: "sum(positive_delta(idx_blks_read))",
        },
        requires: &["indexes"],
        canonical: false,
    },
    WebMetric {
        code: 2,
        name: "scans",
        revision: 1,
        unit: WebUnit::Count,
        aggregation: WebAggregation::Sum,
        formula: WebFormula::PositiveDeltaSum {
            field_sets: &[&["idx_scan"]],
            scale: 1.0,
            expression: "sum(positive_delta(idx_scan))",
        },
        requires: &["indexes"],
        canonical: true,
    },
];

const VACUUM_METRICS: &[WebMetric] = &[WebMetric {
    code: 1,
    name: "progress",
    revision: 2,
    unit: WebUnit::Ratio,
    aggregation: WebAggregation::Max,
    formula: WebFormula::GaugeRatio {
        numerator: "heap_blks_scanned",
        denominator: "heap_blks_total",
        expression: "max(heap_blks_scanned / heap_blks_total)",
    },
    requires: &["vacuum"],
    canonical: true,
}];

const PROCESS_METRICS: &[WebMetric] = &[
    WebMetric {
        code: 1,
        name: "cpu",
        revision: 2,
        unit: WebUnit::Ratio,
        aggregation: WebAggregation::Max,
        formula: WebFormula::PositiveDeltaTickRate {
            field_sets: &[&["utime", "stime"]],
            expression: "positive_delta(utime + stime) / (clock_ticks_per_sec * elapsed_seconds)",
        },
        requires: &["process", "instance"],
        canonical: true,
    },
    WebMetric {
        code: 2,
        name: "io",
        revision: 1,
        unit: WebUnit::BytesPerSecond,
        aggregation: WebAggregation::Max,
        formula: WebFormula::PositiveDeltaRate {
            field_sets: &[&["read_bytes", "write_bytes"]],
            expression: "positive_delta(read_bytes + write_bytes) / elapsed",
        },
        requires: &["process"],
        canonical: false,
    },
];

const LOCK_METRICS: &[WebMetric] = &[WebMetric {
    code: 1,
    name: "wait",
    revision: 2,
    unit: WebUnit::Microseconds,
    aggregation: WebAggregation::Max,
    formula: WebFormula::LockDuration {
        expression: "max(wait_age_us from waitstart)",
    },
    requires: &["locks"],
    canonical: true,
}];

const EVENT_METRICS: &[WebMetric] = &[WebMetric {
    code: 1,
    name: "count",
    revision: 1,
    unit: WebUnit::Count,
    aggregation: WebAggregation::Sum,
    formula: WebFormula::EventCount {
        expression: "count(events)",
    },
    requires: &["events"],
    canonical: true,
}];

const WEB_VIEWS: &[WebView] = &[
    WebView {
        code: 1,
        name: "activity",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: Some(DELTA_MAX_RATE_GAP_US),
        inputs: ACTIVITY_INPUTS,
        metrics: ACTIVITY_METRICS,
    },
    WebView {
        code: 2,
        name: "statements",
        revision: 3,
        identity_revision: 1,
        max_rate_gap_us: Some(DELTA_MAX_RATE_GAP_US),
        inputs: STATEMENTS_INPUTS,
        metrics: STATEMENT_METRICS,
    },
    WebView {
        code: 3,
        name: "plans",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: Some(DELTA_MAX_RATE_GAP_US),
        inputs: PLANS_INPUTS,
        metrics: PLAN_METRICS,
    },
    WebView {
        code: 4,
        name: "tables",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: Some(DELTA_MAX_RATE_GAP_US),
        inputs: TABLES_INPUTS,
        metrics: TABLE_METRICS,
    },
    WebView {
        code: 5,
        name: "indexes",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: Some(DELTA_MAX_RATE_GAP_US),
        inputs: INDEXES_INPUTS,
        metrics: INDEX_METRICS,
    },
    WebView {
        code: 6,
        name: "vacuum",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: None,
        inputs: VACUUM_INPUTS,
        metrics: VACUUM_METRICS,
    },
    WebView {
        code: 7,
        name: "processes",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: Some(DELTA_MAX_RATE_GAP_US),
        inputs: PROCESS_INPUTS,
        metrics: PROCESS_METRICS,
    },
    WebView {
        code: 8,
        name: "locks",
        revision: 2,
        identity_revision: 1,
        max_rate_gap_us: None,
        inputs: LOCK_INPUTS,
        metrics: LOCK_METRICS,
    },
    WebView {
        code: 9,
        name: "events",
        revision: 1,
        identity_revision: 1,
        max_rate_gap_us: None,
        inputs: EVENT_INPUTS,
        metrics: EVENT_METRICS,
    },
];

/// Every stable web projection in ascending numeric-code order.
#[must_use]
pub const fn web_views() -> &'static [WebView] {
    WEB_VIEWS
}

/// Finds a view by its numeric OVF code.
#[must_use]
pub fn web_view_by_code(code: u16) -> Option<&'static WebView> {
    WEB_VIEWS.iter().find(|view| view.code == code)
}

/// Finds a view by its public URL and JSON code.
#[must_use]
pub fn web_view_by_name(name: &str) -> Option<&'static WebView> {
    WEB_VIEWS.iter().find(|view| view.name == name)
}
