//! Steps for `features/user_tables.feature`.
//!
//! Two scenarios: `pg_stat_user_tables` and `pg_stat_user_indexes`, each using
//! two isolated databases created per scenario to prevent cross-scenario
//! collisions.

use anyhow::{Context, Result, bail};
use cucumber::{gherkin::Step, then};
use kronika_reader::Resolved;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;
use crate::harness::oracle::{OracleKind, assert_oracle};
use crate::steps::docstring;

const USER_TABLES_V3_TYPE_ID: u32 = 1_013_003;
const USER_INDEXES_V2_TYPE_ID: u32 = 1_014_002;

/// Assert a `pg_stat_user_tables` row for `relname` in the primary database.
///
/// The data table may contain `>= N` floor assertions for cumulative counters
/// where autovacuum can bump the value beyond the seeded amount.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^section 1_013_\d+ has a pg_stat_user_tables row for table "([^"]+)" in the primary database:$"#
)]
fn user_tables_row_primary(world: &mut BddWorld, relname: String, step: &Step) -> Result<()> {
    let datname = world.harness.database()?.to_owned();
    assert_user_tables_row(world, &datname, &relname, step)
}

/// Assert a `pg_stat_user_tables` row for `relname` in the second database.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^section 1_013_\d+ has a pg_stat_user_tables row for table "([^"]+)" in the second database:$"#
)]
fn user_tables_row_second(world: &mut BddWorld, relname: String, step: &Step) -> Result<()> {
    let datname = world.harness.extra_database_name(0)?.to_owned();
    assert_user_tables_row(world, &datname, &relname, step)
}

/// Assert a `pg_stat_user_indexes` row for `indexrelname` in the primary database.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^section 1_014_\d+ has a pg_stat_user_indexes row for index "([^"]+)" in the primary database:$"#
)]
fn user_indexes_row_primary(world: &mut BddWorld, indexrelname: String, step: &Step) -> Result<()> {
    let datname = world.harness.database()?.to_owned();
    assert_user_indexes_row(world, &datname, &indexrelname, step)
}

/// Assert a `pg_stat_user_indexes` row for `indexrelname` in the second database.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^section 1_014_\d+ has a pg_stat_user_indexes row for index "([^"]+)" in the second database:$"#
)]
fn user_indexes_row_second(world: &mut BddWorld, indexrelname: String, step: &Step) -> Result<()> {
    let datname = world.harness.extra_database_name(0)?.to_owned();
    assert_user_indexes_row(world, &datname, &indexrelname, step)
}

/// Run a subset oracle for `n_tup_ins` in the primary database's `pg_stat_user_tables` section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^pg_stat_user_tables n_tup_ins for "([^"]+)" in the primary database matches the subset oracle:$"#
)]
async fn user_tables_n_tup_ins_oracle_primary(
    world: &mut BddWorld,
    relname: String,
    step: &Step,
) -> Result<()> {
    drop(relname);
    let sql = docstring(step)?;
    run_section_oracle(world, USER_TABLES_V3_TYPE_ID, "n_tup_ins", sql, false).await
}

/// Run a subset oracle for `n_tup_ins` in the second database's `pg_stat_user_tables` section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^pg_stat_user_tables n_tup_ins for "([^"]+)" in the second database matches the subset oracle:$"#
)]
async fn user_tables_n_tup_ins_oracle_second(
    world: &mut BddWorld,
    relname: String,
    step: &Step,
) -> Result<()> {
    drop(relname);
    let sql = docstring(step)?;
    run_section_oracle(world, USER_TABLES_V3_TYPE_ID, "n_tup_ins", sql, true).await
}

/// Run a subset oracle for `idx_scan` in the primary database's `pg_stat_user_indexes` section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^pg_stat_user_indexes idx_scan for "([^"]+)" in the primary database matches the subset oracle:$"#
)]
async fn user_indexes_idx_scan_oracle_primary(
    world: &mut BddWorld,
    indexrelname: String,
    step: &Step,
) -> Result<()> {
    drop(indexrelname);
    let sql = docstring(step)?;
    run_section_oracle(world, USER_INDEXES_V2_TYPE_ID, "idx_scan", sql, false).await
}

