//! Batch section reads across the units of a snapshot.
//!
//! [`sections`] answers several logical sections for one source and time window
//! in a single pass over the snapshot's units: each in-window unit is opened
//! once, its dictionary read once, and every requested section decoded from that
//! one open. Rows are materialized onto each section's union columns, filtered by
//! timestamp, ordered by the section's sort key, and truncated to `limit`.

use std::collections::BTreeMap;

use crate::query::logical::{LogicalSection, logical_section};
use crate::query::value::{Gap, OutRow, Value, cell_to_value};
use crate::{Cell, LocalDirSnapshot, ReadError};

/// One logical section's answer for a source and time window.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionPage {
    /// Logical section name, e.g. `"pg_stat_activity"`.
    pub section: String,
    /// Source the rows belong to.
    pub source_id: u64,
    /// Rows on the section's union columns, ordered by its sort key.
    pub rows: Vec<OutRow>,
    /// Coverage holes in the window. Always empty until a later stage fills it.
    pub gaps: Vec<Gap>,
    /// Cursor to resume after the last row. Always `None` until a later stage.
    pub next_cursor: Option<Cursor>,
}

/// Why a batch section read failed.
#[derive(Debug)]
pub enum QueryError {
    /// No registered contract carries this section name.
    UnknownSection(String),
    /// Reading a unit or decoding a section failed.
    Read(ReadError),
}

impl From<ReadError> for QueryError {
    fn from(err: ReadError) -> Self {
        Self::Read(err)
    }
}

/// Placeholder for the pagination cursor a later stage introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor;

/// Read one logical section for a source and window.
///
/// Equivalent to [`sections`] with a single name, returning that name's page.
/// A registered name always yields a page (possibly with no rows); an
/// unregistered one fails before any page is built, so [`sections`] returns
/// exactly one entry here.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] when `name` is not registered, or
/// [`QueryError::Read`] when a unit cannot be opened or decoded.
pub fn section(
    snap: &mut LocalDirSnapshot,
    name: &str,
    source: u64,
    from: i64,
    to: i64,
    limit: usize,
) -> Result<SectionPage, QueryError> {
    let pages = sections(snap, source, from, to, &[name], limit)?;
    pages
        .into_values()
        .next()
        .ok_or_else(|| QueryError::UnknownSection(name.to_owned()))
}

