//! Typed filesystem failures shared by journal and segment operations.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Parent directory whose entry records a file name.
///
/// A bare relative path belongs to the current directory. Returning `.` for
/// that case keeps directory synchronization identical for absolute, nested,
/// and bare destination names.
pub(crate) fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

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
        f.write_str(match self {
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
        })
    }
}

/// One filesystem failure with its operation, path, and original error.
#[derive(Debug)]
pub struct FilesystemError {
    /// Failed operation.
    pub operation: FilesystemOperation,
    /// Path supplied to the operation.
    pub path: PathBuf,
    /// Original operating-system error.
    pub source: io::Error,
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::parent_directory;

    #[test]
    fn parent_directory_covers_bare_relative_and_nested_names() {
        assert_eq!(parent_directory(Path::new("segment.pgm")), Path::new("."));
        assert_eq!(
            parent_directory(Path::new("segments/segment.pgm")),
            Path::new("segments")
        );
        assert_eq!(
            parent_directory(Path::new("/var/lib/pgkronika/segment.pgm")),
            Path::new("/var/lib/pgkronika")
        );
    }
}