/// Run a subset oracle for `idx_scan` in the second database's `pg_stat_user_indexes` section.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^pg_stat_user_indexes idx_scan for "([^"]+)" in the second database matches the subset oracle:$"#
)]
async fn user_indexes_idx_scan_oracle_second(
    world: &mut BddWorld,
    indexrelname: String,
    step: &Step,
) -> Result<()> {
    drop(indexrelname);
    let sql = docstring(step)?;
    run_section_oracle(world, USER_INDEXES_V2_TYPE_ID, "idx_scan", sql, true).await
}

/// Find and check a `pg_stat_user_tables` row matching (datname, relname).
fn assert_user_tables_row(
    world: &mut BddWorld,
    datname: &str,
    relname: &str,
    step: &Step,
) -> Result<()> {
    let segment = world.harness.segment()?;
    let (rows, dict) = decode_section(segment, USER_TABLES_V3_TYPE_ID)?;
    let failure_log = world.harness.failure_log()?;

    let row = rows.iter().find(|row| {
        str_cell_eq(row.get("datname"), datname, &dict)
            && str_cell_eq(row.get("relname"), relname, &dict)
    });

    let row = row.ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {USER_TABLES_V3_TYPE_ID}: no row for datname={datname:?} relname={relname:?}"
                ),
                &rows,
                &failure_log,
                &[],
            )
        )
    })?;

    let empty: &[Vec<String>] = &[];
    let table_rows = step.table.as_ref().map_or(empty, |t| t.rows.as_slice());

    let mut diffs = Vec::new();
    for table_row in table_rows {
        let [col_name, raw_val] = table_row.as_slice() else {
            bail!("expected table needs exactly two columns, got {table_row:?}");
        };
        if let Some(diff) = check_column(row, col_name, raw_val.trim()) {
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
                "section {USER_TABLES_V3_TYPE_ID}: row for datname={datname:?} relname={relname:?}: {} column(s) did not match",
                diffs.len()
            ),
            &rows,
            &failure_log,
            &[("column diffs", diffs.join("\n"))],
        )
    )
}

/// Find and check a `pg_stat_user_indexes` row matching (datname, indexrelname).
fn assert_user_indexes_row(
    world: &mut BddWorld,
    datname: &str,
    indexrelname: &str,
    step: &Step,
) -> Result<()> {
    let segment = world.harness.segment()?;
    let (rows, dict) = decode_section(segment, USER_INDEXES_V2_TYPE_ID)?;
    let failure_log = world.harness.failure_log()?;

    let row = rows.iter().find(|row| {
        str_cell_eq(row.get("datname"), datname, &dict)
            && str_cell_eq(row.get("indexrelname"), indexrelname, &dict)
    });

    let row = row.ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            dump::section_dump(
                &format!(
                    "section {USER_INDEXES_V2_TYPE_ID}: no row for datname={datname:?} indexrelname={indexrelname:?}"
                ),
                &rows,
                &failure_log,
                &[],
            )
        )
    })?;

    let empty: &[Vec<String>] = &[];
    let table_rows = step.table.as_ref().map_or(empty, |t| t.rows.as_slice());

    let mut diffs = Vec::new();
    for table_row in table_rows {
        let [col_name, raw_val] = table_row.as_slice() else {
            bail!("expected table needs exactly two columns, got {table_row:?}");
        };
        if let Some(diff) = check_column(row, col_name, raw_val.trim()) {
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
                "section {USER_INDEXES_V2_TYPE_ID}: row for datname={datname:?} indexrelname={indexrelname:?}: {} column(s) did not match",
                diffs.len()
            ),
            &rows,
            &failure_log,
            &[("column diffs", diffs.join("\n"))],
        )
    )
}

/// Run a subset oracle against the correct database DSN (primary or extra[0]).
async fn run_section_oracle(
    world: &mut BddWorld,
    type_id: u32,
    column: &str,
    sql: &str,
    use_extra: bool,
) -> Result<()> {
    use crate::steps::common::contract_for;

    let contract = contract_for(type_id)?;
    let dsn = if use_extra {
        world.harness.extra_database_dsn(0)?
    } else {
        world.harness.database_dsn()?
    };
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;

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
        column,
        OracleKind::Subset,
        sql,
        &failure_log,
    )
    .await;
    driver.abort();
    result
}

