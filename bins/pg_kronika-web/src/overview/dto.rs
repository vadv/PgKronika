//! Typed wire projection for timeline event facts.
//!
//! Reader observations retain physical provenance. This module projects one
//! notable observation into the stable, redacted HTTP fact contract used by
//! both the overview preview and `/events`. A semantic event ID is derived
//! from published semantic fields; a separate instance ID binds the physical
//! retained observation, so equal semantic facts at distinct provenance are
//! never collapsed during pagination.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::overview::{
    ErrorCategory, EventObservation, EvidenceQuality, IdentityQuality, LossReason, LossSummary,
    NotableClass, NotablePolicy, ObservationPayload, Severity,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// Canonical three-part `/events` order and cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EventFactPosition {
    pub(crate) sort_ts_us: i64,
    pub(crate) event_id: [u8; 32],
    pub(crate) event_instance_id: [u8; 32],
}

/// One stable, typed notable fact on the public wire.
#[derive(Debug, Serialize)]
pub(crate) struct EventFact {
    pub(crate) event_id: String,
    pub(crate) event_instance_id: String,
    pub(crate) source_id: u64,
    pub(crate) source_scope_id: String,
    pub(crate) source_type_id: u32,
    pub(crate) identity_quality: &'static str,
    pub(crate) sort_ts_us: i64,
    pub(crate) occurred_at_us: Option<i64>,
    pub(crate) occurrence_count: u64,
    pub(crate) event_kind: &'static str,
    pub(crate) notable_class: &'static str,
    pub(crate) evidence_quality: &'static str,
    pub(crate) quality_flags: u32,
    pub(crate) payload: EventPayload,
    pub(crate) supporting_evidence: [SupportingEvidence; 1],
    pub(crate) loss: Option<EventLoss>,
}

/// Redacted typed payload of a notable fact.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum EventPayload {
    Error(ErrorPayload),
    Lifecycle(LifecyclePayload),
}

/// Published grouped-error fields.
#[derive(Debug, Serialize)]
pub(crate) struct ErrorPayload {
    pub(crate) kind: &'static str,
    pub(crate) severity: &'static str,
    pub(crate) category: &'static str,
    pub(crate) sqlstate: Option<String>,
    pub(crate) dropped_field_count: u32,
}

/// Published lifecycle fields.
#[derive(Debug, Serialize)]
pub(crate) struct LifecyclePayload {
    pub(crate) kind: &'static str,
    pub(crate) pid: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) dropped_field_count: u32,
}

/// One physical observation supporting an event fact.
#[derive(Debug, Serialize)]
pub(crate) struct SupportingEvidence {
    pub(crate) observation_id: String,
    pub(crate) section_body_id: String,
    pub(crate) catalog_entry_ordinal: u32,
    pub(crate) row_ordinal: u32,
    pub(crate) dictionary_context_id: String,
    pub(crate) segment_locator: Option<String>,
}

/// Proven upstream loss attached to one retained fact.
#[derive(Debug, Serialize)]
pub(crate) struct EventLoss {
    pub(crate) reasons: Vec<&'static str>,
    pub(crate) lost_count_lower_bound: Option<u64>,
}

/// One half-open wire interval.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct CoverageSpanDto {
    pub(crate) from_us: i64,
    pub(crate) to_us: i64,
}

/// Per-source publication freshness and independent quality axes.
#[derive(Debug, Serialize)]
pub(crate) struct SourceFreshnessDto {
    pub(crate) source_id: u64,
    pub(crate) source_scope_id: Option<String>,
    pub(crate) data_through_us: Option<i64>,
    pub(crate) source_status: &'static str,
    pub(crate) source_completeness: &'static str,
    pub(crate) retained_exactness: &'static str,
    pub(crate) physical_count_semantics: &'static str,
}

/// Proven loss for one selected source and request range.
#[derive(Debug, Serialize)]
pub(crate) struct SourceLossDto {
    pub(crate) source_id: u64,
    pub(crate) known_gaps: Vec<CoverageSpanDto>,
    pub(crate) dropped_count_lower_bound: Option<u64>,
}

