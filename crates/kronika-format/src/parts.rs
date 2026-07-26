//! `active.parts` journal frames.
//!
//! File I/O is in `kronika-writer`. This module defines frame bytes and
//! in-memory recovery.

use std::error::Error;
use std::fmt;
use std::io;

use crate::{Catalog, DecodeError, Entry, MAGIC, ReadAt, TAIL_INDEX_LEN, TailIndex, crc32c};

/// Magic bytes opening every journal frame.
pub const FRAME_MAGIC: [u8; 4] = *b"PGMP";

/// File signature for the only supported active-journal header format.
pub const JOURNAL_MAGIC: [u8; 8] = *b"PGKJNL1\0";

/// Current active-journal format version.
pub const JOURNAL_VERSION: u32 = 1;

/// Size of the version-1 active-journal header.
pub const JOURNAL_HEADER_LEN: usize = 36;

/// Magic bytes opening a committed journal-reset marker.
pub const RESET_MARKER_MAGIC: [u8; 8] = *b"PGKRST1\0";

/// Size of a committed journal-reset marker.
pub const RESET_MARKER_LEN: usize = 32;

/// Size of a frame header on disk, bytes.
pub const FRAME_HEADER_LEN: usize = 16;

/// Hard version-1 admission limit for the complete active journal, bytes.
pub const MAX_JOURNAL_LEN: usize = 1024 * 1024 * 1024;

/// Hard version-1 admission limit for one part body, bytes.
pub const MAX_PART_LEN: u64 = 64 * 1024 * 1024;

/// Hard version-1 admission limit for valid frames in one active journal.
pub const MAX_JOURNAL_PARTS: usize = 1_000_000;

/// Whether a version-1 journal is empty or belongs to one active segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalState {
    /// No frames and no segment identity.
    Empty,
    /// One or more frames belonging to the stored segment identity.
    Active {
        /// Unix microseconds of the first successfully appended window.
        segment_id: i64,
    },
}

/// Versioned root header of `active.parts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalHeader {
    /// Empty or active segment state.
    pub state: JournalState,
    /// Exact number of frame bytes following this header.
    pub body_len: u64,
}

impl JournalHeader {
    /// Canonical empty journal header.
    pub const EMPTY: Self = Self {
        state: JournalState::Empty,
        body_len: 0,
    };

    /// Encodes the complete checksummed 36-byte header.
    #[must_use]
    pub fn encode(self) -> [u8; JOURNAL_HEADER_LEN] {
        let mut bytes = [0_u8; JOURNAL_HEADER_LEN];
        bytes[..8].copy_from_slice(&JOURNAL_MAGIC);
        bytes[8..12].copy_from_slice(&JOURNAL_VERSION.to_le_bytes());
        match self.state {
            JournalState::Empty => {}
            JournalState::Active { segment_id } => {
                bytes[12] = 1;
                bytes[13] = 1;
                bytes[16..24].copy_from_slice(&segment_id.to_le_bytes());
            }
        }
        bytes[24..32].copy_from_slice(&self.body_len.to_le_bytes());
        let checksum = crc32c(&bytes[..32]);
        bytes[32..36].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// Decodes and validates a complete version-1 journal header.
    ///
    /// # Errors
    ///
    /// Returns a specific [`JournalHeaderError`] for incompatible magic or
    /// version, checksum damage, invalid state, or missing identity.
    pub fn decode(bytes: [u8; JOURNAL_HEADER_LEN]) -> Result<Self, JournalHeaderError> {
        if bytes[..8] != JOURNAL_MAGIC {
            let mut actual = [0_u8; 8];
            actual.copy_from_slice(&bytes[..8]);
            return Err(JournalHeaderError::UnsupportedMagic { actual });
        }
        let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if version != JOURNAL_VERSION {
            return Err(JournalHeaderError::UnsupportedVersion { version });
        }
        let stored = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]);
        let computed = crc32c(&bytes[..32]);
        if stored != computed {
            return Err(JournalHeaderError::BadChecksum { stored, computed });
        }
        if bytes[14] != 0 || bytes[15] != 0 {
            return Err(JournalHeaderError::NonZeroReserved);
        }
        let id_present = bytes[13];
        let segment_id = i64::from_le_bytes([
            bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
        ]);
        let body_len = u64::from_le_bytes([
            bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30], bytes[31],
        ]);
        let state = match (bytes[12], id_present) {
            (0, 0) => {
                if segment_id != 0 {
                    return Err(JournalHeaderError::UnexpectedIdentity);
                }
                JournalState::Empty
            }
            (1, 1) => JournalState::Active { segment_id },
            (1, 0) => return Err(JournalHeaderError::MissingIdentity),
            (0, 1) => return Err(JournalHeaderError::UnexpectedIdentity),
            (state, _) => return Err(JournalHeaderError::InvalidState { state }),
        };
        Ok(Self { state, body_len })
    }
}

/// Why a complete active-journal header is not a valid version-1 header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalHeaderError {
    /// Magic identifies an unrelated or malformed file.
    UnsupportedMagic {
        /// Bytes found at the start of the file.
        actual: [u8; 8],
    },
    /// The file declares another journal version.
    UnsupportedVersion {
        /// Version found in the file.
        version: u32,
    },
    /// Header checksum does not match.
    BadChecksum {
        /// Stored checksum.
        stored: u32,
        /// Computed checksum.
        computed: u32,
    },
    /// State byte is not defined.
    InvalidState {
        /// State byte found in the file.
        state: u8,
    },
    /// Active state does not mark its segment identity as present.
    MissingIdentity,
    /// Empty state unexpectedly carries a segment identity.
    UnexpectedIdentity,
    /// Reserved bytes are non-zero.
    NonZeroReserved,
}

