//! Versioned pure projection that selects and ranks notable observations.
//!
//! The policy reads retained [`EventObservation`] records and decides which of
//! them a `/events` page or an overview preview should surface, and in what
//! order. It never mutates an observation, never writes a notable class back
//! into canonical facts, and never invents a causal explanation: an
//! out-of-memory error, a `SIGKILL` and an integrity error stay distinct
//! evidence classes. Response caps apply to a page or preview only, after the
//! full retained set has been classified.
//!
//! Classification maps a payload — a severity, a `SQLSTATE`, a signal — onto a
//! stable machine [`NotableClass`]. Ranking is a total order over the notable
//! subset; the cap keeps the head and reports how many items it omitted.

use core::cmp::Reverse;

use super::counts::{ErrorCategory, Severity, SqlState};
use super::observation::{EventObservation, ObservationId, ObservationPayload};

/// `SIGKILL` signal number; an uncatchable process termination.
const SIGNAL_SIGKILL: i32 = 9;

const SQLSTATE_DISK_FULL: SqlState = SqlState(*b"53100");
const SQLSTATE_OUT_OF_MEMORY: SqlState = SqlState(*b"53200");
const SQLSTATE_TOO_MANY_CONNECTIONS: SqlState = SqlState(*b"53300");
const SQLSTATE_DEADLOCK: SqlState = SqlState(*b"40P01");
const SQLSTATE_SERIALIZATION_FAILURE: SqlState = SqlState(*b"40001");
const SQLSTATE_QUERY_CANCELED: SqlState = SqlState(*b"57014");
const SQLSTATE_LOCK_NOT_AVAILABLE: SqlState = SqlState(*b"55P03");
const SQLSTATE_DATA_CORRUPTED: SqlState = SqlState(*b"XX001");
const SQLSTATE_INDEX_CORRUPTED: SqlState = SqlState(*b"XX002");
const SQLSTATE_INVALID_PASSWORD: SqlState = SqlState(*b"28P01");
const SQLSTATE_INVALID_AUTHORIZATION: SqlState = SqlState(*b"28000");
const SQLSTATE_INSUFFICIENT_PRIVILEGE: SqlState = SqlState(*b"42501");

/// Default page/preview cap; the deployment may lower it.
pub const DEFAULT_RESPONSE_CAP: usize = 100;

/// Absolute page/preview cap for a single response.
pub const MAX_RESPONSE_CAP: usize = 1000;

/// The evidence family of a notable observation.
///
/// The family keeps unlike catastrophes apart so a caller never reads a
/// `SIGKILL` as an out-of-memory kill or a `PANIC` as proven corruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotableEvidenceClass {
    /// A process-fatal `PANIC` line.
    Panic,
    /// An uncatchable `SIGKILL` of a backend.
    Sigkill,
    /// A backend exit with a catchable signal or crash, without `SIGKILL`.
    Availability,
    /// `SQLSTATE` `XX001`/`XX002`: data or index corruption evidence.
    IntegrityEvidence,
    /// `SQLSTATE` `53200`: an out-of-memory error observation.
    OutOfMemory,
    /// `SQLSTATE` `53100`: an out-of-disk error observation.
    StorageCapacity,
    /// `SQLSTATE` `53300`: connection slots exhausted.
    ConnectionCapacity,
    /// Lock, deadlock, or serialization contention.
    Contention,
    /// An authentication, authorization, or permission failure.
    Authentication,
}

/// A stable machine class of a notable observation.
///
/// Each variant names an observation, not a proven cause. The wire code is the
/// v1 rule-catalog identifier; the discriminant order is not the ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotableClass {
    /// A backend terminated by `SIGKILL`.
    ChildSigkill,
    /// A backend terminated by a signal other than `SIGKILL`, or crashed.
    ChildSignalTermination,
    /// A `PANIC` severity error group.
    Panic,
    /// A disk-full error (`SQLSTATE` `53100`).
    DiskFull,
    /// An out-of-memory error (`SQLSTATE` `53200`).
    OutOfMemory,
    /// Connection slots exhausted (`SQLSTATE` `53300`).
    ConnectionSlotsExhausted,
    /// A deadlock error (`SQLSTATE` `40P01`).
    Deadlock,
    /// A data/index corruption error (`SQLSTATE` `XX001`/`XX002`).
    IntegrityError,
    /// A lock-not-available error (`SQLSTATE` `55P03`).
    LockNotAvailable,
    /// A cancelled query (`SQLSTATE` `57014`).
    QueryCancelled,
    /// A serialization failure (`SQLSTATE` `40001`).
    SerializationFailure,
    /// An authentication failure (`SQLSTATE` `28P01` or `Auth` category).
    AuthenticationFailure,
    /// An authorization failure (`SQLSTATE` `28000`).
    AuthorizationFailure,
    /// A permission-denied error (`SQLSTATE` `42501`).
    PermissionDenied,
}

