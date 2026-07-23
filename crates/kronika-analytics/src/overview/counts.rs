//! Exact event-count aggregation over the joint `(severity, category,
//! sqlstate)` dimension.
//!
//! Counts are kept over the joint key, not three marginal maps, so a range can
//! answer how many `Resource` `FATAL` errors occurred — a question the
//! marginals cannot. Marginal totals are projections of the joint set.
//!
//! Construction and merge use checked addition. Constructors bound
//! unnormalized input work; every stored sparse dimension has a separate cap.

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

/// A count exceeded [`u64::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountOverflow;

/// Collection bound exceeded while constructing a count set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountResource {
    /// Input entries consumed while building a sparse dimension.
    InputEntries,
    /// Joint `(severity, category, SQLSTATE)` keys.
    JointKeys,
    /// Distinct lifecycle signal numbers.
    SignalKeys,
}

/// Failure to construct or merge a bounded count set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountError {
    /// A count exceeded [`u64::MAX`].
    Overflow,
    /// A configured key bound was exceeded.
    LimitExceeded(CountResource),
}

impl From<CountOverflow> for CountError {
    fn from(_: CountOverflow) -> Self {
        Self::Overflow
    }
}

/// Allocation bounds for sparse count dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the max_ prefix distinguishes hard caps in the public limits API"
)]
pub struct CountLimits {
    /// Maximum unnormalized entries consumed by one constructor.
    pub max_input_entries: usize,
    /// Maximum distinct joint error keys.
    pub max_joint_keys: usize,
    /// Maximum distinct lifecycle signals.
    pub max_signal_keys: usize,
}

/// Lifecycle observation counts, kept apart from error-line counts.
///
/// They are separate because one physical crash is recorded in both
/// `pg_log_lifecycle` and `pg_log_errors`; adding them across the two would
/// double-count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleCounts {
    crashes: u64,
    shutdowns: u64,
    ready: u64,
    signals: Vec<(i32, u64)>,
}

impl LifecycleCounts {
    /// Builds normalized lifecycle counts within `limits`.
    ///
    /// # Errors
    /// Returns [`CountError::Overflow`] for an overflowing duplicate signal,
    /// or [`CountError::LimitExceeded`] for too many distinct signals.
    pub fn new(
        crashes: u64,
        shutdowns: u64,
        ready: u64,
        signal_entries: impl IntoIterator<Item = (i32, u64)>,
        limits: CountLimits,
    ) -> Result<Self, CountError> {
        let mut signals: BTreeMap<i32, u64> = BTreeMap::new();
        for (index, (signal, count)) in signal_entries.into_iter().enumerate() {
            if index == limits.max_input_entries {
                return Err(CountError::LimitExceeded(CountResource::InputEntries));
            }
            if count == 0 {
                continue;
            }
            if !signals.contains_key(&signal) && signals.len() == limits.max_signal_keys {
                return Err(CountError::LimitExceeded(CountResource::SignalKeys));
            }
            let slot = signals.entry(signal).or_insert(0);
            *slot = slot.checked_add(count).ok_or(CountError::Overflow)?;
        }
        Ok(Self {
            crashes,
            shutdowns,
            ready,
            signals: signals.into_iter().collect(),
        })
    }

    /// Child-process crash and signal-termination observations.
    #[must_use]
    pub const fn crashes(&self) -> u64 {
        self.crashes
    }

    /// Shutdown-request observations.
    #[must_use]
    pub const fn shutdowns(&self) -> u64 {
        self.shutdowns
    }

    /// Ready observations.
    #[must_use]
    pub const fn ready(&self) -> u64 {
        self.ready
    }

    /// Sorted, unique nonzero signal counts.
    #[must_use]
    pub fn signals(&self) -> &[(i32, u64)] {
        &self.signals
    }

