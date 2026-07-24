//! Boots the demo `PostgreSQL` 17 cluster with the stand observability profile.
//!
//! The PG 17 `bin` directory comes from `KRONIKA_PG_MATRIX`, the same
//! `major=bindir;...` contract the BDD image uses.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

use crate::config::{Config, StandPaths};

const READY_TIMEOUT: Duration = Duration::from_mins(1);

const STOP_TIMEOUT: Duration = Duration::from_mins(1);

const PG_MAJOR: u32 = 17;

const PG_PORT: u16 = 5432;

/// GUCs appended to `postgresql.conf`: the spec profile — saturated statement
/// and plan tracking plus log-visible checkpoint/lock/autovacuum events.
const STAND_CONF: &str = "\
max_connections = 100
shared_buffers = 128MB
shared_preload_libraries = 'pg_stat_statements,pg_store_plans'
compute_query_id = on
pg_stat_statements.track = all
pg_store_plans.track = all
pg_store_plans.min_duration = 0
pg_store_plans.sample_rate = 1
pg_store_plans.store_last_plan = on
pg_store_plans.track_planning = on
logging_collector = on
log_min_duration_statement = 1000
log_checkpoints = on
log_lock_waits = on
log_autovacuum_min_duration = 0
deadlock_timeout = 1s
track_io_timing = on
fsync = off
full_page_writes = off
";

/// Locates the PG 17 `bin` directory in `KRONIKA_PG_MATRIX`.
fn pg17_bindir() -> Result<PathBuf> {
    let spec = std::env::var("KRONIKA_PG_MATRIX").context("KRONIKA_PG_MATRIX is not set")?;
    parse_matrix_bindir(&spec, PG_MAJOR)
        .with_context(|| format!("KRONIKA_PG_MATRIX has no PostgreSQL {PG_MAJOR} entry: {spec:?}"))
}

/// Parses `major=bindir;...` and returns the `bin` directory for `major`.
fn parse_matrix_bindir(spec: &str, major: u32) -> Result<PathBuf> {
    for entry in spec.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let (entry_major, bindir) = entry
            .split_once('=')
            .with_context(|| format!("matrix entry has no '=': {entry:?}"))?;
        if entry_major.trim().parse::<u32>().ok() == Some(major) {
            return Ok(PathBuf::from(bindir.trim()));
        }
    }
    bail!("major {major} is absent")
}

/// Verifies the runtime carries everything the stand needs.
pub(crate) fn self_check() -> Result<()> {
    let bindir = pg17_bindir()?;
    let root = bindir
        .parent()
        .with_context(|| format!("{} has no parent", bindir.display()))?;
    for path in [
        bindir.join("initdb"),
        bindir.join("postgres"),
        root.join("lib/pg_store_plans.so"),
        root.join("share/postgresql/extension/pg_store_plans.control"),
        root.join("share/postgresql/extension/pg_stat_statements.control"),
    ] {
        ensure!(path.exists(), "missing {}", path.display());
    }
    println!(
        "self-check: PostgreSQL {PG_MAJOR} runtime at {} is complete",
        bindir.display()
    );
    Ok(())
}

/// A running demo cluster; `postgres` is killed when the value drops.
#[derive(Debug)]
pub(crate) struct Cluster {
    postgres: Child,
    dsn: String,
}

impl Cluster {
    /// `initdb`, spec GUCs, server start, extensions, and tablespaces.
    ///
    /// Cluster state is throwaway: leftovers from a previous run are removed
    /// so a reused data volume boots instead of failing `initdb`. Sealed
    /// segments and fact files survive; only they carry measurement value.
    pub(crate) async fn boot(config: &Config, paths: &StandPaths) -> Result<Self> {
        let bindir = pg17_bindir()?;
        for stale in [&paths.pgdata, &paths.tablespaces[0], &paths.tablespaces[1]] {
            if stale.exists() {
                std::fs::remove_dir_all(stale)
                    .with_context(|| format!("remove stale {}", stale.display()))?;
            }
        }
        std::fs::create_dir_all(&paths.tablespaces[0]).context("recreate the hot tablespace")?;
        std::fs::create_dir_all(&paths.tablespaces[1]).context("recreate the cold tablespace")?;
        run_initdb(&bindir, &paths.pgdata).await?;
        append_stand_conf(&paths.pgdata)?;
        let postgres = spawn_postgres(&bindir, &paths.pgdata, &config.root)?;
        let dsn = format!("host=127.0.0.1 port={PG_PORT} user=postgres dbname=postgres");
        let cluster = Self { postgres, dsn };
        cluster.wait_ready(&config.root).await?;
        cluster.prepare_objects(paths).await?;
        Ok(cluster)
    }

    pub(crate) fn dsn(&self) -> &str {
        &self.dsn
    }

    pub(crate) async fn connect(&self) -> Result<Conn> {
        connect(&self.dsn).await
    }

