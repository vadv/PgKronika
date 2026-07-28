//! Compact identities derived from validated PGM end catalogs.

#[cfg(test)]
use kronika_format::TAIL_INDEX_LEN;
use kronika_format::{Catalog, DecodeError, Entry, FORMAT_VERSION, MAGIC};
use sha2::{Digest as _, Sha256};

const LOGICAL_DIGEST_DOMAIN: &[u8] = b"pgk-overview-catalog-v1\0";
const LAYOUT_DIGEST_DOMAIN: &[u8] = b"pgk-pgm-catalog-layout-v1\0";
const TYPE_BLOOM_WORDS: usize = 8;
const TYPE_BLOOM_HASHES: usize = 8;
const TYPE_BLOOM_BITS: u64 = 512;

/// SHA-256 identity of ordered catalog metadata excluding section offsets.
///
/// A journal part and the same part relocated into a finished PGM therefore
/// have the same logical digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogDigest([u8; 32]);

impl CatalogDigest {
    /// Derives the offset-independent digest of a validated decoded catalog.
    ///
    /// # Panics
    ///
    /// Panics if an in-memory catalog contains more entries than the on-disk
    /// `u32` entry-count field can represent.
    #[must_use]
    pub fn from_catalog(catalog: &Catalog) -> Self {
        let entry_count = u32::try_from(catalog.entries.len())
            .expect("a decoded PGM catalog entry count always fits u32");
        catalog_digests(
            catalog.source_id,
            catalog.min_ts,
            catalog.max_ts,
            catalog.format_version,
            catalog.window_count,
            entry_count,
            catalog.entries.iter().copied(),
        )
        .0
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// SHA-256 identity of the ordered catalog metadata including section offsets.
///
/// This identifies the catalog layout, not the bytes of section bodies or the
/// complete PGM file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogLayoutDigest([u8; 32]);

impl CatalogLayoutDigest {
    /// Derives the canonical layout identity of a validated catalog.
    ///
    /// # Panics
    ///
    /// Panics if an in-memory catalog contains more entries than the on-disk
    /// `u32` entry-count field can represent.
    #[must_use]
    pub fn from_catalog(catalog: &Catalog) -> Self {
        let entry_count = u32::try_from(catalog.entries.len())
            .expect("a decoded PGM catalog entry count always fits u32");
        catalog_digests(
            catalog.source_id,
            catalog.min_ts,
            catalog.max_ts,
            catalog.format_version,
            catalog.window_count,
            entry_count,
            catalog.entries.iter().copied(),
        )
        .1
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Compact validated metadata retained for a finished PGM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSummary {
    /// Minimal timestamp of the segment, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp of the segment, unix microseconds.
    pub max_ts: i64,
    /// `str_id` of `{cluster_id}/{pg_system_identifier}`; 0 = not set.
    pub source_id: u64,
    /// Number of ordered catalog entries.
    pub entry_count: u32,
    /// Container format version.
    pub format_version: u32,
    /// Collection windows coalesced into this container; 0 = unknown.
    pub window_count: u32,
    /// Encoded catalog bytes, excluding the tail index.
    pub catalog_len: u32,
    /// Offset-independent identity used across journal finalization.
    pub logical_digest: CatalogDigest,
    /// Offset-sensitive identity used to pin the exact catalog layout.
    pub layout_digest: CatalogLayoutDigest,
    nonempty_type_bloom: [u64; TYPE_BLOOM_WORDS],
}

impl CatalogSummary {
    /// Validates encoded catalog bytes and derives their compact summary.
    ///
    /// `body_end` is the absolute offset at which the catalog begins. Every
    /// entry must point to a complete section body between `PGM1` and that
    /// offset.
    ///
    /// # Errors
    ///
    /// Returns a structural error for an invalid catalog, unsupported format,
    /// oversized encoded length, or out-of-bounds section entry.
    pub fn from_encoded(bytes: &[u8], body_end: u64) -> Result<Self, CatalogSummaryError> {
        let view = Catalog::view(bytes).map_err(CatalogSummaryError::Decode)?;
        if view.format_version != FORMAT_VERSION {
            return Err(CatalogSummaryError::UnsupportedFormat {
                version: view.format_version,
            });
        }
        let catalog_len =
            u32::try_from(bytes.len()).map_err(|_overflow| CatalogSummaryError::LengthOverflow)?;
        let entries = view.entries();
        for entry in entries.clone() {
            validate_entry_bounds(entry, body_end)?;
        }
        let (logical_digest, layout_digest) = catalog_digests(
            view.source_id,
            view.min_ts,
            view.max_ts,
            view.format_version,
            view.window_count,
            view.entry_count,
            entries.clone(),
        );
        let nonempty_type_bloom = nonempty_type_bloom(entries);
        Ok(Self {
            min_ts: view.min_ts,
            max_ts: view.max_ts,
            source_id: view.source_id,
            entry_count: view.entry_count,
            format_version: view.format_version,
            window_count: view.window_count,
            catalog_len,
            logical_digest,
            layout_digest,
            nonempty_type_bloom,
        })
    }

    /// Derives a summary from an already validated decoded catalog.
    ///
    /// `catalog_len` must be the encoded catalog length from the PGM tail.
    ///
    /// # Panics
    ///
    /// Panics if an in-memory catalog contains more entries than the on-disk
    /// `u32` entry count can represent.
    #[must_use]
    pub fn from_catalog(catalog: &Catalog, catalog_len: u32) -> Self {
        let entry_count = u32::try_from(catalog.entries.len())
            .expect("a decoded PGM catalog entry count always fits u32");
        let (logical_digest, layout_digest) = catalog_digests(
            catalog.source_id,
            catalog.min_ts,
            catalog.max_ts,
            catalog.format_version,
            catalog.window_count,
            entry_count,
            catalog.entries.iter().copied(),
        );
        let nonempty_type_bloom = nonempty_type_bloom(catalog.entries.iter().copied());
        Self {
            min_ts: catalog.min_ts,
            max_ts: catalog.max_ts,
            source_id: catalog.source_id,
            entry_count,
            format_version: catalog.format_version,
            window_count: catalog.window_count,
            catalog_len,
            logical_digest,
            layout_digest,
            nonempty_type_bloom,
        }
    }

    /// Conservatively tests whether any requested type may contain rows.
    ///
    /// The fixed-size filter has no false negatives. A positive result must be
    /// confirmed against the lazily opened catalog before rows or byte budgets
    /// are charged.
    #[must_use]
    pub fn may_contain_any_nonempty_type(&self, type_ids: &[u32]) -> bool {
        type_ids
            .iter()
            .any(|&type_id| bloom_may_contain(&self.nonempty_type_bloom, type_id))
    }
}

/// Structural failure while deriving a compact catalog summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSummaryError {
    /// Catalog framing, count, or CRC is invalid.
    Decode(DecodeError),
    /// The catalog uses another PGM format version.
    UnsupportedFormat {
        /// Version read from catalog metadata.
        version: u32,
    },
    /// The encoded catalog length cannot be represented by its tail field.
    LengthOverflow,
    /// A section entry points outside the body area.
    EntryOutOfBounds {
        /// Section type from the offending entry.
        type_id: u32,
    },
}

impl std::fmt::Display for CatalogSummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "catalog decode failed: {error}"),
            Self::UnsupportedFormat { version } => {
                write!(f, "unsupported format version {version}")
            }
            Self::LengthOverflow => f.write_str("catalog length does not fit u32"),
            Self::EntryOutOfBounds { type_id } => {
                write!(
                    f,
                    "catalog entry for type {type_id} points outside the body area"
                )
            }
        }
    }
}

