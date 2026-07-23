//! Durable, atomic publication of a built fact file.
//!
//! Publication writes a process-unique temp in the target directory, forces it
//! to disk, re-admits its own bytes, then links it into place with no-clobber
//! semantics. Because a fact file is content-addressed by its key, a concurrent
//! writer that already produced the target is a valid winner: the loser accepts
//! it after validating it, and never overwrites it. A build that survives but
//! fails to persist still returns its computed response upstream.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::container::{CacheReadError, FactFile, HeaderIdentity};
use super::limits::Bounds;

/// Process-global counter making temp names unique within one process.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Why a fact file could not be persisted.
#[derive(Debug)]
pub enum PersistError {
    /// The target filesystem is read-only.
    ReadOnlyFilesystem,
    /// The process lacks permission to write the target.
    PermissionDenied,
    /// The filesystem is out of space.
    NoSpace,
    /// A storage quota was exceeded.
    Quota,
    /// A filesystem operation failed.
    Io(io::Error),
    /// The file this writer produced or the race winner did not admit.
    InvalidWinner,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnlyFilesystem => write!(f, "fact-file target is read-only"),
            Self::PermissionDenied => write!(f, "fact-file target denied permission"),
            Self::NoSpace => write!(f, "fact-file target is out of space"),
            Self::Quota => write!(f, "fact-file target exceeded quota"),
            Self::Io(error) => write!(f, "fact-file persist io: {error}"),
            Self::InvalidWinner => write!(f, "fact-file did not re-admit after write"),
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for PersistError {
    fn from(value: io::Error) -> Self {
        match value.kind() {
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::StorageFull => Self::NoSpace,
            io::ErrorKind::QuotaExceeded => Self::Quota,
            _ => Self::Io(value),
        }
    }
}

/// The result of a publish attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// This writer linked the file into place.
    Published,
    /// A concurrent writer already published a valid winner.
    AcceptedExistingWinner,
}

/// Publishes `bytes` as the fact file `directory/file_name`.
///
/// The bytes must already admit against `expected`; this is verified after the
/// durable write and before the file becomes visible, so a partial or wrong
/// file never wins a race.
///
/// # Errors
/// Returns [`PersistError`] for a filesystem failure, or
/// [`PersistError::InvalidWinner`] when this writer's own bytes or the race
/// winner fail admission.
pub fn publish(
    directory: &Path,
    file_name: &str,
    bytes: &[u8],
    expected: &HeaderIdentity,
    bounds: &Bounds,
) -> Result<PublishOutcome, PersistError> {
    // Reject before touching the disk: a file that cannot admit must never win.
    admit_or_invalid(bytes, expected, bounds)?;

    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".{file_name}.tmp.{}.{sequence}", std::process::id());
    let temp_path = directory.join(&temp_name);
    let target_path = directory.join(file_name);

    write_durable_temp(&temp_path, bytes)?;

    // Re-admit what actually landed on disk: write-path corruption must not
    // become the visible winner.
    let written = match std::fs::read(&temp_path) {
        Ok(written) => written,
        Err(error) => {
            std::fs::remove_file(&temp_path).ok();
            return Err(PersistError::from(error));
        }
    };
    if let Err(invalid) = admit_or_invalid(&written, expected, bounds) {
        std::fs::remove_file(&temp_path).ok();
        return Err(invalid);
    }

    let outcome = match std::fs::hard_link(&temp_path, &target_path) {
        Ok(()) => PublishOutcome::Published,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let winner = read_target(&target_path, &temp_path)?;
            if admit_or_invalid(&winner, expected, bounds).is_err() {
                // The existing file is corrupt; replace it with our validated one.
                std::fs::remove_file(&target_path).ok();
                link_or_cleanup(&temp_path, &target_path)?;
                PublishOutcome::Published
            } else {
                PublishOutcome::AcceptedExistingWinner
            }
        }
        Err(error) => {
            std::fs::remove_file(&temp_path).ok();
            return Err(PersistError::from(error));
        }
    };

    sync_directory(directory)?;
    std::fs::remove_file(&temp_path).ok();
    Ok(outcome)
}

