//! The shared snapshot step: run the collector against the scenario's cluster
//! and record the sealed segment on the harness state.

use std::path::PathBuf;

use anyhow::Result;

use crate::collector::Collector;
use crate::harness::HarnessState;

/// Run the collector once against the scenario's cluster and return the sealed
/// segment path. The path and the collector's stderr are stored on `state`: the
/// path for the assertion steps, the stderr for their failure dump.
///
/// The collector snapshots the whole instance, so it observes state set up by
/// any session on this cluster, including held transactions and blocked backends.
///
/// # Errors
///
/// Returns an error if no cluster is selected, or if the collector fails to spawn
/// or seal a segment. On a spawn/seal failure the collector's stderr is folded
/// into the error so CI sees the collector-side cause.
pub(crate) async fn take(state: &mut HarnessState) -> Result<PathBuf> {
    let cluster = state.cluster()?;
    let mut collector = Collector::spawn(cluster).await?;
    let segment = match collector.snapshot().await {
        Ok(segment) => segment,
        Err(err) => {
            let stderr = collector.stderr_captured();
            state.set_collector_log(stderr.clone());
            return Err(err.context(format!("collector stderr:\n{stderr}")));
        }
    };
    state.set_collector_log(collector.stderr_captured());
    if let Some(out_dir) = collector.take_output_dir() {
        state.retain_collector_output_dir(out_dir);
    }
    state.set_segment(segment.clone());
    Ok(segment)
}
