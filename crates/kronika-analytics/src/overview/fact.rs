//! Policy-neutral event facts derived from retained source observations.
//!
//! An [`EventFact`] is canonical data, not a presentation decision. It keeps
//! stable machine taxonomy, bounded normalized fields, evidence quality,
//! coverage provenance, and links to the observations that support it.
//! Notable ranking and incident diagnosis intentionally live outside this
//! module.

use std::cmp::Ordering;

use super::counts::{ErrorCategory, Severity, SqlState};
use super::coverage::{CoverageSpan, RetainedExactness};
use super::finite::FiniteF64;
use super::metric::{MetricFactor, MetricSeriesDescriptor, MetricUnit};
use super::observation::{
    DroppedFieldCount, EventObservation, EvidenceQuality, FactId, LossReason, LossSummary,
    ObservationId, ObservationPayload, ObservationShape, SourceScopeId,
};
use super::reduce::{CounterInterval, CounterSample, GaugeSample, PairQuality};
use super::sha256;

const EVENT_FACT_DOMAIN_TAG: &[u8] = b"pgk-overview-canonical-event-fact-v1";
const METRIC_FACT_DOMAIN_TAG: &[u8] = b"pgk-overview-canonical-metric-fact-v1";
const COUNTER_SAMPLE_OBSERVATION_TAG: &[u8] = b"pgk-overview-counter-sample-observation-v1";
const GAUGE_SAMPLE_OBSERVATION_TAG: &[u8] = b"pgk-overview-gauge-sample-observation-v1";
const COLLECTOR_LOSS_OBSERVATION_TAG: &[u8] = b"pgk-overview-collector-loss-observation-v1";

/// Stable event taxonomy represented by canonical facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    /// Grouped PostgreSQL log error.
    PgLogErrorGroupObserved,
    /// Child process terminated by a signal.
    PgLifecycleChildSignalTermination,
    /// Child process crashed without a retained signal.
    PgLifecycleChildProcessCrash,
    /// Administrative shutdown request.
    PgLifecycleShutdownRequested,
    /// PostgreSQL readiness observation.
    PgLifecycleReadyObserved,
    /// Checkpoint start.
    PgCheckpointStarted,
    /// Checkpoint completion.
    PgCheckpointCompleted,
    /// Too-frequent checkpoint report.
    PgCheckpointTooFrequentReported,
    /// Autovacuum report.
    PgMaintenanceAutovacuumReported,
    /// Autoanalyze report.
    PgMaintenanceAutoanalyzeReported,
    /// Grouped slow-query report.
    PgQuerySlowGroupReported,
    /// Lock-wait report.
    PgLockWaitReported,
    /// Lock acquired after a wait.
    PgLockAcquiredAfterWaitReported,
    /// Temporary-file report.
    PgTempFileReported,
    /// Per-database deadlock counter delta.
    PgDatabaseDeadlockDelta,
    /// Per-database recovery-conflict counter delta.
    PgDatabaseRecoveryConflictDelta,
    /// Per-database checksum-failure counter delta.
    PgDatabaseChecksumFailureDelta,
    /// Per-database abandoned-session counter delta.
    PgDatabaseSessionsAbandonedDelta,
    /// Per-database fatal-session counter delta.
    PgDatabaseSessionsFatalDelta,
    /// Per-database operator-killed-session counter delta.
    PgDatabaseSessionsKilledDelta,
    /// PostgreSQL statistics reset boundary.
    PgStatisticsResetObserved,
    /// Postmaster start identity changed.
    PgPostmasterStartChanged,
    /// Recovery role changed.
    PgRecoveryRoleChanged,
    /// PostgreSQL timeline changed.
    PgTimelineChanged,
    /// Replication sender state changed.
    PgReplicationSenderStateChanged,
    /// Replication sender disappeared from complete snapshots.
    PgReplicationSenderDisappeared,
    /// Replication slot state changed.
    PgReplicationSlotStateChanged,
    /// Replication slot became lost.
    PgReplicationSlotLost,
    /// Cgroup memory.high event delta.
    OsCgroupMemoryHighDelta,
    /// Cgroup memory.max event delta.
    OsCgroupMemoryMaxDelta,
    /// Cgroup OOM event delta.
    OsCgroupOomDelta,
    /// Cgroup OOM-kill event delta.
    OsCgroupOomKillDelta,
    /// Host OOM-kill event delta.
    OsHostOomKillDelta,
    /// Proven PostgreSQL filesystem-capacity observation.
    OsFilesystemCapacityObservation,
    /// Proven transition to zero available filesystem bytes.
    OsFilesystemCapacityZeroTransition,
    /// Snapshot collection gap.
    CollectorSnapshotGap,
    /// Source read failure retained as a fact.
    CollectorSourceReadFailure,
    /// Restricted source visibility retained as a fact.
    CollectorVisibilityRestricted,
}

