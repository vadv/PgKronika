use std::os::unix::fs::symlink;
use std::sync::{Arc, Barrier};

use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};
use tempfile::TempDir;

use super::*;
use crate::overview::{FactStore, FallbackConfig, FileKind, SegmentContext, SegmentFacts};

fn context(stem: &str) -> SegmentContext {
    SegmentContext::new(crate::test_layout::named_address(stem))
}

fn lifecycle_pgm(variant: u64) -> Vec<u8> {
    let rows = [
        PgLogLifecycleV1 {
            ts: Ts(1_500),
            kind: 2,
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        },
        PgLogLifecycleV1 {
            ts: Ts(1_700),
            kind: 0,
            pid: Some(i32::try_from(variant).expect("test variant fits i32")),
            signal: Some(6),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        },
    ];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode section");
    build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 2,
            body: &body,
        }],
        PartMeta {
            min_ts: 1_500,
            max_ts: 1_700,
        },
    )
}

fn facts(bytes: &[u8]) -> SegmentFacts {
    let unit = crate::PgmUnit::open(bytes).expect("open PGM");
    SegmentFacts::extract(&unit, &LIMIT).expect("extract facts")
}

fn key(facts: &SegmentFacts) -> FactBuildKey {
    FactBuildKey::new(
        FactKey::for_identity(facts.identity(), FileKind::SegmentFacts),
        facts.lineage().id(),
    )
}

fn immediate_config(max_entries: usize) -> GcConfig {
    GcConfig::new(max_entries, 2, Duration::ZERO, Duration::ZERO, None, None)
        .expect("valid immediate GC config")
}

fn store(root: &std::path::Path, config: GcConfig) -> FactStore {
    FactStore::with_configs(root, FallbackConfig::default(), config)
}

fn published(
    store: &FactStore,
    directory: &TempDir,
    variant: u64,
    stem: &str,
) -> (SegmentFacts, SegmentContext, std::path::PathBuf) {
    let bytes = lifecycle_pgm(variant);
    let context = context(stem);
    crate::test_layout::write_pgm(directory.path(), context.address(), &bytes);
    let facts = facts(&bytes);
    let path = store
        .publish(&facts, &context, &LIMIT)
        .expect("publish sibling OVF");
    (facts, context, path)
}

#[test]
fn publication_uses_one_same_day_same_id_sidecar() {
    let directory = TempDir::new().expect("data directory");
    let active = crate::test_layout::write_empty_journal(directory.path());
    let store = store(directory.path(), immediate_config(128));

    let (_facts, context, sidecar) = published(&store, &directory, 1, "segment");
    let source = crate::test_layout::file_path(
        directory.path(),
        context.address(),
        kronika_layout::FileKind::Pgm,
    );
    let expected_sidecar = crate::test_layout::file_path(
        directory.path(),
        context.address(),
        kronika_layout::FileKind::Ovf,
    );

    assert_eq!(sidecar, expected_sidecar);
    assert!(active.is_file());
    assert!(source.is_file());
    assert!(sidecar.is_file());
    assert!(
        directory
            .path()
            .join(kronika_layout::OVERVIEW_OWNER_LOCK_NAME)
            .is_file()
    );
    assert!(
        !directory.path().join("overview").exists(),
        "publication must not create a cache tree"
    );
}

#[test]
fn two_scans_of_one_generation_do_not_satisfy_generation_grace() {
    let directory = TempDir::new().expect("data directory");
    let store = store(directory.path(), immediate_config(128));
    let (_facts, context, path) = published(&store, &directory, 2, "segment");
    let source = crate::test_layout::file_path(
        directory.path(),
        context.address(),
        kronika_layout::FileKind::Pgm,
    );
    let mark = GcMark::authoritative(7, []);

    let first = store.collect_garbage(&mark);
    let repeated = store.collect_garbage(&mark);

    assert_eq!(first.deleted, 0);
    assert_eq!(repeated.deleted, 0);
    assert_eq!(repeated.pending, 1);
    assert!(path.is_file());

    let second_generation = store.collect_garbage(&GcMark::authoritative(8, []));
    assert_eq!(second_generation.deleted_sidecars, 1);
    assert!(!path.exists());
    assert!(source.is_file(), "retention never removes source PGM files");
}

