//! Batch section reads across the units of a snapshot.
//!
//! [`sections`] answers several logical sections for one root and time window
//! in a single pass over the snapshot's units: each in-window unit is opened
//! once, its dictionary read once, and every requested section decoded from that
//! one open. Rows are materialized onto each section's union columns, filtered by
//! timestamp, ordered by the section's sort key, and truncated to `limit`.

use std::collections::BTreeMap;

use crate::query::cursor::Cursor;
use crate::query::logical::{LogicalSection, logical_section};
use crate::query::value::{Gap, OutRow, Value, cell_to_value};
use crate::{
    Cell, LocalDirSnapshot, OpenUnit, ReadError, Resolved, SealedFactError, SegmentDescriptor,
};

/// How many times `sections` refreshes a stale snapshot before giving up on the
/// stale unit and letting its time fall into a gap.
pub(super) const MAX_REFRESH: u32 = 2;

/// Maximum number of output cells retained by one query.
const MAX_MATERIALIZED_CELLS: usize = 10_000_000;
/// Maximum owned variable-width payload retained by one query.
const MAX_MATERIALIZED_BYTES: usize = 64 * 1024 * 1024;
/// Maximum catalog/open bytes admitted by one section query.
const MAX_CATALOG_READ_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum dictionary body bytes admitted by one section query.
const MAX_DICTIONARY_READ_BYTES: u64 = 64 * 1024 * 1024;

/// One logical section's answer for a time window.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionPage {
    /// Logical section name, e.g. `"pg_stat_activity"`.
    pub section: String,
    /// Rows on the section's union columns, ordered by its sort key.
    pub rows: Vec<OutRow>,
    /// Stretches of the window that no readable unit covers.
    pub gaps: Vec<Gap>,
    /// Cursor to resume after the last returned row, or `None` when this page
    /// exhausts the stream.
    pub next_cursor: Option<Cursor>,
}

/// Request-wide I/O work ceilings for a section query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryWorkLimits {
    units: usize,
    catalog_read_bytes: u64,
    dictionary_read_bytes: u64,
}

impl QueryWorkLimits {
    /// Set unit, catalog/open-byte, and dictionary-body-byte ceilings.
    #[must_use]
    pub const fn new(
        max_units: usize,
        max_catalog_read_bytes: u64,
        max_dictionary_read_bytes: u64,
    ) -> Self {
        Self {
            units: max_units,
            catalog_read_bytes: max_catalog_read_bytes,
            dictionary_read_bytes: max_dictionary_read_bytes,
        }
    }
}

impl Default for QueryWorkLimits {
    fn default() -> Self {
        Self::new(
            kronika_layout::LayoutLimits::default().max_segments,
            MAX_CATALOG_READ_BYTES,
            MAX_DICTIONARY_READ_BYTES,
        )
    }
}

/// Row, materialization, and request-wide work ceilings for a section query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLimits {
    rows: usize,
    cells: usize,
    bytes: usize,
    work: QueryWorkLimits,
}

impl QueryLimits {
    /// Set the row and cell ceilings.
    #[must_use]
    pub fn new(rows: usize, cells: usize) -> Self {
        Self {
            rows,
            cells,
            bytes: MAX_MATERIALIZED_BYTES,
            work: QueryWorkLimits::default(),
        }
    }

    /// Set row, cell, and owned variable-width byte ceilings.
    #[must_use]
    pub fn with_bytes(rows: usize, cells: usize, bytes: usize) -> Self {
        Self {
            rows,
            cells,
            bytes,
            work: QueryWorkLimits::default(),
        }
    }

    /// Replace the request-wide I/O work ceilings.
    #[must_use]
    pub const fn with_work_limits(mut self, work: QueryWorkLimits) -> Self {
        self.work = work;
        self
    }
}

/// Resource whose section-query work ceiling was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryWorkResource {
    /// Snapshot units inspected after source and time filtering.
    Units,
    /// Stored bytes admitted to open candidate catalogs.
    CatalogBytes,
    /// Stored dictionary body bytes admitted after catalog confirmation.
    DictionaryBytes,
}

/// Why a batch section read failed.
#[derive(Debug)]
pub enum QueryError {
    /// No registered contract carries this section name.
    UnknownSection(String),
    /// A resume cursor was malformed or belonged to another source.
    BadCursor(String),
    /// Reading a unit or decoding a section failed.
    Read(ReadError),
    /// The exact sealed descriptor selected by the caller is unavailable or stale.
    SealedDescriptor(SealedFactError),
    /// The matching rows exceed the query materialization budget.
    ResultTooLarge {
        /// Maximum cells a query may retain.
        max_cells: usize,
    },
    /// Resolved strings, blobs, lists, and row keys exceed the byte budget.
    MaterializedBytesTooLarge {
        /// Maximum owned variable-width bytes a query may retain.
        max_bytes: usize,
    },
    /// Request-wide unit or read-byte work exceeded its ceiling.
    WorkLimitExceeded {
        /// Work dimension that reached its ceiling.
        resource: QueryWorkResource,
        /// Configured ceiling.
        limit: u64,
        /// Work required after admitting the next operation.
        observed: u64,
    },
}

impl From<ReadError> for QueryError {
    fn from(err: ReadError) -> Self {
        Self::Read(err)
    }
}

/// Read one logical section for a window.
///
/// Equivalent to [`sections`] with a single name, returning that name's page.
/// A registered name always yields a page (possibly with no rows); an
/// unregistered one fails before any page is built, so [`sections`] returns
/// exactly one entry here. `cursor`, when set, resumes after the row it pins.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] when `name` is not registered,
/// [`QueryError::Read`] when a unit cannot be opened or decoded. Returns
/// [`QueryError::ResultTooLarge`] before retaining more than the materialization
/// budget, or [`QueryError::WorkLimitExceeded`] before exceeding read work.
pub fn section(
    snap: &mut LocalDirSnapshot,
    name: &str,
    from: i64,
    to: i64,
    limit: usize,
    cursor: Option<Cursor>,
) -> Result<SectionPage, QueryError> {
    section_with_limits(
        snap,
        name,
        from,
        to,
        cursor,
        QueryLimits::new(limit, MAX_MATERIALIZED_CELLS),
    )
}

/// Read one logical section under a caller-supplied materialized-cell cap.
///
/// The reader's hard cap still applies when `max_cells` is larger. This entry
/// point lets a multi-query adapter spend one request budget across calls.
///
/// # Errors
///
/// Returns the same errors as [`section`].
pub fn section_with_limits(
    snap: &mut LocalDirSnapshot,
    name: &str,
    from: i64,
    to: i64,
    cursor: Option<Cursor>,
    limits: QueryLimits,
) -> Result<SectionPage, QueryError> {
    let cursors: BTreeMap<String, Cursor> =
        cursor.map(|c| (name.to_owned(), c)).into_iter().collect();
    let pages = sections_with_limits(snap, from, to, &[name], &cursors, limits)?;
    pages
        .into_values()
        .next()
        .ok_or_else(|| QueryError::UnknownSection(name.to_owned()))
}

/// Read several logical sections for a window in one pass.
///
/// A section named in `cursors` resumes after the row its cursor pins: rows are
/// ordered by the crate's full-row comparator, every row at or before the
/// cursor is dropped, and the remaining tail is paged. When the tail exceeds
/// `limit`, the page's `next_cursor` pins its last row so a further call
/// continues the stream.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] for the first unregistered name,
/// [`QueryError::Read`] when a unit cannot be opened or decoded. Returns
/// [`QueryError::ResultTooLarge`] before retaining more than the materialization
/// budget, or [`QueryError::WorkLimitExceeded`] before exceeding read work.
pub fn sections(
    snap: &mut LocalDirSnapshot,
    from: i64,
    to: i64,
    names: &[&str],
    limit: usize,
    cursors: &BTreeMap<String, Cursor>,
) -> Result<BTreeMap<String, SectionPage>, QueryError> {
    sections_with_limits(
        snap,
        from,
        to,
        names,
        cursors,
        QueryLimits::new(limit, MAX_MATERIALIZED_CELLS),
    )
}

