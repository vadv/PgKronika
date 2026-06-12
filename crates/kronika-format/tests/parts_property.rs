//! Property tests for the `active.parts` journal.
//!
//! The tests check that truncation keeps the fully written prefix, and that a
//! single corrupted byte cannot make a part disappear without a reported
//! damaged region.

use kronika_format::{
    Catalog, DamageKind, Entry, FORMAT_VERSION, FrameHeader, JournalLimits, MAGIC, crc32c,
    scan_journal,
};
use proptest::prelude::*;

// Dependencies of other targets of this crate; anchored for the
// `unused_crate_dependencies` lint, which checks each target separately.
use crc as _;
use sha2 as _;
use xxhash_rust as _;

const fn limits() -> JournalLimits {
    JournalLimits {
        max_part_len: 1 << 20,
    }
}

/// Build a valid mini-PGM part from random section bodies.
fn build_part(sections: &[Vec<u8>]) -> Vec<u8> {
    let mut part = Vec::new();
    part.extend_from_slice(&MAGIC);
    let mut entries = Vec::new();
    for (i, body) in sections.iter().enumerate() {
        entries.push(Entry {
            type_id: 1_000_001 + u32::try_from(i).expect("few sections"),
            flags: 0,
            offset: part.len() as u64,
            len: body.len() as u64,
            rows: 1,
            crc32c: crc32c(body),
        });
        part.extend_from_slice(body);
    }
    let catalog = Catalog {
        entries,
        min_ts: 1,
        max_ts: 2,
        source_id: 0,
        format_version: FORMAT_VERSION,
    };
    part.extend_from_slice(&catalog.encode());
    part
}

fn frame(part: &[u8]) -> Vec<u8> {
    let mut out = FrameHeader {
        part_len: part.len() as u64,
    }
    .encode()
    .to_vec();
    out.extend_from_slice(part);
    out
}

/// A journal of 1..6 random parts, returned with its frame boundaries.
fn journal_strategy() -> impl Strategy<Value = (Vec<u8>, Vec<usize>)> {
    let section = proptest::collection::vec(any::<u8>(), 1..48);
    let part_sections = proptest::collection::vec(section, 0..3);
    proptest::collection::vec(part_sections, 1..6).prop_map(|parts| {
        let mut journal = Vec::new();
        let mut boundaries = Vec::new();
        for sections in &parts {
            let part = build_part(sections);
            journal.extend_from_slice(&frame(&part));
            boundaries.push(journal.len());
        }
        (journal, boundaries)
    })
}

proptest! {
    /// A clean journal yields every part and no damaged regions.
    #[test]
    fn clean_journal_round_trips((journal, boundaries) in journal_strategy()) {
        let report = scan_journal(&journal, limits());
        prop_assert!(report.is_clean());
        prop_assert_eq!(report.parts.len(), boundaries.len());
        prop_assert_eq!(report.valid_len, journal.len());
    }

    /// Truncation at an arbitrary offset loses at most the cut frame.
    #[test]
    fn truncation_recovers_the_full_prefix(
        (journal, boundaries) in journal_strategy(),
        cut in any::<proptest::sample::Index>(),
    ) {
        let cut = cut.index(journal.len());
        let report = scan_journal(&journal[..cut], limits());

        let full_frames_before = boundaries.iter().filter(|&&b| b <= cut).count();
        prop_assert_eq!(report.parts.len(), full_frames_before);

        let cut_is_a_boundary = cut == 0 || boundaries.contains(&cut);
        if cut_is_a_boundary {
            prop_assert!(report.is_clean());
        } else {
            prop_assert_eq!(report.damages.len(), 1);
            prop_assert_eq!(report.damages[0].kind, DamageKind::TornTail);
        }
    }

    /// Flipping one byte either keeps all parts visible or reports damage.
    ///
    /// Parts before the corrupted byte must still be recovered.
    #[test]
    fn single_byte_corruption_is_reported(
        (journal, boundaries) in journal_strategy(),
        position in any::<proptest::sample::Index>(),
        flip in 1_u8..=255,
    ) {
        let position = position.index(journal.len());
        let mut corrupted = journal;
        corrupted[position] ^= flip;

        let report = scan_journal(&corrupted, limits());

        if report.parts.len() < boundaries.len() {
            prop_assert!(
                !report.damages.is_empty(),
                "a part disappeared without reported damage"
            );
        }

        // Frames strictly before the corrupted byte are intact and must
        // all be recovered regardless of what happens after.
        let intact_before = boundaries.iter().filter(|&&b| b <= position).count();
        prop_assert!(report.parts.len() >= intact_before);

        // A single flipped byte damages at most one frame.
        prop_assert!(report.parts.len() + 1 >= boundaries.len());
    }
}
