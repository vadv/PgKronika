//! Store-reader adapter for owned incident-engine input.

use std::collections::{BTreeMap, BTreeSet};

use kronika_reader::{
    DiffPoint, Gap, LocalDirSnapshot, LogicalSection, OutRow, QueryError, QueryLimits, Reason,
    SectionPage, SeriesDiff, SeriesValues, Value, diff_section, gauge_section, logical_section,
    section_with_limits, sections_with_limits,
};
use kronika_registry::ColumnClass;

use crate::anomaly::{EpisodeHit, ScanCounts, ScanParams, scan_section};
use crate::handlers::v1::{DIFF_MAX_ROWS, Gates};
use crate::incident::{
    ActivityBackend, ActivitySnapshot, EnrichedEpisode, EpisodeRefV1, EventInputLimits,
    GaugeQuality, GaugeTrackInput, IdentityValue, LifecycleEvent, LifecycleKind, LockEdge,
    LockSnapshot, LogCoverage, LogErrorGroup, LogEventInputs, Series, SeriesError,
    SeriesInsertError, SeriesSet, SnapshotCompleteness, TypedInputs,
};

/// Sections whose raw snapshots the sampled activity and lock lenses read.
const PG_STAT_ACTIVITY: &str = "pg_stat_activity";
const PG_LOCKS: &str = "pg_locks";
const SNAPSHOT_COVERAGE: &str = "snapshot_coverage";

/// Data exclusions recorded while building engine input.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputQuality {
    /// Rows or episodes dropped because identity was not a canonical scalar
    /// (`Null`, float, timestamp, blob, or list).
    pub non_canonical_identity: u64,
    /// Series points dropped because their diff rate was non-finite.
    pub non_finite_points: u64,
    /// First cumulative samples, which have no preceding pair.
    pub first_points: u64,
    /// Counter reset pairs excluded from numeric timelines.
    pub resets: u64,
    /// Pairs excluded because their interval crosses a coverage gap.
    pub gaps: u64,
    /// Pairs excluded by collection gating.
    pub not_collected: u64,
    /// Invalid timestamp or scalar pairs excluded by the reader.
    pub anomalous_points: u64,
    /// Invalid gauge readings excluded from numeric timelines.
    pub invalid_gauge_points: u64,
    /// Equal duplicate samples removed before folding.
    pub duplicate_timestamps: u64,
    /// Window positions scored by the anomaly kernel.
    pub evaluated_positions: u64,
    /// Window positions rejected because the available data was insufficient
    /// or discontinuous.
    pub unevaluated_positions: u64,
    /// Above-threshold episodes omitted by the retained-episode ceiling.
    pub episodes_truncated: u64,
    /// Snapshot rows (activity backends or lock edges) withheld because the
    /// request snapshot-row ceiling was reached. The section's snapshots are
    /// dropped whole, so a sampled lens sees none rather than a biased subset.
    pub snapshot_rows_withheld: u64,
}

/// Request ceilings for adapter-owned work and data.
pub(crate) struct InputLimits {
    units: usize,
    sections: usize,
    materialized_cells: usize,
    materialized_bytes: usize,
    series_points: usize,
    typed_gauge_points: usize,
    identity_bytes: usize,
    positions: usize,
    score_work: usize,
    episodes: usize,
    snapshot_rows: usize,
}

impl InputLimits {
    /// Fixed ceilings for collection sizes and work units.
    ///
    /// These values do not claim a resident-memory budget.
    pub(crate) const fn production() -> Self {
        Self {
            units: 4_096,
            sections: 64,
            materialized_cells: 2_000_000,
            materialized_bytes: 32 * 1024 * 1024,
            series_points: 500_000,
            typed_gauge_points: 500_000,
            identity_bytes: 1 << 20,
            positions: 10_000,
            score_work: crate::anomaly::MAX_SCORE_WORK,
            episodes: 20_000,
            snapshot_rows: 200_000,
        }
    }

    pub(crate) const fn position_limit(&self) -> usize {
        self.positions
    }
}

#[cfg(test)]
impl InputLimits {
    fn for_test() -> Self {
        Self {
            units: 4_096,
            sections: 64,
            materialized_cells: 1_000_000,
            materialized_bytes: 32 * 1024 * 1024,
            series_points: 1_000_000,
            typed_gauge_points: 1_000_000,
            identity_bytes: 1 << 20,
            positions: 10_000,
            score_work: crate::anomaly::MAX_SCORE_WORK,
            episodes: 50,
            snapshot_rows: 200_000,
        }
    }
}

/// Why building the engine input failed before analysis could start.
#[derive(Debug)]
pub(crate) enum InputError {
    /// A requested name is not a registered logical section.
    UnknownSection(String),
    /// The period, window, or step cannot define a finite scan.
    InvalidScan,
    /// No source unit overlaps the requested period.
    NoData,
    /// Too many source units overlap the request for bounded coverage output.
    UnitLimit { observed: usize, limit: usize },
    /// The scan would materialize too many window positions.
    PositionLimit { observed: usize, limit: usize },
    /// Reading or decoding a section failed, or the registry contract was
    /// inconsistent — never masked as an absence of anomalies.
    Read(QueryError),
    /// A scanned episode named a column absent from its section's union schema.
    UnknownColumn {
        section: &'static str,
        column: String,
    },
    /// More unique sections were requested than the request ceiling allows.
    SectionLimit { observed: usize, limit: usize },
    /// The requested pages exceeded the request-wide materialization ceiling.
    MaterializationLimit { limit: usize },
    /// No resolved node id covers the requested source and interval.
    MissingNodeIdentity,
    /// The interval spans more than one node id for the same source.
    ConflictingNodeIdentity,
    /// Node identity bytes exceeded the adapter ceiling.
    IdentityByteLimit { observed: usize, limit: usize },
    /// Two adapter paths attempted to register the same series.
    DuplicateSeries {
        section: &'static str,
        column: &'static str,
    },
    /// A folded timeline violated the core series invariant.
    InvalidSeries {
        section: &'static str,
        column: &'static str,
        error: SeriesError,
    },
    /// The scanned series exceeded the engine's owned-point limit.
    SeriesLimit { observed: usize, limit: usize },
}

/// One section that could not be scanned in full and was skipped, leaving the
/// others usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionSkip {
    pub section: &'static str,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The section did not fit the remaining materialized-cell budget.
    MaterializationLimit { limit: usize },
    /// The period had more rows than one page could hold.
    IncompletePage,
    /// Scoring the section would exceed the request's numeric-scan budget.
    ScanBudget { required: usize, available: usize },
    /// Conflicting values shared one identity and timestamp.
    ConflictingTimestamp { timestamp: i64 },
    /// Canonical row identities exceeded the request byte budget.
    IdentityByteLimit { observed: usize, limit: usize },
    /// Retaining the section's incident series would exceed the point cap.
    SeriesPointLimit { observed: usize, limit: usize },
    /// Retaining gauge evidence would exceed its independent point cap.
    TypedGaugePointLimit { observed: usize, limit: usize },
    /// A complete activity or lock snapshot set would exceed its row/edge cap.
    SnapshotRowLimit { observed: usize, limit: usize },
    /// Snapshot rows were present but did not form one complete edge set.
    IncompleteSnapshot,
}

/// Owned engine input, coverage, and exclusion counters.
pub(crate) struct PreparedInput {
    pub source_id: u64,
    pub node_self_id: String,
    pub episodes: Vec<EnrichedEpisode>,
    pub series: SeriesSet,
    pub typed: TypedInputs,
    pub log_events: LogEventInputs,
    pub coverage_by_section: BTreeMap<&'static str, Vec<Gap>>,
    pub quality: InputQuality,
    pub skipped: Vec<SectionSkip>,
    pub capability_by_section: BTreeMap<&'static str, CapabilityInputState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityInputState {
    Available,
    Partial,
    NotCollected,
}

