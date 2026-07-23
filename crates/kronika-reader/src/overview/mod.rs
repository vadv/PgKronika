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
mod factkey;
mod facts;
mod limits;
mod observations;
#[cfg(test)]
mod proptests;
mod publish;

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
    dictionary_context_id, lineage_from_catalog, section_body_id, source_scope_id,
};
pub use dictionary::{
    ResolvedPattern, TargetedDictionaryRead, TargetedDictionaryStats, resolve_targeted,
};
pub use factkey::{FactKey, FileKind, placement, placement_dir};
pub use facts::{BuildError, SegmentContext, SegmentContextError, SegmentFacts, SourceError};
pub use limits::{Bounds, LIMIT};
pub use observations::EventObservationsBlock;
pub use publish::{CacheRebuildReason, FactLoad, FactOrigin, FactStore, PersistError};