impl NotableClass {
    /// Every class in stable discriminant order.
    pub const ALL: [Self; 14] = [
        Self::ChildSigkill,
        Self::ChildSignalTermination,
        Self::Panic,
        Self::DiskFull,
        Self::OutOfMemory,
        Self::ConnectionSlotsExhausted,
        Self::Deadlock,
        Self::IntegrityError,
        Self::LockNotAvailable,
        Self::QueryCancelled,
        Self::SerializationFailure,
        Self::AuthenticationFailure,
        Self::AuthorizationFailure,
        Self::PermissionDenied,
    ];

    /// The stable wire code of this class.
    #[must_use]
    pub const fn wire_code(self) -> &'static str {
        match self {
            Self::ChildSigkill => "postgres_child_sigkill_observed",
            Self::ChildSignalTermination => "postgres_child_signal_termination",
            Self::Panic => "postgres_panic_observed",
            Self::DiskFull => "postgres_disk_full_observed",
            Self::OutOfMemory => "postgres_out_of_memory_observed",
            Self::ConnectionSlotsExhausted => "postgres_connection_slots_exhausted_observed",
            Self::Deadlock => "postgres_deadlock_observed",
            Self::IntegrityError => "postgres_integrity_error_observed",
            Self::LockNotAvailable => "postgres_lock_not_available_observed",
            Self::QueryCancelled => "postgres_query_cancelled_observed",
            Self::SerializationFailure => "postgres_serialization_failure_observed",
            Self::AuthenticationFailure => "postgres_authentication_failure_observed",
            Self::AuthorizationFailure => "postgres_authorization_failure_observed",
            Self::PermissionDenied => "postgres_permission_denied_observed",
        }
    }

    /// The evidence family, keeping unlike catastrophes distinct.
    #[must_use]
    pub const fn evidence_class(self) -> NotableEvidenceClass {
        match self {
            Self::ChildSigkill => NotableEvidenceClass::Sigkill,
            Self::ChildSignalTermination => NotableEvidenceClass::Availability,
            Self::Panic => NotableEvidenceClass::Panic,
            Self::DiskFull => NotableEvidenceClass::StorageCapacity,
            Self::OutOfMemory => NotableEvidenceClass::OutOfMemory,
            Self::ConnectionSlotsExhausted => NotableEvidenceClass::ConnectionCapacity,
            Self::Deadlock | Self::LockNotAvailable | Self::SerializationFailure => {
                NotableEvidenceClass::Contention
            }
            Self::IntegrityError => NotableEvidenceClass::IntegrityEvidence,
            Self::QueryCancelled => NotableEvidenceClass::Contention,
            Self::AuthenticationFailure | Self::AuthorizationFailure | Self::PermissionDenied => {
                NotableEvidenceClass::Authentication
            }
        }
    }

    /// Ranking priority; a lower value ranks earlier in a preview.
    ///
    /// Process death outranks catastrophic resource evidence, which outranks a
    /// crash, contention, and finally access-control failures.
    #[must_use]
    const fn priority(self) -> u8 {
        match self {
            Self::Panic | Self::ChildSigkill => 0,
            Self::OutOfMemory | Self::DiskFull | Self::IntegrityError => 1,
            Self::ChildSignalTermination => 2,
            Self::ConnectionSlotsExhausted | Self::Deadlock | Self::LockNotAvailable => 3,
            Self::SerializationFailure | Self::QueryCancelled => 4,
            Self::AuthenticationFailure | Self::AuthorizationFailure | Self::PermissionDenied => 5,
        }
    }
}

