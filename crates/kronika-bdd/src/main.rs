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
use cucumber::{World, given, then};
use kronika_format::{Entry, crc32c};
use kronika_reader::{Dictionary, Resolved, Segment};
use kronika_registry::{
    Bytes, MAX_SECTION_BYTES, Section, StrId, VerifiedSection,
    bgwriter_checkpointer::BgwriterCheckpointer,
    pg_stat_activity::PgStatActivityV3,
    pg_stat_database::{PgStatDatabaseV3, PgStatDatabaseV4},
};
use kronika_source_pg::database::{DatabaseVersion, database_version};
use kronika_source_pg::{ActivityVersion, activity_version, collect_bgwriter_checkpointer};

const PG17_MAJOR: u32 = 17;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;

const PG_STAT_ACTIVITY_V3_TYPE_ID: u32 = 1_001_003;

const PG_STAT_DATABASE_V3_TYPE_ID: u32 = 1_005_003;
const PG_STAT_DATABASE_V4_TYPE_ID: u32 = 1_005_004;

/// Cucumber state for one scenario: the matrix booted by the `Given` step.
#[derive(Debug, Default, World)]
struct BddWorld {
    clusters: Vec<cluster::Cluster>,
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

#[then("each matrix cluster seals pg_stat_database rows with dictionary-backed names")]
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
/// check one snapshot timestamp, the shared row, and dictionary-backed database
/// names.
fn assert_database_section(major: u32, path: &Path) -> anyhow::Result<()> {
    let segment =
        Segment::open(path).with_context(|| format!("postgres {major}: open sealed segment"))?;
    let dict = segment
        .dictionary()
        .with_context(|| format!("postgres {major}: read the segment dictionary"))?;
    match database_version(major) {
        DatabaseVersion::V4 => {
            let rows =
                decode_db_section::<PgStatDatabaseV4>(path, &segment, PG_STAT_DATABASE_V4_TYPE_ID)
                    .with_context(|| format!("postgres {major}: read back section 1_005_004"))?;
            check_database_rows(
                major,
                &dict,
                "section 1_005_004",
                segment.catalog().min_ts,
                segment.catalog().max_ts,
                rows.iter()
                    .map(|r| (r.datid, r.datname, r.ts.0, r.frozen_xid_age)),
            )
        }
        DatabaseVersion::V3 => {
            let rows =
                decode_db_section::<PgStatDatabaseV3>(path, &segment, PG_STAT_DATABASE_V3_TYPE_ID)
                    .with_context(|| format!("postgres {major}: read back section 1_005_003"))?;
            check_database_rows(
                major,
                &dict,
                "section 1_005_003",
                segment.catalog().min_ts,
                segment.catalog().max_ts,
                rows.iter()
                    .map(|r| (r.datid, r.datname, r.ts.0, r.frozen_xid_age)),
            )
        }
        other => {
            anyhow::bail!("postgres {major}: matrix version maps to {other:?}, expected V3 or V4")
        }
    }
}

/// Read the catalog-bounded section and decode its typed rows.
fn decode_db_section<T: Section>(
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
    T::decode(verified).context("typed decode of the pg_stat_database section")
}

/// Shared invariants for the decoded `(datid, datname, ts)` projection.
fn check_database_rows(
    major: u32,
    dict: &Dictionary,
    section: &str,
    min_ts: i64,
    max_ts: i64,
    rows: impl Iterator<Item = (u32, Option<StrId>, i64, Option<i64>)>,
) -> anyhow::Result<()> {
    let rows: Vec<_> = rows.collect();
    anyhow::ensure!(
        !rows.is_empty(),
        "postgres {major}: pg_stat_database section decoded to no rows"
    );

    // One statement_timestamp() covers the whole snapshot.
    let ts = rows[0].2;
    anyhow::ensure!(
        rows.iter().all(|row| row.2 == ts),
        "postgres {major}: snapshot rows carry differing ts"
    );
    ensure_ts_in_segment_range(major, section, ts, min_ts, max_ts)?;

    // PG12+ adds a shared-objects row with `datid = 0` and no `datname`.
    let shared = rows
        .iter()
        .find(|row| row.0 == 0)
        .with_context(|| format!("postgres {major}: no datid=0 shared-objects row"))?;
    anyhow::ensure!(
        shared.1.is_none(),
        "postgres {major}: shared-objects row has a non-null datname"
    );
    anyhow::ensure!(
        shared.3.is_none(),
        "postgres {major}: shared-objects row has a non-null frozen_xid_age"
    );

    // A `datid != 0` row must resolve its `datname` through the dictionary.
    let real = rows
        .iter()
        .find(|row| row.0 != 0)
        .with_context(|| format!("postgres {major}: no datid != 0 database row"))?;
    let datname = real
        .1
        .with_context(|| format!("postgres {major}: datid != 0 row has a null datname"))?;
    match dict.resolve(datname.0) {
        Some(Resolved::String(bytes)) => anyhow::ensure!(
            !bytes.is_empty(),
            "postgres {major}: datname resolved to an empty string"
        ),
        other => anyhow::bail!(
            "postgres {major}: datname str_id {} did not resolve to a string: {other:?}",
            datname.0
        ),
    }
    anyhow::ensure!(
        real.3.is_some(),
        "postgres {major}: datid != 0 row has a null frozen_xid_age"
    );
    Ok(())
}

#[tokio::main]
async fn main() {
    let features = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    BddWorld::cucumber().run_and_exit(features).await;
}