impl std::error::Error for CatalogSummaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            Self::UnsupportedFormat { .. }
            | Self::LengthOverflow
            | Self::EntryOutOfBounds { .. } => None,
        }
    }
}

/// Derives the logical and offset-sensitive identities of ordered entries.
#[must_use]
pub fn catalog_digests(
    source_id: u64,
    min_ts: i64,
    max_ts: i64,
    format_version: u32,
    window_count: u32,
    entry_count: u32,
    entries: impl IntoIterator<Item = Entry>,
) -> (CatalogDigest, CatalogLayoutDigest) {
    let mut logical = Sha256::new();
    logical.update(LOGICAL_DIGEST_DOMAIN);
    logical.update(source_id.to_le_bytes());
    logical.update(min_ts.to_le_bytes());
    logical.update(max_ts.to_le_bytes());
    logical.update(format_version.to_le_bytes());
    logical.update(window_count.to_le_bytes());
    logical.update(u128::from(entry_count).to_le_bytes());

    let mut layout = Sha256::new();
    layout.update(LAYOUT_DIGEST_DOMAIN);
    layout.update(source_id.to_le_bytes());
    layout.update(min_ts.to_le_bytes());
    layout.update(max_ts.to_le_bytes());
    layout.update(format_version.to_le_bytes());
    layout.update(window_count.to_le_bytes());
    layout.update(u128::from(entry_count).to_le_bytes());

    for entry in entries {
        update_common_entry_digest(&mut logical, entry);
        layout.update(entry.offset.to_le_bytes());
        update_common_entry_digest(&mut layout, entry);
    }
    (
        CatalogDigest(logical.finalize().into()),
        CatalogLayoutDigest(layout.finalize().into()),
    )
}

fn update_common_entry_digest(hasher: &mut Sha256, entry: Entry) {
    hasher.update(entry.type_id.to_le_bytes());
    hasher.update(entry.flags.to_le_bytes());
    hasher.update(entry.len.to_le_bytes());
    hasher.update(entry.rows.to_le_bytes());
    hasher.update(entry.crc32c.to_le_bytes());
}

