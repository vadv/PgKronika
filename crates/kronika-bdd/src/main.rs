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
use kronika_reader::Segment;
use kronika_registry::{
    Bytes, MAX_SECTION_BYTES, Section, VerifiedSection,
    bgwriter_checkpointer::BgwriterCheckpointer, reset_metadata::ResetMetadata,
};
use kronika_source_pg::collect_bgwriter_checkpointer;

const PG17_MAJOR: u32 = 17;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;
const RESET_METADATA_TYPE_ID: u32 = 1_020_001;

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
    let (min, max) = (catalog.min_ts, catalog.max_ts);

    // Verify the typed row, not only the catalog entry.
    let bg_entry = find_section(&catalog.entries, BGWRITER_CHECKPOINTER_TYPE_ID, major)?;
    let bgwriter: BgwriterCheckpointer = decode_sealed_row(path, bg_entry)
        .with_context(|| format!("postgres {major}: read back section 1_006_001"))?;
    anyhow::ensure!(
        min > 0 && min <= bgwriter.ts.0 && bgwriter.ts.0 <= max,
        "postgres {major}: bgwriter ts {} outside segment range {min}..={max}",
        bgwriter.ts.0
    );
    anyhow::ensure!(
        bgwriter.bgwriter_stats_reset.0 > 0 && bgwriter.bgwriter_stats_reset.0 <= bgwriter.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        bgwriter.bgwriter_stats_reset.0,
        bgwriter.ts.0
    );
    assert_version_columns(major, &bgwriter)?;

    // reset_metadata is mandatory for sealed PostgreSQL segments.
    let reset_entry = find_section(&catalog.entries, RESET_METADATA_TYPE_ID, major)?;
    let reset: ResetMetadata = decode_sealed_row(path, reset_entry)
        .with_context(|| format!("postgres {major}: read back section 1_020_001"))?;
    anyhow::ensure!(
        reset.postmaster_start_time.0 > 0
            && reset.pg_stat_database_reset_max_at.0 > 0
            && reset.pg_stat_archiver_reset_at.0 > 0,
        "postgres {major}: reset_metadata carries implausible timestamps"
    );
    anyhow::ensure!(
        min <= reset.ts.0 && reset.ts.0 <= max,
        "postgres {major}: reset_metadata ts {} outside segment range {min}..={max}",
        reset.ts.0
    );
    Ok(())
}

fn find_section(entries: &[Entry], type_id: u32, major: u32) -> anyhow::Result<&Entry> {
    entries
        .iter()
        .find(|entry| entry.type_id == type_id)
        .with_context(|| format!("postgres {major}: segment has no section {type_id}"))
}

/// Read a one-row section through the same CRC and typed decoder paths as readers.
fn decode_sealed_row<T: Section>(path: &Path, entry: &Entry) -> anyhow::Result<T> {
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
    let mut rows = T::decode(verified)
        .context("typed section decode")?
        .into_iter();
    let row = rows.next().context("section decoded to no rows")?;
    anyhow::ensure!(
        rows.next().is_none(),
        "section unexpectedly decoded to multiple rows"
    );
    Ok(row)
}

#[tokio::main]
async fn main() {
    let features = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    BddWorld::cucumber().run_and_exit(features).await;
}