/// Shared metadata of all three timeline responses.
#[derive(Debug, Serialize)]
pub(crate) struct TimelineMetaDto {
    pub(crate) response_schema_version: u32,
    pub(crate) view_generation: u64,
    pub(crate) fact_set_id: String,
    pub(crate) requested_range: CoverageSpanDto,
    pub(crate) effective_range: CoverageSpanDto,
    pub(crate) effective_step_us: Option<u64>,
    pub(crate) sources: Vec<u64>,
    pub(crate) available_sources: Vec<u64>,
    pub(crate) data_through_us: Option<i64>,
    pub(crate) store_data_through_us: Option<i64>,
    pub(crate) tail_pending: Option<u64>,
    pub(crate) source_status: &'static str,
    pub(crate) source_freshness: Vec<SourceFreshnessDto>,
    pub(crate) loss: Vec<SourceLossDto>,
}

/// One SQLSTATE digest entry.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SqlstateCountDto {
    pub(crate) code: String,
    pub(crate) count: u64,
}

/// One joint error-dimension digest entry.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct JointCountDto {
    pub(crate) severity: &'static str,
    pub(crate) category: &'static str,
    pub(crate) sqlstate: Option<String>,
    pub(crate) count: u64,
}

/// One child-signal count.
#[derive(Debug, Serialize)]
pub(crate) struct SignalCountDto {
    pub(crate) signal: i32,
    pub(crate) count: u64,
}

/// Typed lifecycle digest.
#[derive(Debug, Serialize)]
pub(crate) struct LifecycleDigestDto {
    pub(crate) crashes: u64,
    pub(crate) shutdowns: u64,
    pub(crate) ready: u64,
    pub(crate) signals: Vec<SignalCountDto>,
}

/// Checked event-count projection.
#[derive(Debug, Serialize)]
pub(crate) struct EventDigestDto {
    pub(crate) retained_error_occurrence_count: u64,
    pub(crate) retained_error_group_count: u64,
    pub(crate) retained_observation_row_count: u64,
    pub(crate) by_severity: [u64; 5],
    pub(crate) by_category: [u64; 11],
    pub(crate) by_sqlstate: Vec<SqlstateCountDto>,
    pub(crate) sqlstate_missing_count: u64,
    pub(crate) sqlstate_other_count: u64,
    pub(crate) joint_top: Vec<JointCountDto>,
    pub(crate) joint_other_count: u64,
    pub(crate) lifecycle: LifecycleDigestDto,
    pub(crate) exactness: &'static str,
}

/// Bounded overview preview.
#[derive(Debug, Serialize)]
pub(crate) struct NotablePreviewDto {
    pub(crate) observations: Vec<EventFact>,
    pub(crate) omitted_count: u64,
    pub(crate) events_query_hash: String,
}

/// Typed overview response; health policy output stays owned by its policy
/// serializer while the event/count/source contracts are compile-time types.
#[derive(Debug, Serialize)]
pub(crate) struct OverviewResponseDto {
    pub(crate) meta: TimelineMetaDto,
    pub(crate) event_digest: EventDigestDto,
    pub(crate) notable_preview: NotablePreviewDto,
    pub(crate) health_summary: Value,
    pub(crate) coverage: Value,
    pub(crate) retained_coverage_duration_us: u64,
}

/// Typed `/events` response.
#[derive(Debug, Serialize)]
pub(crate) struct EventsResponseDto {
    pub(crate) meta: TimelineMetaDto,
    pub(crate) notable_policy_version: u32,
    pub(crate) events: Vec<EventFact>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) omitted_by_response_filter: u64,
    pub(crate) retained_exactness: &'static str,
    pub(crate) source_completeness: &'static str,
    pub(crate) physical_count_semantics: &'static str,
    pub(crate) coverage: Vec<CoverageSpanDto>,
}

/// Shared `EventFact` projection for preview and paginated responses.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EventFactProjection;

