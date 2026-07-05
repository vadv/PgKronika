//! Per-source pacing for collection ticks.
//!
//! Each source has its own interval. A forced tick (`SIGUSR2`, and the first
//! timer tick after start) reads every source, so the first segment after
//! restart is self-contained and signal-driven collection keeps its contract.
//! Positive intervals can pull the next timer wake forward; explicit zero
//! intervals run on every timer wake. The `pg_store_plans` cadence remains
//! inside the plans source cache. The lock-wait graph has no interval; it runs
//! when the current activity snapshot shows a backend waiting on a heavyweight
//! lock.

use std::time::{Duration, Instant};

/// One independently paced source group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Activity,
    Database,
    Bgwriter,
    Wal,
    Io,
    Archiver,
    PreparedXacts,
    ProgressVacuum,
    Statements,
    UserTables,
    UserIndexes,
    Replication,
    ResetMetadata,
    InstanceMetadata,
    Settings,
    OsCore,
    OsMountTopo,
    OsProcesses,
    OsProcessStatus,
    OsCgroup,
    OsCgroupMapping,
    PgLog,
}

/// All source kinds, in collection order.
pub(crate) const ALL_SOURCES: [SourceKind; 22] = [
    SourceKind::Activity,
    SourceKind::Database,
    SourceKind::Bgwriter,
    SourceKind::Wal,
    SourceKind::Io,
    SourceKind::Archiver,
    SourceKind::PreparedXacts,
    SourceKind::ProgressVacuum,
    SourceKind::Statements,
    SourceKind::UserTables,
    SourceKind::UserIndexes,
    SourceKind::Replication,
    SourceKind::ResetMetadata,
    SourceKind::InstanceMetadata,
    SourceKind::Settings,
    SourceKind::OsCore,
    SourceKind::OsMountTopo,
    SourceKind::OsProcesses,
    SourceKind::OsProcessStatus,
    SourceKind::OsCgroup,
    SourceKind::OsCgroupMapping,
    SourceKind::PgLog,
];

/// Per-source intervals, in seconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Intervals {
    pub activity: u64,
    pub database: u64,
    pub bgwriter: u64,
    pub wal: u64,
    pub io: u64,
    pub archiver: u64,
    pub prepared_xacts: u64,
    pub progress_vacuum: u64,
    pub statements: u64,
    pub user_tables: u64,
    pub user_indexes: u64,
    pub replication: u64,
    pub reset_metadata: u64,
    pub instance_metadata: u64,
    pub settings: u64,
    pub os_core: u64,
    pub os_mount_topo: u64,
    pub os_processes: u64,
    pub os_process_status: u64,
    pub os_cgroup: u64,
    pub os_cgroup_mapping: u64,
    pub pg_log: u64,
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            activity: 5,
            database: 10,
            bgwriter: 10,
            wal: 10,
            io: 10,
            archiver: 30,
            prepared_xacts: 30,
            progress_vacuum: 10,
            statements: 30,
            user_tables: 30,
            user_indexes: 60,
            replication: 30,
            reset_metadata: 30,
            instance_metadata: 60,
            settings: 3600,
            os_core: 10,
            os_mount_topo: 60,
            os_processes: 5,
            os_process_status: 30,
            os_cgroup: 10,
            os_cgroup_mapping: 30,
            pg_log: 5,
        }
    }
}

impl Intervals {
    const fn of(&self, kind: SourceKind) -> u64 {
        match kind {
            SourceKind::Activity => self.activity,
            SourceKind::Database => self.database,
            SourceKind::Bgwriter => self.bgwriter,
            SourceKind::Wal => self.wal,
            SourceKind::Io => self.io,
            SourceKind::Archiver => self.archiver,
            SourceKind::PreparedXacts => self.prepared_xacts,
            SourceKind::ProgressVacuum => self.progress_vacuum,
            SourceKind::Statements => self.statements,
            SourceKind::UserTables => self.user_tables,
            SourceKind::UserIndexes => self.user_indexes,
            SourceKind::Replication => self.replication,
            SourceKind::ResetMetadata => self.reset_metadata,
            SourceKind::InstanceMetadata => self.instance_metadata,
            SourceKind::Settings => self.settings,
            SourceKind::OsCore => self.os_core,
            SourceKind::OsMountTopo => self.os_mount_topo,
            SourceKind::OsProcesses => self.os_processes,
            SourceKind::OsProcessStatus => self.os_process_status,
            SourceKind::OsCgroup => self.os_cgroup,
            SourceKind::OsCgroupMapping => self.os_cgroup_mapping,
            SourceKind::PgLog => self.pg_log,
        }
    }
}

