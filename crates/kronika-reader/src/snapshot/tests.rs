use std::fs::{self, FileTimes};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::time::{Duration, UNIX_EPOCH};

use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_layout::{DataRoot, LayoutLimits};
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};
use kronika_writer::{Journal, JournalConfig, seal};

use super::*;
use crate::{FactOrigin, LIMIT};

/// Build one minimal valid part with a real section.
fn make_part(min_ts: i64, max_ts: i64, source_id: u64) -> Vec<u8> {
    make_part_with_timed(min_ts, max_ts, source_id, 0)
}

fn make_part_with_timed(
    min_ts: i64,
    max_ts: i64,
    source_id: u64,
    checkpoints_timed: i64,
) -> Vec<u8> {
    let row = BgwriterCheckpointer {
        ts: Ts(min_ts),
        checkpoints_timed,
        checkpoints_req: 0,
        checkpoint_write_time: 0.0,
        checkpoint_sync_time: 0.0,
        buffers_checkpoint: 0,
        restartpoints_timed: None,
        restartpoints_req: None,
        restartpoints_done: None,
        buffers_clean: 0,
        maxwritten_clean: 0,
        buffers_backend: Some(0),
        buffers_backend_fsync: Some(0),
        buffers_alloc: 0,
        bgwriter_stats_reset: Ts(min_ts),
        checkpointer_stats_reset: None,
    };
    let body = BgwriterCheckpointer::encode(&[row]).expect("encode section");
    build_part(
        &[SectionInput {
            type_id: 1_006_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts,
            max_ts,
            source_id,
        },
    )
}

/// Build a complete version-1 journal for a fixture whose first window starts
/// at the part's minimum timestamp.
fn journal(part_bytes: &[u8]) -> Vec<u8> {
    let unit = PgmUnit::open(part_bytes).expect("valid test part");
    let segment_id =
        SegmentId::new(unit.catalog().min_ts).expect("representable fixture segment id");
    crate::test_layout::journal_bytes(segment_id, &[part_bytes])
}

#[derive(Clone, Copy)]
enum CommittedHeaderPhase {
    Previous,
    Empty,
    Torn,
}

fn committed_reset_journal(part_bytes: &[u8], phase: CommittedHeaderPhase) -> Vec<u8> {
    let mut bytes = journal(part_bytes);
    let previous_len = bytes.len() as u64;
    let previous_header: [u8; JOURNAL_HEADER_LEN] = bytes[..JOURNAL_HEADER_LEN]
        .try_into()
        .expect("complete previous header");
    let header = JournalHeader::decode(previous_header).expect("valid previous header");
    let JournalState::Active { segment_id } = header.state else {
        panic!("test journal must be active");
    };
    bytes.extend_from_slice(
        &ResetMarker::new(previous_len, segment_id)
            .expect("non-empty test journal")
            .encode(),
    );
    match phase {
        CommittedHeaderPhase::Previous => {}
        CommittedHeaderPhase::Empty => {
            bytes[..JOURNAL_HEADER_LEN].copy_from_slice(&JournalHeader::EMPTY.encode());
        }
        CommittedHeaderPhase::Torn => {
            let empty = JournalHeader::EMPTY.encode();
            let split = JOURNAL_HEADER_LEN / 2;
            bytes[..split].copy_from_slice(&empty[..split]);
            bytes[split..JOURNAL_HEADER_LEN]
                .copy_from_slice(&previous_header[split..JOURNAL_HEADER_LEN]);
        }
    }
    bytes
}

fn write_segment(root: &Path, raw_id: i64, bytes: &[u8]) -> PathBuf {
    crate::test_layout::write_pgm(root, crate::test_layout::address(raw_id), bytes)
}

fn seal_parts_without_reset(root: &Path, raw_id: i64, parts: &[&[u8]]) {
    let data_root = DataRoot::open(root).expect("open test data root");
    let owner = data_root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire test writer");
    let address = crate::test_layout::address(raw_id);
    let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open test journal");
    for part in parts {
        journal
            .append(address.id, part)
            .expect("append test journal part");
    }
    seal(&journal, &owner, address).expect("seal test segment");
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

#[test]
fn sealed_snapshot_cache_hit_after_reopen_reads_no_pgm_bodies() {
    let source = tempfile::tempdir().expect("source directory");
    write_segment(source.path(), 1_500, &lifecycle_part(7));
    let store = FactStore::new(source.path());

    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    let cold = snapshot
        .load_sealed_facts(0, &store, &LIMIT)
        .expect("cold facts");
    assert_eq!(cold.origin(), FactOrigin::Rebuilt);
    assert_eq!(cold.pgm_body_read_stats().read_calls, 1);

    let restarted = LocalDirSnapshot::open(source.path()).expect("restart snapshot");
    let warm = restarted
        .load_sealed_facts(0, &store, &LIMIT)
        .expect("warm facts");
    assert_eq!(warm.origin(), FactOrigin::CacheHit);
    assert_eq!(warm.pgm_body_read_stats().read_calls, 0);
    assert_eq!(warm.facts().observations(), cold.facts().observations());
}

#[test]
fn exact_sealed_descriptors_keep_identical_files_distinct_and_warm() {
    let source = tempfile::tempdir().expect("source directory");
    let bytes = lifecycle_part(7);
    write_segment(source.path(), 1_500, &bytes);
    write_segment(source.path(), 1_501, &bytes);
    let store = FactStore::new(source.path());

    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    let descriptors = snapshot.sealed_descriptors().collect::<Vec<_>>();
    assert_eq!(descriptors.len(), 2);
    assert_ne!(descriptors[0].locator, descriptors[1].locator);
    assert_eq!(descriptors[0].catalog_digest, descriptors[1].catalog_digest);
    for descriptor in &descriptors {
        let load = snapshot
            .load_sealed_facts_by_descriptor(descriptor, &store, &LIMIT)
            .expect("cold exact load");
        assert_eq!(load.origin(), FactOrigin::Rebuilt);
    }

    let restarted = LocalDirSnapshot::open(source.path()).expect("restart snapshot");
    for descriptor in restarted.sealed_descriptors() {
        let load = restarted
            .load_sealed_facts_by_descriptor(&descriptor, &FactStore::new(source.path()), &LIMIT)
            .expect("warm exact load");
        assert_eq!(load.origin(), FactOrigin::CacheHit);
        assert_eq!(load.pgm_body_read_stats().read_calls, 0);
    }
}

#[test]
fn exact_active_part_open_is_independent_of_query_unit_deduplication() {
    let source = tempfile::tempdir().expect("source directory");
    let bytes = lifecycle_part(7);
    seal_parts_without_reset(source.path(), 1_500, &[&bytes]);

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
    let sealed = snapshot
        .unit_catalog(0)
        .expect("read sealed catalog")
        .expect("sealed catalog");
    assert_eq!(sealed.source_id, active.catalog().source_id);
    assert_eq!(
        (sealed.min_ts, sealed.max_ts),
        (active.catalog().min_ts, active.catalog().max_ts)
    );
    assert_eq!(sealed.entries[0].rows, active.catalog().entries[0].rows);
}

#[test]
fn exact_sealed_context_uses_the_pgm_stem() {
    let source = tempfile::tempdir().expect("source directory");
    write_segment(source.path(), 1_500, &lifecycle_part(7));
    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    let descriptor = snapshot
        .sealed_descriptors()
        .next()
        .expect("sealed descriptor");
    let context = snapshot
        .sealed_context(&descriptor)
        .expect("derive sealed context");
    assert_eq!(context.pgm_file_name(), "1500.pgm");
    assert_eq!(context.sidecar_file_name(), "1500.ovf");
}

#[test]
fn snapshot_clone_shares_sealed_discovery_without_a_descriptor_baseline() {
    let source = tempfile::tempdir().expect("source directory");
    write_segment(source.path(), 1_500, &lifecycle_part(7));
    fs::write(
        source.path().join("active.parts"),
        journal(&make_part(2_000, 3_000, 7)),
    )
    .expect("write active journal");
    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");

    let cloned = snapshot.clone();

    assert!(Arc::ptr_eq(&snapshot.scan.sealed, &cloned.scan.sealed));
    assert!(Arc::ptr_eq(&snapshot.scan.active, &cloned.scan.active));
    assert_eq!(
        snapshot.sealed_descriptors().collect::<Vec<_>>(),
        cloned.sealed_descriptors().collect::<Vec<_>>()
    );
}

#[test]
fn same_name_replacement_invalidates_pinned_snapshot() {
    let source = tempfile::tempdir().expect("source directory");
    let path = write_segment(source.path(), 1_500, &lifecycle_part(7));
    let pinned = LocalDirSnapshot::open(source.path()).expect("open pinned snapshot");
    let store = FactStore::new(source.path());
    pinned
        .load_sealed_facts(0, &store, &LIMIT)
        .expect("first facts");

    fs::write(&path, lifecycle_part(8)).expect("replace segment");
    assert!(matches!(
        pinned.load_sealed_facts(0, &store, &LIMIT),
        Err(SealedFactError::StaleSnapshot { unit_idx: 0 })
    ));

    let refreshed = LocalDirSnapshot::open(source.path()).expect("refresh snapshot");
    let replacement = refreshed
        .load_sealed_facts(0, &store, &LIMIT)
        .expect("replacement facts");
    assert_eq!(replacement.facts().identity().pgm_source_id, 8);
    assert_eq!(replacement.origin(), FactOrigin::Rebuilt);
}

#[test]
fn removed_source_is_not_resurrected_by_an_orphan_fact_file() {
    let source = tempfile::tempdir().expect("source directory");
    let path = write_segment(source.path(), 1_500, &lifecycle_part(7));
    let store = FactStore::new(source.path());
    LocalDirSnapshot::open(source.path())
        .expect("open snapshot")
        .load_sealed_facts(0, &store, &LIMIT)
        .expect("build facts");
    fs::remove_file(path).expect("remove authoritative segment");

    let after_retention = LocalDirSnapshot::open(source.path()).expect("rescan source");
    assert!(after_retention.units().is_empty());
    assert!(matches!(
        after_retention.load_sealed_facts(0, &store, &LIMIT),
        Err(SealedFactError::UnitOutOfRange { unit_idx: 0 })
    ));
    let orphan_exists = walk_files(source.path())
        .iter()
        .any(|path| path.extension().and_then(|value| value.to_str()) == Some("ovf"));
    assert!(
        orphan_exists,
        "source retention does not remove its sibling sidecar"
    );
}

#[test]
fn active_part_is_rejected_by_sealed_fact_loader() {
    let source = tempfile::tempdir().expect("source directory");
    fs::write(
        source.path().join("active.parts"),
        journal(&lifecycle_part(7)),
    )
    .expect("write active part");
    let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
    assert!(matches!(
        snapshot.load_sealed_facts(0, &FactStore::new(source.path()), &LIMIT),
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
    let journal: Vec<u8> = journal(&part);
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
fn committed_reset_phases_open_as_a_complete_empty_snapshot() {
    for phase in [
        CommittedHeaderPhase::Previous,
        CommittedHeaderPhase::Empty,
        CommittedHeaderPhase::Torn,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bytes = committed_reset_journal(&make_part(1000, 2000, 1), phase);
        fs::write(dir.path().join("active.parts"), &bytes).unwrap();

        let snapshot = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snapshot.units().is_empty());
        assert_eq!(snapshot.last_valid_len, bytes.len() as u64);
        assert_eq!(snapshot.tail_pending, None);
    }
}

#[test]
fn committed_reset_boundary_mints_once_across_marker_cleanup_and_next_append() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first_part = make_part(1000, 2000, 1);
    fs::write(&journal_path, journal(&first_part)).unwrap();
    let mut snapshot = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snapshot
        .refresh_incremental_delta()
        .expect("initial active baseline");
    let initial_generation = initial.journal.generation_id;

    fs::write(
        &journal_path,
        committed_reset_journal(&first_part, CommittedHeaderPhase::Previous),
    )
    .unwrap();
    let reset = snapshot
        .refresh_incremental_delta()
        .expect("committed reset");
    assert_eq!(reset.journal.transition, PartTransition::Reset);
    assert_ne!(reset.journal.generation_id, initial_generation);
    assert!(reset.journal.current_parts.is_empty());
    let post_reset_generation = reset.journal.generation_id;

    for phase in [CommittedHeaderPhase::Empty, CommittedHeaderPhase::Torn] {
        fs::write(&journal_path, committed_reset_journal(&first_part, phase)).unwrap();
        let marker_phase = snapshot.refresh_incremental_delta().expect("marker phase");
        assert_eq!(marker_phase.journal.transition, PartTransition::Append);
        assert_eq!(
            marker_phase.journal.generation_id, post_reset_generation,
            "rewriting the committed marker header must not mint again"
        );
        assert!(marker_phase.journal.current_parts.is_empty());
    }

    crate::test_layout::write_empty_journal(dir.path());
    let empty = snapshot
        .refresh_incremental_delta()
        .expect("canonical empty cleanup");
    assert_eq!(empty.journal.transition, PartTransition::Append);
    assert_eq!(empty.journal.generation_id, post_reset_generation);

    let next_part = make_part(3000, 4000, 1);
    fs::write(&journal_path, journal(&next_part)).unwrap();
    let next = snapshot
        .refresh_incremental_delta()
        .expect("first append after reset");
    assert_eq!(next.journal.transition, PartTransition::Append);
    assert_eq!(next.journal.generation_id, post_reset_generation);
    assert_eq!(next.journal.completed_parts.len(), 1);
    assert_eq!(next.journal.completed_parts[0].min_ts, 3000);
}

#[test]
fn committed_marker_to_direct_active_redelivers_once_in_the_reset_generation() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first_part = make_part(1000, 2000, 1);
    fs::write(&journal_path, journal(&first_part)).unwrap();
    let mut snapshot = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snapshot
        .refresh_incremental_delta()
        .expect("initial active baseline");

    fs::write(
        &journal_path,
        committed_reset_journal(&first_part, CommittedHeaderPhase::Previous),
    )
    .unwrap();
    let reset = snapshot
        .refresh_incremental_delta()
        .expect("committed reset");
    assert_eq!(reset.journal.transition, PartTransition::Reset);
    assert_ne!(reset.journal.generation_id, initial.journal.generation_id);
    assert!(reset.journal.completed_parts.is_empty());
    let reset_generation = reset.journal.generation_id;
    let reset_view_generation = reset.new_view_generation;

    let next_part = make_part(3000, 4000, 1);
    fs::write(&journal_path, journal(&next_part)).unwrap();
    let next = snapshot
        .refresh_incremental_delta()
        .expect("direct first append after marker");
    assert_eq!(next.journal.transition, PartTransition::Append);
    assert_eq!(next.journal.generation_id, reset_generation);
    assert_eq!(next.journal.completed_parts.len(), 1);
    assert_eq!(next.journal.completed_parts[0].min_ts, 3000);
    assert!(next.view_changed);
    assert_eq!(next.previous_view_generation, reset_view_generation);
    assert_eq!(next.new_view_generation, reset_view_generation + 1);

    let unchanged = snapshot
        .refresh_incremental_delta()
        .expect("unchanged first append");
    assert_eq!(unchanged.journal.generation_id, reset_generation);
    assert!(unchanged.journal.completed_parts.is_empty());
    assert!(!unchanged.view_changed);
    assert_eq!(
        unchanged.new_view_generation, next.new_view_generation,
        "the direct append must advance the view exactly once"
    );
}

#[test]
fn different_committed_resets_between_polls_mint_each_observed_generation_once() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first_part = make_part(1000, 2000, 1);
    fs::write(&journal_path, journal(&first_part)).unwrap();
    let mut snapshot = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snapshot
        .refresh_incremental_delta()
        .expect("initial active baseline");

    fs::write(
        &journal_path,
        committed_reset_journal(&first_part, CommittedHeaderPhase::Previous),
    )
    .unwrap();
    let first_reset = snapshot
        .refresh_incremental_delta()
        .expect("first committed reset");
    assert_eq!(first_reset.journal.transition, PartTransition::Reset);
    assert_ne!(
        first_reset.journal.generation_id,
        initial.journal.generation_id
    );

    let second_part = make_part(3000, 4000, 1);
    fs::write(
        &journal_path,
        committed_reset_journal(&second_part, CommittedHeaderPhase::Previous),
    )
    .unwrap();
    let second_reset = snapshot
        .refresh_incremental_delta()
        .expect("different committed reset");
    assert_eq!(second_reset.journal.transition, PartTransition::Reset);
    assert_ne!(
        second_reset.journal.generation_id,
        first_reset.journal.generation_id
    );
    assert!(second_reset.journal.completed_parts.is_empty());
    assert!(second_reset.view_changed);

    let unchanged = snapshot
        .refresh_incremental_delta()
        .expect("unchanged second marker");
    assert_eq!(unchanged.journal.transition, PartTransition::Append);
    assert_eq!(
        unchanged.journal.generation_id,
        second_reset.journal.generation_id
    );
    assert!(unchanged.journal.completed_parts.is_empty());
    assert!(!unchanged.view_changed);
    assert_eq!(
        unchanged.new_view_generation, second_reset.new_view_generation,
        "the second reset must advance the view exactly once"
    );
}

#[test]
fn stable_active_corruption_remains_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = journal(&make_part(1000, 2000, 1));
    bytes[JOURNAL_HEADER_LEN + kronika_format::FRAME_HEADER_LEN] ^= 0xff;
    fs::write(dir.path().join("active.parts"), bytes).unwrap();

    assert_eq!(
        LocalDirSnapshot::open(dir.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn exact_sealed_active_catalog_is_deduped_no_double() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 42);
    // Production sealing publishes the canonical PGM and deliberately leaves
    // the exact source journal in the crash window exercised here.
    seal_parts_without_reset(dir.path(), 1_000, &[&part]);

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(units.len(), 1, "exact active duplicate must be deduped");
    assert!(!units[0].live, "surviving unit must be the sealed one");
    assert_eq!(units[0].source_id, 42);
}

#[test]
fn exact_sealed_multi_part_aggregate_is_deduped_as_one_segment() {
    let dir = tempfile::tempdir().unwrap();
    let first = make_part(1000, 2000, 42);
    let second = make_part(3000, 4000, 42);
    seal_parts_without_reset(dir.path(), 1_000, &[&first, &second]);

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(
        units.len(),
        1,
        "the exact sealed aggregate suppresses every source journal part"
    );
    assert!(!units[0].live);
    assert_eq!((units[0].min_ts, units[0].max_ts), (1000, 4000));
}

#[test]
fn same_catalog_envelope_with_changed_value_does_not_hide_active_parts() {
    let dir = tempfile::tempdir().unwrap();
    let first = make_part(1000, 2000, 42);
    let second = make_part(3000, 4000, 42);
    seal_parts_without_reset(dir.path(), 1_000, &[&first, &second]);

    let changed_second = make_part_with_timed(3000, 4000, 42, 1);
    let segment_id = SegmentId::new(1_000).expect("test segment id");
    fs::write(
        dir.path().join("active.parts"),
        crate::test_layout::journal_bytes(segment_id, &[&first, &changed_second]),
    )
    .expect("replace active journal with a same-envelope generation");

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(
        snap.units().len(),
        3,
        "an exact body mismatch must keep the sealed segment and every active part visible"
    );
    assert!(!snap.units()[0].live);
    assert!(snap.units()[1..].iter().all(|unit| unit.live));
}

#[test]
fn dictionary_only_aggregate_uses_the_sealers_zero_interval() {
    let dir = tempfile::tempdir().unwrap();
    let mut interner = Interner::new(DictLimits::new(256, 1 << 20).expect("limits"));
    interner
        .intern(b"dictionary-only")
        .expect("intern dictionary-only value");
    let dictionary = dict::encode(interner.window())
        .expect("encode dictionary")
        .into_iter()
        .find(|section| section.type_id == DICT_STRINGS_TYPE_ID)
        .expect("string dictionary section");
    let dictionary_only = build_part(
        &[SectionInput {
            type_id: dictionary.type_id,
            rows: dictionary.rows,
            body: &dictionary.body,
        }],
        PartMeta {
            min_ts: i64::MAX,
            max_ts: i64::MIN,
            source_id: 0,
        },
    );
    seal_parts_without_reset(dir.path(), 1_000, &[&dictionary_only]);

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(units.len(), 1, "the dictionary-only live part is deduped");
    assert!(!units[0].live);
    assert_eq!((units[0].min_ts, units[0].max_ts), (0, 0));
}

#[test]
fn partial_sealed_aggregate_does_not_hide_any_active_part() {
    let dir = tempfile::tempdir().unwrap();
    let first = make_part(1000, 2000, 42);
    let second = make_part(3000, 4000, 42);
    let later = make_part(5000, 6000, 42);
    seal_parts_without_reset(dir.path(), 1_000, &[&first, &second]);
    crate::test_layout::append_journal_part(&dir.path().join("active.parts"), &later);

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let units = snap.units();
    assert_eq!(
        units.iter().filter(|unit| unit.live).count(),
        3,
        "an older sealed prefix is not proof that any current active part is duplicated"
    );
    assert_eq!(units.len(), 4, "one sealed aggregate plus all active parts");
}

#[test]
fn identical_aggregate_under_another_segment_id_is_not_deduped() {
    let dir = tempfile::tempdir().unwrap();
    let first = make_part(1000, 2000, 42);
    let second = make_part(3000, 4000, 42);
    seal_parts_without_reset(dir.path(), 1_000, &[&first, &second]);
    fs::write(
        dir.path().join("active.parts"),
        crate::test_layout::journal_bytes(SegmentId::new(2_000).unwrap(), &[&first, &second]),
    )
    .unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 3);
    assert_eq!(
        snap.units().iter().filter(|unit| unit.live).count(),
        2,
        "catalog equality cannot cross a SegmentId boundary"
    );
}

