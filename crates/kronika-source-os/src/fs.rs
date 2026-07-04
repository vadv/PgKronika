//! Reader for a `/proc` tree whose root is overridable for tests and for
//! host-mounted deployments.

use std::io;
use std::path::PathBuf;

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
    /// Returns an error if `rel` is empty (it would read the root directory),
    /// otherwise the underlying `io::Error`, tagged with the path.
    pub fn read_raw(&self, rel: &str) -> io::Result<String> {
        if rel.trim().is_empty() {
            return Err(io::Error::other("empty proc-relative path"));
        }
        let path = self.root.join(rel);
        std::fs::read_to_string(&path)
            .map_err(|err| io::Error::new(err.kind(), format!("{rel}: {err}")))
    }
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
}
