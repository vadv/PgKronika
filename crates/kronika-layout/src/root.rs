use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::File;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use rustix::fs::RenameFlags;
use rustix::fs::{AtFlags, Dir, FileType, FlockOperation, Mode, OFlags};

use crate::{LayoutError, LimitKind, OwnerKind, SegmentAddress, SegmentId, UtcDay};

/// Root-level active segment journal.
pub const ACTIVE_JOURNAL_NAME: &str = "active.parts";
/// Permanent process-ownership lock for the collector.
pub const WRITER_OWNER_LOCK_NAME: &str = ".pgkronika-writer.owner.lock";
/// Permanent process-ownership lock for overview publication and GC.
pub const OVERVIEW_OWNER_LOCK_NAME: &str = ".pgkronika-overview.owner.lock";
/// Opaque directory holding exact quarantined objects.
///
/// Normal tree scans never traverse it. Forensic tools may use the explicit,
/// bounded [`DataRoot::scan_quarantine`] API.
pub const QUARANTINE_DIRECTORY_NAME: &str = ".pgkronika-quarantine-v1";

const HARD_MAX_VISITED_ENTRIES: usize = 4_000_000;
const HARD_MAX_ENTRIES_PER_DAY: usize = 1_000_000;
const HARD_MAX_SEGMENTS: usize = 2_000_000;
const HARD_MAX_METADATA_BYTES: usize = 128 * 1024 * 1024;
const ENTRY_METADATA_BYTES: usize = 128;
const SCAN_RACE_ATTEMPTS: usize = 4;
const WRITER_LOCK_HANDOFF_TIMEOUT: Duration = Duration::from_millis(100);
const ROOT_GENERATION_PREFIX: &str = ".pgkronika-generation-v1.";
const ROOT_GENERATION_SUFFIX: &str = ".parts";
const ROOT_EVIDENCE_PREFIX: &str = ".pgkronika-evidence-v1.";
const ROOT_EVIDENCE_SUFFIX: &str = ".pending";
const ROOT_SLOT_HEX_LEN: usize = 2;
const ROOT_NONCE_HEX_LEN: usize = 16;
const MAX_ROOT_COLLISION_SLOTS: u8 = 64;
const MAX_QUARANTINE_COLLISION_SLOTS: u8 = 64;
const DIRECTORY_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::XUSR)
    .union(Mode::RGRP)
    .union(Mode::XGRP);
const DATA_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::RGRP);
const LOCK_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const QUARANTINE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);

static NEXT_ROOT_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PgmPublishFaultPoint {
    FileSync,
    Link,
    LinkedDirectorySync,
    TemporaryUnlink,
    UnlinkedDirectorySync,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuarantineFaultPoint {
    DirectorySync,
    Rename,
    SourceDirectorySync,
    QuarantineDirectorySync,
    #[cfg(target_os = "linux")]
    Exchange,
}

#[cfg(test)]
std::thread_local! {
    static PGM_PUBLISH_FAULT: Cell<Option<(PgmPublishFaultPoint, i32)>> =
        const { Cell::new(None) };
    static ENSURE_DIRECTORY_SYNC_FAULT: Cell<Option<i32>> = const { Cell::new(None) };
    static OWNER_LOCK_SYNC_FAULT: Cell<Option<i32>> = const { Cell::new(None) };
    static QUARANTINE_FAULT: Cell<Option<(QuarantineFaultPoint, i32)>> =
        const { Cell::new(None) };
}

#[cfg(test)]
struct PgmPublishFaultGuard;

#[cfg(test)]
struct EnsureDirectorySyncFaultGuard;

#[cfg(test)]
struct OwnerLockSyncFaultGuard;

#[cfg(test)]
struct QuarantineFaultGuard;

#[cfg(test)]
impl Drop for PgmPublishFaultGuard {
    fn drop(&mut self) {
        PGM_PUBLISH_FAULT.with(|fault| fault.set(None));
    }
}

#[cfg(test)]
impl EnsureDirectorySyncFaultGuard {
    fn assert_consumed(self) {
        ENSURE_DIRECTORY_SYNC_FAULT.with(|fault| {
            assert!(
                fault.get().is_none(),
                "ensure-directory sync fault was not exercised"
            );
        });
        drop(self);
    }
}

#[cfg(test)]
impl Drop for EnsureDirectorySyncFaultGuard {
    fn drop(&mut self) {
        ENSURE_DIRECTORY_SYNC_FAULT.with(|fault| fault.set(None));
    }
}

#[cfg(test)]
impl OwnerLockSyncFaultGuard {
    fn assert_consumed(self) {
        OWNER_LOCK_SYNC_FAULT.with(|fault| {
            assert!(
                fault.get().is_none(),
                "owner-lock sync fault was not exercised"
            );
        });
        drop(self);
    }
}

#[cfg(test)]
impl Drop for OwnerLockSyncFaultGuard {
    fn drop(&mut self) {
        OWNER_LOCK_SYNC_FAULT.with(|fault| fault.set(None));
    }
}

#[cfg(test)]
impl QuarantineFaultGuard {
    fn assert_consumed(self) {
        QUARANTINE_FAULT.with(|fault| {
            assert!(fault.get().is_none(), "quarantine fault was not exercised");
        });
        drop(self);
    }
}

#[cfg(test)]
impl Drop for QuarantineFaultGuard {
    fn drop(&mut self) {
        QUARANTINE_FAULT.with(|fault| fault.set(None));
    }
}

#[cfg(test)]
fn arm_pgm_publish_fault(point: PgmPublishFaultPoint, raw_os_error: i32) -> PgmPublishFaultGuard {
    PGM_PUBLISH_FAULT.with(|fault| {
        assert!(fault.replace(Some((point, raw_os_error))).is_none());
    });
    PgmPublishFaultGuard
}

#[cfg(test)]
fn arm_ensure_directory_sync_fault(raw_os_error: i32) -> EnsureDirectorySyncFaultGuard {
    ENSURE_DIRECTORY_SYNC_FAULT.with(|fault| {
        assert!(fault.replace(Some(raw_os_error)).is_none());
    });
    EnsureDirectorySyncFaultGuard
}

#[cfg(test)]
fn arm_owner_lock_sync_fault(raw_os_error: i32) -> OwnerLockSyncFaultGuard {
    OWNER_LOCK_SYNC_FAULT.with(|fault| {
        assert!(fault.replace(Some(raw_os_error)).is_none());
    });
    OwnerLockSyncFaultGuard
}

#[cfg(test)]
fn arm_quarantine_fault(point: QuarantineFaultPoint, raw_os_error: i32) -> QuarantineFaultGuard {
    QUARANTINE_FAULT.with(|fault| {
        assert!(fault.replace(Some((point, raw_os_error))).is_none());
    });
    QuarantineFaultGuard
}

#[cfg(test)]
fn inject_pgm_publish_fault(point: PgmPublishFaultPoint) -> io::Result<()> {
    PGM_PUBLISH_FAULT.with(|fault| match fault.get() {
        Some((armed, raw_os_error)) if armed == point => {
            fault.set(None);
            Err(io::Error::from_raw_os_error(raw_os_error))
        }
        _ => Ok(()),
    })
}

#[cfg(test)]
fn inject_ensure_directory_sync_fault() -> io::Result<()> {
    ENSURE_DIRECTORY_SYNC_FAULT.with(|fault| {
        fault.take().map_or(Ok(()), |raw_os_error| {
            Err(io::Error::from_raw_os_error(raw_os_error))
        })
    })
}

#[cfg(test)]
fn inject_owner_lock_sync_fault() -> io::Result<()> {
    OWNER_LOCK_SYNC_FAULT.with(|fault| {
        fault.take().map_or(Ok(()), |raw_os_error| {
            Err(io::Error::from_raw_os_error(raw_os_error))
        })
    })
}

#[cfg(test)]
fn inject_quarantine_fault(point: QuarantineFaultPoint) -> io::Result<()> {
    QUARANTINE_FAULT.with(|fault| match fault.get() {
        Some((armed, raw_os_error)) if armed == point => {
            fault.set(None);
            Err(io::Error::from_raw_os_error(raw_os_error))
        }
        _ => Ok(()),
    })
}

macro_rules! pgm_publish_failpoint {
    ($point:ident) => {
        #[cfg(test)]
        inject_pgm_publish_fault(PgmPublishFaultPoint::$point)?;
    };
}

macro_rules! ensure_directory_sync_failpoint {
    () => {
        #[cfg(test)]
        inject_ensure_directory_sync_fault()?;
    };
}

macro_rules! owner_lock_sync_failpoint {
    () => {
        #[cfg(test)]
        inject_owner_lock_sync_fault()?;
    };
}

macro_rules! quarantine_failpoint {
    ($point:ident) => {
        #[cfg(test)]
        inject_quarantine_fault(QuarantineFaultPoint::$point)?;
    };
}

fn quarantine_failure(stage: QuarantineFailureStage, error: &io::Error) -> QuarantineFailure {
    QuarantineFailure {
        stage,
        error_kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

/// Non-zero hard-capped resource limits for one strict tree traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "`LayoutLimits::max_*` makes each public bound explicit at call sites"
)]
pub struct LayoutLimits {
    /// Maximum number of entries visited across the entire tree.
    pub max_visited_entries: usize,
    /// Maximum number of entries visited in a single day directory.
    pub max_entries_per_day: usize,
    /// Maximum number of finished PGM segments returned.
    pub max_segments: usize,
    /// Maximum accounted bytes for names and result metadata.
    pub max_metadata_bytes: usize,
}

impl LayoutLimits {
    /// Validates all limits against their non-zero hard ranges.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError::InvalidLimits`] for a zero or excessive value.
    pub fn validate(self) -> Result<Self, LayoutError> {
        validate_limit(
            LimitKind::VisitedEntries,
            self.max_visited_entries,
            HARD_MAX_VISITED_ENTRIES,
        )?;
        validate_limit(
            LimitKind::EntriesPerDay,
            self.max_entries_per_day,
            HARD_MAX_ENTRIES_PER_DAY,
        )?;
        validate_limit(LimitKind::Segments, self.max_segments, HARD_MAX_SEGMENTS)?;
        validate_limit(
            LimitKind::MetadataBytes,
            self.max_metadata_bytes,
            HARD_MAX_METADATA_BYTES,
        )?;
        Ok(self)
    }
}

impl Default for LayoutLimits {
    fn default() -> Self {
        Self {
            max_visited_entries: 1_000_000,
            max_entries_per_day: 10_000,
            max_segments: 500_000,
            max_metadata_bytes: HARD_MAX_METADATA_BYTES,
        }
    }
}

/// Final data kind addressed inside a UTC day directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Immutable source segment.
    Pgm,
    /// Replaceable derived overview sidecar.
    Ovf,
}

/// Recognized crash-remnant or in-progress temporary file kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporaryKind {
    /// Writer PGM publication.
    Pgm,
    /// Overview sidecar publication.
    Ovf,
    /// Overview writeability probe.
    OverviewProbe,
}

/// A verified temporary object returned by strict discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporaryObject {
    /// Segment address encoded by the file and parent day.
    pub address: SegmentAddress,
    /// Publisher that owns cleanup of this object.
    pub kind: TemporaryKind,
    /// No-follow identity observed by discovery.
    pub identity: FileIdentity,
    file_name: String,
}

impl TemporaryObject {
    /// Returns the verified leaf file name.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

/// The verified parent scope of an entry that was not interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryScope {
    /// Direct child of the data root.
    Root,
    /// Direct child of a valid UTC year directory.
    Year {
        /// Four-digit UTC year.
        year: u16,
    },
    /// Direct child of a valid UTC month directory.
    Month {
        /// Four-digit UTC year.
        year: u16,
        /// UTC month, `1..=12`.
        month: u8,
    },
    /// Direct child of a valid UTC day directory.
    Day(UtcDay),
    /// Opaque child of the quarantine directory.
    Quarantine,
}

/// Filesystem type observed without following a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryFileType {
    /// Regular file.
    RegularFile,
    /// Directory.
    Directory,
    /// Symbolic link. Its target was not inspected.
    Symlink,
    /// FIFO, socket, device, or another unsupported object.
    Other,
}

/// Bounded, payload-free identity for one named tree entry.
///
/// The raw name is deliberately not exposed. `name_hash` is a stable,
/// non-cryptographic correlation token, not a content digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathIdentity {
    /// Verified parent scope.
    pub scope: EntryScope,
    /// Stable hash of the raw leaf-name bytes.
    pub name_hash: u64,
    /// Raw leaf-name length in bytes.
    pub name_len: u16,
    /// No-follow filesystem type.
    pub file_type: EntryFileType,
    /// No-follow filesystem identity and metadata.
    pub file: FileIdentity,
}

/// Why a tree entry was excluded from the supported readable grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForeignEntryReason {
    /// The name is outside the grammar for its verified parent.
    UnsupportedName,
    /// The name is reserved, but its filesystem type is unsupported.
    UnsupportedType,
    /// A symbolic link was encountered and not followed.
    SymbolicLink,
    /// A root-level PGM or OVF belongs to the unsupported flat layout.
    UnsupportedFlatArtifact,
    /// Raw name bytes are not ASCII and were not interpreted.
    NonAsciiName,
    /// A segment-like leaf points to a different UTC bucket.
    MisbucketedSegment,
}

/// Typed diagnostic for one locally excluded tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryDiagnostic {
    /// Bounded path/object identity.
    pub path: PathIdentity,
    /// Reason the entry was not interpreted.
    pub reason: ForeignEntryReason,
}

#[derive(Clone, PartialEq, Eq)]
struct EntryPath {
    parent: EntryParent,
    name: CString,
}

impl fmt::Debug for EntryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntryPath")
            .field("scope", &self.parent.scope())
            .field("name_hash", &hash_name(self.name.as_bytes()))
            .field("name_len", &self.name.as_bytes().len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryParent {
    Root,
    Year(u16),
    Month { year: u16, month: u8 },
    Day(UtcDay),
}

impl EntryParent {
    const fn scope(self) -> EntryScope {
        match self {
            Self::Root => EntryScope::Root,
            Self::Year(year) => EntryScope::Year { year },
            Self::Month { year, month } => EntryScope::Month { year, month },
            Self::Day(day) => EntryScope::Day(day),
        }
    }
}

/// Opaque capability for one unsupported entry found by a tolerant scan.
///
/// Its raw name remains private so only [`WriterOwner`] can mutate it through
/// descriptor-relative, identity-checked operations.
#[derive(Clone, PartialEq, Eq)]
pub struct ForeignEntry {
    diagnostic: EntryDiagnostic,
    path: EntryPath,
}

impl ForeignEntry {
    /// Returns the bounded diagnostic without exposing the raw leaf name.
    #[must_use]
    pub const fn diagnostic(&self) -> EntryDiagnostic {
        self.diagnostic
    }
}

impl fmt::Debug for ForeignEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForeignEntry")
            .field("diagnostic", &self.diagnostic)
            .finish_non_exhaustive()
    }
}

/// Recognized, bounded root-level recovery object kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PendingRootKind {
    /// Exact evidence retained across an interrupted recovery invocation.
    Evidence,
    /// A fresh alternate journal generation.
    JournalGeneration,
}

/// Opaque capability for a recognized root-level recovery object.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingRootEntry {
    kind: PendingRootKind,
    identity: PathIdentity,
    name: CString,
}

impl PendingRootEntry {
    /// Returns the grammar role encoded by the bounded root name.
    #[must_use]
    pub const fn kind(&self) -> PendingRootKind {
        self.kind
    }

    /// Returns the bounded path identity.
    #[must_use]
    pub const fn identity(&self) -> PathIdentity {
        self.identity
    }
}

impl fmt::Debug for PendingRootEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingRootEntry")
            .field("kind", &self.kind)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// State of the opaque root quarantine directory during a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineDirectoryState {
    /// No quarantine directory is currently present.
    Absent,
    /// A real, non-followed directory was found and deliberately not traversed.
    Present(PathIdentity),
    /// The reserved name exists with an unsupported type.
    Unavailable(PathIdentity),
}

/// Reason code embedded in a quarantine operation and file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum QuarantineReason {
    /// An entry outside the supported owned-store grammar.
    ForeignEntry = 1,
    /// A PGM failed startup catalog or format validation.
    InvalidPgm = 2,
    /// The canonical active journal was corrupt.
    CorruptActiveJournal = 3,
    /// A recognized root evidence object was processed.
    PendingEvidence = 4,
    /// A stale publication temporary was preserved.
    StaleTemporary = 5,
    /// A recovery output failed final canonical validation.
    InvalidRecoveryOutput = 6,
}

impl QuarantineReason {
    /// Stable machine-readable reason code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ForeignEntry => "foreign_entry",
            Self::InvalidPgm => "invalid_pgm",
            Self::CorruptActiveJournal => "corrupt_active_journal",
            Self::PendingEvidence => "pending_evidence",
            Self::StaleTemporary => "stale_temporary",
            Self::InvalidRecoveryOutput => "invalid_recovery_output",
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::ForeignEntry),
            2 => Some(Self::InvalidPgm),
            3 => Some(Self::CorruptActiveJournal),
            4 => Some(Self::PendingEvidence),
            5 => Some(Self::StaleTemporary),
            6 => Some(Self::InvalidRecoveryOutput),
            _ => None,
        }
    }
}

/// One canonical object found by a bounded forensic quarantine scan.
#[derive(Clone, PartialEq, Eq)]
pub struct QuarantineEntry {
    name: CString,
    canonical_name: String,
    reason: QuarantineReason,
    identity: PathIdentity,
}

impl QuarantineEntry {
    /// Opaque canonical leaf name that can be passed back to layout.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.canonical_name
    }

    /// Reason encoded when the object entered quarantine.
    #[must_use]
    pub const fn reason(&self) -> QuarantineReason {
        self.reason
    }

    /// No-follow filesystem identity observed by the scan.
    #[must_use]
    pub const fn identity(&self) -> PathIdentity {
        self.identity
    }
}

impl fmt::Debug for QuarantineEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuarantineEntry")
            .field("reason", &self.reason)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// Filesystem stage at which a local quarantine/rotation operation degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineFailureStage {
    /// The quarantine directory could not be created or safely opened.
    QuarantineDirectory,
    /// The created/existing quarantine root entry could not be synchronized.
    QuarantineDirectoryEntrySync,
    /// The source object disappeared before the operation.
    SourceMissing,
    /// The input entry changed before the operation.
    SourceChanged,
    /// All bounded collision slots already existed.
    CollisionSlotsExhausted,
    /// An atomic rename failed.
    Rename,
    /// The source parent could not be synchronized after a rename.
    SourceDirectorySync,
    /// The quarantine directory could not be synchronized after a rename.
    QuarantineDirectorySync,
    /// The fresh journal file could not be synchronized before activation.
    FreshJournalSync,
    /// The atomic exchange activation failed.
    JournalExchange,
    /// The root directory could not be synchronized after activation.
    RootDirectorySync,
    /// A fallback could not promote the fresh generation to `active.parts`.
    JournalPromotion,
    /// Post-rename identity verification failed.
    IdentityVerification,
}

