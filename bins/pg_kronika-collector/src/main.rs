//! Collects `PostgreSQL` stats and writes sealed PGM segments.
//!
//! The daemon runs on the database host. Each `SIGUSR2` collects one
//! `1_006_001` snapshot, appends it to a temporary journal, seals `<ts>.pgm`,
//! then clears the journal for the next signal.
//!
//! Environment:
//! - `KRONIKA_PG_DSN`: libpq connection string for the target server;
//! - `KRONIKA_OUT_DIR`: directory that receives sealed segments;
//! - `KRONIKA_SOURCE_ID`: optional source id, `0` by default.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kronika_source_pg::{
    collect_bgwriter, collect_checkpointer, collect_reset_metadata, collect_reset_metadata_io,
    server_major,
};
use kronika_writer::{Journal, JournalConfig, SectionBuffers, seal};
use tokio::signal::unix::{SignalKind, signal};
use tokio_postgres::Client;

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

    let mut pg_config: tokio_postgres::Config = config
        .dsn
        .parse()
        .context("parse KRONIKA_PG_DSN as a connection string")?;
    // Name us in pg_stat_activity (SQL-transparency rule).
    pg_config.application_name(concat!("pg_kronika-collector/", env!("CARGO_PKG_VERSION")));
    let (client, connection) = pg_config
        .connect(tokio_postgres::NoTls)
        .await
        .context("connect to PostgreSQL")?;
    // The server reports its version in the handshake; read it once, no query.
    let major = server_major(connection.parameter("server_version"))
        .context("server did not report a parseable server_version")?;
    // tokio-postgres drives I/O through this future.
    tokio::spawn(connection);

    // Only sealed segments leave this process.
    let journal_dir = tempfile::tempdir().context("create the journal directory")?;
    let (mut journal, _report) = Journal::open(
        &journal_dir.path().join("active.parts"),
        JournalConfig::default(),
    )
    .context("open the journal")?;

    let mut sigusr2 = signal(SignalKind::user_defined2()).context("install the SIGUSR2 handler")?;
    let mut sigterm = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;

    announce("ready");

    loop {
        tokio::select! {
            Some(()) = sigusr2.recv() => {
                match snapshot_and_seal(&client, major, &mut journal, &config.out_dir, config.source_id)
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

#[allow(
    clippy::future_not_send,
    reason = "awaited inline in the signal loop, never spawned across threads, so holding the non-Send SectionBuffers across the per-family collect awaits is sound"
)]
async fn snapshot_and_seal(
    client: &Client,
    major: u32,
    journal: &mut Journal,
    out_dir: &Path,
    source_id: u64,
) -> Result<PathBuf> {
    let mut buffers = SectionBuffers::new();

    // Background-writer family: PG17 split the catalog, so each major writes its
    // own exact type_id (see the registry's `type_id` rule).
    let ts = if major >= 17 {
        let row = collect_checkpointer(client)
            .await
            .context("collect type 1_006_002")?;
        let ts = row.ts;
        buffers
            .push(row)
            .map_err(|_row| anyhow::anyhow!("section 1_006_002 buffer full for one row"))?;
        ts
    } else {
        let row = collect_bgwriter(client)
            .await
            .context("collect type 1_006_001")?;
        let ts = row.ts;
        buffers
            .push(row)
            .map_err(|_row| anyhow::anyhow!("section 1_006_001 buffer full for one row"))?;
        ts
    };

    // Reset-context family: PG16 added pg_stat_io, again a separate type_id.
    if major >= 16 {
        let row = collect_reset_metadata_io(client)
            .await
            .context("collect type 1_020_002")?;
        buffers
            .push(row)
            .map_err(|_row| anyhow::anyhow!("section 1_020_002 buffer full for one row"))?;
    } else {
        let row = collect_reset_metadata(client)
            .await
            .context("collect type 1_020_001")?;
        buffers
            .push(row)
            .map_err(|_row| anyhow::anyhow!("section 1_020_001 buffer full for one row"))?;
    }
    let part = buffers
        .flush(&[], source_id)
        .context("encode the collection window")?
        .context("a buffered row must yield a part")?;
    journal
        .append(&part)
        .context("append the part to the journal")?;

    let dest = out_dir.join(format!("{}.pgm", ts.0));
    seal(journal, &dest).context("seal the segment")?;
    // Leave active.parts intact if seal() fails.
    journal.reset().context("reset the journal after seal")?;
    Ok(dest)
}

fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .ok();
}
