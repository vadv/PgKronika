//! Parse the expected-row table from a `.feature` into comparable values.
//!
//! A row assertion writes expectations as a two-column `| column | value |`
//! table. This module turns each raw string value into an [`ExpectedValue`],
//! interpreted against the column's [`ColumnType`]: an integer literal for a
//! numeric column, `true`/`false` for a boolean, a bare string for a `StrId`
//! label (compared after dictionary resolution), `null` for a `NULL` cell, and
//! `[Name]` placeholders for session backend pids. `ListI32` columns accept
//! `[]` and `[Name]`, which covers lock wait edges.
//!
//! Parsing is pure — it takes the raw strings and a placeholder resolver — so it
//! is unit-tested without a database.

use anyhow::{Context, Result, bail};
use kronika_registry::{Cell, ColumnType, TypeContract};

/// What an expected cell compares against.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExpectedValue {
    /// Compare the decoded [`Cell`] for equality.
    Cell(Cell),
    /// A `StrId` column: resolve the decoded id through the dictionary and
    /// compare the bytes to this string.
    Str(String),
}

/// One expected column and its value, parsed from a table row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExpectedColumn {
    /// The column name, as written in the `.feature`.
    pub(crate) name: String,
    /// The value to compare the decoded cell against.
    pub(crate) value: ExpectedValue,
}

/// Parse a `| column | value |` table against `contract`, resolving `[Name]`
/// placeholders to pids via `resolve_pid`.
///
/// `rows` is the raw table (cucumber's `Vec<Vec<String>>`); each row must have
/// exactly two cells. A column name that is not in the contract is an error, so
/// a typo in the `.feature` fails loudly instead of silently passing.
///
/// # Errors
///
/// Returns an error for a malformed row, an unknown column, a value that does
/// not parse for the column's type, or an unresolved placeholder.
pub(crate) fn parse_table(
    contract: &TypeContract,
    rows: &[Vec<String>],
    mut resolve_pid: impl FnMut(&str) -> Result<i32>,
) -> Result<Vec<ExpectedColumn>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let [name, raw] = row.as_slice() else {
            bail!("expected-row table needs exactly two columns, got {row:?}");
        };
        let column = contract.column(name).with_context(|| {
            format!("section {} has no column {name:?}", contract.type_id.get())
        })?;
        let value = parse_value(column.ty, raw, &mut resolve_pid)
            .with_context(|| format!("column {name:?}"))?;
        out.push(ExpectedColumn {
            name: name.clone(),
            value,
        });
    }
    Ok(out)
}

/// Parse one raw value for a column of type `ty`.
fn parse_value(
    ty: ColumnType,
    raw: &str,
    resolve_pid: &mut impl FnMut(&str) -> Result<i32>,
) -> Result<ExpectedValue> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("null") {
        return Ok(ExpectedValue::Cell(Cell::Null));
    }
    let value = match ty {
        ColumnType::ListI32 if raw == "[]" => ExpectedValue::Cell(Cell::ListI32(Vec::new())),
        ColumnType::ListI32 => {
            let Some(name) = placeholder(raw) else {
                bail!("a ListI32 expectation must be [] or a [Name] pid placeholder");
            };
            ExpectedValue::Cell(Cell::ListI32(vec![resolve_pid(name)?]))
        }
        _ if let Some(name) = placeholder(raw) => {
            ExpectedValue::Cell(pid_cell(ty, resolve_pid(name)?)?)
        }
        ColumnType::I8 | ColumnType::I16 => ExpectedValue::Cell(Cell::I16(parse_int(raw)?)),
        ColumnType::I32 => ExpectedValue::Cell(Cell::I32(parse_int(raw)?)),
        ColumnType::I64 => ExpectedValue::Cell(Cell::I64(parse_int(raw)?)),
        ColumnType::U8 | ColumnType::U16 | ColumnType::U32 => {
            ExpectedValue::Cell(Cell::U32(parse_int(raw)?))
        }
        ColumnType::U64 => ExpectedValue::Cell(Cell::U64(parse_int(raw)?)),
        ColumnType::F32 | ColumnType::F64 => ExpectedValue::Cell(Cell::F64(parse_float(raw)?)),
        ColumnType::Bool => ExpectedValue::Cell(Cell::Bool(parse_bool(raw)?)),
        ColumnType::Ts => ExpectedValue::Cell(Cell::Ts(parse_int(raw)?)),
        // A StrId cell is a raw dictionary id; the expectation is the human
        // string, compared after the harness resolves the id.
        ColumnType::StrId => ExpectedValue::Str(unquote(raw)),
    };
    Ok(value)
}

