use std::os::unix::fs::symlink;
use std::sync::{Arc, Barrier};

use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};
use tempfile::TempDir;

use super::*;
use crate::overview::{FactStore, FallbackConfig, SegmentContext, SegmentFacts, placement};

fn context() -> SegmentContext {
    SegmentContext::new(
        b"gc-test".to_vec(),
        NamingContractId([0x33; 16]),
        SegmentLocator([0x44; 32]),
    )
    .expect("valid context")
}

fn lifecycle_pgm(source_id: u64) -> Vec<u8> {
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
            pid: Some(11),
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
            source_id,
        },
    )
}

fn facts(source_id: u64) -> SegmentFacts {
    let bytes = lifecycle_pgm(source_id);
    let unit = crate::PgmUnit::open(bytes.as_slice()).expect("open PGM");
    SegmentFacts::extract(&unit, &context(), &LIMIT).expect("extract facts")
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

fn published(store: &FactStore, facts: &SegmentFacts) -> std::path::PathBuf {
    store.publish(facts, &LIMIT).expect("publish fact")
}

#[test]
fn two_scans_of_one_generation_do_not_satisfy_generation_grace() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(128));
    let facts = facts(1);
    let path = published(&store, &facts);
    let mark = GcMark::authoritative(7, []);

    let first = store.collect_garbage(&mark);
    let repeated = store.collect_garbage(&mark);

    assert_eq!(first.deleted, 0);
    assert_eq!(repeated.deleted, 0);
    assert_eq!(repeated.pending, 1);
    assert!(
        path.is_file(),
        "one view generation cannot authorize deletion"
    );

    let second_generation = store.collect_garbage(&GcMark::authoritative(8, []));
    assert_eq!(second_generation.deleted_finals, 1);
    assert!(
        !path.exists(),
        "a distinct second generation satisfies grace"
    );
}

#[test]
fn unavailable_and_capped_marks_leave_the_namespace_byte_identical() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(1));
    let facts = facts(2);
    let path = published(&store, &facts);
    let before = std::fs::read(&path).expect("read fact");

    let unavailable = store.collect_garbage(&GcMark::unavailable(1));
    assert_eq!(unavailable.skip_reason, Some(GcSkipReason::MarkUnavailable));
    assert_eq!(unavailable.deleted, 0);

    let live = [
        key(&facts),
        FactBuildKey::new(
            FactKey::for_current_segment(
                facts.identity().source_scope_id,
                SourceDescriptor([0x55; 32]),
            ),
            facts.lineage().id(),
        ),
    ];
    let capped = store.collect_garbage(&GcMark::authoritative(2, live));
    assert_eq!(capped.skip_reason, Some(GcSkipReason::LiveSetCapped));
    assert_eq!(capped.deleted, 0);
    assert_eq!(std::fs::read(&path).expect("reread fact"), before);
}

#[test]
fn incomplete_scan_never_advances_grace_or_unlinks_a_valid_final() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(128));
    let facts = facts(3);
    let path = published(&store, &facts);
    let foreign_directory = path.parent().expect("prefix").join("foreign-dir");
    std::fs::create_dir(&foreign_directory).expect("create foreign directory");

    for generation in [1, 2, 3] {
        let outcome = store.collect_garbage(&GcMark::authoritative(generation, []));
        assert_eq!(outcome.skip_reason, Some(GcSkipReason::ScanError));
        assert_eq!(outcome.deleted, 0);
    }
    assert!(path.is_file());
}

#[test]
fn exact_namespace_filter_never_follows_or_deletes_foreign_and_lock_entries() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(256));
    let facts = facts(4);
    let path = published(&store, &facts);
    let prefix = path.parent().expect("prefix");
    let prefix_name = prefix
        .file_name()
        .expect("prefix name")
        .to_str()
        .expect("UTF-8 prefix");
    let foreign = prefix.join("foreign.ovf");
    std::fs::write(&foreign, b"foreign authority").expect("write foreign file");
    let victim = directory.path().join("source.pgm");
    std::fs::write(&victim, b"source authority").expect("write source");
    let linked_name = format!("{prefix_name}{}-{}.ovf", "0".repeat(62), "1".repeat(64));
    let linked = prefix.join(linked_name);
    symlink(&victim, &linked).expect("plant candidate-shaped symlink");
    let lock = prefix.join(format!(
        ".lock-{}-{}",
        key(&facts).fact_key().hex(),
        hex(&key(&facts).segment_lineage_id().0)
    ));
    let lock_before = std::fs::read(&lock).expect("read lock");

    let _ = store.collect_garbage(&GcMark::authoritative(1, []));
    let outcome = store.collect_garbage(&GcMark::authoritative(2, []));

    assert_eq!(outcome.deleted_finals, 1);
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(&foreign).expect("foreign survives"),
        b"foreign authority"
    );
    assert_eq!(
        std::fs::read(&victim).expect("source survives"),
        b"source authority"
    );
    assert!(
        std::fs::symlink_metadata(&linked)
            .expect("link survives")
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&lock).expect("lock survives"), lock_before);
    assert!(outcome.usage.foreign.files >= 2);
    assert!(outcome.usage.locks.files >= 2);
}

