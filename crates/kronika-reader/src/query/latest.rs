//! Latest-row and source-summary lookups over one coherent snapshot.

use std::collections::BTreeMap;

use super::logical::{LogicalSection, logical_section};
use super::section::{MAX_REFRESH, QueryError, compare_full};
use super::value::{OutRow, Value, cell_to_value};
use crate::snapshot::UnitHandle;
use crate::{Cell, Dictionary, LocalDirSnapshot, OpenUnit, ReadError, UnitMeta};

const PG_LOG_SOURCE_STATUS: &str = "pg_log_source_status";

/// Read the latest row of a logical section for one source.
///
/// Catalog metadata filters candidates before any section body is opened. The
/// reverse scan stops when every remaining unit is provably older than the
/// latest decoded row.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] for an unregistered logical name, or
/// [`QueryError::Read`] when a candidate unit cannot be opened or decoded.
pub fn latest_section_row(
    snapshot: &mut LocalDirSnapshot,
    name: &str,
    source: u64,
) -> Result<Option<OutRow>, QueryError> {
    let logical =
        logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
    let mut refreshed = 0_u32;
    loop {
        match latest_once(snapshot, &logical, source) {
            Ok(row) => return Ok(row),
            Err(LatestError::Stale(_unit_idx)) if refreshed < MAX_REFRESH => {
                snapshot
                    .refresh()
                    .map_err(|error| QueryError::Read(ReadError::Io(error)))?;
                refreshed += 1;
            }
            Err(LatestError::Stale(unit_idx)) => {
                return Err(QueryError::Read(ReadError::StaleSnapshot { unit_idx }));
            }
            Err(LatestError::Read(error)) => return Err(QueryError::Read(error)),
        }
    }
}

enum LatestError {
    Stale(usize),
    Read(ReadError),
}

fn latest_once(
    snapshot: &LocalDirSnapshot,
    logical: &LogicalSection,
    source: u64,
) -> Result<Option<OutRow>, LatestError> {
    let mut candidates: Vec<(usize, UnitHandle, UnitMeta)> = snapshot
        .unit_descriptors()
        .filter(|unit| {
            unit.meta.source_id == source && unit.may_contain_any_nonempty_type(&logical.type_ids)
        })
        .map(|unit| (unit.index, unit.handle, unit.meta))
        .collect();
    candidates.sort_by(|left, right| {
        right
            .2
            .max_ts
            .cmp(&left.2.max_ts)
            .then_with(|| right.2.live.cmp(&left.2.live))
            .then_with(|| right.0.cmp(&left.0))
    });

    let union_columns: Vec<&str> = logical.columns.iter().map(|column| column.name).collect();
    let mut best: Option<(i64, OutRow)> = None;
    for (index, handle, unit_meta) in candidates {
        if best
            .as_ref()
            .is_some_and(|(best_ts, _)| unit_meta.max_ts < *best_ts)
        {
            break;
        }
        let unit = match snapshot.open_unit_handle(index, handle) {
            Ok(unit) => unit,
            Err(ReadError::StaleSnapshot { unit_idx }) => {
                return Err(LatestError::Stale(unit_idx));
            }
            Err(error) => return Err(LatestError::Read(error)),
        };
        let has_matching_entry = unit
            .catalog()
            .entries
            .iter()
            .any(|entry| entry.rows != 0 && logical.type_ids.contains(&entry.type_id));
        if !has_matching_entry {
            continue;
        }
        let dictionary = unit.dictionary().map_err(LatestError::Read)?;
        for entry in unit
            .catalog()
            .entries
            .iter()
            .filter(|entry| entry.rows != 0 && logical.type_ids.contains(&entry.type_id))
        {
            let rows = unit.decode_rows(entry).map_err(LatestError::Read)?;
            let Some(first) = rows.first() else {
                continue;
            };
            let contract_columns = first.contract().columns;
            let ts_at = contract_columns
                .iter()
                .position(|column| column.name == "ts");
            let cell_at: Vec<Option<usize>> = logical
                .columns
                .iter()
                .map(|column| {
                    contract_columns
                        .iter()
                        .position(|candidate| candidate.name == column.name)
                })
                .collect();
            for row in rows {
                let cells = row.cells();
                let Some(&Cell::Ts(ts)) = ts_at.and_then(|at| cells.get(at)) else {
                    continue;
                };
                let output: OutRow = logical
                    .columns
                    .iter()
                    .zip(&cell_at)
                    .map(|(column, at)| {
                        let value = at
                            .and_then(|at| cells.get(at))
                            .map_or(Value::Null, |cell| cell_to_value(cell, &dictionary).0);
                        (column.name.to_owned(), value)
                    })
                    .collect();
                let replace = best.as_ref().is_none_or(|(best_ts, best_row)| {
                    ts > *best_ts
                        || (ts == *best_ts
                            && compare_full(&output, best_row, &union_columns, logical.sort_key)
                                == std::cmp::Ordering::Greater)
                });
                if replace {
                    best = Some((ts, output));
                }
            }
        }
    }
    Ok(best.map(|(_, row)| row))
}

