//! Semantic deltas for incremental store scans.

use kronika_format::{Catalog, DamageRegion};
use sha2::{Digest as _, Sha256};

const CATALOG_DIGEST_DOMAIN: &[u8] = b"pgk-overview-catalog-v1\0";
const SEALED_LOCATOR_DOMAIN: &[u8] = b"pgk-overview-sealed-locator-v1\0";

/// SHA-256 identity of an offset-independent catalog descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogDigest([u8; 32]);

impl CatalogDigest {
    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Stable identity of one direct-child sealed file name.
///
/// The locator deliberately excludes the root directory, so moving a complete
/// store does not rename its segments. It includes the exact Unix file-name
/// bytes, so two names containing identical segment content remain distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SealedLocator([u8; 32]);

impl SealedLocator {
    /// Derives a domain-separated locator from direct-child file-name bytes.
    #[must_use]
    pub fn from_file_name_bytes(file_name: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(SEALED_LOCATOR_DOMAIN);
        hasher.update((file_name.len() as u128).to_le_bytes());
        hasher.update(file_name);
        Self(hasher.finalize().into())
    }

    /// Returns the locator bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Monotone identifier of a proven-continuous journal generation.
///
/// A replacement, truncation, or unproven rewrite starts a new generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JournalGenerationId(pub u64);

/// How the journal tail evolved between two scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartTransition {
    /// The validated prefix grew or stayed put under the same file identity.
    Append,
    /// The journal was truncated in place or vanished.
    Reset,
    /// The backing file was replaced (device/inode changed).
    Replaced,
    /// Continuity cannot be proven; the live view must rebuild.
    Uncertain,
}

impl PartTransition {
    /// Whether this transition preserves the prior journal generation.
    ///
    /// Only [`Append`](Self::Append) keeps folded state; every other class
    /// starts a fresh generation and forces the live builder to rebuild.
    #[must_use]
    pub const fn preserves_generation(self) -> bool {
        matches!(self, Self::Append)
    }
}

/// Observable filesystem identity of the journal file at one scan.
///
/// Nanosecond modification and metadata-change times distinguish an unchanged
/// file from an equal-length in-place rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalIdentity {
    /// Backing device number.
    pub device: u64,
    /// Backing inode number.
    pub inode: u64,
    /// File length in bytes.
    pub len: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// Metadata-change time in nanoseconds since the Unix epoch.
    pub ctime_ns: i128,
}

/// Idempotency key of one completed journal part within a generation.
///
/// It binds the frame position, the part body length, and an
/// offset-independent digest of the part catalog, so a re-scan that surfaces the
/// same bytes yields the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartId {
    /// Journal generation the key is scoped to.
    pub generation: JournalGenerationId,
    /// Byte offset of the part body inside the journal.
    pub frame_offset: u64,
    /// Length of the part body in bytes.
    pub body_len: u64,
    /// SHA-256 identity of the offset-independent catalog descriptor.
    pub catalog_digest: CatalogDigest,
}

/// A completed, CRC-valid part surfaced by a refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartDescriptor {
    /// Stable idempotency key.
    pub part_id: PartId,
    /// Source identifier from the part catalog.
    pub source_id: u64,
    /// Earliest timestamp in the part.
    pub min_ts: i64,
    /// Latest timestamp in the part.
    pub max_ts: i64,
}

/// Stable identity and offset-independent catalog descriptor of one sealed segment.
///
/// The locator identifies the direct-child file name, while the catalog digest
/// identifies its content without depending on section-body offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentDescriptor {
    /// Stable identity derived from the exact direct-child file-name bytes.
    pub locator: SealedLocator,
    /// Source identifier from the segment catalog.
    pub source_id: u64,
    /// Earliest timestamp in the segment.
    pub min_ts: i64,
    /// Latest timestamp in the segment.
    pub max_ts: i64,
    /// SHA-256 identity of the offset-independent catalog descriptor.
    pub catalog_digest: CatalogDigest,
}