/// Bounded typed filesystem failure without path names or payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarantineFailure {
    /// Operation stage.
    pub stage: QuarantineFailureStage,
    /// Portable I/O category.
    pub error_kind: io::ErrorKind,
    /// Platform error number, when available.
    pub raw_os_error: Option<i32>,
}

impl QuarantineFailure {
    const fn local(stage: QuarantineFailureStage, error_kind: io::ErrorKind) -> Self {
        Self {
            stage,
            error_kind,
            raw_os_error: None,
        }
    }
}

/// Result of a local quarantine attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineStatus {
    /// The exact named object was atomically moved into the opaque directory.
    Quarantined {
        /// Bounded identity of the destination name and retained object.
        destination: PathIdentity,
    },
    /// The move completed, but a durability or verification step failed.
    QuarantinedDegraded {
        /// Bounded identity of the destination name and retained object.
        destination: PathIdentity,
        /// Local degraded operation.
        failure: QuarantineFailure,
    },
    /// The scanned object disappeared before mutation.
    Missing,
    /// The scanned name no longer identifies the same object.
    Changed {
        /// Newly observed object, when one was present.
        observed: Option<PathIdentity>,
    },
    /// Evidence remained at its source name after a local failure.
    Retained {
        /// Local degraded operation.
        failure: QuarantineFailure,
    },
}

/// Typed, payload-free report for one quarantine attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarantineOutcome {
    /// Why the entry was quarantined.
    pub reason: QuarantineReason,
    /// Identity observed by discovery or rotation.
    pub source: PathIdentity,
    /// Final local disposition.
    pub status: QuarantineStatus,
}

/// Discovered final artifacts for one source segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentArtifacts {
    /// Logical and physical segment address.
    pub address: SegmentAddress,
    /// Filesystem identity observed for the immutable PGM name.
    pub pgm_identity: FileIdentity,
    /// Current PGM file size.
    pub pgm_bytes: u64,
    /// Current sibling OVF file size, when present.
    pub ovf_bytes: Option<u64>,
}

/// Bytes reclaimed by removing one sealed segment during rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegmentRemoval {
    /// Bytes freed by unlinking the immutable PGM; zero if it was already gone.
    pub pgm_bytes: u64,
    /// Bytes freed by unlinking the sibling OVF, when one was present.
    pub ovf_bytes: Option<u64>,
}

impl SegmentRemoval {
    /// Total bytes reclaimed across the PGM and its sibling OVF.
    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        match self.ovf_bytes {
            Some(ovf) => self.pgm_bytes.saturating_add(ovf),
            None => self.pgm_bytes,
        }
    }
}

/// Complete result of one successful strict, bounded traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutSnapshot {
    /// Every valid UTC day directory visited, including empty days.
    pub days: Vec<UtcDay>,
    /// Finished PGM segments in numeric `SegmentId` order.
    pub segments: Vec<SegmentArtifacts>,
    /// OVF files without a sibling PGM, in numeric order.
    pub orphan_overviews: Vec<SegmentAddress>,
    /// Recognized temporary files.
    pub temporaries: Vec<TemporaryObject>,
    /// Unsupported entries excluded locally without aborting valid discovery.
    pub foreign_entries: Vec<ForeignEntry>,
    /// Recognized bounded root recovery objects, never interpreted by scanning.
    pub pending_root_entries: Vec<PendingRootEntry>,
    /// Opaque quarantine-directory state; its contents are never traversed.
    pub quarantine_directory: QuarantineDirectoryState,
    /// Number of filesystem entries visited.
    pub visited_entries: usize,
    /// Accounted metadata bytes.
    pub metadata_bytes: usize,
}

/// Byte occupancy of the filesystem that backs one data root.
///
/// Both figures come from a single `statvfs` of the root descriptor and count
/// every user of the partition, not only `PgKronika` data. `used_bytes` is the
/// classic "how full is the filesystem" figure (`total − free`), the basis for
/// the `auto:P` retention target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemUsage {
    /// Total addressable bytes of the partition.
    pub total_bytes: u64,
    /// Bytes occupied by all data on the partition.
    pub used_bytes: u64,
}

/// Open, stable descriptor for one `PgKronika` data root.
#[derive(Debug, Clone)]
pub struct DataRoot {
    directory: Arc<File>,
    diagnostic_path: Arc<Path>,
}

impl DataRoot {
    /// Opens an existing data root without following a symbolic link.
    ///
    /// This only opens the root itself. Call [`scan`](Self::scan) before using
    /// its contents.
    ///
    /// # Errors
    ///
    /// Returns a structural or filesystem error when the root cannot be opened
    /// as a real directory.
    pub fn open(path: &Path) -> Result<Self, LayoutError> {
        let directory = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                LayoutError::SymlinkNotAllowed {
                    name: path.display().to_string(),
                }
            } else {
                LayoutError::Io(errno_to_io(error))
            }
        })?;
        Ok(Self {
            directory: Arc::new(directory),
            diagnostic_path: Arc::from(path),
        })
    }

    /// Returns the configured root path for diagnostics only.
    ///
    /// Filesystem access should use this type's descriptor-relative methods.
    #[must_use]
    pub fn diagnostic_path(&self) -> &Path {
        &self.diagnostic_path
    }

    /// Builds a path for logs, error reports, and test assertions only.
    ///
    /// The returned path is not a capability and production I/O must use
    /// [`open_pgm`](Self::open_pgm), [`open_ovf`](Self::open_ovf), or an owner
    /// token.
    #[must_use]
    pub fn diagnostic_file_path(&self, address: SegmentAddress, kind: FileKind) -> PathBuf {
        let name = match kind {
            FileKind::Pgm => address.pgm_name(),
            FileKind::Ovf => address.ovf_name(),
        };
        self.diagnostic_path
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component())
            .join(name)
    }

    /// Performs a tolerant, closed-grammar, three-level, bounded traversal.
    ///
    /// Entries outside the owned-store grammar are classified in
    /// [`LayoutSnapshot::foreign_entries`] and never followed or traversed.
    /// Valid entries remain in the returned inventory. Resource exhaustion and
    /// failures while reading the verified tree still fail the whole scan.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError`] for exhausted limits or filesystem failures
    /// that prevent bounded traversal of the verified tree.
    pub fn scan(&self, limits: LayoutLimits) -> Result<LayoutSnapshot, LayoutError> {
        let limits = limits.validate()?;
        for attempt in 0..SCAN_RACE_ATTEMPTS {
            // Fresh per attempt: a retried scan revisits the same entries, and
            // carrying the count over would fail valid trees near the limit.
            let mut visited_entries = 0_usize;
            match self.scan_once(limits, &mut visited_entries) {
                Err(LayoutError::Io(error))
                    if error.kind() == io::ErrorKind::NotFound
                        && attempt + 1 < SCAN_RACE_ATTEMPTS =>
                {
                    std::thread::yield_now();
                }
                result => return result,
            }
        }
        unreachable!("the bounded scan loop always returns on its final attempt")
    }

    fn scan_once(
        &self,
        limits: LayoutLimits,
        visited_entries: &mut usize,
    ) -> Result<LayoutSnapshot, LayoutError> {
        let mut state = ScanState::new(limits, visited_entries);
        let mut root_entries = Dir::read_from(&*self.directory).map_err(errno_to_layout)?;
        for entry in &mut root_entries {
            let entry = entry.map_err(errno_to_layout)?;
            let name = entry.file_name();
            let name_bytes = name.to_bytes();
            if is_dot(name_bytes) {
                continue;
            }
            state.account(name_bytes.len())?;
            let stat = stat_no_follow_name(&self.directory, name)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let Some(name_string) = ascii_name(name_bytes) else {
                state.record_foreign(
                    EntryParent::Root,
                    name,
                    &stat,
                    ForeignEntryReason::NonAsciiName,
                )?;
                continue;
            };
            if name_string == QUARANTINE_DIRECTORY_NAME {
                let identity = path_identity(EntryParent::Root, name, &stat);
                if file_type == FileType::Directory {
                    state.quarantine_directory = QuarantineDirectoryState::Present(identity);
                } else {
                    state.quarantine_directory = QuarantineDirectoryState::Unavailable(identity);
                    state.record_foreign(
                        EntryParent::Root,
                        name,
                        &stat,
                        foreign_type_reason(file_type),
                    )?;
                }
                continue;
            }
            if let Some(kind) = parse_pending_root_name(name_string) {
                if file_type == FileType::RegularFile {
                    state.record_pending_root(kind, name, &stat)?;
                } else {
                    state.record_foreign(
                        EntryParent::Root,
                        name,
                        &stat,
                        foreign_type_reason(file_type),
                    )?;
                }
                continue;
            }
            if is_control_name(name_string) {
                if file_type != FileType::RegularFile {
                    state.record_foreign(
                        EntryParent::Root,
                        name,
                        &stat,
                        foreign_type_reason(file_type),
                    )?;
                }
                continue;
            }
            let root_extension = Path::new(name_string).extension();
            if root_extension.is_some_and(|extension| extension == "pgm" || extension == "ovf") {
                state.record_foreign(
                    EntryParent::Root,
                    name,
                    &stat,
                    if file_type == FileType::RegularFile {
                        ForeignEntryReason::UnsupportedFlatArtifact
                    } else {
                        foreign_type_reason(file_type)
                    },
                )?;
                continue;
            }
            let Some(year) = parse_year(name_string) else {
                state.record_foreign(
                    EntryParent::Root,
                    name,
                    &stat,
                    if file_type == FileType::Symlink {
                        ForeignEntryReason::SymbolicLink
                    } else {
                        ForeignEntryReason::UnsupportedName
                    },
                )?;
                continue;
            };
            if file_type != FileType::Directory {
                state.record_foreign(
                    EntryParent::Root,
                    name,
                    &stat,
                    foreign_type_reason(file_type),
                )?;
                continue;
            }
            let year_directory = open_directory_at(&self.directory, name_string)?;
            Self::scan_year(&year_directory, year, &mut state)?;
        }
        Ok(state.finish())
    }

    fn scan_year(
        year_directory: &File,
        year: u16,
        state: &mut ScanState<'_>,
    ) -> Result<(), LayoutError> {
        let mut entries = Dir::read_from(year_directory).map_err(errno_to_layout)?;
        for entry in &mut entries {
            let entry = entry.map_err(errno_to_layout)?;
            let name = entry.file_name();
            let name_bytes = name.to_bytes();
            if is_dot(name_bytes) {
                continue;
            }
            state.account(name_bytes.len())?;
            let stat = stat_no_follow_name(year_directory, name)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let parent = EntryParent::Year(year);
            let Some(name_string) = ascii_name(name_bytes) else {
                state.record_foreign(parent, name, &stat, ForeignEntryReason::NonAsciiName)?;
                continue;
            };
            let Some(month) = parse_month(name_string) else {
                state.record_foreign(
                    parent,
                    name,
                    &stat,
                    if file_type == FileType::Symlink {
                        ForeignEntryReason::SymbolicLink
                    } else {
                        ForeignEntryReason::UnsupportedName
                    },
                )?;
                continue;
            };
            if file_type != FileType::Directory {
                state.record_foreign(parent, name, &stat, foreign_type_reason(file_type))?;
                continue;
            }
            let month_directory = open_directory_at(year_directory, name_string)?;
            Self::scan_month(&month_directory, year, month, state)?;
        }
        Ok(())
    }

    fn scan_month(
        month_directory: &File,
        year: u16,
        month: u8,
        state: &mut ScanState<'_>,
    ) -> Result<(), LayoutError> {
        let mut entries = Dir::read_from(month_directory).map_err(errno_to_layout)?;
        for entry in &mut entries {
            let entry = entry.map_err(errno_to_layout)?;
            let name = entry.file_name();
            let name_bytes = name.to_bytes();
            if is_dot(name_bytes) {
                continue;
            }
            state.account(name_bytes.len())?;
            let stat = stat_no_follow_name(month_directory, name)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let parent = EntryParent::Month { year, month };
            let Some(name_string) = ascii_name(name_bytes) else {
                state.record_foreign(parent, name, &stat, ForeignEntryReason::NonAsciiName)?;
                continue;
            };
            let Some(day_number) = parse_day(year, month, name_string) else {
                state.record_foreign(
                    parent,
                    name,
                    &stat,
                    if file_type == FileType::Symlink {
                        ForeignEntryReason::SymbolicLink
                    } else {
                        ForeignEntryReason::UnsupportedName
                    },
                )?;
                continue;
            };
            if file_type != FileType::Directory {
                state.record_foreign(parent, name, &stat, foreign_type_reason(file_type))?;
                continue;
            }
            let day_directory = open_directory_at(month_directory, name_string)?;
            let day = UtcDay::new(year, month, day_number)?;
            state.account_metadata(size_of::<UtcDay>())?;
            state.days.push(day);
            Self::scan_day(&day_directory, day, state)?;
        }
        Ok(())
    }

    fn scan_day(
        day_directory: &File,
        day: UtcDay,
        state: &mut ScanState<'_>,
    ) -> Result<(), LayoutError> {
        let mut entries = Dir::read_from(day_directory).map_err(errno_to_layout)?;
        let mut day_entries = 0_usize;
        let mut finals: BTreeMap<SegmentId, DayArtifacts> = BTreeMap::new();
        for entry in &mut entries {
            let entry = entry.map_err(errno_to_layout)?;
            let name = entry.file_name();
            let name_bytes = name.to_bytes();
            if is_dot(name_bytes) {
                continue;
            }
            day_entries =
                day_entries
                    .checked_add(1)
                    .ok_or(LayoutError::TraversalLimitExceeded {
                        kind: LimitKind::EntriesPerDay,
                        limit: state.limits.max_entries_per_day,
                    })?;
            if day_entries > state.limits.max_entries_per_day {
                return Err(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::EntriesPerDay,
                    limit: state.limits.max_entries_per_day,
                });
            }
            state.account(name_bytes.len())?;
            let stat = stat_no_follow_name(day_directory, name)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let parent = EntryParent::Day(day);
            let Some(name_string) = ascii_name(name_bytes) else {
                state.record_foreign(parent, name, &stat, ForeignEntryReason::NonAsciiName)?;
                continue;
            };
            if file_type != FileType::RegularFile {
                state.record_foreign(parent, name, &stat, foreign_type_reason(file_type))?;
                continue;
            }
            let parsed = match parse_leaf(name_string, day) {
                Ok(parsed) => parsed,
                Err(error) => {
                    state.record_foreign(
                        parent,
                        name,
                        &stat,
                        if matches!(error, LayoutError::MisbucketedSegment { .. }) {
                            ForeignEntryReason::MisbucketedSegment
                        } else {
                            ForeignEntryReason::UnsupportedName
                        },
                    )?;
                    continue;
                }
            };
            let identity = FileIdentity::from_stat(&stat);
            let bytes = identity.len;
            match parsed {
                ParsedLeaf::Pgm(address) => {
                    finals.entry(address.id).or_default().pgm = Some(identity);
                }
                ParsedLeaf::Ovf(address) => {
                    finals.entry(address.id).or_default().ovf = Some(bytes);
                }
                ParsedLeaf::Temporary(address, kind) => {
                    state.account_metadata(ENTRY_METADATA_BYTES)?;
                    state.temporaries.push(TemporaryObject {
                        address,
                        kind,
                        identity,
                        file_name: name_string.to_owned(),
                    });
                }
            }
        }

        for (id, artifacts) in finals {
            let address = SegmentAddress::in_day(id, day)?;
            if let Some(pgm_identity) = artifacts.pgm {
                state.account_segment()?;
                state.segments.push(SegmentArtifacts {
                    address,
                    pgm_identity,
                    pgm_bytes: pgm_identity.len,
                    ovf_bytes: artifacts.ovf,
                });
            } else if artifacts.ovf.is_some() {
                state.account_metadata(size_of::<SegmentAddress>())?;
                state.orphan_overviews.push(address);
            }
        }
        Ok(())
    }

    /// Opens a verified PGM relative to the root descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error if a calendar component or final file is missing,
    /// replaced by a symbolic link, or has the wrong type.
    pub fn open_pgm(&self, address: SegmentAddress) -> Result<File, LayoutError> {
        self.open_final(address, FileKind::Pgm)
    }

    /// Opens a verified OVF relative to the root descriptor when it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if a component is unsafe or the existing final object
    /// is not a regular file.
    pub fn open_ovf(&self, address: SegmentAddress) -> Result<Option<File>, LayoutError> {
        let day = self.open_day(address.day)?;
        let name = address.ovf_name();
        match open_regular_at(&day, &name, OFlags::RDONLY) {
            Ok(file) => Ok(Some(file)),
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Opens a temporary object returned by [`scan`](Self::scan).
    ///
    /// The verified leaf name and its typed day address are used directly;
    /// callers cannot substitute an arbitrary relative path.
    ///
    /// # Errors
    ///
    /// Returns an error if the object disappeared, changed type, or any
    /// calendar component became unsafe.
    pub fn open_temporary(&self, temporary: &TemporaryObject) -> Result<File, LayoutError> {
        let day = self.open_day(temporary.address.day)?;
        open_regular_at(&day, temporary.file_name(), OFlags::RDONLY)
    }

    /// Opens the active journal for reading when it exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the existing entry is unsafe or unreadable.
    pub fn open_active_journal(&self) -> Result<Option<File>, LayoutError> {
        match open_regular_at(&self.directory, ACTIVE_JOURNAL_NAME, OFlags::RDONLY) {
            Ok(file) => Ok(Some(file)),
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Lists canonical quarantine objects without following any entry.
    ///
    /// The regular owned-tree scan deliberately treats quarantine as opaque.
    /// This separate forensic pass recognizes only layout-generated `qv1`
    /// names, validates their embedded filesystem identity, and applies the
    /// caller's traversal and metadata limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the quarantine directory is unsafe or unreadable,
    /// or when a traversal or retained-metadata limit is exhausted.
    pub fn scan_quarantine(
        &self,
        limits: LayoutLimits,
    ) -> Result<Vec<QuarantineEntry>, LayoutError> {
        let limits = limits.validate()?;
        let directory = match open_directory_at(&self.directory, QUARANTINE_DIRECTORY_NAME) {
            Ok(directory) => directory,
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let mut entries = Dir::read_from(&directory).map_err(errno_to_layout)?;
        let mut visited = 0_usize;
        let mut metadata_bytes = 0_usize;
        let mut result = Vec::new();

        for entry in &mut entries {
            let entry = entry.map_err(errno_to_layout)?;
            let name = entry.file_name();
            let name_bytes = name.to_bytes();
            if is_dot(name_bytes) {
                continue;
            }
            visited = visited
                .checked_add(1)
                .ok_or(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::VisitedEntries,
                    limit: limits.max_visited_entries,
                })?;
            if visited > limits.max_visited_entries {
                return Err(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::VisitedEntries,
                    limit: limits.max_visited_entries,
                });
            }
            metadata_bytes = metadata_bytes
                .checked_add(name_bytes.len())
                .and_then(|bytes| bytes.checked_add(ENTRY_METADATA_BYTES))
                .ok_or(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::MetadataBytes,
                    limit: limits.max_metadata_bytes,
                })?;
            if metadata_bytes > limits.max_metadata_bytes {
                return Err(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::MetadataBytes,
                    limit: limits.max_metadata_bytes,
                });
            }

            let Some(name_string) = ascii_name(name_bytes) else {
                continue;
            };
            let Some(parsed) = parse_quarantine_name(name_string) else {
                continue;
            };
            let stat = stat_no_follow_name(&directory, name)?;
            let file = FileIdentity::from_stat(&stat);
            if file.device != parsed.device || file.inode != parsed.inode {
                continue;
            }
            metadata_bytes = metadata_bytes
                .checked_add(size_of::<QuarantineEntry>())
                .and_then(|bytes| bytes.checked_add(name_bytes.len()))
                .ok_or(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::MetadataBytes,
                    limit: limits.max_metadata_bytes,
                })?;
            if metadata_bytes > limits.max_metadata_bytes {
                return Err(LayoutError::TraversalLimitExceeded {
                    kind: LimitKind::MetadataBytes,
                    limit: limits.max_metadata_bytes,
                });
            }
            result
                .try_reserve(1)
                .map_err(|error| LayoutError::Io(io::Error::other(error)))?;
            result.push(QuarantineEntry {
                name: name.to_owned(),
                canonical_name: name_string.to_owned(),
                reason: parsed.reason,
                identity: path_identity_from_file(
                    EntryScope::Quarantine,
                    name,
                    entry_file_type(FileType::from_raw_mode(stat.st_mode)),
                    file,
                ),
            });
        }
        result.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
        Ok(result)
    }

    /// Opens and identity-checks a regular object returned by
    /// [`Self::scan_quarantine`].
    ///
    /// # Errors
    ///
    /// Returns an error when the object is not a regular file, disappeared, or
    /// changed since the scan.
    pub fn open_quarantine(&self, entry: &QuarantineEntry) -> Result<File, LayoutError> {
        if entry.identity.file_type != EntryFileType::RegularFile {
            return Err(LayoutError::UnexpectedLeafEntryType {
                name: "opaque quarantine object".to_owned(),
            });
        }
        let directory = open_directory_at(&self.directory, QUARANTINE_DIRECTORY_NAME)?;
        let file = open_regular_name_at(&directory, &entry.name, OFlags::RDONLY)?;
        if FileIdentity::from_file(&file)? != entry.identity.file {
            return Err(LayoutError::TemporaryChanged {
                name: "opaque quarantine object".to_owned(),
            });
        }
        Ok(file)
    }

    /// Opens and revalidates a recognized pending root recovery object.
    ///
    /// The raw name remains encapsulated by [`PendingRootEntry`]. The object
    /// must still be the same regular file observed by [`scan`](Self::scan).
    ///
    /// # Errors
    ///
    /// Returns an error when the entry disappeared, changed, or is unsafe.
    pub fn open_pending_root(&self, pending: &PendingRootEntry) -> Result<File, LayoutError> {
        let file = open_regular_name_at(&self.directory, &pending.name, OFlags::RDONLY)?;
        if FileIdentity::from_file(&file)? != pending.identity.file {
            return Err(LayoutError::TemporaryChanged {
                name: format!("opaque-root:{:016x}", pending.identity.name_hash),
            });
        }
        Ok(file)
    }

    /// Reads the byte occupancy of the partition backing this root.
    ///
    /// The measurement is a single `statvfs` of the already-open root
    /// descriptor, so no path is re-resolved and foreign data on a shared
    /// partition is included in [`FilesystemUsage::used_bytes`].
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the descriptor cannot be queried.
    pub fn filesystem_usage(&self) -> Result<FilesystemUsage, LayoutError> {
        let stat = rustix::fs::fstatvfs(&*self.directory).map_err(errno_to_layout)?;
        let block_bytes = stat.f_frsize;
        let total_bytes = stat.f_blocks.saturating_mul(block_bytes);
        let free_bytes = stat.f_bfree.saturating_mul(block_bytes);
        Ok(FilesystemUsage {
            total_bytes,
            used_bytes: total_bytes.saturating_sub(free_bytes),
        })
    }

    /// Establishes lifetime-exclusive writer ownership after two strict scans.
    ///
    /// # Errors
    ///
    /// Returns a structural error, I/O error, or
    /// [`LayoutError::OwnerContended`].
    pub fn acquire_writer(&self, limits: LayoutLimits) -> Result<WriterOwner, LayoutError> {
        let first = self.scan(limits)?;
        let root_lock = self.acquire_writer_root_lock()?;
        let mut startup_quarantine_outcomes = Vec::new();
        for foreign in &first.foreign_entries {
            if is_writer_bootstrap_control(foreign) {
                startup_quarantine_outcomes.push(quarantine_entry(
                    self,
                    &foreign.path,
                    foreign.diagnostic.path,
                    QuarantineReason::ForeignEntry,
                ));
            }
        }
        let lock = match self.acquire_writer_lock() {
            Ok(lock) => lock,
            Err(_error) if writer_lock_is_poisoned(self) => root_lock.try_clone()?,
            Err(error) => return Err(error),
        };
        let second = self.scan(limits)?;
        let mut owner = WriterOwner {
            root: self.clone(),
            owner_lock: lock,
            _root_lock: root_lock,
            startup_quarantine_outcomes,
        };
        for foreign in &second.foreign_entries {
            if !is_writer_lock_name(foreign) {
                let outcome = owner.quarantine_foreign(foreign);
                owner.startup_quarantine_outcomes.push(outcome);
            }
        }
        Ok(owner)
    }

    /// Establishes lifetime-exclusive overview ownership after two strict scans.
    ///
    /// # Errors
    ///
    /// Returns a structural error, I/O error, or
    /// [`LayoutError::OwnerContended`].
    pub fn acquire_overview(&self, limits: LayoutLimits) -> Result<OverviewOwner, LayoutError> {
        self.scan(limits)?;
        let lock = self.acquire_lock(OVERVIEW_OWNER_LOCK_NAME, OwnerKind::Overview)?;
        self.scan(limits)?;
        Ok(OverviewOwner {
            root: self.clone(),
            _lock: lock,
        })
    }

    fn acquire_lock(&self, name: &str, owner: OwnerKind) -> Result<File, LayoutError> {
        let (lock, _created) =
            open_or_create_regular(&self.directory, name, OFlags::RDWR, LOCK_FILE_MODE)?;
        rustix::fs::fchmod(&lock, LOCK_FILE_MODE)
            .map_err(errno_to_io)
            .map_err(LayoutError::Io)?;
        lock.sync_all()?;
        // A previous creator may have initialized the inode and then failed
        // to synchronize its root entry. EEXIST is not durability proof.
        owner_lock_sync_failpoint!();
        self.directory.sync_all()?;
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(lock),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                Err(LayoutError::OwnerContended { owner })
            }
            Err(error) => Err(LayoutError::Io(errno_to_io(error))),
        }
    }

    fn acquire_writer_lock(&self) -> Result<File, LayoutError> {
        let started = Instant::now();
        loop {
            match self.acquire_lock(WRITER_OWNER_LOCK_NAME, OwnerKind::Writer) {
                Err(LayoutError::OwnerContended {
                    owner: OwnerKind::Writer,
                }) if started.elapsed() < WRITER_LOCK_HANDOFF_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }

    fn acquire_writer_root_lock(&self) -> Result<File, LayoutError> {
        let lock = rustix::fs::openat(
            &*self.directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(errno_to_io)
        .map_err(LayoutError::Io)?;
        let started = Instant::now();
        loop {
            match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(lock),
                Err(error)
                    if error == rustix::io::Errno::WOULDBLOCK
                        && started.elapsed() < WRITER_LOCK_HANDOFF_TIMEOUT =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                    return Err(LayoutError::OwnerContended {
                        owner: OwnerKind::Writer,
                    });
                }
                Err(error) => return Err(LayoutError::Io(errno_to_io(error))),
            }
        }
    }

    fn open_final(&self, address: SegmentAddress, kind: FileKind) -> Result<File, LayoutError> {
        let day = self.open_day(address.day)?;
        let name = match kind {
            FileKind::Pgm => address.pgm_name(),
            FileKind::Ovf => address.ovf_name(),
        };
        open_regular_at(&day, &name, OFlags::RDONLY)
    }

    fn open_day(&self, day: UtcDay) -> Result<File, LayoutError> {
        let year = open_directory_at(&self.directory, &day.year_component())?;
        let month = open_directory_at(&year, &day.month_component())?;
        open_directory_at(&month, &day.day_component())
    }

    fn ensure_day(&self, day: UtcDay) -> Result<File, LayoutError> {
        let year = ensure_directory_at(&self.directory, &day.year_component())?;
        let month = ensure_directory_at(&year, &day.month_component())?;
        ensure_directory_at(&month, &day.day_component())
    }
}

