//! Version-1 file-backed `active.parts` journal.
//!
//! The checksummed root header persists the exact [`SegmentId`] independently
//! of the PGM catalogs carried by its frames. A fresh journal always contains a
//! valid empty header. A zero-length file provably never held data and is
//! re-initialized in place; headerless frame-only files are rejected without
//! modification.

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU64, Ordering};

use kronika_format::{
    DamageRegion, FRAME_HEADER_LEN, FrameHeader, JOURNAL_HEADER_LEN, JournalHeader,
    JournalHeaderError, JournalLimits, JournalScanError, JournalState, MAX_JOURNAL_LEN,
    MAX_JOURNAL_PARTS, MAX_PART_LEN, PartError, PartRef, RESET_MARKER_LEN, ResetMarker,
    scan_journal_streaming_strict_from, validate_part,
};
use kronika_layout::{
    FileIdentity, JournalRotation, JournalSlot, LayoutError, SegmentId, WriterLease, WriterOwner,
};

static NEXT_JOURNAL_GENERATION: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalFaultPoint {
    OpenRootSync,
    AppendHeaderWrite,
    AppendFrameHeaderWrite,
    AppendFrameBodyWrite,
    AppendSync,
    ResetMarkerWrite,
    ResetMarkerSync,
    ResetEmptyHeaderWrite,
    ResetEmptyHeaderSync,
    ResetTruncate,
    ResetFinalSync,
    RollbackTruncate,
    RollbackHeaderWrite,
    RollbackSync,
}

#[cfg(test)]
std::thread_local! {
    static JOURNAL_FAULTS:
        std::cell::RefCell<std::collections::VecDeque<(JournalFaultPoint, i32)>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(test)]
struct JournalFaultGuard;

#[cfg(test)]
impl JournalFaultGuard {
    fn assert_consumed(self) {
        JOURNAL_FAULTS.with(|faults| {
            let faults = faults.borrow();
            assert!(
                faults.is_empty(),
                "journal fault plan was not fully exercised: {faults:?}"
            );
        });
        drop(self);
    }
}

#[cfg(test)]
impl Drop for JournalFaultGuard {
    fn drop(&mut self) {
        JOURNAL_FAULTS.with(|faults| faults.borrow_mut().clear());
    }
}

#[cfg(test)]
fn arm_journal_faults(
    faults: impl IntoIterator<Item = (JournalFaultPoint, i32)>,
) -> JournalFaultGuard {
    JOURNAL_FAULTS.with(|armed| {
        let mut armed = armed.borrow_mut();
        assert!(armed.is_empty());
        armed.extend(faults);
    });
    JournalFaultGuard
}

#[cfg(test)]
fn inject_journal_fault(point: JournalFaultPoint) -> std::io::Result<()> {
    JOURNAL_FAULTS.with(|faults| {
        let mut faults = faults.borrow_mut();
        let Some(&(armed, raw_os_error)) = faults.front() else {
            return Ok(());
        };
        if armed != point {
            return Ok(());
        }
        faults.pop_front();
        Err(std::io::Error::from_raw_os_error(raw_os_error))
    })
}

macro_rules! journal_failpoint {
    ($point:ident) => {
        #[cfg(test)]
        inject_journal_fault(JournalFaultPoint::$point)?;
    };
}

/// Configuration of one journal file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalConfig {
    /// Frame-level limits shared with the scanner.
    pub limits: JournalLimits,
    /// Cap for the complete journal file, including its versioned header.
    pub max_journal_len: usize,
    /// Cap for valid frames retained in one active generation.
    pub max_parts: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            limits: JournalLimits::default(),
            max_journal_len: MAX_JOURNAL_LEN,
            max_parts: MAX_JOURNAL_PARTS,
        }
    }
}

/// Error returned by a journal operation.
#[derive(Debug)]
pub enum JournalError {
    /// The underlying file operation failed.
    Io(std::io::Error),
    /// The data-root capability rejected an unsafe or inaccessible object.
    Layout(LayoutError),
    /// The configured file cap is outside the version-1 admission range.
    InvalidMaxJournalLen {
        /// Configured cap.
        value: usize,
        /// Smallest accepted cap.
        minimum: usize,
        /// Largest accepted cap.
        maximum: usize,
    },
    /// The configured part-count cap is outside the version-1 admission range.
    InvalidMaxParts {
        /// Configured cap.
        value: usize,
        /// Smallest accepted cap.
        minimum: usize,
        /// Largest accepted cap.
        maximum: usize,
    },
    /// The configured part-body cap is outside the version-1 admission range.
    InvalidMaxPartLen {
        /// Configured cap.
        value: u64,
        /// Smallest accepted cap.
        minimum: u64,
        /// Largest accepted cap.
        maximum: u64,
    },
    /// An existing canonical journal exceeds its configured file cap.
    JournalTooLarge {
        /// Existing file length.
        len: u64,
        /// Configured cap.
        max: usize,
    },
    /// An existing or appended frame would exceed the configured part-count cap.
    TooManyParts {
        /// Configured cap.
        max: usize,
    },
    /// A foreign or differently versioned journal header was found.
    UnsupportedJournalFormat,
    /// The file ends before the complete version-1 header.
    TornHeader {
        /// Actual file length.
        len: u64,
    },
    /// The complete header failed a state or checksum check.
    InvalidHeader(JournalHeaderError),
    /// The header's recorded frame length differs from the physical tail.
    BodyLengthMismatch {
        /// Length recorded in the header.
        recorded: u64,
        /// Physical bytes after the header.
        actual: u64,
    },
    /// An empty header has trailing frame bytes.
    EmptyWithFrames {
        /// Physical frame-tail length.
        body_len: u64,
    },
    /// An active header has no complete first frame.
    ActiveWithoutFirstFrame,
    /// A version-1 body contains torn or damaged frame bytes.
    DamagedBody {
        /// Damage reported by the bounded frame scanner.
        damages: Vec<DamageRegion>,
    },
    /// The stored raw identity cannot be represented by the calendar layout.
    InvalidSegmentId(LayoutError),
    /// An append attempted to mix two segment identities.
    SegmentIdMismatch {
        /// Identity persisted in the journal.
        expected: SegmentId,
        /// Identity supplied by the append.
        got: SegmentId,
    },
    /// The part is larger than the configured frame limit.
    PartTooLarge {
        /// Length of the rejected part, bytes.
        len: usize,
        /// The configured limit, bytes.
        max: u64,
    },
    /// Appending would grow the journal past its configured cap.
    Full {
        /// Current journal length, bytes.
        len: usize,
        /// Configured cap, bytes.
        max: usize,
    },
    /// The part is not a valid PGM part.
    InvalidPart(PartError),
    /// The reference no longer points inside the current journal generation.
    StalePartRef {
        /// Offset of the rejected reference.
        offset: usize,
        /// Length of the rejected reference.
        len: usize,
    },
    /// A failed write could not be rolled back to the preceding durable state.
    RollbackFailed {
        /// Error from the original write or synchronization.
        operation: std::io::Error,
        /// Error encountered while restoring the old file.
        rollback: std::io::Error,
    },
    /// A committed reset marker could not be reduced to the canonical empty file.
    ResetIncomplete(std::io::Error),
    /// A previous operation left the open journal in an indeterminate state.
    Poisoned,
    /// A writer-owned rotated generation is not the canonical empty bytes.
    FreshGenerationInvalid {
        /// Observed file length.
        len: u64,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal I/O: {error}"),
            Self::Layout(error) => write!(f, "journal layout: {error}"),
            Self::InvalidMaxJournalLen {
                value,
                minimum,
                maximum,
            } => write!(
                f,
                "journal length cap {value} is outside {minimum}..={maximum}"
            ),
            Self::InvalidMaxParts {
                value,
                minimum,
                maximum,
            } => write!(
                f,
                "journal part-count cap {value} is outside {minimum}..={maximum}"
            ),
            Self::InvalidMaxPartLen {
                value,
                minimum,
                maximum,
            } => write!(
                f,
                "journal part-body cap {value} is outside {minimum}..={maximum} bytes"
            ),
            Self::JournalTooLarge { len, max } => {
                write!(
                    f,
                    "active.parts length {len} exceeds the configured cap of {max}"
                )
            }
            Self::TooManyParts { max } => {
                write!(f, "active.parts exceeds the configured cap of {max} parts")
            }
            Self::UnsupportedJournalFormat => {
                f.write_str("active.parts is not a version-1 journal")
            }
            Self::TornHeader { len } => {
                write!(f, "active.parts has a torn header of {len} bytes")
            }
            Self::InvalidHeader(error) => write!(f, "invalid active.parts header: {error}"),
            Self::BodyLengthMismatch { recorded, actual } => write!(
                f,
                "active.parts header records {recorded} body bytes, but the file contains {actual}"
            ),
            Self::EmptyWithFrames { body_len } => {
                write!(f, "empty active.parts header has {body_len} trailing bytes")
            }
            Self::ActiveWithoutFirstFrame => {
                f.write_str("active active.parts has no complete first frame")
            }
            Self::DamagedBody { damages } => write!(
                f,
                "active.parts contains {} damaged frame region(s)",
                damages.len()
            ),
            Self::InvalidSegmentId(error) => {
                write!(f, "active.parts stores an invalid segment id: {error}")
            }
            Self::SegmentIdMismatch { expected, got } => write!(
                f,
                "active.parts belongs to segment {expected}, not supplied segment {got}"
            ),
            Self::PartTooLarge { len, max } => {
                write!(f, "part of {len} bytes exceeds the frame limit of {max}")
            }
            Self::Full { len, max } => write!(
                f,
                "journal of {len} bytes would exceed the cap of {max}; seal and reset first"
            ),
            Self::InvalidPart(error) => write!(f, "part is not a valid PGM part: {error}"),
            Self::StalePartRef { offset, len } => {
                write!(f, "part reference {offset}+{len} is outside active.parts")
            }
            Self::RollbackFailed {
                operation,
                rollback,
            } => write!(
                f,
                "journal write failed ({operation}) and rollback also failed ({rollback})"
            ),
            Self::ResetIncomplete(error) => write!(
                f,
                "journal reset was committed but requires restart recovery: {error}"
            ),
            Self::Poisoned => {
                f.write_str("journal is poisoned after an incomplete persistence operation")
            }
            Self::FreshGenerationInvalid { len } => write!(
                f,
                "fresh rotated journal generation is not the canonical empty file ({len} bytes)"
            ),
        }
    }
}

