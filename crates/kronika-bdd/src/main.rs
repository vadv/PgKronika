//! BDD runner for Docker-only integration scenarios.
//!
//! Nix supplies `PostgreSQL` versions through `KRONIKA_PG_MATRIX`. Host
//! `cargo test --workspace` stays database-free.
#![allow(
    clippy::trivial_regex,
    reason = "cucumber step phrases are literal English, matched as plain text, not real regexes"
)]
#![allow(
    clippy::multiple_crate_versions,
    reason = "cucumber's dependency tree pulls duplicate transitive versions outside our control"
)]
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber passes &mut World to every step by contract, even read-only ones"
)]

mod cluster;
mod collector;

use std::path::Path;

use anyhow::Context;
use cucumber::{World, event, given, then, when};
use kronika_format::{Entry, crc32c};
use kronika_reader::{Dictionary, Resolved, Segment};
use kronika_registry::{
    Bytes, MAX_SECTION_BYTES, Section, StrId, VerifiedSection,
    bgwriter_checkpointer::BgwriterCheckpointer,
    pg_locks::{PgLocksV1, PgLocksV2},
    pg_prepared_xacts::PgPreparedXacts,
    pg_stat_activity::PgStatActivityV3,
    pg_stat_archiver::PgStatArchiver,
    pg_stat_database::{PgStatDatabaseV3, PgStatDatabaseV4},
    pg_stat_io::{PgStatIoV1, PgStatIoV2},
    pg_stat_progress_vacuum::PgStatProgressVacuum,
    pg_stat_statements::{
        PgStatStatementsV1, PgStatStatementsV2, PgStatStatementsV3, PgStatStatementsV4,
        PgStatStatementsV5, PgStatStatementsV6,
    },
    pg_stat_user_indexes::{PgStatUserIndexesV1, PgStatUserIndexesV2},
    pg_stat_user_tables::{
        PgStatUserTablesV1, PgStatUserTablesV2, PgStatUserTablesV3, PgStatUserTablesV4,
    },
    pg_stat_wal::{PgStatWalV1, PgStatWalV2},
    replication_instance::ReplicationInstance,
};
use kronika_source_pg::database::{DatabaseVersion, database_version};
use kronika_source_pg::io::{IoVersion, io_version};
use kronika_source_pg::statements::{StatementsVersion, statements_extversion, statements_version};
use kronika_source_pg::user_indexes::{UserIndexesVersion, indexdef_max_len, user_indexes_version};
use kronika_source_pg::user_tables::{UserTablesVersion, user_tables_version};
use kronika_source_pg::wal::{WalVersion, wal_version};
use kronika_source_pg::{ActivityVersion, activity_version, collect_bgwriter_checkpointer};

const PG17_MAJOR: u32 = 17;
const PG18_MAJOR: u32 = 18;

/// Each BDD scenario boots the full `PostgreSQL` matrix; one scenario at a time
/// keeps CI from starting dozens of clusters at once.
const MAX_CONCURRENT_SCENARIOS: usize = 1;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;

const PG_STAT_ACTIVITY_V3_TYPE_ID: u32 = 1_001_003;

const PG_STAT_DATABASE_V3_TYPE_ID: u32 = 1_005_003;
const PG_STAT_DATABASE_V4_TYPE_ID: u32 = 1_005_004;

const PG_STAT_STATEMENTS_V1_TYPE_ID: u32 = 1_002_001;
const PG_STAT_STATEMENTS_V2_TYPE_ID: u32 = 1_002_002;
const PG_STAT_STATEMENTS_V3_TYPE_ID: u32 = 1_002_003;
const PG_STAT_STATEMENTS_V4_TYPE_ID: u32 = 1_002_004;
const PG_STAT_STATEMENTS_V5_TYPE_ID: u32 = 1_002_005;
const PG_STAT_STATEMENTS_V6_TYPE_ID: u32 = 1_002_006;

const PG_STAT_USER_TABLES_V1_TYPE_ID: u32 = 1_013_001;
const PG_STAT_USER_TABLES_V2_TYPE_ID: u32 = 1_013_002;
const PG_STAT_USER_TABLES_V3_TYPE_ID: u32 = 1_013_003;
const PG_STAT_USER_TABLES_V4_TYPE_ID: u32 = 1_013_004;

const PG_STAT_USER_INDEXES_V1_TYPE_ID: u32 = 1_014_001;
const PG_STAT_USER_INDEXES_V2_TYPE_ID: u32 = 1_014_002;

/// Databases seeded by the user-tables scenario; each gets one probe table.
const SEEDED_DATABASES: [&str; 2] = ["kronika_ut_a", "kronika_ut_b"];

const PG_REPLICATION_INSTANCE_TYPE_ID: u32 = 1_015_001;
const PG_STAT_WAL_V1_TYPE_ID: u32 = 1_007_001;
const PG_STAT_WAL_V2_TYPE_ID: u32 = 1_007_002;

const PG_STAT_ARCHIVER_TYPE_ID: u32 = 1_008_001;

const PG_STAT_IO_V1_TYPE_ID: u32 = 1_009_001;
const PG_STAT_IO_V2_TYPE_ID: u32 = 1_009_002;

const PG_PREPARED_XACTS_TYPE_ID: u32 = 1_010_001;

const PG_LOCKS_V1_TYPE_ID: u32 = 1_011_001;
const PG_LOCKS_V2_TYPE_ID: u32 = 1_011_002;

/// First major on the PG14+ `pg_locks` layout that carries `waitstart`.
const PG_LOCKS_V2_MIN_MAJOR: u32 = 14;

const PG_STAT_PROGRESS_VACUUM_TYPE_ID: u32 = 1_012_001;

/// A live blocking pair held open across a snapshot: the holder session (whose
/// uncommitted transaction keeps the row lock) and the waiter's blocked `UPDATE`
/// task. Both must outlive the snapshot; dropping either releases the wait.
#[derive(Debug)]
struct LockWait {
    /// Holder session; its `BEGIN; UPDATE` transaction is never committed, so the
    /// row lock stays held until this connection is dropped.
    #[allow(
        dead_code,
        reason = "held open to keep the row lock; dropped to release it, not read"
    )]
    holder: cluster::Conn,
    /// The waiter's blocked `UPDATE`, running on its own connection inside the
    /// task. Aborting it drops that connection and cancels the query.
    waiter: tokio::task::JoinHandle<()>,
}

impl Drop for LockWait {
    fn drop(&mut self) {
        // Abort the waiter first so its query stops; the holder connection then
        // drops via its own `Drop`, rolling back the open transaction.
        self.waiter.abort();
    }
}

/// Cucumber state for one scenario: the matrix booted by the `Given` step.
#[derive(Debug, Default, World)]
struct BddWorld {
    clusters: Vec<cluster::Cluster>,
    /// Blocking pairs kept alive while the wait-tree scenario snapshots; one per
    /// cluster. Cleared (and thus released) after the assertion.
    lock_waits: Vec<LockWait>,
}

#[given("the PostgreSQL matrix is booted")]
async fn boot_matrix_step(world: &mut BddWorld) -> anyhow::Result<()> {
    let spec = std::env::var("KRONIKA_PG_MATRIX").context("KRONIKA_PG_MATRIX is not set")?;
    let matrix = cluster::parse_matrix(&spec)?;
    world.clusters = cluster::boot_matrix(&matrix).await?;
    Ok(())
}

#[then("every version answers a version query")]
async fn every_version_answers(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let version = db.server_version().await?;
        let major = db.major().to_string();
        anyhow::ensure!(
            version.starts_with(&major),
            "postgres {major} reported server_version {version:?}"
        );
    }
    Ok(())
}

#[then("every version reports valid bgwriter/checkpointer stats")]
async fn every_version_reports_stats(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let conn = db.connect().await?;
        let major = conn
            .major()
            .with_context(|| format!("postgres {}: server reported no version", db.major()))?;
        anyhow::ensure!(
            major == db.major(),
            "postgres {}: handshake reported major {major} instead",
            db.major()
        );
        let snapshot = collect_bgwriter_checkpointer(conn.client(), major)
            .await
            .with_context(|| format!("collect type 1_006_001 on postgres {}", db.major()))?;
        check_snapshot(db.major(), now_micros()?, &snapshot)?;
    }
    Ok(())
}

fn now_micros() -> anyhow::Result<i64> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?;
    i64::try_from(since_epoch.as_micros()).context("unix microseconds overflow i64")
}

/// Basic invariants for a row read directly from `PostgreSQL`.
fn check_snapshot(major: u32, host_now: i64, snap: &BgwriterCheckpointer) -> anyhow::Result<()> {
    anyhow::ensure!(
        snap.ts.0 > 0 && (snap.ts.0 - host_now).abs() < 300_000_000,
        "postgres {major}: snapshot ts {} is not within 5 min of the runner clock {host_now}",
        snap.ts.0
    );
    anyhow::ensure!(
        snap.checkpoints_timed >= 0 && snap.buffers_clean >= 0 && snap.buffers_alloc >= 0,
        "postgres {major}: a counter came back negative"
    );
    anyhow::ensure!(
        snap.bgwriter_stats_reset.0 > 0 && snap.bgwriter_stats_reset.0 <= snap.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        snap.bgwriter_stats_reset.0,
        snap.ts.0
    );
    assert_version_columns(major, snap)
}

