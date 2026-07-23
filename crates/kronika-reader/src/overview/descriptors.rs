//! Content-derived identities for overview facts.
//!
//! Namespace, section-body, and dictionary-value hashes include explicit
//! length prefixes. Catalog entry descriptors omit offsets so resealing a
//! verbatim section does not change its lineage.

use kronika_analytics::overview::{
    DictionaryContextId, NamingContractId, SectionBodyId, SegmentIdentity, SegmentLineageId,
    SegmentLocator, SourceScopeId,
};
use kronika_format::{Catalog, Entry};
use sha2::{Digest, Sha256};

const SOURCE_SCOPE_TAG: &[u8] = b"pgk-overview-source-scope-v1";
const CATALOG_DESCRIPTOR_TAG: &[u8] = b"pgk-pgm-catalog-descriptor-v1";
const SECTION_BODY_TAG: &[u8] = b"pgk-overview-section-body-v1";
const DICTIONARY_CONTEXT_TAG: &[u8] = b"pgk-overview-dictionary-context-v1";

/// The offset-independent descriptor of one catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntryDescriptor {
    /// Registry type and layout identifier.
    pub type_id: u32,
    /// Catalog flags.
    pub flags: u32,
    /// Section body length in bytes.
    pub body_len: u64,
    /// Row count declared by the section.
    pub rows: u32,
    /// CRC32C of the section body.
    pub body_crc32c: u32,
}

impl CatalogEntryDescriptor {
    /// Copies the offset-independent fields of a catalog entry.
    #[must_use]
    pub const fn of(entry: &Entry) -> Self {
        Self {
            type_id: entry.type_id,
            flags: entry.flags,
            body_len: entry.len,
            rows: entry.rows,
            body_crc32c: entry.crc32c,
        }
    }

    /// Encodes the descriptor in its canonical 24-byte representation.
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 24] {
        let mut out = [0_u8; 24];
        out[0..4].copy_from_slice(&self.type_id.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.body_len.to_le_bytes());
        out[16..20].copy_from_slice(&self.rows.to_le_bytes());
        out[20..24].copy_from_slice(&self.body_crc32c.to_le_bytes());
        out
    }
}

/// Catalog metadata with an optional verified body identity.
///
/// The manifest records every catalog entry without reading unrelated bodies.
/// A CRC-verified body adds its SHA-256 identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestEntryDescriptor {
    /// Offset-independent catalog fields.
    pub catalog: CatalogEntryDescriptor,
    /// SHA-256 identity when the section body was read and verified.
    pub section_body_id: Option<SectionBodyId>,
}

impl ManifestEntryDescriptor {
    /// Copies catalog metadata without claiming that the body was read.
    #[must_use]
    pub const fn from_catalog(entry: &Entry) -> Self {
        Self {
            catalog: CatalogEntryDescriptor::of(entry),
            section_body_id: None,
        }
    }

    /// Builds an entry after the caller has CRC-verified `body`.
    #[must_use]
    pub fn from_verified(entry: &Entry, body: &[u8]) -> Self {
        Self {
            catalog: CatalogEntryDescriptor::of(entry),
            section_body_id: Some(section_body_id(entry.type_id, body)),
        }
    }
}

/// SHA-256 descriptor of the exact PGM tail index and catalog.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceDescriptor(pub [u8; 32]);

impl std::fmt::Debug for SourceDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceDescriptor(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

impl SourceDescriptor {
    /// Derives a descriptor from the exact bytes read while opening a PGM.
    #[must_use]
    pub fn derive(source_file_len: u64, tail_index_bytes: &[u8], raw_catalog_bytes: &[u8]) -> Self {
        Self(hash_parts(&[
            CATALOG_DESCRIPTOR_TAG,
            &source_file_len.to_le_bytes(),
            tail_index_bytes,
            raw_catalog_bytes,
        ]))
    }
}

/// One resolved dictionary value included in an observation identity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DictionaryContextEntry<'a> {
    /// Dictionary `str_id`.
    pub str_id: u64,
    /// Stored value bytes.
    pub bytes: &'a [u8],
    /// Full source length for a possibly truncated blob.
    pub full_len: u64,
    /// Whether `bytes` is a retained prefix.
    pub truncated: bool,
}

impl std::fmt::Debug for DictionaryContextEntry<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictionaryContextEntry")
            .field("str_id", &self.str_id)
            .field("stored_len", &self.bytes.len())
            .field("full_len", &self.full_len)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Derives a source scope from a stable namespace and the PGM source ID.
#[must_use]
pub fn source_scope_id(normalized_store_namespace: &[u8], pgm_source_id: u64) -> SourceScopeId {
    SourceScopeId(hash_parts(&[
        SOURCE_SCOPE_TAG,
        &length_prefix(normalized_store_namespace),
        normalized_store_namespace,
        &pgm_source_id.to_le_bytes(),
    ]))
}

/// Derives the identity of an exact section body.
#[must_use]
pub fn section_body_id(type_id: u32, body: &[u8]) -> SectionBodyId {
    SectionBodyId(hash_parts(&[
        SECTION_BODY_TAG,
        &type_id.to_le_bytes(),
        &length_prefix(body),
        body,
    ]))
}