#[test]
fn overlapping_active_part_is_not_deduped_by_range_only() {
    let dir = tempfile::tempdir().unwrap();
    let sealed = make_part(1000, 5000, 42);
    let active = make_part(2000, 3000, 42);
    write_segment(dir.path(), 1_000, &sealed);
    fs::write(dir.path().join("active.parts"), journal(&active)).unwrap();

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
    let journal = journal(&part1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Append a second part.
    let part2 = make_part(3000, 4000, 1);
    crate::test_layout::append_journal_part(&journal_path, &part2);

    snap.refresh().unwrap();
    assert_eq!(snap.units().len(), 2, "refresh must surface the new part");
}

#[test]
fn refresh_incremental_surfaces_appended_part_and_keeps_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    let journal = journal(&part1);
    fs::write(&journal_path, &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    let first_offset = snap.scan.active[0].part.offset;
    let valid_before = snap.last_valid_len;
    assert_eq!(valid_before, journal.len() as u64);

    // Append a second part.
    let part2 = make_part(3000, 4000, 1);
    let appended_len = crate::test_layout::journal_frame(&part2).len() as u64;
    crate::test_layout::append_journal_part(&journal_path, &part2);

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
        valid_before + appended_len,
        "valid_len advances by exactly the appended frame"
    );
}

