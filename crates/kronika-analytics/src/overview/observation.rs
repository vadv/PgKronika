//! Retained event observations and their provenance-derived identities.
//!
//! Observations describe event-shaped source rows. Counter deltas and state
//! transitions are derived facts and do not use this type. A signal remains a
//! signal; causal conclusions belong to a later diagnosis layer.

use std::cmp::Ordering;
use std::fmt;

use super::counts::{ErrorCategory, Severity, SqlState};
use super::coverage::CoverageSpan;
use super::finite::FiniteF64;
use super::sha256;

const LINEAGE_DOMAIN_TAG: &[u8] = b"pgk-overview-lineage-v1";
const LIVE_LINEAGE_DOMAIN_TAG: &[u8] = b"pgk-overview-live-view-v1";
const OBSERVATION_DOMAIN_TAG: &[u8] = b"pgk-overview-observation-v1";

/// Identity of the source scope a segment belongs to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceScopeId(pub [u8; 32]);

/// Versioned identity of the reader's canonical segment-naming contract.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamingContractId(pub [u8; 16]);

/// Canonical identity of an existing sealed segment.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentLocator(pub [u8; 32]);

/// Stable identity of one proven segment lineage.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentLineageId(pub [u8; 32]);

/// Digest of one exact section body together with its type and length.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionBodyId(pub [u8; 32]);

/// Digest of the canonical resolved dictionary entries used by a row.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DictionaryContextId(pub [u8; 32]);

/// Identity of one retained observation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObservationId(pub [u8; 32]);

/// Identity of one derived fact that cites observations as evidence.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactId(pub [u8; 32]);

macro_rules! impl_id_debug {
    ($type_name:ident) => {
        impl fmt::Debug for $type_name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}(", stringify!($type_name))?;
                for byte in &self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                write!(formatter, ")")
            }
        }
    };
}

impl_id_debug!(SourceScopeId);
impl_id_debug!(NamingContractId);
impl_id_debug!(SegmentLocator);
impl_id_debug!(SegmentLineageId);
impl_id_debug!(SectionBodyId);
impl_id_debug!(DictionaryContextId);
impl_id_debug!(ObservationId);
impl_id_debug!(FactId);

/// How strongly an observation identity is tied to the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityQuality {
    /// The source carries a unique occurrence identity.
    SourceExact,
    /// Identity is derived from retained content and proven provenance.
    ContentDerived,
    /// Identity is stable only within the pinned live view.
    Approximate,
}

/// A lineage together with the scope, locator, and quality it proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentIdentity {
    id: SegmentLineageId,
    source_scope_id: SourceScopeId,
    segment_locator: Option<SegmentLocator>,
    quality: IdentityQuality,
}

impl SegmentIdentity {
    /// Derives a rebuild-stable lineage for a proven sealed locator.
    #[must_use]
    pub fn sealed(
        source_scope_id: SourceScopeId,
        naming_contract_id: NamingContractId,
        segment_locator: SegmentLocator,
        first_entry_type: u32,
        first_entry_content_descriptor: &[u8],
    ) -> Self {
        let descriptor_len = u64::try_from(first_entry_content_descriptor.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes();
        let id = SegmentLineageId(sha256::digest_parts(&[
            LINEAGE_DOMAIN_TAG,
            &source_scope_id.0,
            &naming_contract_id.0,
            &segment_locator.0,
            &first_entry_type.to_le_bytes(),
            &descriptor_len,
            first_entry_content_descriptor,
        ]));
        Self {
            id,
            source_scope_id,
            segment_locator: Some(segment_locator),
            quality: IdentityQuality::ContentDerived,
        }
    }

    /// Derives a view-scoped lineage when a future sealed locator is unknown.
    #[must_use]
    pub fn live_approximate(
        source_scope_id: SourceScopeId,
        journal_generation: u64,
        first_part_descriptor: &[u8],
    ) -> Self {
        let descriptor_len = u64::try_from(first_part_descriptor.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes();
        let id = SegmentLineageId(sha256::digest_parts(&[
            LIVE_LINEAGE_DOMAIN_TAG,
            &source_scope_id.0,
            &journal_generation.to_le_bytes(),
            &descriptor_len,
            first_part_descriptor,
        ]));
        Self {
            id,
            source_scope_id,
            segment_locator: None,
            quality: IdentityQuality::Approximate,
        }
    }

    /// The derived lineage ID.
    #[must_use]
    pub const fn id(self) -> SegmentLineageId {
        self.id
    }

    /// The identity quality proven by the constructor.
    #[must_use]
    pub const fn quality(self) -> IdentityQuality {
        self.quality
    }

    /// Source scope carried by this lineage.
    #[must_use]
    pub const fn source_scope_id(self) -> SourceScopeId {
        self.source_scope_id
    }
}

impl ObservationId {
    fn derive(
        lineage: SegmentLineageId,
        source_type_id: u32,
        provenance: &ObservationProvenance,
    ) -> Self {
        Self(sha256::digest_parts(&[
            OBSERVATION_DOMAIN_TAG,
            &lineage.0,
            &source_type_id.to_le_bytes(),
            &provenance.section_body_id.0,
            &provenance.catalog_entry_ordinal.to_le_bytes(),
            &provenance.row_ordinal.to_le_bytes(),
            &provenance.dictionary_context_id.0,
        ]))
    }
}

/// The event shape asserted by one retained row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationShape {
    /// One occurrence at one point in time.
    Individual,
    /// Several occurrences retained as one grouped row.
    GroupedCount,
    /// A declared interval with incomplete collection.
    Gap,
}

/// Quality of an observation timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeQuality {
    /// The source contract proves the occurrence timestamp and offset.
    Exact,
    /// The timestamp is the first occurrence retained for a group.
    FirstInGroup,
    /// The timestamp belongs to a representative group member.
    RepresentativeSample,
    /// The timestamp belongs to the maximum-duration group member.
    MaxDurationSample,
    /// A civil timestamp was parsed without a verified UTC offset.
    ParsedWithoutVerifiedOffset,
    /// Collection time substitutes for a missing source timestamp.
    CollectionFallback,
    /// Only a containing interval is known.
    IntervalOnly,
}