impl fmt::Display for JournalHeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMagic { actual } => {
                write!(f, "unsupported active-journal magic {actual:02x?}")
            }
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported active-journal version {version}")
            }
            Self::BadChecksum { stored, computed } => write!(
                f,
                "active-journal header crc32c mismatch: stored {stored:#010x}, computed {computed:#010x}"
            ),
            Self::InvalidState { state } => {
                write!(f, "invalid active-journal state {state}")
            }
            Self::MissingIdentity => {
                f.write_str("active journal state is missing its segment identity")
            }
            Self::UnexpectedIdentity => {
                f.write_str("empty journal state unexpectedly carries a segment identity")
            }
            Self::NonZeroReserved => f.write_str("active-journal reserved bytes are non-zero"),
        }
    }
}

impl Error for JournalHeaderError {}

/// Durable intent to replace one active journal generation with the canonical
/// empty header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "the shared on-disk fields explicitly describe the previous journal generation"
)]
pub struct ResetMarker {
    /// Complete journal length before the marker was appended.
    pub previous_len: u64,
    /// Segment identity stored by the previous active header.
    pub previous_segment_id: i64,
    /// CRC32C of the complete encoded previous active header.
    pub previous_header_crc: u32,
}

impl ResetMarker {
    /// Builds a marker for one non-empty active journal generation.
    #[must_use]
    pub fn new(previous_len: u64, previous_segment_id: i64) -> Option<Self> {
        if previous_len <= JOURNAL_HEADER_LEN as u64 {
            return None;
        }
        let header = JournalHeader {
            state: JournalState::Active {
                segment_id: previous_segment_id,
            },
            body_len: previous_len - JOURNAL_HEADER_LEN as u64,
        };
        Some(Self {
            previous_len,
            previous_segment_id,
            previous_header_crc: crc32c(&header.encode()),
        })
    }

    /// Encodes the checksummed marker.
    #[must_use]
    pub fn encode(self) -> [u8; RESET_MARKER_LEN] {
        let mut bytes = [0_u8; RESET_MARKER_LEN];
        bytes[..8].copy_from_slice(&RESET_MARKER_MAGIC);
        bytes[8..16].copy_from_slice(&self.previous_len.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.previous_segment_id.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.previous_header_crc.to_le_bytes());
        let checksum = crc32c(&bytes[..28]);
        bytes[28..32].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// Decodes a marker after checking its magic and checksum.
    #[must_use]
    pub fn decode(bytes: [u8; RESET_MARKER_LEN]) -> Option<Self> {
        if bytes[..8] != RESET_MARKER_MAGIC {
            return None;
        }
        let stored = u32::from_le_bytes(bytes[28..32].try_into().ok()?);
        if stored != crc32c(&bytes[..28]) {
            return None;
        }
        Some(Self {
            previous_len: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            previous_segment_id: i64::from_le_bytes(bytes[16..24].try_into().ok()?),
            previous_header_crc: u32::from_le_bytes(bytes[24..28].try_into().ok()?),
        })
    }

    /// Reconstructs and verifies the active header named by this marker.
    #[must_use]
    pub fn expected_previous_header(self) -> Option<JournalHeader> {
        let marker = Self::new(self.previous_len, self.previous_segment_id)?;
        (marker.previous_header_crc == self.previous_header_crc).then_some(JournalHeader {
            state: JournalState::Active {
                segment_id: self.previous_segment_id,
            },
            body_len: self.previous_len - JOURNAL_HEADER_LEN as u64,
        })
    }

    /// Classifies an observed root header during a committed reset.
    ///
    /// Besides either complete header, only a prefix rewrite from the previous
    /// active bytes to the empty bytes is accepted. Unrelated corruption and
    /// non-v1 headers are not reset transitions.
    #[must_use]
    pub fn classify_header_transition(
        self,
        observed: [u8; JOURNAL_HEADER_LEN],
    ) -> Option<ResetHeaderTransition> {
        let previous = self.expected_previous_header()?.encode();
        let empty = JournalHeader::EMPTY.encode();
        if observed == previous {
            return Some(ResetHeaderTransition::Previous);
        }
        if observed == empty {
            return Some(ResetHeaderTransition::Empty);
        }
        (1..JOURNAL_HEADER_LEN)
            .any(|split| {
                observed[..split] == empty[..split] && observed[split..] == previous[split..]
            })
            .then_some(ResetHeaderTransition::Torn)
    }
}

/// Valid root-header states observed after a reset marker was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetHeaderTransition {
    /// The previous active header is still complete.
    Previous,
    /// The canonical empty header is already complete.
    Empty,
    /// A prefix of the empty header replaced the corresponding active bytes.
    Torn,
}

/// Header of one journal frame.
///
/// The header stores the length of the part body that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Length of the part body following the header, bytes.
    pub part_len: u64,
}

impl FrameHeader {
    /// Encode this header as its 16-byte on-disk form.
    #[must_use]
    pub fn encode(self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0_u8; FRAME_HEADER_LEN];
        out[..4].copy_from_slice(&FRAME_MAGIC);
        out[4..12].copy_from_slice(&self.part_len.to_le_bytes());
        let crc = crc32c(&out[..12]);
        out[12..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decode a frame header; validates magic and header CRC.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] when the magic bytes or header CRC are invalid.
    pub fn decode(bytes: [u8; FRAME_HEADER_LEN]) -> Result<Self, FrameError> {
        let (meta, stored_crc) = split_header(&bytes);
        if meta[..4] != FRAME_MAGIC {
            let mut actual = [0_u8; 4];
            actual.copy_from_slice(&meta[..4]);
            return Err(FrameError::BadMagic { actual });
        }
        let computed = crc32c(meta);
        if stored_crc != computed {
            return Err(FrameError::BadCrc {
                stored: stored_crc,
                computed,
            });
        }
        let mut len = [0_u8; 8];
        len.copy_from_slice(&meta[4..12]);
        Ok(Self {
            part_len: u64::from_le_bytes(len),
        })
    }
}