/// PG17 moved checkpoint fields out of `pg_stat_bgwriter`; older versions keep
/// the old column set.
fn assert_version_columns(major: u32, snap: &BgwriterCheckpointer) -> anyhow::Result<()> {
    if major >= PG17_MAJOR {
        anyhow::ensure!(
            snap.restartpoints_timed.is_some()
                && snap.restartpoints_req.is_some()
                && snap.restartpoints_done.is_some()
                && snap.checkpointer_stats_reset.is_some(),
            "postgres {major}: PG17+ checkpointer columns came back NULL"
        );
        anyhow::ensure!(
            snap.buffers_backend.is_none() && snap.buffers_backend_fsync.is_none(),
            "postgres {major}: PG17 dropped buffers_backend, but the snapshot has it"
        );
    } else {
        anyhow::ensure!(
            snap.buffers_backend.is_some() && snap.buffers_backend_fsync.is_some(),
            "postgres {major}: pre-PG17 buffers_backend came back NULL"
        );
        anyhow::ensure!(
            snap.restartpoints_timed.is_none()
                && snap.restartpoints_req.is_none()
                && snap.restartpoints_done.is_none()
                && snap.checkpointer_stats_reset.is_none(),
            "postgres {major}: pre-PG17 must not fill the checkpointer columns"
        );
    }
    Ok(())
}

#[then("every version is collected into a sealed segment with section 1_006_001")]
async fn every_version_seals_a_segment(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_sealed_section(db.major(), &segment)?;
    }
    Ok(())
}

fn assert_sealed_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.type_id == BGWRITER_CHECKPOINTER_TYPE_ID)
        .with_context(|| format!("postgres {major}: segment has no section 1_006_001"))?;

    // Check typed values, not just section presence.
    let row = decode_sealed_row(path, entry)
        .with_context(|| format!("postgres {major}: read back section 1_006_001"))?;

    ensure_ts_in_segment_range(
        major,
        "section 1_006_001",
        row.ts.0,
        catalog.min_ts,
        catalog.max_ts,
    )?;
    anyhow::ensure!(
        row.bgwriter_stats_reset.0 > 0 && row.bgwriter_stats_reset.0 <= row.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        row.bgwriter_stats_reset.0,
        row.ts.0
    );
    assert_version_columns(major, &row)
}

/// Read the catalog-bounded section and decode its single typed row.
fn decode_sealed_row(path: &Path, entry: &Entry) -> anyhow::Result<BgwriterCheckpointer> {
    use std::os::unix::fs::FileExt;

    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;

    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    let mut rows = BgwriterCheckpointer::decode(verified)
        .context("typed decode of section 1_006_001")?
        .into_iter();
    let row = rows.next().context("section decoded to no rows")?;
    anyhow::ensure!(
        rows.next().is_none(),
        "section unexpectedly decoded to multiple rows"
    );
    Ok(row)
}

#[then("every version seals a segment whose pg_stat_activity rows resolve through the dictionary")]
async fn every_version_seals_activity(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_activity_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Decode the sealed `pg_stat_activity` section and resolve its strings.
///
/// The matrix runs PG14+, so every cluster maps to the V3 layout. The check also
/// verifies that the collector's own backend is present in the snapshot.
fn assert_activity_section(major: u32, path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        activity_version(major) == ActivityVersion::V3,
        "postgres {major}: matrix version is not on the V3 activity layout"
    );
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.type_id == PG_STAT_ACTIVITY_V3_TYPE_ID)
        .with_context(|| format!("postgres {major}: segment has no section 1_001_003"))?;

    let rows = decode_activity_rows(path, entry)
        .with_context(|| format!("postgres {major}: read back section 1_001_003"))?;
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_activity section decoded to no rows"
    );

    // One statement_timestamp() covers the whole snapshot.
    let ts = rows[0].ts.0;
    anyhow::ensure!(
        rows.iter().all(|row| row.ts.0 == ts),
        "postgres {major}: snapshot rows carry differing ts"
    );
    ensure_ts_in_segment_range(
        major,
        "section 1_001_003",
        ts,
        catalog.min_ts,
        catalog.max_ts,
    )?;

    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let mut saw_collector = false;
    for row in &rows {
        match dict.resolve(row.application_name.0) {
            Some(Resolved::String(bytes)) => {
                if bytes.starts_with(b"pg_kronika-collector") {
                    saw_collector = true;
                }
            }
            other => anyhow::bail!(
                "postgres {major}: application_name str_id {} did not resolve to a string: {other:?}",
                row.application_name.0
            ),
        }
    }
    anyhow::ensure!(
        saw_collector,
        "postgres {major}: the collector's own backend was not found in the snapshot"
    );
    Ok(())
}

fn ensure_ts_in_segment_range(
    major: u32,
    section: &str,
    ts: i64,
    min_ts: i64,
    max_ts: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        ts > 0 && ts >= min_ts && ts <= max_ts,
        "postgres {major}: {section} ts {ts} outside segment range {min_ts}..={max_ts}"
    );
    Ok(())
}

/// Read the catalog-bounded section and decode its typed rows.
fn decode_activity_rows(path: &Path, entry: &Entry) -> anyhow::Result<Vec<PgStatActivityV3>> {
    use std::os::unix::fs::FileExt;

    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;

    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    PgStatActivityV3::decode(verified).context("typed decode of section 1_001_003")
}

#[then(
    "each matrix cluster seals pg_stat_database rows with catalog fields and dictionary-backed names"
)]
async fn every_version_seals_database(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_database_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Decode the sealed `pg_stat_database` section for the selected layout, then
/// check one snapshot timestamp, the shared row, dictionary-backed database
/// names, and `pg_database` catalog fields.
fn assert_database_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    match database_version(major) {
        DatabaseVersion::V4 => {
            let rows = decode_section_rows::<PgStatDatabaseV4>(
                path,
                &segment,
                PG_STAT_DATABASE_V4_TYPE_ID,
            )
            .with_context(|| format!("postgres {major}: read back section 1_005_004"))?;
            check_database_rows(
                major,
                &dict,
                "section 1_005_004",
                segment.catalog().min_ts,
                segment.catalog().max_ts,
                rows.iter().map(|r| DatabaseObservation {
                    datid: r.datid,
                    datname: r.datname,
                    ts: r.ts.0,
                    numbackends: r.numbackends,
                    frozen_xid_age: r.frozen_xid_age,
                    min_mxid_age: r.min_mxid_age,
                    datconnlimit: r.datconnlimit,
                    datallowconn: r.datallowconn,
                    datistemplate: r.datistemplate,
                }),
            )
        }
        DatabaseVersion::V3 => {
            let rows = decode_section_rows::<PgStatDatabaseV3>(
                path,
                &segment,
                PG_STAT_DATABASE_V3_TYPE_ID,
            )
            .with_context(|| format!("postgres {major}: read back section 1_005_003"))?;
            check_database_rows(
                major,
                &dict,
                "section 1_005_003",
                segment.catalog().min_ts,
                segment.catalog().max_ts,
                rows.iter().map(|r| DatabaseObservation {
                    datid: r.datid,
                    datname: r.datname,
                    ts: r.ts.0,
                    numbackends: r.numbackends,
                    frozen_xid_age: r.frozen_xid_age,
                    min_mxid_age: r.min_mxid_age,
                    datconnlimit: r.datconnlimit,
                    datallowconn: r.datallowconn,
                    datistemplate: r.datistemplate,
                }),
            )
        }
        other => {
            anyhow::bail!("postgres {major}: matrix version maps to {other:?}, expected V3 or V4")
        }
    }
}

/// Database row fields covered by the live BDD matrix for V3/V4 layouts.
#[derive(Debug, Clone, Copy)]
struct DatabaseObservation {
    datid: u32,
    datname: Option<StrId>,
    ts: i64,
    numbackends: Option<i32>,
    frozen_xid_age: Option<i64>,
    min_mxid_age: Option<i64>,
    datconnlimit: Option<i32>,
    datallowconn: Option<bool>,
    datistemplate: Option<bool>,
}

/// Read the catalog-bounded section and decode its typed rows.
fn decode_section_rows<T: Section>(
    path: &Path,
    segment: &Segment,
    type_id: u32,
) -> anyhow::Result<Vec<T>> {
    use std::os::unix::fs::FileExt;

    let entry = segment
        .catalog()
        .entries
        .iter()
        .find(|entry| entry.type_id == type_id)
        .with_context(|| format!("segment has no section {type_id}"))?;
    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;

    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    T::decode(verified).context("typed decode of the sealed section")
}