/// Read and scan `sections` for one source over `[from, to)`.
///
/// Limits cover the whole call. An incomplete or locally oversized section is
/// skipped; input, decode, and request-wide admission failures are typed errors.
pub(crate) fn prepare_input(
    snap: &mut LocalDirSnapshot,
    source: u64,
    scan: &ScanParams,
    sections: &[&'static str],
    limits: &InputLimits,
) -> Result<PreparedInput, InputError> {
    let position_count = scan_position_count(scan).ok_or(InputError::InvalidScan)?;
    if position_count > limits.positions {
        return Err(InputError::PositionLimit {
            observed: position_count,
            limit: limits.positions,
        });
    }
    let overlapping_units = snap
        .units()
        .iter()
        .filter(|unit| {
            unit.source_id == source && unit.max_ts >= scan.from && unit.min_ts <= scan.to
        })
        .count();
    if overlapping_units > limits.units {
        return Err(InputError::UnitLimit {
            observed: overlapping_units,
            limit: limits.units,
        });
    }
    let mut input = read_input_pages(snap, source, scan, sections, limits)?;
    let log_events = build_log_events(
        &input.pages,
        &input.skipped,
        EventInputLimits::production(),
        scan.from,
        scan.to,
    );
    let mut remaining_identity_bytes = limits.identity_bytes;
    let node_self_id = load_node_identity(
        &input.metadata,
        &mut remaining_identity_bytes,
        limits.identity_bytes,
    )?;
    let gates = Gates::from_pages(&input.logicals, &input.pages);
    let snapshot_provenance = SnapshotProvenance::from_page(input.pages.get(SNAPSHOT_COVERAGE));
    let mut state = BuildState::new(
        limits,
        remaining_identity_bytes,
        input.skipped,
        snapshot_provenance,
    );
    for logical in &input.logicals {
        let Some(page) = input.pages.remove(logical.name) else {
            if state
                .skipped
                .iter()
                .any(|skip| skip.section == logical.name)
            {
                continue;
            }
            return Err(InputError::Read(QueryError::UnknownSection(
                logical.name.to_owned(),
            )));
        };
        state.process(logical, page, &gates, scan, limits)?;
    }

    Ok(PreparedInput {
        source_id: source,
        node_self_id,
        episodes: state.episodes,
        series: state.series,
        typed: state.typed,
        log_events,
        coverage_by_section: state.coverage_by_section,
        quality: state.quality,
        skipped: state.skipped,
        capability_by_section: state.capability_by_section,
    })
}

const PG_LOG_ERRORS: &str = "pg_log_errors";
const PG_LOG_LIFECYCLE: &str = "pg_log_lifecycle";
const PG_LOG_GAP: &str = "pg_log_gap";

/// Extract bounded, typed log events from the already-materialized log-section
/// pages, alongside — never through — the numeric series path.
///
/// A section that was skipped or not read is `NotCollected`, so a lens never
/// reads its silence as health. Stored groups stay separate because their
/// timestamps belong to one collector batch and cannot support a request-wide
/// count or historical merge.
fn build_log_events(
    pages: &BTreeMap<String, SectionPage>,
    skipped: &[SectionSkip],
    limits: EventInputLimits,
    from_us: i64,
    to_us: i64,
) -> LogEventInputs {
    let mut events = LogEventInputs::new(limits);
    ingest_log_errors(
        pages.get(PG_LOG_ERRORS),
        skipped,
        &mut events,
        from_us,
        to_us,
    );
    ingest_log_lifecycle(
        pages.get(PG_LOG_LIFECYCLE),
        skipped,
        &mut events,
        from_us,
        to_us,
    );
    if pages
        .get(PG_LOG_GAP)
        .is_some_and(|page| !page.rows.is_empty() || !page.gaps.is_empty())
    {
        events.set_coverage(PG_LOG_ERRORS, LogCoverage::Gap);
        events.set_coverage(PG_LOG_LIFECYCLE, LogCoverage::Gap);
    }
    events
}

/// `Gap` when the page is partial or has coverage gaps; otherwise `Unknown`,
/// since effective log configuration is never proven from the rows alone.
const fn log_coverage(page: &SectionPage) -> LogCoverage {
    if page.next_cursor.is_some() || !page.gaps.is_empty() {
        LogCoverage::Gap
    } else {
        LogCoverage::Unknown
    }
}

fn ingest_log_errors(
    page: Option<&SectionPage>,
    skipped: &[SectionSkip],
    events: &mut LogEventInputs,
    from_us: i64,
    to_us: i64,
) {
    let Some(page) = page.filter(|_| !skipped.iter().any(|skip| skip.section == PG_LOG_ERRORS))
    else {
        events.set_coverage(PG_LOG_ERRORS, LogCoverage::NotCollected);
        return;
    };
    for row in &page.rows {
        let (Some(ts), Some(severity), Some(count)) = (
            read_ts(row, "ts"),
            read_u8(row, "severity"),
            read_u64(row, "count"),
        ) else {
            continue;
        };
        if !(from_us..to_us).contains(&ts) {
            continue;
        }
        if !events.push_error(LogErrorGroup::new(
            ts,
            severity,
            read_str(row, "sqlstate"),
            count,
        )) {
            break;
        }
    }
    events.set_coverage(PG_LOG_ERRORS, log_coverage(page));
}

fn ingest_log_lifecycle(
    page: Option<&SectionPage>,
    skipped: &[SectionSkip],
    events: &mut LogEventInputs,
    from_us: i64,
    to_us: i64,
) {
    let Some(page) = page.filter(|_| !skipped.iter().any(|skip| skip.section == PG_LOG_LIFECYCLE))
    else {
        events.set_coverage(PG_LOG_LIFECYCLE, LogCoverage::NotCollected);
        return;
    };
    for row in &page.rows {
        let (Some(ts), Some(kind)) = (
            read_ts(row, "ts"),
            read_u8(row, "kind").and_then(lifecycle_kind),
        ) else {
            continue;
        };
        if !(from_us..to_us).contains(&ts) {
            continue;
        }
        if !events.push_lifecycle(LifecycleEvent::new(
            ts,
            read_i64(row, "pid"),
            kind,
            read_i32(row, "signal"),
        )) {
            break;
        }
    }
    events.set_coverage(PG_LOG_LIFECYCLE, log_coverage(page));
}

const fn lifecycle_kind(code: u8) -> Option<LifecycleKind> {
    match code {
        0 => Some(LifecycleKind::Crash),
        1 => Some(LifecycleKind::Shutdown),
        2 => Some(LifecycleKind::Ready),
        _ => None,
    }
}

fn read_u8(row: &OutRow, name: &str) -> Option<u8> {
    match row_value(row, name)? {
        Value::U64(value) => u8::try_from(*value).ok(),
        Value::I64(value) => u8::try_from(*value).ok(),
        _ => None,
    }
}

fn read_u64(row: &OutRow, name: &str) -> Option<u64> {
    match row_value(row, name)? {
        Value::U64(value) => Some(*value),
        Value::I64(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

fn read_i32(row: &OutRow, name: &str) -> Option<i32> {
    match row_value(row, name)? {
        Value::I64(value) => i32::try_from(*value).ok(),
        Value::U64(value) => i32::try_from(*value).ok(),
        _ => None,
    }
}

fn read_i64(row: &OutRow, name: &str) -> Option<i64> {
    match row_value(row, name)? {
        Value::I64(value) => Some(*value),
        Value::U64(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn read_ts(row: &OutRow, name: &str) -> Option<i64> {
    match row_value(row, name)? {
        Value::Ts(value) => Some(*value),
        _ => None,
    }
}

fn read_str(row: &OutRow, name: &str) -> Option<String> {
    match row_value(row, name)? {
        Value::Str(value) => Some(value.clone()),
        _ => None,
    }
}

struct InputPages {
    logicals: Vec<LogicalSection>,
    metadata: SectionPage,
    pages: BTreeMap<String, SectionPage>,
    skipped: Vec<SectionSkip>,
}

fn read_input_pages(
    snap: &mut LocalDirSnapshot,
    source: u64,
    scan: &ScanParams,
    sections: &[&'static str],
    limits: &InputLimits,
) -> Result<InputPages, InputError> {
    let names: BTreeSet<&'static str> = sections.iter().copied().collect();
    if names.len() > limits.sections {
        return Err(InputError::SectionLimit {
            observed: names.len(),
            limit: limits.sections,
        });
    }
    let logicals: Vec<LogicalSection> = names
        .into_iter()
        .map(|name| {
            logical_section(name).ok_or_else(|| InputError::UnknownSection(name.to_owned()))
        })
        .collect::<Result<_, _>>()?;

    let mut read_names = BTreeSet::new();
    read_names.extend(logicals.iter().map(|logical| logical.name));
    read_names.extend(Gates::sections(&logicals));
    read_names.insert(SNAPSHOT_COVERAGE);
    let observed_sections = read_names.len().saturating_add(1);
    if observed_sections > limits.sections {
        return Err(InputError::SectionLimit {
            observed: observed_sections,
            limit: limits.sections,
        });
    }

    let metadata_from = snap
        .units()
        .iter()
        .filter(|unit| {
            unit.source_id == source && unit.max_ts >= scan.from && unit.min_ts <= scan.to
        })
        .map(|unit| unit.min_ts)
        .min()
        .ok_or(InputError::NoData)?;
    let mut remaining_cells = limits.materialized_cells;
    let metadata_page = section_with_limits(
        snap,
        "instance_metadata",
        source,
        metadata_from,
        scan.to,
        None,
        QueryLimits::with_bytes(DIFF_MAX_ROWS, remaining_cells, limits.materialized_bytes),
    )
    .map_err(|err| map_query_error(err, limits.materialized_cells))?;
    charge_materialized_cells(
        &metadata_page,
        &mut remaining_cells,
        limits.materialized_cells,
    )?;

    let read_names: Vec<&str> = read_names.into_iter().collect();
    let batch = sections_with_limits(
        snap,
        source,
        scan.from,
        scan.to,
        &read_names,
        &BTreeMap::new(),
        QueryLimits::with_bytes(DIFF_MAX_ROWS, remaining_cells, limits.materialized_bytes),
    );
    let (pages, skipped) = match batch {
        Ok(pages) => (pages, Vec::new()),
        Err(QueryError::ResultTooLarge { .. }) => read_partial_pages(
            snap,
            source,
            scan,
            &read_names,
            remaining_cells,
            limits.materialized_cells,
            limits.materialized_bytes,
        )?,
        Err(QueryError::MaterializedBytesTooLarge { .. }) => {
            return Err(InputError::MaterializationLimit {
                limit: limits.materialized_bytes,
            });
        }
        Err(error) => return Err(InputError::Read(error)),
    };

    Ok(InputPages {
        logicals,
        metadata: metadata_page,
        pages,
        skipped,
    })
}

fn read_partial_pages(
    snap: &mut LocalDirSnapshot,
    source: u64,
    scan: &ScanParams,
    names: &[&str],
    mut remaining_cells: usize,
    materialization_limit: usize,
    materialized_byte_limit: usize,
) -> Result<(BTreeMap<String, SectionPage>, Vec<SectionSkip>), InputError> {
    let mut pages = BTreeMap::new();
    let mut skipped = Vec::new();
    let per_section_bytes = materialized_byte_limit / names.len().max(1);
    for &name in names {
        if remaining_cells == 0 {
            skipped.push(materialization_skip(name, materialization_limit)?);
            continue;
        }
        match section_with_limits(
            snap,
            name,
            source,
            scan.from,
            scan.to,
            None,
            QueryLimits::with_bytes(DIFF_MAX_ROWS, remaining_cells, per_section_bytes),
        ) {
            Ok(page) => {
                charge_materialized_cells(&page, &mut remaining_cells, materialization_limit)?;
                pages.insert(name.to_owned(), page);
            }
            Err(
                QueryError::ResultTooLarge { .. } | QueryError::MaterializedBytesTooLarge { .. },
            ) => {
                skipped.push(materialization_skip(name, materialization_limit)?);
                remaining_cells = 0;
            }
            Err(error) => return Err(InputError::Read(error)),
        }
    }
    Ok((pages, skipped))
}

fn materialization_skip(name: &str, limit: usize) -> Result<SectionSkip, InputError> {
    let logical =
        logical_section(name).ok_or_else(|| InputError::UnknownSection(name.to_owned()))?;
    Ok(SectionSkip {
        section: logical.name,
        reason: SkipReason::MaterializationLimit { limit },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotMarkerState {
    Complete,
    Restricted,
    Unavailable,
    Unknown,
}

struct SnapshotProvenance {
    activity: BTreeMap<i64, SnapshotMarkerState>,
    page_partial: bool,
}

impl SnapshotProvenance {
    fn from_page(page: Option<&SectionPage>) -> Self {
        let mut activity = BTreeMap::new();
        let page_partial =
            page.is_none_or(|page| page.next_cursor.is_some() || !page.gaps.is_empty());
        if let Some(page) = page {
            for row in &page.rows {
                let (Some(ts), Some(source_type_id), Some(read_state), Some(visibility)) = (
                    read_ts(row, "ts"),
                    read_u64(row, "source_type_id").and_then(|value| u32::try_from(value).ok()),
                    read_u8(row, "read_state"),
                    read_u8(row, "visibility"),
                ) else {
                    continue;
                };
                if !matches!(source_type_id, 1_001_001..=1_001_003) {
                    continue;
                }
                let counts_match = matches!(
                    (read_u64(row, "source_total"), read_u64(row, "collected")),
                    (Some(total), Some(collected)) if total == collected
                );
                let session_valid = read_u64(row, "collector_pid").is_some_and(|pid| pid > 0)
                    && read_ts(row, "collector_started_at").is_some_and(|start| start > 0);
                let marker = match (read_state, visibility, counts_match, session_valid) {
                    (0, 0, true, true) => SnapshotMarkerState::Complete,
                    (0, 1, true, true) => SnapshotMarkerState::Restricted,
                    (1..=4, _, _, true) => SnapshotMarkerState::Unavailable,
                    _ => SnapshotMarkerState::Unknown,
                };
                activity
                    .entry(ts)
                    .and_modify(|current| {
                        if *current != marker {
                            *current = SnapshotMarkerState::Unknown;
                        }
                    })
                    .or_insert(marker);
            }
        }
        Self {
            activity,
            page_partial,
        }
    }

    fn activity_snapshot(&self, ts: i64) -> SnapshotCompleteness {
        match self.activity.get(&ts) {
            Some(SnapshotMarkerState::Complete) => SnapshotCompleteness::Complete,
            Some(SnapshotMarkerState::Restricted) => SnapshotCompleteness::Restricted,
            Some(SnapshotMarkerState::Unavailable | SnapshotMarkerState::Unknown) | None => {
                SnapshotCompleteness::Unknown
            }
        }
    }

    fn activity_capability(&self) -> CapabilityInputState {
        if self.page_partial || self.activity.is_empty() {
            return CapabilityInputState::Partial;
        }
        if self
            .activity
            .values()
            .all(|state| matches!(state, SnapshotMarkerState::Complete))
        {
            CapabilityInputState::Available
        } else if self
            .activity
            .values()
            .all(|state| matches!(state, SnapshotMarkerState::Unavailable))
        {
            CapabilityInputState::NotCollected
        } else {
            CapabilityInputState::Partial
        }
    }
}

struct BuildState {
    episodes: Vec<EnrichedEpisode>,
    series: SeriesSet,
    typed: TypedInputs,
    coverage_by_section: BTreeMap<&'static str, Vec<Gap>>,
    quality: InputQuality,
    skipped: Vec<SectionSkip>,
    remaining_identity_bytes: usize,
    remaining_scan: usize,
    remaining_snapshot_rows: usize,
    snapshot_row_limit: usize,
    remaining_typed_gauge_points: usize,
    capability_by_section: BTreeMap<&'static str, CapabilityInputState>,
    snapshot_provenance: SnapshotProvenance,
}

impl BuildState {
    fn new(
        limits: &InputLimits,
        remaining_identity_bytes: usize,
        skipped: Vec<SectionSkip>,
        snapshot_provenance: SnapshotProvenance,
    ) -> Self {
        let activity_capability = snapshot_provenance.activity_capability();
        let mut capability_by_section = BTreeMap::new();
        capability_by_section.insert(PG_STAT_ACTIVITY, activity_capability);
        Self {
            episodes: Vec::new(),
            series: SeriesSet::new(limits.series_points),
            typed: TypedInputs::new(),
            coverage_by_section: BTreeMap::new(),
            quality: InputQuality::default(),
            skipped,
            remaining_identity_bytes,
            remaining_scan: limits.score_work,
            remaining_snapshot_rows: limits.snapshot_rows,
            snapshot_row_limit: limits.snapshot_rows,
            remaining_typed_gauge_points: limits.typed_gauge_points,
            capability_by_section,
            snapshot_provenance,
        }
    }

    const fn tally_scan(&mut self, counts: &ScanCounts) {
        self.quality.evaluated_positions = self
            .quality
            .evaluated_positions
            .saturating_add(counts.evaluated);
        self.quality.unevaluated_positions = self
            .quality
            .unevaluated_positions
            .saturating_add(counts.ref_too_small)
            .saturating_add(counts.cur_too_small)
            .saturating_add(counts.all_no_data)
            .saturating_add(counts.non_finite)
            .saturating_add(counts.discontinuity);
        self.quality.episodes_truncated = self
            .quality
            .episodes_truncated
            .saturating_add(counts.episodes_truncated);
    }

    fn process(
        &mut self,
        logical: &LogicalSection,
        mut page: SectionPage,
        gates: &Gates,
        scan: &ScanParams,
        limits: &InputLimits,
    ) -> Result<(), InputError> {
        if page.next_cursor.is_some() {
            self.skip_incomplete(logical.name);
            return Ok(());
        }
        self.record_page_state(logical.name, &page);

        if logical.name == SNAPSHOT_COVERAGE {
            return Ok(());
        }

        // Log rows contain deliberately retained diagnostic text. They feed only
        // the typed event adapter below; admitting them to generic anomaly
        // identities could expose patterns, statements, users, or addresses in
        // incident keys and JSON.
        if logical.name.starts_with("pg_log_") {
            return Ok(());
        }

        let identity = logical.diff_key();
        let (cumulative, gauges) = scorable_columns(logical);
        let Some(duplicate_gauge_breaks) = self.normalize_page(
            logical,
            &mut page.rows,
            &identity,
            &cumulative,
            &gauges,
            limits,
        ) else {
            return Ok(());
        };
        let mut diffs = diff_section(&identity, &cumulative, &page.rows, &page.gaps);
        gates.apply(logical, &mut diffs);
        let gauge_series = gauge_section(&identity, &gauges, &page.rows);
        tally_quality(&diffs, &gauge_series, &mut self.quality);
        self.retain_typed(logical.name, &cumulative, &diffs);
        self.retain_snapshots(logical.name, &page.rows);
        let gauge_points = self.admit_typed_gauge_points(logical.name, &gauge_series, limits);

        let scanned = match scan_section(
            &diffs,
            &gauge_series,
            scan,
            self.remaining_scan,
            limits.episodes,
        ) {
            Ok((hits, counts, work)) => {
                self.remaining_scan -= work;
                self.tally_scan(&counts);
                hits
            }
            Err(limit) => {
                self.skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::ScanBudget {
                        required: limit.required,
                        available: limit.available,
                    },
                });
                return Ok(());
            }
        };

        match ingest_section(
            logical,
            &diffs,
            &gauge_series,
            scanned,
            &mut self.series,
            &mut self.quality,
        ) {
            Ok(section_episodes) => {
                if let Some(gauge_points) = gauge_points {
                    self.remaining_typed_gauge_points -= gauge_points;
                    let gaps: Vec<(i64, i64)> =
                        page.gaps.iter().map(|gap| (gap.from, gap.to)).collect();
                    self.retain_typed_gauges(
                        logical.name,
                        &gauges,
                        gauge_series,
                        &gaps,
                        &duplicate_gauge_breaks,
                    );
                }
                self.episodes.extend(section_episodes);
                self.quality.episodes_truncated = self
                    .quality
                    .episodes_truncated
                    .saturating_add(rank_episodes(&mut self.episodes, limits.episodes));
            }
            Err(InputError::SeriesLimit { observed, limit }) => {
                self.skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::SeriesPointLimit { observed, limit },
                });
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }

    fn normalize_page(
        &mut self,
        logical: &LogicalSection,
        rows: &mut Vec<OutRow>,
        identity: &[&str],
        cumulative: &[&str],
        gauges: &[&str],
        limits: &InputLimits,
    ) -> Option<Vec<DuplicateGaugeBreak>> {
        let result = normalize_duplicate_rows(
            rows,
            identity,
            cumulative,
            gauges,
            snapshot_fields(logical.name),
            &mut self.quality,
            &mut IdentityBudget {
                remaining: &mut self.remaining_identity_bytes,
                limit: limits.identity_bytes,
            },
        );
        match result {
            Ok(breaks) => Some(breaks),
            Err(NormalizeError::Conflict(timestamp)) => {
                self.skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::ConflictingTimestamp { timestamp },
                });
                None
            }
            Err(NormalizeError::IdentityByteLimit { observed, limit }) => {
                self.skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::IdentityByteLimit { observed, limit },
                });
                None
            }
        }
    }

    fn record_page_state(&mut self, section: &'static str, page: &SectionPage) {
        if let Some(state) = capability_state(section, &page.rows) {
            self.capability_by_section.insert(section, state);
        }
        self.coverage_by_section.insert(section, page.gaps.clone());
    }

    fn skip_incomplete(&mut self, section: &'static str) {
        self.skipped.push(SectionSkip {
            section,
            reason: SkipReason::IncompletePage,
        });
    }

    fn admit_typed_gauge_points(
        &mut self,
        section: &'static str,
        gauge_series: &[SeriesValues],
        limits: &InputLimits,
    ) -> Option<usize> {
        let points = gauge_series
            .iter()
            .flat_map(|series| &series.columns)
            .try_fold(0_usize, |total, column| {
                total.checked_add(column.points.len())
            })
            .unwrap_or(usize::MAX);
        if points <= self.remaining_typed_gauge_points {
            return Some(points);
        }
        self.skipped.push(SectionSkip {
            section,
            reason: SkipReason::TypedGaugePointLimit {
                observed: limits
                    .typed_gauge_points
                    .saturating_sub(self.remaining_typed_gauge_points)
                    .saturating_add(points),
                limit: limits.typed_gauge_points,
            },
        });
        None
    }

    /// Retain the typed counter diffs the section already folded, keyed for lens
    /// lookup. Non-canonical identities are skipped; their series carry no
    /// episode either, so a lens has nothing to join. Columns line up positionally
    /// with `cumulative`, the order `diff_section` folded them in.
    fn retain_typed(
        &mut self,
        section: &'static str,
        cumulative: &[&'static str],
        diffs: &[SeriesDiff],
    ) {
        for series in diffs {
            let Some(identity) = accept_identity(&series.key) else {
                continue;
            };
            for (&name, column) in cumulative.iter().zip(&series.columns) {
                let points = column
                    .points
                    .iter()
                    .map(|point| (point.ts, point.point))
                    .collect();
                self.typed
                    .insert_counter(section, name, std::sync::Arc::clone(&identity), points);
            }
        }
    }

    /// Retain each series' raw gauge readings, keyed for lens lookup. Gauges are
    /// instantaneous levels, so nothing is differenced: the reader already
    /// dropped NULL, non-numeric, and non-finite readings, leaving one `(ts,
    /// value)` per valid sample. Columns line up positionally with `gauges`, the
    /// order `gauge_section` collected them in.
    fn retain_typed_gauges(
        &mut self,
        section: &'static str,
        gauges: &[&'static str],
        gauge_series: Vec<SeriesValues>,
        gaps: &[(i64, i64)],
        duplicate_breaks: &[DuplicateGaugeBreak],
    ) {
        let quality = GaugeQuality::new(gaps);
        for series in gauge_series {
            let Some(identity) = accept_identity(&series.key) else {
                continue;
            };
            let shared_breaks: std::sync::Arc<[i64]> = duplicate_breaks
                .iter()
                .filter(|duplicate| duplicate.identity.as_slice() == identity.as_ref())
                .map(|duplicate| duplicate.timestamp)
                .collect();
            for (&name, column) in gauges.iter().zip(series.columns) {
                self.typed.insert_gauge_with_shared_quality(
                    GaugeTrackInput {
                        section,
                        column: name,
                        identity: std::sync::Arc::clone(&identity),
                        raw_points: column.points,
                        breaks: column.breaks,
                        shared_breaks: std::sync::Arc::clone(&shared_breaks),
                    },
                    &quality,
                );
            }
        }
    }

    /// Retain the raw activity or lock snapshots the sampled lenses read,
    /// grouped by collection time. A section whose rows would exceed the
    /// request snapshot-row ceiling is withheld whole and counted, never
    /// sampled, so a lens sees the full section or none. Sections without a
    /// snapshot lens are ignored.
    fn retain_snapshots(&mut self, section: &'static str, rows: &[OutRow]) {
        match section {
            PG_STAT_ACTIVITY => {
                let observed = rows
                    .iter()
                    .filter(|row| matches!(row_value(row, "ts"), Some(Value::Ts(_))))
                    .count();
                if self.withhold_snapshots(section, observed) {
                    return;
                }
                for snapshot in build_activity_snapshots(rows, &self.snapshot_provenance) {
                    self.typed.insert_activity_snapshot(snapshot);
                }
            }
            PG_LOCKS => {
                let observed = rows
                    .iter()
                    .try_fold(0_usize, |total, row| {
                        let edges = match row_value(row, "blocked_by") {
                            Some(Value::ListI32(blockers)) => blockers.len(),
                            _ => 0,
                        };
                        total.checked_add(edges)
                    })
                    .unwrap_or(usize::MAX);
                if self.withhold_snapshots(section, observed) {
                    return;
                }
                let snapshots = build_lock_snapshots(rows);
                if observed > 0 && snapshots.is_empty() {
                    self.capability_by_section
                        .insert(section, CapabilityInputState::Partial);
                    self.skipped.push(SectionSkip {
                        section,
                        reason: SkipReason::IncompleteSnapshot,
                    });
                    return;
                }
                for snapshot in snapshots {
                    self.typed.insert_lock_snapshot(snapshot);
                }
            }
            _ => {}
        }
    }

    /// Charge `observed` snapshot rows against the request ceiling. Returns
    /// `true` when the section does not fit and must be withheld whole; the
    /// withheld rows are counted so the response reports the incompleteness.
    fn withhold_snapshots(&mut self, section: &'static str, observed: usize) -> bool {
        if observed > self.remaining_snapshot_rows {
            self.quality.snapshot_rows_withheld = self
                .quality
                .snapshot_rows_withheld
                .saturating_add(u64::try_from(observed).unwrap_or(u64::MAX));
            self.skipped.push(SectionSkip {
                section,
                reason: SkipReason::SnapshotRowLimit {
                    observed: self
                        .snapshot_row_limit
                        .saturating_sub(self.remaining_snapshot_rows)
                        .saturating_add(observed),
                    limit: self.snapshot_row_limit,
                },
            });
            return true;
        }
        self.remaining_snapshot_rows -= observed;
        false
    }
}

/// Group `pg_stat_activity` rows into per-collection-time snapshots. A row with
/// no timestamp carries no snapshot to join and is dropped.
fn build_activity_snapshots(
    rows: &[OutRow],
    provenance: &SnapshotProvenance,
) -> Vec<ActivitySnapshot> {
    let mut by_ts: BTreeMap<i64, Vec<ActivityBackend>> = BTreeMap::new();
    for row in rows {
        let Some(Value::Ts(ts)) = row_value(row, "ts") else {
            continue;
        };
        by_ts
            .entry(*ts)
            .or_default()
            .push(activity_backend(row, *ts));
    }
    by_ts
        .into_iter()
        .map(|(ts, backends)| ActivitySnapshot {
            ts,
            backends,
            completeness: provenance.activity_snapshot(ts),
        })
        .collect()
}

/// Reduce one activity row to the fields the sampled lenses read. `xact_age_us`
/// is the open-transaction age against the snapshot time; a start after the
/// snapshot (clock disagreement) yields `None` rather than a negative age.
fn activity_backend(row: &OutRow, ts: i64) -> ActivityBackend {
    ActivityBackend {
        pid: read_i64(row, "pid").unwrap_or(0),
        backend_start: read_ts(row, "backend_start").unwrap_or(0),
        xid_age: read_i64(row, "backend_xid_age"),
        xmin_age: match row_value(row, "backend_xmin_age") {
            Some(Value::I64(age)) => Some(*age),
            _ => None,
        },
        state: str_value(row, "state"),
        wait_event_type: str_value(row, "wait_event_type"),
        wait_event: str_value(row, "wait_event"),
        xact_age_us: match row_value(row, "xact_start") {
            Some(Value::Ts(start)) => ts.checked_sub(*start).filter(|age| *age >= 0),
            _ => None,
        },
    }
}

/// A resolved string label, or `None` for `NULL` or any non-string cell.
fn str_value(row: &OutRow, name: &str) -> Option<Box<str>> {
    match row_value(row, name) {
        Some(Value::Str(text)) => Some(text.as_str().into()),
        _ => None,
    }
}

/// Group `pg_locks` rows into per-collection-time edge sets. Each waiter row
/// expands its `blocked_by` list into one edge per blocker; a root row (empty
/// `blocked_by`) contributes no edge, so a snapshot with no contention is
/// simply absent.
fn build_lock_snapshots(rows: &[OutRow]) -> Vec<LockSnapshot> {
    let mut pids_by_ts: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for row in rows {
        let (Some(Value::Ts(ts)), Some(Value::I64(pid))) =
            (row_value(row, "ts"), row_value(row, "pid"))
        else {
            continue;
        };
        if *pid > 0 {
            pids_by_ts.entry(*ts).or_default().insert(*pid);
        }
    }

    let mut by_ts: BTreeMap<i64, BTreeSet<LockEdge>> = BTreeMap::new();
    let mut invalid_ts = BTreeSet::new();
    for row in rows {
        let Some(Value::Ts(ts)) = row_value(row, "ts") else {
            continue;
        };
        let Some(Value::I64(waiter)) = row_value(row, "pid") else {
            continue;
        };
        let Some(Value::ListI32(blockers)) = row_value(row, "blocked_by") else {
            continue;
        };
        if blockers.is_empty() {
            continue;
        }
        let edges = by_ts.entry(*ts).or_default();
        for &blocker in blockers {
            let blocker = i64::from(blocker);
            if *waiter <= 0
                || blocker < 0
                || blocker == *waiter
                || (blocker != 0
                    && !pids_by_ts
                        .get(ts)
                        .is_some_and(|pids| pids.contains(&blocker)))
            {
                invalid_ts.insert(*ts);
                continue;
            }
            edges.insert(LockEdge {
                waiter_pid: *waiter,
                blocker_pid: blocker,
            });
        }
    }
    if !invalid_ts.is_empty() {
        return Vec::new();
    }
    by_ts
        .into_iter()
        .map(|(ts, edges)| LockSnapshot {
            ts,
            edges: edges.into_iter().collect(),
        })
        .collect()
}

fn capability_state(section: &'static str, rows: &[OutRow]) -> Option<CapabilityInputState> {
    let empty_state = match section {
        PG_LOCKS => CapabilityInputState::NotCollected,
        "pg_vacuum_observation" | "pg_replication_physical" | "pg_replication_slot_retention" => {
            CapabilityInputState::Available
        }
        "pg_freeze_horizon" | "pg_storage_mount" | "pg_process_cgroup_memory" => {
            CapabilityInputState::NotCollected
        }
        _ => return None,
    };
    if rows.is_empty() {
        return Some(empty_state);
    }
    if section == PG_LOCKS {
        return Some(CapabilityInputState::Available);
    }
    if matches!(section, "pg_storage_mount" | "pg_process_cgroup_memory") {
        let all_verified = rows.iter().all(|row| {
            matches!(
                row_value(row, "mapping_state"),
                Some(Value::I64(1) | Value::U64(1))
            )
        });
        return Some(if all_verified {
            CapabilityInputState::Available
        } else {
            CapabilityInputState::Partial
        });
    }
    Some(CapabilityInputState::Available)
}

pub(crate) fn scan_position_count(scan: &ScanParams) -> Option<usize> {
    if scan.from >= scan.to
        || scan.window <= 0
        || scan.step <= 0
        || !scan.threshold.is_finite()
        || scan.threshold < 0.0
        || !scan.eps_rel.is_finite()
        || scan.eps_rel < 0.0
    {
        return None;
    }
    let first = scan.from.checked_add(scan.window)?;
    if first > scan.to {
        return None;
    }
    if first == scan.to {
        return Some(1);
    }
    let interior = scan
        .to
        .checked_sub(first)?
        .checked_sub(1)?
        .checked_div(scan.step)?
        .checked_add(1)?;
    usize::try_from(interior.checked_add(1)?).ok()
}

fn map_query_error(error: QueryError, materialization_limit: usize) -> InputError {
    match error {
        QueryError::ResultTooLarge { .. } | QueryError::MaterializedBytesTooLarge { .. } => {
            InputError::MaterializationLimit {
                limit: materialization_limit,
            }
        }
        other => InputError::Read(other),
    }
}

fn charge_materialized_cells(
    page: &SectionPage,
    remaining: &mut usize,
    limit: usize,
) -> Result<(), InputError> {
    let cells = page.rows.iter().try_fold(0_usize, |total, row| {
        total
            .checked_add(row.len())
            .ok_or(InputError::MaterializationLimit { limit })
    })?;
    *remaining = remaining
        .checked_sub(cells)
        .ok_or(InputError::MaterializationLimit { limit })?;
    Ok(())
}

fn load_node_identity(
    page: &SectionPage,
    remaining_bytes: &mut usize,
    byte_limit: usize,
) -> Result<String, InputError> {
    if page.next_cursor.is_some() {
        return Err(InputError::MissingNodeIdentity);
    }
    let mut identities = BTreeSet::new();
    for row in &page.rows {
        let Some(Value::Str(value)) = row_value(row, "node_self_id") else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        charge_identity_bytes(value.len(), remaining_bytes, byte_limit)
            .map_err(|(observed, limit)| InputError::IdentityByteLimit { observed, limit })?;
        identities.insert(value.clone());
    }
    match identities.len() {
        0 => Err(InputError::MissingNodeIdentity),
        1 => identities
            .into_iter()
            .next()
            .ok_or(InputError::MissingNodeIdentity),
        _ => Err(InputError::ConflictingNodeIdentity),
    }
}

fn row_value<'a>(row: &'a OutRow, name: &str) -> Option<&'a Value> {
    row.iter()
        .find_map(|(column, value)| (column == name).then_some(value))
}

fn canonical_identity_values(row: &OutRow, names: &[&str]) -> Option<Vec<IdentityValue>> {
    names
        .iter()
        .map(|name| match row_value(row, name)? {
            Value::I64(value) => Some(IdentityValue::I64(*value)),
            Value::U64(value) => Some(IdentityValue::U64(*value)),
            Value::Bool(value) => Some(IdentityValue::Bool(*value)),
            Value::Str(value) => Some(IdentityValue::Text(value.clone())),
            Value::Null | Value::F64(_) | Value::Ts(_) | Value::Blob { .. } | Value::ListI32(_) => {
                None
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalizeError {
    Conflict(i64),
    IdentityByteLimit { observed: usize, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DuplicateGaugeBreak {
    identity: Vec<IdentityValue>,
    timestamp: i64,
}

struct IdentityBudget<'a> {
    remaining: &'a mut usize,
    limit: usize,
}

fn snapshot_fields(section: &str) -> &'static [&'static str] {
    match section {
        PG_STAT_ACTIVITY => &[
            "backend_start",
            "leader_pid",
            "backend_type",
            "state",
            "wait_event_type",
            "wait_event",
            "backend_xmin_age",
            "xact_start",
            "query_start",
        ],
        PG_LOCKS => &[
            "backend_start",
            "blocked_by",
            "waitstart",
            "mode",
            "locktype",
        ],
        _ => &[],
    }
}

fn identity_bytes(identity: &[IdentityValue]) -> Option<usize> {
    identity.iter().try_fold(0_usize, |sum, value| {
        let bytes = match value {
            IdentityValue::I64(_) | IdentityValue::U64(_) => 9,
            IdentityValue::Bool(_) => 2,
            IdentityValue::Text(text) => 9_usize.checked_add(text.len())?,
        };
        sum.checked_add(bytes)
    })
}

fn charge_identity_bytes(
    bytes: usize,
    remaining: &mut usize,
    limit: usize,
) -> Result<(), (usize, usize)> {
    let Some(left) = remaining.checked_sub(bytes) else {
        let observed = limit
            .checked_sub(*remaining)
            .and_then(|spent| spent.checked_add(bytes))
            .unwrap_or(usize::MAX);
        return Err((observed, limit));
    };
    *remaining = left;
    Ok(())
}

fn normalize_duplicate_rows(
    rows: &mut Vec<OutRow>,
    identity: &[&str],
    cumulative: &[&str],
    gauges: &[&str],
    snapshot_fields: &[&str],
    quality: &mut InputQuality,
    identity_budget: &mut IdentityBudget<'_>,
) -> Result<Vec<DuplicateGaugeBreak>, NormalizeError> {
    let compared: Vec<&str> = cumulative
        .iter()
        .chain(gauges)
        .chain(snapshot_fields)
        .copied()
        .collect();
    let mut seen: BTreeMap<(Vec<IdentityValue>, i64), usize> = BTreeMap::new();
    let mut kept = Vec::with_capacity(rows.len());
    let mut duplicate_breaks = Vec::new();
    for row in std::mem::take(rows) {
        let Some(key) = canonical_identity_values(&row, identity) else {
            quality.non_canonical_identity = quality.non_canonical_identity.saturating_add(1);
            continue;
        };
        let bytes = identity_bytes(&key).ok_or(NormalizeError::IdentityByteLimit {
            observed: usize::MAX,
            limit: identity_budget.limit,
        })?;
        charge_identity_bytes(bytes, identity_budget.remaining, identity_budget.limit)
            .map_err(|(observed, limit)| NormalizeError::IdentityByteLimit { observed, limit })?;
        let Some(Value::Ts(timestamp)) = row_value(&row, "ts") else {
            kept.push(row);
            continue;
        };
        let row_key = (key, *timestamp);
        if let Some(&previous) = seen.get(&row_key) {
            let equal = compared
                .iter()
                .all(|name| row_value(&kept[previous], name) == row_value(&row, name));
            if !equal {
                return Err(NormalizeError::Conflict(*timestamp));
            }
            quality.duplicate_timestamps = quality.duplicate_timestamps.saturating_add(1);
            duplicate_breaks.push(DuplicateGaugeBreak {
                identity: row_key.0,
                timestamp: *timestamp,
            });
        } else {
            seen.insert(row_key, kept.len());
            kept.push(row);
        }
    }
    *rows = kept;
    Ok(duplicate_breaks)
}

fn tally_quality(diffs: &[SeriesDiff], gauges: &[SeriesValues], quality: &mut InputQuality) {
    for point in diffs
        .iter()
        .flat_map(|series| &series.columns)
        .flat_map(|column| &column.points)
    {
        match point.point {
            DiffPoint::Value { rate, .. } if rate.is_finite() => {}
            DiffPoint::Value { .. } => {
                quality.non_finite_points = quality.non_finite_points.saturating_add(1);
            }
            DiffPoint::NoData {
                reason: Reason::FirstPoint,
            } => quality.first_points = quality.first_points.saturating_add(1),
            DiffPoint::NoData {
                reason: Reason::Reset,
            } => quality.resets = quality.resets.saturating_add(1),
            DiffPoint::NoData {
                reason: Reason::Gap,
            } => quality.gaps = quality.gaps.saturating_add(1),
            DiffPoint::NoData {
                reason: Reason::NotCollected,
            } => quality.not_collected = quality.not_collected.saturating_add(1),
            DiffPoint::NoData {
                reason: Reason::Anomaly,
            } => quality.anomalous_points = quality.anomalous_points.saturating_add(1),
        }
    }
    for column in gauges.iter().flat_map(|series| &series.columns) {
        quality.invalid_gauge_points = quality
            .invalid_gauge_points
            .saturating_add(column.skipped as u64);
    }
}

fn rank_episodes(episodes: &mut Vec<EnrichedEpisode>, limit: usize) -> u64 {
    let removed = episodes.len().saturating_sub(limit) as u64;
    episodes.sort_by(|left, right| {
        right
            .episode
            .peak
            .m
            .abs()
            .total_cmp(&left.episode.peak.m.abs())
            .then_with(|| left.reference.cmp(&right.reference))
    });
    episodes.truncate(limit);
    removed
}

/// Turn one section's scan hits and scanned series into engine input.
///
/// Both the episode references and the inserted series carry the same
/// canonical identity, so a lens looks a member's series up by its reference.
fn ingest_section(
    logical: &LogicalSection,
    diffs: &[SeriesDiff],
    gauges: &[SeriesValues],
    hits: Vec<EpisodeHit>,
    series: &mut SeriesSet,
    quality: &mut InputQuality,
) -> Result<Vec<EnrichedEpisode>, InputError> {
    let mut inserted: BTreeSet<(&'static str, Vec<IdentityValue>)> = BTreeSet::new();
    let mut episodes = Vec::with_capacity(hits.len());
    let mut built_series = Vec::new();
    let mut remaining_points = series.remaining_points();
    for hit in hits {
        let Some(identity) = canonical_identity(&hit.key, quality) else {
            continue;
        };
        let column = resolve_column(logical, &hit.column)?;
        let reference = EpisodeRefV1 {
            logical_section: logical.name,
            column,
            identity: identity.clone().into(),
            start_us: hit.episode.start,
            end_us: hit.episode.end,
        };

        let key = (column, identity.clone());
        if inserted.insert(key)
            && let Some(built) = column_series(
                diffs,
                gauges,
                &hit.key,
                logical.name,
                column,
                remaining_points,
                series.point_limit(),
            )?
        {
            remaining_points -= built.len();
            built_series.push((column, identity, built));
        }

        episodes.push(EnrichedEpisode {
            episode: hit.episode,
            reference,
        });
    }

    for (column, identity, _) in &built_series {
        if series.contains(logical.name, column, identity) {
            return Err(InputError::DuplicateSeries {
                section: logical.name,
                column,
            });
        }
    }
    for (column, identity, built) in built_series {
        insert_series(series, logical.name, column, identity, built)?;
    }
    Ok(episodes)
}

/// Convert an identity row to canonical scalars, or count the drop.
///
/// Only signed/unsigned integers, booleans, and resolved text encode into a
/// series identity; a `Null`, float, timestamp, blob, or list drops the whole
/// episode with a counted reason (§5.2 conservative rejection).
fn canonical_identity(key: &[Value], quality: &mut InputQuality) -> Option<Vec<IdentityValue>> {
    let mut identity = Vec::with_capacity(key.len());
    for value in key {
        let scalar = match value {
            Value::I64(v) => IdentityValue::I64(*v),
            Value::U64(v) => IdentityValue::U64(*v),
            Value::Bool(v) => IdentityValue::Bool(*v),
            Value::Str(text) => IdentityValue::Text(text.clone()),
            Value::Null | Value::F64(_) | Value::Ts(_) | Value::Blob { .. } | Value::ListI32(_) => {
                quality.non_canonical_identity = quality.non_canonical_identity.saturating_add(1);
                return None;
            }
        };
        identity.push(scalar);
    }
    Some(identity)
}

/// Convert a series key to a canonical identity without counting, for typed
/// retention. Mirrors [`canonical_identity`]'s accepted kinds; a rejected kind
/// drops the series, which the episode path already excluded and counted.
fn accept_identity(key: &[Value]) -> Option<std::sync::Arc<[IdentityValue]>> {
    let mut out = Vec::with_capacity(key.len());
    for value in key {
        out.push(match value {
            Value::I64(v) => IdentityValue::I64(*v),
            Value::U64(v) => IdentityValue::U64(*v),
            Value::Bool(v) => IdentityValue::Bool(*v),
            Value::Str(v) => IdentityValue::Text(v.clone()),
            _ => return None,
        });
    }
    Some(std::sync::Arc::from(out))
}

/// Resolve a scanned column name to the section's static union name.
///
/// A scan hit always names a column of the section it scanned; a miss means the
/// registry contract and the scan disagree, which is a typed error, not a drop.
fn resolve_column(logical: &LogicalSection, column: &str) -> Result<&'static str, InputError> {
    logical
        .columns
        .iter()
        .find(|candidate| candidate.name == column)
        .map(|candidate| candidate.name)
        .ok_or_else(|| InputError::UnknownColumn {
            section: logical.name,
            column: column.to_owned(),
        })
}

fn column_series(
    diffs: &[SeriesDiff],
    gauges: &[SeriesValues],
    key: &[Value],
    section: &'static str,
    column: &'static str,
    remaining_points: usize,
    point_limit: usize,
) -> Result<Option<Series>, InputError> {
    if let Some(series) = diffs.iter().find(|series| series.key == key)
        && let Some(diff) = series.columns.iter().find(|diff| diff.name == column)
    {
        let mut runs = Vec::new();
        let mut ts = Vec::new();
        let mut values = Vec::new();
        let mut retained = 0_usize;
        for at in &diff.points {
            match at.point {
                DiffPoint::Value { rate, .. } if rate.is_finite() => {
                    charge_series_point(&mut retained, remaining_points, point_limit)?;
                    ts.push(at.ts);
                    values.push(rate);
                }
                DiffPoint::NoData {
                    reason: Reason::FirstPoint,
                } => {}
                DiffPoint::Value { .. } | DiffPoint::NoData { .. } => {
                    finish_run(&mut runs, &mut ts, &mut values);
                }
            }
        }
        finish_run(&mut runs, &mut ts, &mut values);
        return build_series(runs, section, column);
    }
    if let Some(series) = gauges.iter().find(|series| series.key == key)
        && let Some(values) = series.columns.iter().find(|values| values.name == column)
    {
        let mut runs = Vec::new();
        let mut ts = Vec::new();
        let mut readings = Vec::new();
        let mut retained = 0_usize;
        let mut breaks = values.breaks.iter().peekable();
        for &(at, value) in &values.points {
            if breaks.next_if(|&&gap| gap <= at).is_some() {
                while breaks.next_if(|&&gap| gap <= at).is_some() {}
                finish_run(&mut runs, &mut ts, &mut readings);
            }
            charge_series_point(&mut retained, remaining_points, point_limit)?;
            ts.push(at);
            readings.push(value);
        }
        finish_run(&mut runs, &mut ts, &mut readings);
        return build_series(runs, section, column);
    }
    Ok(None)
}

fn charge_series_point(
    retained: &mut usize,
    remaining: usize,
    limit: usize,
) -> Result<(), InputError> {
    *retained = retained.checked_add(1).ok_or(InputError::SeriesLimit {
        observed: usize::MAX,
        limit,
    })?;
    if *retained > remaining {
        return Err(InputError::SeriesLimit {
            observed: limit.saturating_add(1),
            limit,
        });
    }
    Ok(())
}

fn finish_run(runs: &mut Vec<(Vec<i64>, Vec<f64>)>, ts: &mut Vec<i64>, values: &mut Vec<f64>) {
    if !ts.is_empty() {
        runs.push((std::mem::take(ts), std::mem::take(values)));
    }
}

fn build_series(
    runs: Vec<(Vec<i64>, Vec<f64>)>,
    section: &'static str,
    column: &'static str,
) -> Result<Option<Series>, InputError> {
    let built = Series::from_runs(runs).map_err(|error| InputError::InvalidSeries {
        section,
        column,
        error,
    })?;
    Ok((!built.is_empty()).then_some(built))
}

fn insert_series(
    series: &mut SeriesSet,
    section: &'static str,
    column: &'static str,
    identity: Vec<IdentityValue>,
    built: Series,
) -> Result<(), InputError> {
    match series.insert(section, column, identity.into(), built) {
        Ok(()) => Ok(()),
        Err(SeriesInsertError::Duplicate) => Err(InputError::DuplicateSeries { section, column }),
        Err(SeriesInsertError::PointLimit { observed, limit }) => {
            Err(InputError::SeriesLimit { observed, limit })
        }
    }
}

/// Split a section's columns into cumulative and gauge name lists, matching the
/// anomaly scan's scorable-column selection.
fn scorable_columns(logical: &LogicalSection) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut cumulative = Vec::new();
    let mut gauges = Vec::new();
    for column in &logical.columns {
        match column.class {
            ColumnClass::Cumulative => cumulative.push(column.name),
            ColumnClass::Gauge => gauges.push(column.name),
            ColumnClass::Label | ColumnClass::Timestamp => {}
        }
    }
    (cumulative, gauges)
}

#[cfg(test)]
mod tests {
    use kronika_reader::{ColumnDiff, ColumnValues, DiffAt, Scalar};

    use super::*;
    use crate::incident::{ClockRelation, IncidentConfig, analyze};

    const MINUTE: i64 = 60 * 1_000_000;

    fn quality() -> InputQuality {
        InputQuality::default()
    }

    #[test]
    fn every_canonical_scalar_converts_and_the_rest_drop_the_episode() {
        let mut q = quality();
        let accepted = canonical_identity(
            &[
                Value::I64(-7),
                Value::U64(9),
                Value::Bool(true),
                Value::Str("db".to_owned()),
            ],
            &mut q,
        );
        assert_eq!(
            accepted,
            Some(vec![
                IdentityValue::I64(-7),
                IdentityValue::U64(9),
                IdentityValue::Bool(true),
                IdentityValue::Text("db".to_owned()),
            ])
        );
        assert_eq!(q.non_canonical_identity, 0);

        for rejected in [
            Value::Null,
            Value::F64(1.0),
            Value::Ts(1),
            Value::Blob {
                text: "x".to_owned(),
                full_len: 1,
                truncated: false,
            },
            Value::ListI32(vec![1]),
        ] {
            let mut q = quality();
            assert_eq!(
                canonical_identity(&[Value::I64(1), rejected], &mut q),
                None,
                "a non-canonical value drops the whole identity"
            );
            assert_eq!(q.non_canonical_identity, 1);
        }
    }

    #[test]
    fn accept_identity_mirrors_canonical_kinds_without_counting() {
        assert_eq!(
            accept_identity(&[
                Value::I64(-7),
                Value::U64(9),
                Value::Bool(true),
                Value::Str("db".to_owned()),
            ])
            .as_deref(),
            Some(
                [
                    IdentityValue::I64(-7),
                    IdentityValue::U64(9),
                    IdentityValue::Bool(true),
                    IdentityValue::Text("db".to_owned()),
                ]
                .as_slice()
            )
        );

        for rejected in [
            Value::Null,
            Value::F64(1.0),
            Value::Ts(1),
            Value::Blob {
                text: "x".to_owned(),
                full_len: 1,
                truncated: false,
            },
            Value::ListI32(vec![1]),
        ] {
            assert_eq!(
                accept_identity(&[Value::I64(1), rejected]).as_deref(),
                None,
                "a non-canonical value drops the whole identity"
            );
        }
    }

    #[test]
    fn an_empty_identity_is_the_canonical_singleton_key() {
        let mut q = quality();
        assert_eq!(canonical_identity(&[], &mut q), Some(Vec::new()));
        assert_eq!(q.non_canonical_identity, 0);
    }

    #[test]
    fn scan_position_count_rejects_degenerate_arithmetic() {
        let mut scan = spike_scan(10 * MINUTE);
        assert_eq!(scan_position_count(&scan), Some(3));
        scan.step = 0;
        assert_eq!(scan_position_count(&scan), None);
        scan.step = MINUTE;
        scan.from = i64::MAX - 1;
        scan.to = i64::MAX;
        assert_eq!(scan_position_count(&scan), None);
    }

    #[test]
    fn resolve_column_returns_the_static_name_or_a_typed_error() {
        let logical = logical_section("pg_stat_archiver").expect("archiver in the registry");
        let resolved = resolve_column(&logical, "archived_count").expect("column exists");
        assert_eq!(resolved, "archived_count");
        assert!(matches!(
            resolve_column(&logical, "no_such_column"),
            Err(InputError::UnknownColumn { section, column })
                if section == "pg_stat_archiver" && column == "no_such_column"
        ));
    }

    fn sample_row(timestamp: i64, value: f64) -> OutRow {
        vec![
            ("ts".to_owned(), Value::Ts(timestamp)),
            ("c".to_owned(), Value::F64(value)),
        ]
    }

    #[test]
    fn equal_duplicate_rows_are_deduplicated() {
        let mut q = quality();
        let mut rows = vec![sample_row(1, 10.0), sample_row(1, 10.0)];
        let mut identity_bytes = 100;
        assert_eq!(
            normalize_duplicate_rows(
                &mut rows,
                &[],
                &["c"],
                &[],
                &[],
                &mut q,
                &mut IdentityBudget {
                    remaining: &mut identity_bytes,
                    limit: 100,
                },
            ),
            Ok(vec![DuplicateGaugeBreak {
                identity: Vec::new(),
                timestamp: 1,
            }])
        );
        assert_eq!(rows, vec![sample_row(1, 10.0)]);
        assert_eq!(q.duplicate_timestamps, 1);
    }

    #[test]
    fn conflicting_duplicate_rows_are_rejected_in_either_order() {
        let mut forward_quality = quality();
        let mut forward = vec![sample_row(1, 10.0), sample_row(1, 11.0)];
        let mut reverse_quality = quality();
        let mut reverse = vec![sample_row(1, 11.0), sample_row(1, 10.0)];
        let mut forward_bytes = 100;
        let mut reverse_bytes = 100;

        assert_eq!(
            normalize_duplicate_rows(
                &mut forward,
                &[],
                &["c"],
                &[],
                &[],
                &mut forward_quality,
                &mut IdentityBudget {
                    remaining: &mut forward_bytes,
                    limit: 100,
                },
            ),
            Err(NormalizeError::Conflict(1))
        );
        assert_eq!(
            normalize_duplicate_rows(
                &mut reverse,
                &[],
                &["c"],
                &[],
                &[],
                &mut reverse_quality,
                &mut IdentityBudget {
                    remaining: &mut reverse_bytes,
                    limit: 100,
                },
            ),
            Err(NormalizeError::Conflict(1))
        );
    }

    #[test]
    fn conflicting_snapshot_labels_are_not_silently_deduplicated() {
        let mut first = activity_row(100, 7, "active", 10, 50);
        first.push(cell("wait_event_type", Value::Str("IPC".to_owned())));
        first.push(cell("wait_event", Value::Str("SyncRep".to_owned())));
        let mut second = activity_row(100, 7, "active", 10, 50);
        second.push(cell("wait_event_type", Value::Str("Client".to_owned())));
        second.push(cell("wait_event", Value::Str("ClientRead".to_owned())));
        let mut rows = vec![first, second];
        let mut q = quality();
        let mut identity_bytes = 1_000;
        assert_eq!(
            normalize_duplicate_rows(
                &mut rows,
                &["pid"],
                &[],
                &[],
                &["wait_event_type", "wait_event"],
                &mut q,
                &mut IdentityBudget {
                    remaining: &mut identity_bytes,
                    limit: 1_000,
                },
            ),
            Err(NormalizeError::Conflict(100))
        );
    }

    #[test]
    fn distinct_timestamps_are_unchanged() {
        let mut q = quality();
        let expected = vec![sample_row(1, 1.0), sample_row(2, 2.0)];
        let mut rows = expected.clone();
        let mut identity_bytes = 100;
        assert_eq!(
            normalize_duplicate_rows(
                &mut rows,
                &[],
                &["c"],
                &[],
                &[],
                &mut q,
                &mut IdentityBudget {
                    remaining: &mut identity_bytes,
                    limit: 100,
                },
            ),
            Ok(Vec::new())
        );
        assert_eq!(rows, expected);
        assert_eq!(q.duplicate_timestamps, 0);
    }

    #[test]
    fn unresolved_identity_row_is_counted_and_removed() {
        let mut rows = vec![vec![
            ("ts".to_owned(), Value::Ts(1)),
            ("id".to_owned(), Value::Null),
            ("c".to_owned(), Value::F64(1.0)),
        ]];
        let mut q = quality();
        let mut identity_bytes = 100;

        assert_eq!(
            normalize_duplicate_rows(
                &mut rows,
                &["id"],
                &["c"],
                &[],
                &[],
                &mut q,
                &mut IdentityBudget {
                    remaining: &mut identity_bytes,
                    limit: 100,
                },
            ),
            Ok(Vec::new())
        );
        assert!(rows.is_empty());
        assert_eq!(q.non_canonical_identity, 1);
    }

    #[test]
    fn identity_bytes_are_charged_across_rows() {
        let row = |timestamp, identity: &str| {
            vec![
                ("ts".to_owned(), Value::Ts(timestamp)),
                ("id".to_owned(), Value::Str(identity.to_owned())),
                ("c".to_owned(), Value::F64(1.0)),
            ]
        };
        let mut rows = vec![row(1, "abc"), row(2, "def")];
        let mut q = quality();
        let mut identity_bytes = 20;

        assert_eq!(
            normalize_duplicate_rows(
                &mut rows,
                &["id"],
                &["c"],
                &[],
                &[],
                &mut q,
                &mut IdentityBudget {
                    remaining: &mut identity_bytes,
                    limit: 20,
                },
            ),
            Err(NormalizeError::IdentityByteLimit {
                observed: 24,
                limit: 20,
            })
        );
    }

    #[test]
    fn invalid_diff_points_split_retained_series() {
        let key = vec![Value::I64(1)];
        let diffs = vec![SeriesDiff {
            key: key.clone(),
            columns: vec![ColumnDiff {
                name: "c".to_owned(),
                points: vec![
                    DiffAt {
                        ts: 0,
                        point: DiffPoint::NoData {
                            reason: Reason::FirstPoint,
                        },
                    },
                    DiffAt {
                        ts: 60,
                        point: DiffPoint::Value {
                            delta: Scalar::Int(5),
                            rate: 5.0,
                            dt_micros: 60,
                        },
                    },
                    DiffAt {
                        ts: 120,
                        point: DiffPoint::Value {
                            delta: Scalar::Float(f64::NAN),
                            rate: f64::NAN,
                            dt_micros: 60,
                        },
                    },
                    DiffAt {
                        ts: 180,
                        point: DiffPoint::NoData {
                            reason: Reason::Reset,
                        },
                    },
                    DiffAt {
                        ts: 240,
                        point: DiffPoint::Value {
                            delta: Scalar::Int(7),
                            rate: 7.0,
                            dt_micros: 60,
                        },
                    },
                ],
            }],
        }];
        let series = column_series(&diffs, &[], &key, "s", "c", 10, 10)
            .expect("valid series")
            .expect("nonempty series");
        assert_eq!(series.runs().len(), 2);
        assert_eq!(series.runs()[0].ts(), &[60]);
        assert_eq!(series.runs()[1].ts(), &[240]);
    }

    #[test]
    fn point_limit_is_checked_while_building_a_series() {
        let key = vec![Value::I64(1)];
        let diffs = vec![SeriesDiff {
            key: key.clone(),
            columns: vec![ColumnDiff {
                name: "c".to_owned(),
                points: vec![
                    DiffAt {
                        ts: 1,
                        point: DiffPoint::Value {
                            delta: Scalar::Int(1),
                            rate: 1.0,
                            dt_micros: 1,
                        },
                    },
                    DiffAt {
                        ts: 2,
                        point: DiffPoint::Value {
                            delta: Scalar::Int(1),
                            rate: 1.0,
                            dt_micros: 1,
                        },
                    },
                ],
            }],
        }];

        assert!(matches!(
            column_series(&diffs, &[], &key, "s", "c", 1, 1),
            Err(InputError::SeriesLimit {
                observed: 2,
                limit: 1,
            })
        ));
    }

    #[test]
    fn typed_gauge_point_admission_rejects_retention_all_or_nothing() {
        let mut limits = InputLimits::for_test();
        limits.typed_gauge_points = 2;
        let mut state = BuildState::new(
            &limits,
            limits.identity_bytes,
            Vec::new(),
            SnapshotProvenance::from_page(None),
        );
        let series = vec![SeriesValues {
            key: Vec::new(),
            columns: vec![ColumnValues {
                name: "mem_available".to_owned(),
                points: vec![(1, 1.0), (2, 2.0), (3, 3.0)],
                breaks: Vec::new(),
                skipped: 0,
            }],
        }];

        assert_eq!(
            state.admit_typed_gauge_points("os_meminfo", &series, &limits),
            None
        );
        assert_eq!(
            state.skipped,
            vec![SectionSkip {
                section: "os_meminfo",
                reason: SkipReason::TypedGaugePointLimit {
                    observed: 3,
                    limit: 2,
                },
            }]
        );
        assert_eq!(state.remaining_typed_gauge_points, 2);
    }

    #[test]
    fn duplicate_series_is_a_typed_adapter_error() {
        let mut series = SeriesSet::for_test(10);
        insert_series(
            &mut series,
            "s",
            "c",
            vec![IdentityValue::I64(1)],
            Series::new(vec![1], vec![1.0]).expect("valid series"),
        )
        .expect("first insert");

        assert!(matches!(
            insert_series(
                &mut series,
                "s",
                "c",
                vec![IdentityValue::I64(1)],
                Series::new(vec![2], vec![2.0]).expect("valid series"),
            ),
            Err(InputError::DuplicateSeries {
                section: "s",
                column: "c",
            })
        ));
    }

    #[test]
    fn invalid_gauge_point_splits_retained_series() {
        let key = vec![Value::I64(1)];
        let gauges = vec![SeriesValues {
            key: key.clone(),
            columns: vec![ColumnValues {
                name: "g".to_owned(),
                points: vec![(0, 1.0), (120, 2.0)],
                breaks: vec![60],
                skipped: 1,
            }],
        }];
        let series = column_series(&[], &gauges, &key, "s", "g", 10, 10)
            .expect("valid series")
            .expect("nonempty series");
        assert_eq!(series.runs().len(), 2);
        assert_eq!(series.runs()[0].values(), &[1.0]);
        assert_eq!(series.runs()[1].values(), &[2.0]);
    }

    #[test]
    fn diff_exclusions_have_distinct_quality_counts() {
        let reasons = [
            Reason::FirstPoint,
            Reason::Reset,
            Reason::Gap,
            Reason::NotCollected,
            Reason::Anomaly,
        ];
        let diffs = vec![SeriesDiff {
            key: Vec::new(),
            columns: vec![ColumnDiff {
                name: "c".to_owned(),
                points: reasons
                    .into_iter()
                    .enumerate()
                    .map(|(timestamp, reason)| DiffAt {
                        ts: i64::try_from(timestamp).expect("small timestamp"),
                        point: DiffPoint::NoData { reason },
                    })
                    .collect(),
            }],
        }];
        let mut q = quality();

        tally_quality(&diffs, &[], &mut q);

        assert_eq!(q.first_points, 1);
        assert_eq!(q.resets, 1);
        assert_eq!(q.gaps, 1);
        assert_eq!(q.not_collected, 1);
        assert_eq!(q.anomalous_points, 1);
    }

    fn write_archiver_segment(
        path: &std::path::Path,
        rows: &[kronika_registry::pg_stat_archiver::PgStatArchiver],
        min_ts: i64,
        max_ts: i64,
    ) {
        use kronika_format::{DictLimits, PartMeta, SectionInput};
        use kronika_registry::instance_metadata::InstanceMetadata;
        use kronika_registry::{Section, StrId, Ts};

        let mut interner = kronika_writer::Interner::new(
            DictLimits::new(4096, 1 << 20).expect("dictionary limits"),
        );
        let mut intern = |value: &str| {
            interner
                .intern(value.as_bytes())
                .map(|id| StrId(id.get()))
                .expect("intern fixture identity")
        };
        let metadata = InstanceMetadata {
            ts: Ts(min_ts),
            hostname: intern("db-host-7"),
            node_self_id: intern("node-7"),
            pg_version_num: 170_000,
            kernel_version: intern("test-kernel"),
            pg_system_identifier: Some(7),
            clock_ticks_per_sec: 100,
            page_size_bytes: 4096,
            boot_id: intern("test-boot"),
            btime: Ts(0),
        };
        let dictionary =
            kronika_writer::dict::encode(interner.window()).expect("encode dictionary");
        let archiver = kronika_registry::pg_stat_archiver::PgStatArchiver::encode(rows)
            .expect("encode archiver");
        let metadata = InstanceMetadata::encode(&[metadata]).expect("encode metadata");
        let mut sections: Vec<SectionInput<'_>> = dictionary
            .iter()
            .map(|section| SectionInput {
                type_id: section.type_id,
                rows: section.rows,
                body: &section.body,
            })
            .collect();
        sections.push(SectionInput {
            type_id: 1_008_001,
            rows: u32::try_from(rows.len()).expect("fixture row count"),
            body: &archiver,
        });
        sections.push(SectionInput {
            type_id: 1_021_001,
            rows: 1,
            body: &metadata,
        });
        let bytes = kronika_format::build_part(
            &sections,
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        );
        std::fs::write(path, bytes).expect("write segment");
    }

    /// Split forty per-minute archiver snapshots across two segments; the
    /// counter climbs by one per minute except minutes 20..25, where it climbs
    /// by fifty. Returns the last snapshot time.
    fn write_two_segment_spike(dir: &std::path::Path) -> i64 {
        use kronika_registry::Ts;
        use kronika_registry::pg_stat_archiver::PgStatArchiver;

        let row = |ts: i64, archived: i64| PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        };

        let mut count = 0_i64;
        let mut rows = Vec::new();
        for minute in 0..40_i64 {
            count += if (20..25).contains(&minute) { 50 } else { 1 };
            rows.push(row(minute * MINUTE, count));
        }

        write_archiver_segment(&dir.join("0.pgm"), &rows[..21], 0, 20 * MINUTE);
        write_archiver_segment(&dir.join("2000.pgm"), &rows[20..], 20 * MINUTE, 39 * MINUTE);
        39 * MINUTE
    }

    fn spike_scan(to: i64) -> ScanParams {
        ScanParams {
            from: 0,
            to,
            window: 6 * MINUTE,
            step: 2 * MINUTE,
            threshold: 3.5,
            eps_rel: 0.05,
        }
    }

    #[test]
    fn a_real_two_segment_spike_becomes_an_incident_with_matching_members() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_two_segment_spike(dir.path());
        let mut snap = LocalDirSnapshot::open(dir.path()).expect("open two-segment snapshot");

        let scan = spike_scan(to);
        let prepared = prepare_input(
            &mut snap,
            7,
            &scan,
            &["pg_stat_archiver"],
            &InputLimits::for_test(),
        )
        .expect("prepare input from real segments");

        assert!(
            prepared.skipped.is_empty(),
            "the two segments read and scan cleanly: {:?}",
            prepared.skipped
        );
        assert_eq!(prepared.source_id, 7);
        assert_eq!(prepared.node_self_id, "node-7");
        assert_eq!(prepared.quality.duplicate_timestamps, 1);
        let episode = prepared
            .episodes
            .iter()
            .find(|episode| {
                episode.reference.logical_section == "pg_stat_archiver"
                    && episode.reference.column == "archived_count"
            })
            .expect("the archived_count spike surfaces as an episode");
        assert!(
            episode.reference.identity.is_empty(),
            "pg_stat_archiver is a singleton series"
        );
        assert_eq!(
            episode.episode.peak.dir,
            kronika_analytics::Direction::Up,
            "the counter jump is an upward rate anomaly"
        );
        let (start_us, end_us) = (episode.reference.start_us, episode.reference.end_us);

        let config = IncidentConfig::for_test(
            &prepared.node_self_id,
            MINUTE,
            3_600 * MINUTE,
            ClockRelation::Unknown,
        );
        let outcome = analyze(
            prepared.episodes,
            &prepared.series,
            &prepared.typed,
            &[],
            &config,
        )
        .expect("engine analyzes prepared input");

        assert_eq!(
            outcome.incidents.len(),
            1,
            "one contiguous spike yields one incident cluster"
        );
        let incident = &outcome.incidents[0];
        assert!(
            incident
                .members
                .iter()
                .any(|member| member.logical_section == "pg_stat_archiver"
                    && member.column == "archived_count"),
            "the incident's members carry the real anomalous series"
        );
        assert!(
            incident.start_us <= start_us && incident.end_us >= end_us,
            "the incident interval covers the episode it was built from"
        );
        assert!(
            outcome.complete,
            "no lens is registered, so nothing is skipped"
        );
        assert!(
            incident.findings.is_empty(),
            "no lens catalog yet, so the cluster carries no findings"
        );
    }

    #[test]
    fn a_calm_two_segment_series_yields_no_incident() {
        use kronika_registry::Ts;
        use kronika_registry::pg_stat_archiver::PgStatArchiver;

        let dir = tempfile::tempdir().expect("tempdir");
        let row = |ts: i64, archived: i64| PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        };
        let rows: Vec<PgStatArchiver> = (0..40_i64).map(|m| row(m * MINUTE, m + 1)).collect();
        write_archiver_segment(&dir.path().join("0.pgm"), &rows[..21], 0, 20 * MINUTE);
        write_archiver_segment(
            &dir.path().join("2000.pgm"),
            &rows[20..],
            20 * MINUTE,
            39 * MINUTE,
        );

        let mut snap = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let scan = spike_scan(39 * MINUTE);
        let prepared = prepare_input(
            &mut snap,
            7,
            &scan,
            &["pg_stat_archiver"],
            &InputLimits::for_test(),
        )
        .expect("prepare input");

        assert_eq!(prepared.node_self_id, "node-7");
        let config = IncidentConfig::for_test(
            &prepared.node_self_id,
            MINUTE,
            3_600 * MINUTE,
            ClockRelation::Unknown,
        );
        let outcome = analyze(
            prepared.episodes,
            &prepared.series,
            &prepared.typed,
            &[],
            &config,
        )
        .expect("analyze");
        assert!(
            outcome.incidents.is_empty(),
            "a steady counter produces no anomaly and no incident"
        );
    }

    #[test]
    fn materialization_limit_applies_to_the_whole_batch() {
        use kronika_registry::Ts;
        use kronika_registry::pg_stat_archiver::PgStatArchiver;

        let dir = tempfile::tempdir().expect("tempdir");
        let rows = [
            PgStatArchiver {
                ts: Ts(0),
                archived_count: 1,
                last_archived_wal: None,
                last_archived_time: None,
                failed_count: 0,
                last_failed_wal: None,
                last_failed_time: None,
                stats_reset: None,
            },
            PgStatArchiver {
                ts: Ts(MINUTE),
                archived_count: 2,
                last_archived_wal: None,
                last_archived_time: None,
                failed_count: 0,
                last_failed_wal: None,
                last_failed_time: None,
                stats_reset: None,
            },
        ];
        write_archiver_segment(&dir.path().join("0.pgm"), &rows, 0, MINUTE);
        let mut snap = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let mut limits = InputLimits::for_test();
        limits.materialized_cells = logical_section("instance_metadata")
            .expect("metadata contract")
            .columns
            .len();
        let scan = ScanParams {
            from: 0,
            to: MINUTE,
            window: MINUTE,
            step: MINUTE,
            threshold: 3.5,
            eps_rel: 0.05,
        };

        let prepared = prepare_input(&mut snap, 7, &scan, &["pg_stat_archiver"], &limits)
            .expect("metadata fits and oversized data is skipped");
        assert_eq!(
            prepared.skipped,
            vec![
                SectionSkip {
                    section: "pg_stat_archiver",
                    reason: SkipReason::MaterializationLimit {
                        limit: limits.materialized_cells,
                    },
                },
                SectionSkip {
                    section: SNAPSHOT_COVERAGE,
                    reason: SkipReason::MaterializationLimit {
                        limit: limits.materialized_cells,
                    },
                },
            ]
        );
        assert!(prepared.episodes.is_empty());
    }

    #[test]
    fn overlapping_units_are_admitted_before_section_reads() {
        use kronika_registry::Ts;
        use kronika_registry::pg_stat_archiver::PgStatArchiver;

        let dir = tempfile::tempdir().expect("tempdir");
        let rows = [PgStatArchiver {
            ts: Ts(0),
            archived_count: 1,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }];
        write_archiver_segment(&dir.path().join("0.pgm"), &rows, 0, MINUTE);
        let mut snap = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let mut limits = InputLimits::for_test();
        limits.units = 0;

        let scan = ScanParams {
            from: 0,
            to: MINUTE,
            window: MINUTE,
            step: MINUTE,
            threshold: 3.5,
            eps_rel: 0.05,
        };
        let error = prepare_input(&mut snap, 7, &scan, &["pg_stat_archiver"], &limits)
            .err()
            .expect("unit admission rejects before metadata is required");
        assert!(matches!(
            error,
            InputError::UnitLimit {
                observed: 1,
                limit: 0,
            }
        ));
    }

    fn cell(name: &str, value: Value) -> (String, Value) {
        (name.to_owned(), value)
    }

    fn activity_row(ts: i64, pid: i64, state: &str, xmin_age: i64, xact_start: i64) -> OutRow {
        vec![
            cell("ts", Value::Ts(ts)),
            cell("pid", Value::I64(pid)),
            cell("backend_start", Value::Ts(10)),
            cell("backend_xid_age", Value::I64(11)),
            cell("state", Value::Str(state.to_owned())),
            cell("backend_xmin_age", Value::I64(xmin_age)),
            cell("xact_start", Value::Ts(xact_start)),
        ]
    }

    #[test]
    fn activity_snapshots_group_rows_by_collection_time() {
        let rows = vec![
            activity_row(100, 1, "active", 5, 40),
            activity_row(100, 2, "idle in transaction", 9, 30),
            activity_row(200, 1, "active", 7, 150),
        ];
        let provenance = SnapshotProvenance::from_page(None);
        let snapshots = build_activity_snapshots(&rows, &provenance);
        assert_eq!(snapshots.len(), 2, "two distinct timestamps, two snapshots");
        assert_eq!(snapshots[0].ts, 100);
        assert_eq!(snapshots[0].backends.len(), 2);
        assert_eq!(snapshots[1].ts, 200);
        assert_eq!(snapshots[1].backends.len(), 1);
    }

    #[test]
    fn activity_backend_reads_labels_and_transaction_age() {
        let mut row = activity_row(1_000, 7, "idle in transaction", 42, 400);
        row.push(cell("wait_event_type", Value::Str("Client".to_owned())));
        row.push(cell("wait_event", Value::Str("ClientRead".to_owned())));
        let backend = activity_backend(&row, 1_000);
        assert_eq!(backend.xmin_age, Some(42));
        assert_eq!((backend.pid, backend.backend_start), (7, 10));
        assert_eq!(backend.xid_age, Some(11));
        assert_eq!(backend.state.as_deref(), Some("idle in transaction"));
        assert_eq!(backend.wait_event_type.as_deref(), Some("Client"));
        assert_eq!(backend.wait_event.as_deref(), Some("ClientRead"));
        assert_eq!(backend.xact_age_us, Some(600), "1000 - 400");
    }

    #[test]
    fn activity_backend_drops_a_transaction_start_after_the_snapshot() {
        // A start after the snapshot time is a clock disagreement, not a
        // negative-duration transaction.
        let backend = activity_backend(&activity_row(500, 1, "active", 3, 900), 500);
        assert_eq!(backend.xact_age_us, None);
    }

    fn snapshot_marker_row(
        ts: i64,
        read_state: u64,
        visibility: u64,
        source_total: u64,
        collected: u64,
    ) -> OutRow {
        vec![
            cell("ts", Value::Ts(ts)),
            cell("source_type_id", Value::U64(1_001_003)),
            cell("collector_pid", Value::U64(42)),
            cell("collector_started_at", Value::Ts(1)),
            cell("read_state", Value::U64(read_state)),
            cell("visibility", Value::U64(visibility)),
            cell("source_total", Value::U64(source_total)),
            cell("collected", Value::U64(collected)),
        ]
    }

    fn snapshot_marker_page(rows: Vec<OutRow>) -> SectionPage {
        SectionPage {
            section: SNAPSHOT_COVERAGE.to_owned(),
            source_id: 7,
            rows,
            gaps: Vec::new(),
            next_cursor: None,
        }
    }

    #[test]
    fn complete_marker_is_required_for_an_activity_denominator() {
        let page = snapshot_marker_page(vec![snapshot_marker_row(100, 0, 0, 2, 2)]);
        let provenance = SnapshotProvenance::from_page(Some(&page));
        assert_eq!(
            provenance.activity_snapshot(100),
            SnapshotCompleteness::Complete
        );
        assert_eq!(
            provenance.activity_capability(),
            CapabilityInputState::Available
        );
    }

    #[test]
    fn restricted_visibility_keeps_positive_rows_but_withholds_denominators() {
        let page = snapshot_marker_page(vec![snapshot_marker_row(100, 0, 1, 2, 2)]);
        let provenance = SnapshotProvenance::from_page(Some(&page));
        assert_eq!(
            provenance.activity_snapshot(100),
            SnapshotCompleteness::Restricted
        );
        assert_eq!(
            provenance.activity_capability(),
            CapabilityInputState::Partial
        );
    }

    #[test]
    fn source_limit_or_read_failure_is_typed_as_not_collected() {
        for read_state in [1, 2, 3, 4] {
            let page =
                snapshot_marker_page(vec![snapshot_marker_row(100, read_state, 2, 4_097, 0)]);
            let provenance = SnapshotProvenance::from_page(Some(&page));
            assert_eq!(
                provenance.activity_snapshot(100),
                SnapshotCompleteness::Unknown
            );
            assert_eq!(
                provenance.activity_capability(),
                CapabilityInputState::NotCollected
            );
        }
    }

    #[test]
    fn old_segments_and_conflicting_markers_are_coverage_unknown() {
        let old = SnapshotProvenance::from_page(None);
        assert_eq!(old.activity_snapshot(100), SnapshotCompleteness::Unknown);
        assert_eq!(old.activity_capability(), CapabilityInputState::Partial);

        let page = snapshot_marker_page(vec![
            snapshot_marker_row(100, 0, 0, 2, 2),
            snapshot_marker_row(100, 0, 1, 2, 2),
        ]);
        let conflicting = SnapshotProvenance::from_page(Some(&page));
        assert_eq!(
            conflicting.activity_snapshot(100),
            SnapshotCompleteness::Unknown
        );
        assert_eq!(
            conflicting.activity_capability(),
            CapabilityInputState::Partial
        );
    }

    #[test]
    fn activity_backend_maps_missing_fields_to_none() {
        let row = vec![cell("ts", Value::Ts(1)), cell("pid", Value::I64(9))];
        let backend = activity_backend(&row, 1);
        assert_eq!(backend.xmin_age, None);
        assert_eq!(backend.state, None);
        assert_eq!(backend.wait_event, None);
        assert_eq!(backend.xact_age_us, None);
    }

    #[test]
    fn activity_rows_without_a_timestamp_are_dropped() {
        let rows = vec![vec![
            cell("pid", Value::I64(1)),
            cell("state", Value::Str("active".to_owned())),
        ]];
        let provenance = SnapshotProvenance::from_page(None);
        assert!(build_activity_snapshots(&rows, &provenance).is_empty());
    }

    fn lock_row(ts: i64, pid: i64, blocked_by: Vec<i32>) -> OutRow {
        vec![
            cell("ts", Value::Ts(ts)),
            cell("pid", Value::I64(pid)),
            cell("blocked_by", Value::ListI32(blocked_by)),
        ]
    }

    #[test]
    fn lock_snapshots_expand_blocked_by_into_edges() {
        let rows = vec![
            lock_row(100, 10, vec![]),      // root: no edge
            lock_row(100, 20, vec![10, 0]), // waiter blocked by pid 10 and a prepared xact
        ];
        let snapshots = build_lock_snapshots(&rows);
        assert_eq!(snapshots.len(), 1, "root alone carries no edge snapshot");
        assert_eq!(
            snapshots[0].edges,
            vec![
                LockEdge {
                    waiter_pid: 20,
                    blocker_pid: 0,
                },
                LockEdge {
                    waiter_pid: 20,
                    blocker_pid: 10,
                },
            ]
        );
    }

    #[test]
    fn lock_snapshots_are_absent_without_contention() {
        let rows = vec![lock_row(100, 10, vec![]), lock_row(100, 11, vec![])];
        assert!(
            build_lock_snapshots(&rows).is_empty(),
            "only roots means no contention and no snapshot"
        );
    }

    #[test]
    fn empty_lock_page_is_not_claimed_as_complete_collection() {
        assert_eq!(
            capability_state(PG_LOCKS, &[]),
            Some(CapabilityInputState::NotCollected)
        );
        assert_eq!(
            capability_state(PG_LOCKS, &[lock_row(100, 10, vec![])]),
            Some(CapabilityInputState::Available)
        );
    }

    fn build_state(snapshot_rows: usize) -> BuildState {
        let mut limits = InputLimits::for_test();
        limits.snapshot_rows = snapshot_rows;
        BuildState::new(
            &limits,
            limits.identity_bytes,
            Vec::new(),
            SnapshotProvenance::from_page(None),
        )
    }

    #[test]
    fn a_section_within_the_row_ceiling_is_retained_whole() {
        let mut state = build_state(10);
        let rows = vec![
            activity_row(100, 1, "active", 5, 40),
            activity_row(100, 2, "active", 6, 40),
        ];
        state.retain_snapshots("pg_stat_activity", &rows);
        assert_eq!(state.quality.snapshot_rows_withheld, 0);
        assert_eq!(
            state.typed.activity_window(i64::MIN, i64::MAX).count(),
            1,
            "one snapshot retained"
        );
    }

    #[test]
    fn a_section_over_the_row_ceiling_is_withheld_whole_and_counted() {
        let mut state = build_state(1);
        let rows = vec![
            activity_row(100, 1, "active", 5, 40),
            activity_row(100, 2, "active", 6, 40),
        ];
        state.retain_snapshots("pg_stat_activity", &rows);
        assert_eq!(
            state.quality.snapshot_rows_withheld, 2,
            "two backends exceed the ceiling of one, so both are withheld"
        );
        assert_eq!(
            state.typed.activity_window(i64::MIN, i64::MAX).count(),
            0,
            "nothing is sampled when the section does not fit"
        );
    }

    #[test]
    fn lock_edges_are_charged_against_the_row_ceiling() {
        let mut state = build_state(1);
        // One waiter blocked by two holders is two edges, over the ceiling of one.
        state.retain_snapshots(
            "pg_locks",
            &[
                lock_row(100, 10, vec![]),
                lock_row(100, 30, vec![]),
                lock_row(100, 20, vec![10, 30]),
            ],
        );
        assert_eq!(state.quality.snapshot_rows_withheld, 2);
        assert_eq!(state.typed.lock_window(i64::MIN, i64::MAX).count(), 0);
    }

    #[test]
    fn lock_snapshot_with_missing_or_self_endpoint_is_withheld() {
        assert!(build_lock_snapshots(&[lock_row(100, 20, vec![10])]).is_empty());
        assert!(build_lock_snapshots(&[lock_row(100, 20, vec![20])]).is_empty());
    }

    #[test]
    fn duplicate_lock_edges_are_deduplicated_deterministically() {
        let snapshots =
            build_lock_snapshots(&[lock_row(100, 10, vec![]), lock_row(100, 20, vec![10, 10])]);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].edges.len(), 1);
    }

    fn error_row(severity: u64, sqlstate: Option<&str>, count: u64) -> OutRow {
        vec![
            ("ts".to_owned(), Value::Ts(0)),
            ("severity".to_owned(), Value::U64(severity)),
            (
                "sqlstate".to_owned(),
                sqlstate.map_or(Value::Null, |code| Value::Str(code.to_owned())),
            ),
            ("count".to_owned(), Value::U64(count)),
        ]
    }

    fn lifecycle_row(kind: u64, signal: Option<i32>) -> OutRow {
        vec![
            ("ts".to_owned(), Value::Ts(0)),
            ("kind".to_owned(), Value::U64(kind)),
            (
                "signal".to_owned(),
                signal.map_or(Value::Null, |value| Value::I64(i64::from(value))),
            ),
        ]
    }

    fn log_page(section: &str, rows: Vec<OutRow>) -> SectionPage {
        SectionPage {
            section: section.to_owned(),
            source_id: 7,
            rows,
            gaps: Vec::new(),
            next_cursor: None,
        }
    }

    fn event_findings(events: &LogEventInputs) -> Vec<(String, Vec<IdentityValue>)> {
        use crate::incident::{EventConfig, EventLens, evaluate_events, event_catalog};
        let catalog = event_catalog();
        let lenses: Vec<&dyn EventLens> = catalog.iter().map(AsRef::as_ref).collect();
        let outcome = evaluate_events(events, &lenses, &EventConfig::production()).expect("valid");
        outcome
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.lens_id().to_owned(),
                    finding.scope().identity().to_vec(),
                )
            })
            .collect()
    }

    #[test]
    fn value_readers_accept_both_integer_widths_and_reject_the_rest() {
        let row = vec![
            ("u".to_owned(), Value::U64(5)),
            ("i".to_owned(), Value::I64(7)),
            ("neg".to_owned(), Value::I64(-1)),
            ("big".to_owned(), Value::U64(300)),
            ("s".to_owned(), Value::Str("40P01".to_owned())),
            ("null".to_owned(), Value::Null),
        ];
        assert_eq!(read_u8(&row, "u"), Some(5));
        assert_eq!(read_u8(&row, "i"), Some(7));
        assert_eq!(read_u8(&row, "neg"), None, "a negative does not fit u8");
        assert_eq!(read_u8(&row, "big"), None, "300 does not fit u8");
        assert_eq!(read_u64(&row, "i"), Some(7));
        assert_eq!(read_u64(&row, "neg"), None);
        assert_eq!(read_i32(&row, "u"), Some(5));
        assert_eq!(read_str(&row, "s"), Some("40P01".to_owned()));
        assert_eq!(read_str(&row, "null"), None);
        assert_eq!(read_u8(&row, "missing"), None);
        assert_eq!(read_u8(&row, "s"), None, "a string is not an integer");
    }

    #[test]
    fn lifecycle_kind_maps_only_known_codes() {
        assert_eq!(lifecycle_kind(0), Some(LifecycleKind::Crash));
        assert_eq!(lifecycle_kind(1), Some(LifecycleKind::Shutdown));
        assert_eq!(lifecycle_kind(2), Some(LifecycleKind::Ready));
        assert_eq!(lifecycle_kind(3), None);
    }

    #[test]
    fn error_groups_keep_batch_boundaries_but_duplicate_facts_are_suppressed() {
        let page = log_page(
            "pg_log_errors",
            vec![
                error_row(0, Some("40P01"), 2),
                error_row(0, Some("40P01"), 3),
                error_row(2, None, 1),
            ],
        );
        let mut events = LogEventInputs::new(EventInputLimits::production());
        ingest_log_errors(Some(&page), &[], &mut events, -1, 1);

        assert_eq!(
            events.coverage().get("pg_log_errors"),
            Some(&LogCoverage::Unknown),
            "a clean read is unknown, never proven-complete",
        );
        let facts = event_findings(&events);
        assert_eq!(
            facts.iter().filter(|(id, _)| id == "PG-EVT-007").count(),
            1,
            "identical public facts are deduplicated without merging stored counts: {facts:?}",
        );
        assert!(
            facts
                .iter()
                .filter(|(id, _)| id == "PG-EVT-007")
                .all(|(_, identity)| identity
                    == &vec![
                        IdentityValue::Text("40P01".to_owned()),
                        IdentityValue::I64(0),
                    ])
        );
        assert!(
            facts.iter().any(|(id, _)| id == "PG-EVT-003"),
            "the panic severity is its own fact",
        );
    }

    #[test]
    fn a_skipped_error_section_is_not_collected_and_yields_no_fact() {
        let page = log_page("pg_log_errors", vec![error_row(0, Some("40P01"), 1)]);
        let mut events = LogEventInputs::new(EventInputLimits::production());
        ingest_log_errors(
            Some(&page),
            &[SectionSkip {
                section: "pg_log_errors",
                reason: SkipReason::IncompletePage,
            }],
            &mut events,
            -1,
            1,
        );
        assert_eq!(
            events.coverage().get("pg_log_errors"),
            Some(&LogCoverage::NotCollected),
        );
        assert!(
            event_findings(&events).is_empty(),
            "a partial section is not mined for facts",
        );
    }

    #[test]
    fn a_missing_page_is_not_collected() {
        let events = build_log_events(&BTreeMap::new(), &[], EventInputLimits::production(), -1, 1);
        assert_eq!(
            events.coverage().get("pg_log_errors"),
            Some(&LogCoverage::NotCollected),
        );
        assert_eq!(
            events.coverage().get("pg_log_lifecycle"),
            Some(&LogCoverage::NotCollected),
        );
        assert!(event_findings(&events).is_empty());
    }

    #[test]
    fn lifecycle_signals_become_sigkill_and_crash_facts() {
        let page = log_page(
            "pg_log_lifecycle",
            vec![
                lifecycle_row(0, Some(9)),
                lifecycle_row(0, Some(11)),
                lifecycle_row(2, None),
            ],
        );
        let mut events = LogEventInputs::new(EventInputLimits::production());
        ingest_log_lifecycle(Some(&page), &[], &mut events, -1, 1);
        let ids: Vec<_> = event_findings(&events)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(ids.contains(&"PG-EVT-001".to_owned()), "sigkill: {ids:?}");
        assert!(ids.contains(&"PG-EVT-002".to_owned()), "crash: {ids:?}");
    }

    #[test]
    fn a_coverage_gap_downgrades_the_section() {
        let mut page = log_page("pg_log_errors", vec![error_row(0, Some("40P01"), 1)]);
        page.gaps = vec![Gap { from: 0, to: 5 }];
        let mut events = LogEventInputs::new(EventInputLimits::production());
        ingest_log_errors(Some(&page), &[], &mut events, -1, 1);
        assert_eq!(
            events.coverage().get("pg_log_errors"),
            Some(&LogCoverage::Gap),
        );
    }

    #[test]
    fn log_event_adapter_uses_the_half_open_request_window() {
        let at = |ts| {
            let mut row = error_row(0, Some("40P01"), 1);
            row[0].1 = Value::Ts(ts);
            row
        };
        let page = log_page(PG_LOG_ERRORS, vec![at(-1), at(0), at(9), at(10)]);
        let mut events = LogEventInputs::new(EventInputLimits::production());
        ingest_log_errors(Some(&page), &[], &mut events, 0, 10);
        let facts = event_findings(&events);
        assert_eq!(facts.iter().filter(|(id, _)| id == "PG-EVT-007").count(), 2);
        let timestamps: BTreeSet<_> = facts
            .iter()
            .filter_map(|(_, identity)| match identity.get(1) {
                Some(IdentityValue::I64(ts)) => Some(*ts),
                _ => None,
            })
            .collect();
        assert_eq!(timestamps, BTreeSet::from([0, 9]));
    }

    #[test]
    fn sensitive_log_text_never_enters_generic_incident_identity() {
        let secret = "select * from users where password='hunter2' -- 10.0.0.7 /var/lib/postgresql appdb alice";
        let mut row = error_row(0, Some("40P01"), 1);
        row.push(("pattern".to_owned(), Value::Str(secret.to_owned())));
        row.push(("sample".to_owned(), Value::Str(secret.to_owned())));
        row.push(("statement".to_owned(), Value::Str(secret.to_owned())));
        row.push(("database".to_owned(), Value::Str("appdb".to_owned())));
        row.push(("user".to_owned(), Value::Str("alice".to_owned())));
        let page = log_page(PG_LOG_ERRORS, vec![row]);
        let logical = logical_section(PG_LOG_ERRORS).expect("registered log section");
        let limits = InputLimits::for_test();
        let mut state = BuildState::new(
            &limits,
            limits.identity_bytes,
            Vec::new(),
            SnapshotProvenance::from_page(None),
        );
        let gates = Gates::from_pages(std::slice::from_ref(&logical), &BTreeMap::new());
        let scan = ScanParams {
            from: 0,
            to: 10,
            window: 5,
            step: 1,
            threshold: 3.5,
            eps_rel: 0.05,
        };
        state
            .process(&logical, page.clone(), &gates, &scan, &limits)
            .expect("typed log section is accepted");
        assert!(
            state.episodes.is_empty(),
            "log patterns never form episode keys"
        );
        assert_eq!(state.quality.evaluated_positions, 0);

        let mut pages = BTreeMap::new();
        pages.insert(PG_LOG_ERRORS.to_owned(), page);
        let events = build_log_events(&pages, &[], EventInputLimits::production(), -1, 1);
        let rendered = format!("{:?}", event_findings(&events));
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("10.0.0.7"));
    }
}
