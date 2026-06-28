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
    bgwriter_checkpointer::{Bgwriter, BgwriterCheckpointer},
    reset_metadata::{ResetMetadata, ResetMetadataIo},
};
use kronika_source_pg::{collect_bgwriter, collect_checkpointer};

const PG16_MAJOR: u32 = 16;
const PG17_MAJOR: u32 = 17;

/// Background-writer family `type_id` for `major`: PG17 split the catalog into
/// its own schema, so each major writes a distinct type.
const fn bgwriter_type_id(major: u32) -> u32 {
    if major >= PG17_MAJOR {
        1_006_002
    } else {
        1_006_001
    }
}

/// Reset-context family `type_id` for `major`: PG16 added `pg_stat_io`.
const fn reset_type_id(major: u32) -> u32 {
    if major >= PG16_MAJOR {
        1_020_002
    } else {
        1_020_001
    }
}

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
        // The major picks the exact collector and type_id, not a branch inside
        // one merged type.
        let host_now = now_micros()?;
        if major >= PG17_MAJOR {
            let snap = collect_checkpointer(conn.client())
                .await
                .with_context(|| format!("collect type 1_006_002 on postgres {major}"))?;
            check_checkpointer(major, host_now, &snap)?;
        } else {
            let snap = collect_bgwriter(conn.client())
                .await
                .with_context(|| format!("collect type 1_006_001 on postgres {major}"))?;
            check_bgwriter(major, host_now, &snap)?;
        }
    }
    Ok(())
}

fn now_micros() -> anyhow::Result<i64> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the unix epoch")?;
    i64::try_from(since_epoch.as_micros()).context("unix microseconds overflow i64")
}

/// A directly-collected `ts` must be positive and within 5 minutes of the runner
/// clock (server and runner share the container clock).
fn check_collection_ts(major: u32, host_now: i64, ts: i64) -> anyhow::Result<()> {
    anyhow::ensure!(
        ts > 0 && (ts - host_now).abs() < 300_000_000,
        "postgres {major}: snapshot ts {ts} is not within 5 min of the runner clock {host_now}"
    );
    Ok(())
}

/// Invariants for a `1_006_001` row (`pg_stat_bgwriter`, PG 15–16).
fn check_bgwriter(major: u32, host_now: i64, snap: &Bgwriter) -> anyhow::Result<()> {
    check_collection_ts(major, host_now, snap.ts.0)?;
    anyhow::ensure!(
        snap.checkpoints_timed >= 0 && snap.buffers_clean >= 0 && snap.buffers_alloc >= 0,
        "postgres {major}: a counter came back negative"
    );
    anyhow::ensure!(
        snap.stats_reset.0 > 0 && snap.stats_reset.0 <= snap.ts.0,
        "postgres {major}: stats_reset {} not in (0, {}]",
        snap.stats_reset.0,
        snap.ts.0
    );
    Ok(())
}

/// Invariants for a `1_006_002` row (`pg_stat_checkpointer` + `pg_stat_bgwriter`,
/// PG 17+).
fn check_checkpointer(
    major: u32,
    host_now: i64,
    snap: &BgwriterCheckpointer,
) -> anyhow::Result<()> {
    check_collection_ts(major, host_now, snap.ts.0)?;
    anyhow::ensure!(
        snap.num_timed >= 0 && snap.restartpoints_done >= 0 && snap.buffers_alloc >= 0,
        "postgres {major}: a counter came back negative"
    );
    anyhow::ensure!(
        snap.checkpointer_stats_reset.0 > 0 && snap.checkpointer_stats_reset.0 <= snap.ts.0,
        "postgres {major}: checkpointer_stats_reset {} not in (0, {}]",
        snap.checkpointer_stats_reset.0,
        snap.ts.0
    );
    Ok(())
}

#[then("every version is collected into a sealed segment with its version's sections")]
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

    // Background-writer family: the exact type_id for this major must be present,
    // and its typed row reads back through the reader's CRC + decode path.
    let bg_id = bgwriter_type_id(major);
    let bg_entry = find_section(&catalog.entries, bg_id, major)?;
    let bg_ts = if major >= PG17_MAJOR {
        let row: BgwriterCheckpointer = decode_sealed_row(path, bg_entry)
            .with_context(|| format!("postgres {major}: read back section {bg_id}"))?;
        ensure_reset_before(
            major,
            "checkpointer_stats_reset",
            row.checkpointer_stats_reset.0,
            row.ts.0,
        )?;
        row.ts.0
    } else {
        let row: Bgwriter = decode_sealed_row(path, bg_entry)
            .with_context(|| format!("postgres {major}: read back section {bg_id}"))?;
        ensure_reset_before(major, "stats_reset", row.stats_reset.0, row.ts.0)?;
        row.ts.0
    };
    ensure_ts_in_range(major, "bgwriter", bg_ts, min, max)?;

    // Reset-context family: mandatory in every segment, exact type_id per major.
    let reset_id = reset_type_id(major);
    let reset_entry = find_section(&catalog.entries, reset_id, major)?;
    let reset_ts = if major >= PG16_MAJOR {
        let row: ResetMetadataIo = decode_sealed_row(path, reset_entry)
            .with_context(|| format!("postgres {major}: read back section {reset_id}"))?;
        ensure_reset_metadata(
            major,
            row.postmaster_start_time.0,
            row.pg_stat_archiver_reset_at.0,
        )?;
        anyhow::ensure!(
            row.pg_stat_io_reset_at.0 > 0,
            "postgres {major}: pg_stat_io_reset_at is not set"
        );
        row.ts.0
    } else {
        let row: ResetMetadata = decode_sealed_row(path, reset_entry)
            .with_context(|| format!("postgres {major}: read back section {reset_id}"))?;
        ensure_reset_metadata(
            major,
            row.postmaster_start_time.0,
            row.pg_stat_archiver_reset_at.0,
        )?;
        row.ts.0
    };
    ensure_ts_in_range(major, "reset_metadata", reset_ts, min, max)?;
    Ok(())
}

/// A reset timestamp must be positive and not in the future relative to `ts`.
fn ensure_reset_before(major: u32, field: &str, reset: i64, ts: i64) -> anyhow::Result<()> {
    anyhow::ensure!(
        reset > 0 && reset <= ts,
        "postgres {major}: {field} {reset} not in (0, {ts}]"
    );
    Ok(())
}

/// A section's `ts` must fall inside the sealed segment's time range.
fn ensure_ts_in_range(major: u32, what: &str, ts: i64, min: i64, max: i64) -> anyhow::Result<()> {
    anyhow::ensure!(
        min > 0 && min <= ts && ts <= max,
        "postgres {major}: {what} ts {ts} outside segment range {min}..={max}"
    );
    Ok(())
}

/// Required `reset_metadata` timestamps that both versions carry.
fn ensure_reset_metadata(major: u32, postmaster: i64, archiver_reset: i64) -> anyhow::Result<()> {
    anyhow::ensure!(
        postmaster > 0 && archiver_reset > 0,
        "postgres {major}: reset_metadata carries implausible timestamps"
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