fn validate_entry_bounds(entry: Entry, body_end: u64) -> Result<(), CatalogSummaryError> {
    let in_bounds = entry.offset >= MAGIC.len() as u64
        && entry
            .offset
            .checked_add(entry.len)
            .is_some_and(|end| end <= body_end);
    if in_bounds {
        Ok(())
    } else {
        Err(CatalogSummaryError::EntryOutOfBounds {
            type_id: entry.type_id,
        })
    }
}

fn nonempty_type_bloom(entries: impl IntoIterator<Item = Entry>) -> [u64; TYPE_BLOOM_WORDS] {
    let mut bloom = [0_u64; TYPE_BLOOM_WORDS];
    for entry in entries {
        if entry.rows != 0 {
            for bit in bloom_bits(entry.type_id) {
                bloom[bit / u64::BITS as usize] |= 1_u64 << (bit % u64::BITS as usize);
            }
        }
    }
    bloom
}

fn bloom_may_contain(bloom: &[u64; TYPE_BLOOM_WORDS], type_id: u32) -> bool {
    bloom_bits(type_id)
        .into_iter()
        .all(|bit| bloom[bit / u64::BITS as usize] & (1_u64 << (bit % u64::BITS as usize)) != 0)
}

fn bloom_bits(type_id: u32) -> [usize; TYPE_BLOOM_HASHES] {
    let first = mix64(u64::from(type_id));
    let step = mix64(u64::from(type_id) ^ 0x9E37_79B9_7F4A_7C15) | 1;
    std::array::from_fn(|index| {
        let hash = first.wrapping_add((index as u64).wrapping_mul(step));
        usize::try_from(hash % TYPE_BLOOM_BITS).expect("512 bloom bits fit usize")
    })
}

const fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog {
            entries: vec![
                Entry {
                    type_id: 20,
                    flags: 0,
                    offset: 4,
                    len: 3,
                    rows: 0,
                    crc32c: 1,
                },
                Entry {
                    type_id: 10,
                    flags: 0,
                    offset: 7,
                    len: 5,
                    rows: 2,
                    crc32c: 2,
                },
                Entry {
                    type_id: 10,
                    flags: 0,
                    offset: 12,
                    len: 7,
                    rows: 3,
                    crc32c: 3,
                },
            ],
            min_ts: 100,
            max_ts: 200,
            source_id: 7,
            format_version: FORMAT_VERSION,
            window_count: 3,
        }
    }

    #[test]
    fn encoded_summary_matches_owned_summary() {
        let catalog = catalog();
        let encoded = catalog.encode();
        let catalog_bytes = &encoded[..encoded.len() - TAIL_INDEX_LEN];
        let from_bytes =
            CatalogSummary::from_encoded(catalog_bytes, 19).expect("valid encoded summary");
        let from_catalog =
            CatalogSummary::from_catalog(&catalog, u32::try_from(catalog_bytes.len()).unwrap());

        assert_eq!(from_bytes, from_catalog);
        assert!(from_bytes.may_contain_any_nonempty_type(&[10]));
        assert!(!from_bytes.may_contain_any_nonempty_type(&[20]));
    }

    #[test]
    fn logical_digest_ignores_relocated_offsets_but_layout_digest_does_not() {
        let first = catalog();
        let mut relocated = first.clone();
        for entry in &mut relocated.entries {
            entry.offset += 100;
        }
        let first = CatalogSummary::from_catalog(&first, 136);
        let relocated = CatalogSummary::from_catalog(&relocated, 136);

        assert_eq!(first.logical_digest, relocated.logical_digest);
        assert_ne!(first.layout_digest, relocated.layout_digest);
    }

    #[test]
    fn catalog_digests_include_window_count() {
        let first = catalog();
        let mut more_windows = first.clone();
        more_windows.window_count += 1;

        let first = CatalogSummary::from_catalog(&first, 136);
        let more_windows = CatalogSummary::from_catalog(&more_windows, 136);

        assert_ne!(first.logical_digest, more_windows.logical_digest);
        assert_ne!(first.layout_digest, more_windows.layout_digest);
    }

    #[test]
    fn summary_rejects_entry_outside_the_body_area() {
        let catalog = catalog();
        let encoded = catalog.encode();
        let catalog_bytes = &encoded[..encoded.len() - TAIL_INDEX_LEN];
        assert!(matches!(
            CatalogSummary::from_encoded(catalog_bytes, 18),
            Err(CatalogSummaryError::EntryOutOfBounds { type_id: 10 })
        ));
    }

    #[test]
    fn nonempty_type_filter_has_no_false_negatives() {
        let entries = (1..=10_000).map(|type_id| Entry {
            type_id,
            flags: 0,
            offset: 4,
            len: 0,
            rows: 1,
            crc32c: 0,
        });
        let bloom = nonempty_type_bloom(entries);
        for type_id in 1..=10_000 {
            assert!(bloom_may_contain(&bloom, type_id));
        }
    }
}
