//! Steps for `features/pg_log.feature`.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given, then, when};

use crate::BddWorld;
use crate::harness::{snapshot, web};
use crate::steps::docstring;

/// Route the collector to a deterministic `PostgreSQL` stderr log fixture.
#[given("a PostgreSQL stderr log fixture:")]
fn stderr_log_fixture(world: &mut BddWorld, step: &Step) -> Result<()> {
    let content = docstring(step)?;
    world.harness.write_log_fixture("postgresql.log", content)?;
    Ok(())
}

/// Keep one collector alive while `PostgreSQL` changes its current stderr file.
#[when("the running collector observes a PostgreSQL stderr log rotation")]
async fn collector_observes_log_rotation(world: &mut BddWorld) -> Result<()> {
    snapshot::take_across_log_rotation(&mut world.harness).await?;
    Ok(())
}

/// Generate a real backend timeout after the collector has established EOF.
#[when("the running collector captures a real PostgreSQL statement timeout")]
async fn collector_captures_statement_timeout(world: &mut BddWorld) -> Result<()> {
    snapshot::take_after_live_statement_timeout(&mut world.harness).await?;
    Ok(())
}

/// Assert both status transitions retained their resolved physical paths.
#[then("pg_log_source_status contains two distinct source_path values")]
async fn two_log_source_paths(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    web::assert_two_log_source_paths(dir).await
}
