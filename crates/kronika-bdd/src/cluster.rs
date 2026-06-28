//! Boots throwaway `PostgreSQL` clusters for BDD scenarios.
//!
//! Nix passes each version's `bin` directory through `KRONIKA_PG_MATRIX`.
//! Clusters run in private data directories with `fsync` disabled.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

const READY_TIMEOUT: Duration = Duration::from_secs(30);

const SERVER_LOG: &str = "server.log";

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

/// Boot every entry concurrently on distinct loopback ports.
pub(crate) async fn boot_matrix(matrix: &[PgBinary]) -> Result<Vec<Cluster>> {
    let ports = pick_distinct_ports(matrix.len())?;
    let mut set = tokio::task::JoinSet::new();
    for (bin, port) in matrix.iter().zip(ports) {
        let bin = bin.clone();
        set.spawn(async move { Cluster::boot(&bin, port).await });
    }
    let mut clusters = Vec::with_capacity(matrix.len());
    while let Some(joined) = set.join_next().await {
        clusters.push(joined.context("cluster boot task panicked")??);
    }
    Ok(clusters)
}

/// Pick distinct ports before parallel startup to avoid self-collisions.
fn pick_distinct_ports(n: usize) -> Result<Vec<u16>> {
    let mut ports = Vec::with_capacity(n);
    for _ in 0..(n * 20) {
        if ports.len() == n {
            break;
        }
        let port = portpicker::pick_unused_port().context("no free TCP port available")?;
        if !ports.contains(&port) {
            ports.push(port);
        }
    }
    ensure!(ports.len() == n, "could not find {n} distinct free ports");
    Ok(ports)
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
    /// Data directory and unix-socket directory; removed on drop.
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

    pub(crate) fn conn_string(&self) -> String {
        format!(
            "host=127.0.0.1 port={} user=postgres dbname=postgres",
            self.port
        )
    }

    pub(crate) async fn connect(&self) -> Result<Conn> {
        let (client, connection) =
            tokio_postgres::connect(&self.conn_string(), tokio_postgres::NoTls)
                .await
                .context("connect")?;
        Ok(Conn {
            client,
            driver: tokio::spawn(connection),
        })
    }

    /// `server_version` reported by the running cluster.
    pub(crate) async fn server_version(&self) -> Result<String> {
        let conn = self.connect().await?;
        let row = conn
            .client()
            .query_one("SHOW server_version", &[])
            .await
            .context("query server_version")?;
        Ok(row.get(0))
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
        if timeout(READY_TIMEOUT, probe).await.is_err() {
            anyhow::bail!(
                "postgres {} not ready on port {} within {READY_TIMEOUT:?}; server log:\n{}",
                self.major,
                self.port,
                self.server_log(),
            );
        }
        Ok(())
    }

    fn server_log(&self) -> String {
        std::fs::read_to_string(self.data_dir.path().join(SERVER_LOG))
            .unwrap_or_else(|_| "(server log unavailable)".to_owned())
    }
}

pub(crate) struct Conn {
    client: tokio_postgres::Client,
    /// Abort on drop; otherwise the protocol driver keeps running.
    driver: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl Conn {
    pub(crate) const fn client(&self) -> &tokio_postgres::Client {
        &self.client
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

async fn run_initdb(bin: &PgBinary, data_dir: &Path) -> Result<()> {
    let status = Command::new(bin.bindir.join("initdb"))
        .arg("-D")
        .arg(data_dir)
        .args([
            "-U",
            "postgres",
            "-A",
            "trust",
            "--no-sync",
            "--locale-provider=libc",
            "--locale=C",
        ])
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
    let log = std::fs::File::create(data_dir.join(SERVER_LOG))
        .with_context(|| format!("create server log for postgres {}", bin.major))?;
    Command::new(bin.bindir.join("postgres"))
        .arg("-D")
        .arg(data_dir)
        // The unix socket goes in the writable data dir; the packaged default
        // (/run/postgresql) does not exist in the minimal image, and postgres
        // creates a socket even when it only listens on TCP.
        .arg("-k")
        .arg(data_dir)
        .args(["-c", "listen_addresses=127.0.0.1"])
        .arg("-p")
        .arg(port.to_string())
        .args(["-c", "fsync=off", "-c", "full_page_writes=off"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
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