/// Read several logical sections under a caller-supplied materialized-cell cap.
///
/// # Errors
///
/// Returns the same errors as [`sections`].
pub fn sections_with_limits(
    snap: &mut LocalDirSnapshot,
    from: i64,
    to: i64,
    names: &[&str],
    cursors: &BTreeMap<String, Cursor>,
    limits: QueryLimits,
) -> Result<BTreeMap<String, SectionPage>, QueryError> {
    let max_cells = limits.cells.min(MAX_MATERIALIZED_CELLS);
    let max_bytes = limits.bytes.min(MAX_MATERIALIZED_BYTES);
    let effective_limits = QueryLimits {
        rows: limits.rows,
        cells: max_cells,
        bytes: max_bytes,
        work: limits.work,
    };
    // Resolve every requested name up front; an unknown name fails the whole call.
    let mut requested: Vec<(String, LogicalSection)> = Vec::with_capacity(names.len());
    for &name in names {
        let logical =
            logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
        requested.push((name.to_owned(), logical));
    }
    let mut requested_type_ids = requested
        .iter()
        .flat_map(|(_, logical)| logical.type_ids.iter().copied())
        .collect::<Vec<_>>();
    requested_type_ids.sort_unstable();
    requested_type_ids.dedup();

    // Gather rows and the time ranges actually read. A unit that goes stale mid
    // read (concurrent seal/reset) triggers a snapshot refresh and a full retry,
    // up to MAX_REFRESH times; after that the still-stale unit is skipped and its
    // time drops out of coverage, surfacing as a gap.
    let mut refreshed: u32 = 0;
    let mut work_budget = QueryWorkBudget::new(effective_limits.work);
    let (buffers, covered) = loop {
        let skip_stale = refreshed >= MAX_REFRESH;
        let query = GatherQuery {
            from,
            to,
            requested: &requested,
            requested_type_ids: &requested_type_ids,
            skip_stale,
            limits: effective_limits,
        };
        match gather(snap, &query, &mut work_budget) {
            Ok(gathered) => break gathered,
            Err(GatherError::Stale) => {
                snap.refresh()
                    .map_err(|err| QueryError::Read(ReadError::Io(err)))?;
                refreshed += 1;
            }
            Err(GatherError::Read(err)) => return Err(QueryError::Read(err)),
            Err(GatherError::ResultTooLarge) => {
                return Err(QueryError::ResultTooLarge { max_cells });
            }
            Err(GatherError::MaterializedBytesTooLarge) => {
                return Err(QueryError::MaterializedBytesTooLarge { max_bytes });
            }
            Err(GatherError::WorkLimitExceeded {
                resource,
                limit,
                observed,
            }) => {
                return Err(QueryError::WorkLimitExceeded {
                    resource,
                    limit,
                    observed,
                });
            }
        }
    };

    // Coverage holes are a property of the source over the window, so they are
    // identical for every requested section.
    let gaps = coverage_gaps(from, to, &covered);

    // Order each buffer by the section's total order, drop everything at or
    // before the resume cursor, then page the tail.
    let mut pages = BTreeMap::new();
    for ((name, logical), mut rows) in requested.into_iter().zip(buffers) {
        let columns: Vec<&str> = logical.columns.iter().map(|col| col.name).collect();
        rows.sort_by(|a, b| compare_full(a, b, &columns, logical.sort_key));

        if let Some(cursor) = cursors.get(&name) {
            // Pair the cursor's values back with their column names so the same
            // total order compares the cursor against every candidate row.
            let cursor_row: OutRow = columns
                .iter()
                .map(|&name| name.to_owned())
                .zip(cursor.values.iter().cloned())
                .collect();
            let start = rows.partition_point(|row| {
                compare_full(row, &cursor_row, &columns, logical.sort_key)
                    != std::cmp::Ordering::Greater
            });
            rows.drain(..start);
        }

        let has_more = rows.len() > limits.rows;
        rows.truncate(limits.rows);
        // A cursor pins the last returned row, so an empty page (e.g. `limit`
        // of zero) never emits one, even when rows remain.
        let next_cursor = rows.last().filter(|_| has_more).map(|row| Cursor {
            values: row.iter().map(|(_, v)| v.clone()).collect(),
        });

        let page = SectionPage {
            section: name.clone(),
            rows,
            gaps: gaps.clone(),
            next_cursor,
        };
        pages.insert(name, page);
    }
    Ok(pages)
}

/// Read several logical sections from one exact sealed descriptor.
///
/// Unlike [`sections_with_limits`], this path never scans or opens another
/// unit whose time range overlaps the requested window. It is intended for
/// callers that selected a PGM through reader-authored index metadata.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] for an unregistered name,
/// [`QueryError::SealedDescriptor`] when the pinned descriptor is unavailable
/// or stale, and the same decode and resource errors as
/// [`sections_with_limits`].
pub fn sections_from_sealed_descriptor_with_limits(
    snap: &LocalDirSnapshot,
    descriptor: &SegmentDescriptor,
    from: i64,
    to: i64,
    names: &[&str],
    cursors: &BTreeMap<String, Cursor>,
    limits: QueryLimits,
) -> Result<BTreeMap<String, SectionPage>, QueryError> {
    let max_cells = limits.cells.min(MAX_MATERIALIZED_CELLS);
    let max_bytes = limits.bytes.min(MAX_MATERIALIZED_BYTES);
    let effective_limits = QueryLimits {
        rows: limits.rows,
        cells: max_cells,
        bytes: max_bytes,
        work: limits.work,
    };
    let mut requested = Vec::with_capacity(names.len());
    for &name in names {
        let logical =
            logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
        requested.push((name.to_owned(), logical));
    }
    let mut requested_type_ids = requested
        .iter()
        .flat_map(|(_, logical)| logical.type_ids.iter().copied())
        .collect::<Vec<_>>();
    requested_type_ids.sort_unstable();
    requested_type_ids.dedup();

    let mut work_budget = QueryWorkBudget::new(effective_limits.work);
    work_budget
        .charge_unit()
        .map_err(|error| query_error_from_gather(error, max_cells, max_bytes))?;
    let eager_open_bytes = snap
        .sealed_query_eager_open_bytes(descriptor)
        .map_err(QueryError::SealedDescriptor)?;
    work_budget
        .charge_catalog_read(eager_open_bytes)
        .map_err(|error| query_error_from_gather(error, max_cells, max_bytes))?;
    let unit = snap
        .open_sealed_for_query_by_descriptor(descriptor)
        .map_err(QueryError::SealedDescriptor)?;
    let unit = OpenUnit::Sealed(unit);
    let mut buffers = vec![Vec::new(); requested.len()];
    if unit
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.rows != 0 && requested_type_ids.contains(&entry.type_id))
    {
        let dictionary_read_bytes = unit
            .catalog()
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.type_id,
                    kronika_registry::DICT_STRINGS_TYPE_ID | kronika_registry::DICT_BLOBS_TYPE_ID
                )
            })
            .fold(0_u64, |bytes, entry| bytes.saturating_add(entry.len));
        work_budget
            .charge_dictionary_read(dictionary_read_bytes)
            .map_err(|error| query_error_from_gather(error, max_cells, max_bytes))?;
        let dictionary = unit.dictionary().map_err(QueryError::Read)?;
        let query = GatherQuery {
            from,
            to,
            requested: &requested,
            requested_type_ids: &requested_type_ids,
            skip_stale: false,
            limits: effective_limits,
        };
        decode_requested_rows(
            &unit,
            &dictionary,
            &query,
            &mut buffers,
            &mut MaterializationUsage::default(),
        )
        .map_err(|error| query_error_from_gather(error, max_cells, max_bytes))?;
    }

    let gaps = coverage_gaps(from, to, &[(descriptor.min_ts, descriptor.max_ts)]);
    Ok(finish_pages(
        requested,
        buffers,
        &gaps,
        cursors,
        limits.rows,
    ))
}

fn query_error_from_gather(error: GatherError, max_cells: usize, max_bytes: usize) -> QueryError {
    match error {
        GatherError::Stale => unreachable!("exact sealed decoding cannot report a live-unit race"),
        GatherError::Read(error) => QueryError::Read(error),
        GatherError::ResultTooLarge => QueryError::ResultTooLarge { max_cells },
        GatherError::MaterializedBytesTooLarge => {
            QueryError::MaterializedBytesTooLarge { max_bytes }
        }
        GatherError::WorkLimitExceeded {
            resource,
            limit,
            observed,
        } => QueryError::WorkLimitExceeded {
            resource,
            limit,
            observed,
        },
    }
}

