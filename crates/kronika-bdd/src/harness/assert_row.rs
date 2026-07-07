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
use kronika_registry::{
    Bytes, Cell, MAX_SECTION_BYTES, Row, VerifiedSection, decode_rows, section_name,
};

use crate::harness::dump;
use crate::harness::expected::{ExpectedColumn, ExpectedValue};

/// How the assertion picks the row to check.
///
/// `SingleRow` serves singleton sections; `ByPid` matches a session's backend
/// pid; `ByKey` matches one scalar key column; `ByKeys` matches a conjunction of
/// scalar and dictionary-string keys, which covers the per-database metrics that
/// select a row by `datname` plus `relname`/label columns.
#[derive(Debug, Clone)]
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
    /// Find the row that matches every key in the conjunction.
    ///
    /// A scalar key compares the decoded [`Cell`]; a string key resolves the
    /// `StrId` column through the segment dictionary. Multi-key metrics use this
    /// to select by e.g. `relname` plus `datname`, or by three `pg_stat_io`
    /// label columns.
    ByKeys(Vec<KeyMatch>),
}

/// One key of a [`RowSelector::ByKeys`] conjunction.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum KeyMatch {
    /// The row's `column` cell equals `cell`.
    Cell {
        /// The key column name.
        column: String,
        /// The value to match.
        cell: Cell,
    },
    /// The row's `column` is a `StrId` that resolves to `value`.
    Str {
        /// The `StrId` column name, e.g. `datname`, `relname`.
        column: String,
        /// The expected string value.
        value: String,
    },
}

impl KeyMatch {
    /// Whether `row` satisfies this key, resolving `StrId` cells through `dict`.
    fn matches(&self, row: &Row, dict: &Dictionary) -> bool {
        match self {
            Self::Cell { column, cell } => row.get(column.as_str()) == Some(cell),
            Self::Str { column, value } => str_matches(row, column, value, dict),
        }
    }
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
    decode_section_labeled(path, type_id, &fallback_section_label(type_id))
}

/// Decode a section, using `section_label` in diagnostics.
pub(crate) fn decode_section_labeled(
    path: &Path,
    type_id: u32,
    section_label: &str,
) -> Result<(Vec<Row>, Dictionary)> {
    use std::os::unix::fs::FileExt;

    let segment = Segment::open(path).context("open sealed segment")?;
    let entry = segment
        .catalog()
        .entries
        .iter()
        .find(|entry| entry.type_id == type_id)
        .with_context(|| format!("segment has no section {section_label}"))?;
    let len = usize::try_from(entry.len)
        .with_context(|| format!("section {section_label} len overflows usize"))?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section {section_label} of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;
    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section {section_label} crc check failed: {err}"))?;
    let rows = decode_rows(type_id, verified)
        .with_context(|| format!("generic decode of section {section_label}"))?;
    let dict = segment
        .dictionary()
        .context("read the segment dictionary")?;
    Ok((rows, dict))
}

fn fallback_section_label(type_id: u32) -> String {
    section_name(type_id).map_or_else(|| type_id.to_string(), str::to_owned)
}

/// Assert that the selected row of section `section_label` matches `expected`.
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
    section_label: &str,
    selector: &RowSelector,
    single_row_expected: bool,
    expected: &[ExpectedColumn],
    subprocess_logs: &str,
) -> Result<()> {
    let (rows, dict) = decode_section_labeled(path, type_id, section_label)?;

    if single_row_expected && rows.len() != 1 {
        bail!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {section_label}: expected exactly one row, got {}",
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
                &format!("section {section_label}: no row matched {selector:?}"),
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
            &format!(
                "section {section_label}: {} column(s) did not match",
                diffs.len()
            ),
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
/// String keys resolve through `dict`; scalar selectors ignore it.
fn select_row<'a>(rows: &'a [Row], selector: &RowSelector, dict: &Dictionary) -> Option<&'a Row> {
    match selector {
        RowSelector::SingleRow => rows.first(),
        RowSelector::ByPid { column, pid } => {
            rows.iter().find(|row| pid_matches(row, column, *pid))
        }
        RowSelector::ByKey { column, cell } => rows
            .iter()
            .find(|row| row.get(column.as_str()) == Some(cell)),
        RowSelector::ByKeys(keys) => rows
            .iter()
            .find(|row| keys.iter().all(|key| key.matches(row, dict))),
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
        ExpectedValue::Cell(want) => (!cells_match(actual, want)).then(|| {
            format!(
                "{}: expected {}, got {}",
                col.name,
                dump::render_cell(want),
                dump::render_cell(actual),
            )
        }),
        ExpectedValue::Str(want) => compare_str(&col.name, actual, want, dict),
        ExpectedValue::AtLeast(floor) => compare_at_least(&col.name, actual, *floor),
    }
}

fn cells_match(actual: &Cell, want: &Cell) -> bool {
    match (actual, want) {
        (Cell::F64(actual), Cell::F64(want)) => floats_match(*actual, *want),
        _ => actual == want,
    }
}

fn floats_match(actual: f64, want: f64) -> bool {
    if !actual.is_finite() || !want.is_finite() {
        return actual.to_bits() == want.to_bits();
    }
    let tolerance = 0.000_001_f64.max(want.abs() * 0.000_000_001);
    (actual - want).abs() <= tolerance
}