impl EventFactProjection {
    /// Derives the stable page position without allocating wire strings.
    pub(crate) fn position(
        observation: &EventObservation,
        class: NotableClass,
    ) -> Option<EventFactPosition> {
        if NotablePolicy::v1().classify(observation) != Some(class) {
            return None;
        }
        Some(EventFactPosition {
            sort_ts_us: observation.time().sort_ts_us,
            event_id: semantic_event_id(observation)?,
            event_instance_id: event_instance_id(observation),
        })
    }

    /// Projects one classified observation into the shared typed fact.
    pub(crate) fn project(
        observation: &EventObservation,
        class: NotableClass,
        source_id: u64,
    ) -> Option<EventFact> {
        let position = Self::position(observation, class)?;
        let time = observation.time();
        let provenance = observation.provenance();
        Some(EventFact {
            event_id: URL_SAFE_NO_PAD.encode(position.event_id),
            event_instance_id: URL_SAFE_NO_PAD.encode(position.event_instance_id),
            source_id,
            source_scope_id: URL_SAFE_NO_PAD.encode(observation.source_scope_id().0),
            source_type_id: observation.source_type_id(),
            identity_quality: identity_quality_name(observation.identity_quality()),
            sort_ts_us: time.sort_ts_us,
            occurred_at_us: time.occurred_at_us,
            occurrence_count: observation.occurrence_count(),
            event_kind: observation.payload().kind_code(),
            notable_class: class.wire_code(),
            evidence_quality: evidence_quality_name(observation.evidence_quality()),
            quality_flags: observation.quality_flags().0,
            payload: event_payload(observation.payload())?,
            supporting_evidence: [SupportingEvidence {
                observation_id: URL_SAFE_NO_PAD.encode(observation.observation_id().0),
                section_body_id: URL_SAFE_NO_PAD.encode(provenance.section_body_id.0),
                catalog_entry_ordinal: provenance.catalog_entry_ordinal,
                row_ordinal: provenance.row_ordinal,
                dictionary_context_id: URL_SAFE_NO_PAD.encode(provenance.dictionary_context_id.0),
                segment_locator: provenance
                    .segment_locator
                    .map(|locator| URL_SAFE_NO_PAD.encode(locator.0)),
            }],
            loss: observation.loss().map(event_loss),
        })
    }
}

