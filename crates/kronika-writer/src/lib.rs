//! Writer-side state for building PGM segments.
//!
//! The implemented pieces today are the per-segment string interner and the
//! `active.parts` journal. Later steps add per-type buffers, merge, seal, and
//! Parquet encoding.

mod interner;
mod journal;

pub use interner::{FlushedEntry, Interner, SealedSegment};
pub use journal::{Journal, JournalError, OpenReport};
