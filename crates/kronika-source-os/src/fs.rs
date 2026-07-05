//! Reader for a `/proc` tree whose root is overridable for tests and for
//! host-mounted deployments.
//!
//! Also provides [`statvfs`] for filesystem capacity queries.

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

/// Convert raw `statvfs` fields to [`FsSpace`].
///
/// Saturates to `i64::MAX` when the product exceeds `i64::MAX` so that
/// very large filesystems never wrap or panic.
#[must_use]
pub fn space_from_raw(blocks: u64, bavail: u64, frsize: u64) -> FsSpace {
    let saturating_mul = |a: u64, b: u64| -> i64 {
        a.saturating_mul(b)
            .min(i64::MAX as u64)
            .try_into()
            .unwrap_or(i64::MAX)
    };
    FsSpace {
        total_bytes: saturating_mul(blocks, frsize),
        free_bytes: saturating_mul(bavail, frsize),
    }
}

/// Query filesystem capacity for `mount_point`.
///
/// **Env fixture override:** if `KRONIKA_STATVFS_FIXTURE` is set, its value
/// is parsed as `path1=TOTAL:FREE;path2=TOTAL:FREE` (bytes, decimal). The
/// entry whose path equals `mount_point` is returned; no entry → `None`.
/// This lets BDD tests inject deterministic capacity without a real filesystem.
///
/// Otherwise calls `statvfs(2)` and maps success via [`space_from_raw`].
/// Returns `None` on any syscall error — a mount can vanish mid-scan.
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
        let (path, rest) = entry.split_once('=')?;
        let (total_str, free_str) = rest.split_once(':')?;
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
    use super::{FsSpace, ProcFs, parse_fixture, space_from_raw};
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