/// A validation failure while configuring a [`NotablePolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidNotablePolicy {
    /// The response cap was zero.
    ZeroResponseCap,
    /// The response cap exceeded [`MAX_RESPONSE_CAP`].
    ResponseCapTooLarge,
}

/// A ranked notable observation: its index in the input and its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankedNotable {
    index: usize,
    class: NotableClass,
}

impl RankedNotable {
    /// The index of this observation in the classified input slice.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// The notable class of this observation.
    #[must_use]
    pub const fn class(self) -> NotableClass {
        self.class
    }
}

/// A capped, ranked preview of the notable subset.
///
/// `ranked` holds the highest-ranked items up to the cap; `omitted_count` is
/// how many further notable items the cap dropped. Upstream retained loss is
/// separate and is not folded into this omission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotablePreview {
    ranked: Vec<RankedNotable>,
    total_notable: u64,
    omitted_count: u64,
}

impl NotablePreview {
    /// The ranked head, capped to the policy response cap.
    #[must_use]
    pub fn ranked(&self) -> &[RankedNotable] {
        &self.ranked
    }

    /// Every notable item found, before the cap.
    #[must_use]
    pub const fn total_notable(&self) -> u64 {
        self.total_notable
    }

    /// Notable items the cap dropped from this preview.
    #[must_use]
    pub const fn omitted_count(&self) -> u64 {
        self.omitted_count
    }
}

/// A versioned, pure notable-event projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotablePolicy {
    version: u32,
    response_cap: usize,
}

impl NotablePolicy {
    /// The v1 policy with the default response cap.
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            version: super::NOTABLE_POLICY_VERSION,
            response_cap: DEFAULT_RESPONSE_CAP,
        }
    }

    /// A v1 policy with an explicit response cap.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidNotablePolicy`] when the cap is zero or above
    /// [`MAX_RESPONSE_CAP`].
    pub const fn with_response_cap(response_cap: usize) -> Result<Self, InvalidNotablePolicy> {
        if response_cap == 0 {
            return Err(InvalidNotablePolicy::ZeroResponseCap);
        }
        if response_cap > MAX_RESPONSE_CAP {
            return Err(InvalidNotablePolicy::ResponseCapTooLarge);
        }
        Ok(Self {
            version: super::NOTABLE_POLICY_VERSION,
            response_cap,
        })
    }

    /// The policy version. A version change re-keys projections, not facts.
    #[must_use]
    pub const fn version(self) -> u32 {
        self.version
    }

    /// The response page/preview cap.
    #[must_use]
    pub const fn response_cap(self) -> usize {
        self.response_cap
    }

    /// Classifies one observation, or `None` when it is not notable.
    ///
    /// A generic application error without a catastrophic `SQLSTATE` is not
    /// notable in v1: rate and window calibration, not an unconditional
    /// verdict, decides those. Lifecycle records other than a crash are digest
    /// facts, not notable events.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "classification is policy-versioned; the v1 rule set is fixed"
    )]
    pub fn classify(self, observation: &EventObservation) -> Option<NotableClass> {
        match observation.payload() {
            ObservationPayload::ChildSignalTermination(payload)
            | ObservationPayload::ChildProcessCrash(payload) => {
                if payload.signal == Some(SIGNAL_SIGKILL) {
                    Some(NotableClass::ChildSigkill)
                } else {
                    Some(NotableClass::ChildSignalTermination)
                }
            }
            ObservationPayload::ErrorGroup(payload) => {
                classify_error_group(payload.severity, payload.category, payload.sqlstate)
            }
            _ => None,
        }
    }

    /// Ranks the notable subset and caps it to a preview head.
    ///
    /// The order is a strict total order: class priority, then most-recent
    /// first, then observation id. Distinct ids make the order deterministic.
    #[must_use]
    pub fn preview(self, observations: &[EventObservation]) -> NotablePreview {
        let mut sortable: Vec<(u8, Reverse<i64>, ObservationId, usize, NotableClass)> = Vec::new();
        for (index, observation) in observations.iter().enumerate() {
            if let Some(class) = self.classify(observation) {
                sortable.push((
                    class.priority(),
                    Reverse(observation.time().sort_ts_us),
                    observation.observation_id(),
                    index,
                    class,
                ));
            }
        }
        sortable.sort_unstable();

        let total_notable = saturating_u64(sortable.len());
        let ranked: Vec<RankedNotable> = sortable
            .iter()
            .take(self.response_cap)
            .map(|entry| RankedNotable {
                index: entry.3,
                class: entry.4,
            })
            .collect();
        let omitted_count = total_notable.saturating_sub(saturating_u64(ranked.len()));
        NotablePreview {
            ranked,
            total_notable,
            omitted_count,
        }
    }
}