impl Error for JournalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) | Self::ResetIncomplete(error) => Some(error),
            Self::Layout(error) | Self::InvalidSegmentId(error) => Some(error),
            Self::InvalidHeader(error) => Some(error),
            Self::InvalidPart(error) => Some(error),
            Self::RollbackFailed { operation, .. } => Some(operation),
            Self::UnsupportedJournalFormat
            | Self::InvalidMaxJournalLen { .. }
            | Self::InvalidMaxParts { .. }
            | Self::InvalidMaxPartLen { .. }
            | Self::JournalTooLarge { .. }
            | Self::TooManyParts { .. }
            | Self::TornHeader { .. }
            | Self::BodyLengthMismatch { .. }
            | Self::EmptyWithFrames { .. }
            | Self::ActiveWithoutFirstFrame
            | Self::DamagedBody { .. }
            | Self::SegmentIdMismatch { .. }
            | Self::PartTooLarge { .. }
            | Self::Full { .. }
            | Self::StalePartRef { .. }
            | Self::Poisoned
            | Self::FreshGenerationInvalid { .. } => None,
        }
    }
}

impl From<std::io::Error> for JournalError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<LayoutError> for JournalError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

/// Opaque reference to one part in a specific in-process journal generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalPartRef {
    raw: PartRef,
    generation: u64,
}

impl JournalPartRef {
    const fn new(raw: PartRef, generation: u64) -> Self {
        Self { raw, generation }
    }

    /// Offset of the part body in `active.parts`.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.raw.offset
    }

    /// Length of the part body, bytes.
    #[must_use]
    pub const fn len(self) -> usize {
        self.raw.len
    }

    /// Whether the referenced part body is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.raw.len == 0
    }
}

/// Open active journal owned by exactly one collector process.
#[derive(Debug)]
pub struct Journal {
    _owner_lease: WriterLease,
    file: File,
    end: usize,
    config: JournalConfig,
    parts: Vec<JournalPartRef>,
    generation: u64,
    segment_id: Option<SegmentId>,
    poisoned: bool,
}

impl Journal {
    /// Initializes a layout-owned fresh journal slot durably.
    ///
    /// This is the alternate-generation counterpart of
    /// [`prepare_rotation`](Self::prepare_rotation). Repeating the call after
    /// the canonical empty header was synchronized is idempotent.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O error or [`JournalError::FreshGenerationInvalid`].
    pub fn prepare_slot(slot: &mut JournalSlot) -> Result<(), JournalError> {
        prepare_fresh_file(slot.file_mut())
    }

    /// Initializes a layout-owned fresh rotation descriptor durably.
    ///
    /// Repeating this call after the exact empty header was synchronized is
    /// idempotent. Any other existing bytes are preserved and rejected.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O error or [`JournalError::FreshGenerationInvalid`].
    pub fn prepare_rotation(rotation: &mut JournalRotation) -> Result<(), JournalError> {
        prepare_fresh_file(rotation.fresh_file_mut())
    }

    /// Opens an activated canonical or recognized alternate fresh generation.
    ///
    /// The layout slot transfers its exact writable descriptor and writer-lock
    /// lease. Only the synchronized canonical empty header is accepted.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, I/O, or fresh-generation error.
    pub fn open_slot(slot: JournalSlot, config: JournalConfig) -> Result<Self, JournalError> {
        validate_config(config)?;
        let (file, owner_lease) = slot.into_file_and_lease();
        let identity = FileIdentity::from_file(&file)?;
        if identity.len != JOURNAL_HEADER_LEN as u64 {
            return Err(JournalError::FreshGenerationInvalid { len: identity.len });
        }
        let mut encoded = [0_u8; JOURNAL_HEADER_LEN];
        file.read_exact_at(&mut encoded, 0)?;
        if encoded != JournalHeader::EMPTY.encode() || FileIdentity::from_file(&file)? != identity {
            return Err(JournalError::FreshGenerationInvalid { len: identity.len });
        }
        Ok(Self {
            _owner_lease: owner_lease,
            file,
            end: JOURNAL_HEADER_LEN,
            config,
            parts: Vec::new(),
            generation: next_journal_generation(),
            segment_id: None,
            poisoned: false,
        })
    }