/// How payload fields were obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceQuality {
    /// Read from structured source fields.
    Structured,
    /// Parsed from free text.
    Parsed,
    /// Inferred by a heuristic.
    Heuristic,
    /// Computed exactly from retained values.
    DerivedExact,
}

/// Time attribution of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationTime {
    /// Timestamp used for canonical ordering.
    pub sort_ts_us: i64,
    /// Source occurrence timestamp, when retained.
    pub occurred_at_us: Option<i64>,
    /// Containing interval for an interval-only observation.
    pub observed_interval: Option<CoverageSpan>,
    /// Timestamp quality.
    pub quality: TimeQuality,
}

/// Advisory physical source location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLocator {
    /// Writer-declared physical source unit identity.
    pub source_unit_id: [u8; 32],
    /// Byte offset inside that source unit.
    pub byte_offset: u64,
}

/// Provenance of one retained row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationProvenance {
    /// Sealed segment locator, absent for an approximate live view.
    pub segment_locator: Option<SegmentLocator>,
    /// Digest of the exact section body.
    pub section_body_id: SectionBodyId,
    /// Segment-global catalog ordinal of the section entry.
    pub catalog_entry_ordinal: u32,
    /// Row position inside the section body.
    pub row_ordinal: u32,
    /// Digest of resolved dictionary values used by the row.
    pub dictionary_context_id: DictionaryContextId,
    /// Physical location claim, when provided by the writer.
    pub source_locator: Option<SourceLocator>,
}

/// Extractor-versioned quality bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QualityFlags(pub u32);

/// Why source data around an observation is known to be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LossReason {
    /// The error-group cap dropped groups.
    GroupCapExceeded,
    /// The lifecycle cap dropped records.
    LifecycleCapExceeded,
    /// Parser bounds dropped input.
    ParserBound,
    /// The tailer lost input.
    TailerBound,
    /// Dictionary bounds dropped values.
    DictionaryBound,
}

impl LossReason {
    pub(crate) const ALL: [Self; 5] = [
        Self::GroupCapExceeded,
        Self::LifecycleCapExceeded,
        Self::ParserBound,
        Self::TailerBound,
        Self::DictionaryBound,
    ];

    const fn index(self) -> usize {
        match self {
            Self::GroupCapExceeded => 0,
            Self::LifecycleCapExceeded => 1,
            Self::ParserBound => 2,
            Self::TailerBound => 3,
            Self::DictionaryBound => 4,
        }
    }
}

/// Proven source loss attached to an observation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LossSummary {
    reasons: Vec<LossReason>,
    /// Proven lower bound on lost records, when counted.
    pub lost_count_lower_bound: Option<u64>,
}