/// Shared invariants for the decoded database rows.
fn check_database_rows(
    major: u32,
    dict: &Dictionary,
    section: &str,
    min_ts: i64,
    max_ts: i64,
    rows: impl Iterator<Item = DatabaseObservation>,
) -> anyhow::Result<()> {
    let rows: Vec<_> = rows.collect();
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_database section decoded to no rows"
    );

    // One statement_timestamp() covers the whole snapshot.
    let ts = rows[0].ts;
    anyhow::ensure!(
        rows.iter().all(|row| row.ts == ts),
        "postgres {major}: snapshot rows carry differing ts"
    );
    ensure_ts_in_segment_range(major, section, ts, min_ts, max_ts)?;

    // PG12+ adds a shared-objects row with `datid = 0` and no `datname`.
    // PostgreSQL docs allow NULL `numbackends`, while PG12+ source returns 0.
    let shared = rows
        .iter()
        .find(|row| row.datid == 0)
        .with_context(|| format!("postgres {major}: no datid=0 shared-objects row"))?;
    anyhow::ensure!(
        shared.datname.is_none(),
        "postgres {major}: shared-objects row has a non-null datname"
    );
    if let Some(numbackends) = shared.numbackends {
        anyhow::ensure!(
            numbackends >= 0,
            "postgres {major}: shared-objects row has a negative numbackends"
        );
    }
    anyhow::ensure!(
        shared.frozen_xid_age.is_none(),
        "postgres {major}: shared-objects row has a non-null frozen_xid_age"
    );
    anyhow::ensure!(
        shared.min_mxid_age.is_none(),
        "postgres {major}: shared-objects row has a non-null min_mxid_age"
    );
    anyhow::ensure!(
        shared.datconnlimit.is_none()
            && shared.datallowconn.is_none()
            && shared.datistemplate.is_none(),
        "postgres {major}: shared-objects row has non-null pg_database flags"
    );

    let real_rows: Vec<_> = rows.iter().filter(|row| row.datid != 0).collect();
    anyhow::ensure!(
        !real_rows.is_empty(),
        "postgres {major}: no datid != 0 database row"
    );

    for real in real_rows {
        let datname = real.datname.with_context(|| {
            format!(
                "postgres {major}: datid {} row has a null datname",
                real.datid
            )
        })?;
        match dict.resolve(datname.0) {
            Some(Resolved::String(bytes)) => anyhow::ensure!(
                !bytes.is_empty(),
                "postgres {major}: datid {} datname resolved to an empty string",
                real.datid
            ),
            other => anyhow::bail!(
                "postgres {major}: datname str_id {} did not resolve to a string: {other:?}",
                datname.0
            ),
        }
        anyhow::ensure!(
            real.numbackends.is_some(),
            "postgres {major}: datid {} row has a null numbackends",
            real.datid
        );
        anyhow::ensure!(
            real.frozen_xid_age.is_some(),
            "postgres {major}: datid {} row has a null frozen_xid_age",
            real.datid
        );
        anyhow::ensure!(
            real.min_mxid_age.is_some(),
            "postgres {major}: datid {} row has a null min_mxid_age",
            real.datid
        );
        anyhow::ensure!(
            real.datconnlimit.is_some(),
            "postgres {major}: datid {} row has a null datconnlimit",
            real.datid
        );
        anyhow::ensure!(
            real.datallowconn.is_some(),
            "postgres {major}: datid {} row has a null datallowconn",
            real.datid
        );
        anyhow::ensure!(
            real.datistemplate.is_some(),
            "postgres {major}: datid {} row has a null datistemplate",
            real.datid
        );
    }
    Ok(())
}

#[then(
    "each matrix cluster seals pg_stat_user_tables rows from two seeded databases with dictionary-backed names"
)]
async fn every_version_seals_user_tables(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        for datname in SEEDED_DATABASES {
            seed_user_table_database(db, datname).await?;
        }
        // The collector refreshes the pool on SIGUSR2, so the seeded databases
        // are enumerated and walked in this snapshot.
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_user_tables_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Create a database with one probe table carrying rows and fresh statistics, so
/// the table lands in the size and activity candidate axes. `CREATE DATABASE`
/// cannot run inside a transaction block, hence the separate statements.
async fn seed_user_table_database(db: &cluster::Cluster, datname: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        datname.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
        "seed database name {datname:?} is not a safe identifier"
    );
    let admin = db.connect().await?;
    let exists = admin
        .client()
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&datname])
        .await
        .with_context(|| format!("postgres {}: probe database {datname}", db.major()))?;
    if exists.is_none() {
        admin
            .client()
            .batch_execute(&format!("CREATE DATABASE {datname}"))
            .await
            .with_context(|| format!("postgres {}: create database {datname}", db.major()))?;
    }
    drop(admin);

    let dsn = db.conn_string_db(datname);
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .with_context(|| format!("postgres {}: connect to {datname}", db.major()))?;
    let driver = tokio::spawn(connection);
    let result = async {
        let long_predicate = long_partial_index_predicate();
        let seed_sql = format!(
            "CREATE TABLE IF NOT EXISTS kronika_ut_probe (id int primary key, payload text); \
             INSERT INTO kronika_ut_probe \
               SELECT g, repeat('x', 16) FROM generate_series(1, 200) g \
               ON CONFLICT (id) DO NOTHING; \
             DROP INDEX IF EXISTS kronika_ut_probe_long_idx; \
             CREATE INDEX kronika_ut_probe_long_idx \
               ON kronika_ut_probe (lower(payload || '_' || id::text)) \
               WHERE {long_predicate};",
        );
        client
            .batch_execute(&seed_sql)
            .await
            .with_context(|| format!("postgres {}: seed table in {datname}", db.major()))?;
        // VACUUM cannot run inside a transaction block, and a multi-statement
        // simple query executes as one implicit transaction — so VACUUM ANALYZE
        // must be its own statement.
        client
            .batch_execute("VACUUM ANALYZE kronika_ut_probe;")
            .await
            .with_context(|| format!("postgres {}: vacuum analyze in {datname}", db.major()))?;
        if db.major() >= 16 {
            client
                .batch_execute(
                    "BEGIN; \
                     SET LOCAL enable_seqscan = off; \
                     SELECT payload FROM kronika_ut_probe WHERE id = 1; \
                     COMMIT;",
                )
                .await
                .with_context(|| format!("postgres {}: scan pkey in {datname}", db.major()))?;
            client
                .batch_execute("SELECT pg_stat_force_next_flush();")
                .await
                .with_context(|| format!("postgres {}: flush stats in {datname}", db.major()))?;
        }
        Ok(())
    }
    .await;
    driver.abort();
    result
}

/// Build a predicate whose `pg_get_indexdef` text exceeds the collector cap.
fn long_partial_index_predicate() -> String {
    // A large IN-list makes old PostgreSQL versions reject the pg_index row as
    // too wide. One long text constant keeps the catalog tuple smaller while the
    // deparsed index definition still exceeds the SQL-side cap.
    let literal = "x".repeat(5_200);
    format!("payload IS NOT NULL AND payload <> '{literal}'")
}

/// Database row fields covered by the live BDD matrix for user-tables layouts.
#[derive(Debug, Clone, Copy)]
struct UserTableObservation {
    datname: StrId,
    schemaname: StrId,
    relname: StrId,
    ts: i64,
}

/// Decode the sealed `pg_stat_user_tables` section for the selected layout, then
/// check one snapshot timestamp, that the two seeded databases both contributed
/// the probe table, and that datname/schemaname/relname resolve through the
/// dictionary.
fn assert_user_tables_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    let observations = match user_tables_version(major) {
        UserTablesVersion::V4 => decode_section_rows::<PgStatUserTablesV4>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V4_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_004"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserTablesVersion::V3 => decode_section_rows::<PgStatUserTablesV3>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V3_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_003"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserTablesVersion::V2 => decode_section_rows::<PgStatUserTablesV2>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V2_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_002"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserTablesVersion::V1 => decode_section_rows::<PgStatUserTablesV1>(
            path,
            &segment,
            PG_STAT_USER_TABLES_V1_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_013_001"))?
        .iter()
        .map(|r| UserTableObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            relname: r.relname,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
    };
    check_user_tables_rows(major, &dict, min_ts, max_ts, &observations)
}

/// Shared invariants over the decoded user-tables rows.
fn check_user_tables_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[UserTableObservation],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_user_tables section decoded to no rows"
    );

    // One statement_timestamp() per database, but every snapshot ts must fall in
    // the segment range; the rows share a single collection window.
    for row in rows {
        ensure_ts_in_segment_range(major, "section 1_013", row.ts, min_ts, max_ts)?;
    }

    // Every label resolves, and the probe table from both seeded databases is
    // present (datname is the discriminator: the table name is identical).
    let mut seeded_with_probe: std::collections::HashSet<Vec<u8>> =
        std::collections::HashSet::new();
    for row in rows {
        let datname = resolve_string(major, dict, "datname", row.datname.0)?;
        let schemaname = resolve_string(major, dict, "schemaname", row.schemaname.0)?;
        let relname = resolve_string(major, dict, "relname", row.relname.0)?;
        anyhow::ensure!(
            !schemaname.is_empty(),
            "postgres {major}: schemaname resolved to an empty string"
        );
        if relname == b"kronika_ut_probe"
            && SEEDED_DATABASES
                .iter()
                .any(|name| name.as_bytes() == datname.as_slice())
        {
            seeded_with_probe.insert(datname);
        }
    }
    for datname in SEEDED_DATABASES {
        anyhow::ensure!(
            seeded_with_probe.contains(datname.as_bytes()),
            "postgres {major}: no kronika_ut_probe row for database {datname}"
        );
    }
    Ok(())
}