/// Lifetime token for the only collector allowed to mutate one data root.
#[derive(Debug)]
pub struct WriterOwner {
    root: DataRoot,
    owner_lock: File,
    _root_lock: File,
    startup_quarantine_outcomes: Vec<QuarantineOutcome>,
}

/// Opaque clone of the writer lock held by a long-lived mutation handle.
///
/// The operating-system lock is released only after both the owner and every
/// lease cloned from it have been dropped.
#[derive(Debug)]
pub struct WriterLease {
    _lock: File,
}

/// Filesystem role of an already-open fresh journal descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalSlotKind {
    /// Canonical root `active.parts`.
    Canonical,
    /// Recognized bounded alternate generation retained at the root.
    Alternate(PathIdentity),
}

/// Writer-owned, already-open fresh journal slot.
///
/// The raw root name is private. Consuming the slot transfers both the file
/// descriptor and the writer-lock lease to `kronika-writer`.
pub struct JournalSlot {
    lease: WriterLease,
    file: File,
    kind: JournalSlotKind,
}

impl JournalSlot {
    /// Returns the slot's root grammar role.
    #[must_use]
    pub const fn kind(&self) -> JournalSlotKind {
        self.kind
    }

    /// Returns the fresh writable descriptor.
    pub const fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Consumes the slot into the exact writer lease and descriptor.
    #[must_use]
    pub fn into_file_and_lease(self) -> (File, WriterLease) {
        (self.file, self.lease)
    }
}

impl fmt::Debug for JournalSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalSlot")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Fresh alternate generation created when no safe canonical journal can be
/// opened. The slot remains usable even when root-directory durability
/// degraded.
#[derive(Debug)]
pub struct FreshJournalGeneration {
    /// Already-open writable generation.
    pub slot: JournalSlot,
    /// Local root-directory synchronization failure, when any.
    pub diagnostic: Option<QuarantineFailure>,
}

/// Current owned-store location of a pinned exact journal evidence descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceLocation {
    /// The damaged object still occupies canonical `active.parts`.
    Canonical,
    /// The object is retained by a recognized bounded root recovery name.
    Pending(PendingRootKind),
    /// The object was moved into opaque quarantine.
    Quarantined(PathIdentity),
}

/// Pinned exact journal evidence retained across activation and quarantine.
pub struct EvidenceFile {
    file: File,
    identity: PathIdentity,
    location: EvidenceLocation,
    name: CString,
}

impl EvidenceFile {
    /// Returns the already-open exact evidence descriptor.
    #[must_use]
    pub const fn file(&self) -> &File {
        &self.file
    }

    /// Returns the original canonical object identity.
    #[must_use]
    pub const fn identity(&self) -> PathIdentity {
        self.identity
    }

    /// Returns the current typed location without exposing its raw name.
    #[must_use]
    pub const fn location(&self) -> EvidenceLocation {
        self.location
    }
}

impl fmt::Debug for EvidenceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvidenceFile")
            .field("identity", &self.identity)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

/// How a fresh journal generation became usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalActivation {
    /// Atomic `RENAME_EXCHANGE` installed it at `active.parts`.
    Exchanged,
    /// Two no-replace renames installed it after retaining evidence.
    Fallback,
    /// It remains a recognized alternate generation after a local failure.
    Alternate,
}

/// Completed journal rotation. Collection can continue through `fresh` for
/// every activation variant.
#[derive(Debug)]
pub struct JournalRotationOutcome {
    /// Already-open fresh canonical or alternate journal.
    pub fresh: JournalSlot,
    /// Already-open exact original evidence.
    pub evidence: EvidenceFile,
    /// Activation path used.
    pub activation: JournalActivation,
    /// Bounded payload-free local degradation details.
    pub diagnostics: Vec<QuarantineFailure>,
}

/// Prepared exact-evidence rotation with a caller-initialized fresh file.
pub struct JournalRotation {
    root: DataRoot,
    lease: WriterLease,
    fresh: File,
    fresh_name: CString,
    evidence: EvidenceFile,
    diagnostics: Vec<QuarantineFailure>,
    nonce: u64,
}

impl JournalRotation {
    /// Returns the fresh descriptor for canonical journal initialization.
    ///
    /// The caller must write a complete valid header before [`activate`](Self::activate).
    pub const fn fresh_file_mut(&mut self) -> &mut File {
        &mut self.fresh
    }

    /// Installs the initialized fresh journal without overwriting evidence.
    ///
    /// Atomic exchange is preferred. A bounded two-rename no-replace fallback
    /// preserves the original under a recognized evidence name. Any local
    /// failure returns a usable alternate generation and typed diagnostics.
    #[must_use]
    pub fn activate(mut self) -> JournalRotationOutcome {
        if let Err(error) = self.fresh.sync_all() {
            self.diagnostics.push(quarantine_failure(
                QuarantineFailureStage::FreshJournalSync,
                &error,
            ));
            return self.finish_alternate();
        }
        if let Err(failure) = verify_active_evidence(&self.root, &self.evidence) {
            self.diagnostics.push(failure);
            return self.finish_alternate();
        }

        match exchange_root_names(&self.root.directory, ACTIVE_JOURNAL_NAME, &self.fresh_name) {
            Ok(()) => {
                if let Err(error) = self.root.directory.sync_all() {
                    self.diagnostics.push(quarantine_failure(
                        QuarantineFailureStage::RootDirectorySync,
                        &error,
                    ));
                }
                self.evidence.name = self.fresh_name.clone();
                self.evidence.location =
                    EvidenceLocation::Pending(PendingRootKind::JournalGeneration);
                self.move_evidence_to_pending_name();
                return self.finish(JournalSlotKind::Canonical, JournalActivation::Exchanged);
            }
            Err(error) => self.diagnostics.push(quarantine_failure(
                QuarantineFailureStage::JournalExchange,
                &error,
            )),
        }

        if !self.move_canonical_to_pending_name() {
            return self.finish_alternate();
        }
        match rename_generation_to_active(&self.root.directory, &self.fresh_name) {
            Ok(()) => {
                if let Err(error) = self.root.directory.sync_all() {
                    self.diagnostics.push(quarantine_failure(
                        QuarantineFailureStage::RootDirectorySync,
                        &error,
                    ));
                }
                self.finish(JournalSlotKind::Canonical, JournalActivation::Fallback)
            }
            Err(error) => {
                self.diagnostics.push(quarantine_failure(
                    QuarantineFailureStage::JournalPromotion,
                    &error,
                ));
                self.finish_alternate()
            }
        }
    }

    fn move_evidence_to_pending_name(&mut self) {
        let Some(destination) =
            available_evidence_name(&self.root.directory, self.nonce, &self.evidence.name)
        else {
            self.diagnostics.push(QuarantineFailure::local(
                QuarantineFailureStage::CollisionSlotsExhausted,
                io::ErrorKind::AlreadyExists,
            ));
            return;
        };
        match rename_noreplace(
            &self.root.directory,
            &self.evidence.name,
            &self.root.directory,
            &destination,
        ) {
            Ok(()) => {
                self.evidence.name = destination;
                self.evidence.location = EvidenceLocation::Pending(PendingRootKind::Evidence);
                if let Err(error) = self.root.directory.sync_all() {
                    self.diagnostics.push(quarantine_failure(
                        QuarantineFailureStage::RootDirectorySync,
                        &error,
                    ));
                }
            }
            Err(error) => self
                .diagnostics
                .push(quarantine_failure(QuarantineFailureStage::Rename, &error)),
        }
    }

    fn move_canonical_to_pending_name(&mut self) -> bool {
        let Some(destination) =
            available_evidence_name(&self.root.directory, self.nonce, c"active.parts")
        else {
            self.diagnostics.push(QuarantineFailure::local(
                QuarantineFailureStage::CollisionSlotsExhausted,
                io::ErrorKind::AlreadyExists,
            ));
            return false;
        };
        let active = c"active.parts";
        match rename_noreplace(
            &self.root.directory,
            active,
            &self.root.directory,
            &destination,
        ) {
            Ok(()) => {
                self.evidence.name = destination;
                self.evidence.location = EvidenceLocation::Pending(PendingRootKind::Evidence);
                if let Err(error) = self.root.directory.sync_all() {
                    self.diagnostics.push(quarantine_failure(
                        QuarantineFailureStage::RootDirectorySync,
                        &error,
                    ));
                }
                true
            }
            Err(error) => {
                self.diagnostics
                    .push(quarantine_failure(QuarantineFailureStage::Rename, &error));
                false
            }
        }
    }

    fn finish_alternate(self) -> JournalRotationOutcome {
        let file = FileIdentity::from_file(&self.fresh).unwrap_or(self.evidence.identity.file);
        let identity = path_identity_from_file(
            EntryScope::Root,
            &self.fresh_name,
            EntryFileType::RegularFile,
            file,
        );
        self.finish(
            JournalSlotKind::Alternate(identity),
            JournalActivation::Alternate,
        )
    }

    fn finish(
        self,
        kind: JournalSlotKind,
        activation: JournalActivation,
    ) -> JournalRotationOutcome {
        JournalRotationOutcome {
            fresh: JournalSlot {
                lease: self.lease,
                file: self.fresh,
                kind,
            },
            evidence: self.evidence,
            activation,
            diagnostics: self.diagnostics,
        }
    }
}