/// Compare an `i64` counter cell against a `>= floor` expectation.
fn compare_at_least(name: &str, actual: &Cell, floor: i64) -> Option<String> {
    match actual {
        Cell::I64(value) => {
            (*value < floor).then(|| format!("{name}: expected >= {floor}, got {value}"))
        }
        other => Some(format!(
            "{name}: expected an i64 >= {floor}, got {}",
            dump::render_cell(other)
        )),
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
            ExpectedValue::AtLeast(floor) => format!(">= {floor}"),
        };
        let _ = writeln!(out, "  {} = {value}", col.name);
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::{KeyMatch, RowSelector, compare_column, pid_matches, select_row};
    use crate::harness::expected::{ExpectedColumn, ExpectedValue};
    use kronika_reader::{Dictionary, Segment};
    use kronika_registry::{Cell, Row};

    fn row(pairs: &[(&'static str, Cell)]) -> Row {
        pairs.iter().cloned().collect()
    }

    /// A real segment dictionary resolving `values`, returned with their ids.
    fn dictionary_of(values: &[&str]) -> (Dictionary, Vec<u64>) {
        use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};

        let limits = DictLimits::new(1 << 10, 1 << 20).expect("limits");
        let mut interner = kronika_writer::Interner::new(limits);
        let ids: Vec<u64> = values
            .iter()
            .map(|v| interner.intern(v.as_bytes()).expect("intern").get())
            .collect();
        let dict_sections = kronika_writer::dict::encode(interner.window()).expect("encode");
        let section_inputs: Vec<_> = dict_sections
            .iter()
            .map(|s| SectionInput {
                type_id: s.type_id,
                rows: s.rows,
                body: &s.body,
            })
            .collect();
        let bytes = build_part(
            &section_inputs,
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dict.pgm");
        std::fs::write(&path, &bytes).expect("write segment");
        let dict = Segment::open(&path)
            .expect("open segment")
            .dictionary()
            .expect("read dictionary");
        (dict, ids)
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
    fn by_keys_requires_every_scalar_key_to_match() {
        let dict = Dictionary::default();
        let rows = vec![
            row(&[
                ("datid", Cell::U32(1)),
                ("relid", Cell::U32(9)),
                ("v", Cell::I64(10)),
            ]),
            row(&[
                ("datid", Cell::U32(2)),
                ("relid", Cell::U32(9)),
                ("v", Cell::I64(20)),
            ]),
        ];
        let selector = RowSelector::ByKeys(vec![
            KeyMatch::Cell {
                column: "datid".to_owned(),
                cell: Cell::U32(2),
            },
            KeyMatch::Cell {
                column: "relid".to_owned(),
                cell: Cell::U32(9),
            },
        ]);
        let found = select_row(&rows, &selector, &dict).expect("row with datid=2 and relid=9");
        assert_eq!(found["v"], Cell::I64(20), "the conjunction picks one row");

        let no_match = RowSelector::ByKeys(vec![
            KeyMatch::Cell {
                column: "datid".to_owned(),
                cell: Cell::U32(2),
            },
            KeyMatch::Cell {
                column: "relid".to_owned(),
                cell: Cell::U32(999),
            },
        ]);
        assert!(
            select_row(&rows, &no_match, &dict).is_none(),
            "one unmet key fails the whole conjunction"
        );
    }

    #[test]
    fn by_keys_resolves_string_keys_through_the_dictionary() {
        let (dict, ids) = dictionary_of(&["probe", "kronika_db", "other_db"]);
        let rows = vec![
            row(&[
                ("relname", Cell::StrId(ids[0])),
                ("datname", Cell::StrId(ids[1])),
                ("v", Cell::I64(10)),
            ]),
            row(&[
                ("relname", Cell::StrId(ids[0])),
                ("datname", Cell::StrId(ids[2])),
                ("v", Cell::I64(20)),
            ]),
        ];
        let selector = RowSelector::ByKeys(vec![
            KeyMatch::Str {
                column: "relname".to_owned(),
                value: "probe".to_owned(),
            },
            KeyMatch::Str {
                column: "datname".to_owned(),
                value: "other_db".to_owned(),
            },
        ]);
        let found = select_row(&rows, &selector, &dict).expect("probe in other_db");
        assert_eq!(
            found["v"],
            Cell::I64(20),
            "the same table name in another database is a distinct row"
        );

        let mixed = RowSelector::ByKeys(vec![
            KeyMatch::Str {
                column: "relname".to_owned(),
                value: "probe".to_owned(),
            },
            KeyMatch::Str {
                column: "datname".to_owned(),
                value: "absent_db".to_owned(),
            },
        ]);
        assert!(
            select_row(&rows, &mixed, &dict).is_none(),
            "an unmatched string key fails the conjunction"
        );
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
    fn compare_column_tolerates_small_float_rounding() {
        let dict = Dictionary::default();
        let r = row(&[("duration_ms", Cell::F64(1_500.250_000_000_1))]);
        let want_ok = ExpectedColumn {
            name: "duration_ms".to_owned(),
            value: ExpectedValue::Cell(Cell::F64(1500.25)),
        };
        assert!(
            compare_column(&r, &want_ok, &dict).is_none(),
            "small parquet/arrow float roundoff is accepted"
        );

        let want_bad = ExpectedColumn {
            name: "duration_ms".to_owned(),
            value: ExpectedValue::Cell(Cell::F64(1500.0)),
        };
        assert!(
            compare_column(&r, &want_bad, &dict).is_some(),
            "material float differences still fail"
        );
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
