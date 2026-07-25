//! Real-process web lifecycle and sibling-index recovery steps.

use anyhow::Result;
use cucumber::then;

use crate::BddWorld;
use crate::harness::web_lifecycle;

#[then("a real web process builds the sibling and a new process reuses it without PGM body reads")]
async fn real_process_builds_and_reuses(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = web_lifecycle::establish_restart_baseline(&segment).await?;
    world.harness.set_web_lifecycle_baseline(baseline);
    Ok(())
}

#[then("a corrupt sibling is rebuilt atomically and survives another real-process restart")]
async fn corrupt_sibling_recovers(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = world.harness.web_lifecycle_baseline()?.clone();
    web_lifecycle::assert_corrupt_recovery(&segment, &baseline).await
}

#[then("every stale descriptor schema extractor registry and lineage sibling is rebuilt")]
async fn stale_identity_classes_recover(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = world.harness.web_lifecycle_baseline()?.clone();
    web_lifecycle::assert_stale_identity_recovery(&segment, &baseline).await
}

#[then("a stopped build and temporary sibling residue recover without changing source artifacts")]
async fn interrupted_publication_recovers(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = world.harness.web_lifecycle_baseline()?.clone();
    web_lifecycle::assert_interrupted_publication_recovery(&segment, &baseline).await
}

#[then(
    "a recoverable publication failure uses bounded fallback then becomes a durable restart hit"
)]
async fn recoverable_failure_recovers(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = world.harness.web_lifecycle_baseline()?.clone();
    web_lifecycle::assert_publication_failure_recovery(&segment, &baseline).await
}

#[then("a prior-process cursor expires while ordinary timeline data stays equal")]
async fn cursor_restart_is_honest(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = world.harness.web_lifecycle_baseline()?.clone();
    web_lifecycle::assert_cursor_restart_contract(&segment, &baseline).await
}

#[then("a second writer process reports deterministic contention without sidecar corruption")]
async fn second_writer_contends_safely(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = world.harness.web_lifecycle_baseline()?.clone();
    web_lifecycle::assert_writer_contention(&segment, &baseline).await
}
