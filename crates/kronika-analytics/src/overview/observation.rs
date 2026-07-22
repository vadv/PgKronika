//! Event observations: what the source retained, with content-derived
//! identity.
//!
//! An observation states exactly what one stored row or sample asserted —
//! never more. A grouped error row stays one observation with its retained
//! `occurrence_count`; a signal-9 termination is a SIGKILL observation, not an
//! OOM verdict. Layers above (facts, notable policy, diagnosis) may interpret,
//! but they cannot rewrite these records or their IDs.
//!
//! Identity is derived from provenance by SHA-256 over domain-separated,
//! offset-independent inputs, so an observation keeps the same ID across the
//! live-to-sealed handoff and a derived rebuild. Policy and formula versions
//! never enter an ID: re-scoring must not re-identify history.

use std::cmp::Ordering;
use std::fmt;

use super::counts::{ErrorCategory, Severity, SqlState};
use super::coverage::CoverageSpan;
use super::sha256;

/// Domain-separation tag of the lineage preimage.
const LINEAGE_DOMAIN_TAG: &[u8] = b"pgk-overview-lineage-v1";

/// Domain-separation tag of the observation preimage.
const OBSERVATION_DOMAIN_TAG: &[u8] = b"pgk-overview-observation-v1";

/// Identity of the source scope a segment belongs to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceScopeId(pub [u8; 32]);

/// Stable identity of one segment lineage across live, sealed, and rebuilt
/// states.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentLineageId(pub [u8; 32]);

/// Digest of one exact section body together with its type and length.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionBodyId(pub [u8; 32]);

/// Digest of the canonical `(StrId, resolved bytes)` set an observation
/// depends on.
///
/// Identical section bytes can mean something else under another dictionary,
/// so the resolved context is part of identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DictionaryContextId(pub [u8; 32]);

/// Content-derived identity of one retained observation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObservationId(pub [u8; 32]);

/// Identity of one derived fact that cites observations as evidence.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactId(pub [u8; 32]);

fn fmt_id(f: &mut fmt::Formatter<'_>, name: &str, bytes: &[u8; 32]) -> fmt::Result {
    write!(f, "{name}(")?;
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    write!(f, ")")
}

impl fmt::Debug for SourceScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_id(f, "SourceScopeId", &self.0)
    }
}

impl fmt::Debug for SegmentLineageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_id(f, "SegmentLineageId", &self.0)
    }
}

impl fmt::Debug for SectionBodyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_id(f, "SectionBodyId", &self.0)
    }
}

impl fmt::Debug for DictionaryContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_id(f, "DictionaryContextId", &self.0)
    }
}

impl fmt::Debug for ObservationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_id(f, "ObservationId", &self.0)
    }
}

impl fmt::Debug for FactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_id(f, "FactId", &self.0)
    }
}

impl SegmentLineageId {
    /// Derives the lineage identity from offset-independent segment inputs.
    ///
    /// The descriptor bytes must be built only from offset-independent catalog
    /// fields, so a lineage can be identified without reading unrelated
    /// bodies. Integers are encoded little-endian fixed-width; the descriptor
    /// is the only variable-length field and comes last, so the preimage is
    /// unambiguous.
    #[must_use]
    pub fn derive(
        source_scope_id: SourceScopeId,
        first_entry_type: u32,
        first_entry_content_descriptor: &[u8],
    ) -> Self {
        Self(sha256::digest_parts(&[
            LINEAGE_DOMAIN_TAG,
            &source_scope_id.0,
            &first_entry_type.to_le_bytes(),
            first_entry_content_descriptor,
        ]))
    }
}

impl ObservationId {
    /// Derives the observation identity from its lineage and provenance.
    ///
    /// The preimage covers the section body digest, both ordinals, and the
    /// dictionary context; every field is fixed-width. [`SourceLocator`] is
    /// deliberately excluded: identity is content-derived and must survive
    /// sources that carry no physical location.
    #[must_use]
    pub fn derive(
        lineage: SegmentLineageId,
        source_type_id: u32,
        provenance: &ObservationProvenance,
    ) -> Self {
        Self(sha256::digest_parts(&[
            OBSERVATION_DOMAIN_TAG,
            &lineage.0,
            &source_type_id.to_le_bytes(),
            &provenance.section_body_id.0,
            &provenance.section_instance_ordinal.to_le_bytes(),
            &provenance.row_ordinal.to_le_bytes(),
            &provenance.dictionary_context_id.0,
        ]))
    }
}