#[then(
    "each matrix cluster seals pg_stat_user_indexes rows from two seeded databases with dictionary-backed names"
)]
async fn every_version_seals_user_indexes(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        for datname in SEEDED_DATABASES {
            seed_user_table_database(db, datname).await?;
        }
        // The collector refreshes the pool on SIGUSR2, so the seeded databases
        // are enumerated and walked in this snapshot.
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_user_indexes_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Index row fields covered by the live BDD matrix for user-indexes layouts.
#[derive(Debug, Clone, Copy)]
struct UserIndexObservation {
    datname: StrId,
    schemaname: StrId,
    indexrelname: StrId,
    amname: StrId,
    indexdef: StrId,
    indisprimary: bool,
    indisexclusion: bool,
    indisready: bool,
    main_fork_bytes: i64,
    idx_blks_read: i64,
    idx_blks_hit: i64,
    /// Scan recency in unix microseconds on V2 (PG16+); `None` on V1 layouts and
    /// when the index has never been scanned.
    last_idx_scan: Option<i64>,
    ts: i64,
}

/// Decode the sealed `pg_stat_user_indexes` section for the selected layout, then
/// check one snapshot timestamp, that the two seeded databases both contributed
/// the probe table's primary-key index, and that the label strings resolve
/// through the dictionary.
fn assert_user_indexes_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    let observations = match user_indexes_version(major) {
        UserIndexesVersion::V2 => decode_section_rows::<PgStatUserIndexesV2>(
            path,
            &segment,
            PG_STAT_USER_INDEXES_V2_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_014_002"))?
        .iter()
        .map(|r| UserIndexObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            indexrelname: r.indexrelname,
            amname: r.amname,
            indexdef: r.indexdef,
            indisprimary: r.indisprimary,
            indisexclusion: r.indisexclusion,
            indisready: r.indisready,
            main_fork_bytes: r.main_fork_bytes,
            idx_blks_read: r.idx_blks_read,
            idx_blks_hit: r.idx_blks_hit,
            last_idx_scan: r.last_idx_scan.map(|t| t.0),
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
        UserIndexesVersion::V1 => decode_section_rows::<PgStatUserIndexesV1>(
            path,
            &segment,
            PG_STAT_USER_INDEXES_V1_TYPE_ID,
        )
        .with_context(|| format!("postgres {major}: read back section 1_014_001"))?
        .iter()
        .map(|r| UserIndexObservation {
            datname: r.datname,
            schemaname: r.schemaname,
            indexrelname: r.indexrelname,
            amname: r.amname,
            indexdef: r.indexdef,
            indisprimary: r.indisprimary,
            indisexclusion: r.indisexclusion,
            indisready: r.indisready,
            main_fork_bytes: r.main_fork_bytes,
            idx_blks_read: r.idx_blks_read,
            idx_blks_hit: r.idx_blks_hit,
            last_idx_scan: None,
            ts: r.ts.0,
        })
        .collect::<Vec<_>>(),
    };
    check_user_indexes_rows(major, &dict, min_ts, max_ts, &observations)
}

/// Shared invariants over the decoded user-indexes rows.
fn check_user_indexes_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[UserIndexObservation],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_user_indexes section decoded to no rows"
    );

    for row in rows {
        ensure_ts_in_segment_range(major, "section 1_014", row.ts, min_ts, max_ts)?;
        // The buffer counters are COALESCE'd to 0 in SQL, so they always decode as
        // a non-negative i64 even when pg_statio has no row yet.
        anyhow::ensure!(
            row.idx_blks_read >= 0 && row.idx_blks_hit >= 0,
            "postgres {major}: an index buffer counter came back negative"
        );
        // last_idx_scan is a V2-only column; it must be absent below PG16.
        if major < 16 {
            anyhow::ensure!(
                row.last_idx_scan.is_none(),
                "postgres {major}: pre-PG16 row carries a last_idx_scan value"
            );
        }
    }

    // The probe table has a primary key, so both seeded databases must contribute
    // its pkey index. datname is the discriminator: the index name is identical.
    let mut seeded_with_pkey: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    // The long-expression index must also survive, with its (truncated) definition
    // resolving to a non-empty CREATE statement.
    let mut seeded_with_long_index: std::collections::HashSet<Vec<u8>> =
        std::collections::HashSet::new();
    for row in rows {
        let datname = resolve_string(major, dict, "datname", row.datname.0)?;
        let schemaname = resolve_string(major, dict, "schemaname", row.schemaname.0)?;
        let indexrelname = resolve_string(major, dict, "indexrelname", row.indexrelname.0)?;
        let amname = resolve_string(major, dict, "amname", row.amname.0)?;
        let indexdef = resolve_dictionary_bytes(major, dict, "indexdef", row.indexdef.0)?;
        anyhow::ensure!(
            !schemaname.is_empty(),
            "postgres {major}: schemaname resolved to an empty string"
        );
        let seeded = SEEDED_DATABASES
            .iter()
            .any(|name| name.as_bytes() == datname.as_slice());
        if indexrelname == b"kronika_ut_probe_pkey" && seeded {
            anyhow::ensure!(
                row.indisprimary,
                "postgres {major}: kronika_ut_probe_pkey is not flagged as a primary key"
            );
            // A plain primary key is a ready, non-exclusion index.
            anyhow::ensure!(
                !row.indisexclusion,
                "postgres {major}: a plain pkey is flagged as an exclusion index"
            );
            anyhow::ensure!(
                row.indisready,
                "postgres {major}: a live pkey is not flagged as ready"
            );
            anyhow::ensure!(
                amname == b"btree",
                "postgres {major}: kronika_ut_probe_pkey amname is not btree"
            );
            anyhow::ensure!(
                indexdef.windows(6).any(|w| w == b"CREATE"),
                "postgres {major}: kronika_ut_probe_pkey indexdef is not a CREATE statement"
            );
            // The probe table carries rows, so its pkey has real storage.
            anyhow::ensure!(
                row.main_fork_bytes > 0,
                "postgres {major}: kronika_ut_probe_pkey main_fork_bytes is not positive"
            );
            if major >= 16 {
                anyhow::ensure!(
                    row.last_idx_scan.is_some(),
                    "postgres {major}: kronika_ut_probe_pkey has no last_idx_scan after a forced pkey scan"
                );
            }
            seeded_with_pkey.insert(datname.clone());
        }
        if indexrelname == b"kronika_ut_probe_long_idx" && seeded {
            anyhow::ensure!(
                !indexdef.is_empty() && indexdef.windows(6).any(|w| w == b"CREATE"),
                "postgres {major}: kronika_ut_probe_long_idx indexdef did not survive truncation"
            );
            let indexdef_cap =
                usize::try_from(indexdef_max_len()).context("indexdef cap fits usize")?;
            anyhow::ensure!(
                indexdef.len() == indexdef_cap,
                "postgres {major}: kronika_ut_probe_long_idx indexdef len {} is not the cap {indexdef_cap}",
                indexdef.len()
            );
            if major >= 16 {
                anyhow::ensure!(
                    row.last_idx_scan.is_none(),
                    "postgres {major}: never-scanned kronika_ut_probe_long_idx has last_idx_scan"
                );
            }
            seeded_with_long_index.insert(datname);
        }
    }
    for datname in SEEDED_DATABASES {
        anyhow::ensure!(
            seeded_with_pkey.contains(datname.as_bytes()),
            "postgres {major}: no kronika_ut_probe_pkey row for database {datname}"
        );
        anyhow::ensure!(
            seeded_with_long_index.contains(datname.as_bytes()),
            "postgres {major}: no kronika_ut_probe_long_idx row for database {datname}"
        );
    }
    Ok(())
}

#[then(
    "each matrix cluster installs pg_stat_statements and seals rows with dictionary-backed query text"
)]
async fn every_version_seals_statements(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let version = install_statements_and_run_workload(db).await?;
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_statements_section(db.major(), version, &segment)?;
    }
    Ok(())
}

/// Install the extension, run a probe workload so the view has a row, and
/// return the layout the installed extension version maps to.
///
/// The test reads `pg_extension.extversion` instead of deriving the layout from
/// the server major.
async fn install_statements_and_run_workload(
    db: &cluster::Cluster,
) -> anyhow::Result<StatementsVersion> {
    let conn = db.connect().await?;
    conn.client()
        .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .await
        .with_context(|| format!("postgres {}: create pg_stat_statements", db.major()))?;
    // A distinctive statement the assertion can find in the sealed rows. Run it a
    // few times so it accrues calls and lands in both candidate axes.
    for _ in 0..3 {
        conn.client()
            .query("SELECT 42 AS kronika_pgss_probe", &[])
            .await
            .with_context(|| format!("postgres {}: run probe query", db.major()))?;
    }
    let extversion = statements_extversion(conn.client())
        .await
        .with_context(|| format!("postgres {}: read extension version", db.major()))?
        .with_context(|| {
            format!(
                "postgres {}: extension not installed after CREATE",
                db.major()
            )
        })?;
    Ok(statements_version(&extversion))
}