/// Read several logical sections for a source and window in one pass.
///
/// # Errors
///
/// Returns [`QueryError::UnknownSection`] for the first unregistered name, or
/// [`QueryError::Read`] when a unit cannot be opened or decoded.
// `&mut` is reserved for a later refresh-on-stale retry; today only `&self`
// methods run through it.
#[allow(clippy::needless_pass_by_ref_mut, reason = "later change adds refresh")]
pub fn sections(
    snap: &mut LocalDirSnapshot,
    source: u64,
    from: i64,
    to: i64,
    names: &[&str],
    limit: usize,
) -> Result<BTreeMap<String, SectionPage>, QueryError> {
    // Resolve every requested name up front; an unknown name fails the whole call.
    let mut requested: Vec<(String, LogicalSection)> = Vec::with_capacity(names.len());
    for &name in names {
        let logical =
            logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
        requested.push((name.to_owned(), logical));
    }

    // Units of this source whose time range overlaps the window, in units() order.
    let in_window: Vec<usize> = snap
        .units()
        .iter()
        .enumerate()
        .filter(|(_, meta)| meta.source_id == source && meta.max_ts >= from && meta.min_ts <= to)
        .map(|(idx, _)| idx)
        .collect();

    // One buffer per requested section, positionally aligned with `requested`.
    let mut buffers: Vec<Vec<OutRow>> = vec![Vec::new(); requested.len()];

    // One open per unit: read its dictionary once, then decode every section.
    for &idx in &in_window {
        let unit = snap.open_unit(idx)?;
        let dict = unit.dictionary()?;
        let catalog = unit.catalog();
        for (buffer, (_, logical)) in buffers.iter_mut().zip(&requested) {
            for entry in &catalog.entries {
                if !logical.type_ids.contains(&entry.type_id) {
                    continue;
                }
                for row in unit.decode_rows(entry)? {
                    // Registry contracts all carry a `ts` column; `get` (not
                    // indexing) keeps a future ts-less section from panicking.
                    let Some(&Cell::Ts(t)) = row.get("ts") else {
                        continue;
                    };
                    if t < from || t > to {
                        continue;
                    }
                    let out: OutRow = logical
                        .columns
                        .iter()
                        .map(|col| {
                            let value = row
                                .get(col.name)
                                .map_or(Value::Null, |cell| cell_to_value(cell, &dict).0);
                            (col.name.to_owned(), value)
                        })
                        .collect();
                    buffer.push(out);
                }
            }
        }
    }

    // Order each buffer by its section's sort key, then truncate to `limit`.
    let pages = requested
        .into_iter()
        .zip(buffers)
        .map(|((name, logical), mut rows)| {
            rows.sort_by(|a, b| compare_by_sort_key(a, b, logical.sort_key));
            rows.truncate(limit);
            let page = SectionPage {
                section: name.clone(),
                source_id: source,
                rows,
                gaps: Vec::new(),
                next_cursor: None,
            };
            (name, page)
        })
        .collect();
    Ok(pages)
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
    use std::fs;

    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::pg_stat_activity::{PgStatActivityV1, PgStatActivityV3};
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{StrId, Ts};

    use super::{QueryError, Value, section, sections};
    use crate::LocalDirSnapshot;
    use crate::snapshot::OPEN_UNIT_CALLS;

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
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100).expect("section");
        let ts: Vec<&Value> = page.rows.iter().map(|r| cell(r, "ts")).collect();
        assert_eq!(
            ts,
            vec![&Value::Ts(1000), &Value::Ts(2000), &Value::Ts(3000)]
        );
        assert_eq!(page.section, "pg_stat_archiver");
        assert_eq!(page.source_id, 7);
        assert!(page.gaps.is_empty());
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
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100).expect("section");
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
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100).expect("section");
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
        let page = section(&mut snap, "pg_stat_activity", 7, 0, 10_000, 100).expect("section");
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
        let page = section(&mut snap, "pg_stat_archiver", 7, 2000, 3000, 100).expect("section");
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
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 2).expect("section");
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
        let err = sections(&mut snap, 7, 0, 10_000, &["no_such_section"], 100).unwrap_err();
        match err {
            QueryError::UnknownSection(name) => assert_eq!(name, "no_such_section"),
            other @ QueryError::Read(_) => panic!("expected UnknownSection, got {other:?}"),
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
        let one = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100).expect("section");
        let many = sections(&mut snap, 7, 0, 10_000, &["pg_stat_archiver"], 100).expect("sections");
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
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100).expect("section");
        assert!(page.rows.is_empty(), "no rows for a different source");
        assert_eq!(page.source_id, 7);
    }

    #[test]
    fn active_unit_removed_before_read_surfaces_stale() {
        let dir = tempfile::tempdir().unwrap();
        let body = PgStatArchiver::encode(&[archiver_row(1000, 1)]).expect("encode");
        let part = part_from(&[(1_008_001, 1, body)], 1000, 1000, 7);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);

        fs::remove_file(&journal_path).unwrap();

        let err = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 100).unwrap_err();
        assert!(
            matches!(
                err,
                QueryError::Read(crate::ReadError::StaleSnapshot { unit_idx: 0 })
            ),
            "removed journal must surface StaleSnapshot, got: {err:?}"
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
        let page = section(&mut snap, "pg_stat_archiver", 7, 0, 10_000, 0).expect("section");
        assert!(page.rows.is_empty(), "limit 0 yields no rows");
        assert!(page.gaps.is_empty());
        assert!(page.next_cursor.is_none());
    }
}
