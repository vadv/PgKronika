//! Semantic deltas for incremental store scans.

use std::sync::Arc;

use kronika_format::{Catalog, DamageRegion};
use kronika_layout::{FileIdentity, SegmentId};
pub use kronika_store::CatalogDigest;
use kronika_store::{CatalogLayoutDigest, CatalogSummary};

/// Stable logical identity of one sealed segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SealedLocator(SegmentId);

impl SealedLocator {
    /// Creates a locator from a verified layout identity.
    #[must_use]
    pub const fn from_segment_id(segment_id: SegmentId) -> Self {
        Self(segment_id)
    }

    /// Returns the segment identity.
    #[must_use]
    pub const fn segment_id(self) -> SegmentId {
        self.0
    }

    /// Returns a canonical signed-integer byte representation for hashing.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 8] {
        self.0.get().to_be_bytes()
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
/// The locator identifies `SegmentId`, while the catalog digest identifies its
/// content without depending on section-body offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentDescriptor {
    /// Stable identity derived from the verified segment id.
    pub locator: SealedLocator,
    /// Source identifier from the segment catalog.
    pub source_id: u64,
    /// Earliest timestamp in the segment.
    pub min_ts: i64,
    /// Latest timestamp in the segment.
    pub max_ts: i64,
    /// SHA-256 identity of the offset-independent catalog descriptor.
    pub catalog_digest: CatalogDigest,
    /// SHA-256 identity of the ordered catalog fields including body offsets.
    pub catalog_layout_digest: CatalogLayoutDigest,
    /// Filesystem identity pinned by the scan that produced this descriptor.
    pub file_identity: FileIdentity,
}

