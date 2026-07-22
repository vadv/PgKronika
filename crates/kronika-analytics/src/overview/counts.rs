//! Exact event-count aggregation over the joint `(severity, category,
//! sqlstate)` dimension.
//!
//! Counts are kept over the joint key, not three marginal maps, so a range can
//! answer how many `Resource` `FATAL` errors occurred — a question the
//! marginals cannot. Marginal totals are projections of the joint set.
//!
//! Every merge is checked: two count sets combine with checked addition and a
//! sum that would overflow returns [`CountOverflow`] rather than saturating.
//! This keeps merge associative and commutative, so splitting a stream into
//! parts and segments and recombining in any order yields the identical total.

use std::collections::BTreeMap;

/// A `PostgreSQL` log severity.
///
/// A closed set of five; the discriminant is the stable marginal index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// A statement-level `ERROR`.
    Error = 0,
    /// A session-fatal `FATAL`.
    Fatal = 1,
    /// A process-fatal `PANIC`.
    Panic = 2,
    /// A `WARNING`.
    Warning = 3,
    /// A `LOG` line, including crash and `OOM` lifecycle records.
    Log = 4,
}

impl Severity {
    /// Every severity in marginal-index order.
    pub const ALL: [Self; 5] = [
        Self::Error,
        Self::Fatal,
        Self::Panic,
        Self::Warning,
        Self::Log,
    ];

    /// The stable marginal index, `0..5`.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// A normalized error category.
///
/// A closed set of eleven, assigned by the log classifier; the discriminant is
/// the stable marginal index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorCategory {
    /// Lock/deadlock class.
    Lock = 0,
    /// Constraint violation class.
    Constraint = 1,
    /// Serialization failure class.
    Serialization = 2,
    /// Statement/lock timeout class.
    Timeout = 3,
    /// Connection/protocol class.
    Connection = 4,
    /// Authentication/authorization class.
    Auth = 5,
    /// Syntax/undefined-object class.
    Syntax = 6,
    /// Insufficient-resources class.
    Resource = 7,
    /// Data/index corruption class.
    DataCorruption = 8,
    /// System/IO class.
    System = 9,
    /// Anything not otherwise classified.
    Other = 10,
}

impl ErrorCategory {
    /// Every category in marginal-index order.
    pub const ALL: [Self; 11] = [
        Self::Lock,
        Self::Constraint,
        Self::Serialization,
        Self::Timeout,
        Self::Connection,
        Self::Auth,
        Self::Syntax,
        Self::Resource,
        Self::DataCorruption,
        Self::System,
        Self::Other,
    ];

    /// The stable marginal index, `0..11`.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// A five-byte SQLSTATE code exactly as retained, never widened or localized.
///
/// SQLSTATE is optional in the source: it is present only when the stderr line
/// carried it, so it refines a count but is never the primary axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SqlState(pub [u8; 5]);

/// The joint key of one error dimension.
///
/// The exact `(severity, category, sqlstate)` combination whose occurrences are
/// counted together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct JointErrorKey {
    /// The line severity.
    pub severity: Severity,
    /// The classified category.
    pub category: ErrorCategory,
    /// The SQLSTATE, when the source retained one.
    pub sqlstate: Option<SqlState>,
}

/// A count exceeded [`u64::MAX`] during a merge.
///
/// Returned instead of saturating, so a total is never silently forged smaller
/// than the truth. A caller marks the block or response as uncacheable or
/// incomplete with this reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountOverflow;

/// Lifecycle observation counts, kept apart from error-line counts.
///
/// They are separate because one physical crash is recorded in both
/// `pg_log_lifecycle` and `pg_log_errors`; adding them across the two would
/// double-count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleCounts {
    /// Child-process crash and signal-termination observations.
    pub crashes: u64,
    /// Shutdown-requested observations.
    pub shutdowns: u64,
    /// Ready-observed observations.
    pub ready: u64,
    /// Termination signals, as a sorted-unique `(signal, count)` vector so the
    /// on-disk and merged order never depends on hash iteration.
    pub signals: Vec<(i32, u64)>,
}

