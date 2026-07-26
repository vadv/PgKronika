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
use std::path::{Path, PathBuf};

use kronika_format::{
    DEFAULT_RESYNC_CHUNK, DamageKind, DamageRegion, FRAME_HEADER_LEN, FrameHeader, JournalLimits,
    PartError, PartRef, ScanReport, scan_journal_streaming, validate_part,
};

use crate::{FilesystemError, FilesystemOperation};

/// Default cap for the whole journal file, bytes.
///
/// A starting value. [`Journal::append`] returns [`JournalError::Full`] before
/// any frame, including the first, would cross this hard cap.
pub const DEFAULT_MAX_JOURNAL_LEN: usize = 1024 * 1024 * 1024;
/// Default maximum number of synchronized parts in one journal generation.
pub const DEFAULT_MAX_JOURNAL_PARTS: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalFaultPoint {
    BeforeResetTruncate,
    AfterResetTruncate,
    BeforeResetSync,
    AfterResetSync,
}

#[cfg(test)]
std::thread_local! {
    static INJECTED_JOURNAL_FAULT: std::cell::Cell<Option<(JournalFaultPoint, i32)>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn maybe_fail(
    point: JournalFaultPoint,
    operation: FilesystemOperation,
    path: &Path,
) -> Result<(), JournalError> {
    INJECTED_JOURNAL_FAULT.with(|injected| {
        if injected.get().is_some_and(|(at, _errno)| at == point) {
            let (_at, errno) = injected.take().expect("matched injected journal fault");
            Err(journal_io(
                operation,
                path,
                std::io::Error::from_raw_os_error(errno),
            ))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "production and test fault hooks deliberately share one call-site signature"
)]
const fn maybe_fail(
    _point: JournalFaultPoint,
    _operation: FilesystemOperation,
    _path: &Path,
) -> Result<(), JournalError> {
    Ok(())
}

/// Configuration of one journal file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalConfig {
    /// Frame-level limits shared with the scanner.
    pub limits: JournalLimits,
    /// Cap for the whole journal file, bytes.
    pub max_journal_len: usize,
    /// Cap for valid part frames retained in one generation.
    pub max_parts: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            limits: JournalLimits::default(),
            max_journal_len: DEFAULT_MAX_JOURNAL_LEN,
            max_parts: DEFAULT_MAX_JOURNAL_PARTS,
        }
    }
}

/// Error returned by a journal operation.
#[derive(Debug)]
pub enum JournalError {
    /// The underlying file operation failed.
    Io(FilesystemError),
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
    /// Appending another part would exceed the frame-count directory cap.
    TooManyParts {
        /// Current valid part count.
        parts: usize,
        /// Configured maximum.
        max: usize,
    },
    /// The bounded in-memory part directory could not grow.
    DirectoryAllocation {
        /// Entries required after the attempted append.
        entries: usize,
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
            Self::Io(err) => write!(f, "journal {err}"),
            Self::PartTooLarge { len, max } => {
                write!(f, "part of {len} bytes exceeds the frame limit of {max}")
            }
            Self::Full { len, max } => {
                write!(
                    f,
                    "journal of {len} bytes would exceed the cap of {max}; merge and reset first"
                )
            }
            Self::TooManyParts { parts, max } => {
                write!(
                    f,
                    "journal with {parts} parts reached the cap of {max}; merge and reset first"
                )
            }
            Self::DirectoryAllocation { entries } => {
                write!(f, "journal could not allocate a directory for {entries} parts")
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
            Self::PartTooLarge { .. }
            | Self::Full { .. }
            | Self::TooManyParts { .. }
            | Self::DirectoryAllocation { .. }
            | Self::StalePartRef { .. } => None,
        }
    }
}

impl From<FilesystemError> for JournalError {
    fn from(error: FilesystemError) -> Self {
        Self::Io(error)
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
    path: PathBuf,
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
            .open(path)
            .map_err(|error| journal_io(FilesystemOperation::Open, path, error))?;
        sync_parent_dir(path)?;

        let metadata = file
            .metadata()
            .map_err(|error| journal_io(FilesystemOperation::Metadata, path, error))?;
        let file_len = usize::try_from(metadata.len()).map_err(|_overflow| {
            journal_io(
                FilesystemOperation::Metadata,
                path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "journal does not fit the address space",
                ),
            )
        })?;
        let mut scan = scan_file(&file, file_len, config.limits, DEFAULT_RESYNC_CHUNK)
            .map_err(|error| journal_io(FilesystemOperation::Read, path, error))?;
        // The directory is the only per-frame state kept after recovery;
        // dropping the push-growth slack keeps it at exactly 16 B per part.
        scan.parts.shrink_to_fit();
        if scan.parts.len() > config.max_parts {
            return Err(JournalError::TooManyParts {
                parts: scan.parts.len(),
                max: config.max_parts,
            });
        }