#[test]
fn refresh_incremental_noop_when_journal_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, journal(&part1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let first_offset = snap.scan.active[0].part.offset;
    let valid_before = snap.last_valid_len;
    let active_before = Arc::clone(&snap.scan.active);

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
    assert!(
        Arc::ptr_eq(&snap.scan.active, &active_before),
        "a no-op refresh must retain the validated active allocation"
    );
}

#[test]
fn refresh_incremental_reset_with_an_empty_v1_header() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, journal(&part1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    crate::test_layout::write_empty_journal(dir.path());

    snap.refresh_incremental().unwrap();
    assert!(snap.units().is_empty(), "reset clears the live parts");
    assert_eq!(
        snap.last_valid_len, JOURNAL_HEADER_LEN as u64,
        "valid_len resets to the empty header boundary"
    );
}

#[test]
fn refresh_incremental_rejects_and_preserves_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    let journal = journal(&part1);
    fs::write(&journal_path, &journal).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let part2 = make_part(3000, 4000, 1);
    let full = crate::test_layout::journal_frame(&part2);
    let mut buf = journal;
    buf.extend_from_slice(&full[..full.len() - 3]);
    fs::write(&journal_path, &buf).unwrap();

    let error = snap
        .refresh_incremental()
        .expect_err("a torn journal is fatal");
    assert_eq!(
        error.kind(),
        io::ErrorKind::InvalidData,
        "a torn journal must not be treated as a partial snapshot"
    );
    assert_eq!(
        fs::read(&journal_path).unwrap(),
        buf,
        "read-side validation does not rewrite damaged evidence"
    );
}

