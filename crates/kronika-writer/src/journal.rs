//! File-backed `active.parts` journal.
//!
//! `kronika-format` defines frame bytes and damage classification. This module
//! validates appends, syncs the file, scans it on open, truncates an incomplete
//! final frame, and reads parts for merging.
//!
//! Recovery policy:
//!
//! - an incomplete final frame is normal after a crash: the file is truncated
//!   to the last valid frame and writing continues;
//! - damage in the middle of the file, or damage at the end that is not a
//!   partial write, is reported in [`OpenReport`];
//! - damaged bytes that cannot be repaired stay on disk, and new frames are
//!   appended after them.
//!
//! Recovery streams frame by frame. Peak memory is one part, its decoded
//! catalog, a small resynchronization window, and 16 bytes per recovered frame.
//! [`JournalError::Full`] tells the caller to merge early and reset.

use std::error::Error;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

use kronika_format::{
    DEFAULT_RESYNC_CHUNK, DamageKind, DamageRegion, FRAME_HEADER_LEN, FrameHeader, JournalLimits,
    PartError, PartRef, ScanReport, scan_journal_streaming, validate_part,
};

/// Default cap for the whole journal file, bytes.
///
/// A starting value. [`Journal::append`] returns [`JournalError::Full`] when
/// this cap is reached. The first frame after open/reset is exempt, so a tiny
/// cap cannot wedge an empty journal.
pub const DEFAULT_MAX_JOURNAL_LEN: usize = 1024 * 1024 * 1024;

/// Configuration of one journal file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalConfig {
    /// Frame-level limits shared with the scanner.
    pub limits: JournalLimits,
    /// Cap for the whole journal file, bytes.
    pub max_journal_len: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            limits: JournalLimits::default(),
            max_journal_len: DEFAULT_MAX_JOURNAL_LEN,
        }
    }
}

/// Error returned by a journal operation.
#[derive(Debug)]
pub enum JournalError {
    /// The underlying file operation failed.
    Io(std::io::Error),
    /// The part is larger than the configured frame limit.
    PartTooLarge {
        /// Length of the rejected part, bytes.
        len: usize,
        /// The configured limit, bytes.
        max: u64,
    },
    /// Appending would grow the journal past
    /// [`JournalConfig::max_journal_len`].
    ///
    /// This is flow control, not corruption: the caller should merge the
    /// journal into a segment early and [`Journal::reset`] it.
    Full {
        /// Current journal size, bytes.
        len: usize,
        /// The configured cap, bytes.
        max: usize,
    },
    /// The part is not a valid PGM part.
    ///
    /// Writing it would make the next recovery scan classify the frame as
    /// damaged and skip the part.
    InvalidPart(PartError),
    /// The part reference does not point into the current journal, e.g.
    /// it was kept across a [`Journal::reset`].
    StalePartRef {
        /// Offset of the rejected reference.
        offset: usize,
        /// Length of the rejected reference, bytes.
        len: usize,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "journal io: {err}"),
            Self::PartTooLarge { len, max } => {
                write!(f, "part of {len} bytes exceeds the frame limit of {max}")
            }
            Self::Full { len, max } => {
                write!(
                    f,
                    "journal of {len} bytes would exceed the cap of {max}; merge and reset first"
                )
            }
            Self::InvalidPart(err) => write!(f, "part is not a valid PGM part: {err}"),
            Self::StalePartRef { offset, len } => {
                write!(
                    f,
                    "part reference {offset}+{len} points outside the journal"
                )
            }
        }
    }
}

impl Error for JournalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::InvalidPart(err) => Some(err),
            Self::PartTooLarge { .. } | Self::Full { .. } | Self::StalePartRef { .. } => None,
        }
    }
}

impl From<std::io::Error> for JournalError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Result of opening and scanning a journal file.
///
/// Recovered parts are not duplicated here; [`Journal::parts`] stores the part
/// directory. The report carries only the damage findings.
#[derive(Debug)]
pub struct OpenReport {
    /// Damaged regions found during recovery, in journal order.
    pub damages: Vec<DamageRegion>,
    /// Whether recovery truncated an incomplete final frame.
    pub truncated_torn_tail: bool,
}