        let has_incomplete_final_frame = scan
            .damages
            .last()
            .is_some_and(|damage| damage.kind == DamageKind::TornTail);
        let end = if has_incomplete_final_frame {
            file.set_len(scan.valid_len as u64)
                .map_err(|error| journal_io(FilesystemOperation::Truncate, path, error))?;
            file.sync_data()
                .map_err(|error| journal_io(FilesystemOperation::SyncFile, path, error))?;
            scan.valid_len
        } else {
            file_len
        };

        let journal = Self {
            file,
            path: path.to_owned(),
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
        self.admit(part)?;
        let part_len =
            u64::try_from(part.len()).map_err(|_overflow| JournalError::PartTooLarge {
                len: part.len(),
                max: self.config.limits.max_part_len,
            })?;
        let frame_len = FRAME_HEADER_LEN
            .checked_add(part.len())
            .ok_or(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            })?;
        let next_end = self.end.checked_add(frame_len).ok_or(JournalError::Full {
            len: self.end,
            max: self.config.max_journal_len,
        })?;
        let part_offset = self
            .end
            .checked_add(FRAME_HEADER_LEN)
            .ok_or(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            })?;

        let header = FrameHeader { part_len }.encode();
        let entries = self
            .parts
            .len()
            .checked_add(1)
            .ok_or(JournalError::DirectoryAllocation {
                entries: usize::MAX,
            })?;
        self.parts
            .try_reserve_exact(1)
            .map_err(|_error| JournalError::DirectoryAllocation { entries })?;
        if let Err(err) = self.write_frame(&header, part) {
            // Roll the file back so a half-written frame from a transient
            // I/O error does not remain on disk where later appends would
            // push it into the middle of the journal.
            // If truncation also fails, the next open truncates the
            // incomplete frame.
            self.rollback_append()?;
            return Err(err);
        }

        let part_ref = PartRef {
            offset: part_offset,
            len: part.len(),
        };
        self.end = next_end;
        self.parts.push(part_ref);
        Ok(part_ref)
    }

    /// Validate a candidate part and exact append bounds without writing it.
    ///
    /// The collector uses this before sealing decisions so neither byte nor
    /// part-directory hard limits are crossed and then repaired afterward.
    ///
    /// # Errors
    ///
    /// Returns the same size/count/format errors as [`Self::append`].
    pub fn admit(&self, part: &[u8]) -> Result<(), JournalError> {
        let part_len =
            u64::try_from(part.len()).map_err(|_overflow| JournalError::PartTooLarge {
                len: part.len(),
                max: self.config.limits.max_part_len,
            })?;
        if part_len > self.config.limits.max_part_len {
            return Err(JournalError::PartTooLarge {
                len: part.len(),
                max: self.config.limits.max_part_len,
            });
        }
        let frame_len = FRAME_HEADER_LEN
            .checked_add(part.len())
            .ok_or(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            })?;
        let next_end = self.end.checked_add(frame_len).ok_or(JournalError::Full {
            len: self.end,
            max: self.config.max_journal_len,
        })?;
        if next_end > self.config.max_journal_len {
            return Err(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            });
        }
        if self.parts.len() >= self.config.max_parts {
            return Err(JournalError::TooManyParts {
                parts: self.parts.len(),
                max: self.config.max_parts,
            });
        }
        // An invalid body would be framed and synced, but the next recovery
        // scan would report the frame as damage and skip it. Treat that as a
        // writer bug and fail before writing.
        validate_part(part).map_err(JournalError::InvalidPart)?;
        Ok(())
    }

    /// The raw write sequence of one frame, separated so that the error
    /// path of [`Journal::append`] can roll the file back.
    fn write_frame(&mut self, header: &[u8], part: &[u8]) -> Result<(), JournalError> {
        self.file
            .seek(SeekFrom::Start(self.end as u64))
            .map_err(|error| journal_io(FilesystemOperation::Seek, &self.path, error))?;
        self.file
            .write_all(header)
            .map_err(|error| journal_io(FilesystemOperation::Write, &self.path, error))?;
        self.file
            .write_all(part)
            .map_err(|error| journal_io(FilesystemOperation::Write, &self.path, error))?;
        self.file
            .sync_data()
            .map_err(|error| journal_io(FilesystemOperation::SyncFile, &self.path, error))
    }

    fn rollback_append(&self) -> Result<(), JournalError> {
        self.file
            .set_len(self.end as u64)
            .map_err(|error| journal_io(FilesystemOperation::Truncate, &self.path, error))?;
        self.file
            .sync_data()
            .map_err(|error| journal_io(FilesystemOperation::SyncFile, &self.path, error))
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
        self.file
            .read_exact_at(&mut body, part.offset as u64)
            .map_err(|error| journal_io(FilesystemOperation::Read, &self.path, error))?;
        Ok(body)
    }

    /// Empty the journal after a segment has been completed successfully.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] if truncation or sync fails.
    pub fn reset(&mut self) -> Result<(), JournalError> {
        maybe_fail(
            JournalFaultPoint::BeforeResetTruncate,
            FilesystemOperation::Truncate,
            &self.path,
        )?;
        self.file
            .set_len(0)
            .map_err(|error| journal_io(FilesystemOperation::Truncate, &self.path, error))?;
        self.end = 0;
        self.parts.clear();
        maybe_fail(
            JournalFaultPoint::AfterResetTruncate,
            FilesystemOperation::Truncate,
            &self.path,
        )?;
        maybe_fail(
            JournalFaultPoint::BeforeResetSync,
            FilesystemOperation::SyncFile,
            &self.path,
        )?;
        self.file
            .sync_data()
            .map_err(|error| journal_io(FilesystemOperation::SyncFile, &self.path, error))?;
        maybe_fail(
            JournalFaultPoint::AfterResetSync,
            FilesystemOperation::SyncFile,
            &self.path,
        )?;
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
        let directory = File::open(parent)
            .map_err(|error| journal_io(FilesystemOperation::Open, parent, error))?;
        directory
            .sync_all()
            .map_err(|error| journal_io(FilesystemOperation::SyncDirectory, parent, error))?;
    }
    Ok(())
}

