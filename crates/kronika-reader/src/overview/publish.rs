//! Durable publication and disposable-cache lookup for segment fact files.
//!
//! [`FactStore::load_or_build`] serves a sealed segment from its cached fact
//! file when a valid one exists and otherwise rebuilds it from the PGM and
//! republishes. A missing, incompatible, corrupt, wrong-source, or oversized
//! file is only a latency event: it is ignored and rebuilt. A PGM read failure
//! is never masked as a cache miss — it surfaces as [`BuildError::Source`].
//!
//! Publication is content-addressed and crash-safe: bytes are written to a
//! process-unique temporary file, fsynced, re-admitted through the same
//! validation path, and linked into place with no-clobber semantics so a
//! concurrent writer of the same segment never corrupts the winner.

use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kronika_analytics::overview::SegmentIdentity;
use kronika_format::ReadAt;

use super::container::{CacheReadError, FactFile, HeaderIdentity};
use super::factkey::{FactKey, FileKind, placement, placement_dir};
use super::facts::{BuildError, SegmentContext, SegmentFacts};
use super::limits::Bounds;
use crate::unit::PgmUnit;

/// Owner mode for a fact file: readable and writable only by the owner.
const FILE_MODE: u32 = 0o600;
/// Owner mode for the cache namespace directories.
const DIR_MODE: u32 = 0o700;

/// A distinct temp-file suffix within this process.
static PUBLISH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Why a fact file could not be published durably.
///
/// A persist failure never turns a correct computed response into an error; the
/// build stays in memory and later writes back off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistError {
    /// The target filesystem is mounted read-only.
    ReadOnlyFilesystem,
    /// The process lacks permission to write the cache namespace.
    PermissionDenied,
    /// The filesystem is out of space.
    NoSpace,
    /// A transient I/O failure occurred.
    Io,
    /// An existing target failed validation and must not be clobbered.
    InvalidWinner,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::ReadOnlyFilesystem => "cache filesystem is read-only",
            Self::PermissionDenied => "cache write permission denied",
            Self::NoSpace => "cache filesystem is out of space",
            Self::Io => "transient cache I/O failure",
            Self::InvalidWinner => "existing cache winner failed validation",
        };
        f.write_str(text)
    }
}

impl std::error::Error for PersistError {}

impl From<&io::Error> for PersistError {
    fn from(error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            io::ErrorKind::StorageFull => Self::NoSpace,
            _ => Self::Io,
        }
    }
}

/// A reader-owned disposable cache of per-segment fact files.
#[derive(Debug, Clone)]
pub struct FactStore {
    cache_root: PathBuf,
}

impl FactStore {
    /// Binds a fact store to a trusted cache root directory.
    #[must_use]
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: cache_root.into(),
        }
    }

    /// The trusted cache root.
    #[must_use]
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Serves the segment from disk when possible, otherwise builds and caches.
    ///
    /// A cache read failure of any kind rebuilds from the PGM; a rebuild's PGM
    /// failure propagates as [`BuildError::Source`]. Persisting the rebuild is
    /// best effort and never fails the returned facts.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] only for a source or build failure, never for a
    /// cache read or persist failure.
    pub fn load_or_build<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<SegmentFacts, BuildError> {
        if let Ok(cached) = self.read(unit, context, bounds) {
            return Ok(cached);
        }
        let facts = SegmentFacts::extract(unit, context)?;
        drop(self.publish(&facts, bounds));
        Ok(facts)
    }

    /// Reads a valid cached fact file for the segment `unit` describes.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when the file is missing, incompatible,
    /// corrupt, from another source, or oversized. Every variant is a rebuild
    /// signal for [`Self::load_or_build`].
    pub fn read<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<SegmentFacts, CacheReadError> {
        let (identity, lineage) =
            SegmentFacts::provenance(unit, context).map_err(|_error| CacheReadError::Corrupt)?;
        let path = self.path_for(&identity);
        let file = std::fs::File::open(&path)?;
        SegmentFacts::from_reader(file, &identity, &lineage, bounds)
    }

    /// Publishes `facts` durably and returns the final content-addressed path.
    ///
    /// A concurrent writer that already produced the same segment is accepted
    /// as the winner after its file validates.
    ///
    /// # Errors
    ///
    /// Returns [`PersistError`] when the cache cannot be written, or when an
    /// existing target fails validation.
    pub fn publish(&self, facts: &SegmentFacts, bounds: &Bounds) -> Result<PathBuf, PersistError> {
        let bytes = facts.encode(bounds).map_err(|_error| PersistError::Io)?;
        self.publish_bytes(facts.identity(), facts.lineage(), &bytes, bounds)
    }

    /// The trusted path a segment with `identity` occupies.
    fn path_for(&self, identity: &HeaderIdentity) -> PathBuf {
        let key = FactKey::for_identity(identity, FileKind::SegmentFacts);
        placement(&self.cache_root, identity.source_scope_id, &key)
    }

    fn publish_bytes(
        &self,
        identity: &HeaderIdentity,
        lineage: &SegmentIdentity,
        bytes: &[u8],
        bounds: &Bounds,
    ) -> Result<PathBuf, PersistError> {
        let key = FactKey::for_identity(identity, FileKind::SegmentFacts);
        let directory = placement_dir(&self.cache_root, identity.source_scope_id, &key);
        let final_path = placement(&self.cache_root, identity.source_scope_id, &key);
        create_secure_dir(&directory)?;

        let temp = write_temp(&directory, &key, bytes)?;
        let result = commit_temp(&temp, &final_path, identity, lineage, bounds);
        drop(std::fs::remove_file(&temp));
        result
    }
}

