//! Collector daemon: snapshot `PostgreSQL` stats into sealed PGM segments.
//!
//! This is the only privileged pgKronika process — it runs on the database
//! host. On `SIGUSR2` it takes one snapshot (currently type `1_006_001`),
//! encodes it into a journal part, and seals one `<ts>.pgm` segment. Peak memory
//! is a single snapshot in flight: the writer bounds journal and section sizes,
//! and the daemon never reads a segment back.
//!
//! Configuration is environment-only, matching the BDD harness:
//! - `KRONIKA_PG_DSN`: libpq connection string for the target server;
//! - `KRONIKA_OUT_DIR`: directory that receives sealed segments;
//! - `KRONIKA_SOURCE_ID`: optional `u64` instance id stamped into parts (0 if unset).
//!
//! The part/journal/seal contract lives in `crates/kronika-writer/README.md`.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kronika_registry::Ts;
use kronika_source_pg::collect_bgwriter_checkpointer;
use kronika_writer::{Journal, JournalConfig, SectionBuffers, seal};
use tokio::signal::unix::{SignalKind, signal};
use tokio_postgres::Client;

/// Environment configuration; see the module docs for each variable.
struct Config {
    dsn: String,
    out_dir: PathBuf,
    source_id: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        let dsn = std::env::var("KRONIKA_PG_DSN").context("KRONIKA_PG_DSN is not set")?;
        let out_dir = std::env::var("KRONIKA_OUT_DIR")
            .context("KRONIKA_OUT_DIR is not set")?
            .into();
        let source_id = match std::env::var("KRONIKA_SOURCE_ID") {
            Ok(value) => value.parse().context("KRONIKA_SOURCE_ID is not a u64")?,
            Err(_) => 0,
        };
        Ok(Self {
            dsn,
            out_dir,
            source_id,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    std::fs::create_dir_all(&config.out_dir).context("create the output directory")?;

    let (client, connection) = tokio_postgres::connect(&config.dsn, tokio_postgres::NoTls)
        .await
        .context("connect to PostgreSQL")?;
    // Drive the connection in the background. If it ends, later collects fail and
    // are logged, but the daemon keeps serving signals.
    tokio::spawn(connection);

    // The journal is process-private: each snapshot appends one part, seals a
    // segment, then resets. Only sealed segments reach the configured out dir.
    let journal_dir = tempfile::tempdir().context("create the journal directory")?;
    let (mut journal, _report) = Journal::open(
        &journal_dir.path().join("active.parts"),
        JournalConfig::default(),
    )
    .context("open the journal")?;

    let mut sigusr2 = signal(SignalKind::user_defined2()).context("install the SIGUSR2 handler")?;
    let mut sigterm = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;

    // The readiness line lets a supervisor (and the BDD harness) wait until the
    // daemon is connected and listening before sending the first SIGUSR2.
    announce("ready");

    loop {
        tokio::select! {
            Some(()) = sigusr2.recv() => {
                match snapshot_and_seal(&client, &mut journal, &config.out_dir, config.source_id)
                    .await
                {
                    Ok(dest) => announce(&format!("sealed {}", dest.display())),
                    Err(err) => eprintln!("pg_kronika-collector: snapshot failed: {err:#}"),
                }
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }
    }
    Ok(())
}

/// Take one snapshot and seal it into a fresh segment under `out_dir`, returning
/// the published path.
///
/// # Errors
///
/// Returns an error if the collection query, encoding, journal append, or seal
/// fails. The journal is reset only after a successful seal, per the writer
/// contract.
async fn snapshot_and_seal(
    client: &Client,
    journal: &mut Journal,
    out_dir: &Path,
    source_id: u64,
) -> Result<PathBuf> {
    let ts = Ts(now_micros()?);
    let row = collect_bgwriter_checkpointer(client, ts)
        .await
        .context("collect type 1_006_001")?;

    let mut buffers = SectionBuffers::new();
    buffers
        .push(row)
        .map_err(|_row| anyhow::anyhow!("section buffer full for a single row"))?;
    let part = buffers
        .flush(&[], source_id)
        .context("encode the collection window")?
        .context("a buffered row must yield a part")?;
    journal
        .append(&part)
        .context("append the part to the journal")?;

    let dest = out_dir.join(format!("{}.pgm", ts.0));
    seal(journal, &dest).context("seal the segment")?;
    journal.reset().context("reset the journal after seal")?;
    Ok(dest)
}

/// Current unix time in microseconds, the snapshot timestamp.
fn now_micros() -> Result<i64> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?;
    i64::try_from(since_epoch.as_micros()).context("unix microseconds overflow i64")
}

/// Print a status line to stdout and flush it, so a reader blocked on our output
/// sees each event immediately. Best-effort: a closed stdout never crashes the
/// collector.
fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .ok();
}
