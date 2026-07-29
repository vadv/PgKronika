use crate::buffering::push_activity;
use crate::plans_source::PlansSourceCache;
use crate::scheduler::{Intervals, Scheduler, SourceKind};
use crate::segments::{
    SegmentState, open_collector_journal, quarantine_invalid_segments,
    seal_open_segment_with_reset, seal_reason,
};
use crate::source_contracts::activity_dict_limits;
use crate::{
    acquire_collector_writer, cleanup_writer_temporaries, prepare_collector_storage,
    stop_if_persistence_unhealthy, timer_sleep_delay,
};
use kronika_layout::{DataRoot, LayoutLimits, SegmentAddress, SegmentId};
use kronika_source_pg::{ActivityRow, ActivityVersion};
use kronika_writer::{Interner, JournalError, SectionBuffers, dict};

fn quarantine_payloads(root: &std::path::Path) -> Vec<Vec<u8>> {
    let quarantine = root.join(".pgkronika-quarantine-v1");
    let mut payloads = std::fs::read_dir(quarantine)
        .expect("read quarantine")
        .map(|entry| {
            std::fs::read(entry.expect("read quarantine entry").path())
                .expect("read quarantined evidence")
        })
        .collect::<Vec<_>>();
    payloads.sort();
    payloads
}

#[test]
fn segment_seals_on_force_zero_cap_size_or_age() {
    assert_eq!(
        seal_reason(true, 0, u64::MAX, false),
        Some("forced"),
        "force always seals"
    );
    assert_eq!(
        seal_reason(false, 1, 0, false),
        Some("tick"),
        "zero cap seals every tick"
    );
    assert_eq!(
        seal_reason(false, 64, 64, false),
        Some("size"),
        "size cap reached"
    );
    assert_eq!(seal_reason(false, 63, 64, false), None, "under the cap");
    assert_eq!(
        seal_reason(false, 1, u64::MAX, true),
        Some("age"),
        "age cap reached"
    );
    assert_eq!(
        seal_reason(true, 64, 64, true),
        Some("forced"),
        "the forced reason outranks size and age"
    );
}

#[test]
fn segment_state_opens_on_the_first_window_only() {
    use std::time::{Duration, Instant};

    let mut segment = SegmentState::default();
    let now = Instant::now();
    assert!(!segment.age_expired(now, Duration::from_secs(1)));
    segment.on_window_appended(SegmentId::new(100).unwrap(), now);
    segment.on_window_appended(SegmentId::new(200).unwrap(), now + Duration::from_secs(5));
    assert_eq!(
        segment.first_ts(),
        Some(100),
        "the first window names the file"
    );
    assert!(segment.age_expired(now + Duration::from_secs(5), Duration::from_secs(5)));
    assert!(!segment.age_expired(now + Duration::from_secs(4), Duration::from_secs(5)));
}

#[test]
fn plans_cache_is_due_without_a_deadline_and_after_it() {
    use std::time::{Duration, Instant};

    let mut cache = PlansSourceCache::default();
    let now = Instant::now();
    assert!(cache.is_due(now), "a fresh cache reads immediately");
    cache.next_read = Some(now + Duration::from_mins(5));
    assert!(!cache.is_due(now), "before the deadline nothing is due");
    assert!(
        cache.is_due(now + Duration::from_mins(5)),
        "the deadline itself is due"
    );
}

#[test]
fn timer_sleep_uses_source_deadline_before_regular_tick() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let intervals = Intervals {
        activity: 1,
        ..Intervals::default()
    };
    let mut sched = Scheduler::new(intervals);
    sched.plan(start, false);

    assert_eq!(
        timer_sleep_delay(
            start,
            5,
            900,
            &sched,
            &PlansSourceCache::default(),
            &SegmentState::default(),
            None,
        ),
        Some(Duration::from_secs(1)),
        "a 1s source interval is not capped by a 5s regular wake"
    );
}

