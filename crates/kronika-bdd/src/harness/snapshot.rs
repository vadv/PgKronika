//! The shared snapshot step: run the collector against the scenario's cluster
//! and record the sealed segment on the harness state.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use tokio::time::{Instant, sleep};

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
    let extra_env = state.collector_env().to_vec();
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;
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

/// Run timer-driven log collection across a real `PostgreSQL` stderr rotation,
/// then seal both source-status transitions into one segment.
pub(crate) async fn take_across_log_rotation(state: &mut HarnessState) -> Result<PathBuf> {
    let cluster = state.cluster()?;
    let mut extra_env = state.collector_env().to_vec();
    extra_env.extend([
        ("KRONIKA_INTERVAL_S".to_owned(), "1".to_owned()),
        ("KRONIKA_PG_LOG_INTERVAL_S".to_owned(), "1".to_owned()),
        (
            "KRONIKA_LOG_DISCOVERY_INTERVAL_S".to_owned(),
            "1".to_owned(),
        ),
        (
            "KRONIKA_PG_LOG_STATUS_INTERVAL_S".to_owned(),
            "300".to_owned(),
        ),
        ("KRONIKA_SEGMENT_MAX_AGE_S".to_owned(), "900".to_owned()),
    ]);
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;

    sleep(Duration::from_millis(1_500)).await;

    let connection = cluster.connect().await?;
    let before = current_stderr_log(connection.client()).await?;

    // The test filename has second precision. Cross a wall-clock boundary so
    // PostgreSQL cannot reopen the same path and hide the rotation from us.
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    let until_next_second = Duration::from_secs(1)
        .saturating_sub(Duration::from_nanos(u64::from(elapsed.subsec_nanos())));
    sleep(until_next_second + Duration::from_millis(50)).await;

    let rotated: bool = connection
        .client()
        .query_one("SELECT pg_rotate_logfile()", &[])
        .await
        .context("request PostgreSQL stderr log rotation")?
        .get(0);
    ensure!(rotated, "PostgreSQL rejected the stderr log rotation");

    let deadline = Instant::now() + Duration::from_secs(10);
    let after = loop {
        let current = current_stderr_log(connection.client()).await?;
        if current != before {
            break current;
        }
        ensure!(
            Instant::now() < deadline,
            "PostgreSQL kept the same stderr log path after rotation: {before}"
        );
        sleep(Duration::from_millis(100)).await;
    };
    ensure!(
        after != before,
        "PostgreSQL stderr log path did not change after rotation"
    );

    sleep(Duration::from_millis(1_500)).await;
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

async fn current_stderr_log(client: &tokio_postgres::Client) -> Result<String> {
    let path: Option<String> = client
        .query_one("SELECT pg_current_logfile('stderr')", &[])
        .await
        .context("read the current PostgreSQL stderr log path")?
        .get(0);
    path.context("pg_current_logfile('stderr') returned NULL")
}