impl EventKind {
    /// Stable numeric code used by the canonical fact codec.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::PgLogErrorGroupObserved => 1,
            Self::PgLifecycleChildSignalTermination => 2,
            Self::PgLifecycleChildProcessCrash => 3,
            Self::PgLifecycleShutdownRequested => 4,
            Self::PgLifecycleReadyObserved => 5,
            Self::PgCheckpointStarted => 6,
            Self::PgCheckpointCompleted => 7,
            Self::PgCheckpointTooFrequentReported => 8,
            Self::PgMaintenanceAutovacuumReported => 9,
            Self::PgMaintenanceAutoanalyzeReported => 10,
            Self::PgQuerySlowGroupReported => 11,
            Self::PgLockWaitReported => 12,
            Self::PgLockAcquiredAfterWaitReported => 13,
            Self::PgTempFileReported => 14,
            Self::PgDatabaseDeadlockDelta => 100,
            Self::PgDatabaseRecoveryConflictDelta => 101,
            Self::PgDatabaseChecksumFailureDelta => 102,
            Self::PgDatabaseSessionsAbandonedDelta => 103,
            Self::PgDatabaseSessionsFatalDelta => 104,
            Self::PgDatabaseSessionsKilledDelta => 105,
            Self::PgStatisticsResetObserved => 106,
            Self::PgPostmasterStartChanged => 107,
            Self::PgRecoveryRoleChanged => 108,
            Self::PgTimelineChanged => 109,
            Self::PgReplicationSenderStateChanged => 110,
            Self::PgReplicationSenderDisappeared => 111,
            Self::PgReplicationSlotStateChanged => 112,
            Self::PgReplicationSlotLost => 113,
            Self::OsCgroupMemoryHighDelta => 200,
            Self::OsCgroupMemoryMaxDelta => 201,
            Self::OsCgroupOomDelta => 202,
            Self::OsCgroupOomKillDelta => 203,
            Self::OsHostOomKillDelta => 204,
            Self::OsFilesystemCapacityObservation => 205,
            Self::OsFilesystemCapacityZeroTransition => 206,
            Self::CollectorSnapshotGap => 300,
            Self::CollectorSourceReadFailure => 301,
            Self::CollectorVisibilityRestricted => 302,
        }
    }

    /// Decodes a stable numeric code.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            1 => Some(Self::PgLogErrorGroupObserved),
            2 => Some(Self::PgLifecycleChildSignalTermination),
            3 => Some(Self::PgLifecycleChildProcessCrash),
            4 => Some(Self::PgLifecycleShutdownRequested),
            5 => Some(Self::PgLifecycleReadyObserved),
            6 => Some(Self::PgCheckpointStarted),
            7 => Some(Self::PgCheckpointCompleted),
            8 => Some(Self::PgCheckpointTooFrequentReported),
            9 => Some(Self::PgMaintenanceAutovacuumReported),
            10 => Some(Self::PgMaintenanceAutoanalyzeReported),
            11 => Some(Self::PgQuerySlowGroupReported),
            12 => Some(Self::PgLockWaitReported),
            13 => Some(Self::PgLockAcquiredAfterWaitReported),
            14 => Some(Self::PgTempFileReported),
            100 => Some(Self::PgDatabaseDeadlockDelta),
            101 => Some(Self::PgDatabaseRecoveryConflictDelta),
            102 => Some(Self::PgDatabaseChecksumFailureDelta),
            103 => Some(Self::PgDatabaseSessionsAbandonedDelta),
            104 => Some(Self::PgDatabaseSessionsFatalDelta),
            105 => Some(Self::PgDatabaseSessionsKilledDelta),
            106 => Some(Self::PgStatisticsResetObserved),
            107 => Some(Self::PgPostmasterStartChanged),
            108 => Some(Self::PgRecoveryRoleChanged),
            109 => Some(Self::PgTimelineChanged),
            110 => Some(Self::PgReplicationSenderStateChanged),
            111 => Some(Self::PgReplicationSenderDisappeared),
            112 => Some(Self::PgReplicationSlotStateChanged),
            113 => Some(Self::PgReplicationSlotLost),
            200 => Some(Self::OsCgroupMemoryHighDelta),
            201 => Some(Self::OsCgroupMemoryMaxDelta),
            202 => Some(Self::OsCgroupOomDelta),
            203 => Some(Self::OsCgroupOomKillDelta),
            204 => Some(Self::OsHostOomKillDelta),
            205 => Some(Self::OsFilesystemCapacityObservation),
            206 => Some(Self::OsFilesystemCapacityZeroTransition),
            300 => Some(Self::CollectorSnapshotGap),
            301 => Some(Self::CollectorSourceReadFailure),
            302 => Some(Self::CollectorVisibilityRestricted),
            _ => None,
        }
    }

    /// Locale-neutral machine code used by HTTP presenters and diagnostics.
    #[must_use]
    pub const fn wire_code(self) -> &'static str {
        match self {
            Self::PgLogErrorGroupObserved => "pg.log.error_group_observed",
            Self::PgLifecycleChildSignalTermination => {
                "pg.lifecycle.child_signal_termination"
            }
            Self::PgLifecycleChildProcessCrash => "pg.lifecycle.child_process_crash",
            Self::PgLifecycleShutdownRequested => "pg.lifecycle.shutdown_requested",
            Self::PgLifecycleReadyObserved => "pg.lifecycle.ready_observed",
            Self::PgCheckpointStarted => "pg.checkpoint.started",
            Self::PgCheckpointCompleted => "pg.checkpoint.completed",
            Self::PgCheckpointTooFrequentReported => {
                "pg.checkpoint.too_frequent_reported"
            }
            Self::PgMaintenanceAutovacuumReported => {
                "pg.maintenance.autovacuum_reported"
            }
            Self::PgMaintenanceAutoanalyzeReported => {
                "pg.maintenance.autoanalyze_reported"
            }
            Self::PgQuerySlowGroupReported => "pg.query.slow_group_reported",
            Self::PgLockWaitReported => "pg.lock.wait_reported",
            Self::PgLockAcquiredAfterWaitReported => {
                "pg.lock.acquired_after_wait_reported"
            }
            Self::PgTempFileReported => "pg.temp_file.reported",
            Self::PgDatabaseDeadlockDelta => "pg.database.deadlock_delta",
            Self::PgDatabaseRecoveryConflictDelta => "pg.database.recovery_conflict_delta",
            Self::PgDatabaseChecksumFailureDelta => "pg.database.checksum_failure_delta",
            Self::PgDatabaseSessionsAbandonedDelta => "pg.database.sessions_abandoned_delta",
            Self::PgDatabaseSessionsFatalDelta => "pg.database.sessions_fatal_delta",
            Self::PgDatabaseSessionsKilledDelta => "pg.database.sessions_killed_delta",
            Self::PgStatisticsResetObserved => "pg.statistics.reset_observed",
            Self::PgPostmasterStartChanged => "pg.postmaster.start_changed",
            Self::PgRecoveryRoleChanged => "pg.recovery.role_changed",
            Self::PgTimelineChanged => "pg.timeline.changed",
            Self::PgReplicationSenderStateChanged => "pg.replication.sender_state_changed",
            Self::PgReplicationSenderDisappeared => "pg.replication.sender_disappeared",
            Self::PgReplicationSlotStateChanged => "pg.replication.slot_state_changed",
            Self::PgReplicationSlotLost => "pg.replication.slot_lost",
            Self::OsCgroupMemoryHighDelta => "os.cgroup.memory_high_delta",
            Self::OsCgroupMemoryMaxDelta => "os.cgroup.memory_max_delta",
            Self::OsCgroupOomDelta => "os.cgroup.oom_delta",
            Self::OsCgroupOomKillDelta => "os.cgroup.oom_kill_delta",
            Self::OsHostOomKillDelta => "os.host.oom_kill_delta",
            Self::OsFilesystemCapacityObservation => "os.filesystem.capacity_observation",
            Self::OsFilesystemCapacityZeroTransition => {
                "os.filesystem.capacity_zero_transition"
            }
            Self::CollectorSnapshotGap => "collector.snapshot_gap",
            Self::CollectorSourceReadFailure => "collector.source_read_failure",
            Self::CollectorVisibilityRestricted => "collector.visibility_restricted",
        }
    }
}

/// Cardinality/time shape of one canonical fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactShape {
    /// One occurrence represented by one fact.
    Individual,
    /// A bounded group with a retained occurrence count.
    GroupedCount,
    /// A condition applying to a half-open interval.
    Interval,
}

/// Stable entity class for a fact or metric series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityKind {
    /// A PostgreSQL database.
    Database,
    /// A PostgreSQL postmaster instance.
    Postmaster,
    /// A physical replication sender.
    ReplicationSender,
    /// A replication slot.
    ReplicationSlot,
    /// A cgroup.
    Cgroup,
    /// The host.
    Host,
    /// A proven PostgreSQL storage mount.
    Filesystem,
}