#[test]
fn timer_sleep_uses_accelerated_deadline_before_regular_tick() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let mut sched = Scheduler::new(Intervals::default());
    sched.plan(start, false);
    assert!(sched.accelerate(SourceKind::Activity, 1));

    assert_eq!(
        timer_sleep_delay(
            start,
            5,
            900,
            &sched,
            &PlansSourceCache::default(),
            &SegmentState::default(),
            None,
        ),
        Some(Duration::from_secs(1)),
        "default activity fast pace can wake before the 5s regular timer"
    );
}

#[test]
fn timer_sleep_keeps_zero_interval_on_regular_wakes() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let intervals = Intervals {
        activity: 0,
        ..Intervals::default()
    };
    let mut sched = Scheduler::new(intervals);
    sched.plan(start, false);

    assert_eq!(
        timer_sleep_delay(
            start,
            5,
            900,
            &sched,
            &PlansSourceCache::default(),
            &SegmentState::default(),
            None,
        ),
        Some(Duration::from_secs(5)),
        "zero means every timer wake, not an immediate busy loop"
    );
}
fn client_row_at(pid: i32, ts: i64) -> ActivityRow {
    ActivityRow {
        ts,
        pid,
        leader_pid: None,
        datname: Some("appdb".to_owned()),
        usename: Some("alice".to_owned()),
        application_name: "psql".to_owned(),
        client_addr: String::new(),
        backend_type: "client backend".to_owned(),
        state: Some("active".to_owned()),
        wait_event_type: None,
        wait_event: None,
        query: Some("select 1".to_owned()),
        query_id: Some(42),
        backend_xid_age: None,
        backend_xmin_age: Some(7),
        backend_start: 100,
        xact_start: Some(500),
        query_start: Some(800),
        state_change: Some(900),
    }
}

/// One encoded collection window holding a single activity row.
fn activity_window() -> Vec<u8> {
    activity_window_at(1_000)
}

fn activity_window_at(ts: i64) -> Vec<u8> {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_activity(
        &mut buffers,
        &mut interner,
        ActivityVersion::V3,
        &[client_row_at(7, ts)],
    )
    .expect("push interns and buffers");
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    buffers
        .flush(&dict_sections)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part")
}

#[test]
fn first_window_identity_owns_the_segment_across_utc_year_and_late_data() {
    use kronika_format::{Catalog, TAIL_INDEX_LEN, TailIndex};
    use kronika_writer::{Journal, JournalConfig};

    const FIRST_WINDOW: i64 = 1_735_689_540_000_000; // 2024-12-31 23:59:00 UTC
    const NEXT_YEAR_WINDOW: i64 = FIRST_WINDOW + 120_000_000;
    const LATE_SAMPLE: i64 = FIRST_WINDOW - 3_600_000_000;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let first_id = SegmentId::new(FIRST_WINDOW).unwrap();
    let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
    let mut segment = SegmentState::default();

    for sample_ts in [FIRST_WINDOW, NEXT_YEAR_WINDOW, LATE_SAMPLE] {
        journal
            .append(first_id, &activity_window_at(sample_ts))
            .expect("append one window to the first generation");
        segment.on_window_appended(first_id, std::time::Instant::now());
    }

    let dest =
        seal_open_segment_with_reset(&mut journal, &owner, &mut segment, "test", Journal::reset)
            .expect("seal cross-year segment");
    let first_address = SegmentAddress::new(first_id).unwrap();
    assert_eq!(
        dest,
        root.diagnostic_file_path(first_address, kronika_layout::FileKind::Pgm),
        "the first successful window selects the UTC bucket and file name"
    );
    let next_address = SegmentAddress::new(SegmentId::new(NEXT_YEAR_WINDOW).unwrap()).unwrap();
    assert!(
        !root
            .diagnostic_file_path(next_address, kronika_layout::FileKind::Pgm)
            .exists(),
        "a later window in the next UTC year must not move the open segment"
    );

    let bytes = std::fs::read(&dest).expect("read sealed segment");
    let tail_at = bytes.len() - TAIL_INDEX_LEN;
    let tail = TailIndex::decode(
        bytes[tail_at..]
            .try_into()
            .expect("tail index has the fixed encoded length"),
    )
    .expect("decode tail index");
    let catalog_at =
        tail_at - usize::try_from(tail.catalog_len).expect("catalog length fits the fixture");
    let catalog = Catalog::decode(&bytes[catalog_at..tail_at]).expect("decode sealed catalog");
    assert_eq!(
        catalog.min_ts, LATE_SAMPLE,
        "late source data may precede SegmentId without changing the physical address"
    );
    assert_eq!(catalog.max_ts, NEXT_YEAR_WINDOW);
}

