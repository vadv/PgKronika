use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;

use kronika_analytics::web_projection::WebView;
use kronika_reader::{
    Gap, LIMIT, LocalDirSnapshot, OutRow, QueryError, QueryLimits, SealedQuerySession, SectionPage,
    SnapshotNeighbors, Value, WebIndexReadError, logical_section,
};
use kronika_registry::ColumnType;

use super::FrameRequest;
use super::cursor::FrameCursor;
use super::dto::{FrameValue, SparkDto};
use super::query::{filter_sort_page, value_sort_key};
use super::threshold::{CellClassification, FrameThresholdContext, classify_row};
use crate::ui::catalog::{ColumnSpec, ProjectionCatalog, ValueType, ViewSpec};

const JS_EXACT_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) enum DeltaOperand {
    #[default]
    Missing,
    Reset,
    Gap,
    Value(f64),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StatementOperands {
    pub calls: DeltaOperand,
    pub rows: DeltaOperand,
    pub exec_ms: DeltaOperand,
    pub plan_ms: DeltaOperand,
    pub planning_fields: bool,
    pub track_planning: bool,
    pub total_exec_ms: Option<f64>,
    pub snapshot_exec_ms_delta_sum: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TableOperands {
    pub live: Option<f64>,
    pub dead: Option<f64>,
    pub seq_scan: DeltaOperand,
    pub idx_scan: DeltaOperand,
    pub modified: Option<f64>,
    #[allow(
        clippy::option_option,
        reason = "outer None means an older layout; inner None is a collected SQL NULL"
    )]
    pub inserted: Option<Option<f64>>,
    pub last_autovacuum_us: Option<i64>,
    pub last_autoanalyze_us: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RowOperands {
    pub snapshot_ts_us: i64,
    pub activity_state: Option<String>,
    pub query_start_us: Option<i64>,
    pub transaction_start_us: Option<i64>,
    pub statements: Option<StatementOperands>,
    pub table: Option<TableOperands>,
    pub process_rss_kib: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectedRow {
    pub entity: Vec<u8>,
    pub spark_entity: Vec<u8>,
    pub label: String,
    pub cells: Vec<FrameValue>,
    pub operands: RowOperands,
    pub classifications: Vec<CellClassification>,
    pub spark: SparkDto,
    pub(crate) values: Vec<(&'static str, FrameValue)>,
    pub(crate) database: Option<String>,
    pub(crate) searchable: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FrameQuality {
    pub snapshots: usize,
    pub gaps: Vec<Gap>,
    pub gated: Vec<String>,
    pub unavailable_revision: Vec<String>,
    pub resource_limited: Vec<String>,
    pub active_tail: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectedFrame {
    pub snapshot_ts_us: i64,
    pub predecessor_ts_us: Option<i64>,
    pub neighbors: SnapshotNeighbors,
    pub rows: Vec<ProjectedRow>,
    pub quality: FrameQuality,
    pub matched: usize,
    pub next: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionInput {
    pub snapshot_ts_us: i64,
    pub predecessor_ts_us: Option<i64>,
    pub neighbors: SnapshotNeighbors,
    current: BTreeMap<String, Vec<OutRow>>,
    previous: BTreeMap<String, Vec<OutRow>>,
    gaps: Vec<Gap>,
}

impl ProjectionInput {
    #[cfg(test)]
    pub(crate) const fn empty(snapshot_ts_us: i64) -> Self {
        Self {
            snapshot_ts_us,
            predecessor_ts_us: None,
            neighbors: SnapshotNeighbors {
                previous: None,
                current: snapshot_ts_us,
                next: None,
            },
            current: BTreeMap::new(),
            previous: BTreeMap::new(),
            gaps: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn single(snapshot_ts_us: i64, section: &str, row: OutRow) -> Self {
        let mut input = Self::empty(snapshot_ts_us);
        input.push(section, row);
        input
    }

    #[cfg(test)]
    pub(crate) fn push(&mut self, section: &str, row: OutRow) {
        self.current
            .entry(section.to_owned())
            .or_default()
            .push(row);
    }

    #[cfg(test)]
    pub(crate) fn push_previous(&mut self, timestamp_us: i64, section: &str, row: OutRow) {
        self.predecessor_ts_us = Some(timestamp_us);
        self.neighbors.previous = Some(timestamp_us);
        self.previous
            .entry(section.to_owned())
            .or_default()
            .push(row);
    }

    pub(crate) fn from_pages(
        snapshot_ts_us: i64,
        predecessor_ts_us: Option<i64>,
        neighbors: SnapshotNeighbors,
        current: BTreeMap<String, SectionPage>,
        previous: BTreeMap<String, SectionPage>,
    ) -> Self {
        let mut gaps = Vec::new();
        let current = current
            .into_iter()
            .map(|(name, page)| {
                gaps.extend(page.gaps);
                (name, page.rows)
            })
            .collect();
        let previous = previous
            .into_iter()
            .map(|(name, page)| {
                gaps.extend(page.gaps);
                (name, page.rows)
            })
            .collect();
        gaps.sort_by_key(|gap| (gap.from, gap.to));
        gaps.dedup();
        Self {
            snapshot_ts_us,
            predecessor_ts_us,
            neighbors,
            current,
            previous,
            gaps,
        }
    }

    fn push_gap(&mut self, gap: Gap) {
        self.gaps.push(gap);
        self.gaps.sort_by_key(|gap| (gap.from, gap.to));
        self.gaps.dedup();
    }
}

#[derive(Debug)]
pub(crate) enum ProjectionError {
    MissingCatalogView,
    MissingPreset,
    InvalidValue(&'static str),
    Cursor,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameLimits {
    pub rows: usize,
    pub cells: usize,
    pub bytes: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            rows: 100_000,
            cells: 2_000_000,
            bytes: 32 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub(crate) enum FrameError {
    WebIndex(WebIndexReadError),
    Query(QueryError),
    Projection(ProjectionError),
    CursorExpired,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebIndex(error) => write!(f, "web index read failed: {error}"),
            Self::Query(error) => write!(f, "frame query failed: {error:?}"),
            Self::Projection(error) => write!(f, "frame projection failed: {error}"),
            Self::CursorExpired => f.write_str("frame cursor snapshot expired"),
        }
    }
}

impl From<WebIndexReadError> for FrameError {
    fn from(error: WebIndexReadError) -> Self {
        Self::WebIndex(error)
    }
}

impl From<QueryError> for FrameError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<ProjectionError> for FrameError {
    fn from(error: ProjectionError) -> Self {
        Self::Projection(error)
    }
}

pub(crate) fn project_frame(
    snapshot: &LocalDirSnapshot,
    request: &FrameRequest,
    catalog: &ProjectionCatalog,
    limits: FrameLimits,
) -> Result<ProjectedFrame, FrameError> {
    let resolved = resolve_snapshot(snapshot, request)?;
    let Some(neighbors) = resolved.neighbors else {
        if request.cursor.is_some() {
            return Err(FrameError::CursorExpired);
        }
        return Ok(ProjectedFrame {
            snapshot_ts_us: request.at_us,
            predecessor_ts_us: None,
            neighbors: SnapshotNeighbors {
                previous: None,
                current: request.at_us,
                next: resolved.next,
            },
            rows: Vec::new(),
            quality: FrameQuality {
                snapshots: 0,
                gaps: vec![Gap {
                    from: request.at_us,
                    to: request.at_us,
                }],
                gated: resolved.gated,
                unavailable_revision: resolved.unavailable_revision,
                resource_limited: resolved.resource_limited,
                active_tail: false,
            },
            matched: 0,
            next: None,
        });
    };
    if request
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.snapshot_ts_us() != neighbors.current)
    {
        return Err(FrameError::CursorExpired);
    }

    let input = read_projection_input(snapshot, request, catalog, limits, &resolved, neighbors)?;
    let mut frame = project_input(request, catalog, input)?;
    frame.quality.gated.extend(resolved.gated);
    frame
        .quality
        .unavailable_revision
        .extend(resolved.unavailable_revision);
    frame
        .quality
        .resource_limited
        .extend(resolved.resource_limited);
    Ok(frame)
}

fn read_projection_input(
    snapshot: &LocalDirSnapshot,
    request: &FrameRequest,
    catalog: &ProjectionCatalog,
    limits: FrameLimits,
    resolved: &ResolvedSnapshot,
    neighbors: SnapshotNeighbors,
) -> Result<ProjectionInput, FrameError> {
    let section_names = request
        .view
        .inputs
        .iter()
        .flat_map(|input| input.sections.iter().copied())
        .collect::<Vec<_>>();
    let exact_section_names = section_names
        .iter()
        .copied()
        .filter(|name| *name != "pg_settings")
        .collect::<Vec<_>>();
    let cursors = BTreeMap::new();
    let current_descriptor = resolved
        .current_descriptor
        .ok_or(ProjectionError::MissingCatalogView)?;
    let needs_predecessor = preset_requires_predecessor(request, catalog)?;
    let rate_previous = neighbors.previous.filter(|previous| {
        needs_predecessor
            && neighbors
                .current
                .checked_sub(*previous)
                .zip(request.view.max_rate_gap_us)
                .is_some_and(|(gap, maximum)| gap > 0 && gap <= maximum)
    });
    let rate_gap = needs_predecessor
        .then_some(neighbors.previous)
        .flatten()
        .filter(|previous| Some(*previous) != rate_previous)
        .map(|previous| Gap {
            from: previous,
            to: neighbors.current,
        });
    let query_limits = QueryLimits::with_bytes(limits.rows, limits.cells, limits.bytes);
    let mut query = SealedQuerySession::new(snapshot, query_limits);
    let mut current = query.sections(
        &current_descriptor,
        neighbors.current,
        neighbors.current,
        &exact_section_names,
        &cursors,
    )?;
    reject_internal_continuation(&current, limits.rows)?;
    if section_names.contains(&"pg_settings") {
        let settings = query.sections(
            &current_descriptor,
            current_descriptor.min_ts,
            neighbors.current,
            &["pg_settings"],
            &cursors,
        )?;
        reject_internal_continuation(&settings, limits.rows)?;
        current.extend(settings);
    }
    let previous = match (rate_previous, resolved.previous_descriptor.as_ref()) {
        (Some(previous), Some(previous_descriptor)) => {
            let pages = query.sections(
                previous_descriptor,
                previous,
                previous,
                &exact_section_names,
                &cursors,
            )?;
            reject_internal_continuation(&pages, limits.rows)?;
            pages
        }
        _ => BTreeMap::new(),
    };
    let mut input = ProjectionInput::from_pages(
        neighbors.current,
        rate_previous,
        neighbors,
        current,
        previous,
    );
    if let Some(gap) = rate_gap {
        input.push_gap(gap);
    }
    Ok(input)
}

fn reject_internal_continuation(
    pages: &BTreeMap<String, SectionPage>,
    max_rows: usize,
) -> Result<(), QueryError> {
    if pages.values().any(|page| page.next_cursor.is_some()) {
        return Err(QueryError::RowsTooLarge { max_rows });
    }
    Ok(())
}

fn preset_requires_predecessor(
    request: &FrameRequest,
    catalog: &ProjectionCatalog,
) -> Result<bool, ProjectionError> {
    let view = catalog
        .views()
        .iter()
        .find(|view| view.code == request.view.name)
        .ok_or(ProjectionError::MissingCatalogView)?;
    let preset = view
        .presets
        .iter()
        .find(|preset| preset.code == request.preset)
        .ok_or(ProjectionError::MissingPreset)?;
    Ok(preset.columns.iter().any(|column| {
        matches!(
            (request.view.name, *column),
            ("activity", "cpu")
                | (
                    "statements",
                    "calls"
                        | "total"
                        | "ms_per_row"
                        | "mean"
                        | "time_pct"
                        | "plan_time_pct"
                        | "rows"
                        | "hit_pct"
                        | "blks_read"
                        | "temp_written"
                        | "wal_bytes"
                )
                | ("plans", "calls" | "mean" | "rows")
                | ("tables", "seq_scan_pct")
                | ("indexes", "scans" | "rows_per_scan")
                | (
                    "processes",
                    "cpu" | "read_bytes_per_second" | "write_bytes_per_second" | "block_delay"
                )
        )
    }))
}

struct ResolvedSnapshot {
    neighbors: Option<SnapshotNeighbors>,
    current_descriptor: Option<kronika_reader::SegmentDescriptor>,
    previous_descriptor: Option<kronika_reader::SegmentDescriptor>,
    next: Option<i64>,
    gated: Vec<String>,
    unavailable_revision: Vec<String>,
    resource_limited: Vec<String>,
}

fn resolve_snapshot(
    snapshot: &LocalDirSnapshot,
    request: &FrameRequest,
) -> Result<ResolvedSnapshot, FrameError> {
    let mut snapshots = BTreeMap::new();
    let mut gated = Vec::new();
    let mut unavailable_revision = Vec::new();
    let mut resource_limited = Vec::new();
    let mut descriptors = snapshot.sealed_descriptors().collect::<Vec<_>>();
    descriptors
        .sort_by_key(|descriptor| (descriptor.max_ts, descriptor.min_ts, descriptor.locator));
    for descriptor in descriptors {
        let (summary, _stats) = snapshot.read_ui_summary(&descriptor, &LIMIT)?;
        let Some(view) = summary
            .views()
            .iter()
            .find(|view| view.view_code() == request.view.code)
        else {
            continue;
        };
        if view.view_revision() != request.view.revision {
            unavailable_revision.push(request.view.name.to_owned());
            continue;
        }
        match view.status() {
            kronika_reader::IndexStatus::Gated => gated.push(request.view.name.to_owned()),
            kronika_reader::IndexStatus::UnsupportedType => {
                unavailable_revision.push(request.view.name.to_owned());
            }
            kronika_reader::IndexStatus::ResourceLimited => {
                resource_limited.push(request.view.name.to_owned());
            }
            kronika_reader::IndexStatus::Complete | kronika_reader::IndexStatus::Empty => {}
        }
        for (index, timestamp) in summary.snapshot_times().iter().copied().enumerate() {
            if view
                .snapshot_presence()
                .get(index / 8)
                .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
            {
                snapshots.insert(timestamp, descriptor);
            }
        }
    }
    let all_times = snapshots.keys().copied().collect::<Vec<_>>();
    gated.sort();
    gated.dedup();
    unavailable_revision.sort();
    unavailable_revision.dedup();
    resource_limited.sort();
    resource_limited.dedup();

    let upper = all_times.partition_point(|timestamp| *timestamp <= request.at_us);
    let current = upper
        .checked_sub(1)
        .and_then(|index| all_times.get(index))
        .copied();
    let next = all_times.get(upper).copied();
    let neighbors = current.map(|current| SnapshotNeighbors {
        previous: upper
            .checked_sub(2)
            .and_then(|index| all_times.get(index))
            .copied(),
        current,
        next,
    });
    let current_descriptor = current.and_then(|timestamp| snapshots.get(&timestamp).copied());
    let previous_descriptor = neighbors
        .and_then(|neighbors| neighbors.previous)
        .and_then(|timestamp| snapshots.get(&timestamp).copied());
    Ok(ResolvedSnapshot {
        neighbors,
        current_descriptor,
        previous_descriptor,
        next,
        gated,
        unavailable_revision,
        resource_limited,
    })
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "snapshot-wide denominator and classification ordering remain explicit in one pipeline"
)]
pub(crate) fn project_input(
    request: &FrameRequest,
    catalog: &ProjectionCatalog,
    input: ProjectionInput,
) -> Result<ProjectedFrame, ProjectionError> {
    let view_spec = catalog
        .views()
        .iter()
        .find(|view| view.code == request.view.name)
        .ok_or(ProjectionError::MissingCatalogView)?;
    let preset = view_spec
        .presets
        .iter()
        .find(|preset| preset.code == request.preset)
        .ok_or(ProjectionError::MissingPreset)?;
    let columns = preset
        .columns
        .iter()
        .filter_map(|code| {
            view_spec
                .columns
                .iter()
                .find(|column| column.code == *code && !column.lazy)
        })
        .collect::<Vec<_>>();
    let primary_sections = request.view.inputs[0].sections;
    let mut previous = BTreeMap::new();
    for section in primary_sections {
        for row in input.previous.get(*section).into_iter().flatten() {
            if timestamp(row, "ts")? != input.predecessor_ts_us {
                continue;
            }
            previous.insert(entity_for(request.view, section, row)?, row);
        }
    }

    let mut track_planning_value = None;
    for row in input.current.get("pg_settings").into_iter().flatten() {
        if text(row, "name") != Some("pg_stat_statements.track_planning") {
            continue;
        }
        let Some(timestamp) = timestamp(row, "ts")? else {
            continue;
        };
        if timestamp <= input.snapshot_ts_us
            && track_planning_value.is_none_or(|(selected, _)| timestamp > selected)
        {
            track_planning_value = text(row, "setting").map(|setting| (timestamp, setting));
        }
    }
    let track_planning = track_planning_value.is_some_and(|(_timestamp, setting)| setting == "on");
    let has_gap = !input.gaps.is_empty();
    let mut rows = Vec::new();
    for (section_index, section) in primary_sections.iter().enumerate() {
        for (row_index, row) in input
            .current
            .get(*section)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if timestamp(row, "ts")? != Some(input.snapshot_ts_us) {
                continue;
            }
            let entity = entity_for(request.view, section, row)?;
            let predecessor = previous.get(&entity).copied();
            let mut projected = project_view(
                request.view,
                view_spec,
                &columns,
                section,
                row,
                predecessor,
                &input,
                track_planning,
                has_gap,
            )?;
            if request.view.name == "events" {
                projected.entity = event_frame_entity(section_index, row_index)?;
            }
            rows.push(projected);
        }
    }

    if request.view.name == "statements" {
        let denominator = rows
            .iter()
            .filter(|row| {
                request
                    .database
                    .as_ref()
                    .is_none_or(|database| row.database.as_ref() == Some(database))
            })
            .try_fold(0.0, |sum, row| {
                let operands = row.operands.statements.as_ref()?;
                match operands.exec_ms {
                    DeltaOperand::Value(value) => Some(sum + value),
                    DeltaOperand::Missing | DeltaOperand::Reset | DeltaOperand::Gap => None,
                }
            })
            .filter(|denominator| *denominator > 0.0);
        for row in &mut rows {
            if let Some(operands) = &mut row.operands.statements {
                operands.snapshot_exec_ms_delta_sum = denominator;
                let value = match (operands.exec_ms, denominator) {
                    (DeltaOperand::Value(value), Some(denominator)) => {
                        finite_frame(100.0 * value / denominator)
                    }
                    _ => FrameValue::Null,
                };
                replace_value(row, "time_pct", value);
            }
        }
    }
    let threshold_context = FrameThresholdContext;
    for row in &mut rows {
        row.searchable = public_searchable(&row.label, &row.values);
        row.classifications = classify_row(request.view.name, &columns, row, &threshold_context);
    }

    let paged = filter_sort_page(request, input.snapshot_ts_us, rows)?;
    Ok(ProjectedFrame {
        snapshot_ts_us: input.snapshot_ts_us,
        predecessor_ts_us: input.predecessor_ts_us,
        neighbors: input.neighbors,
        rows: paged.rows,
        quality: FrameQuality {
            snapshots: 1 + usize::from(input.predecessor_ts_us.is_some()),
            gaps: input.gaps,
            ..FrameQuality::default()
        },
        matched: paged.matched,
        next: paged.next,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the explicit projection evaluator receives one bounded row context"
)]
fn project_view(
    view: &'static WebView,
    view_spec: &ViewSpec,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    previous: Option<&OutRow>,
    input: &ProjectionInput,
    track_planning: bool,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    match view.name {
        "activity" => project_activity(view, columns, section, row, previous, input, has_gap),
        "statements" => project_statements(
            view,
            columns,
            section,
            row,
            previous,
            input,
            track_planning,
            has_gap,
        ),
        "plans" => project_plans(view, columns, section, row, previous, input, has_gap),
        "tables" => project_tables(view, columns, section, row, previous, input, has_gap),
        "indexes" => project_indexes(view, columns, section, row, previous, input, has_gap),
        "vacuum" => project_vacuum(view, columns, section, row, input),
        "processes" => project_processes(view, columns, section, row, previous, input, has_gap),
        "locks" => project_locks(view, columns, section, row, input),
        "events" => project_events(view, columns, section, row, input),
        _ => Err(ProjectionError::MissingCatalogView),
    }
    .map(|mut projected| {
        debug_assert_eq!(
            projected.values.len(),
            columns.len(),
            "an explicit evaluator must produce every selected non-lazy column"
        );
        projected.cells = projected
            .values
            .iter()
            .map(|(_, value)| value.clone())
            .collect();
        if projected.label.is_empty() {
            projected.label = fallback_label(&projected.entity);
        }
        let _ = view_spec;
        projected
    })
}

fn row_shell(
    view: &WebView,
    section: &str,
    row: &OutRow,
    snapshot_ts_us: i64,
) -> Result<ProjectedRow, ProjectionError> {
    let entity = entity_for(view, section, row)?;
    Ok(ProjectedRow {
        label: label_for(view.name, row).unwrap_or_else(|| fallback_label(&entity)),
        spark_entity: entity.clone(),
        entity,
        cells: Vec::new(),
        operands: RowOperands {
            snapshot_ts_us,
            ..RowOperands::default()
        },
        classifications: Vec::new(),
        spark: SparkDto {
            values: Vec::new(),
            complete: false,
        },
        values: Vec::new(),
        database: text(row, "datname").map(str::to_owned),
        searchable: String::new(),
    })
}

fn project_activity(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    _previous: Option<&OutRow>,
    input: &ProjectionInput,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    out.operands.activity_state = text(row, "state").map(str::to_owned);
    out.operands.query_start_us = timestamp(row, "query_start")?;
    out.operands.transaction_start_us = timestamp(row, "xact_start")?;
    for column in columns {
        let value = match column.code {
            "pid" => raw_frame(row, "pid", column.value_type),
            "user" => raw_frame(row, "usename", column.value_type),
            "database" => raw_frame(row, "datname", column.value_type),
            "application" => raw_frame(row, "application_name", column.value_type),
            "state" => raw_frame(row, "state", column.value_type),
            "wait_event" => joined(row, "wait_event_type", ":", "wait_event"),
            "query_duration_us" => duration_us(input.snapshot_ts_us, row, "query_start")?,
            "transaction_duration_us" => duration_us(input.snapshot_ts_us, row, "xact_start")?,
            "cpu" => {
                let current_process = activity_process(row, &input.current, input.snapshot_ts_us);
                let previous_process = input
                    .predecessor_ts_us
                    .and_then(|timestamp| activity_process(row, &input.previous, timestamp));
                current_process.map_or(FrameValue::Null, |current| {
                    rate_sum(
                        current,
                        previous_process,
                        &["utime", "stime"],
                        input,
                        has_gap,
                    )
                })
            }
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn activity_process<'a>(
    activity: &OutRow,
    sections: &'a BTreeMap<String, Vec<OutRow>>,
    timestamp_us: i64,
) -> Option<&'a OutRow> {
    let pid = finite_number(activity, "pid").ok().flatten()?;
    let backend_start = timestamp(activity, "backend_start").ok().flatten()?;
    sections.get("os_process")?.iter().find(|process| {
        timestamp(process, "ts").ok().flatten() == Some(timestamp_us)
            && finite_number(process, "pid").ok().flatten() == Some(pid)
            && timestamp(process, "starttime").ok().flatten() == Some(backend_start)
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "statement projection keeps denominator and planning provenance explicit"
)]
fn project_statements(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    previous: Option<&OutRow>,
    input: &ProjectionInput,
    track_planning: bool,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    let calls = delta(row, previous, "calls", has_gap);
    let rows = delta(row, previous, "rows", has_gap);
    let exec = delta(row, previous, "total_exec_time", has_gap);
    let planning_fields = value(row, "total_plan_time").is_some_and(|value| *value != Value::Null);
    let plan = if planning_fields {
        delta(row, previous, "total_plan_time", has_gap)
    } else {
        DeltaOperand::Missing
    };
    out.operands.statements = Some(StatementOperands {
        calls,
        rows,
        exec_ms: exec,
        plan_ms: plan,
        planning_fields,
        track_planning,
        total_exec_ms: finite_number(row, "total_exec_time")?,
        snapshot_exec_ms_delta_sum: None,
    });
    for column in columns {
        let value = match column.code {
            "queryid" => raw_frame(row, "queryid", column.value_type),
            "calls" => delta_frame(calls),
            "total" => delta_frame(exec),
            "ms_per_row" => divide_delta(exec, rows),
            "mean" => divide_delta(exec, calls),
            "plan_time_pct" if track_planning && planning_fields => percent_of_sum(plan, exec),
            "rows" => delta_frame(rows),
            "hit_pct" => ratio_delta(
                delta(row, previous, "shared_blks_hit", has_gap),
                delta_sum(
                    row,
                    previous,
                    &["shared_blks_hit", "shared_blks_read"],
                    has_gap,
                ),
                100.0,
            ),
            "blks_read" => delta_sum_frame(
                row,
                previous,
                &["shared_blks_read", "local_blks_read"],
                has_gap,
            ),
            "temp_written" => delta_frame(delta(row, previous, "temp_blks_written", has_gap)),
            "wal_bytes" => delta_frame(delta(row, previous, "wal_bytes", has_gap)),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn project_plans(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    previous: Option<&OutRow>,
    input: &ProjectionInput,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    let calls = delta(row, previous, "calls", has_gap);
    let total = delta(row, previous, "total_time", has_gap);
    let rows = delta(row, previous, "rows", has_gap);
    for column in columns {
        let value = match column.code {
            "planid" => raw_frame(row, "planid", column.value_type),
            "queryid" => raw_frame(row, "queryid", column.value_type),
            "calls" => delta_frame(calls),
            "mean" => divide_delta(total, calls),
            "rows" => delta_frame(rows),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn project_tables(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    previous: Option<&OutRow>,
    input: &ProjectionInput,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    let live = finite_number(row, "n_live_tup")?;
    let dead = finite_number(row, "n_dead_tup")?;
    let seq = delta(row, previous, "seq_scan", has_gap);
    let idx = delta(row, previous, "idx_scan", has_gap);
    out.operands.table = Some(TableOperands {
        live,
        dead,
        seq_scan: seq,
        idx_scan: idx,
        modified: finite_number(row, "n_mod_since_analyze")?,
        inserted: value(row, "n_ins_since_vacuum")
            .map(|_| finite_number(row, "n_ins_since_vacuum"))
            .transpose()?,
        last_autovacuum_us: timestamp(row, "last_autovacuum")?,
        last_autoanalyze_us: timestamp(row, "last_autoanalyze")?,
    });
    for column in columns {
        let value = match column.code {
            "relation" => joined(row, "schemaname", ".", "relname"),
            "seq_scan" => raw_frame(row, "seq_scan", column.value_type),
            "idx_scan" => raw_frame(row, "idx_scan", column.value_type),
            "dead_pct" => ratio(dead, live, 100.0),
            "dead_tuples" => raw_frame(row, "n_dead_tup", column.value_type),
            "seq_scan_pct" => percent_of_sum(seq, idx),
            "modified_since_analyze" => raw_frame(row, "n_mod_since_analyze", column.value_type),
            "inserted_since_vacuum" => raw_frame(row, "n_ins_since_vacuum", column.value_type),
            "last_autovacuum" => raw_frame(row, "last_autovacuum", column.value_type),
            "autovacuum_age_seconds" => age_seconds(input.snapshot_ts_us, row, "last_autovacuum")?,
            "autoanalyze_age_seconds" => {
                age_seconds(input.snapshot_ts_us, row, "last_autoanalyze")?
            }
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn project_indexes(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    previous: Option<&OutRow>,
    input: &ProjectionInput,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    let scans = delta(row, previous, "idx_scan", has_gap);
    let read = delta(row, previous, "idx_tup_read", has_gap);
    for column in columns {
        let value = match column.code {
            "index" => raw_frame(row, "indexrelname", column.value_type),
            "table" => raw_frame(row, "relname", column.value_type),
            "scans" => delta_frame(scans),
            "rows_per_scan" => divide_delta(read, scans),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn project_vacuum(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    input: &ProjectionInput,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    for column in columns {
        let value = match column.code {
            "pid" => raw_frame(row, "pid", column.value_type),
            "table" => raw_frame(row, "relid", column.value_type),
            "phase" => raw_frame(row, "phase", column.value_type),
            "is_autovacuum" => raw_frame(row, "is_autovacuum", column.value_type),
            "progress" => ratio(
                finite_number(row, "heap_blks_scanned")?,
                finite_number(row, "heap_blks_total")?,
                1.0,
            ),
            "dead_tuples" => finite_number(row, "num_dead_tuples")?
                .or(finite_number(row, "num_dead_item_ids")?)
                .map_or(FrameValue::Null, finite_frame),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn project_processes(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    previous: Option<&OutRow>,
    input: &ProjectionInput,
    has_gap: bool,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    out.operands.process_rss_kib = finite_number(row, "rmem_kb")?;
    for column in columns {
        let value = match column.code {
            "pid" => raw_frame(row, "pid", column.value_type),
            "type" => raw_frame(row, "comm", column.value_type),
            "cpu" => rate_sum(row, previous, &["utime", "stime"], input, has_gap),
            "rss" => raw_frame(row, "rmem_kb", column.value_type),
            "read_bytes_per_second" => rate_sum(row, previous, &["read_bytes"], input, has_gap),
            "write_bytes_per_second" => rate_sum(row, previous, &["write_bytes"], input, has_gap),
            "block_delay" => rate_sum(row, previous, &["blkdelay_ticks"], input, has_gap),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded timestamp differences become the projection's documented f64 duration"
)]
fn project_locks(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    input: &ProjectionInput,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    for column in columns {
        let value = match column.code {
            "pid" => raw_frame(row, "pid", column.value_type),
            "user_application" => joined(row, "usename", " / ", "application_name"),
            "lock" => joined(row, "wait_event_type", ":", "wait_event"),
            "target" => raw_frame(row, "lock_relname", column.value_type),
            "wait_or_hold_us" => ["waitstart", "xact_start", "query_start"]
                .iter()
                .find_map(|name| timestamp(row, name).ok().flatten())
                .map_or(FrameValue::Null, |start| {
                    finite_frame((input.snapshot_ts_us - start) as f64)
                }),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn project_events(
    view: &WebView,
    columns: &[&ColumnSpec],
    section: &str,
    row: &OutRow,
    input: &ProjectionInput,
) -> Result<ProjectedRow, ProjectionError> {
    let mut out = row_shell(view, section, row, input.snapshot_ts_us)?;
    out.label = event_kind_code(section, row);
    for column in columns {
        let value = match column.code {
            "time" => raw_frame(row, "ts", column.value_type),
            "severity" => raw_frame(row, "severity", column.value_type),
            "type" => event_type(row, section),
            "duration" => event_duration(row),
            _ => FrameValue::Null,
        };
        out.values.push((column.code, value));
    }
    Ok(out)
}

fn replace_value(row: &mut ProjectedRow, code: &str, replacement: FrameValue) {
    if let Some((_, value)) = row.values.iter_mut().find(|(column, _)| *column == code) {
        *value = replacement;
        row.cells = row.values.iter().map(|(_, value)| value.clone()).collect();
    }
}

fn entity_for(view: &WebView, section: &str, row: &OutRow) -> Result<Vec<u8>, ProjectionError> {
    if view.name == "events" {
        let category = event_kind_code(section, row);
        let len = u16::try_from(category.len())
            .map_err(|_error| ProjectionError::InvalidValue("event"))?;
        let mut entity = Vec::with_capacity(4 + category.len());
        entity.extend_from_slice(&view.identity_revision.to_le_bytes());
        entity.extend_from_slice(&len.to_le_bytes());
        entity.extend_from_slice(category.as_bytes());
        return Ok(entity);
    }
    let logical = logical_section(section).ok_or(ProjectionError::MissingCatalogView)?;
    let special: &[&str] = match view.name {
        "activity" | "locks" => &["pid", "backend_start"],
        "processes" => &["pid", "starttime"],
        "vacuum" => &["pid", "datid", "relid"],
        _ => &[],
    };
    let fallback;
    let fields = if !special.is_empty() {
        special
    } else if !logical.identity.is_empty() {
        &logical.identity
    } else {
        fallback = logical.diff_key();
        &fallback
    };
    let mut entity = Vec::new();
    entity.extend_from_slice(&view.identity_revision.to_le_bytes());
    for field in fields {
        let ty = logical
            .columns
            .iter()
            .find(|column| column.name == *field)
            .map(|column| column.ty)
            .ok_or(ProjectionError::InvalidValue("identity"))?;
        encode_value(&mut entity, value(row, field).unwrap_or(&Value::Null), ty)?;
    }
    Ok(entity)
}

fn event_frame_entity(section_index: usize, row_index: usize) -> Result<Vec<u8>, ProjectionError> {
    let section_index =
        u16::try_from(section_index).map_err(|_error| ProjectionError::InvalidValue("event"))?;
    let row_index =
        u32::try_from(row_index).map_err(|_error| ProjectionError::InvalidValue("event"))?;
    let mut entity = Vec::with_capacity(6);
    entity.extend_from_slice(&section_index.to_le_bytes());
    entity.extend_from_slice(&row_index.to_le_bytes());
    Ok(entity)
}

fn encode_value(
    output: &mut Vec<u8>,
    value: &Value,
    ty: ColumnType,
) -> Result<(), ProjectionError> {
    match (value, ty) {
        (Value::Null, _) => output.push(0),
        (Value::I64(value), ColumnType::I8 | ColumnType::I16) => {
            let value = i16::try_from(*value)
                .map_err(|_error| ProjectionError::InvalidValue("identity"))?;
            output.push(1);
            output.extend_from_slice(&value.to_le_bytes());
        }
        (Value::I64(value), ColumnType::I32) => {
            let value = i32::try_from(*value)
                .map_err(|_error| ProjectionError::InvalidValue("identity"))?;
            output.push(2);
            output.extend_from_slice(&value.to_le_bytes());
        }
        (Value::I64(value), ColumnType::I64) => {
            output.push(3);
            output.extend_from_slice(&value.to_le_bytes());
        }
        (Value::U64(value), ColumnType::U8 | ColumnType::U16 | ColumnType::U32) => {
            let value = u32::try_from(*value)
                .map_err(|_error| ProjectionError::InvalidValue("identity"))?;
            output.push(4);
            output.extend_from_slice(&value.to_le_bytes());
        }
        (Value::U64(value), ColumnType::U64) => {
            output.push(5);
            output.extend_from_slice(&value.to_le_bytes());
        }
        (Value::F64(value), ColumnType::F32 | ColumnType::F64) if value.is_finite() => {
            output.push(6);
            output.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        (Value::Bool(value), ColumnType::Bool) => {
            output.push(7);
            output.push(u8::from(*value));
        }
        (Value::Ts(value), ColumnType::Ts) => {
            output.push(8);
            output.extend_from_slice(&value.to_le_bytes());
        }
        (Value::ListI32(values), ColumnType::ListI32) => {
            output.push(10);
            let len = u32::try_from(values.len())
                .map_err(|_error| ProjectionError::InvalidValue("identity"))?;
            output.extend_from_slice(&len.to_le_bytes());
            for value in values {
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        _ => return Err(ProjectionError::InvalidValue("identity")),
    }
    Ok(())
}

fn label_for(view: &str, row: &OutRow) -> Option<String> {
    let fields: &[&str] = match view {
        "activity" => &["pid", "application_name"],
        "statements" => &["queryid"],
        "plans" => &["planid"],
        "tables" => &["schemaname", "relname"],
        "indexes" => &["schemaname", "indexrelname"],
        "vacuum" => &["pid", "relid"],
        "processes" => &["comm", "pid"],
        "locks" => &["pid", "lock_relname"],
        _ => return None,
    };
    let parts = fields
        .iter()
        .filter_map(|field| display(value(row, field)?))
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" / "))
}

fn fallback_label(entity: &[u8]) -> String {
    entity
        .iter()
        .take(32)
        .fold(String::new(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

fn public_searchable(label: &str, values: &[(&str, FrameValue)]) -> String {
    let mut searchable = label.to_owned();
    for (_, value) in values {
        searchable.push('\0');
        match value {
            FrameValue::Null => {}
            FrameValue::Number(value) => {
                write!(searchable, "{value}").expect("writing to String cannot fail");
            }
            FrameValue::Boolean(value) => {
                searchable.push_str(if *value { "true" } else { "false" });
            }
            FrameValue::String(value) => searchable.push_str(value),
        }
    }
    searchable
}

fn value<'a>(row: &'a OutRow, name: &str) -> Option<&'a Value> {
    row.iter()
        .find(|(column, _)| column == name)
        .map(|(_, value)| value)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "projection formulas and threshold MetricInput use finite f64 operands"
)]
pub(crate) fn finite_number(
    row: &OutRow,
    name: &'static str,
) -> Result<Option<f64>, ProjectionError> {
    match value(row, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::I64(value)) => Ok(Some(*value as f64)),
        Some(Value::U64(value)) => Ok(Some(*value as f64)),
        Some(Value::F64(value)) if value.is_finite() => Ok(Some(*value)),
        _ => Err(ProjectionError::InvalidValue(name)),
    }
}

pub(crate) fn timestamp(row: &OutRow, name: &'static str) -> Result<Option<i64>, ProjectionError> {
    match value(row, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Ts(value)) => Ok(Some(*value)),
        _ => Err(ProjectionError::InvalidValue(name)),
    }
}

pub(crate) fn text<'a>(row: &'a OutRow, name: &'static str) -> Option<&'a str> {
    match value(row, name) {
        Some(Value::Str(value) | Value::Blob { text: value, .. }) => Some(value),
        _ => None,
    }
}

fn raw_frame(row: &OutRow, name: &'static str, ty: ValueType) -> FrameValue {
    value(row, name).map_or(FrameValue::Null, |value| frame_value(value, ty))
}

#[allow(
    clippy::cast_precision_loss,
    clippy::match_same_arms,
    reason = "integers are converted only after an exact-JavaScript-range check"
)]
fn frame_value(value: &Value, ty: ValueType) -> FrameValue {
    match value {
        Value::Null => FrameValue::Null,
        Value::I64(value) if value.unsigned_abs() <= JS_EXACT_INTEGER => {
            FrameValue::Number(*value as f64)
        }
        Value::I64(value) | Value::Ts(value) => FrameValue::String(value.to_string()),
        Value::U64(value) if *value <= JS_EXACT_INTEGER => FrameValue::Number(*value as f64),
        Value::U64(value) => FrameValue::String(value.to_string()),
        Value::F64(value) if value.is_finite() => FrameValue::Number(*value),
        Value::Bool(value) => FrameValue::Boolean(*value),
        Value::Str(value) => FrameValue::String(value.clone()),
        Value::Blob { text, .. } => FrameValue::String(text.clone()),
        Value::ListI32(values) => FrameValue::String(
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
        Value::F64(_) => FrameValue::Null,
    }
    .coerce(ty)
}

trait CoerceFrame {
    fn coerce(self, ty: ValueType) -> Self;
}

impl CoerceFrame for FrameValue {
    fn coerce(self, ty: ValueType) -> Self {
        match (self, ty) {
            (Self::Number(value), ValueType::Text) => Self::String(value.to_string()),
            (value, _) => value,
        }
    }
}

#[allow(
    clippy::match_same_arms,
    reason = "NULL and a non-finite stored float both have no display value"
)]
fn display(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::I64(value) | Value::Ts(value) => Some(value.to_string()),
        Value::U64(value) => Some(value.to_string()),
        Value::F64(value) if value.is_finite() => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Str(value) => Some(value.clone()),
        Value::Blob { text, .. } => Some(text.clone()),
        Value::ListI32(values) => Some(
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
        Value::F64(_) => None,
    }
}

fn joined(row: &OutRow, left: &'static str, separator: &str, right: &'static str) -> FrameValue {
    match (
        display(value(row, left).unwrap_or(&Value::Null)),
        display(value(row, right).unwrap_or(&Value::Null)),
    ) {
        (Some(left), Some(right)) => FrameValue::String(format!("{left}{separator}{right}")),
        (Some(value), None) | (None, Some(value)) => FrameValue::String(value),
        (None, None) => FrameValue::Null,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded timestamp differences become the projection's documented f64 duration"
)]
fn duration_us(now: i64, row: &OutRow, field: &'static str) -> Result<FrameValue, ProjectionError> {
    Ok(timestamp(row, field)?.map_or(FrameValue::Null, |start| finite_frame((now - start) as f64)))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded timestamp differences become the projection's documented f64 age"
)]
fn age_seconds(now: i64, row: &OutRow, field: &'static str) -> Result<FrameValue, ProjectionError> {
    Ok(timestamp(row, field)?.map_or(FrameValue::Null, |start| {
        finite_frame((now - start) as f64 / 1_000_000.0)
    }))
}

fn delta(
    current: &OutRow,
    previous: Option<&OutRow>,
    field: &'static str,
    has_gap: bool,
) -> DeltaOperand {
    if has_gap {
        return DeltaOperand::Gap;
    }
    let (Ok(Some(current)), Some(previous)) = (finite_number(current, field), previous) else {
        return DeltaOperand::Missing;
    };
    let Ok(Some(previous)) = finite_number(previous, field) else {
        return DeltaOperand::Missing;
    };
    if current < previous {
        DeltaOperand::Reset
    } else {
        DeltaOperand::Value(current - previous)
    }
}

fn delta_sum(
    current: &OutRow,
    previous: Option<&OutRow>,
    fields: &[&'static str],
    has_gap: bool,
) -> DeltaOperand {
    fields.iter().fold(DeltaOperand::Value(0.0), |sum, field| {
        match (sum, delta(current, previous, field, has_gap)) {
            (DeltaOperand::Value(left), DeltaOperand::Value(right)) => {
                DeltaOperand::Value(left + right)
            }
            (_, DeltaOperand::Reset) => DeltaOperand::Reset,
            (_, DeltaOperand::Gap) => DeltaOperand::Gap,
            _ => DeltaOperand::Missing,
        }
    })
}

const fn delta_frame(delta: DeltaOperand) -> FrameValue {
    match delta {
        DeltaOperand::Value(value) => finite_frame(value),
        _ => FrameValue::Null,
    }
}

fn delta_sum_frame(
    current: &OutRow,
    previous: Option<&OutRow>,
    fields: &[&'static str],
    has_gap: bool,
) -> FrameValue {
    delta_frame(delta_sum(current, previous, fields, has_gap))
}

fn divide_delta(numerator: DeltaOperand, denominator: DeltaOperand) -> FrameValue {
    match (numerator, denominator) {
        (DeltaOperand::Value(numerator), DeltaOperand::Value(denominator)) if denominator > 0.0 => {
            finite_frame(numerator / denominator)
        }
        _ => FrameValue::Null,
    }
}

fn ratio_delta(numerator: DeltaOperand, denominator: DeltaOperand, scale: f64) -> FrameValue {
    match (numerator, denominator) {
        (DeltaOperand::Value(numerator), DeltaOperand::Value(denominator)) if denominator > 0.0 => {
            finite_frame(scale * numerator / denominator)
        }
        _ => FrameValue::Null,
    }
}

fn percent_of_sum(left: DeltaOperand, right: DeltaOperand) -> FrameValue {
    match (left, right) {
        (DeltaOperand::Value(left), DeltaOperand::Value(right)) if left + right > 0.0 => {
            finite_frame(100.0 * left / (left + right))
        }
        _ => FrameValue::Null,
    }
}

fn ratio(numerator: Option<f64>, other: Option<f64>, scale: f64) -> FrameValue {
    match (numerator, other) {
        (Some(numerator), Some(other)) if numerator + other > 0.0 => {
            finite_frame(scale * numerator / (numerator + other))
        }
        _ => FrameValue::Null,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded microsecond elapsed time becomes the projection's f64 rate denominator"
)]
fn rate_sum(
    current: &OutRow,
    previous: Option<&OutRow>,
    fields: &[&'static str],
    input: &ProjectionInput,
    has_gap: bool,
) -> FrameValue {
    let Some(previous_ts) = input.predecessor_ts_us else {
        return FrameValue::Null;
    };
    let elapsed = input.snapshot_ts_us - previous_ts;
    if elapsed <= 0 {
        return FrameValue::Null;
    }
    match delta_sum(current, previous, fields, has_gap) {
        DeltaOperand::Value(value) => finite_frame(value / (elapsed as f64 / 1_000_000.0)),
        _ => FrameValue::Null,
    }
}

const fn finite_frame(value: f64) -> FrameValue {
    if value.is_finite() {
        FrameValue::Number(value)
    } else {
        FrameValue::Null
    }
}

fn event_type(row: &OutRow, section: &str) -> FrameValue {
    value(row, "category")
        .or_else(|| value(row, "kind"))
        .or_else(|| value(row, "phase"))
        .map_or_else(
            || FrameValue::String(section.to_owned()),
            |value| frame_value(value, ValueType::U64),
        )
}

fn event_duration(row: &OutRow) -> FrameValue {
    for field in ["duration_us", "total_ms", "elapsed_ms", "duration_ms"] {
        if let Ok(Some(value)) = finite_number(row, field) {
            let micros = if field.ends_with("_ms") {
                value * 1_000.0
            } else {
                value
            };
            return finite_frame(micros);
        }
    }
    FrameValue::Null
}

fn event_kind_code(section: &str, row: &OutRow) -> String {
    let discriminator = |name| unsigned_integer(row, name);
    match section {
        "pg_log_errors" => "pg.log.error_group_observed",
        "pg_log_checkpoints" => match discriminator("phase") {
            Some(0) => "pg.checkpoint.started",
            Some(1) => "pg.checkpoint.completed",
            _ => "pg.checkpoint.too_frequent_reported",
        },
        "pg_log_autovacuum" => match discriminator("kind") {
            Some(1) => "pg.maintenance.autoanalyze_reported",
            _ => "pg.maintenance.autovacuum_reported",
        },
        "pg_log_slow_queries" => "pg.query.slow_group_reported",
        "pg_log_lock_waits" => match discriminator("kind") {
            Some(1) => "pg.lock.acquired_after_wait_reported",
            _ => "pg.lock.wait_reported",
        },
        "pg_log_lifecycle" => match discriminator("kind") {
            Some(0) => "pg.lifecycle.child_signal_termination",
            Some(1) => "pg.lifecycle.child_process_crash",
            Some(2) => "pg.lifecycle.shutdown_requested",
            _ => "pg.lifecycle.ready_observed",
        },
        "pg_log_gap" => "collector.pg_log_gap",
        "pg_log_temp_files" => "pg.temp_file.reported",
        _ => section,
    }
    .to_owned()
}

fn unsigned_integer(row: &OutRow, name: &str) -> Option<u64> {
    match value(row, name) {
        Some(Value::U64(value)) => Some(*value),
        Some(Value::I64(value)) => u64::try_from(*value).ok(),
        _ => None,
    }
}

pub(crate) fn compare_frame_values(left: &FrameValue, right: &FrameValue) -> Ordering {
    match (left, right) {
        (FrameValue::Null, FrameValue::Null) => Ordering::Equal,
        (FrameValue::Null, _) => Ordering::Greater,
        (_, FrameValue::Null) => Ordering::Less,
        (FrameValue::Number(left), FrameValue::Number(right)) => left.total_cmp(right),
        (FrameValue::Boolean(left), FrameValue::Boolean(right)) => left.cmp(right),
        (FrameValue::String(left), FrameValue::String(right)) => left.cmp(right),
        (left, right) => value_sort_key(left).tag().cmp(&value_sort_key(right).tag()),
    }
}

pub(crate) fn cursor_for(
    request: &FrameRequest,
    snapshot_ts_us: i64,
    row: &ProjectedRow,
) -> Result<String, ProjectionError> {
    let value = row
        .values
        .iter()
        .find(|(column, _)| *column == request.sort)
        .map_or(&FrameValue::Null, |(_, value)| value);
    FrameCursor::new(
        request.view.code,
        request.view.revision,
        snapshot_ts_us,
        request.query_fingerprint(),
        value_sort_key(value),
        row.entity.clone(),
    )
    .and_then(|cursor| cursor.encode())
    .map_err(|_error| ProjectionError::Cursor)
}

pub(crate) fn selected_columns<'a>(
    request: &FrameRequest,
    catalog: &'a ProjectionCatalog,
) -> Result<Vec<&'a ColumnSpec>, ProjectionError> {
    let view = catalog
        .views()
        .iter()
        .find(|view| view.code == request.view.name)
        .ok_or(ProjectionError::MissingCatalogView)?;
    let preset = view
        .presets
        .iter()
        .find(|preset| preset.code == request.preset)
        .ok_or(ProjectionError::MissingPreset)?;
    let selected = preset.columns.iter().copied().collect::<BTreeSet<_>>();
    Ok(view
        .columns
        .iter()
        .filter(|column| selected.contains(column.code) && !column.lazy)
        .collect())
}