/// How strongly an observation's identity is tied to the physical source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityQuality {
    /// The source itself carries a unique occurrence identity.
    SourceExact,
    /// Identity is derived from retained content and provenance ordinals.
    ContentDerived,
    /// Identity may collide across sources or rebuilds.
    Approximate,
}

/// The shape of what one observation asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationShape {
    /// One occurrence at one point in time.
    Individual,
    /// A retained group of occurrences with a single stored timestamp.
    GroupedCount,
    /// A declared hole in collection: nothing is asserted about the interval.
    Gap,
    /// A pair of cumulative counter samples forming a candidate interval.
    CounterInterval,
    /// A single state change between two observed states.
    StateTransition,
}

/// How trustworthy the stored timestamps are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeQuality {
    /// The source stored the exact occurrence timestamp.
    Exact,
    /// The timestamp belongs to the first occurrence of a retained group.
    FirstInGroup,
    /// The collection cycle time stands in for a missing source timestamp.
    CollectionFallback,
    /// Only a containing interval is known, not a point.
    IntervalOnly,
}

/// How the payload content was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceQuality {
    /// Read from structured source fields.
    Structured,
    /// Parsed out of free text; tokens may be misclassified.
    Parsed,
    /// Inferred by a heuristic; the weakest class.
    Heuristic,
    /// Computed exactly from other retained values.
    DerivedExact,
}

/// The time attribution of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationTime {
    /// The timestamp that places the observation into exactly one bucket.
    pub sort_ts_us: i64,
    /// The exact occurrence time, when the source stored one.
    pub occurred_at_us: Option<i64>,
    /// The containing interval, when only an interval is known.
    pub observed_interval: Option<CoverageSpan>,
    /// Which of the timestamps above can be trusted, and how far.
    pub quality: TimeQuality,
}

/// Where the physical source says a row came from.
///
/// Advisory evidence for `identity_quality`; never part of a content-derived
/// ID, which must survive resegmentation and offset changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLocator {
    /// Digest of the writer-declared physical source unit identity.
    pub source_unit_id: [u8; 32],
    /// Byte offset of the row within that unit.
    pub byte_offset: u64,
}

/// The provenance that identifies one retained row inside its lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationProvenance {
    /// Digest of the section body the row was stored in.
    pub section_body_id: SectionBodyId,
    /// Which repetition of an identical body this is, in catalog order.
    pub section_instance_ordinal: u32,
    /// The row position inside the section body.
    pub row_ordinal: u32,
    /// Digest of the resolved dictionary context the row depends on.
    pub dictionary_context_id: DictionaryContextId,
    /// Physical location claim, when the writer provided one.
    pub source_locator: Option<SourceLocator>,
}

/// Extractor-versioned quality bits.
///
/// Bit assignment is owned by the extractor semantics version; the pure core
/// carries the set opaquely and compares it exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QualityFlags(pub u32);

/// Why retained data around an observation is known to be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LossReason {
    /// The per-cycle error group cap dropped whole groups.
    GroupCapExceeded,
    /// The lifecycle observation cap dropped records.
    LifecycleCapExceeded,
    /// The parser could not decode part of the stream.
    ParserBound,
    /// The tailer lost part of the stream.
    TailerBound,
    /// A dictionary bound dropped referenced values.
    DictionaryBound,
}

/// A summary of proven loss attached to an observation.
///
/// Loss is a positive statement — the source proved that data is missing —
/// and never an inferred zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LossSummary {
    reasons: Vec<LossReason>,
    /// The proven minimum number of lost records, when the source counted.
    pub lost_count_lower_bound: Option<u64>,
}

impl LossSummary {
    /// Builds a summary with sorted, deduplicated reasons.
    #[must_use]
    pub fn new(
        reasons: impl IntoIterator<Item = LossReason>,
        lost_count_lower_bound: Option<u64>,
    ) -> Self {
        let mut reasons: Vec<LossReason> = reasons.into_iter().collect();
        reasons.sort_unstable();
        reasons.dedup();
        Self {
            reasons,
            lost_count_lower_bound,
        }
    }

    /// The loss reasons, sorted and unique.
    #[must_use]
    pub fn reasons(&self) -> &[LossReason] {
        &self.reasons
    }
}

/// Which optional error-group fields were dropped by bounds.
///
/// Distinguishes a field the source never had (`None`, not dropped) from one
/// that was lost to a cap (`None`, dropped), so absence never silently
/// upgrades to knowledge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DroppedFields {
    /// The normalized pattern was dropped.
    pub normalized_pattern: bool,
    /// The database name was dropped.
    pub database: bool,
    /// The user name was dropped.
    pub user: bool,
}

