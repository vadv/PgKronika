//! Steps for `features/pg_stat_progress_vacuum.feature`.
//!
//! `pg_stat_progress_vacuum` rows exist only while a VACUUM runs. The shared
//! steps in `common` handle generic row assertions and oracle checks; this
//! module adds the three steps specific to this metric:
//!
//! - asserting the section is absent (empty-state scenario),
//! - opening a background VACUUM session without waiting for a lock,
//! - polling until a `pg_stat_progress_vacuum` row for that session appears.

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given, then};
use kronika_reader::Segment;

use crate::BddWorld;
use crate::harness::session::Session;
use crate::steps::docstring;

/// Assert that section `type_id` is absent from the sealed segment.
///
/// Passes when the section was not written (no rows were collected). Fails with
/// a message if the section is present.
#[then(regex = r"^section (\d+) is absent$")]
fn section_is_absent(world: &mut BddWorld, type_id: u32) -> Result<()> {
    let segment_path = world.harness.segment()?.clone();
    let segment = Segment::open(&segment_path).context("open sealed segment")?;
    let present = segment
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.type_id == type_id);
    anyhow::ensure!(
        !present,
        "section {type_id} is present in the segment but was expected to be absent; \
         a VACUUM may have started unexpectedly"
    );
    Ok(())
}

/// Open a named session and run its docstring SQL on a background task, returning
/// immediately without waiting for any particular wait state.
///
/// Used for VACUUM statements that do not block on a lock. The session is held
/// until cleanup so the background task can be aborted if the scenario fails.
#[given(regex = r#"^session "([^"]+)" runs VACUUM in the background:$"#)]
async fn session_runs_vacuum_background(
    world: &mut BddWorld,
    name: String,
    step: &Step,
) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.database_dsn()?;
    let session = Session::open_background(&dsn, sql).await?;
    world.harness.insert_session(name, session);
    Ok(())
}

/// Poll `pg_stat_progress_vacuum` until a row for session `name` appears.
///
/// Waits up to `VACUUM_WAIT_TIMEOUT` before failing. The next step (snapshot)
/// runs only after this poll succeeds, so the collector observes the in-flight
/// vacuum row.
#[given(regex = r#"^the harness waits for pg_stat_progress_vacuum to show session "([^"]+)"$"#)]
async fn wait_for_vacuum_progress(world: &mut BddWorld, name: String) -> Result<()> {
    world
        .harness
        .wait_for_vacuum_progress(&name)
        .await
        .with_context(|| {
            format!("pg_stat_progress_vacuum row for session {name:?} did not appear in time")
        })?;
    Ok(())
}