/// The comparable severity of an observation, when it carries one.
///
/// Only error groups carry a severity. Typed lifecycle and state facts return
/// `None`, so a `min_severity` filter leaves them eligible.
#[must_use]
pub fn observation_severity(observation: &EventObservation) -> Option<Severity> {
    match observation.payload() {
        ObservationPayload::ErrorGroup(payload) => Some(payload.severity),
        _ => None,
    }
}

/// The importance rank of a severity, highest for `PANIC`.
///
/// The [`Severity`] discriminant is a marginal index, not an importance order,
/// so a `min_severity` filter compares this rank instead.
#[must_use]
pub const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Panic => 4,
        Severity::Fatal => 3,
        Severity::Error => 2,
        Severity::Warning => 1,
        Severity::Log => 0,
    }
}

/// Whether an observation passes a `min_severity` filter.
///
/// Observations without a severity always pass; typed lifecycle facts stay
/// eligible by kind.
#[must_use]
pub fn passes_min_severity(observation: &EventObservation, min_severity: Option<Severity>) -> bool {
    let Some(min_severity) = min_severity else {
        return true;
    };
    observation_severity(observation)
        .is_none_or(|severity| severity_rank(severity) >= severity_rank(min_severity))
}

fn classify_error_group(
    severity: Severity,
    category: ErrorCategory,
    sqlstate: Option<SqlState>,
) -> Option<NotableClass> {
    if severity == Severity::Panic {
        return Some(NotableClass::Panic);
    }
    if let Some(sqlstate) = sqlstate {
        let class = match sqlstate {
            SQLSTATE_DISK_FULL => Some(NotableClass::DiskFull),
            SQLSTATE_OUT_OF_MEMORY => Some(NotableClass::OutOfMemory),
            SQLSTATE_TOO_MANY_CONNECTIONS => Some(NotableClass::ConnectionSlotsExhausted),
            SQLSTATE_DEADLOCK => Some(NotableClass::Deadlock),
            SQLSTATE_SERIALIZATION_FAILURE => Some(NotableClass::SerializationFailure),
            SQLSTATE_QUERY_CANCELED => Some(NotableClass::QueryCancelled),
            SQLSTATE_LOCK_NOT_AVAILABLE => Some(NotableClass::LockNotAvailable),
            SQLSTATE_DATA_CORRUPTED | SQLSTATE_INDEX_CORRUPTED => {
                Some(NotableClass::IntegrityError)
            }
            SQLSTATE_INVALID_PASSWORD => Some(NotableClass::AuthenticationFailure),
            SQLSTATE_INVALID_AUTHORIZATION => Some(NotableClass::AuthorizationFailure),
            SQLSTATE_INSUFFICIENT_PRIVILEGE => Some(NotableClass::PermissionDenied),
            _ => None,
        };
        if class.is_some() {
            return class;
        }
    }
    if category == ErrorCategory::Auth {
        return Some(NotableClass::AuthenticationFailure);
    }
    None
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::super::DictionaryContextId;
    use super::*;
    use crate::overview::observation::{
        DroppedFieldCount, ErrorGroupPayload, EvidenceQuality, LifecyclePayload, NamingContractId,
        ObservationProvenance, ObservationShape, ObservationTime, QualityFlags, SectionBodyId,
        SegmentIdentity, SegmentLocator, SourceScopeId, TimeQuality,
    };

    fn lineage() -> SegmentIdentity {
        SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([2; 16]),
            SegmentLocator([3; 32]),
            7,
            b"type=7 rows=3 crc=abc",
        )
    }

    fn provenance(row_ordinal: u32) -> ObservationProvenance {
        ObservationProvenance {
            segment_locator: Some(SegmentLocator([3; 32])),
            section_body_id: SectionBodyId([0xAA; 32]),
            catalog_entry_ordinal: 0,
            row_ordinal,
            dictionary_context_id: DictionaryContextId([0xBB; 32]),
            source_locator: None,
        }
    }

    fn error_group(
        row_ordinal: u32,
        sort_ts_us: i64,
        severity: Severity,
        category: ErrorCategory,
        sqlstate: Option<[u8; 5]>,
    ) -> EventObservation {
        let payload = ErrorGroupPayload {
            severity,
            category,
            sqlstate: sqlstate.map(SqlState),
            normalized_pattern: None,
            sample: None,
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: None,
            user: None,
            dropped_field_count: DroppedFieldCount::default(),
        };
        EventObservation::new(
            lineage(),
            7,
            provenance(row_ordinal),
            ObservationShape::GroupedCount,
            ObservationTime {
                sort_ts_us,
                occurred_at_us: Some(sort_ts_us),
                observed_interval: None,
                quality: TimeQuality::FirstInGroup,
            },
            2,
            ObservationPayload::ErrorGroup(Box::new(payload)),
            EvidenceQuality::Parsed,
            QualityFlags::default(),
            None,
        )
        .expect("valid error-group fixture")
    }

    fn signal_termination(row_ordinal: u32, sort_ts_us: i64, signal: i32) -> EventObservation {
        let payload = LifecyclePayload {
            pid: Some(4242),
            signal: Some(signal),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dropped_field_count: DroppedFieldCount::default(),
        };
        EventObservation::new(
            lineage(),
            7,
            provenance(row_ordinal),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us,
                occurred_at_us: Some(sort_ts_us),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ChildSignalTermination(Box::new(payload)),
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
        .expect("valid signal-termination fixture")
    }

    fn ready(row_ordinal: u32) -> EventObservation {
        let payload = LifecyclePayload {
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dropped_field_count: DroppedFieldCount::default(),
        };
        EventObservation::new(
            lineage(),
            7,
            provenance(row_ordinal),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: 1,
                occurred_at_us: Some(1),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ReadyObserved(Box::new(payload)),
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
        .expect("valid ready fixture")
    }

    #[test]
    fn sigkill_signal_nine_classifies_as_sigkill_not_signal_termination() {
        let policy = NotablePolicy::v1();
        let sigkill = signal_termination(0, 10, 9);
        let other = signal_termination(1, 10, 15);
        assert_eq!(policy.classify(&sigkill), Some(NotableClass::ChildSigkill));
        assert_eq!(
            policy.classify(&other),
            Some(NotableClass::ChildSignalTermination)
        );
    }

    #[test]
    fn panic_severity_classifies_as_panic_before_sqlstate() {
        let policy = NotablePolicy::v1();
        let observation = error_group(0, 5, Severity::Panic, ErrorCategory::System, None);
        assert_eq!(policy.classify(&observation), Some(NotableClass::Panic));
    }

    #[test]
    fn each_catastrophic_sqlstate_maps_to_its_own_class() {
        let policy = NotablePolicy::v1();
        let cases: &[(&[u8; 5], NotableClass)] = &[
            (b"53100", NotableClass::DiskFull),
            (b"53200", NotableClass::OutOfMemory),
            (b"53300", NotableClass::ConnectionSlotsExhausted),
            (b"40P01", NotableClass::Deadlock),
            (b"40001", NotableClass::SerializationFailure),
            (b"57014", NotableClass::QueryCancelled),
            (b"55P03", NotableClass::LockNotAvailable),
            (b"XX001", NotableClass::IntegrityError),
            (b"XX002", NotableClass::IntegrityError),
            (b"28P01", NotableClass::AuthenticationFailure),
            (b"28000", NotableClass::AuthorizationFailure),
            (b"42501", NotableClass::PermissionDenied),
        ];
        for (code, expected) in cases {
            let observation =
                error_group(0, 5, Severity::Error, ErrorCategory::Resource, Some(**code));
            assert_eq!(
                policy.classify(&observation),
                Some(*expected),
                "sqlstate {} must classify deterministically",
                core::str::from_utf8(*code).expect("ascii sqlstate")
            );
        }
    }

    #[test]
    fn generic_application_error_is_not_notable() {
        let policy = NotablePolicy::v1();
        let observation = error_group(0, 5, Severity::Error, ErrorCategory::Syntax, None);
        assert_eq!(policy.classify(&observation), None);
    }

    #[test]
    fn auth_category_without_sqlstate_is_notable() {
        let policy = NotablePolicy::v1();
        let observation = error_group(0, 5, Severity::Fatal, ErrorCategory::Auth, None);
        assert_eq!(
            policy.classify(&observation),
            Some(NotableClass::AuthenticationFailure)
        );
    }

    #[test]
    fn lifecycle_ready_is_not_notable() {
        let policy = NotablePolicy::v1();
        assert_eq!(policy.classify(&ready(0)), None);
    }

    #[test]
    fn preview_ranks_by_priority_then_recency() {
        let policy = NotablePolicy::v1();
        let observations = vec![
            error_group(
                0,
                100,
                Severity::Fatal,
                ErrorCategory::Auth,
                Some(*b"28P01"),
            ),
            error_group(1, 50, Severity::Panic, ErrorCategory::System, None),
            error_group(2, 200, Severity::Panic, ErrorCategory::System, None),
        ];
        let preview = policy.preview(&observations);
        let order: Vec<NotableClass> = preview.ranked().iter().map(|r| r.class()).collect();
        assert_eq!(
            order,
            vec![
                NotableClass::Panic,
                NotableClass::Panic,
                NotableClass::AuthenticationFailure,
            ],
            "panics outrank auth; the newer panic ranks first"
        );
        let indices: Vec<usize> = preview.ranked().iter().map(|r| r.index()).collect();
        assert_eq!(
            indices,
            vec![2, 1, 0],
            "newer panic (index 2) precedes older"
        );
    }

    #[test]
    fn preview_cap_limits_page_and_reports_omitted() {
        let policy = NotablePolicy::with_response_cap(2).expect("valid cap");
        let observations = vec![
            error_group(0, 10, Severity::Panic, ErrorCategory::System, None),
            error_group(1, 20, Severity::Panic, ErrorCategory::System, None),
            error_group(2, 30, Severity::Panic, ErrorCategory::System, None),
        ];
        let preview = policy.preview(&observations);
        assert_eq!(preview.ranked().len(), 2, "cap keeps only the head");
        assert_eq!(preview.total_notable(), 3);
        assert_eq!(preview.omitted_count(), 1);
    }

    #[test]
    fn preview_of_non_notable_input_is_empty() {
        let policy = NotablePolicy::v1();
        let observations = vec![ready(0), ready(1)];
        let preview = policy.preview(&observations);
        assert!(preview.ranked().is_empty());
        assert_eq!(preview.total_notable(), 0);
        assert_eq!(preview.omitted_count(), 0);
    }

    #[test]
    fn min_severity_filters_error_groups_but_keeps_lifecycle() {
        let panic = error_group(0, 5, Severity::Panic, ErrorCategory::System, None);
        let warning = error_group(1, 5, Severity::Warning, ErrorCategory::Other, None);
        let crash = signal_termination(2, 5, 9);
        assert!(passes_min_severity(&panic, Some(Severity::Fatal)));
        assert!(!passes_min_severity(&warning, Some(Severity::Fatal)));
        assert!(
            passes_min_severity(&crash, Some(Severity::Fatal)),
            "a crash without severity stays eligible by kind"
        );
    }

    #[test]
    fn response_cap_bounds_are_enforced() {
        assert_eq!(
            NotablePolicy::with_response_cap(0),
            Err(InvalidNotablePolicy::ZeroResponseCap)
        );
        assert_eq!(
            NotablePolicy::with_response_cap(MAX_RESPONSE_CAP + 1),
            Err(InvalidNotablePolicy::ResponseCapTooLarge)
        );
        assert!(NotablePolicy::with_response_cap(MAX_RESPONSE_CAP).is_ok());
    }

    #[test]
    fn wire_codes_are_stable_and_unique() {
        let mut codes: Vec<&str> = NotableClass::ALL.iter().map(|c| c.wire_code()).collect();
        codes.sort_unstable();
        let unique = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), unique, "every class has a distinct wire code");
        assert_eq!(
            NotableClass::ChildSigkill.wire_code(),
            "postgres_child_sigkill_observed"
        );
    }
}
