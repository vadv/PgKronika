//! The shared snapshot step: run the collector against the scenario's cluster
//! and record the sealed segment on the harness state.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use tokio::time::{Instant, sleep};
use tokio_postgres::NoTls;

use crate::collector::Collector;
use crate::harness::HarnessState;

const LIVE_STATEMENT_TIMEOUT_SQL: &str =
    "SELECT pg_sleep(0.2) /* pgkronika_bdd_statement_timeout */";
const LIVE_STATEMENT_TIMEOUT_MARKER: &str = "pgkronika_bdd_statement_timeout";
const LOG_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_WRITE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const LOG_TAIL_LIMIT_BYTES: u64 = 256 * 1024;

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

/// Establish the default tail-at-EOF state, make the running `PostgreSQL`
/// backend emit a statement-timeout error, and seal the newly appended record.
pub(crate) async fn take_after_live_statement_timeout(state: &mut HarnessState) -> Result<PathBuf> {
    let cluster = state.cluster()?;
    let extra_env = state.collector_env().to_vec();
    let database_dsn = state.database_dsn()?;
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;

    let outcome = capture_live_statement_timeout(&mut collector, &database_dsn).await;
    let stderr = collector.stderr_captured();
    state.set_collector_log(stderr.clone());
    if let Some(out_dir) = collector.take_output_dir() {
        state.retain_collector_output_dir(out_dir);
    }

    let segment = outcome.map_err(|err| err.context(format!("collector stderr:\n{stderr}")))?;
    state.set_segment(segment.clone());
    Ok(segment)
}

async fn capture_live_statement_timeout(
    collector: &mut Collector,
    database_dsn: &str,
) -> Result<PathBuf> {
    // A newly discovered source starts at EOF. The first committed cycle
    // establishes that offset; the second one must observe only the live event.
    collector
        .snapshot()
        .await
        .context("establish the PostgreSQL stderr tail offset")?;

    let (client, connection) = tokio_postgres::connect(database_dsn, NoTls)
        .await
        .context("connect for the live statement-timeout probe")?;
    let driver = tokio::spawn(connection);
    let outcome = async {
        let log_path = current_stderr_log_path(&client).await?;
        client
            .batch_execute(
                "SET log_error_verbosity = verbose;
                 SET log_min_error_statement = error;
                 SET statement_timeout = '50ms';",
            )
            .await
            .context("configure the live statement-timeout probe")?;

        let query_error = client
            .simple_query(LIVE_STATEMENT_TIMEOUT_SQL)
            .await
            .err()
            .context("pg_sleep unexpectedly completed before statement_timeout")?;
        ensure!(
            query_error.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED),
            "statement-timeout probe returned an unexpected PostgreSQL error: {query_error}"
        );

        wait_for_log_marker(&log_path, LIVE_STATEMENT_TIMEOUT_MARKER).await?;
        collector
            .snapshot()
            .await
            .context("seal the live PostgreSQL statement-timeout record")
    }
    .await;
    drop(client);
    driver.abort();
    outcome
}

async fn current_stderr_log(client: &tokio_postgres::Client) -> Result<String> {
    let path: Option<String> = client
        .query_one("SELECT pg_current_logfile('stderr')", &[])
        .await
        .context("read the current PostgreSQL stderr log path")?
        .get(0);
    path.context("pg_current_logfile('stderr') returned NULL")
}

async fn current_stderr_log_path(client: &tokio_postgres::Client) -> Result<PathBuf> {
    let row = client
        .query_one(
            "SELECT current_setting('data_directory'), pg_current_logfile('stderr')",
            &[],
        )
        .await
        .context("resolve the current PostgreSQL stderr log path")?;
    let data_directory: String = row.get(0);
    let current_log: Option<String> = row.get(1);
    let current_log =
        PathBuf::from(current_log.context("pg_current_logfile('stderr') returned NULL")?);
    if current_log.is_absolute() {
        Ok(current_log)
    } else {
        Ok(PathBuf::from(data_directory).join(current_log))
    }
}

async fn wait_for_log_marker(path: &Path, marker: &str) -> Result<()> {
    let deadline = Instant::now() + LOG_WRITE_TIMEOUT;
    loop {
        match log_tail_contains(path, marker.as_bytes()) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("read PostgreSQL log tail {}", path.display()));
            }
        }
        ensure!(
            Instant::now() < deadline,
            "PostgreSQL did not write marker {marker:?} to {} within {LOG_WRITE_TIMEOUT:?}",
            path.display()
        );
        sleep(LOG_WRITE_POLL_INTERVAL).await;
    }
}

fn log_tail_contains(path: &Path, marker: &[u8]) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_TAIL_LIMIT_BYTES);
    file.seek(SeekFrom::Start(start))?;

    let capacity = usize::try_from(len.saturating_sub(start)).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PostgreSQL log tail length does not fit usize: {err}"),
        )
    })?;
    let mut tail = Vec::with_capacity(capacity);
    file.take(LOG_TAIL_LIMIT_BYTES).read_to_end(&mut tail)?;
    Ok(tail
        .windows(marker.len())
        .any(|candidate| candidate == marker))
}