/// Decode the sealed `pg_stat_statements` section for the layout the extension
/// version selects, then check one snapshot timestamp, dictionary-backed query
/// text, and that the probe statement is present.
fn assert_statements_section(
    major: u32,
    version: StatementsVersion,
    path: &Path,
) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    // Project each layout to `(query, ts)`: query resolution and the probe check
    // are the same across versions.
    let observations = match version {
        StatementsVersion::V6 => {
            decode_section_rows::<PgStatStatementsV6>(path, &segment, PG_STAT_STATEMENTS_V6_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_006"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V5 => {
            decode_section_rows::<PgStatStatementsV5>(path, &segment, PG_STAT_STATEMENTS_V5_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_005"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V4 => {
            decode_section_rows::<PgStatStatementsV4>(path, &segment, PG_STAT_STATEMENTS_V4_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_004"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V3 => {
            decode_section_rows::<PgStatStatementsV3>(path, &segment, PG_STAT_STATEMENTS_V3_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_003"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V2 => {
            decode_section_rows::<PgStatStatementsV2>(path, &segment, PG_STAT_STATEMENTS_V2_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_002"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
        StatementsVersion::V1 => {
            decode_section_rows::<PgStatStatementsV1>(path, &segment, PG_STAT_STATEMENTS_V1_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_002_001"))?
                .iter()
                .map(|r| (r.query, r.ts.0))
                .collect::<Vec<_>>()
        }
    };
    check_statements_rows(major, &dict, min_ts, max_ts, &observations)
}

/// Shared invariants over the decoded `(query, ts)` projection: at least one row,
/// every ts in the segment range, every present query text resolves, and the
/// probe statement is among them.
fn check_statements_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[(Option<StrId>, i64)],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_statements section decoded to no rows"
    );

    let mut saw_probe = false;
    for (query, ts) in rows {
        ensure_ts_in_segment_range(major, "section 1_002", *ts, min_ts, max_ts)?;
        // The collector runs as a superuser, so query text is present, not NULL.
        let id = query.with_context(|| {
            format!("postgres {major}: a pg_stat_statements row has a NULL query text")
        })?;
        let text = resolve_string(major, dict, "query", id.0)?;
        if text
            .windows(b"kronika_pgss_probe".len())
            .any(|w| w == b"kronika_pgss_probe")
        {
            saw_probe = true;
        }
    }
    anyhow::ensure!(
        saw_probe,
        "postgres {major}: the probe statement was not found in the sealed rows"
    );
    Ok(())
}

/// Resolve a dictionary id to its bytes or fail with context.
fn resolve_string(major: u32, dict: &Dictionary, label: &str, id: u64) -> anyhow::Result<Vec<u8>> {
    match dict.resolve(id) {
        Some(Resolved::String(bytes)) => Ok(bytes.to_vec()),
        other => anyhow::bail!(
            "postgres {major}: {label} str_id {id} did not resolve to a string: {other:?}"
        ),
    }
}

/// Resolve a dictionary id stored either in `dict.strings` or `dict.blobs`.
fn resolve_dictionary_bytes(
    major: u32,
    dict: &Dictionary,
    label: &str,
    id: u64,
) -> anyhow::Result<Vec<u8>> {
    match dict.resolve(id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => Ok(bytes.to_vec()),
        other => anyhow::bail!(
            "postgres {major}: {label} str_id {id} did not resolve to bytes: {other:?}"
        ),
    }
}

#[then("each matrix cluster seals its replication instance status")]
async fn every_version_seals_replication_instance(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_replication_instance_section(db.major(), &segment)?;
    }
    Ok(())
}

#[then("each matrix cluster accepts optional pg_stat_progress_vacuum sections")]
async fn every_version_accepts_progress_vacuum(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_optional_progress_vacuum_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Decode the sealed instance-replication section: one row, ts in range, a
/// positive timeline, and the standalone-primary shape (not in recovery,
/// `current_wal_lsn` set, standby/receiver columns NULL). The standby shape is
/// covered by source and codec tests, since the matrix runs standalone primaries.
fn assert_replication_instance_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let rows =
        decode_section_rows::<ReplicationInstance>(path, &segment, PG_REPLICATION_INSTANCE_TYPE_ID)
            .with_context(|| format!("postgres {major}: read back section 1_015_001"))?;
    anyhow::ensure!(
        rows.len() == 1,
        "postgres {major}: expected one replication-instance row, got {}",
        rows.len()
    );
    let row = &rows[0];
    ensure_ts_in_segment_range(
        major,
        "section 1_015_001",
        row.ts.0,
        catalog.min_ts,
        catalog.max_ts,
    )?;
    anyhow::ensure!(
        row.timeline_id >= 1,
        "postgres {major}: timeline_id {} is not positive",
        row.timeline_id
    );
    anyhow::ensure!(
        !row.is_in_recovery,
        "postgres {major}: a standalone cluster reports being in recovery"
    );
    anyhow::ensure!(
        row.current_wal_lsn.is_some(),
        "postgres {major}: a primary reports no current_wal_lsn"
    );
    anyhow::ensure!(
        row.standby_receive_lsn.is_none()
            && row.standby_replay_lsn.is_none()
            && row.replay_lag_s.is_none()
            && row.standby_last_replay_at.is_none()
            && row.sender_host.is_none()
            && row.wal_receiver_status.is_none()
            && row.sender_port.is_none()
            && row.slot_name.is_none()
            && row.latest_end_lsn.is_none()
            && row.latest_end_time.is_none()
            && row.received_tli.is_none(),
        "postgres {major}: a primary must not fill standby receiver columns"
    );
    anyhow::ensure!(
        row.streaming_replicas == 0,
        "postgres {major}: a standalone cluster reports {} streaming replicas",
        row.streaming_replicas
    );
    // synchronous_standby_names defaults to an empty string, so resolution must
    // accept an empty value; only the dictionary lookup itself must succeed.
    for (label, id) in [
        ("synchronous_standby_names", row.synchronous_standby_names.0),
        ("synchronous_commit", row.synchronous_commit.0),
    ] {
        anyhow::ensure!(
            matches!(dict.resolve(id), Some(Resolved::String(_))),
            "postgres {major}: {label} str_id {id} did not resolve to a string"
        );
    }
    Ok(())
}

/// A missing progress-vacuum section means there were no active VACUUM rows.
fn assert_optional_progress_vacuum_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    if !has_section(&segment, PG_STAT_PROGRESS_VACUUM_TYPE_ID) {
        return Ok(());
    }

    let catalog = segment.catalog();
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let rows = decode_section_rows::<PgStatProgressVacuum>(
        path,
        &segment,
        PG_STAT_PROGRESS_VACUUM_TYPE_ID,
    )
    .with_context(|| format!("postgres {major}: read back section 1_012_001"))?;
    check_progress_vacuum_rows(
        major,
        &dict,
        catalog.min_ts,
        catalog.max_ts,
        rows.as_slice(),
    )
}

fn check_progress_vacuum_rows(
    major: u32,
    dict: &Dictionary,
    min_ts: i64,
    max_ts: i64,
    rows: &[PgStatProgressVacuum],
) -> anyhow::Result<()> {
    let first = rows
        .first()
        .with_context(|| format!("postgres {major}: progress-vacuum section decoded to no rows"))?;

    let ts = first.ts.0;
    anyhow::ensure!(
        rows.iter().all(|row| row.ts.0 == ts),
        "postgres {major}: progress-vacuum rows carry differing ts"
    );
    ensure_ts_in_segment_range(major, "section 1_012_001", ts, min_ts, max_ts)?;

    for row in rows {
        anyhow::ensure!(
            row.pid > 0,
            "postgres {major}: progress-vacuum pid {} is not positive",
            row.pid
        );
        anyhow::ensure!(
            row.datid > 0 && row.relid > 0,
            "postgres {major}: progress-vacuum row has datid {} and relid {}",
            row.datid,
            row.relid
        );
        for (label, id) in [("datname", row.datname.0), ("phase", row.phase.0)] {
            match dict.resolve(id) {
                Some(Resolved::String(bytes)) => anyhow::ensure!(
                    !bytes.is_empty(),
                    "postgres {major}: {label} resolved to an empty string"
                ),
                other => anyhow::bail!(
                    "postgres {major}: {label} str_id {id} did not resolve to a string: {other:?}"
                ),
            }
        }
        if major >= PG17_MAJOR {
            anyhow::ensure!(
                row.dead_tuple_bytes.is_some() && row.max_dead_tuples.is_none(),
                "postgres {major}: PG17+ row must use the byte-era dead-tuple columns"
            );
        } else {
            anyhow::ensure!(
                row.max_dead_tuples.is_some() && row.dead_tuple_bytes.is_none(),
                "postgres {major}: pre-PG17 row must use the count-era dead-tuple columns"
            );
        }
        anyhow::ensure!(
            row.delay_time.is_some() == (major >= PG18_MAJOR),
            "postgres {major}: delay_time presence must match PG18+"
        );
    }
    Ok(())
}

#[then("each matrix cluster seals a single-row pg_stat_wal section")]
async fn every_version_seals_wal(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_wal_section(db.major(), &segment)?;
    }
    Ok(())
}

#[then("each matrix cluster seals prepared pg_prepared_xacts rows")]
async fn every_version_seals_prepared(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let idle_segment = collector.snapshot().await?;
        assert_no_prepared_section(db.major(), &idle_segment)?;

        let gid = prepared_gid(db.major());
        prepare_transaction(db, &gid).await?;
        let assertion = async {
            let segment = collector.snapshot().await?;
            assert_prepared_section(db.major(), &segment, "postgres")
        }
        .await;
        let cleanup = rollback_prepared(db, &gid).await;
        if let Err(err) = cleanup {
            assertion?;
            return Err(err);
        }
        assertion?;
    }
    Ok(())
}