/// Split header bytes into the CRC-covered prefix and the stored CRC.
fn split_header(bytes: &[u8; FRAME_HEADER_LEN]) -> (&[u8], u32) {
    let mut crc = [0_u8; 4];
    crc.copy_from_slice(&bytes[12..]);
    (&bytes[..12], u32::from_le_bytes(crc))
}

/// Why frame header bytes failed to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The first four bytes are not [`FRAME_MAGIC`].
    BadMagic {
        /// The bytes actually found.
        actual: [u8; 4],
    },
    /// Stored header CRC32C does not match the computed one.
    BadCrc {
        /// CRC stored in the header.
        stored: u32,
        /// CRC computed over magic + length.
        computed: u32,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic { actual } => {
                write!(f, "frame magic is {actual:02x?}, expected \"PGMP\"")
            }
            Self::BadCrc { stored, computed } => {
                write!(
                    f,
                    "frame header crc32c mismatch: stored {stored:#010x}, computed {computed:#010x}"
                )
            }
        }
    }
}

impl Error for FrameError {}

/// Why a part body is not a valid PGM part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartError {
    /// The body is shorter than magic + empty catalog + tail index.
    TooShort {
        /// The byte length actually given.
        actual: usize,
    },
    /// The body does not start with the segment magic.
    BadMagic {
        /// The bytes actually found.
        actual: [u8; 4],
    },
    /// The tail index failed to decode.
    Tail(DecodeError),
    /// `catalog_len` does not fit between the magic and the tail index.
    BadCatalogLen {
        /// `catalog_len` stored in the tail index.
        catalog_len: u32,
    },
    /// The catalog failed to decode.
    Catalog(DecodeError),
    /// A catalog entry points outside the section area of the body.
    SectionOutOfBounds {
        /// `type_id` of the entry that failed validation.
        type_id: u32,
    },
    /// A section body does not match its catalog CRC32C.
    SectionCrc {
        /// `type_id` of the entry that failed validation.
        type_id: u32,
        /// CRC stored in the catalog entry.
        stored: u32,
        /// CRC computed over the section body.
        computed: u32,
    },
}

impl fmt::Display for PartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { actual } => {
                write!(f, "part body of {actual} bytes is too short for a PGM part")
            }
            Self::BadMagic { actual } => {
                write!(f, "part magic is {actual:02x?}, expected \"PGM1\"")
            }
            Self::Tail(err) => write!(f, "part tail index: {err}"),
            Self::BadCatalogLen { catalog_len } => {
                write!(f, "part catalog_len {catalog_len} does not fit the body")
            }
            Self::Catalog(err) => write!(f, "part catalog: {err}"),
            Self::SectionOutOfBounds { type_id } => {
                write!(f, "section {type_id} points outside the part body")
            }
            Self::SectionCrc {
                type_id,
                stored,
                computed,
            } => {
                write!(
                    f,
                    "section {type_id} crc32c mismatch: stored {stored:#010x}, computed {computed:#010x}"
                )
            }
        }
    }
}

impl Error for PartError {}

/// Validate a self-contained PGM part, including section CRCs.
///
/// # Errors
///
/// Returns [`PartError`] when framing, catalog, section bounds, or section CRC
/// checks fail.
pub fn validate_part(bytes: &[u8]) -> Result<Catalog, PartError> {
    let catalog = decode_and_bound(bytes)?;
    for entry in &catalog.entries {
        // `decode_and_bound` confirmed every section is in range, so the casts
        // and the slice are safe.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "offset and len fit in usize: both are bounded by the part length"
        )]
        let body = &bytes[entry.offset as usize..(entry.offset + entry.len) as usize];
        let computed = crc32c(body);
        if computed != entry.crc32c {
            return Err(PartError::SectionCrc {
                type_id: entry.type_id,
                stored: entry.crc32c,
                computed,
            });
        }
    }
    Ok(catalog)
}

/// Validate part framing and catalog without hashing section bodies.
///
/// Use only when section CRCs are checked elsewhere.
///
/// # Errors
///
/// Returns [`PartError`] when framing, catalog, or section bounds checks fail.
pub fn validate_part_catalog(bytes: &[u8]) -> Result<Catalog, PartError> {
    decode_and_bound(bytes)
}

/// Decode a part catalog and confirm section bounds.
fn decode_and_bound(bytes: &[u8]) -> Result<Catalog, PartError> {
    // Smallest possible part: magic + empty catalog (meta only) + tail.
    let min_len = MAGIC.len() + crate::META_LEN + TAIL_INDEX_LEN;
    if bytes.len() < min_len {
        return Err(PartError::TooShort {
            actual: bytes.len(),
        });
    }
    if bytes[..4] != MAGIC {
        let mut actual = [0_u8; 4];
        actual.copy_from_slice(&bytes[..4]);
        return Err(PartError::BadMagic { actual });
    }

    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    tail_bytes.copy_from_slice(&bytes[bytes.len() - TAIL_INDEX_LEN..]);
    let tail = TailIndex::decode(tail_bytes).map_err(PartError::Tail)?;

    let catalog_len = tail.catalog_len as usize;
    let body_end = bytes.len() - TAIL_INDEX_LEN;
    let Some(catalog_start) = body_end.checked_sub(catalog_len) else {
        return Err(PartError::BadCatalogLen {
            catalog_len: tail.catalog_len,
        });
    };
    if catalog_start < MAGIC.len() {
        return Err(PartError::BadCatalogLen {
            catalog_len: tail.catalog_len,
        });
    }

    let catalog = Catalog::decode(&bytes[catalog_start..body_end]).map_err(PartError::Catalog)?;

    for entry in &catalog.entries {
        let in_bounds = entry.offset >= MAGIC.len() as u64
            && entry
                .offset
                .checked_add(entry.len)
                .is_some_and(|end| end <= catalog_start as u64);
        if !in_bounds {
            return Err(PartError::SectionOutOfBounds {
                type_id: entry.type_id,
            });
        }
    }

    Ok(catalog)
}