/// Check one column value from the step's data table against a decoded row.
///
/// Supports `>= N` for i64 window assertions; booleans as `true`/`false`.
/// Returns `None` when the column is absent from the row (the caller handles
/// absent columns separately via the row-not-found error).
fn check_column(row: &kronika_registry::Row, col_name: &str, raw_val: &str) -> Option<String> {
    let actual = row.get(col_name)?;

    // Window/tolerance: `>= N` floor check for cumulative i64 counters.
    if let Some(floor_str) = raw_val.strip_prefix(">=").map(str::trim) {
        return check_at_least(col_name, actual, floor_str);
    }

    // Boolean comparison.
    let bool_expected = match raw_val.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    if let Some(expected) = bool_expected {
        return match actual {
            Cell::Bool(v) => {
                (*v != expected).then(|| format!("{col_name}: expected {expected}, got {v}"))
            }
            other => Some(format!(
                "{col_name}: expected bool {expected}, got {}",
                dump::render_cell(other)
            )),
        };
    }

    None
}

/// Check that `actual` (an i64 counter) is `>= floor`.
fn check_at_least(col_name: &str, actual: &Cell, floor_str: &str) -> Option<String> {
    let floor = match floor_str.parse::<i64>() {
        Ok(n) => n,
        Err(e) => {
            return Some(format!("{col_name}: cannot parse floor {floor_str:?}: {e}"));
        }
    };
    let actual_val = match actual {
        Cell::I64(v) => *v,
        other => {
            return Some(format!(
                "{col_name}: expected i64 >= {floor}, got {}",
                dump::render_cell(other)
            ));
        }
    };
    (actual_val < floor).then(|| format!("{col_name}: expected >= {floor}, got {actual_val}"))
}

/// Whether a `StrId` cell resolves to `expected` through the dictionary.
fn str_cell_eq(cell: Option<&Cell>, expected: &str, dict: &kronika_reader::Dictionary) -> bool {
    match cell {
        Some(Cell::StrId(id)) => match dict.resolve(*id) {
            Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => {
                bytes == expected.as_bytes()
            }
            None => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{check_at_least, check_column};
    use kronika_registry::{Cell, Row};

    fn row_of(pairs: &[(&'static str, Cell)]) -> Row {
        pairs.iter().cloned().collect()
    }

    #[test]
    fn check_at_least_accepts_value_meeting_the_floor() {
        assert!(
            check_at_least("n_tup_ins", &Cell::I64(200), "200").is_none(),
            "exactly at floor passes"
        );
        assert!(
            check_at_least("n_tup_ins", &Cell::I64(250), "200").is_none(),
            "above floor passes"
        );
    }

    #[test]
    fn check_at_least_rejects_value_below_the_floor() {
        let diff = check_at_least("n_tup_ins", &Cell::I64(199), "200");
        assert!(diff.is_some(), "below floor fails");
        assert!(
            diff.unwrap().contains("199"),
            "diff reports the actual value"
        );
    }

    #[test]
    fn check_column_parses_ge_prefix() {
        let row = row_of(&[("n_tup_ins", Cell::I64(300))]);
        assert!(
            check_column(&row, "n_tup_ins", ">= 200").is_none(),
            ">= 200 passes for 300"
        );
        assert!(
            check_column(&row, "n_tup_ins", ">= 500").is_some(),
            ">= 500 fails for 300"
        );
    }

    #[test]
    fn check_column_parses_booleans() {
        let row = row_of(&[("indisprimary", Cell::Bool(false))]);
        assert!(
            check_column(&row, "indisprimary", "false").is_none(),
            "false matches false"
        );
        assert!(
            check_column(&row, "indisprimary", "true").is_some(),
            "true fails against false"
        );
    }

    #[test]
    fn check_column_returns_none_for_absent_column() {
        let row = row_of(&[("other", Cell::I64(1))]);
        assert!(
            check_column(&row, "missing", ">= 0").is_none(),
            "absent column returns None — caller handles it separately"
        );
    }
}
