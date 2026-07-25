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
    EXTRACTOR_SEMANTICS_VERSION, FACT_SCHEMA_VERSION, REGISTRY_CONTRACT_VERSION, SegmentLineageId,
    SourceScopeId,
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

    /// Parses the canonical lowercase hexadecimal encoding.
    #[must_use]
    pub fn from_hex(value: &str) -> Option<Self> {
        parse_hex_32(value).map(Self)
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

/// Complete immutable identity of one segment-fact build.
///
/// Content-identical retained occurrences remain distinct because their
/// lineage IDs differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactBuildKey {
    fact_key: FactKey,
    segment_lineage_id: SegmentLineageId,
}

impl FactBuildKey {
    /// Binds a content-addressed key to one retained segment occurrence.
    #[must_use]
    pub const fn new(fact_key: FactKey, segment_lineage_id: SegmentLineageId) -> Self {
        Self {
            fact_key,
            segment_lineage_id,
        }
    }

    /// Content-addressed fact identity.
    #[must_use]
    pub const fn fact_key(self) -> FactKey {
        self.fact_key
    }

    /// Retained segment occurrence.
    #[must_use]
    pub const fn segment_lineage_id(self) -> SegmentLineageId {
        self.segment_lineage_id
    }

    /// Canonical committed filename.
    #[must_use]
    pub fn final_name(self) -> String {
        format!(
            "{}-{}.ovf",
            self.fact_key.hex(),
            to_hex(&self.segment_lineage_id.0)
        )
    }

    /// Parses an exact canonical committed filename.
    #[must_use]
    pub fn from_final_name(value: &str) -> Option<Self> {
        let stem = value.strip_suffix(".ovf")?;
        let (key, lineage) = stem.split_once('-')?;
        if stem.matches('-').count() != 1 {
            return None;
        }
        Some(Self::new(
            FactKey::from_hex(key)?,
            SegmentLineageId(parse_hex_32(lineage)?),
        ))
    }
}

/// The expected path for the fact file identified by `key` and `lineage`.
///
/// Layout:
/// `<cache_root>/overview/v1/<scope_hex>/<prefix>/<key_hex>-<lineage_hex>.ovf`.
///
/// `FactKey` remains content-addressed. The lineage suffix distinguishes
/// separate retained occurrences with identical PGM content.
#[must_use]
pub fn placement(
    cache_root: &Path,
    source_scope_id: SourceScopeId,
    key: &FactKey,
    lineage: SegmentLineageId,
) -> PathBuf {
    cache_root
        .join("overview")
        .join("v1")
        .join(to_hex(&source_scope_id.0))
        .join(key.prefix())
        .join(FactBuildKey::new(*key, lineage).final_name())
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

pub(super) fn parse_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = lowercase_hex_nibble(pair[0])?;
        let low = lowercase_hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

const fn lowercase_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
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
    fn placement_uses_scope_prefix_key_and_lineage_name() {
        let key = FactKey::for_current_segment(scope(0xAB), descriptor(0xCD));
        let lineage = SegmentLineageId([0xEF; 32]);
        let path = placement(Path::new("/cache"), scope(0xAB), &key, lineage);
        let text = path.to_string_lossy();
        assert!(text.starts_with("/cache/overview/v1/"));
        assert!(text.ends_with(&format!("/{}-{}.ovf", key.hex(), to_hex(&lineage.0))));
        assert!(text.contains(&format!("/{}/", key.prefix())));
        assert_eq!(
            placement_dir(Path::new("/cache"), scope(0xAB), &key),
            path.parent().expect("named file has a parent")
        );
    }

    #[test]
    fn placement_distinguishes_identical_content_under_distinct_lineages() {
        let key = FactKey::for_current_segment(scope(1), descriptor(2));
        let first = placement(
            Path::new("/cache"),
            scope(1),
            &key,
            SegmentLineageId([3; 32]),
        );
        let second = placement(
            Path::new("/cache"),
            scope(1),
            &key,
            SegmentLineageId([4; 32]),
        );
        assert_ne!(first, second);
        assert_eq!(first.parent(), second.parent());
    }

    #[test]
    fn prefix_is_first_key_byte() {
        let key = FactKey::for_current_segment(scope(3), descriptor(4));
        assert_eq!(key.prefix(), format!("{:02x}", key.as_bytes()[0]));
        assert_eq!(key.hex().len(), 64);
    }

    #[test]
    fn build_key_round_trips_only_canonical_final_names() {
        let key = FactKey::for_current_segment(scope(0xAB), descriptor(0xCD));
        let build = FactBuildKey::new(key, SegmentLineageId([0xEF; 32]));
        let name = build.final_name();
        assert_eq!(FactBuildKey::from_final_name(&name), Some(build));
        assert!(FactBuildKey::from_final_name(&name.to_uppercase()).is_none());
        assert!(FactBuildKey::from_final_name("abcd-1111.ovf").is_none());
        assert!(FactBuildKey::from_final_name(&format!("{name}.tmp")).is_none());
    }
}