impl fmt::Debug for JournalRotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalRotation")
            .field("fresh_name_hash", &hash_name(self.fresh_name.as_bytes()))
            .field("evidence", &self.evidence)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

impl WriterOwner {
    /// Returns read-only access to the same verified root.
    #[must_use]
    pub const fn root(&self) -> &DataRoot {
        &self.root
    }

    /// Returns bounded local outcomes from automatic startup quarantine.
    #[must_use]
    pub fn startup_quarantine_outcomes(&self) -> &[QuarantineOutcome] {
        &self.startup_quarantine_outcomes
    }

    /// Clones the operating-system writer-lock lease.
    ///
    /// Long-lived mutation handles retain this value so dropping the original
    /// [`WriterOwner`] cannot release ownership while they are still usable.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the lock descriptor cannot be duplicated.
    pub fn try_clone_lease(&self) -> Result<WriterLease, LayoutError> {
        Ok(WriterLease {
            _lock: self.owner_lock.try_clone()?,
        })
    }

    /// Prepares an exact-evidence rotation of the canonical active journal.
    ///
    /// The existing regular file is opened without following links and pinned
    /// by identity. A fresh, process-unique, bounded root generation is created
    /// exclusively and returned for caller initialization.
    ///
    /// # Errors
    ///
    /// Returns a typed layout/I/O error if the canonical file is absent or
    /// unsafe, no generation slot is available, or a descriptor cannot be
    /// opened.
    pub fn begin_journal_rotation(&self) -> Result<JournalRotation, LayoutError> {
        let evidence_file = self
            .root
            .open_active_journal()?
            .ok_or(LayoutError::ActiveJournalMissing)?;
        let evidence_identity = FileIdentity::from_file(&evidence_file)?;
        let active_name = c"active.parts";
        let evidence = EvidenceFile {
            file: evidence_file,
            identity: path_identity_from_file(
                EntryScope::Root,
                active_name,
                EntryFileType::RegularFile,
                evidence_identity,
            ),
            location: EvidenceLocation::Canonical,
            name: active_name.to_owned(),
        };
        let nonce = root_generation_nonce(evidence_identity);
        let mut fresh = None;
        for slot in 0..MAX_ROOT_COLLISION_SLOTS {
            let name = generation_name(nonce, slot);
            let name =
                CString::new(name).map_err(|_invalid_name| LayoutError::UnexpectedRootEntry {
                    name: "generated journal name contains NUL".to_owned(),
                })?;
            match create_regular_name_at(&self.root.directory, &name, OFlags::RDWR, DATA_FILE_MODE)
            {
                Ok(file) => {
                    fresh = Some((file, name));
                    break;
                }
                Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let (fresh, fresh_name) = fresh.ok_or_else(|| LayoutError::RecoverySlotsExhausted {
            limit: usize::from(MAX_ROOT_COLLISION_SLOTS),
        })?;
        let mut diagnostics = Vec::new();
        if let Err(error) = self.root.directory.sync_all() {
            diagnostics.push(quarantine_failure(
                QuarantineFailureStage::RootDirectorySync,
                &error,
            ));
        }
        Ok(JournalRotation {
            root: self.root.clone(),
            lease: self.try_clone_lease()?,
            fresh,
            fresh_name,
            evidence,
            diagnostics,
            nonce,
        })
    }

    /// Creates a fresh alternate root journal generation without interpreting
    /// an unsafe canonical control entry.
    ///
    /// This is the local-degradation path for a missing, symlinked, or
    /// wrong-type `active.parts`. The caller initializes the returned file
    /// through the normal journal contract and collection can continue without
    /// replacing or deleting the affected entry.
    ///
    /// # Errors
    ///
    /// Returns an error only when no exclusive generation name can be created
    /// or the writer lease/file operation itself fails.
    pub fn create_journal_generation(&self) -> Result<FreshJournalGeneration, LayoutError> {
        let root_identity = FileIdentity::from_file(&self.root.directory)?;
        let nonce = root_generation_nonce(root_identity);
        for slot in 0..MAX_ROOT_COLLISION_SLOTS {
            let name = CString::new(generation_name(nonce, slot)).map_err(|_invalid_name| {
                LayoutError::UnexpectedRootEntry {
                    name: "generated journal name contains NUL".to_owned(),
                }
            })?;
            match create_regular_name_at(&self.root.directory, &name, OFlags::RDWR, DATA_FILE_MODE)
            {
                Ok(file) => {
                    let identity = path_identity_from_file(
                        EntryScope::Root,
                        &name,
                        EntryFileType::RegularFile,
                        FileIdentity::from_file(&file)?,
                    );
                    let diagnostic = self.root.directory.sync_all().err().map(|error| {
                        quarantine_failure(QuarantineFailureStage::RootDirectorySync, &error)
                    });
                    return Ok(FreshJournalGeneration {
                        slot: JournalSlot {
                            lease: self.try_clone_lease()?,
                            file,
                            kind: JournalSlotKind::Alternate(identity),
                        },
                        diagnostic,
                    });
                }
                Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(LayoutError::RecoverySlotsExhausted {
            limit: usize::from(MAX_ROOT_COLLISION_SLOTS),
        })
    }

    /// Reopens a recognized alternate generation as a writer-owned slot.
    ///
    /// # Errors
    ///
    /// Returns an error for an evidence entry, identity change, unsafe type, or
    /// descriptor failure.
    pub fn open_journal_generation(
        &self,
        pending: &PendingRootEntry,
    ) -> Result<JournalSlot, LayoutError> {
        if pending.kind != PendingRootKind::JournalGeneration {
            return Err(LayoutError::TemporaryChanged {
                name: format!("opaque-root:{:016x}", pending.identity.name_hash),
            });
        }
        let file = open_regular_name_at(&self.root.directory, &pending.name, OFlags::RDWR)?;
        if FileIdentity::from_file(&file)? != pending.identity.file {
            return Err(LayoutError::TemporaryChanged {
                name: format!("opaque-root:{:016x}", pending.identity.name_hash),
            });
        }
        Ok(JournalSlot {
            lease: self.try_clone_lease()?,
            file,
            kind: JournalSlotKind::Alternate(pending.identity),
        })
    }

    /// Moves pinned active-journal evidence into opaque quarantine.
    ///
    /// The already-open evidence descriptor remains readable after a
    /// successful rename. Local failures retain the source and return a typed
    /// degraded outcome.
    #[must_use]
    pub fn quarantine_evidence(
        &self,
        evidence: &mut EvidenceFile,
        reason: QuarantineReason,
    ) -> QuarantineOutcome {
        let current = FileIdentity::from_file(&evidence.file).unwrap_or(evidence.identity.file);
        let source = path_identity_from_file(
            EntryScope::Root,
            &evidence.name,
            EntryFileType::RegularFile,
            current,
        );
        let path = EntryPath {
            parent: EntryParent::Root,
            name: evidence.name.clone(),
        };
        let outcome = self.quarantine_entry(&path, source, reason);
        match outcome.status {
            QuarantineStatus::Quarantined { destination }
            | QuarantineStatus::QuarantinedDegraded { destination, .. } => {
                evidence.location = EvidenceLocation::Quarantined(destination);
            }
            QuarantineStatus::Missing
            | QuarantineStatus::Changed { .. }
            | QuarantineStatus::Retained { .. } => {}
        }
        outcome
    }

    /// Opens or creates the root-level journal without following links.
    ///
    /// The boolean result is `true` only when this operation created the
    /// directory entry. A creator must write and synchronize the valid initial
    /// header before calling [`sync_root`](Self::sync_root).
    ///
    /// # Errors
    ///
    /// Returns an error when the journal entry is unsafe or inaccessible.
    pub fn open_or_create_journal(&self) -> Result<(File, bool), LayoutError> {
        open_or_create_regular(
            &self.root.directory,
            ACTIVE_JOURNAL_NAME,
            OFlags::RDWR,
            DATA_FILE_MODE,
        )
    }

    /// Synchronizes the data-root directory after a new control entry has been
    /// fully initialized.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory sync fails.
    pub fn sync_root(&self) -> Result<(), LayoutError> {
        self.root.directory.sync_all().map_err(LayoutError::Io)
    }

    /// Atomically moves a discovered unsupported entry into opaque quarantine.
    ///
    /// The raw path remains private, the input entry is revalidated without
    /// following links, destination names are bounded and collision-safe, and
    /// no existing object is overwritten. Every local failure is returned as a
    /// degraded outcome so callers can continue processing other entries.
    #[must_use]
    pub fn quarantine_foreign(&self, foreign: &ForeignEntry) -> QuarantineOutcome {
        self.quarantine_entry(
            &foreign.path,
            foreign.diagnostic.path,
            QuarantineReason::ForeignEntry,
        )
    }

    /// Atomically preserves a PGM that failed startup validation.
    ///
    /// The immutable segment identity from discovery pins the exact source
    /// object. No existing quarantine object is overwritten.
    #[must_use]
    pub fn quarantine_invalid_pgm(&self, segment: SegmentArtifacts) -> QuarantineOutcome {
        let name = segment.address.pgm_name();
        let Ok(name) = CString::new(name.as_bytes()) else {
            return invalid_generated_name_outcome(
                QuarantineReason::InvalidPgm,
                EntryScope::Day(segment.address.day),
                name.as_bytes(),
                segment.pgm_identity,
            );
        };
        let path = EntryPath {
            parent: EntryParent::Day(segment.address.day),
            name,
        };
        let source = path_identity_from_file(
            path.parent.scope(),
            &path.name,
            EntryFileType::RegularFile,
            segment.pgm_identity,
        );
        self.quarantine_entry(&path, source, QuarantineReason::InvalidPgm)
    }

    /// Atomically preserves a stale writer temporary after identity recheck.
    #[must_use]
    pub fn quarantine_temporary(&self, temporary: &TemporaryObject) -> QuarantineOutcome {
        let Ok(name) = CString::new(temporary.file_name.as_bytes()) else {
            return invalid_generated_name_outcome(
                QuarantineReason::StaleTemporary,
                EntryScope::Day(temporary.address.day),
                temporary.file_name.as_bytes(),
                temporary.identity,
            );
        };
        let path = EntryPath {
            parent: EntryParent::Day(temporary.address.day),
            name,
        };
        let source = path_identity_from_file(
            path.parent.scope(),
            &path.name,
            EntryFileType::RegularFile,
            temporary.identity,
        );
        self.quarantine_entry(&path, source, QuarantineReason::StaleTemporary)
    }

    /// Atomically preserves a recognized root recovery object.
    #[must_use]
    pub fn quarantine_pending_root(
        &self,
        pending: &PendingRootEntry,
        reason: QuarantineReason,
    ) -> QuarantineOutcome {
        let path = EntryPath {
            parent: EntryParent::Root,
            name: pending.name.clone(),
        };
        self.quarantine_entry(&path, pending.identity, reason)
    }

    fn quarantine_entry(
        &self,
        path: &EntryPath,
        source: PathIdentity,
        reason: QuarantineReason,
    ) -> QuarantineOutcome {
        quarantine_entry(&self.root, path, source, reason)
    }

    /// Creates a new process-unique PGM temporary in the segment's UTC day.
    ///
    /// # Errors
    ///
    /// Returns an error when the day cannot be safely created or the temporary
    /// cannot be opened exclusively.
    pub fn create_pgm_temp(&self, address: SegmentAddress) -> Result<PgmTemp<'_>, LayoutError> {
        let day = self.root.ensure_day(address.day)?;
        let temp_name = temporary_name(address, TemporaryKind::Pgm);
        let file = create_regular_at(&day, &temp_name, OFlags::RDWR, DATA_FILE_MODE)?;
        Ok(PgmTemp {
            _owner: self,
            day,
            file,
            prepared_identity: Cell::new(None),
            temp_name,
            final_name: address.pgm_name(),
            address,
            published: false,
        })
    }

    /// Removes a verified stale writer temporary and synchronizes its day.
    ///
    /// # Errors
    ///
    /// Returns an error for the wrong owner kind, a changed file type, or I/O.
    pub fn remove_temporary(&self, temporary: &TemporaryObject) -> Result<(), LayoutError> {
        if temporary.kind != TemporaryKind::Pgm {
            return Err(LayoutError::UnexpectedLeafEntry {
                name: temporary.file_name.clone(),
            });
        }
        remove_verified_regular(&self.root, temporary.address, temporary.file_name())
    }

    /// Removes a sealed segment (its PGM and sibling OVF) and prunes the day.
    ///
    /// Direct unlink is safe: live readers keep their own descriptors, the
    /// overview owner revalidates the input file before publishing an OVF, and
    /// its GC rechecks device/inode, so no delayed two-step deletion is needed.
    /// A part of the pair that already vanished frees zero bytes instead of
    /// failing. The writer owns the root, so empty calendar ancestors are
    /// pruned in place.
    ///
    /// # Errors
    ///
    /// Returns an error if a present entry is an unsafe type or a filesystem
    /// operation other than a missing-file race fails.
    pub fn remove_sealed_segment(
        &self,
        address: SegmentAddress,
    ) -> Result<SegmentRemoval, LayoutError> {
        let day = match self.root.open_day(address.day) {
            Ok(day) => day,
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SegmentRemoval::default());
            }
            Err(error) => return Err(error),
        };
        let pgm_bytes = unlink_regular_capturing_size(&day, &address.pgm_name())?;
        let ovf_bytes = unlink_regular_capturing_size(&day, &address.ovf_name())?;
        day.sync_all()?;
        prune_empty_calendar(&self.root, address.day)?;
        Ok(SegmentRemoval {
            pgm_bytes: pgm_bytes.unwrap_or(0),
            ovf_bytes,
        })
    }

    /// Removes an overview sidecar that has no sibling PGM and prunes the day.
    ///
    /// Returns the bytes reclaimed, or zero if the sidecar was already gone.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry is an unsafe type or the unlink fails for
    /// a reason other than the file already being absent.
    pub fn remove_orphan_overview(&self, address: SegmentAddress) -> Result<u64, LayoutError> {
        let day = match self.root.open_day(address.day) {
            Ok(day) => day,
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(0);
            }
            Err(error) => return Err(error),
        };
        let freed = unlink_regular_capturing_size(&day, &address.ovf_name())?;
        day.sync_all()?;
        prune_empty_calendar(&self.root, address.day)?;
        Ok(freed.unwrap_or(0))
    }

    /// Removes one object found by [`DataRoot::scan_quarantine`] after an
    /// identity recheck and reports the bytes it freed; a vanished or replaced
    /// object frees zero. Non-regular entries are never removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry is an unsafe type or a filesystem
    /// operation other than a missing-file race fails.
    pub fn remove_quarantine_entry(&self, entry: &QuarantineEntry) -> Result<u64, LayoutError> {
        if entry.identity.file_type != EntryFileType::RegularFile {
            return Ok(0);
        }
        let directory = match open_directory_at(&self.root.directory, QUARANTINE_DIRECTORY_NAME) {
            Ok(directory) => directory,
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(0);
            }
            Err(error) => return Err(error),
        };
        if unlink_named_if_identity(&directory, entry.file_name(), entry.identity.file)? {
            directory.sync_all()?;
            Ok(entry.identity.file.len)
        } else {
            Ok(0)
        }
    }
}

/// Exclusive, crash-safe PGM publication in one verified day directory.
#[derive(Debug)]
pub struct PgmTemp<'owner> {
    _owner: &'owner WriterOwner,
    day: File,
    file: File,
    prepared_identity: Cell<Option<FileIdentity>>,
    temp_name: String,
    final_name: String,
    address: SegmentAddress,
    published: bool,
}

impl PgmTemp<'_> {
    /// Returns the temporary file to the segment encoder.
    ///
    /// The caller must flush buffered writers before [`publish`](Self::publish).
    pub const fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Clones the open temporary descriptor and freezes its exact identity for
    /// validation or exact recovery comparison.
    ///
    /// Mutating the temporary after this call makes publication fail.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the descriptor cannot be duplicated.
    pub fn try_clone_file(&self) -> Result<File, LayoutError> {
        let file = self.file.try_clone()?;
        self.prepared_identity
            .set(Some(FileIdentity::from_file(&file)?));
        Ok(file)
    }

    /// Synchronizes and publishes the PGM without overwriting an existing one.
    ///
    /// The day directory is synchronized after adding the final link and again
    /// after removing the temporary name.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError::SegmentAlreadyExists`] for an existing final PGM,
    /// preserving it unchanged.
    pub fn publish(&mut self) -> Result<(), LayoutError> {
        pgm_publish_failpoint!(FileSync);
        self.file.sync_all()?;
        let current_identity = FileIdentity::from_file(&self.file)?;
        let expected_identity = self.prepared_identity.get().unwrap_or(current_identity);
        if current_identity != expected_identity {
            return Err(LayoutError::TemporaryChanged {
                name: self.temp_name.clone(),
            });
        }
        verify_named_identity(
            &self.day,
            &self.temp_name,
            expected_identity,
            &self.temp_name,
        )?;
        pgm_publish_failpoint!(Link);
        match link_open_file(&self.file, &self.day, &self.temp_name, &self.final_name) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => {
                return Err(LayoutError::SegmentAlreadyExists {
                    id: self.address.id,
                });
            }
            Err(error) => return Err(LayoutError::Io(errno_to_io(error))),
        }
        // Adding a hard link changes the inode ctime, so compare the final name
        // with the exact post-link identity of the still-open descriptor.
        let linked_identity = FileIdentity::from_file(&self.file)?;
        verify_named_identity(
            &self.day,
            &self.final_name,
            linked_identity,
            &self.temp_name,
        )?;
        pgm_publish_failpoint!(LinkedDirectorySync);
        self.day.sync_all()?;
        pgm_publish_failpoint!(TemporaryUnlink);
        rustix::fs::unlinkat(&self.day, &self.temp_name, AtFlags::empty())
            .map_err(errno_to_io)
            .map_err(LayoutError::Io)?;
        pgm_publish_failpoint!(UnlinkedDirectorySync);
        self.day.sync_all()?;
        self.published = true;
        Ok(())
    }

    /// Removes an unpublished temporary name and synchronizes its day.
    ///
    /// This is used after recovery proved that an already published final PGM
    /// is byte-identical to the newly encoded journal contents.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if cleanup cannot be made durable.
    pub fn discard(mut self) -> Result<(), LayoutError> {
        let expected = FileIdentity::from_file(&self.file)?;
        if !unlink_named_if_identity(&self.day, &self.temp_name, expected)? {
            return Err(LayoutError::TemporaryChanged {
                name: self.temp_name.clone(),
            });
        }
        self.day.sync_all()?;
        self.published = true;
        Ok(())
    }
}

