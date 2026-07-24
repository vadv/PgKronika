use std::fs::{self, FileTimes};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::time::{Duration, UNIX_EPOCH};

use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
use kronika_registry::Section;
use kronika_registry::Ts;
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_registry::pg_log::PgLogLifecycleV1;

use super::*;
use crate::{FactOrigin, LIMIT};

/// Build one minimal valid part with a real section.
fn make_part(min_ts: i64, max_ts: i64, source_id: u64) -> Vec<u8> {
    let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
    build_part(
        &[SectionInput {
            type_id: 1_006_001,
            rows: 0,
            body: &body,
        }],
        PartMeta {
            min_ts,
            max_ts,
            source_id,
        },
    )
}

/// Wrap `part_bytes` in a journal frame.
fn framed(part_bytes: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(
        &FrameHeader {
            part_len: part_bytes.len() as u64,
        }
        .encode(),
    );
    buf.extend_from_slice(part_bytes);
    buf
}

fn lifecycle_part(source_id: u64) -> Vec<u8> {
    let rows = [PgLogLifecycleV1 {
        ts: Ts(1_500),
        kind: 0,
        pid: Some(42),
        signal: Some(9),
        shutdown_mode: None,
        message: None,
        query_detail: None,
        dict_dropped_fields: 0,
    }];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle");
    build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: 1_500,
            max_ts: 1_500,
            source_id,
        },
    )
}

fn fact_context() -> SegmentContext {
    SegmentContext::new(
        b"snapshot-store".to_vec(),
        NamingContractId([0x11; 16]),
        SegmentLocator([0x22; 32]),
    )
    .expect("valid context")
}

fn descriptor_context(descriptor: &SegmentDescriptor) -> SegmentContext {
    SegmentContext::new(
        b"snapshot-store".to_vec(),
        NamingContractId([0x11; 16]),
        SegmentLocator(*descriptor.locator.as_bytes()),
    )
    .expect("valid descriptor context")
}

#[test]
fn sealed_snapshot_cache_hit_after_reopen_reads_no_pgm_bodies() {
    let source = tempfile::tempdir().expect("source directory");
    let cache = tempfile::tempdir().expect("cache directory");
    fs::write(source.path().join("1500.pgm"), lifecycle_part(7)).expect("write segment");
    let store = FactStore::new(cache.path());

    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    let cold = snapshot
        .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
        .expect("cold facts");
    assert_eq!(cold.origin(), FactOrigin::Rebuilt);
    assert_eq!(cold.pgm_body_read_stats().read_calls, 1);

    let restarted = LocalDirSnapshot::open(source.path()).expect("restart snapshot");
    let warm = restarted
        .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
        .expect("warm facts");
    assert_eq!(warm.origin(), FactOrigin::CacheHit);
    assert_eq!(warm.pgm_body_read_stats().read_calls, 0);
    assert_eq!(warm.facts().observations(), cold.facts().observations());
}

#[test]
fn exact_sealed_descriptors_keep_identical_files_distinct_and_warm() {
    let source = tempfile::tempdir().expect("source directory");
    let cache = tempfile::tempdir().expect("cache directory");
    let bytes = lifecycle_part(7);
    fs::write(source.path().join("1500-a.pgm"), &bytes).expect("write first segment");
    fs::write(source.path().join("1500-b.pgm"), &bytes).expect("write second segment");
    let store = FactStore::new(cache.path());

    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    let descriptors = snapshot.sealed_descriptors();
    assert_eq!(descriptors.len(), 2);
    assert_ne!(descriptors[0].locator, descriptors[1].locator);
    assert_eq!(descriptors[0].catalog_digest, descriptors[1].catalog_digest);
    for descriptor in descriptors {
        let load = snapshot
            .load_sealed_facts_by_descriptor(
                descriptor,
                &store,
                &descriptor_context(descriptor),
                &LIMIT,
            )
            .expect("cold exact load");
        assert_eq!(load.origin(), FactOrigin::Rebuilt);
    }

    let restarted = LocalDirSnapshot::open(source.path()).expect("restart snapshot");
    for descriptor in restarted.sealed_descriptors() {
        let load = restarted
            .load_sealed_facts_by_descriptor(
                descriptor,
                &FactStore::new(cache.path()),
                &descriptor_context(descriptor),
                &LIMIT,
            )
            .expect("warm exact load");
        assert_eq!(load.origin(), FactOrigin::CacheHit);
        assert_eq!(load.pgm_body_read_stats().read_calls, 0);
    }
}

#[test]
fn exact_active_part_open_is_independent_of_query_unit_deduplication() {
    let source = tempfile::tempdir().expect("source directory");
    let bytes = lifecycle_part(7);
    fs::write(source.path().join("1500.pgm"), &bytes).expect("write sealed segment");
    fs::write(source.path().join("active.parts"), framed(&bytes)).expect("write active part");

    let mut snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    assert_eq!(snapshot.units().len(), 1, "query view suppresses duplicate");
    let delta = snapshot
        .refresh_incremental_delta()
        .expect("bootstrap exact descriptors");
    let descriptor = delta
        .journal
        .completed_parts
        .first()
        .expect("active descriptor");
    let active = snapshot
        .open_active_part(descriptor)
        .expect("open exact active part");
    assert_eq!(active.catalog().source_id, 7);
    assert_eq!(
        active.catalog(),
        snapshot.unit_catalog(0).expect("sealed catalog")
    );
}