fn admit_or_invalid(
    bytes: &[u8],
    expected: &HeaderIdentity,
    bounds: &Bounds,
) -> Result<(), PersistError> {
    match FactFile::admit(bytes, expected, bounds) {
        Ok(_) => Ok(()),
        Err(CacheReadError::Io(error)) => Err(PersistError::Io(error)),
        Err(_) => Err(PersistError::InvalidWinner),
    }
}

fn write_durable_temp(temp_path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read_target(target_path: &Path, temp_path: &Path) -> Result<Vec<u8>, PersistError> {
    match std::fs::read(target_path) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            std::fs::remove_file(temp_path).ok();
            Err(PersistError::from(error))
        }
    }
}

fn link_or_cleanup(temp_path: &Path, target_path: &Path) -> Result<(), PersistError> {
    match std::fs::hard_link(temp_path, target_path) {
        Ok(()) => Ok(()),
        Err(error) => {
            std::fs::remove_file(temp_path).ok();
            Err(PersistError::from(error))
        }
    }
}

fn sync_directory(directory: &Path) -> Result<(), PersistError> {
    #[cfg(unix)]
    {
        // Opening the directory read-only is enough to fsync its entries.
        File::open(directory)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{AlignmentId, CounterSample, MetricSeriesId};

    use super::super::block::CounterSamplesBlock;
    use super::super::container::BlockContent;
    use super::super::limits::LIMIT;
    use super::*;

    fn identity() -> HeaderIdentity {
        HeaderIdentity::from_current_contract(1, 7, 0, 1_000, 0, [0x11; 32], [0x22; 32])
    }

    fn valid_bytes() -> Vec<u8> {
        let sample = CounterSample::new(MetricSeriesId([1; 16]), AlignmentId([1; 16]), 10, 5, 1);
        let block = CounterSamplesBlock::new(vec![sample], &LIMIT).expect("block");
        FactFile::build(
            &identity(),
            vec![BlockContent::CounterSamples(Box::new(block))],
            &LIMIT,
        )
        .expect("build")
    }

    fn temp_files(directory: &Path) -> usize {
        std::fs::read_dir(directory)
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".facts.ovf.tmp")
            })
            .count()
    }

    #[test]
    fn publish_writes_a_file_that_admits_and_leaves_no_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = valid_bytes();
        let outcome =
            publish(dir.path(), "facts.ovf", &bytes, &identity(), &LIMIT).expect("publish");
        assert_eq!(outcome, PublishOutcome::Published);

        let target = dir.path().join("facts.ovf");
        let written = std::fs::read(&target).expect("read published");
        assert!(FactFile::admit(&written, &identity(), &LIMIT).is_ok());
        assert_eq!(temp_files(dir.path()), 0, "no temp file remains");
    }

    #[test]
    fn a_second_publisher_accepts_the_existing_winner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = valid_bytes();
        publish(dir.path(), "facts.ovf", &bytes, &identity(), &LIMIT).expect("first");
        let outcome =
            publish(dir.path(), "facts.ovf", &bytes, &identity(), &LIMIT).expect("second");
        assert_eq!(outcome, PublishOutcome::AcceptedExistingWinner);
    }

    #[test]
    fn publishing_bytes_that_do_not_admit_fails_without_a_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let garbage = vec![0_u8; 200];
        let result = publish(dir.path(), "facts.ovf", &garbage, &identity(), &LIMIT);
        assert!(matches!(result, Err(PersistError::InvalidWinner)));
        assert!(!dir.path().join("facts.ovf").exists(), "no target created");
        assert_eq!(temp_files(dir.path()), 0);
    }

    #[test]
    fn a_corrupt_existing_target_is_replaced_by_a_valid_winner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("facts.ovf");
        std::fs::write(&target, [0_u8; 32]).expect("seed corrupt target");
        let bytes = valid_bytes();
        let outcome =
            publish(dir.path(), "facts.ovf", &bytes, &identity(), &LIMIT).expect("publish");
        assert_eq!(outcome, PublishOutcome::Published);
        let written = std::fs::read(&target).expect("read");
        assert!(FactFile::admit(&written, &identity(), &LIMIT).is_ok());
    }
}
