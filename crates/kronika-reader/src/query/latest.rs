//! Bounded lookup of the latest row in one logical section.

use super::logical::{LogicalSection, logical_section};
use super::section::{MAX_REFRESH, QueryError, compare_full};
use super::value::{OutRow, Value, cell_to_value};
use crate::{Cell, LocalDirSnapshot, ReadError, UnitMeta};

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
        let skip_stale = refreshed >= MAX_REFRESH;
        match latest_once(snapshot, &logical, source, skip_stale) {
            Ok(row) => return Ok(row),
            Err(LatestError::Stale) => {
                snapshot
                    .refresh()
                    .map_err(|error| QueryError::Read(ReadError::Io(error)))?;
                refreshed += 1;
            }
            Err(LatestError::Read(error)) => return Err(QueryError::Read(error)),
        }
    }
}

enum LatestError {
    Stale,
    Read(ReadError),
}

fn latest_once(
    snapshot: &LocalDirSnapshot,
    logical: &LogicalSection,
    source: u64,
    skip_stale: bool,
) -> Result<Option<OutRow>, LatestError> {
    let units = snapshot.units();
    let mut candidates: Vec<(usize, UnitMeta)> = units
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, unit)| {
            unit.source_id == source
                && snapshot.unit_catalog(*index).is_some_and(|catalog| {
                    catalog
                        .entries
                        .iter()
                        .any(|entry| entry.rows != 0 && logical.type_ids.contains(&entry.type_id))
                })
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .1
            .max_ts
            .cmp(&left.1.max_ts)
            .then_with(|| right.1.live.cmp(&left.1.live))
            .then_with(|| right.0.cmp(&left.0))
    });

    let union_columns: Vec<&str> = logical.columns.iter().map(|column| column.name).collect();
    let mut best: Option<(i64, OutRow)> = None;
    for (index, unit_meta) in candidates {
        if best
            .as_ref()
            .is_some_and(|(best_ts, _)| unit_meta.max_ts < *best_ts)
        {
            break;
        }
        let unit = match snapshot.open_unit(index) {
            Ok(unit) => unit,
            Err(ReadError::StaleSnapshot { .. }) if skip_stale => continue,
            Err(ReadError::StaleSnapshot { .. }) => return Err(LatestError::Stale),
            Err(error) => return Err(LatestError::Read(error)),
        };
        let dictionary = unit.dictionary().map_err(LatestError::Read)?;
        for entry in &unit.catalog().entries {
            if entry.rows == 0 || !logical.type_ids.contains(&entry.type_id) {
                continue;
            }
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

#[cfg(test)]
mod tests {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::Ts;
    use kronika_registry::pg_log::PgLogSourceStatusV1;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;

    use super::latest_section_row;
    use crate::LocalDirSnapshot;
    use crate::query::Value;
    use crate::snapshot::OPEN_UNIT_CALLS;

    fn write_status(
        dir: &std::path::Path,
        file: &str,
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
        std::fs::write(dir.join(file), part).expect("write PGM");
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
        std::fs::write(dir.join("old.pgm"), part).expect("write old-store PGM");
    }

    #[test]
    fn latest_row_uses_row_timestamp_across_overlapping_units() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_status(dir.path(), "wide.pgm", 7, 0, 1_000, 100, 2);
        write_status(dir.path(), "older-max.pgm", 7, 0, 900, 800, 0);
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
        write_status(dir.path(), "new.pgm", 7, 200, 300, 250, 0);
        write_status(dir.path(), "old.pgm", 7, 100, 200, 190, 2);
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
    fn latest_row_rejects_an_unregistered_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
        let error =
            latest_section_row(&mut snapshot, "not_registered", 7).expect_err("unknown section");
        assert!(
            matches!(error, crate::QueryError::UnknownSection(name) if name == "not_registered")
        );
    }
}
