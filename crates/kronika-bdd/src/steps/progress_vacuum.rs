//! Steps for `features/pg_stat_progress_vacuum.feature`.
//!
//! `pg_stat_progress_vacuum` rows exist only while a VACUUM runs. This module
//! adds the metric-specific steps that the generic row assertion and oracle
//! steps cannot express:
//!
//! - opening a background VACUUM session without waiting for a lock,
//! - polling until a `pg_stat_progress_vacuum` row for that session appears.
//!
//! The empty-state check reuses the shared `section X is absent from the
//! segment` step in [`super::common`].

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given};

use crate::BddWorld;
use crate::harness::session::Session;
use crate::steps::docstring;

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
#[given(regex = r#"^pg_stat_progress_vacuum shows session "([^"]+)"$"#)]
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
