//! BDD runner for collector integration scenarios.
//!
//! The binary runs inside Docker, where Nix supplies the `PostgreSQL` versions.
//! It is not a `cargo test` target: host `cargo test --workspace` must not need
//! a database. The matrix to boot is passed in `KRONIKA_PG_MATRIX`; see
//! [`cluster`].
#![allow(
    clippy::trivial_regex,
    reason = "cucumber step phrases are literal English, matched as plain text, not real regexes"
)]
#![allow(
    clippy::multiple_crate_versions,
    reason = "cucumber's dependency tree pulls duplicate transitive versions outside our control"
)]
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber passes &mut World to every step by contract, even read-only ones"
)]

mod cluster;

use anyhow::Context;
use cucumber::{World, given, then};

/// Cucumber state for one scenario: the matrix booted by the `Given` step.
#[derive(Debug, Default, World)]
struct BddWorld {
    clusters: Vec<cluster::Cluster>,
}

#[given("the PostgreSQL matrix is booted")]
async fn boot_matrix_step(world: &mut BddWorld) -> anyhow::Result<()> {
    let spec = std::env::var("KRONIKA_PG_MATRIX").context("KRONIKA_PG_MATRIX is not set")?;
    let matrix = cluster::parse_matrix(&spec)?;
    world.clusters = cluster::boot_matrix(&matrix).await?;
    Ok(())
}

#[then("every version answers a version query")]
async fn every_version_answers(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let version = db.server_version().await?;
        let major = db.major().to_string();
        anyhow::ensure!(
            version.starts_with(&major),
            "postgres {major} reported server_version {version:?}"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    BddWorld::run("features").await;
}
