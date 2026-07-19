//! Reader for a `/proc` tree whose root is overridable for tests and for
//! host-mounted deployments.
//!
//! Also provides [`statvfs`] for filesystem capacity queries.

use std::collections::BinaryHeap;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Filesystem capacity at a mount point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsSpace {
    /// Total filesystem size in bytes (`f_blocks * f_frsize`).
    pub total_bytes: i64,
    /// Available bytes for unprivileged writes (`f_bavail * f_frsize`).
    pub free_bytes: i64,
}

/// Bounded list of numeric `/proc` directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedPids {
    /// PID numbers retained by the cap, sorted ascending.
    pub pids: Vec<i32>,
    /// Numeric PID directories skipped because the cap was reached.
    pub dropped: usize,
}

/// One child directory under a configured filesystem root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryName {
    /// File name only, never a path.
    pub name: String,
    /// Whether the entry is a directory according to `file_type`.
    pub is_dir: bool,
}

/// Convert raw `statvfs` fields to [`FsSpace`].
///
/// Saturates to `i64::MAX` when the product exceeds `i64::MAX` so that
/// very large filesystems never wrap or panic.
#[must_use]
pub fn space_from_raw(blocks: u64, bavail: u64, frsize: u64) -> FsSpace {
    let saturating_mul =
        |a: u64, b: u64| -> i64 { a.saturating_mul(b).min(i64::MAX as u64).cast_signed() };
    FsSpace {
        total_bytes: saturating_mul(blocks, frsize),
        free_bytes: saturating_mul(bavail, frsize),
    }
}

/// Query filesystem capacity for `mount_point`.
///
/// **Env fixture override:** if `KRONIKA_STATVFS_FIXTURE` is set, its value
/// is parsed as `path1=TOTAL:FREE;path2=TOTAL:FREE` (bytes, decimal). The
/// entry whose path equals `mount_point` is returned; no entry returns `None`.
/// This lets BDD tests inject deterministic capacity without a real filesystem.
///
/// Otherwise calls `statvfs(2)` and maps success via [`space_from_raw`].
/// Returns `None` on any syscall error because a mount can vanish mid-scan.
#[must_use]
pub fn statvfs(mount_point: &str) -> Option<FsSpace> {
    if let Ok(fixture) = std::env::var("KRONIKA_STATVFS_FIXTURE") {
        return parse_fixture(&fixture, mount_point);
    }
    rustix::fs::statvfs(mount_point)
        .ok()
        .map(|s| space_from_raw(s.f_blocks, s.f_bavail, s.f_frsize))
}

pub(crate) fn parse_fixture(fixture: &str, mount_point: &str) -> Option<FsSpace> {
    for entry in fixture.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((path, rest)) = entry.split_once('=') else {
            continue;
        };
        let Some((total_str, free_str)) = rest.split_once(':') else {
            continue;
        };
        if path == mount_point {
            let total_bytes = total_str.trim().parse().ok()?;
            let free_bytes = free_str.trim().parse().ok()?;
            return Some(FsSpace {
                total_bytes,
                free_bytes,
            });
        }
    }
    None
}

/// Maximum bytes read from one procfs file.
///
/// Wave 1 files are small, but fixture roots and host-mounted procfs paths are
/// still external input. The collector rejects larger files before parsing.
pub const MAX_PROC_FILE_BYTES: usize = 4 * 1024 * 1024;

/// A `/proc` root. Real collection uses `/proc`; tests and host-mounted pods
/// point it elsewhere via `KRONIKA_PROC_ROOT`.
#[derive(Debug, Clone)]
pub struct ProcFs {
    root: PathBuf,
}

impl ProcFs {
    /// Root from `KRONIKA_PROC_ROOT`, defaulting to `/proc`.
    #[must_use]
    pub fn from_env() -> Self {
        let root = std::env::var_os("KRONIKA_PROC_ROOT")
            .map_or_else(|| PathBuf::from("/proc"), PathBuf::from);
        Self { root }
    }

    /// A reader rooted at `root`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Return the absolute path for a checked procfs-relative path.
    ///
    /// # Errors
    /// Returns an error when `rel` is empty or escapes the configured root.
    pub fn path(&self, rel: &str) -> io::Result<PathBuf> {
        Ok(self.root.join(checked_relative_path(rel)?))
    }

