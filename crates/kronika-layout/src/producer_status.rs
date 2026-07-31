//! Bounded revisioned collector status stored at the data-root boundary.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

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
        /// Whole-tree byte ceiling.
        target_bytes: u64,
    },
    /// Maximum used fraction of the backing filesystem.
    Auto {
        /// Used-filesystem percentage in `1..=99`.
        target_percent: u8,
    },
}

impl RetentionStatus {
    /// Builds a fixed non-zero byte target.
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
    /// Wire revision.
    pub revision: u16,
    /// Explicitly published lifecycle state.
    pub state: ProducerState,
    /// Collector process id.
    pub collector_pid: u32,
    /// Process start time, unix microseconds.
    pub collector_started_at_us: i64,
    /// Last successful status publication, unix microseconds.
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
    let path = root.join(PRODUCER_STATUS_NAME);
    let file = match File::open(path) {
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
    let temporary = root.join(PRODUCER_STATUS_TEMP_NAME);
    let destination = root.join(PRODUCER_STATUS_NAME);
    let result = publish_status(&temporary, &destination, root, &bytes);
    if result.is_err() {
        drop(std::fs::remove_file(&temporary));
    }
    result
}

fn publish_status(
    temporary: &Path,
    destination: &Path,
    root: &Path,
    bytes: &[u8],
) -> Result<(), ProducerStatusError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, destination)?;
    File::open(root)?.sync_all()?;
    Ok(())
}
