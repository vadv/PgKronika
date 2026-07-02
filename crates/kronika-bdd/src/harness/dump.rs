//! Unified failure dump for the assertion error paths.
//!
//! The guide requires that any assertion failure carry context: the decoded
//! section as a table, the oracle rows when there are any, and the tail of the
//! subprocess logs (the cluster's `server.log` and the collector's stderr).
//! This module renders that context; a long log is written to its data
//! directory, so the message carries a bounded tail rather than megabytes.

use std::fmt::Write as _;

use kronika_registry::{Cell, Row};

/// How many trailing log lines the failure message keeps.
const LOG_TAIL_LINES: usize = 40;

/// Render decoded rows as an aligned table, one column per line per row.
///
/// A flat per-row block (rather than a wide grid) keeps sections with many
/// columns readable in a CI log.
#[must_use]
pub(crate) fn rows_table(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "(no rows)".to_owned();
    }
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let _ = writeln!(out, "row {i}:");
        for (name, cell) in row {
            let _ = writeln!(out, "  {name} = {}", render_cell(cell));
        }
    }
    out.trim_end().to_owned()
}

/// Render one [`Cell`] for a diff line.
#[must_use]
pub(crate) fn render_cell(cell: &Cell) -> String {
    match cell {
        Cell::I16(v) => v.to_string(),
        Cell::I32(v) => v.to_string(),
        Cell::I64(v) => v.to_string(),
        Cell::U32(v) => v.to_string(),
        Cell::U64(v) => v.to_string(),
        Cell::F64(v) => v.to_string(),
        Cell::Bool(v) => v.to_string(),
        Cell::Ts(v) => format!("Ts({v})"),
        Cell::StrId(v) => format!("StrId({v})"),
        Cell::ListI32(v) => format!("{v:?}"),
        Cell::Null => "null".to_owned(),
    }
}

/// Keep the last [`LOG_TAIL_LINES`] lines of `log`, prefixed with a note about
/// where the full text lives.
#[must_use]
pub(crate) fn log_tail(label: &str, log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    let tail = lines[start..].join("\n");
    if start > 0 {
        format!(
            "--- {label} (last {LOG_TAIL_LINES} of {} lines) ---\n{tail}",
            lines.len()
        )
    } else {
        format!("--- {label} ---\n{tail}")
    }
}

/// Compose the full failure dump: the decoded section table, the subprocess-log
/// tail, and any extra context blocks the caller adds (oracle rows, expected
/// rows). `sections` are appended in order after the decoded rows.
///
/// `subprocess_logs` is the combined `postgres` and collector output; only its
/// tail goes into the message.
#[must_use]
pub(crate) fn section_dump(
    heading: &str,
    decoded: &[Row],
    subprocess_logs: &str,
    sections: &[(&str, String)],
) -> String {
    let mut out = format!(
        "{heading}\n--- decoded section ---\n{}",
        rows_table(decoded)
    );
    for (label, body) in sections {
        let _ = write!(out, "\n--- {label} ---\n{body}");
    }
    let _ = write!(
        out,
        "\n{}",
        log_tail("subprocess logs tail", subprocess_logs)
    );
    out
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::{log_tail, render_cell, rows_table, section_dump};
    use kronika_registry::{Cell, Row};

    fn sample_rows() -> Vec<Row> {
        let mut a = Row::new();
        a.insert("archived_count", Cell::I64(7));
        a.insert("last_archived_wal", Cell::Null);
        vec![a]
    }

    #[test]
    fn renders_each_cell_kind_distinctly() {
        assert_eq!(render_cell(&Cell::I64(7)), "7");
        assert_eq!(render_cell(&Cell::Ts(9)), "Ts(9)");
        assert_eq!(render_cell(&Cell::StrId(3)), "StrId(3)");
        assert_eq!(render_cell(&Cell::Null), "null");
        assert_eq!(render_cell(&Cell::Bool(true)), "true");
    }

    #[test]
    fn rows_table_lists_columns_per_row() {
        let table = rows_table(&sample_rows());
        assert!(table.contains("row 0:"), "row index header present");
        assert!(table.contains("archived_count = 7"), "column value present");
        assert!(table.contains("last_archived_wal = null"), "null rendered");
    }

    #[test]
    fn rows_table_handles_no_rows() {
        assert_eq!(rows_table(&[]), "(no rows)");
    }

    #[test]
    fn log_tail_keeps_only_the_last_lines_and_notes_truncation() {
        let mut log = String::new();
        for i in 0..100 {
            let _ = writeln!(log, "line{i}");
        }
        let tail = log_tail("server.log", &log);
        assert!(tail.contains("line99"), "keeps the final line");
        assert!(!tail.contains("line0\n"), "drops the earliest lines");
        assert!(tail.contains("of 100 lines"), "notes how much was dropped");
    }

    #[test]
    fn section_dump_joins_heading_rows_extra_and_log() {
        let dump = section_dump(
            "row for session W not found",
            &sample_rows(),
            "server started\nLOG: ready",
            &[("expected", "archived_count = 8".to_owned())],
        );
        assert!(dump.contains("row for session W not found"), "heading");
        assert!(dump.contains("decoded section"), "decoded block");
        assert!(dump.contains("expected"), "extra block label");
        assert!(dump.contains("subprocess logs tail"), "log block");
    }
}
