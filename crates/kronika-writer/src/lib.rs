//! Bounded writer state, journal recovery, and PGM sealing.
//!
//! [`SectionBuffers`] accepts registered rows until the registry row cap,
//! encodes one collection window, and places data sections before dictionary
//! sections. [`Interner`] owns the current segment's dictionary: unflushed
//! values retain their bytes, while flushed values retain only identity and
//! placement metadata for deduplication.
//!
//! [`Journal`] appends self-contained PGM parts as synchronized `PGMP` frames
//! in `active.parts`. Opening a journal truncates only a torn final frame;
//! middle or terminal media damage remains in [`OpenReport`].
//! [`JournalConfig::max_journal_len`] is the hard growth bound, reported as
//! [`JournalError::Full`] so the collector can seal early.
//!
//! [`seal`] streams one journal part at a time into a sibling temporary file,
//! writes and synchronizes the end catalog, and publishes the result without
//! overwriting an existing destination. It never resets the journal; the
//! caller does so only after a successful seal.

mod buffer;
pub mod dict;
mod interner;
mod journal;
mod segment;

pub use buffer::{FlushSummary, FlushedPart, SectionBuffers, SectionFlushSummary};
pub use interner::{FlushedEntry, Interner, SealedSegment};
pub use journal::{DEFAULT_MAX_JOURNAL_LEN, Journal, JournalConfig, JournalError, OpenReport};
pub use segment::{SealError, SealSummary, seal};

#[cfg(test)]
mod composition_tests {
    //! Cross-crate check: a part built by `kronika-format` survives the
    //! file-backed journal unchanged.

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