#[test]
fn refresh_incremental_discovers_new_sealed_segment() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(2000, 3000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, journal(&part1)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(snap.units()[0].live);

    // A new sealed segment appears in the directory.
    let sealed = make_part(500, 1000, 1);
    write_segment(dir.path(), 500, &sealed);

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
fn middle_corruption_is_fatal_and_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let part1 = make_part(1000, 2000, 1);
    let part2 = make_part(3000, 4000, 1);
    let mut body = crate::test_layout::journal_frame(&part1);
    body.extend_from_slice(b"GARBAGE_BYTES_HERE_THAT_ARE_NOT_A_VALID_FRAME");
    body.extend_from_slice(&crate::test_layout::journal_frame(&part2));
    let mut bytes = JournalHeader {
        state: JournalState::Active { segment_id: 1_000 },
        body_len: body.len() as u64,
    }
    .encode()
    .to_vec();
    bytes.extend_from_slice(&body);
    let path = dir.path().join("active.parts");
    fs::write(&path, &bytes).unwrap();

    let error = LocalDirSnapshot::open(dir.path()).expect_err("damaged journal must fail");
    assert_eq!(
        error.kind(),
        io::ErrorKind::InvalidData,
        "the reader must not serve a selectively recovered journal"
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn decode_unit_sealed_happy_path() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    write_segment(dir.path(), 1_000, &part);

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(!snap.units()[0].live);

    let catalog = snap
        .unit_catalog(0)
        .expect("read catalog for unit 0")
        .expect("catalog for unit 0");
    assert!(!catalog.entries.is_empty());

    let decoded = snap.decode_unit(0, 0).expect("decode sealed unit");
    assert_eq!(decoded.stats.type_id, 1_006_001);
}

#[test]
fn decode_unit_active_happy_path() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    let journal = journal(&part);
    fs::write(dir.path().join("active.parts"), &journal).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);
    assert!(snap.units()[0].live);

    let catalog = snap
        .unit_catalog(0)
        .expect("read catalog for unit 0")
        .expect("catalog for unit 0");
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
    write_segment(dir.path(), 1_000, &part);

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
    fs::write(&journal_path, journal(&part_a)).unwrap();

    // Snapshot taken while part_a is live.
    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Replace the journal with a different part (different timestamps).
    let part_b = make_part(5000, 6000, 2);
    fs::write(&journal_path, journal(&part_b)).unwrap();

    // The cached offset now maps to bytes belonging to a different part.
    let err = snap.decode_unit(0, 0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "replaced journal must trigger StaleSnapshot, got: {err}"
    );
}

