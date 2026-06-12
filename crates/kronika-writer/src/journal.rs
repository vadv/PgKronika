//! File-backed `active.parts` journal.
//!
//! `kronika-format` defines frame bytes and recovery classification. This
//! module handles the file: validated appends, file sync, recovery on open,
//! truncation of an incomplete final frame, and reads for later merging.
//!
//! Recovery policy:
//!
//! - an incomplete final frame is normal after a crash: the file is truncated
//!   to the last valid frame and writing continues;
//! - damage in the middle of the file, or damage at the end that is not a
//!   partial write, is reported in [`OpenReport`];
//! - damaged bytes that cannot be repaired stay on disk, and new frames are
//!   appended after them.

use std::error::Error;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use kronika_format::{
    FRAME_HEADER_LEN, FrameHeader, JournalLimits, PartError, PartRef, ScanReport, scan_journal,
    validate_part,
};

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
    /// The part is not a valid mini-PGM.
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
            Self::InvalidPart(err) => write!(f, "part is not a valid mini-PGM: {err}"),
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
            Self::PartTooLarge { .. } | Self::StalePartRef { .. } => None,
        }
    }
}

impl From<std::io::Error> for JournalError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Result of opening and scanning a journal file.
#[derive(Debug)]
pub struct OpenReport {
    /// Valid parts and damaged regions found during recovery.
    pub scan: ScanReport,
    /// Whether recovery truncated an incomplete final frame.
    pub truncated_torn_tail: bool,
}

impl OpenReport {
    /// Return whether recovery found damage other than an incomplete final frame.
    #[must_use]
    pub fn has_media_damage(&self) -> bool {
        self.scan
            .damages
            .iter()
            .any(|damage| damage.kind != kronika_format::DamageKind::TornTail)
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
    limits: JournalLimits,
    parts: Vec<PartRef>,
}

impl Journal {
    /// Open or create the journal at `path`, then scan it for recovery.
    ///
    /// An incomplete final frame is truncated immediately and the file is
    /// synced. Other damaged regions are reported but left on disk; new frames
    /// are appended after them.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] if the file cannot be opened, read,
    /// truncated, or synced.
    pub fn open(path: &Path, limits: JournalLimits) -> Result<(Self, OpenReport), JournalError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        sync_parent_dir(path)?;

        let bytes = std::fs::read(path)?;
        let scan = scan_journal(&bytes, limits);

        let has_incomplete_final_frame = scan
            .damages
            .last()
            .is_some_and(|damage| damage.kind == kronika_format::DamageKind::TornTail);
        let end = if has_incomplete_final_frame {
            file.set_len(scan.valid_len as u64)?;
            file.sync_data()?;
            scan.valid_len
        } else {
            bytes.len()
        };

        let journal = Self {
            file,
            end,
            limits,
            parts: scan.parts.clone(),
        };
        let report = OpenReport {
            scan,
            truncated_torn_tail: has_incomplete_final_frame,
        };
        Ok((journal, report))
    }

    /// Append one part as a frame and sync the file.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::PartTooLarge`] if the part exceeds the frame
    /// limit. Returns [`JournalError::Io`] if the write or sync fails. On
    /// error, the in-memory journal state is unchanged.
    pub fn append(&mut self, part: &[u8]) -> Result<PartRef, JournalError> {
        let part_len = part.len() as u64;
        if part_len > self.limits.max_part_len {
            return Err(JournalError::PartTooLarge {
                len: part.len(),
                max: self.limits.max_part_len,
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
        self.end += FRAME_HEADER_LEN + part.len();
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
    /// [`Journal::reset`]). Returns [`JournalError::Io`] if the seek or
    /// read fails.
    pub fn read_part(&mut self, part: PartRef) -> Result<Vec<u8>, JournalError> {
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
        self.file.seek(SeekFrom::Start(part.offset as u64))?;
        self.file.read_exact(&mut body)?;
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
    use kronika_format::{Catalog, DamageKind, Entry, FORMAT_VERSION, MAGIC, crc32c};

    use super::*;

    const fn small_limits() -> JournalLimits {
        JournalLimits { max_part_len: 4096 }
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

    fn temp_journal_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("active.parts")
    }

    #[test]
    fn append_read_reopen_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();

        let (mut journal, report) = Journal::open(&path, small_limits()).expect("open");
        assert!(report.scan.is_clean());
        let first = journal.append(&part).expect("append");
        let second = journal.append(&part).expect("append");
        assert_eq!(journal.parts(), &[first, second]);
        assert_eq!(journal.read_part(first).expect("read"), part);

        // Reopen: the recovery scan finds both parts, clean.
        drop(journal);
        let (mut journal, report) = Journal::open(&path, small_limits()).expect("reopen");
        assert!(report.scan.is_clean());
        assert!(!report.truncated_torn_tail);
        assert_eq!(journal.parts().len(), 2);
        assert_eq!(journal.read_part(second).expect("read"), part);
    }

    #[test]
    fn incomplete_final_frame_is_truncated_on_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);
        let part = sample_part();

        let (mut journal, _) = Journal::open(&path, small_limits()).expect("open");
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

        let (journal, report) = Journal::open(&path, small_limits()).expect("recover");
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

        let (mut journal, _) = Journal::open(&path, small_limits()).expect("open");
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

        let (mut journal, report) = Journal::open(&path, small_limits()).expect("recover");
        assert!(report.has_media_damage());
        assert!(!report.truncated_torn_tail);
        assert_eq!(report.scan.damages[0].kind, DamageKind::QuarantinedTail);
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
        let (mut journal, report) = Journal::open(&path, small_limits()).expect("rescan");
        assert!(report.has_media_damage());
        assert_eq!(journal.parts().len(), 2);
        assert_eq!(journal.read_part(appended).expect("read"), part);
    }

    #[test]
    fn reset_empties_the_journal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);

        let (mut journal, _) = Journal::open(&path, small_limits()).expect("open");
        journal.append(&sample_part()).expect("append");
        journal.reset().expect("reset");
        assert!(journal.is_empty());
        assert_eq!(journal.parts().len(), 0);
        assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
        // Idempotent.
        journal.reset().expect("reset again");
    }

    #[test]
    fn oversized_part_is_rejected_without_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = temp_journal_path(&dir);

        let (mut journal, _) = Journal::open(&path, small_limits()).expect("open");
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

        let (mut journal, _) = Journal::open(&path, small_limits()).expect("open");
        // A valid-by-size but invalid body would be framed and synced, then
        // reported as damage and skipped by the next recovery scan.
        assert!(matches!(
            journal.append(b""),
            Err(JournalError::InvalidPart(_))
        ));
        assert!(matches!(
            journal.append(b"not a mini-PGM at all, just bytes of the right size"),
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

        let (mut journal, _) = Journal::open(&path, small_limits()).expect("open");
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