impl SegmentDescriptor {
    /// Derives a segment descriptor from a stable locator and decoded catalog.
    #[must_use]
    pub fn from_catalog(locator: SealedLocator, catalog: &Catalog) -> Self {
        Self {
            locator,
            source_id: catalog.source_id,
            min_ts: catalog.min_ts,
            max_ts: catalog.max_ts,
            catalog_digest: catalog_digest(catalog),
        }
    }
}

/// A half-open byte range of the journal tail that is not yet valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Inclusive start offset.
    pub start: u64,
    /// Exclusive end offset.
    pub end: u64,
}

/// Journal-scoped portion of a refresh delta.
#[derive(Debug, Clone)]
pub struct JournalDelta {
    /// Whether this delta delivers a baseline that has not been consumed yet.
    ///
    /// A bootstrap re-lists every current part even though
    /// `previous_valid_len` remains the physical watermark captured by the
    /// preceding open or non-delta refresh.
    pub bootstrap: bool,
    /// Generation the post-refresh journal belongs to.
    pub generation_id: JournalGenerationId,
    /// Validated journal length before this refresh.
    pub previous_valid_len: u64,
    /// Validated journal length after this refresh.
    pub new_valid_len: u64,
    /// Parts that completed since the previous scan, in journal order.
    pub completed_parts: Vec<PartDescriptor>,
    /// Every valid part in the post-refresh journal, in journal order.
    ///
    /// This is the authoritative completion target when
    /// [`current_parts_complete`](Self::current_parts_complete) is `true`.
    pub current_parts: Vec<PartDescriptor>,
    /// Whether `current_parts` is an authoritative descriptor set.
    ///
    /// An `active.parts` warning makes this `false`: callers must not publish a
    /// view from a scan that may have skipped journal content.
    pub current_parts_complete: bool,
    /// Proven continuity class of the tail.
    pub transition: PartTransition,
    /// Torn-tail bytes past the validated prefix, when present.
    pub tail_pending: Option<ByteRange>,
    /// Damaged journal regions found in this scan.
    pub damages: Vec<DamageRegion>,
}

/// Semantic result of one incremental store scan.
#[derive(Debug, Clone)]
pub struct RefreshDelta {
    /// View generation captured before this refresh.
    pub previous_view_generation: u64,
    /// View generation captured after this refresh.
    pub new_view_generation: u64,
    /// Whether the producer observed any raw state change at this boundary.
    ///
    /// This includes changes, such as a warning appearing or clearing, that
    /// cannot be reconstructed from the semantic descriptor lists alone.
    pub view_changed: bool,
    /// Sealed segments newly visible in this scan.
    pub sealed_added: Vec<SegmentDescriptor>,
    /// Sealed segments no longer visible in this scan.
    pub sealed_removed: Vec<SegmentDescriptor>,
    /// Journal-scoped delta.
    pub journal: JournalDelta,
}

impl RefreshDelta {
    /// Whether the live builder must discard folded state and rebuild.
    ///
    /// A tail that cannot be proven a clean append (`Reset`, `Replaced`, or
    /// `Uncertain`) invalidates the folded watermark.
    #[must_use]
    pub const fn requires_live_rebuild(&self) -> bool {
        !self.journal.transition.preserves_generation()
    }
}

/// Classifies the journal tail transition from filesystem identity alone.
///
/// `previous_valid_len` is the resumable offset the last scan validated; it may
/// be shorter than the previous file length when the tail held a torn frame.
#[must_use]
pub const fn classify_transition(
    previous: Option<JournalIdentity>,
    current: Option<JournalIdentity>,
    previous_valid_len: u64,
) -> PartTransition {
    match (previous, current) {
        // Continuity holds trivially: the journal is still absent, or a fresh
        // journal appeared over a proven-empty baseline.
        (None, _) => PartTransition::Append,
        // The journal vanished after holding data.
        (Some(_), None) => PartTransition::Reset,
        (Some(previous), Some(current)) => {
            if previous.device != current.device || previous.inode != current.inode {
                return PartTransition::Replaced;
            }
            if current.len < previous_valid_len {
                return PartTransition::Reset;
            }
            if current.len > previous.len {
                return PartTransition::Append;
            }
            if current.len < previous.len {
                return PartTransition::Uncertain;
            }
            // An equal-length change to either filesystem timestamp invalidates
            // the cached prefix.
            if previous.mtime_ns == current.mtime_ns && previous.ctime_ns == current.ctime_ns {
                PartTransition::Append
            } else {
                PartTransition::Uncertain
            }
        }
    }
}