    /// Read `<root>/<rel>`, trimmed; empty content is an error.
    ///
    /// # Errors
    /// Returns the underlying `io::Error` (with the path) or an empty-file error.
    pub fn read(&self, rel: &str) -> io::Result<String> {
        let trimmed = self.read_raw(rel)?;
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            return Err(io::Error::other(format!("{rel}: empty")));
        }
        Ok(trimmed.to_owned())
    }

    /// Read `<root>/<rel>` without trimming.
    ///
    /// # Errors
    /// Returns an error if `rel` is empty, escapes the root, exceeds
    /// [`MAX_PROC_FILE_BYTES`], or cannot be read as UTF-8.
    pub fn read_raw(&self, rel: &str) -> io::Result<String> {
        let rel_path = checked_relative_path(rel)?;
        let path = self.root.join(rel_path);
        let mut file = std::fs::File::open(&path).map_err(|err| tag_io_error(rel, &err))?;
        let mut content = String::new();
        file.by_ref()
            .take((MAX_PROC_FILE_BYTES + 1) as u64)
            .read_to_string(&mut content)
            .map_err(|err| tag_io_error(rel, &err))?;
        if content.len() > MAX_PROC_FILE_BYTES {
            return Err(io::Error::other(format!(
                "{rel}: exceeds {MAX_PROC_FILE_BYTES} byte procfs read limit"
            )));
        }
        Ok(content)
    }

    /// Return the lowest numeric `/proc` directory names, bounded by `max`.
    ///
    /// # Errors
    /// Returns the underlying `read_dir` error for the proc root.
    pub fn pid_dirs_capped(&self, max: usize) -> io::Result<CappedPids> {
        let mut kept = BinaryHeap::new();
        let mut dropped = 0_usize;
        for entry in std::fs::read_dir(&self.root)? {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(pid) = name.parse::<i32>() else {
                continue;
            };
            if max == 0 {
                dropped = dropped.saturating_add(1);
                continue;
            }
            if kept.len() < max {
                kept.push(pid);
            } else if kept.peek().is_some_and(|highest| pid < *highest) {
                kept.pop();
                kept.push(pid);
                dropped = dropped.saturating_add(1);
            } else {
                dropped = dropped.saturating_add(1);
            }
        }
        let mut pids = kept.into_vec();
        pids.sort_unstable();
        Ok(CappedPids { pids, dropped })
    }
}

/// A `/sys` root, overridable via `KRONIKA_SYS_ROOT`.
///
/// Used to recover the real `(major, minor)` of `major == 0` subvolume mounts
/// (btrfs, ZFS) from `class/block/<name>/dev`, and to let BDD fixture the
/// sysfs tree.
#[derive(Debug, Clone)]
pub struct SysFs {
    root: PathBuf,
}

impl SysFs {
    /// Root from `KRONIKA_SYS_ROOT`, defaulting to `/sys`.
    #[must_use]
    pub fn from_env() -> Self {
        let root = std::env::var_os("KRONIKA_SYS_ROOT")
            .map_or_else(|| PathBuf::from("/sys"), PathBuf::from);
        Self { root }
    }

