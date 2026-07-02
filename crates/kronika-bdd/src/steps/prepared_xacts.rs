//! Steps for `features/pg_prepared_xacts.feature`.
//!
//! `pg_prepared_xacts` is a per-database aggregate (type `1_010_001`). A
//! prepared transaction is not tied to any session — it persists in the
//! transaction manager until explicitly committed or rolled back. The harness
//! cleanup registers each GID so `ROLLBACK PREPARED` runs before the scenario
//! database is dropped, even when an assertion fails.
//!
//! The row is selected by `datname = [scenario database]` through the shared
//! multi-key row step in [`super::common`]; this module only prepares the
//! transaction and arranges its rollback.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given};

use crate::BddWorld;
use crate::steps::docstring;

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