impl EntityKind {
    /// Stable codec discriminant.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Database => 1,
            Self::Postmaster => 2,
            Self::ReplicationSender => 3,
            Self::ReplicationSlot => 4,
            Self::Cgroup => 5,
            Self::Host => 6,
            Self::Filesystem => 7,
        }
    }

    /// Decodes a stable discriminant.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Database),
            2 => Some(Self::Postmaster),
            3 => Some(Self::ReplicationSender),
            4 => Some(Self::ReplicationSlot),
            5 => Some(Self::Cgroup),
            6 => Some(Self::Host),
            7 => Some(Self::Filesystem),
            _ => None,
        }
    }
}

/// Bounded content identity of a source entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityRef {
    /// Entity class.
    pub kind: EntityKind,
    /// Source-scope-qualified content identity.
    pub id: [u8; 16],
}

/// Coverage provenance attached to a canonical fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageRef {
    /// Scope in which supporting observations and samples are authoritative.
    pub source_scope_id: SourceScopeId,
    /// Exactness of values retained for this fact.
    pub retained_exactness: RetainedExactness,
    /// Proven upstream loss, if any.
    pub loss: Option<LossSummary>,
}

/// Canonical grouped-error dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorFactPayload {
    /// Retained severity.
    pub severity: Severity,
    /// Policy-neutral classifier category.
    pub category: ErrorCategory,
    /// Structured or parsed SQLSTATE.
    pub sqlstate: Option<SqlState>,
    /// Bounded normalized grouping pattern.
    pub normalized_pattern: Option<Box<str>>,
    /// Database dimension, when retained.
    pub database: Option<Box<str>>,
    /// User dimension, when retained.
    pub user: Option<Box<str>>,
    /// Number of source text fields dropped by dictionary bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Canonical lifecycle dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleFactPayload {
    /// Child process ID, when present.
    pub pid: Option<i32>,
    /// Raw signal, when present.
    pub signal: Option<i32>,
    /// Bounded administrative shutdown mode.
    pub shutdown_mode: Option<Box<str>>,
    /// Number of source fields dropped by bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Canonical checkpoint dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointFactPayload {
    /// Start reason or bounded warning class.
    pub reason: Option<Box<str>>,
    /// Reported seconds between checkpoints.
    pub seconds_apart: Option<i64>,
    /// Buffers written.
    pub buffers_written: Option<i64>,
    /// Write phase, milliseconds.
    pub write_ms: Option<FiniteF64>,
    /// Sync phase, milliseconds.
    pub sync_ms: Option<FiniteF64>,
    /// Total duration, milliseconds.
    pub total_ms: Option<FiniteF64>,
    /// WAL distance, KiB.
    pub distance_kb: Option<i64>,
    /// Estimated WAL distance, KiB.
    pub estimate_kb: Option<i64>,
    /// WAL files added.
    pub wal_added: Option<i64>,
    /// WAL files removed.
    pub wal_removed: Option<i64>,
    /// WAL files recycled.
    pub wal_recycled: Option<i64>,
    /// Files synchronized.
    pub sync_files: Option<i64>,
    /// Longest file sync, milliseconds.
    pub longest_sync_ms: Option<FiniteF64>,
    /// Average file sync, milliseconds.
    pub average_sync_ms: Option<FiniteF64>,
    /// Number of source fields dropped by bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Canonical maintenance dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceFactPayload {
    /// Bounded qualified relation name.
    pub relation: Option<Box<str>>,
    /// Index scans.
    pub index_scans: Option<i64>,
    /// Heap pages removed.
    pub pages_removed: Option<i64>,
    /// Heap pages remaining.
    pub pages_remaining: Option<i64>,
    /// Tuples removed.
    pub tuples_removed: Option<i64>,
    /// Tuples remaining.
    pub tuples_remaining: Option<i64>,
    /// Dead tuples not yet removable.
    pub tuples_dead_not_removable: Option<i64>,
    /// Elapsed duration, milliseconds.
    pub elapsed_ms: Option<FiniteF64>,
    /// Buffer hits.
    pub buffer_hits: Option<i64>,
    /// Buffer misses.
    pub buffer_misses: Option<i64>,
    /// Buffers dirtied.
    pub buffer_dirtied: Option<i64>,
    /// Average read rate, MiB/s.
    pub avg_read_rate_mbs: Option<FiniteF64>,
    /// Average write rate, MiB/s.
    pub avg_write_rate_mbs: Option<FiniteF64>,
    /// User CPU, milliseconds.
    pub cpu_user_ms: Option<FiniteF64>,
    /// System CPU, milliseconds.
    pub cpu_system_ms: Option<FiniteF64>,
    /// WAL records.
    pub wal_records: Option<i64>,
    /// WAL full-page images.
    pub wal_fpi: Option<i64>,
    /// WAL bytes.
    pub wal_bytes: Option<i64>,
    /// Number of source fields dropped by bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Canonical grouped slow-query dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowQueryFactPayload {
    /// Bounded normalized query pattern.
    pub pattern: Option<Box<str>>,
    /// Maximum duration, milliseconds.
    pub max_duration_ms: FiniteF64,
    /// Total retained duration, milliseconds.
    pub total_duration_ms: FiniteF64,
    /// Number of source fields dropped by bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Canonical lock-wait dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockWaitFactPayload {
    /// Waiting backend PID.
    pub pid: Option<i32>,
    /// Bounded lock mode.
    pub lock_mode: Option<Box<str>>,
    /// Bounded lock target.
    pub lock_target: Option<Box<str>>,
    /// Wait duration, milliseconds.
    pub duration_ms: Option<FiniteF64>,
    /// Number of source fields dropped by bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Canonical temporary-file dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TempFileFactPayload {
    /// Reported file size, bytes.
    pub size_bytes: i64,
    /// Number of source fields dropped by bounds.
    pub dropped_field_count: DroppedFieldCount,
}

/// Exact counter delta retained as an event-shaped fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterDeltaFactPayload {
    /// Stable factor ID.
    pub factor_id: super::health::FactorId,
    /// Non-negative counter delta.
    pub delta: u64,
    /// Actual adjacent-sample duration.
    pub duration_us: u64,
    /// Reset epoch shared by the valid pair.
    pub reset_epoch: u64,
}

/// Exact entity-state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateTransitionFactPayload {
    /// Stable factor ID.
    pub factor_id: super::health::FactorId,
    /// Previous state discriminant.
    pub previous_state: u32,
    /// Current state discriminant.
    pub current_state: u32,
    /// Proven complete source population.
    pub population_total: u64,
}

/// Proven filesystem-capacity sample or transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityFactPayload {
    /// Total bytes.
    pub total_bytes: u64,
    /// Available bytes for unprivileged writes.
    pub available_bytes: u64,
}