fn journal_io(
    operation: FilesystemOperation,
    path: &Path,
    source: std::io::Error,
) -> JournalError {
    FilesystemError::new(operation, path, source).into()
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
            max_parts: 1024,
        }
    }

    #[derive(Debug)]
    struct JournalFaultGuard;

    impl Drop for JournalFaultGuard {
        fn drop(&mut self) {
            INJECTED_JOURNAL_FAULT.with(|injected| injected.set(None));
        }
    }

    fn inject_journal_fault(point: JournalFaultPoint, errno: i32) -> JournalFaultGuard {
        INJECTED_JOURNAL_FAULT.with(|injected| {
            assert!(
                injected.replace(Some((point, errno))).is_none(),
                "one journal fault may be active per test thread"
            );
        });
        JournalFaultGuard
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

    fn temp_journal_path(dir: &tempfile::TempDir) -> PathBuf {
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
    fn reset_crash_points_reopen_as_the_old_or_empty_journal() {
        const POINTS: &[(JournalFaultPoint, FilesystemOperation, bool)] = &[
            (
                JournalFaultPoint::BeforeResetTruncate,
                FilesystemOperation::Truncate,
                true,
            ),
            (
                JournalFaultPoint::AfterResetTruncate,
                FilesystemOperation::Truncate,
                false,
            ),
            (
                JournalFaultPoint::BeforeResetSync,
                FilesystemOperation::SyncFile,
                false,
            ),
            (
                JournalFaultPoint::AfterResetSync,
                FilesystemOperation::SyncFile,
                false,
            ),
        ];

        for &(point, operation, old_journal_remains) in POINTS {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = temp_journal_path(&dir);
            let (mut journal, _) = Journal::open(&path, small_config()).expect("open");
            journal.append(&sample_part()).expect("append");
            let before = std::fs::read(&path).expect("journal bytes");

            let guard = inject_journal_fault(point, 5);
            let error = journal.reset().expect_err("reset fault");
            drop(guard);
            let JournalError::Io(error) = error else {
                panic!("{point:?} did not return a filesystem error");
            };
            assert_eq!(error.operation(), operation, "{point:?}");
            assert_eq!(error.path(), path, "{point:?}");
            assert_eq!(error.io_error().raw_os_error(), Some(5), "{point:?}");
            assert_eq!(journal.is_empty(), !old_journal_remains, "{point:?}");

            drop(journal);
            let (journal, report) = Journal::open(&path, small_config()).expect("restart");
            assert!(report.is_clean(), "{point:?}");
            if old_journal_remains {
                assert_eq!(std::fs::read(&path).expect("old bytes"), before);
                assert_eq!(journal.parts().len(), 1);
            } else {
                assert!(journal.is_empty(), "{point:?}");
                assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
            }
        }
    }

    #[test]
    fn open_failure_retains_operation_path_and_os_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing").join("active.parts");
        let error = Journal::open(&path, small_config()).expect_err("parent is absent");
        let JournalError::Io(error) = error else {
            panic!("open failure was not typed");
        };
        assert_eq!(error.operation(), FilesystemOperation::Open);
        assert_eq!(error.path(), path);
        assert_eq!(error.io_error().kind(), std::io::ErrorKind::NotFound);
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
            max_parts: 1024,
        };
        let (mut journal, _) = Journal::open(&path, config).expect("open");
        journal.append(&part).expect("the first frame fits");
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
