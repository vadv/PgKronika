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
    Bytes, MAX_SECTION_BYTES, Section, VerifiedSection, bgwriter_checkpointer::BgwriterCheckpointer,
};
use kronika_source_pg::collect_bgwriter_checkpointer;

const PG17_MAJOR: u32 = 17;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;

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
        let snapshot = collect_bgwriter_checkpointer(conn.client())
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

/// Plausibility of a freshly collected snapshot. `ts` now comes from the
/// server's `clock_timestamp()`, so we check it sits near the harness clock
/// (same host) instead of equalling a value we passed in.
fn check_snapshot(major: u32, host_now: i64, snap: &BgwriterCheckpointer) -> anyhow::Result<()> {
    anyhow::ensure!(
        snap.ts.0 > 0 && (snap.ts.0 - host_now).abs() < 300_000_000,
        "postgres {major}: snapshot ts {} is not within 5 min of the harness clock {host_now}",
        snap.ts.0
    );
    anyhow::ensure!(
        snap.checkpoints_timed >= 0 && snap.buffers_clean >= 0 && snap.buffers_alloc >= 0,
        "postgres {major}: a counter came back negative"
    );
    // The reset timestamp must predate the sampled row.
    anyhow::ensure!(
        snap.bgwriter_stats_reset.0 > 0 && snap.bgwriter_stats_reset.0 <= snap.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        snap.bgwriter_stats_reset.0,
        snap.ts.0
    );
    assert_version_columns(major, snap)
}

/// The version-specific column shape, asserted on both the live snapshot and the
/// row read back from a sealed segment: PG17+ fills the restartpoint and
/// checkpointer-reset columns and drops `buffers_backend`; earlier versions do
/// the reverse.
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

    // Decode the body typed, so the end-to-end check covers the values that
    // survived collect -> seal -> read, not just that a section is present.
    let row = decode_sealed_row(path, entry)
        .with_context(|| format!("postgres {major}: read back section 1_006_001"))?;

    // One signal -> one row, so the body's ts is the whole segment range.
    anyhow::ensure!(
        row.ts.0 > 0 && row.ts.0 == catalog.min_ts && row.ts.0 == catalog.max_ts,
        "postgres {major}: row ts {} disagrees with segment range {}..={}",
        row.ts.0,
        catalog.min_ts,
        catalog.max_ts
    );
    anyhow::ensure!(
        row.bgwriter_stats_reset.0 > 0 && row.bgwriter_stats_reset.0 <= row.ts.0,
        "postgres {major}: bgwriter_stats_reset {} not in (0, {}]",
        row.bgwriter_stats_reset.0,
        row.ts.0
    );
    assert_version_columns(major, &row)
}

/// Read just one section's bytes from the sealed segment, CRC-check them, and
/// decode the single typed row. Reads only the catalog-bounded section, never
/// the whole file.
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

#[tokio::main]
async fn main() {
    // Docker uses the Nix-store feature directory; local `cargo run` uses ./features.
    let features = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    BddWorld::cucumber().run_and_exit(features).await;
}