/// One opaque section body to place in a part.
#[derive(Debug, Clone, Copy)]
pub struct SectionInput<'a> {
    /// Section type from the type registry (`kronika-registry`).
    pub type_id: u32,
    /// Number of rows or records the body holds; recorded in the catalog.
    pub rows: u32,
    /// The section body bytes, placed verbatim.
    pub body: &'a [u8],
}

/// Segment-level catalog metadata for a part, the fields not derivable from the
/// section bodies.
#[derive(Debug, Clone, Copy)]
pub struct PartMeta {
    /// Minimal timestamp across the part's rows, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp across the part's rows, unix microseconds.
    pub max_ts: i64,
    /// `str_id` of `{cluster_id}/{pg_system_identifier}`; 0 = not set.
    pub source_id: u64,
}

/// Assemble section bodies into a self-contained PGM part.
///
/// Offsets and CRCs are computed here.
///
/// # Panics
///
/// If the encoded catalog block does not fit in `u32`.
#[must_use]
pub fn build_part(sections: &[SectionInput<'_>], meta: PartMeta) -> Vec<u8> {
    // The exact part length is known up front.
    let bodies: usize = sections.iter().map(|section| section.body.len()).sum();
    let capacity =
        MAGIC.len() + bodies + sections.len() * crate::ENTRY_LEN + crate::META_LEN + TAIL_INDEX_LEN;
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(&MAGIC);

    let entries = sections
        .iter()
        .map(|section| {
            // Catalog offsets are absolute from the part start.
            let offset = out.len() as u64;
            out.extend_from_slice(section.body);
            Entry {
                type_id: section.type_id,
                flags: 0,
                offset,
                len: section.body.len() as u64,
                rows: section.rows,
                crc32c: crc32c(section.body),
            }
        })
        .collect();

    let catalog = Catalog {
        entries,
        min_ts: meta.min_ts,
        max_ts: meta.max_ts,
        source_id: meta.source_id,
        format_version: crate::FORMAT_VERSION,
    };
    out.extend_from_slice(&catalog.encode());
    out
}

/// Limits used while scanning a journal buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalLimits {
    /// Frames claiming a part longer than this are rejected.
    pub max_part_len: u64,
}

impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_part_len: MAX_PART_LEN,
        }
    }
}

/// Location of one valid part body inside the journal buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartRef {
    /// Offset of the part body (after the frame header).
    pub offset: usize,
    /// Length of the part body, bytes.
    pub len: usize,
}

/// One damaged region found by the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRegion {
    /// Offset where the damaged frame starts.
    pub from: usize,
    /// What the damage means for the journal.
    pub kind: DamageKind,
}

/// Classification of a damaged journal region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageKind {
    /// An incomplete final frame.
    ///
    /// This is diagnostic classification only. The production journal opens
    /// fail closed and does not truncate or repair damaged bytes.
    TornTail,
    /// A damaged frame with a valid frame after it.
    Middle {
        /// Offset of the next valid frame.
        resumed_at: usize,
    },
    /// Damage at the end of the journal with no later valid frame.
    ///
    /// This is diagnostic classification only; the production journal leaves
    /// these bytes unchanged and rejects the file.
    QuarantinedTail,
}

/// Result of scanning a journal buffer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanReport {
    /// Valid parts in journal order.
    pub parts: Vec<PartRef>,
    /// Damaged regions in journal order; empty for a clean journal.
    pub damages: Vec<DamageRegion>,
    /// Length of the journal prefix ending at the last valid frame.
    /// After an incomplete final frame this is the truncation point.
    pub valid_len: usize,
}

impl ScanReport {
    /// Return whether the buffer contains only valid frames.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.damages.is_empty()
    }
}

/// Failure while scanning a journal with an explicit part-count bound.
#[derive(Debug)]
pub enum JournalScanError {
    /// The byte source could not be read.
    Io(io::Error),
    /// Another valid frame would exceed the caller's part-count bound.
    PartLimitExceeded {
        /// Maximum number of returned parts.
        limit: usize,
    },
}

impl fmt::Display for JournalScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal scan I/O: {error}"),
            Self::PartLimitExceeded { limit } => {
                write!(f, "journal contains more than {limit} parts")
            }
        }
    }
}

impl Error for JournalScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::PartLimitExceeded { .. } => None,
        }
    }
}

impl From<io::Error> for JournalScanError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Scan an in-memory journal buffer.
#[must_use]
pub fn scan_journal(bytes: &[u8], limits: JournalLimits) -> ScanReport {
    let mut report = ScanReport::default();
    let mut pos = 0_usize;

    while pos < bytes.len() {
        match frame_at(bytes, pos, limits) {
            FrameCheck::Valid { body_len } => {
                report.parts.push(PartRef {
                    offset: pos + FRAME_HEADER_LEN,
                    len: body_len,
                });
                pos += FRAME_HEADER_LEN + body_len;
                report.valid_len = pos;
            }
            FrameCheck::Torn => {
                report.damages.push(DamageRegion {
                    from: pos,
                    kind: DamageKind::TornTail,
                });
                return report;
            }
            FrameCheck::Damaged { implied_end } => {
                if let Some(next) = resync(bytes, pos, implied_end, limits) {
                    report.damages.push(DamageRegion {
                        from: pos,
                        kind: DamageKind::Middle { resumed_at: next },
                    });
                    pos = next;
                    continue;
                }
                // A complete-looking final frame with a sane header is treated
                // like an interrupted write; otherwise keep the damaged tail.
                let kind = if implied_end == Some(bytes.len()) {
                    DamageKind::TornTail
                } else {
                    DamageKind::QuarantinedTail
                };
                report.damages.push(DamageRegion { from: pos, kind });
                return report;
            }
        }
    }

    report
}

