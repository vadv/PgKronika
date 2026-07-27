use std::collections::HashSet;
use std::io;
use std::sync::{Arc, Barrier, Mutex};

use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};
use tempfile::TempDir;

use super::super::facts::{SegmentContext, SegmentFacts};
use super::super::gc::GcMark;
use super::super::limits::LIMIT;
use super::super::publish::{FactStore, PersistFailureClass, PersistenceProbeOutcome};
use super::*;
use crate::PgmUnit;

fn reservation_id(reservation: Reservation) -> u64 {
    match reservation {
        Reservation::Acquired(reservation) => reservation,
        other => panic!("expected acquired reservation, got {other:?}"),
    }
}

fn context() -> SegmentContext {
    SegmentContext::new(crate::test_layout::named_address("persist-mode-tests"))
}

fn lifecycle_pgm() -> Vec<u8> {
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
            source_id: 7,
        },
    )
}

fn facts(bytes: &[u8]) -> SegmentFacts {
    let unit = PgmUnit::open(bytes).expect("open PGM");
    SegmentFacts::extract(&unit, &LIMIT).expect("extract facts")
}

fn write_source(directory: &TempDir, bytes: &[u8]) {
    crate::test_layout::write_pgm(directory.path(), context().address(), bytes);
}

fn seed_authoritative_mark(store: &FactStore) {
    let outcome = store.collect_garbage(&GcMark::authoritative(1, []));
    assert!(outcome.scan_complete);
    assert!(outcome.sweep_authorized);
}

#[test]
fn recoverable_failure_suppresses_writes_until_one_success_resets_state() {
    let now = Instant::now();
    let mut state = PersistState::new(7);
    let first = reservation_id(state.reserve_write(now));
    state.finish(first, Err(PersistError::TransientIo), now);

    assert_eq!(
        state.reserve_write(now),
        Reservation::Backoff(PersistError::TransientIo)
    );
    let backed_off = state.snapshot(now);
    assert_eq!(backed_off.mode, PersistMode::UnavailableBackoff);
    assert_eq!(backed_off.failures, 1);
    assert_eq!(backed_off.reason, Some(PersistError::TransientIo));
    assert!(backed_off.retry_after > Duration::ZERO);

    state.force_due(now);
    let recovery = reservation_id(state.reserve_write(now));
    state.finish(recovery, Ok(()), now);
    assert_eq!(
        state.snapshot(now),
        PersistModeSnapshot {
            mode: PersistMode::ReadWrite,
            failures: 0,
            reason: None,
            retry_after: Duration::ZERO,
            probe_in_flight: false,
        }
    );
}

#[test]
fn read_only_and_permission_failures_start_at_the_five_minute_cap() {
    let now = Instant::now();
    for error in [
        PersistError::ReadOnlyFilesystem,
        PersistError::PermissionDenied,
    ] {
        let mut state = PersistState::new(11);
        let reservation = reservation_id(state.reserve_write(now));
        state.finish(reservation, Err(error), now);
        assert_eq!(state.snapshot(now).retry_after, MAX_BACKOFF);
        assert_eq!(state.snapshot(now).mode, PersistMode::ReadOnlyBackoff);
    }
}

#[test]
fn exponential_jitter_never_exceeds_the_five_minute_cap() {
    let now = Instant::now();
    let mut state = PersistState::new(u64::MAX);
    for _ in 0..64 {
        state.force_due(now);
        let reservation = reservation_id(state.reserve_write(now));
        state.finish(reservation, Err(PersistError::NoSpace), now);
        assert!(state.snapshot(now).retry_after <= MAX_BACKOFF);
    }
}

#[test]
fn jitter_is_repeatable_per_seed_and_desynchronizes_stores() {
    let same_a = backoff_delay(PersistError::NoSpace, 8, 0x1234);
    let same_b = backoff_delay(PersistError::NoSpace, 8, 0x1234);
    assert_eq!(same_a, same_b);

    let delays: HashSet<_> = (1..=16)
        .map(|seed| backoff_delay(PersistError::NoSpace, 8, seed))
        .collect();
    assert!(
        delays.len() > 1,
        "independent store seeds must spread retries"
    );
}

#[test]
fn concurrent_due_callers_create_exactly_one_reservation() {
    const CALLERS: usize = 8;
    let now = Instant::now();
    let mut initial = PersistState::new(19);
    let reservation = reservation_id(initial.reserve_write(now));
    initial.finish(reservation, Err(PersistError::TransientIo), now);
    initial.force_due(now);

    let state = Arc::new(Mutex::new(initial));
    let barrier = Arc::new(Barrier::new(CALLERS + 1));
    let mut callers = Vec::new();
    for _ in 0..CALLERS {
        let state = Arc::clone(&state);
        let barrier = Arc::clone(&barrier);
        callers.push(std::thread::spawn(move || {
            barrier.wait();
            state.lock().expect("state lock").reserve_due_probe(now)
        }));
    }
    barrier.wait();
    let outcomes: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("caller"))
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Reservation::Acquired(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Reservation::InFlight))
            .count(),
        CALLERS - 1
    );
}

