//! BDD runner for Docker-only integration scenarios.
//!
//! Nix supplies the `PostgreSQL` 15–18 matrix through `KRONIKA_PG_MATRIX`. Host
//! `cargo test --workspace` stays database-free.
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
mod collector;
mod harness;
mod steps;

use cucumber::{World, event, given};
use harness::HarnessState;

/// Each BDD scenario boots the full `PostgreSQL` matrix; one scenario at a time
/// keeps CI from starting dozens of clusters at once.
const MAX_CONCURRENT_SCENARIOS: usize = 1;

/// Cucumber state for one scenario.
///
/// `clusters` borrows the process-wide matrix (booted once, not per scenario).
/// `harness` holds per-scenario state: named sessions, the selected database,
/// and the last snapshot. The `after` hook tears it down.
#[derive(Debug, Default, World)]
struct BddWorld {
    clusters: &'static [cluster::Cluster],
    harness: HarnessState,
}

#[given("the PostgreSQL matrix is booted")]
async fn boot_matrix_step(world: &mut BddWorld) -> anyhow::Result<()> {
    world.clusters = cluster::shared_matrix().await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let feature_path = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    BddWorld::cucumber()
        .max_concurrent_scenarios(MAX_CONCURRENT_SCENARIOS)
        .fail_on_skipped()
        .after(|_feature, _rule, _scenario, scenario_event, world| {
            Box::pin(async move {
                let failed = matches!(
                    scenario_event,
                    event::ScenarioFinished::StepFailed(..)
                        | event::ScenarioFinished::BeforeHookFailed(_)
                );
                if let event::ScenarioFinished::StepFailed(_, _, err) = scenario_event {
                    eprintln!("=== BDD step failed: {err} ===");
                }
                if let Some(world) = world {
                    if failed {
                        for cluster in world.clusters {
                            eprintln!(
                                "=== postgres {} server.log ===\n{}\n=== end postgres {} server.log ===",
                                cluster.major(),
                                cluster.server_log(),
                                cluster.major(),
                            );
                        }
                    }
                    world.harness.cleanup().await;
                }
            })
        })
        .run_and_exit(feature_path)
        .await;
}
