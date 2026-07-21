//! Transport-free incident domain core.
//!
//! Reader preparation and HTTP/JSON adapters live outside this module.

mod active;
mod cluster;
mod dispatch;
mod engine;
mod events;
mod evidence;
mod gauge_contracts;
mod lens;
mod lenses;
mod model;
mod os_lenses;
mod query_plan;
mod series;
mod typed;

// Input preparation and consumers live outside this module (`incident_input`),
// so the reader-facing input surface and the engine entry point are exported.
pub(crate) use model::{EnrichedEpisode, EpisodeRefV1, IdentityValue};
pub(crate) use series::{Series, SeriesError, SeriesInsertError, SeriesSet};
pub(crate) use typed::{
    ActivityBackend, ActivitySnapshot, GaugeQuality, GaugeTrackInput, LockEdge, LockSnapshot,
    PlanFork, PlanSample, ProcessCgroupSample, SnapshotCompleteness, TypedInputs,
};

pub(crate) use active::{active_catalog, active_catalog_ids};
pub(crate) use dispatch::LimitAxis;
pub(crate) use engine::{
    AnalyzeError, ClockRelation, EngineOutcome, EngineSkip, Incident, IncidentConfig, analyze,
};
pub(crate) use events::{
    EventConfig, EventError, EventInputLimits, EventLens, EventOutcome, LifecycleEvent,
    LifecycleKind, LogCoverage, LogErrorGroup, LogEventInputs, evaluate_events, event_catalog,
    event_catalog_ids, event_catalog_metadata,
};
pub(crate) use evidence::{
    CounterEvidence, Evidence, Finding, GaugeEvidence, GaugeMeasurement, SampledLockEdge,
    SourceWindow,
};
#[cfg(test)]
pub(crate) use evidence::{
    CounterEvidenceInput, CounterEvidenceWindow, CounterEvidenceWindowInput,
    CounterMeasurementKind, CounterOperand, CounterOperandPurpose, DirectEvidence, GaugeEntity,
    GaugeUnit, GaugeValueInput, LockParticipant, ThresholdKind,
};
#[allow(
    unused_imports,
    reason = "engine tests use Lens while the HTTP endpoint exposes clustering only"
)]
pub(crate) use lens::Lens;
#[cfg(test)]
pub(crate) use lenses::dormant_catalog;
pub(crate) use lenses::{DormantLens, core_catalog};
#[cfg(test)]
pub(crate) use lenses::{
    MAX_CATALOG_TEXT_BYTES, MAX_CATALOG_TOKEN_BYTES, MAX_DORMANT_LENSES, MAX_MISSING_PER_LENS,
};
