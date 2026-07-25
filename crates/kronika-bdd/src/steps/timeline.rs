//! Supported-major timeline regression over fixed-semantics `PostgreSQL` log facts.

use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use cucumber::{given, then};

use crate::BddWorld;
use crate::harness::web;

#[given("a time-local fixed-semantics PostgreSQL stderr log fixture")]
fn time_local_timeline_log_fixture(world: &mut BddWorld) -> Result<()> {
    let fixture = timeline_log_fixture()?;
    world
        .harness
        .write_log_fixture("timeline-postgresql.log", &fixture)?;
    Ok(())
}

#[then("the fixed log and metric facts reconcile through the source-scoped timeline")]
async fn source_scoped_timeline_reconciles(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    web::assert_timeline_pg_log_contract(&segment).await
}

/// Keep fixed event semantics close to the collection timestamp. This makes
/// the sealed segment a legitimate resident of the production 24
/// segment-hour publication-fallback budget on every date the BDD suite runs.
fn timeline_log_fixture() -> Result<String> {
    let now = DateTime::<Utc>::from(SystemTime::now());
    let anchor = now
        .checked_sub_signed(TimeDelta::hours(1))
        .context("timeline fixture timestamp underflow")?;
    let second = anchor
        .checked_add_signed(TimeDelta::seconds(1))
        .context("timeline fixture second timestamp overflow")?;
    let third = anchor
        .checked_add_signed(TimeDelta::seconds(2))
        .context("timeline fixture third timestamp overflow")?;
    let format = "%Y-%m-%d %H:%M:%S UTC";
    Ok(format!(
        "{anchor} [101]: PANIC:  could not write to file \"pg_wal/xlogtemp.1\": No space left on device\n\
         {second} [102]: ERROR:  40P01: deadlock detected\n\
         {third} [103]: LOG:  server process (PID 4242) was terminated by signal 9: Killed\n\
         {third} [103]: DETAIL:  Failed process was running: SELECT pg_sleep(10)\n",
        anchor = anchor.format(format),
        second = second.format(format),
        third = third.format(format),
    ))
}
