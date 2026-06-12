//! PGM container primitives: end catalog, tail index, CRC32C, `str_id`,
//! dictionaries.
//!
//! This crate owns the byte layout of the PGM container and nothing else:
//! no Parquet, no I/O, no knowledge of section contents. The layout and
//! the contracts are documented in this crate's README.md; every
//! structure here points back to its README section. HOT block headers
//! and `active.parts` frames will be added in later steps.
//!
//! Reading a segment starts from the end of the file:
//!
//! 1. [`TailIndex::decode`] on the last [`TAIL_INDEX_LEN`] bytes gives the
//!    catalog length and validates the magic.
//! 2. [`Catalog::decode`] on the `catalog_len` bytes right before the tail
//!    index gives the section table and validates its CRC.
//! 3. Section bodies are then read by `offset`/`len` from catalog entries.

mod catalog;
mod crc;
mod dictionary;
mod str_id;

// proptest is used by tests/property.rs only; anchored for the
// `unused_crate_dependencies` lint, which checks each target separately.
#[cfg(test)]
use proptest as _;

pub use catalog::{Catalog, DecodeError, ENTRY_LEN, Entry, META_LEN, TAIL_INDEX_LEN, TailIndex};
pub use crc::crc32c;
pub use dictionary::{
    BlobEntry, DEFAULT_BLOB_THRESHOLD, DEFAULT_TRUNCATE_LIMIT, DictError, DictLimits, DictStats,
    InvalidLimits, Resolved, SegmentDicts,
};
pub use str_id::StrId;

/// Magic bytes `PGM1`. They open the file and close the tail index, so both
/// the first and the last four bytes of a segment are recognizable
/// (README.md, "File Layout").
pub const MAGIC: [u8; 4] = *b"PGM1";

/// Current container format version, stored in the catalog meta. Changes
/// only when the container itself changes; data evolves through new type
/// ids instead (README.md, "Catalog Metadata").
pub const FORMAT_VERSION: u32 = 1;