#[test]
fn cancellation_releases_the_flight_without_erasing_the_standing_failure() {
    let now = Instant::now();
    let mut state = PersistState::new(23);
    let first = reservation_id(state.reserve_write(now));
    state.finish(first, Err(PersistError::TransientIo), now);
    state.force_due(now);
    let cancelled = reservation_id(state.reserve_due_probe(now));
    state.cancel(cancelled, now);
    assert_eq!(state.snapshot(now).reason, Some(PersistError::TransientIo));
    state.force_due(now);
    assert!(matches!(
        state.reserve_due_probe(now),
        Reservation::Acquired(_)
    ));
}

#[test]
fn contention_and_permanent_failures_do_not_arm_global_backoff() {
    let now = Instant::now();
    for error in [
        PersistError::Busy,
        PersistError::InvalidFacts,
        PersistError::UnsafePath,
        PersistError::InvalidSidecarState,
        PersistError::Io,
    ] {
        let mut state = PersistState::new(29);
        let reservation = reservation_id(state.reserve_write(now));
        state.finish(reservation, Err(error), now);
        assert_eq!(state.snapshot(now).failures, 0, "{error:?}");
        assert_eq!(
            state.snapshot(now).mode,
            PersistMode::ReadWrite,
            "{error:?}"
        );
    }
}

#[test]
fn permanent_failure_during_recovery_does_not_clear_the_standing_reason() {
    let now = Instant::now();
    let mut state = PersistState::new(31);
    let first = reservation_id(state.reserve_write(now));
    state.finish(first, Err(PersistError::TransientIo), now);
    state.force_due(now);
    let recovery = reservation_id(state.reserve_due_probe(now));
    state.finish(recovery, Err(PersistError::InvalidSidecarState), now);
    let snapshot = state.snapshot(now);
    assert_eq!(snapshot.failures, 1);
    assert_eq!(snapshot.reason, Some(PersistError::TransientIo));
    assert!(snapshot.retry_after > Duration::ZERO);
}

#[test]
fn persistence_errors_have_closed_recovery_classes() {
    assert_eq!(
        PersistError::ReadOnlyFilesystem.class(),
        PersistFailureClass::ReadOnly
    );
    assert_eq!(
        PersistError::PermissionDenied.class(),
        PersistFailureClass::Permission
    );
    for error in [PersistError::NoSpace, PersistError::QuotaExceeded] {
        assert_eq!(error.class(), PersistFailureClass::Capacity);
    }
    for error in [PersistError::TransientIo, PersistError::StaleFilesystem] {
        assert_eq!(error.class(), PersistFailureClass::Transient);
    }
    assert_eq!(PersistError::Busy.class(), PersistFailureClass::Contended);
    for error in [
        PersistError::InvalidFacts,
        PersistError::UnsafePath,
        PersistError::InvalidSidecarState,
        PersistError::Io,
    ] {
        assert_eq!(error.class(), PersistFailureClass::Permanent);
    }
}

#[test]
fn filesystem_error_mapping_distinguishes_recovery_policy() {
    for (errno, expected) in [
        (rustix::io::Errno::ROFS, PersistError::ReadOnlyFilesystem),
        (rustix::io::Errno::ACCESS, PersistError::PermissionDenied),
        (rustix::io::Errno::PERM, PersistError::PermissionDenied),
        (rustix::io::Errno::NOSPC, PersistError::NoSpace),
        (rustix::io::Errno::DQUOT, PersistError::QuotaExceeded),
        (rustix::io::Errno::STALE, PersistError::StaleFilesystem),
        (rustix::io::Errno::IO, PersistError::TransientIo),
        (rustix::io::Errno::AGAIN, PersistError::TransientIo),
        (rustix::io::Errno::BUSY, PersistError::TransientIo),
        (rustix::io::Errno::INTR, PersistError::TransientIo),
        (rustix::io::Errno::TIMEDOUT, PersistError::TransientIo),
        (rustix::io::Errno::LOOP, PersistError::UnsafePath),
        (rustix::io::Errno::BADF, PersistError::Io),
    ] {
        assert_eq!(PersistError::from_errno(errno), expected, "{errno:?}");
    }
    assert_eq!(
        PersistError::from_io(io::Error::new(io::ErrorKind::NotADirectory, "invalid root",)),
        PersistError::InvalidSidecarState
    );
}