#[test]
fn exact_sealed_load_rejects_a_context_for_another_locator() {
    let source = tempfile::tempdir().expect("source directory");
    let cache = tempfile::tempdir().expect("cache directory");
    fs::write(source.path().join("1500.pgm"), lifecycle_part(7)).expect("write segment");
    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    let descriptor = snapshot.sealed_descriptors()[0];

    assert!(matches!(
        snapshot.load_sealed_facts_by_descriptor(
            &descriptor,
            &FactStore::new(cache.path()),
            &fact_context(),
            &LIMIT,
        ),
        Err(SealedFactError::ContextLocatorMismatch { locator })
            if locator == descriptor.locator
    ));
}

#[test]
fn same_name_replacement_invalidates_pinned_snapshot() {
    let source = tempfile::tempdir().expect("source directory");
    let cache = tempfile::tempdir().expect("cache directory");
    let path = source.path().join("1500.pgm");
    fs::write(&path, lifecycle_part(7)).expect("write first segment");
    let pinned = LocalDirSnapshot::open(source.path()).expect("open pinned snapshot");
    let store = FactStore::new(cache.path());
    pinned
        .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
        .expect("first facts");

    fs::write(&path, lifecycle_part(8)).expect("replace segment");
    assert!(matches!(
        pinned.load_sealed_facts(0, &store, &fact_context(), &LIMIT),
        Err(SealedFactError::StaleSnapshot { unit_idx: 0 })
    ));

    let refreshed = LocalDirSnapshot::open(source.path()).expect("refresh snapshot");
    let replacement = refreshed
        .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
        .expect("replacement facts");
    assert_eq!(replacement.facts().identity().pgm_source_id, 8);
    assert_eq!(replacement.origin(), FactOrigin::Rebuilt);
}

#[test]
fn removed_source_is_not_resurrected_by_an_orphan_fact_file() {
    let source = tempfile::tempdir().expect("source directory");
    let cache = tempfile::tempdir().expect("cache directory");
    let path = source.path().join("1500.pgm");
    fs::write(&path, lifecycle_part(7)).expect("write segment");
    let store = FactStore::new(cache.path());
    LocalDirSnapshot::open(source.path())
        .expect("open snapshot")
        .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
        .expect("build facts");
    fs::remove_file(path).expect("remove authoritative segment");

    let after_retention = LocalDirSnapshot::open(source.path()).expect("rescan source");
    assert!(after_retention.units().is_empty());
    assert!(matches!(
        after_retention.load_sealed_facts(0, &store, &fact_context(), &LIMIT),
        Err(SealedFactError::UnitOutOfRange { unit_idx: 0 })
    ));
    let orphan_exists = walk_files(cache.path())
        .iter()
        .any(|path| path.extension().and_then(|value| value.to_str()) == Some("ovf"));
    assert!(
        orphan_exists,
        "source retention does not remove disposable cache files"
    );
}

#[test]
fn active_part_is_rejected_by_sealed_fact_loader() {
    let source = tempfile::tempdir().expect("source directory");
    let cache = tempfile::tempdir().expect("cache directory");
    fs::write(
        source.path().join("active.parts"),
        framed(&lifecycle_part(7)),
    )
    .expect("write active part");
    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    assert!(matches!(
        snapshot.load_sealed_facts(0, &FactStore::new(cache.path()), &fact_context(), &LIMIT),
        Err(SealedFactError::LiveUnit { unit_idx: 0 })
    ));
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("walk cache") {
            let path = entry.expect("cache entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn live_part_is_visible_before_seal() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 1);
    let journal: Vec<u8> = framed(&part);
    fs::write(dir.path().join("active.parts"), &journal).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(units.len(), 1, "one live part expected");
    assert!(units[0].live, "part must be marked live");
    assert_eq!(units[0].source_id, 1);
    assert_eq!(units[0].min_ts, 1000);
    assert_eq!(units[0].max_ts, 2000);
}

#[test]
fn exact_sealed_active_catalog_is_deduped_no_double() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 42);
    // Write the same data as a sealed .pgm.
    fs::write(dir.path().join("1000.pgm"), &part).unwrap();
    // And as the exact same active part.
    let journal = framed(&part);
    fs::write(dir.path().join("active.parts"), &journal).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(units.len(), 1, "exact active duplicate must be deduped");
    assert!(!units[0].live, "surviving unit must be the sealed one");
    assert_eq!(units[0].source_id, 42);
}

