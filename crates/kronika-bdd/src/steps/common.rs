//! Generic assertion and oracle steps shared by every converted feature.
//!
//! Cucumber registers each step phrase once; these phrases are reused across
//! all per-feature submodules. Feature-specific glue lives in the matching
//! submodule (e.g. [`archiver`]).

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};
use kronika_registry::{Cell, ColumnType, TypeContract, registry};

use crate::BddWorld;
use crate::harness::assert_row::{RowSelector, assert_row};
use crate::harness::expected::{ExpectedColumn, ExpectedValue, parse_table};
use crate::harness::oracle::{OracleKind, assert_oracle};
use crate::steps::{docstring, table};

/// Assert that a singleton section has exactly one row matching the table.
///
/// Used by any metric whose section carries a single row (archiver, wal,
/// replication instance). The expected values are written as a `| column |
/// value |` table in the `.feature`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) has exactly one row:$")]
fn section_single_row(world: &mut BddWorld, type_id: String, step: &Step) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let contract = contract_for(type_id)?;
    let rows = table(step)?;
    let expected = parse_table(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        type_id,
        &RowSelector::SingleRow,
        true,
        &expected,
        &failure_log,
    )
}

/// Assert that a section has a row for the named session, matched by its
/// backend pid.
///
/// The optional `exactly one` qualifier additionally asserts that the section
/// carries exactly one row (singleton). `[Name]` placeholders in the table are
/// resolved to pids; `[]` is an empty `ListI32`.
///
/// Matches:
/// - `section 1_011_001 has a row for session "W":`
/// - `section 1_011_001 has exactly one row for session "W":`
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = "^section ([\\d_]+) has ((?:exactly one )?a? ?)row for session \"([^\"]+)\":$")]
fn section_row_for_session(
    world: &mut BddWorld,
    type_id: String,
    qualifier: String,
    session_name: String,
    step: &Step,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let exactly_one = qualifier.trim().starts_with("exactly one");
    let pid = world.harness.placeholder_pid(&session_name)?;
    let contract = contract_for(type_id)?;
    let rows = table(step)?;
    let expected =
        parse_table_with_empty_list(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        type_id,
        &RowSelector::ByPid { column: "pid", pid },
        exactly_one,
        &expected,
        &failure_log,
    )
}

/// Assert that a section has a row identified by a named key column and value.
///
/// The key column and value appear in the step phrase and select the row; the
/// data table checks further columns. `[Name]` placeholders in the table are
/// resolved to pids.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\d_]+) has a row with (\w+) = ([^:]+):$")]
fn section_row_by_key(
    world: &mut BddWorld,
    type_id: String,
    key_column: String,
    key_value: String,
    step: &Step,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let contract = contract_for(type_id)?;
    let key_cell = parse_key_cell(contract, &key_column, key_value.trim())?;
    let rows = table(step)?;
    let expected =
        parse_table_with_empty_list(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        type_id,
        &RowSelector::ByKey {
            column: key_column,
            cell: key_cell,
        },
        false,
        &expected,
        &failure_log,
    )
}

/// Verify a section column against an independent `PostgreSQL` oracle query.
///
/// The oracle kind (`exact`, `subset`, `top-n`, …) is named in the step; the
/// docstring carries the oracle SQL. The query runs on the scenario database.
#[then(regex = r"^section ([\d_]+) (\w+) matches the ([\w-]+) oracle:$")]
async fn section_oracle(
    world: &mut BddWorld,
    type_id: String,
    column: String,
    kind: String,
    step: &Step,
) -> Result<()> {
    let type_id = parse_type_id(&type_id)?;
    let contract = contract_for(type_id)?;
    let kind = OracleKind::parse(&kind)?;
    let sql = docstring(step)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let dsn = world.harness.database_dsn()?;
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the oracle query")?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let result = assert_oracle(
        &client,
        contract,
        &segment,
        &column,
        kind,
        sql,
        &failure_log,
    )
    .await;
    driver.abort();
    result
}

/// Parse a section id as written in features (`1_008_001` or `1008001`).
pub(crate) fn parse_type_id(raw: &str) -> Result<u32> {
    raw.replace('_', "")
        .parse::<u32>()
        .with_context(|| format!("invalid section type_id {raw:?}"))
}

/// The registry contract for `type_id`, or an error if it is not registered.
pub(crate) fn contract_for(type_id: u32) -> Result<&'static TypeContract> {
    registry()
        .iter()
        .find(|contract| contract.type_id.get() == type_id)
        .with_context(|| format!("no registered section has type_id {type_id}"))
}

/// Parse a `| column | value |` table, additionally recognising `[]` as an
/// empty `ListI32` cell.
///
/// Delegates to [`parse_table`] for all other value forms. `[]` is a literal
/// token in the feature, not a placeholder (a `[Name]` placeholder has content
/// between the brackets); the [`crate::harness::expected::placeholder`] helper
/// already rejects empty brackets, so `[]` falls through to this layer.
pub(crate) fn parse_table_with_empty_list(
    contract: &TypeContract,
    rows: &[Vec<String>],
    mut resolve_pid: impl FnMut(&str) -> Result<i32>,
) -> Result<Vec<ExpectedColumn>> {
    use anyhow::bail;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let [name, raw] = row.as_slice() else {
            bail!("expected-row table needs exactly two columns, got {row:?}");
        };
        if raw.trim() == "[]" {
            out.push(ExpectedColumn {
                name: name.clone(),
                value: ExpectedValue::Cell(Cell::ListI32(Vec::new())),
            });
            continue;
        }
        // All other forms are handled by the standard parser.
        let single = std::slice::from_ref(row);
        let parsed = parse_table(contract, single, &mut resolve_pid)?;
        out.extend(parsed);
    }
    Ok(out)
}