#[then("every version handles pg_stat_io per its layout, resolving labels through the dictionary")]
async fn every_version_handles_io(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_io_section(db.major(), &segment)?;
    }
    Ok(())
}

#[then("every version seals a single-row pg_stat_archiver section")]
async fn every_version_seals_archiver(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_archiver_section(db.major(), &segment)?;
    }
    Ok(())
}

/// Check selected layout, timestamp, singleton shape, reset, and counters.
fn assert_wal_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let min_ts = segment.catalog().min_ts;
    let max_ts = segment.catalog().max_ts;
    let has = |type_id: u32| {
        segment
            .catalog()
            .entries
            .iter()
            .any(|entry| entry.type_id == type_id)
    };
    match wal_version(major) {
        Some(WalVersion::V1) => {
            anyhow::ensure!(
                !has(PG_STAT_WAL_V2_TYPE_ID),
                "postgres {major}: PG14-17 sealed the PG18 wal layout 1_007_002"
            );
            let rows = decode_section_rows::<PgStatWalV1>(path, &segment, PG_STAT_WAL_V1_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_007_001"))?;
            let row = single_wal_row(major, rows)?;
            ensure_ts_in_segment_range(major, "section 1_007_001", row.ts.0, min_ts, max_ts)?;
            anyhow::ensure!(
                row.wal_records >= 0
                    && row.wal_fpi >= 0
                    && row.wal_bytes >= 0
                    && row.wal_buffers_full >= 0
                    && row.wal_write >= 0
                    && row.wal_sync >= 0,
                "postgres {major}: a pg_stat_wal counter came back negative"
            );
            anyhow::ensure!(
                row.wal_write_time.is_finite()
                    && row.wal_write_time >= 0.0
                    && row.wal_sync_time.is_finite()
                    && row.wal_sync_time >= 0.0,
                "postgres {major}: pg_stat_wal timing came back invalid"
            );
            check_wal_stats_reset(major, row.stats_reset.map(|ts| ts.0), row.ts.0)?;
        }
        Some(WalVersion::V2) => {
            anyhow::ensure!(
                !has(PG_STAT_WAL_V1_TYPE_ID),
                "postgres {major}: PG18 sealed the PG14-17 wal layout 1_007_001"
            );
            let rows = decode_section_rows::<PgStatWalV2>(path, &segment, PG_STAT_WAL_V2_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_007_002"))?;
            let row = single_wal_row(major, rows)?;
            ensure_ts_in_segment_range(major, "section 1_007_002", row.ts.0, min_ts, max_ts)?;
            anyhow::ensure!(
                row.wal_records >= 0
                    && row.wal_fpi >= 0
                    && row.wal_bytes >= 0
                    && row.wal_buffers_full >= 0,
                "postgres {major}: a pg_stat_wal counter came back negative"
            );
            check_wal_stats_reset(major, row.stats_reset.map(|ts| ts.0), row.ts.0)?;
        }
        None => {
            anyhow::ensure!(
                !has(PG_STAT_WAL_V1_TYPE_ID) && !has(PG_STAT_WAL_V2_TYPE_ID),
                "postgres {major}: pg_stat_wal section present before PG14"
            );
        }
    }
    Ok(())
}

/// A missing prepared-xacts section means there were no prepared transactions.
fn assert_no_prepared_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    anyhow::ensure!(
        !has_section(&segment, PG_PREPARED_XACTS_TYPE_ID),
        "postgres {major}: idle cluster unexpectedly sealed section 1_010_001"
    );
    Ok(())
}

/// Decode `pg_prepared_xacts` and check the test transaction's aggregate row.
fn assert_prepared_section(major: u32, path: &Path, want_datname: &str) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let rows = decode_section_rows::<PgPreparedXacts>(path, &segment, PG_PREPARED_XACTS_TYPE_ID)
        .with_context(|| format!("postgres {major}: read back section 1_010_001"))?;
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_prepared_xacts section decoded to no rows"
    );
    let ts = rows[0].ts.0;
    anyhow::ensure!(
        rows.iter().all(|row| row.ts.0 == ts),
        "postgres {major}: prepared-xacts rows carry differing ts"
    );
    ensure_ts_in_segment_range(
        major,
        "section 1_010_001",
        ts,
        catalog.min_ts,
        catalog.max_ts,
    )?;

    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    let mut found = 0;
    for row in &rows {
        let datname = match dict.resolve(row.datname.0) {
            Some(Resolved::String(bytes)) => bytes,
            other => anyhow::bail!(
                "postgres {major}: datname str_id {} did not resolve to a string: {other:?}",
                row.datname.0
            ),
        };
        if datname == want_datname.as_bytes() {
            found += 1;
            anyhow::ensure!(
                row.prepared_count == 1,
                "postgres {major}: prepared_count {}, expected 1",
                row.prepared_count
            );
            anyhow::ensure!(
                (0..300_000_000).contains(&row.max_age_us),
                "postgres {major}: max_age_us {} is not sane for a fresh prepared transaction",
                row.max_age_us
            );
            anyhow::ensure!(
                row.max_xid_age_tx >= 0,
                "postgres {major}: negative max_xid_age_tx {}",
                row.max_xid_age_tx
            );
        }
    }
    anyhow::ensure!(
        found == 1,
        "postgres {major}: expected one prepared-xacts row for {want_datname}, got {found}"
    );
    Ok(())
}

async fn prepare_transaction(db: &cluster::Cluster, gid: &str) -> anyhow::Result<()> {
    let conn = db.connect().await?;
    let gid = prepared_gid_literal(gid)?;
    conn.client()
        .batch_execute(&format!(
            "CREATE TABLE IF NOT EXISTS kronika_prepared_xacts_probe (id int); \
             BEGIN; \
             INSERT INTO kronika_prepared_xacts_probe VALUES ({}); \
             PREPARE TRANSACTION {gid};",
            db.major()
        ))
        .await
        .with_context(|| format!("postgres {}: prepare transaction", db.major()))
}

async fn rollback_prepared(db: &cluster::Cluster, gid: &str) -> anyhow::Result<()> {
    let conn = db.connect().await?;
    let gid = prepared_gid_literal(gid)?;
    conn.client()
        .batch_execute(&format!("ROLLBACK PREPARED {gid};"))
        .await
        .with_context(|| format!("postgres {}: rollback prepared transaction", db.major()))
}

fn prepared_gid(major: u32) -> String {
    format!("kronika_bdd_prepared_{major}_{}", std::process::id())
}

fn prepared_gid_literal(gid: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        gid.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "prepared transaction gid contains unsafe characters"
    );
    Ok(format!("'{gid}'"))
}

fn has_section(segment: &Segment, type_id: u32) -> bool {
    segment
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.type_id == type_id)
}

/// `stats_reset`, when present, must not be after the snapshot ts.
fn check_wal_stats_reset(major: u32, reset: Option<i64>, ts: i64) -> anyhow::Result<()> {
    if let Some(reset) = reset {
        anyhow::ensure!(
            reset <= ts,
            "postgres {major}: wal stats_reset {reset} is after snapshot ts {ts}"
        );
    }
    Ok(())
}

