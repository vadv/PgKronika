//! Steps for `features/pg_stat_activity.feature` (section `1_001_003`).
//!
//! Generic transport and row-assertion steps come from [`super::mod`] and
//! [`super::common`]. The one step here is specific to `pg_stat_activity` so
//! its phrase mentions the view name and cannot clash with other features.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};

use crate::BddWorld;
use crate::harness::oracle::{OracleKind, assert_oracle};
use crate::steps::{common::contract_for, docstring};

const ACTIVITY_TYPE_ID: u32 = 1_001_003;

/// Verify that the session's recorded `pid` appears in a live `pg_stat_activity`
/// query, using a subset oracle.
///
/// The docstring is the oracle SQL. The phrase names `pg_stat_activity` so it
/// cannot collide with the generic oracle step in `common`.
#[then(regex = "^section 1_001_003 pid is present in pg_stat_activity:$")]
async fn activity_pid_subset_oracle(world: &mut BddWorld, step: &Step) -> Result<()> {
    let contract = contract_for(ACTIVITY_TYPE_ID)?;
    let sql = docstring(step)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let dsn = world.harness.database_dsn()?;
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the pg_stat_activity oracle")?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let result = assert_oracle(
        &client,
        contract,
        &segment,
        "pid",
        OracleKind::Subset,
        sql,
        &failure_log,
    )
    .await;
    driver.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::ACTIVITY_TYPE_ID;
    use crate::steps::common::contract_for;

    #[test]
    fn activity_type_id_resolves_in_the_registry() {
        let contract = contract_for(ACTIVITY_TYPE_ID).expect("1_001_003 is registered");
        assert_eq!(contract.type_id.get(), ACTIVITY_TYPE_ID);
        assert!(
            contract.column("pid").is_some(),
            "pid column present in the activity contract"
        );
        assert!(
            contract.column("state").is_some(),
            "state column present in the activity contract"
        );
        assert!(
            contract.column("query").is_some(),
            "query column present in the activity contract"
        );
    }
}