#[test]
fn only_strictly_recognized_old_artifacts_are_removed() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(256));
    let facts = facts(5);
    let path = published(&store, &facts);
    let prefix = path.parent().expect("prefix");
    let prefix_name = prefix
        .file_name()
        .expect("prefix name")
        .to_str()
        .expect("UTF-8 prefix");
    let temporary = prefix.join(format!(".tmp-12-34-{prefix_name}"));
    let quarantine = prefix.join(format!(".bad-12-35-{prefix_name}"));
    let lookalike = prefix.join(format!(".tmp-active-{prefix_name}"));
    std::fs::write(&temporary, b"temp").expect("write temp");
    std::fs::write(&quarantine, b"bad").expect("write quarantine");
    std::fs::write(&lookalike, b"foreign").expect("write lookalike");

    let outcome = store.collect_garbage(&GcMark::authoritative(1, [key(&facts)]));

    assert_eq!(outcome.deleted_artifacts, 2);
    assert!(!temporary.exists());
    assert!(!quarantine.exists());
    assert_eq!(
        std::fs::read(&lookalike).expect("lookalike survives"),
        b"foreign"
    );
    assert!(path.is_file(), "the live committed final survives");
}

#[test]
fn root_owner_contention_fails_closed() {
    let directory = TempDir::new().expect("cache directory");
    let first = store(directory.path(), immediate_config(128));
    let facts = facts(6);
    let path = published(&first, &facts);
    let second = store(directory.path(), immediate_config(128));

    let outcome = second.collect_garbage(&GcMark::authoritative(1, []));

    assert_eq!(outcome.skip_reason, Some(GcSkipReason::OwnerUnavailable));
    assert_eq!(outcome.deleted, 0);
    assert!(path.is_file());
}

#[test]
fn quota_overrun_keeps_live_fact_and_files_outside_the_exact_namespace() {
    let directory = TempDir::new().expect("cache directory");
    let unbounded = store(directory.path(), immediate_config(256));
    let facts = facts(7);
    let path = published(&unbounded, &facts);
    drop(unbounded);
    let source = directory.path().join("active.parts");
    std::fs::write(&source, b"source bytes").expect("write source fixture");
    let config = GcConfig::new(256, 2, Duration::ZERO, Duration::ZERO, Some(1), Some(1))
        .expect("tiny quota");
    let bounded = store(directory.path(), config);

    let outcome = bounded.collect_garbage(&GcMark::authoritative(1, [key(&facts)]));

    assert!(outcome.quota_exceeded);
    assert_eq!(outcome.deleted_finals, 0);
    assert!(path.is_file());
    assert_eq!(
        std::fs::read(&source).expect("source survives"),
        b"source bytes"
    );
}

#[test]
fn unlinked_bytes_come_from_the_reopened_validated_inode() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(128));
    let facts = facts(8);
    let path = published(&store, &facts);
    let expected = std::fs::metadata(&path).expect("fact metadata").len();

    let _ = store.collect_garbage(&GcMark::authoritative(10, []));
    let outcome = store.collect_garbage(&GcMark::authoritative(11, []));

    assert_eq!(outcome.deleted_finals, 1);
    assert_eq!(outcome.unlinked_logical_bytes, expected);
}

#[test]
fn corrupted_header_is_foreign_and_never_destructive_authority() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(128));
    let facts = facts(9);
    let path = published(&store, &facts);
    let mut bytes = std::fs::read(&path).expect("read fact");
    bytes[64] ^= 0xff;
    std::fs::write(&path, &bytes).expect("damage header identity");

    for generation in [1, 2, 3] {
        let outcome = store.collect_garbage(&GcMark::authoritative(generation, []));
        assert_eq!(outcome.deleted_finals, 0);
        assert_eq!(outcome.usage.committed.files, 0);
        assert!(outcome.usage.foreign.files >= 1);
    }
    assert_eq!(std::fs::read(&path).expect("corrupt file survives"), bytes);
}

#[test]
fn concurrent_live_gc_read_and_publish_preserve_the_final() {
    let directory = TempDir::new().expect("cache directory");
    let store = Arc::new(store(directory.path(), immediate_config(256)));
    let facts = Arc::new(facts(10));
    let path = published(&store, &facts);
    let barrier = Arc::new(Barrier::new(3));

    let publisher = {
        let store = Arc::clone(&store);
        let facts = Arc::clone(&facts);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store.publish(&facts, &LIMIT)
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
    assert_eq!(outcome.deleted_finals, 0);
    assert!(path.is_file());
}

#[test]
fn optional_quota_blocks_new_publication_without_touching_source() {
    let directory = TempDir::new().expect("cache directory");
    let source = directory.path().join("source.pgm");
    std::fs::write(&source, b"source bytes").expect("write source");
    let config = GcConfig::new(128, 2, Duration::ZERO, Duration::ZERO, None, Some(1))
        .expect("one-file quota");
    let store = store(directory.path(), config);
    let facts = facts(11);

    assert_eq!(
        store.publish(&facts, &LIMIT),
        Err(crate::PersistError::QuotaExceeded)
    );
    assert_eq!(
        std::fs::read(&source).expect("source survives"),
        b"source bytes"
    );
    let expected = placement(
        directory.path(),
        facts.identity().source_scope_id,
        &key(&facts).fact_key(),
        facts.lineage().id(),
    );
    assert!(!expected.exists());
}

#[test]
fn typed_live_set_is_complete_without_path_aliasing() {
    let directory = TempDir::new().expect("cache directory");
    let store = store(directory.path(), immediate_config(256));
    let first = facts(12);
    let second = facts(13);
    let first_path = published(&store, &first);
    let second_path = published(&store, &second);
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
