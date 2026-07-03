//! Step definitions for `features/scheduler.feature`.
//!
//! The timer scenario starts the collector with a 1-second internal tick and
//! reads the paths it announces on its own; no signal is sent. Segment
//! composition is then checked per sealed segment: the first tick reads
//! everything, later ticks only what is due.

use anyhow::{Context, Result};
use cucumber::{then, when};
use kronika_reader::Segment;

use crate::BddWorld;
use crate::collector::Collector;
use crate::steps::common::parse_type_id;

/// Start the collector with the scenario env and wait for `count` sealed
/// segments driven purely by its internal timer.
#[when(regex = r"^the collector runs on its own timer until (\d+) segments are sealed$")]
async fn run_on_timer(world: &mut BddWorld, count: usize) -> Result<()> {
    let cluster = world.harness.cluster()?;
    let extra_env = world.harness.collector_env().to_vec();
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        match collector.wait_sealed().await {
            Ok(path) => segments.push(path),
            Err(err) => {
                let stderr = collector.stderr_captured();
                world.harness.set_collector_log(stderr.clone());
                return Err(err.context(format!("collector stderr:\n{stderr}")));
            }
        }
    }
    world.harness.set_collector_log(collector.stderr_captured());
    if let Some(out_dir) = collector.take_output_dir() {
        world.harness.retain_collector_output_dir(out_dir);
    }
    world.harness.set_timer_segments(segments);
    Ok(())
}

/// Assert a section's presence in the `index`-th (1-based) timer segment.
#[then(regex = r"^timer segment (\d+) has section ([\d_]+)$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn timer_segment_has(world: &mut BddWorld, index: usize, type_id: String) -> Result<()> {
    assert_timer_section(world, index, &type_id, true)
}

/// Assert a section's absence from the `index`-th (1-based) timer segment.
#[then(regex = r"^timer segment (\d+) is missing section ([\d_]+)$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn timer_segment_missing(world: &mut BddWorld, index: usize, type_id: String) -> Result<()> {
    assert_timer_section(world, index, &type_id, false)
}

fn assert_timer_section(
    world: &BddWorld,
    index: usize,
    type_id: &str,
    want_present: bool,
) -> Result<()> {
    let type_id = parse_type_id(type_id)?;
    let path = world.harness.timer_segment(index)?;
    let segment = Segment::open(path)
        .with_context(|| format!("open timer segment {index} at {}", path.display()))?;
    let present = segment
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.type_id == type_id);
    anyhow::ensure!(
        present == want_present,
        "timer segment {index}: section {type_id} is {}, expected it {}\ncollector stderr:\n{}",
        if present { "present" } else { "absent" },
        if want_present { "present" } else { "absent" },
        world.harness.failure_log().unwrap_or_default(),
    );
    Ok(())
}