/// Writes bytes into a process-unique owner-only temp file and fsyncs it.
fn write_temp(directory: &Path, key: &FactKey, bytes: &[u8]) -> Result<PathBuf, PersistError> {
    use std::io::Write as _;

    let sequence = PUBLISH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = directory.join(format!(
        ".tmp-{}-{}-{}",
        std::process::id(),
        sequence,
        key.prefix()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(FILE_MODE)
        .open(&temp)
        .map_err(|error| PersistError::from(&error))?;
    file.write_all(bytes)
        .map_err(|error| PersistError::from(&error))?;
    file.sync_all()
        .map_err(|error| PersistError::from(&error))?;
    Ok(temp)
}

/// Re-admits the file as it landed on disk, then links it into place with no
/// clobber.
fn commit_temp(
    temp: &Path,
    final_path: &Path,
    identity: &HeaderIdentity,
    lineage: &SegmentIdentity,
    bounds: &Bounds,
) -> Result<PathBuf, PersistError> {
    // Re-read the synced temp: a write-path fault must not become the winner.
    let written = std::fs::read(temp).map_err(|error| PersistError::from(&error))?;
    FactFile::admit(&written, identity, lineage, bounds).map_err(|_error| PersistError::Io)?;

    match std::fs::hard_link(temp, final_path) {
        Ok(()) => {
            sync_directory(final_path)?;
            Ok(final_path.to_path_buf())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            accept_existing_winner(final_path, identity, lineage, bounds)
        }
        Err(error) => Err(PersistError::from(&error)),
    }
}

/// Accepts an already-present target only after it validates.
fn accept_existing_winner(
    final_path: &Path,
    identity: &HeaderIdentity,
    lineage: &SegmentIdentity,
    bounds: &Bounds,
) -> Result<PathBuf, PersistError> {
    let existing = std::fs::read(final_path).map_err(|error| PersistError::from(&error))?;
    match FactFile::admit(&existing, identity, lineage, bounds) {
        Ok(_) => Ok(final_path.to_path_buf()),
        Err(_) => Err(PersistError::InvalidWinner),
    }
}

/// Creates the cache directory chain with owner-only permissions.
fn create_secure_dir(directory: &Path) -> Result<(), PersistError> {
    std::fs::create_dir_all(directory).map_err(|error| PersistError::from(&error))?;
    // Tighten the leaf; parents keep whatever the operator configured.
    if let Ok(metadata) = std::fs::metadata(directory) {
        let mut perms = metadata.permissions();
        perms.set_mode(DIR_MODE);
        drop(std::fs::set_permissions(directory, perms));
    }
    Ok(())
}

/// Flushes a rename into the parent directory so it survives a crash.
fn sync_directory(final_path: &Path) -> Result<(), PersistError> {
    let Some(parent) = final_path.parent() else {
        return Ok(());
    };
    let directory = std::fs::File::open(parent).map_err(|error| PersistError::from(&error))?;
    directory
        .sync_all()
        .map_err(|error| PersistError::from(&error))
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::CoverageSpan;
    use kronika_analytics::overview::{
        CountLimits, NamingContractId, OracleLimits, SegmentLocator, semantic_divergences,
    };
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};
    use tempfile::TempDir;

    use super::super::limits::LIMIT;
    use super::*;

    const LIMITS: OracleLimits = OracleLimits {
        max_observations: 256,
        max_coverage_spans: 256,
        count_limits: CountLimits {
            max_input_entries: 256,
            max_joint_keys: 256,
            max_signal_keys: 256,
        },
    };

    fn context() -> SegmentContext {
        SegmentContext {
            normalized_store_namespace: b"store-a".to_vec(),
            naming_contract_id: NamingContractId([0x33; 16]),
            segment_locator: SegmentLocator([0x44; 32]),
        }
    }

    fn lifecycle_pgm() -> Vec<u8> {
        let rows = [
            PgLogLifecycleV1 {
                ts: Ts(1_500),
                kind: 2,
                pid: None,
                signal: None,
                shutdown_mode: None,
                message: None,
                query_detail: None,
                dict_dropped_fields: 0,
            },
            PgLogLifecycleV1 {
                ts: Ts(1_700),
                kind: 0,
                pid: Some(11),
                signal: Some(6),
                shutdown_mode: None,
                message: None,
                query_detail: None,
                dict_dropped_fields: 0,
            },
        ];
        let body = PgLogLifecycleV1::encode(&rows).expect("encode section");
        build_part(
            &[SectionInput {
                type_id: 1_028_001,
                rows: 2,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_500,
                max_ts: 1_700,
                source_id: 7,
            },
        )
    }

    fn full_range() -> CoverageSpan {
        CoverageSpan::new(0, 10_000).expect("range")
    }

    fn agree(left: &SegmentFacts, right: &SegmentFacts) -> bool {
        semantic_divergences(left, right, full_range(), LIMITS)
            .expect("bounded comparison")
            .is_empty()
    }

    #[test]
    fn publish_then_read_round_trips_the_facts() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let bytes = lifecycle_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let built = SegmentFacts::extract(&unit, &context()).expect("extract");

        let path = store.publish(&built, &LIMIT).expect("publish");
        assert!(path.exists(), "the fact file is durable");
        let cached = store.read(&unit, &context(), &LIMIT).expect("read cache");
        assert_eq!(cached.observations(), built.observations());
        assert!(agree(&cached, &built));
    }

    #[test]
    fn load_or_build_builds_on_a_miss_then_serves_the_cache() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let bytes = lifecycle_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");

        assert!(
            store.read(&unit, &context(), &LIMIT).is_err(),
            "cold cache has no file yet"
        );
        let cold = store
            .load_or_build(&unit, &context(), &LIMIT)
            .expect("cold build");
        let warm = store
            .read(&unit, &context(), &LIMIT)
            .expect("file exists after a cold build");
        assert!(agree(&cold, &warm));
    }

    #[test]
    fn a_forced_recompute_equals_the_cached_answer() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let bytes = lifecycle_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");

        let cached = store
            .load_or_build(&unit, &context(), &LIMIT)
            .expect("build");
        let forced = SegmentFacts::extract(&unit, &context()).expect("forced raw decode");
        assert!(
            agree(&cached, &forced),
            "the derived answer differs from a forced recompute only in speed"
        );
    }

    #[test]
    fn deleting_the_cache_only_costs_a_rebuild() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let bytes = lifecycle_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");

        let path = store
            .publish(
                &SegmentFacts::extract(&unit, &context()).expect("extract"),
                &LIMIT,
            )
            .expect("publish");
        std::fs::remove_file(&path).expect("remove derived file");

        let rebuilt = store
            .load_or_build(&unit, &context(), &LIMIT)
            .expect("rebuild after deletion");
        assert!(path.exists(), "the rebuild republished the file");
        let reread = store.read(&unit, &context(), &LIMIT).expect("reread");
        assert!(agree(&rebuilt, &reread));
    }

    #[test]
    fn a_corrupt_cache_file_is_rebuilt_not_trusted() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let bytes = lifecycle_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");

        let path = store
            .publish(
                &SegmentFacts::extract(&unit, &context()).expect("extract"),
                &LIMIT,
            )
            .expect("publish");
        let mut on_disk = std::fs::read(&path).expect("read fact file");
        let last = on_disk.len() - 1;
        on_disk[last] ^= 0xFF;
        std::fs::write(&path, &on_disk).expect("corrupt fact file");

        assert!(
            store.read(&unit, &context(), &LIMIT).is_err(),
            "a corrupt cache file is not served"
        );
        let rebuilt = store
            .load_or_build(&unit, &context(), &LIMIT)
            .expect("rebuild over corruption");
        let forced = SegmentFacts::extract(&unit, &context()).expect("forced");
        assert!(agree(&rebuilt, &forced));
    }

    #[test]
    fn republishing_the_same_segment_accepts_the_existing_winner() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let bytes = lifecycle_pgm();
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pgm");
        let facts = SegmentFacts::extract(&unit, &context()).expect("extract");

        let first = store.publish(&facts, &LIMIT).expect("first publish");
        let second = store.publish(&facts, &LIMIT).expect("idempotent republish");
        assert_eq!(first, second, "content addressing yields one stable path");
    }

    #[test]
    fn a_source_read_failure_is_not_masked_as_a_cache_miss() {
        let dir = TempDir::new().expect("temp dir");
        let store = FactStore::new(dir.path());
        let mut bytes = lifecycle_pgm();
        // Flip a byte inside the section body: the catalog still opens, but the
        // section fails its CRC on decode.
        let unit = PgmUnit::open(bytes.as_slice()).expect("open pristine");
        let body_offset =
            usize::try_from(unit.catalog().entries[0].offset).expect("offset fits usize");
        bytes[body_offset] ^= 0xFF;
        let corrupt = PgmUnit::open(bytes.as_slice()).expect("catalog still valid");

        let outcome = store.load_or_build(&corrupt, &context(), &LIMIT);
        assert!(
            matches!(outcome, Err(BuildError::Source(_))),
            "a source failure surfaces as a source error, not empty facts"
        );
    }
}