/// Derives the offset-independent SHA-256 identity of a catalog.
///
/// Section body offsets are excluded so a verbatim part keeps its digest after a
/// seal that relocates bodies.
#[must_use]
pub fn catalog_digest(catalog: &Catalog) -> CatalogDigest {
    let mut hasher = Sha256::new();
    hasher.update(CATALOG_DIGEST_DOMAIN);
    hasher.update(catalog.source_id.to_le_bytes());
    hasher.update(catalog.min_ts.to_le_bytes());
    hasher.update(catalog.max_ts.to_le_bytes());
    hasher.update(catalog.format_version.to_le_bytes());
    hasher.update((catalog.entries.len() as u128).to_le_bytes());
    for entry in &catalog.entries {
        hasher.update(entry.type_id.to_le_bytes());
        hasher.update(entry.flags.to_le_bytes());
        hasher.update(entry.len.to_le_bytes());
        hasher.update(entry.rows.to_le_bytes());
        hasher.update(entry.crc32c.to_le_bytes());
    }
    CatalogDigest(hasher.finalize().into())
}

/// Derives the idempotency key of a completed part.
#[must_use]
pub fn part_id(
    generation: JournalGenerationId,
    frame_offset: u64,
    body_len: u64,
    catalog: &Catalog,
) -> PartId {
    PartId {
        generation,
        frame_offset,
        body_len,
        catalog_digest: catalog_digest(catalog),
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use super::*;

    fn identity(device: u64, inode: u64, len: u64, changed_ns: i128) -> JournalIdentity {
        JournalIdentity {
            device,
            inode,
            len,
            mtime_ns: changed_ns,
            ctime_ns: changed_ns,
        }
    }

    fn part_catalog(min_ts: i64, max_ts: i64, source_id: u64) -> Catalog {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id,
            },
        );
        let unit = crate::PgmUnit::open(bytes.as_slice()).expect("open part");
        unit.catalog().clone()
    }

    #[test]
    fn an_absent_journal_that_stays_absent_is_a_continuous_append() {
        assert_eq!(classify_transition(None, None, 0), PartTransition::Append);
    }

    #[test]
    fn a_vanished_journal_is_a_reset() {
        let previous = identity(1, 2, 100, 10);
        assert_eq!(
            classify_transition(Some(previous), None, 100),
            PartTransition::Reset
        );
    }

    #[test]
    fn a_fresh_journal_over_empty_baseline_is_an_append() {
        let current = identity(1, 2, 100, 10);
        assert_eq!(
            classify_transition(None, Some(current), 0),
            PartTransition::Append
        );
    }

    #[test]
    fn a_grown_journal_under_stable_identity_is_an_append() {
        let previous = identity(1, 2, 100, 10);
        let current = identity(1, 2, 240, 20);
        assert_eq!(
            classify_transition(Some(previous), Some(current), 100),
            PartTransition::Append
        );
    }

    #[test]
    fn a_shrunk_journal_is_a_truncation_reset() {
        let previous = identity(1, 2, 100, 10);
        let current = identity(1, 2, 40, 20);
        assert_eq!(
            classify_transition(Some(previous), Some(current), 100),
            PartTransition::Reset
        );
    }

    #[test]
    fn replacing_a_pending_tail_without_shrinking_the_valid_prefix_is_uncertain() {
        let previous = identity(1, 2, 150, 10);
        let current = identity(1, 2, 130, 20);
        assert_eq!(
            classify_transition(Some(previous), Some(current), 100),
            PartTransition::Uncertain
        );
    }

    #[test]
    fn a_changed_inode_at_the_same_length_is_a_replacement() {
        let previous = identity(1, 2, 100, 10);
        let replaced = identity(1, 9, 100, 10);
        assert_eq!(
            classify_transition(Some(previous), Some(replaced), 100),
            PartTransition::Replaced
        );
    }

    #[test]
    fn an_equal_length_rewrite_that_moves_mtime_is_uncertain() {
        let previous = identity(1, 2, 100, 10);
        let rewritten = identity(1, 2, 100, 55);
        assert_eq!(
            classify_transition(Some(previous), Some(rewritten), 100),
            PartTransition::Uncertain
        );
    }

    #[test]
    fn an_equal_length_rewrite_that_only_moves_ctime_is_uncertain() {
        let previous = identity(1, 2, 100, 10);
        let rewritten = JournalIdentity {
            ctime_ns: 55,
            ..previous
        };
        assert_eq!(
            classify_transition(Some(previous), Some(rewritten), 100),
            PartTransition::Uncertain
        );
    }

    #[test]
    fn an_untouched_equal_length_journal_stays_a_continuous_append() {
        let previous = identity(1, 2, 100, 10);
        let same = identity(1, 2, 100, 10);
        assert_eq!(
            classify_transition(Some(previous), Some(same), 100),
            PartTransition::Append
        );
    }

    #[test]
    fn only_append_preserves_the_generation() {
        assert!(PartTransition::Append.preserves_generation());
        assert!(!PartTransition::Reset.preserves_generation());
        assert!(!PartTransition::Replaced.preserves_generation());
        assert!(!PartTransition::Uncertain.preserves_generation());
    }

    #[test]
    fn a_verbatim_part_keeps_its_catalog_digest_regardless_of_frame_offset() {
        let catalog = part_catalog(1_000, 2_000, 7);
        let generation = JournalGenerationId(3);
        let here = part_id(generation, 64, 128, &catalog);
        let moved = part_id(generation, 4_096, 128, &catalog);
        assert_eq!(
            here.catalog_digest, moved.catalog_digest,
            "the digest is offset-independent"
        );
        assert_ne!(here, moved, "frame offset still distinguishes the keys");
    }

    #[test]
    fn distinct_catalog_content_yields_distinct_segment_digests() {
        let locator = SealedLocator::from_file_name_bytes(b"1000.pgm");
        let first = SegmentDescriptor::from_catalog(locator, &part_catalog(1_000, 2_000, 7));
        let later = SegmentDescriptor::from_catalog(locator, &part_catalog(3_000, 4_000, 7));
        assert_ne!(first.catalog_digest, later.catalog_digest);
    }

    #[test]
    fn identical_catalogs_under_distinct_names_have_distinct_locators() {
        let catalog = part_catalog(1_000, 2_000, 7);
        let first = SegmentDescriptor::from_catalog(
            SealedLocator::from_file_name_bytes(b"1000.pgm"),
            &catalog,
        );
        let alias = SegmentDescriptor::from_catalog(
            SealedLocator::from_file_name_bytes(b"segment-copy.pgm"),
            &catalog,
        );

        assert_eq!(first.catalog_digest, alias.catalog_digest);
        assert_ne!(first.locator, alias.locator);
    }

    #[test]
    fn an_uncertain_refresh_requires_a_live_rebuild() {
        let delta = RefreshDelta {
            previous_view_generation: 4,
            new_view_generation: 5,
            view_changed: true,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: JournalDelta {
                bootstrap: false,
                generation_id: JournalGenerationId(5),
                previous_valid_len: 100,
                new_valid_len: 100,
                completed_parts: Vec::new(),
                current_parts: Vec::new(),
                current_parts_complete: true,
                transition: PartTransition::Uncertain,
                tail_pending: None,
                damages: Vec::new(),
            },
        };
        assert!(delta.requires_live_rebuild());
    }
}