fn finish_pages(
    requested: Vec<(String, LogicalSection)>,
    buffers: Vec<Vec<OutRow>>,
    gaps: &[Gap],
    cursors: &BTreeMap<String, Cursor>,
    limit: usize,
) -> BTreeMap<String, SectionPage> {
    let mut pages = BTreeMap::new();
    for ((name, logical), mut rows) in requested.into_iter().zip(buffers) {
        let columns: Vec<&str> = logical.columns.iter().map(|col| col.name).collect();
        rows.sort_by(|a, b| compare_full(a, b, &columns, logical.sort_key));

        if let Some(cursor) = cursors.get(&name) {
            let cursor_row: OutRow = columns
                .iter()
                .map(|&name| name.to_owned())
                .zip(cursor.values.iter().cloned())
                .collect();
            let start = rows.partition_point(|row| {
                compare_full(row, &cursor_row, &columns, logical.sort_key)
                    != std::cmp::Ordering::Greater
            });
            rows.drain(..start);
        }

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = rows.last().filter(|_| has_more).map(|row| Cursor {
            values: row.iter().map(|(_, value)| value.clone()).collect(),
        });
        pages.insert(
            name.clone(),
            SectionPage {
                section: name,
                rows,
                gaps: gaps.to_owned(),
                next_cursor,
            },
        );
    }
    pages
}

/// Failure while gathering a window's rows.
#[derive(Debug)]
enum GatherError {
    /// A unit went stale (concurrent seal/reset); the caller should refresh and retry.
    Stale,
    /// A read failed for a reason a refresh will not fix.
    Read(ReadError),
    /// Retaining another row would exceed the materialization budget.
    ResultTooLarge,
    /// Retaining another row would exceed the variable-width byte budget.
    MaterializedBytesTooLarge,
    /// Admitting another unit or read would exceed a request-wide work budget.
    WorkLimitExceeded {
        resource: QueryWorkResource,
        limit: u64,
        observed: u64,
    },
}

/// Per-section row buffers plus the `[min, max]` ranges actually read.
type Gathered = (Vec<Vec<OutRow>>, Vec<(i64, i64)>);

struct QueryWorkBudget {
    limits: QueryWorkLimits,
    units: u64,
    catalog_read_bytes: u64,
    dictionary_read_bytes: u64,
}

impl QueryWorkBudget {
    const fn new(limits: QueryWorkLimits) -> Self {
        Self {
            limits,
            units: 0,
            catalog_read_bytes: 0,
            dictionary_read_bytes: 0,
        }
    }

    fn charge_unit(&mut self) -> Result<(), GatherError> {
        let limit = u64::try_from(self.limits.units).unwrap_or(u64::MAX);
        charge_work(&mut self.units, 1, limit, QueryWorkResource::Units)
    }

    fn charge_catalog_read(&mut self, bytes: u64) -> Result<(), GatherError> {
        charge_work(
            &mut self.catalog_read_bytes,
            bytes,
            self.limits.catalog_read_bytes,
            QueryWorkResource::CatalogBytes,
        )
    }

    fn charge_dictionary_read(&mut self, bytes: u64) -> Result<(), GatherError> {
        charge_work(
            &mut self.dictionary_read_bytes,
            bytes,
            self.limits.dictionary_read_bytes,
            QueryWorkResource::DictionaryBytes,
        )
    }
}

fn charge_work(
    current: &mut u64,
    amount: u64,
    limit: u64,
    resource: QueryWorkResource,
) -> Result<(), GatherError> {
    let observed = current.checked_add(amount).unwrap_or(u64::MAX);
    if observed > limit {
        return Err(GatherError::WorkLimitExceeded {
            resource,
            limit,
            observed,
        });
    }
    *current = observed;
    Ok(())
}

struct GatherQuery<'a> {
    from: i64,
    to: i64,
    requested: &'a [(String, LogicalSection)],
    requested_type_ids: &'a [u32],
    skip_stale: bool,
    limits: QueryLimits,
}

#[derive(Default)]
struct MaterializationUsage {
    cells: usize,
    bytes: usize,
}

/// Decode every requested section from the root's in-window units in one pass.
///
/// Catalog summaries reject definite type misses before open. A Bloom positive
/// is confirmed against the opened catalog before its dictionary is read. With
/// `skip_stale` a unit that opens stale is skipped; otherwise the first stale
/// unit returns [`GatherError::Stale`] so the caller can refresh and retry.
fn gather(
    snap: &LocalDirSnapshot,
    query: &GatherQuery<'_>,
    work_budget: &mut QueryWorkBudget,
) -> Result<Gathered, GatherError> {
    let mut buffers: Vec<Vec<OutRow>> = vec![Vec::new(); query.requested.len()];
    let mut covered: Vec<(i64, i64)> = Vec::new();
    let mut materialization = MaterializationUsage::default();

    for descriptor in snap.unit_descriptors().filter(|descriptor| {
        descriptor.meta.max_ts >= query.from && descriptor.meta.min_ts <= query.to
    }) {
        work_budget.charge_unit()?;
        let range = (descriptor.meta.min_ts, descriptor.meta.max_ts);
        if !descriptor.may_contain_any_nonempty_type(query.requested_type_ids) {
            covered.push(range);
            continue;
        }

        work_budget.charge_catalog_read(descriptor.eager_open_bytes)?;
        let unit = match snap.open_unit_handle(descriptor.index, descriptor.handle) {
            Ok(unit) => unit,
            Err(ReadError::StaleSnapshot { .. }) if query.skip_stale => continue,
            Err(ReadError::StaleSnapshot { .. }) => return Err(GatherError::Stale),
            Err(err) => return Err(GatherError::Read(err)),
        };
        let catalog = unit.catalog();
        covered.push(range);
        if !catalog
            .entries
            .iter()
            .any(|entry| entry.rows != 0 && query.requested_type_ids.contains(&entry.type_id))
        {
            continue;
        }

        let dictionary_read_bytes = catalog
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.type_id,
                    kronika_registry::DICT_STRINGS_TYPE_ID | kronika_registry::DICT_BLOBS_TYPE_ID
                )
            })
            .fold(0_u64, |bytes, entry| bytes.saturating_add(entry.len));
        work_budget.charge_dictionary_read(dictionary_read_bytes)?;
        let dict = unit.dictionary().map_err(GatherError::Read)?;
        decode_requested_rows(&unit, &dict, query, &mut buffers, &mut materialization)?;
    }
    Ok((buffers, covered))
}

fn decode_requested_rows(
    unit: &OpenUnit,
    dict: &crate::Dictionary,
    query: &GatherQuery<'_>,
    buffers: &mut [Vec<OutRow>],
    materialization: &mut MaterializationUsage,
) -> Result<(), GatherError> {
    for (buffer, (_, logical)) in buffers.iter_mut().zip(query.requested) {
        for entry in &unit.catalog().entries {
            if entry.rows == 0 || !logical.type_ids.contains(&entry.type_id) {
                continue;
            }
            let rows = unit.decode_rows(entry).map_err(GatherError::Read)?;
            let Some(first) = rows.first() else {
                continue;
            };
            // Cell positions are fixed per contract, so resolve each union
            // column (and `ts`) once per entry, not per row.
            let columns = first.contract().columns;
            let ts_at = columns.iter().position(|column| column.name == "ts");
            let cell_at: Vec<Option<usize>> = logical
                .columns
                .iter()
                .map(|col| columns.iter().position(|column| column.name == col.name))
                .collect();
            for row in rows {
                let cells = row.cells();
                let Some(&Cell::Ts(t)) = ts_at.and_then(|at| cells.get(at)) else {
                    continue;
                };
                if t < query.from || t > query.to {
                    continue;
                }
                charge_materialization(
                    &mut materialization.cells,
                    logical.columns.len(),
                    query.limits.cells,
                )?;
                let row_bytes = logical.columns.iter().zip(&cell_at).try_fold(
                    0_usize,
                    |total, (column, at)| {
                        let value_bytes = at
                            .and_then(|at| cells.get(at))
                            .map_or(0, |cell| materialized_value_bytes(cell, dict));
                        total
                            .checked_add(column.name.len())
                            .and_then(|sum| sum.checked_add(value_bytes))
                    },
                );
                let Some(row_bytes) = row_bytes else {
                    return Err(GatherError::MaterializedBytesTooLarge);
                };
                materialization.bytes = materialization
                    .bytes
                    .checked_add(row_bytes)
                    .filter(|total| *total <= query.limits.bytes)
                    .ok_or(GatherError::MaterializedBytesTooLarge)?;
                let out = logical
                    .columns
                    .iter()
                    .zip(&cell_at)
                    .map(|(col, at)| {
                        let value = at
                            .and_then(|at| cells.get(at))
                            .map_or(Value::Null, |cell| cell_to_value(cell, dict).0);
                        (col.name.to_owned(), value)
                    })
                    .collect();
                buffer.push(out);
            }
        }
    }
    Ok(())
}