#[test]
fn overlapping_active_part_is_not_deduped_by_range_only() {
    let dir = tempfile::tempdir().unwrap();
    let sealed = make_part(1000, 5000, 42);
    let active = make_part(2000, 3000, 42);
    fs::write(dir.path().join("1000.pgm"), &sealed).unwrap();
    fs::write(dir.path().join("active.parts"), framed(&active)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(
        units.len(),
        2,
        "range overlap must not hide a distinct live part"
    );
    assert!(
        units
            .iter()
            .any(|u| !u.live && u.min_ts == 1000 && u.max_ts == 5000),
        "sealed unit must remain visible"
    );
    assert!(
        units
            .iter()
            .any(|u| u.live && u.min_ts == 2000 && u.max_ts == 3000),
        "overlapping live unit must remain visible"
    );
}

#[test]
fn refresh_picks_up_appended_part() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal = framed(&part1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Append a second part.
    let part2 = make_part(3000, 4000, 1);
    let mut journal_buf = fs::read(&journal_path).unwrap();
    journal_buf.extend_from_slice(&framed(&part2));
    fs::write(&journal_path, &journal_buf).unwrap();

    snap.refresh().unwrap();
    assert_eq!(snap.units().len(), 2, "refresh must surface the new part");
}

#[test]
fn refresh_incremental_surfaces_appended_part_and_keeps_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    let journal = framed(&part1);
    fs::write(&journal_path, &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    let first_offset = snap.scan.active[0].part.offset;
    let valid_before = snap.last_valid_len;
    assert_eq!(valid_before, journal.len() as u64);

    // Append a second part.
    let part2 = make_part(3000, 4000, 1);
    let mut buf = fs::read(&journal_path).unwrap();
    let appended = framed(&part2);
    buf.extend_from_slice(&appended);
    fs::write(&journal_path, &buf).unwrap();

    snap.refresh_incremental().unwrap();
    assert_eq!(
        snap.units().len(),
        2,
        "incremental refresh surfaces the new part"
    );
    assert_eq!(
        snap.scan.active[0].part.offset, first_offset,
        "the first part is carried over, not re-scanned"
    );
    assert_eq!(
        snap.last_valid_len,
        valid_before + appended.len() as u64,
        "valid_len advances by exactly the appended frame"
    );
}

#[test]
fn refresh_incremental_noop_when_journal_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let first_offset = snap.scan.active[0].part.offset;
    let valid_before = snap.last_valid_len;

    snap.refresh_incremental().unwrap();

    assert_eq!(
        snap.units().len(),
        1,
        "unchanged journal keeps its one unit"
    );
    assert_eq!(
        snap.scan.active[0].part.offset, first_offset,
        "the part is carried unchanged, the journal body is not re-read"
    );
    assert_eq!(
        snap.last_valid_len, valid_before,
        "valid_len is unchanged on a noop refresh"
    );
}

#[test]
fn refresh_incremental_reset_when_journal_truncated_to_zero() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Truncate-in-place to zero (a reset).
    fs::write(&journal_path, b"").unwrap();

    snap.refresh_incremental().unwrap();
    assert!(snap.units().is_empty(), "reset clears the live parts");
    assert_eq!(snap.last_valid_len, 0, "valid_len resets to zero");
}

#[test]
fn refresh_incremental_torn_tail_holds_then_completes() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    let journal = framed(&part1);
    fs::write(&journal_path, &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let valid_before = snap.last_valid_len;

    // Append a truncated (incomplete) second frame.
    let part2 = make_part(3000, 4000, 1);
    let full = framed(&part2);
    let mut buf = journal.clone();
    buf.extend_from_slice(&full[..full.len() - 3]);
    fs::write(&journal_path, &buf).unwrap();

    snap.refresh_incremental().unwrap();
    assert_eq!(snap.units().len(), 1, "torn tail must not surface a part");
    assert_eq!(
        snap.last_valid_len, valid_before,
        "valid_len does not move past the last complete frame"
    );

    // Complete the frame; the next incremental refresh sees it.
    let mut done = journal;
    done.extend_from_slice(&full);
    fs::write(&journal_path, &done).unwrap();

    snap.refresh_incremental().unwrap();
    assert_eq!(
        snap.units().len(),
        2,
        "completed frame is surfaced next tick"
    );
    assert_eq!(snap.last_valid_len, done.len() as u64);
}

#[test]
fn refresh_incremental_discovers_new_sealed_segment() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(2000, 3000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(snap.units()[0].live);

    // A new sealed segment appears in the directory.
    let sealed = make_part(500, 1000, 1);
    fs::write(dir.path().join("0500.pgm"), &sealed).unwrap();

    snap.refresh_incremental().unwrap();
    let units = snap.units();
    assert_eq!(units.len(), 2, "new sealed segment is discovered");
    assert!(
        units.iter().any(|u| !u.live && u.min_ts == 500),
        "the sealed unit is visible"
    );
    assert!(
        units.iter().any(|u| u.live && u.min_ts == 2000),
        "the live part remains visible"
    );
}

#[test]
fn middle_corruption_reported_through_snapshot_api_rest_served() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let part2 = make_part(3000, 4000, 1);
    let mut journal = framed(&part1);
    journal.extend_from_slice(b"GARBAGE_BYTES_HERE_THAT_ARE_NOT_A_VALID_FRAME");
    journal.extend_from_slice(&framed(&part2));
    fs::write(dir.path().join("active.parts"), &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(units.len(), 2, "both valid parts must be visible");
    assert!(
        !snap.damages().is_empty(),
        "corrupt region must be recorded as a damage"
    );
    let damage = snap.damages().to_vec();
    snap.refresh_incremental().expect("incremental refresh");
    assert_eq!(
        snap.damages(),
        damage,
        "a tail-only refresh retains earlier middle damage"
    );
}

#[test]
fn decode_unit_sealed_happy_path() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    fs::write(dir.path().join("1000.pgm"), &part).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(!snap.units()[0].live);

    let catalog = snap.unit_catalog(0).expect("catalog for unit 0");
    assert!(!catalog.entries.is_empty());

    let decoded = snap.decode_unit(0, 0).expect("decode sealed unit");
    assert_eq!(decoded.stats.type_id, 1_006_001);
}

