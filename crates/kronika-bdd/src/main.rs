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
mod harness;
mod steps;

use std::path::Path;

use anyhow::Context;
use cucumber::{World, event, given, then};
use harness::HarnessState;
use kronika_format::{Entry, crc32c};
use kronika_reader::{Dictionary, Resolved, Segment};
use kronika_registry::{
    Bytes, MAX_SECTION_BYTES, Section, VerifiedSection,
    bgwriter_checkpointer::BgwriterCheckpointer,
    pg_stat_wal::{PgStatWalV1, PgStatWalV2},
};
use kronika_source_pg::collect_bgwriter_checkpointer;

use kronika_source_pg::wal::{WalVersion, wal_version};

const PG17_MAJOR: u32 = 17;

const BGWRITER_CHECKPOINTER_TYPE_ID: u32 = 1_006_001;

const PG_STAT_WAL_V1_TYPE_ID: u32 = 1_007_001;
const PG_STAT_WAL_V2_TYPE_ID: u32 = 1_007_002;

/// Cucumber state for one scenario.
///
/// `clusters` borrows the process-wide matrix (booted once, not per scenario).
/// `harness` holds the new-style per-scenario state — named sessions, the
/// selected database, and the last snapshot — and is torn down in the `after`
/// hook.
#[derive(Debug, Default, World)]
struct BddWorld {
    clusters: &'static [cluster::Cluster],
    harness: HarnessState,
}

#[given("the PostgreSQL matrix is booted")]
async fn boot_matrix_step(world: &mut BddWorld) -> anyhow::Result<()> {
    world.clusters = cluster::shared_matrix().await?;
    Ok(())
}

#[then("every version reports valid bgwriter/checkpointer stats")]
async fn every_version_reports_stats(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
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
    for db in world.clusters {
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

/// Resolve a dictionary id to its bytes or fail with context.
///
/// Query text that exceeds the blob threshold is classified as `Resolved::Blob`
/// rather than `Resolved::String`. Both variants carry the stored bytes.
fn resolve_string(major: u32, dict: &Dictionary, label: &str, id: u64) -> anyhow::Result<Vec<u8>> {
    match dict.resolve(id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => Ok(bytes.to_vec()),
        other => anyhow::bail!(
            "postgres {major}: {label} str_id {id} did not resolve to a string: {other:?}"
        ),
    }
}

#[then("each matrix cluster seals a single-row pg_stat_wal section")]
async fn every_version_seals_wal(world: &mut BddWorld) -> anyhow::Result<()> {
    anyhow::ensure!(!world.clusters.is_empty(), "no clusters were booted");
    for db in world.clusters {
        let mut collector = collector::Collector::spawn(db).await?;
        let segment = collector.snapshot().await?;
        assert_wal_section(db.major(), &segment)?;
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

#[tokio::main]
async fn main() {
    let features = std::env::var("KRONIKA_FEATURES").unwrap_or_else(|_| "features".to_owned());
    // Every scenario: tear down its harness state (held transactions, blocking
    // tasks, temp databases). On failure: dump each booted cluster's PostgreSQL
    // server log so CI shows the server-side cause (e.g. a rejected statement)
    // instead of only the opaque step panic. PostgreSQL logs errors regardless
    // of DEBUG.
    BddWorld::cucumber()
        .fail_on_skipped()
        .after(|_feature, _rule, _scenario, ev, world| {
            Box::pin(async move {
                let failed = matches!(
                    ev,
                    event::ScenarioFinished::StepFailed(..)
                        | event::ScenarioFinished::BeforeHookFailed(_)
                );
                if let event::ScenarioFinished::StepFailed(_, _, err) = ev {
                    eprintln!("=== BDD step failed: {err} ===");
                }
                if let Some(world) = world {
                    if failed {
                        for cluster in world.clusters {
                            eprintln!(
                                "=== postgres {} server.log ===\n{}\n=== end postgres {} server.log ===",
                                cluster.major(),
                                cluster.server_log(),
                                cluster.major(),
                            );
                        }
                    }
                    world.harness.cleanup().await;
                }
            })
        })
        .run_and_exit(features)
        .await;
}

#[cfg(test)]
mod tests {
    use super::single_wal_row;

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

    #[test]
    fn resolve_string_accepts_blob_entry() {
        use super::resolve_string;
        use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
        use kronika_reader::{Resolved, Segment};

        // A blob_threshold of 4 forces any value longer than 3 bytes into
        // dict.blobs. "hello" (5 bytes) lands there.
        let limits = DictLimits::new(4, 1 << 20).expect("limits");
        let mut interner = kronika_writer::Interner::new(limits);
        let id = interner.intern_blob(b"hello").expect("intern blob");

        let dict_sections = kronika_writer::dict::encode(interner.window()).expect("encode");
        let section_inputs: Vec<_> = dict_sections
            .iter()
            .map(|s| SectionInput {
                type_id: s.type_id,
                rows: s.rows,
                body: &s.body,
            })
            .collect();
        let bytes = build_part(
            &section_inputs,
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.pgm");
        std::fs::write(&path, &bytes).expect("write segment");

        let segment = Segment::open(&path).expect("open segment");
        let dict = segment.dictionary().expect("read dictionary");

        assert_eq!(
            dict.resolve(id.get()),
            Some(Resolved::Blob {
                bytes: b"hello",
                full_len: 5,
                truncated: false
            }),
            "entry is classified as Blob"
        );
        let result = resolve_string(15, &dict, "query", id.get());
        assert_eq!(result.expect("resolve_string accepts Blob"), b"hello");
    }
}