/// The retained payload of one grouped error row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorGroupPayload {
    /// The line severity.
    pub severity: Severity,
    /// The classified category.
    pub category: ErrorCategory,
    /// The SQLSTATE, when the source retained one.
    pub sqlstate: Option<SqlState>,
    /// The normalized message pattern, when retained.
    pub normalized_pattern: Option<Box<str>>,
    /// The database name, when retained.
    pub database: Option<Box<str>>,
    /// The user name, when retained.
    pub user: Option<Box<str>>,
    /// Which of the optional fields above were dropped by bounds.
    pub dropped_fields: DroppedFields,
}

/// The typed payload of one retained observation.
///
/// The closed v1 set of retained `PostgreSQL` log observation kinds. Each
/// variant maps to one stable wire code via [`Self::kind_code`]; kinds whose
/// canonical index stores no decoded fields in v1 are unit variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationPayload {
    /// A grouped error row with its joint dimension.
    ErrorGroup(Box<ErrorGroupPayload>),
    /// A child process terminated by the given raw signal.
    ChildSignalTermination {
        /// The raw signal number as logged; never interpreted as a cause.
        signal: i32,
    },
    /// A shutdown request was observed.
    ShutdownRequested,
    /// The server reported readiness.
    ReadyObserved,
    /// A checkpoint started.
    CheckpointStarted,
    /// A checkpoint completed.
    CheckpointCompleted,
    /// Checkpoints were reported as too frequent.
    CheckpointTooFrequent,
    /// An autovacuum run was reported.
    AutovacuumReported,
    /// An autoanalyze run was reported.
    AutoanalyzeReported,
    /// A slow query group was reported.
    SlowQueryGroup,
    /// A lock wait was reported.
    LockWaitReported,
    /// A lock was acquired after waiting.
    LockAcquiredAfterWait,
    /// A temporary file was reported.
    TempFileReported,
    /// The collector declared a gap in the log stream.
    LogGap,
}

impl ObservationPayload {
    /// The stable machine code of this observation kind on the wire.
    #[must_use]
    pub const fn kind_code(&self) -> &'static str {
        match self {
            Self::ErrorGroup(_) => "pg.log.error_group_observed",
            Self::ChildSignalTermination { .. } => "pg.lifecycle.child_signal_termination",
            Self::ShutdownRequested => "pg.lifecycle.shutdown_requested",
            Self::ReadyObserved => "pg.lifecycle.ready_observed",
            Self::CheckpointStarted => "pg.checkpoint.started",
            Self::CheckpointCompleted => "pg.checkpoint.completed",
            Self::CheckpointTooFrequent => "pg.checkpoint.too_frequent_reported",
            Self::AutovacuumReported => "pg.maintenance.autovacuum_reported",
            Self::AutoanalyzeReported => "pg.maintenance.autoanalyze_reported",
            Self::SlowQueryGroup => "pg.query.slow_group_reported",
            Self::LockWaitReported => "pg.lock.wait_reported",
            Self::LockAcquiredAfterWait => "pg.lock.acquired_after_wait_reported",
            Self::TempFileReported => "pg.temp_file.reported",
            Self::LogGap => "collector.pg_log_gap",
        }
    }
}

/// A validation failure of an observation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidObservation {
    /// `occurrence_count` is zero: a retained observation asserts at least
    /// one occurrence.
    ZeroOccurrenceCount,
    /// An individual or state-transition shape carries a count other than
    /// one.
    SingleOccurrenceShapeCountNotOne,
}

/// One retained observation: exactly what the source stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventObservation {
    /// Content-derived identity, stable across live, sealed, and rebuilt
    /// states.
    pub observation_id: ObservationId,
    /// How strongly the identity is tied to the physical source.
    pub identity_quality: IdentityQuality,
    /// The source scope the observation belongs to.
    pub source_scope_id: SourceScopeId,
    /// The source type the row was extracted from.
    pub source_type_id: u32,
    /// The provenance that derived [`Self::observation_id`].
    pub provenance: ObservationProvenance,
    /// What kind of assertion this observation makes.
    pub shape: ObservationShape,
    /// The time attribution.
    pub time: ObservationTime,
    /// How many occurrences the source retained for this record, never zero.
    pub occurrence_count: u64,
    /// The typed retained payload.
    pub payload: ObservationPayload,
    /// How the payload content was obtained.
    pub evidence_quality: EvidenceQuality,
    /// Extractor-versioned quality bits.
    pub quality_flags: QualityFlags,
    /// Proven loss around this observation, when any.
    pub loss: Option<LossSummary>,
}

