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

/// Size of a frame header on disk, bytes.
pub const FRAME_HEADER_LEN: usize = 16;

/// Default upper bound for one part, bytes.
///
/// This is a starting value, not a fixed format constant.
pub const DEFAULT_MAX_PART_LEN: u64 = 64 * 1024 * 1024;

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
            max_part_len: DEFAULT_MAX_PART_LEN,
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
    /// An incomplete final frame. Normal after a crash; the journal is
    /// truncated to `from` and writing continues. Loss is bounded by one
    /// unfinished part.
    TornTail,
    /// A damaged frame with a valid frame after it.
    Middle {
        /// Offset of the next valid frame.
        resumed_at: usize,
    },
    /// Damage at the end of the journal with no later valid frame.
    ///
    /// These bytes stay on disk for diagnostics.
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

/// Scan a journal source frame by frame, keeping peak memory to one part body.
///
/// Produces a [`ScanReport`] identical to `scan_journal` over the same bytes.
///
/// `resync_chunk` is the caller-owned read-window size used when searching past
/// damage. The window allocation is proportional to `resync_chunk`; a value of
/// `1 << 20` (1 MiB) is a reasonable default. Must be greater than zero.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] when `resync_chunk` is zero.
/// Returns an I/O error if `reader` fails on any read or on `byte_len`.
pub fn scan_journal_streaming<R: ReadAt>(
    reader: &R,
    limits: JournalLimits,
    resync_chunk: usize,
) -> io::Result<ScanReport> {
    if resync_chunk == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resync_chunk must be greater than zero",
        ));
    }
    let overlap = FRAME_MAGIC.len() - 1;
    resync_chunk.checked_add(overlap).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "resync_chunk + overlap overflows usize",
        )
    })?;
    let total_len = usize::try_from(reader.byte_len()?).map_err(|_overflow| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "source does not fit the address space",
        )
    })?;

    let mut report = ScanReport::default();
    let mut part_buf = Vec::new();
    let mut pos = 0_usize;

    while pos < total_len {
        match streaming_frame_at(reader, total_len, pos, limits, &mut part_buf)? {
            StreamingFrame::Valid { body_len } => {
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
                if let Some(next) = streaming_resync(
                    reader,
                    total_len,
                    pos,
                    implied_end,
                    limits,
                    &mut part_buf,
                    resync_chunk,
                )? {
                    report.damages.push(DamageRegion {
                        from: pos,
                        kind: DamageKind::Middle { resumed_at: next },
                    });
                    pos = next;
                    continue;
                }
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

fn streaming_resync<R: ReadAt>(
    reader: &R,
    total_len: usize,
    damaged_at: usize,
    implied_end: Option<usize>,
    limits: JournalLimits,
    part_buf: &mut Vec<u8>,
    chunk_len: usize,
) -> io::Result<Option<usize>> {
    if let Some(boundary) = implied_end
        && boundary < total_len
        && matches!(
            streaming_frame_at(reader, total_len, boundary, limits, part_buf)?,
            StreamingFrame::Valid { .. }
        )
    {
        return Ok(Some(boundary));
    }

    let overlap = FRAME_MAGIC.len() - 1;
    let mut window = vec![0_u8; chunk_len + overlap];
    let mut base = damaged_at + 1;
    while base + FRAME_HEADER_LEN <= total_len {
        let take = (total_len - base).min(window.len());
        reader.read_exact_at(&mut window[..take], base as u64)?;

        let mut from = 0;
        while let Some(found) = find_magic(&window[from..take]) {
            let at = base + from + found;
            if matches!(
                streaming_frame_at(reader, total_len, at, limits, part_buf)?,
                StreamingFrame::Valid { .. }
            ) {
                return Ok(Some(at));
            }
            from = from + found + 1;
        }

        if base + take >= total_len {
            break;
        }
        base += chunk_len;
    }
    Ok(None)
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
        let got =
            scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 1 << 20).unwrap();
        assert_eq!(got, want);
    }
    #[test]
    fn streaming_matches_buffer_on_torn_tail() {
        let p = sample_part();
        let mut buf = framed(&[&p]);
        buf.extend_from_slice(&FrameHeader { part_len: 999 }.encode()); // header for absent body
        let want = scan_journal(&buf, JournalLimits::default());
        let got =
            scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 1 << 20).unwrap();
        assert_eq!(got, want);
    }
    #[test]
    fn streaming_matches_buffer_on_middle_corruption() {
        let p = sample_part();
        let mut buf = framed(&[&p]);
        buf.extend_from_slice(&[0xFF; 8]); // garbage between valid frames
        buf.extend_from_slice(&framed(&[&p]));
        let want = scan_journal(&buf, JournalLimits::default());
        let got =
            scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 1 << 20).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn resync_chunk_zero_returns_invalid_input() {
        let p = sample_part();
        let buf = framed(&[&p]);
        let err = scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 0)
            .expect_err("resync_chunk=0 must be rejected");
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