    /// A reader rooted at `root`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Read `<root>/<rel>`, trimmed; empty content is an error.
    ///
    /// # Errors
    /// Returns the underlying `io::Error` (with the path), an empty-file error,
    /// or a path-escape error when `rel` leaves the configured root.
    pub fn read(&self, rel: &str) -> io::Result<String> {
        let rel_path = checked_relative_path(rel)?;
        let path = self.root.join(rel_path);
        let mut file = std::fs::File::open(&path).map_err(|err| tag_io_error(rel, &err))?;
        let mut content = String::new();
        file.by_ref()
            .take((MAX_PROC_FILE_BYTES + 1) as u64)
            .read_to_string(&mut content)
            .map_err(|err| tag_io_error(rel, &err))?;
        if content.len() > MAX_PROC_FILE_BYTES {
            return Err(io::Error::other(format!(
                "{rel}: exceeds {MAX_PROC_FILE_BYTES} byte sysfs read limit"
            )));
        }
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(io::Error::other(format!("{rel}: empty")));
        }
        Ok(trimmed.to_owned())
    }

    /// Return the absolute path for a checked relative sysfs path.
    ///
    /// # Errors
    /// Returns an error when `rel` is empty or escapes the configured root.
    pub fn path(&self, rel: &str) -> io::Result<PathBuf> {
        Ok(self.root.join(checked_relative_path(rel)?))
    }

    /// Read immediate children under `<root>/<rel>`.
    ///
    /// # Errors
    /// Returns the underlying `read_dir` error or a path validation error.
    pub fn read_dir(&self, rel: &str) -> io::Result<Vec<DirEntryName>> {
        let path = self.path(rel)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            entries.push(DirEntryName {
                name,
                is_dir: entry.file_type()?.is_dir(),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}

/// Parse a `MAJ:MIN` device string (the content of `class/block/<name>/dev`).
///
/// Returns `None` when the string is not exactly two colon-separated `i32`s.
#[must_use]
pub fn parse_dev_pair(content: &str) -> Option<(i32, i32)> {
    let (major, minor) = content.trim().split_once(':')?;
    Some((major.trim().parse().ok()?, minor.trim().parse().ok()?))
}

fn checked_relative_path(rel: &str) -> io::Result<&Path> {
    if rel.trim().is_empty() {
        return Err(io::Error::other("empty proc-relative path"));
    }
    let path = Path::new(rel);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(io::Error::other(format!(
            "{rel}: proc-relative path must stay under the configured root"
        )));
    }
    Ok(path)
}

fn tag_io_error(rel: &str, err: &io::Error) -> io::Error {
    io::Error::new(err.kind(), format!("{rel}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::{FsSpace, ProcFs, SysFs, parse_dev_pair, parse_fixture, space_from_raw};
    use std::io::Write;

    #[test]
    fn space_from_raw_normal() {
        let s = space_from_raw(1000, 400, 4096);
        assert_eq!(s.total_bytes, 1000 * 4096);
        assert_eq!(s.free_bytes, 400 * 4096);
    }

    #[test]
    fn space_from_raw_overflow_saturates() {
        let s = space_from_raw(u64::MAX, u64::MAX, 4096);
        assert_eq!(s.total_bytes, i64::MAX);
        assert_eq!(s.free_bytes, i64::MAX);
    }

    #[test]
    fn statvfs_fixture_hit() {
        assert_eq!(
            parse_fixture("/data=1000:400", "/data"),
            Some(FsSpace {
                total_bytes: 1000,
                free_bytes: 400
            })
        );
    }

    #[test]
    fn statvfs_fixture_miss() {
        assert_eq!(parse_fixture("/data=1000:400", "/other"), None);
    }

    #[test]
    fn statvfs_fixture_multiple_entries() {
        let fixture = "/data=1000:400;/var=2048:512";
        assert_eq!(
            parse_fixture(fixture, "/var"),
            Some(FsSpace {
                total_bytes: 2048,
                free_bytes: 512
            })
        );
        assert_eq!(parse_fixture(fixture, "/missing"), None);
    }

    #[test]
    fn statvfs_fixture_malformed_entry_skipped() {
        // A malformed entry before the target must be skipped, not abort the scan.
        assert_eq!(
            parse_fixture("garbage;/data=1000:400", "/data"),
            Some(FsSpace {
                total_bytes: 1000,
                free_bytes: 400
            })
        );
        // A miss still returns None even when malformed entries are present.
        assert_eq!(parse_fixture("garbage;/data=1000:400", "/other"), None);
    }

    #[test]
    fn reads_relative_path_under_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("sys/kernel")).expect("mkdir");
        let mut f = std::fs::File::create(dir.path().join("sys/kernel/hostname")).expect("create");
        writeln!(f, "  probe-host  ").expect("write");
        let fs = ProcFs::new(dir.path().to_path_buf());
        assert_eq!(fs.read("sys/kernel/hostname").expect("read"), "probe-host");
    }

    #[test]
    fn empty_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::File::create(dir.path().join("stat")).expect("create");
        let fs = ProcFs::new(dir.path().to_path_buf());
        assert!(fs.read("stat").is_err());
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = ProcFs::new(dir.path().to_path_buf());
        assert!(fs.read("nope").is_err());
    }

    #[test]
    fn empty_relative_path_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = ProcFs::new(dir.path().to_path_buf());
        assert!(fs.read("").is_err(), "empty rel must not read the root dir");
        assert!(fs.read_raw("   ").is_err());
    }

    #[test]
    fn parent_relative_path_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = ProcFs::new(dir.path().to_path_buf());
        assert!(fs.read_raw("../stat").is_err());
        assert!(fs.read_raw("/proc/stat").is_err());
    }

    #[test]
    fn parse_dev_pair_reads_major_minor() {
        assert_eq!(parse_dev_pair("259:3\n"), Some((259, 3)));
        assert_eq!(parse_dev_pair("  8:1  "), Some((8, 1)));
    }

    #[test]
    fn parse_dev_pair_rejects_malformed() {
        assert_eq!(parse_dev_pair("259"), None);
        assert_eq!(parse_dev_pair("a:b"), None);
        assert_eq!(parse_dev_pair(""), None);
    }

    #[test]
    fn sysfs_reads_block_dev_under_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("class/block/dm-0")).expect("mkdir");
        std::fs::write(dir.path().join("class/block/dm-0/dev"), "253:0\n").expect("write");
        let sys = SysFs::new(dir.path().to_path_buf());
        assert_eq!(sys.read("class/block/dm-0/dev").expect("read"), "253:0");
    }

    #[test]
    fn sysfs_rejects_escape_and_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sys = SysFs::new(dir.path().to_path_buf());
        assert!(sys.read("../etc/passwd").is_err());
        assert!(sys.read("class/block/nope/dev").is_err());
    }

    #[test]
    fn oversized_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("stat"),
            "x".repeat(super::MAX_PROC_FILE_BYTES + 1),
        )
        .expect("write large fixture");
        let fs = ProcFs::new(dir.path().to_path_buf());
        let err = fs.read_raw("stat").expect_err("oversized file rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}