impl OpenReport {
    /// Return whether recovery found no damage of any kind.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.damages.is_empty()
    }

    /// Return whether recovery found damage other than an incomplete final frame.
    #[must_use]
    pub fn has_media_damage(&self) -> bool {
        self.damages
            .iter()
            .any(|damage| damage.kind != DamageKind::TornTail)
    }
}

/// Open `active.parts` file.
///
/// Each appended frame is written and synced before [`Journal::append`] returns.
#[derive(Debug)]
pub struct Journal {
    file: File,
    /// Append position: either the end of the last valid frame or the end of a
    /// damaged final region kept for diagnostics.
    end: usize,
    config: JournalConfig,
    parts: Vec<PartRef>,
}

impl Journal {
    /// Open or create the journal at `path`, then scan it for recovery.
    ///
    /// An incomplete final frame is truncated immediately. Other damaged
    /// regions are reported but left on disk; new frames are appended after
    /// them.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] if the file cannot be opened, read,
    /// truncated, or synced.
    pub fn open(path: &Path, config: JournalConfig) -> Result<(Self, OpenReport), JournalError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        sync_parent_dir(path)?;

        let file_len = usize::try_from(file.metadata()?.len()).map_err(|_overflow| {
            JournalError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "journal does not fit the address space",
            ))
        })?;
        let mut scan = scan_file(&file, file_len, config.limits, DEFAULT_RESYNC_CHUNK)?;
        // The directory is the only per-frame state kept after recovery;
        // dropping the push-growth slack keeps it at exactly 16 B per part.
        scan.parts.shrink_to_fit();

        let has_incomplete_final_frame = scan
            .damages
            .last()
            .is_some_and(|damage| damage.kind == DamageKind::TornTail);
        let end = if has_incomplete_final_frame {
            file.set_len(scan.valid_len as u64)?;
            file.sync_data()?;
            scan.valid_len
        } else {
            file_len
        };

        let journal = Self {
            file,
            end,
            config,
            parts: scan.parts,
        };
        let report = OpenReport {
            damages: scan.damages,
            truncated_torn_tail: has_incomplete_final_frame,
        };
        Ok((journal, report))
    }

    /// Bytes currently occupying the journal file, including damaged regions.
    ///
    /// The collector compares this raw frame length with its segment byte cap
    /// before packing the segment.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.end
    }

    /// Append one part as a frame and sync the file.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the part is too large, invalid, the
    /// journal is full, or the file write/sync fails. On error, in-memory
    /// state is unchanged.
    pub fn append(&mut self, part: &[u8]) -> Result<PartRef, JournalError> {
        let part_len = part.len() as u64;
        if part_len > self.config.limits.max_part_len {
            return Err(JournalError::PartTooLarge {
                len: part.len(),
                max: self.config.limits.max_part_len,
            });
        }
        // The cap decides when the writer must merge; the first frame is
        // always allowed so that a tiny cap cannot wedge an empty journal.
        let frame_len = FRAME_HEADER_LEN + part.len();
        if self.end > 0 && self.end + frame_len > self.config.max_journal_len {
            return Err(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            });
        }
        // An invalid body would be framed and synced, but the next recovery
        // scan would report the frame as damage and skip it. Treat that as a
        // writer bug and fail before writing.
        validate_part(part).map_err(JournalError::InvalidPart)?;

        let header = FrameHeader { part_len }.encode();
        if let Err(err) = self.write_frame(&header, part) {
            // Roll the file back so a half-written frame from a transient
            // I/O error does not remain on disk where later appends would
            // push it into the middle of the journal.
            // If truncation also fails, the next open truncates the
            // incomplete frame.
            self.file.set_len(self.end as u64).ok();
            return Err(err.into());
        }

        let part_ref = PartRef {
            offset: self.end + FRAME_HEADER_LEN,
            len: part.len(),
        };
        self.end += frame_len;
        self.parts.push(part_ref);
        Ok(part_ref)
    }

    /// The raw write sequence of one frame, separated so that the error
    /// path of [`Journal::append`] can roll the file back.
    fn write_frame(&mut self, header: &[u8], part: &[u8]) -> Result<(), std::io::Error> {
        self.file.seek(SeekFrom::Start(self.end as u64))?;
        self.file.write_all(header)?;
        self.file.write_all(part)?;
        self.file.sync_data()
    }

    /// Return valid parts known to this journal, in journal order.
    #[must_use]
    pub fn parts(&self) -> &[PartRef] {
        &self.parts
    }

    /// Read one part body back.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::StalePartRef`] if the reference does not
    /// point inside the current journal (e.g. it was kept across a
    /// [`Journal::reset`]). Returns [`JournalError::Io`] if the read fails.
    pub fn read_part(&self, part: PartRef) -> Result<Vec<u8>, JournalError> {
        let in_bounds = part.offset >= FRAME_HEADER_LEN
            && part
                .offset
                .checked_add(part.len)
                .is_some_and(|end| end <= self.end);
        if !in_bounds {
            return Err(JournalError::StalePartRef {
                offset: part.offset,
                len: part.len,
            });
        }
        let mut body = vec![0_u8; part.len];
        self.file.read_exact_at(&mut body, part.offset as u64)?;
        Ok(body)
    }

    /// Empty the journal after a segment has been completed successfully.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] if truncation or sync fails.
    pub fn reset(&mut self) -> Result<(), JournalError> {
        self.file.set_len(0)?;
        self.file.sync_data()?;
        self.end = 0;
        self.parts.clear();
        Ok(())
    }

    /// Return the current journal size in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end
    }

    /// Return whether the journal holds no frames.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.end == 0
    }
}

