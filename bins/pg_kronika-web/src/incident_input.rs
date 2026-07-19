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
    EnrichedEpisode, EpisodeRefV1, GaugeQuality, GaugeTrackInput, IdentityValue, Series,
    SeriesError, SeriesInsertError, SeriesSet, TypedInputs,
};

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
}

/// Request ceilings for adapter-owned work and data.
pub(crate) struct InputLimits {
    units: usize,
    sections: usize,
    materialized_cells: usize,
    series_points: usize,
    typed_gauge_points: usize,
    identity_bytes: usize,
    positions: usize,
    score_work: usize,
    episodes: usize,
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
            series_points: 500_000,
            typed_gauge_points: 500_000,
            identity_bytes: 1 << 20,
            positions: 10_000,
            score_work: crate::anomaly::MAX_SCORE_WORK,
            episodes: 20_000,
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
            series_points: 1_000_000,
            typed_gauge_points: 1_000_000,
            identity_bytes: 1 << 20,
            positions: 10_000,
            score_work: crate::anomaly::MAX_SCORE_WORK,
            episodes: 50,
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
}

/// Owned engine input, coverage, and exclusion counters.
pub(crate) struct PreparedInput {
    pub source_id: u64,
    pub node_self_id: String,
    pub episodes: Vec<EnrichedEpisode>,
    pub series: SeriesSet,
    pub typed: TypedInputs,
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
    let input = read_input_pages(snap, source, scan, sections, limits)?;
    let mut remaining_identity_bytes = limits.identity_bytes;
    let node_self_id = load_node_identity(
        &input.metadata,
        &mut remaining_identity_bytes,
        limits.identity_bytes,
    )?;
    let gates = Gates::from_pages(&input.logicals, &input.pages);
    let mut state = BuildState::new(limits, remaining_identity_bytes, input.skipped);
    for logical in &input.logicals {
        let Some(page) = input.pages.get(logical.name).cloned() else {
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
        coverage_by_section: state.coverage_by_section,
        quality: state.quality,
        skipped: state.skipped,
        capability_by_section: state.capability_by_section,
    })
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
        QueryLimits::new(DIFF_MAX_ROWS, remaining_cells),
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
        QueryLimits::new(DIFF_MAX_ROWS, remaining_cells),
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
        )?,
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
) -> Result<(BTreeMap<String, SectionPage>, Vec<SectionSkip>), InputError> {
    let mut pages = BTreeMap::new();
    let mut skipped = Vec::new();
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
            QueryLimits::new(DIFF_MAX_ROWS, remaining_cells),
        ) {
            Ok(page) => {
                charge_materialized_cells(&page, &mut remaining_cells, materialization_limit)?;
                pages.insert(name.to_owned(), page);
            }
            Err(QueryError::ResultTooLarge { .. }) => {
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

struct BuildState {
    episodes: Vec<EnrichedEpisode>,
    series: SeriesSet,
    typed: TypedInputs,
    coverage_by_section: BTreeMap<&'static str, Vec<Gap>>,
    quality: InputQuality,
    skipped: Vec<SectionSkip>,
    remaining_identity_bytes: usize,
    remaining_scan: usize,
    remaining_typed_gauge_points: usize,
    capability_by_section: BTreeMap<&'static str, CapabilityInputState>,
}

impl BuildState {
    fn new(
        limits: &InputLimits,
        remaining_identity_bytes: usize,
        skipped: Vec<SectionSkip>,
    ) -> Self {
        Self {
            episodes: Vec::new(),
            series: SeriesSet::new(limits.series_points),
            typed: TypedInputs::new(),
            coverage_by_section: BTreeMap::new(),
            quality: InputQuality::default(),
            skipped,
            remaining_identity_bytes,
            remaining_scan: limits.score_work,
            remaining_typed_gauge_points: limits.typed_gauge_points,
            capability_by_section: BTreeMap::new(),
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

        let identity = logical.diff_key();
        let (cumulative, gauges) = scorable_columns(logical);
        let duplicate_gauge_breaks = match normalize_duplicate_rows(
            &mut page.rows,
            &identity,
            &cumulative,
            &gauges,
            &mut self.quality,
            &mut self.remaining_identity_bytes,
            limits.identity_bytes,
        ) {
            Ok(breaks) => breaks,
            Err(NormalizeError::Conflict(timestamp)) => {
                self.skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::ConflictingTimestamp { timestamp },
                });
                return Ok(());
            }
            Err(NormalizeError::IdentityByteLimit { observed, limit }) => {
                self.skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::IdentityByteLimit { observed, limit },
                });
                return Ok(());
            }
        };
        let mut diffs = diff_section(&identity, &cumulative, &page.rows, &page.gaps);
        gates.apply(logical, &mut diffs);
        let gauge_series = gauge_section(&identity, &gauges, &page.rows);
        tally_quality(&diffs, &gauge_series, &mut self.quality);
        self.retain_typed(logical.name, &cumulative, &diffs);
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
}

fn capability_state(section: &'static str, rows: &[OutRow]) -> Option<CapabilityInputState> {
    let empty_state = match section {
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
        QueryError::ResultTooLarge { .. } => InputError::MaterializationLimit {
            limit: materialization_limit,
        },
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
    quality: &mut InputQuality,
    remaining_identity_bytes: &mut usize,
    identity_byte_limit: usize,
) -> Result<Vec<DuplicateGaugeBreak>, NormalizeError> {
    let compared: Vec<&str> = cumulative.iter().chain(gauges).copied().collect();
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
            limit: identity_byte_limit,
        })?;
        charge_identity_bytes(bytes, remaining_identity_bytes, identity_byte_limit)
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
                &mut q,
                &mut identity_bytes,
                100,
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
                &mut forward_quality,
                &mut forward_bytes,
                100,
            ),
            Err(NormalizeError::Conflict(1))
        );
        assert_eq!(
            normalize_duplicate_rows(
                &mut reverse,
                &[],
                &["c"],
                &[],
                &mut reverse_quality,
                &mut reverse_bytes,
                100,
            ),
            Err(NormalizeError::Conflict(1))
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
                &mut q,
                &mut identity_bytes,
                100,
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
                &mut q,
                &mut identity_bytes,
                100,
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
                &mut q,
                &mut identity_bytes,
                20,
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
        let mut state = BuildState::new(&limits, limits.identity_bytes, Vec::new());
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
            vec![SectionSkip {
                section: "pg_stat_archiver",
                reason: SkipReason::MaterializationLimit {
                    limit: limits.materialized_cells,
                },
            }]
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
}