fn semantic_event_id(observation: &EventObservation) -> Option<[u8; 32]> {
    let time = observation.time();
    let mut hasher = Sha256::new();
    hasher.update(b"pgk-overview-event-fact-v1");
    hasher.update(observation.source_scope_id().0);
    hasher.update(observation.source_type_id().to_le_bytes());
    hasher.update(time.sort_ts_us.to_le_bytes());
    match time.occurred_at_us {
        Some(occurred_at_us) => {
            hasher.update([1]);
            hasher.update(occurred_at_us.to_le_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.update(observation.occurrence_count().to_le_bytes());
    let kind = observation.payload().kind_code().as_bytes();
    hasher.update(
        u64::try_from(kind.len())
            .expect("static event-kind length fits u64")
            .to_le_bytes(),
    );
    hasher.update(kind);
    hash_public_payload(&mut hasher, observation.payload())?;
    Some(hasher.finalize().into())
}

fn hash_public_payload(hasher: &mut Sha256, payload: &ObservationPayload) -> Option<()> {
    match payload {
        ObservationPayload::ErrorGroup(error) => {
            hasher.update(severity_name(error.severity).as_bytes());
            hasher.update(category_name(error.category).as_bytes());
            match error.sqlstate {
                Some(code) => {
                    hasher.update([1]);
                    hasher.update(code.0);
                }
                None => hasher.update([0]),
            }
            hasher.update(error.dropped_field_count.0.to_le_bytes());
        }
        ObservationPayload::ChildSignalTermination(lifecycle)
        | ObservationPayload::ChildProcessCrash(lifecycle)
        | ObservationPayload::ShutdownRequested(lifecycle)
        | ObservationPayload::ReadyObserved(lifecycle) => {
            hash_optional_i32(hasher, lifecycle.pid);
            hash_optional_i32(hasher, lifecycle.signal);
            hasher.update(lifecycle.dropped_field_count.0.to_le_bytes());
        }
        _ => return None,
    }
    Some(())
}

fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn event_instance_id(observation: &EventObservation) -> [u8; 32] {
    let provenance = observation.provenance();
    let mut hasher = Sha256::new();
    hasher.update(b"pgk-overview-event-instance-v1");
    hasher.update(observation.observation_id().0);
    hasher.update(provenance.section_body_id.0);
    hasher.update(provenance.catalog_entry_ordinal.to_le_bytes());
    hasher.update(provenance.row_ordinal.to_le_bytes());
    hasher.update(provenance.dictionary_context_id.0);
    match provenance.segment_locator {
        Some(locator) => {
            hasher.update([1]);
            hasher.update(locator.0);
        }
        None => hasher.update([0]),
    }
    hasher.finalize().into()
}

fn event_payload(payload: &ObservationPayload) -> Option<EventPayload> {
    match payload {
        ObservationPayload::ErrorGroup(error) => Some(EventPayload::Error(ErrorPayload {
            kind: payload.kind_code(),
            severity: severity_name(error.severity),
            category: category_name(error.category),
            sqlstate: error.sqlstate.map(|code| sqlstate_text(code.0)),
            dropped_field_count: error.dropped_field_count.0,
        })),
        ObservationPayload::ChildSignalTermination(lifecycle)
        | ObservationPayload::ChildProcessCrash(lifecycle)
        | ObservationPayload::ShutdownRequested(lifecycle)
        | ObservationPayload::ReadyObserved(lifecycle) => {
            Some(EventPayload::Lifecycle(LifecyclePayload {
                kind: payload.kind_code(),
                pid: lifecycle.pid,
                signal: lifecycle.signal,
                dropped_field_count: lifecycle.dropped_field_count.0,
            }))
        }
        _ => None,
    }
}

fn event_loss(loss: &LossSummary) -> EventLoss {
    EventLoss {
        reasons: loss
            .reasons()
            .iter()
            .map(|reason| loss_reason_name(*reason))
            .collect(),
        lost_count_lower_bound: loss.lost_count_lower_bound,
    }
}

pub(crate) const fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Fatal => "fatal",
        Severity::Panic => "panic",
        Severity::Warning => "warning",
        Severity::Log => "log",
    }
}

pub(crate) const fn category_name(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Lock => "lock",
        ErrorCategory::Constraint => "constraint",
        ErrorCategory::Serialization => "serialization",
        ErrorCategory::Timeout => "timeout",
        ErrorCategory::Connection => "connection",
        ErrorCategory::Auth => "auth",
        ErrorCategory::Syntax => "syntax",
        ErrorCategory::Resource => "resource",
        ErrorCategory::DataCorruption => "data_corruption",
        ErrorCategory::System => "system",
        ErrorCategory::Other => "other",
    }
}

const fn identity_quality_name(quality: IdentityQuality) -> &'static str {
    match quality {
        IdentityQuality::SourceExact => "source_exact",
        IdentityQuality::ContentDerived => "content_derived",
        IdentityQuality::Approximate => "approximate",
    }
}

const fn evidence_quality_name(quality: EvidenceQuality) -> &'static str {
    match quality {
        EvidenceQuality::Structured => "structured",
        EvidenceQuality::Parsed => "parsed",
        EvidenceQuality::Heuristic => "heuristic",
        EvidenceQuality::DerivedExact => "derived_exact",
    }
}

const fn loss_reason_name(reason: LossReason) -> &'static str {
    match reason {
        LossReason::GroupCapExceeded => "group_cap_exceeded",
        LossReason::LifecycleCapExceeded => "lifecycle_cap_exceeded",
        LossReason::ParserBound => "parser_bound",
        LossReason::TailerBound => "tailer_bound",
        LossReason::DictionaryBound => "dictionary_bound",
    }
}

pub(crate) fn sqlstate_text(code: [u8; 5]) -> String {
    String::from_utf8_lossy(&code).into_owned()
}
