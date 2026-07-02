//! Generic row assertion: decode a section, select one row, and compare each
//! named column to the value written in the `.feature`.
//!
//! The comparison is column-level: a mismatch names the column, the expected
//! value, and the actual decoded value, alongside the full [`dump`] of the
//! section. `StrId` columns are resolved through the segment dictionary so the
//! expectation can be the human string, not the internal id.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use kronika_format::crc32c;
use kronika_reader::{Dictionary, Resolved, Segment};
use kronika_registry::{Bytes, Cell, MAX_SECTION_BYTES, Row, VerifiedSection, decode_rows};

use crate::harness::dump;
use crate::harness::expected::{ExpectedColumn, ExpectedValue};

/// How the assertion picks the row to check.
///
/// `SingleRow` serves singleton sections; `ByPid`, `ByKey`, and `ByStr` are the
/// harness API for the per-session, keyed-row, and string-keyed assertions that
/// per-database metric features use.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "ByPid/ByKey are harness API for session-keyed and key-column assertions; archiver uses SingleRow, prepared_xacts uses ByStr"
)]
pub(crate) enum RowSelector {
    /// The section must hold exactly one row; check that one.
    SingleRow,
    /// Find the row whose `column` (an integer pid column) equals `pid`.
    ByPid {
        /// The pid column name, e.g. `pid`.
        column: &'static str,
        /// The session backend pid to match.
        pid: i32,
    },
    /// Find the row whose `column` equals `cell`.
    ByKey {
        /// The key column name, e.g. `datid`, `queryid`, `slot_name`.
        column: String,
        /// The key value to match.
        cell: Cell,
    },
    /// Find the row whose `column` (a `StrId` column) resolves to `value`
    /// through the segment dictionary.
    ///
    /// Used for string-keyed metrics like `pg_prepared_xacts` where the key
    /// column is stored as a dictionary id, not a scalar.
    ByStr {
        /// The `StrId` column name, e.g. `datname`.
        column: String,
        /// The expected string value.
        value: String,
    },
    /// Find the row whose `StrId` columns resolve to the expected values.
    ByStrFields {
        /// The `(column, expected value)` pairs that must all match.
        fields: Vec<(String, String)>,
    },
}

/// Decode a section into generic rows and load the segment dictionary.
///
/// Reads the catalog entry for `type_id`, CRC-checks the bytes, and decodes.
///
/// # Errors
///
/// Returns an error if the segment has no such section, the section is too
/// large, the CRC fails, or decoding fails.
pub(crate) fn decode_section(path: &Path, type_id: u32) -> Result<(Vec<Row>, Dictionary)> {
    use std::os::unix::fs::FileExt;

    let segment = Segment::open(path).context("open sealed segment")?;
    let entry = segment
        .catalog()
        .entries
        .iter()
        .find(|entry| entry.type_id == type_id)
        .with_context(|| format!("segment has no section {type_id}"))?;
    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section {type_id} of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;
    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section {type_id} crc check failed: {err}"))?;
    let rows = decode_rows(type_id, verified)
        .with_context(|| format!("generic decode of section {type_id}"))?;
    let dict = segment
        .dictionary()
        .context("read the segment dictionary")?;
    Ok((rows, dict))
}

