use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, Dir, FileType, FlockOperation, Mode, OFlags};

use crate::{LayoutError, LimitKind, OwnerKind, SegmentAddress, SegmentId, UtcDay};

/// Root-level active segment journal.
pub const ACTIVE_JOURNAL_NAME: &str = "active.parts";
/// Permanent process-ownership lock for the collector.
pub const WRITER_OWNER_LOCK_NAME: &str = ".pgkronika-writer.owner.lock";
/// Permanent process-ownership lock for overview publication and GC.
pub const OVERVIEW_OWNER_LOCK_NAME: &str = ".pgkronika-overview.owner.lock";

const HARD_MAX_VISITED_ENTRIES: usize = 4_000_000;
const HARD_MAX_ENTRIES_PER_DAY: usize = 1_000_000;
const HARD_MAX_SEGMENTS: usize = 2_000_000;
const HARD_MAX_METADATA_BYTES: usize = 128 * 1024 * 1024;
const ENTRY_METADATA_BYTES: usize = 128;
const SCAN_RACE_ATTEMPTS: usize = 4;
const WRITER_LOCK_HANDOFF_TIMEOUT: Duration = Duration::from_millis(100);
const DIRECTORY_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::XUSR)
    .union(Mode::RGRP)
    .union(Mode::XGRP);
const DATA_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::RGRP);
const LOCK_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

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
std::thread_local! {
    static PGM_PUBLISH_FAULT: Cell<Option<(PgmPublishFaultPoint, i32)>> =
        const { Cell::new(None) };
    static ENSURE_DIRECTORY_SYNC_FAULT: Cell<Option<i32>> = const { Cell::new(None) };
    static OWNER_LOCK_SYNC_FAULT: Cell<Option<i32>> = const { Cell::new(None) };
}

#[cfg(test)]
struct PgmPublishFaultGuard;

#[cfg(test)]
struct EnsureDirectorySyncFaultGuard;

#[cfg(test)]
struct OwnerLockSyncFaultGuard;

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
    file_name: String,
}

impl TemporaryObject {
    /// Returns the verified leaf file name.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
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
    /// Number of filesystem entries visited.
    pub visited_entries: usize,
    /// Accounted metadata bytes.
    pub metadata_bytes: usize,
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

