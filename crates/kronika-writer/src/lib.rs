//! Writer-side state for building PGM segments.
//!
//! The crate currently contains the per-segment string interner and the
//! `active.parts` journal. Later steps add per-type buffers, part merging,
//! segment completion, and Parquet encoding.

mod interner;
mod journal;

pub use interner::{FlushedEntry, Interner, SealedSegment};
pub use journal::{DEFAULT_MAX_JOURNAL_LEN, Journal, JournalConfig, JournalError, OpenReport};

#[cfg(test)]
mod composition_tests {
    //! A part assembled by `kronika_format::build_part` survives the
    //! file-backed journal: append, persist, re-read, and re-validate. The
    //! first proof that the part builder and the `active.parts` journal meet on
    //! the real file path; bodies stay opaque, as in the format crate.

    use kronika_format::{PartMeta, SectionInput, build_part, validate_part};

    use crate::{Journal, JournalConfig};

    #[test]
    fn a_built_part_survives_the_file_journal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("active.parts");

        let part = build_part(
            &[
                SectionInput {
                    type_id: 1_006_001,
                    rows: 2,
                    body: b"bgwriter-section-body",
                },
                SectionInput {
                    type_id: 1_021_001,
                    rows: 1,
                    body: b"instance-metadata-body",
                },
            ],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );

        let (mut journal, report) = Journal::open(&path, JournalConfig::default()).expect("open");
        assert!(report.is_clean(), "a fresh journal opens clean");
        let part_ref = journal.append(&part).expect("append a valid part");

        let read_back = journal.read_part(part_ref).expect("read the part back");
        assert_eq!(read_back, part, "the journal returns the bytes appended");

        let catalog = validate_part(&read_back).expect("the persisted part validates");
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.entries[0].type_id, 1_006_001);
        assert_eq!(catalog.source_id, 7);
    }
}