    /// Opens or initializes the root journal through a writer-owner capability.
    ///
    /// Existing files are validated without truncation or repair. A newly
    /// created or zero-length file receives and synchronizes the canonical
    /// empty header before its root directory entry is synchronized.
    ///
    /// # Errors
    ///
    /// Returns a typed format, consistency, layout, or I/O error.
    pub fn open(owner: &WriterOwner, config: JournalConfig) -> Result<Self, JournalError> {
        validate_config(config)?;
        let owner_lease = owner.try_clone_lease()?;
        let (mut file, created) = owner.open_or_create_journal()?;
        if created {
            write_header(&mut file, JournalHeader::EMPTY)?;
            file.set_len(JOURNAL_HEADER_LEN as u64)?;
            file.sync_data()?;
        }

        let mut file_len = file.metadata()?.len();
        let max_journal_len = u64::try_from(config.max_journal_len).unwrap_or(u64::MAX);
        if file_len > max_journal_len {
            return Err(JournalError::JournalTooLarge {
                len: file_len,
                max: config.max_journal_len,
            });
        }
        if recover_committed_reset(&mut file, file_len, config)? {
            file_len = JOURNAL_HEADER_LEN as u64;
        }
        if file_len == 0 {
            // A crash between creation and the first header write leaves an
            // empty file; no journal state ever truncates below the header.
            write_header(&mut file, JournalHeader::EMPTY)?;
            file.set_len(JOURNAL_HEADER_LEN as u64)?;
            file.sync_data()?;
            file_len = JOURNAL_HEADER_LEN as u64;
        }
        if file_len < JOURNAL_HEADER_LEN as u64 {
            return Err(JournalError::TornHeader { len: file_len });
        }
        let mut header_bytes = [0_u8; JOURNAL_HEADER_LEN];
        file.read_exact_at(&mut header_bytes, 0)?;
        let header = JournalHeader::decode(header_bytes).map_err(|error| match error {
            JournalHeaderError::UnsupportedMagic { .. }
            | JournalHeaderError::UnsupportedVersion { .. } => {
                JournalError::UnsupportedJournalFormat
            }
            other => JournalError::InvalidHeader(other),
        })?;
        let actual_body_len = file_len - JOURNAL_HEADER_LEN as u64;
        if header.body_len != actual_body_len {
            return Err(JournalError::BodyLengthMismatch {
                recorded: header.body_len,
                actual: actual_body_len,
            });
        }

        let (segment_id, parts) = match header.state {
            JournalState::Empty => {
                if actual_body_len != 0 {
                    return Err(JournalError::EmptyWithFrames {
                        body_len: actual_body_len,
                    });
                }
                (None, Vec::new())
            }
            JournalState::Active { segment_id } => {
                let segment_id =
                    SegmentId::new(segment_id).map_err(JournalError::InvalidSegmentId)?;
                let scan = scan_journal_streaming_strict_from(
                    &file,
                    JOURNAL_HEADER_LEN as u64,
                    config.limits,
                    config.max_parts,
                )
                .map_err(map_scan_error)?;
                if !scan.damages.is_empty()
                    || u64::try_from(scan.valid_len).unwrap_or(u64::MAX) != file_len
                {
                    return Err(JournalError::DamagedBody {
                        damages: scan.damages,
                    });
                }
                if scan.parts.is_empty() {
                    return Err(JournalError::ActiveWithoutFirstFrame);
                }
                (Some(segment_id), scan.parts)
            }
        };
        let end = usize::try_from(file_len).map_err(|_overflow| {
            JournalError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "active.parts does not fit the address space",
            ))
        })?;
        // A preceding create may have synchronized the file but failed while
        // synchronizing its root entry. EEXIST alone is not durability proof.
        journal_failpoint!(OpenRootSync);
        owner.sync_root()?;
        let generation = next_journal_generation();
        let parts = parts
            .into_iter()
            .map(|raw| JournalPartRef::new(raw, generation))
            .collect();
        Ok(Self {
            _owner_lease: owner_lease,
            file,
            end,
            config,
            parts,
            generation,
            segment_id,
            poisoned: false,
        })
    }

    /// Complete file size, including the versioned header.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.end
    }

    /// Identity of the active segment, or `None` for the empty header.
    #[must_use]
    pub const fn segment_id(&self) -> Option<SegmentId> {
        self.segment_id
    }

    /// Appends one PGM part under the exact active segment identity.
    ///
    /// The first append writes the identity header and first frame before one
    /// shared synchronization boundary. Later appends reject another identity.
    ///
    /// # Errors
    ///
    /// Returns a validation, capacity, identity, or I/O error. A transient I/O
    /// error is rolled back to the preceding valid generation. A failed
    /// rollback permanently poisons this handle.
    pub fn append(
        &mut self,
        segment_id: SegmentId,
        part: &[u8],
    ) -> Result<JournalPartRef, JournalError> {
        self.ensure_healthy()?;
        if let Some(expected) = self.segment_id
            && expected != segment_id
        {
            return Err(JournalError::SegmentIdMismatch {
                expected,
                got: segment_id,
            });
        }
        let part_len = part.len() as u64;
        if part_len > self.config.limits.max_part_len {
            return Err(JournalError::PartTooLarge {
                len: part.len(),
                max: self.config.limits.max_part_len,
            });
        }
        // Validate before flow control. A full journal must not cause the
        // caller to publish/reset valid accumulated data for an invalid
        // incoming part.
        validate_part(part).map_err(JournalError::InvalidPart)?;

        if self.parts.len() >= self.config.max_parts {
            return Err(JournalError::TooManyParts {
                max: self.config.max_parts,
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
        let reset_end = next_end
            .checked_add(RESET_MARKER_LEN)
            .ok_or(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            })?;
        if reset_end > self.config.max_journal_len {
            return Err(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            });
        }
        let frame_header = FrameHeader { part_len }.encode();
        let old_header = self.current_header();
        let body_len = u64::try_from(next_end - JOURNAL_HEADER_LEN).map_err(|_overflow| {
            JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            }
        })?;
        let new_header = JournalHeader {
            state: JournalState::Active {
                segment_id: segment_id.get(),
            },
            body_len,
        };
        let write_result = (|| -> std::io::Result<()> {
            if self.parts.is_empty() {
                journal_failpoint!(AppendHeaderWrite);
                write_header(&mut self.file, new_header)?;
                self.write_frame(self.end, &frame_header, part)?;
            } else {
                self.write_frame(self.end, &frame_header, part)?;
                journal_failpoint!(AppendHeaderWrite);
                write_header(&mut self.file, new_header)?;
            }
            journal_failpoint!(AppendSync);
            self.file.sync_data()
        })();
        if let Err(error) = write_result {
            return match rollback(&mut self.file, self.end, old_header) {
                Ok(()) => Err(JournalError::Io(error)),
                Err(rollback) => {
                    self.poisoned = true;
                    Err(JournalError::RollbackFailed {
                        operation: error,
                        rollback,
                    })
                }
            };
        }

        let part_ref = JournalPartRef::new(
            PartRef {
                offset: self.end + FRAME_HEADER_LEN,
                len: part.len(),
            },
            self.generation,
        );
        self.end = next_end;
        self.parts.push(part_ref);
        self.segment_id = Some(segment_id);
        Ok(part_ref)
    }

    fn write_frame(&mut self, at: usize, header: &[u8], part: &[u8]) -> Result<(), std::io::Error> {
        self.file.seek(SeekFrom::Start(at as u64))?;
        journal_failpoint!(AppendFrameHeaderWrite);
        self.file.write_all(header)?;
        journal_failpoint!(AppendFrameBodyWrite);
        self.file.write_all(part)
    }

    fn current_header(&self) -> JournalHeader {
        JournalHeader {
            state: self
                .segment_id
                .map_or(JournalState::Empty, |segment_id| JournalState::Active {
                    segment_id: segment_id.get(),
                }),
            body_len: u64::try_from(self.end - JOURNAL_HEADER_LEN).unwrap_or(u64::MAX),
        }
    }

    /// Valid frame bodies in journal order.
    #[must_use]
    pub fn parts(&self) -> &[JournalPartRef] {
        &self.parts
    }

    /// Reads one referenced part body.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::StalePartRef`] for a reference from another
    /// generation, or an I/O error.
    pub fn read_part(&self, part: JournalPartRef) -> Result<Vec<u8>, JournalError> {
        self.check_part_ref(part)?;
        let mut body = vec![0_u8; part.raw.len];
        self.file.read_exact_at(&mut body, part.raw.offset as u64)?;
        Ok(body)
    }

    /// Read a bounded byte range relative to one part body.
    ///
    /// Sealing uses this after catalog validation so each Parquet body is read
    /// once without allocating the rest of its journal part again.
    pub(crate) fn read_part_range(
        &self,
        part: JournalPartRef,
        offset: usize,
        len: usize,
    ) -> Result<Vec<u8>, JournalError> {
        self.check_part_ref(part)?;
        let Some(relative_end) = offset.checked_add(len) else {
            return Err(JournalError::StalePartRef { offset, len });
        };
        if relative_end > part.raw.len {
            return Err(JournalError::StalePartRef { offset, len });
        }
        let absolute = part
            .raw
            .offset
            .checked_add(offset)
            .ok_or(JournalError::StalePartRef { offset, len })?;
        let mut body = vec![0_u8; len];
        self.file.read_exact_at(&mut body, absolute as u64)?;
        Ok(body)
    }

    fn check_part_ref(&self, part: JournalPartRef) -> Result<(), JournalError> {
        self.ensure_healthy()?;
        let minimum = JOURNAL_HEADER_LEN + FRAME_HEADER_LEN;
        let is_member = part.generation == self.generation
            && self
                .parts
                .binary_search_by_key(&part.raw.offset, |candidate| candidate.raw.offset)
                .is_ok_and(|index| self.parts[index] == part);
        let in_bounds = part.raw.offset >= minimum
            && part
                .raw
                .offset
                .checked_add(part.raw.len)
                .is_some_and(|end| end <= self.end);
        if !is_member || !in_bounds {
            return Err(JournalError::StalePartRef {
                offset: part.raw.offset,
                len: part.raw.len,
            });
        }
        Ok(())
    }

    /// Commits a reset marker, then reduces the journal to the synchronized
    /// canonical empty version-1 header.
    ///
    /// A crash after the marker sync is completed by [`open`](Self::open).
    /// Until that marker is durable, a failed write is rolled back to the
    /// preceding active generation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error. In-memory state changes only after persistence.
    pub fn reset(&mut self) -> Result<(), JournalError> {
        self.ensure_healthy()?;
        if self.parts.is_empty() {
            return Ok(());
        }
        let Some(segment_id) = self.segment_id else {
            self.poisoned = true;
            return Err(JournalError::Poisoned);
        };
        let next_generation = next_journal_generation();
        let marker_len = u64::try_from(self.end).map_err(|_overflow| JournalError::Full {
            len: self.end,
            max: self.config.max_journal_len,
        })?;
        if self
            .end
            .checked_add(RESET_MARKER_LEN)
            .is_none_or(|marker_end| marker_end > self.config.max_journal_len)
        {
            return Err(JournalError::Full {
                len: self.end,
                max: self.config.max_journal_len,
            });
        }
        let old_header = self.current_header();
        let Some(marker) = ResetMarker::new(marker_len, segment_id.get()) else {
            self.poisoned = true;
            return Err(JournalError::Poisoned);
        };
        let marker = marker.encode();

        let marker_result = (|| -> std::io::Result<()> {
            self.file.seek(SeekFrom::Start(self.end as u64))?;
            journal_failpoint!(ResetMarkerWrite);
            self.file.write_all(&marker)?;
            journal_failpoint!(ResetMarkerSync);
            self.file.sync_data()
        })();
        if let Err(error) = marker_result {
            return match rollback(&mut self.file, self.end, old_header) {
                Ok(()) => Err(JournalError::Io(error)),
                Err(rollback) => {
                    self.poisoned = true;
                    Err(JournalError::RollbackFailed {
                        operation: error,
                        rollback,
                    })
                }
            };
        }

        if let Err(error) = finish_committed_reset(&mut self.file) {
            self.poisoned = true;
            return Err(JournalError::ResetIncomplete(error));
        }
        self.end = JOURNAL_HEADER_LEN;
        self.parts.clear();
        self.generation = next_generation;
        self.segment_id = None;
        Ok(())
    }

    /// Complete journal length, including the header.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end
    }

    /// Whether the journal is in its canonical empty state.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Whether a partial persistence failure made further use unsafe.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    const fn ensure_healthy(&self) -> Result<(), JournalError> {
        if self.poisoned {
            Err(JournalError::Poisoned)
        } else {
            Ok(())
        }
    }
}