impl LossSummary {
    /// Builds a summary with sorted, unique reasons.
    #[must_use]
    pub fn new(
        reasons: impl IntoIterator<Item = LossReason>,
        lost_count_lower_bound: Option<u64>,
    ) -> Self {
        let mut present = [false; LossReason::ALL.len()];
        for reason in reasons {
            present[reason.index()] = true;
        }
        let reasons = LossReason::ALL
            .into_iter()
            .filter(|reason| present[reason.index()])
            .collect();
        Self {
            reasons,
            lost_count_lower_bound,
        }
    }

    /// Sorted, unique loss reasons.
    #[must_use]
    pub fn reasons(&self) -> &[LossReason] {
        &self.reasons
    }
}

/// Number of text fields the source could not intern.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DroppedFieldCount(pub u32);

/// Retained grouped-error fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorGroupPayload {
    /// Line severity.
    pub severity: Severity,
    /// Classifier category.
    pub category: ErrorCategory,
    /// SQLSTATE, when retained.
    pub sqlstate: Option<SqlState>,
    /// Normalized grouping pattern.
    pub normalized_pattern: Option<Box<str>>,
    /// First concrete message sample.
    pub sample: Option<Box<str>>,
    /// `DETAIL` continuation.
    pub detail: Option<Box<str>>,
    /// `HINT` continuation.
    pub hint: Option<Box<str>>,
    /// `CONTEXT` continuation.
    pub context: Option<Box<str>>,
    /// Following `STATEMENT` text.
    pub statement: Option<Box<str>>,
    /// Database name.
    pub database: Option<Box<str>>,
    /// User name.
    pub user: Option<Box<str>>,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained fields shared by checkpoint event kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPayload {
    /// Start reason or warning text.
    pub reason: Option<Box<str>>,
    /// Reported interval between checkpoints.
    pub seconds_apart: Option<i64>,
    /// Buffers written.
    pub buffers_written: Option<i64>,
    /// Write phase duration in milliseconds.
    pub write_ms: Option<FiniteF64>,
    /// Sync phase duration in milliseconds.
    pub sync_ms: Option<FiniteF64>,
    /// Total duration in milliseconds.
    pub total_ms: Option<FiniteF64>,
    /// WAL distance in kB.
    pub distance_kb: Option<i64>,
    /// Estimated WAL distance in kB.
    pub estimate_kb: Option<i64>,
    /// WAL files added.
    pub wal_added: Option<i64>,
    /// WAL files removed.
    pub wal_removed: Option<i64>,
    /// WAL files recycled.
    pub wal_recycled: Option<i64>,
    /// Files synced.
    pub sync_files: Option<i64>,
    /// Longest sync duration in milliseconds.
    pub longest_sync_ms: Option<FiniteF64>,
    /// Average sync duration in milliseconds.
    pub average_sync_ms: Option<FiniteF64>,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained autovacuum or autoanalyze fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenancePayload {
    /// Qualified relation name.
    pub relation: Option<Box<str>>,
    /// Number of index scans.
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
    /// Elapsed duration in milliseconds.
    pub elapsed_ms: Option<FiniteF64>,
    /// Buffer hits.
    pub buffer_hits: Option<i64>,
    /// Buffer misses.
    pub buffer_misses: Option<i64>,
    /// Buffers dirtied.
    pub buffer_dirtied: Option<i64>,
    /// Average read rate in MB/s.
    pub avg_read_rate_mbs: Option<FiniteF64>,
    /// Average write rate in MB/s.
    pub avg_write_rate_mbs: Option<FiniteF64>,
    /// User CPU time in milliseconds.
    pub cpu_user_ms: Option<FiniteF64>,
    /// System CPU time in milliseconds.
    pub cpu_system_ms: Option<FiniteF64>,
    /// WAL records generated.
    pub wal_records: Option<i64>,
    /// WAL full-page images generated.
    pub wal_fpi: Option<i64>,
    /// WAL bytes generated.
    pub wal_bytes: Option<i64>,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained slow-query group fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowQueryPayload {
    /// Normalized SQL pattern.
    pub pattern: Option<Box<str>>,
    /// Representative SQL sample.
    pub sample: Option<Box<str>>,
    /// Maximum duration in milliseconds.
    pub max_duration_ms: FiniteF64,
    /// Total duration in milliseconds.
    pub total_duration_ms: FiniteF64,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained lock-wait fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockWaitPayload {
    /// Waiting backend PID.
    pub pid: Option<i32>,
    /// Lock mode.
    pub lock_mode: Option<Box<str>>,
    /// Lock target.
    pub lock_target: Option<Box<str>>,
    /// Wait duration in milliseconds.
    pub duration_ms: Option<FiniteF64>,
    /// `DETAIL` continuation.
    pub detail: Option<Box<str>>,
    /// `CONTEXT` continuation.
    pub context: Option<Box<str>>,
    /// Following `STATEMENT` text.
    pub statement: Option<Box<str>>,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained lifecycle fields other than the event kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecyclePayload {
    /// Child process ID, when present.
    pub pid: Option<i32>,
    /// Raw termination signal field, when present.
    pub signal: Option<i32>,
    /// Shutdown mode, when present.
    pub shutdown_mode: Option<Box<str>>,
    /// Bounded lifecycle message.
    pub message: Option<Box<str>>,
    /// Query text from crash detail, when present.
    pub query_detail: Option<Box<str>>,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained temporary-file fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempFilePayload {
    /// File path, when retained.
    pub path: Option<Box<str>>,
    /// File size in bytes.
    pub size_bytes: i64,
    /// Following `STATEMENT` text.
    pub statement: Option<Box<str>>,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
}

/// Retained log-gap fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogGapPayload {
    /// Source path, when known.
    pub source_path: Option<Box<str>>,
    /// Parser kind machine value.
    pub parser_kind: u8,
    /// Gap reason machine value.
    pub reason: u8,
    /// File device ID.
    pub dev: Option<u64>,
    /// File inode.
    pub inode: Option<u64>,
    /// Tail offset after the degraded read.
    pub offset: Option<u64>,
    /// Bytes skipped.
    pub bytes_skipped: u64,
    /// Truncated physical lines.
    pub truncated_lines: u32,
    /// Invalid UTF-8 lines.
    pub invalid_utf8: u32,
    /// Binary lines dropped.
    pub binary_dropped: u32,
    /// Rotation detections.
    pub rotations: u32,
    /// Missing-file observations.
    pub missing_files: u32,
    /// Budget exhaustion count.
    pub budget_exhaustions: u32,
    /// Count reported by `dict_dropped_fields`.
    pub dropped_field_count: DroppedFieldCount,
    /// Parser-dropped complete lines.
    pub parser_dropped_lines: u32,
}

/// Typed retained payload of one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationPayload {
    /// Grouped log error.
    ErrorGroup(Box<ErrorGroupPayload>),
    /// Child process terminated by a raw signal.
    ChildSignalTermination(Box<LifecyclePayload>),
    /// Shutdown request.
    ShutdownRequested(Box<LifecyclePayload>),
    /// Server readiness record.
    ReadyObserved(Box<LifecyclePayload>),
    /// Checkpoint start.
    CheckpointStarted(Box<CheckpointPayload>),
    /// Checkpoint completion.
    CheckpointCompleted(Box<CheckpointPayload>),
    /// Too-frequent checkpoint report.
    CheckpointTooFrequent(Box<CheckpointPayload>),
    /// Autovacuum report.
    AutovacuumReported(Box<MaintenancePayload>),
    /// Autoanalyze report.
    AutoanalyzeReported(Box<MaintenancePayload>),
    /// Grouped slow-query report.
    SlowQueryGroup(Box<SlowQueryPayload>),
    /// Lock wait report.
    LockWaitReported(Box<LockWaitPayload>),
    /// Lock acquired after waiting.
    LockAcquiredAfterWait(Box<LockWaitPayload>),
    /// Temporary-file report.
    TempFileReported(Box<TempFilePayload>),
    /// Collector log-gap report.
    LogGap(Box<LogGapPayload>),
}

impl ObservationPayload {
    /// Stable machine code of this observation kind.
    #[must_use]
    pub const fn kind_code(&self) -> &'static str {
        match self {
            Self::ErrorGroup(_) => "pg.log.error_group_observed",
            Self::ChildSignalTermination(_) => "pg.lifecycle.child_signal_termination",
            Self::ShutdownRequested(_) => "pg.lifecycle.shutdown_requested",
            Self::ReadyObserved(_) => "pg.lifecycle.ready_observed",
            Self::CheckpointStarted(_) => "pg.checkpoint.started",
            Self::CheckpointCompleted(_) => "pg.checkpoint.completed",
            Self::CheckpointTooFrequent(_) => "pg.checkpoint.too_frequent_reported",
            Self::AutovacuumReported(_) => "pg.maintenance.autovacuum_reported",
            Self::AutoanalyzeReported(_) => "pg.maintenance.autoanalyze_reported",
            Self::SlowQueryGroup(_) => "pg.query.slow_group_reported",
            Self::LockWaitReported(_) => "pg.lock.wait_reported",
            Self::LockAcquiredAfterWait(_) => "pg.lock.acquired_after_wait_reported",
            Self::TempFileReported(_) => "pg.temp_file.reported",
            Self::LogGap(_) => "collector.pg_log_gap",
        }
    }

    const fn expected_shape(&self) -> ObservationShape {
        match self {
            Self::ErrorGroup(_) | Self::SlowQueryGroup(_) => ObservationShape::GroupedCount,
            Self::LogGap(_) => ObservationShape::Gap,
            _ => ObservationShape::Individual,
        }
    }
}

/// Validation failure while constructing an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidObservation {
    /// Provenance locator does not match the lineage constructor.
    SegmentLocatorMismatch,
    /// Payload kind and declared shape disagree.
    PayloadShapeMismatch,
    /// Occurrence count is zero.
    ZeroOccurrenceCount,
    /// An individual or gap record has a count other than one.
    SingleOccurrenceShapeCountNotOne,
    /// Time fields do not match their quality.
    InvalidTimeAttribution,
    /// The observation shape does not allow the selected time quality.
    TimeQualityShapeMismatch,
    /// A child-signal payload has no raw signal field.
    ChildTerminationWithoutSignal,
}

/// One validated retained observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventObservation {
    observation_id: ObservationId,
    identity_quality: IdentityQuality,
    source_scope_id: SourceScopeId,
    source_type_id: u32,
    provenance: ObservationProvenance,
    shape: ObservationShape,
    time: ObservationTime,
    occurrence_count: u64,
    payload: ObservationPayload,
    evidence_quality: EvidenceQuality,
    quality_flags: QualityFlags,
    loss: Option<LossSummary>,
}