impl LifecycleCounts {
    /// Merge two lifecycle sets with checked addition.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if any summed count exceeds [`u64::MAX`].
    pub fn merge(&self, other: &Self) -> Result<Self, CountOverflow> {
        let mut signals: BTreeMap<i32, u64> = BTreeMap::new();
        for (signal, count) in self.signals.iter().chain(&other.signals) {
            let slot = signals.entry(*signal).or_insert(0);
            *slot = slot.checked_add(*count).ok_or(CountOverflow)?;
        }
        Ok(Self {
            crashes: checked_sum(self.crashes, other.crashes)?,
            shutdowns: checked_sum(self.shutdowns, other.shutdowns)?,
            ready: checked_sum(self.ready, other.ready)?,
            signals: signals.into_iter().collect(),
        })
    }
}

/// Exact error-occurrence counts over the joint dimension, plus lifecycle
/// counts.
///
/// The joint map is stored sorted and unique; marginal severity, category, and
/// SQLSTATE totals are derived on demand so no marginal can disagree with the
/// joint truth.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventCounts {
    joint: Vec<(JointErrorKey, u64)>,
    /// Lifecycle observation counts.
    pub lifecycle: LifecycleCounts,
}

impl EventCounts {
    /// Builds a count set from joint entries.
    ///
    /// Duplicate keys sum with checked addition, zero-count keys drop, and the
    /// stored vector is sorted and unique regardless of input order.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if duplicate keys sum past [`u64::MAX`].
    pub fn from_joint(
        entries: impl IntoIterator<Item = (JointErrorKey, u64)>,
        lifecycle: LifecycleCounts,
    ) -> Result<Self, CountOverflow> {
        let mut map: BTreeMap<JointErrorKey, u64> = BTreeMap::new();
        for (key, count) in entries {
            let slot = map.entry(key).or_insert(0);
            *slot = slot.checked_add(count).ok_or(CountOverflow)?;
        }
        Ok(Self {
            joint: map.into_iter().filter(|&(_, count)| count > 0).collect(),
            lifecycle,
        })
    }

    /// The joint entries, sorted by key and unique.
    #[must_use]
    pub fn joint(&self) -> &[(JointErrorKey, u64)] {
        &self.joint
    }

    /// Total retained error occurrences across every joint key.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if the total exceeds [`u64::MAX`].
    pub fn total_occurrences(&self) -> Result<u64, CountOverflow> {
        self.joint
            .iter()
            .try_fold(0_u64, |acc, &(_, count)| checked_sum(acc, count))
    }

    /// Marginal counts per severity, in [`Severity::index`] order.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if a marginal sum exceeds [`u64::MAX`].
    pub fn by_severity(&self) -> Result<[u64; 5], CountOverflow> {
        let mut out = [0_u64; 5];
        for &(key, count) in &self.joint {
            let slot = &mut out[key.severity.index()];
            *slot = slot.checked_add(count).ok_or(CountOverflow)?;
        }
        Ok(out)
    }

    /// Marginal counts per category, in [`ErrorCategory::index`] order.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if a marginal sum exceeds [`u64::MAX`].
    pub fn by_category(&self) -> Result<[u64; 11], CountOverflow> {
        let mut out = [0_u64; 11];
        for &(key, count) in &self.joint {
            let slot = &mut out[key.category.index()];
            *slot = slot.checked_add(count).ok_or(CountOverflow)?;
        }
        Ok(out)
    }

    /// Merge two count sets exactly.
    ///
    /// The result is independent of argument order and of how a stream was
    /// partitioned before merging: keys union, counts add checked, and the
    /// stored vector stays sorted and unique.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if any summed count exceeds [`u64::MAX`].
    pub fn merge(&self, other: &Self) -> Result<Self, CountOverflow> {
        let mut map: BTreeMap<JointErrorKey, u64> = BTreeMap::new();
        for &(key, count) in self.joint.iter().chain(&other.joint) {
            let slot = map.entry(key).or_insert(0);
            *slot = slot.checked_add(count).ok_or(CountOverflow)?;
        }
        Ok(Self {
            joint: map.into_iter().filter(|&(_, count)| count > 0).collect(),
            lifecycle: self.lifecycle.merge(&other.lifecycle)?,
        })
    }
}