/// Versioned typed payload of a canonical fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPayload {
    /// Grouped log error.
    Error(Box<ErrorFactPayload>),
    /// Lifecycle event.
    Lifecycle(Box<LifecycleFactPayload>),
    /// Checkpoint event.
    Checkpoint(Box<CheckpointFactPayload>),
    /// Autovacuum or autoanalyze event.
    Maintenance(Box<MaintenanceFactPayload>),
    /// Grouped slow-query event.
    SlowQuery(Box<SlowQueryFactPayload>),
    /// Lock-wait event.
    LockWait(Box<LockWaitFactPayload>),
    /// Temporary-file event.
    TempFile(TempFileFactPayload),
    /// Counter delta.
    CounterDelta(CounterDeltaFactPayload),
    /// Entity-state transition.
    StateTransition(StateTransitionFactPayload),
    /// Filesystem capacity.
    Capacity(CapacityFactPayload),
    /// Event kind has no additional dimensions.
    Marker,
}

/// Why a canonical fact could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidEventFact {
    /// Point timestamp cannot form a non-empty half-open interval.
    TimestampOverflow,
    /// A fact has no supporting evidence.
    MissingSupportingEvidence,
    /// Supporting observation IDs are not sorted and unique.
    NonCanonicalSupportingEvidence,
    /// The occurrence count is zero.
    ZeroCount,
    /// An individual fact has a count other than one.
    IndividualCountNotOne,
}

/// One validated, policy-neutral canonical event fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFact {
    fact_id: FactId,
    kind: EventKind,
    shape: FactShape,
    interval: CoverageSpan,
    count: u64,
    entity: Option<EntityRef>,
    payload: EventPayload,
    supporting_observation_ids: Vec<ObservationId>,
    evidence_quality: EvidenceQuality,
    coverage: CoverageRef,
}

