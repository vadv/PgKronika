//! Formula-neutral contracts and bounded reductions for a timeline overview.
//!
//! The module defines retained observations, reductions, coverage, health
//! evaluation, and an adapter contract for semantic comparisons. It does not
//! implement a PGM reader, persistent index, HTTP endpoint, or response cache.
//!
//! Counts use checked arithmetic. Missing samples, unsupported factors, and
//! measured zero remain distinct.
//!
//! # Version axes
//!
//! Each constant reserves an independent compatibility axis for later storage
//! and API adapters. This crate does not yet write fact files, caches, cursors,
//! or responses.

pub mod counts;
pub mod coverage;
mod finite;
pub mod health;
pub mod observation;
pub mod oracle;
pub mod reduce;
mod sha256;

pub use counts::{
    CountError, CountLimits, CountOverflow, CountResource, ErrorCategory, EventCounts,
    JointErrorKey, LifecycleCounts, Severity, SqlState,
};
pub use coverage::{
    Applicability, BoundaryQuality, Coverage, CoverageSpan, CoverageState, PeriodQuality,
    PhysicalCountSemantics, RetainedExactness, SourceCompleteness,
};
pub use finite::FiniteF64;
pub use health::{
    CadenceEpochId, DomainId, DomainPenalty, DownsampleError, DownsampledHealthPoint,
    FactorCoverage, FactorId, FactorPenalty, FactorSetId, FloorClass, FloorEvidence,
    HealthEvaluationError, HealthLimits, HealthPoint, HealthPolicy, HealthResource, HealthState,
    InvalidFactorProfile, InvalidHealthPolicy, PopulationTotalQuality, ProfileTopologyId,
    RequiredFactorProfile, SourcePopulation, downsample_worst,
};
pub use observation::{
    CheckpointPayload, DictionaryContextId, DroppedFieldCount, ErrorGroupPayload, EventObservation,
    EvidenceQuality, FactId, IdentityQuality, InvalidObservation, LifecyclePayload,
    LockWaitPayload, LogGapPayload, LossReason, LossSummary, MaintenancePayload, NamingContractId,
    ObservationId, ObservationPayload, ObservationProvenance, ObservationShape, ObservationTime,
    QualityFlags, SectionBodyId, SegmentIdentity, SegmentLineageId, SegmentLocator,
    SlowQueryPayload, SourceLocator, SourceScopeId, TempFilePayload, TimeQuality,
};
pub use oracle::{
    MemoryOracle, OracleError, OracleLimits, OracleResource, OracleResult, OracleSourceError,
    RawOracle, SemanticDivergence, fold_counts, observation_in_range, semantic_divergences,
};
pub use reduce::{
    AlignmentId, CounterInterval, CounterReduction, CounterSample, GaugeReduction, GaugeSample,
    HoldModel, MetricSeriesId, PairQuality, RatioReduction, ReductionError, ReductionLimits,
    TimeWeightedReduction, classify_series, time_weighted_mean,
};

/// Reserved version for future fact-container framing.
pub const CONTAINER_VERSION: u16 = 1;

/// Reserved version for the future canonical fact schema.
pub const FACT_SCHEMA_VERSION: u32 = 1;

/// Reserved version for PGM-to-fact extraction and normalization.
pub const EXTRACTOR_SEMANTICS_VERSION: u32 = 1;

/// Counter/gauge reduction, alignment, and bucket-attribution semantics.
///
/// Used by current health factor-set identity.
pub const REDUCTION_SEMANTICS_VERSION: u32 = 1;

/// Version of supported PGM contracts used by factor-set identity.
pub const REGISTRY_CONTRACT_VERSION: u32 = 1;

/// Reserved default version for health policy configuration.
pub const HEALTH_POLICY_VERSION: u32 = 1;

/// Reserved version for future notable-event policy.
pub const NOTABLE_POLICY_VERSION: u32 = 1;

/// Reserved version for future response redaction policy.
pub const REDACTION_POLICY_VERSION: u32 = 1;

/// Reserved version for future incident diagnosis policy.
pub const DIAGNOSIS_POLICY_VERSION: u32 = 1;

/// Reserved version for a future response schema.
pub const RESPONSE_SCHEMA_VERSION: u32 = 1;

/// Reserved version for future cursor encoding.
pub const CURSOR_VERSION: u16 = 1;