/// Read back the `pg_stat_io` section per the major's layout.
///
/// Before PG16 the view does not exist and neither layout may appear. On PG16-17
/// it is `1_009_001` (with `op_bytes`), on PG18 `1_009_002` (per-op byte
/// counters); only the version's own layout may be sealed. The check decodes the
/// rows, confirms one snapshot timestamp, the layout-specific columns, that any
/// `stats_reset` precedes the snapshot, and resolves the labels.
fn assert_io_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let has = |type_id: u32| {
        segment
            .catalog()
            .entries
            .iter()
            .any(|entry| entry.type_id == type_id)
    };
    let Some(version) = io_version(major) else {
        anyhow::ensure!(
            !has(PG_STAT_IO_V1_TYPE_ID) && !has(PG_STAT_IO_V2_TYPE_ID),
            "postgres {major}: pg_stat_io section present, but the view does not exist before PG16"
        );
        return Ok(());
    };
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    match version {
        IoVersion::V1 => {
            anyhow::ensure!(
                !has(PG_STAT_IO_V2_TYPE_ID),
                "postgres {major}: PG16-17 sealed the PG18 io layout 1_009_002"
            );
            let rows = decode_io_section::<PgStatIoV1>(path, &segment, PG_STAT_IO_V1_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_009_001"))?;
            anyhow::ensure!(
                rows.iter().any(|r| r.op_bytes.is_some()),
                "postgres {major}: V1 io rows carry no op_bytes"
            );
            check_io_stats_reset(
                major,
                rows.iter().map(|r| (r.stats_reset.map(|t| t.0), r.ts.0)),
            )?;
            check_io_rows(
                major,
                &dict,
                rows.iter()
                    .map(|r| (r.backend_type, r.object, r.context, r.ts.0)),
            )
        }
        IoVersion::V2 => {
            anyhow::ensure!(
                !has(PG_STAT_IO_V1_TYPE_ID),
                "postgres {major}: PG18 sealed the PG16-17 io layout 1_009_001"
            );
            let rows = decode_io_section::<PgStatIoV2>(path, &segment, PG_STAT_IO_V2_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_009_002"))?;
            anyhow::ensure!(
                rows.iter()
                    .any(|r| r.read_bytes.is_some() || r.write_bytes.is_some()),
                "postgres {major}: V2 io rows carry no byte counters"
            );
            check_io_stats_reset(
                major,
                rows.iter().map(|r| (r.stats_reset.map(|t| t.0), r.ts.0)),
            )?;
            check_io_rows(
                major,
                &dict,
                rows.iter()
                    .map(|r| (r.backend_type, r.object, r.context, r.ts.0)),
            )
        }
    }
}

/// Read the catalog-bounded section and decode its typed rows.
fn decode_io_section<T: Section>(
    path: &Path,
    segment: &Segment,
    type_id: u32,
) -> anyhow::Result<Vec<T>> {
    use std::os::unix::fs::FileExt;

    let entry = segment
        .catalog()
        .entries
        .iter()
        .find(|entry| entry.type_id == type_id)
        .with_context(|| format!("segment has no section {type_id}"))?;
    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;

    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    T::decode(verified).context("typed decode of the pg_stat_io section")
}

/// Shared invariants over the decoded `(backend_type, object, context, ts)`
/// projection: one snapshot ts, every label resolves, at least one relation row.
fn check_io_rows(
    major: u32,
    dict: &Dictionary,
    rows: impl Iterator<Item = (StrId, StrId, StrId, i64)>,
) -> anyhow::Result<()> {
    let rows: Vec<_> = rows.collect();
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_io section decoded to no rows"
    );
    let ts = rows[0].3;
    anyhow::ensure!(
        rows.iter().all(|row| row.3 == ts),
        "postgres {major}: snapshot rows carry differing ts"
    );

    let mut saw_relation = false;
    for (backend_type, object, context, _) in &rows {
        for (label, id) in [
            ("backend_type", backend_type),
            ("object", object),
            ("context", context),
        ] {
            match dict.resolve(id.0) {
                Some(Resolved::String(bytes)) => {
                    if label == "object" && bytes == b"relation".as_slice() {
                        saw_relation = true;
                    }
                }
                other => anyhow::bail!(
                    "postgres {major}: {label} str_id {} did not resolve to a string: {other:?}",
                    id.0
                ),
            }
        }
    }
    anyhow::ensure!(
        saw_relation,
        "postgres {major}: no pg_stat_io row for object=relation"
    );
    Ok(())
}

/// `stats_reset`, when present, must not be after the snapshot ts.
fn check_io_stats_reset(
    major: u32,
    rows: impl Iterator<Item = (Option<i64>, i64)>,
) -> anyhow::Result<()> {
    for (reset, ts) in rows {
        if let Some(reset) = reset {
            anyhow::ensure!(
                reset <= ts,
                "postgres {major}: io stats_reset {reset} is after snapshot ts {ts}"
            );
        }
    }
    Ok(())
}

/// Extract the one row a `pg_stat_wal` snapshot must hold.
fn single_wal_row<T>(major: u32, rows: Vec<T>) -> anyhow::Result<T> {
    let mut rows = rows.into_iter();
    let row = rows
        .next()
        .with_context(|| format!("postgres {major}: pg_stat_wal section decoded to no rows"))?;
    anyhow::ensure!(
        rows.next().is_none(),
        "postgres {major}: pg_stat_wal section decoded to multiple rows"
    );
    Ok(row)
}

/// Check singleton shape, counters, timestamp, and optional WAL-name dictionary ids.
fn assert_archiver_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let catalog = segment.catalog();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.type_id == PG_STAT_ARCHIVER_TYPE_ID)
        .with_context(|| format!("postgres {major}: segment has no section 1_008_001"))?;
    let rows = decode_archiver(path, entry)
        .with_context(|| format!("postgres {major}: read back section 1_008_001"))?;
    anyhow::ensure!(
        rows.len() == 1,
        "postgres {major}: pg_stat_archiver is a singleton, got {} rows",
        rows.len()
    );
    let row = rows[0];
    ensure_ts_in_segment_range(
        major,
        "section 1_008_001",
        row.ts.0,
        catalog.min_ts,
        catalog.max_ts,
    )?;
    anyhow::ensure!(
        row.archived_count >= 0 && row.failed_count >= 0,
        "postgres {major}: archiver counters came back negative"
    );
    if let Some(reset) = row.stats_reset {
        anyhow::ensure!(
            reset.0 <= row.ts.0,
            "postgres {major}: archiver stats_reset {} is after snapshot ts {}",
            reset.0,
            row.ts.0
        );
    }
    if row.last_archived_wal.is_some() || row.last_failed_wal.is_some() {
        let dict = segment
            .dictionary()
            .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
        resolve_archiver_wal(major, &dict, "last_archived_wal", row.last_archived_wal)?;
        resolve_archiver_wal(major, &dict, "last_failed_wal", row.last_failed_wal)?;
    }
    Ok(())
}

fn resolve_archiver_wal(
    major: u32,
    dict: &Dictionary,
    field: &str,
    wal: Option<StrId>,
) -> anyhow::Result<()> {
    if let Some(wal) = wal {
        anyhow::ensure!(
            matches!(dict.resolve(wal.0), Some(Resolved::String(_))),
            "postgres {major}: {field} str_id {} did not resolve through the dictionary",
            wal.0
        );
    }
    Ok(())
}

/// Read the catalog-bounded section and decode its typed rows.
fn decode_archiver(path: &Path, entry: &Entry) -> anyhow::Result<Vec<PgStatArchiver>> {
    use std::os::unix::fs::FileExt;

    let len = usize::try_from(entry.len).context("section len overflows usize")?;
    anyhow::ensure!(
        len <= MAX_SECTION_BYTES,
        "section of {len} bytes is above the {MAX_SECTION_BYTES}-byte cap"
    );
    let mut body = vec![0_u8; len];
    std::fs::File::open(path)?.read_exact_at(&mut body, entry.offset)?;
    let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
        .map_err(|err| anyhow::anyhow!("section crc check failed: {err}"))?;
    PgStatArchiver::decode(verified).context("typed decode of section 1_008_001")
}
#[then("each matrix cluster opens per-database pool connections")]
async fn every_cluster_opens_per_db_pool_connections(world: &mut BddWorld) -> anyhow::Result<()> {
    use kronika_source_pg::pool::{
        ConnectionPool, DEFAULT_MAX_DATABASES, SessionConfig, enumerate_databases,
    };
    use std::collections::HashSet;
    use std::time::Duration;
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    let session = SessionConfig {
        statement_timeout_ms: 15_000,
        lock_timeout_ms: 1_000,
        idle_in_tx_timeout_ms: 10_000,
    };
    for db in &world.clusters {
        let dsn = db.conn_string();
        let mut pool =
            ConnectionPool::connect(&dsn, "pg_kronika-bdd", session, HashSet::new()).await?;
        pool.refresh(Duration::from_secs(0), DEFAULT_MAX_DATABASES)
            .await?;
        anyhow::ensure!(
            !pool.per_db().is_empty(),
            "postgres {}: no per-db connections",
            db.major()
        );
        anyhow::ensure!(
            pool.uncovered().is_empty(),
            "postgres {}: databases without pool connection: {:?}",
            db.major(),
            pool.uncovered()
        );
        let names = enumerate_databases(pool.main(), &HashSet::new()).await?;
        anyhow::ensure!(
            !names.iter().any(|n| n == "template0" || n == "template1"),
            "postgres {}: template database was enumerated",
            db.major()
        );
    }
    Ok(())
}

/// How long to wait for the waiter's `UPDATE` to actually block on a lock.
const LOCK_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One decoded wait-tree row projected to the fields the assertion checks, so the
/// V1 and V2 layouts share a single check.
#[derive(Debug)]
struct LockWaitObservation {
    pid: i32,
    /// Deduped `pg_blocking_pids(pid)`; empty for roots.
    blocked_by: Vec<i32>,
    /// Whether the row carries an awaited lock (`lock_locktype` set).
    has_awaited_lock: bool,
}

#[when("session H holds a row lock and session W blocks on it")]
async fn open_blocking_pair(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    let mut waits = Vec::with_capacity(world.clusters.len());
    for db in &world.clusters {
        waits.push(start_blocking_pair(db).await?);
    }
    // Store only after every cluster is blocked, so a mid-loop failure does not
    // strand half-open transactions on the world.
    world.lock_waits = waits;
    Ok(())
}