/// Scans a journal source sequentially from `start_at`.
///
/// `start_at` must be a frame boundary. Returned part and damage offsets, and
/// [`ScanReport::valid_len`], remain absolute from the start of the source. If
/// no bytes follow `start_at`, the report is empty and `valid_len` equals
/// `start_at`.
///
/// Scanning stops at the first damaged frame and never searches the damaged
/// bytes for candidate frame magic. Peak memory is one part body and at most
/// `max_parts` references. The part limit is checked before adding each
/// [`PartRef`] to the report.
///
/// # Errors
///
/// Returns [`JournalScanError::PartLimitExceeded`] before returning more than
/// `max_parts` valid frames. A `start_at` beyond the source and failures from
/// the source are returned as [`JournalScanError::Io`].
pub fn scan_journal_streaming_strict_from<R: ReadAt>(
    reader: &R,
    start_at: u64,
    limits: JournalLimits,
    max_parts: usize,
) -> Result<ScanReport, JournalScanError> {
    let total_len = usize::try_from(reader.byte_len()?).map_err(|_overflow| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "source does not fit the address space",
        )
    })?;
    let mut pos = usize::try_from(start_at).map_err(|_overflow| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "start_at does not fit the address space",
        )
    })?;
    if pos > total_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "start_at is beyond the journal source",
        )
        .into());
    }

    let mut report = ScanReport {
        valid_len: pos,
        ..ScanReport::default()
    };
    let mut part_buf = Vec::new();

    while pos < total_len {
        match streaming_frame_at(reader, total_len, pos, limits, &mut part_buf)? {
            StreamingFrame::Valid { body_len } => {
                if report.parts.len() >= max_parts {
                    return Err(JournalScanError::PartLimitExceeded { limit: max_parts });
                }
                report.parts.push(PartRef {
                    offset: pos + FRAME_HEADER_LEN,
                    len: body_len,
                });
                pos += FRAME_HEADER_LEN + body_len;
                report.valid_len = pos;
            }
            StreamingFrame::Torn => {
                report.damages.push(DamageRegion {
                    from: pos,
                    kind: DamageKind::TornTail,
                });
                return Ok(report);
            }
            StreamingFrame::Damaged { implied_end } => {
                let kind = if implied_end == Some(total_len) {
                    DamageKind::TornTail
                } else {
                    DamageKind::QuarantinedTail
                };
                report.damages.push(DamageRegion { from: pos, kind });
                return Ok(report);
            }
        }
    }

    Ok(report)
}

/// Outcome of checking one frame position in the streaming scanner.
enum StreamingFrame {
    Valid { body_len: usize },
    Torn,
    Damaged { implied_end: Option<usize> },
}

fn streaming_frame_at<R: ReadAt>(
    reader: &R,
    total_len: usize,
    pos: usize,
    limits: JournalLimits,
    part_buf: &mut Vec<u8>,
) -> io::Result<StreamingFrame> {
    let rem = total_len - pos;
    if rem < FRAME_HEADER_LEN {
        return Ok(StreamingFrame::Torn);
    }
    let mut header_bytes = [0_u8; FRAME_HEADER_LEN];
    reader.read_exact_at(&mut header_bytes, pos as u64)?;
    let Ok(header) = FrameHeader::decode(header_bytes) else {
        return Ok(StreamingFrame::Damaged { implied_end: None });
    };
    if header.part_len > limits.max_part_len {
        return Ok(StreamingFrame::Damaged { implied_end: None });
    }
    let Ok(body_len) = usize::try_from(header.part_len) else {
        return Ok(StreamingFrame::Damaged { implied_end: None });
    };
    if rem - FRAME_HEADER_LEN < body_len {
        return Ok(StreamingFrame::Torn);
    }
    part_buf.resize(body_len, 0);
    reader.read_exact_at(&mut part_buf[..body_len], (pos + FRAME_HEADER_LEN) as u64)?;
    if validate_part(&part_buf[..body_len]).is_err() {
        return Ok(StreamingFrame::Damaged {
            implied_end: Some(pos + FRAME_HEADER_LEN + body_len),
        });
    }
    Ok(StreamingFrame::Valid { body_len })
}

/// Outcome of checking one frame position.
enum FrameCheck {
    /// A valid frame with a validated part of this length.
    Valid { body_len: usize },
    /// The frame is cut off by the end of the buffer: header and length
    /// are plausible (or the header itself is incomplete), nothing
    /// follows. This is an incomplete write, not media damage.
    Torn,
    /// Damaged frame. `implied_end` is set only if the header gave a sane end.
    Damaged { implied_end: Option<usize> },
}

fn frame_at(bytes: &[u8], pos: usize, limits: JournalLimits) -> FrameCheck {
    let rem = bytes.len() - pos;
    if rem < FRAME_HEADER_LEN {
        return FrameCheck::Torn;
    }
    let mut header_bytes = [0_u8; FRAME_HEADER_LEN];
    header_bytes.copy_from_slice(&bytes[pos..pos + FRAME_HEADER_LEN]);
    let Ok(header) = FrameHeader::decode(header_bytes) else {
        return FrameCheck::Damaged { implied_end: None };
    };
    if header.part_len > limits.max_part_len {
        return FrameCheck::Damaged { implied_end: None };
    }
    let Ok(body_len) = usize::try_from(header.part_len) else {
        return FrameCheck::Damaged { implied_end: None };
    };
    if rem - FRAME_HEADER_LEN < body_len {
        // The header CRC is valid and the length is sane, but the body
        // extends past the end: the write was cut mid-frame.
        return FrameCheck::Torn;
    }
    let body = &bytes[pos + FRAME_HEADER_LEN..pos + FRAME_HEADER_LEN + body_len];
    if validate_part(body).is_err() {
        return FrameCheck::Damaged {
            implied_end: Some(pos + FRAME_HEADER_LEN + body_len),
        };
    }
    FrameCheck::Valid { body_len }
}