#[test]
fn decode_unit_active_happy_path() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    let journal = framed(&part);
    fs::write(dir.path().join("active.parts"), &journal).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(snap.units()[0].live);

    let catalog = snap.unit_catalog(0).expect("catalog for unit 0");
    assert!(!catalog.entries.is_empty());

    let decoded = snap.decode_unit(0, 0).expect("decode active unit");
    assert_eq!(decoded.stats.type_id, 1_006_001);
}

#[test]
fn decode_unit_out_of_range_unit_idx() {
    let dir = tempfile::tempdir().unwrap();
    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let err = snap.decode_unit(99, 0).unwrap_err();
    assert!(
        matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput),
        "out-of-range unit index must return InvalidInput"
    );
}

#[test]
fn decode_unit_out_of_range_entry_idx() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    fs::write(dir.path().join("1000.pgm"), &part).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    // Entry 0 exists, entry 99 does not.
    let err = snap.decode_unit(0, 99).unwrap_err();
    assert!(
        matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput),
        "out-of-range entry index must return InvalidInput"
    );
}

#[test]
fn decode_unit_stale_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let part_a = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part_a)).unwrap();

    // Snapshot taken while part_a is live.
    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Replace the journal with a different part (different timestamps).
    let part_b = make_part(5000, 6000, 2);
    fs::write(&journal_path, framed(&part_b)).unwrap();

    // The cached offset now maps to bytes belonging to a different part.
    let err = snap.decode_unit(0, 0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "replaced journal must trigger StaleSnapshot, got: {err}"
    );
}

#[test]
fn missing_active_parts_is_empty_live_journal() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    // Write only a sealed file — no active.parts.
    fs::write(dir.path().join("1000.pgm"), &part).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    // The sealed unit is present; no live parts (active.parts absent).
    assert_eq!(units.len(), 1);
    assert!(!units[0].live);
    assert!(snap.scan.active.is_empty());
    assert!(snap.damages().is_empty());
}

// ---- decode_unit_rows / unit_dictionary tests ----

use kronika_format::DictLimits;
use kronika_registry::Cell;
use kronika_registry::StrId;
use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_writer::Interner;
use kronika_writer::dict;

/// Build a part with one `pg_stat_archiver` row (carrying a `StrId`) and
/// the corresponding `dict.strings` section. Returns the part bytes and the
/// interned `str_id` for the WAL file name.
fn make_archiver_part_with_dict(min_ts: i64, max_ts: i64, source_id: u64) -> (Vec<u8>, u64) {
    let mut interner = Interner::new(DictLimits::new(256, 1 << 20).expect("limits"));
    let wal_id = interner
        .intern(b"000000010000000000000001")
        .expect("intern");

    let archiver_body = PgStatArchiver::encode(&[PgStatArchiver {
        ts: Ts(min_ts),
        archived_count: 5,
        last_archived_wal: Some(StrId(wal_id.get())),
        last_archived_time: Some(Ts(min_ts - 1000)),
        failed_count: 0,
        last_failed_wal: None,
        last_failed_time: None,
        stats_reset: None,
    }])
    .expect("encode archiver");

    let dict_sections = dict::encode(interner.window()).expect("encode dict");
    // Collect owned bodies so all SectionInput borrows can point to them.
    let dict_owned: Vec<(u32, u32, Vec<u8>)> = dict_sections
        .into_iter()
        .map(|s| (s.type_id, s.rows, s.body))
        .collect();

    let mut all: Vec<SectionInput<'_>> = vec![SectionInput {
        type_id: 1_008_001,
        rows: 1,
        body: &archiver_body,
    }];
    for (type_id, rows, body) in &dict_owned {
        all.push(SectionInput {
            type_id: *type_id,
            rows: *rows,
            body,
        });
    }

    let bytes = build_part(
        &all,
        PartMeta {
            min_ts,
            max_ts,
            source_id,
        },
    );
    (bytes, wal_id.get())
}

#[test]
fn decode_unit_rows_sealed_and_active_match() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, _wal_id) = make_archiver_part_with_dict(1000, 2000, 9);

    // Write as sealed.
    fs::write(dir.path().join("1000.pgm"), &part_bytes).unwrap();
    let sealed_snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(!sealed_snap.units()[0].live);
    // pg_stat_archiver is entry 0 (first non-dict section, but dict sections
    // come after data sections in our fixture, so entry 0 is archiver).
    let catalog = sealed_snap.unit_catalog(0).expect("catalog");
    let archiver_entry_idx = catalog
        .entries
        .iter()
        .position(|e| e.type_id == 1_008_001)
        .expect("archiver entry");
    let sealed_rows = sealed_snap
        .decode_unit_rows(0, archiver_entry_idx)
        .expect("decode sealed rows");

    // Write same bytes as active part.
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part_bytes)).unwrap();
    let active_snap = LocalDirSnapshot::open(dir.path()).unwrap();
    // The active part is deduped by the sealed unit, so only 1 unit total.
    // The sealed unit is at index 0. Write only active (remove sealed).
    fs::remove_file(dir.path().join("1000.pgm")).unwrap();
    let active_snap2 = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(active_snap2.units()[0].live);
    let catalog2 = active_snap2.unit_catalog(0).expect("catalog");
    let archiver_entry_idx2 = catalog2
        .entries
        .iter()
        .position(|e| e.type_id == 1_008_001)
        .expect("archiver entry");
    let active_rows = active_snap2
        .decode_unit_rows(0, archiver_entry_idx2)
        .expect("decode active rows");

    assert_eq!(
        sealed_rows, active_rows,
        "sealed and active paths yield identical named-cell rows"
    );
    assert_eq!(sealed_rows.len(), 1, "one row decoded");
    assert_eq!(
        sealed_rows[0].get("archived_count"),
        Some(&Cell::I64(5)),
        "archived_count cell"
    );
    // last_archived_wal carries a StrId.
    assert!(
        matches!(
            sealed_rows[0].get("last_archived_wal"),
            Some(&Cell::StrId(_))
        ),
        "last_archived_wal is a StrId cell"
    );
    // Suppress the active_snap binding unused warning.
    drop(active_snap);
}

