//! Drives `pg_kronika-collector` from BDD scenarios.
//!
//! `KRONIKA_COLLECTOR_BIN` points to the binary built into the Docker image.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::cluster::Cluster;

const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(30);

type ChildLines = Lines<BufReader<ChildStdout>>;

pub(crate) struct Collector {
    child: Child,
    lines: ChildLines,
    /// Everything the collector has written to stderr, drained by a background
    /// task so a full pipe never stalls the collector and the bytes survive for
    /// the failure dump.
    stderr: Arc<Mutex<Vec<u8>>>,
    /// The stderr-draining task, aborted on drop.
    stderr_task: JoinHandle<()>,
    /// Keep the output directory alive until the harness retains it for the scenario.
    out_dir: Option<tempfile::TempDir>,
}

impl Collector {
    pub(crate) async fn spawn(cluster: &Cluster) -> Result<Self> {
        let bin =
            std::env::var("KRONIKA_COLLECTOR_BIN").context("KRONIKA_COLLECTOR_BIN is not set")?;
        let out_dir = tempfile::tempdir().context("create the collector output directory")?;
        let mut child = Command::new(&bin)
            .env("KRONIKA_PG_DSN", cluster.conn_string())
            .env("KRONIKA_OUT_DIR", out_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn collector {bin}"))?;
        let stdout = child
            .stdout
            .take()
            .context("collector stdout was not piped")?;
        let stderr_pipe = child
            .stderr
            .take()
            .context("collector stderr was not piped")?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = spawn_stderr_drain(stderr_pipe, Arc::clone(&stderr));
        let mut lines = BufReader::new(stdout).lines();
        expect_line(&mut lines, "ready").await?;
        Ok(Self {
            child,
            lines,
            stderr,
            stderr_task,
            out_dir: Some(out_dir),
        })
    }

    pub(crate) async fn snapshot(&mut self) -> Result<PathBuf> {
        let raw = self.child.id().context("collector already exited")?;
        let pid = Pid::from_raw(i32::try_from(raw).context("collector pid out of range")?);
        kill(pid, Signal::SIGUSR2).context("send SIGUSR2 to the collector")?;

        let line = next_line(&mut self.lines).await?;
        let path = line
            .strip_prefix("sealed ")
            .with_context(|| format!("expected 'sealed <path>' from collector, got {line:?}"))?;
        Ok(PathBuf::from(path))
    }

    /// The collector's stderr captured so far, decoded lossily.
    ///
    /// The guide requires collector stderr in the failure report; a scenario
    /// reads this after a snapshot to feed the section dump.
    pub(crate) fn stderr_captured(&self) -> String {
        let bytes = self.stderr.lock().unwrap_or_else(|poisoned| {
            // A panicked drain task poisons the lock; the partial buffer is still
            // the best diagnostic we have, so recover it rather than panic here.
            poisoned.into_inner()
        });
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Hand the output directory to the scenario harness after a successful snapshot.
    pub(crate) const fn take_output_dir(&mut self) -> Option<tempfile::TempDir> {
        self.out_dir.take()
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.stderr_task.abort();
    }
}

/// Drain `stderr` into `sink` until the pipe closes, so the collector never
/// stalls on a full stderr buffer and the bytes are available for a dump.
fn spawn_stderr_drain(mut stderr: ChildStderr, sink: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = [0_u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut guard) = sink.lock() {
                        guard.extend_from_slice(&buf[..n]);
                    }
                }
            }
        }
    })
}

async fn expect_line(lines: &mut ChildLines, want: &str) -> Result<()> {
    let line = next_line(lines).await?;
    ensure!(
        line == want,
        "expected {want:?} from collector, got {line:?}"
    );
    Ok(())
}

async fn next_line(lines: &mut ChildLines) -> Result<String> {
    match timeout(COLLECTOR_TIMEOUT, lines.next_line()).await {
        Ok(Ok(Some(line))) => Ok(line),
        Ok(Ok(None)) => bail!("collector closed stdout before the expected line"),
        Ok(Err(err)) => Err(anyhow::Error::new(err).context("read collector stdout")),
        Err(_) => bail!("collector produced no line within {COLLECTOR_TIMEOUT:?}"),
    }
}
