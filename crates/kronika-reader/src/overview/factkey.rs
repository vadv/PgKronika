//! Content-addressed identity and on-disk placement for overview fact files.
//!
//! A [`FactKey`] binds a fact file to its source scope, the content descriptor
//! of the PGM it was built from, the file kind, and the three version axes that
//! change the logical fact bytes. Health, notable, and response versions are
//! deliberately excluded: they do not change retained facts, so they never
//! invalidate a cached file.
//!
//! [`placement`] maps a key to a path under the cache root. The reader does not
//! infer identity from the path; it validates the file header against the
//! expected identity.

use std::path::{Path, PathBuf};

use kronika_analytics::overview::{
    EXTRACTOR_SEMANTICS_VERSION, FACT_SCHEMA_VERSION, REGISTRY_CONTRACT_VERSION, SourceScopeId,
};
use sha2::{Digest, Sha256};

use super::container::HeaderIdentity;
use super::descriptors::SourceDescriptor;

/// Domain separator for the overview fact-key hash.
const FACT_KEY_TAG: &[u8] = b"pgk-overview-fact-key-v1";

/// The logical kind of a fact file under the overview cache namespace.
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

/// The content-addressed identity of one overview fact file.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKey([u8; 32]);

impl std::fmt::Debug for FactKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FactKey({})", self.hex())
    }
}

impl FactKey {
    /// Derives a key from a scope, PGM descriptor, kind, and version axes.
    #[must_use]
    pub fn derive(
        source_scope_id: SourceScopeId,
        source_descriptor: SourceDescriptor,
        file_kind: FileKind,
        fact_schema_version: u32,
        extractor_semantics_version: u32,
        registry_contract_version: u32,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(FACT_KEY_TAG);
        hasher.update(source_scope_id.0);
        hasher.update(source_descriptor.0);
        hasher.update(file_kind.code().to_le_bytes());
        hasher.update(fact_schema_version.to_le_bytes());
        hasher.update(extractor_semantics_version.to_le_bytes());
        hasher.update(registry_contract_version.to_le_bytes());
        Self(hasher.finalize().into())
    }

    /// Derives the key of a fact file that carries `identity`.
    ///
    /// The version axes come from the header, so a file only matches a lookup
    /// when its logical fact contract equals the reader's current contract.
    #[must_use]
    pub fn for_identity(identity: &HeaderIdentity, file_kind: FileKind) -> Self {
        Self::derive(
            identity.source_scope_id,
            identity.source_descriptor,
            file_kind,
            identity.fact_schema_version,
            identity.extractor_semantics_version,
            identity.registry_contract_version,
        )
    }

    /// Derives the key for a `SegmentFacts` file under the current contract.
    #[must_use]
    pub fn for_current_segment(
        source_scope_id: SourceScopeId,
        source_descriptor: SourceDescriptor,
    ) -> Self {
        Self::derive(
            source_scope_id,
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

    /// The lowercase hex encoding of the key.
    #[must_use]
    pub fn hex(&self) -> String {
        to_hex(&self.0)
    }

    /// The two-hex-character directory prefix that bounds the fan-out.
    #[must_use]
    pub fn prefix(&self) -> String {
        format!("{:02x}", self.0[0])
    }
}

/// The expected path for the fact file identified by `key` under `cache_root`.
///
/// Layout: `<cache_root>/overview/v1/<scope_hex>/<prefix>/<key_hex>.ovf`.
#[must_use]
pub fn placement(cache_root: &Path, source_scope_id: SourceScopeId, key: &FactKey) -> PathBuf {
    cache_root
        .join("overview")
        .join("v1")
        .join(to_hex(&source_scope_id.0))
        .join(key.prefix())
        .join(format!("{}.ovf", key.hex()))
}

/// The prefix directory containing `key`'s fact file and publication artifacts.
///
/// Temporary files are created here so publication can use a same-filesystem
/// rename.
#[must_use]
pub fn placement_dir(cache_root: &Path, source_scope_id: SourceScopeId, key: &FactKey) -> PathBuf {
    cache_root
        .join("overview")
        .join("v1")
        .join(to_hex(&source_scope_id.0))
        .join(key.prefix())
}

/// Lowercase hex without external dependencies.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String never fails.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(byte: u8) -> SourceScopeId {
        SourceScopeId([byte; 32])
    }

    fn descriptor(byte: u8) -> SourceDescriptor {
        SourceDescriptor([byte; 32])
    }

    #[test]
    fn key_is_stable_for_identical_inputs() {
        let left = FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        let right = FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        assert_eq!(left, right);
    }

    #[test]
    fn each_contract_version_axis_changes_the_key() {
        let base = FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        assert_ne!(
            base,
            FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 2, 1, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 1, 2, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 1, 1, 2)
        );
    }

    #[test]
    fn fact_key_binds_scope_and_descriptor() {
        let base = FactKey::derive(scope(1), descriptor(2), FileKind::SegmentFacts, 1, 1, 1);
        assert_ne!(
            base,
            FactKey::derive(scope(9), descriptor(2), FileKind::SegmentFacts, 1, 1, 1)
        );
        assert_ne!(
            base,
            FactKey::derive(scope(1), descriptor(9), FileKind::SegmentFacts, 1, 1, 1)
        );
    }

    #[test]
    fn placement_uses_scope_prefix_and_key_name() {
        let key = FactKey::for_current_segment(scope(0xAB), descriptor(0xCD));
        let path = placement(Path::new("/cache"), scope(0xAB), &key);
        let text = path.to_string_lossy();
        assert!(text.starts_with("/cache/overview/v1/"));
        assert!(text.ends_with(&format!("/{}.ovf", key.hex())));
        assert!(text.contains(&format!("/{}/", key.prefix())));
        assert_eq!(
            placement_dir(Path::new("/cache"), scope(0xAB), &key),
            path.parent().expect("named file has a parent")
        );
    }

    #[test]
    fn prefix_is_first_key_byte() {
        let key = FactKey::for_current_segment(scope(3), descriptor(4));
        assert_eq!(key.prefix(), format!("{:02x}", key.as_bytes()[0]));
        assert_eq!(key.hex().len(), 64);
    }
}