#[test]
fn startup_seals_windows_a_dead_process_left_in_the_journal() {
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let segment_id = SegmentId::new(1_000).unwrap();
    {
        let mut journal =
            Journal::open(&owner, JournalConfig::default()).expect("open the journal");
        journal
            .append(segment_id, &activity_window())
            .expect("append");
        // Dropping without seal is the crash: the file stays behind.
    }

    let (journal, recovered) = open_collector_journal(&owner, 1 << 30).expect("reopen the journal");
    let dest = recovered.expect("leftover windows must become a segment");
    let address = SegmentAddress::new(segment_id).unwrap();
    assert_eq!(
        dest,
        root.diagnostic_file_path(address, kronika_layout::FileKind::Pgm)
    );
    assert!(dest.exists(), "the recovered segment is on disk");
    assert!(journal.parts().is_empty(), "the journal restarts empty");
}

#[test]
fn startup_finishes_a_segment_published_before_journal_reset() {
    use kronika_writer::{Journal, JournalConfig, seal};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let segment_id = SegmentId::new(1_000).expect("valid segment id");
    let address = SegmentAddress::new(segment_id).expect("valid UTC address");
    let dest = root.diagnostic_file_path(address, kronika_layout::FileKind::Pgm);
    {
        let mut journal =
            Journal::open(&owner, JournalConfig::default()).expect("open the journal");
        journal
            .append(segment_id, &activity_window_at(999))
            .expect("append");
        seal(&journal, &owner, address).expect("publish before simulated crash");
        assert!(!journal.parts().is_empty(), "the crash precedes reset");
    }
    let published = std::fs::read(&dest).expect("read published segment");

    let (journal, recovered) =
        open_collector_journal(&owner, 1 << 30).expect("finish interrupted publication");
    assert_eq!(recovered, Some(dest.clone()));
    assert!(journal.parts().is_empty());
    assert_eq!(
        std::fs::read(dest).expect("read recovered segment"),
        published
    );
    let data_address =
        SegmentAddress::new(SegmentId::new(999).expect("valid earlier data timestamp"))
            .expect("valid earlier data address");
    assert!(
        !root
            .diagnostic_file_path(data_address, kronika_layout::FileKind::Pgm)
            .exists(),
        "recovery must not derive a second destination from the earlier data timestamp"
    );
    assert_eq!(
        root.scan(LayoutLimits::default())
            .expect("scan recovered root")
            .segments
            .len(),
        1,
        "the interrupted publication leaves exactly one canonical PGM"
    );
}

#[test]
fn startup_with_an_empty_journal_recovers_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let (journal, recovered) = open_collector_journal(&owner, 1 << 30).expect("open the journal");
    assert!(recovered.is_none());
    assert!(journal.parts().is_empty());
}