#[test]
fn unit_dictionary_resolves_interned_str_id() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
    fs::write(dir.path().join("1000.pgm"), &part_bytes).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let dict = snap.unit_dictionary(0).expect("unit dictionary");
    assert!(!dict.is_empty(), "at least one entry in the dictionary");
    let resolved = dict.resolve(wal_id).expect("wal_id is in the dictionary");
    assert_eq!(
        resolved,
        crate::Resolved::String(b"000000010000000000000001"),
        "str_id resolves to the interned WAL name"
    );
}

#[test]
fn unit_dictionary_active_resolves_str_id() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part_bytes)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(snap.units()[0].live);
    let dict = snap.unit_dictionary(0).expect("unit dictionary for active");
    let resolved = dict.resolve(wal_id).expect("wal_id resolved");
    assert_eq!(
        resolved,
        crate::Resolved::String(b"000000010000000000000001"),
    );
}

#[test]
fn decode_unit_rows_stale_after_journal_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, _) = make_archiver_part_with_dict(1000, 2000, 9);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part_bytes)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(snap.units()[0].live);
    let archiver_entry_idx = snap
        .unit_catalog(0)
        .expect("catalog")
        .entries
        .iter()
        .position(|e| e.type_id == 1_008_001)
        .expect("archiver entry");

    fs::remove_file(&journal_path).unwrap();

    let err = snap.decode_unit_rows(0, archiver_entry_idx).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "removed journal must return StaleSnapshot for decode_unit_rows, got: {err}"
    );
}

#[test]
fn unit_dictionary_stale_after_journal_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, _) = make_archiver_part_with_dict(1000, 2000, 9);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part_bytes)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(snap.units()[0].live);

    fs::remove_file(&journal_path).unwrap();

    let err = snap.unit_dictionary(0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "removed journal must return StaleSnapshot for unit_dictionary, got: {err}"
    );
}

// When active.parts disappears (removed or truncated to zero) between
// snapshot time and decode_unit time, read_active_part returns NotFound or
// UnexpectedEof. decode_unit must map that to StaleSnapshot, not ReadError::Io.
#[test]
fn decode_unit_active_truncated_after_snapshot_returns_stale_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(snap.units()[0].live);

    // Remove the journal file to simulate post-seal reset.
    fs::remove_file(&journal_path).unwrap();

    let err = snap.decode_unit(0, 0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "removed journal must return StaleSnapshot, got: {err}"
    );
}

#[test]
fn decode_unit_active_zero_truncated_after_snapshot_returns_stale_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(snap.units()[0].live);

    fs::write(&journal_path, b"").unwrap();

    let err = snap.decode_unit(0, 0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "truncated journal must return StaleSnapshot, got: {err}"
    );
}

// ---- open_unit / OpenUnit tests ----

#[test]
fn open_unit_sealed_decodes_rows_and_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
    fs::write(dir.path().join("1000.pgm"), &part_bytes).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let unit = snap.open_unit(0).expect("open sealed unit");
    assert!(matches!(unit, OpenUnit::Sealed(_)));
    assert_eq!(unit.catalog().source_id, 9);

    let archiver = unit
        .catalog()
        .entries
        .iter()
        .find(|e| e.type_id == 1_008_001)
        .expect("archiver entry");
    let rows = unit.decode_rows(archiver).expect("decode rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("archived_count"), Some(&Cell::I64(5)));

    let dict = unit.dictionary().expect("dictionary");
    assert_eq!(
        dict.resolve(wal_id).expect("resolve"),
        crate::Resolved::String(b"000000010000000000000001")
    );
}

#[test]
fn open_unit_active_decodes_rows_and_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
    fs::write(dir.path().join("active.parts"), framed(&part_bytes)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(snap.units()[0].live);
    let unit = snap.open_unit(0).expect("open active unit");
    assert!(matches!(unit, OpenUnit::Active(_)));
    assert_eq!(unit.catalog().source_id, 9);

    let archiver = unit
        .catalog()
        .entries
        .iter()
        .find(|e| e.type_id == 1_008_001)
        .expect("archiver entry");
    let rows = unit.decode_rows(archiver).expect("decode rows");
    assert_eq!(rows.len(), 1);

    let dict = unit.dictionary().expect("dictionary");
    assert_eq!(
        dict.resolve(wal_id).expect("resolve"),
        crate::Resolved::String(b"000000010000000000000001")
    );
}

#[test]
fn open_unit_out_of_range_is_invalid_input() {
    let dir = tempfile::tempdir().unwrap();
    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let err = snap.open_unit(99).unwrap_err();
    assert!(
        matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput),
        "out-of-range unit index must return InvalidInput"
    );
}

