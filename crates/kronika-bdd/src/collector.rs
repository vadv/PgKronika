//! Drives the collector binary for end-to-end scenarios.
//!
//! Spawns `pg_kronika-collector` (path from `KRONIKA_COLLECTOR_BIN`) against one
//! cluster, waits for its readiness line, triggers a snapshot with `SIGUSR2`,
//! and returns the sealed segment path it prints. The child is killed and its
//! output directory removed when the [`Collector`] guard drops.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail, ensure};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{Duration, timeout};

use crate::cluster::Cluster;

/// How long the collector has to print each expected line. Generous: it covers
/// connecting, one snapshot, and a segment seal with an fsync.
const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(30);

/// Buffered stdout of the collector child, read one status line at a time.
type ChildLines = Lines<BufReader<ChildStdout>>;

/// A collector child driven against one cluster. Killed on drop (`kill_on_drop`).
pub(crate) struct Collector {
    child: Child,
    lines: ChildLines,
    /// Receives sealed segments; removed on drop, after the scenario has read
    /// the segment back.
    _out_dir: tempfile::TempDir,
}

impl Collector {
    /// Spawn the collector against `cluster` and wait until it reports ready.
    pub(crate) async fn spawn(cluster: &Cluster) -> Result<Self> {
        let bin =
            std::env::var("KRONIKA_COLLECTOR_BIN").context("KRONIKA_COLLECTOR_BIN is not set")?;
        let out_dir = tempfile::tempdir().context("create the collector output directory")?;
        let mut child = Command::new(&bin)
            .env("KRONIKA_PG_DSN", cluster.conn_string())
            .env("KRONIKA_OUT_DIR", out_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn collector {bin}"))?;
        let stdout = child
            .stdout
            .take()
            .context("collector stdout was not piped")?;
        let mut lines = BufReader::new(stdout).lines();
        expect_line(&mut lines, "ready").await?;
        Ok(Self {
            child,
            lines,
            _out_dir: out_dir,
        })
    }

    /// Trigger one snapshot with `SIGUSR2` and return the sealed segment path.
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
}

/// Read the next status line, failing if it is not exactly `want`.
async fn expect_line(lines: &mut ChildLines, want: &str) -> Result<()> {
    let line = next_line(lines).await?;
    ensure!(
        line == want,
        "expected {want:?} from collector, got {line:?}"
    );
    Ok(())
}

/// Read one line of collector stdout, bounding the wait so a stuck child fails
/// the scenario instead of hanging it.
async fn next_line(lines: &mut ChildLines) -> Result<String> {
    match timeout(COLLECTOR_TIMEOUT, lines.next_line()).await {
        Ok(Ok(Some(line))) => Ok(line),
        Ok(Ok(None)) => bail!("collector closed stdout before the expected line"),
        Ok(Err(err)) => Err(anyhow::Error::new(err).context("read collector stdout")),
        Err(_) => bail!("collector produced no line within {COLLECTOR_TIMEOUT:?}"),
    }
}