fn write_header(file: &mut File, header: JournalHeader) -> Result<(), std::io::Error> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header.encode())
}

fn prepare_fresh_file(file: &mut File) -> Result<(), JournalError> {
    let len = file.metadata()?.len();
    if len == 0 {
        write_header(file, JournalHeader::EMPTY)?;
        file.set_len(JOURNAL_HEADER_LEN as u64)?;
        file.sync_data()?;
        return Ok(());
    }
    if len != JOURNAL_HEADER_LEN as u64 {
        return Err(JournalError::FreshGenerationInvalid { len });
    }
    let identity = FileIdentity::from_file(file)?;
    let mut encoded = [0_u8; JOURNAL_HEADER_LEN];
    file.read_exact_at(&mut encoded, 0)?;
    if encoded != JournalHeader::EMPTY.encode() || FileIdentity::from_file(file)? != identity {
        return Err(JournalError::FreshGenerationInvalid { len });
    }
    file.sync_data()?;
    Ok(())
}

fn rollback(file: &mut File, end: usize, header: JournalHeader) -> Result<(), std::io::Error> {
    journal_failpoint!(RollbackTruncate);
    file.set_len(end as u64)?;
    journal_failpoint!(RollbackHeaderWrite);
    write_header(file, header)?;
    journal_failpoint!(RollbackSync);
    file.sync_data()
}

pub(crate) const fn validate_config(config: JournalConfig) -> Result<(), JournalError> {
    if config.max_journal_len < JOURNAL_HEADER_LEN || config.max_journal_len > MAX_JOURNAL_LEN {
        return Err(JournalError::InvalidMaxJournalLen {
            value: config.max_journal_len,
            minimum: JOURNAL_HEADER_LEN,
            maximum: MAX_JOURNAL_LEN,
        });
    }
    if config.max_parts == 0 || config.max_parts > MAX_JOURNAL_PARTS {
        return Err(JournalError::InvalidMaxParts {
            value: config.max_parts,
            minimum: 1,
            maximum: MAX_JOURNAL_PARTS,
        });
    }
    if config.limits.max_part_len == 0 || config.limits.max_part_len > MAX_PART_LEN {
        return Err(JournalError::InvalidMaxPartLen {
            value: config.limits.max_part_len,
            minimum: 1,
            maximum: MAX_PART_LEN,
        });
    }
    Ok(())
}

fn next_journal_generation() -> u64 {
    NEXT_JOURNAL_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .expect("journal generation counter exhausted")
}

fn map_scan_error(error: JournalScanError) -> JournalError {
    match error {
        JournalScanError::Io(error) => JournalError::Io(error),
        JournalScanError::PartLimitExceeded { limit } => JournalError::TooManyParts { max: limit },
    }
}

struct PrefixReader<'a> {
    file: &'a File,
    len: u64,
}

impl kronika_format::ReadAt for PrefixReader<'_> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let requested = u64::try_from(buf.len())
            .map_err(|_overflow| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        if offset
            .checked_add(requested)
            .is_none_or(|end| end > self.len)
        {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        FileExt::read_exact_at(self.file, buf, offset)
    }

    fn byte_len(&self) -> std::io::Result<u64> {
        Ok(self.len)
    }
}

fn finish_committed_reset(file: &mut File) -> Result<(), std::io::Error> {
    journal_failpoint!(ResetEmptyHeaderWrite);
    write_header(file, JournalHeader::EMPTY)?;
    journal_failpoint!(ResetEmptyHeaderSync);
    file.sync_data()?;
    journal_failpoint!(ResetTruncate);
    file.set_len(JOURNAL_HEADER_LEN as u64)?;
    journal_failpoint!(ResetFinalSync);
    file.sync_data()
}

