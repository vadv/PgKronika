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

const INITDB_LOG: &str = "initdb.log";

/// Majors whose image clusters carry the vadv `pg_store_plans` fork.
///
/// Both forks install identically named files, so one cluster carries one
/// fork; the ossc upstream lives on [`OSSC_STORE_PLANS_MAJORS`].
pub(crate) const STORE_PLANS_MAJORS: [u32; 2] = [17, 18];

/// Majors whose image clusters carry the ossc upstream `pg_store_plans`.
pub(crate) const OSSC_STORE_PLANS_MAJORS: [u32; 2] = [15, 16];

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

/// The matrix booted once for the whole process.
///
/// Re-running `initdb` per scenario creates unnecessary concurrent boot work.
/// The matrix boots on first use and every scenario borrows the same clusters;
/// per-scenario state is isolated with uniquely named databases/schemas, not
/// fresh clusters. Teardown is the clusters' `kill_on_drop`, which fires when
/// the process exits.
static SHARED_MATRIX: tokio::sync::OnceCell<Vec<Cluster>> = tokio::sync::OnceCell::const_new();

/// Borrow the process-wide matrix, booting it from `KRONIKA_PG_MATRIX` on the
/// first call.
///
/// # Errors
///
/// Returns an error if `KRONIKA_PG_MATRIX` is unset or malformed, or if any
/// cluster fails to boot.
pub(crate) async fn shared_matrix() -> Result<&'static [Cluster]> {
    let clusters = SHARED_MATRIX
        .get_or_try_init(|| async {
            let spec =
                std::env::var("KRONIKA_PG_MATRIX").context("KRONIKA_PG_MATRIX is not set")?;
            let matrix = parse_matrix(&spec)?;
            boot_matrix(&matrix).await
        })
        .await?;
    Ok(clusters)
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
    /// The `bin` directory this cluster was booted from; steps use it to
    /// spawn client tools of the matching major (e.g. `pg_receivewal`).
    bindir: PathBuf,
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
        let bindir = bin.bindir.clone();
        let cluster = Self {
            major: bin.major,
            port,
            bindir,
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

    /// The `bin` directory of this cluster's `PostgreSQL` installation.
    pub(crate) fn bindir(&self) -> &Path {
        &self.bindir
    }

    pub(crate) fn conn_string(&self) -> String {
        self.conn_string_db("postgres")
    }

    /// Connection string targeting a specific database on this cluster.
    pub(crate) fn conn_string_db(&self, dbname: &str) -> String {
        format!(
            "host=127.0.0.1 port={} user=postgres dbname={dbname}",
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

    pub(crate) fn server_log(&self) -> String {
        std::fs::read_to_string(self.data_dir.path().join(SERVER_LOG))
            .unwrap_or_else(|_| "(server log unavailable)".to_owned())
    }
}

#[derive(Debug)]
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
    // Capture stdout+stderr so a non-zero exit carries the reason. The
    // after-hook cannot help here because the cluster's data dir does not
    // exist yet.
    let output = Command::new(bin.bindir.join("initdb"))
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("spawn initdb for postgres {}", bin.major))?;
    let diagnostic = initdb_diagnostic(&output.stdout, &output.stderr);
    // Persist the output next to the data dir. The failure message below still
    // carries it inline, so a write error here is only noted, never fatal.
    if let Err(err) = std::fs::write(data_dir.join(INITDB_LOG), &diagnostic) {
        eprintln!(
            "=== BDD: could not write {INITDB_LOG} for postgres {}: {err} ===",
            bin.major
        );
    }
    ensure!(
        output.status.success(),
        "initdb for postgres {} failed: {}\n{diagnostic}",
        bin.major,
        output.status,
    );
    // In postgresql.conf, not a `-c` flag: a command-line GUC outranks
    // postgresql.auto.conf, so `ALTER SYSTEM` scenarios could never flip it.
    let conf = data_dir.join("postgresql.conf");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&conf)
        .with_context(|| format!("open {} to append GUCs", conf.display()))?;
    std::io::Write::write_all(&mut file, b"track_io_timing = on\n")
        .context("append track_io_timing to postgresql.conf")?;
    Ok(())
}

/// Render captured `initdb` output for a failure message and the `initdb.log`.
fn initdb_diagnostic(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    format!(
        "--- initdb stdout ---\n{}\n--- initdb stderr ---\n{}",
        stdout.trim_end(),
        stderr.trim_end(),
    )
}

fn spawn_postgres(bin: &PgBinary, data_dir: &Path, port: u16) -> Result<Child> {
    let log = std::fs::File::create(data_dir.join(SERVER_LOG))
        .with_context(|| format!("create server log for postgres {}", bin.major))?;
    let mut cmd = Command::new(bin.bindir.join("postgres"));
    cmd.arg("-D")
        .arg(data_dir)
        // The unix socket goes in the writable data dir; the packaged default
        // (/run/postgresql) does not exist in the minimal image, and postgres
        // creates a socket even when it only listens on TCP.
        .arg("-k")
        .arg(data_dir)
        .args(["-c", "listen_addresses=127.0.0.1"])
        .arg("-p")
        .arg(port.to_string())
        .args([
            "-c",
            "fsync=off",
            "-c",
            "full_page_writes=off",
            "-c",
            "max_prepared_transactions=16",
            // Logical slots need it; the other replication scenarios do not
            // assert WAL records produced by this setting.
            "-c",
            "wal_level=logical",
        ]);
    if STORE_PLANS_MAJORS.contains(&bin.major) {
        // These majors include the vadv pg_store_plans fork. The GUCs record
        // every plan (no threshold, no sampling); compute_query_id keeps the
        // pg_stat_statements bridge column populated.
        cmd.args([
            "-c",
            "shared_preload_libraries=pg_stat_statements,pg_store_plans",
            "-c",
            "compute_query_id=on",
            "-c",
            "pg_store_plans.min_duration=0",
            "-c",
            "pg_store_plans.track=all",
            "-c",
            "pg_store_plans.sample_rate=1",
            "-c",
            "pg_store_plans.store_last_plan=on",
            "-c",
            "pg_store_plans.track_planning=on",
        ]);
    } else if OSSC_STORE_PLANS_MAJORS.contains(&bin.major) {
        // The upstream fork has no sampling or track_planning GUCs; setting
        // them would fail startup, so this list stays minimal.
        cmd.args([
            "-c",
            "shared_preload_libraries=pg_stat_statements,pg_store_plans",
            "-c",
            "compute_query_id=on",
            "-c",
            "pg_store_plans.min_duration=0",
            "-c",
            "pg_store_plans.track=all",
        ]);
    } else {
        // pg_stat_statements must be preloaded before it can be created.
        cmd.args(["-c", "shared_preload_libraries=pg_stat_statements"]);
    }
    // DEBUG=1 turns the server log into a full SQL trace; errors are logged
    // regardless, so the on-failure dump always shows the offending statement.
    if std::env::var("DEBUG").is_ok() {
        cmd.args(["-c", "log_statement=all", "-c", "log_min_messages=info"]);
    }
    cmd.stdout(Stdio::null())
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
