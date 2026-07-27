//! Runs `pg_kronika-collector` and the web viewer against the demo cluster.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use kronika_layout::{DataRoot, LayoutLimits};
use tokio::process::{Child, Command};
use tokio::time::timeout;

use crate::config::{Config, StandPaths};

const STOP_TIMEOUT: Duration = Duration::from_mins(1);

/// A running collector; `SIGTERM` on [`stop`](Collector::stop) exits cleanly.
#[derive(Debug)]
pub(crate) struct Collector {
    child: Child,
}

/// Starts the web viewer over the segments directory when the image carries
/// one (`KRONIKA_WEB_BIN`); the child dies with the stand via `kill_on_drop`.
pub(crate) fn spawn_web(paths: &StandPaths, config: &Config) -> Result<Option<Child>> {
    let Ok(bin) = std::env::var("KRONIKA_WEB_BIN") else {
        return Ok(None);
    };
    let addr = std::env::var("KRONIKA_WEB_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let log = std::fs::File::create(config.root.join("web.log")).context("create web.log")?;
    let child = Command::new(&bin)
        .env("KRONIKA_WEB_DIR", &paths.segments)
        .env("KRONIKA_WEB_ADDR", &addr)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone().context("share web.log for stdout")?,
        ))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn web {bin}"))?;
    println!("stand: web viewer on {addr}");
    Ok(Some(child))
}

impl Collector {
    /// Spawns the collector with the stand DSN and output directory.
    ///
    /// Segment age, caps, and interval variables pass through from the
    /// process environment, so `docker run -e KRONIKA_...` reaches the
    /// collector unchanged.
    ///
    pub(crate) fn spawn(dsn: &str, paths: &StandPaths, config: &Config) -> Result<Self> {
        Self::spawn_with(dsn, paths, config, &[])
    }

    fn spawn_with(
        dsn: &str,
        paths: &StandPaths,
        config: &Config,
        extra_env: &[(&str, &str)],
    ) -> Result<Self> {
        let bin =
            std::env::var("KRONIKA_COLLECTOR_BIN").context("KRONIKA_COLLECTOR_BIN is not set")?;
        let log = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(config.root.join("collector.log"))
            .context("open collector.log")?;
        let mut command = Command::new(&bin);
        command
            .env("KRONIKA_PG_DSN", dsn)
            .env("KRONIKA_OUT_DIR", &paths.segments)
            .env("KRONIKA_LOG_STATE_PATH", &paths.log_state);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().context("share collector.log for stdout")?,
            ))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn collector {bin}"))?;
        Ok(Self { child })
    }

    /// `SIGTERM`, then wait; the collector exits cleanly and either seals or
    /// leaves a recoverable journal.
    pub(crate) async fn stop(mut self) -> Result<()> {
        let pid = self.child.id().context("collector has already exited")?;
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid.cast_signed()),
            nix::sys::signal::Signal::SIGTERM,
        )
        .context("send SIGTERM to the collector")?;
        let status = timeout(STOP_TIMEOUT, self.child.wait())
            .await
            .context("collector did not exit after SIGTERM")?
            .context("wait for the collector")?;
        ensure!(status.success(), "collector exited with {status}");
        Ok(())
    }
}

/// Seals the tail segment the stopped collector left as a journal.
///
/// `SIGTERM` stops the collector without sealing; startup recovery is the
/// sealing path. A short-lived respawn with ticks disabled recovers the
/// journal into a new `.pgm` (and opens a fresh, empty journal of its own),
/// so even a run shorter than one segment age has something to measure. An
/// empty tail recovers into nothing; that is a warning, not a failure.
pub(crate) async fn seal_tail(dsn: &str, paths: &StandPaths, config: &Config) -> Result<()> {
    let root = DataRoot::open(&paths.segments).context("open the segments data root")?;
    if root
        .open_active_journal()
        .context("inspect the active segment journal")?
        .is_none()
    {
        return Ok(());
    }
    let before = sealed_count(&root)?;
    let recovery = Collector::spawn_with(dsn, paths, config, &[("KRONIKA_INTERVAL_S", "0")])?;
    let sealed = timeout(STOP_TIMEOUT, async {
        loop {
            match sealed_count(&root) {
                Ok(count) if count > before => break,
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    })
    .await;
    recovery.stop().await?;
    if sealed.is_err() {
        println!("stand: the tail journal recovered into no segment (likely empty)");
    }
    Ok(())
}

fn sealed_count(root: &DataRoot) -> Result<usize> {
    let snapshot = root
        .scan(LayoutLimits::default())
        .context("scan the segments data root")?;
    Ok(snapshot.segments.len())
}
