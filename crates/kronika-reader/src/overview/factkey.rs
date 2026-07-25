//! Logical identities for overview facts and in-process builds.
//!
//! A [`FactKey`] binds retained facts to the source ID, exact PGM descriptor,
//! file kind, and contract versions that determine the logical fact bytes.
//! It is stored and validated inside the sibling `.ovf`; it never determines
//! the sidecar filename or directory layout.

use kronika_analytics::overview::{
    EXTRACTOR_SEMANTICS_VERSION, FACT_SCHEMA_VERSION, REGISTRY_CONTRACT_VERSION, SegmentLineageId,
};
use sha2::{Digest, Sha256};

use super::container::HeaderIdentity;
use super::descriptors::SourceDescriptor;

const FACT_KEY_TAG: &[u8] = b"pgk-overview-fact-key-v2";

/// The logical kind of an overview fact file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Per-segment sealed facts.
    SegmentFacts,
}

impl FileKind {
    /// The on-disk file-kind code shared with the fact-file header.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::SegmentFacts => 1,
        }
    }
}

/// The logical identity stored in one overview sidecar header.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKey([u8; 32]);

impl std::fmt::Debug for FactKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FactKey(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

impl FactKey {
    /// Derives a key from source provenance, file kind, and contract versions.
    #[must_use]
    pub fn derive(
        pgm_source_id: u64,
        source_descriptor: SourceDescriptor,
        file_kind: FileKind,
        fact_schema_version: u32,
        extractor_semantics_version: u32,
        registry_contract_version: u32,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(FACT_KEY_TAG);
        hasher.update(pgm_source_id.to_le_bytes());
        hasher.update(source_descriptor.0);
        hasher.update(file_kind.code().to_le_bytes());
        hasher.update(fact_schema_version.to_le_bytes());
        hasher.update(extractor_semantics_version.to_le_bytes());
        hasher.update(registry_contract_version.to_le_bytes());
        Self(hasher.finalize().into())
    }

    /// Derives the key represented by a decoded header.
    #[must_use]
    pub fn for_identity(identity: &HeaderIdentity, file_kind: FileKind) -> Self {
        Self::derive(
            identity.pgm_source_id,
            identity.source_descriptor,
            file_kind,
            identity.fact_schema_version,
            identity.extractor_semantics_version,
            identity.registry_contract_version,
        )
    }

    /// Derives the current `SegmentFacts` key for one exact PGM source.
    #[must_use]
    pub fn for_current_segment(pgm_source_id: u64, source_descriptor: SourceDescriptor) -> Self {
        Self::derive(
            pgm_source_id,
            source_descriptor,
            FileKind::SegmentFacts,
            FACT_SCHEMA_VERSION,
            EXTRACTOR_SEMANTICS_VERSION,
            REGISTRY_CONTRACT_VERSION,
        )
    }

    /// The raw 32-byte key.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Complete immutable identity of one segment-fact build.
///
/// The lineage distinguishes retained segment occurrences for singleflight,
/// admission, and validation. It does not affect the sibling sidecar path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactBuildKey {
    fact_key: FactKey,
    segment_lineage_id: SegmentLineageId,
}

impl FactBuildKey {
    /// Binds a logical fact key to one retained segment occurrence.
    #[must_use]
    pub const fn new(fact_key: FactKey, segment_lineage_id: SegmentLineageId) -> Self {
        Self {
            fact_key,
            segment_lineage_id,
        }
    }

    /// Logical fact identity.
    #[must_use]
    pub const fn fact_key(self) -> FactKey {
        self.fact_key
    }

    /// Retained segment occurrence.
    #[must_use]
    pub const fn segment_lineage_id(self) -> SegmentLineageId {
        self.segment_lineage_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(byte: u8) -> SourceDescriptor {
        SourceDescriptor([byte; 32])
    }

    #[test]
    fn key_is_stable_for_identical_inputs() {
        let left = FactKey::derive(1, descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        let right = FactKey::derive(1, descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        assert_eq!(left, right);
    }

    #[test]
    fn source_descriptor_and_each_contract_axis_change_the_key() {
        let base = FactKey::derive(1, descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        assert_ne!(
            base,
            FactKey::derive(2, descriptor(2), FileKind::SegmentFacts, 1, 1, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(1, descriptor(9), FileKind::SegmentFacts, 1, 1, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(1, descriptor(2), FileKind::SegmentFacts, 2, 1, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(1, descriptor(2), FileKind::SegmentFacts, 1, 2, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(1, descriptor(2), FileKind::SegmentFacts, 1, 1, 2)
        );
    }
}
