use crate::buffering::push_activity;
use crate::plans_source::PlansSourceCache;
use crate::scheduler::{Intervals, Scheduler, SourceKind};
use crate::segments::{SegmentState, open_collector_journal, seal_reason};
use crate::source_contracts::activity_dict_limits;
use crate::timer_sleep_delay;
use kronika_source_pg::{ActivityRow, ActivityVersion};
use kronika_writer::{Interner, SectionBuffers, dict};

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
    segment.on_window_appended(100, now);
    segment.on_window_appended(200, now + Duration::from_secs(5));
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
            &SegmentState::default()
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
            &SegmentState::default()
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
            &SegmentState::default()
        ),
        Some(Duration::from_secs(5)),
        "zero means every timer wake, not an immediate busy loop"
    );
}
fn client_row(pid: i32) -> ActivityRow {
    ActivityRow {
        ts: 1_000,
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
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_activity(
        &mut buffers,
        &mut interner,
        ActivityVersion::V3,
        &[client_row(7)],
    )
    .expect("push interns and buffers");
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part")
}

#[test]
fn startup_seals_windows_a_dead_process_left_in_the_journal() {
    use kronika_writer::{Journal, JournalConfig};

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let (mut journal, _report) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("open the journal");
        journal.append(&activity_window()).expect("append");
        // Dropping without seal is the crash: the file stays behind.
    }

    let (journal, recovered) =
        open_collector_journal(dir.path(), 1 << 30).expect("reopen the journal");
    let dest = recovered.expect("leftover windows must become a segment");
    // client_row stamps ts = 1_000, which names the recovered file.
    assert_eq!(dest, dir.path().join("1000.pgm"));
    assert!(dest.exists(), "the recovered segment is on disk");
    assert!(journal.parts().is_empty(), "the journal restarts empty");
}

#[test]
fn startup_with_an_empty_journal_recovers_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (journal, recovered) =
        open_collector_journal(dir.path(), 1 << 30).expect("open the journal");
    assert!(recovered.is_none());
    assert!(journal.parts().is_empty());
}
