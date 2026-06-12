//! End catalog and tail index.
//!
//! A reader opens a segment from the end. The last 8 bytes are the tail
//! index; it gives the byte length of the catalog block immediately before
//! it. The catalog block contains fixed-size entries followed by fixed-size
//! segment metadata.
//!
//! All integers are little-endian. Catalog entry offsets are absolute file
//! offsets from the start of the segment.
//!
//! ```text
//! catalog entry: 32 B       metadata: 40 B            tail index: 8 B
//!   type_id        u32        min_ts          i64       catalog_len u32
//!   flags          u32        max_ts          i64       magic       "PGM1"
//!   offset         u64        source_id       u64
//!   len            u64        entry_count     u32
//!   rows           u32        format_version  u32
//!   crc32c         u32        crc32c          u32
//!                             reserved        u32
//! ```

use std::error::Error;
use std::fmt;

use crate::{MAGIC, crc32c};

/// Size of one catalog entry on disk, bytes.
pub const ENTRY_LEN: usize = 32;
/// Size of the catalog meta block on disk, bytes.
pub const META_LEN: usize = 40;
/// Size of the tail index on disk, bytes. Always the last bytes of a file.
pub const TAIL_INDEX_LEN: usize = 8;

/// Offset of the `crc32c` field inside the meta block.
const META_CRC_OFFSET: usize = 32;

/// One row in the end catalog.
///
/// Each row points to one section body and records the checksum of that body.
/// A `type_id` may repeat: repeated rows are parts of one logical section in
/// catalog order, except for chart sections where repeated rows describe
/// different entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Section type from the type registry (`kronika-registry`).
    pub type_id: u32,
    /// Reserved, written as zero.
    pub flags: u32,
    /// Absolute offset of the section body from the start of the file.
    pub offset: u64,
    /// Length of the section body, bytes.
    pub len: u64,
    /// Number of rows or records in the section.
    pub rows: u32,
    /// CRC32C of the section body.
    pub crc32c: u32,
}

/// Decoded end catalog.
///
/// The catalog contains all section entries and the segment-level metadata
/// stored after those entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    /// Section table, in on-disk order. Order matters for multi-part
    /// sections, so it is preserved exactly.
    pub entries: Vec<Entry>,
    /// Minimal timestamp of the segment, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp of the segment, unix microseconds.
    pub max_ts: i64,
    /// `str_id` of `{cluster_id}/{pg_system_identifier}`; 0 = not set.
    pub source_id: u64,
    /// Container format version, [`crate::FORMAT_VERSION`] for new files.
    pub format_version: u32,
}

/// Pointer to the end catalog.
///
/// This is always the last 8 bytes of a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailIndex {
    /// Length of the catalog (entries + meta) preceding the tail index.
    pub catalog_len: u32,
}

/// Why catalog or tail index bytes failed to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The last four bytes of the tail index are not [`MAGIC`].
    BadTailMagic {
        /// The bytes actually found.
        actual: [u8; 4],
    },
    /// Catalog byte length is not `entries × 32 + 40`.
    BadCatalogLen {
        /// The byte length actually given.
        actual: usize,
    },
    /// `entry_count` in meta does not match the byte length.
    EntryCountMismatch {
        /// Entry count stored in meta.
        stored: u32,
        /// Entry count implied by the byte length.
        derived: u32,
    },
    /// Stored catalog CRC32C does not match the computed one.
    BadCrc {
        /// CRC stored in meta.
        stored: u32,
        /// CRC computed over the bytes.
        computed: u32,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadTailMagic { actual } => {
                write!(f, "tail index magic is {actual:02x?}, expected \"PGM1\"")
            }
            Self::BadCatalogLen { actual } => {
                write!(
                    f,
                    "catalog length {actual} is not entries x {ENTRY_LEN} + {META_LEN}"
                )
            }
            Self::EntryCountMismatch { stored, derived } => {
                write!(
                    f,
                    "entry_count in meta is {stored}, but byte length implies {derived}"
                )
            }
            Self::BadCrc { stored, computed } => {
                write!(
                    f,
                    "catalog crc32c mismatch: stored {stored:#010x}, computed {computed:#010x}"
                )
            }
        }
    }
}

