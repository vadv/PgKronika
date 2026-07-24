//! Supported-major timeline regression over deterministic `PostgreSQL` log facts.

use anyhow::{Context, Result};
use cucumber::{given, then};

use crate::BddWorld;
use crate::harness::web;

const FIXTURE_FROM_US: i64 = 1_783_252_800_000_000;
const FIXTURE_TO_US: i64 = 1_783_253_400_000_000;

const TIMELINE_LOG_FIXTURE: &str = "\
2026-07-05 12:00:00 UTC [101]: PANIC:  could not write to file \"pg_wal/xlogtemp.1\": No space left on device
2026-07-05 12:00:01 UTC [102]: ERROR:  40P01: deadlock detected
2026-07-05 12:00:02 UTC [103]: LOG:  server process (PID 4242) was terminated by signal 9: Killed
2026-07-05 12:00:02 UTC [103]: DETAIL:  Failed process was running: SELECT pg_sleep(10)
";

#[given("a fixed timeline PostgreSQL stderr log fixture")]
fn fixed_timeline_log_fixture(world: &mut BddWorld) -> Result<()> {
    world
        .harness
        .write_log_fixture("timeline-postgresql.log", TIMELINE_LOG_FIXTURE)?;
    Ok(())
}

#[then("the fixed log facts reconcile through the source-scoped timeline")]
async fn source_scoped_timeline_reconciles(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    web::assert_timeline_pg_log_contract(dir, FIXTURE_FROM_US, FIXTURE_TO_US).await
}