/// The sources one tick must read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DueSet {
    kinds: Vec<SourceKind>,
    forced: bool,
}

impl DueSet {
    /// Whether `kind` is due this tick.
    pub(crate) fn has(&self, kind: SourceKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// No source is due for this tick.
    pub(crate) const fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Whether this tick was forced (SIGUSR2): paced reads outside the
    /// scheduler ignore their own deadline too.
    pub(crate) const fn forced(&self) -> bool {
        self.forced
    }

    /// A set with every source due (forced tick).
    fn all() -> Self {
        Self {
            kinds: ALL_SOURCES.to_vec(),
            forced: true,
        }
    }

    /// A set with exactly the given sources due. For use in unit tests only.
    #[cfg(test)]
    #[allow(clippy::missing_const_for_fn, reason = "Vec disallows const context")]
    pub(crate) fn for_test(kinds: Vec<SourceKind>) -> Self {
        Self {
            kinds,
            forced: false,
        }
    }
}

/// The sources a fresh segment re-reads on its first tick, so every sealed
/// file carries its own instance identity, reset context, and configuration.
const SEGMENT_OPEN_SOURCES: [SourceKind; 4] = [
    SourceKind::ResetMetadata,
    SourceKind::InstanceMetadata,
    SourceKind::Settings,
    SourceKind::OsMountTopo,
];

/// Decides which sources each tick reads, one entry per source.
#[derive(Debug)]
pub(crate) struct Scheduler {
    intervals: Intervals,
    /// Trigger-accelerated intervals, overriding the base while active.
    overrides: [Option<u64>; ALL_SOURCES.len()],
    last_read: [Option<Instant>; ALL_SOURCES.len()],
}

impl Scheduler {
    pub(crate) const fn new(intervals: Intervals) -> Self {
        Self {
            intervals,
            overrides: [None; ALL_SOURCES.len()],
            last_read: [None; ALL_SOURCES.len()],
        }
    }

    /// Pace `kind` at `secs` until [`Scheduler::relax`], returning whether the
    /// pace changed. An acceleration at or above the base interval is a no-op:
    /// operators disable a trigger by setting its fast interval to the base.
    pub(crate) fn accelerate(&mut self, kind: SourceKind, secs: u64) -> bool {
        if secs >= self.intervals.of(kind) {
            return false;
        }
        let slot = slot_of(kind);
        if self.overrides[slot] == Some(secs) {
            return false;
        }
        self.overrides[slot] = Some(secs);
        true
    }

    /// Return `kind` to its base interval, returning whether the pace changed.
    pub(crate) fn relax(&mut self, kind: SourceKind) -> bool {
        self.overrides[slot_of(kind)].take().is_some()
    }

    /// A segment was just sealed, so the next window must re-read the
    /// per-segment service sources.
    pub(crate) fn mark_segment_opened(&mut self) {
        for (slot, kind) in ALL_SOURCES.iter().enumerate() {
            if SEGMENT_OPEN_SOURCES.contains(kind) {
                self.last_read[slot] = None;
            }
        }
    }

    /// Put a planned-but-unread source back: it comes due on the next tick.
    pub(crate) fn defer(&mut self, kind: SourceKind) {
        self.last_read[slot_of(kind)] = None;
    }