#[test]
fn open_unit_rejects_a_same_name_sealed_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_segment(dir.path(), 1_000, &make_part(1000, 2000, 1));
    let snap = LocalDirSnapshot::open(dir.path()).unwrap();

    fs::write(&path, make_part(5000, 6000, 2)).unwrap();

    let err = snap.open_unit(0).unwrap_err();
    assert!(
        matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
        "same-name sealed replacement must trigger StaleSnapshot, got: {err}"
    );
}

#[test]
fn open_unit_fails_closed_when_a_sealed_segment_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_segment(dir.path(), 1_000, &make_part(1000, 2000, 1));
    let snap = LocalDirSnapshot::open(dir.path()).unwrap();

    fs::remove_file(path).unwrap();

    let err = snap.open_unit(0).unwrap_err();
    assert!(
        matches!(err, ReadError::Io(ref error) if error.kind() == io::ErrorKind::NotFound),
        "missing sealed input must remain a hard read failure, got: {err}"
    );
}

#[test]
fn missing_active_parts_is_empty_live_journal() {
    let dir = tempfile::tempdir().unwrap();
    let part = make_part(1000, 2000, 7);
    // Write only a sealed file — no active.parts.
    write_segment(dir.path(), 1_000, &part);

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
    let sealed_path = write_segment(dir.path(), 1_000, &part_bytes);
    let sealed_snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(!sealed_snap.units()[0].live);
    // pg_stat_archiver is entry 0 (first non-dict section, but dict sections
    // come after data sections in our fixture, so entry 0 is archiver).
    let catalog = sealed_snap
        .unit_catalog(0)
        .expect("read catalog")
        .expect("catalog");
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
    fs::write(&journal_path, journal(&part_bytes)).unwrap();
    let active_snap = LocalDirSnapshot::open(dir.path()).unwrap();
    // The active part is deduped by the sealed unit, so only 1 unit total.
    // The sealed unit is at index 0. Write only active (remove sealed).
    fs::remove_file(sealed_path).unwrap();
    let active_snap2 = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(active_snap2.units()[0].live);
    let catalog2 = active_snap2
        .unit_catalog(0)
        .expect("read catalog")
        .expect("catalog");
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
    write_segment(dir.path(), 1_000, &part_bytes);

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
    fs::write(&journal_path, journal(&part_bytes)).unwrap();

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
    fs::write(&journal_path, journal(&part_bytes)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert!(snap.units()[0].live);
    let archiver_entry_idx = snap
        .unit_catalog(0)
        .expect("read catalog")
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
    fs::write(&journal_path, journal(&part_bytes)).unwrap();

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
    fs::write(&journal_path, journal(&part)).unwrap();

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
    fs::write(&journal_path, journal(&part)).unwrap();

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
    write_segment(dir.path(), 1_000, &part_bytes);

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
    fs::write(dir.path().join("active.parts"), journal(&part_bytes)).unwrap();

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
    fs::write(&journal_path, journal(&part)).unwrap();

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
    let part = make_part(1000, 2000, 1);
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, journal(&part)).unwrap();

    let snap = LocalDirSnapshot::open(dir.path()).unwrap();
    assert_eq!(snap.units().len(), 1);

    // Reuse the exact offset and catalog under another journal SegmentId.
    fs::write(
        &journal_path,
        crate::test_layout::journal_bytes(SegmentId::new(5_000).unwrap(), &[&part]),
    )
    .unwrap();

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
    write_segment(dir.path(), 1_000, &part);

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
    fs::write(&journal_path, journal(&make_part(1000, 2000, 1))).unwrap();

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

    crate::test_layout::append_journal_part(&journal_path, &make_part(3000, 4000, 1));

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
    fs::write(&journal_path, journal(&make_part(1000, 2000, 1))).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert_eq!(initial.journal.completed_parts.len(), 1);
    crate::test_layout::append_journal_part(&journal_path, &make_part(3000, 4000, 1));

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
fn refresh_delta_empty_v1_reset_mints_a_new_generation() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    fs::write(&journal_path, journal(&make_part(1000, 2000, 1))).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let generation_before = snap.journal_generation();

    crate::test_layout::write_empty_journal(dir.path());

    let delta = snap.refresh_incremental_delta().expect("delta");
    assert_eq!(delta.journal.transition, PartTransition::Reset);
    assert_ne!(delta.journal.generation_id, generation_before);
    assert_eq!(delta.journal.new_valid_len, JOURNAL_HEADER_LEN as u64);
    assert!(delta.journal.completed_parts.is_empty());
    assert!(delta.journal.current_parts.is_empty());
    assert!(delta.journal.current_parts_complete);
    assert!(delta.requires_live_rebuild());
}

