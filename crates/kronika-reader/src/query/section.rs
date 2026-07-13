//! Batch section reads across the units of a snapshot.
//!
//! [`sections`] answers several logical sections for one source and time window
//! in a single pass over the snapshot's units: each in-window unit is opened
//! once, its dictionary read once, and every requested section decoded from that
//! one open. Rows are materialized onto each section's union columns, filtered by
//! timestamp, ordered by the section's sort key, and truncated to `limit`.

use std::collections::BTreeMap;

use crate::query::cursor::Cursor;
use crate::query::logical::{LogicalSection, logical_section};
use crate::query::value::{Gap, OutRow, Value, cell_to_value};
use crate::{Cell, LocalDirSnapshot, ReadError};

/// How many times `sections` refreshes a stale snapshot before giving up on the
/// stale unit and letting its time fall into a gap.
const MAX_REFRESH: u32 = 2;

/// One logical section's answer for a source and time window.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionPage {
    /// Logical section name, e.g. `"pg_stat_activity"`.
    pub section: String,
    /// Source the rows belong to.
    pub source_id: u64,
    /// Rows on the section's union columns, ordered by its sort key.
    pub rows: Vec<OutRow>,
    /// Stretches of the window that no readable unit covers.
    pub gaps: Vec<Gap>,
    /// Cursor to resume after the last returned row, or `None` when this page
    /// exhausts the stream.
    pub next_cursor: Option<Cursor>,
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
}

impl From<ReadError> for QueryError {
    fn from(err: ReadError) -> Self {
        Self::Read(err)
    }
}

/// Read one logical section for a source and window.
///
/// Equivalent to [`sections`] with a single name, returning that name's page.
/// A registered name always yields a page (possibly with no rows); an
/// unregistered one fails before any page is built, so [`sections`] returns
/// exactly one entry here. `cursor`, when set, resumes after the row it pins.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] when `name` is not registered,
/// [`QueryError::BadCursor`] when `cursor` targets another source, or
/// [`QueryError::Read`] when a unit cannot be opened or decoded.
pub fn section(
    snap: &mut LocalDirSnapshot,
    name: &str,
    source: u64,
    from: i64,
    to: i64,
    limit: usize,
    cursor: Option<Cursor>,
) -> Result<SectionPage, QueryError> {
    let cursors: BTreeMap<String, Cursor> =
        cursor.map(|c| (name.to_owned(), c)).into_iter().collect();
    let pages = sections(snap, source, from, to, &[name], limit, &cursors)?;
    pages
        .into_values()
        .next()
        .ok_or_else(|| QueryError::UnknownSection(name.to_owned()))
}

