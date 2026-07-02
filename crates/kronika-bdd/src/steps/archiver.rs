//! Archiver (`pg_stat_archiver`, type `1_008_001`) in the guide's style, and the
//! generic assertion/oracle steps it is the first feature to use.
//!
//! `pg_stat_archiver` is a singleton: one row per snapshot, no per-session key.
//! The scenario asserts that singleton's fields with concrete values and checks
//! them against an independent `pg_stat_archiver` query. The `Then section …`
//! and oracle steps here are generic transport — later features reuse the same
//! phrases; cucumber registers each phrase once, so they live with the first
//! feature that needs them.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, then};
use kronika_registry::{TypeContract, registry};

use crate::BddWorld;
use crate::harness::assert_row::{RowSelector, assert_row};
use crate::harness::expected::parse_table;
use crate::harness::oracle::{OracleKind, assert_oracle};
use crate::steps::{docstring, table};

/// Assert that a singleton section has exactly one row matching the table.
///
/// Used by any metric whose section carries a single row (archiver, wal,
/// replication instance). The expected values are written as a `| column |
/// value |` table in the `.feature`.
#[then(regex = r"^section (\d+) has exactly one row:$")]
fn section_single_row(world: &mut BddWorld, type_id: u32, step: &Step) -> Result<()> {
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

/// Verify a section column against an independent `PostgreSQL` oracle query.
///
/// The oracle kind (`exact`, `subset`, …) is named in the step; the docstring
/// carries the oracle SQL, which must be an independent check, not a copy of the
/// collector query. The query runs on the scenario database, in the session
/// pool the collector also used, so it observes the same instance state.
#[then(regex = r"^section (\d+) (\w+) matches the (\w+) oracle:$")]
async fn section_oracle(
    world: &mut BddWorld,
    type_id: u32,
    column: String,
    kind: String,
    step: &Step,
) -> Result<()> {
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

/// The registry contract for `type_id`, or an error if it is not registered.
fn contract_for(type_id: u32) -> Result<&'static TypeContract> {
    registry()
        .iter()
        .find(|contract| contract.type_id.get() == type_id)
        .with_context(|| format!("no registered section has type_id {type_id}"))
}

#[cfg(test)]
mod tests {
    use super::contract_for;

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
}