#[test]
fn startup_quarantines_a_torn_header_and_accepts_future_windows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let evidence = b"PGKJNL1".to_vec();
    std::fs::write(dir.path().join("active.parts"), &evidence).expect("write torn header");

    let (mut journal, recovered) =
        open_collector_journal(&owner, 1 << 30).expect("recover torn journal header");

    assert!(recovered.is_none());
    assert!(journal.parts().is_empty());
    assert_eq!(quarantine_payloads(dir.path()), vec![evidence]);
    journal
        .append(SegmentId::new(2_000).unwrap(), &activity_window())
        .expect("collection continues in the fresh journal");
}

#[test]
fn startup_recovers_complete_frames_despite_a_wrong_recorded_body_length() {
    use kronika_format::{JOURNAL_HEADER_LEN, JournalHeader, JournalState};
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let segment_id = SegmentId::new(1_000).unwrap();
    {
        let mut journal =
            Journal::open(&owner, JournalConfig::default()).expect("open the journal");
        journal
            .append(segment_id, &activity_window())
            .expect("append valid frame");
    }
    let active = dir.path().join("active.parts");
    let mut evidence = std::fs::read(&active).expect("read active journal");
    let physical_body_len = u64::try_from(evidence.len() - JOURNAL_HEADER_LEN).unwrap();
    evidence[..JOURNAL_HEADER_LEN].copy_from_slice(
        &JournalHeader {
            state: JournalState::Active {
                segment_id: segment_id.get(),
            },
            body_len: physical_body_len + 17,
        }
        .encode(),
    );
    std::fs::write(&active, &evidence).expect("write mismatched header");

    let (journal, recovered) =
        open_collector_journal(&owner, 1 << 30).expect("recover verified physical frame");

    assert!(journal.parts().is_empty());
    assert!(recovered.expect("verified frame is sealed").is_file());
    assert_eq!(quarantine_payloads(dir.path()), vec![evidence]);
}

#[test]
fn startup_finishes_recovery_pending_evidence_after_activation() {
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let segment_id = SegmentId::new(1_000).unwrap();
    {
        let mut journal =
            Journal::open(&owner, JournalConfig::default()).expect("open the journal");
        journal
            .append(segment_id, &activity_window())
            .expect("append valid frame");
    }
    let evidence = std::fs::read(dir.path().join("active.parts")).expect("read evidence");
    let mut rotation = owner
        .begin_journal_rotation()
        .expect("begin exact-evidence rotation");
    Journal::prepare_rotation(&mut rotation).expect("prepare fresh journal");
    let activated = rotation.activate();
    drop(activated);

    let (journal, recovered) =
        open_collector_journal(&owner, 1 << 30).expect("finish pending evidence recovery");

    assert!(journal.parts().is_empty());
    assert!(recovered.expect("pending evidence is sealed").is_file());
    assert_eq!(quarantine_payloads(dir.path()), vec![evidence]);
}

#[test]
fn startup_recovers_a_pending_alternate_generation() {
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let canonical =
        Journal::open(&owner, JournalConfig::default()).expect("create canonical journal");
    drop(canonical);
    let mut generation = owner
        .create_journal_generation()
        .expect("create alternate generation");
    Journal::prepare_slot(&mut generation.slot).expect("prepare alternate generation");
    let mut alternate = Journal::open_slot(generation.slot, JournalConfig::default())
        .expect("open alternate generation");
    alternate
        .append(SegmentId::new(1_000).unwrap(), &activity_window())
        .expect("append alternate frame");
    drop(alternate);

    let (journal, recovered) =
        open_collector_journal(&owner, 1 << 30).expect("recover pending generation");

    assert!(journal.parts().is_empty());
    assert!(recovered.expect("pending generation is sealed").is_file());
    assert_eq!(quarantine_payloads(dir.path()).len(), 1);
}