/// Assert that the selected row of section `type_id` matches `expected`.
///
/// `subprocess_logs` (the cluster and collector output) is included in the
/// failure dump. When `single_row_expected` is set (the `exactly one row`
/// phrasing), a section with more than one row fails even before selection.
///
/// # Errors
///
/// Returns an error if the section is missing, the row cannot be selected, the
/// row count contradicts `single_row_expected`, or any column mismatches. Every
/// error carries the full section dump.
pub(crate) fn assert_row(
    path: &Path,
    type_id: u32,
    selector: &RowSelector,
    single_row_expected: bool,
    expected: &[ExpectedColumn],
    subprocess_logs: &str,
) -> Result<()> {
    let (rows, dict) = decode_section(path, type_id)?;

    if single_row_expected && rows.len() != 1 {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {type_id}: expected exactly one row, got {}",
                    rows.len()
                ),
                &rows,
                subprocess_logs,
                &[("expected", render_expected(expected))],
            )
        );
    }

    let row = select_row(&rows, selector, &dict).ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            dump::section_dump(
                &format!("section {type_id}: no row matched {selector:?}"),
                &rows,
                subprocess_logs,
                &[("expected", render_expected(expected))],
            )
        )
    })?;

    let mut diffs = Vec::new();
    for col in expected {
        if let Some(diff) = compare_column(row, col, &dict) {
            diffs.push(diff);
        }
    }
    if diffs.is_empty() {
        return Ok(());
    }
    bail!(
        "{}",
        dump::section_dump(
            &format!("section {type_id}: {} column(s) did not match", diffs.len()),
            &rows,
            subprocess_logs,
            &[
                ("expected", render_expected(expected)),
                ("column diffs", diffs.join("\n")),
            ],
        )
    )
}

/// Pick the row named by `selector`, if present.
///
/// `ByStr` resolves through `dict`; all other selectors ignore it.
fn select_row<'a>(rows: &'a [Row], selector: &RowSelector, dict: &Dictionary) -> Option<&'a Row> {
    match selector {
        RowSelector::SingleRow => rows.first(),
        RowSelector::ByPid { column, pid } => {
            rows.iter().find(|row| pid_matches(row, column, *pid))
        }
        RowSelector::ByKey { column, cell } => rows
            .iter()
            .find(|row| row.get(column.as_str()) == Some(cell)),
        RowSelector::ByStr { column, value } => rows
            .iter()
            .find(|row| str_matches(row, column, value, dict)),
        RowSelector::ByStrFields { fields } => rows.iter().find(|row| {
            fields
                .iter()
                .all(|(column, value)| str_matches(row, column, value, dict))
        }),
    }
}

/// Whether `row`'s `StrId` column resolves to `value` through the dictionary.
fn str_matches(row: &Row, column: &str, value: &str, dict: &Dictionary) -> bool {
    let Some(Cell::StrId(id)) = row.get(column) else {
        return false;
    };
    match dict.resolve(*id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => bytes == value.as_bytes(),
        None => false,
    }
}

/// Whether `row`'s integer pid column equals `pid` (I32 or widened I64).
fn pid_matches(row: &Row, column: &str, pid: i32) -> bool {
    match row.get(column) {
        Some(Cell::I32(v)) => *v == pid,
        Some(Cell::I64(v)) => *v == i64::from(pid),
        _ => false,
    }
}

/// Compare one expected column to the row's cell; return a diff line on
/// mismatch, or `None` when they agree.
fn compare_column(row: &Row, col: &ExpectedColumn, dict: &Dictionary) -> Option<String> {
    let Some(actual) = row.get(col.name.as_str()) else {
        return Some(format!("{}: column absent from decoded row", col.name));
    };
    match &col.value {
        ExpectedValue::Cell(want) => (actual != want).then(|| {
            format!(
                "{}: expected {}, got {}",
                col.name,
                dump::render_cell(want),
                dump::render_cell(actual),
            )
        }),
        ExpectedValue::Str(want) => compare_str(&col.name, actual, want, dict),
    }
}

/// Compare a `StrId` cell to an expected string by resolving it.
fn compare_str(name: &str, actual: &Cell, want: &str, dict: &Dictionary) -> Option<String> {
    let Cell::StrId(id) = actual else {
        return Some(format!(
            "{name}: expected the string {want:?}, but the cell is {}",
            dump::render_cell(actual)
        ));
    };
    match dict.resolve(*id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => (bytes != want.as_bytes())
            .then(|| {
                format!(
                    "{name}: expected {want:?}, got {:?} (str_id {id})",
                    String::from_utf8_lossy(bytes)
                )
            }),
        None => Some(format!(
            "{name}: str_id {id} did not resolve through the dictionary"
        )),
    }
}

