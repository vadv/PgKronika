//! Oracle steps: verify a section column against an independent `PostgreSQL`
//! query.
//!
//! The oracle SQL is written in the `.feature` and must be an independent check
//! of meaning, not a copy of the collector's own query. The scenario names the
//! oracle *kind*; the kind decides how the query result relates to the decoded
//! section:
//!
//! - `exact`: the section's column equals the query's value(s) after type
//!   normalization. For a singleton section, the query returns one scalar. The
//!   oracle SQL projects the value as the collector stores it, without applying a
//!   transformation.
//! - `transformed`: same equality check, but the oracle SQL carries the
//!   collector's transformation (a `CASE ... NULL`, a `to_timestamp`, a unit
//!   scaling) so the projected value matches the transformed section value. The
//!   distinction from `exact` is the contract on the SQL, not the comparison.
//! - `subset`: every value the query returns must appear in the section's column.
//!
//! `top-n`, `window`, and `schema` are declared by the guide but not yet
//! implemented; they return an error naming the kind so the first feature that
//! needs one adds it, rather than silently degrading to a weaker check.

use anyhow::{Context, Result, bail};
use kronika_registry::{Cell, ColumnType, Row, TypeContract};
use tokio_postgres::Client;

use crate::harness::assert_row::decode_section;
use crate::harness::dump;

/// The oracle kind named in the step, parsed from the scenario text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleKind {
    /// Section column equals the query result, after type normalization.
    Exact,
    /// Every query value appears somewhere in the section column.
    Subset,
    /// Equality check where the oracle SQL, not the harness, carries the
    /// collector's transformation.
    Transformed,
    /// Selected rows, a limit, an order, and a coverage marker.
    TopN,
    /// A timestamp window or a monotonic relation, not exact equality.
    Window,
    /// Codec/layout contract only; no live comparison.
    Schema,
}

impl OracleKind {
    /// Parse the kind word used in the step, e.g. `exact` or `top-n`.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown kind word.
    pub(crate) fn parse(word: &str) -> Result<Self> {
        let kind = match word.trim().to_ascii_lowercase().as_str() {
            "exact" => Self::Exact,
            "subset" => Self::Subset,
            "transformed" => Self::Transformed,
            "top-n" | "top_n" | "topn" => Self::TopN,
            "window" => Self::Window,
            "schema" => Self::Schema,
            other => bail!("unknown oracle kind {other:?}"),
        };
        Ok(kind)
    }
}

/// Run the oracle `sql` on `client` and compare its result to `column` of the
/// decoded section per `kind`.
///
/// # Errors
///
/// Returns an error if the section or column is unknown, the query fails, the
/// oracle kind is not yet implemented, or the comparison fails. Comparison
/// failures carry the section dump.
pub(crate) async fn assert_oracle(
    client: &Client,
    contract: &TypeContract,
    segment: &std::path::Path,
    column: &str,
    kind: OracleKind,
    sql: &str,
    subprocess_logs: &str,
) -> Result<()> {
    let ty = contract
        .column(column)
        .with_context(|| {
            format!(
                "section {} has no column {column:?}",
                contract.type_id.get()
            )
        })?
        .ty;
    let type_id = contract.type_id.get();

    let expected = query_cells(client, sql, ty)
        .await
        .with_context(|| format!("run the {kind:?} oracle for section {type_id} {column}"))?;
    let (rows, _dict) = decode_section(segment, type_id)?;
    let actual = column_cells(&rows, column);

    match kind {
        // Exact and Transformed share the equality check; they differ only in
        // whether the oracle SQL is allowed to carry the transformation.
        OracleKind::Exact | OracleKind::Transformed => {
            compare_exact(type_id, column, &expected, &actual, &rows, subprocess_logs)
        }
        OracleKind::Subset => {
            compare_subset(type_id, column, &expected, &actual, &rows, subprocess_logs)
        }
        OracleKind::TopN => deferred_kind("top-n"),
        OracleKind::Window => deferred_kind("window"),
        OracleKind::Schema => deferred_kind("schema"),
    }
}

/// A declared-but-unimplemented oracle kind. Returns an error (not a panic) that
/// names the kind, so the first scenario needing it implements it here instead
/// of the harness silently accepting a weaker check.
fn deferred_kind(kind: &str) -> Result<()> {
    bail!("oracle kind {kind:?} is not implemented yet — add it when the first scenario needs it")
}

/// Every value of `column` across the decoded rows.
fn column_cells(rows: &[Row], column: &str) -> Vec<Cell> {
    rows.iter()
        .filter_map(|row| row.get(column).cloned())
        .collect()
}

/// The section column equals the oracle result exactly.
///
/// The order-independent multiset comparison suits a singleton (one value each)
/// and unordered stat rows alike; a section that also sorts by this column will
/// still match.
fn compare_exact(
    type_id: u32,
    column: &str,
    expected: &[Cell],
    actual: &[Cell],
    rows: &[Row],
    subprocess_logs: &str,
) -> Result<()> {
    if multiset_eq(expected, actual) {
        return Ok(());
    }
    bail!(
        "{}",
        dump::section_dump(
            &format!("section {type_id} {column}: exact oracle mismatch"),
            rows,
            subprocess_logs,
            &[
                ("oracle values", render_cells(expected)),
                ("section values", render_cells(actual)),
            ],
        )
    )
}

/// Every oracle value appears among the section column's values.
fn compare_subset(
    type_id: u32,
    column: &str,
    expected: &[Cell],
    actual: &[Cell],
    rows: &[Row],
    subprocess_logs: &str,
) -> Result<()> {
    let missing: Vec<&Cell> = expected
        .iter()
        .filter(|want| !actual.contains(want))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "{}",
        dump::section_dump(
            &format!("section {type_id} {column}: subset oracle missing values"),
            rows,
            subprocess_logs,
            &[
                (
                    "missing",
                    missing
                        .iter()
                        .map(|c| dump::render_cell(c))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                ("section values", render_cells(actual)),
            ],
        )
    )
}

