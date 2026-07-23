//! Semantic delta of one incremental store scan.
//!
//! A refresh reports more than a changed file length: it names the journal
//! generation, the parts that completed since the last scan, the proven
//! continuity class of the tail, any torn-tail bytes, and the sealed segments
//! that appeared or disappeared. The live builder consumes this to fold each
//! completed part exactly once and to decide when continuity is broken and a
//! rebuild is required.
//!
//! [`classify_transition`] and [`part_id`] are pure: the scan-facing method on
//! [`crate::LocalDirSnapshot`] captures file identity and calls them.

use kronika_format::{Catalog, DamageRegion};

/// Monotone identifier of a proven-continuous journal generation.
///
/// A new generation is minted whenever append continuity cannot be proven:
/// device/inode replacement, truncation, or an equal-length rewrite. Within one
/// generation a [`PartId`] is a stable idempotency key, so redelivering the same
/// part does not fold it twice.
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
/// `mtime_ns` folds seconds and nanoseconds into one signed value so an
/// equal-length rewrite that touches the modification time is distinguishable
/// from a genuinely unchanged file.
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
    /// CRC32C of the offset-independent catalog descriptor.
    pub catalog_digest: u32,
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

/// Offset-independent identity of one sealed segment.
///
/// Two segments compare equal when their content-derived catalog descriptors
/// match, which lets a refresh report the sealed set difference without opening
/// section bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentDescriptor {
    /// Source identifier from the segment catalog.
    pub source_id: u64,
    /// Earliest timestamp in the segment.
    pub min_ts: i64,
    /// Latest timestamp in the segment.
    pub max_ts: i64,
    /// CRC32C of the offset-independent catalog descriptor.
    pub catalog_digest: u32,
}

impl SegmentDescriptor {
    /// Derives a segment descriptor from a decoded catalog.
    #[must_use]
    pub fn from_catalog(catalog: &Catalog) -> Self {
        Self {
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
    /// Generation the post-refresh journal belongs to.
    pub generation_id: JournalGenerationId,
    /// Validated journal length before this refresh.
    pub previous_valid_len: u64,
    /// Validated journal length after this refresh.
    pub new_valid_len: u64,
    /// Parts that completed since the previous scan, in journal order.
    pub completed_parts: Vec<PartDescriptor>,
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
            if current.len > previous_valid_len {
                return PartTransition::Append;
            }
            // Equal validated length under the same identity: only a byte-for-byte
            // unchanged file proves an append; a rewrite that moves the mtime is
            // not provably continuous.
            if previous.mtime_ns == current.mtime_ns {
                PartTransition::Append
            } else {
                PartTransition::Uncertain
            }
        }
    }
}

/// Derives the offset-independent CRC32C digest of a part or segment catalog.
///
/// Section body offsets are excluded so a verbatim part keeps its digest after a
/// seal that relocates bodies.
#[must_use]
pub fn catalog_digest(catalog: &Catalog) -> u32 {
    let mut bytes = Vec::with_capacity(28 + catalog.entries.len() * 24);
    bytes.extend_from_slice(&catalog.source_id.to_le_bytes());
    bytes.extend_from_slice(&catalog.min_ts.to_le_bytes());
    bytes.extend_from_slice(&catalog.max_ts.to_le_bytes());
    bytes.extend_from_slice(&catalog.format_version.to_le_bytes());
    for entry in &catalog.entries {
        bytes.extend_from_slice(&entry.type_id.to_le_bytes());
        bytes.extend_from_slice(&entry.flags.to_le_bytes());
        bytes.extend_from_slice(&entry.len.to_le_bytes());
        bytes.extend_from_slice(&entry.rows.to_le_bytes());
        bytes.extend_from_slice(&entry.crc32c.to_le_bytes());
    }
    kronika_format::crc32c(&bytes)
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

    fn identity(device: u64, inode: u64, len: u64, mtime_ns: i128) -> JournalIdentity {
        JournalIdentity {
            device,
            inode,
            len,
            mtime_ns,
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
        let first = SegmentDescriptor::from_catalog(&part_catalog(1_000, 2_000, 7));
        let later = SegmentDescriptor::from_catalog(&part_catalog(3_000, 4_000, 7));
        assert_ne!(first.catalog_digest, later.catalog_digest);
    }

    #[test]
    fn an_uncertain_refresh_requires_a_live_rebuild() {
        let delta = RefreshDelta {
            previous_view_generation: 4,
            new_view_generation: 5,
            sealed_added: Vec::new(),
            sealed_removed: Vec::new(),
            journal: JournalDelta {
                generation_id: JournalGenerationId(5),
                previous_valid_len: 100,
                new_valid_len: 100,
                completed_parts: Vec::new(),
                transition: PartTransition::Uncertain,
                tail_pending: None,
                damages: Vec::new(),
            },
        };
        assert!(delta.requires_live_rebuild());
    }
}
