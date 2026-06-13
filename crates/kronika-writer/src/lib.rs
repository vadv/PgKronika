//! Writer-side state for building PGM segments.
//!
//! The crate currently contains the per-segment string interner and the
//! `active.parts` journal. Later steps add per-type buffers, part merging,
//! segment completion, and Parquet encoding.

mod interner;
mod journal;

pub use interner::{FlushedEntry, Interner, SealedSegment};
pub use journal::{DEFAULT_MAX_JOURNAL_LEN, Journal, JournalConfig, JournalError, OpenReport};