impl Drop for PgmTemp<'_> {
    fn drop(&mut self) {
        if !self.published
            && let Ok(expected) = FileIdentity::from_file(&self.file)
        {
            unlink_named_if_identity(&self.day, &self.temp_name, expected).ok();
        }
    }
}

/// Lifetime token for the only process allowed to mutate overview artifacts.
#[derive(Debug)]
pub struct OverviewOwner {
    root: DataRoot,
    _lock: File,
}

impl OverviewOwner {
    /// Returns read-only access to the same verified root.
    #[must_use]
    pub const fn root(&self) -> &DataRoot {
        &self.root
    }

    /// Creates an OVF temporary and captures the input PGM file identity.
    ///
    /// # Errors
    ///
    /// Returns an error if the source PGM or destination day is unsafe.
    pub fn create_ovf_temp(&self, address: SegmentAddress) -> Result<OvfTemp<'_>, LayoutError> {
        self.create_overview_temp(address, TemporaryKind::Ovf)
    }

    /// Creates a writeability-probe temporary beside the addressed PGM.
    ///
    /// # Errors
    ///
    /// Returns an error if the source PGM or destination day is unsafe.
    pub fn create_probe_temp(&self, address: SegmentAddress) -> Result<OvfTemp<'_>, LayoutError> {
        self.create_overview_temp(address, TemporaryKind::OverviewProbe)
    }

    fn create_overview_temp(
        &self,
        address: SegmentAddress,
        kind: TemporaryKind,
    ) -> Result<OvfTemp<'_>, LayoutError> {
        let source = self.root.open_pgm(address)?;
        let input_file_identity = FileIdentity::from_file(&source)?;
        let day = self.root.open_day(address.day)?;
        let temp_name = temporary_name(address, kind);
        let file = create_regular_at(&day, &temp_name, OFlags::RDWR, DATA_FILE_MODE)?;
        Ok(OvfTemp {
            _owner: self,
            root: self.root.clone(),
            day,
            file,
            prepared_identity: Cell::new(None),
            temp_name,
            final_name: address.ovf_name(),
            address,
            input_file_identity,
            kind,
            completed: false,
        })
    }

    /// Removes a verified OVF and synchronizes its day.
    ///
    /// # Errors
    ///
    /// Returns an error if the file changed to an unsafe type or unlink fails.
    pub fn remove_ovf(&self, address: SegmentAddress) -> Result<(), LayoutError> {
        remove_verified_regular(&self.root, address, &address.ovf_name())
    }

    /// Removes an OVF only if the currently named object still has the
    /// previously observed filesystem identity.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry is unsafe or the unlink cannot be
    /// synchronized. A changed or missing entry returns `Ok(false)`.
    pub fn remove_ovf_if_identity(
        &self,
        address: SegmentAddress,
        device: u64,
        inode: u64,
    ) -> Result<bool, LayoutError> {
        remove_regular_if_identity(&self.root, address, &address.ovf_name(), device, inode)
    }

    /// Removes a verified stale OVF or probe temporary.
    ///
    /// # Errors
    ///
    /// Returns an error for a writer temporary, changed type, or I/O failure.
    pub fn remove_temporary(&self, temporary: &TemporaryObject) -> Result<(), LayoutError> {
        if temporary.kind == TemporaryKind::Pgm {
            return Err(LayoutError::UnexpectedLeafEntry {
                name: temporary.file_name.clone(),
            });
        }
        remove_verified_regular(&self.root, temporary.address, temporary.file_name())
    }

    /// Removes an overview-owned temporary only if its filesystem identity is
    /// unchanged since the strict inventory.
    ///
    /// # Errors
    ///
    /// Returns an error for a writer temporary or unsafe entry. A changed or
    /// missing object returns `Ok(false)`.
    pub fn remove_temporary_if_identity(
        &self,
        temporary: &TemporaryObject,
        device: u64,
        inode: u64,
    ) -> Result<bool, LayoutError> {
        if temporary.kind == TemporaryKind::Pgm {
            return Err(LayoutError::UnexpectedLeafEntry {
                name: temporary.file_name.clone(),
            });
        }
        remove_regular_if_identity(
            &self.root,
            temporary.address,
            temporary.file_name(),
            device,
            inode,
        )
    }

    /// Removes an empty UTC day and then its empty month/year ancestors.
    ///
    /// A non-empty or concurrently removed directory is a successful no-op.
    /// Pruning is also a no-op while a writer owns the root, so a collector
    /// cannot publish through a descriptor for a directory removed underneath
    /// it.
    /// Every removed directory entry is synchronized in its parent.
    ///
    /// # Errors
    ///
    /// Returns an error when an existing calendar component is unsafe or a
    /// filesystem operation other than the expected empty-directory races
    /// fails.
    pub fn prune_empty_day(&self, day: UtcDay) -> Result<(), LayoutError> {
        let _writer_quiescence = match self
            .root
            .acquire_lock(WRITER_OWNER_LOCK_NAME, OwnerKind::Writer)
        {
            Ok(lock) => lock,
            Err(LayoutError::OwnerContended {
                owner: OwnerKind::Writer,
            }) => return Ok(()),
            Err(error) => return Err(error),
        };
        prune_empty_calendar(&self.root, day)
    }
}

/// Removes an empty UTC day directory and its now-empty month/year ancestors.
///
/// The caller must already hold writer quiescence: [`OverviewOwner`] takes the
/// writer lock first, while [`WriterOwner`] owns it for its whole lifetime. A
/// non-empty or concurrently removed directory ends the walk as a no-op.
fn prune_empty_calendar(root: &DataRoot, day: UtcDay) -> Result<(), LayoutError> {
    let year_name = day.year_component();
    let month_name = day.month_component();
    let day_name = day.day_component();
    let year = match open_directory_at(&root.directory, &year_name) {
        Ok(directory) => directory,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let month = match open_directory_at(&year, &month_name) {
        Ok(directory) => directory,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    match open_directory_at(&month, &day_name) {
        Ok(_directory) => {}
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    if !remove_empty_directory_at(&month, &day_name)? {
        return Ok(());
    }
    if !remove_empty_directory_at(&year, &month_name)? {
        return Ok(());
    }
    remove_empty_directory_at(&root.directory, &year_name)?;
    Ok(())
}

/// Exclusive OVF or probe temporary tied to one stable source PGM.
#[derive(Debug)]
pub struct OvfTemp<'owner> {
    _owner: &'owner OverviewOwner,
    root: DataRoot,
    day: File,
    file: File,
    prepared_identity: Cell<Option<FileIdentity>>,
    temp_name: String,
    final_name: String,
    address: SegmentAddress,
    input_file_identity: FileIdentity,
    kind: TemporaryKind,
    completed: bool,
}

impl OvfTemp<'_> {
    /// Returns the temporary file to the overview encoder.
    pub const fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Clones the open temporary descriptor and freezes its exact identity for
    /// validation before publication.
    ///
    /// Mutating the temporary after this call makes publication fail.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the descriptor cannot be duplicated.
    pub fn try_clone_file(&self) -> Result<File, LayoutError> {
        let file = self.file.try_clone()?;
        self.prepared_identity
            .set(Some(FileIdentity::from_file(&file)?));
        Ok(file)
    }

    /// Returns the verified process-unique leaf name for diagnostics and
    /// qualification barriers.
    #[must_use]
    pub fn temp_name(&self) -> &str {
        &self.temp_name
    }

    /// Synchronizes and atomically replaces the final OVF after source
    /// revalidation.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError::SourceChanged`] if the PGM changed while the OVF
    /// was built.
    pub fn publish(mut self) -> Result<(), LayoutError> {
        if self.kind != TemporaryKind::Ovf {
            return Err(LayoutError::UnexpectedLeafEntry {
                name: self.temp_name.clone(),
            });
        }
        self.file.sync_all()?;
        let current_identity = FileIdentity::from_file(&self.file)?;
        let expected_temporary = self.prepared_identity.get().unwrap_or(current_identity);
        if current_identity != expected_temporary {
            return Err(LayoutError::TemporaryChanged {
                name: self.temp_name.clone(),
            });
        }
        verify_named_identity(
            &self.day,
            &self.temp_name,
            expected_temporary,
            &self.temp_name,
        )?;
        let current_source = self.root.open_pgm(self.address)?;
        if FileIdentity::from_file(&current_source)? != self.input_file_identity {
            return Err(LayoutError::SourceChanged {
                id: self.address.id,
            });
        }
        match stat_no_follow(&self.day, &self.final_name) {
            Ok(stat) => {
                let kind = FileType::from_raw_mode(stat.st_mode);
                if kind == FileType::Symlink {
                    return Err(LayoutError::SymlinkNotAllowed {
                        name: self.final_name.clone(),
                    });
                }
                if kind != FileType::RegularFile {
                    return Err(LayoutError::UnexpectedLeafEntryType {
                        name: self.final_name.clone(),
                    });
                }
            }
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        rustix::fs::renameat(&self.day, &self.temp_name, &self.day, &self.final_name)
            .map_err(errno_to_io)
            .map_err(LayoutError::Io)?;
        // Renaming may change inode ctime. Pin the final name to the exact
        // post-rename identity of the descriptor that was validated above.
        let renamed_identity = FileIdentity::from_file(&self.file)?;
        verify_named_identity(
            &self.day,
            &self.final_name,
            renamed_identity,
            &self.temp_name,
        )?;
        self.day.sync_all()?;
        self.completed = true;
        Ok(())
    }

    /// Synchronizes and removes a writeability probe without publishing an OVF.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-probe temporary or failed persistence step.
    pub fn finish_probe(mut self) -> Result<(), LayoutError> {
        if self.kind != TemporaryKind::OverviewProbe {
            return Err(LayoutError::UnexpectedLeafEntry {
                name: self.temp_name.clone(),
            });
        }
        self.file.sync_all()?;
        let expected = FileIdentity::from_file(&self.file)?;
        if !unlink_named_if_identity(&self.day, &self.temp_name, expected)? {
            return Err(LayoutError::TemporaryChanged {
                name: self.temp_name.clone(),
            });
        }
        self.day.sync_all()?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for OvfTemp<'_> {
    fn drop(&mut self) {
        if !self.completed
            && let Ok(expected) = FileIdentity::from_file(&self.file)
        {
            unlink_named_if_identity(&self.day, &self.temp_name, expected).ok();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DayArtifacts {
    pgm: Option<FileIdentity>,
    ovf: Option<u64>,
}

#[derive(Debug)]
struct ScanState<'scan> {
    limits: LayoutLimits,
    visited_entries: &'scan mut usize,
    metadata_bytes: usize,
    days: Vec<UtcDay>,
    segments: Vec<SegmentArtifacts>,
    orphan_overviews: Vec<SegmentAddress>,
    temporaries: Vec<TemporaryObject>,
    foreign_entries: Vec<ForeignEntry>,
    pending_root_entries: Vec<PendingRootEntry>,
    quarantine_directory: QuarantineDirectoryState,
}

impl<'scan> ScanState<'scan> {
    const fn new(limits: LayoutLimits, visited_entries: &'scan mut usize) -> Self {
        Self {
            limits,
            visited_entries,
            metadata_bytes: 0,
            days: Vec::new(),
            segments: Vec::new(),
            orphan_overviews: Vec::new(),
            temporaries: Vec::new(),
            foreign_entries: Vec::new(),
            pending_root_entries: Vec::new(),
            quarantine_directory: QuarantineDirectoryState::Absent,
        }
    }

    fn record_foreign(
        &mut self,
        parent: EntryParent,
        name: &CStr,
        stat: &rustix::fs::Stat,
        reason: ForeignEntryReason,
    ) -> Result<(), LayoutError> {
        self.account_metadata(size_of::<ForeignEntry>())?;
        let path = EntryPath {
            parent,
            name: name.to_owned(),
        };
        let diagnostic = EntryDiagnostic {
            path: path_identity(parent, name, stat),
            reason,
        };
        self.foreign_entries.push(ForeignEntry { diagnostic, path });
        Ok(())
    }

    fn record_pending_root(
        &mut self,
        kind: PendingRootKind,
        name: &CStr,
        stat: &rustix::fs::Stat,
    ) -> Result<(), LayoutError> {
        self.account_metadata(size_of::<PendingRootEntry>())?;
        self.pending_root_entries.push(PendingRootEntry {
            kind,
            identity: path_identity(EntryParent::Root, name, stat),
            name: name.to_owned(),
        });
        Ok(())
    }

    fn account(&mut self, name_bytes: usize) -> Result<(), LayoutError> {
        *self.visited_entries = self.visited_entries.checked_add(1).ok_or({
            LayoutError::TraversalLimitExceeded {
                kind: LimitKind::VisitedEntries,
                limit: self.limits.max_visited_entries,
            }
        })?;
        if *self.visited_entries > self.limits.max_visited_entries {
            return Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::VisitedEntries,
                limit: self.limits.max_visited_entries,
            });
        }
        self.account_metadata(name_bytes.saturating_add(ENTRY_METADATA_BYTES))
    }

    fn account_metadata(&mut self, bytes: usize) -> Result<(), LayoutError> {
        self.metadata_bytes = self.metadata_bytes.checked_add(bytes).ok_or({
            LayoutError::TraversalLimitExceeded {
                kind: LimitKind::MetadataBytes,
                limit: self.limits.max_metadata_bytes,
            }
        })?;
        if self.metadata_bytes > self.limits.max_metadata_bytes {
            return Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::MetadataBytes,
                limit: self.limits.max_metadata_bytes,
            });
        }
        Ok(())
    }

    fn account_segment(&mut self) -> Result<(), LayoutError> {
        if self.segments.len() >= self.limits.max_segments {
            return Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::Segments,
                limit: self.limits.max_segments,
            });
        }
        self.account_metadata(size_of::<SegmentArtifacts>())
    }

    fn finish(mut self) -> LayoutSnapshot {
        self.days.sort_unstable();
        self.segments.sort_by_key(|segment| segment.address.id);
        self.orphan_overviews.sort_by_key(|address| address.id);
        self.temporaries
            .sort_by_key(|temporary| (temporary.address.id, temporary.kind as u8));
        self.foreign_entries
            .sort_by_key(|entry| entry.diagnostic.path);
        self.pending_root_entries
            .sort_by_key(|entry| (entry.kind, entry.identity));
        LayoutSnapshot {
            days: self.days,
            segments: self.segments,
            orphan_overviews: self.orphan_overviews,
            temporaries: self.temporaries,
            foreign_entries: self.foreign_entries,
            pending_root_entries: self.pending_root_entries,
            quarantine_directory: self.quarantine_directory,
            visited_entries: *self.visited_entries,
            metadata_bytes: self.metadata_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ParsedLeaf {
    Pgm(SegmentAddress),
    Ovf(SegmentAddress),
    Temporary(SegmentAddress, TemporaryKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Filesystem identity used to pin an immutable segment between discovery and
/// opening its contents.
pub struct FileIdentity {
    /// Filesystem device number.
    pub device: u64,
    /// Inode number on the device.
    pub inode: u64,
    /// File length in bytes.
    pub len: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime_seconds: i64,
    /// Nanosecond part of the modification time.
    pub mtime_nanoseconds: i64,
    /// Metadata-change time, whole seconds since the Unix epoch.
    pub ctime_seconds: i64,
    /// Nanosecond part of the metadata-change time.
    pub ctime_nanoseconds: i64,
}

impl FileIdentity {
    #[allow(
        clippy::useless_conversion,
        reason = "rustix Stat integer field types differ across supported Unix targets"
    )]
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: u64::try_from(stat.st_dev).unwrap_or(u64::MAX),
            inode: u64::try_from(stat.st_ino).unwrap_or(u64::MAX),
            len: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
            mtime_seconds: stat.st_mtime,
            mtime_nanoseconds: i64::try_from(stat.st_mtime_nsec).unwrap_or(i64::MAX),
            ctime_seconds: stat.st_ctime,
            ctime_nanoseconds: i64::try_from(stat.st_ctime_nsec).unwrap_or(i64::MAX),
        }
    }

    /// Reads the identity from an already-open file descriptor.
    ///
    /// # Errors
    ///
    /// Returns the underlying `fstat` error.
    pub fn from_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            mtime_seconds: metadata.mtime(),
            mtime_nanoseconds: metadata.mtime_nsec(),
            ctime_seconds: metadata.ctime(),
            ctime_nanoseconds: metadata.ctime_nsec(),
        })
    }

    const fn same_named_object(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.len == other.len
            && self.mtime_seconds == other.mtime_seconds
            && self.mtime_nanoseconds == other.mtime_nanoseconds
            && self.ctime_seconds == other.ctime_seconds
            && self.ctime_nanoseconds == other.ctime_nanoseconds
    }
}

const fn validate_limit(kind: LimitKind, value: usize, hard_max: usize) -> Result<(), LayoutError> {
    if value == 0 || value > hard_max {
        Err(LayoutError::InvalidLimits {
            kind,
            value,
            hard_max,
        })
    } else {
        Ok(())
    }
}

fn is_control_name(name: &str) -> bool {
    matches!(
        name,
        ACTIVE_JOURNAL_NAME | WRITER_OWNER_LOCK_NAME | OVERVIEW_OWNER_LOCK_NAME
    )
}

fn is_writer_bootstrap_control(foreign: &ForeignEntry) -> bool {
    foreign.diagnostic.path.scope == EntryScope::Root
        && matches!(
            foreign.path.name.as_bytes(),
            b"active.parts" | b".pgkronika-writer.owner.lock"
        )
}

fn is_writer_lock_name(foreign: &ForeignEntry) -> bool {
    foreign.diagnostic.path.scope == EntryScope::Root
        && foreign.path.name.as_bytes() == WRITER_OWNER_LOCK_NAME.as_bytes()
}

fn writer_lock_is_poisoned(root: &DataRoot) -> bool {
    stat_no_follow(&root.directory, WRITER_OWNER_LOCK_NAME)
        .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile)
}

fn is_dot(name: &[u8]) -> bool {
    name == b"." || name == b".."
}

fn ascii_name(name: &[u8]) -> Option<&str> {
    name.is_ascii()
        .then(|| std::str::from_utf8(name).ok())
        .flatten()
}

const fn entry_file_type(file_type: FileType) -> EntryFileType {
    match file_type {
        FileType::RegularFile => EntryFileType::RegularFile,
        FileType::Directory => EntryFileType::Directory,
        FileType::Symlink => EntryFileType::Symlink,
        _ => EntryFileType::Other,
    }
}

fn foreign_type_reason(file_type: FileType) -> ForeignEntryReason {
    if file_type == FileType::Symlink {
        ForeignEntryReason::SymbolicLink
    } else {
        ForeignEntryReason::UnsupportedType
    }
}