#[test]
fn refresh_delta_rejects_and_preserves_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let base = journal(&make_part(1000, 2000, 1));
    fs::write(&journal_path, &base).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert_eq!(initial.journal.completed_parts.len(), 1);

    let full = crate::test_layout::journal_frame(&make_part(3000, 4000, 1));
    let mut buf = base;
    buf.extend_from_slice(&full[..full.len() - 3]);
    fs::write(&journal_path, &buf).unwrap();

    let error = snap
        .refresh_incremental_delta()
        .expect_err("torn journal must fail the refresh");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&journal_path).unwrap(), buf);
}

#[test]
fn refresh_delta_reports_a_newly_sealed_segment() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("active.parts"),
        journal(&make_part(2000, 3000, 1)),
    )
    .unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    assert_eq!(initial.journal.completed_parts.len(), 1);
    write_segment(dir.path(), 500, &make_part(500, 1000, 1));

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
        journal(&make_part(1000, 2000, 1)),
    )
    .unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let first = snap.refresh_incremental_delta().expect("first delta");
    let second = snap.refresh_incremental_delta().expect("second delta");

    assert_eq!(first.journal.completed_parts.len(), 1);
    assert_eq!(first.journal.completed_parts[0].min_ts, 1000);
    assert_eq!(
        first.journal.completed_parts[0].part_id.catalog_digest,
        snap.scan.active[0].catalog_digest
    );
    assert!(
        Arc::ptr_eq(&first.journal.completed_parts, &first.journal.current_parts),
        "a bootstrap delta must not allocate a duplicate full descriptor set"
    );
    assert!(second.journal.completed_parts.is_empty());
    assert_eq!(second.new_view_generation, first.new_view_generation);
}

