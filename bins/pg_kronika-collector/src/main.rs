//! Collects `PostgreSQL` stats and writes sealed PGM segments.
//!
//! The daemon runs on the database host. A collection signal gathers the enabled
//! `PostgreSQL` sources, writes one journal part, seals `<ts>.pgm`, and resets
//! the journal after a successful seal.
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
use kronika_format::DictLimits;
use kronika_registry::StrId;
use kronika_source_pg::database::{self, DatabaseRow, DatabaseVersion, collect_database};
use kronika_source_pg::wraparound::{WraparoundRow, collect_wraparound, to_wraparound};
use kronika_source_pg::{
    ActivityRow, ActivityVersion, collect_activity, collect_bgwriter_checkpointer, server_major,
    to_v1, to_v2, to_v3,
};
use kronika_writer::{Interner, Journal, JournalConfig, SectionBuffers, dict, seal};
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
    // Set application_name for SQL transparency.
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

async fn snapshot_and_seal(
    client: &Client,
    major: u32,
    journal: &mut Journal,
    out_dir: &Path,
    source_id: u64,
) -> Result<PathBuf> {
    // Run every query first: SectionBuffers and Interner are `!Send`, so they
    // must not be held across an await.
    let bgwriter = collect_bgwriter_checkpointer(client, major)
        .await
        .context("collect type 1_006_001")?;
    let ts = bgwriter.ts;
    let (activity_version, activity_rows) = collect_activity(client, major)
        .await
        .context("collect pg_stat_activity")?;
    let (database_version, database_rows) = collect_database(client, major)
        .await
        .context("collect pg_stat_database")?;
    let wraparound_rows = collect_wraparound(client)
        .await
        .context("collect wraparound ages")?;

    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    buffers
        .push(bgwriter)
        .map_err(|_row| anyhow::anyhow!("section buffer full for bgwriter"))?;
    push_activity(
        &mut buffers,
        &mut interner,
        activity_version,
        &activity_rows,
    )?;
    push_database(
        &mut buffers,
        &mut interner,
        database_version,
        &database_rows,
    )?;
    push_wraparound(&mut buffers, &mut interner, &wraparound_rows)?;

    let dict_sections = dict::encode(interner.window()).context("encode the segment dictionary")?;
    let part = buffers
        .flush(&dict_sections, source_id)
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

/// Limits for interned activity strings.
///
/// Query text can dominate the dictionary. Long values spill to `dict.blobs`,
/// truncate after 64 KiB, and the dictionary is capped at 16 MiB.
fn activity_dict_limits() -> DictLimits {
    DictLimits::new(4096, 64 * 1024)
        .and_then(|limits| limits.with_max_total_bytes(16 * 1024 * 1024))
        .expect("static activity dictionary limits satisfy 0 < blob <= truncate <= total")
}

/// Intern each row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_activity(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: ActivityVersion,
    rows: &[ActivityRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            ActivityVersion::V1 => buffer_row(buffers, to_v1(row, &mut intern)?)?,
            ActivityVersion::V2 => buffer_row(buffers, to_v2(row, &mut intern)?)?,
            ActivityVersion::V3 => buffer_row(buffers, to_v3(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each row's `datname` and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if `datname` cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_database(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: DatabaseVersion,
    rows: &[DatabaseRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            DatabaseVersion::V1 => buffer_row(buffers, database::to_v1(row, &mut intern)?)?,
            DatabaseVersion::V2 => buffer_row(buffers, database::to_v2(row, &mut intern)?)?,
            DatabaseVersion::V3 => buffer_row(buffers, database::to_v3(row, &mut intern)?)?,
            DatabaseVersion::V4 => buffer_row(buffers, database::to_v4(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each row's `datname` and buffer it as a wraparound section row.
///
/// # Errors
/// Returns an error if `datname` cannot be interned (dictionary full) or a
/// section buffer is full.
fn push_wraparound(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[WraparoundRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_wraparound(row, &mut intern)?)?;
    }
    Ok(())
}

/// Buffer one typed snapshot row, mapping a full buffer to an error.
fn buffer_row<S: kronika_registry::Section + 'static>(
    buffers: &mut SectionBuffers,
    row: S,
) -> Result<()> {
    buffers
        .push(row)
        .map_err(|_row| anyhow::anyhow!("section buffer is full"))
}

fn announce(line: &str) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .ok();
}

#[cfg(test)]
mod tests {
    use super::{activity_dict_limits, push_activity, push_database};
    use kronika_source_pg::database::{DatabaseRow, DatabaseVersion};
    use kronika_source_pg::{ActivityRow, ActivityVersion};
    use kronika_writer::{Interner, SectionBuffers, dict};

    fn client_row(pid: i32) -> ActivityRow {
        ActivityRow {
            ts: 1_000,
            pid,
            leader_pid: None,
            datname: Some("appdb".to_owned()),
            usename: Some("alice".to_owned()),
            application_name: "psql".to_owned(),
            client_addr: String::new(),
            backend_type: "client backend".to_owned(),
            state: Some("active".to_owned()),
            wait_event_type: None,
            wait_event: None,
            query: Some("select 1".to_owned()),
            query_id: Some(42),
            backend_xid_age: None,
            backend_xmin_age: Some(7),
            backend_start: 100,
            xact_start: Some(500),
            query_start: Some(800),
            state_change: Some(900),
        }
    }

    #[test]
    fn push_activity_buffers_rows_and_interns_their_strings() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_activity(
            &mut buffers,
            &mut interner,
            ActivityVersion::V3,
            &[client_row(1), client_row(2)],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The buffered rows use dictionary ids, and the part carries the V3
        // activity section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "strings reached the dictionary");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_001_003),
            "the part carries the pg_stat_activity section"
        );
    }

    fn db_row(datid: u32) -> DatabaseRow {
        DatabaseRow {
            ts: 1_000,
            datid,
            datname: if datid == 0 {
                None
            } else {
                Some("appdb".to_owned())
            },
            numbackends: 4,
            xact_commit: 100,
            xact_rollback: 2,
            blks_read: 4_000,
            blks_hit: 90_000,
            tup_returned: 500,
            tup_fetched: 400,
            tup_inserted: 50,
            tup_updated: 30,
            tup_deleted: 10,
            conflicts: 0,
            temp_files: 1,
            temp_bytes: 8_192,
            deadlocks: 0,
            blk_read_time: 12.5,
            blk_write_time: 3.0,
            stats_reset: Some(1_500),
            checksum_failures: Some(0),
            checksum_last_failure: None,
            session_time: Some(1_000.0),
            active_time: Some(250.0),
            idle_in_transaction_time: Some(50.0),
            sessions: Some(7),
            sessions_abandoned: Some(1),
            sessions_fatal: Some(0),
            sessions_killed: Some(0),
            parallel_workers_to_launch: Some(9),
            parallel_workers_launched: Some(8),
        }
    }

    #[test]
    fn push_database_buffers_rows_and_interns_datname() {
        let mut buffers = SectionBuffers::new();
        let mut interner = Interner::new(activity_dict_limits());
        push_database(
            &mut buffers,
            &mut interner,
            DatabaseVersion::V4,
            &[db_row(0), db_row(1)],
        )
        .expect("push interns and buffers");
        assert!(!buffers.is_empty(), "rows were buffered");

        // The non-shared row's datname should be interned, and the part should
        // contain the V4 database section.
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        assert!(!dict_sections.is_empty(), "datname was interned");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush encodes the window")
            .expect("buffered rows produce a part");
        let catalog = kronika_format::validate_part(&part).expect("a valid container");
        assert!(
            catalog
                .entries
                .iter()
                .any(|entry| entry.type_id == 1_005_004),
            "the part carries the pg_stat_database section"
        );
    }
}