impl Error for DecodeError {}

impl TailIndex {
    /// Encode this tail index as its 8-byte on-disk form.
    #[must_use]
    pub fn encode(self) -> [u8; TAIL_INDEX_LEN] {
        let mut out = [0_u8; TAIL_INDEX_LEN];
        out[..4].copy_from_slice(&self.catalog_len.to_le_bytes());
        out[4..].copy_from_slice(&MAGIC);
        out
    }

    /// Decode the final 8 bytes of a segment.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::BadTailMagic`] when the trailing magic bytes are
    /// not `PGM1`.
    pub fn decode(bytes: [u8; TAIL_INDEX_LEN]) -> Result<Self, DecodeError> {
        let [l0, l1, l2, l3, m0, m1, m2, m3] = bytes;
        let magic = [m0, m1, m2, m3];
        if magic != MAGIC {
            return Err(DecodeError::BadTailMagic { actual: magic });
        }
        let catalog_len = u32::from_le_bytes([l0, l1, l2, l3]);
        Ok(Self { catalog_len })
    }
}

impl Catalog {
    /// Length of the catalog block, excluding the tail index.
    ///
    /// This is the value stored as `catalog_len` in [`TailIndex`].
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        self.entries.len() * ENTRY_LEN + META_LEN
    }

    /// Encode catalog entries, metadata, and the tail index.
    ///
    /// The returned buffer starts immediately after the last section body and
    /// ends with the 8-byte tail index.
    ///
    /// # Panics
    ///
    /// Panics if the encoded catalog block does not fit in `u32`. That is a
    /// writer bug: a valid segment cannot address a larger catalog block.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let catalog_len = u32::try_from(self.encoded_len())
            .expect("catalog length must fit u32, the writer produced an absurd section count");

        let mut out = Vec::with_capacity(self.encoded_len() + TAIL_INDEX_LEN);
        for e in &self.entries {
            out.extend_from_slice(&e.type_id.to_le_bytes());
            out.extend_from_slice(&e.flags.to_le_bytes());
            out.extend_from_slice(&e.offset.to_le_bytes());
            out.extend_from_slice(&e.len.to_le_bytes());
            out.extend_from_slice(&e.rows.to_le_bytes());
            out.extend_from_slice(&e.crc32c.to_le_bytes());
        }
        out.extend_from_slice(&self.min_ts.to_le_bytes());
        out.extend_from_slice(&self.max_ts.to_le_bytes());
        out.extend_from_slice(&self.source_id.to_le_bytes());
        let entry_count =
            u32::try_from(self.entries.len()).expect("entry count fits u32, checked above");
        out.extend_from_slice(&entry_count.to_le_bytes());
        out.extend_from_slice(&self.format_version.to_le_bytes());
        // CRC is computed over the whole catalog with this field zeroed,
        // then patched in.
        let crc_at = out.len();
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes()); // reserved
        let crc = crc32c(&out);
        out[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());

        out.extend_from_slice(&TailIndex { catalog_len }.encode());
        out
    }

    /// Decode a catalog block.
    ///
    /// `bytes` must contain catalog entries followed by the 40-byte metadata
    /// block. Do not include the tail index.
    ///
    /// # Errors
    ///
    /// Returns a [`DecodeError`] when the block length is impossible, the
    /// stored entry count does not match the block length, or the catalog CRC
    /// does not match.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < META_LEN || !(bytes.len() - META_LEN).is_multiple_of(ENTRY_LEN) {
            return Err(DecodeError::BadCatalogLen {
                actual: bytes.len(),
            });
        }
        // The only possible error is overflow of an absurd length; the
        // original `TryFromIntError` carries nothing worth keeping.
        let derived = u32::try_from((bytes.len() - META_LEN) / ENTRY_LEN).map_err(|_overflow| {
            DecodeError::BadCatalogLen {
                actual: bytes.len(),
            }
        })?;

        let meta = &bytes[bytes.len() - META_LEN..];
        let stored_count = u32_at(meta, 24);
        if stored_count != derived {
            return Err(DecodeError::EntryCountMismatch {
                stored: stored_count,
                derived,
            });
        }

        // The CRC field participates in the checksum as zeroes. The copy is
        // one short-lived allocation per segment open; clarity wins over
        // an incremental-digest micro-optimization here.
        let stored_crc = u32_at(meta, META_CRC_OFFSET);
        let mut zeroed = bytes.to_vec();
        let crc_at = bytes.len() - META_LEN + META_CRC_OFFSET;
        zeroed[crc_at..crc_at + 4].fill(0);
        let computed = crc32c(&zeroed);
        if stored_crc != computed {
            return Err(DecodeError::BadCrc {
                stored: stored_crc,
                computed,
            });
        }

        let entries = bytes[..bytes.len() - META_LEN]
            .chunks_exact(ENTRY_LEN)
            .map(|c| Entry {
                type_id: u32_at(c, 0),
                flags: u32_at(c, 4),
                offset: u64_at(c, 8),
                len: u64_at(c, 16),
                rows: u32_at(c, 24),
                crc32c: u32_at(c, 28),
            })
            .collect();

        Ok(Self {
            entries,
            min_ts: i64_at(meta, 0),
            max_ts: i64_at(meta, 8),
            source_id: u64_at(meta, 16),
            format_version: u32_at(meta, 28),
        })
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("caller checked bounds"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("caller checked bounds"))
}

