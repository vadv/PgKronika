//! Exclusive mutation ownership for overview sidecars in one data directory.

use std::fs::File;
use std::io;
use std::path::Path;

use rustix::fs::{FlockOperation, Mode, OFlags};

const FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
pub(super) const OWNER_LOCK_NAME: &str = ".pgkronika-overview.owner.lock";

/// Failure to establish the sole sidecar writer for a data directory.
#[derive(Debug)]
pub(super) enum SidecarOwnerError {
    /// Another process or independently constructed store owns this directory.
    Contended,
    /// The configured data-directory path resolves through an unsafe file type.
    UnsafePath,
    /// The filesystem rejected the ownership operation.
    Io(io::Error),
}

/// Lifetime token for the only process allowed to mutate overview sidecars.
#[derive(Debug)]
pub(super) struct SidecarOwner {
    _lock: File,
}

impl SidecarOwner {
    pub(super) fn acquire(data_dir: &Path) -> Result<Self, SidecarOwnerError> {
        let directory = open_data_dir(data_dir).map_err(classify_io)?;
        let lock = open_file_at(
            &directory,
            OWNER_LOCK_NAME,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FILE_MODE,
        )
        .map_err(classify_io)?;
        rustix::fs::fchmod(&lock, FILE_MODE)
            .map_err(errno_to_io)
            .map_err(SidecarOwnerError::Io)?;
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self { _lock: lock }),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                Err(SidecarOwnerError::Contended)
            }
            Err(error) => Err(SidecarOwnerError::Io(errno_to_io(error))),
        }
    }
}

pub(super) fn open_data_dir(data_dir: &Path) -> io::Result<File> {
    let directory = rustix::fs::open(
        data_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)?;
    if !directory.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "data directory is not a directory",
        ));
    }
    Ok(directory)
}

pub(super) fn open_file_at<P: rustix::path::Arg>(
    directory: &File,
    name: P,
    flags: OFlags,
    mode: Mode,
) -> io::Result<File> {
    rustix::fs::openat(directory, name, flags, mode)
        .map(File::from)
        .map_err(errno_to_io)
}

fn classify_io(error: io::Error) -> SidecarOwnerError {
    if error
        .raw_os_error()
        .is_some_and(|code| code == rustix::io::Errno::LOOP.raw_os_error())
        || error.kind() == io::ErrorKind::NotADirectory
    {
        SidecarOwnerError::UnsafePath
    } else {
        SidecarOwnerError::Io(error)
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}