    /// Performs a closed-grammar, three-level, bounded traversal.
    ///
    /// No partial snapshot is returned when a structural or resource error is
    /// encountered.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutError`] for malformed entries, symbolic links,
    /// incompatible flat files, exhausted limits, or filesystem failures.
    pub fn scan(&self, limits: LayoutLimits) -> Result<LayoutSnapshot, LayoutError> {
        let limits = limits.validate()?;
        let mut visited_entries = 0_usize;
        for attempt in 0..SCAN_RACE_ATTEMPTS {
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
            let name = entry_name(&entry);
            if is_dot(name) {
                continue;
            }
            state.account(name.len())?;
            let name_string = ascii_name(name)?;
            let stat = stat_no_follow(&self.directory, name_string)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            if file_type == FileType::Symlink {
                return Err(LayoutError::SymlinkNotAllowed {
                    name: name_string.to_owned(),
                });
            }
            if is_control_name(name_string) {
                if file_type != FileType::RegularFile {
                    return Err(LayoutError::UnexpectedRootEntryType {
                        name: name_string.to_owned(),
                    });
                }
                continue;
            }
            let root_extension = Path::new(name_string).extension();
            if root_extension.is_some_and(|extension| extension == "pgm" || extension == "ovf") {
                return if file_type == FileType::RegularFile {
                    Err(LayoutError::UnsupportedFlatLayout {
                        name: name_string.to_owned(),
                    })
                } else {
                    Err(LayoutError::UnexpectedRootEntryType {
                        name: name_string.to_owned(),
                    })
                };
            }
            let Some(year) = parse_year(name_string) else {
                return if file_type == FileType::Directory {
                    Err(LayoutError::MalformedYearDirectory {
                        name: name_string.to_owned(),
                    })
                } else {
                    Err(LayoutError::UnexpectedRootEntry {
                        name: name_string.to_owned(),
                    })
                };
            };
            if file_type != FileType::Directory {
                return Err(LayoutError::UnexpectedCalendarEntryType {
                    name: name_string.to_owned(),
                });
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
            let name = entry_name(&entry);
            if is_dot(name) {
                continue;
            }
            state.account(name.len())?;
            let name_string = ascii_name(name)?;
            let stat = stat_no_follow(year_directory, name_string)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let relative = format!("{year:04}/{name_string}");
            if file_type == FileType::Symlink {
                return Err(LayoutError::SymlinkNotAllowed { name: relative });
            }
            let Some(month) = parse_month(name_string) else {
                return Err(LayoutError::MalformedMonthDirectory { name: relative });
            };
            if file_type != FileType::Directory {
                return Err(LayoutError::UnexpectedCalendarEntryType { name: relative });
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
            let name = entry_name(&entry);
            if is_dot(name) {
                continue;
            }
            state.account(name.len())?;
            let name_string = ascii_name(name)?;
            let stat = stat_no_follow(month_directory, name_string)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let relative = format!("{year:04}/{month:02}/{name_string}");
            if file_type == FileType::Symlink {
                return Err(LayoutError::SymlinkNotAllowed { name: relative });
            }
            let day_number = parse_day(year, month, name_string).ok_or_else(|| {
                LayoutError::MalformedDayDirectory {
                    name: relative.clone(),
                }
            })?;
            if file_type != FileType::Directory {
                return Err(LayoutError::UnexpectedCalendarEntryType { name: relative });
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
            let name = entry_name(&entry);
            if is_dot(name) {
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
            state.account(name.len())?;
            let name_string = ascii_name(name)?;
            let stat = stat_no_follow(day_directory, name_string)?;
            let file_type = FileType::from_raw_mode(stat.st_mode);
            let relative = format!("{day}/{name_string}");
            if file_type == FileType::Symlink {
                return Err(LayoutError::SymlinkNotAllowed { name: relative });
            }
            if file_type != FileType::RegularFile {
                return Err(LayoutError::UnexpectedLeafEntryType { name: relative });
            }
            let parsed = parse_leaf(name_string, day).map_err(|error| match error {
                LayoutError::UnexpectedLeafEntry { .. } => {
                    LayoutError::UnexpectedLeafEntry { name: relative }
                }
                other => other,
            })?;
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

    /// Establishes lifetime-exclusive writer ownership after two strict scans.
    ///
    /// # Errors
    ///
    /// Returns a structural error, I/O error, or
    /// [`LayoutError::OwnerContended`].
    pub fn acquire_writer(&self, limits: LayoutLimits) -> Result<WriterOwner, LayoutError> {
        self.scan(limits)?;
        let lock = self.acquire_writer_lock()?;
        self.scan(limits)?;
        Ok(WriterOwner {
            root: self.clone(),
            owner_lock: lock,
        })
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
}

/// Opaque clone of the writer lock held by a long-lived mutation handle.
///
/// The operating-system lock is released only after both the owner and every
/// lease cloned from it have been dropped.
#[derive(Debug)]
pub struct WriterLease {
    _lock: File,
}

impl WriterOwner {
    /// Returns read-only access to the same verified root.
    #[must_use]
    pub const fn root(&self) -> &DataRoot {
        &self.root
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

    /// Creates an OVF temporary and captures the source PGM identity.
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
        let source_identity = FileIdentity::from_file(&source)?;
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
            source_identity,
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
        let year_name = day.year_component();
        let month_name = day.month_component();
        let day_name = day.day_component();
        let year = match open_directory_at(&self.root.directory, &year_name) {
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
        remove_empty_directory_at(&self.root.directory, &year_name)?;
        Ok(())
    }
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
    source_identity: FileIdentity,
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
        if FileIdentity::from_file(&current_source)? != self.source_identity {
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
        }
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
        LayoutSnapshot {
            days: self.days,
            segments: self.segments,
            orphan_overviews: self.orphan_overviews,
            temporaries: self.temporaries,
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

fn entry_name(entry: &rustix::fs::DirEntry) -> &[u8] {
    entry.file_name().to_bytes()
}

fn is_dot(name: &[u8]) -> bool {
    name == b"." || name == b".."
}

fn ascii_name(name: &[u8]) -> Result<&str, LayoutError> {
    if name.is_ascii() {
        std::str::from_utf8(name).map_err(|_error| LayoutError::UnexpectedRootEntry {
            name: String::from_utf8_lossy(name).into_owned(),
        })
    } else {
        Err(LayoutError::UnexpectedRootEntry {
            name: String::from_utf8_lossy(name).into_owned(),
        })
    }
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
                name: name.to_owned(),
            }
        } else {
            LayoutError::Io(errno_to_io(error))
        }
    })?;
    if !file.metadata()?.is_file() {
        return Err(LayoutError::UnexpectedLeafEntryType {
            name: name.to_owned(),
        });
    }
    Ok(file)
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
    fn flat_segment_is_rejected_without_reading_it() {
        for name in ["1000.pgm", "1000.ovf"] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join(name), b"not a container").unwrap();
            let root = DataRoot::open(directory.path()).unwrap();
            assert!(matches!(
                root.scan(LayoutLimits::default()),
                Err(LayoutError::UnsupportedFlatLayout { .. })
            ));
        }
    }

    #[test]
    fn symlinked_calendar_component_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        symlink(target.path(), directory.path().join("2024")).unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        assert!(matches!(
            root.scan(LayoutLimits::default()),
            Err(LayoutError::SymlinkNotAllowed { .. })
        ));
    }

    #[test]
    fn symlinks_are_rejected_at_month_day_and_leaf_levels() {
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
            assert!(
                matches!(
                    root.scan(LayoutLimits::default()),
                    Err(LayoutError::SymlinkNotAllowed { .. })
                ),
                "{level} symlink must fail the complete scan"
            );
        }
    }

    #[test]
    fn noncanonical_segment_names_are_rejected() {
        for name in ["+1.pgm", "01.pgm", "-0.pgm"] {
            let directory = tempfile::tempdir().unwrap();
            let day = directory.path().join("1970/01/01");
            std::fs::create_dir_all(&day).unwrap();
            std::fs::write(day.join(name), b"PGM").unwrap();
            let root = DataRoot::open(directory.path()).unwrap();
            assert!(
                matches!(
                    root.scan(LayoutLimits::default()),
                    Err(LayoutError::UnexpectedLeafEntry { .. })
                ),
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
    fn misbucketed_segment_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let day = directory.path().join("2024/02/28");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("1709164800000000.pgm"), b"PGM").unwrap();
        let root = DataRoot::open(directory.path()).unwrap();
        assert!(matches!(
            root.scan(LayoutLimits::default()),
            Err(LayoutError::MisbucketedSegment { .. })
        ));
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
