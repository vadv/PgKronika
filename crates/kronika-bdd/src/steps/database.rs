//! Steps for `features/pg_stat_database.feature`.
//!
//! `pg_stat_database` has one row per database in the cluster; the row key is
//! `datid`. The step here selects the scenario's own database row by querying
//! its OID from `pg_database`, then delegates column assertions to the generic
//! row assertion from `harness::assert_row`.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};
use kronika_registry::pg_stat_database::PgStatDatabaseV3;
use kronika_registry::{Cell, Section};

use crate::BddWorld;
use crate::harness::assert_row::{RowSelector, assert_row};
use crate::harness::expected::parse_table;
use crate::steps::table;

const DATABASE_V3_TYPE_ID: u32 = 1_005_003;

/// Assert that section `1_005_003` has a row for the scenario database.
///
/// Queries `pg_database` for the scenario database OID, then finds that row in
/// the sealed section and checks the columns in the step table.
#[then(regex = "^section 1_005_003 has a pg_stat_database row for the scenario database:$")]
async fn database_row_for_scenario(world: &mut BddWorld, step: &Step) -> Result<()> {
    let dsn = world.harness.database_dsn()?;
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect to look up scenario database OID")?;
    let driver = tokio::spawn(async move { drop(connection.await) });
    let oid_row = client
        .query_one(
            "SELECT oid::bigint FROM pg_database WHERE datname = current_database()",
            &[],
        )
        .await
        .context("query pg_database for scenario database OID")?;
    driver.abort();
    let oid: i64 = oid_row.get(0);
    let datid = u32::try_from(oid).context("database OID overflows u32")?;

    let rows = table(step)?;
    let expected = parse_table(&PgStatDatabaseV3::CONTRACT, rows, |name| {
        world.harness.placeholder_pid(name)
    })?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        DATABASE_V3_TYPE_ID,
        &RowSelector::ByKey {
            column: "datid".to_owned(),
            cell: Cell::U32(datid),
        },
        false,
        &expected,
        &failure_log,
    )
}