impl EventObservation {
    /// Checks the occurrence-count invariants and returns the record intact.
    ///
    /// An individual observation or state transition asserts exactly one
    /// occurrence; a grouped row keeps its retained count. A count of zero is
    /// invalid for every shape: a record that observed nothing must not
    /// exist.
    ///
    /// # Errors
    /// Returns [`InvalidObservation`] naming the violated invariant.
    pub fn validated(self) -> Result<Self, InvalidObservation> {
        if self.occurrence_count == 0 {
            return Err(InvalidObservation::ZeroOccurrenceCount);
        }
        let single_occurrence = matches!(
            self.shape,
            ObservationShape::Individual | ObservationShape::StateTransition
        );
        if single_occurrence && self.occurrence_count != 1 {
            return Err(InvalidObservation::SingleOccurrenceShapeCountNotOne);
        }
        Ok(self)
    }

    /// Canonical response order: `sort_ts_us` ascending, then the observation
    /// ID.
    ///
    /// Total and content-derived, so byte-identical rows with distinct
    /// provenance keep a stable relative order everywhere.
    #[must_use]
    pub fn canonical_cmp(&self, other: &Self) -> Ordering {
        self.time
            .sort_ts_us
            .cmp(&other.time.sort_ts_us)
            .then_with(|| self.observation_id.cmp(&other.observation_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(row_ordinal: u32) -> ObservationProvenance {
        ObservationProvenance {
            section_body_id: SectionBodyId([0xAA; 32]),
            section_instance_ordinal: 0,
            row_ordinal,
            dictionary_context_id: DictionaryContextId([0xBB; 32]),
            source_locator: None,
        }
    }

    fn lineage() -> SegmentLineageId {
        SegmentLineageId::derive(SourceScopeId([1; 32]), 7, b"type=7 rows=3 crc=abc")
    }

    fn observation(shape: ObservationShape, occurrence_count: u64) -> EventObservation {
        let provenance = provenance(0);
        EventObservation {
            observation_id: ObservationId::derive(lineage(), 7, &provenance),
            identity_quality: IdentityQuality::ContentDerived,
            source_scope_id: SourceScopeId([1; 32]),
            source_type_id: 7,
            provenance,
            shape,
            time: ObservationTime {
                sort_ts_us: 1_000,
                occurred_at_us: Some(1_000),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            occurrence_count,
            payload: ObservationPayload::ReadyObserved,
            evidence_quality: EvidenceQuality::Structured,
            quality_flags: QualityFlags::default(),
            loss: None,
        }
    }

    #[test]
    fn identity_derivation_is_deterministic() {
        // A live extraction and a sealed rebuild feed the same inputs and
        // must agree on the ID.
        let a = ObservationId::derive(lineage(), 7, &provenance(4));
        let b = ObservationId::derive(lineage(), 7, &provenance(4));
        assert_eq!(a, b);
    }

    #[test]
    fn provenance_ordinals_distinguish_byte_identical_rows() {
        let base = ObservationId::derive(lineage(), 7, &provenance(4));
        let other_row = ObservationId::derive(lineage(), 7, &provenance(5));
        assert_ne!(base, other_row);

        let mut repeated_section = provenance(4);
        repeated_section.section_instance_ordinal = 1;
        let other_instance = ObservationId::derive(lineage(), 7, &repeated_section);
        assert_ne!(base, other_instance);
        assert_ne!(other_row, other_instance);
    }

    #[test]
    fn dictionary_context_changes_the_observation_id() {
        let base = ObservationId::derive(lineage(), 7, &provenance(0));
        let mut shifted = provenance(0);
        shifted.dictionary_context_id = DictionaryContextId([0xCC; 32]);
        assert_ne!(base, ObservationId::derive(lineage(), 7, &shifted));
    }

    #[test]
    fn source_locator_never_affects_the_observation_id() {
        let base = ObservationId::derive(lineage(), 7, &provenance(0));
        let mut located = provenance(0);
        located.source_locator = Some(SourceLocator {
            source_unit_id: [3; 32],
            byte_offset: 12_345,
        });
        assert_eq!(base, ObservationId::derive(lineage(), 7, &located));
    }

    #[test]
    fn every_lineage_input_changes_the_lineage_id() {
        let base = SegmentLineageId::derive(SourceScopeId([1; 32]), 7, b"desc");
        assert_ne!(
            base,
            SegmentLineageId::derive(SourceScopeId([2; 32]), 7, b"desc")
        );
        assert_ne!(
            base,
            SegmentLineageId::derive(SourceScopeId([1; 32]), 8, b"desc")
        );
        assert_ne!(
            base,
            SegmentLineageId::derive(SourceScopeId([1; 32]), 7, b"other")
        );
    }

    #[test]
    fn validated_rejects_a_zero_occurrence_count() {
        for shape in [
            ObservationShape::Individual,
            ObservationShape::GroupedCount,
            ObservationShape::Gap,
        ] {
            assert_eq!(
                observation(shape, 0).validated(),
                Err(InvalidObservation::ZeroOccurrenceCount)
            );
        }
    }

    #[test]
    fn validated_pins_individual_and_transition_counts_to_one() {
        assert_eq!(
            observation(ObservationShape::Individual, 2).validated(),
            Err(InvalidObservation::SingleOccurrenceShapeCountNotOne)
        );
        assert_eq!(
            observation(ObservationShape::StateTransition, 3).validated(),
            Err(InvalidObservation::SingleOccurrenceShapeCountNotOne)
        );
        assert!(
            observation(ObservationShape::Individual, 1)
                .validated()
                .is_ok()
        );
        assert!(
            observation(ObservationShape::StateTransition, 1)
                .validated()
                .is_ok()
        );
    }

    #[test]
    fn validated_keeps_a_grouped_row_count_as_retained() {
        let grouped = observation(ObservationShape::GroupedCount, 42).validated();
        assert_eq!(grouped.map(|o| o.occurrence_count), Ok(42));
    }

    #[test]
    fn kind_codes_are_the_stable_wire_codes() {
        let error_group = ObservationPayload::ErrorGroup(Box::new(ErrorGroupPayload {
            severity: Severity::Fatal,
            category: ErrorCategory::Resource,
            sqlstate: Some(SqlState(*b"53300")),
            normalized_pattern: None,
            database: None,
            user: None,
            dropped_fields: DroppedFields::default(),
        }));
        let cases = [
            (error_group, "pg.log.error_group_observed"),
            (
                ObservationPayload::ChildSignalTermination { signal: 9 },
                "pg.lifecycle.child_signal_termination",
            ),
            (
                ObservationPayload::ShutdownRequested,
                "pg.lifecycle.shutdown_requested",
            ),
            (
                ObservationPayload::ReadyObserved,
                "pg.lifecycle.ready_observed",
            ),
            (
                ObservationPayload::CheckpointStarted,
                "pg.checkpoint.started",
            ),
            (
                ObservationPayload::CheckpointCompleted,
                "pg.checkpoint.completed",
            ),
            (
                ObservationPayload::CheckpointTooFrequent,
                "pg.checkpoint.too_frequent_reported",
            ),
            (
                ObservationPayload::AutovacuumReported,
                "pg.maintenance.autovacuum_reported",
            ),
            (
                ObservationPayload::AutoanalyzeReported,
                "pg.maintenance.autoanalyze_reported",
            ),
            (
                ObservationPayload::SlowQueryGroup,
                "pg.query.slow_group_reported",
            ),
            (
                ObservationPayload::LockWaitReported,
                "pg.lock.wait_reported",
            ),
            (
                ObservationPayload::LockAcquiredAfterWait,
                "pg.lock.acquired_after_wait_reported",
            ),
            (
                ObservationPayload::TempFileReported,
                "pg.temp_file.reported",
            ),
            (ObservationPayload::LogGap, "collector.pg_log_gap"),
        ];
        for (payload, code) in cases {
            assert_eq!(payload.kind_code(), code);
        }
    }

    #[test]
    fn canonical_order_is_sort_ts_then_observation_id() {
        let mut early = observation(ObservationShape::Individual, 1);
        early.time.sort_ts_us = 100;
        let mut late = observation(ObservationShape::Individual, 1);
        late.time.sort_ts_us = 200;
        assert_eq!(early.canonical_cmp(&late), Ordering::Less);

        // Same timestamp: the ID breaks the tie, in either direction.
        let mut tied = late.clone();
        tied.observation_id = ObservationId::derive(lineage(), 7, &provenance(9));
        assert_eq!(
            late.canonical_cmp(&tied),
            late.observation_id.cmp(&tied.observation_id)
        );
    }

    #[test]
    fn loss_summary_reasons_sort_and_dedup() {
        let summary = LossSummary::new(
            [
                LossReason::TailerBound,
                LossReason::GroupCapExceeded,
                LossReason::TailerBound,
            ],
            Some(3),
        );
        assert_eq!(
            summary.reasons(),
            &[LossReason::GroupCapExceeded, LossReason::TailerBound]
        );
        assert_eq!(summary.lost_count_lower_bound, Some(3));
    }

    #[test]
    fn id_debug_prints_hex() {
        let id = ObservationId([0xAB; 32]);
        let text = format!("{id:?}");
        assert!(text.starts_with("ObservationId(abab"), "got {text}");
    }
}
