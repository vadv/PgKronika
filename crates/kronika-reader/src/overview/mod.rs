//! Physical fact-file format and selective extraction for the timeline overview.
//!
//! This module owns the reader side of the overview index: the bounded
//! `PGKOVF` container codec (header, block directory, and typed logical
//! blocks), the safety bounds that guard every untrusted length before it
//! reaches an allocation, durable publication, and the catalog-derived
//! descriptors that name a segment's facts.
//!
//! Formula, notable, and HTTP semantics do not live here. The codec stores
//! and validates content-addressed identities as opaque bytes; it never
//! decides what a fact means.
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
    BlockContent, BlockDirectoryEntry, CacheReadError, FactFile, FactFileHeader, HeaderIdentity,
};
pub use descriptors::{
    CatalogEntryDescriptor, DescriptorGap, FactKeyPreimage, SegmentContentDescriptor,
    SourceScopePreimage, lineage_from_catalog,
};
pub use dictionary::{ResolvedPattern, resolve_targeted};
pub use limits::{Bounds, LIMIT};
pub use observations::EventObservationsBlock;
pub use publish::{PersistError, PublishOutcome, publish};