impl EventFact {
    /// Constructs a canonical fact and validates evidence/cardinality bounds.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEventFact`] for empty evidence, non-canonical evidence
    /// order, or contradictory count/shape.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor mirrors the stable canonical record"
    )]
    pub fn new(
        fact_id: FactId,
        kind: EventKind,
        shape: FactShape,
        interval: CoverageSpan,
        count: u64,
        entity: Option<EntityRef>,
        payload: EventPayload,
        supporting_observation_ids: Vec<ObservationId>,
        evidence_quality: EvidenceQuality,
        coverage: CoverageRef,
    ) -> Result<Self, InvalidEventFact> {
        if count == 0 {
            return Err(InvalidEventFact::ZeroCount);
        }
        if shape == FactShape::Individual && count != 1 {
            return Err(InvalidEventFact::IndividualCountNotOne);
        }
        if supporting_observation_ids.is_empty() {
            return Err(InvalidEventFact::MissingSupportingEvidence);
        }
        if supporting_observation_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(InvalidEventFact::NonCanonicalSupportingEvidence);
        }
        Ok(Self {
            fact_id,
            kind,
            shape,
            interval,
            count,
            entity,
            payload,
            supporting_observation_ids,
            evidence_quality,
            coverage,
        })
    }

    /// Projects one supported retained observation into a canonical fact.
    ///
    /// A `collector.pg_log_gap` observation remains coverage evidence and does
    /// not create a duplicate event fact.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEventFact::TimestampOverflow`] only for an individual
    /// point at `i64::MAX`.
    pub fn from_observation(
        observation: &EventObservation,
    ) -> Result<Option<Self>, InvalidEventFact> {
        let Some((kind, payload)) = project_payload(observation.payload()) else {
            return Ok(None);
        };
        let shape = match observation.shape() {
            ObservationShape::Individual => FactShape::Individual,
            ObservationShape::GroupedCount => FactShape::GroupedCount,
            ObservationShape::Gap => FactShape::Interval,
        };
        let time = observation.time();
        let end = time
            .sort_ts_us
            .checked_add(1)
            .ok_or(InvalidEventFact::TimestampOverflow)?;
        let interval = CoverageSpan::new(time.sort_ts_us, end)
            .ok_or(InvalidEventFact::TimestampOverflow)?;
        let observation_id = observation.observation_id();
        let fact_id = FactId(sha256::digest_parts(&[
            EVENT_FACT_DOMAIN_TAG,
            &observation_id.0,
        ]));
        let retained_exactness = if observation.loss().is_some() {
            RetainedExactness::LowerBound
        } else {
            RetainedExactness::Exact
        };
        Self::new(
            fact_id,
            kind,
            shape,
            interval,
            observation.occurrence_count(),
            None,
            payload,
            vec![observation_id],
            observation.evidence_quality(),
            CoverageRef {
                source_scope_id: observation.source_scope_id(),
                retained_exactness,
                loss: observation.loss().cloned(),
            },
        )
        .map(Some)
    }

    /// Derives one exact event fact from a valid adjacent cumulative pair.
    ///
    /// The later sample owns the event. Reset/decrease/gap/misalignment and a
    /// measured zero delta produce no event rather than a fabricated zero.
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] only when the current timestamp cannot form
    /// a point interval or the supplied evidence violates the fact contract.
    pub fn from_counter_pair(
        descriptor: MetricSeriesDescriptor,
        previous: CounterSample,
        current: CounterSample,
        known_gaps: &super::coverage::Coverage,
    ) -> Result<Option<Self>, InvalidEventFact> {
        if descriptor.series_id != previous.series_id()
            || descriptor.series_id != current.series_id()
            || descriptor.unit != MetricUnit::Count
            || descriptor.reset_family.is_none()
        {
            return Ok(None);
        }
        let interval = CounterInterval::classify(Some(previous), current, known_gaps);
        if interval.quality() != PairQuality::Valid {
            return Ok(None);
        }
        let Some(delta) = interval.delta().filter(|delta| *delta != 0) else {
            return Ok(None);
        };
        let Some(duration_us) = interval.duration_us() else {
            return Ok(None);
        };
        let Some(kind) = counter_event_kind(descriptor.factor_id) else {
            return Ok(None);
        };
        let evidence = canonical_evidence([
            counter_sample_observation_id(previous),
            counter_sample_observation_id(current),
        ]);
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            if delta == 1 {
                FactShape::Individual
            } else {
                FactShape::GroupedCount
            },
            point_interval(current.ts_us())?,
            delta,
            descriptor.entity,
            EventPayload::CounterDelta(CounterDeltaFactPayload {
                factor_id: descriptor.factor_id,
                delta,
                duration_us,
                reset_epoch: current.reset_epoch(),
            }),
            evidence,
            EvidenceQuality::DerivedExact,
            CoverageRef {
                source_scope_id: descriptor.source_scope_id,
                retained_exactness: RetainedExactness::Exact,
                loss: None,
            },
        )
        .map(Some)
    }

    /// Derives a reset or postmaster marker from two explicit metadata samples.
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] when the current timestamp overflows.
    pub fn from_metadata_change(
        descriptor: MetricSeriesDescriptor,
        previous: GaugeSample,
        current: GaugeSample,
    ) -> Result<Option<Self>, InvalidEventFact> {
        if descriptor.series_id != previous.series_id()
            || descriptor.series_id != current.series_id()
            || descriptor.unit != MetricUnit::Microseconds
            || current.ts_us() <= previous.ts_us()
            || current.value() == previous.value()
            || previous.value().fract() != 0.0
            || current.value().fract() != 0.0
        {
            return Ok(None);
        }
        let kind = match MetricFactor::from_id(descriptor.factor_id) {
            Some(MetricFactor::PgStatisticsResetAt) => EventKind::PgStatisticsResetObserved,
            Some(MetricFactor::PgPostmasterStartTime) => EventKind::PgPostmasterStartChanged,
            _ => return Ok(None),
        };
        let evidence = canonical_evidence([
            gauge_sample_observation_id(previous),
            gauge_sample_observation_id(current),
        ]);
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            FactShape::Individual,
            point_interval(current.ts_us())?,
            1,
            descriptor.entity,
            EventPayload::Marker,
            evidence,
            EvidenceQuality::DerivedExact,
            CoverageRef {
                source_scope_id: descriptor.source_scope_id,
                retained_exactness: RetainedExactness::Exact,
                loss: None,
            },
        )
        .map(Some)
    }

    /// Derives a sender disappearance from adjacent complete snapshot sets.
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] when the current boundary overflows.
    pub fn from_sender_disappearance(
        sender: MetricSeriesDescriptor,
        previous_ts_us: i64,
        previous_state: u32,
        boundary: MetricSeriesDescriptor,
        current_boundary: GaugeSample,
        current_population_total: u64,
    ) -> Result<Option<Self>, InvalidEventFact> {
        if MetricFactor::from_id(sender.factor_id)
            != Some(MetricFactor::PgReplicationSenderState)
            || MetricFactor::from_id(boundary.factor_id)
                != Some(MetricFactor::PgReplicationSenderSnapshotPopulation)
            || sender.source_scope_id != boundary.source_scope_id
            || sender.source_type_id != boundary.source_type_id
            || sender.entity.is_none()
            || current_boundary.series_id() != boundary.series_id
            || current_boundary.ts_us() <= previous_ts_us
        {
            return Ok(None);
        }
        let previous = GaugeSample::new(sender.series_id, previous_ts_us, f64::from(previous_state))
            .expect("u32 state is finite");
        let evidence = canonical_evidence([
            gauge_sample_observation_id(previous),
            gauge_sample_observation_id(current_boundary),
        ]);
        let kind = EventKind::PgReplicationSenderDisappeared;
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            FactShape::Individual,
            point_interval(current_boundary.ts_us())?,
            1,
            sender.entity,
            EventPayload::StateTransition(StateTransitionFactPayload {
                factor_id: sender.factor_id,
                previous_state,
                current_state: u32::MAX,
                population_total: current_population_total,
            }),
            evidence,
            EvidenceQuality::DerivedExact,
            CoverageRef {
                source_scope_id: sender.source_scope_id,
                retained_exactness: RetainedExactness::Exact,
                loss: None,
            },
        )
        .map(Some)
    }

    /// Materializes one retained collector-loss marker.
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] when `ts_us` overflows.
    pub fn from_collector_loss(
        source_scope_id: SourceScopeId,
        source_type_id: u32,
        ts_us: i64,
        kind: EventKind,
        reasons: &[LossReason],
        lost_count_lower_bound: Option<u64>,
    ) -> Result<Option<Self>, InvalidEventFact> {
        if !matches!(
            kind,
            EventKind::CollectorSnapshotGap
                | EventKind::CollectorSourceReadFailure
                | EventKind::CollectorVisibilityRestricted
        ) || reasons.is_empty()
        {
            return Ok(None);
        }
        let loss = LossSummary::new(reasons.iter().copied(), lost_count_lower_bound);
        let mut evidence_input = Vec::with_capacity(16 + 4 + 8 + 2 + loss.reasons().len());
        evidence_input.extend_from_slice(&source_scope_id.0);
        evidence_input.extend_from_slice(&source_type_id.to_le_bytes());
        evidence_input.extend_from_slice(&ts_us.to_le_bytes());
        evidence_input.extend_from_slice(&kind.code().to_le_bytes());
        evidence_input.extend(loss.reasons().iter().map(|reason| reason.code()));
        let evidence = vec![ObservationId(sha256::digest_parts(&[
            COLLECTOR_LOSS_OBSERVATION_TAG,
            &evidence_input,
        ]))];
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            FactShape::Individual,
            point_interval(ts_us)?,
            1,
            None,
            EventPayload::Marker,
            evidence,
            EvidenceQuality::Structured,
            CoverageRef {
                source_scope_id,
                retained_exactness: RetainedExactness::LowerBound,
                loss: Some(loss),
            },
        )
        .map(Some)
    }

    /// Derives an exact state change between complete compatible snapshots.
    ///
    /// Callers supply state records retained only from complete populations.
    /// Equal states and non-advancing timestamps create no fact.
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] when the current timestamp overflows.
    #[allow(
        clippy::too_many_arguments,
        reason = "both complete snapshot records are explicit evidence inputs"
    )]
    pub fn from_state_transition(
        descriptor: MetricSeriesDescriptor,
        previous_ts_us: i64,
        previous_state: u32,
        current_ts_us: i64,
        current_state: u32,
        population_total: u64,
    ) -> Result<Option<Self>, InvalidEventFact> {
        if descriptor.unit != MetricUnit::StateCode
            || descriptor.entity.is_none()
            || current_ts_us <= previous_ts_us
            || current_state == previous_state
        {
            return Ok(None);
        }
        let Some(kind) = state_event_kind(descriptor.factor_id, current_state) else {
            return Ok(None);
        };
        let previous = GaugeSample::new(
            descriptor.series_id,
            previous_ts_us,
            f64::from(previous_state),
        )
        .expect("u32 state is finite");
        let current =
            GaugeSample::new(descriptor.series_id, current_ts_us, f64::from(current_state))
                .expect("u32 state is finite");
        let evidence = canonical_evidence([
            gauge_sample_observation_id(previous),
            gauge_sample_observation_id(current),
        ]);
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            FactShape::Individual,
            point_interval(current_ts_us)?,
            1,
            descriptor.entity,
            EventPayload::StateTransition(StateTransitionFactPayload {
                factor_id: descriptor.factor_id,
                previous_state,
                current_state,
                population_total,
            }),
            evidence,
            EvidenceQuality::DerivedExact,
            CoverageRef {
                source_scope_id: descriptor.source_scope_id,
                retained_exactness: RetainedExactness::Exact,
                loss: None,
            },
        )
        .map(Some)
    }

    /// Materializes one proven filesystem capacity observation.
    ///
    /// Total and available samples must describe the same source/entity and
    /// timestamp. Low-space thresholds remain a policy concern.
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] when the sample timestamp overflows.
    pub fn from_capacity_samples(
        total_descriptor: MetricSeriesDescriptor,
        total: GaugeSample,
        available_descriptor: MetricSeriesDescriptor,
        available: GaugeSample,
    ) -> Result<Option<Self>, InvalidEventFact> {
        if total_descriptor.factor_id != MetricFactor::PgFilesystemTotalBytes.id()
            || available_descriptor.factor_id
                != MetricFactor::PgFilesystemAvailableBytes.id()
            || total_descriptor.source_scope_id != available_descriptor.source_scope_id
            || total_descriptor.entity != available_descriptor.entity
            || total_descriptor.entity.is_none()
            || total.ts_us() != available.ts_us()
            || total.value() < 0.0
            || available.value() < 0.0
            || total.value().fract() != 0.0
            || available.value().fract() != 0.0
            || total.value() > u64::MAX as f64
            || available.value() > u64::MAX as f64
        {
            return Ok(None);
        }
        let total_bytes = total.value() as u64;
        let available_bytes = available.value() as u64;
        if available_bytes > total_bytes {
            return Ok(None);
        }
        let evidence = canonical_evidence([
            gauge_sample_observation_id(total),
            gauge_sample_observation_id(available),
        ]);
        let kind = EventKind::OsFilesystemCapacityObservation;
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            FactShape::Individual,
            point_interval(total.ts_us())?,
            1,
            total_descriptor.entity,
            EventPayload::Capacity(CapacityFactPayload {
                total_bytes,
                available_bytes,
            }),
            evidence,
            EvidenceQuality::Structured,
            CoverageRef {
                source_scope_id: total_descriptor.source_scope_id,
                retained_exactness: RetainedExactness::Exact,
                loss: None,
            },
        )
        .map(Some)
    }

    /// Derives a proven transition from positive to zero available bytes.
    ///
    /// The previous and current available samples must be the same series and
    /// the total/current pair must pass [`Self::from_capacity_samples`].
    ///
    /// # Errors
    /// Returns [`InvalidEventFact`] when the current timestamp overflows.
    pub fn from_capacity_zero_transition(
        total_descriptor: MetricSeriesDescriptor,
        total: GaugeSample,
        available_descriptor: MetricSeriesDescriptor,
        previous_available: GaugeSample,
        current_available: GaugeSample,
        known_gaps: &super::coverage::Coverage,
    ) -> Result<Option<Self>, InvalidEventFact> {
        let Some(capacity) = Self::from_capacity_samples(
            total_descriptor,
            total,
            available_descriptor,
            current_available,
        )?
        else {
            return Ok(None);
        };
        if previous_available.series_id() != current_available.series_id()
            || previous_available.ts_us() >= current_available.ts_us()
            || previous_available.value() <= 0.0
            || current_available.value() != 0.0
            || known_gaps.spans().iter().any(|gap| {
                gap.start_us() < current_available.ts_us()
                    && gap.end_us() > previous_available.ts_us()
            })
        {
            return Ok(None);
        }
        let evidence = canonical_evidence([
            gauge_sample_observation_id(previous_available),
            gauge_sample_observation_id(total),
            gauge_sample_observation_id(current_available),
        ]);
        let kind = EventKind::OsFilesystemCapacityZeroTransition;
        Self::new(
            metric_fact_id(kind, &evidence),
            kind,
            FactShape::Individual,
            capacity.interval(),
            1,
            capacity.entity(),
            capacity.payload().clone(),
            evidence,
            EvidenceQuality::DerivedExact,
            capacity.coverage().clone(),
        )
        .map(Some)
    }

    /// Stable fact identity.
    #[must_use]
    pub const fn fact_id(&self) -> FactId {
        self.fact_id
    }

    /// Stable taxonomy kind.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        self.kind
    }

    /// Fact shape.
    #[must_use]
    pub const fn shape(&self) -> FactShape {
        self.shape
    }

    /// Half-open fact interval.
    #[must_use]
    pub const fn interval(&self) -> CoverageSpan {
        self.interval
    }

    /// Retained occurrence count.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Stable entity, when the source proves one.
    #[must_use]
    pub const fn entity(&self) -> Option<EntityRef> {
        self.entity
    }

    /// Typed canonical payload.
    #[must_use]
    pub const fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// Sorted unique supporting observation IDs.
    #[must_use]
    pub fn supporting_observation_ids(&self) -> &[ObservationId] {
        &self.supporting_observation_ids
    }

    /// Evidence quality.
    #[must_use]
    pub const fn evidence_quality(&self) -> EvidenceQuality {
        self.evidence_quality
    }

    /// Coverage provenance.
    #[must_use]
    pub const fn coverage(&self) -> &CoverageRef {
        &self.coverage
    }

    /// Canonical order by interval start and fact identity.
    #[must_use]
    pub fn canonical_cmp(&self, other: &Self) -> Ordering {
        self.interval
            .start_us()
            .cmp(&other.interval.start_us())
            .then_with(|| self.fact_id.cmp(&other.fact_id))
    }

    /// Checked heap bytes retained below this fact's vector slot.
    #[must_use]
    pub fn resident_heap_bytes(&self) -> Option<usize> {
        let evidence = self
            .supporting_observation_ids
            .capacity()
            .checked_mul(size_of::<ObservationId>())?;
        let payload = payload_heap_bytes(&self.payload)?;
        let loss = self.coverage.loss.as_ref().map_or(Some(0), |loss| {
            loss.reasons()
                .len()
                .checked_mul(size_of::<LossReason>())
        })?;
        evidence.checked_add(payload)?.checked_add(loss)
    }
}

