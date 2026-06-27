//! Parallel boot of a `PostgreSQL` version matrix.
//!
//! Each major version runs as a throwaway cluster: a private data directory,
//! its own loopback TCP port, `fsync` off (a test never needs durability). The
//! version binaries come from Nix inside the image; the harness learns their
//! `bin` directories only through `KRONIKA_PG_MATRIX`. Clusters boot
//! concurrently, so wall-clock cost is one `initdb`, not one per version.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

/// Loopback port of the first matrix entry; entry *i* listens on `BASE_PORT + i`.
const BASE_PORT: u16 = 55_432;

/// How long a freshly started `postgres` has to begin accepting connections.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// A `PostgreSQL` major version and the `bin` directory that provides its
/// `initdb` and `postgres` (a Nix store path inside the image).
#[derive(Debug, Clone)]
pub(crate) struct PgBinary {
    pub(crate) major: u32,
    pub(crate) bindir: PathBuf,
}

/// Parse `KRONIKA_PG_MATRIX`: `;`-separated `major=bindir` entries, e.g.
/// `15=/nix/store/aaa/bin;16=/nix/store/bbb/bin`.
pub(crate) fn parse_matrix(spec: &str) -> Result<Vec<PgBinary>> {
    spec.split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (major, bindir) = entry
                .split_once('=')
                .with_context(|| format!("matrix entry has no '=': {entry:?}"))?;
            let major = major
                .trim()
                .parse::<u32>()
                .with_context(|| format!("matrix entry has a non-numeric major: {entry:?}"))?;
            Ok(PgBinary {
                major,
                bindir: PathBuf::from(bindir.trim()),
            })
        })
        .collect()
}

/// Boot every entry concurrently; entry *i* gets `BASE_PORT + i`.
pub(crate) async fn boot_matrix(matrix: &[PgBinary]) -> Result<Vec<Cluster>> {
    let mut set = tokio::task::JoinSet::new();
    for (i, bin) in matrix.iter().enumerate() {
        let bin = bin.clone();
        let offset = u16::try_from(i).context("matrix has more entries than loopback ports")?;
        set.spawn(async move { Cluster::boot(&bin, BASE_PORT + offset).await });
    }
    let mut clusters = Vec::with_capacity(matrix.len());
    while let Some(joined) = set.join_next().await {
        clusters.push(joined.context("cluster boot task panicked")??);
    }
    Ok(clusters)
}

/// A running throwaway cluster.
#[derive(Debug)]
pub(crate) struct Cluster {
    major: u32,
    port: u16,
    /// Spawned with `kill_on_drop`; held so `postgres` stops when the cluster
    /// is dropped. Declared before `data_dir` so it dies before the dir goes.
    #[allow(dead_code, reason = "owned for its Drop side effect, not read")]
    postgres: Child,
    /// Removed on drop; held so the data directory outlives `postgres`.
    #[allow(dead_code, reason = "owned for its Drop side effect, not read")]
    data_dir: TempDir,
}

impl Cluster {
    async fn boot(bin: &PgBinary, port: u16) -> Result<Self> {
        let data_dir = TempDir::new().context("create data directory")?;
        run_initdb(bin, data_dir.path()).await?;
        let postgres = spawn_postgres(bin, data_dir.path(), port)?;
        let cluster = Self {
            major: bin.major,
            port,
            postgres,
            data_dir,
        };
        cluster.wait_ready().await?;
        Ok(cluster)
    }

    /// Major version this cluster runs.
    pub(crate) const fn major(&self) -> u32 {
        self.major
    }

    fn conn_string(&self) -> String {
        format!(
            "host=127.0.0.1 port={} user=postgres dbname=postgres",
            self.port
        )
    }

    /// `server_version` reported by the running cluster.
    pub(crate) async fn server_version(&self) -> Result<String> {
        let (client, connection) =
            tokio_postgres::connect(&self.conn_string(), tokio_postgres::NoTls)
                .await
                .context("connect")?;
        let driver = tokio::spawn(connection);
        let row = client.query_one("SHOW server_version", &[]).await;
        driver.abort();
        Ok(row.context("query server_version")?.get(0))
    }

    async fn wait_ready(&self) -> Result<()> {
        let probe = async {
            while tokio_postgres::connect(&self.conn_string(), tokio_postgres::NoTls)
                .await
                .is_err()
            {
                sleep(Duration::from_millis(100)).await;
            }
        };
        timeout(READY_TIMEOUT, probe)
            .await
            .with_context(|| format!("postgres {} not ready on port {}", self.major, self.port))
    }
}

async fn run_initdb(bin: &PgBinary, data_dir: &Path) -> Result<()> {
    let status = Command::new(bin.bindir.join("initdb"))
        .arg("-D")
        .arg(data_dir)
        .args(["-U", "postgres", "-A", "trust", "--no-sync"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("spawn initdb for postgres {}", bin.major))?;
    ensure!(
        status.success(),
        "initdb for postgres {} failed: {status}",
        bin.major
    );
    Ok(())
}

fn spawn_postgres(bin: &PgBinary, data_dir: &Path, port: u16) -> Result<Child> {
    Command::new(bin.bindir.join("postgres"))
        .arg("-D")
        .arg(data_dir)
        .args(["-c", "listen_addresses=127.0.0.1"])
        .arg("-p")
        .arg(port.to_string())
        .args(["-c", "fsync=off", "-c", "full_page_writes=off"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn postgres {}", bin.major))
}

#[cfg(test)]
mod tests {
    use super::parse_matrix;
    use std::path::PathBuf;

    #[test]
    fn parses_entries_and_trims_whitespace() {
        let matrix =
            parse_matrix("15=/nix/a/bin; 16=/nix/b/bin ;17=/nix/c/bin").expect("valid spec parses");
        let majors: Vec<u32> = matrix.iter().map(|b| b.major).collect();
        assert_eq!(majors, vec![15_u32, 16, 17], "all majors, in order");
        assert_eq!(
            matrix[2].bindir,
            PathBuf::from("/nix/c/bin"),
            "bindir is trimmed and preserved"
        );
    }

    #[test]
    fn rejects_a_non_numeric_major() {
        assert!(
            parse_matrix("x=/nix/a/bin").is_err(),
            "a non-numeric major is rejected"
        );
    }

    #[test]
    fn rejects_an_entry_without_equals() {
        assert!(
            parse_matrix("15:/nix/a/bin").is_err(),
            "an entry without '=' is rejected"
        );
    }
}