/// Open a holder session on a fresh row lock, then a waiter whose conflicting
/// `UPDATE` blocks behind it. Returns once the waiter is confirmed waiting on a
/// lock, with both sessions still live.
async fn start_blocking_pair(db: &cluster::Cluster) -> anyhow::Result<LockWait> {
    let holder = db.connect().await?;
    holder
        .client()
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS kronika_lock_probe (id int primary key, v int); \
             INSERT INTO kronika_lock_probe VALUES (1, 0) ON CONFLICT (id) DO NOTHING;",
        )
        .await
        .with_context(|| format!("postgres {}: seed lock probe table", db.major()))?;
    // The transaction stays open (no COMMIT), so the row lock is held until the
    // holder connection is dropped.
    holder
        .client()
        .batch_execute("BEGIN; UPDATE kronika_lock_probe SET v = v + 1 WHERE id = 1;")
        .await
        .with_context(|| format!("postgres {}: hold the row lock", db.major()))?;

    // The waiter runs its conflicting UPDATE on its own connection inside the
    // task, so it stays blocked while we snapshot. The task owns the connection;
    // aborting it drops the connection and cancels the query.
    let waiter_conn = db.connect().await?;
    let waiter = tokio::spawn(async move {
        // The query blocks until the holder commits or the task is aborted; the
        // outcome is irrelevant, only that it holds the lock wait open.
        let _result = waiter_conn
            .client()
            .execute("UPDATE kronika_lock_probe SET v = v + 1 WHERE id = 1", &[])
            .await;
    });

    if let Err(err) = wait_until_lock_wait(&holder).await {
        // Nothing is stored yet, so tear the pair down before propagating.
        waiter.abort();
        return Err(err.context(format!("postgres {}: waiter never blocked", db.major())));
    }
    Ok(LockWait { holder, waiter })
}

/// Poll `pg_stat_activity` until at least one backend is waiting on a lock, or
/// fail after [`LOCK_WAIT_TIMEOUT`]. The view is cluster-wide, so any connection
/// observes the waiter.
async fn wait_until_lock_wait(conn: &cluster::Conn) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        let row = conn
            .client()
            .query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'",
                &[],
            )
            .await
            .context("count lock waiters")?;
        let waiting: i64 = row.get(0);
        if waiting > 0 {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "no backend entered a lock wait within {LOCK_WAIT_TIMEOUT:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[then("each matrix cluster seals a wait tree with W blocked by H")]
async fn assert_wait_tree(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    anyhow::ensure!(
        world.lock_waits.len() == world.clusters.len(),
        "the blocking step did not open a pair for every cluster"
    );
    // Snapshot every cluster while its blocking pair is still held, then release
    // the holders and waiters regardless of the assertion outcome.
    let mut result = Ok(());
    for db in &world.clusters {
        match collect_one(db).await {
            Ok(segment) => {
                if let Err(err) = assert_wait_tree_section(db.major(), &segment) {
                    result = Err(err);
                    break;
                }
            }
            Err(err) => {
                result = Err(err);
                break;
            }
        }
    }
    world.lock_waits.clear();
    result
}

/// Spawn the collector and take one snapshot.
async fn collect_one(db: &cluster::Cluster) -> anyhow::Result<std::path::PathBuf> {
    let mut collector = collector::Collector::spawn(db).await?;
    collector.snapshot().await
}

/// Decode the wait-tree section for the cluster's layout and check that a waiter
/// points at a blocker that is itself a root, and that the waiter has an awaited
/// lock. The matrix runs PG14+, so this normally reads the V2 layout; the V1
/// branch keeps the split explicit and is covered directly by a codec golden.
fn assert_wait_tree_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let observations = decode_locks_section(major, path, &segment)?;
    check_wait_tree(major, &observations)
}

/// Project the sealed wait-tree rows to [`LockWaitObservation`] for the layout
/// the major selects.
fn decode_locks_section(
    major: u32,
    path: &Path,
    segment: &Segment,
) -> anyhow::Result<Vec<LockWaitObservation>> {
    if major >= PG_LOCKS_V2_MIN_MAJOR {
        Ok(
            decode_section_rows::<PgLocksV2>(path, segment, PG_LOCKS_V2_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_011_002"))?
                .into_iter()
                .map(|r| LockWaitObservation {
                    pid: r.pid,
                    blocked_by: r.blocked_by,
                    has_awaited_lock: r.lock_locktype.is_some(),
                })
                .collect(),
        )
    } else {
        Ok(
            decode_section_rows::<PgLocksV1>(path, segment, PG_LOCKS_V1_TYPE_ID)
                .with_context(|| format!("postgres {major}: read back section 1_011_001"))?
                .into_iter()
                .map(|r| LockWaitObservation {
                    pid: r.pid,
                    blocked_by: r.blocked_by,
                    has_awaited_lock: r.lock_locktype.is_some(),
                })
                .collect(),
        )
    }
}

/// Shared invariants over the projected wait-tree rows: a waiter with a non-empty
/// `blocked_by`, its blocker present as a root, and the waiter awaiting a lock.
fn check_wait_tree(major: u32, rows: &[LockWaitObservation]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: wait-tree section decoded to no rows"
    );
    let waiter = rows
        .iter()
        .find(|r| !r.blocked_by.is_empty())
        .with_context(|| format!("postgres {major}: no waiter row with a blocked_by edge"))?;
    let blocker_pid = waiter.blocked_by[0];
    anyhow::ensure!(
        rows.iter()
            .any(|r| r.pid == blocker_pid && r.blocked_by.is_empty()),
        "postgres {major}: blocker pid {blocker_pid} is not present as a root"
    );
    anyhow::ensure!(
        waiter.has_awaited_lock,
        "postgres {major}: the waiter row carries no awaited lock",
    );
    Ok(())
}

#[then("no matrix cluster seals a pg_locks wait-tree section")]
async fn assert_no_wait_tree(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in &world.clusters {
        let segment_path = collect_one(db).await?;
        let segment = Segment::open(&segment_path)
            .with_context(|| format!("postgres {}: open sealed segment", db.major()))?;
        anyhow::ensure!(
            !has_section(&segment, PG_LOCKS_V1_TYPE_ID)
                && !has_section(&segment, PG_LOCKS_V2_TYPE_ID),
            "postgres {}: an idle cluster sealed a pg_locks wait-tree section",
            db.major()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let features = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    // On a failed scenario, dump each booted cluster's PostgreSQL server log so
    // CI shows the server-side cause (e.g. a rejected statement) instead of only
    // the opaque step panic. PostgreSQL logs errors regardless of DEBUG.
    BddWorld::cucumber()
        .max_concurrent_scenarios(MAX_CONCURRENT_SCENARIOS)
        .after(|_feature, _rule, _scenario, ev, world| {
            Box::pin(async move {
                if !matches!(
                    ev,
                    event::ScenarioFinished::StepFailed(..)
                        | event::ScenarioFinished::BeforeHookFailed(_)
                ) {
                    return;
                }
                if let event::ScenarioFinished::StepFailed(_, _, err) = ev {
                    eprintln!("=== BDD step failed: {err} ===");
                }
                if let Some(world) = world {
                    for cluster in &world.clusters {
                        eprintln!(
                            "=== postgres {} server.log ===\n{}\n=== end postgres {} server.log ===",
                            cluster.major(),
                            cluster.server_log(),
                            cluster.major(),
                        );
                    }
                }
            })
        })
        .run_and_exit(features)
        .await;
}

#[cfg(test)]
mod tests {
    use super::{LockWaitObservation, check_wait_tree, single_wal_row};

    #[test]
    fn single_wal_row_accepts_exactly_one() {
        assert_eq!(single_wal_row(15, vec![42_i32]).expect("one row"), 42);
    }

    #[test]
    fn single_wal_row_rejects_no_rows() {
        assert!(single_wal_row::<i32>(15, Vec::new()).is_err());
    }

    #[test]
    fn single_wal_row_rejects_multiple_rows() {
        assert!(single_wal_row(15, vec![1_i32, 2]).is_err());
    }

    fn obs(pid: i32, blocked_by: Vec<i32>, has_awaited_lock: bool) -> LockWaitObservation {
        LockWaitObservation {
            pid,
            blocked_by,
            has_awaited_lock,
        }
    }

    #[test]
    fn check_wait_tree_accepts_waiter_pointing_at_a_root() {
        let rows = [obs(100, vec![], false), obs(200, vec![100], true)];
        assert!(check_wait_tree(15, &rows).is_ok());
    }

    #[test]
    fn check_wait_tree_rejects_empty_section() {
        assert!(check_wait_tree(15, &[]).is_err());
    }

    #[test]
    fn check_wait_tree_rejects_no_waiter() {
        let rows = [obs(100, vec![], false), obs(200, vec![], false)];
        assert!(check_wait_tree(15, &rows).is_err());
    }

    #[test]
    fn check_wait_tree_rejects_blocker_missing_as_root() {
        // The waiter points at pid 100, but no row for 100 exists.
        let rows = [obs(200, vec![100], true)];
        assert!(check_wait_tree(15, &rows).is_err());
    }

    #[test]
    fn check_wait_tree_rejects_waiter_without_awaited_lock() {
        let rows = [obs(100, vec![], false), obs(200, vec![100], false)];
        assert!(check_wait_tree(15, &rows).is_err());
    }
}