#[test]
fn open_unit_active_stale_after_journal_removed() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(snap.units()[0].live);

    fs::remove_file(&journal_path).unwrap();

    let err = snap.open_unit(0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "removed journal must return StaleSnapshot, got: {err}"
    );
}

#[test]
fn open_unit_active_stale_when_journal_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let part_a = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&part_a)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Replace with a different part so the cached catalog no longer matches.
    let part_b = make_part(5000, 6000, 2);
    fs::write(&journal_path, framed(&part_b)).unwrap();

    let err = snap.open_unit(0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "replaced journal must trigger StaleSnapshot, got: {err}"
    );
}

#[test]
fn open_unit_increments_the_test_counter() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    fs::write(dir.path().join("1000.pgm"), &part).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    OPEN_UNIT_CALLS.with(|c| c.set(0));
    drop(snap.open_unit(0).expect("open"));
    drop(snap.open_unit(0).expect("open"));
    assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 2);
}

#[test]
fn refresh_delta_reports_appended_part_as_completed_under_the_same_generation() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&make_part(1000, 2000, 1))).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let generation_before = snap.journal_generation();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert!(initial.journal.bootstrap);
    assert_eq!(initial.journal.completed_parts.len(), 1);
    assert_eq!(
        initial.journal.current_parts,
        initial.journal.completed_parts
    );
    assert!(initial.journal.current_parts_complete);
    assert_eq!(initial.journal.completed_parts[0].min_ts, 1000);

    let appended = framed(&make_part(3000, 4000, 1));
    let mut buf = fs::read(&journal_path).unwrap();
    buf.extend_from_slice(&appended);
    fs::write(&journal_path, &buf).unwrap();

    let delta = snap.refresh_incremental_delta().expect("delta");
    assert!(!delta.journal.bootstrap);
    assert_eq!(delta.journal.transition, PartTransition::Append);
    assert_eq!(delta.journal.generation_id, generation_before);
    assert_eq!(
        delta.journal.completed_parts.len(),
        1,
        "only the newly appended part is completed"
    );
    assert_eq!(
        delta.journal.current_parts.len(),
        2,
        "completion evidence includes the old and appended parts"
    );
    assert_eq!(delta.journal.current_parts[0].min_ts, 1000);
    assert_eq!(delta.journal.current_parts[1].min_ts, 3000);
    let final_part = delta.journal.current_parts.last().expect("final part");
    assert_eq!(
        final_part.part_id.frame_offset + final_part.part_id.body_len,
        delta.journal.new_valid_len,
        "the complete descriptor set reaches the validated watermark"
    );
    assert!(delta.journal.current_parts_complete);
    assert_eq!(delta.journal.completed_parts[0].min_ts, 3000);
    assert!(delta.new_view_generation > delta.previous_view_generation);
    assert!(!delta.requires_live_rebuild());
}

#[test]
fn refresh_delta_redelivery_is_idempotent_and_reports_no_new_parts() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&make_part(1000, 2000, 1))).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert_eq!(initial.journal.completed_parts.len(), 1);
    let mut buf = fs::read(&journal_path).unwrap();
    buf.extend_from_slice(&framed(&make_part(3000, 4000, 1)));
    fs::write(&journal_path, &buf).unwrap();

    let first = snap.refresh_incremental_delta().expect("first delta");
    assert_eq!(first.journal.completed_parts.len(), 1);
    let generation = first.journal.generation_id;

    let second = snap.refresh_incremental_delta().expect("second delta");
    assert!(
        second.journal.completed_parts.is_empty(),
        "an unchanged journal re-delivers no parts"
    );
    assert_eq!(
        second.journal.current_parts.len(),
        2,
        "completion evidence remains whole on a no-op refresh"
    );
    assert_eq!(second.journal.generation_id, generation);
    assert_eq!(second.new_view_generation, first.new_view_generation);
}

#[test]
fn refresh_delta_truncation_resets_and_mints_a_new_generation() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, framed(&make_part(1000, 2000, 1))).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let generation_before = snap.journal_generation();

    fs::write(&journal_path, b"").unwrap();

    let delta = snap.refresh_incremental_delta().expect("delta");
    assert_eq!(delta.journal.transition, PartTransition::Reset);
    assert_ne!(delta.journal.generation_id, generation_before);
    assert_eq!(delta.journal.new_valid_len, 0);
    assert!(delta.journal.completed_parts.is_empty());
    assert!(delta.journal.current_parts.is_empty());
    assert!(delta.journal.current_parts_complete);
    assert!(delta.requires_live_rebuild());
}

#[test]
fn refresh_delta_torn_tail_reports_pending_bytes_and_holds_the_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let base = framed(&make_part(1000, 2000, 1));
    fs::write(&journal_path, &base).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let valid_before = snap.last_valid_len;
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert_eq!(initial.journal.completed_parts.len(), 1);

    let full = framed(&make_part(3000, 4000, 1));
    let mut buf = base;
    buf.extend_from_slice(&full[..full.len() - 3]);
    let torn_len = buf.len() as u64;
    fs::write(&journal_path, &buf).unwrap();

    let delta = snap.refresh_incremental_delta().expect("delta");
    assert_eq!(delta.journal.transition, PartTransition::Append);
    assert!(
        delta.journal.completed_parts.is_empty(),
        "a torn frame surfaces no completed part"
    );
    assert_eq!(
        delta.journal.current_parts.len(),
        1,
        "the whole validated prefix remains the completion target"
    );
    assert!(delta.journal.current_parts_complete);
    assert_eq!(delta.journal.new_valid_len, valid_before, "watermark holds");
    assert_eq!(
        delta.journal.tail_pending,
        Some(ByteRange {
            start: valid_before,
            end: torn_len,
        })
    );
}