/// Render the expected columns for the failure dump.
fn render_expected(expected: &[ExpectedColumn]) -> String {
    let mut out = String::new();
    for col in expected {
        let value = match &col.value {
            ExpectedValue::Cell(cell) => dump::render_cell(cell),
            ExpectedValue::Str(s) => format!("{s:?}"),
        };
        let _ = writeln!(out, "  {} = {value}", col.name);
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::{RowSelector, compare_column, pid_matches, select_row};
    use crate::harness::expected::{ExpectedColumn, ExpectedValue};
    use kronika_reader::Dictionary;
    use kronika_registry::{Cell, Row};

    fn row(pairs: &[(&'static str, Cell)]) -> Row {
        pairs.iter().cloned().collect()
    }

    #[test]
    fn single_row_selector_takes_the_first() {
        let dict = Dictionary::default();
        let rows = vec![row(&[("archived_count", Cell::I64(1))])];
        assert!(select_row(&rows, &RowSelector::SingleRow, &dict).is_some());
        assert!(
            select_row(&[], &RowSelector::SingleRow, &dict).is_none(),
            "no rows, nothing to select"
        );
    }

    #[test]
    fn by_pid_matches_i32_and_widened_i64_columns() {
        assert!(pid_matches(&row(&[("pid", Cell::I32(42))]), "pid", 42));
        assert!(pid_matches(&row(&[("pid", Cell::I64(42))]), "pid", 42));
        assert!(!pid_matches(&row(&[("pid", Cell::I32(7))]), "pid", 42));
        assert!(
            !pid_matches(&row(&[("pid", Cell::Null)]), "pid", 42),
            "a null pid never matches"
        );
    }

    #[test]
    fn by_key_finds_the_matching_row() {
        let dict = Dictionary::default();
        let rows = vec![
            row(&[("datid", Cell::U32(1)), ("v", Cell::I64(10))]),
            row(&[("datid", Cell::U32(2)), ("v", Cell::I64(20))]),
        ];
        let selector = RowSelector::ByKey {
            column: "datid".to_owned(),
            cell: Cell::U32(2),
        };
        let found = select_row(&rows, &selector, &dict).expect("row with datid=2");
        assert_eq!(found["v"], Cell::I64(20));
    }

    #[test]
    fn compare_column_reports_a_value_mismatch() {
        let dict = Dictionary::default();
        let r = row(&[("archived_count", Cell::I64(7))]);
        let want_ok = ExpectedColumn {
            name: "archived_count".to_owned(),
            value: ExpectedValue::Cell(Cell::I64(7)),
        };
        assert!(
            compare_column(&r, &want_ok, &dict).is_none(),
            "equal cells agree"
        );

        let want_bad = ExpectedColumn {
            name: "archived_count".to_owned(),
            value: ExpectedValue::Cell(Cell::I64(8)),
        };
        let diff = compare_column(&r, &want_bad, &dict).expect("a diff");
        assert!(diff.contains("expected 8"), "diff names the expected value");
        assert!(diff.contains("got 7"), "diff names the actual value");
    }

    #[test]
    fn compare_column_flags_an_absent_column() {
        let dict = Dictionary::default();
        let r = row(&[("archived_count", Cell::I64(7))]);
        let want = ExpectedColumn {
            name: "failed_count".to_owned(),
            value: ExpectedValue::Cell(Cell::I64(0)),
        };
        assert!(
            compare_column(&r, &want, &dict)
                .expect("a diff")
                .contains("absent"),
            "an absent column is reported, not silently skipped"
        );
    }

    #[test]
    fn compare_str_fails_when_the_cell_is_not_a_strid() {
        let dict = Dictionary::default();
        let r = row(&[("last_archived_wal", Cell::I64(1))]);
        let want = ExpectedColumn {
            name: "last_archived_wal".to_owned(),
            value: ExpectedValue::Str("00000001".to_owned()),
        };
        assert!(
            compare_column(&r, &want, &dict)
                .expect("a diff")
                .contains("the cell is"),
            "a string expectation against a non-StrId cell is a mismatch"
        );
    }
}
