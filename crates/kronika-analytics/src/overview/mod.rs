//! Formula-neutral facts and honesty-preserving algebra for the timeline
//! overview.
//!
//! This module is the pure core of the overview index: the fact and event
//! types, the reductions that merge them, and the health evaluation. It knows
//! nothing about `Catalog`, `Part`, `Row`, `StrId`, the filesystem, Parquet, or
//! HTTP — a reader adapter extracts facts from PGM and a web adapter bounds and
//! serves them.
//!
//! Two rules shape every type here. Merges are exact: counts add with checked
//! arithmetic and saturate never, so a partition into parts and segments cannot
//! change a total (a silent overflow would forge a smaller number). Absence is
//! not zero: a missing sample, an unsupported factor, and a measured zero are
//! distinct, so a gap never becomes a healthy score.
//!
//! # Version axes
//!
//! Each constant below guards a distinct compatibility surface, and they move
//! independently. A stored fact file is keyed by the fact/extractor/registry
//! versions; the health and notable versions key only projections, so
//! re-scoring a range never rebuilds facts.

pub mod counts;
pub mod coverage;
pub mod health;
pub mod observation;
pub mod oracle;
pub mod reduce;
mod sha256;

pub use counts::{CountOverflow, ErrorCategory, EventCounts, LifecycleCounts, Severity, SqlState};
pub use coverage::{Applicability, Coverage, CoverageSpan, CoverageState};
pub use health::{
    DomainId, DomainPenalty, Exactness, FactorCoverage, FactorId, FactorPenalty, FactorSetId,
    FloorClass, FloorEvidence, HealthPoint, HealthPolicy, HealthState, RequiredFactorProfile,
    SourcePopulation, downsample_worst,
};
pub use observation::{
    DictionaryContextId, DroppedFields, ErrorGroupPayload, EventObservation, EvidenceQuality,
    FactId, IdentityQuality, InvalidObservation, LossReason, LossSummary, ObservationId,
    ObservationPayload, ObservationProvenance, ObservationShape, ObservationTime, QualityFlags,
    SectionBodyId, SegmentLineageId, SourceLocator, SourceScopeId, TimeQuality,
};
pub use oracle::{
    MemoryOracle, RawOracle, SemanticDivergence, fold_counts, observation_in_range,
    semantic_divergences,
};
pub use reduce::{
    CounterInterval, CounterReduction, CounterSample, GaugeReduction, GaugeSample, HoldModel,
    PairQuality, RatioReduction, classify_series, time_weighted_mean,
};

/// Physical container framing: header, directory, and block layout.
///
/// Bumped only when the on-disk fact file structure changes, independent of the
/// facts it carries.
pub const CONTAINER_VERSION: u16 = 1;

/// Logical shape of canonical facts and their fields.
///
/// A bump invalidates stored fact files: the decoder cannot trust an older
/// logical layout.
pub const FACT_SCHEMA_VERSION: u32 = 1;

/// PGM-to-facts mapping, normalization, and reducer/reset semantics.
///
/// A bump invalidates stored fact files even when their shape is unchanged: the
/// same bytes would now be extracted or reduced differently.
pub const EXTRACTOR_SEMANTICS_VERSION: u32 = 1;

/// Supported PGM types, layouts, and required inputs.
///
/// A bump invalidates stored fact files whose source contract the current
/// registry no longer matches.
pub const REGISTRY_CONTRACT_VERSION: u32 = 1;

/// Factor set, penalty curves, domains, floors, and required profile.
///
/// A bump invalidates only health projections and responses, never stored
/// facts.
pub const HEALTH_POLICY_VERSION: u32 = 1;

/// Notable selection, dedup, ranking, and caps.
///
/// A bump invalidates only event projections and responses, never stored facts.
pub const NOTABLE_POLICY_VERSION: u32 = 1;

/// Correlation and cause model for incident diagnosis.
///
/// A bump invalidates only diagnosis output, never stored facts.
pub const DIAGNOSIS_POLICY_VERSION: u32 = 1;

/// JSON/wire response shape.
///
/// A bump invalidates only the serialized response cache.
pub const RESPONSE_SCHEMA_VERSION: u32 = 1;

/// Cursor encoding and validation.
///
/// A bump invalidates only outstanding cursors.
pub const CURSOR_VERSION: u16 = 1;