/// Work ceilings for one all-source status query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSummaryLimits {
    units: usize,
    rows: u64,
    read_bytes: u64,
}

impl SourceSummaryLimits {
    /// Construct explicit metadata, decoded-row, and stored-byte limits.
    #[must_use]
    pub const fn new(max_units: usize, max_rows: u64, max_read_bytes: u64) -> Self {
        Self {
            units: max_units,
            rows: max_rows,
            read_bytes: max_read_bytes,
        }
    }

    /// Maximum snapshot units one source-summary query may inspect.
    #[must_use]
    pub const fn max_units(self) -> usize {
        self.units
    }
}

impl Default for SourceSummaryLimits {
    fn default() -> Self {
        Self::new(
            kronika_layout::LayoutLimits::default().max_segments,
            1_048_576,
            64 * 1_048_576,
        )
    }
}

/// Resource whose source-summary ceiling was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSummaryResource {
    /// Snapshot units inspected.
    Units,
    /// Status rows admitted for decoding.
    Rows,
    /// Stored status and dictionary bytes admitted for reading.
    Bytes,
}

/// Failure of one coherent all-source summary.
#[derive(Debug)]
pub enum SourceSummaryError {
    /// A unit or dictionary could not be read.
    Read(ReadError),
    /// Every bounded retry observed another stale unit.
    IncompleteSnapshot {
        /// Unit that was stale on the final attempt.
        unit_idx: usize,
        /// Number of full snapshot refreshes already attempted.
        refreshes: u32,
    },
    /// A request-wide work ceiling was exceeded before the next read.
    LimitExceeded {
        /// Resource dimension that reached its ceiling.
        resource: SourceSummaryResource,
        /// Configured ceiling.
        limit: u64,
        /// Work required after admitting the next item.
        observed: u64,
    },
}

/// Time span and latest log-source status for one collector source.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSummary {
    /// Collector source identifier.
    pub source_id: u64,
    /// Earliest unit timestamp.
    pub min_ts: i64,
    /// Latest unit timestamp.
    pub max_ts: i64,
    /// Number of units contributing to the span.
    pub segments: usize,
    /// Latest `pg_log_source_status` row, when this store contains one.
    pub latest_status: Option<OutRow>,
}

/// Build all source spans and latest log-source statuses from one generation.
///
/// A stale unit restarts the whole operation after refreshing the request-local
/// snapshot. The final stale attempt is an explicit error; an older status is
/// never returned as current. Work admitted by stale attempts still counts
/// toward the request-wide ceilings.
///
/// # Errors
///
/// Returns [`SourceSummaryError::IncompleteSnapshot`] after bounded stale
/// retries, [`SourceSummaryError::LimitExceeded`] before exceeding a work
/// ceiling, or [`SourceSummaryError::Read`] for other store failures.
///
/// # Panics
///
/// Panics when the compiled type registry omits `pg_log_source_status`.
pub fn source_summaries(
    snapshot: &mut LocalDirSnapshot,
    limits: SourceSummaryLimits,
) -> Result<Vec<SourceSummary>, SourceSummaryError> {
    let logical =
        logical_section(PG_LOG_SOURCE_STATUS).expect("pg_log_source_status is a registry contract");
    let mut refreshes = 0_u32;
    let mut budget = SummaryBudget::new(limits);
    loop {
        match source_summaries_once(snapshot, &logical, &mut budget) {
            Ok(summaries) => return Ok(summaries),
            Err(SummaryAttemptError::Stale(_unit_idx)) if refreshes < MAX_REFRESH => {
                snapshot
                    .refresh()
                    .map_err(|error| SourceSummaryError::Read(ReadError::Io(error)))?;
                refreshes += 1;
            }
            Err(SummaryAttemptError::Stale(unit_idx)) => {
                return Err(SourceSummaryError::IncompleteSnapshot {
                    unit_idx,
                    refreshes,
                });
            }
            Err(SummaryAttemptError::Failed(error)) => return Err(error),
        }
    }
}

