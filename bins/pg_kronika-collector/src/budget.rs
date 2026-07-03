//! Wall-time budget for the sized pool sources of one collection cycle.
//!
//! `pg_stat_statements`, user tables, and user indexes read every covered
//! database, so their cost grows with the instance. The budget compares the
//! database time already spent this cycle against a ceiling before each of
//! them runs. A source over the ceiling is deferred, not dropped: the
//! scheduler re-arms it for the next tick, and a deferral guarantees
//! admission on that tick, so a permanently tight budget degrades to
//! every-other-tick reads instead of starvation.

use std::time::Duration;

/// The pool sources the budget can defer, in eviction order: the later a
/// source runs in the cycle, the earlier it is deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PoolSource {
    Statements,
    UserTables,
    UserIndexes,
}

impl PoolSource {
    const fn slot(self) -> usize {
        match self {
            Self::Statements => 0,
            Self::UserTables => 1,
            Self::UserIndexes => 2,
        }
    }
}

/// Admission control for one cycle's pool reads.
#[derive(Debug)]
pub(crate) struct PoolBudget {
    /// Ceiling for the cycle's database time; zero disables the budget.
    limit: Duration,
    /// Sources deferred on the previous tick, admitted unconditionally now.
    starved: [bool; 3],
}

impl PoolBudget {
    pub(crate) const fn new(limit: Duration) -> Self {
        Self {
            limit,
            starved: [false; 3],
        }
    }

    /// Whether `source` may read now, given the database time `spent` so far
    /// this cycle.
    ///
    /// Forced ticks (SIGUSR2) and a zero limit admit everything. A source
    /// deferred on the previous tick is admitted regardless of `spent` —
    /// deferral must not become starvation. Otherwise the source is admitted
    /// while the cycle is under the ceiling, and deferred once over it.
    pub(crate) const fn admit(
        &mut self,
        source: PoolSource,
        spent: Duration,
        forced: bool,
    ) -> bool {
        let slot = source.slot();
        if forced || self.limit.is_zero() {
            self.starved[slot] = false;
            return true;
        }
        if self.starved[slot] {
            self.starved[slot] = false;
            return true;
        }
        if spent.as_millis() >= self.limit.as_millis() {
            self.starved[slot] = true;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{PoolBudget, PoolSource};
    use std::time::Duration;

    #[test]
    fn under_the_ceiling_everything_is_admitted() {
        let mut budget = PoolBudget::new(Duration::from_millis(100));
        assert!(budget.admit(PoolSource::Statements, Duration::from_millis(10), false));
        assert!(budget.admit(PoolSource::UserTables, Duration::from_millis(50), false));
        assert!(budget.admit(PoolSource::UserIndexes, Duration::from_millis(99), false));
    }

    #[test]
    fn over_the_ceiling_defers_and_the_next_tick_admits() {
        let mut budget = PoolBudget::new(Duration::from_millis(100));
        let spent = Duration::from_millis(100);
        assert!(!budget.admit(PoolSource::UserTables, spent, false));
        // The deferred source is admitted next tick even over the ceiling.
        assert!(budget.admit(PoolSource::UserTables, spent, false));
        // And the tick after that it competes normally again.
        assert!(!budget.admit(PoolSource::UserTables, spent, false));
    }

    #[test]
    fn sources_starve_independently() {
        let mut budget = PoolBudget::new(Duration::from_millis(100));
        let spent = Duration::from_millis(200);
        assert!(!budget.admit(PoolSource::UserTables, spent, false));
        assert!(!budget.admit(PoolSource::UserIndexes, spent, false));
        assert!(budget.admit(PoolSource::UserTables, spent, false));
        assert!(budget.admit(PoolSource::UserIndexes, spent, false));
    }

    #[test]
    fn a_forced_tick_ignores_the_budget_and_clears_starvation() {
        let mut budget = PoolBudget::new(Duration::from_millis(100));
        let spent = Duration::from_millis(200);
        assert!(!budget.admit(PoolSource::Statements, spent, false));
        assert!(budget.admit(PoolSource::Statements, spent, true));
        // The forced read satisfied the deferral: the next tick competes.
        assert!(!budget.admit(PoolSource::Statements, spent, false));
    }

    #[test]
    fn a_zero_limit_disables_the_budget() {
        let mut budget = PoolBudget::new(Duration::ZERO);
        assert!(budget.admit(PoolSource::UserIndexes, Duration::from_hours(1), false));
    }
}
