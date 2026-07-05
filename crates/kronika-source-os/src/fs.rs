//! Reader for a `/proc` tree whose root is overridable for tests and for
//! host-mounted deployments.

use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

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
    use super::ProcFs;
    use std::io::Write;

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
