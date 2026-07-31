//! Bounded revisioned collector status stored at the data-root boundary.

use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use rustix::fs::{AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};

/// Published collector-status control file.
pub const PRODUCER_STATUS_NAME: &str = "producer-status.json";
/// Same-directory temporary used for atomic replacement.
pub const PRODUCER_STATUS_TEMP_NAME: &str = ".producer-status.json.tmp";

const PRODUCER_STATUS_REVISION: u16 = 1;
const MAX_STATUS_BYTES: u64 = 16 * 1024;

/// Persisted collector lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerState {
    /// The collector published a startup or cycle heartbeat.
    Running,
    /// The collector published a terminal status during orderly shutdown.
    Stopped,
}

/// Configured storage-retention target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    variant_size_differences,
    reason = "fixed bytes and one-byte percentage are intentionally distinct wire variants"
)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RetentionStatus {
    /// Fixed whole-tree byte ceiling.
    Fixed {
        /// Target in bytes; zero is rejected at publication.
        target_bytes: u64,
    },
    /// Maximum used fraction of the backing filesystem.
    Auto {
        /// Used-filesystem percentage in `1..=99`.
        target_percent: u8,
    },
}

impl RetentionStatus {
    /// Builds a fixed byte target; a zero target is rejected at publication.
    #[must_use]
    pub const fn fixed(target_bytes: u64) -> Self {
        Self::Fixed { target_bytes }
    }

    /// Builds an auto target in `1..=99`.
    ///
    /// # Errors
    ///
    /// Returns [`ProducerStatusError::Invalid`] outside the closed range.
    pub const fn auto(target_percent: u8) -> Result<Self, ProducerStatusError> {
        if target_percent == 0 || target_percent >= 100 {
            Err(ProducerStatusError::Invalid)
        } else {
            Ok(Self::Auto { target_percent })
        }
    }

    const fn valid(self) -> bool {
        match self {
            Self::Fixed { target_bytes } => target_bytes > 0,
            Self::Auto { target_percent } => target_percent > 0 && target_percent < 100,
        }
    }
}

/// Revisioned factual collector status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerStatus {
    /// Wire revision; only an exact match decodes.
    pub revision: u16,
    /// Explicitly published lifecycle state.
    pub state: ProducerState,
    /// Publishing collector's process id, non-zero.
    pub collector_pid: u32,
    /// Collector start time as Unix epoch microseconds.
    pub collector_started_at_us: i64,
    /// Last successful publication as Unix epoch microseconds.
    pub last_status_at_us: i64,
    /// Configured retention target, absent when rotation is disabled.
    pub retention: Option<RetentionStatus>,
}

impl ProducerStatus {
    /// Builds a running heartbeat.
    #[must_use]
    pub const fn running(
        collector_pid: u32,
        collector_started_at_us: i64,
        last_status_at_us: i64,
        retention: Option<RetentionStatus>,
    ) -> Self {
        Self {
            revision: PRODUCER_STATUS_REVISION,
            state: ProducerState::Running,
            collector_pid,
            collector_started_at_us,
            last_status_at_us,
            retention,
        }
    }

    /// Converts this status to an explicit terminal publication.
    #[must_use]
    pub const fn stopped(mut self, stopped_at_us: i64) -> Self {
        self.state = ProducerState::Stopped;
        self.last_status_at_us = stopped_at_us;
        self
    }

    fn validate(&self) -> Result<(), ProducerStatusError> {
        if self.revision != PRODUCER_STATUS_REVISION
            || self.collector_pid == 0
            || self.collector_started_at_us < 0
            || self.last_status_at_us < self.collector_started_at_us
            || self.retention.is_some_and(|retention| !retention.valid())
        {
            return Err(ProducerStatusError::Invalid);
        }
        Ok(())
    }
}

/// Status read or atomic-publication failure.
#[derive(Debug)]
pub enum ProducerStatusError {
    /// Filesystem operation failed.
    Io(io::Error),
    /// File exceeded the fixed codec bound.
    TooLarge,
    /// JSON was malformed or contained unknown fields.
    Json(serde_json::Error),
    /// Decoded values violated the revisioned contract.
    Invalid,
}

impl fmt::Display for ProducerStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "producer status I/O failed: {error}"),
            Self::TooLarge => f.write_str("producer status exceeds the byte limit"),
            Self::Json(error) => write!(f, "producer status JSON is invalid: {error}"),
            Self::Invalid => f.write_str("producer status contract is invalid"),
        }
    }
}

impl std::error::Error for ProducerStatusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::TooLarge | Self::Invalid => None,
        }
    }
}

impl From<io::Error> for ProducerStatusError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ProducerStatusError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Reads and validates the optional bounded status file.
///
/// # Errors
///
/// Returns a typed failure for I/O, oversized, malformed, or invalid content.
pub fn read_producer_status(root: &Path) -> Result<Option<ProducerStatus>, ProducerStatusError> {
    let directory = open_root_directory(root)?;
    let file = match open_leaf(&directory, PRODUCER_STATUS_NAME, OFlags::RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    if length > MAX_STATUS_BYTES {
        return Err(ProducerStatusError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    file.take(MAX_STATUS_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATUS_BYTES {
        return Err(ProducerStatusError::TooLarge);
    }
    let status: ProducerStatus = serde_json::from_slice(&bytes)?;
    status.validate()?;
    Ok(Some(status))
}

/// Atomically publishes one validated status through a same-directory file.
///
/// # Errors
///
/// Returns a typed failure when validation, serialization, write, sync, rename,
/// or directory sync fails.
pub fn write_producer_status(
    root: &Path,
    status: &ProducerStatus,
) -> Result<(), ProducerStatusError> {
    status.validate()?;
    let bytes = serde_json::to_vec(status)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATUS_BYTES {
        return Err(ProducerStatusError::TooLarge);
    }
    let directory = open_root_directory(root)?;
    let result = publish_status(&directory, &bytes);
    if result.is_err() {
        let name = leaf_cstring(PRODUCER_STATUS_TEMP_NAME)?;
        let _ = rustix::fs::unlinkat(&directory, name, AtFlags::empty());
    }
    result
}

fn publish_status(directory: &File, bytes: &[u8]) -> Result<(), ProducerStatusError> {
    let mut file = open_leaf(
        directory,
        PRODUCER_STATUS_TEMP_NAME,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
    )?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let temporary = leaf_cstring(PRODUCER_STATUS_TEMP_NAME)?;
    let destination = leaf_cstring(PRODUCER_STATUS_NAME)?;
    rustix::fs::renameat(directory, temporary, directory, destination).map_err(errno_to_status)?;
    directory.sync_all()?;
    Ok(())
}

/// Every status-file operation stays relative to an already-open directory
/// descriptor and refuses symlinks, matching the layout discipline.
fn open_root_directory(root: &Path) -> Result<File, ProducerStatusError> {
    rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        if error == rustix::io::Errno::LOOP {
            ProducerStatusError::Invalid
        } else {
            errno_to_status(error)
        }
    })
}

fn open_leaf(directory: &File, name: &str, access: OFlags) -> io::Result<File> {
    let name = leaf_cstring(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    rustix::fs::openat(
        directory,
        name,
        access | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR | Mode::RGRP,
    )
    .map(File::from)
    .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))
}

fn leaf_cstring(name: &str) -> Result<CString, ProducerStatusError> {
    CString::new(name).map_err(|_error| ProducerStatusError::Invalid)
}

fn errno_to_status(error: rustix::io::Errno) -> ProducerStatusError {
    io::Error::from_raw_os_error(error.raw_os_error()).into()
}
