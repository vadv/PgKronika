//! Per-source pacing for collection ticks.
//!
//! Each source has its own interval. A forced tick (`SIGUSR2`, and the first
//! tick after start) reads every source, so the first segment after restart is
//! self-contained and signal-driven collection keeps its contract. The
//! `pg_store_plans` cadence remains inside the plans source cache. The
//! lock-wait graph has no interval; it runs when the current activity snapshot
//! shows a backend waiting on a heavyweight lock.

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
}

/// All source kinds, in collection order.
pub(crate) const ALL_SOURCES: [SourceKind; 15] = [
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

    /// No source is due: the tick seals nothing.
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
}

/// The sources a fresh segment re-reads on its first tick, so every sealed
/// file carries its own instance identity, reset context, and configuration.
const SEGMENT_OPEN_SOURCES: [SourceKind; 3] = [
    SourceKind::ResetMetadata,
    SourceKind::InstanceMetadata,
    SourceKind::Settings,
];

/// Decides which sources each tick reads, one entry per source.
#[derive(Debug)]
pub(crate) struct Scheduler {
    intervals: Intervals,
    last_read: [Option<Instant>; ALL_SOURCES.len()],
}

impl Scheduler {
    pub(crate) const fn new(intervals: Intervals) -> Self {
        Self {
            intervals,
            last_read: [None; ALL_SOURCES.len()],
        }
    }

    /// A segment was just sealed: the per-segment service sources come due
    /// again, so the next window opens a self-contained file.
    pub(crate) fn mark_segment_opened(&mut self) {
        for (slot, kind) in ALL_SOURCES.iter().enumerate() {
            if SEGMENT_OPEN_SOURCES.contains(kind) {
                self.last_read[slot] = None;
            }
        }
    }

    /// The due set for a tick at `now`.
    ///
    /// A source is due when it has never been read or its interval elapsed.
    /// `force` marks everything due — the SIGUSR2 contract. Due sources are
    /// immediately marked read: a failed read retries after its interval,
    /// not on the next tick.
    pub(crate) fn plan(&mut self, now: Instant, force: bool) -> DueSet {
        if force {
            self.last_read = [Some(now); ALL_SOURCES.len()];
            return DueSet::all();
        }
        let mut kinds = Vec::new();
        for (slot, kind) in ALL_SOURCES.iter().enumerate() {
            let interval = Duration::from_secs(self.intervals.of(*kind));
            let is_due =
                self.last_read[slot].is_none_or(|last| now.duration_since(last) >= interval);
            if is_due {
                self.last_read[slot] = Some(now);
                kinds.push(*kind);
            }
        }
        DueSet {
            kinds,
            forced: false,
        }
    }
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
}