/// Find the next valid frame after damage.
fn resync(
    bytes: &[u8],
    damaged_at: usize,
    implied_end: Option<usize>,
    limits: JournalLimits,
) -> Option<usize> {
    if let Some(boundary) = implied_end
        && boundary < bytes.len()
        && matches!(frame_at(bytes, boundary, limits), FrameCheck::Valid { .. })
    {
        return Some(boundary);
    }
    let mut cand = damaged_at + 1;
    while cand + FRAME_HEADER_LEN <= bytes.len() {
        match find_magic(&bytes[cand..]) {
            Some(found) => {
                let at = cand + found;
                if let FrameCheck::Valid { .. } = frame_at(bytes, at, limits) {
                    return Some(at);
                }
                cand = at + 1;
            }
            None => return None,
        }
    }
    None
}

/// Position of the first `FRAME_MAGIC` occurrence in `haystack`.
fn find_magic(haystack: &[u8]) -> Option<usize> {
    haystack
        .windows(FRAME_MAGIC.len())
        .position(|window| window == FRAME_MAGIC)
}

#[cfg(test)]
mod streaming_tests {
    use super::*;

    const TEST_MAX_PARTS: usize = 16;

    fn scan_streaming(bytes: &[u8], start_at: u64) -> Result<ScanReport, JournalScanError> {
        scan_journal_streaming_strict_from(
            &bytes,
            start_at,
            JournalLimits::default(),
            TEST_MAX_PARTS,
        )
    }