#[test]
fn equal_length_rewrite_discards_the_cached_active_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first_part = journal(&make_part(1000, 2000, 1));
    let replacement = journal(&make_part(3000, 4000, 1));
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
    let first = journal(&make_part(1000, 2000, 1));
    fs::write(&journal_path, &first).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    let initial = snap.refresh_incremental_delta().expect("initial delta");
    let initial_generation = initial.journal.generation_id;
    let initial_inode = fs::metadata(&journal_path).unwrap().ino();

    let replacement_part = make_part(3000, 4000, 1);
    let later_part = make_part(5000, 6000, 1);
    let replacement = journal(&replacement_part);
    assert_eq!(first.len(), replacement.len());
    let rewritten_and_grown = crate::test_layout::journal_bytes(
        SegmentId::new(3_000).unwrap(),
        &[&replacement_part, &later_part],
    );
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
    fs::write(&journal_path, journal(&valid_part)).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    Arc::make_mut(&mut snap.scan.active).clear();
    snap.scan.warnings.push(StoreWarning {
        affected: StoreObject::ActiveJournal,
        reason: kronika_store::StoreWarningReason::ActiveJournal(
            kronika_store::ActiveJournalWarningReason::Io,
        ),
        identity: None,
        failure: None,
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
fn damaged_sealed_file_is_excluded_until_a_valid_identity_returns() {
    let dir = tempfile::tempdir().unwrap();
    let valid_segment = make_part(1000, 2000, 1);
    let sealed_path = write_segment(dir.path(), 1_000, &valid_segment);

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    snap.refresh_incremental_delta().expect("initial delta");
    let generation = snap.view_generation();

    fs::write(&sealed_path, b"not a pgm segment").unwrap();
    let excluded = snap
        .refresh_incremental_delta()
        .expect("damage is localized to the invalid segment");
    assert!(excluded.view_changed);
    assert_eq!(excluded.sealed_added.len(), 0);
    assert_eq!(excluded.sealed_removed.len(), 1);
    assert!(snap.view_generation() > generation);
    assert!(snap.units().is_empty());
    assert!(matches!(
        snap.warnings(),
        [StoreWarning {
            affected: StoreObject::Segment(_),
            reason: kronika_store::StoreWarningReason::InvalidPgm(_),
            ..
        }]
    ));

    fs::write(&sealed_path, &valid_segment).unwrap();
    let recovered = snap.refresh_incremental_delta().expect("readable recovery");
    assert!(recovered.view_changed);
    assert_eq!(recovered.sealed_added.len(), 1);
    assert!(recovered.sealed_removed.is_empty());
    assert!(snap.warnings().is_empty());

    fs::write(&sealed_path, b"not a pgm segment").unwrap();
    let excluded_again = snap
        .refresh_incremental_delta()
        .expect("second damage is localized again");
    assert!(excluded_again.view_changed);
    assert_eq!(excluded_again.sealed_removed.len(), 1);
    assert!(excluded_again.sealed_added.is_empty());

    fs::remove_file(&sealed_path).unwrap();
    let absent = snap
        .refresh_incremental_delta()
        .expect("authoritative absence");
    assert!(
        absent.sealed_removed.is_empty(),
        "the invalid descriptor was already removed exactly once"
    );
    assert!(
        snap.warnings().is_empty(),
        "absence clears the invalid-file warning"
    );
    let repeated = snap.refresh_incremental_delta().expect("repeated absence");
    assert!(repeated.sealed_removed.is_empty());
}

#[test]
fn same_name_sealed_replacement_reports_remove_and_add() {
    let dir = tempfile::tempdir().unwrap();
    let sealed_path = write_segment(dir.path(), 1_000, &make_part(1000, 2000, 1));

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
fn sealed_delta_compares_compact_scans_without_a_full_descriptor_baseline() {
    let catalog = PgmUnit::open(make_part(1000, 2000, 1).as_slice())
        .expect("unit")
        .catalog()
        .clone();
    let address = crate::test_layout::address(1_000);
    let identity = kronika_layout::FileIdentity {
        device: 1,
        inode: 2,
        len: 3,
        mtime_seconds: 4,
        mtime_nanoseconds: 5,
        ctime_seconds: 6,
        ctime_nanoseconds: 7,
    };
    let summary = CatalogSummary::from_catalog(
        &catalog,
        u32::try_from(catalog.encoded_len()).expect("catalog length"),
    );
    let previous_unit = SealedUnit {
        address,
        identity,
        summary: Arc::new(summary),
    };
    let previous = descriptor_for_sealed(&previous_unit);
    let previous_units = vec![previous_unit];
    let unchanged_scan = LocalScan {
        sealed: Arc::new(previous_units.clone()),
        active: Arc::new(Vec::new()),
        damages: Vec::new(),
        warnings: Vec::new(),
        valid_len: 0,
        committed_reset: false,
    };
    let unchanged = sealed_delta(&unchanged_scan, &previous_units);
    assert!(unchanged.added.is_empty());
    assert!(unchanged.removed.is_empty());

    let empty_scan = LocalScan {
        sealed: Arc::new(Vec::new()),
        active: Arc::new(Vec::new()),
        damages: Vec::new(),
        warnings: Vec::new(),
        valid_len: 0,
        committed_reset: false,
    };
    let removed = sealed_delta(&empty_scan, &previous_units);
    assert_eq!(removed.removed, vec![previous]);
    assert!(removed.added.is_empty());

    let replacement_catalog = PgmUnit::open(make_part(3000, 4000, 1).as_slice())
        .expect("replacement unit")
        .catalog()
        .clone();
    let visible_replacement = LocalScan {
        sealed: Arc::new(vec![SealedUnit {
            address,
            identity: kronika_layout::FileIdentity {
                inode: 3,
                ..identity
            },
            summary: Arc::new(CatalogSummary::from_catalog(
                &replacement_catalog,
                u32::try_from(replacement_catalog.encoded_len()).expect("catalog length"),
            )),
        }]),
        ..empty_scan.clone()
    };
    let replaced = sealed_delta(&visible_replacement, &previous_units);
    assert_eq!(replaced.removed, vec![previous]);
    assert_eq!(replaced.added.len(), 1);

    let journal_warning = LocalScan {
        warnings: vec![StoreWarning {
            affected: StoreObject::ActiveJournal,
            reason: kronika_store::StoreWarningReason::ActiveJournal(
                kronika_store::ActiveJournalWarningReason::Io,
            ),
            identity: None,
            failure: None,
        }],
        ..empty_scan
    };
    assert!(!journal_descriptors_complete(&journal_warning));
}

#[test]
fn failed_refresh_preserves_the_previous_delta_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("active.parts");
    let first_part = make_part(1000, 2000, 1);
    let second_part = make_part(3000, 4000, 1);
    let bytes = journal(&first_part);
    fs::write(&journal_path, &bytes).unwrap();

    let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
    snap.refresh_incremental_delta().expect("initial delta");
    crate::test_layout::append_journal_part(&journal_path, &second_part);

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

fn phase_identity(revision: i128) -> JournalIdentity {
    JournalIdentity {
        device: 1,
        inode: 2,
        len: 128,
        mtime_ns: revision,
        ctime_ns: revision,
    }
}

#[test]
fn changed_identity_retries_a_transition_read_error_then_accepts_stable_success() {
    let first = phase_identity(1);
    let completed = phase_identity(2);
    let mut identities = [first, completed, completed, completed].into_iter();
    let attempts = std::cell::Cell::new(0_usize);

    let (value, identity) = with_stable_journal_identity(
        || Ok(Some(identities.next().expect("identity phase"))),
        |_identity_before| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 0 {
                Err(ScanAttemptError::journal(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "header and appended body are between publication phases",
                )))
            } else {
                Ok(42_u8)
            }
        },
    )
    .expect("the completed append is retried once");

    assert_eq!(value, 42);
    assert_eq!(identity, Some(completed));
    assert_eq!(attempts.get(), 2);
}

#[test]
fn stable_transition_corruption_returns_the_original_error_without_retry() {
    let stable = phase_identity(1);
    let mut identities = [stable, stable].into_iter();
    let attempts = std::cell::Cell::new(0_usize);

    let error = with_stable_journal_identity(
        || Ok(Some(identities.next().expect("identity phase"))),
        |_identity_before| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(ScanAttemptError::journal(io::Error::new(
                io::ErrorKind::InvalidData,
                "stable frame corruption",
            )))
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "stable frame corruption");
    assert_eq!(attempts.get(), 1);
}

#[test]
fn repeated_identity_churn_is_bounded() {
    let mut identities = [
        phase_identity(1),
        phase_identity(2),
        phase_identity(3),
        phase_identity(4),
    ]
    .into_iter();
    let attempts = std::cell::Cell::new(0_usize);

    let error = with_stable_journal_identity(
        || Ok(Some(identities.next().expect("identity phase"))),
        |_identity_before| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(ScanAttemptError::journal(io::Error::new(
                io::ErrorKind::Interrupted,
                "journal changed during scan",
            )))
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(attempts.get(), MAX_CONSISTENT_SCAN_ATTEMPTS);
}

#[test]
fn non_journal_corruption_is_not_retried_during_journal_churn() {
    let identity_reads = std::cell::Cell::new(0_usize);
    let attempts = std::cell::Cell::new(0_usize);

    let error = with_stable_journal_identity(
        || {
            let read = identity_reads.get();
            identity_reads.set(read + 1);
            Ok(Some(phase_identity(read as i128)))
        },
        |_identity_before| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(ScanAttemptError::store(io::Error::new(
                io::ErrorKind::InvalidData,
                "sealed PGM corruption",
            )))
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(attempts.get(), 1);
    assert_eq!(
        identity_reads.get(),
        1,
        "a non-journal failure does not consume an after-identity retry gate"
    );
}