fn materialized_value_bytes(cell: &Cell, dict: &crate::Dictionary) -> usize {
    match cell {
        Cell::ListI32(values) => values.len().saturating_mul(size_of::<i32>()),
        Cell::StrId(0)
        | Cell::Null
        | Cell::I16(_)
        | Cell::I32(_)
        | Cell::I64(_)
        | Cell::U32(_)
        | Cell::U64(_)
        | Cell::F64(_)
        | Cell::Bool(_)
        | Cell::Ts(_) => 0,
        Cell::StrId(id) => match dict.resolve(*id) {
            Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => {
                bytes.len().saturating_mul(3)
            }
            None => 0,
        },
    }
}

fn charge_materialization(
    materialized_cells: &mut usize,
    row_width: usize,
    max_cells: usize,
) -> Result<(), GatherError> {
    *materialized_cells = materialized_cells
        .checked_add(row_width)
        .filter(|&cells| cells <= max_cells)
        .ok_or(GatherError::ResultTooLarge)?;
    Ok(())
}

/// Stretches of `[from, to]` that no readable unit covers, given each unit's
/// `[min, max]` range. Ranges are clamped to the window, merged where they
/// overlap or touch, and the complement within the window is returned.
fn coverage_gaps(from: i64, to: i64, covered: &[(i64, i64)]) -> Vec<Gap> {
    let mut ranges: Vec<(i64, i64)> = covered
        .iter()
        .map(|&(min, max)| (min.max(from), max.min(to)))
        .filter(|&(start, end)| start <= end)
        .collect();
    ranges.sort_by_key(|&(start, _)| start);

    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }

    let mut gaps = Vec::new();
    let mut cursor = from;
    for (start, end) in merged {
        if start > cursor {
            gaps.push(Gap {
                from: cursor,
                to: start,
            });
        }
        cursor = cursor.max(end);
    }
    if cursor < to {
        gaps.push(Gap { from: cursor, to });
    }
    gaps
}