#[derive(Debug)]
struct SourceCandidate {
    index: usize,
    handle: UnitHandle,
    meta: UnitMeta,
    eager_open_bytes: u64,
}

#[derive(Debug)]
struct SourceAccumulator {
    min_ts: i64,
    max_ts: i64,
    segments: usize,
    candidates: Vec<SourceCandidate>,
}

impl SourceAccumulator {
    const fn new(meta: UnitMeta) -> Self {
        Self {
            min_ts: meta.min_ts,
            max_ts: meta.max_ts,
            segments: 0,
            candidates: Vec::new(),
        }
    }
}

struct StatusWinner {
    ts: i64,
    row: kronika_registry::Row,
    unit: OpenUnit,
    dictionary_precharged: bool,
}

enum SummaryAttemptError {
    Stale(usize),
    Failed(SourceSummaryError),
}

impl From<SourceSummaryError> for SummaryAttemptError {
    fn from(error: SourceSummaryError) -> Self {
        Self::Failed(error)
    }
}

fn source_summaries_once(
    snapshot: &LocalDirSnapshot,
    logical: &LogicalSection,
    budget: &mut SummaryBudget,
) -> Result<Vec<SourceSummary>, SummaryAttemptError> {
    let mut by_source = BTreeMap::<u64, SourceAccumulator>::new();
    for descriptor in snapshot.unit_descriptors() {
        budget.charge_unit()?;
        let accumulator = by_source
            .entry(descriptor.meta.source_id)
            .or_insert_with(|| SourceAccumulator::new(descriptor.meta));
        accumulator.min_ts = accumulator.min_ts.min(descriptor.meta.min_ts);
        accumulator.max_ts = accumulator.max_ts.max(descriptor.meta.max_ts);
        accumulator.segments = accumulator.segments.saturating_add(1);

        if descriptor.may_contain_any_nonempty_type(&logical.type_ids) {
            accumulator.candidates.push(SourceCandidate {
                index: descriptor.index,
                handle: descriptor.handle,
                meta: descriptor.meta,
                eager_open_bytes: descriptor.eager_open_bytes,
            });
        }
    }

    let mut summaries = Vec::with_capacity(by_source.len());
    for (source_id, mut accumulator) in by_source {
        accumulator.candidates.sort_by(|left, right| {
            right
                .meta
                .max_ts
                .cmp(&left.meta.max_ts)
                .then_with(|| right.meta.live.cmp(&left.meta.live))
                .then_with(|| right.index.cmp(&left.index))
        });
        let latest_status =
            latest_status_for_source(snapshot, logical, &accumulator.candidates, budget)?;
        summaries.push(SourceSummary {
            source_id,
            min_ts: accumulator.min_ts,
            max_ts: accumulator.max_ts,
            segments: accumulator.segments,
            latest_status,
        });
    }
    Ok(summaries)
}

