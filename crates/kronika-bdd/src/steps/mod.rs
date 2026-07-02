//! Step definitions in the guide's style, split per feature.
//!
//! This module holds the generic transport steps used by converted features:
//! selecting a cluster and isolated database, opening named sessions, and
//! taking the collector snapshot. Metric-specific assertions live in a
//! submodule per metric (e.g. [`archiver`]).
//!
//! Shared assertion and oracle phrases live in [`common`] so cucumber registers
//! each phrase once.

pub(crate) mod archiver;
pub(crate) mod common;

use anyhow::{Context, Result};
use cucumber::{gherkin::Step, given, when};

use crate::BddWorld;
use crate::harness::session::Session;
use crate::harness::snapshot;

/// Select the matrix cluster of the given major and open an isolated database.
///
/// Every session opened afterwards connects to this database, so per-scenario
/// table state never leaks between scenarios sharing the boot-once matrix.
#[given(regex = r"^a fresh database on PostgreSQL (\d+)$")]
async fn fresh_database(world: &mut BddWorld, major: u32) -> Result<()> {
    world.harness.use_database(major, "db").await?;
    Ok(())
}

/// Seed the scenario database with the step's docstring SQL, on a throwaway
/// session. The setup SQL is visible in the `.feature`, per the guide.
#[given("a database seeded with:")]
async fn seed_database(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.database_dsn()?;
    let session = Session::open(&dsn, sql).await?;
    // The seed session has served its purpose; close it so it holds nothing.
    session.close().await?;
    Ok(())
}

/// Create a second isolated database on the scenario's cluster and seed it with
/// the step's docstring SQL, on a throwaway session.
///
/// Per-database fan-out scenarios use this to give the pool more than one
/// target. The database is `extra_databases[0]`, dropped in cleanup.
#[given("a second database seeded with:")]
async fn seed_second_database(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.add_database("db2").await?;
    let session = Session::open(&dsn, sql).await?;
    session.close().await?;
    Ok(())
}

/// Open a named session and run its docstring SQL to completion.
#[given(regex = r#"^session "([^"]+)" runs:$"#)]
async fn session_runs(world: &mut BddWorld, name: String, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.database_dsn()?;
    let session = Session::open(&dsn, sql).await?;
    world.harness.insert_session(name, session);
    Ok(())
}

/// Open a named session, run its docstring SQL, and hold the transaction open
/// until cleanup — so its locks persist across the snapshot.
#[given(regex = r#"^session "([^"]+)" runs and holds its transaction open:$"#)]
async fn session_holds(world: &mut BddWorld, name: String, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.database_dsn()?;
    let session = Session::open_holding(&dsn, sql).await?;
    world.harness.insert_session(name, session);
    Ok(())
}

/// Open a named session and run its docstring SQL on a background task, waiting
/// until the backend is observed blocked on a lock.
#[given(regex = r#"^session "([^"]+)" runs and blocks:$"#)]
async fn session_blocks(world: &mut BddWorld, name: String, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.database_dsn()?;
    let session = Session::open_blocking(&dsn, sql).await?;
    world.harness.insert_session(name, session);
    Ok(())
}

/// Run the collector against the scenario cluster and record the sealed segment.
#[when("the collector snapshots the segment")]
async fn snapshot_segment(world: &mut BddWorld) -> Result<()> {
    snapshot::take(&mut world.harness).await?;
    Ok(())
}

/// The docstring of a step, or an error naming the missing input.
pub(crate) fn docstring(step: &Step) -> Result<&str> {
    step.docstring
        .as_deref()
        .map(str::trim)
        .context("step needs a \"\"\" docstring with its SQL")
}

/// The data table of a step as raw rows, or an error naming the missing input.
pub(crate) fn table(step: &Step) -> Result<&[Vec<String>]> {
    step.table
        .as_ref()
        .map(|table| table.rows.as_slice())
        .context("step needs a `| column | value |` table")
}
mod activity;
mod collector;
mod connection_pool;
mod database;
mod io;
mod prepared_xacts;
mod progress_vacuum;
mod replication_instance;
mod smoke;
mod statements;
mod user_tables;
mod wal;
