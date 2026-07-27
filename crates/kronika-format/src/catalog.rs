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
use std::io::{self, Write};

use crate::{MAGIC, crc::Crc32c};

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
/// Physical readers additionally require the canonical ordering checked by
/// [`validate_catalog_layout`].
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
    /// Section table in canonical physical order.
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

/// Validated borrowed view of an encoded end catalog.
///
/// Unlike [`Catalog`], this type does not allocate or retain decoded entries.
/// It is intended for bounded discovery paths that need to validate and
/// summarize a catalog before deciding whether to open the complete segment.
#[derive(Debug, Clone, Copy)]
pub struct CatalogView<'a> {
    entries: &'a [u8],
    /// Minimal timestamp of the segment, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp of the segment, unix microseconds.
    pub max_ts: i64,
    /// `str_id` of `{cluster_id}/{pg_system_identifier}`; 0 = not set.
    pub source_id: u64,
    /// Number of catalog entries.
    pub entry_count: u32,
    /// Container format version.
    pub format_version: u32,
}

impl CatalogView<'_> {
    /// Iterates over decoded entries without allocating a `Vec<Entry>`.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = Entry> + Clone + '_ {
        self.entries.chunks_exact(ENTRY_LEN).map(decode_entry)
    }
}

/// Pointer to the end catalog.
///
/// This is always the last 8 bytes of a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailIndex {
    /// Length of the catalog (entries + meta) preceding the tail index.
    pub catalog_len: u32,
}

/// Why a decoded catalog is not the canonical physical section layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogLayoutError {
    type_id: Option<u32>,
    reason: &'static str,
}

impl CatalogLayoutError {
    const fn entry(type_id: u32, reason: &'static str) -> Self {
        Self {
            type_id: Some(type_id),
            reason,
        }
    }

    const fn container(reason: &'static str) -> Self {
        Self {
            type_id: None,
            reason,
        }
    }
}

impl fmt::Display for CatalogLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(type_id) = self.type_id {
            write!(f, "section {type_id}: {}", self.reason)
        } else {
            f.write_str(self.reason)
        }
    }
}

impl Error for CatalogLayoutError {}

const DICT_STRINGS_TYPE_ID: u32 = 3_001_001;
const DICT_BLOBS_TYPE_ID: u32 = 3_002_001;
const MAX_PHYSICAL_SECTION_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PHYSICAL_SECTION_ROWS: u32 = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SectionOrder {
    Data(u32),
    Strings,
    Blobs,
}