#[test]
fn refresh_delta_reports_a_newly_sealed_segment() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("active.parts"),
        framed(&make_part(2000, 3000, 1)),
    )
    .unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert_eq!(initial.journal.completed_parts.len(), 1);
    fs::write(dir.path().join("0500.pgm"), make_part(500, 1000, 1)).unwrap();

    let delta = snap.refresh_incremental_delta().expect("delta");
    assert_eq!(delta.sealed_added.len(), 1);
    assert_eq!(delta.sealed_added[0].min_ts, 500);
    assert!(delta.sealed_removed.is_empty());
}

#[test]
fn first_delta_delivers_parts_found_during_open() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("active.parts"),
        framed(&make_part(1000, 2000, 1)),
    )
    .unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let first = snap.refresh_incremental_delta().expect("first delta");
    let second = snap.refresh_incremental_delta().expect("second delta");

    assert_eq!(first.journal.completed_parts.len(), 1);
    assert_eq!(first.journal.completed_parts[0].min_ts, 1000);
    assert!(second.journal.completed_parts.is_empty());
    assert_eq!(second.new_view_generation, first.new_view_generation);
}

#[test]
fn equal_length_rewrite_discards_the_cached_active_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first_part = framed(&make_part(1000, 2000, 1));
    let replacement = framed(&make_part(3000, 4000, 1));
    assert_eq!(first_part.len(), replacement.len());
    fs::write(&journal_path, first_part).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    let initial_generation = initial.journal.generation_id;

    fs::write(&journal_path, replacement).unwrap();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&journal_path)
        .unwrap();
    file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();

    let delta = snap.refresh_incremental_delta().expect("replacement delta");
    assert_eq!(delta.journal.transition, PartTransition::Uncertain);
    assert_ne!(delta.journal.generation_id, initial_generation);
    assert_eq!(delta.journal.completed_parts.len(), 1);
    assert_eq!(delta.journal.completed_parts[0].min_ts, 3000);
    assert_eq!(snap.units()[0].min_ts, 3000);
}

#[test]
fn same_inode_growth_with_a_rewritten_prefix_forces_a_full_generation() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first = framed(&make_part(1000, 2000, 1));
    fs::write(&journal_path, &first).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    let initial_generation = initial.journal.generation_id;
    let initial_inode = fs::metadata(&journal_path).unwrap().ino();

    let replacement = framed(&make_part(3000, 4000, 1));
    assert_eq!(first.len(), replacement.len());
    let mut rewritten_and_grown = replacement;
    rewritten_and_grown.extend_from_slice(&framed(&make_part(5000, 6000, 1)));
    fs::write(&journal_path, rewritten_and_grown).unwrap();
    assert_eq!(fs::metadata(&journal_path).unwrap().ino(), initial_inode);

    let delta = snap.refresh_incremental_delta().expect("growth delta");
    assert_eq!(delta.journal.transition, PartTransition::Uncertain);
    assert_ne!(delta.journal.generation_id, initial_generation);
    assert_eq!(delta.journal.completed_parts.len(), 2);
    assert_eq!(delta.journal.current_parts, delta.journal.completed_parts);
    assert_eq!(delta.journal.current_parts[0].min_ts, 3000);
    assert_eq!(delta.journal.current_parts[1].min_ts, 5000);
}

#[test]
fn an_incomplete_active_baseline_forces_full_descriptor_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let valid_part = make_part(1000, 2000, 1);
    fs::write(&journal_path, framed(&valid_part)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    snap.scan.active.clear();
    snap.scan.warnings.push(StoreWarning {
        path: journal_path,
        reason: "simulated scan race omitted the validated prefix".to_owned(),
    });
    snap.journal_descriptors_complete = false;
    snap.delta_initialized = true;
    assert!(!snap.journal_descriptors_complete);
    assert!(snap.units().is_empty());

    let recovered = snap
        .refresh_incremental_delta()
        .expect("descriptor recovery");
    assert_eq!(recovered.journal.transition, PartTransition::Uncertain);
    assert!(recovered.journal.current_parts_complete);
    assert_eq!(recovered.journal.completed_parts.len(), 1);
    assert_eq!(
        recovered.journal.current_parts,
        recovered.journal.completed_parts
    );
    assert!(snap.warnings().is_empty());
}

