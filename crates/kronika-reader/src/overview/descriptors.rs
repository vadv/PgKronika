//! Catalog-derived descriptors and the content-addressed identity preimages.
//!
//! A segment's facts are named by identities that hash offset-independent
//! catalog content: the section body descriptor, the segment lineage, the
//! source scope, and the fact key. This module assembles the exact canonical
//! preimage bytes for each identity from a real [`kronika_format::Catalog`],
//! and derives the lineage through the analytics constructor that already owns
//! the hash.
//!
//! # Reported gap
//!
//! The overview hash ([`kronika_analytics::overview`] domain-separated
//! SHA-256) is private to analytics. Only [`SegmentIdentity`] and
//! `EventObservation` expose public hashing constructors. Source scope,
//! segment locator, section body, dictionary context, source descriptor, and
//! fact key have no public constructor from their preimage. This module
//! therefore produces their canonical preimage bytes and takes the already
//! hashed 32-byte identities as opaque inputs where analytics cannot fold
//! them. Folding those preimages must move into analytics before the reader
//! can originate the identities itself.

use kronika_analytics::overview::{
    EXTRACTOR_SEMANTICS_VERSION, FACT_SCHEMA_VERSION, NamingContractId, REGISTRY_CONTRACT_VERSION,
    SegmentIdentity, SegmentLineageId, SegmentLocator, SourceScopeId,
};
use kronika_format::{Catalog, Entry};

/// The file kind stored in the fact-key preimage.
const FILE_KIND_SEGMENT_FACTS: u16 = 1;

const SOURCE_SCOPE_TAG: &[u8] = b"pgk-overview-source-scope-v1";
const CATALOG_DESCRIPTOR_TAG: &[u8] = b"pgk-pgm-catalog-descriptor-v1";
const FACT_KEY_TAG: &[u8] = b"pgk-overview-fact-key-v1";

/// The offset-independent descriptor of one catalog entry.
///
/// It excludes the byte offset on purpose: the same section body keeps the
/// same descriptor wherever it lands in the file, so lineage and body identity
/// survive a verbatim reseal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntryDescriptor {
    /// Section type and schema, from the registry `type_id`.
    pub type_id: u32,
    /// Reserved catalog flags.
    pub flags: u32,
    /// Section body length, bytes.
    pub body_len: u64,
    /// Row or record count in the section.
    pub rows: u32,
    /// CRC32C of the section body.
    pub body_crc32c: u32,
}

impl CatalogEntryDescriptor {
    /// Reads the offset-independent fields of a catalog entry.
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

    /// The canonical content-descriptor preimage bytes for this entry.
    ///
    /// This is the exact byte run hashed into a section body identity and,
    /// for the first entry, into the segment lineage.
    #[must_use]
    pub fn content_descriptor(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(&self.type_id.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.body_len.to_le_bytes());
        out.extend_from_slice(&self.rows.to_le_bytes());
        out.extend_from_slice(&self.body_crc32c.to_le_bytes());
        out
    }
}

/// The inputs analytics still needs before the reader can hash an identity.
///
/// Each variant names an identity whose preimage this module builds but whose
/// SHA-256 fold is not exposed by a public analytics constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorGap {
    /// No public constructor folds a source-scope preimage.
    SourceScope,
    /// No public constructor folds a sealed segment locator.
    SegmentLocator,
    /// No public constructor folds a section-body preimage.
    SectionBody,
    /// No public constructor folds a dictionary-context preimage.
    DictionaryContext,
    /// No public constructor folds a source-descriptor preimage.
    SourceDescriptor,
    /// No public constructor folds a fact-key preimage.
    FactKey,
}

/// The canonical source-scope preimage `tag || namespace || source_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceScopePreimage {
    bytes: Vec<u8>,
}

impl SourceScopePreimage {
    /// Builds the preimage from the store namespace and PGM source ID.
    #[must_use]
    pub fn new(normalized_store_namespace: &[u8], pgm_source_id: u64) -> Self {
        let mut bytes =
            Vec::with_capacity(SOURCE_SCOPE_TAG.len() + normalized_store_namespace.len() + 8);
        bytes.extend_from_slice(SOURCE_SCOPE_TAG);
        bytes.extend_from_slice(normalized_store_namespace);
        bytes.extend_from_slice(&pgm_source_id.to_le_bytes());
        Self { bytes }
    }

    /// The preimage bytes a future analytics constructor would fold.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The unfolded identity: this reader cannot hash it yet.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "the gap names this preimage's identity, a property of the type"
    )]
    pub const fn gap(&self) -> DescriptorGap {
        DescriptorGap::SourceScope
    }
}

/// The canonical PGM catalog descriptor preimage.
///
/// It binds the descriptor to the exact file length, tail index, and raw
/// catalog block, so ordinary replacement or corruption changes it without any
/// body read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentContentDescriptor {
    bytes: Vec<u8>,
}

impl SegmentContentDescriptor {
    /// Builds the preimage from the file length, tail-index, and catalog bytes.
    #[must_use]
    pub fn new(source_file_len: u64, tail_index_bytes: &[u8], raw_catalog_bytes: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(
            CATALOG_DESCRIPTOR_TAG.len() + 8 + tail_index_bytes.len() + raw_catalog_bytes.len(),
        );
        bytes.extend_from_slice(CATALOG_DESCRIPTOR_TAG);
        bytes.extend_from_slice(&source_file_len.to_le_bytes());
        bytes.extend_from_slice(tail_index_bytes);
        bytes.extend_from_slice(raw_catalog_bytes);
        Self { bytes }
    }