    /// Time until the next positive source interval elapses.
    ///
    /// Sources with no previous read, deferred sources, and explicit zero
    /// intervals are read on the next timer wake; they do not pull the wake
    /// forward by themselves.
    pub(crate) fn next_elapsed_due_in(&self, now: Instant) -> Option<Duration> {
        ALL_SOURCES
            .iter()
            .enumerate()
            .filter_map(|(slot, kind)| {
                let last = self.last_read[slot]?;
                let secs = self.overrides[slot].unwrap_or_else(|| self.intervals.of(*kind));
                if secs == 0 {
                    return None;
                }
                let interval = Duration::from_secs(secs);
                Some(interval.saturating_sub(now.saturating_duration_since(last)))
            })
            .min()
    }

    /// The due set for a tick at `now`.
    ///
    /// A source is due when it has never been read or its interval elapsed.
    /// `force` marks everything due — the SIGUSR2 contract. Due sources are
    /// immediately marked read: a failed read retries after its interval,
    /// not on the next tick. The two heaviest sized sources do not share an
    /// unforced tick: when tables and indexes come due together, indexes
    /// yields to the next tick — except on its first-ever read, so the first
    /// window stays self-contained.
    pub(crate) fn plan(&mut self, now: Instant, force: bool) -> DueSet {
        if force {
            self.last_read = [Some(now); ALL_SOURCES.len()];
            return DueSet::all();
        }
        let indexes_read_before = self.last_read[slot_of(SourceKind::UserIndexes)].is_some();
        let mut kinds = Vec::new();
        for (slot, kind) in ALL_SOURCES.iter().enumerate() {
            let secs = self.overrides[slot].unwrap_or_else(|| self.intervals.of(*kind));
            let interval = Duration::from_secs(secs);
            let is_due =
                self.last_read[slot].is_none_or(|last| now.duration_since(last) >= interval);
            if is_due {
                self.last_read[slot] = Some(now);
                kinds.push(*kind);
            }
        }
        // An explicit zero interval is an operator's "every tick" and is
        // exempt from phase separation.
        if indexes_read_before
            && self.intervals.user_indexes > 0
            && kinds.contains(&SourceKind::UserTables)
            && kinds.contains(&SourceKind::UserIndexes)
        {
            kinds.retain(|kind| *kind != SourceKind::UserIndexes);
            self.defer(SourceKind::UserIndexes);
        }
        DueSet {
            kinds,
            forced: false,
        }
    }
}

/// Index of `kind` in [`ALL_SOURCES`] and every parallel array.
fn slot_of(kind: SourceKind) -> usize {
    ALL_SOURCES
        .iter()
        .position(|candidate| *candidate == kind)
        .expect("ALL_SOURCES lists every kind")
}

#[cfg(test)]
mod tests {
    use super::{ALL_SOURCES, DueSet, Intervals, Scheduler, SourceKind};
    use std::time::{Duration, Instant};

    fn uniform(secs: u64) -> Intervals {
        Intervals {
            activity: secs,
            database: secs,
            bgwriter: secs,
            wal: secs,
            io: secs,
            archiver: secs,
            prepared_xacts: secs,
            progress_vacuum: secs,
            statements: secs,
            user_tables: secs,
            user_indexes: secs,
            replication: secs,
            reset_metadata: secs,
            instance_metadata: secs,
            settings: secs,
            os_core: secs,
            os_mount_topo: secs,
            os_processes: secs,
            os_process_status: secs,
            os_cgroup: secs,
            os_cgroup_mapping: secs,
            pg_log: secs,
        }
    }

    #[test]
    fn first_tick_reads_everything() {
        let mut scheduler = Scheduler::new(Intervals::default());
        let due = scheduler.plan(Instant::now(), false);
        for kind in ALL_SOURCES {
            assert!(due.has(kind), "{kind:?} must be due on the first tick");
        }
    }

    #[test]
    fn force_reads_everything_and_resets_the_clocks() {
        let mut scheduler = Scheduler::new(uniform(1000));
        let start = Instant::now();
        assert_eq!(scheduler.plan(start, true), DueSet::all());
        // Immediately after a forced tick nothing is due.
        assert!(
            scheduler
                .plan(start + Duration::from_secs(1), false)
                .is_empty()
        );
    }

