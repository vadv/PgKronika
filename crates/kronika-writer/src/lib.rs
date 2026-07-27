//! Bounded writer state, journal recovery, and PGM sealing.
//!
//! [`SectionBuffers`] accepts registered rows until the registry row cap,
//! encodes one collection window, and places data sections before dictionary
//! sections. [`Interner`] owns the current segment's dictionary: unflushed
//! values retain their bytes, while flushed values retain only identity and
//! placement metadata for deduplication.
//!
//! [`Journal`] appends self-contained PGM parts as synchronized `PGMP` frames
//! after a checksummed version-1 header in `active.parts`. Opening validates
//! the complete header and body without repairing or truncating damage.
//! [`JournalConfig::max_journal_len`] is the hard growth bound, reported as
//! [`JournalError::Full`] so the collector can seal early.
//!
//! [`seal`] validates and decodes journal bodies, coalesces each registered
//! type, normalizes dictionaries, and emits canonical Parquet 1.0 bodies with
//! PLAIN values and Zstandard level 6. It writes a temporary file in the
//! segment's UTC day and publishes without overwriting another identity.
//! Recovery accepts an existing final file only after exact comparison. Seal
//! never resets the journal, so the caller does so only after `Ok`.

mod buffer;
pub mod dict;
mod interner;
mod journal;
mod segment;

pub use buffer::{FlushSummary, FlushedPart, SectionBuffers, SectionFlushSummary};
pub use interner::{FlushedEntry, Interner, SealedSegment};
pub use journal::{Journal, JournalConfig, JournalError, JournalPartRef};
pub use kronika_format::{MAX_JOURNAL_LEN, MAX_JOURNAL_PARTS, MAX_PART_LEN};
pub use segment::{SealError, SealSummary, seal};

#[cfg(test)]
use kronika_reader as _;

#[cfg(test)]
mod composition_tests {
    //! Cross-crate check: a part built by `kronika-format` survives the
    //! file-backed journal unchanged.

    use kronika_format::{PartMeta, SectionInput, build_part, validate_part};
    use kronika_layout::{DataRoot, LayoutLimits, SegmentId};

    use crate::{Journal, JournalConfig};

    #[test]
    fn a_built_part_survives_the_file_journal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = DataRoot::open(dir.path()).expect("open root");
        let owner = root
            .acquire_writer(LayoutLimits::default())
            .expect("acquire writer");
        let segment_id = SegmentId::new(1_709_164_800_000_000).expect("segment id");

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

        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open");
        let part_ref = journal
            .append(segment_id, &part)
            .expect("append a valid part");

        let read_back = journal.read_part(part_ref).expect("read the part back");
        assert_eq!(read_back, part, "the journal returns the bytes appended");

        let catalog = validate_part(&read_back).expect("the persisted part validates");
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.entries[0].type_id, 1_006_001);
        assert_eq!(catalog.source_id, 7);
    }
}