/// Validate the canonical physical section inventory and exact body geometry.
///
/// Data sections must be unique and ordered by ascending `type_id`, followed
/// by at most one strings dictionary and at most one blobs dictionary. Bodies
/// must be contiguous from the opening magic through `catalog_at`, with zero
/// flags and bounded lengths and row counts.
///
/// # Errors
///
/// Returns [`CatalogLayoutError`] for any non-canonical entry or body range.
pub fn validate_catalog_layout(
    catalog: &Catalog,
    catalog_at: u64,
) -> Result<(), CatalogLayoutError> {
    let mut expected_offset = MAGIC.len() as u64;
    let mut previous: Option<SectionOrder> = None;

    for entry in &catalog.entries {
        if entry.flags != 0 {
            return Err(CatalogLayoutError::entry(
                entry.type_id,
                "reserved flags are not zero",
            ));
        }
        if entry.rows == 0 {
            return Err(CatalogLayoutError::entry(
                entry.type_id,
                "populated section has zero rows",
            ));
        }
        if entry.len == 0 {
            return Err(CatalogLayoutError::entry(
                entry.type_id,
                "section body is empty",
            ));
        }
        if entry.len > MAX_PHYSICAL_SECTION_BYTES {
            return Err(CatalogLayoutError::entry(
                entry.type_id,
                "body length is above the physical cap",
            ));
        }
        if entry.rows > MAX_PHYSICAL_SECTION_ROWS {
            return Err(CatalogLayoutError::entry(
                entry.type_id,
                "row count is above the physical cap",
            ));
        }
        if entry.offset != expected_offset {
            return Err(CatalogLayoutError::entry(
                entry.type_id,
                "body is not contiguous with the preceding section",
            ));
        }

        let order = match entry.type_id {
            DICT_STRINGS_TYPE_ID => SectionOrder::Strings,
            DICT_BLOBS_TYPE_ID => SectionOrder::Blobs,
            type_id => SectionOrder::Data(type_id),
        };
        if let Some(previous_order) = previous {
            if order == previous_order {
                return Err(CatalogLayoutError::entry(
                    entry.type_id,
                    "section type occurs more than once",
                ));
            }
            if order < previous_order {
                return Err(CatalogLayoutError::entry(
                    entry.type_id,
                    "section is out of canonical order",
                ));
            }
        }
        previous = Some(order);
        expected_offset = entry.offset.checked_add(entry.len).ok_or_else(|| {
            CatalogLayoutError::entry(entry.type_id, "body range overflows the container")
        })?;
    }

    if expected_offset != catalog_at {
        return Err(CatalogLayoutError::container(
            "section bodies do not end at the catalog start",
        ));
    }
    Ok(())
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
        let mut out = Vec::with_capacity(self.encoded_len() + TAIL_INDEX_LEN);
        self.write_encoded(&mut out)
            .expect("catalog length must fit u32 and writing to Vec cannot fail");
        out
    }

    /// Write catalog entries, metadata, and the tail index without allocating
    /// a second catalog-sized buffer.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] if the catalog block does not
    /// fit in `u32`, or forwards an error from `writer`.
    pub fn write_encoded(&self, mut writer: impl Write) -> io::Result<()> {
        let catalog_len = u32::try_from(self.encoded_len()).map_err(|_overflow| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "catalog length does not fit the PGM tail index",
            )
        })?;
        let mut checksum = Crc32c::new();
        for e in &self.entries {
            let mut bytes = [0_u8; ENTRY_LEN];
            bytes[0..4].copy_from_slice(&e.type_id.to_le_bytes());
            bytes[4..8].copy_from_slice(&e.flags.to_le_bytes());
            bytes[8..16].copy_from_slice(&e.offset.to_le_bytes());
            bytes[16..24].copy_from_slice(&e.len.to_le_bytes());
            bytes[24..28].copy_from_slice(&e.rows.to_le_bytes());
            bytes[28..32].copy_from_slice(&e.crc32c.to_le_bytes());
            checksum.update(&bytes);
            writer.write_all(&bytes)?;
        }

        let mut meta = [0_u8; META_LEN];
        meta[0..8].copy_from_slice(&self.min_ts.to_le_bytes());
        meta[8..16].copy_from_slice(&self.max_ts.to_le_bytes());
        meta[16..24].copy_from_slice(&self.source_id.to_le_bytes());
        let entry_count = u32::try_from(self.entries.len()).map_err(|_overflow| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "catalog entry count does not fit the PGM metadata",
            )
        })?;
        meta[24..28].copy_from_slice(&entry_count.to_le_bytes());
        meta[28..32].copy_from_slice(&self.format_version.to_le_bytes());
        // The CRC field and reserved field are already zeroed.
        checksum.update(&meta);
        meta[META_CRC_OFFSET..META_CRC_OFFSET + 4]
            .copy_from_slice(&checksum.finalize().to_le_bytes());
        writer.write_all(&meta)?;
        writer.write_all(&TailIndex { catalog_len }.encode())
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
        let view = Self::view(bytes)?;
        Ok(Self {
            entries: view.entries().collect(),
            min_ts: view.min_ts,
            max_ts: view.max_ts,
            source_id: view.source_id,
            format_version: view.format_version,
        })
    }

    /// Validate an encoded catalog and borrow its fixed-size entries.
    ///
    /// `bytes` must contain catalog entries followed by the 40-byte metadata
    /// block. The returned view decodes entries on iteration and allocates
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns a [`DecodeError`] under the same conditions as [`Self::decode`].
    pub fn view(bytes: &[u8]) -> Result<CatalogView<'_>, DecodeError> {
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

        // The CRC field participates in the checksum as zeroes. Computed
        // incrementally: decode runs once per part during journal recovery,
        // and cloning the whole catalog block here would double the peak
        // memory of that path.
        let stored_crc = u32_at(meta, META_CRC_OFFSET);
        let crc_at = bytes.len() - META_LEN + META_CRC_OFFSET;
        let computed = crate::crc::crc32c_with_zeroed_field(bytes, crc_at);
        if stored_crc != computed {
            return Err(DecodeError::BadCrc {
                stored: stored_crc,
                computed,
            });
        }

        Ok(CatalogView {
            entries: &bytes[..bytes.len() - META_LEN],
            min_ts: i64_at(meta, 0),
            max_ts: i64_at(meta, 8),
            source_id: u64_at(meta, 16),
            entry_count: stored_count,
            format_version: u32_at(meta, 28),
        })
    }
}

