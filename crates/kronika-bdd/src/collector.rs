//! Drives `pg_kronika-collector` from BDD scenarios.
//!
//! `KRONIKA_COLLECTOR_BIN` points to the binary built into the Docker image.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail, ensure};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{Duration, timeout};

use crate::cluster::Cluster;

const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(30);

type ChildLines = Lines<BufReader<ChildStdout>>;

pub(crate) struct Collector {
    child: Child,
    lines: ChildLines,
    /// Keep the output directory alive until the scenario opens the segment.
    _out_dir: tempfile::TempDir,
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