/// The name inside a `[Name]` placeholder, if `raw` is one.
fn placeholder(raw: &str) -> Option<&str> {
    raw.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// Build the cell a resolved pid compares against, for a pid-typed column.
fn pid_cell(ty: ColumnType, pid: i32) -> Result<Cell> {
    match ty {
        ColumnType::I32 => Ok(Cell::I32(pid)),
        ColumnType::I64 => Ok(Cell::I64(i64::from(pid))),
        other => bail!("a [Name] pid placeholder does not fit column type {other:?}"),
    }
}

/// Parse an integer that fits the target type, rejecting overflow.
fn parse_int<T>(raw: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse::<T>()
        .map_err(|err| anyhow::anyhow!("{raw:?} is not a valid integer: {err}"))
}

/// Parse a floating-point literal.
fn parse_float(raw: &str) -> Result<f64> {
    raw.parse::<f64>()
        .map_err(|err| anyhow::anyhow!("{raw:?} is not a valid float: {err}"))
}

/// Parse `true`/`false`, case-insensitively.
fn parse_bool(raw: &str) -> Result<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => bail!("{other:?} is not a boolean"),
    }
}

/// Strip one pair of surrounding double quotes, if present.
fn unquote(raw: &str) -> String {
    raw.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(raw)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{ExpectedValue, parse_table, parse_value, placeholder};
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{Cell, ColumnType, Section};

    fn no_pids(name: &str) -> anyhow::Result<i32> {
        anyhow::bail!("no session named {name:?}")
    }

    #[test]
    fn parses_each_value_kind_for_its_column_type() {
        let mut resolve = no_pids;
        assert_eq!(
            parse_value(ColumnType::I64, "42", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::I64(42))
        );
        assert_eq!(
            parse_value(ColumnType::Bool, "true", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::Bool(true))
        );
        assert_eq!(
            parse_value(ColumnType::Ts, "1700000000000000", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::Ts(1_700_000_000_000_000))
        );
        assert_eq!(
            parse_value(
                ColumnType::StrId,
                "\"000000010000000000000001\"",
                &mut resolve
            )
            .unwrap(),
            ExpectedValue::Str("000000010000000000000001".to_owned()),
            "a StrId expectation is the human string, unquoted"
        );
        assert_eq!(
            parse_value(ColumnType::StrId, "null", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::Null),
            "null is a NULL cell regardless of column type"
        );
        assert_eq!(
            parse_value(ColumnType::ListI32, "[]", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::ListI32(Vec::new())),
            "an empty list literal compares to an empty ListI32 cell"
        );
    }

    #[test]
    fn resolves_a_pid_placeholder_to_the_session_cell() {
        let mut resolve = |name: &str| {
            assert_eq!(name, "W", "the placeholder name is passed through");
            Ok(4242)
        };
        assert_eq!(
            parse_value(ColumnType::I32, "[W]", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::I32(4242))
        );
        assert_eq!(
            parse_value(ColumnType::ListI32, "[W]", &mut resolve).unwrap(),
            ExpectedValue::Cell(Cell::ListI32(vec![4242])),
            "a ListI32 placeholder becomes a one-element pid list"
        );
    }

    #[test]
    fn placeholder_detects_the_bracket_form_only() {
        assert_eq!(placeholder("[H]"), Some("H"));
        assert_eq!(placeholder("[ W ]"), Some("W"));
        assert_eq!(placeholder("plain"), None);
        assert_eq!(
            placeholder("[]"),
            None,
            "an empty placeholder is not a name"
        );
    }

    #[test]
    fn rejects_a_bad_integer_and_an_unknown_column() {
        let mut resolve = no_pids;
        assert!(parse_value(ColumnType::I64, "not-a-number", &mut resolve).is_err());

        let rows = vec![vec!["no_such_column".to_owned(), "1".to_owned()]];
        assert!(
            parse_table(&PgStatArchiver::CONTRACT, &rows, no_pids).is_err(),
            "a column not in the contract is rejected"
        );
    }

    #[test]
    fn parses_a_full_archiver_table() {
        let rows = vec![
            vec!["archived_count".to_owned(), "0".to_owned()],
            vec!["failed_count".to_owned(), "0".to_owned()],
            vec!["last_archived_wal".to_owned(), "null".to_owned()],
        ];
        let parsed = parse_table(&PgStatArchiver::CONTRACT, &rows, no_pids).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].name, "archived_count");
        assert_eq!(parsed[0].value, ExpectedValue::Cell(Cell::I64(0)));
        assert_eq!(parsed[2].value, ExpectedValue::Cell(Cell::Null));
    }
}