fn i64_at(bytes: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(bytes[at..at + 8].try_into().expect("caller checked bounds"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Catalog {
        Catalog {
            entries: vec![Entry {
                type_id: 1_006_001,
                flags: 0,
                offset: 4,
                len: 4,
                rows: 1,
                crc32c: 0x2930_8CF4,
            }],
            min_ts: 1_000_000,
            max_ts: 2_000_000,
            source_id: 0,
            format_version: 1,
        }
    }

    #[test]
    fn tail_index_roundtrip() {
        let tail = TailIndex { catalog_len: 72 };
        assert_eq!(TailIndex::decode(tail.encode()), Ok(tail));
    }

    #[test]
    fn tail_index_rejects_bad_magic() {
        let mut bytes = TailIndex { catalog_len: 72 }.encode();
        bytes[5] ^= 0xFF;
        assert!(matches!(
            TailIndex::decode(bytes),
            Err(DecodeError::BadTailMagic { .. })
        ));
    }

    #[test]
    fn catalog_roundtrip() {
        let catalog = sample();
        let encoded = catalog.encode();
        let body = &encoded[..encoded.len() - TAIL_INDEX_LEN];
        assert_eq!(Catalog::decode(body), Ok(catalog));
    }

    #[test]
    fn empty_catalog_roundtrip() {
        let catalog = Catalog {
            entries: vec![],
            min_ts: 0,
            max_ts: 0,
            source_id: 0,
            format_version: 1,
        };
        let encoded = catalog.encode();
        let body = &encoded[..encoded.len() - TAIL_INDEX_LEN];
        assert_eq!(Catalog::decode(body), Ok(catalog));
    }

    #[test]
    fn decode_rejects_wrong_length() {
        assert!(matches!(
            Catalog::decode(&[0_u8; META_LEN + 1]),
            Err(DecodeError::BadCatalogLen { .. })
        ));
        assert!(matches!(
            Catalog::decode(&[0_u8; META_LEN - 1]),
            Err(DecodeError::BadCatalogLen { .. })
        ));
    }

    #[test]
    fn decode_rejects_entry_count_mismatch() {
        let encoded = sample().encode();
        let mut body = encoded[..encoded.len() - TAIL_INDEX_LEN].to_vec();
        // Patch entry_count from 1 to 2; offset 24 within meta.
        let at = body.len() - META_LEN + 24;
        body[at..at + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert_eq!(
            Catalog::decode(&body),
            Err(DecodeError::EntryCountMismatch {
                stored: 2,
                derived: 1
            })
        );
    }

    #[test]
    fn decode_rejects_corrupted_byte() {
        let encoded = sample().encode();
        let mut body = encoded[..encoded.len() - TAIL_INDEX_LEN].to_vec();
        body[0] ^= 0x01;
        assert!(matches!(
            Catalog::decode(&body),
            Err(DecodeError::BadCrc { .. })
        ));
    }
}