/// Read several logical sections for a source and window in one pass.
///
/// A section named in `cursors` resumes after the row its cursor pins: rows are
/// ordered by [`compare_full`], every row at or before the cursor is dropped,
/// and the remaining tail is paged. When the tail exceeds `limit`, the page's
/// `next_cursor` pins its last row so a further call continues the stream.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] for the first unregistered name,
/// [`QueryError::BadCursor`] when a cursor targets another source, or
/// [`QueryError::Read`] when a unit cannot be opened or decoded.
pub fn sections(
    snap: &mut LocalDirSnapshot,
    source: u64,
    from: i64,
    to: i64,
    names: &[&str],
    limit: usize,
    cursors: &BTreeMap<String, Cursor>,
) -> Result<BTreeMap<String, SectionPage>, QueryError> {
    // Resolve every requested name up front; an unknown name fails the whole call.
    let mut requested: Vec<(String, LogicalSection)> = Vec::with_capacity(names.len());
    for &name in names {
        let logical =
            logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
        requested.push((name.to_owned(), logical));
    }

    // Gather rows and the time ranges actually read. A unit that goes stale mid
    // read (concurrent seal/reset) triggers a snapshot refresh and a full retry,
    // up to MAX_REFRESH times; after that the still-stale unit is skipped and its
    // time drops out of coverage, surfacing as a gap.
    let mut refreshed: u32 = 0;
    let (buffers, covered) = loop {
        let skip_stale = refreshed >= MAX_REFRESH;
        match gather(snap, source, from, to, &requested, skip_stale) {
            Ok(gathered) => break gathered,
            Err(GatherError::Stale) => {
                snap.refresh()
                    .map_err(|err| QueryError::Read(ReadError::Io(err)))?;
                refreshed += 1;
            }
            Err(GatherError::Read(err)) => return Err(QueryError::Read(err)),
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
            if cursor.source_id != source {
                return Err(QueryError::BadCursor(format!(
                    "cursor source {} does not match query source {source}",
                    cursor.source_id
                )));
            }
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

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        // A cursor pins the last returned row, so an empty page (e.g. `limit`
        // of zero) never emits one, even when rows remain.
        let next_cursor = rows.last().filter(|_| has_more).map(|row| Cursor {
            source_id: source,
            values: row.iter().map(|(_, v)| v.clone()).collect(),
        });

        let page = SectionPage {
            section: name.clone(),
            source_id: source,
            rows,
            gaps: gaps.clone(),
            next_cursor,
        };
        pages.insert(name, page);
    }
    Ok(pages)
}

/// Failure while gathering a window's rows.
enum GatherError {
    /// A unit went stale (concurrent seal/reset); the caller should refresh and retry.
    Stale,
    /// A read failed for a reason a refresh will not fix.
    Read(ReadError),
}

/// Per-section row buffers plus the `[min, max]` ranges actually read.
type Gathered = (Vec<Vec<OutRow>>, Vec<(i64, i64)>);

/// Decode every requested section from the source's in-window units in one pass.
///
/// Opens each unit once, reads its dictionary once, and returns per-section row
/// buffers alongside the units' `[min, max]` ranges. With `skip_stale` a unit
/// that opens stale is skipped; otherwise the first stale unit returns
/// [`GatherError::Stale`] so the caller can refresh and retry.
fn gather(
    snap: &LocalDirSnapshot,
    source: u64,
    from: i64,
    to: i64,
    requested: &[(String, LogicalSection)],
    skip_stale: bool,
) -> Result<Gathered, GatherError> {
    let metas = snap.units();
    let in_window: Vec<usize> = metas
        .iter()
        .enumerate()
        .filter(|(_, meta)| meta.source_id == source && meta.max_ts >= from && meta.min_ts <= to)
        .map(|(idx, _)| idx)
        .collect();

    let mut buffers: Vec<Vec<OutRow>> = vec![Vec::new(); requested.len()];
    let mut covered: Vec<(i64, i64)> = Vec::new();

    for &idx in &in_window {
        let unit = match snap.open_unit(idx) {
            Ok(unit) => unit,
            Err(ReadError::StaleSnapshot { .. }) if skip_stale => continue,
            Err(ReadError::StaleSnapshot { .. }) => return Err(GatherError::Stale),
            Err(err) => return Err(GatherError::Read(err)),
        };
        let dict = unit.dictionary().map_err(GatherError::Read)?;
        let catalog = unit.catalog();
        covered.push((metas[idx].min_ts, metas[idx].max_ts));
        for (buffer, (_, logical)) in buffers.iter_mut().zip(requested) {
            for entry in &catalog.entries {
                if !logical.type_ids.contains(&entry.type_id) {
                    continue;
                }
                let rows = unit.decode_rows(entry).map_err(GatherError::Read)?;
                let Some(first) = rows.first() else {
                    continue;
                };
                // Cell positions are fixed per contract, so resolve each union
                // column (and `ts`) to its index once per entry, not per row.
                // A missing `ts` skips the rows rather than panicking, keeping
                // a future ts-less section harmless.
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
                    if t < from || t > to {
                        continue;
                    }
                    let out: OutRow = logical
                        .columns
                        .iter()
                        .zip(&cell_at)
                        .map(|(col, at)| {
                            let value = at
                                .and_then(|at| cells.get(at))
                                .map_or(Value::Null, |cell| cell_to_value(cell, &dict).0);
                            (col.name.to_owned(), value)
                        })
                        .collect();
                    buffer.push(out);
                }
            }
        }
    }
    Ok((buffers, covered))
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
fn compare_full(a: &OutRow, b: &OutRow, columns: &[&str], sort_key: &[&str]) -> std::cmp::Ordering {
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

    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::pg_stat_activity::{PgStatActivityV1, PgStatActivityV3};
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{StrId, Ts};

    use super::{Cursor, QueryError, Value, section, sections};
    use crate::LocalDirSnapshot;
    use crate::snapshot::OPEN_UNIT_CALLS;

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
    fn part_from(
        sections: &[(u32, u32, Vec<u8>)],
        min_ts: i64,
        max_ts: i64,
        source: u64,
    ) -> Vec<u8> {
        let inputs: Vec<SectionInput<'_>> = sections
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        build_part(
            &inputs,
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        )
    }

    /// Wrap part bytes in a journal frame.
    fn framed(part: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &FrameHeader {
                part_len: part.len() as u64,
            }
            .encode(),
        );
        buf.extend_from_slice(part);
        buf
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
        let part = part_from(&[(1_008_001, 3, body)], 1000, 3000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(
            ts,
            vec![&Value::Ts(1000), &Value::Ts(2000), &Value::Ts(3000)]
        );
        assert_eq!(page.section, "pg_stat_archiver");
        assert_eq!(page.source_id, 7);
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
    fn multi_window_reads_all_entries_of_a_type() {
        let dir = tempfile::tempdir().unwrap();
        // Two entries of the same type_id in one unit (a multi-window part).
        let body_a = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let body_b = PgStatArchiver::encode(&[archiver_row(2000, 2)]).expect("encode");
        let part = part_from(
            &[(1_008_001, 1, body_a), (1_008_001, 1, body_b)],
            1000,
            2000,
            7,
        );
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(ts, vec![&Value::Ts(1000), &Value::Ts(2000)]);
    }

    #[test]
    fn merge_two_sealed_units_orders_across_units() {
        let dir = tempfile::tempdir().unwrap();
        let body_a = PgStatArchiver::encode(&[archiver_row(1000, 1), archiver_row(3000, 3)])
            .expect("encode");
        let part_a = part_from(&[(1_008_001, 2, body_a)], 1000, 3000, 7);
        fs::write(dir.path().join("1000.pgm"), &part_a).unwrap();

        let body_b = PgStatArchiver::encode(&[archiver_row(2000, 2), archiver_row(4000, 4)])
            .expect("encode");
        let part_b = part_from(&[(1_008_001, 2, body_b)], 2000, 4000, 7);
        fs::write(dir.path().join("2000.pgm"), &part_b).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 2);
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
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
    fn union_across_versions_fills_missing_column_with_null() {
        let dir = tempfile::tempdir().unwrap();
        // V3 unit carries leader_pid; V1 unit does not.
        let body_v3 = PgStatActivityV3::encode(&[activity_v3(1000, 10, Some(9))]).expect("encode");
        let part_v3 = part_from(&[(1_001_003, 1, body_v3)], 1000, 1000, 7);
        fs::write(dir.path().join("1000.pgm"), &part_v3).unwrap();

        let body_v1 = PgStatActivityV1::encode(&[activity_v1(2000, 20)]).expect("encode");
        let part_v1 = part_from(&[(1_001_001, 1, body_v1)], 2000, 2000, 7);
        fs::write(dir.path().join("2000.pgm"), &part_v1).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_activity", 7, 0, 10_000, 100, None).expect("section");
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
        fs::write(
            dir.path().join("1000.pgm"),
            part_from(&[(1_001_003, 1, body_v3)], 1000, 1000, 7),
        )
        .unwrap();
        let body_v1 = PgStatActivityV1::encode(&[activity_v1(2000, 20)]).expect("encode");
        fs::write(
            dir.path().join("2000.pgm"),
            part_from(&[(1_001_001, 1, body_v1)], 2000, 2000, 7),
        )
        .unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_activity", 7, 0, 10_000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 2, "one row per layout version");

        let union: Vec<&str> = crate::query::logical::logical_section("pg_stat_activity")
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
        let part = part_from(&[(1_008_001, 4, body)], 1000, 4000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // Window [2000, 3000]: boundaries included, 1000 and 4000 excluded.
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 2000, 3000, 100, None).expect("section");
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
        let part = part_from(&[(1_008_001, 3, body)], 1000, 3000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 2, None).expect("section");
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
            7,
            0,
            10_000,
            &["no_such_section"],
            100,
            &no_cursors(),
        )
        .unwrap_err();
        match err {
            QueryError::UnknownSection(name) => assert_eq!(name, "no_such_section"),
            other @ (QueryError::Read(_) | QueryError::BadCursor(_)) => {
                panic!("expected UnknownSection, got {other:?}")
            }
        }
    }

    #[test]
    fn batch_opens_each_unit_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        // Two sealed units, each carrying both sections.
        let arch_a = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let act_a = PgStatActivityV3::encode(&[activity_v3(1000, 5, None)]).expect("encode");
        let part_a = part_from(
            &[(1_008_001, 1, arch_a), (1_001_003, 1, act_a)],
            1000,
            1000,
            7,
        );
        fs::write(dir.path().join("1000.pgm"), &part_a).unwrap();

        let arch_b = PgStatArchiver::encode(&[archiver_row(2000, 2)]).expect("encode");
        let act_b = PgStatActivityV3::encode(&[activity_v3(2000, 6, None)]).expect("encode");
        let part_b = part_from(
            &[(1_008_001, 1, arch_b), (1_001_003, 1, act_b)],
            2000,
            2000,
            7,
        );
        fs::write(dir.path().join("2000.pgm"), &part_b).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 2);

        OPEN_UNIT_CALLS.with(|c| c.set(0));
        let pages = sections(
            &mut snap,
            7,
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
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let one = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
        let many = sections(
            &mut snap,
            7,
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
    fn other_source_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 1000, 1000, 42);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // Query source 7 — the only unit belongs to source 42.
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
        assert!(page.rows.is_empty(), "no rows for a different source");
        assert_eq!(page.source_id, 7);
    }

    #[test]
    fn active_unit_removed_mid_read_degrades_to_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 1000, 1000, 7);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);

        fs::remove_file(&journal_path).unwrap();

        // The unit is gone; the retry refreshes, finds nothing, and the window
        // degrades to one uncovered gap instead of an error.
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
        assert!(page.rows.is_empty());
        assert_eq!(
            page.gaps,
            vec![super::Gap {
                from: 0,
                to: 10_000
            }]
        );
    }

    // ---- pure ordering helpers ----

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
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 0, None).expect("section");
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

    // ---- keyset pagination ----

    /// Read one archiver section, paging by `limit` and following `next_cursor`
    /// until it runs out. Returns each page's `archived_count` sequence.
    fn page_archived_counts(
        snap: &mut LocalDirSnapshot,
        source: u64,
        limit: usize,
    ) -> Vec<Vec<i64>> {
        let mut pages = Vec::new();
        let mut cursor: Option<Cursor> = None;
        loop {
            let page = section(
                snap,
                "pg_stat_archiver",
                source,
                0,
                10_000,
                limit,
                cursor.clone(),
            )
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
        let part = part_from(&[(1_008_001, 5, body)], 1000, 5000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let pages = page_archived_counts(&mut snap, 7, 2);
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
        let part_a = part_from(&[(1_008_001, 2, body_a)], 1000, 3000, 7);
        fs::write(dir.path().join("1000.pgm"), &part_a).unwrap();

        let body_b = PgStatArchiver::encode(&[archiver_row(2000, 2), archiver_row(4000, 4)])
            .expect("encode");
        let part_b = part_from(&[(1_008_001, 2, body_b)], 2000, 4000, 7);
        fs::write(dir.path().join("2000.pgm"), &part_b).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 2);
        let pages = page_archived_counts(&mut snap, 7, 3);
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
        let part = part_from(&[(1_008_001, 2, body)], 5000, 5000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // limit 1 cuts between the two equal-ts rows.
        let page1 =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 1, None).expect("section page1");
        assert_eq!(
            page1.rows.iter().map(|r| cell(r, "ts")).collect::<Vec<_>>(),
            vec![&Value::Ts(5000)]
        );
        assert_eq!(cell(&page1.rows[0], "archived_count"), &Value::I64(1));
        let cursor = page1.next_cursor.expect("more rows remain after the tie");

        let page2 = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 1, Some(cursor))
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
        let part = part_from(&[(1_008_001, 2, body)], 1000, 2000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // limit equals the row count: the first page already drains the stream.
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 2, None).expect("section");
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
    fn cursor_from_another_source_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 1000, 1000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // A cursor minted for source 42, replayed against source 7.
        let foreign = Cursor {
            source_id: 42,
            values: vec![Value::Ts(1000)],
        };
        let err = section(
            &mut snap,
            "pg_stat_archiver",
            7,
            0,
            10_000,
            100,
            Some(foreign),
        )
        .unwrap_err();
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

    // ---- stale-retry + coverage gaps ----

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
        let part = part_from(&[(1_008_001, 1, body)], 5000, 5000, 7);
        fs::write(dir.path().join("5000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 1000, 100, None).expect("section");
        assert!(page.rows.is_empty(), "unit lies outside the window");
        assert_eq!(page.gaps, vec![super::Gap { from: 0, to: 1000 }]);
    }

    #[test]
    fn partial_coverage_leaves_leading_and_trailing_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(2000, 1), archiver_row(3000, 2)])
            .expect("encode");
        let part = part_from(&[(1_008_001, 2, body)], 2000, 3000, 7);
        fs::write(dir.path().join("2000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 1000, 4000, 100, None).expect("section");
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
        fs::write(
            dir.path().join("1000.pgm"),
            part_from(&[(1_008_001, 1, a)], 1000, 1000, 7),
        )
        .unwrap();
        let b = PgStatArchiver::encode(&[archiver_row(5000, 2)]).expect("encode");
        fs::write(
            dir.path().join("5000.pgm"),
            part_from(&[(1_008_001, 1, b)], 5000, 5000, 7),
        )
        .unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 1000, 5000, 100, None).expect("section");
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
        let part = part_from(&[(1_008_001, 2, body)], 1000, 4000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let page =
            section(&mut snap, "pg_stat_archiver", 7, 1000, 4000, 100, None).expect("section");
        assert!(page.gaps.is_empty(), "window equals coverage");
    }

    #[test]
    fn stale_active_unit_refreshes_and_reads_the_new_part() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("active.parts");
        let a = part_from(
            &[(
                1_008_001,
                1,
                PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode"),
            )],
            1000,
            1000,
            7,
        );
        fs::write(&journal, framed(&a)).unwrap();

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
            7,
        );
        fs::write(&journal, framed(&b)).unwrap();

        let page =
            section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100, None).expect("section");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(cell(&page.rows[0], "ts"), &Value::Ts(2000));
        assert_eq!(cell(&page.rows[0], "archived_count"), &Value::I64(9));
    }
}
