//! Steps for `features/pg_log.feature`.

use anyhow::Result;
use cucumber::{gherkin::Step, given};

use crate::BddWorld;
use crate::steps::docstring;

/// Route the collector to a deterministic `PostgreSQL` stderr log fixture.
#[given("a PostgreSQL stderr log fixture:")]
fn stderr_log_fixture(world: &mut BddWorld, step: &Step) -> Result<()> {
    let content = docstring(step)?;
    world.harness.write_log_fixture("postgresql.log", content)?;
    Ok(())
}