impl SegmentDescriptor {
    /// Derives a segment descriptor from compact validated PGM metadata.
    #[must_use]
    pub const fn from_summary(
        locator: SealedLocator,
        file_identity: FileIdentity,
        summary: &CatalogSummary,
    ) -> Self {
        Self {
            locator,
            source_id: summary.source_id,
            min_ts: summary.min_ts,
            max_ts: summary.max_ts,
            catalog_digest: summary.logical_digest,
            catalog_layout_digest: summary.layout_digest,
            file_identity,
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
    pub completed_parts: Arc<[PartDescriptor]>,
    /// Every valid part in the post-refresh journal, in journal order.
    ///
    /// This is the authoritative completion target when
    /// [`current_parts_complete`](Self::current_parts_complete) is `true`.
    pub current_parts: Arc<[PartDescriptor]>,
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

/// Applies the logical committed-reset boundary to an already verified
/// filesystem/prefix transition.
///
/// `base` must include every continuity check available to the caller, notably
/// the validated-prefix comparison for a same-inode growth. This function only
/// resolves the states that file length and timestamps cannot classify:
///
/// - an active baseline entering any committed marker phase is one
///   [`Reset`](PartTransition::Reset);
/// - repeated phases of the same marker, marker cleanup to canonical empty, and
///   the first post-reset append on the same file preserve that new generation;
/// - a different committed marker on the same file is another reset;
/// - a replacement is never reclassified as a continuation.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalPhase {
    Empty,
    Active,
    CommittedReset,
}

#[must_use]
pub(crate) const fn apply_committed_reset_transition(
    base: PartTransition,
    same_file: bool,
    previous: JournalPhase,
    current: JournalPhase,
    same_committed_reset: bool,
) -> PartTransition {
    if matches!(base, PartTransition::Replaced) || !same_file {
        return base;
    }
    if matches!(current, JournalPhase::CommittedReset) {
        return if matches!(previous, JournalPhase::CommittedReset) && same_committed_reset {
            PartTransition::Append
        } else {
            PartTransition::Reset
        };
    }
    match (previous, current) {
        (JournalPhase::CommittedReset, _) => PartTransition::Append,
        _ => base,
    }
}

/// Derives the offset-independent SHA-256 identity of a catalog.
///
/// Section body offsets are excluded so a verbatim part keeps its digest after a
/// seal that relocates bodies.
///
/// # Panics
///
/// Panics only for an in-memory [`Catalog`] with more entries than the PGM v1
/// `u32` entry-count field can represent. Decoded catalogs are bounded by that
/// field and cannot trigger this condition.
#[must_use]
pub fn catalog_digest(catalog: &Catalog) -> CatalogDigest {
    CatalogDigest::from_catalog(catalog)
}

/// Derives the idempotency key of a completed part.
#[must_use]
pub fn part_id(
    generation: JournalGenerationId,
    frame_offset: u64,
    body_len: u64,
    catalog: &Catalog,
) -> PartId {
    part_id_from_digest(generation, frame_offset, body_len, catalog_digest(catalog))
}

pub(crate) const fn part_id_from_digest(
    generation: JournalGenerationId,
    frame_offset: u64,
    body_len: u64,
    catalog_digest: CatalogDigest,
) -> PartId {
    PartId {
        generation,
        frame_offset,
        body_len,
        catalog_digest,
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

    fn segment_descriptor(locator: SealedLocator, catalog: &Catalog) -> SegmentDescriptor {
        let summary = CatalogSummary::from_catalog(
            catalog,
            u32::try_from(catalog.encoded_len()).expect("test catalog length fits u32"),
        );
        SegmentDescriptor::from_summary(
            locator,
            FileIdentity {
                device: 1,
                inode: 2,
                len: 3,
                mtime_seconds: 4,
                mtime_nanoseconds: 5,
                ctime_seconds: 6,
                ctime_nanoseconds: 7,
            },
            &summary,
        )
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

    fn advance_generation(generation: u64, transition: PartTransition) -> u64 {
        generation + u64::from(!transition.preserves_generation())
    }

    #[test]
    fn active_to_committed_reset_mints_exactly_one_generation() {
        let generation = 7;
        let transition = apply_committed_reset_transition(
            PartTransition::Append,
            true,
            JournalPhase::Active,
            JournalPhase::CommittedReset,
            false,
        );

        assert_eq!(transition, PartTransition::Reset);
        assert_eq!(advance_generation(generation, transition), generation + 1);
    }

    #[test]
    fn committed_marker_header_phases_do_not_mint_again() {
        let generation_after_reset = 8;
        let phases = [
            ("previous", PartTransition::Append),
            ("empty", PartTransition::Uncertain),
            ("torn", PartTransition::Uncertain),
        ];

        for (phase, base) in phases {
            let transition = apply_committed_reset_transition(
                base,
                true,
                JournalPhase::CommittedReset,
                JournalPhase::CommittedReset,
                true,
            );
            assert_eq!(
                transition,
                PartTransition::Append,
                "{phase} marker phase must continue the post-reset generation"
            );
            assert_eq!(
                advance_generation(generation_after_reset, transition),
                generation_after_reset,
                "{phase} marker phase must not mint a second generation"
            );
        }
    }

    #[test]
    fn committed_marker_cleanup_and_first_active_part_are_continuations() {
        let marker_to_empty = apply_committed_reset_transition(
            PartTransition::Reset,
            true,
            JournalPhase::CommittedReset,
            JournalPhase::Empty,
            false,
        );
        let marker_to_active = apply_committed_reset_transition(
            PartTransition::Uncertain,
            true,
            JournalPhase::CommittedReset,
            JournalPhase::Active,
            false,
        );

        assert_eq!(marker_to_empty, PartTransition::Append);
        assert_eq!(marker_to_active, PartTransition::Append);
    }

    #[test]
    fn empty_or_absent_to_first_active_part_is_an_append() {
        let active = identity(1, 2, 240, 20);
        let from_absent = classify_transition(None, Some(active), 0);
        assert_eq!(
            apply_committed_reset_transition(
                from_absent,
                false,
                JournalPhase::Empty,
                JournalPhase::Active,
                false,
            ),
            PartTransition::Append
        );

        let empty_len = kronika_format::JOURNAL_HEADER_LEN as u64;
        let empty = identity(1, 2, empty_len, 10);
        let from_empty = classify_transition(Some(empty), Some(active), empty_len);
        assert_eq!(
            apply_committed_reset_transition(
                from_empty,
                true,
                JournalPhase::Empty,
                JournalPhase::Active,
                false,
            ),
            PartTransition::Append
        );
    }

    #[test]
    fn active_rewrite_without_committed_boundary_stays_non_continuous() {
        for base in [PartTransition::Reset, PartTransition::Uncertain] {
            assert_eq!(
                apply_committed_reset_transition(
                    base,
                    true,
                    JournalPhase::Active,
                    JournalPhase::Active,
                    false,
                ),
                base
            );
        }
        assert_eq!(
            apply_committed_reset_transition(
                PartTransition::Replaced,
                false,
                JournalPhase::Active,
                JournalPhase::CommittedReset,
                false,
            ),
            PartTransition::Replaced,
            "a replacement must not inherit committed-reset continuity"
        );
    }

    #[test]
    fn a_different_committed_marker_mints_another_generation() {
        let transition = apply_committed_reset_transition(
            PartTransition::Uncertain,
            true,
            JournalPhase::CommittedReset,
            JournalPhase::CommittedReset,
            false,
        );

        assert_eq!(transition, PartTransition::Reset);
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
        let locator = SealedLocator::from_segment_id(SegmentId::new(1_000).unwrap());
        let first = segment_descriptor(locator, &part_catalog(1_000, 2_000, 7));
        let later = segment_descriptor(locator, &part_catalog(3_000, 4_000, 7));
        assert_ne!(first.catalog_digest, later.catalog_digest);
    }

    #[test]
    fn identical_catalogs_under_distinct_names_have_distinct_locators() {
        let catalog = part_catalog(1_000, 2_000, 7);
        let first = segment_descriptor(
            SealedLocator::from_segment_id(SegmentId::new(1_000).unwrap()),
            &catalog,
        );
        let alias = segment_descriptor(
            SealedLocator::from_segment_id(SegmentId::new(2_000).unwrap()),
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
                completed_parts: Arc::from([]),
                current_parts: Arc::from([]),
                current_parts_complete: true,
                transition: PartTransition::Uncertain,
                tail_pending: None,
                damages: Vec::new(),
            },
        };
        assert!(delta.requires_live_rebuild());
    }
}
