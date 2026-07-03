//! Step definitions for `features/scheduler.feature`.
//!
//! The timer scenario starts the collector with a 1-second internal tick and
//! waits for announced sealed segments without sending signals. Assertions then
//! check that the first timer tick reads every source and later ticks read only
//! due sources.

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

/// The section of a timer segment carries at least `min` distinct `ts`
/// values — the segment accumulated that many collection windows. Each
/// window seals its own catalog entry of the type, so the step aggregates
/// every entry, not just the first.
#[then(regex = r"^timer segment (\d+) section ([\d_]+) spans at least (\d+) snapshots$")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
fn timer_segment_spans(
    world: &mut BddWorld,
    index: usize,
    type_id: String,
    min: usize,
) -> Result<()> {
    use kronika_registry::Cell;

    let type_id = parse_type_id(&type_id)?;
    let path = world.harness.timer_segment(index)?;
    let rows = decode_all_sections_of(path, type_id)?;
    let mut stamps: Vec<i64> = rows
        .iter()
        .filter_map(|row| match row.get("ts") {
            Some(Cell::Ts(ts)) => Some(*ts),
            _ => None,
        })
        .collect();
    stamps.sort_unstable();
    stamps.dedup();
    anyhow::ensure!(
        stamps.len() >= min,
        "timer segment {index}: section {type_id} holds {} distinct snapshot \
         timestamps, expected at least {min}\ncollector stderr:\n{}",
        stamps.len(),
        world.harness.failure_log().unwrap_or_default(),
    );
    Ok(())
}

/// Decode and concatenate the rows of every catalog entry of `type_id`:
/// a multi-window segment holds one entry per collection window.
fn decode_all_sections_of(
    path: &std::path::Path,
    type_id: u32,
) -> Result<Vec<kronika_registry::Row>> {
    use kronika_format::crc32c;
    use kronika_registry::{Bytes, VerifiedSection, decode_rows};
    use std::os::unix::fs::FileExt;

    let segment = Segment::open(path).context("open sealed segment")?;
    let entries: Vec<_> = segment
        .catalog()
        .entries
        .iter()
        .filter(|entry| entry.type_id == type_id)
        .copied()
        .collect();
    anyhow::ensure!(!entries.is_empty(), "segment has no section {type_id}");
    let mut rows = Vec::new();
    for entry in entries {
        let len = usize::try_from(entry.len).context("section len overflows usize")?;
        let mut body = vec![0_u8; len];
        std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;
        let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
            .map_err(|err| anyhow::anyhow!("section {type_id} crc check failed: {err}"))?;
        rows.extend(
            decode_rows(type_id, verified)
                .with_context(|| format!("generic decode of section {type_id}"))?,
        );
    }
    Ok(rows)
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