#[test]
fn unavailable_and_bounded_marks_never_authorize_deletion() {
    let directory = TempDir::new().expect("data directory");
    let store = store(directory.path(), immediate_config(3));
    let (_facts, _context, path) = published(&store, &directory, 3, "segment");
    let before = std::fs::read(&path).expect("read sidecar");

    let unavailable = store.collect_garbage(&GcMark::unavailable(1));
    assert_eq!(unavailable.skip_reason, Some(GcSkipReason::MarkUnavailable));
    assert_eq!(unavailable.deleted, 0);

    let live = (0_u8..4).map(|byte| {
        FactBuildKey::new(
            FactKey::from_bytes([byte; 32]),
            SegmentLineageId([byte; 32]),
        )
    });
    let capped_live = store.collect_garbage(&GcMark::authoritative(2, live));
    assert_eq!(capped_live.skip_reason, Some(GcSkipReason::LiveSetCapped));
    assert_eq!(capped_live.deleted, 0);

    assert_eq!(std::fs::read(&path).expect("reread sidecar"), before);
}

#[test]
fn bounded_scan_fails_closed_without_advancing_grace() {
    let directory = TempDir::new().expect("data directory");
    let store = store(
        directory.path(),
        GcConfig::new(2, 2, Duration::ZERO, Duration::ZERO, None, None)
            .expect("valid capped config"),
    );
    let (_facts, _context, path) = published(&store, &directory, 4, "segment");

    for generation in [1, 2, 3] {
        let outcome = store.collect_garbage(&GcMark::authoritative(generation, []));
        assert_eq!(outcome.skip_reason, Some(GcSkipReason::ScanCapped));
        assert_eq!(outcome.deleted, 0);
    }
    assert!(path.is_file());
}