#[test]
fn unreadable_sealed_file_is_not_removed_until_absence_is_authoritative() {
    let dir = tempfile::tempdir().unwrap();
    let sealed_path = dir.path().join("1000.pgm");
    let valid_segment = make_part(1000, 2000, 1);
    fs::write(&sealed_path, &valid_segment).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    snap.refresh_incremental_delta().expect("initial delta");

    fs::write(&sealed_path, b"not a pgm segment").unwrap();
    let unavailable = snap
        .refresh_incremental_delta()
        .expect("warning-bearing delta");
    assert!(unavailable.view_changed);
    assert!(unavailable.sealed_removed.is_empty());
    assert!(
        snap.warnings()
            .iter()
            .any(|warning| warning.path == sealed_path)
    );
    assert!(
        unavailable.new_view_generation > unavailable.previous_view_generation,
        "warning-visible raw state advances the view generation"
    );

    let repeated_unavailable = snap
        .refresh_incremental_delta()
        .expect("repeated warning-bearing delta");
    assert!(!repeated_unavailable.view_changed);
    assert_eq!(
        repeated_unavailable.new_view_generation,
        repeated_unavailable.previous_view_generation
    );
    assert!(repeated_unavailable.sealed_added.is_empty());
    assert!(repeated_unavailable.sealed_removed.is_empty());

    fs::write(&sealed_path, &valid_segment).unwrap();
    let recovered = snap.refresh_incremental_delta().expect("readable recovery");
    assert!(recovered.view_changed);
    assert!(recovered.sealed_added.is_empty());
    assert!(recovered.sealed_removed.is_empty());
    assert!(recovered.new_view_generation > recovered.previous_view_generation);
    assert!(snap.warnings().is_empty());

    fs::write(&sealed_path, b"not a pgm segment").unwrap();
    let unavailable_again = snap
        .refresh_incremental_delta()
        .expect("second warning-bearing delta");
    assert!(unavailable_again.sealed_removed.is_empty());

    fs::remove_file(&sealed_path).unwrap();
    let absent = snap
        .refresh_incremental_delta()
        .expect("authoritative absence");
    assert_eq!(
        absent.sealed_removed.len(),
        1,
        "the preserved descriptor is removed exactly once"
    );
    let repeated = snap.refresh_incremental_delta().expect("repeated absence");
    assert!(repeated.sealed_removed.is_empty());
}

#[test]
fn same_name_sealed_replacement_reports_remove_and_add() {
    let dir = tempfile::tempdir().unwrap();
    let sealed_path = dir.path().join("1000.pgm");
    fs::write(&sealed_path, make_part(1000, 2000, 1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    snap.refresh_incremental_delta().expect("initial delta");
    fs::write(&sealed_path, make_part(3000, 4000, 1)).unwrap();

    let delta = snap.refresh_incremental_delta().expect("replacement delta");
    assert_eq!(delta.sealed_removed.len(), 1);
    assert_eq!(delta.sealed_added.len(), 1);
    assert_eq!(
        delta.sealed_removed[0].locator, delta.sealed_added[0].locator,
        "the stable file-name locator connects replacement identities"
    );
    assert_ne!(
        delta.sealed_removed[0].catalog_digest,
        delta.sealed_added[0].catalog_digest
    );
}

#[test]
fn root_warning_suppresses_removal_but_journal_warning_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = PgmUnit::open(make_part(1000, 2000, 1).as_slice())
        .expect("unit")
        .catalog()
        .clone();
    let previous =
        SegmentDescriptor::from_catalog(SealedLocator::from_file_name_bytes(b"1000.pgm"), &catalog);
    let root_warning = LocalScan {
        sealed: Vec::new(),
        active: Vec::new(),
        damages: Vec::new(),
        warnings: vec![StoreWarning {
            path: dir.path().to_path_buf(),
            reason: "read_dir entry unavailable".to_owned(),
        }],
        valid_len: 0,
    };
    let preserved = sealed_delta(&root_warning, &[previous], dir.path()).expect("delta");
    assert!(preserved.removed.is_empty());
    assert_eq!(preserved.baseline, vec![previous]);

    let replacement_catalog = PgmUnit::open(make_part(3000, 4000, 1).as_slice())
        .expect("replacement unit")
        .catalog()
        .clone();
    let visible_replacement = LocalScan {
        sealed: vec![kronika_store::SealedUnit {
            path: dir.path().join("1000.pgm"),
            catalog: replacement_catalog,
        }],
        warnings: root_warning.warnings.clone(),
        ..root_warning.clone()
    };
    let replaced =
        sealed_delta(&visible_replacement, &[previous], dir.path()).expect("replacement");
    assert_eq!(replaced.removed, vec![previous]);
    assert_eq!(replaced.added.len(), 1);
    assert_eq!(replaced.baseline, replaced.added);

    let journal_warning = LocalScan {
        warnings: vec![StoreWarning {
            path: dir.path().join("active.parts"),
            reason: "journal unavailable".to_owned(),
        }],
        ..root_warning
    };
    let removed = sealed_delta(&journal_warning, &[previous], dir.path()).expect("delta");
    assert_eq!(removed.removed, vec![previous]);
    assert!(removed.baseline.is_empty());
    assert!(!journal_descriptors_complete(&journal_warning, dir.path()));
}

#[test]
fn failed_refresh_preserves_the_previous_delta_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let mut bytes = framed(&make_part(1000, 2000, 1));
    fs::write(&journal_path, &bytes).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    snap.refresh_incremental_delta().expect("initial delta");
    bytes.extend_from_slice(&framed(&make_part(3000, 4000, 1)));
    fs::write(&journal_path, bytes).unwrap();

    let original_mode = fs::metadata(dir.path()).unwrap().permissions().mode();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o000)).unwrap();
    let failed = snap.refresh_incremental_delta();
    fs::set_permissions(
        dir.path(),
        fs::Permissions::from_mode(original_mode & 0o7777),
    )
    .unwrap();

    assert_eq!(
        failed.expect_err("permission error").kind(),
        io::ErrorKind::PermissionDenied
    );
    let recovered = snap.refresh_incremental_delta().expect("recovered delta");
    assert_eq!(recovered.journal.completed_parts.len(), 1);
    assert_eq!(recovered.journal.completed_parts[0].min_ts, 3000);
}