fn decode_entry(bytes: &[u8]) -> Entry {
    Entry {
        type_id: u32_at(bytes, 0),
        flags: u32_at(bytes, 4),
        offset: u64_at(bytes, 8),
        len: u64_at(bytes, 16),
        rows: u32_at(bytes, 24),
        crc32c: u32_at(bytes, 28),
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
    fn streaming_encoder_matches_the_in_memory_encoding() {
        let catalog = sample();
        let expected = catalog.encode();
        let mut streamed = Vec::new();
        catalog
            .write_encoded(&mut streamed)
            .expect("write catalog to memory");
        assert_eq!(streamed, expected);
    }

    #[test]
    fn borrowed_view_matches_owned_decode_without_allocating_entries() {
        let catalog = sample();
        let encoded = catalog.encode();
        let body = &encoded[..encoded.len() - TAIL_INDEX_LEN];
        let view = Catalog::view(body).expect("valid borrowed view");

        assert_eq!(view.min_ts, catalog.min_ts);
        assert_eq!(view.max_ts, catalog.max_ts);
        assert_eq!(view.source_id, catalog.source_id);
        assert_eq!(view.entry_count, 1);
        assert_eq!(view.format_version, catalog.format_version);
        assert_eq!(view.entries().collect::<Vec<_>>(), catalog.entries);
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

    fn layout_catalog(entries: Vec<Entry>) -> Catalog {
        Catalog {
            entries,
            min_ts: 0,
            max_ts: 0,
            source_id: 0,
            format_version: crate::FORMAT_VERSION,
        }
    }

    const fn layout_entry(type_id: u32, offset: u64, len: u64) -> Entry {
        Entry {
            type_id,
            flags: 0,
            offset,
            len,
            rows: 1,
            crc32c: 0,
        }
    }

    #[test]
    fn canonical_layout_accepts_data_then_dictionary_tail() {
        let catalog = layout_catalog(vec![
            layout_entry(1_006_001, 4, 2),
            layout_entry(1_021_001, 6, 3),
            layout_entry(DICT_STRINGS_TYPE_ID, 9, 1),
            layout_entry(DICT_BLOBS_TYPE_ID, 10, 2),
        ]);

        assert_eq!(validate_catalog_layout(&catalog, 12), Ok(()));
    }

    #[test]
    fn canonical_layout_rejects_duplicate_or_misordered_sections() {
        let duplicate = layout_catalog(vec![
            layout_entry(1_006_001, 4, 1),
            layout_entry(1_006_001, 5, 1),
        ]);
        assert!(validate_catalog_layout(&duplicate, 6).is_err());

        let misordered = layout_catalog(vec![
            layout_entry(DICT_STRINGS_TYPE_ID, 4, 1),
            layout_entry(1_006_001, 5, 1),
        ]);
        assert!(validate_catalog_layout(&misordered, 6).is_err());
    }

    #[test]
    fn canonical_layout_rejects_flags_caps_and_noncontiguous_bodies() {
        let mut flagged = layout_catalog(vec![layout_entry(1_006_001, 4, 1)]);
        flagged.entries[0].flags = 1;
        assert!(validate_catalog_layout(&flagged, 5).is_err());

        let mut too_many_rows = layout_catalog(vec![layout_entry(1_006_001, 4, 1)]);
        too_many_rows.entries[0].rows = MAX_PHYSICAL_SECTION_ROWS + 1;
        assert!(validate_catalog_layout(&too_many_rows, 5).is_err());

        let oversized = layout_catalog(vec![layout_entry(
            1_006_001,
            4,
            MAX_PHYSICAL_SECTION_BYTES + 1,
        )]);
        assert!(validate_catalog_layout(&oversized, 4 + MAX_PHYSICAL_SECTION_BYTES + 1).is_err());

        let gap = layout_catalog(vec![layout_entry(1_006_001, 5, 1)]);
        assert!(validate_catalog_layout(&gap, 6).is_err());

        let trailing = layout_catalog(vec![layout_entry(1_006_001, 4, 1)]);
        assert!(validate_catalog_layout(&trailing, 6).is_err());
    }
}