#[test]
fn capacity_failure_runs_at_most_one_gc_and_one_publication_retry() {
    let directory = TempDir::new().expect("cache directory");
    let store = FactStore::new(directory.path());
    seed_authoritative_mark(&store);
    store.inject_publish_faults([
        PersistError::NoSpace,
        PersistError::NoSpace,
        PersistError::NoSpace,
    ]);

    let bytes = lifecycle_pgm();
    let error = store
        .publish(&facts(&bytes), &context(), &LIMIT)
        .expect_err("the bounded retry still fails");
    assert_eq!(error, PersistError::NoSpace);
    assert_eq!(store.test_publish_attempts(), 2);
    assert_eq!(store.test_recovery_gc_attempts(), 1);
    assert_eq!(store.persist_mode().failures, 1);
}

#[test]
fn one_gc_retry_can_recover_and_reset_persistence_state() {
    let directory = TempDir::new().expect("cache directory");
    let store = FactStore::new(directory.path());
    seed_authoritative_mark(&store);
    store.inject_publish_faults([PersistError::QuotaExceeded]);

    let bytes = lifecycle_pgm();
    write_source(&directory, &bytes);
    store
        .publish(&facts(&bytes), &context(), &LIMIT)
        .expect("second publication attempt succeeds");
    assert_eq!(store.test_publish_attempts(), 2);
    assert_eq!(store.test_recovery_gc_attempts(), 1);
    assert_eq!(store.persist_mode().mode, PersistMode::ReadWrite);
    assert_eq!(store.persist_mode().failures, 0);
}

#[test]
fn incomplete_recovery_scan_forbids_the_capacity_retry() {
    let directory = TempDir::new().expect("cache directory");
    let gc_config =
        super::super::gc::GcConfig::new(2, 2, Duration::ZERO, Duration::ZERO, None, None)
            .expect("valid bounded GC config");
    let store = FactStore::with_configs(
        directory.path(),
        super::super::fallback::FallbackConfig::default(),
        gc_config,
    );
    seed_authoritative_mark(&store);
    store.inject_publish_faults([PersistError::NoSpace, PersistError::NoSpace]);

    let bytes = lifecycle_pgm();
    write_source(&directory, &bytes);
    crate::test_layout::write_empty_journal(directory.path());
    assert_eq!(
        store.publish(&facts(&bytes), &context(), &LIMIT),
        Err(PersistError::NoSpace)
    );
    assert_eq!(store.test_publish_attempts(), 1);
    assert_eq!(store.test_recovery_gc_attempts(), 1);
}

#[test]
fn durable_reads_continue_during_write_backoff_and_do_not_reset_it() {
    let directory = TempDir::new().expect("cache directory");
    let store = FactStore::new(directory.path());
    let bytes = lifecycle_pgm();
    let facts = facts(&bytes);
    write_source(&directory, &bytes);
    store
        .publish(&facts, &context(), &LIMIT)
        .expect("initial publication");
    store.inject_publish_faults([PersistError::TransientIo]);
    assert_eq!(
        store.publish(&facts, &context(), &LIMIT),
        Err(PersistError::TransientIo)
    );

    let unit = PgmUnit::open(bytes.as_slice()).expect("open PGM");
    store
        .read(&unit, &context(), &LIMIT)
        .expect("durable read bypasses write backoff");
    assert_eq!(store.persist_mode().failures, 1);
    assert_eq!(store.persist_mode().reason, Some(PersistError::TransientIo));
}

#[test]
fn due_probe_resets_backoff_and_removes_its_sentinel() {
    let directory = TempDir::new().expect("cache directory");
    let store = FactStore::new(directory.path());
    let bytes = lifecycle_pgm();
    let facts = facts(&bytes);
    write_source(&directory, &bytes);
    store.inject_publish_faults([PersistError::TransientIo]);
    assert_eq!(
        store.publish(&facts, &context(), &LIMIT),
        Err(PersistError::TransientIo)
    );
    store.force_persistence_probe_due();

    assert_eq!(
        store.probe_persistence(),
        PersistenceProbeOutcome::Succeeded
    );
    assert_eq!(store.persist_mode().mode, PersistMode::ReadWrite);
    let prefix = format!("{}.ovf.probe.", context().address().id);
    let probes = std::fs::read_dir(crate::test_layout::day_path(
        directory.path(),
        context().address(),
    ))
    .expect("UTC day")
    .filter_map(Result::ok)
    .map(|entry| entry.file_name().to_string_lossy().into_owned())
    .filter(|name| {
        name.starts_with(&prefix)
            && std::path::Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "tmp")
    })
    .count();
    assert_eq!(probes, 0);
}