/// Stable evidence identity of one canonical cumulative sample.
#[must_use]
pub fn counter_sample_observation_id(sample: CounterSample) -> ObservationId {
    ObservationId(sha256::digest_parts(&[
        COUNTER_SAMPLE_OBSERVATION_TAG,
        &sample.series_id().0,
        &sample.alignment_id().0,
        &sample.ts_us().to_le_bytes(),
        &sample.value().to_le_bytes(),
        &sample.reset_epoch().to_le_bytes(),
    ]))
}

/// Stable evidence identity of one canonical instantaneous sample.
#[must_use]
pub fn gauge_sample_observation_id(sample: GaugeSample) -> ObservationId {
    ObservationId(sha256::digest_parts(&[
        GAUGE_SAMPLE_OBSERVATION_TAG,
        &sample.series_id().0,
        &sample.ts_us().to_le_bytes(),
        &sample.value().to_bits().to_le_bytes(),
    ]))
}

fn point_interval(ts_us: i64) -> Result<CoverageSpan, InvalidEventFact> {
    CoverageSpan::new(
        ts_us,
        ts_us
            .checked_add(1)
            .ok_or(InvalidEventFact::TimestampOverflow)?,
    )
    .ok_or(InvalidEventFact::TimestampOverflow)
}

fn canonical_evidence<const N: usize>(ids: [ObservationId; N]) -> Vec<ObservationId> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn metric_fact_id(kind: EventKind, evidence: &[ObservationId]) -> FactId {
    let mut input = Vec::with_capacity(2 + evidence.len() * 32);
    input.extend_from_slice(&kind.code().to_le_bytes());
    for id in evidence {
        input.extend_from_slice(&id.0);
    }
    FactId(sha256::digest_parts(&[METRIC_FACT_DOMAIN_TAG, &input]))
}

