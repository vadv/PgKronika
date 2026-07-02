//! Steps for `features/pg_prepared_xacts.feature`.
//!
//! `pg_prepared_xacts` is a per-database aggregate (type `1_010_001`). A
//! prepared transaction is not tied to any session — it persists in the
//! transaction manager until explicitly committed or rolled back. The harness
//! cleanup registers each GID so `ROLLBACK PREPARED` runs before the scenario
//! database is dropped, even when an assertion fails.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given, then};

use super::common::{contract_for, parse_table_with_empty_list};
use crate::BddWorld;
use crate::harness::assert_row::{RowSelector, assert_row};
use crate::steps::{docstring, table};

const TYPE_ID: u32 = 1_010_001;

const GID: &str = "kronika_bdd_prepared_xacts_probe";

/// Prepare a transaction in the scenario database using the docstring SQL.
///
/// The `PREPARE TRANSACTION` statement must name the GID `kronika_bdd_prepared_xacts_probe`.
/// The GID is registered for cleanup so `ROLLBACK PREPARED` runs even when an
/// assertion step fails — a prepared transaction holds locks and prevents
/// `DROP DATABASE` from completing.
#[given(regex = "^the pg_prepared_xacts transaction is prepared:$")]
async fn prepare_transaction(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.database_dsn()?;
    let (client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect to prepare the transaction")?;
    let driver = tokio::spawn(async move {
        drop(conn.await);
    });
    let result = client
        .batch_execute(sql)
        .await
        .context("PREPARE TRANSACTION");
    driver.abort();
    result?;
    // Register cleanup before returning so a panic or assertion failure still
    // triggers ROLLBACK PREPARED in the after hook.
    world.harness.add_rollback_prepared(GID.to_owned())?;
    Ok(())
}

/// Assert that section `1_010_001` has a row whose `datname` matches the
/// scenario's isolated database.
///
/// The key resolves through the segment dictionary: the section stores
/// `datname` as a `StrId`, not a plain string. The data table checks
/// additional columns on that row.
#[then(regex = "^section 1_010_001 has a row with datname = the scenario database:$")]
fn section_row_by_datname(world: &mut BddWorld, step: &Step) -> Result<()> {
    let datname = world.harness.database()?.to_owned();
    let contract = contract_for(TYPE_ID)?;
    let rows = table(step)?;
    let expected =
        parse_table_with_empty_list(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        TYPE_ID,
        &RowSelector::ByStr {
            column: "datname".to_owned(),
            value: datname,
        },
        false,
        &expected,
        &failure_log,
    )
}