    #[test]
    fn journal_v1_header_has_the_initial_magic_and_version() {
        let bytes = JournalHeader::EMPTY.encode();
        assert_eq!(&bytes[..8], b"PGKJNL1\0");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);
        assert_eq!(JournalHeader::decode(bytes), Ok(JournalHeader::EMPTY));
    }

    #[test]
    fn reset_marker_accepts_only_the_two_headers_and_their_prefix_transition() {
        let segment_id = 0x0102_0304_0506_0708;
        let marker = ResetMarker::new(4096, segment_id).unwrap();
        assert_eq!(ResetMarker::decode(marker.encode()), Some(marker));
        let previous = marker.expected_previous_header().unwrap().encode();
        let empty = JournalHeader::EMPTY.encode();
        assert_eq!(
            marker.classify_header_transition(previous),
            Some(ResetHeaderTransition::Previous)
        );
        assert_eq!(
            marker.classify_header_transition(empty),
            Some(ResetHeaderTransition::Empty)
        );
        for split in 1..JOURNAL_HEADER_LEN {
            let mut torn = previous;
            torn[..split].copy_from_slice(&empty[..split]);
            assert!(
                marker.classify_header_transition(torn).is_some(),
                "prefix split {split} must be an admissible reset transition"
            );
        }

        let mut non_prefix = previous;
        non_prefix[20] = empty[20];
        assert_eq!(marker.classify_header_transition(non_prefix), None);
        assert_eq!(
            marker.classify_header_transition([0xA5; JOURNAL_HEADER_LEN]),
            None
        );
    }

    fn framed(parts: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in parts {
            out.extend_from_slice(
                &FrameHeader {
                    part_len: p.len() as u64,
                }
                .encode(),
            );
            out.extend_from_slice(p);
        }
        out
    }
    fn sample_part() -> Vec<u8> {
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: b"",
            }],
            PartMeta {
                min_ts: 1,
                max_ts: 2,
                source_id: 7,
            },
        )
    }
    #[test]
    fn streaming_matches_buffer_on_clean_journal() {
        let p = sample_part();
        let buf = framed(&[&p, &p]);
        let want = scan_journal(&buf, JournalLimits::default());
        let got = scan_streaming(&buf, 0).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn bounded_streaming_scan_stops_before_exceeding_the_part_limit() {
        let part = sample_part();
        let bytes = framed(&[&part, &part]);
        assert!(matches!(
            scan_journal_streaming_strict_from(&bytes.as_slice(), 0, JournalLimits::default(), 1,),
            Err(JournalScanError::PartLimitExceeded { limit: 1 })
        ));
    }
    #[test]
    fn streaming_matches_buffer_on_torn_tail() {
        let p = sample_part();
        let mut buf = framed(&[&p]);
        buf.extend_from_slice(&FrameHeader { part_len: 999 }.encode()); // header for absent body
        let want = scan_journal(&buf, JournalLimits::default());
        let got = scan_streaming(&buf, 0).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn strict_streaming_stops_at_middle_corruption_without_resyncing() {
        let p = sample_part();
        let mut buf = framed(&[&p]);
        let first_frame_len = buf.len();
        buf.extend_from_slice(&[0xFF; 8]); // garbage between valid frames
        buf.extend_from_slice(&framed(&[&p]));
        let report = scan_streaming(&buf, 0).unwrap();
        assert_eq!(report.parts.len(), 1);
        assert_eq!(report.valid_len, first_frame_len);
        assert_eq!(
            report.damages,
            vec![DamageRegion {
                from: first_frame_len,
                kind: DamageKind::QuarantinedTail,
            }]
        );
    }

    #[test]
    fn streaming_from_valid_len_scans_only_the_tail() {
        // A two-part journal. The first frame ends at `first_len`; scanning from
        // there must find only the second part, with an absolute offset, and not
        // re-report the first.
        let p = sample_part();
        let buf = framed(&[&p, &p]);
        let first_len = FRAME_HEADER_LEN + p.len();

        let report = scan_streaming(&buf, first_len as u64).unwrap();
        assert_eq!(report.parts.len(), 1, "only the tail part is scanned");
        assert_eq!(
            report.parts[0].offset,
            first_len + FRAME_HEADER_LEN,
            "the tail part offset is absolute from the file start"
        );
        assert_eq!(report.parts[0].len, p.len());
        assert_eq!(
            report.valid_len,
            buf.len(),
            "valid_len spans the whole file"
        );
        assert!(report.is_clean());
    }

    #[test]
    fn streaming_from_end_of_journal_is_empty() {
        // Starting exactly at the journal length yields no parts and a valid_len
        // pinned to the start offset (nothing new to read).
        let p = sample_part();
        let buf = framed(&[&p, &p]);
        let report = scan_streaming(&buf, buf.len() as u64).unwrap();
        assert!(report.parts.is_empty(), "no parts past the end");
        assert!(report.damages.is_empty(), "no damage past the end");
        assert_eq!(
            report.valid_len,
            buf.len(),
            "valid_len stays at the start offset when nothing follows"
        );
    }

    #[test]
    fn streaming_rejects_a_start_offset_beyond_the_source() {
        let p = sample_part();
        let buf = framed(&[&p]);
        let JournalScanError::Io(err) = scan_streaming(&buf, buf.len() as u64 + 1)
            .expect_err("start_at beyond the source must be rejected")
        else {
            panic!("invalid start offset must be reported as an I/O validation error");
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid part: magic + one tiny section + catalog + tail.
    fn sample_part() -> Vec<u8> {
        let section = *b"data";
        let mut part = Vec::new();
        part.extend_from_slice(&MAGIC);
        part.extend_from_slice(&section);
        let catalog = Catalog {
            entries: vec![Entry {
                type_id: 1_006_001,
                flags: 0,
                offset: 4,
                len: section.len() as u64,
                rows: 1,
                crc32c: crc32c(&section),
            }],
            min_ts: 1,
            max_ts: 2,
            source_id: 0,
            format_version: crate::FORMAT_VERSION,
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

    const fn small_limits() -> JournalLimits {
        JournalLimits { max_part_len: 4096 }
    }

    #[test]
    fn frame_header_layout_is_byte_exact() {
        let encoded = FrameHeader { part_len: 88 }.encode();
        assert_eq!(&encoded[..4], b"PGMP");
        assert_eq!(&encoded[4..12], &88_u64.to_le_bytes());
        // The CRC pins the covered range: magic + length, little-endian.
        assert_eq!(
            &encoded[12..],
            &crc32c(&encoded[..12]).to_le_bytes(),
            "header crc covers exactly the first 12 bytes"
        );
        assert_eq!(
            FrameHeader::decode(encoded),
            Ok(FrameHeader { part_len: 88 })
        );
    }

    #[test]
    fn frame_header_rejects_damage() {
        let mut bytes = FrameHeader { part_len: 7 }.encode();
        bytes[0] ^= 0xFF;
        assert!(matches!(
            FrameHeader::decode(bytes),
            Err(FrameError::BadMagic { .. })
        ));

        let mut bytes = FrameHeader { part_len: 7 }.encode();
        bytes[5] ^= 0x01;
        assert!(matches!(
            FrameHeader::decode(bytes),
            Err(FrameError::BadCrc { .. })
        ));
    }

    #[test]
    fn validates_a_real_part_and_catches_section_corruption() {
        let part = sample_part();
        let catalog = validate_part(&part).expect("sample part is valid");
        assert_eq!(catalog.entries.len(), 1);

        // Corrupting the section body is caught by the section CRC even
        // though the catalog itself is intact.
        let mut corrupted = part;
        corrupted[5] ^= 0x01;
        assert!(matches!(
            validate_part(&corrupted),
            Err(PartError::SectionCrc { .. })
        ));
    }

    #[test]
    fn catalog_validation_skips_section_body_crc() {
        // A part whose body is corrupt but whose catalog is intact: the full
        // check rejects it, the catalog-only check accepts it (the reader
        // re-verifies bodies on decode).
        let mut part = sample_part();
        part[5] ^= 0x01;
        assert!(matches!(
            validate_part(&part),
            Err(PartError::SectionCrc { .. })
        ));
        assert!(validate_part_catalog(&part).is_ok());
        // The catalog-only check still rejects a structural failure.
        let mut bad_magic = sample_part();
        bad_magic[0] ^= 0xFF;
        assert!(matches!(
            validate_part_catalog(&bad_magic),
            Err(PartError::BadMagic { .. })
        ));
    }

    #[test]
    fn build_part_round_trips_through_validate_part() {
        let first: &[u8] = b"section-one-body";
        let second: &[u8] = b"second";
        let part = build_part(
            &[
                SectionInput {
                    type_id: 1_006_001,
                    rows: 3,
                    body: first,
                },
                SectionInput {
                    type_id: 1_021_001,
                    rows: 1,
                    body: second,
                },
            ],
            PartMeta {
                min_ts: 100,
                max_ts: 900,
                source_id: 42,
            },
        );

        let catalog = validate_part(&part).expect("built part is valid");
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(
            (catalog.min_ts, catalog.max_ts, catalog.source_id),
            (100, 900, 42)
        );
        assert_eq!(catalog.entries[0].type_id, 1_006_001);
        assert_eq!(catalog.entries[0].rows, 3);
        assert_eq!(catalog.entries[0].offset, MAGIC.len() as u64);

        // Each recorded (offset, len) slices back to the exact body that went in.
        for (entry, body) in catalog.entries.iter().zip([first, second]) {
            let start = usize::try_from(entry.offset).expect("offset fits usize");
            let len = usize::try_from(entry.len).expect("len fits usize");
            assert_eq!(&part[start..start + len], body);
        }
    }

    #[test]
    fn build_part_accepts_no_sections() {
        let part = build_part(
            &[],
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );
        let catalog = validate_part(&part).expect("empty part is valid");
        assert!(catalog.entries.is_empty());
    }

    #[test]
    fn a_built_part_passes_the_journal_scan() {
        let part = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 1,
                body: b"data",
            }],
            PartMeta {
                min_ts: 1,
                max_ts: 2,
                source_id: 0,
            },
        );
        let report = scan_journal(&frame(&part), small_limits());
        assert!(report.is_clean());
        assert_eq!(report.parts.len(), 1);
    }

    #[test]
    fn clean_journal_scans_clean() {
        let part = sample_part();
        let mut journal = Vec::new();
        journal.extend_from_slice(&frame(&part));
        journal.extend_from_slice(&frame(&part));

        let report = scan_journal(&journal, small_limits());
        assert!(report.is_clean());
        assert_eq!(report.parts.len(), 2);
        assert_eq!(report.valid_len, journal.len());
        for part_ref in &report.parts {
            let body = &journal[part_ref.offset..part_ref.offset + part_ref.len];
            assert_eq!(body, part.as_slice());
        }
    }

    #[test]
    fn incomplete_final_frame_keeps_the_valid_prefix() {
        let part = sample_part();
        let mut journal = frame(&part);
        let full = frame(&part);
        journal.extend_from_slice(&full[..full.len() - 3]);

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 1);
        assert_eq!(report.damages.len(), 1);
        assert_eq!(report.damages[0].kind, DamageKind::TornTail);
        assert_eq!(
            report.valid_len,
            frame(&part).len(),
            "truncation point is the end of the last valid frame"
        );
    }

    #[test]
    fn middle_corruption_resyncs_and_keeps_both_sides() {
        let part = sample_part();
        let one = frame(&part);
        let mut journal = Vec::new();
        journal.extend_from_slice(&one);
        journal.extend_from_slice(&one);
        journal.extend_from_slice(&one);
        // Corrupt a byte inside the second frame's part body.
        let target = one.len() + FRAME_HEADER_LEN + 5;
        journal[target] ^= 0x01;

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 2, "first and third parts survive");
        assert_eq!(report.damages.len(), 1);
        assert!(matches!(
            report.damages[0].kind,
            DamageKind::Middle { resumed_at } if resumed_at == 2 * one.len()
        ));
    }

    #[test]
    fn corrupted_final_header_is_reported_without_truncation() {
        let part = sample_part();
        let one = frame(&part);
        let mut journal = Vec::new();
        journal.extend_from_slice(&one);
        journal.extend_from_slice(&one);
        // Corrupt the second frame's header magic: recovery cannot know where
        // that frame ends, and nothing valid follows it.
        let target = one.len();
        journal[target] ^= 0xFF;

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 1);
        assert_eq!(report.damages.len(), 1);
        assert_eq!(report.damages[0].kind, DamageKind::QuarantinedTail);
        assert_eq!(report.valid_len, one.len());
    }

    #[test]
    fn corrupted_final_body_with_intact_header_is_recoverable() {
        let part = sample_part();
        let one = frame(&part);
        let mut journal = Vec::new();
        journal.extend_from_slice(&one);
        journal.extend_from_slice(&one);
        // The header is intact and the frame ends exactly at the buffer end,
        // but the body is invalid. Treat it like an interrupted write and
        // keep only the valid prefix.
        let target = one.len() + FRAME_HEADER_LEN + 5;
        journal[target] ^= 0x01;

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 1);
        assert_eq!(report.damages.len(), 1);
        assert_eq!(report.damages[0].kind, DamageKind::TornTail);
        assert_eq!(report.valid_len, one.len());
    }

    #[test]
    fn resync_prefers_the_header_implied_boundary_over_embedded_frames() {
        // The embedded frame is legitimate section data, not a journal frame.
        let inner = frame(&sample_part());
        let mut tricky = Vec::new();
        tricky.extend_from_slice(&MAGIC);
        tricky.extend_from_slice(&inner);
        let catalog = Catalog {
            entries: vec![Entry {
                type_id: 1_000_001,
                flags: 0,
                offset: 4,
                len: inner.len() as u64,
                rows: 1,
                crc32c: crc32c(&inner),
            }],
            min_ts: 1,
            max_ts: 2,
            source_id: 0,
            format_version: crate::FORMAT_VERSION,
        };
        tricky.extend_from_slice(&catalog.encode());

        let plain = sample_part();
        let mut journal = Vec::new();
        journal.extend_from_slice(&frame(&tricky));
        journal.extend_from_slice(&frame(&plain));
        // Corrupt one byte of the outer catalog of the tricky part, past
        // the embedded frame.
        let target = FRAME_HEADER_LEN + 4 + inner.len() + 3;
        journal[target] ^= 0x01;

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 1, "only the real second part");
        let recovered =
            &journal[report.parts[0].offset..report.parts[0].offset + report.parts[0].len];
        assert_eq!(recovered, plain.as_slice());
        assert!(matches!(
            report.damages[0].kind,
            DamageKind::Middle { resumed_at } if resumed_at == FRAME_HEADER_LEN + tricky.len()
        ));
    }

    #[test]
    fn resync_searches_to_the_end_of_the_buffer() {
        // A long damaged region followed by a valid frame: the search must
        // not give up early, or later appends would be lost on reopen.
        let part = sample_part();
        let mut journal = frame(&part);
        journal.extend_from_slice(&[0xAB_u8; 2048]);
        journal.extend_from_slice(&frame(&part));

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 2);
        assert!(matches!(report.damages[0].kind, DamageKind::Middle { .. }));
    }

    #[test]
    fn oversized_length_claim_is_final_damage() {
        let part = sample_part();
        let mut journal = frame(&part);
        // A frame claiming a part over the configured limit, with a
        // valid CRC: damaged by definition, and nothing valid follows.
        journal.extend_from_slice(
            &FrameHeader {
                part_len: small_limits().max_part_len + 1,
            }
            .encode(),
        );

        let report = scan_journal(&journal, small_limits());
        assert_eq!(report.parts.len(), 1);
        assert_eq!(report.damages.len(), 1);
        assert_eq!(report.damages[0].kind, DamageKind::QuarantinedTail);
    }
}