#[test]
fn a_foreign_sidecar_symlink_is_skipped_without_blocking_gc() {
    let directory = TempDir::new().expect("data directory");
    let store = store(directory.path(), immediate_config(256));
    let (_facts, context, path) = published(&store, &directory, 5, "segment");
    let source = crate::test_layout::file_path(
        directory.path(),
        context.address(),
        kronika_layout::FileKind::Pgm,
    );
    let active = crate::test_layout::write_empty_journal(directory.path());
    let linked_address = crate::test_layout::named_address("linked");
    let linked = crate::test_layout::file_path(
        directory.path(),
        linked_address,
        kronika_layout::FileKind::Ovf,
    );
    symlink(&source, &linked).expect("create sidecar-shaped symlink");
    let active_before = std::fs::read(&active).expect("read active journal");

    let first = store.collect_garbage(&GcMark::authoritative(2, []));

    assert_eq!(first.skip_reason, None);
    assert_eq!(first.deleted, 0);
    assert_eq!(first.pending, 1);
    let second = store.collect_garbage(&GcMark::authoritative(3, []));
    assert_eq!(second.skip_reason, None);
    assert_eq!(second.deleted, 1);
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(&source).expect("source survives"),
        lifecycle_pgm(5)
    );
    assert_eq!(
        std::fs::read(&active).expect("active view survives"),
        active_before
    );
    assert!(
        std::fs::symlink_metadata(&linked)
            .expect("symlink survives")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn stale_publisher_artifacts_and_invalid_sidecars_are_removed() {
    let directory = TempDir::new().expect("data directory");
    let store = store(directory.path(), immediate_config(256));
    let (facts, context, path) = published(&store, &directory, 6, "segment");
    let day = crate::test_layout::day_path(directory.path(), context.address());
    let temporary = day.join(format!("{}.ovf.12.34.tmp", context.address().id));
    let stale_address = crate::test_layout::named_address("stale");
    let invalid = crate::test_layout::file_path(
        directory.path(),
        stale_address,
        kronika_layout::FileKind::Ovf,
    );
    std::fs::write(&temporary, b"temporary").expect("write publisher artifact");
    std::fs::write(&invalid, b"invalid sidecar").expect("write invalid sidecar");

    let outcome = store.collect_garbage(&GcMark::authoritative(1, [key(&facts)]));

    assert_eq!(outcome.deleted_artifacts, 2);
    assert!(!temporary.exists());
    assert!(!invalid.exists());
    assert!(path.is_file());
}

#[test]
fn data_directory_owner_contention_fails_closed() {
    let directory = TempDir::new().expect("data directory");
    let first = store(directory.path(), immediate_config(128));
    let (_facts, _context, path) = published(&first, &directory, 7, "segment");
    let second = store(directory.path(), immediate_config(128));

    let outcome = second.collect_garbage(&GcMark::authoritative(1, []));

    assert_eq!(outcome.skip_reason, Some(GcSkipReason::OwnerUnavailable));
    assert_eq!(outcome.deleted, 0);
    assert!(path.is_file());
}

#[test]
fn quota_accounts_only_derived_files_in_the_owned_data_directory() {
    let directory = TempDir::new().expect("data directory");
    let bytes = lifecycle_pgm(8);
    let facts = facts(&bytes);
    let encoded_len =
        u64::try_from(facts.encode(&LIMIT).expect("encode facts").len()).expect("encoded size");
    let context = context("segment");
    crate::test_layout::write_pgm(directory.path(), context.address(), &bytes);
    crate::test_layout::write_empty_journal(directory.path());
    let config = GcConfig::new(
        128,
        2,
        Duration::ZERO,
        Duration::ZERO,
        Some(encoded_len),
        Some(2),
    )
    .expect("exact sidecar quota");
    let store = store(directory.path(), config);

    let path = store
        .publish(&facts, &context, &LIMIT)
        .expect("PGM and active view do not consume the derived-file quota");
    let outcome = store.collect_garbage(&GcMark::authoritative(1, [key(&facts)]));

    assert!(path.is_file());
    assert!(!outcome.quota_exceeded);
    assert_eq!(outcome.usage.sidecars.files, 1);
    assert_eq!(outcome.usage.locks.files, 0);
    assert_eq!(outcome.usage.total_files(), 1);
}

#[test]
fn optional_quota_blocks_publication_without_touching_the_source() {
    let directory = TempDir::new().expect("data directory");
    let bytes = lifecycle_pgm(9);
    let facts = facts(&bytes);
    let context = context("segment");
    let source = crate::test_layout::write_pgm(directory.path(), context.address(), &bytes);
    let encoded_len =
        u64::try_from(facts.encode(&LIMIT).expect("encode facts").len()).expect("encoded size");
    let config = GcConfig::new(
        128,
        2,
        Duration::ZERO,
        Duration::ZERO,
        Some(encoded_len - 1),
        None,
    )
    .expect("undersized byte quota");
    let store = store(directory.path(), config);

    assert_eq!(
        store.publish(&facts, &context, &LIMIT),
        Err(crate::PersistError::QuotaExceeded)
    );
    assert_eq!(std::fs::read(&source).expect("source survives"), bytes);
    let sidecar = crate::test_layout::file_path(
        directory.path(),
        context.address(),
        kronika_layout::FileKind::Ovf,
    );
    assert!(!sidecar.exists());
}

#[test]
fn unlinked_bytes_come_from_the_reopened_validated_inode() {
    let directory = TempDir::new().expect("data directory");
    let store = store(directory.path(), immediate_config(128));
    let (_facts, _context, path) = published(&store, &directory, 10, "segment");
    let expected = std::fs::metadata(&path).expect("sidecar metadata").len();

    let _ = store.collect_garbage(&GcMark::authoritative(10, []));
    let outcome = store.collect_garbage(&GcMark::authoritative(11, []));

    assert_eq!(outcome.deleted_sidecars, 1);
    assert_eq!(outcome.unlinked_logical_bytes, expected);
}

#[test]
fn concurrent_live_gc_read_and_publish_preserve_the_sidecar() {
    let directory = TempDir::new().expect("data directory");
    let store = Arc::new(store(directory.path(), immediate_config(256)));
    let (facts, context, path) = published(&store, &directory, 11, "segment");
    let facts = Arc::new(facts);
    let context = Arc::new(context);
    let barrier = Arc::new(Barrier::new(3));

    let publisher = {
        let store = Arc::clone(&store);
        let facts = Arc::clone(&facts);
        let context = Arc::clone(&context);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store.publish(&facts, &context, &LIMIT)
        })
    };
    let collector = {
        let store = Arc::clone(&store);
        let facts = Arc::clone(&facts);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store.collect_garbage(&GcMark::authoritative(1, [key(&facts)]))
        })
    };
    barrier.wait();

    assert_eq!(publisher.join().expect("publisher"), Ok(path.clone()));
    let outcome = collector.join().expect("collector");
    assert_eq!(outcome.deleted_sidecars, 0);
    assert!(path.is_file());
}

#[test]
fn complete_typed_live_set_preserves_each_sibling_sidecar() {
    let directory = TempDir::new().expect("data directory");
    let store = store(directory.path(), immediate_config(256));
    let (first, _first_context, first_path) = published(&store, &directory, 12, "first");
    let (second, _second_context, second_path) = published(&store, &directory, 13, "second");
    let live: HashSet<_> = [key(&first), key(&second)].into_iter().collect();

    for generation in 1..=3 {
        let outcome =
            store.collect_garbage(&GcMark::authoritative(generation, live.iter().copied()));
        assert_eq!(outcome.live, 2);
        assert_eq!(outcome.deleted, 0);
    }
    assert!(first_path.is_file());
    assert!(second_path.is_file());
}