/// Derives the identity of canonically ordered dictionary values for one row.
///
/// Returns `None` unless `str_id`s are strictly increasing and each stored
/// length matches its truncation flag and declared full length.
#[must_use]
pub fn dictionary_context_id(
    entries: &[DictionaryContextEntry<'_>],
) -> Option<DictionaryContextId> {
    if entries
        .windows(2)
        .any(|pair| pair[0].str_id >= pair[1].str_id)
        || entries.iter().any(|entry| {
            let stored_len = entry.bytes.len() as u64;
            stored_len > entry.full_len
                || (!entry.truncated && stored_len != entry.full_len)
                || (entry.truncated && stored_len == entry.full_len)
        })
    {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(DICTIONARY_CONTEXT_TAG);
    hasher.update((entries.len() as u64).to_le_bytes());
    for entry in entries {
        hasher.update(entry.str_id.to_le_bytes());
        hasher.update(length_prefix(entry.bytes));
        hasher.update(entry.bytes);
        hasher.update(entry.full_len.to_le_bytes());
        hasher.update([u8::from(entry.truncated)]);
    }
    Some(DictionaryContextId(hasher.finalize().into()))
}

/// Derives a sealed segment lineage from its first catalog entry.
#[must_use]
pub fn lineage_from_catalog(
    catalog: &Catalog,
    source_scope_id: SourceScopeId,
    naming_contract_id: NamingContractId,
    segment_locator: SegmentLocator,
) -> Option<SegmentLineageId> {
    let first = catalog.entries.first()?;
    let descriptor = CatalogEntryDescriptor::of(first).canonical_bytes();
    Some(
        SegmentIdentity::sealed(
            source_scope_id,
            naming_contract_id,
            segment_locator,
            first.type_id,
            &descriptor,
        )
        .id(),
    )
}

const fn length_prefix(bytes: &[u8]) -> [u8; 8] {
    (bytes.len() as u64).to_le_bytes()
}

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(type_id: u32, len: u64, rows: u32, crc: u32) -> Entry {
        Entry {
            type_id,
            flags: 0,
            offset: 12,
            len,
            rows,
            crc32c: crc,
        }
    }

    #[test]
    fn catalog_entry_identity_does_not_depend_on_offset() {
        let mut left = entry(1_022_001, 4_096, 12, 0xDEAD_BEEF);
        let mut right = left;
        left.offset = 12;
        right.offset = 999_999;
        assert_eq!(
            CatalogEntryDescriptor::of(&left).canonical_bytes(),
            CatalogEntryDescriptor::of(&right).canonical_bytes()
        );
    }

    #[test]
    fn source_descriptor_binds_length_tail_and_catalog() {
        let base = SourceDescriptor::derive(1_000, b"tail", b"catalog");
        assert_ne!(base, SourceDescriptor::derive(1_001, b"tail", b"catalog"));
        assert_ne!(base, SourceDescriptor::derive(1_000, b"TAIL", b"catalog"));
        assert_ne!(base, SourceDescriptor::derive(1_000, b"tail", b"CATALOG"));
    }

    #[test]
    fn source_scope_uses_unambiguous_namespace_encoding() {
        assert_ne!(source_scope_id(b"ab", 7), source_scope_id(b"abc", 7));
        assert_ne!(source_scope_id(b"ab", 7), source_scope_id(b"ab", 8));
    }

    #[test]
    fn section_body_identity_binds_type_length_and_bytes() {
        let base = section_body_id(7, b"body");
        assert_ne!(base, section_body_id(8, b"body"));
        assert_ne!(base, section_body_id(7, b"body!"));
    }

    #[test]
    fn dictionary_context_requires_canonical_order() {
        let entries = [
            DictionaryContextEntry {
                str_id: 1,
                bytes: b"one",
                full_len: 3,
                truncated: false,
            },
            DictionaryContextEntry {
                str_id: 2,
                bytes: b"tw",
                full_len: 3,
                truncated: true,
            },
        ];
        let id = dictionary_context_id(&entries).expect("ordered context");
        assert_ne!(id, DictionaryContextId([0; 32]));

        let reversed = [entries[1], entries[0]];
        assert_eq!(dictionary_context_id(&reversed), None);
    }

    #[test]
    fn lineage_derivation_matches_segment_identity() {
        let catalog = Catalog {
            entries: vec![
                entry(1_022_001, 4_096, 12, 0xABCD),
                entry(1_028_001, 8, 1, 0x1234),
            ],
            min_ts: 1_000,
            max_ts: 2_000,
            source_id: 7,
            format_version: 1,
        };
        let scope = SourceScopeId([3; 32]);
        let naming = NamingContractId([4; 16]);
        let locator = SegmentLocator([5; 32]);
        let derived = lineage_from_catalog(&catalog, scope, naming, locator).expect("entry");
        let descriptor = CatalogEntryDescriptor::of(&catalog.entries[0]).canonical_bytes();
        let expected = SegmentIdentity::sealed(scope, naming, locator, 1_022_001, &descriptor).id();
        assert_eq!(derived, expected);
    }
}
