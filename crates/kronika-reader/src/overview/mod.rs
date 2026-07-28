//! Bounded `PGKOVF` codec and positional reads for overview facts.
//!
//! This module defines typed logical blocks, pre-allocation safety limits,
//! catalog-derived identities, and selective reads. It does not interpret
//! factor semantics.
//!
//! # Layering
//!
//! Logical fact types come from [`kronika_analytics::overview`]. This module
//! carries them across disk without reinterpreting them: a decoded
//! [`kronika_analytics::overview::CounterSample`] equals the one that was
//! encoded. The CRC32C primitive is reused from [`kronika_format::crc32c`].

mod block;
mod bytes;
mod container;
mod descriptors;
mod dictionary;
mod event_extract;
mod event_facts;
mod factkey;
mod facts;
mod fallback;
mod gc;
mod limits;
mod live;
mod metric_extract;
mod observations;
mod persist_mode;
#[cfg(test)]
mod proptests;
mod publish;
#[cfg(any(test, feature = "qualification"))]
mod qualification_fixture;
mod web_index;

pub use block::{
    BlockCodec, BlockError, BlockFlags, BlockKind, CounterSamplesBlock, EntityStateRecord,
    EntityStatesBlock, GaugeSamplesBlock, LossCoverageBlock, ResetMarker, ResetMarkersBlock,
    SourceManifestBlock, StringTableBlock,
};
pub use container::{
    BlockContent, BlockDirectoryEntry, CacheReadError, FactFile, FactFileHeader, FactFileReader,
    FactReadStats, HeaderIdentity,
};
pub use descriptors::{
    CatalogEntryDescriptor, DictionaryContextEntry, ManifestEntryDescriptor, SourceDescriptor,
    dictionary_context_id, lineage_from_catalog, section_body_id,
};
pub use dictionary::{
    ResolvedPattern, TargetedDictionaryRead, TargetedDictionaryStats, resolve_targeted,
};
pub use event_facts::EventFactsBlock;
pub use factkey::{FactBuildKey, FactKey, FileKind};
pub use facts::{BuildError, SegmentContext, SegmentFacts, SourceError};
pub use fallback::{
    DEFAULT_FALLBACK_BYTES, DEFAULT_FALLBACK_SEGMENT_HOURS, FallbackConfig, FallbackConfigError,
    FallbackStats, MAX_FALLBACK_BYTES, MAX_FALLBACK_SEGMENT_HOURS,
};
pub use gc::{GcCategoryUsage, GcConfig, GcConfigError, GcMark, GcOutcome, GcSkipReason, GcUsage};
pub use limits::{Bounds, LIMIT};
pub use live::{
    FoldEffect, LiveBuilder, LiveConfigError, LiveFoldError, LiveState, LiveView, SealOutcome,
    reconcile_seal,
};
pub use observations::EventObservationsBlock;
pub use persist_mode::{PersistMode, PersistModeSnapshot};
pub use publish::{
    CacheRebuildReason, FactLoad, FactOrigin, FactStore, PersistError, PersistFailureClass,
    PersistenceProbeOutcome,
};
#[cfg(feature = "qualification")]
pub use publish::{
    QUALIFICATION_PUBLISH_BARRIER_ENV, QUALIFICATION_PUBLISH_BARRIER_READY,
    QUALIFICATION_PUBLISH_BARRIER_RELEASE, QUALIFICATION_PUBLISH_FAULT_ENV,
};
pub use web_index::{
    EntityDictionaryEntry, EntityMetric, EntitySeries, EntitySeriesBlock, IndexStatus,
    METRIC_FLAG_CANONICAL, MetricAggregation, MetricStatus, TimeGrid, UiSummaryBlock, ViewSummary,
};

#[cfg(feature = "qualification")]
pub(crate) fn qualification_all_family_pgm() -> Vec<u8> {
    qualification_fixture::all_family_fixture().sealed_bytes()
}
