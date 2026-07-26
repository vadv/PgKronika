//! Typed filesystem failures shared by journal and segment operations.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Filesystem operation that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemOperation {
    /// Open an existing file or directory.
    Open,
    /// Create a new file without replacing an existing name.
    CreateNew,
    /// Read filesystem metadata.
    Metadata,
    /// Seek within a file.
    Seek,
    /// Read file contents.
    Read,
    /// Write file contents.
    Write,
    /// Flush a userspace writer buffer.
    Flush,
    /// Change a file's length.
    Truncate,
    /// Synchronize file data or metadata.
    SyncFile,
    /// Synchronize a parent directory entry.
    SyncDirectory,
    /// Publish a completed file without replacing an existing name.
    PublishNoReplace,
    /// Remove an owned temporary name.
    Remove,
}

impl fmt::Display for FilesystemOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::Open => "open",
            Self::CreateNew => "create-new",
            Self::Metadata => "metadata",
            Self::Seek => "seek",
            Self::Read => "read",
            Self::Write => "write",
            Self::Flush => "flush",
            Self::Truncate => "truncate",
            Self::SyncFile => "sync-file",
            Self::SyncDirectory => "sync-directory",
            Self::PublishNoReplace => "publish-no-replace",
            Self::Remove => "remove",
        };
        f.write_str(operation)
    }
}

/// One filesystem failure with its operation, safe path, and original error.
#[derive(Debug)]
pub struct FilesystemError {
    operation: FilesystemOperation,
    path: PathBuf,
    source: io::Error,
}

impl FilesystemError {
    pub(crate) fn new(
        operation: FilesystemOperation,
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self {
            operation,
            path: path.into(),
            source,
        }
    }

    /// Return the failed operation.
    #[must_use]
    pub const fn operation(&self) -> FilesystemOperation {
        self.operation
    }

    /// Return the path supplied to the failed operation.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the original operating-system error.
    #[must_use]
    pub const fn io_error(&self) -> &io::Error {
        &self.source
    }
}

impl fmt::Display for FilesystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "filesystem {} at {}: {}",
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl Error for FilesystemError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