const fn counter_event_kind(factor_id: super::health::FactorId) -> Option<EventKind> {
    match MetricFactor::from_id(factor_id) {
        Some(MetricFactor::PgDatabaseDeadlocks) => Some(EventKind::PgDatabaseDeadlockDelta),
        Some(MetricFactor::PgDatabaseRecoveryConflicts) => {
            Some(EventKind::PgDatabaseRecoveryConflictDelta)
        }
        Some(MetricFactor::PgDatabaseChecksumFailures) => {
            Some(EventKind::PgDatabaseChecksumFailureDelta)
        }
        Some(MetricFactor::PgDatabaseSessionsAbandoned) => {
            Some(EventKind::PgDatabaseSessionsAbandonedDelta)
        }
        Some(MetricFactor::PgDatabaseSessionsFatal) => {
            Some(EventKind::PgDatabaseSessionsFatalDelta)
        }
        Some(MetricFactor::PgDatabaseSessionsKilled) => {
            Some(EventKind::PgDatabaseSessionsKilledDelta)
        }
        Some(MetricFactor::OsCgroupMemoryHighEvents) => {
            Some(EventKind::OsCgroupMemoryHighDelta)
        }
        Some(MetricFactor::OsCgroupMemoryMaxEvents) => {
            Some(EventKind::OsCgroupMemoryMaxDelta)
        }
        Some(MetricFactor::OsCgroupOomEvents) => Some(EventKind::OsCgroupOomDelta),
        Some(MetricFactor::OsCgroupOomKills) => Some(EventKind::OsCgroupOomKillDelta),
        Some(MetricFactor::OsHostOomKills) => Some(EventKind::OsHostOomKillDelta),
        _ => None,
    }
}

const fn state_event_kind(
    factor_id: super::health::FactorId,
    current_state: u32,
) -> Option<EventKind> {
    match MetricFactor::from_id(factor_id) {
        Some(MetricFactor::PgRecoveryRole) => Some(EventKind::PgRecoveryRoleChanged),
        Some(MetricFactor::PgTimeline) => Some(EventKind::PgTimelineChanged),
        Some(MetricFactor::PgReplicationSenderState) => {
            Some(EventKind::PgReplicationSenderStateChanged)
        }
        Some(MetricFactor::PgReplicationSlotState) if current_state == 4 => {
            Some(EventKind::PgReplicationSlotLost)
        }
        Some(MetricFactor::PgReplicationSlotState) => {
            Some(EventKind::PgReplicationSlotStateChanged)
        }
        _ => None,
    }
}

fn project_payload(payload: &ObservationPayload) -> Option<(EventKind, EventPayload)> {
    let projected = match payload {
        ObservationPayload::ErrorGroup(value) => (
            EventKind::PgLogErrorGroupObserved,
            EventPayload::Error(Box::new(ErrorFactPayload {
                severity: value.severity,
                category: value.category,
                sqlstate: value.sqlstate,
                normalized_pattern: value.normalized_pattern.clone(),
                database: value.database.clone(),
                user: value.user.clone(),
                dropped_field_count: value.dropped_field_count,
            })),
        ),
        ObservationPayload::ChildSignalTermination(value) => (
            EventKind::PgLifecycleChildSignalTermination,
            lifecycle_payload(value),
        ),
        ObservationPayload::ChildProcessCrash(value) => (
            EventKind::PgLifecycleChildProcessCrash,
            lifecycle_payload(value),
        ),
        ObservationPayload::ShutdownRequested(value) => (
            EventKind::PgLifecycleShutdownRequested,
            lifecycle_payload(value),
        ),
        ObservationPayload::ReadyObserved(value) => (
            EventKind::PgLifecycleReadyObserved,
            lifecycle_payload(value),
        ),
        ObservationPayload::CheckpointStarted(value) => (
            EventKind::PgCheckpointStarted,
            checkpoint_payload(value),
        ),
        ObservationPayload::CheckpointCompleted(value) => (
            EventKind::PgCheckpointCompleted,
            checkpoint_payload(value),
        ),
        ObservationPayload::CheckpointTooFrequent(value) => (
            EventKind::PgCheckpointTooFrequentReported,
            checkpoint_payload(value),
        ),
        ObservationPayload::AutovacuumReported(value) => (
            EventKind::PgMaintenanceAutovacuumReported,
            maintenance_payload(value),
        ),
        ObservationPayload::AutoanalyzeReported(value) => (
            EventKind::PgMaintenanceAutoanalyzeReported,
            maintenance_payload(value),
        ),
        ObservationPayload::SlowQueryGroup(value) => (
            EventKind::PgQuerySlowGroupReported,
            EventPayload::SlowQuery(Box::new(SlowQueryFactPayload {
                pattern: value.pattern.clone(),
                max_duration_ms: value.max_duration_ms,
                total_duration_ms: value.total_duration_ms,
                dropped_field_count: value.dropped_field_count,
            })),
        ),
        ObservationPayload::LockWaitReported(value) => (
            EventKind::PgLockWaitReported,
            lock_wait_payload(value),
        ),
        ObservationPayload::LockAcquiredAfterWait(value) => (
            EventKind::PgLockAcquiredAfterWaitReported,
            lock_wait_payload(value),
        ),
        ObservationPayload::TempFileReported(value) => (
            EventKind::PgTempFileReported,
            EventPayload::TempFile(TempFileFactPayload {
                size_bytes: value.size_bytes,
                dropped_field_count: value.dropped_field_count,
            }),
        ),
        ObservationPayload::LogGap(_) => return None,
    };
    Some(projected)
}

fn lifecycle_payload(value: &super::observation::LifecyclePayload) -> EventPayload {
    EventPayload::Lifecycle(Box::new(LifecycleFactPayload {
        pid: value.pid,
        signal: value.signal,
        shutdown_mode: value.shutdown_mode.clone(),
        dropped_field_count: value.dropped_field_count,
    }))
}

fn checkpoint_payload(value: &super::observation::CheckpointPayload) -> EventPayload {
    EventPayload::Checkpoint(Box::new(CheckpointFactPayload {
        reason: value.reason.clone(),
        seconds_apart: value.seconds_apart,
        buffers_written: value.buffers_written,
        write_ms: value.write_ms,
        sync_ms: value.sync_ms,
        total_ms: value.total_ms,
        distance_kb: value.distance_kb,
        estimate_kb: value.estimate_kb,
        wal_added: value.wal_added,
        wal_removed: value.wal_removed,
        wal_recycled: value.wal_recycled,
        sync_files: value.sync_files,
        longest_sync_ms: value.longest_sync_ms,
        average_sync_ms: value.average_sync_ms,
        dropped_field_count: value.dropped_field_count,
    }))
}