/// Checked `u64` addition, mapping overflow to [`CountOverflow`].
fn checked_sum(a: u64, b: u64) -> Result<u64, CountOverflow> {
    a.checked_add(b).ok_or(CountOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(
        severity: Severity,
        category: ErrorCategory,
        sqlstate: Option<[u8; 5]>,
    ) -> JointErrorKey {
        JointErrorKey {
            severity,
            category,
            sqlstate: sqlstate.map(SqlState),
        }
    }

    fn counts(entries: &[(JointErrorKey, u64)]) -> EventCounts {
        EventCounts::from_joint(entries.iter().copied(), LifecycleCounts::default())
            .expect("no overflow in fixture")
    }

    #[test]
    fn from_joint_sorts_sums_duplicates_and_drops_zeros() {
        let k_fatal = key(Severity::Fatal, ErrorCategory::Resource, Some(*b"53300"));
        let k_warn = key(Severity::Warning, ErrorCategory::Other, None);
        let c = counts(&[
            (k_warn, 0), // dropped
            (k_fatal, 2),
            (k_fatal, 3), // summed with the earlier k_fatal
        ]);
        assert_eq!(c.joint(), &[(k_fatal, 5)]);
        assert_eq!(c.total_occurrences(), Ok(5));
    }

    #[test]
    fn marginals_are_projections_of_the_joint_truth() {
        // Two joint keys share severity Fatal; the marginal must sum them.
        let a = key(Severity::Fatal, ErrorCategory::Resource, Some(*b"53300"));
        let b = key(Severity::Fatal, ErrorCategory::Connection, None);
        let d = key(Severity::Error, ErrorCategory::Syntax, Some(*b"42601"));
        let c = counts(&[(a, 4), (b, 1), (d, 9)]);

        let mut severity = [0_u64; 5];
        severity[Severity::Fatal.index()] = 5;
        severity[Severity::Error.index()] = 9;
        assert_eq!(c.by_severity(), Ok(severity));

        let mut category = [0_u64; 11];
        category[ErrorCategory::Resource.index()] = 4;
        category[ErrorCategory::Connection.index()] = 1;
        category[ErrorCategory::Syntax.index()] = 9;
        assert_eq!(c.by_category(), Ok(category));
    }

    #[test]
    fn merge_is_commutative() {
        let a = counts(&[(key(Severity::Error, ErrorCategory::Lock, None), 3)]);
        let b = counts(&[
            (key(Severity::Error, ErrorCategory::Lock, None), 7),
            (key(Severity::Panic, ErrorCategory::DataCorruption, None), 1),
        ]);
        assert_eq!(a.merge(&b), b.merge(&a));
    }

    #[test]
    fn merge_is_associative() {
        let a = counts(&[(key(Severity::Error, ErrorCategory::Lock, None), 3)]);
        let b = counts(&[(key(Severity::Error, ErrorCategory::Lock, None), 4)]);
        let c = counts(&[(
            key(Severity::Fatal, ErrorCategory::Auth, Some(*b"28000")),
            2,
        )]);
        let left = a.merge(&b).and_then(|ab| ab.merge(&c));
        let right = b.merge(&c).and_then(|bc| a.merge(&bc));
        assert_eq!(left, right);
    }

    #[test]
    fn empty_is_the_merge_identity() {
        let a = counts(&[(key(Severity::Warning, ErrorCategory::Other, None), 5)]);
        let empty = EventCounts::default();
        assert_eq!(a.merge(&empty), Ok(a.clone()));
        assert_eq!(empty.merge(&a), Ok(a));
    }

    #[test]
    fn overflow_is_reported_not_saturated() {
        let k = key(Severity::Error, ErrorCategory::Other, None);
        let a = counts(&[(k, u64::MAX)]);
        let b = counts(&[(k, 1)]);
        assert_eq!(a.merge(&b), Err(CountOverflow));
    }

    #[test]
    fn lifecycle_signals_merge_sorted_and_checked() {
        let a = LifecycleCounts {
            crashes: 1,
            signals: vec![(9, 2), (6, 1)],
            ..LifecycleCounts::default()
        };
        let b = LifecycleCounts {
            crashes: 1,
            signals: vec![(9, 3)],
            ..LifecycleCounts::default()
        };
        let merged = a.merge(&b).expect("no overflow");
        assert_eq!(merged.crashes, 2);
        // Sorted-unique by signal number; the shared signal 9 is summed.
        assert_eq!(merged.signals, vec![(6, 1), (9, 5)]);
    }

    #[test]
    fn lifecycle_overflow_is_reported() {
        let a = LifecycleCounts {
            crashes: u64::MAX,
            ..LifecycleCounts::default()
        };
        let b = LifecycleCounts {
            crashes: 1,
            ..LifecycleCounts::default()
        };
        assert_eq!(a.merge(&b), Err(CountOverflow));
    }
}