impl EventObservation {
    /// Builds a record and derives its ID from the proven lineage.
    ///
    /// # Errors
    /// Returns [`InvalidObservation`] when provenance, shape, count, or time
    /// fields contradict one another.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor mirrors the stable record"
    )]
    pub fn new(
        lineage: SegmentIdentity,
        source_type_id: u32,
        provenance: ObservationProvenance,
        shape: ObservationShape,
        time: ObservationTime,
        occurrence_count: u64,
        payload: ObservationPayload,
        evidence_quality: EvidenceQuality,
        quality_flags: QualityFlags,
        loss: Option<LossSummary>,
    ) -> Result<Self, InvalidObservation> {
        if provenance.segment_locator != lineage.segment_locator {
            return Err(InvalidObservation::SegmentLocatorMismatch);
        }
        if shape != payload.expected_shape() {
            return Err(InvalidObservation::PayloadShapeMismatch);
        }
        if occurrence_count == 0 {
            return Err(InvalidObservation::ZeroOccurrenceCount);
        }
        if shape != ObservationShape::GroupedCount && occurrence_count != 1 {
            return Err(InvalidObservation::SingleOccurrenceShapeCountNotOne);
        }
        validate_time(shape, time)?;
        if matches!(payload, ObservationPayload::SlowQueryGroup(_))
            && time.quality != TimeQuality::MaxDurationSample
        {
            return Err(InvalidObservation::TimeQualityShapeMismatch);
        }
        if matches!(
            &payload,
            ObservationPayload::ChildSignalTermination(retained) if retained.signal.is_none()
        ) {
            return Err(InvalidObservation::ChildTerminationWithoutSignal);
        }
        let time_quality_is_valid_for_payload = match &payload {
            ObservationPayload::ErrorGroup(_) => matches!(
                time.quality,
                TimeQuality::FirstInGroup
                    | TimeQuality::RepresentativeSample
                    | TimeQuality::ParsedWithoutVerifiedOffset
                    | TimeQuality::CollectionFallback
            ),
            ObservationPayload::SlowQueryGroup(_) => time.quality == TimeQuality::MaxDurationSample,
            ObservationPayload::LogGap(_) => time.quality == TimeQuality::IntervalOnly,
            _ => matches!(
                time.quality,
                TimeQuality::Exact
                    | TimeQuality::ParsedWithoutVerifiedOffset
                    | TimeQuality::CollectionFallback
            ),
        };
        if !time_quality_is_valid_for_payload {
            return Err(InvalidObservation::TimeQualityShapeMismatch);
        }
        let observation_id = ObservationId::derive(lineage.id, source_type_id, &provenance);
        Ok(Self {
            observation_id,
            identity_quality: lineage.quality,
            source_scope_id: lineage.source_scope_id,
            source_type_id,
            provenance,
            shape,
            time,
            occurrence_count,
            payload,
            evidence_quality,
            quality_flags,
            loss,
        })
    }

    /// Observation identity.
    #[must_use]
    pub const fn observation_id(&self) -> ObservationId {
        self.observation_id
    }

    /// Identity quality.
    #[must_use]
    pub const fn identity_quality(&self) -> IdentityQuality {
        self.identity_quality
    }

    /// Source scope identity.
    #[must_use]
    pub const fn source_scope_id(&self) -> SourceScopeId {
        self.source_scope_id
    }

    /// Source type ID.
    #[must_use]
    pub const fn source_type_id(&self) -> u32 {
        self.source_type_id
    }

    /// Provenance record.
    #[must_use]
    pub const fn provenance(&self) -> &ObservationProvenance {
        &self.provenance
    }

    /// Observation shape.
    #[must_use]
    pub const fn shape(&self) -> ObservationShape {
        self.shape
    }

    /// Time attribution.
    #[must_use]
    pub const fn time(&self) -> ObservationTime {
        self.time
    }

    /// Retained occurrence count.
    #[must_use]
    pub const fn occurrence_count(&self) -> u64 {
        self.occurrence_count
    }

    /// Typed retained payload.
    #[must_use]
    pub const fn payload(&self) -> &ObservationPayload {
        &self.payload
    }

    /// Payload evidence quality.
    #[must_use]
    pub const fn evidence_quality(&self) -> EvidenceQuality {
        self.evidence_quality
    }

    /// Extractor-versioned quality flags.
    #[must_use]
    pub const fn quality_flags(&self) -> QualityFlags {
        self.quality_flags
    }

    /// Proven loss summary, when present.
    #[must_use]
    pub const fn loss(&self) -> Option<&LossSummary> {
        self.loss.as_ref()
    }

    /// Canonical order by timestamp and identity.
    #[must_use]
    pub fn canonical_cmp(&self, other: &Self) -> Ordering {
        self.time
            .sort_ts_us
            .cmp(&other.time.sort_ts_us)
            .then_with(|| self.observation_id.cmp(&other.observation_id))
    }
}