fn path_identity(parent: EntryParent, name: &CStr, stat: &rustix::fs::Stat) -> PathIdentity {
    PathIdentity {
        scope: parent.scope(),
        name_hash: hash_name(name.to_bytes()),
        name_len: u16::try_from(name.to_bytes().len()).unwrap_or(u16::MAX),
        file_type: entry_file_type(FileType::from_raw_mode(stat.st_mode)),
        file: FileIdentity::from_stat(stat),
    }
}

const fn path_identity_from_file(
    scope: EntryScope,
    name: &CStr,
    file_type: EntryFileType,
    file: FileIdentity,
) -> PathIdentity {
    PathIdentity {
        scope,
        name_hash: hash_name(name.to_bytes()),
        name_len: bounded_name_len(name.to_bytes().len()),
        file_type,
        file,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the explicit bound saturates every length above u16::MAX"
)]
const fn bounded_name_len(len: usize) -> u16 {
    if len > u16::MAX as usize {
        u16::MAX
    } else {
        len as u16
    }
}

const fn invalid_generated_name_outcome(
    reason: QuarantineReason,
    scope: EntryScope,
    name: &[u8],
    file: FileIdentity,
) -> QuarantineOutcome {
    QuarantineOutcome {
        reason,
        source: PathIdentity {
            scope,
            name_hash: hash_name(name),
            name_len: bounded_name_len(name.len()),
            file_type: EntryFileType::RegularFile,
            file,
        },
        status: QuarantineStatus::Retained {
            failure: QuarantineFailure::local(
                QuarantineFailureStage::SourceChanged,
                io::ErrorKind::InvalidInput,
            ),
        },
    }
}

const fn hash_name(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    hash
}

fn parse_pending_root_name(name: &str) -> Option<PendingRootKind> {
    if is_bounded_root_name(name, ROOT_EVIDENCE_PREFIX, ROOT_EVIDENCE_SUFFIX) {
        Some(PendingRootKind::Evidence)
    } else if is_bounded_root_name(name, ROOT_GENERATION_PREFIX, ROOT_GENERATION_SUFFIX) {
        Some(PendingRootKind::JournalGeneration)
    } else {
        None
    }
}

fn is_bounded_root_name(name: &str, prefix: &str, suffix: &str) -> bool {
    let Some(middle) = name
        .strip_prefix(prefix)
        .and_then(|remainder| remainder.strip_suffix(suffix))
    else {
        return false;
    };
    let expected_len = ROOT_NONCE_HEX_LEN + 1 + ROOT_SLOT_HEX_LEN;
    let middle = middle.as_bytes();
    middle.len() == expected_len
        && middle[ROOT_NONCE_HEX_LEN] == b'.'
        && middle[..ROOT_NONCE_HEX_LEN]
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && middle[ROOT_NONCE_HEX_LEN + 1..]
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn parse_year(name: &str) -> Option<u16> {
    (name.len() == 4 && name.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

fn parse_month(name: &str) -> Option<u8> {
    if name.len() != 2 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let month = name.parse().ok()?;
    (1..=12).contains(&month).then_some(month)
}

fn parse_day(year: u16, month: u8, name: &str) -> Option<u8> {
    if name.len() != 2 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let day = name.parse().ok()?;
    UtcDay::new(year, month, day).ok().map(|valid| valid.day)
}

fn parse_leaf(name: &str, day: UtcDay) -> Result<ParsedLeaf, LayoutError> {
    let fields: Vec<&str> = name.split('.').collect();
    let parsed = match fields.as_slice() {
        [id, "pgm"] => ParsedLeaf::Pgm(parse_address(id, day)?),
        [id, "ovf"] => ParsedLeaf::Ovf(parse_address(id, day)?),
        [id, "pgm", pid, seq, "tmp"]
            if parse_canonical_u64(pid).is_some() && parse_canonical_u64(seq).is_some() =>
        {
            ParsedLeaf::Temporary(parse_address(id, day)?, TemporaryKind::Pgm)
        }
        [id, "ovf", pid, seq, "tmp"]
            if parse_canonical_u64(pid).is_some() && parse_canonical_u64(seq).is_some() =>
        {
            ParsedLeaf::Temporary(parse_address(id, day)?, TemporaryKind::Ovf)
        }
        [id, "ovf", "probe", pid, seq, "tmp"]
            if parse_canonical_u64(pid).is_some() && parse_canonical_u64(seq).is_some() =>
        {
            ParsedLeaf::Temporary(parse_address(id, day)?, TemporaryKind::OverviewProbe)
        }
        _ => {
            return Err(LayoutError::UnexpectedLeafEntry {
                name: name.to_owned(),
            });
        }
    };
    Ok(parsed)
}

fn parse_address(id: &str, day: UtcDay) -> Result<SegmentAddress, LayoutError> {
    let value = parse_canonical_i64(id).ok_or_else(|| LayoutError::UnexpectedLeafEntry {
        name: id.to_owned(),
    })?;
    SegmentAddress::in_day(SegmentId::new(value)?, day)
}

fn parse_canonical_i64(value: &str) -> Option<i64> {
    if value.is_empty()
        || value.starts_with('+')
        || value == "-0"
        || (value.starts_with('0') && value.len() > 1)
        || (value.starts_with("-0") && value.len() > 2)
    {
        return None;
    }
    let parsed: i64 = value.parse().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty() || (value.starts_with('0') && value.len() > 1) {
        return None;
    }
    let parsed: u64 = value.parse().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn stat_no_follow(directory: &File, name: &str) -> Result<rustix::fs::Stat, LayoutError> {
    let name = CString::new(name).map_err(|_error| {
        LayoutError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "layout leaf contains a NUL byte",
        ))
    })?;
    stat_no_follow_name(directory, &name)
}

fn stat_no_follow_name(directory: &File, name: &CStr) -> Result<rustix::fs::Stat, LayoutError> {
    rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(errno_to_io)
        .map_err(LayoutError::Io)
}

fn open_directory_at(directory: &File, name: &str) -> Result<File, LayoutError> {
    rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            LayoutError::SymlinkNotAllowed {
                name: name.to_owned(),
            }
        } else {
            LayoutError::Io(errno_to_io(error))
        }
    })
}

fn ensure_directory_at(directory: &File, name: &str) -> Result<File, LayoutError> {
    match rustix::fs::mkdirat(directory, name, DIRECTORY_MODE) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::EXIST => {}
        Err(error) => return Err(LayoutError::Io(errno_to_io(error))),
    }
    let child = open_directory_at(directory, name)?;
    // An earlier attempt may have completed mkdirat but failed before its
    // parent fsync. EEXIST therefore cannot prove that the entry is durable.
    ensure_directory_sync_failpoint!();
    directory.sync_all()?;
    Ok(child)
}

fn open_regular_at(directory: &File, name: &str, access: OFlags) -> Result<File, LayoutError> {
    let name = CString::new(name).map_err(|_error| {
        LayoutError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "layout leaf contains a NUL byte",
        ))
    })?;
    open_regular_name_at(directory, &name, access)
}

fn open_regular_name_at(
    directory: &File,
    name: &CStr,
    access: OFlags,
) -> Result<File, LayoutError> {
    let file = rustix::fs::openat(
        directory,
        name,
        access | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            LayoutError::SymlinkNotAllowed {
                name: format!("opaque:{:016x}", hash_name(name.to_bytes())),
            }
        } else {
            LayoutError::Io(errno_to_io(error))
        }
    })?;
    if !file.metadata()?.is_file() {
        return Err(LayoutError::UnexpectedLeafEntryType {
            name: format!("opaque:{:016x}", hash_name(name.to_bytes())),
        });
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn pin_entry_name(directory: &File, name: &CStr) -> Result<File, LayoutError> {
    rustix::fs::openat(
        directory,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)
    .map_err(LayoutError::Io)
}

#[cfg(not(target_os = "linux"))]
fn pin_entry_name(directory: &File, name: &CStr) -> Result<File, LayoutError> {
    rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)
    .map_err(LayoutError::Io)
}

#[cfg(target_os = "linux")]
fn link_open_file(
    file: &File,
    directory: &File,
    _temporary_name: &str,
    final_name: &str,
) -> rustix::io::Result<()> {
    let descriptor_path = format!("/proc/self/fd/{}", file.as_raw_fd());
    match rustix::fs::linkat(
        rustix::fs::CWD,
        descriptor_path,
        directory,
        final_name,
        AtFlags::SYMLINK_FOLLOW,
    ) {
        Ok(()) => Ok(()),
        Err(error) if error == rustix::io::Errno::NOENT => {
            rustix::fs::linkat(file, "", directory, final_name, AtFlags::EMPTY_PATH)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn link_open_file(
    _file: &File,
    directory: &File,
    temporary_name: &str,
    final_name: &str,
) -> rustix::io::Result<()> {
    rustix::fs::linkat(
        directory,
        temporary_name,
        directory,
        final_name,
        AtFlags::empty(),
    )
}

fn create_regular_at(
    directory: &File,
    name: &str,
    access: OFlags,
    mode: Mode,
) -> Result<File, LayoutError> {
    let name = CString::new(name).map_err(|_error| {
        LayoutError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "layout leaf contains a NUL byte",
        ))
    })?;
    create_regular_name_at(directory, &name, access, mode)
}

fn create_regular_name_at(
    directory: &File,
    name: &CStr,
    access: OFlags,
    mode: Mode,
) -> Result<File, LayoutError> {
    rustix::fs::openat(
        directory,
        name,
        access | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )
    .map(File::from)
    .map_err(errno_to_io)
    .map_err(LayoutError::Io)
}

fn open_or_create_regular(
    directory: &File,
    name: &str,
    access: OFlags,
    mode: Mode,
) -> Result<(File, bool), LayoutError> {
    match open_regular_at(directory, name, access) {
        Ok(file) => Ok((file, false)),
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            match create_regular_at(directory, name, access, mode) {
                Ok(file) => Ok((file, true)),
                Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open_regular_at(directory, name, access).map(|file| (file, false))
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one descriptor-relative transaction pins, renames, syncs, and verifies exact evidence"
)]
fn quarantine_entry(
    root: &DataRoot,
    path: &EntryPath,
    source: PathIdentity,
    reason: QuarantineReason,
) -> QuarantineOutcome {
    let retained = |failure| QuarantineOutcome {
        reason,
        source,
        status: QuarantineStatus::Retained { failure },
    };
    let source_parent = match open_entry_parent(root, path.parent) {
        Ok(parent) => parent,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return QuarantineOutcome {
                reason,
                source,
                status: QuarantineStatus::Missing,
            };
        }
        Err(error) => {
            return retained(layout_failure(
                QuarantineFailureStage::SourceChanged,
                &error,
            ));
        }
    };
    let observed_stat = match stat_no_follow_name(&source_parent, &path.name) {
        Ok(stat) => stat,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return QuarantineOutcome {
                reason,
                source,
                status: QuarantineStatus::Missing,
            };
        }
        Err(error) => {
            return retained(layout_failure(
                QuarantineFailureStage::SourceChanged,
                &error,
            ));
        }
    };
    let observed = path_identity(path.parent, &path.name, &observed_stat);
    if observed != source {
        return QuarantineOutcome {
            reason,
            source,
            status: QuarantineStatus::Changed {
                observed: Some(observed),
            },
        };
    }
    let pinned = match pin_entry_name(&source_parent, &path.name) {
        Ok(file) => file,
        Err(error) => {
            return retained(layout_failure(
                QuarantineFailureStage::SourceChanged,
                &error,
            ));
        }
    };
    let pinned_identity = match FileIdentity::from_file(&pinned) {
        Ok(identity) => identity,
        Err(error) => {
            return retained(quarantine_failure(
                QuarantineFailureStage::SourceChanged,
                &error,
            ));
        }
    };
    if pinned_identity != source.file {
        return QuarantineOutcome {
            reason,
            source,
            status: QuarantineStatus::Changed {
                observed: Some(path_identity_from_file(
                    path.parent.scope(),
                    &path.name,
                    source.file_type,
                    pinned_identity,
                )),
            },
        };
    }
    let quarantine = match ensure_quarantine_directory(root) {
        Ok(directory) => directory,
        Err(failure) => return retained(failure),
    };

    for slot in 0..MAX_QUARANTINE_COLLISION_SLOTS {
        let destination_name = quarantine_name(reason, source, slot);
        let Ok(destination) = CString::new(destination_name) else {
            return retained(QuarantineFailure::local(
                QuarantineFailureStage::QuarantineDirectory,
                io::ErrorKind::InvalidInput,
            ));
        };
        match rename_noreplace(&source_parent, &path.name, &quarantine, &destination) {
            Ok(()) => {
                let source_sync = sync_source_directory(&source_parent);
                let quarantine_sync = sync_quarantine_directory(&quarantine);
                let destination_stat = stat_no_follow_name(&quarantine, &destination);
                let destination_identity = destination_stat.as_ref().map_or_else(
                    |_error| {
                        path_identity_from_file(
                            EntryScope::Quarantine,
                            &destination,
                            source.file_type,
                            source.file,
                        )
                    },
                    |stat| PathIdentity {
                        scope: EntryScope::Quarantine,
                        ..path_identity(EntryParent::Root, &destination, stat)
                    },
                );
                let verification_failure = match destination_stat {
                    Ok(stat) => {
                        let observed_type = entry_file_type(FileType::from_raw_mode(stat.st_mode));
                        let pinned_after = FileIdentity::from_file(&pinned);
                        let named_after = FileIdentity::from_stat(&stat);
                        match pinned_after {
                            Ok(pinned_after)
                                if same_object_after_rename(source.file, named_after)
                                    && same_object_after_rename(source.file, pinned_after)
                                    && observed_type == source.file_type =>
                            {
                                None
                            }
                            Ok(_) => Some(QuarantineFailure::local(
                                QuarantineFailureStage::IdentityVerification,
                                io::ErrorKind::InvalidData,
                            )),
                            Err(error) => Some(quarantine_failure(
                                QuarantineFailureStage::IdentityVerification,
                                &error,
                            )),
                        }
                    }
                    Err(error) => Some(layout_failure(
                        QuarantineFailureStage::IdentityVerification,
                        &error,
                    )),
                };
                let failure = source_sync
                    .err()
                    .map(|error| {
                        quarantine_failure(QuarantineFailureStage::SourceDirectorySync, &error)
                    })
                    .or_else(|| {
                        quarantine_sync.err().map(|error| {
                            quarantine_failure(
                                QuarantineFailureStage::QuarantineDirectorySync,
                                &error,
                            )
                        })
                    })
                    .or(verification_failure);
                return QuarantineOutcome {
                    reason,
                    source,
                    status: failure.map_or(
                        QuarantineStatus::Quarantined {
                            destination: destination_identity,
                        },
                        |failure| QuarantineStatus::QuarantinedDegraded {
                            destination: destination_identity,
                            failure,
                        },
                    ),
                };
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return retained(quarantine_failure(QuarantineFailureStage::Rename, &error));
            }
        }
    }
    retained(QuarantineFailure::local(
        QuarantineFailureStage::CollisionSlotsExhausted,
        io::ErrorKind::AlreadyExists,
    ))
}

fn root_generation_nonce(identity: FileIdentity) -> u64 {
    let sequence = NEXT_ROOT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let mut nonce = identity.device.rotate_left(7)
        ^ identity.inode.rotate_left(23)
        ^ identity.len.rotate_left(41)
        ^ u64::from(std::process::id())
        ^ sequence;
    nonce ^= u64::from_ne_bytes(identity.ctime_seconds.to_ne_bytes()).rotate_left(13);
    nonce ^= u64::from_ne_bytes(identity.ctime_nanoseconds.to_ne_bytes()).rotate_left(37);
    nonce
}

fn generation_name(nonce: u64, slot: u8) -> String {
    format!("{ROOT_GENERATION_PREFIX}{nonce:016x}.{slot:02x}{ROOT_GENERATION_SUFFIX}")
}

fn evidence_name(nonce: u64, slot: u8) -> String {
    format!("{ROOT_EVIDENCE_PREFIX}{nonce:016x}.{slot:02x}{ROOT_EVIDENCE_SUFFIX}")
}

fn available_evidence_name(root: &File, nonce: u64, _current_name: &CStr) -> Option<CString> {
    (0..MAX_ROOT_COLLISION_SLOTS).find_map(|slot| {
        let candidate = CString::new(evidence_name(nonce, slot)).ok()?;
        match stat_no_follow_name(root, &candidate) {
            Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                Some(candidate)
            }
            Ok(_) | Err(_) => None,
        }
    })
}