    fn merge(&self, other: &Self, limits: CountLimits) -> Result<Self, CountError> {
        if self.signals.len() > limits.max_signal_keys
            || other.signals.len() > limits.max_signal_keys
        {
            return Err(CountError::LimitExceeded(CountResource::SignalKeys));
        }
        let mut signals: BTreeMap<i32, u64> = BTreeMap::new();
        for &(signal, count) in self.signals.iter().chain(&other.signals) {
            if !signals.contains_key(&signal) && signals.len() == limits.max_signal_keys {
                return Err(CountError::LimitExceeded(CountResource::SignalKeys));
            }
            let slot = signals.entry(signal).or_insert(0);
            *slot = slot.checked_add(count).ok_or(CountError::Overflow)?;
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
    lifecycle: LifecycleCounts,
}

impl EventCounts {
    /// Builds a count set from joint entries.
    ///
    /// Duplicate keys sum with checked addition, zero-count keys drop, and the
    /// stored vector is sorted and unique regardless of input order.
    ///
    /// # Errors
    /// Returns [`CountError`] if a count or key bound is exceeded.
    pub fn from_joint(
        entries: impl IntoIterator<Item = (JointErrorKey, u64)>,
        lifecycle: LifecycleCounts,
        limits: CountLimits,
    ) -> Result<Self, CountError> {
        if lifecycle.signals.len() > limits.max_signal_keys {
            return Err(CountError::LimitExceeded(CountResource::SignalKeys));
        }
        let mut map: BTreeMap<JointErrorKey, u64> = BTreeMap::new();
        for (index, (key, count)) in entries.into_iter().enumerate() {
            if index == limits.max_input_entries {
                return Err(CountError::LimitExceeded(CountResource::InputEntries));
            }
            if count == 0 {
                continue;
            }
            if !map.contains_key(&key) && map.len() == limits.max_joint_keys {
                return Err(CountError::LimitExceeded(CountResource::JointKeys));
            }
            let slot = map.entry(key).or_insert(0);
            *slot = slot.checked_add(count).ok_or(CountError::Overflow)?;
        }
        Ok(Self {
            joint: map.into_iter().collect(),
            lifecycle,
        })
    }

    /// The joint entries, sorted by key and unique.
    #[must_use]
    pub fn joint(&self) -> &[(JointErrorKey, u64)] {
        &self.joint
    }

    /// Lifecycle observation counts.
    #[must_use]
    pub const fn lifecycle(&self) -> &LifecycleCounts {
        &self.lifecycle
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
    /// Returns [`CountError`] if a count or key bound is exceeded.
    pub fn merge(&self, other: &Self, limits: CountLimits) -> Result<Self, CountError> {
        if self.joint.len() > limits.max_joint_keys || other.joint.len() > limits.max_joint_keys {
            return Err(CountError::LimitExceeded(CountResource::JointKeys));
        }
        let mut map: BTreeMap<JointErrorKey, u64> = BTreeMap::new();
        for &(key, count) in self.joint.iter().chain(&other.joint) {
            if !map.contains_key(&key) && map.len() == limits.max_joint_keys {
                return Err(CountError::LimitExceeded(CountResource::JointKeys));
            }
            let slot = map.entry(key).or_insert(0);
            *slot = slot.checked_add(count).ok_or(CountError::Overflow)?;
        }
        Ok(Self {
            joint: map.into_iter().collect(),
            lifecycle: self.lifecycle.merge(&other.lifecycle, limits)?,
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

    const LIMITS: CountLimits = CountLimits {
        max_input_entries: 64,
        max_joint_keys: 32,
        max_signal_keys: 32,
    };

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
        EventCounts::from_joint(entries.iter().copied(), LifecycleCounts::default(), LIMITS)
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
        assert_eq!(a.merge(&b, LIMITS), b.merge(&a, LIMITS));
    }

    #[test]
    fn merge_is_associative() {
        let a = counts(&[(key(Severity::Error, ErrorCategory::Lock, None), 3)]);
        let b = counts(&[(key(Severity::Error, ErrorCategory::Lock, None), 4)]);
        let c = counts(&[(
            key(Severity::Fatal, ErrorCategory::Auth, Some(*b"28000")),
            2,
        )]);
        let left = a.merge(&b, LIMITS).and_then(|ab| ab.merge(&c, LIMITS));
        let right = b.merge(&c, LIMITS).and_then(|bc| a.merge(&bc, LIMITS));
        assert_eq!(left, right);
    }

    #[test]
    fn empty_is_the_merge_identity() {
        let a = counts(&[(key(Severity::Warning, ErrorCategory::Other, None), 5)]);
        let empty = EventCounts::default();
        assert_eq!(a.merge(&empty, LIMITS), Ok(a.clone()));
        assert_eq!(empty.merge(&a, LIMITS), Ok(a));
    }

    #[test]
    fn overflow_is_reported_not_saturated() {
        let k = key(Severity::Error, ErrorCategory::Other, None);
        let a = counts(&[(k, u64::MAX)]);
        let b = counts(&[(k, 1)]);
        assert_eq!(a.merge(&b, LIMITS), Err(CountError::Overflow));
    }

    #[test]
    fn lifecycle_signals_merge_sorted_and_checked() {
        let a = LifecycleCounts::new(1, 0, 0, [(9, 2), (6, 1)], LIMITS).expect("valid fixture");
        let b = LifecycleCounts::new(1, 0, 0, [(9, 3)], LIMITS).expect("valid fixture");
        let merged = a.merge(&b, LIMITS).expect("no overflow");
        assert_eq!(merged.crashes(), 2);
        assert_eq!(merged.signals(), &[(6, 1), (9, 5)]);
    }

    #[test]
    fn lifecycle_overflow_is_reported() {
        let a = LifecycleCounts::new(u64::MAX, 0, 0, [], LIMITS).expect("valid fixture");
        let b = LifecycleCounts::new(1, 0, 0, [], LIMITS).expect("valid fixture");
        assert_eq!(a.merge(&b, LIMITS), Err(CountError::Overflow));
    }

    #[test]
    fn sparse_dimensions_are_bounded_and_normalized() {
        let signal_limit = CountLimits {
            max_input_entries: 3,
            max_joint_keys: 1,
            max_signal_keys: 1,
        };
        assert_eq!(
            LifecycleCounts::new(0, 0, 0, [(9, 0), (6, 1), (9, 1)], signal_limit),
            Err(CountError::LimitExceeded(CountResource::SignalKeys))
        );

        let a = key(Severity::Error, ErrorCategory::Lock, None);
        let b = key(Severity::Fatal, ErrorCategory::Resource, None);
        assert_eq!(
            EventCounts::from_joint([(a, 1), (b, 1)], LifecycleCounts::default(), signal_limit,),
            Err(CountError::LimitExceeded(CountResource::JointKeys))
        );

        let work_limit = CountLimits {
            max_input_entries: 1,
            max_joint_keys: 2,
            max_signal_keys: 2,
        };
        assert_eq!(
            EventCounts::from_joint([(a, 1), (a, 1)], LifecycleCounts::default(), work_limit,),
            Err(CountError::LimitExceeded(CountResource::InputEntries))
        );

        let one = EventCounts::from_joint([(a, 1)], LifecycleCounts::default(), work_limit)
            .expect("one entry fits");
        assert_eq!(
            one.merge(&one, work_limit),
            Ok(
                EventCounts::from_joint([(a, 2)], LifecycleCounts::default(), work_limit,)
                    .expect("canonical merge is bounded by key caps")
            )
        );

        let wide_lifecycle =
            LifecycleCounts::new(0, 0, 0, [(6, 1), (9, 1)], LIMITS).expect("wide fixture");
        assert_eq!(
            EventCounts::from_joint([], wide_lifecycle, signal_limit),
            Err(CountError::LimitExceeded(CountResource::SignalKeys))
        );

        let merge_limits = CountLimits {
            max_input_entries: 1,
            max_joint_keys: 2,
            max_signal_keys: 2,
        };
        let signal_six = LifecycleCounts::new(0, 0, 0, [(6, 1)], merge_limits).expect("one signal");
        let signal_nine =
            LifecycleCounts::new(0, 0, 0, [(9, 1)], merge_limits).expect("one signal");
        assert_eq!(
            signal_six
                .merge(&signal_nine, merge_limits)
                .map(|counts| counts.signals),
            Ok(vec![(6, 1), (9, 1)])
        );
    }
}