fn latest_status_for_source(
    snapshot: &LocalDirSnapshot,
    logical: &LogicalSection,
    candidates: &[SourceCandidate],
    budget: &mut SummaryBudget,
) -> Result<Option<OutRow>, SummaryAttemptError> {
    let mut winner = None::<StatusWinner>;
    for candidate in candidates {
        if winner
            .as_ref()
            .is_some_and(|winner| candidate.meta.max_ts < winner.ts)
        {
            break;
        }
        budget.charge_bytes(candidate.eager_open_bytes)?;
        let unit = snapshot
            .open_unit_handle(candidate.index, candidate.handle)
            .map_err(attempt_read_error)?;
        let (status_rows, status_bytes) = unit
            .catalog()
            .entries
            .iter()
            .filter(|entry| entry.rows != 0 && logical.type_ids.contains(&entry.type_id))
            .fold((0_u64, 0_u64), |(rows, bytes), entry| {
                (
                    rows.saturating_add(u64::from(entry.rows)),
                    bytes.saturating_add(entry.len),
                )
            });
        if status_rows == 0 {
            continue;
        }
        budget.charge_rows(status_rows)?;
        if !candidate.meta.live {
            budget.charge_bytes(status_bytes)?;
        }
        let mut unit_best = None::<(i64, kronika_registry::Row)>;
        for entry in unit
            .catalog()
            .entries
            .iter()
            .filter(|entry| entry.rows != 0 && logical.type_ids.contains(&entry.type_id))
        {
            let rows = unit.decode_rows(entry).map_err(attempt_read_error)?;
            let decoded = u64::try_from(rows.len())
                .map_err(|_overflow| attempt_read_error(ReadError::CounterOverflow))?;
            if decoded != u64::from(entry.rows) {
                return Err(attempt_read_error(ReadError::CatalogRowCountMismatch {
                    type_id: entry.type_id,
                    declared: entry.rows,
                    decoded,
                }));
            }
            for row in rows {
                let Some(Cell::Ts(ts)) = row.get("ts") else {
                    continue;
                };
                if unit_best.as_ref().is_none_or(|(best_ts, _)| ts >= best_ts) {
                    unit_best = Some((*ts, row));
                }
            }
        }
        let Some((ts, row)) = unit_best else {
            continue;
        };
        if winner.as_ref().is_none_or(|best| ts > best.ts) {
            winner = Some(StatusWinner {
                ts,
                row,
                unit,
                dictionary_precharged: candidate.meta.live,
            });
        }
    }

    let Some(winner) = winner else {
        return Ok(None);
    };
    let needs_dictionary = winner
        .row
        .cells()
        .iter()
        .any(|cell| matches!(cell, Cell::StrId(id) if *id != 0));
    let dictionary = if needs_dictionary {
        if !winner.dictionary_precharged {
            for entry in &winner.unit.catalog().entries {
                if matches!(
                    entry.type_id,
                    kronika_registry::DICT_STRINGS_TYPE_ID | kronika_registry::DICT_BLOBS_TYPE_ID
                ) {
                    budget.charge_bytes(entry.len)?;
                }
            }
        }
        winner.unit.dictionary().map_err(attempt_read_error)?
    } else {
        Dictionary::default()
    };
    Ok(Some(materialize_row(logical, &winner.row, &dictionary)))
}

fn materialize_row(
    logical: &LogicalSection,
    row: &kronika_registry::Row,
    dictionary: &Dictionary,
) -> OutRow {
    let contract_columns = row.contract().columns;
    logical
        .columns
        .iter()
        .map(|column| {
            let value = contract_columns
                .iter()
                .position(|candidate| candidate.name == column.name)
                .and_then(|at| row.cells().get(at))
                .map_or(Value::Null, |cell| cell_to_value(cell, dictionary).0);
            (column.name.to_owned(), value)
        })
        .collect()
}

struct SummaryBudget {
    limits: SourceSummaryLimits,
    units: usize,
    rows: u64,
    read_bytes: u64,
}

impl SummaryBudget {
    const fn new(limits: SourceSummaryLimits) -> Self {
        Self {
            limits,
            units: 0,
            rows: 0,
            read_bytes: 0,
        }
    }

    fn charge_unit(&mut self) -> Result<(), SummaryAttemptError> {
        self.units = self.units.saturating_add(1);
        if self.units > self.limits.units {
            return Err(limit_error(
                SourceSummaryResource::Units,
                u64::try_from(self.limits.units).unwrap_or(u64::MAX),
                u64::try_from(self.units).unwrap_or(u64::MAX),
            )
            .into());
        }
        Ok(())
    }