fn validate_time(shape: ObservationShape, time: ObservationTime) -> Result<(), InvalidObservation> {
    if time.quality == TimeQuality::IntervalOnly {
        let Some(interval) = time.observed_interval else {
            return Err(InvalidObservation::InvalidTimeAttribution);
        };
        if shape != ObservationShape::Gap
            || time.occurred_at_us.is_some()
            || time.sort_ts_us != interval.start_us()
        {
            return Err(InvalidObservation::TimeQualityShapeMismatch);
        }
        return Ok(());
    }
    if time.observed_interval.is_some() || shape == ObservationShape::Gap {
        return Err(InvalidObservation::TimeQualityShapeMismatch);
    }
    match time.quality {
        TimeQuality::CollectionFallback if time.occurred_at_us.is_none() => Ok(()),
        TimeQuality::FirstInGroup
        | TimeQuality::RepresentativeSample
        | TimeQuality::MaxDurationSample
            if shape != ObservationShape::GroupedCount =>
        {
            Err(InvalidObservation::TimeQualityShapeMismatch)
        }
        TimeQuality::Exact
        | TimeQuality::FirstInGroup
        | TimeQuality::RepresentativeSample
        | TimeQuality::MaxDurationSample
        | TimeQuality::ParsedWithoutVerifiedOffset
            if time.occurred_at_us == Some(time.sort_ts_us) =>
        {
            Ok(())
        }
        _ => Err(InvalidObservation::InvalidTimeAttribution),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(value: &str) -> [u8; 32] {
        let mut out = [0_u8; 32];
        for (slot, pair) in out.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            let pair = str::from_utf8(pair).expect("hex fixture is ASCII");
            *slot = u8::from_str_radix(pair, 16).expect("hex fixture digit");
        }
        out
    }

    fn locator(value: u8) -> SegmentLocator {
        SegmentLocator([value; 32])
    }

    fn lineage(segment_locator: SegmentLocator) -> SegmentIdentity {
        SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([2; 16]),
            segment_locator,
            7,
            b"type=7 rows=3 crc=abc",
        )
    }

    fn provenance(
        row_ordinal: u32,
        segment_locator: Option<SegmentLocator>,
    ) -> ObservationProvenance {
        ObservationProvenance {
            segment_locator,
            section_body_id: SectionBodyId([0xAA; 32]),
            catalog_entry_ordinal: 0,
            row_ordinal,
            dictionary_context_id: DictionaryContextId([0xBB; 32]),
            source_locator: None,
        }
    }

    #[allow(
        clippy::unnecessary_box_returns,
        reason = "the fixture is passed directly to boxed payload variants"
    )]
    fn lifecycle() -> Box<LifecyclePayload> {
        Box::new(LifecyclePayload {
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dropped_field_count: DroppedFieldCount::default(),
        })
    }

    fn individual(row: u32) -> EventObservation {
        let segment_locator = locator(3);
        make_observation(
            provenance(row, Some(segment_locator)),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: 1_000,
                occurred_at_us: Some(1_000),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ReadyObserved(lifecycle()),
        )
        .expect("valid fixture")
    }

    fn make_observation(
        provenance: ObservationProvenance,
        shape: ObservationShape,
        time: ObservationTime,
        count: u64,
        payload: ObservationPayload,
    ) -> Result<EventObservation, InvalidObservation> {
        EventObservation::new(
            lineage(locator(3)),
            7,
            provenance,
            shape,
            time,
            count,
            payload,
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
    }

    fn point_time() -> ObservationTime {
        ObservationTime {
            sort_ts_us: 1,
            occurred_at_us: Some(1),
            observed_interval: None,
            quality: TimeQuality::Exact,
        }
    }

    fn slow_query() -> ObservationPayload {
        ObservationPayload::SlowQueryGroup(Box::new(SlowQueryPayload {
            pattern: None,
            sample: None,
            max_duration_ms: FiniteF64::new(2.0).expect("finite fixture"),
            total_duration_ms: FiniteF64::new(3.0).expect("finite fixture"),
            dropped_field_count: DroppedFieldCount::default(),
        }))
    }

    #[test]
    fn sealed_lineage_uses_naming_contract_and_locator() {
        let base = lineage(locator(3)).id();
        assert_ne!(base, lineage(locator(4)).id());
        let changed_contract = SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([9; 16]),
            locator(3),
            7,
            b"type=7 rows=3 crc=abc",
        );
        assert_ne!(base, changed_contract.id());
    }

    #[test]
    fn unproven_live_lineage_is_separate_and_approximate() {
        let live = SegmentIdentity::live_approximate(SourceScopeId([1; 32]), 8, b"first-part");
        assert_eq!(live.quality(), IdentityQuality::Approximate);
        assert_ne!(live.id(), lineage(locator(3)).id());
    }

    #[test]
    fn identity_preimages_are_pinned() {
        let sealed = lineage(locator(3));
        assert_eq!(
            sealed.id().0,
            hex32("c70f5cc2edee83f3cbcf707be01519c8a13ec7419247bf181b13916ff9738016")
        );
        let live = SegmentIdentity::live_approximate(SourceScopeId([1; 32]), 8, b"first-part");
        assert_eq!(
            live.id().0,
            hex32("9e2a5dcb39d5b100b7d4e7a1c1f0a105efac30f24be2b4cf6c548f25bce321b0")
        );
        assert_eq!(
            individual(4).observation_id().0,
            hex32("35b184dbaacc0a087dabef84a1900ddc4d3e4fa4b73c1d1af80d5adec193b3c0")
        );
    }

    #[test]
    fn catalog_and_row_ordinals_distinguish_identical_bodies() {
        let segment_locator = locator(3);
        let base = individual(4);
        let mut other_provenance = provenance(4, Some(segment_locator));
        other_provenance.catalog_entry_ordinal = 1;
        let other = EventObservation::new(
            lineage(segment_locator),
            7,
            other_provenance,
            ObservationShape::Individual,
            base.time(),
            1,
            ObservationPayload::ReadyObserved(lifecycle()),
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
        .expect("valid fixture");
        assert_ne!(base.observation_id(), other.observation_id());
        assert_ne!(base.observation_id(), individual(5).observation_id());
    }

    #[test]
    fn constructor_rejects_locator_shape_and_count_mismatches() {
        let segment_locator = locator(3);
        assert_eq!(
            make_observation(
                provenance(0, Some(locator(9))),
                ObservationShape::Individual,
                point_time(),
                1,
                ObservationPayload::ReadyObserved(lifecycle()),
            ),
            Err(InvalidObservation::SegmentLocatorMismatch)
        );
        assert_eq!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::GroupedCount,
                point_time(),
                2,
                ObservationPayload::ReadyObserved(lifecycle()),
            ),
            Err(InvalidObservation::PayloadShapeMismatch)
        );
        assert_eq!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::Individual,
                point_time(),
                2,
                ObservationPayload::ReadyObserved(lifecycle()),
            ),
            Err(InvalidObservation::SingleOccurrenceShapeCountNotOne)
        );
        assert_eq!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::Individual,
                point_time(),
                0,
                ObservationPayload::ReadyObserved(lifecycle()),
            ),
            Err(InvalidObservation::ZeroOccurrenceCount)
        );
    }

    #[test]
    fn constructor_rejects_invalid_time_and_signal_contracts() {
        let segment_locator = locator(3);
        let invalid_exact_time = ObservationTime {
            sort_ts_us: 1,
            occurred_at_us: None,
            observed_interval: None,
            quality: TimeQuality::Exact,
        };
        assert_eq!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::Individual,
                invalid_exact_time,
                1,
                ObservationPayload::ReadyObserved(lifecycle()),
            ),
            Err(InvalidObservation::InvalidTimeAttribution)
        );

        let first_in_group = ObservationTime {
            sort_ts_us: 1,
            occurred_at_us: Some(1),
            observed_interval: None,
            quality: TimeQuality::FirstInGroup,
        };
        assert_eq!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::GroupedCount,
                first_in_group,
                1,
                slow_query(),
            ),
            Err(InvalidObservation::TimeQualityShapeMismatch)
        );
        assert_eq!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::Individual,
                point_time(),
                1,
                ObservationPayload::ChildSignalTermination(lifecycle()),
            ),
            Err(InvalidObservation::ChildTerminationWithoutSignal)
        );
    }

    #[test]
    fn constructor_accepts_fallback_and_slow_query_time_contracts() {
        let segment_locator = locator(3);
        let fallback = ObservationTime {
            sort_ts_us: 1,
            occurred_at_us: None,
            observed_interval: None,
            quality: TimeQuality::CollectionFallback,
        };
        assert!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::Individual,
                fallback,
                1,
                ObservationPayload::ReadyObserved(lifecycle()),
            )
            .is_ok()
        );
        let max_duration = ObservationTime {
            quality: TimeQuality::MaxDurationSample,
            ..point_time()
        };
        assert!(
            make_observation(
                provenance(0, Some(segment_locator)),
                ObservationShape::GroupedCount,
                max_duration,
                1,
                slow_query(),
            )
            .is_ok()
        );
    }

    #[test]
    fn interval_only_gap_requires_a_matching_interval() {
        let segment_locator = locator(3);
        let gap = Box::new(LogGapPayload {
            source_path: None,
            parser_kind: 2,
            reason: 10,
            dev: None,
            inode: None,
            offset: None,
            bytes_skipped: 0,
            truncated_lines: 0,
            invalid_utf8: 0,
            binary_dropped: 0,
            rotations: 0,
            missing_files: 0,
            budget_exhaustions: 0,
            dropped_field_count: DroppedFieldCount::default(),
            parser_dropped_lines: 1,
        });
        let interval = CoverageSpan::new(10, 30).expect("valid span");
        let observation = EventObservation::new(
            lineage(segment_locator),
            7,
            provenance(0, Some(segment_locator)),
            ObservationShape::Gap,
            ObservationTime {
                sort_ts_us: 10,
                occurred_at_us: None,
                observed_interval: Some(interval),
                quality: TimeQuality::IntervalOnly,
            },
            1,
            ObservationPayload::LogGap(gap),
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        );
        assert!(observation.is_ok());
    }

    #[test]
    fn error_payload_retains_all_text_fields_and_drop_count() {
        let payload = ErrorGroupPayload {
            severity: Severity::Fatal,
            category: ErrorCategory::Resource,
            sqlstate: Some(SqlState(*b"53300")),
            normalized_pattern: Some("pattern".into()),
            sample: Some("sample".into()),
            detail: Some("detail".into()),
            hint: Some("hint".into()),
            context: Some("context".into()),
            statement: Some("statement".into()),
            database: Some("postgres".into()),
            user: Some("alice".into()),
            dropped_field_count: DroppedFieldCount(2),
        };
        assert_eq!(payload.sample.as_deref(), Some("sample"));
        assert_eq!(payload.dropped_field_count, DroppedFieldCount(2));
    }

    #[test]
    fn canonical_order_uses_time_then_id() {
        let early = individual(1);
        let later_id = individual(2);
        assert_eq!(
            early.canonical_cmp(&later_id),
            early.observation_id().cmp(&later_id.observation_id())
        );
    }

    #[test]
    fn loss_reasons_are_sorted_and_deduplicated() {
        let reasons = std::iter::repeat_n(LossReason::TailerBound, 10_000)
            .chain([LossReason::GroupCapExceeded]);
        let summary = LossSummary::new(reasons, Some(3));
        assert_eq!(
            summary.reasons(),
            &[LossReason::GroupCapExceeded, LossReason::TailerBound]
        );
    }
}