#[test]
fn failed_recovery_seal_preserves_evidence_and_continues_empty() {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let path = dir.path().join("active.parts");
    let segment_id = SegmentId::new(123).expect("valid segment id");
    let part = build_part(
        &[SectionInput {
            type_id: 9_999_999,
            rows: 1,
            body: b"not a registered Parquet section",
        }],
        PartMeta {
            min_ts: 123,
            max_ts: 123,
        },
    );
    {
        let mut journal =
            Journal::open(&owner, JournalConfig::default()).expect("open the journal");
        journal
            .append(segment_id, &part)
            .expect("append a structurally valid part");
    }
    let expected_evidence = std::fs::read(&path).expect("read exact journal evidence");

    let (mut journal, recovered) = open_collector_journal(&owner, 1 << 30)
        .expect("an unknown recovered section degrades locally");
    assert!(recovered.is_none());
    assert!(journal.parts().is_empty());
    assert_eq!(
        quarantine_payloads(dir.path()),
        vec![expected_evidence],
        "the exact unsealable journal is preserved"
    );
    journal
        .append(SegmentId::new(456).unwrap(), &activity_window())
        .expect("fresh collection continues");
}

#[test]
fn startup_quarantines_only_stale_writer_temporaries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let day = dir.path().join("1970/01/01");
    std::fs::create_dir_all(&day).expect("create canonical day");
    let stale_pgm = day.join("1000.pgm.4242.0.tmp");
    let overview_temp = day.join("1000.ovf.4242.1.tmp");
    std::fs::write(&stale_pgm, b"incomplete PGM").expect("write stale writer temporary");
    std::fs::write(&overview_temp, b"incomplete OVF").expect("write overview temporary");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");

    assert_eq!(
        cleanup_writer_temporaries(&owner, LayoutLimits::default()).expect("cleanup"),
        1
    );
    assert!(!stale_pgm.exists());
    assert!(
        quarantine_payloads(dir.path())
            .iter()
            .any(|bytes| bytes == b"incomplete PGM"),
        "the stale writer temporary is preserved as evidence"
    );
    assert!(
        overview_temp.exists(),
        "the collector must not clean another owner's temporary"
    );
}

#[test]
fn published_pgm_with_failed_reset_requires_restart_before_another_append() {
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let segment_id = SegmentId::new(1_000).unwrap();
    let address = SegmentAddress::new(segment_id).unwrap();
    let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
    journal
        .append(segment_id, &activity_window())
        .expect("append source window");
    let journal_before = std::fs::read(dir.path().join("active.parts")).expect("read journal");
    let mut segment = SegmentState::default();
    segment.on_window_appended(segment_id, std::time::Instant::now());

    let error =
        seal_open_segment_with_reset(&mut journal, &owner, &mut segment, "test", |_journal| {
            Err(JournalError::Io(std::io::Error::other(
                "injected reset failure",
            )))
        })
        .expect_err("reset failure after publication must be terminal");

    assert!(
        error.to_string().contains("reset the journal after seal"),
        "the failure identifies the post-publication reset"
    );
    assert!(
        root.diagnostic_file_path(address, kronika_layout::FileKind::Pgm)
            .is_file(),
        "the PGM was published before the injected reset failure"
    );
    assert_eq!(
        std::fs::read(dir.path().join("active.parts")).expect("reread journal"),
        journal_before,
        "the injected reset did not discard the active source"
    );
    assert!(segment.ensure_append_allowed().is_err());
    assert!(
        stop_if_persistence_unhealthy(&journal, &segment).is_err(),
        "the main loop must exit before another append"
    );
}

