//! Latest-row lookup over one coherent single-root snapshot.

use super::logical::{LogicalSection, logical_section};
use super::section::{MAX_REFRESH, QueryError, compare_full};
use super::value::{OutRow, Value, cell_to_value};
use crate::snapshot::UnitHandle;
use crate::{Cell, LocalDirSnapshot, ReadError, UnitMeta};

/// Read the latest row of a logical section.
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
) -> Result<Option<OutRow>, QueryError> {
    let logical =
        logical_section(name).ok_or_else(|| QueryError::UnknownSection(name.to_owned()))?;
    let mut refreshed = 0_u32;
    loop {
        match latest_once(snapshot, &logical) {
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
) -> Result<Option<OutRow>, LatestError> {
    let mut candidates: Vec<(usize, UnitHandle, UnitMeta)> = snapshot
        .unit_descriptors()
        .filter(|unit| unit.may_contain_any_nonempty_type(&logical.type_ids))
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