    /// Fast shutdown (`SIGINT`): terminate sessions, exit cleanly.
    pub(crate) async fn stop(mut self) {
        if let Some(pid) = self.postgres.id() {
            let pid = nix::unistd::Pid::from_raw(pid.cast_signed());
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGINT);
            if timeout(STOP_TIMEOUT, self.postgres.wait()).await.is_ok() {
                return;
            }
        }
        // Exited already, has no id, or ignored the fast shutdown: kill.
        drop(self.postgres.kill().await);
    }

    async fn wait_ready(&self, root: &Path) -> Result<()> {
        let probe = async {
            while tokio_postgres::connect(&self.dsn, tokio_postgres::NoTls)
                .await
                .is_err()
            {
                sleep(Duration::from_millis(200)).await;
            }
        };
        if timeout(READY_TIMEOUT, probe).await.is_err() {
            let log = std::fs::read_to_string(root.join("postgres-boot.log"))
                .unwrap_or_else(|_| "(boot log unavailable)".to_owned());
            bail!("postgres not ready within {READY_TIMEOUT:?}; boot log:\n{log}");
        }
        Ok(())
    }

    async fn prepare_objects(&self, paths: &StandPaths) -> Result<()> {
        let conn = self.connect().await?;
        let client = conn.client();
        client
            .batch_execute(
                "CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
                 CREATE EXTENSION IF NOT EXISTS pg_store_plans;",
            )
            .await
            .context("create extensions")?;
        for (name, location) in [
            ("ts_hot", &paths.tablespaces[0]),
            ("ts_cold", &paths.tablespaces[1]),
        ] {
            let location = location
                .to_str()
                .with_context(|| format!("tablespace path {} is not UTF-8", location.display()))?;
            client
                .execute(
                    &format!("CREATE TABLESPACE {name} LOCATION '{location}'"),
                    &[],
                )
                .await
                .with_context(|| format!("create tablespace {name}"))?;
        }
        Ok(())
    }
}

/// A client plus its aborted-on-drop protocol driver.
#[derive(Debug)]
pub(crate) struct Conn {
    client: tokio_postgres::Client,
    driver: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl Conn {
    pub(crate) const fn client(&self) -> &tokio_postgres::Client {
        &self.client
    }

    pub(crate) const fn client_mut(&mut self) -> &mut tokio_postgres::Client {
        &mut self.client
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

pub(crate) async fn connect(dsn: &str) -> Result<Conn> {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .context("connect")?;
    Ok(Conn {
        client,
        driver: tokio::spawn(connection),
    })
}

async fn run_initdb(bindir: &Path, pgdata: &Path) -> Result<()> {
    let output = Command::new(bindir.join("initdb"))
        .arg("-D")
        .arg(pgdata)
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
        .context("spawn initdb")?;
    ensure!(
        output.status.success(),
        "initdb failed: {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

fn append_stand_conf(pgdata: &Path) -> Result<()> {
    let conf = pgdata.join("postgresql.conf");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&conf)
        .with_context(|| format!("open {}", conf.display()))?;
    std::io::Write::write_all(&mut file, STAND_CONF.as_bytes()).context("append the stand GUCs")?;
    // Trust with no password: reachable from the host-published port and
    // from any container on the same docker network. Acceptable only because
    // the stand is a local throwaway with synthetic data.
    let hba = pgdata.join("pg_hba.conf");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&hba)
        .with_context(|| format!("open {}", hba.display()))?;
    std::io::Write::write_all(&mut file, b"host all all 0.0.0.0/0 trust\n")
        .context("append the container-network HBA rule")?;
    Ok(())
}

fn spawn_postgres(bindir: &Path, pgdata: &Path, root: &Path) -> Result<Child> {
    // Everything before the logging collector takes over lands here.
    let boot_log = std::fs::File::create(root.join("postgres-boot.log"))
        .context("create postgres-boot.log")?;
    Command::new(bindir.join("postgres"))
        .arg("-D")
        .arg(pgdata)
        // The minimal image has no /run/postgresql; keep the socket in pgdata.
        .arg("-k")
        .arg(pgdata)
        // All interfaces: the docker-published port must reach the server.
        .args(["-c", "listen_addresses=*"])
        .args(["-p", &PG_PORT.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::from(boot_log))
        .kill_on_drop(true)
        .spawn()
        .context("spawn postgres")
}

#[cfg(test)]
mod tests {
    use super::parse_matrix_bindir;
    use std::path::PathBuf;

    #[test]
    fn finds_the_requested_major() {
        let bindir = parse_matrix_bindir("15=/nix/a/bin; 17=/nix/c/bin", 17).expect("major found");
        assert_eq!(bindir, PathBuf::from("/nix/c/bin"), "17 maps to its bindir");
    }

    #[test]
    fn rejects_an_absent_major() {
        assert!(
            parse_matrix_bindir("15=/nix/a/bin", 17).is_err(),
            "missing major is an error"
        );
    }

    #[test]
    fn rejects_a_malformed_entry() {
        assert!(
            parse_matrix_bindir("17:/nix/c/bin", 17).is_err(),
            "an entry without '=' is an error"
        );
    }
}
