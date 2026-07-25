//! Exclusive ownership of one persistent overview-cache root.

use std::fs::File;
use std::io;
use std::path::Path;

use rustix::fs::{FlockOperation, Mode, OFlags};

const DIR_MODE: Mode = Mode::RWXU;
const FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const OWNER_LOCK_NAME: &str = ".owner.lock";

/// Failure to establish the destructive/write owner of a cache root.
#[derive(Debug)]
pub(super) enum CacheOwnerError {
    /// Another process or independently constructed store owns this root.
    Contended,
    /// A generated namespace component is not a real directory or file.
    UnsafePath,
    /// The filesystem rejected the ownership operation.
    Io(io::Error),
}

/// Lifetime token for the only process allowed to mutate a cache root.
#[derive(Debug)]
pub(super) struct CacheOwner {
    _lock: File,
}

impl CacheOwner {
    pub(super) fn acquire(cache_root: &Path) -> Result<Self, CacheOwnerError> {
        let namespace = open_namespace(cache_root, true).map_err(classify_io)?;
        let lock = open_file_at(
            &namespace,
            OWNER_LOCK_NAME,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FILE_MODE,
        )
        .map_err(classify_io)?;
        rustix::fs::fchmod(&lock, FILE_MODE)
            .map_err(errno_to_io)
            .map_err(CacheOwnerError::Io)?;
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self { _lock: lock }),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => Err(CacheOwnerError::Contended),
            Err(error) => Err(CacheOwnerError::Io(errno_to_io(error))),
        }
    }
}

pub(super) fn open_namespace(cache_root: &Path, create: bool) -> io::Result<File> {
    let root = open_root(cache_root, create)?;
    let overview = open_child_directory(&root, "overview", create)?;
    open_child_directory(&overview, "v1", create)
}

pub(super) fn open_root(cache_root: &Path, create: bool) -> io::Result<File> {
    if create {
        std::fs::create_dir_all(cache_root)?;
    }
    let root = rustix::fs::open(
        cache_root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)?;
    if !root.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "cache root is not a directory",
        ));
    }
    Ok(root)
}

pub(super) fn open_child_directory(parent: &File, name: &str, create: bool) -> io::Result<File> {
    let mut created = false;
    if create {
        match rustix::fs::mkdirat(parent, name, DIR_MODE) {
            Ok(()) => created = true,
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => return Err(errno_to_io(error)),
        }
    }
    let child = open_file_at(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        if error.raw_os_error().is_some_and(|code| {
            code == rustix::io::Errno::LOOP.raw_os_error()
                || code == rustix::io::Errno::NOTDIR.raw_os_error()
        }) {
            errno_to_io(rustix::io::Errno::LOOP)
        } else {
            error
        }
    })?;
    if create {
        rustix::fs::fchmod(&child, DIR_MODE).map_err(errno_to_io)?;
    }
    if created {
        parent.sync_all()?;
    }
    Ok(child)
}

pub(super) fn open_file_at(
    directory: &File,
    name: &str,
    flags: OFlags,
    mode: Mode,
) -> io::Result<File> {
    rustix::fs::openat(directory, name, flags, mode)
        .map(File::from)
        .map_err(errno_to_io)
}

fn classify_io(error: io::Error) -> CacheOwnerError {
    if error
        .raw_os_error()
        .is_some_and(|code| code == rustix::io::Errno::LOOP.raw_os_error())
        || error.kind() == io::ErrorKind::NotADirectory
    {
        CacheOwnerError::UnsafePath
    } else {
        CacheOwnerError::Io(error)
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}