fn maintenance_payload(value: &super::observation::MaintenancePayload) -> EventPayload {
    EventPayload::Maintenance(Box::new(MaintenanceFactPayload {
        relation: value.relation.clone(),
        index_scans: value.index_scans,
        pages_removed: value.pages_removed,
        pages_remaining: value.pages_remaining,
        tuples_removed: value.tuples_removed,
        tuples_remaining: value.tuples_remaining,
        tuples_dead_not_removable: value.tuples_dead_not_removable,
        elapsed_ms: value.elapsed_ms,
        buffer_hits: value.buffer_hits,
        buffer_misses: value.buffer_misses,
        buffer_dirtied: value.buffer_dirtied,
        avg_read_rate_mbs: value.avg_read_rate_mbs,
        avg_write_rate_mbs: value.avg_write_rate_mbs,
        cpu_user_ms: value.cpu_user_ms,
        cpu_system_ms: value.cpu_system_ms,
        wal_records: value.wal_records,
        wal_fpi: value.wal_fpi,
        wal_bytes: value.wal_bytes,
        dropped_field_count: value.dropped_field_count,
    }))
}

fn lock_wait_payload(value: &super::observation::LockWaitPayload) -> EventPayload {
    EventPayload::LockWait(Box::new(LockWaitFactPayload {
        pid: value.pid,
        lock_mode: value.lock_mode.clone(),
        lock_target: value.lock_target.clone(),
        duration_ms: value.duration_ms,
        dropped_field_count: value.dropped_field_count,
    }))
}

fn payload_heap_bytes(payload: &EventPayload) -> Option<usize> {
    match payload {
        EventPayload::Error(value) => size_of::<ErrorFactPayload>()
            .checked_add(text_bytes(&[
                &value.normalized_pattern,
                &value.database,
                &value.user,
            ])),
        EventPayload::Lifecycle(value) => size_of::<LifecycleFactPayload>()
            .checked_add(text_bytes(&[&value.shutdown_mode])),
        EventPayload::Checkpoint(value) => {
            size_of::<CheckpointFactPayload>().checked_add(text_bytes(&[&value.reason]))
        }
        EventPayload::Maintenance(value) => {
            size_of::<MaintenanceFactPayload>().checked_add(text_bytes(&[&value.relation]))
        }
        EventPayload::SlowQuery(value) => {
            size_of::<SlowQueryFactPayload>().checked_add(text_bytes(&[&value.pattern]))
        }
        EventPayload::LockWait(value) => size_of::<LockWaitFactPayload>()
            .checked_add(text_bytes(&[&value.lock_mode, &value.lock_target])),
        EventPayload::TempFile(_)
        | EventPayload::CounterDelta(_)
        | EventPayload::StateTransition(_)
        | EventPayload::Capacity(_)
        | EventPayload::Marker => Some(0),
    }
}

fn text_bytes(values: &[&Option<Box<str>>]) -> usize {
    values
        .iter()
        .fold(0_usize, |total, value| total.saturating_add(value.as_deref().map_or(0, str::len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::{
        DictionaryContextId, LifecyclePayload, NamingContractId, ObservationProvenance,
        ObservationTime, QualityFlags, SectionBodyId, SegmentIdentity, SegmentLocator,
        TimeQuality,
    };

    fn observation(payload: ObservationPayload, shape: ObservationShape, count: u64) -> EventObservation {
        let locator = SegmentLocator([3; 32]);
        EventObservation::new(
            SegmentIdentity::sealed(
                SourceScopeId([1; 32]),
                NamingContractId([2; 16]),
                locator,
                7,
                b"entry",
            ),
            7,
            ObservationProvenance {
                segment_locator: Some(locator),
                section_body_id: SectionBodyId([4; 32]),
                catalog_entry_ordinal: 0,
                row_ordinal: 0,
                dictionary_context_id: DictionaryContextId([5; 32]),
                source_locator: None,
            },
            shape,
            ObservationTime {
                sort_ts_us: 10,
                occurred_at_us: Some(10),
                observed_interval: None,
                quality: if shape == ObservationShape::GroupedCount {
                    TimeQuality::FirstInGroup
                } else {
                    TimeQuality::Exact
                },
            },
            count,
            payload,
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
        .expect("valid observation")
    }

    #[test]
    fn lifecycle_projection_omits_source_shaped_message_and_query() {
        let source = observation(
            ObservationPayload::ReadyObserved(Box::new(LifecyclePayload {
                pid: Some(42),
                signal: None,
                shutdown_mode: Some("smart".into()),
                message: Some("server is ready".into()),
                query_detail: Some("select secret".into()),
                dropped_field_count: DroppedFieldCount(2),
            })),
            ObservationShape::Individual,
            1,
        );
        let fact = EventFact::from_observation(&source)
            .expect("projection")
            .expect("fact");
        assert_eq!(fact.kind(), EventKind::PgLifecycleReadyObserved);
        let EventPayload::Lifecycle(payload) = fact.payload() else {
            panic!("lifecycle payload");
        };
        assert_eq!(payload.pid, Some(42));
        assert_eq!(payload.shutdown_mode.as_deref(), Some("smart"));
        assert_eq!(fact.supporting_observation_ids(), &[source.observation_id()]);
    }

    #[test]
    fn grouped_count_and_loss_survive_projection() {
        let source = observation(
            ObservationPayload::ErrorGroup(Box::new(ErrorFactPayload {
                severity: Severity::Fatal,
                category: ErrorCategory::Connection,
                sqlstate: Some(SqlState(*b"57P01")),
                normalized_pattern: Some("terminating connection".into()),
                database: None,
                user: None,
                dropped_field_count: DroppedFieldCount(0),
            }.into_observation_payload())),
            ObservationShape::GroupedCount,
            7,
        );
        let fact = EventFact::from_observation(&source)
            .expect("projection")
            .expect("fact");
        assert_eq!(fact.shape(), FactShape::GroupedCount);
        assert_eq!(fact.count(), 7);
    }

    #[test]
    fn log_gap_stays_coverage_only() {
        assert_eq!(
            EventKind::from_code(EventKind::OsCgroupOomKillDelta.code()),
            Some(EventKind::OsCgroupOomKillDelta)
        );
    }

    impl ErrorFactPayload {
        fn into_observation_payload(self) -> super::super::observation::ErrorGroupPayload {
            super::super::observation::ErrorGroupPayload {
                severity: self.severity,
                category: self.category,
                sqlstate: self.sqlstate,
                normalized_pattern: self.normalized_pattern,
                sample: None,
                detail: None,
                hint: None,
                context: None,
                statement: None,
                database: self.database,
                user: self.user,
                dropped_field_count: self.dropped_field_count,
            }
        }
    }
}