fn recover_committed_reset(
    file: &mut File,
    file_len: u64,
    config: JournalConfig,
) -> Result<bool, JournalError> {
    let minimum = (JOURNAL_HEADER_LEN + RESET_MARKER_LEN) as u64;
    if file_len < minimum {
        return Ok(false);
    }
    let marker_at = file_len - RESET_MARKER_LEN as u64;
    let mut bytes = [0_u8; RESET_MARKER_LEN];
    file.read_exact_at(&mut bytes, marker_at)?;
    let Some(marker) = ResetMarker::decode(bytes) else {
        return Ok(false);
    };
    if marker.previous_len != marker_at
        || marker.previous_len > u64::try_from(config.max_journal_len).unwrap_or(u64::MAX)
        || SegmentId::new(marker.previous_segment_id).is_err()
    {
        return Ok(false);
    }
    let Some(_expected_header) = marker.expected_previous_header() else {
        return Ok(false);
    };
    let mut header_bytes = [0_u8; JOURNAL_HEADER_LEN];
    file.read_exact_at(&mut header_bytes, 0)?;
    if marker.classify_header_transition(header_bytes).is_none() {
        return Ok(false);
    }
    let previous = PrefixReader {
        file,
        len: marker.previous_len,
    };
    let scan = scan_journal_streaming_strict_from(
        &previous,
        JOURNAL_HEADER_LEN as u64,
        config.limits,
        config.max_parts,
    )
    .map_err(map_scan_error)?;
    if scan.parts.is_empty()
        || !scan.damages.is_empty()
        || u64::try_from(scan.valid_len).unwrap_or(u64::MAX) != marker.previous_len
    {
        return Ok(false);
    }
    finish_committed_reset(file)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt as _;

    use kronika_format::{Catalog, Entry, FORMAT_VERSION, MAGIC, crc32c};
    use kronika_layout::{ACTIVE_JOURNAL_NAME, DataRoot, LayoutLimits, OwnerKind};

    use super::*;

    fn sample_part_with_section(section: &[u8]) -> Vec<u8> {
        let mut part = Vec::new();
        part.extend_from_slice(&MAGIC);
        part.extend_from_slice(section);
        let catalog = Catalog {
            entries: vec![Entry {
                type_id: 1_006_001,
                flags: 0,
                offset: 4,
                len: section.len() as u64,
                rows: 1,
                crc32c: crc32c(section),
            }],
            min_ts: 1,
            max_ts: 2,
            source_id: 0,
            format_version: FORMAT_VERSION,
            window_count: 1,
        };
        part.extend_from_slice(&catalog.encode());
        assert!(validate_part(&part).is_ok());
        part
    }

    fn sample_part() -> Vec<u8> {
        sample_part_with_section(b"data")
    }

    fn owner(directory: &tempfile::TempDir) -> WriterOwner {
        DataRoot::open(directory.path())
            .unwrap()
            .acquire_writer(LayoutLimits::default())
            .unwrap()
    }

    fn id(value: i64) -> SegmentId {
        SegmentId::new(value).unwrap()
    }

    fn rejected_config(config: JournalConfig) -> JournalError {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let error = Journal::open(&owner, config).unwrap_err();
        assert!(!directory.path().join("active.parts").exists());
        error
    }

    fn injected_operation_raw_os_error(error: &JournalError) -> Option<i32> {
        match error {
            JournalError::Io(source) | JournalError::ResetIncomplete(source) => {
                source.raw_os_error()
            }
            _ => None,
        }
    }

    #[test]
    fn append_read_reopen_roundtrip() {
        let directory = tempfile::tempdir().expect("tempdir");
        let owner = owner(&directory);
        let part = sample_part();
        let segment_id = id(1_000);

        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open");
        let first = journal.append(segment_id, &part).expect("append");
        let second = journal.append(segment_id, &part).expect("append");
        assert_eq!(journal.parts(), &[first, second]);
        assert_eq!(journal.read_part(first).expect("read"), part);
        assert_eq!(
            journal
                .read_part_range(first, MAGIC.len(), 4)
                .expect("read range"),
            b"data"
        );
        assert!(matches!(
            journal.read_part_range(first, part.len() - 1, 2),
            Err(JournalError::StalePartRef { .. })
        ));

        drop(journal);
        let journal = Journal::open(&owner, JournalConfig::default()).expect("reopen");
        assert_eq!(journal.parts().len(), 2);
        assert_eq!(journal.read_part(journal.parts()[1]).expect("read"), part);
    }

    #[test]
    fn prepared_rotated_slot_opens_as_a_fresh_journal() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        drop(journal);

        let mut rotation = owner.begin_journal_rotation().unwrap();
        Journal::prepare_rotation(&mut rotation).unwrap();
        Journal::prepare_rotation(&mut rotation).expect("preparation is restart-idempotent");
        let outcome = rotation.activate();
        let journal = Journal::open_slot(outcome.fresh, JournalConfig::default()).unwrap();

        assert!(journal.is_empty());
        assert_eq!(journal.segment_id(), None);
        assert_eq!(journal.len(), JOURNAL_HEADER_LEN);
        assert_eq!(
            outcome.evidence.file().metadata().unwrap().len(),
            JOURNAL_HEADER_LEN as u64
        );
    }

    #[test]
    fn rotation_preparation_preserves_and_rejects_unexpected_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        drop(journal);

        let mut rotation = owner.begin_journal_rotation().unwrap();
        rotation.fresh_file_mut().write_all(b"x").unwrap();
        assert!(matches!(
            Journal::prepare_rotation(&mut rotation),
            Err(JournalError::FreshGenerationInvalid { len: 1 })
        ));
        assert_eq!(rotation.fresh_file_mut().metadata().unwrap().len(), 1);
    }

    #[test]
    fn incomplete_final_frame_is_rejected_and_preserved_on_open() {
        let directory = tempfile::tempdir().expect("tempdir");
        let owner = owner(&directory);
        let path = directory.path().join(ACTIVE_JOURNAL_NAME);
        let part = sample_part();

        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open");
        journal.append(id(1_000), &part).expect("append");
        drop(journal);

        let mut file = OpenOptions::new().append(true).open(&path).expect("raw");
        let partial_frame_header = FrameHeader {
            part_len: part.len() as u64,
        }
        .encode();
        file.write_all(&partial_frame_header).expect("write");
        file.write_all(&part[..part.len() / 2]).expect("write");
        drop(file);
        let before = std::fs::read(&path).expect("read damaged journal");

        assert!(matches!(
            Journal::open(&owner, JournalConfig::default()),
            Err(JournalError::BodyLengthMismatch { .. })
        ));
        assert_eq!(std::fs::read(path).expect("read preserved journal"), before);
    }

    #[test]
    fn damaged_frame_is_rejected_and_preserved_on_open() {
        let directory = tempfile::tempdir().expect("tempdir");
        let owner = owner(&directory);
        let path = directory.path().join(ACTIVE_JOURNAL_NAME);
        let part = sample_part();

        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open");
        let part_ref = journal.append(id(1_000), &part).expect("append");
        drop(journal);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("raw");
        let frame_at = part_ref.offset() - FRAME_HEADER_LEN;
        file.write_all_at(&[0], frame_at as u64)
            .expect("damage frame magic");
        file.sync_all().expect("persist damage");
        let before = std::fs::read(&path).expect("read damaged journal");

        assert!(matches!(
            Journal::open(&owner, JournalConfig::default()),
            Err(JournalError::DamagedBody { .. })
        ));
        assert_eq!(std::fs::read(path).expect("read preserved journal"), before);
    }

    #[test]
    fn reset_empties_the_journal() {
        let directory = tempfile::tempdir().expect("tempdir");
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open");
        journal.append(id(1_000), &sample_part()).expect("append");
        journal.reset().expect("reset");
        assert!(journal.is_empty());
        assert_eq!(journal.len(), JOURNAL_HEADER_LEN);
    }

    #[test]
    fn fresh_journal_has_a_durable_empty_v1_header() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        assert!(journal.is_empty());
        assert_eq!(journal.len(), JOURNAL_HEADER_LEN);
        let bytes = std::fs::read(directory.path().join("active.parts")).unwrap();
        assert_eq!(
            JournalHeader::decode(bytes.try_into().unwrap()).unwrap(),
            JournalHeader::EMPTY
        );
    }

    #[test]
    fn existing_journal_retries_root_sync_after_initialization_sync_failure() {
        const FIRST_ERROR: i32 = 5;
        const RETRY_ERROR: i32 = 116;

        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let path = directory.path().join("active.parts");

        let first_faults = arm_journal_faults([(JournalFaultPoint::OpenRootSync, FIRST_ERROR)]);
        let first_error = Journal::open(&owner, JournalConfig::default())
            .expect_err("the initial root sync failure must reject open");
        assert_eq!(
            injected_operation_raw_os_error(&first_error),
            Some(FIRST_ERROR)
        );
        first_faults.assert_consumed();
        assert_eq!(
            JournalHeader::decode(std::fs::read(&path).unwrap().try_into().unwrap()).unwrap(),
            JournalHeader::EMPTY,
            "the file was initialized before its root sync failed"
        );

        let retry_faults = arm_journal_faults([(JournalFaultPoint::OpenRootSync, RETRY_ERROR)]);
        let retry_error = Journal::open(&owner, JournalConfig::default())
            .expect_err("an existing valid journal must retry the root sync");
        assert_eq!(
            injected_operation_raw_os_error(&retry_error),
            Some(RETRY_ERROR)
        );
        retry_faults.assert_consumed();

        let journal = Journal::open(&owner, JournalConfig::default())
            .expect("the next retry proves root-entry durability");
        assert!(journal.is_empty());
    }

    #[test]
    fn invalid_length_config_is_rejected_before_creating_the_journal() {
        assert!(matches!(
            rejected_config(JournalConfig {
                max_journal_len: JOURNAL_HEADER_LEN - 1,
                ..JournalConfig::default()
            }),
            JournalError::InvalidMaxJournalLen {
                value,
                minimum: JOURNAL_HEADER_LEN,
                maximum: MAX_JOURNAL_LEN,
            } if value == JOURNAL_HEADER_LEN - 1
        ));
        assert!(matches!(
            rejected_config(JournalConfig {
                max_journal_len: MAX_JOURNAL_LEN + 1,
                ..JournalConfig::default()
            }),
            JournalError::InvalidMaxJournalLen {
                value,
                minimum: JOURNAL_HEADER_LEN,
                maximum: MAX_JOURNAL_LEN,
            } if value == MAX_JOURNAL_LEN + 1
        ));
    }

    #[test]
    fn invalid_part_count_config_is_rejected_before_creating_the_journal() {
        assert!(matches!(
            rejected_config(JournalConfig {
                max_parts: 0,
                ..JournalConfig::default()
            }),
            JournalError::InvalidMaxParts {
                value: 0,
                minimum: 1,
                maximum: MAX_JOURNAL_PARTS,
            }
        ));
        assert!(matches!(
            rejected_config(JournalConfig {
                max_parts: MAX_JOURNAL_PARTS + 1,
                ..JournalConfig::default()
            }),
            JournalError::InvalidMaxParts {
                value,
                minimum: 1,
                maximum: MAX_JOURNAL_PARTS,
            } if value == MAX_JOURNAL_PARTS + 1
        ));
    }

    #[test]
    fn invalid_part_length_config_is_rejected_before_creating_the_journal() {
        assert!(matches!(
            rejected_config(JournalConfig {
                limits: JournalLimits { max_part_len: 0 },
                ..JournalConfig::default()
            }),
            JournalError::InvalidMaxPartLen {
                value: 0,
                minimum: 1,
                maximum: MAX_PART_LEN,
            }
        ));
        assert!(matches!(
            rejected_config(JournalConfig {
                limits: JournalLimits {
                    max_part_len: MAX_PART_LEN + 1,
                },
                ..JournalConfig::default()
            }),
            JournalError::InvalidMaxPartLen {
                value,
                minimum: 1,
                maximum: MAX_PART_LEN,
            } if value == MAX_PART_LEN + 1
        ));
    }

    #[test]
    fn exact_header_length_cap_admits_only_an_empty_journal() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let config = JournalConfig {
            max_journal_len: JOURNAL_HEADER_LEN,
            ..JournalConfig::default()
        };
        let mut journal = Journal::open(&owner, config).unwrap();
        assert!(journal.is_empty());
        assert!(matches!(
            journal.append(id(1_000), &sample_part()),
            Err(JournalError::Full {
                len: JOURNAL_HEADER_LEN,
                max: JOURNAL_HEADER_LEN
            })
        ));
        assert_eq!(
            std::fs::metadata(directory.path().join("active.parts"))
                .unwrap()
                .len(),
            JOURNAL_HEADER_LEN as u64
        );
    }

    #[test]
    fn first_append_persists_identity_and_frame_together() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_709_164_800_000_000);
        let part = sample_part();
        let part_ref = journal.append(segment_id, &part).unwrap();
        assert_eq!(journal.segment_id(), Some(segment_id));
        assert_eq!(journal.read_part(part_ref).unwrap(), part);
        drop(journal);

        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        assert_eq!(journal.segment_id(), Some(segment_id));
        assert_eq!(journal.parts().len(), 1);
    }

    #[test]
    fn every_first_append_write_and_sync_fault_reopens_as_empty() {
        const INJECTED_EIO: i32 = 5;
        for point in [
            JournalFaultPoint::AppendHeaderWrite,
            JournalFaultPoint::AppendFrameHeaderWrite,
            JournalFaultPoint::AppendFrameBodyWrite,
            JournalFaultPoint::AppendSync,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let owner = owner(&directory);
            let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
            let before = std::fs::read(directory.path().join("active.parts")).unwrap();
            let faults = arm_journal_faults([(point, INJECTED_EIO)]);

            let error = journal
                .append(id(1_000), &sample_part())
                .expect_err("an injected append fault cannot report success");
            assert_eq!(
                injected_operation_raw_os_error(&error),
                Some(INJECTED_EIO),
                "{point:?} must preserve the injected I/O error"
            );
            assert!(!journal.is_poisoned(), "{point:?} rollback must succeed");
            faults.assert_consumed();
            drop(journal);

            assert_eq!(
                std::fs::read(directory.path().join("active.parts")).unwrap(),
                before,
                "{point:?} must restore the exact empty journal"
            );
            let reopened = Journal::open(&owner, JournalConfig::default()).unwrap();
            assert!(reopened.is_empty(), "{point:?} invented an active append");
        }
    }

    #[test]
    fn every_later_append_write_and_sync_fault_preserves_the_previous_generation() {
        const INJECTED_EIO: i32 = 5;
        for point in [
            JournalFaultPoint::AppendFrameHeaderWrite,
            JournalFaultPoint::AppendFrameBodyWrite,
            JournalFaultPoint::AppendHeaderWrite,
            JournalFaultPoint::AppendSync,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let owner = owner(&directory);
            let first = sample_part();
            let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
            journal.append(id(1_000), &first).unwrap();
            let before = std::fs::read(directory.path().join("active.parts")).unwrap();
            let faults = arm_journal_faults([(point, INJECTED_EIO)]);

            let error = journal
                .append(id(1_000), &sample_part_with_section(b"later"))
                .expect_err("an injected append fault cannot report success");
            assert_eq!(
                injected_operation_raw_os_error(&error),
                Some(INJECTED_EIO),
                "{point:?} must preserve the injected I/O error"
            );
            assert!(!journal.is_poisoned(), "{point:?} rollback must succeed");
            faults.assert_consumed();
            drop(journal);

            assert_eq!(
                std::fs::read(directory.path().join("active.parts")).unwrap(),
                before,
                "{point:?} must restore the exact previous generation"
            );
            let reopened = Journal::open(&owner, JournalConfig::default()).unwrap();
            assert_eq!(reopened.parts().len(), 1);
            assert_eq!(reopened.read_part(reopened.parts()[0]).unwrap(), first);
        }
    }

    #[test]
    fn rollback_faults_poison_the_handle_and_reopen_never_accepts_a_partial_append() {
        const INJECTED_EIO: i32 = 5;
        const INJECTED_ENOSPC: i32 = 28;
        for rollback_point in [
            JournalFaultPoint::RollbackTruncate,
            JournalFaultPoint::RollbackHeaderWrite,
            JournalFaultPoint::RollbackSync,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let owner = owner(&directory);
            let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
            let canonical_empty = std::fs::read(directory.path().join("active.parts")).unwrap();
            let faults = arm_journal_faults([
                (JournalFaultPoint::AppendFrameBodyWrite, INJECTED_EIO),
                (rollback_point, INJECTED_ENOSPC),
            ]);

            let error = journal
                .append(id(1_000), &sample_part())
                .expect_err("a failed rollback cannot report success");
            assert!(
                matches!(
                    &error,
                    JournalError::RollbackFailed {
                        operation,
                        rollback,
                    } if operation.raw_os_error() == Some(INJECTED_EIO)
                        && rollback.raw_os_error() == Some(INJECTED_ENOSPC)
                ),
                "{rollback_point:?} returned {error:?}"
            );
            assert!(journal.is_poisoned());
            faults.assert_consumed();
            let interrupted = std::fs::read(directory.path().join("active.parts")).unwrap();
            drop(journal);

            let reopened = Journal::open(&owner, JournalConfig::default());
            if rollback_point == JournalFaultPoint::RollbackSync {
                let reopened = reopened.expect("the restored old bytes remain a valid journal");
                assert!(reopened.is_empty());
                assert_eq!(interrupted, canonical_empty);
            } else {
                assert!(
                    matches!(reopened, Err(JournalError::BodyLengthMismatch { .. })),
                    "{rollback_point:?} must diagnose, not accept, the interrupted append"
                );
                assert_eq!(
                    std::fs::read(directory.path().join("active.parts")).unwrap(),
                    interrupted,
                    "{rollback_point:?} reopen must preserve the damaged evidence"
                );
            }
        }
    }

    #[test]
    fn a_reference_is_stale_after_reset_even_when_raw_location_is_reused() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        let stale = journal.append(segment_id, &sample_part()).unwrap();
        journal.reset().unwrap();

        let replacement = sample_part_with_section(b"more");
        let fresh = journal.append(segment_id, &replacement).unwrap();
        assert_eq!(stale.offset(), fresh.offset());
        assert_eq!(stale.len(), fresh.len());
        assert_ne!(stale.generation, fresh.generation);
        assert!(matches!(
            journal.read_part(stale),
            Err(JournalError::StalePartRef { .. })
        ));
        assert_eq!(journal.read_part(fresh).unwrap(), replacement);
    }

    #[test]
    fn a_reference_from_a_previous_open_is_stale() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let part = sample_part();
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let stale = journal.append(id(1_000), &part).unwrap();
        drop(journal);

        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let current = journal.parts()[0];
        assert_eq!(stale.offset(), current.offset());
        assert_eq!(stale.len(), current.len());
        assert_ne!(stale.generation, current.generation);
        assert!(matches!(
            journal.read_part(stale),
            Err(JournalError::StalePartRef { .. })
        ));
        assert_eq!(journal.read_part(current).unwrap(), part);
    }

    #[test]
    fn a_fabricated_in_bounds_reference_is_rejected_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let genuine = journal.append(id(1_000), &sample_part()).unwrap();
        let fabricated = JournalPartRef::new(
            PartRef {
                offset: genuine.offset() + 1,
                len: genuine.len() - 1,
            },
            journal.generation,
        );
        assert!(fabricated.offset() + fabricated.len() <= journal.len());
        assert!(matches!(
            journal.read_part(fabricated),
            Err(JournalError::StalePartRef { .. })
        ));
        assert!(journal.read_part(genuine).is_ok());
    }

    #[test]
    fn another_segment_id_is_rejected_before_writing() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        journal.append(id(1_000), &sample_part()).unwrap();
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        assert!(matches!(
            journal.append(id(2_000), &sample_part()),
            Err(JournalError::SegmentIdMismatch { .. })
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn reset_writes_an_empty_header_instead_of_truncating_to_zero() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        journal.append(id(1_000), &sample_part()).unwrap();
        journal.reset().unwrap();
        assert_eq!(
            std::fs::metadata(directory.path().join("active.parts"))
                .unwrap()
                .len(),
            JOURNAL_HEADER_LEN as u64
        );
        assert_eq!(journal.segment_id(), None);
    }

    #[test]
    fn every_reset_write_truncate_and_sync_fault_reopens_as_old_or_empty() {
        const INJECTED_EIO: i32 = 5;
        for (point, committed) in [
            (JournalFaultPoint::ResetMarkerWrite, false),
            (JournalFaultPoint::ResetMarkerSync, false),
            (JournalFaultPoint::ResetEmptyHeaderWrite, true),
            (JournalFaultPoint::ResetEmptyHeaderSync, true),
            (JournalFaultPoint::ResetTruncate, true),
            (JournalFaultPoint::ResetFinalSync, true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let owner = owner(&directory);
            let part = sample_part();
            let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
            journal.append(id(1_000), &part).unwrap();
            let active_before = std::fs::read(directory.path().join("active.parts")).unwrap();
            let faults = arm_journal_faults([(point, INJECTED_EIO)]);

            let error = journal
                .reset()
                .expect_err("an injected reset fault cannot report success");
            assert_eq!(
                injected_operation_raw_os_error(&error),
                Some(INJECTED_EIO),
                "{point:?} must preserve the injected I/O error"
            );
            assert_eq!(
                journal.is_poisoned(),
                committed,
                "{point:?} poison state must follow the reset commit boundary"
            );
            faults.assert_consumed();
            drop(journal);

            let reopened = Journal::open(&owner, JournalConfig::default()).unwrap();
            if committed {
                assert!(reopened.is_empty(), "{point:?} lost a committed reset");
                assert_eq!(reopened.len(), JOURNAL_HEADER_LEN);
            } else {
                assert_eq!(
                    std::fs::read(directory.path().join("active.parts")).unwrap(),
                    active_before,
                    "{point:?} must restore the exact active generation"
                );
                assert_eq!(reopened.parts().len(), 1);
                assert_eq!(reopened.read_part(reopened.parts()[0]).unwrap(), part);
            }
        }
    }

    #[test]
    fn open_completes_a_reset_committed_before_the_final_truncate() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        journal.append(segment_id, &sample_part()).unwrap();
        let previous_len = journal.end as u64;
        let marker = ResetMarker::new(previous_len, segment_id.get())
            .unwrap()
            .encode();
        journal
            .file
            .write_all_at(&marker, previous_len)
            .expect("write committed marker");
        journal.file.sync_data().expect("commit reset marker");
        drop(journal);

        let recovered = Journal::open(&owner, JournalConfig::default()).unwrap();
        assert!(recovered.is_empty());
        assert_eq!(recovered.len(), JOURNAL_HEADER_LEN);
    }

    #[test]
    fn open_completes_a_reset_after_the_root_header_was_partly_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        journal.append(segment_id, &sample_part()).unwrap();
        let previous_len = journal.end as u64;
        let marker = ResetMarker::new(previous_len, segment_id.get())
            .unwrap()
            .encode();
        journal
            .file
            .write_all_at(&marker, previous_len)
            .expect("write committed marker");
        journal.file.sync_data().expect("commit reset marker");
        journal
            .file
            .write_all_at(&JournalHeader::EMPTY.encode()[..17], 0)
            .expect("simulate interrupted root-header replacement");
        journal.file.sync_data().expect("persist interrupted state");
        drop(journal);

        let recovered = Journal::open(&owner, JournalConfig::default()).unwrap();
        assert!(recovered.is_empty());
        assert_eq!(recovered.len(), JOURNAL_HEADER_LEN);
    }

    #[test]
    fn a_marker_with_an_inconsistent_previous_header_is_not_a_reset_commit() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        journal.append(segment_id, &sample_part()).unwrap();
        let previous_len = journal.end as u64;
        let forged = ResetMarker {
            previous_len,
            previous_segment_id: segment_id.get(),
            previous_header_crc: 0,
        }
        .encode();
        journal.file.write_all_at(&forged, previous_len).unwrap();
        journal.file.sync_data().unwrap();
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        drop(journal);

        assert!(Journal::open(&owner, JournalConfig::default()).is_err());
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn a_valid_marker_does_not_reset_an_unrelated_header() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        journal.append(segment_id, &sample_part()).unwrap();
        let previous_len = journal.end as u64;
        let marker = ResetMarker::new(previous_len, segment_id.get())
            .unwrap()
            .encode();
        journal.file.write_all_at(&marker, previous_len).unwrap();
        journal.file.write_all_at(b"NOT-V1!!", 0).unwrap();
        journal.file.sync_data().unwrap();
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        drop(journal);

        assert!(matches!(
            Journal::open(&owner, JournalConfig::default()),
            Err(JournalError::UnsupportedJournalFormat)
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn a_valid_marker_does_not_reset_a_damaged_previous_body() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        journal.append(segment_id, &sample_part()).unwrap();
        let previous_len = journal.end as u64;
        let marker = ResetMarker::new(previous_len, segment_id.get())
            .unwrap()
            .encode();
        journal.file.write_all_at(&marker, previous_len).unwrap();
        let section_at = journal.parts()[0].offset() as u64 + MAGIC.len() as u64;
        journal.file.write_all_at(&[0xFF], section_at).unwrap();
        journal.file.sync_data().unwrap();
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        drop(journal);

        assert!(Journal::open(&owner, JournalConfig::default()).is_err());
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn configured_length_cap_rejects_an_existing_larger_journal() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        journal.append(id(1_000), &sample_part()).unwrap();
        let len = journal.len();
        drop(journal);
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        let config = JournalConfig {
            max_journal_len: len - 1,
            ..JournalConfig::default()
        };

        assert!(matches!(
            Journal::open(&owner, config),
            Err(JournalError::JournalTooLarge { .. })
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn physical_length_cap_is_checked_before_committed_reset_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let mut journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        let segment_id = id(1_000);
        journal.append(segment_id, &sample_part()).unwrap();
        let previous_len = journal.len() as u64;
        let marker = ResetMarker::new(previous_len, segment_id.get())
            .unwrap()
            .encode();
        journal.file.write_all_at(&marker, previous_len).unwrap();
        journal.file.sync_data().unwrap();
        drop(journal);

        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        let config = JournalConfig {
            max_journal_len: before.len() - 1,
            ..JournalConfig::default()
        };
        assert!(matches!(
            Journal::open(&owner, config),
            Err(JournalError::JournalTooLarge { len, max })
                if len == before.len() as u64 && max == before.len() - 1
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn configured_length_cap_applies_to_the_first_and_later_appends() {
        let part = sample_part();
        let one_frame_len = JOURNAL_HEADER_LEN + FRAME_HEADER_LEN + part.len();
        let reset_peak_len = one_frame_len + RESET_MARKER_LEN;

        let first_directory = tempfile::tempdir().unwrap();
        let first_owner = owner(&first_directory);
        let first_config = JournalConfig {
            max_journal_len: reset_peak_len - 1,
            ..JournalConfig::default()
        };
        let mut first = Journal::open(&first_owner, first_config).unwrap();
        assert!(matches!(
            first.append(id(1_000), &part),
            Err(JournalError::Full { .. })
        ));
        assert_eq!(first.len(), JOURNAL_HEADER_LEN);

        let later_directory = tempfile::tempdir().unwrap();
        let later_owner = owner(&later_directory);
        let later_config = JournalConfig {
            max_journal_len: reset_peak_len,
            ..JournalConfig::default()
        };
        let mut later = Journal::open(&later_owner, later_config).unwrap();
        later.append(id(1_000), &part).unwrap();
        let before = std::fs::read(later_directory.path().join("active.parts")).unwrap();
        assert!(matches!(
            later.append(id(1_000), &part),
            Err(JournalError::Full { .. })
        ));
        assert_eq!(
            std::fs::read(later_directory.path().join("active.parts")).unwrap(),
            before
        );
        later.reset().unwrap();
        assert_eq!(later.len(), JOURNAL_HEADER_LEN);
        assert_eq!(
            std::fs::metadata(later_directory.path().join("active.parts"))
                .unwrap()
                .len(),
            JOURNAL_HEADER_LEN as u64
        );
    }

    #[test]
    fn part_count_cap_applies_to_append_and_open() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let config = JournalConfig {
            max_parts: 2,
            ..JournalConfig::default()
        };
        let mut journal = Journal::open(&owner, config).unwrap();
        let part = sample_part();
        journal.append(id(1_000), &part).unwrap();
        journal.append(id(1_000), &part).unwrap();
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        assert!(matches!(
            journal.append(id(1_000), &part),
            Err(JournalError::TooManyParts { max: 2 })
        ));
        drop(journal);

        let strict = JournalConfig {
            max_parts: 1,
            ..JournalConfig::default()
        };
        assert!(matches!(
            Journal::open(&owner, strict),
            Err(JournalError::TooManyParts { max: 1 })
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn journal_keeps_writer_ownership_after_the_original_owner_is_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let first_root = DataRoot::open(directory.path()).unwrap();
        let second_root = DataRoot::open(directory.path()).unwrap();
        let owner = first_root.acquire_writer(LayoutLimits::default()).unwrap();
        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        drop(owner);

        assert!(matches!(
            second_root.acquire_writer(LayoutLimits::default()),
            Err(LayoutError::OwnerContended {
                owner: OwnerKind::Writer
            })
        ));
        drop(journal);
        second_root.acquire_writer(LayoutLimits::default()).unwrap();
    }

    #[test]
    fn zero_length_journal_is_reinitialized_to_the_empty_header() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("active.parts"), []).unwrap();
        let owner = owner(&directory);
        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        assert!(journal.segment_id().is_none());
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            JournalHeader::EMPTY.encode()
        );
    }

    #[test]
    fn headerless_journal_is_rejected_without_mutation() {
        let bytes = b"PGMPheaderless".to_vec();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("active.parts"), &bytes).unwrap();
        let owner = owner(&directory);
        assert!(matches!(
            Journal::open(&owner, JournalConfig::default()),
            Err(JournalError::TornHeader { .. })
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            bytes
        );
    }

    #[test]
    fn fabricated_v2_journal_identities_are_rejected_without_mutation() {
        let canonical = JournalHeader::EMPTY.encode();
        let mut magic_v2 = canonical;
        magic_v2[..8].copy_from_slice(b"PGKJNL2\0");
        let magic_v2_crc = crc32c(&magic_v2[..32]);
        magic_v2[32..].copy_from_slice(&magic_v2_crc.to_le_bytes());

        let mut version_v2 = canonical;
        version_v2[8..12].copy_from_slice(&2_u32.to_le_bytes());
        let version_v2_crc = crc32c(&version_v2[..32]);
        version_v2[32..].copy_from_slice(&version_v2_crc.to_le_bytes());

        for bytes in [magic_v2, version_v2] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("active.parts");
            std::fs::write(&path, bytes).unwrap();
            let before = std::fs::read(&path).unwrap();
            let owner = owner(&directory);

            assert!(matches!(
                Journal::open(&owner, JournalConfig::default()),
                Err(JournalError::UnsupportedJournalFormat)
            ));
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "unsupported pre-release journal identities must not trigger fallback or migration"
            );
        }
    }

    #[test]
    fn bad_header_checksum_is_fatal_and_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let owner = owner(&directory);
        let journal = Journal::open(&owner, JournalConfig::default()).unwrap();
        drop(journal);
        let file = OpenOptions::new()
            .write(true)
            .open(directory.path().join("active.parts"))
            .unwrap();
        file.write_all_at(&[0xAA], 16).unwrap();
        let before = std::fs::read(directory.path().join("active.parts")).unwrap();
        assert!(matches!(
            Journal::open(&owner, JournalConfig::default()),
            Err(JournalError::InvalidHeader(
                JournalHeaderError::BadChecksum { .. }
            ))
        ));
        assert_eq!(
            std::fs::read(directory.path().join("active.parts")).unwrap(),
            before
        );
    }

    #[test]
    fn reset_final_sync_failure_reopens_as_logically_empty() {
        const INJECTED_EIO: i32 = 5;
        let directory = tempfile::tempdir().expect("tempdir");
        let owner = owner(&directory);
        let part = sample_part();
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open");
        let stale = journal.append(id(1_000), &part).expect("append");
        let faults = arm_journal_faults([(JournalFaultPoint::ResetFinalSync, INJECTED_EIO)]);

        let err = journal.reset().expect_err("sync failure is reported");
        assert_eq!(injected_operation_raw_os_error(&err), Some(INJECTED_EIO));
        assert!(journal.is_poisoned());
        assert!(matches!(
            journal.read_part(stale),
            Err(JournalError::Poisoned)
        ));
        faults.assert_consumed();
        drop(journal);

        let reopened = Journal::open(&owner, JournalConfig::default()).expect("reopen");
        assert!(reopened.is_empty());
        assert_eq!(reopened.len(), JOURNAL_HEADER_LEN);
    }
}