/// Stream the recovery scan over the file by delegating to
/// `kronika_format::scan_journal_streaming`.
fn scan_file(
    file: &File,
    _file_len: usize,
    limits: JournalLimits,
    resync_chunk: usize,
) -> Result<ScanReport, std::io::Error> {
    scan_journal_streaming(file, limits, resync_chunk)
}

/// Sync the directory entry after creating the journal file.
fn sync_parent_dir(path: &Path) -> Result<(), JournalError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kronika_format::{
        Catalog, Entry, FORMAT_VERSION, FRAME_MAGIC, MAGIC, crc32c, scan_journal,
    };

    use super::*;

    const fn small_limits() -> JournalLimits {
        JournalLimits { max_part_len: 4096 }
    }

    const fn small_config() -> JournalConfig {
        JournalConfig {
            limits: small_limits(),
            max_journal_len: 1 << 20,
        }
    }

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
            format_version: FORMAT_VERSION,
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

    fn temp_journal_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("active.parts")
    }

    /// The streaming scanner must report exactly what the in-memory scanner
    /// reports, for every chunk size including degenerate ones.
    fn assert_stream_matches_buffer(bytes: &[u8]) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("journal");
        std::fs::write(&path, bytes).expect("write");
        let file = File::open(&path).expect("open");

        let expected = scan_journal(bytes, small_limits());
        for chunk in [1, 2, 3, 5, 16, 1024] {
            let streamed =
                scan_file(&file, bytes.len(), small_limits(), chunk).expect("streaming scan");
            assert_eq!(streamed, expected, "chunk size {chunk}");
        }
    }

    #[test]
    fn streaming_scan_matches_the_buffer_scan() {
        let part = sample_part();
        let one = frame(&part);

        // Clean journals.
        assert_stream_matches_buffer(&[]);
        assert_stream_matches_buffer(&one);
        let mut two = one.clone();
        two.extend_from_slice(&one);
        assert_stream_matches_buffer(&two);

        // Truncation at every offset of a two-frame journal.
        for cut in 0..two.len() {
            assert_stream_matches_buffer(&two[..cut]);
        }

        // A final frame with an intact header but corrupted body: the
        // resync finds nothing after it, and the implied frame end at EOF
        // classifies it as a torn write, not unrecoverable trailing damage.
        let mut torn_body = two.clone();
        let last = torn_body.len() - 1;
        torn_body[last] ^= 0x01;
        assert_stream_matches_buffer(&torn_body);

        // A trailing header with a valid CRC but an absurd length claim:
        // unrecoverable trailing damage.
        let mut absurd = one.clone();
        absurd.extend_from_slice(
            &FrameHeader {
                part_len: small_limits().max_part_len + 1,
            }
            .encode(),
        );
        assert_stream_matches_buffer(&absurd);

        // A decoy magic 3 bytes before the real frame: damaged bytes ending in
        // "PGM" followed by the real frame's "PGMP" creates overlapping
        // magic occurrences, and the scanner must advance by one byte, not
        // by a whole magic length, after the decoy fails.
        let mut decoy = one.clone();
        decoy.extend_from_slice(&[0xEE_u8; 21]);
        decoy.extend_from_slice(b"PGM");
        decoy.extend_from_slice(&one);
        assert_stream_matches_buffer(&decoy);

        // A corrupted byte in the middle frame of three, in the header and
        // in the body.
        let mut three = two.clone();
        three.extend_from_slice(&one);
        for target in [one.len(), one.len() + FRAME_HEADER_LEN + 5] {
            let mut corrupted = three.clone();
            corrupted[target] ^= 0x01;
            assert_stream_matches_buffer(&corrupted);
        }

        // A long damaged region followed by a valid frame: the sliding
        // search must cross many chunk boundaries to find it.
        let mut damaged_then_frame = one.clone();
        damaged_then_frame.extend_from_slice(&[0xAB_u8; 257]);
        damaged_then_frame.extend_from_slice(&one);
        assert_stream_matches_buffer(&damaged_then_frame);

        // Damaged bytes that contain stray FRAME_MAGIC bytes positioned to span
        // chunk boundaries.
        let mut tricky = one.clone();
        let mut damaged = vec![0xCD_u8; 64];
        damaged[6..10].copy_from_slice(&FRAME_MAGIC);
        damaged[31..35].copy_from_slice(&FRAME_MAGIC);
        tricky.extend_from_slice(&damaged);
        tricky.extend_from_slice(&one);
        assert_stream_matches_buffer(&tricky);
    }

    #[test]
    fn append_read_reopen_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();

        let (mut journal, report) = Journal::open(&path, small_config()).expect("open");
        assert!(report.is_clean());
        let first = journal.append(&part).expect("append");
        let second = journal.append(&part).expect("append");
        assert_eq!(journal.parts(), &[first, second]);
        assert_eq!(journal.read_part(first).expect("read"), part);

        // Reopen: the recovery scan finds both parts, clean.
        drop(journal);
        let (journal, report) = Journal::open(&path, small_config()).expect("reopen");
        assert!(report.is_clean());
        assert!(!report.truncated_torn_tail);
        assert_eq!(journal.parts().len(), 2);
        assert_eq!(journal.read_part(second).expect("read"), part);
    }

    #[test]
    fn incomplete_final_frame_is_truncated_on_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();

        let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
        journal.append(&part).expect("append");
        let valid_len = journal.len();
        drop(journal);

        // Simulate a crash mid-append: a complete header, half a body.
        let mut file = OpenOptions::new().append(true).open(&path).expect("raw");
        let partial_frame_header = FrameHeader {
            part_len: part.len() as u64,
        }
        .encode();
        file.write_all(&partial_frame_header).expect("write");
        file.write_all(&part[..part.len() / 2]).expect("write");
        drop(file);

        let (journal, report) = Journal::open(&path, small_config()).expect("recover");
        assert!(report.truncated_torn_tail);
        assert!(!report.has_media_damage());
        assert_eq!(journal.parts().len(), 1);
        assert_eq!(journal.len(), valid_len);
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            valid_len as u64,
            "the incomplete frame is gone from disk"
        );
    }

    #[test]
    fn damaged_final_region_is_preserved_and_appendable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();

        let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
        journal.append(&part).expect("append");
        drop(journal);

        // Media damage at the end: a full frame with a corrupted header,
        // not a truncation.
        let mut bad_header = FrameHeader {
            part_len: part.len() as u64,
        }
        .encode();
        bad_header[0] ^= 0xFF;
        let mut file = OpenOptions::new().append(true).open(&path).expect("raw");
        file.write_all(&bad_header).expect("write");
        file.write_all(&part).expect("write");
        drop(file);
        let damaged_len = std::fs::metadata(&path).expect("metadata").len();

        let (mut journal, report) = Journal::open(&path, small_config()).expect("recover");
        assert!(report.has_media_damage());
        assert!(!report.truncated_torn_tail);
        assert_eq!(report.damages[0].kind, DamageKind::QuarantinedTail);
        assert_eq!(journal.parts().len(), 1);
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            damaged_len,
            "damaged bytes stay on disk for diagnostics"
        );

        // New frames are appended after the damaged region and found on the
        // next recovery scan.
        let appended = journal.append(&part).expect("append after damage");
        drop(journal);
        let (journal, report) = Journal::open(&path, small_config()).expect("rescan");
        assert!(report.has_media_damage());
        assert_eq!(journal.parts().len(), 2);
        assert_eq!(journal.read_part(appended).expect("read"), part);
    }

    #[test]
    fn reset_empties_the_journal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);

        let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
        journal.append(&sample_part()).expect("append");
        journal.reset().expect("reset");
        assert!(journal.is_empty());
        assert_eq!(journal.parts().len(), 0);
        assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
        // Idempotent.
        journal.reset().expect("reset again");
    }

    #[test]
    fn full_journal_rejects_appends_until_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();
        let frame_len = FRAME_HEADER_LEN + part.len();

        let config = JournalConfig {
            limits: small_limits(),
            // Room for one frame, not two.
            max_journal_len: frame_len + frame_len / 2,
        };
        let (mut journal, _) = Journal::open(&path, config).expect("open");
        journal.append(&part).expect("the first frame always fits");
        assert!(matches!(
            journal.append(&part),
            Err(JournalError::Full { .. })
        ));
        assert_eq!(
            journal.parts().len(),
            1,
            "a rejected append changes nothing"
        );

        // After the merge resets the journal, appends work again.
        journal.reset().expect("reset");
        journal.append(&part).expect("append after reset");
    }

    #[test]
    fn oversized_part_is_rejected_without_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);

        let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
        let huge = vec![0_u8; 4097];
        assert!(matches!(
            journal.append(&huge),
            Err(JournalError::PartTooLarge { .. })
        ));
        assert!(journal.is_empty());
        assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
    }

    #[test]
    fn invalid_part_is_rejected_without_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);

        let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
        // A valid-by-size but invalid body would be framed and synced, then
        // reported as damage and skipped by the next recovery scan.
        assert!(matches!(
            journal.append(b""),
            Err(JournalError::InvalidPart(_))
        ));
        assert!(matches!(
            journal.append(b"not a PGM part at all, just bytes of the right size"),
            Err(JournalError::InvalidPart(_))
        ));
        assert!(journal.is_empty());
        assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
    }

    #[test]
    fn stale_part_ref_is_rejected_after_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();

        let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
        let stale = journal.append(&part).expect("append");
        journal.reset().expect("reset");
        assert!(matches!(
            journal.read_part(stale),
            Err(JournalError::StalePartRef { .. })
        ));

        // A fresh ref works again after new appends.
        let fresh = journal.append(&part).expect("append");
        assert_eq!(journal.read_part(fresh).expect("read"), part);
    }
}