#[test]
fn startup_validation_quarantines_body_and_catalog_corruption() {
    use kronika_format::{MAGIC, TAIL_INDEX_LEN, TailIndex};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let address = SegmentAddress::new(SegmentId::new(1_000).unwrap()).unwrap();
    let path = root.diagnostic_file_path(address, kronika_layout::FileKind::Pgm);
    std::fs::create_dir_all(path.parent().expect("PGM has a day directory"))
        .expect("create canonical day");

    let mut body_corrupt = activity_window();
    let tail_at = body_corrupt.len() - TAIL_INDEX_LEN;
    let tail = TailIndex::decode(body_corrupt[tail_at..].try_into().unwrap()).expect("valid tail");
    let catalog_at = tail_at - usize::try_from(tail.catalog_len).unwrap();
    assert!(catalog_at > MAGIC.len(), "fixture contains section bodies");
    body_corrupt[MAGIC.len()] ^= 0xff;
    std::fs::write(&path, &body_corrupt).expect("write body-corrupt PGM");

    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    assert_eq!(
        quarantine_invalid_segments(&owner, LayoutLimits::default())
            .expect("quarantine body-corrupt PGM"),
        0
    );
    assert!(!path.exists());

    let mut catalog_corrupt = activity_window();
    let tail_at = catalog_corrupt.len() - TAIL_INDEX_LEN;
    let tail =
        TailIndex::decode(catalog_corrupt[tail_at..].try_into().unwrap()).expect("valid tail");
    let catalog_at = tail_at - usize::try_from(tail.catalog_len).unwrap();
    catalog_corrupt[catalog_at] ^= 0xff;
    std::fs::write(&path, catalog_corrupt).expect("write catalog-corrupt PGM");
    assert_eq!(
        quarantine_invalid_segments(&owner, LayoutLimits::default())
            .expect("quarantine catalog-corrupt PGM"),
        0
    );
    assert!(!path.exists());
    assert_eq!(quarantine_payloads(dir.path()).len(), 2);
}

#[test]
fn corrupt_existing_pgm_does_not_block_active_journal_recovery() {
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");

    let active_id = SegmentId::new(2_000).unwrap();
    {
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        journal
            .append(active_id, &activity_window())
            .expect("append recoverable window");
    }
    let corrupt_address = SegmentAddress::new(SegmentId::new(1_000).unwrap()).unwrap();
    let corrupt_path = root.diagnostic_file_path(corrupt_address, kronika_layout::FileKind::Pgm);
    std::fs::create_dir_all(corrupt_path.parent().expect("PGM has a day directory"))
        .expect("create canonical day");
    std::fs::write(&corrupt_path, b"not a PGM").expect("write corrupt canonical PGM");

    let (journal, recovered) = prepare_collector_storage(&owner, LayoutLimits::default(), 1 << 30)
        .expect("localized PGM corruption does not reject startup");
    assert!(journal.parts().is_empty());
    let active_address = SegmentAddress::new(active_id).unwrap();
    assert!(
        root.diagnostic_file_path(active_address, kronika_layout::FileKind::Pgm)
            .exists(),
        "the valid active journal is still sealed"
    );
    assert!(recovered.is_some());
    assert!(!corrupt_path.exists());
    assert!(
        quarantine_payloads(dir.path())
            .iter()
            .any(|bytes| bytes == b"not a PGM")
    );
}

#[test]
fn corrupt_existing_pgm_does_not_block_writer_ownership() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(dir.path()).expect("open data root");
    let corrupt_address = SegmentAddress::new(SegmentId::new(1_000).unwrap()).unwrap();
    let corrupt_path = root.diagnostic_file_path(corrupt_address, kronika_layout::FileKind::Pgm);
    std::fs::create_dir_all(corrupt_path.parent().expect("PGM has a day directory"))
        .expect("create canonical day");
    std::fs::write(&corrupt_path, b"not a PGM").expect("write corrupt canonical PGM");

    let owner = acquire_collector_writer(&root, LayoutLimits::default())
        .expect("localized PGM corruption must not reject writer ownership");
    assert!(
        dir.path()
            .join(kronika_layout::WRITER_OWNER_LOCK_NAME)
            .exists(),
        "writer ownership uses the persistent lock"
    );
    assert_eq!(
        quarantine_invalid_segments(&owner, LayoutLimits::default())
            .expect("quarantine invalid PGM"),
        0
    );
    assert!(!corrupt_path.exists());
}