    /// The preimage bytes a future analytics constructor would fold.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The unfolded identity: this reader cannot hash it yet.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "the gap names this preimage's identity, a property of the type"
    )]
    pub const fn gap(&self) -> DescriptorGap {
        DescriptorGap::SourceDescriptor
    }
}

/// The canonical fact-key preimage that names a segment's fact file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactKeyPreimage {
    bytes: Vec<u8>,
}

impl FactKeyPreimage {
    /// Builds the preimage from the scope, descriptor, and version axes.
    ///
    /// The `fact_schema`, `extractor`, and `registry` version axes come from
    /// the analytics constants, so they cannot drift from the fact contract.
    #[must_use]
    pub fn new(source_scope_id: SourceScopeId, source_descriptor: [u8; 32]) -> Self {
        let mut bytes = Vec::with_capacity(FACT_KEY_TAG.len() + 32 + 32 + 2 + 12);
        bytes.extend_from_slice(FACT_KEY_TAG);
        bytes.extend_from_slice(&source_scope_id.0);
        bytes.extend_from_slice(&source_descriptor);
        bytes.extend_from_slice(&FILE_KIND_SEGMENT_FACTS.to_le_bytes());
        bytes.extend_from_slice(&FACT_SCHEMA_VERSION.to_le_bytes());
        bytes.extend_from_slice(&EXTRACTOR_SEMANTICS_VERSION.to_le_bytes());
        bytes.extend_from_slice(&REGISTRY_CONTRACT_VERSION.to_le_bytes());
        Self { bytes }
    }

    /// The preimage bytes a future analytics constructor would fold.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The unfolded identity: this reader cannot hash it yet.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "the gap names this preimage's identity, a property of the type"
    )]
    pub const fn gap(&self) -> DescriptorGap {
        DescriptorGap::FactKey
    }
}

/// Derives the segment lineage from a real catalog and its opaque scope inputs.
///
/// The lineage is the one identity M1 can originate: analytics folds it through
/// [`SegmentIdentity::sealed`]. The first catalog entry supplies both the
/// `first_entry_type` and its offset-independent content descriptor.
///
/// Returns `None` for a catalog with no entries, which cannot name a lineage.
#[must_use]
pub fn lineage_from_catalog(
    catalog: &Catalog,
    source_scope_id: SourceScopeId,
    naming_contract_id: NamingContractId,
    segment_locator: SegmentLocator,
) -> Option<SegmentLineageId> {
    let first = catalog.entries.first()?;
    let descriptor = CatalogEntryDescriptor::of(first).content_descriptor();
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
    fn a_content_descriptor_ignores_the_byte_offset() {
        let mut a = entry(1_022_001, 4_096, 12, 0xDEAD_BEEF);
        let mut b = a;
        a.offset = 12;
        b.offset = 999_999;
        assert_eq!(
            CatalogEntryDescriptor::of(&a).content_descriptor(),
            CatalogEntryDescriptor::of(&b).content_descriptor(),
            "offset must not change the descriptor"
        );
    }

    #[test]
    fn a_content_descriptor_reacts_to_every_retained_field() {
        let base = CatalogEntryDescriptor::of(&entry(1, 2, 3, 4)).content_descriptor();
        assert_ne!(
            base,
            CatalogEntryDescriptor::of(&entry(9, 2, 3, 4)).content_descriptor()
        );
        assert_ne!(
            base,
            CatalogEntryDescriptor::of(&entry(1, 9, 3, 4)).content_descriptor()
        );
        assert_ne!(
            base,
            CatalogEntryDescriptor::of(&entry(1, 2, 9, 4)).content_descriptor()
        );
        assert_ne!(
            base,
            CatalogEntryDescriptor::of(&entry(1, 2, 3, 9)).content_descriptor()
        );
        assert_eq!(base.len(), 24);
    }

    #[test]
    fn preimages_are_domain_separated_and_bind_their_inputs() {
        let scope = SourceScopePreimage::new(b"/srv/pgm", 7);
        assert!(scope.bytes().starts_with(SOURCE_SCOPE_TAG));
        assert_ne!(scope, SourceScopePreimage::new(b"/srv/pgm", 8));
        assert_ne!(scope, SourceScopePreimage::new(b"/srv/other", 7));
        assert_eq!(scope.gap(), DescriptorGap::SourceScope);

        let descriptor = SegmentContentDescriptor::new(1_000, b"tail", b"catalog");
        assert!(descriptor.bytes().starts_with(CATALOG_DESCRIPTOR_TAG));
        assert_ne!(
            descriptor,
            SegmentContentDescriptor::new(1_001, b"tail", b"catalog")
        );

        let key = FactKeyPreimage::new(SourceScopeId([1; 32]), [2; 32]);
        assert!(key.bytes().starts_with(FACT_KEY_TAG));
        assert_ne!(key, FactKeyPreimage::new(SourceScopeId([9; 32]), [2; 32]));
    }

    #[test]
    fn lineage_uses_the_first_entry_and_matches_the_analytics_constructor() {
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
        let derived = lineage_from_catalog(&catalog, scope, naming, locator).expect("has entries");

        let descriptor = CatalogEntryDescriptor::of(&catalog.entries[0]).content_descriptor();
        let expected = SegmentIdentity::sealed(scope, naming, locator, 1_022_001, &descriptor).id();
        assert_eq!(derived, expected);
    }

    #[test]
    fn an_empty_catalog_names_no_lineage() {
        let catalog = Catalog {
            entries: vec![],
            min_ts: 0,
            max_ts: 0,
            source_id: 0,
            format_version: 1,
        };
        assert_eq!(
            lineage_from_catalog(
                &catalog,
                SourceScopeId([0; 32]),
                NamingContractId([0; 16]),
                SegmentLocator([0; 32]),
            ),
            None
        );
    }
}