fn verify_active_evidence(
    root: &DataRoot,
    evidence: &EvidenceFile,
) -> Result<(), QuarantineFailure> {
    let pinned = FileIdentity::from_file(&evidence.file)
        .map_err(|error| quarantine_failure(QuarantineFailureStage::SourceChanged, &error))?;
    if pinned != evidence.identity.file {
        return Err(QuarantineFailure::local(
            QuarantineFailureStage::SourceChanged,
            io::ErrorKind::InvalidData,
        ));
    }
    let stat = stat_no_follow(&root.directory, ACTIVE_JOURNAL_NAME)
        .map_err(|error| layout_failure(QuarantineFailureStage::SourceChanged, &error))?;
    let named = FileIdentity::from_stat(&stat);
    if entry_file_type(FileType::from_raw_mode(stat.st_mode)) != EntryFileType::RegularFile
        || named != evidence.identity.file
    {
        return Err(QuarantineFailure::local(
            QuarantineFailureStage::SourceChanged,
            io::ErrorKind::InvalidData,
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn exchange_root_names(root: &File, first: &str, second: &CStr) -> io::Result<()> {
    quarantine_failpoint!(Exchange);
    rustix::fs::renameat_with(root, first, root, second, RenameFlags::EXCHANGE).map_err(errno_to_io)
}

#[cfg(not(target_os = "linux"))]
fn exchange_root_names(_root: &File, _first: &str, _second: &CStr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic exchange rename is unsupported on this platform",
    ))
}

fn rename_generation_to_active(root: &File, generation: &CStr) -> io::Result<()> {
    rename_noreplace(root, generation, root, c"active.parts")
}

fn open_entry_parent(root: &DataRoot, parent: EntryParent) -> Result<File, LayoutError> {
    match parent {
        EntryParent::Root => root.directory.try_clone().map_err(LayoutError::Io),
        EntryParent::Year(year) => open_directory_at(&root.directory, &format!("{year:04}")),
        EntryParent::Month { year, month } => {
            let year = open_directory_at(&root.directory, &format!("{year:04}"))?;
            open_directory_at(&year, &format!("{month:02}"))
        }
        EntryParent::Day(day) => root.open_day(day),
    }
}

fn ensure_quarantine_directory(root: &DataRoot) -> Result<File, QuarantineFailure> {
    match rustix::fs::mkdirat(
        &*root.directory,
        QUARANTINE_DIRECTORY_NAME,
        QUARANTINE_DIRECTORY_MODE,
    ) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => {
            let error = errno_to_io(error);
            return Err(quarantine_failure(
                QuarantineFailureStage::QuarantineDirectory,
                &error,
            ));
        }
    }
    let directory = open_directory_at(&root.directory, QUARANTINE_DIRECTORY_NAME)
        .map_err(|error| layout_failure(QuarantineFailureStage::QuarantineDirectory, &error))?;
    rustix::fs::fchmod(&directory, QUARANTINE_DIRECTORY_MODE)
        .map_err(errno_to_io)
        .map_err(|error| quarantine_failure(QuarantineFailureStage::QuarantineDirectory, &error))?;
    directory
        .sync_all()
        .map_err(|error| quarantine_failure(QuarantineFailureStage::QuarantineDirectory, &error))?;
    sync_quarantine_entry(&root.directory).map_err(|error| {
        quarantine_failure(QuarantineFailureStage::QuarantineDirectoryEntrySync, &error)
    })?;
    Ok(directory)
}

fn layout_failure(stage: QuarantineFailureStage, error: &LayoutError) -> QuarantineFailure {
    match error {
        LayoutError::Io(error) => quarantine_failure(stage, error),
        _ => QuarantineFailure::local(stage, io::ErrorKind::InvalidData),
    }
}

fn quarantine_name(reason: QuarantineReason, source: PathIdentity, slot: u8) -> String {
    format!(
        "qv1-{:02x}-{:016x}-{:016x}-{:016x}-{slot:02x}",
        reason as u8, source.name_hash, source.file.device, source.file.inode
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedQuarantineName {
    reason: QuarantineReason,
    device: u64,
    inode: u64,
}

fn parse_quarantine_name(name: &str) -> Option<ParsedQuarantineName> {
    let mut fields = name.split('-');
    if fields.next()? != "qv1" {
        return None;
    }
    let reason = parse_lower_hex(fields.next()?, 2)?;
    let _name_hash = parse_lower_hex(fields.next()?, 16)?;
    let device = parse_lower_hex(fields.next()?, 16)?;
    let inode = parse_lower_hex(fields.next()?, 16)?;
    let slot = parse_lower_hex(fields.next()?, 2)?;
    if fields.next().is_some() || slot >= u64::from(MAX_QUARANTINE_COLLISION_SLOTS) {
        return None;
    }
    Some(ParsedQuarantineName {
        reason: QuarantineReason::from_code(u8::try_from(reason).ok()?)?,
        device,
        inode,
    })
}

fn parse_lower_hex(field: &str, expected_len: usize) -> Option<u64> {
    if field.len() != expected_len
        || !field
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    u64::from_str_radix(field, 16).ok()
}

#[cfg(target_os = "linux")]
fn rename_noreplace(
    source_directory: &File,
    source_name: &CStr,
    destination_directory: &File,
    destination_name: &CStr,
) -> io::Result<()> {
    quarantine_failpoint!(Rename);
    rustix::fs::renameat_with(
        source_directory,
        source_name,
        destination_directory,
        destination_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(errno_to_io)
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(
    _source_directory: &File,
    _source_name: &CStr,
    _destination_directory: &File,
    _destination_name: &CStr,
) -> io::Result<()> {
    quarantine_failpoint!(Rename);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unsupported on this platform",
    ))
}

fn sync_quarantine_entry(root: &File) -> io::Result<()> {
    quarantine_failpoint!(DirectorySync);
    root.sync_all()
}

fn sync_source_directory(directory: &File) -> io::Result<()> {
    quarantine_failpoint!(SourceDirectorySync);
    directory.sync_all()
}

fn sync_quarantine_directory(directory: &File) -> io::Result<()> {
    quarantine_failpoint!(QuarantineDirectorySync);
    directory.sync_all()
}

const fn same_object_after_rename(expected: FileIdentity, observed: FileIdentity) -> bool {
    expected.device == observed.device
        && expected.inode == observed.inode
        && expected.len == observed.len
        && expected.mtime_seconds == observed.mtime_seconds
        && expected.mtime_nanoseconds == observed.mtime_nanoseconds
}

fn remove_verified_regular(
    root: &DataRoot,
    address: SegmentAddress,
    name: &str,
) -> Result<(), LayoutError> {
    let day = root.open_day(address.day)?;
    let stat = stat_no_follow(&day, name)?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind == FileType::Symlink {
        return Err(LayoutError::SymlinkNotAllowed {
            name: name.to_owned(),
        });
    }
    if kind != FileType::RegularFile {
        return Err(LayoutError::UnexpectedLeafEntryType {
            name: name.to_owned(),
        });
    }
    rustix::fs::unlinkat(&day, name, AtFlags::empty())
        .map_err(errno_to_io)
        .map_err(LayoutError::Io)?;
    day.sync_all()?;
    Ok(())
}

/// Unlinks a regular leaf and reports the bytes it held.
///
/// A missing name returns `Ok(None)`; the day directory is not synchronized
/// here so a caller removing several siblings can batch one `sync_all`.
fn unlink_regular_capturing_size(directory: &File, name: &str) -> Result<Option<u64>, LayoutError> {
    let stat = match stat_no_follow(directory, name) {
        Ok(stat) => stat,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let kind = FileType::from_raw_mode(stat.st_mode);
    if kind == FileType::Symlink {
        return Err(LayoutError::SymlinkNotAllowed {
            name: name.to_owned(),
        });
    }
    if kind != FileType::RegularFile {
        return Err(LayoutError::UnexpectedLeafEntryType {
            name: name.to_owned(),
        });
    }
    let bytes = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    rustix::fs::unlinkat(directory, name, AtFlags::empty())
        .map_err(errno_to_io)
        .map_err(LayoutError::Io)?;
    Ok(Some(bytes))
}

fn verify_named_identity(
    directory: &File,
    file_name: &str,
    expected: FileIdentity,
    temporary_name: &str,
) -> Result<(), LayoutError> {
    let file = open_regular_at(directory, file_name, OFlags::RDONLY)?;
    if !FileIdentity::from_file(&file)?.same_named_object(expected) {
        return Err(LayoutError::TemporaryChanged {
            name: temporary_name.to_owned(),
        });
    }
    Ok(())
}

fn unlink_named_if_identity(
    directory: &File,
    name: &str,
    expected: FileIdentity,
) -> Result<bool, LayoutError> {
    let named = match open_regular_at(directory, name, OFlags::RDONLY) {
        Ok(file) => file,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if !FileIdentity::from_file(&named)?.same_named_object(expected) {
        return Ok(false);
    }
    rustix::fs::unlinkat(directory, name, AtFlags::empty())
        .map_err(errno_to_io)
        .map_err(LayoutError::Io)?;
    Ok(true)
}

fn remove_regular_if_identity(
    root: &DataRoot,
    address: SegmentAddress,
    name: &str,
    device: u64,
    inode: u64,
) -> Result<bool, LayoutError> {
    let day = root.open_day(address.day)?;
    let file = match open_regular_at(&day, name, OFlags::RDONLY) {
        Ok(file) => file,
        Err(LayoutError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if metadata.dev() != device || metadata.ino() != inode {
        return Ok(false);
    }
    rustix::fs::unlinkat(&day, name, AtFlags::empty())
        .map_err(errno_to_io)
        .map_err(LayoutError::Io)?;
    day.sync_all()?;
    Ok(true)
}

fn remove_empty_directory_at(parent: &File, name: &str) -> Result<bool, LayoutError> {
    match rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
        Ok(()) => {
            parent.sync_all()?;
            Ok(true)
        }
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTEMPTY | rustix::io::Errno::EXIST) => {
            Ok(false)
        }
        Err(error) => Err(LayoutError::Io(errno_to_io(error))),
    }
}

fn temporary_name(address: SegmentAddress, kind: TemporaryKind) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    match kind {
        TemporaryKind::Pgm => format!("{}.pgm.{pid}.{sequence}.tmp", address.id),
        TemporaryKind::Ovf => format!("{}.ovf.{pid}.{sequence}.tmp", address.id),
        TemporaryKind::OverviewProbe => {
            format!("{}.ovf.probe.{pid}.{sequence}.tmp", address.id)
        }
    }
}

fn errno_to_layout(error: rustix::io::Errno) -> LayoutError {
    LayoutError::Io(errno_to_io(error))
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use std::fs::{FileTimes, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::{FileExt as _, symlink};
    use std::time::SystemTime;

    use super::*;

    fn address(value: i64) -> SegmentAddress {
        SegmentAddress::new(SegmentId::new(value).unwrap()).unwrap()
    }

    fn rewrite_same_inode_with_restored_mtime(
        path: &Path,
        prepared_identity: FileIdentity,
        prepared_mtime: SystemTime,
        replacement: &[u8],
    ) -> FileIdentity {
        assert_eq!(replacement.len() as u64, prepared_identity.len);
        let rewritten = OpenOptions::new().write(true).open(path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            rewritten.write_all_at(replacement, 0).unwrap();
            rewritten
                .set_times(FileTimes::new().set_modified(prepared_mtime))
                .unwrap();
            rewritten.sync_all().unwrap();
            let identity = FileIdentity::from_file(&rewritten).unwrap();
            if (identity.ctime_seconds, identity.ctime_nanoseconds)
                != (
                    prepared_identity.ctime_seconds,
                    prepared_identity.ctime_nanoseconds,
                )
            {
                return identity;
            }
            assert!(
                Instant::now() < deadline,
                "the filesystem did not expose the same-inode rewrite through ctime"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn canonical_quarantine_name_roundtrips_forensic_fields() {
        let file = tempfile::tempfile().unwrap();
        let identity = FileIdentity::from_file(&file).unwrap();
        let source = path_identity_from_file(
            EntryScope::Root,
            c"active.parts",
            EntryFileType::RegularFile,
            identity,
        );
        let name = quarantine_name(QuarantineReason::CorruptActiveJournal, source, 7);
        let parsed = parse_quarantine_name(&name).expect("canonical quarantine name");

        assert_eq!(parsed.reason, QuarantineReason::CorruptActiveJournal);
        assert_eq!(parsed.device, identity.device);
        assert_eq!(parsed.inode, identity.inode);
        assert_eq!(parse_quarantine_name(&name.to_uppercase()), None);
    }

    #[test]
    fn strict_scan_sorts_numeric_ids_and_associates_overviews() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let later = address(1_709_164_802_000_000);
        let earlier = address(1_709_164_801_000_000);
        for item in [later, earlier] {
            let mut temp = owner.create_pgm_temp(item).unwrap();
            temp.file_mut().write_all(b"PGM").unwrap();
            temp.publish().unwrap();
        }

        let snapshot = root.scan(LayoutLimits::default()).unwrap();
        assert_eq!(
            snapshot
                .segments
                .iter()
                .map(|segment| segment.address.id)
                .collect::<Vec<_>>(),
            vec![earlier.id, later.id]
        );
    }

    #[test]
    fn remove_sealed_segment_unlinks_the_pgm_and_reports_freed_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let older = address(1_709_164_801_000_000);
        let newer = address(1_709_164_802_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        for item in [older, newer] {
            let mut temp = writer.create_pgm_temp(item).unwrap();
            temp.file_mut().write_all(b"PGMBODY").unwrap();
            temp.publish().unwrap();
        }

        let removal = writer.remove_sealed_segment(older).unwrap();
        assert_eq!(removal.pgm_bytes, b"PGMBODY".len() as u64);
        assert_eq!(removal.ovf_bytes, None, "no sibling overview was present");

        let snapshot = root.scan(LayoutLimits::default()).unwrap();
        assert_eq!(
            snapshot
                .segments
                .iter()
                .map(|segment| segment.address.id)
                .collect::<Vec<_>>(),
            vec![newer.id],
            "only the newer segment survives"
        );
    }

    #[test]
    fn remove_sealed_segment_frees_nothing_when_it_is_already_gone() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let older = address(1_709_164_801_000_000);
        let keeper = address(1_709_164_802_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        for item in [older, keeper] {
            let mut temp = writer.create_pgm_temp(item).unwrap();
            temp.file_mut().write_all(b"PGM").unwrap();
            temp.publish().unwrap();
        }
        writer.remove_sealed_segment(older).unwrap();

        let second = writer.remove_sealed_segment(older).unwrap();
        assert_eq!(second.total_bytes(), 0, "a repeated removal frees nothing");
    }

    #[test]
    fn remove_quarantine_entry_frees_bytes_once_and_rechecks_identity() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        std::fs::write(directory.path().join("junk.bin"), b"JUNK").unwrap();
        let snapshot = root.scan(LayoutLimits::default()).unwrap();
        assert_eq!(snapshot.foreign_entries.len(), 1);
        for foreign in &snapshot.foreign_entries {
            let outcome = writer.quarantine_foreign(foreign);
            assert!(
                matches!(outcome.status, QuarantineStatus::Quarantined { .. }),
                "the fixture entry must reach quarantine, got {:?}",
                outcome.status
            );
        }

        let entries = root.scan_quarantine(LayoutLimits::default()).unwrap();
        assert_eq!(entries.len(), 1);
        let freed = writer.remove_quarantine_entry(&entries[0]).unwrap();
        assert_eq!(freed, b"JUNK".len() as u64);
        let repeat = writer.remove_quarantine_entry(&entries[0]).unwrap();
        assert_eq!(repeat, 0, "a repeated removal frees nothing");
        assert!(
            root.scan_quarantine(LayoutLimits::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn segment_removal_total_sums_the_pgm_and_overview() {
        assert_eq!(
            SegmentRemoval {
                pgm_bytes: 100,
                ovf_bytes: Some(30),
            }
            .total_bytes(),
            130
        );
        assert_eq!(
            SegmentRemoval {
                pgm_bytes: 100,
                ovf_bytes: None,
            }
            .total_bytes(),
            100
        );
    }

    #[test]
    fn overview_owner_prunes_empty_calendar_ancestors_bottom_up() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let address = address(1_709_164_801_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        let temp = writer.create_pgm_temp(address).unwrap();
        temp.discard().unwrap();
        drop(writer);
        assert!(directory.path().join(address.day.year_component()).is_dir());

        let overview = root.acquire_overview(LayoutLimits::default()).unwrap();
        overview.prune_empty_day(address.day).unwrap();

        assert!(!directory.path().join(address.day.year_component()).exists());
    }

    #[test]
    fn overview_does_not_prune_a_day_while_the_writer_owns_the_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let address = address(1_709_164_801_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        let temp = writer.create_pgm_temp(address).unwrap();
        temp.discard().unwrap();
        let overview = root.acquire_overview(LayoutLimits::default()).unwrap();

        overview.prune_empty_day(address.day).unwrap();

        assert!(directory.path().join(address.day.year_component()).is_dir());
        drop(writer);
        overview.prune_empty_day(address.day).unwrap();
        assert!(!directory.path().join(address.day.year_component()).exists());
    }

    #[test]
    fn flat_segment_is_excluded_without_reading_it() {
        for name in ["1000.pgm", "1000.ovf"] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join(name), b"not a container").unwrap();
            let root = DataRoot::open(directory.path()).unwrap();
            let snapshot = root.scan(LayoutLimits::default()).unwrap();
            assert!(snapshot.segments.is_empty());
            assert_eq!(snapshot.foreign_entries.len(), 1);
            assert_eq!(
                snapshot.foreign_entries[0].diagnostic().reason,
                ForeignEntryReason::UnsupportedFlatArtifact
            );
        }
    }

    #[test]
    fn symlinked_calendar_component_is_excluded_without_following() {
        let directory = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        symlink(target.path(), directory.path().join("2024")).unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let snapshot = root.scan(LayoutLimits::default()).unwrap();
        assert!(snapshot.days.is_empty());
        assert_eq!(snapshot.foreign_entries.len(), 1);
        assert_eq!(
            snapshot.foreign_entries[0].diagnostic().reason,
            ForeignEntryReason::SymbolicLink
        );
    }

    #[test]
    fn symlinks_are_excluded_at_month_day_and_leaf_levels() {
        for level in ["month", "day", "leaf"] {
            let directory = tempfile::tempdir().unwrap();
            let target = tempfile::tempdir().unwrap();
            match level {
                "month" => {
                    std::fs::create_dir(directory.path().join("2024")).unwrap();
                    symlink(target.path(), directory.path().join("2024/02")).unwrap();
                }
                "day" => {
                    std::fs::create_dir_all(directory.path().join("2024/02")).unwrap();
                    symlink(target.path(), directory.path().join("2024/02/29")).unwrap();
                }
                "leaf" => {
                    let day = directory.path().join("2024/02/29");
                    std::fs::create_dir_all(&day).unwrap();
                    let target_file = target.path().join("segment");
                    std::fs::write(&target_file, b"PGM").unwrap();
                    symlink(&target_file, day.join("1709164800000000.pgm")).unwrap();
                }
                _ => unreachable!(),
            }
            let root = DataRoot::open(directory.path()).unwrap();
            let snapshot = root.scan(LayoutLimits::default()).unwrap();
            assert!(
                snapshot.segments.is_empty(),
                "{level} symlink must not become a segment"
            );
            assert_eq!(snapshot.foreign_entries.len(), 1);
            assert_eq!(
                snapshot.foreign_entries[0].diagnostic().reason,
                ForeignEntryReason::SymbolicLink
            );
        }
    }

    #[test]
    fn noncanonical_segment_names_are_excluded() {
        for name in ["+1.pgm", "01.pgm", "-0.pgm"] {
            let directory = tempfile::tempdir().unwrap();
            let day = directory.path().join("1970/01/01");
            std::fs::create_dir_all(&day).unwrap();
            std::fs::write(day.join(name), b"PGM").unwrap();
            let root = DataRoot::open(directory.path()).unwrap();
            let snapshot = root.scan(LayoutLimits::default()).unwrap();
            assert!(snapshot.segments.is_empty());
            assert_eq!(snapshot.foreign_entries.len(), 1);
            assert_eq!(
                snapshot.foreign_entries[0].diagnostic().reason,
                ForeignEntryReason::UnsupportedName,
                "{name} must not alias a canonical SegmentId"
            );
        }
    }

    #[test]
    fn a_day_with_more_than_192_segments_is_valid_when_within_explicit_limits() {
        let directory = tempfile::tempdir().unwrap();
        let day = directory.path().join("2024/02/29");
        std::fs::create_dir_all(&day).unwrap();
        let midnight = 1_709_164_800_000_000_i64;
        for offset in 0..256_i64 {
            std::fs::write(day.join(format!("{}.pgm", midnight + offset)), b"PGM").unwrap();
        }

        let snapshot = DataRoot::open(directory.path())
            .unwrap()
            .scan(LayoutLimits::default())
            .unwrap();
        assert_eq!(snapshot.segments.len(), 256);
        assert_eq!(snapshot.days, vec![UtcDay::new(2024, 2, 29).unwrap()]);
    }

    #[test]
    fn misbucketed_segment_is_excluded() {
        let directory = tempfile::tempdir().unwrap();
        let day = directory.path().join("2024/02/28");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("1709164800000000.pgm"), b"PGM").unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let snapshot = root.scan(LayoutLimits::default()).unwrap();
        assert!(snapshot.segments.is_empty());
        assert_eq!(snapshot.foreign_entries.len(), 1);
        assert_eq!(
            snapshot.foreign_entries[0].diagnostic().reason,
            ForeignEntryReason::MisbucketedSegment
        );
    }

    #[test]
    fn traversal_returns_no_partial_result_at_a_limit() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        for value in [1_709_164_801_000_000, 1_709_164_802_000_000] {
            let mut temp = owner.create_pgm_temp(address(value)).unwrap();
            temp.file_mut().write_all(b"PGM").unwrap();
            temp.publish().unwrap();
        }
        let limits = LayoutLimits {
            max_segments: 1,
            ..LayoutLimits::default()
        };
        assert!(matches!(
            root.scan(limits),
            Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::Segments,
                ..
            })
        ));
    }

    #[test]
    fn visited_entry_limit_accepts_the_boundary_and_rejects_the_next_entry() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("2024/01/01")).unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let exact = LayoutLimits {
            max_visited_entries: 3,
            ..LayoutLimits::default()
        };
        assert_eq!(root.scan(exact).unwrap().visited_entries, 3);

        let below = LayoutLimits {
            max_visited_entries: 2,
            ..LayoutLimits::default()
        };
        assert!(matches!(
            root.scan(below),
            Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::VisitedEntries,
                limit: 2,
            })
        ));
    }

    #[test]
    fn entries_per_day_limit_accepts_the_boundary_and_rejects_the_next_entry() {
        let directory = tempfile::tempdir().unwrap();
        let day = directory.path().join("2024/02/29");
        std::fs::create_dir_all(&day).unwrap();
        let midnight = 1_709_164_800_000_000_i64;
        for offset in 0..2_i64 {
            std::fs::write(day.join(format!("{}.pgm", midnight + offset)), b"PGM").unwrap();
        }
        let root = DataRoot::open(directory.path()).unwrap();
        let exact = LayoutLimits {
            max_entries_per_day: 2,
            ..LayoutLimits::default()
        };
        assert_eq!(root.scan(exact).unwrap().segments.len(), 2);

        let below = LayoutLimits {
            max_entries_per_day: 1,
            ..LayoutLimits::default()
        };
        assert!(matches!(
            root.scan(below),
            Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::EntriesPerDay,
                limit: 1,
            })
        ));
    }

    #[test]
    fn metadata_limit_accepts_the_boundary_and_rejects_the_next_byte() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("2024")).unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let exact_bytes = ENTRY_METADATA_BYTES + "2024".len();
        let exact = LayoutLimits {
            max_metadata_bytes: exact_bytes,
            ..LayoutLimits::default()
        };
        assert_eq!(root.scan(exact).unwrap().metadata_bytes, exact_bytes);

        let below = LayoutLimits {
            max_metadata_bytes: exact_bytes - 1,
            ..LayoutLimits::default()
        };
        assert!(matches!(
            root.scan(below),
            Err(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::MetadataBytes,
                limit,
            }) if limit == exact_bytes - 1
        ));
    }

    #[test]
    fn one_writer_owner_is_enforced() {
        let directory = tempfile::tempdir().unwrap();
        let first_root = DataRoot::open(directory.path()).unwrap();
        let second_root = DataRoot::open(directory.path()).unwrap();
        let _first = first_root.acquire_writer(LayoutLimits::default()).unwrap();
        assert!(matches!(
            second_root.acquire_writer(LayoutLimits::default()),
            Err(LayoutError::OwnerContended {
                owner: OwnerKind::Writer
            })
        ));
    }

    #[test]
    fn existing_owner_lock_retries_root_sync_after_creation_sync_failure() {
        const FIRST_ERROR: i32 = 5;
        const RETRY_ERROR: i32 = 116;

        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let lock_path = directory.path().join(WRITER_OWNER_LOCK_NAME);

        let first_fault = arm_owner_lock_sync_fault(FIRST_ERROR);
        assert!(matches!(
            root.acquire_lock(WRITER_OWNER_LOCK_NAME, OwnerKind::Writer),
            Err(LayoutError::Io(ref error)) if error.raw_os_error() == Some(FIRST_ERROR)
        ));
        first_fault.assert_consumed();
        assert!(
            lock_path.is_file(),
            "the lock inode was initialized before root sync failed"
        );

        let retry_fault = arm_owner_lock_sync_fault(RETRY_ERROR);
        assert!(matches!(
            root.acquire_lock(WRITER_OWNER_LOCK_NAME, OwnerKind::Writer),
            Err(LayoutError::Io(ref error)) if error.raw_os_error() == Some(RETRY_ERROR)
        ));
        retry_fault.assert_consumed();

        let _lock = root
            .acquire_lock(WRITER_OWNER_LOCK_NAME, OwnerKind::Writer)
            .expect("a later retry synchronizes and locks the existing inode");
    }

    #[test]
    fn cloned_writer_lease_keeps_exclusive_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let first_root = DataRoot::open(directory.path()).unwrap();
        let second_root = DataRoot::open(directory.path()).unwrap();
        let owner = first_root.acquire_writer(LayoutLimits::default()).unwrap();
        let lease = owner.try_clone_lease().unwrap();
        drop(owner);

        assert!(matches!(
            second_root.acquire_writer(LayoutLimits::default()),
            Err(LayoutError::OwnerContended {
                owner: OwnerKind::Writer
            })
        ));

        drop(lease);
        second_root.acquire_writer(LayoutLimits::default()).unwrap();
    }

    #[test]
    fn existing_calendar_component_retries_parent_sync_after_a_failed_creation_sync() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let address = address(1_709_164_801_000_000);
        let year = directory.path().join(address.day.year_component());

        let first_fault = arm_ensure_directory_sync_fault(rustix::io::Errno::IO.raw_os_error());
        assert!(matches!(
            owner.create_pgm_temp(address),
            Err(LayoutError::Io(ref error))
                if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
        ));
        first_fault.assert_consumed();
        assert!(year.is_dir(), "mkdirat completed before the failed sync");

        let retry_fault = arm_ensure_directory_sync_fault(rustix::io::Errno::STALE.raw_os_error());
        assert!(matches!(
            owner.create_pgm_temp(address),
            Err(LayoutError::Io(ref error))
                if error.raw_os_error() == Some(rustix::io::Errno::STALE.raw_os_error())
        ));
        retry_fault.assert_consumed();

        let temporary = owner
            .create_pgm_temp(address)
            .expect("a retry synchronizes the existing year before descending");
        temporary.discard().unwrap();
    }

    #[test]
    fn direct_open_rejects_a_fifo_without_blocking() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let address = address(1_709_164_801_000_000);
        let day = directory
            .path()
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component());
        std::fs::create_dir_all(&day).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(day.join(address.pgm_name()))
                .status()
                .unwrap()
                .success()
        );

        assert!(matches!(
            root.open_pgm(address),
            Err(LayoutError::UnexpectedLeafEntryType { .. })
        ));
    }

    #[test]
    fn pgm_publication_rejects_a_replaced_temporary_name() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let address = address(1_709_164_801_000_000);
        let mut temporary = owner.create_pgm_temp(address).unwrap();
        temporary.file_mut().write_all(b"expected PGM").unwrap();
        let day = directory
            .path()
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component());
        let temporary_name = temporary.temp_name.clone();
        std::fs::remove_file(day.join(&temporary_name)).unwrap();
        std::fs::write(day.join(&temporary_name), b"replacement").unwrap();

        assert!(matches!(
            temporary.publish(),
            Err(LayoutError::TemporaryChanged { .. })
        ));
        drop(temporary);
        assert!(!day.join(address.pgm_name()).exists());
        assert_eq!(
            std::fs::read(day.join(&temporary_name)).unwrap(),
            b"replacement"
        );
    }

    #[test]
    fn pgm_publication_rejects_same_inode_rewrite_with_restored_mtime() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let address = address(1_709_164_801_000_000);
        let mut temporary = owner.create_pgm_temp(address).unwrap();
        temporary.file_mut().write_all(b"expected PGM").unwrap();
        let prepared = temporary.try_clone_file().unwrap();
        let prepared_identity = FileIdentity::from_file(&prepared).unwrap();
        let prepared_mtime = prepared.metadata().unwrap().modified().unwrap();
        drop(prepared);

        let day = directory
            .path()
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component());
        let temporary_path = day.join(&temporary.temp_name);
        let rewritten_identity = rewrite_same_inode_with_restored_mtime(
            &temporary_path,
            prepared_identity,
            prepared_mtime,
            b"tampered PGM",
        );
        assert_eq!(rewritten_identity.device, prepared_identity.device);
        assert_eq!(rewritten_identity.inode, prepared_identity.inode);
        assert_eq!(rewritten_identity.len, prepared_identity.len);
        assert_eq!(
            (
                rewritten_identity.mtime_seconds,
                rewritten_identity.mtime_nanoseconds
            ),
            (
                prepared_identity.mtime_seconds,
                prepared_identity.mtime_nanoseconds
            )
        );

        assert!(matches!(
            temporary.publish(),
            Err(LayoutError::TemporaryChanged { .. })
        ));
        assert!(!day.join(address.pgm_name()).exists());
        assert_eq!(std::fs::read(temporary_path).unwrap(), b"tampered PGM");
    }

    #[test]
    fn injected_pgm_publication_faults_leave_only_reopenable_states() {
        let cases = [
            (
                PgmPublishFaultPoint::FileSync,
                rustix::io::Errno::NOSPC,
                false,
                true,
            ),
            (
                PgmPublishFaultPoint::Link,
                rustix::io::Errno::DQUOT,
                false,
                true,
            ),
            (
                PgmPublishFaultPoint::LinkedDirectorySync,
                rustix::io::Errno::ROFS,
                true,
                true,
            ),
            (
                PgmPublishFaultPoint::TemporaryUnlink,
                rustix::io::Errno::IO,
                true,
                true,
            ),
            (
                PgmPublishFaultPoint::UnlinkedDirectorySync,
                rustix::io::Errno::STALE,
                true,
                false,
            ),
        ];

        for (point, errno, final_exists, temporary_exists) in cases {
            let directory = tempfile::tempdir().unwrap();
            let root = DataRoot::open(directory.path()).unwrap();
            let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
            let address = address(1_709_164_801_000_000);
            let mut temporary = owner.create_pgm_temp(address).unwrap();
            temporary.file_mut().write_all(b"complete PGM").unwrap();
            let temporary_name = temporary.temp_name.clone();
            let final_path = root.diagnostic_file_path(address, FileKind::Pgm);
            let raw_os_error = errno.raw_os_error();
            let fault = arm_pgm_publish_fault(point, raw_os_error);

            let error = temporary
                .publish()
                .expect_err("an injected persistence fault cannot report success");
            assert!(
                matches!(
                    error,
                    LayoutError::Io(ref source)
                        if source.raw_os_error() == Some(raw_os_error)
                ),
                "{point:?} must preserve {errno:?}, got {error:?}"
            );

            // A killed process does not run PgmTemp::drop. Suppress only its
            // cleanup while still closing the descriptor deterministically.
            temporary.published = true;
            drop(temporary);
            drop(fault);
            drop(owner);
            drop(root);

            let reopened = DataRoot::open(directory.path()).unwrap();
            let snapshot = reopened.scan(LayoutLimits::default()).unwrap();
            assert_eq!(
                final_path.exists(),
                final_exists,
                "{point:?} final-name state"
            );
            assert_eq!(
                snapshot
                    .temporaries
                    .iter()
                    .any(|temporary| temporary.file_name() == temporary_name),
                temporary_exists,
                "{point:?} temporary-name state"
            );
            if final_exists {
                assert_eq!(
                    std::fs::read(&final_path).unwrap(),
                    b"complete PGM",
                    "{point:?} exposed a partial final file"
                );
                assert_eq!(snapshot.segments.len(), 1);
            } else {
                assert!(snapshot.segments.is_empty());
            }
        }
    }

    #[test]
    fn quarantine_rename_failure_retains_the_exact_source() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let address = address(1_709_164_801_000_000);
        let mut temporary = owner.create_pgm_temp(address).unwrap();
        temporary.file_mut().write_all(b"damaged PGM").unwrap();
        temporary.publish().unwrap();
        let segment = root
            .scan(LayoutLimits::default())
            .unwrap()
            .segments
            .into_iter()
            .next()
            .unwrap();
        let raw_os_error = rustix::io::Errno::IO.raw_os_error();
        let fault = arm_quarantine_fault(QuarantineFaultPoint::Rename, raw_os_error);

        let outcome = owner.quarantine_invalid_pgm(segment);

        fault.assert_consumed();
        assert!(matches!(
            outcome.status,
            QuarantineStatus::Retained {
                failure: QuarantineFailure {
                    stage: QuarantineFailureStage::Rename,
                    raw_os_error: Some(error),
                    ..
                }
            } if error == raw_os_error
        ));
        assert_eq!(
            std::fs::read(root.diagnostic_file_path(address, FileKind::Pgm)).unwrap(),
            b"damaged PGM"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn forensic_quarantine_scan_lists_and_reopens_canonical_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let owner = root.acquire_writer(LayoutLimits::default()).unwrap();
        let address = address(1_709_164_801_000_000);
        let mut temporary = owner.create_pgm_temp(address).unwrap();
        temporary.file_mut().write_all(b"damaged PGM").unwrap();
        temporary.publish().unwrap();
        let segment = root
            .scan(LayoutLimits::default())
            .unwrap()
            .segments
            .into_iter()
            .next()
            .unwrap();

        let outcome = owner.quarantine_invalid_pgm(segment);
        assert!(matches!(
            outcome.status,
            QuarantineStatus::Quarantined { .. } | QuarantineStatus::QuarantinedDegraded { .. }
        ));

        let entries = root.scan_quarantine(LayoutLimits::default()).unwrap();
        assert_eq!(entries.len(), 1);
        let evidence = &entries[0];
        assert_eq!(evidence.reason(), QuarantineReason::InvalidPgm);
        assert!(evidence.file_name().starts_with("qv1-02-"));
        assert_eq!(evidence.identity().file.len, 11);

        let file = root.open_quarantine(evidence).unwrap();
        let mut bytes = [0_u8; 11];
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"damaged PGM");
    }

    #[test]
    fn prepared_ovf_publishes_under_its_post_rename_identity() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let address = address(1_709_164_801_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        let mut pgm = writer.create_pgm_temp(address).unwrap();
        pgm.file_mut().write_all(b"source PGM").unwrap();
        pgm.publish().unwrap();
        drop(pgm);
        drop(writer);

        let owner = root.acquire_overview(LayoutLimits::default()).unwrap();
        let mut temporary = owner.create_ovf_temp(address).unwrap();
        temporary.file_mut().write_all(b"expected OVF").unwrap();
        drop(temporary.try_clone_file().unwrap());
        temporary.publish().unwrap();

        assert_eq!(
            std::fs::read(root.diagnostic_file_path(address, FileKind::Ovf)).unwrap(),
            b"expected OVF"
        );
    }

    #[test]
    fn ovf_publication_rejects_same_inode_rewrite_with_restored_mtime() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let address = address(1_709_164_801_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        let mut pgm = writer.create_pgm_temp(address).unwrap();
        pgm.file_mut().write_all(b"source PGM").unwrap();
        pgm.publish().unwrap();
        drop(pgm);
        drop(writer);

        let owner = root.acquire_overview(LayoutLimits::default()).unwrap();
        let mut temporary = owner.create_ovf_temp(address).unwrap();
        temporary.file_mut().write_all(b"expected OVF").unwrap();
        let prepared = temporary.try_clone_file().unwrap();
        let prepared_identity = FileIdentity::from_file(&prepared).unwrap();
        let prepared_mtime = prepared.metadata().unwrap().modified().unwrap();
        drop(prepared);

        let day = directory
            .path()
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component());
        let temporary_path = day.join(&temporary.temp_name);
        let rewritten_identity = rewrite_same_inode_with_restored_mtime(
            &temporary_path,
            prepared_identity,
            prepared_mtime,
            b"tampered OVF",
        );
        assert_eq!(rewritten_identity.device, prepared_identity.device);
        assert_eq!(rewritten_identity.inode, prepared_identity.inode);
        assert_eq!(rewritten_identity.len, prepared_identity.len);
        assert_eq!(
            (
                rewritten_identity.mtime_seconds,
                rewritten_identity.mtime_nanoseconds
            ),
            (
                prepared_identity.mtime_seconds,
                prepared_identity.mtime_nanoseconds
            )
        );

        assert!(matches!(
            temporary.publish(),
            Err(LayoutError::TemporaryChanged { .. })
        ));
        assert!(!day.join(address.ovf_name()).exists());
        assert_eq!(
            std::fs::read(day.join(address.pgm_name())).unwrap(),
            b"source PGM"
        );
    }

    #[test]
    fn ovf_publication_rejects_a_replaced_temporary_name() {
        let directory = tempfile::tempdir().unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        let address = address(1_709_164_801_000_000);
        let writer = root.acquire_writer(LayoutLimits::default()).unwrap();
        let mut pgm = writer.create_pgm_temp(address).unwrap();
        pgm.file_mut().write_all(b"source PGM").unwrap();
        pgm.publish().unwrap();
        drop(pgm);
        drop(writer);

        let owner = root.acquire_overview(LayoutLimits::default()).unwrap();
        let mut temporary = owner.create_ovf_temp(address).unwrap();
        temporary.file_mut().write_all(b"expected OVF").unwrap();
        let day = directory
            .path()
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component());
        let temporary_name = temporary.temp_name.clone();
        std::fs::remove_file(day.join(&temporary_name)).unwrap();
        std::fs::write(day.join(&temporary_name), b"replacement").unwrap();

        assert!(matches!(
            temporary.publish(),
            Err(LayoutError::TemporaryChanged { .. })
        ));
        assert!(!day.join(address.ovf_name()).exists());
        assert_eq!(
            std::fs::read(day.join(temporary_name)).unwrap(),
            b"replacement"
        );
    }
}