/// Order two rows by the sort-key column values, ascending.
///
/// Missing columns compare as [`Value::Null`], so the order is total even if a
/// row lacks a sort-key column.
fn compare_by_sort_key(a: &OutRow, b: &OutRow, sort_key: &[&str]) -> std::cmp::Ordering {
    for key in sort_key {
        let va = row_value(a, key);
        let vb = row_value(b, key);
        let ordering = compare_values(va, vb);
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

/// A total order over rows: sort key first, then the remaining union columns.
///
/// Ties on the sort key break on the other `columns` (those not in `sort_key`,
/// in `columns` order), so equal-sort-key rows still order deterministically —
/// the property keyset pagination needs to tile a stream without gap or repeat.
pub(super) fn compare_full(
    a: &OutRow,
    b: &OutRow,
    columns: &[&str],
    sort_key: &[&str],
) -> std::cmp::Ordering {
    let by_key = compare_by_sort_key(a, b, sort_key);
    if by_key != std::cmp::Ordering::Equal {
        return by_key;
    }
    for &col in columns {
        if sort_key.contains(&col) {
            continue;
        }
        let ordering = compare_values(row_value(a, col), row_value(b, col));
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

/// The value stored under `name` in a row, or [`Value::Null`] when absent.
fn row_value<'a>(row: &'a OutRow, name: &str) -> &'a Value {
    row.iter()
        .find(|(col, _)| col == name)
        .map_or(&Value::Null, |(_, value)| value)
}

/// A total, panic-free order over output values.
///
/// Values first order by variant rank
/// (`Null` < `Bool` < `I64` < `U64` < `F64` < `Ts` < `Str` < `Blob` <
/// `ListI32`), then within a variant by their natural order: floats via
/// [`f64::total_cmp`], strings and blobs by bytes, lists lexicographically.
#[allow(
    clippy::match_same_arms,
    reason = "arms bind different value types; the identical bodies are not mergeable"
)]
fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    const fn rank(value: &Value) -> u8 {
        match value {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::I64(_) => 2,
            Value::U64(_) => 3,
            Value::F64(_) => 4,
            Value::Ts(_) => 5,
            Value::Str(_) => 6,
            Value::Blob { .. } => 7,
            Value::ListI32(_) => 8,
        }
    }

    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::I64(x), Value::I64(y)) => x.cmp(y),
        (Value::U64(x), Value::U64(y)) => x.cmp(y),
        (Value::F64(x), Value::F64(y)) => x.total_cmp(y),
        (Value::Ts(x), Value::Ts(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
        (
            Value::Blob {
                text: xt,
                full_len: xl,
                truncated: xtr,
            },
            Value::Blob {
                text: yt,
                full_len: yl,
                truncated: ytr,
            },
        ) => xt
            .as_bytes()
            .cmp(yt.as_bytes())
            .then(xl.cmp(yl))
            .then(xtr.cmp(ytr)),
        (Value::ListI32(x), Value::ListI32(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::pg_stat_activity::{PgStatActivityV1, PgStatActivityV3};
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{StrId, Ts};

    use super::{
        Cursor, QueryError, QueryLimits, QueryWorkLimits, QueryWorkResource, Value,
        charge_materialization, section, section_with_limits, sections,
        sections_from_sealed_descriptor_with_limits,
    };
    use crate::LocalDirSnapshot;
    use crate::query::logical::logical_section;
    use crate::snapshot::{FORCED_STALE_OPEN_UNIT_CALLS, OPEN_UNIT_CALLS};

    /// No cursors; the common resume-nothing case for the batch entry point.
    fn no_cursors() -> BTreeMap<String, Cursor> {
        BTreeMap::new()
    }

    /// One archiver row with the given timestamp and archived count.
    fn archiver_row(ts: i64, archived: i64) -> PgStatArchiver {
        PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }
    }

    /// A minimal V3 activity row: only the sort-key columns carry data.
    fn activity_v3(ts: i64, pid: i32, leader: Option<i32>) -> PgStatActivityV3 {
        PgStatActivityV3 {
            ts: Ts(ts),
            pid,
            leader_pid: leader,
            datname: None,
            usename: None,
            application_name: StrId(0),
            client_addr: StrId(0),
            backend_type: StrId(0),
            state: None,
            wait_event_type: None,
            wait_event: None,
            query: None,
            query_id: None,
            backend_xid_age: None,
            backend_xmin_age: None,
            backend_start: Ts(ts),
            xact_start: None,
            query_start: None,
            state_change: None,
        }
    }

    /// A V1 activity row (no `leader_pid`, no `query_id`).
    fn activity_v1(ts: i64, pid: i32) -> PgStatActivityV1 {
        PgStatActivityV1 {
            ts: Ts(ts),
            pid,
            datname: None,
            usename: None,
            application_name: StrId(0),
            client_addr: StrId(0),
            backend_type: StrId(0),
            state: None,
            wait_event_type: None,
            wait_event: None,
            query: None,
            backend_xid_age: None,
            backend_xmin_age: None,
            backend_start: Ts(ts),
            xact_start: None,
            query_start: None,
            state_change: None,
        }
    }

    /// Build a part from already-encoded `(type_id, rows, body)` sections.
    fn part_from(sections: &[(u32, u32, Vec<u8>)], min_ts: i64, max_ts: i64) -> Vec<u8> {
        let mut inputs: Vec<SectionInput<'_>> = sections
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        inputs.sort_unstable_by_key(|section| section.type_id);
        build_part(&inputs, PartMeta { min_ts, max_ts })
    }

    fn write_pgm(root: &std::path::Path, segment_id: i64, part: &[u8]) {
        crate::test_layout::write_pgm(root, crate::test_layout::address(segment_id), part);
    }

    fn write_journal(root: &std::path::Path, segment_id: i64, part: &[u8]) -> std::path::PathBuf {
        crate::test_layout::write_journal(root, crate::test_layout::address(segment_id).id, &[part])
    }

    fn write_bloom_false_positive_with_broken_dictionary(root: &std::path::Path) {
        let mut sections = (0..1_024_u32)
            .map(|index| (2_000_000 + index, 1, vec![0]))
            .collect::<Vec<_>>();
        sections.push((
            kronika_registry::DICT_STRINGS_TYPE_ID,
            1,
            b"not a parquet dictionary".to_vec(),
        ));
        write_pgm(root, 5, &part_from(&sections, 0, 10));
    }

    /// Extract a named value out of one output row.
    fn cell<'a>(row: &'a super::OutRow, name: &str) -> &'a Value {
        row.iter()
            .find(|(col, _)| col == name)
            .map_or_else(|| panic!("column {name:?} present"), |(_, value)| value)
    }

    #[test]
    fn one_unit_rows_come_out_in_sort_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[
            archiver_row(3000, 3),
            archiver_row(1000, 1),
            archiver_row(2000, 2),
        ])
        .expect("encode");
        let part = part_from(&[(1_008_001, 3, body)], 1000, 3000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 100, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(
            ts,
            vec![&Value::Ts(1000), &Value::Ts(2000), &Value::Ts(3000)]
        );
        assert_eq!(page.section, "pg_stat_archiver");
        // Window [0, 10_000] over coverage [1000, 3000] leaves edge gaps.
        assert_eq!(
            page.gaps,
            vec![
                super::Gap { from: 0, to: 1000 },
                super::Gap {
                    from: 3000,
                    to: 10_000
                },
            ]
        );
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn coalesced_window_reads_all_rows_of_a_type() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(2000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 100, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(ts, vec![&Value::Ts(1000), &Value::Ts(2000)]);
    }

    #[test]
    fn merge_two_sealed_units_orders_across_units() {
        let dir = tempfile::tempdir().unwrap();
        let body_a = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(3000, 3)])
            .expect("encode");
        let part_a = part_from(&[(1_008_001, 2, body_a)], 1000, 3000);
        write_pgm(dir.path(), 1000, &part_a);

        let body_b = PgStatArchiver::encode(&[archiver_row(2000, 2), archiver_row(4000, 4)])
            .expect("encode");
        let part_b = part_from(&[(1_008_001, 2, body_b)], 2000, 4000);
        write_pgm(dir.path(), 2000, &part_b);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 2);
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 100, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(
            ts,
            vec![
                &Value::Ts(1000),
                &Value::Ts(2000),
                &Value::Ts(3000),
                &Value::Ts(4000)
            ],
            "rows from both units merged and ordered by ts"
        );
    }

    #[test]
    fn exact_descriptor_query_reads_only_the_selected_sealed_unit() {
        let dir = tempfile::tempdir().unwrap();
        let body_a = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(3000, 3)])
            .expect("encode");
        let part_a = part_from(&[(1_008_001, 2, body_a)], 1000, 3000);
        write_pgm(dir.path(), 1000, &part_a);

        let body_b = PgStatArchiver::encode(&[archiver_row(2000, 2), archiver_row(4000, 4)])
            .expect("encode");
        let part_b = part_from(&[(1_008_001, 2, body_b)], 2000, 4000);
        write_pgm(dir.path(), 2000, &part_b);

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let descriptor = snap
            .sealed_descriptors()
            .find(|descriptor| descriptor.min_ts == 1000)
            .expect("first descriptor");
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        let pages = sections_from_sealed_descriptor_with_limits(
            &snap,
            &descriptor,
            1000,
            3000,
            &["pg_stat_archiver"],
            &no_cursors(),
            QueryLimits::new(100, 10_000),
        )
        .expect("exact descriptor query");

        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 1);
        let page = pages.get("pg_stat_archiver").expect("archiver page");
        let ts = page
            .rows
            .iter()
            .map(|row| cell(row, "ts"))
            .collect::<Vec<_>>();
        assert_eq!(ts, vec![&Value::Ts(1000), &Value::Ts(3000)]);
    }

    #[test]
    fn union_across_versions_fills_missing_column_with_null() {
        let dir = tempfile::tempdir().unwrap();
        // V3 unit carries leader_pid; V1 unit does not.
        let body_v3 = PgStatActivityV3::encode(&[activity_v3(1000, 10, Some(9))]).expect("encode");
        let part_v3 = part_from(&[(1_001_003, 1, body_v3)], 1000, 1000);
        write_pgm(dir.path(), 1000, &part_v3);

        let body_v1 = PgStatActivityV1::encode(&[activity_v1(2000, 20)]).expect("encode");
        let part_v1 = part_from(&[(1_001_001, 1, body_v1)], 2000, 2000);
        write_pgm(dir.path(), 2000, &part_v1);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_activity", 0, 10_000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 2);

        // The union carries leader_pid; ordering is by (ts, pid).
        let v3_row = &page.rows[0];
        assert_eq!(cell(v3_row, "ts"), &Value::Ts(1000));
        assert_eq!(cell(v3_row, "pid"), &Value::I64(10));
        assert_eq!(
            cell(v3_row, "leader_pid"),
            &Value::I64(9),
            "V3 row keeps its leader_pid"
        );

        let v1_row = &page.rows[1];
        assert_eq!(cell(v1_row, "ts"), &Value::Ts(2000));
        assert_eq!(cell(v1_row, "pid"), &Value::I64(20));
        assert_eq!(
            cell(v1_row, "leader_pid"),
            &Value::Null,
            "V1 row has no leader_pid, so the union column is Null"
        );
        // query_id (V3-only) is Null on the V1 row too.
        assert_eq!(cell(v1_row, "query_id"), &Value::Null);
    }

    #[test]
    fn out_rows_carry_the_full_union_in_logical_column_order() {
        let dir = tempfile::tempdir().unwrap();
        // Rows decoded from different layout versions must still present the
        // same column list: the full union, in logical-section order.
        let body_v3 = PgStatActivityV3::encode(&[activity_v3(1000, 10, Some(9))]).expect("encode");
        write_pgm(
            dir.path(),
            1000,
            &part_from(&[(1_001_003, 1, body_v3)], 1000, 1000),
        );
        let body_v1 = PgStatActivityV1::encode(&[activity_v1(2000, 20)]).expect("encode");
        write_pgm(
            dir.path(),
            2000,
            &part_from(&[(1_001_001, 1, body_v1)], 2000, 2000),
        );

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_activity", 0, 10_000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 2, "one row per layout version");

        let union: Vec<&str> = logical_section("pg_stat_activity")
            .expect("registered section")
            .columns
            .iter()
            .map(|col| col.name)
            .collect();
        for row in &page.rows {
            let names: Vec<&str> = row.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, union, "row lists the full union in logical order");
        }
    }

    #[test]
    fn ts_filter_drops_out_of_window_keeps_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[
            archiver_row(1000, 1),
            archiver_row(2000, 2),
            archiver_row(3000, 3),
            archiver_row(4000, 4),
        ])
        .expect("encode");
        let part = part_from(&[(1_008_001, 4, body)], 1000, 4000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // Window [2000, 3000]: boundaries included, 1000 and 4000 excluded.
        let page = section(&mut snap, "pg_stat_archiver", 2000, 3000, 100, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(ts, vec![&Value::Ts(2000), &Value::Ts(3000)]);
    }

    #[test]
    fn limit_truncates_to_first_n_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[
            archiver_row(1000, 1),
            archiver_row(2000, 2),
            archiver_row(3000, 3),
        ])
        .expect("encode");
        let part = part_from(&[(1_008_001, 3, body)], 1000, 3000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 2, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(
            ts,
            vec![&Value::Ts(1000), &Value::Ts(2000)],
            "first two by ts"
        );
    }

    #[test]
    fn unknown_section_name_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let err = sections(
            &mut snap,
            0,
            10_000,
            &["no_such_section"],
            100,
            &no_cursors(),
        )
        .unwrap_err();
        match err {
            QueryError::UnknownSection(name) => assert_eq!(name, "no_such_section"),
            other @ (QueryError::Read(_)
            | QueryError::SealedDescriptor(_)
            | QueryError::BadCursor(_)
            | QueryError::ResultTooLarge { .. }
            | QueryError::MaterializedBytesTooLarge { .. }
            | QueryError::WorkLimitExceeded { .. }) => {
                panic!("expected UnknownSection, got {other:?}")
            }
        }
    }

    #[test]
    fn materialization_budget_rejects_the_first_excess_row() {
        let mut cells = 8;
        charge_materialization(&mut cells, 2, 10).expect("the exact limit is allowed");
        assert_eq!(cells, 10);
        assert!(charge_materialization(&mut cells, 1, 10).is_err());
        assert_eq!(cells, 10);
    }

    #[test]
    fn materialization_budget_rejects_integer_overflow() {
        let mut cells = usize::MAX;
        assert!(charge_materialization(&mut cells, 1, usize::MAX).is_err());
    }

    #[test]
    fn variable_width_budget_rejects_before_building_output_values() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1_000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 1_000, 1_000);
        write_pgm(dir.path(), 1000, &part);
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let error = section_with_limits(
            &mut snap,
            "pg_stat_archiver",
            0,
            10_000,
            None,
            QueryLimits::with_bytes(100, 10_000, 1),
        )
        .expect_err("column keys alone exceed one byte");
        assert!(matches!(
            error,
            QueryError::MaterializedBytesTooLarge { max_bytes: 1 }
        ));
    }

    #[test]
    fn default_work_unit_limit_matches_the_layout_ceiling() {
        assert_eq!(
            QueryWorkLimits::default().units,
            kronika_layout::LayoutLimits::default().max_segments
        );
    }

    #[test]
    fn catalog_summary_rejects_a_definite_type_miss_before_open() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(5, 1)]).expect("encode");
        write_pgm(dir.path(), 5, &part_from(&[(1_008_001, 1, body)], 0, 10));
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let activity = logical_section("pg_stat_activity").expect("registered section");
        assert!(
            !snap
                .unit_descriptors()
                .next()
                .expect("unit")
                .may_contain_any_nonempty_type(&activity.type_ids),
            "fixture must exercise the definite-negative Bloom path"
        );
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let page = section(&mut snap, "pg_stat_activity", 0, 10, 100, None).expect("section");

        assert!(page.rows.is_empty());
        assert!(page.gaps.is_empty());
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn bloom_false_positive_is_confirmed_before_dictionary_read() {
        let dir = tempfile::tempdir().unwrap();
        write_bloom_false_positive_with_broken_dictionary(dir.path());
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let archiver = logical_section("pg_stat_archiver").expect("registered section");
        assert!(
            snap.unit_descriptors()
                .next()
                .expect("unit")
                .may_contain_any_nonempty_type(&archiver.type_ids),
            "fixture must exercise a Bloom false positive"
        );
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let page =
            section(&mut snap, "pg_stat_archiver", 0, 10, 100, None).expect("false positive");

        assert!(page.rows.is_empty());
        assert!(page.gaps.is_empty());
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn unit_work_limit_precedes_catalog_opens() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(5, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 0, 10);
        write_pgm(dir.path(), 1, &part);
        write_pgm(dir.path(), 2, &part);
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let error = section_with_limits(
            &mut snap,
            "pg_stat_activity",
            0,
            10,
            None,
            QueryLimits::new(100, 10_000).with_work_limits(QueryWorkLimits::new(
                1,
                u64::MAX,
                u64::MAX,
            )),
        )
        .expect_err("the second in-window unit exceeds the work ceiling");

        assert!(matches!(
            error,
            QueryError::WorkLimitExceeded {
                resource: QueryWorkResource::Units,
                limit: 1,
                observed: 2,
            }
        ));
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn catalog_byte_limit_precedes_open() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(5, 1)]).expect("encode");
        write_pgm(dir.path(), 5, &part_from(&[(1_008_001, 1, body)], 0, 10));
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let catalog_bytes = snap
            .unit_descriptors()
            .next()
            .expect("unit")
            .eager_open_bytes;
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let error = section_with_limits(
            &mut snap,
            "pg_stat_archiver",
            0,
            10,
            None,
            QueryLimits::new(100, 10_000).with_work_limits(QueryWorkLimits::new(
                1,
                catalog_bytes - 1,
                u64::MAX,
            )),
        )
        .expect_err("catalog open exceeds the byte ceiling");

        assert!(matches!(
            error,
            QueryError::WorkLimitExceeded {
                resource: QueryWorkResource::CatalogBytes,
                limit,
                observed,
            } if limit == catalog_bytes - 1 && observed == catalog_bytes
        ));
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 0);
    }

    #[test]
    fn dictionary_byte_limit_precedes_dictionary_decode() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(5, 1)]).expect("encode");
        let broken_dictionary = b"not a parquet dictionary".to_vec();
        write_pgm(
            dir.path(),
            5,
            &part_from(
                &[
                    (1_008_001, 1, body),
                    (
                        kronika_registry::DICT_STRINGS_TYPE_ID,
                        1,
                        broken_dictionary.clone(),
                    ),
                ],
                0,
                10,
            ),
        );
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));

        let error = section_with_limits(
            &mut snap,
            "pg_stat_archiver",
            0,
            10,
            None,
            QueryLimits::new(100, 10_000).with_work_limits(QueryWorkLimits::new(1, u64::MAX, 0)),
        )
        .expect_err("dictionary body exceeds the byte ceiling");

        assert!(matches!(
            error,
            QueryError::WorkLimitExceeded {
                resource: QueryWorkResource::DictionaryBytes,
                limit: 0,
                observed,
            } if observed == broken_dictionary.len() as u64
        ));
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn catalog_byte_budget_is_cumulative_across_stale_retries() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(5, 1)]).expect("encode");
        write_pgm(dir.path(), 5, &part_from(&[(1_008_001, 1, body)], 0, 10));
        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let catalog_bytes = snap
            .unit_descriptors()
            .next()
            .expect("unit")
            .eager_open_bytes;
        OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| calls.set(1));

        let error = section_with_limits(
            &mut snap,
            "pg_stat_archiver",
            0,
            10,
            None,
            QueryLimits::new(100, 10_000).with_work_limits(QueryWorkLimits::new(
                usize::MAX,
                catalog_bytes,
                u64::MAX,
            )),
        )
        .expect_err("the retried catalog open must consume the same request budget");

        FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| calls.set(0));
        assert!(matches!(
            error,
            QueryError::WorkLimitExceeded {
                resource: QueryWorkResource::CatalogBytes,
                limit,
                observed,
            } if limit == catalog_bytes && observed == catalog_bytes * 2
        ));
        assert_eq!(
            OPEN_UNIT_CALLS.with(std::cell::Cell::get),
            1,
            "the retry is rejected before the second open"
        );
    }

    #[test]
    fn batch_opens_each_unit_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        // Two sealed units, each carrying both sections.
        let arch_a = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let act_a = PgStatActivityV3::encode(&[activity_v3(1000, 5, None)]).expect("encode");
        let part_a = part_from(&[(1_008_001, 1, arch_a), (1_001_003, 1, act_a)], 1000, 1000);
        write_pgm(dir.path(), 1000, &part_a);

        let arch_b = PgStatArchiver::encode(&[archiver_row(2000, 2)]).expect("encode");
        let act_b = PgStatActivityV3::encode(&[activity_v3(2000, 6, None)]).expect("encode");
        let part_b = part_from(&[(1_008_001, 1, arch_b), (1_001_003, 1, act_b)], 2000, 2000);
        write_pgm(dir.path(), 2000, &part_b);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 2);

        OPEN_UNIT_CALLS.with(|c| c.set(0));
        let pages = sections(
            &mut snap,
            0,
            10_000,
            &["pg_stat_archiver", "pg_stat_activity"],
            100,
            &no_cursors(),
        )
        .expect("sections");
        assert_eq!(
            OPEN_UNIT_CALLS.with(std::cell::Cell::get),
            2,
            "two units, not names times units"
        );

        // Both sections resolved, both units represented.
        let arch = &pages["pg_stat_archiver"];
        assert_eq!(
            arch.rows.iter().map(|r| cell(r, "ts")).collect::<Vec<_>>(),
            vec![&Value::Ts(1000), &Value::Ts(2000)]
        );
        let act = &pages["pg_stat_activity"];
        assert_eq!(
            act.rows.iter().map(|r| cell(r, "pid")).collect::<Vec<_>>(),
            vec![&Value::I64(5), &Value::I64(6)]
        );
    }

    #[test]
    fn section_equals_sections_of_one_name() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(2000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let one = section(&mut snap, "pg_stat_archiver", 0, 10_000, 100, None).expect("section");
        let many = sections(
            &mut snap,
            0,
            10_000,
            &["pg_stat_archiver"],
            100,
            &no_cursors(),
        )
        .expect("sections");
        assert_eq!(one, many["pg_stat_archiver"]);
    }

    #[test]
    fn active_unit_removed_mid_read_degrades_to_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 1000, 1000);
        let journal_path = write_journal(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);

        fs::remove_file(&journal_path).unwrap();

        // The unit is gone; the retry refreshes, finds nothing, and the window
        // degrades to one uncovered gap instead of an error.
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 100, None).expect("section");
        assert!(page.rows.is_empty());
        assert_eq!(
            page.gaps,
            vec![super::Gap {
                from: 0,
                to: 10_000
            }]
        );
    }

    #[test]
    fn compare_values_orders_by_variant_rank() {
        use std::cmp::Ordering;
        // Ascending by variant rank; the extreme inner values prove the rank,
        // not the payload, decides cross-variant order.
        let ascending = [
            Value::Null,
            Value::Bool(true),
            Value::I64(i64::MAX),
            Value::U64(0),
            Value::F64(f64::NEG_INFINITY),
            Value::Ts(i64::MIN),
            Value::Str("z".to_owned()),
            Value::Blob {
                text: "z".to_owned(),
                full_len: 9,
                truncated: true,
            },
            Value::ListI32(vec![i32::MAX]),
        ];
        for (i, lo) in ascending.iter().enumerate() {
            for (j, hi) in ascending.iter().enumerate() {
                let expected = i.cmp(&j);
                if expected != Ordering::Equal {
                    assert_eq!(super::compare_values(lo, hi), expected, "rank {i} vs {j}");
                }
            }
        }
    }

    #[test]
    fn compare_values_within_variant_is_natural() {
        use std::cmp::Ordering;
        assert_eq!(
            super::compare_values(&Value::I64(-5), &Value::I64(3)),
            Ordering::Less
        );
        assert_eq!(
            super::compare_values(&Value::U64(10), &Value::U64(2)),
            Ordering::Greater
        );
        assert_eq!(
            super::compare_values(&Value::Ts(7), &Value::Ts(7)),
            Ordering::Equal
        );
        assert_eq!(
            super::compare_values(&Value::Bool(false), &Value::Bool(true)),
            Ordering::Less
        );
        assert_eq!(
            super::compare_values(&Value::Str("a".to_owned()), &Value::Str("b".to_owned())),
            Ordering::Less
        );
        assert_eq!(
            super::compare_values(&Value::ListI32(vec![1, 2]), &Value::ListI32(vec![1, 3])),
            Ordering::Less
        );
    }

    #[test]
    fn compare_values_f64_uses_total_order() {
        use std::cmp::Ordering;
        assert_eq!(
            super::compare_values(&Value::F64(-0.0), &Value::F64(0.0)),
            Ordering::Less
        );
        assert_eq!(
            super::compare_values(&Value::F64(1.0), &Value::F64(f64::NAN)),
            Ordering::Less
        );
        assert_eq!(
            super::compare_values(&Value::F64(f64::NAN), &Value::F64(f64::NAN)),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_values_blob_orders_by_text_then_len_then_truncated() {
        use std::cmp::Ordering;
        let base = Value::Blob {
            text: "x".to_owned(),
            full_len: 1,
            truncated: false,
        };
        let longer = Value::Blob {
            text: "x".to_owned(),
            full_len: 2,
            truncated: false,
        };
        assert_eq!(super::compare_values(&base, &longer), Ordering::Less);
        let truncated = Value::Blob {
            text: "x".to_owned(),
            full_len: 1,
            truncated: true,
        };
        assert_eq!(super::compare_values(&base, &truncated), Ordering::Less);
    }

    #[test]
    fn compare_by_sort_key_uses_first_differing_column() {
        use std::cmp::Ordering;
        let a: super::OutRow = vec![
            ("ts".to_owned(), Value::Ts(1)),
            ("pid".to_owned(), Value::I64(9)),
        ];
        let b: super::OutRow = vec![
            ("ts".to_owned(), Value::Ts(1)),
            ("pid".to_owned(), Value::I64(5)),
        ];
        // ts ties, so pid decides: 9 > 5.
        assert_eq!(
            super::compare_by_sort_key(&a, &b, &["ts", "pid"]),
            Ordering::Greater
        );
        let c: super::OutRow = vec![
            ("ts".to_owned(), Value::Ts(2)),
            ("pid".to_owned(), Value::I64(0)),
        ];
        // First column differs, deciding regardless of the second.
        assert_eq!(
            super::compare_by_sort_key(&a, &c, &["ts", "pid"]),
            Ordering::Less
        );
        // Empty sort key: all rows are equal.
        assert_eq!(super::compare_by_sort_key(&a, &c, &[]), Ordering::Equal);
    }

    #[test]
    fn compare_by_sort_key_absent_column_ranks_as_null() {
        use std::cmp::Ordering;
        let with: super::OutRow = vec![("pid".to_owned(), Value::I64(5))];
        let without: super::OutRow = Vec::new();
        // The missing column reads as Null, which ranks below any I64.
        assert_eq!(
            super::compare_by_sort_key(&with, &without, &["pid"]),
            Ordering::Greater
        );
        assert_eq!(
            super::compare_by_sort_key(&without, &with, &["pid"]),
            Ordering::Less
        );
    }

    #[test]
    fn row_value_is_null_when_column_absent() {
        let row: super::OutRow = vec![("a".to_owned(), Value::I64(1))];
        assert_eq!(super::row_value(&row, "a"), &Value::I64(1));
        assert_eq!(super::row_value(&row, "missing"), &Value::Null);
    }

    #[test]
    fn limit_zero_yields_empty_page() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(2000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 0, None).expect("section");
        assert!(page.rows.is_empty(), "limit 0 yields no rows");
        // Coverage [1000, 2000] is read even at limit 0, so the window edges are gaps.
        assert_eq!(
            page.gaps,
            vec![
                super::Gap { from: 0, to: 1000 },
                super::Gap {
                    from: 2000,
                    to: 10_000
                },
            ]
        );
        assert!(page.next_cursor.is_none());
    }

    /// Read one archiver section, paging by `limit` and following `next_cursor`
    /// until it runs out. Returns each page's `archived_count` sequence.
    fn page_archived_counts(snap: &mut LocalDirSnapshot, limit: usize) -> Vec<Vec<i64>> {
        let mut pages = Vec::new();
        let mut cursor: Option<Cursor> = None;
        loop {
            let page = section(snap, "pg_stat_archiver", 0, 10_000, limit, cursor.clone())
                .expect("section");
            let counts: Vec<i64> = page
                .rows
                .iter()
                .map(|r| match cell(r, "archived_count") {
                    Value::I64(v) => *v,
                    other => panic!("archived_count is I64, got {other:?}"),
                })
                .collect();
            pages.push(counts);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        pages
    }

    #[test]
    fn pagination_covers_every_row_once_across_pages() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[
            archiver_row(1000, 1),
            archiver_row(2000, 2),
            archiver_row(3000, 3),
            archiver_row(4000, 4),
            archiver_row(5000, 5),
        ])
        .expect("encode");
        let part = part_from(&[(1_008_001, 5, body)], 1000, 5000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let pages = page_archived_counts(&mut snap, 2);
        // limit 2 over 5 rows: [1,2], [3,4], [5], then the stream is exhausted.
        assert_eq!(pages, vec![vec![1, 2], vec![3, 4], vec![5]]);
    }

    #[test]
    fn pagination_across_unit_boundary_loses_no_row() {
        let dir = tempfile::tempdir().unwrap();
        // Two sealed units whose rows interleave by ts, so a page that crosses
        // the boundary must merge both units, not restart per unit.
        let body_a = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(3000, 3)])
            .expect("encode");
        let part_a = part_from(&[(1_008_001, 2, body_a)], 1000, 3000);
        write_pgm(dir.path(), 1000, &part_a);

        let body_b = PgStatArchiver::encode(&[archiver_row(2000, 2), archiver_row(4000, 4)])
            .expect("encode");
        let part_b = part_from(&[(1_008_001, 2, body_b)], 2000, 4000);
        write_pgm(dir.path(), 2000, &part_b);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 2);
        let pages = page_archived_counts(&mut snap, 3);
        // Merged ts order 1000..4000: page1=[1,2,3] spans both units, page2=[4].
        assert_eq!(pages, vec![vec![1, 2, 3], vec![4]]);
    }

    #[test]
    fn pagination_breaks_ties_on_non_sort_key_columns() {
        let dir = tempfile::tempdir().unwrap();
        // Two rows share the sort key (ts), differing only in archived_count.
        // The total order must still split them so a cursor lands between.
        let body = PgStatArchiver::encode(&[archiver_row(5000, 1), archiver_row(5000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 5000, 5000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // limit 1 cuts between the two equal-ts rows.
        let page1 =
            section(&mut snap, "pg_stat_archiver", 0, 10_000, 1, None).expect("section page1");
        assert_eq!(
            page1.rows.iter().map(|r| cell(r, "ts")).collect::<Vec<_>>(),
            vec![&Value::Ts(5000)]
        );
        assert_eq!(cell(&page1.rows[0], "archived_count"), &Value::I64(1));
        let cursor = page1.next_cursor.expect("more rows remain after the tie");

        let page2 = section(&mut snap, "pg_stat_archiver", 0, 10_000, 1, Some(cursor))
            .expect("section page2");
        assert_eq!(
            cell(&page2.rows[0], "archived_count"),
            &Value::I64(2),
            "page2 continues with the second equal-ts row, no repeat or skip"
        );
        assert!(page2.next_cursor.is_none(), "two rows, both now returned");
    }

    #[test]
    fn last_page_has_no_next_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(2000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // limit equals the row count: the first page already drains the stream.
        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 2, None).expect("section");
        assert_eq!(page.rows.len(), 2);
        assert!(
            page.next_cursor.is_none(),
            "a page that returns the last row emits no cursor"
        );
    }

    #[test]
    fn broken_cursor_text_is_rejected() {
        let err = Cursor::decode("this is not a cursor").unwrap_err();
        assert!(matches!(err, QueryError::BadCursor(_)), "got {err:?}");
    }

    #[test]
    fn compare_full_breaks_sort_key_ties_on_remaining_columns() {
        use std::cmp::Ordering;
        let columns = ["ts", "archived_count"];
        let sort_key = ["ts"];
        let low: super::OutRow = vec![
            ("ts".to_owned(), Value::Ts(5)),
            ("archived_count".to_owned(), Value::I64(1)),
        ];
        let high: super::OutRow = vec![
            ("ts".to_owned(), Value::Ts(5)),
            ("archived_count".to_owned(), Value::I64(2)),
        ];
        // Equal sort key; the non-key column decides.
        assert_eq!(
            super::compare_full(&low, &high, &columns, &sort_key),
            Ordering::Less
        );
        // Sort key alone would tie these.
        assert_eq!(
            super::compare_by_sort_key(&low, &high, &sort_key),
            Ordering::Equal
        );
        // A leading sort-key difference decides before the tie-break runs.
        let later: super::OutRow = vec![
            ("ts".to_owned(), Value::Ts(6)),
            ("archived_count".to_owned(), Value::I64(0)),
        ];
        assert_eq!(
            super::compare_full(&high, &later, &columns, &sort_key),
            Ordering::Less
        );
    }

    #[test]
    fn coverage_gaps_covers_window_edges_and_holes() {
        use super::{Gap, coverage_gaps};
        // No coverage at all: the whole window is one gap.
        assert_eq!(coverage_gaps(0, 100, &[]), vec![Gap { from: 0, to: 100 }]);
        // Full coverage: no gaps.
        assert!(coverage_gaps(0, 100, &[(0, 100)]).is_empty());
        // Leading and trailing gaps around one interior block.
        assert_eq!(
            coverage_gaps(0, 100, &[(40, 60)]),
            vec![Gap { from: 0, to: 40 }, Gap { from: 60, to: 100 }]
        );
        // Overlapping and touching ranges merge, leaving no gap.
        assert!(coverage_gaps(0, 100, &[(0, 50), (40, 100)]).is_empty());
        assert!(coverage_gaps(0, 100, &[(0, 50), (50, 100)]).is_empty());
        // Unsorted input with one interior hole.
        assert_eq!(
            coverage_gaps(0, 100, &[(60, 100), (0, 40)]),
            vec![Gap { from: 40, to: 60 }]
        );
        // Ranges are clamped to the window before subtraction.
        assert_eq!(
            coverage_gaps(10, 90, &[(0, 50), (80, 200)]),
            vec![Gap { from: 50, to: 80 }]
        );
    }

    #[test]
    fn window_before_any_unit_is_one_gap_with_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(5000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 5000, 5000);
        write_pgm(dir.path(), 5000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 0, 1000, 100, None).expect("section");
        assert!(page.rows.is_empty(), "unit lies outside the window");
        assert_eq!(page.gaps, vec![super::Gap { from: 0, to: 1000 }]);
    }

    #[test]
    fn partial_coverage_leaves_leading_and_trailing_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(2000, 1), archiver_row(3000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 2000, 3000);
        write_pgm(dir.path(), 2000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 1000, 4000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 2);
        assert_eq!(
            page.gaps,
            vec![
                super::Gap {
                    from: 1000,
                    to: 2000
                },
                super::Gap {
                    from: 3000,
                    to: 4000
                },
            ]
        );
    }

    #[test]
    fn hole_between_two_units_becomes_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let a = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        write_pgm(
            dir.path(),
            1000,
            &part_from(&[(1_008_001, 1, a)], 1000, 1000),
        );
        let b = PgStatArchiver::encode(&[archiver_row(5000, 2)]).expect("encode");
        write_pgm(
            dir.path(),
            5000,
            &part_from(&[(1_008_001, 1, b)], 5000, 5000),
        );

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 1000, 5000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 2, "both samples fall in the window");
        assert_eq!(
            page.gaps,
            vec![super::Gap {
                from: 1000,
                to: 5000
            }]
        );
    }

    #[test]
    fn full_coverage_reports_no_gap() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(4000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 1000, 4000);
        write_pgm(dir.path(), 1000, &part);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 1000, 4000, 100, None).expect("section");
        assert!(page.gaps.is_empty(), "window equals coverage");
    }

    #[test]
    fn stale_active_unit_refreshes_and_reads_the_new_part() {
        let dir = tempfile::tempdir().unwrap();
        let a = part_from(
            &[(
                1_008_001,
                1,
                PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode"),
            )],
            1000,
            1000,
        );
        let journal = write_journal(dir.path(), 1000, &a);

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);

        // The journal is replaced with a different part after the snapshot was
        // taken. The first open sees a mismatched catalog (stale), refresh picks
        // up the new part, and the retry reads it consistently.
        let b = part_from(
            &[(
                1_008_001,
                1,
                PgStatArchiver::encode(&[archiver_row(2000, 9)]).expect("encode"),
            )],
            2000,
            2000,
        );
        let replacement =
            crate::test_layout::journal_bytes(crate::test_layout::address(2000).id, &[&b]);
        fs::write(&journal, replacement).unwrap();

        let page = section(&mut snap, "pg_stat_archiver", 0, 10_000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(cell(&page.rows[0], "ts"), &Value::Ts(2000));
        assert_eq!(cell(&page.rows[0], "archived_count"), &Value::I64(9));
    }
}