/// Parse `raw` as a key cell for `column` in `contract`.
///
/// Looks up the column type in the contract and parses `raw` as a scalar.
/// `[]` short-circuits the contract lookup and returns `Cell::ListI32(vec![])`.
pub(crate) fn parse_key_cell(contract: &TypeContract, column: &str, raw: &str) -> Result<Cell> {
    use anyhow::bail;

    if raw == "[]" {
        return Ok(Cell::ListI32(Vec::new()));
    }
    let col = contract.column(column).with_context(|| {
        format!(
            "section {} has no column {column:?}",
            contract.type_id.get()
        )
    })?;
    let cell = match col.ty {
        ColumnType::I8 | ColumnType::I16 => Cell::I16(
            raw.parse::<i16>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::I32 => Cell::I32(
            raw.parse::<i32>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::I64 => Cell::I64(
            raw.parse::<i64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::U8 | ColumnType::U16 | ColumnType::U32 => Cell::U32(
            raw.parse::<u32>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::U64 => Cell::U64(
            raw.parse::<u64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::F32 | ColumnType::F64 => Cell::F64(
            raw.parse::<f64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::Bool => {
            let v = match raw.to_ascii_lowercase().as_str() {
                "true" => true,
                "false" => false,
                other => bail!("{other:?} is not a boolean"),
            };
            Cell::Bool(v)
        }
        ColumnType::Ts => Cell::Ts(
            raw.parse::<i64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::StrId => {
            bail!("key column {column:?} is a StrId; use the string form in the step phrase")
        }
    };
    Ok(cell)
}

#[cfg(test)]
mod tests {
    use super::{contract_for, parse_key_cell, parse_table_with_empty_list};
    use crate::harness::expected::ExpectedValue;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{Cell, Section};

    fn no_pids(name: &str) -> anyhow::Result<i32> {
        anyhow::bail!("no session named {name:?}")
    }

    #[test]
    fn resolves_the_archiver_contract_and_rejects_an_unknown_id() {
        assert_eq!(
            contract_for(1_008_001).unwrap().type_id.get(),
            1_008_001,
            "the archiver section resolves"
        );
        assert!(
            contract_for(9_999_999).is_err(),
            "an unknown id is rejected"
        );
    }

    #[test]
    fn parse_table_with_empty_list_passes_through_normal_values() {
        let rows = vec![
            vec!["archived_count".to_owned(), "5".to_owned()],
            vec!["failed_count".to_owned(), "0".to_owned()],
        ];
        let parsed =
            parse_table_with_empty_list(&PgStatArchiver::CONTRACT, &rows, no_pids).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].value,
            ExpectedValue::Cell(Cell::I64(5)),
            "regular integer passes through"
        );
    }

    #[test]
    fn parse_table_with_empty_list_accepts_empty_list_literal() {
        let rows = vec![vec!["some_list_col".to_owned(), "[]".to_owned()]];
        let parsed =
            parse_table_with_empty_list(&PgStatArchiver::CONTRACT, &rows, no_pids).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].value,
            ExpectedValue::Cell(Cell::ListI32(Vec::new())),
            "[] produces an empty ListI32"
        );
    }

    #[test]
    fn parse_key_cell_parses_scalars_for_known_column_types() {
        let cell = parse_key_cell(&PgStatArchiver::CONTRACT, "archived_count", "42").unwrap();
        assert_eq!(cell, Cell::I64(42));

        let cell = parse_key_cell(&PgStatArchiver::CONTRACT, "failed_count", "0").unwrap();
        assert_eq!(cell, Cell::I64(0));
    }

    #[test]
    fn parse_key_cell_accepts_empty_list() {
        let cell = parse_key_cell(&PgStatArchiver::CONTRACT, "any_col", "[]").unwrap();
        assert_eq!(cell, Cell::ListI32(Vec::new()));
    }

    #[test]
    fn parse_key_cell_rejects_unparseable_integer() {
        assert!(
            parse_key_cell(&PgStatArchiver::CONTRACT, "archived_count", "not-a-number").is_err()
        );
    }

    #[test]
    fn oracle_kind_accepts_hyphenated_top_n() {
        use crate::harness::oracle::OracleKind;
        assert_eq!(OracleKind::parse("top-n").unwrap(), OracleKind::TopN);
        assert_eq!(OracleKind::parse("exact").unwrap(), OracleKind::Exact);
        assert_eq!(OracleKind::parse("subset").unwrap(), OracleKind::Subset);
        assert_eq!(
            OracleKind::parse("transformed").unwrap(),
            OracleKind::Transformed
        );
    }

    #[test]
    fn type_id_parses_with_and_without_underscores() {
        assert_eq!(super::parse_type_id("1_008_001").unwrap(), 1_008_001);
        assert_eq!(super::parse_type_id("1008001").unwrap(), 1_008_001);
        assert!(super::parse_type_id("porridge").is_err());
    }
}