    #[test]
    fn sources_come_due_by_their_own_intervals() {
        let mut intervals = uniform(1000);
        intervals.activity = 5;
        intervals.statements = 30;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false); // first tick: everything

        let at_5s = scheduler.plan(start + Duration::from_secs(5), false);
        assert!(at_5s.has(SourceKind::Activity));
        assert!(!at_5s.has(SourceKind::Statements));
        assert!(!at_5s.has(SourceKind::Settings));

        let at_30s = scheduler.plan(start + Duration::from_secs(30), false);
        assert!(at_30s.has(SourceKind::Activity));
        assert!(at_30s.has(SourceKind::Statements));
        assert!(!at_30s.has(SourceKind::Settings));
    }

    #[test]
    fn a_read_marks_the_source_even_between_ticks() {
        let mut intervals = uniform(1000);
        intervals.activity = 5;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false);
        // 4 seconds later activity is not due yet.
        assert!(
            !scheduler
                .plan(start + Duration::from_secs(4), false)
                .has(SourceKind::Activity)
        );
        // The 4-second tick did not reset activity's clock.
        assert!(
            scheduler
                .plan(start + Duration::from_secs(5), false)
                .has(SourceKind::Activity)
        );
    }

    #[test]
    fn segment_open_re_arms_only_the_service_sources() {
        let mut scheduler = Scheduler::new(uniform(1000));
        let start = Instant::now();
        scheduler.plan(start, false); // first tick: everything read
        scheduler.mark_segment_opened();
        let next = scheduler.plan(start + Duration::from_secs(1), false);
        assert!(next.has(SourceKind::ResetMetadata));
        assert!(next.has(SourceKind::InstanceMetadata));
        assert!(next.has(SourceKind::Settings));
        assert!(next.has(SourceKind::OsMountTopo));
        assert!(!next.has(SourceKind::Activity));
        assert!(!next.has(SourceKind::UserTables));
    }

    #[test]
    fn zero_interval_is_due_on_every_tick() {
        let mut intervals = uniform(1000);
        intervals.activity = 0;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false);
        let next = scheduler.plan(start + Duration::from_secs(1), false);
        assert!(next.has(SourceKind::Activity));
        assert!(!next.has(SourceKind::Database));
    }

    #[test]
    fn empty_due_set_reports_empty() {
        let mut scheduler = Scheduler::new(uniform(1000));
        let start = Instant::now();
        scheduler.plan(start, false);
        assert!(
            scheduler
                .plan(start + Duration::from_secs(1), false)
                .is_empty()
        );
    }

    #[test]
    fn a_deferred_source_comes_due_on_the_next_tick() {
        let mut scheduler = Scheduler::new(uniform(1000));
        let start = Instant::now();
        scheduler.plan(start, false); // first tick: everything read
        scheduler.defer(SourceKind::UserTables);
        let next = scheduler.plan(start + Duration::from_secs(1), false);
        assert!(next.has(SourceKind::UserTables));
        assert!(
            !next.has(SourceKind::UserIndexes),
            "only the deferred source returns"
        );
    }

    #[test]
    fn indexes_yield_when_tables_share_the_tick() {
        let mut intervals = uniform(1000);
        intervals.user_tables = 30;
        intervals.user_indexes = 60;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();

        let first = scheduler.plan(start, false);
        assert!(
            first.has(SourceKind::UserTables) && first.has(SourceKind::UserIndexes),
            "the first window carries both sized sources"
        );

        let at_60 = scheduler.plan(start + Duration::from_mins(1), false);
        assert!(at_60.has(SourceKind::UserTables));
        assert!(
            !at_60.has(SourceKind::UserIndexes),
            "indexes yield the shared tick"
        );

        let at_61 = scheduler.plan(start + Duration::from_secs(61), false);
        assert!(!at_61.has(SourceKind::UserTables));
        assert!(
            at_61.has(SourceKind::UserIndexes),
            "indexes run one tick later"
        );
    }

    #[test]
    fn acceleration_paces_a_source_faster_until_relaxed() {
        let mut intervals = uniform(1000);
        intervals.activity = 5;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false);

        assert!(scheduler.accelerate(SourceKind::Activity, 1));
        assert!(
            !scheduler.accelerate(SourceKind::Activity, 1),
            "re-arming the same pace is not a change"
        );
        assert!(
            scheduler
                .plan(start + Duration::from_secs(1), false)
                .has(SourceKind::Activity),
            "the accelerated pace makes a 1s tick due"
        );

        assert!(scheduler.relax(SourceKind::Activity));
        assert!(!scheduler.relax(SourceKind::Activity), "already at base");
        assert!(
            !scheduler
                .plan(start + Duration::from_secs(3), false)
                .has(SourceKind::Activity),
            "back at the 5s base, 2s after the last read is not due"
        );
    }

    #[test]
    fn acceleration_at_or_above_the_base_interval_is_ignored() {
        let mut intervals = uniform(1000);
        intervals.activity = 5;
        let mut scheduler = Scheduler::new(intervals);
        assert!(
            !scheduler.accelerate(SourceKind::Activity, 5),
            "a fast interval equal to the base disables the trigger"
        );
        assert!(!scheduler.accelerate(SourceKind::Activity, 30));
    }

    #[test]
    fn acceleration_does_not_touch_other_sources() {
        let mut scheduler = Scheduler::new(uniform(1000));
        let start = Instant::now();
        scheduler.plan(start, false);
        scheduler.accelerate(SourceKind::Activity, 1);
        let next = scheduler.plan(start + Duration::from_secs(1), false);
        assert!(next.has(SourceKind::Activity));
        assert!(!next.has(SourceKind::Replication));
    }

    #[test]
    fn next_due_uses_positive_accelerated_interval() {
        let mut intervals = uniform(1000);
        intervals.activity = 5;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false);
        scheduler.accelerate(SourceKind::Activity, 1);

        assert_eq!(
            scheduler.next_elapsed_due_in(start),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            scheduler.next_elapsed_due_in(start + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn zero_and_deferred_sources_wait_for_a_timer_wake() {
        let mut intervals = uniform(1000);
        intervals.activity = 0;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false);

        assert_eq!(
            scheduler.next_elapsed_due_in(start),
            Some(Duration::from_secs(1000)),
            "zero-interval sources are due on timer wakes, not a tight loop"
        );

        scheduler.defer(SourceKind::Replication);
        assert_eq!(
            scheduler.next_elapsed_due_in(start),
            Some(Duration::from_secs(1000)),
            "deferred sources return on the next timer wake"
        );
    }

    #[test]
    fn a_zero_indexes_interval_is_exempt_from_phase_separation() {
        let mut intervals = uniform(1000);
        intervals.user_tables = 0;
        intervals.user_indexes = 0;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false);
        let next = scheduler.plan(start + Duration::from_secs(1), false);
        assert!(next.has(SourceKind::UserTables));
        assert!(next.has(SourceKind::UserIndexes));
    }

    #[test]
    fn os_mount_topo_default_interval_is_60s() {
        assert_eq!(Intervals::default().os_mount_topo, 60);
    }

    #[test]
    fn os_mount_topo_comes_due_after_its_interval() {
        let mut intervals = uniform(1000);
        intervals.os_mount_topo = 60;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false); // first tick: everything due

        let at_59s = scheduler.plan(start + Duration::from_secs(59), false);
        assert!(!at_59s.has(SourceKind::OsMountTopo));

        let at_60s = scheduler.plan(start + Duration::from_mins(1), false);
        assert!(at_60s.has(SourceKind::OsMountTopo));
    }

    #[test]
    fn segment_open_re_arms_os_mount_topo() {
        let mut intervals = uniform(1000);
        intervals.os_mount_topo = 60;
        let mut scheduler = Scheduler::new(intervals);
        let start = Instant::now();
        scheduler.plan(start, false); // first tick: everything read
        scheduler.mark_segment_opened();
        // One second later OsMountTopo fires again because segment was opened.
        let next = scheduler.plan(start + Duration::from_secs(1), false);
        assert!(next.has(SourceKind::OsMountTopo));
    }
}
