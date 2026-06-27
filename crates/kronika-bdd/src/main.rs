//! BDD runner for collector integration scenarios.
//!
//! The binary runs inside Docker, where Nix supplies the `PostgreSQL` versions.
//! It is not a `cargo test` target: host `cargo test --workspace` must not need
//! a database.
#![allow(
    clippy::trivial_regex,
    reason = "cucumber step phrases are literal English, matched as plain text, not real regexes"
)]
#![allow(
    clippy::missing_const_for_fn,
    reason = "cucumber registers step fns by macro; an empty placeholder looks const, but real steps do async I/O"
)]
#![allow(
    clippy::multiple_crate_versions,
    reason = "cucumber's dependency tree pulls duplicate transitive versions outside our control"
)]

use cucumber::{World, given};

/// Cucumber state for one scenario.
#[derive(Debug, Default, World)]
struct BddWorld;

#[given("the harness is running")]
fn harness_running(_world: &mut BddWorld) {}

#[tokio::main]
async fn main() {
    BddWorld::run("features").await;
}