/// Multiset equality: same elements with the same multiplicity, any order.
fn multiset_eq(a: &[Cell], b: &[Cell]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut remaining: Vec<&Cell> = b.iter().collect();
    for want in a {
        if let Some(pos) = remaining.iter().position(|c| *c == want) {
            remaining.swap_remove(pos);
        } else {
            return false;
        }
    }
    remaining.is_empty()
}

/// Render a list of cells for a failure block.
fn render_cells(cells: &[Cell]) -> String {
    if cells.is_empty() {
        return "(none)".to_owned();
    }
    cells
        .iter()
        .map(dump::render_cell)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Run `sql` and read its first column of every row as a [`Cell`] of type `ty`.
///
/// The oracle SQL is expected to project exactly the value under test as its
/// first column. A `NULL` becomes [`Cell::Null`].
async fn query_cells(client: &Client, sql: &str, ty: ColumnType) -> Result<Vec<Cell>> {
    let rows = client.query(sql, &[]).await.context("oracle query")?;
    let mut cells = Vec::with_capacity(rows.len());
    for row in &rows {
        cells.push(cell_from_pg(row, ty)?);
    }
    Ok(cells)
}

/// Read the first column of a `PostgreSQL` row as a [`Cell`] of the section type.
///
/// The `get::<usize, Option<T>>` form maps SQL `NULL` to [`Cell::Null`]. The
/// integer widths match the section column so the comparison is exact.
fn cell_from_pg(row: &tokio_postgres::Row, ty: ColumnType) -> Result<Cell> {
    let cell = match ty {
        ColumnType::I8 | ColumnType::I16 => row
            .try_get::<_, Option<i16>>(0)
            .map(|v| v.map_or(Cell::Null, Cell::I16))?,
        ColumnType::I32 | ColumnType::U8 | ColumnType::U16 => row
            .try_get::<_, Option<i32>>(0)
            .map(|v| v.map_or(Cell::Null, Cell::I32))?,
        ColumnType::I64 | ColumnType::Ts => row
            .try_get::<_, Option<i64>>(0)
            .map(|v| v.map_or(Cell::Null, |n| into_cell(ty, n)))?,
        // PostgreSQL has no unsigned types, so an unsigned section column comes
        // back as i64; a value outside the target range is a real mismatch and
        // must error, not clamp to MAX (which would read as a spurious diff).
        ColumnType::U32 => match row.try_get::<_, Option<i64>>(0)? {
            None => Cell::Null,
            Some(n) => Cell::U32(u32::try_from(n).context("oracle value out of u32 range")?),
        },
        ColumnType::U64 => match row.try_get::<_, Option<i64>>(0)? {
            None => Cell::Null,
            Some(n) => Cell::U64(u64::try_from(n).context("oracle value out of u64 range")?),
        },
        ColumnType::F32 | ColumnType::F64 => row
            .try_get::<_, Option<f64>>(0)
            .map(|v| v.map_or(Cell::Null, Cell::F64))?,
        ColumnType::Bool => row
            .try_get::<_, Option<bool>>(0)
            .map(|v| v.map_or(Cell::Null, Cell::Bool))?,
        ColumnType::StrId => bail!(
            "a StrId column cannot be compared to a raw oracle value; \
             resolve the string in the oracle SQL and compare a text column instead"
        ),
    };
    Ok(cell)
}

/// Map a signed 64-bit oracle value to the exact cell kind of a `Ts`/`I64`
/// column.
const fn into_cell(ty: ColumnType, n: i64) -> Cell {
    match ty {
        ColumnType::Ts => Cell::Ts(n),
        _ => Cell::I64(n),
    }
}

#[cfg(test)]
mod tests {
    use super::{OracleKind, column_cells, deferred_kind, multiset_eq};
    use kronika_registry::{Cell, Row};

    #[test]
    fn parses_known_kinds_and_rejects_unknown() {
        assert_eq!(OracleKind::parse("exact").unwrap(), OracleKind::Exact);
        assert_eq!(OracleKind::parse("Subset").unwrap(), OracleKind::Subset);
        assert_eq!(
            OracleKind::parse("transformed").unwrap(),
            OracleKind::Transformed
        );
        assert_eq!(OracleKind::parse("top-n").unwrap(), OracleKind::TopN);
        assert!(OracleKind::parse("nonsense").is_err());
    }

    #[test]
    fn a_deferred_kind_errors_rather_than_panics() {
        let err = deferred_kind("top-n").expect_err("a deferred kind returns Err");
        assert!(
            err.to_string().contains("top-n"),
            "the error names the missing kind"
        );
    }

    #[test]
    fn multiset_eq_ignores_order_but_respects_multiplicity() {
        assert!(multiset_eq(
            &[Cell::I64(1), Cell::I64(2)],
            &[Cell::I64(2), Cell::I64(1)]
        ));
        assert!(!multiset_eq(&[Cell::I64(1)], &[Cell::I64(1), Cell::I64(1)]));
        assert!(!multiset_eq(&[Cell::I64(1)], &[Cell::I64(2)]));
    }

    #[test]
    fn column_cells_collects_the_named_column_from_every_row() {
        let rows = vec![
            Row::from([("v", Cell::I64(10))]),
            Row::from([("v", Cell::I64(20))]),
            Row::from([("other", Cell::I64(99))]),
        ];
        assert_eq!(column_cells(&rows, "v"), vec![Cell::I64(10), Cell::I64(20)]);
    }
}