    fn charge_rows(&mut self, rows: u64) -> Result<(), SummaryAttemptError> {
        self.rows = self.rows.saturating_add(rows);
        if self.rows > self.limits.rows {
            return Err(
                limit_error(SourceSummaryResource::Rows, self.limits.rows, self.rows).into(),
            );
        }
        Ok(())
    }

    fn charge_bytes(&mut self, bytes: u64) -> Result<(), SummaryAttemptError> {
        self.read_bytes = self.read_bytes.saturating_add(bytes);
        if self.read_bytes > self.limits.read_bytes {
            return Err(limit_error(
                SourceSummaryResource::Bytes,
                self.limits.read_bytes,
                self.read_bytes,
            )
            .into());
        }
        Ok(())
    }
}

const fn limit_error(
    resource: SourceSummaryResource,
    limit: u64,
    observed: u64,
) -> SourceSummaryError {
    SourceSummaryError::LimitExceeded {
        resource,
        limit,
        observed,
    }
}

fn attempt_read_error(error: ReadError) -> SummaryAttemptError {
    match error {
        ReadError::StaleSnapshot { unit_idx } => SummaryAttemptError::Stale(unit_idx),
        other => SummaryAttemptError::Failed(SourceSummaryError::Read(other)),
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::{DictLimits, PartMeta, SectionInput, SegmentDicts, build_part};
    use kronika_registry::Section;
    use kronika_registry::StrId as RegistryStrId;
    use kronika_registry::Ts;
    use kronika_registry::pg_log::PgLogSourceStatusV1;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;

    use super::{
        PG_LOG_SOURCE_STATUS, SourceSummaryError, SourceSummaryLimits, SourceSummaryResource,
        latest_section_row, logical_section, source_summaries,
    };
    use crate::LocalDirSnapshot;
    use crate::query::Value;
    use crate::snapshot::{DECODE_ROWS_CALLS, FORCED_STALE_OPEN_UNIT_CALLS, OPEN_UNIT_CALLS};

    #[test]
    fn default_source_summary_unit_limit_covers_the_supported_five_year_store() {
        const FIVE_YEARS_OF_FIFTEEN_MINUTE_SEGMENTS: usize = 5 * 365 * 24 * 4;

        let limits = SourceSummaryLimits::default();

        assert_eq!(
            limits.max_units(),
            kronika_layout::LayoutLimits::default().max_segments,
            "reader and layout must admit the same supported unit cardinality"
        );
        assert!(limits.max_units() >= FIVE_YEARS_OF_FIFTEEN_MINUTE_SEGMENTS);
    }

    fn write_status(
        dir: &std::path::Path,
        segment_id: i64,
        source: u64,
        min_ts: i64,
        max_ts: i64,
        row_ts: i64,
        state: u8,
    ) {
        let body = PgLogSourceStatusV1::encode(&[PgLogSourceStatusV1 {
            ts: Ts(row_ts),
            state,
            reason: 0,
            parser_kind: 0,
            source_path: None,
            dict_dropped_fields: 0,
        }])
        .expect("encode status");
        let part = build_part(
            &[SectionInput {
                type_id: 1_039_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        );
        crate::test_layout::write_pgm(dir, crate::test_layout::address(segment_id), &part);
    }

    fn write_status_with_path(dir: &std::path::Path, path: &str) {
        let mut dictionary = SegmentDicts::new(DictLimits::default());
        let path_id = dictionary.intern(path.as_bytes()).expect("intern path");
        let status_body = PgLogSourceStatusV1::encode(&[PgLogSourceStatusV1 {
            ts: Ts(100),
            state: 0,
            reason: 0,
            parser_kind: 0,
            source_path: Some(RegistryStrId(path_id.get())),
            dict_dropped_fields: 0,
        }])
        .expect("encode status");
        let mut sections = vec![(1_039_001, 1, status_body)];
        sections.extend(
            kronika_writer::dict::encode(&dictionary)
                .expect("encode dictionary")
                .into_iter()
                .map(|section| (section.type_id, section.rows, section.body)),
        );
        let inputs: Vec<_> = sections
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        let part = build_part(
            &inputs,
            PartMeta {
                min_ts: 100,
                max_ts: 100,
                source_id: 7,
            },
        );
        crate::test_layout::write_pgm(dir, crate::test_layout::address(100), &part);
    }

    fn field<'a>(row: &'a crate::OutRow, name: &str) -> &'a Value {
        row.iter()
            .find(|(column, _)| column == name)
            .map(|(_, value)| value)
            .expect("field")
    }

    fn write_old_store_unit(dir: &std::path::Path, source: u64) {
        let body = PgStatArchiver::encode(&[PgStatArchiver {
            ts: Ts(5),
            archived_count: 1,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }])
        .expect("encode old-store section");
        let part = build_part(
            &[SectionInput {
                type_id: 1_008_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: 0,
                max_ts: 10,
                source_id: source,
            },
        );
        crate::test_layout::write_pgm(dir, crate::test_layout::address(5), &part);
    }

    fn write_bloom_false_positive_without_status(dir: &std::path::Path) {
        let sections = (0..1_024_u32)
            .map(|index| SectionInput {
                type_id: 2_000_000 + index,
                rows: 1,
                body: &[][..],
            })
            .collect::<Vec<_>>();
        let part = build_part(
            &sections,
            PartMeta {
                min_ts: 0,
                max_ts: 10,
                source_id: 7,
            },
        );
        crate::test_layout::write_pgm(dir, crate::test_layout::address(5), &part);
    }

    #[test]
    fn latest_row_uses_row_timestamp_across_overlapping_units() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 100, 7, 0, 1_000, 100, 2);
        write_status(dir.path(), 200, 7, 0, 900, 800, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");

        let row = latest_section_row(&mut snapshot, "pg_log_source_status", 7)
            .expect("latest query")
            .expect("status row");
        assert_eq!(field(&row, "ts"), &Value::Ts(800));
        assert_eq!(field(&row, "state"), &Value::U64(0));
    }

    #[test]
    fn latest_row_stops_before_provably_older_units() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 200, 7, 200, 300, 250, 0);
        write_status(dir.path(), 100, 7, 100, 200, 190, 2);
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");

        let row =
            latest_section_row(&mut snapshot, "pg_log_source_status", 7).expect("latest query");
        assert!(row.is_some());
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn latest_row_returns_none_for_an_old_store_without_the_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_old_store_unit(dir.path(), 7);
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        assert_eq!(
            latest_section_row(&mut snapshot, "pg_log_source_status", 7).expect("latest query"),
            None
        );
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn source_summary_confirms_a_bloom_positive_against_the_real_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bloom_false_positive_without_status(dir.path());
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        let logical = logical_section(PG_LOG_SOURCE_STATUS).expect("registered status");
        assert!(
            snapshot
                .unit_descriptors()
                .next()
                .expect("unit")
                .may_contain_any_nonempty_type(&logical.type_ids),
            "fixture must exercise a Bloom false positive"
        );
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let summaries = source_summaries(&mut snapshot, SourceSummaryLimits::default())
            .expect("false positive is not an error");

        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].latest_status.is_none());
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn latest_row_rejects_an_unregistered_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        let error =
            latest_section_row(&mut snapshot, "not_registered", 7).expect_err("unknown section");
        assert!(
            matches!(error, crate::QueryError::UnknownSection(name) if name == "not_registered")
        );
    }

    #[test]
    fn source_summaries_scan_all_sources_in_one_bounded_operation() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 100, 7, 0, 1_000, 900, 2);
        write_status(dir.path(), 200, 7, 0, 2_000, 1_900, 0);
        write_status(dir.path(), 300, 42, 0, 1_500, 1_400, 1);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let summaries = source_summaries(&mut snapshot, SourceSummaryLimits::default())
            .expect("source summaries");

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source_id, 7);
        assert_eq!(summaries[0].segments, 2);
        assert_eq!(
            field(
                summaries[0]
                    .latest_status
                    .as_ref()
                    .expect("source 7 status"),
                "ts"
            ),
            &Value::Ts(1_900)
        );
        assert_eq!(summaries[1].source_id, 42);
        assert_eq!(
            field(
                summaries[1]
                    .latest_status
                    .as_ref()
                    .expect("source 42 status"),
                "ts"
            ),
            &Value::Ts(1_400)
        );
        assert_eq!(
            OPEN_UNIT_CALLS.with(std::cell::Cell::get),
            2,
            "the provably older source-7 unit is not opened"
        );
    }

    #[test]
    fn source_summary_unit_limit_precedes_body_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 1, 7, 0, 1, 1, 0);
        write_status(dir.path(), 2, 42, 0, 1, 1, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let error = source_summaries(
            &mut snapshot,
            SourceSummaryLimits::new(1, u64::MAX, u64::MAX),
        )
        .expect_err("unit ceiling");

        assert!(matches!(
            error,
            SourceSummaryError::LimitExceeded {
                resource: SourceSummaryResource::Units,
                limit: 1,
                observed: 2,
            }
        ));
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn source_summary_row_and_byte_limits_precede_body_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 1, 7, 0, 1, 1, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");

        for (limits, expected_resource) in [
            (
                SourceSummaryLimits::new(1, 0, u64::MAX),
                SourceSummaryResource::Rows,
            ),
            (
                SourceSummaryLimits::new(1, u64::MAX, 0),
                SourceSummaryResource::Bytes,
            ),
        ] {
            OPEN_UNIT_CALLS.with(|calls| calls.set(0));
            DECODE_ROWS_CALLS.with(|calls| calls.set(0));
            let error =
                source_summaries(&mut snapshot, limits).expect_err("work ceiling must reject");
            let SourceSummaryError::LimitExceeded {
                resource,
                limit,
                observed,
            } = error
            else {
                panic!("expected a work limit, got {error:?}");
            };
            assert_eq!(resource, expected_resource);
            assert_eq!(limit, 0);
            assert!(observed > 0);
            let expected_catalog_opens =
                usize::from(expected_resource == SourceSummaryResource::Rows);
            assert_eq!(
                OPEN_UNIT_CALLS.with(std::cell::Cell::get),
                expected_catalog_opens
            );
            assert_eq!(DECODE_ROWS_CALLS.with(std::cell::Cell::get), 0);
        }
    }

    #[test]
    fn source_summary_resolves_the_winning_status_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status_with_path(dir.path(), "/var/log/postgresql/postgresql.log");
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");

        let summaries = source_summaries(&mut snapshot, SourceSummaryLimits::default())
            .expect("source summaries");
        let status = summaries[0].latest_status.as_ref().expect("latest status");

        assert_eq!(
            field(status, "source_path"),
            &Value::Str("/var/log/postgresql/postgresql.log".to_owned())
        );
    }

    #[test]
    fn source_summary_byte_limit_is_cumulative_across_stale_retries() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 1, 7, 0, 1, 1, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        let catalog_bytes = snapshot
            .unit_descriptors()
            .next()
            .expect("status unit")
            .eager_open_bytes;
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| calls.set(1));

        let error = source_summaries(
            &mut snapshot,
            SourceSummaryLimits::new(usize::MAX, u64::MAX, catalog_bytes),
        )
        .expect_err("the retried catalog read must exceed the request-wide byte ceiling");

        FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        assert!(matches!(
            error,
            SourceSummaryError::LimitExceeded {
                resource: SourceSummaryResource::Bytes,
                limit,
                observed,
            } if limit == catalog_bytes && observed == catalog_bytes * 2
        ));
        assert_eq!(
            OPEN_UNIT_CALLS.with(std::cell::Cell::get),
            1,
            "the second attempt must be rejected before reopening the unit"
        );
    }

    #[test]
    fn repeated_staleness_is_explicit_instead_of_returning_an_old_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), 1, 7, 0, 1, 1, 0);
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| calls.set(3));

        let error = source_summaries(&mut snapshot, SourceSummaryLimits::default())
            .expect_err("bounded stale retries");

        FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        assert!(matches!(
            error,
            SourceSummaryError::IncompleteSnapshot { refreshes: 2, .. }
        ));
    }
}
