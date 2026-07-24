//! Lookup and publication of disposable segment fact files.
//!
//! Cache rejection triggers a PGM rebuild. Source failures remain source
//! failures, and persistence failures return the computed facts with a typed
//! diagnostic.

use std::fs::File;
use std::io::{self, Write as _};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kronika_analytics::overview::SegmentIdentity;
use kronika_format::ReadAt;
use rustix::fs::{AtFlags, FlockOperation, Mode, OFlags, RenameFlags};

use super::container::{CacheReadError, FactReadStats, HeaderIdentity};
use super::descriptors::CatalogEntryDescriptor;
use super::factkey::{FactKey, FileKind, placement};
use super::facts::{BuildError, SegmentContext, SegmentFacts};
use super::fallback::{FallbackConfig, FallbackFactKey, FallbackFactLru, FallbackStats};
use super::limits::Bounds;
use crate::unit::{PgmBodyReadStats, PgmUnit};

const FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const DIR_MODE: Mode = Mode::RWXU;
const NAME_RETRIES: usize = 32;

static PUBLISH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Why a fact-file write could not be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistError {
    /// The target filesystem is mounted read-only.
    ReadOnlyFilesystem,
    /// The process lacks permission to mutate the cache.
    PermissionDenied,
    /// The filesystem has no free blocks.
    NoSpace,
    /// The filesystem quota is exhausted.
    QuotaExceeded,
    /// Computed facts could not be encoded under the configured bounds.
    InvalidFacts,
    /// Another cache owner currently publishes this key.
    Busy,
    /// A generated cache component resolved through an unsafe file type.
    UnsafePath,
    /// Another I/O failure occurred.
    Io,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::ReadOnlyFilesystem => "cache filesystem is read-only",
            Self::PermissionDenied => "cache write permission denied",
            Self::NoSpace => "cache filesystem is out of space",
            Self::QuotaExceeded => "cache filesystem quota is exhausted",
            Self::InvalidFacts => "computed facts cannot be encoded",
            Self::Busy => "cache key is being published by another owner",
            Self::UnsafePath => "cache path contains an unsafe file type",
            Self::Io => "cache I/O failed",
        };
        f.write_str(text)
    }
}

impl std::error::Error for PersistError {}

/// Why a cache candidate caused a source rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRebuildReason {
    /// No committed fact file exists.
    Missing,
    /// The fact contract is incompatible with this build.
    Incompatible,
    /// Physical or logical validation failed.
    Corrupt,
    /// The admitted header or manifest names another source.
    WrongSource,
    /// A safety limit rejected the candidate.
    Oversized,
    /// Reading the candidate failed.
    Io,
}

/// Where a completed fact load came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactOrigin {
    /// A committed fact file served the request.
    CacheHit,
    /// A fully admitted in-memory fallback served the request.
    FallbackHit,
    /// PGM section bodies were decoded.
    Rebuilt,
}

/// Facts with bounded read and persistence diagnostics.
#[derive(Debug)]
pub struct FactLoad {
    facts: Arc<SegmentFacts>,
    origin: FactOrigin,
    rebuild_reason: Option<CacheRebuildReason>,
    persist_error: Option<PersistError>,
    fact_read_stats: Option<FactReadStats>,
    pgm_body_read_stats: PgmBodyReadStats,
}

impl FactLoad {
    /// Loaded canonical facts.
    #[must_use]
    pub fn facts(&self) -> &SegmentFacts {
        self.facts.as_ref()
    }

    /// Shares the admitted immutable facts without copying their payloads.
    #[must_use]
    pub fn shared_facts(&self) -> Arc<SegmentFacts> {
        Arc::clone(&self.facts)
    }

    /// Consumes the diagnostic wrapper.
    #[must_use]
    pub fn into_facts(self) -> SegmentFacts {
        Arc::unwrap_or_clone(self.facts)
    }

    /// Consumes the diagnostic wrapper without copying admitted payloads.
    #[must_use]
    pub fn into_shared_facts(self) -> Arc<SegmentFacts> {
        self.facts
    }

    /// Durable hit, fallback hit, or source rebuild.
    #[must_use]
    pub const fn origin(&self) -> FactOrigin {
        self.origin
    }

    /// Durable candidate rejection that preceded fallback lookup or rebuild.
    #[must_use]
    pub const fn rebuild_reason(&self) -> Option<CacheRebuildReason> {
        self.rebuild_reason
    }

    /// Best-effort persistence failure after a successful rebuild.
    #[must_use]
    pub const fn persist_error(&self) -> Option<PersistError> {
        self.persist_error
    }

    /// Positional fact-file reads on a cache hit.
    #[must_use]
    pub const fn fact_read_stats(&self) -> Option<FactReadStats> {
        self.fact_read_stats
    }

    /// PGM body reads performed by this load.
    #[must_use]
    pub const fn pgm_body_read_stats(&self) -> PgmBodyReadStats {
        self.pgm_body_read_stats
    }
}

/// Disposable per-segment fact-file cache used by the reader.
#[derive(Debug, Clone)]
pub struct FactStore {
    cache_root: PathBuf,
    fallback: Arc<Mutex<FallbackFactLru>>,
}

impl FactStore {
    /// Creates a fact store under an operator-trusted cache root.
    #[must_use]
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        Self::with_fallback_config(cache_root, FallbackConfig::default())
    }

    /// Creates a fact store with validated process-local fallback budgets.
    #[must_use]
    pub fn with_fallback_config(
        cache_root: impl Into<PathBuf>,
        fallback_config: FallbackConfig,
    ) -> Self {
        Self {
            cache_root: cache_root.into(),
            fallback: Arc::new(Mutex::new(FallbackFactLru::new(fallback_config))),
        }
    }

    /// Operator-trusted cache root.
    #[must_use]
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Returns fallback lifetime counters and exact current residency.
    #[must_use]
    pub fn fallback_stats(&self) -> FallbackStats {
        self.with_fallback(|fallback| fallback.stats())
    }

    /// Loads a committed fact file or extracts facts from the source PGM.
    ///
    /// Persistence is best effort. A successful source build is returned with
    /// [`FactLoad::persist_error`] when the cache is read-only, full, busy, or
    /// otherwise unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] when source extraction or its safety bounds fail.
    pub fn load_or_build<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<FactLoad, BuildError> {
        match self.read_with_stats(unit, context, bounds) {
            Ok((facts, stats)) => {
                let facts = Arc::new(facts);
                self.discard_fallback_for(facts.identity());
                Ok(FactLoad {
                    facts,
                    origin: FactOrigin::CacheHit,
                    rebuild_reason: None,
                    persist_error: None,
                    fact_read_stats: Some(stats),
                    pgm_body_read_stats: PgmBodyReadStats::default(),
                })
            }
            Err(cache_error) => {
                let rebuild_reason = cache_rebuild_reason(&cache_error);
                let (identity, lineage) = SegmentFacts::provenance(unit, context)?;
                let fallback_key = FallbackFactKey::for_expected(&identity, &lineage);
                if let Some(facts) =
                    self.with_fallback(|fallback| fallback.get(&fallback_key, *bounds))
                {
                    return Ok(FactLoad {
                        facts,
                        origin: FactOrigin::FallbackHit,
                        rebuild_reason: Some(rebuild_reason),
                        persist_error: None,
                        fact_read_stats: None,
                        pgm_body_read_stats: PgmBodyReadStats::default(),
                    });
                }

                let (facts, pgm_body_read_stats) =
                    SegmentFacts::extract_with_stats(unit, context, bounds)?;
                let (facts, persist_error) = self.admit_publish_or_fallback(facts, bounds)?;
                Ok(FactLoad {
                    facts,
                    origin: FactOrigin::Rebuilt,
                    rebuild_reason: Some(rebuild_reason),
                    persist_error,
                    fact_read_stats: None,
                    pgm_body_read_stats,
                })
            }
        }
    }

    /// Reads and validates committed facts for `unit`.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] for a missing, incompatible, corrupt,
    /// wrong-source, oversized, or unsafe candidate.
    pub fn read<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<SegmentFacts, CacheReadError> {
        self.read_with_stats(unit, context, bounds)
            .map(|(facts, _stats)| facts)
    }

    /// Publishes `facts` with owner-only files and atomic no-replace rename.
    ///
    /// An invalid existing target is moved to a bounded quarantine name while
    /// the per-key owner lock is held. Publication never follows generated
    /// namespace symlinks.
    ///
    /// # Errors
    ///
    /// Returns [`PersistError`] when the cache cannot be mutated safely.
    pub fn publish(&self, facts: &SegmentFacts, bounds: &Bounds) -> Result<PathBuf, PersistError> {
        let bytes = facts
            .encode(bounds)
            .map_err(|_error| PersistError::InvalidFacts)?;
        let path = self.publish_encoded(facts, &bytes, bounds)?;
        self.discard_fallback_for(facts.identity());
        Ok(path)
    }

    pub(super) fn admit_publish_or_fallback(
        &self,
        facts: SegmentFacts,
        bounds: &Bounds,
    ) -> Result<(Arc<SegmentFacts>, Option<PersistError>), BuildError> {
        let bytes = facts.encode(bounds).map_err(BuildError::from)?;
        let canonical_byte_len = u64::try_from(bytes.len())
            .map_err(|_error| BuildError::Overflow)
            .and_then(|length| NonZeroU64::new(length).ok_or(BuildError::Internal))?;
        let expected_catalog = facts.catalog_descriptors();
        let admitted = Arc::new(
            SegmentFacts::from_bytes(
                &bytes,
                facts.identity(),
                facts.lineage(),
                &expected_catalog,
                bounds,
            )
            .map_err(BuildError::from)?,
        );

        let persist_error = self.publish_encoded(&admitted, &bytes, bounds).err();
        let durable_key = FactKey::for_identity(admitted.identity(), FileKind::SegmentFacts);
        match persist_error {
            None => self.discard_fallback(durable_key),
            Some(error) if error.is_fallback_eligible() => {
                self.with_fallback(|fallback| {
                    fallback.insert_after_publication_failure(
                        Arc::clone(&admitted),
                        canonical_byte_len,
                        *bounds,
                    );
                });

                if self
                    .read_expected_with_stats(
                        admitted.identity(),
                        admitted.lineage(),
                        &expected_catalog,
                        bounds,
                    )
                    .is_ok()
                {
                    self.discard_fallback(durable_key);
                }
            }
            Some(_error) => {}
        }
        Ok((admitted, persist_error))
    }

    fn publish_encoded(
        &self,
        facts: &SegmentFacts,
        bytes: &[u8],
        bounds: &Bounds,
    ) -> Result<PathBuf, PersistError> {
        let key = FactKey::for_identity(facts.identity(), FileKind::SegmentFacts);
        let final_path = placement(&self.cache_root, facts.identity().source_scope_id, &key);
        let directory = self
            .open_key_directory(facts.identity(), &key, true)
            .map_err(PersistError::from_io)?;
        let lock_name = format!(".lock-{}", key.hex());
        let lock = open_file_at(
            &directory,
            &lock_name,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FILE_MODE,
        )
        .map_err(PersistError::from_io)?;
        rustix::fs::fchmod(&lock, FILE_MODE).map_err(PersistError::from_errno)?;
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(PersistError::Busy);
            }
            Err(error) => return Err(PersistError::from_errno(error)),
        }

        let final_name = format!("{}.ovf", key.hex());
        let expected_catalog = facts.catalog_descriptors();
        if let Ok(existing) = open_regular_at(&directory, &final_name) {
            if SegmentFacts::from_reader(
                existing,
                facts.identity(),
                facts.lineage(),
                &expected_catalog,
                bounds,
            )
            .is_ok()
            {
                return Ok(final_path);
            }
            quarantine(&directory, &final_name, &key)?;
        } else if path_exists_at(&directory, &final_name)? {
            quarantine(&directory, &final_name, &key)?;
        }

        let (temp_name, temp_file) = create_temp(&directory, &key)?;
        let write_result = write_synced(temp_file, bytes);
        if let Err(error) = write_result {
            unlink_ignoring_missing(&directory, &temp_name);
            return Err(error);
        }

        let outcome = commit_temp(
            &directory,
            &temp_name,
            &final_name,
            facts,
            &expected_catalog,
            &key,
            bounds,
        );
        unlink_ignoring_missing(&directory, &temp_name);
        outcome.map(|()| final_path)
    }

    fn read_with_stats<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<(SegmentFacts, FactReadStats), CacheReadError> {
        let (identity, lineage) =
            SegmentFacts::provenance(unit, context).map_err(|_error| CacheReadError::Corrupt)?;
        let expected_catalog: Vec<_> = unit
            .catalog()
            .entries
            .iter()
            .map(CatalogEntryDescriptor::of)
            .collect();
        self.read_expected_with_stats(&identity, &lineage, &expected_catalog, bounds)
    }

    fn read_expected_with_stats(
        &self,
        identity: &HeaderIdentity,
        lineage: &SegmentIdentity,
        expected_catalog: &[CatalogEntryDescriptor],
        bounds: &Bounds,
    ) -> Result<(SegmentFacts, FactReadStats), CacheReadError> {
        let key = FactKey::for_identity(identity, FileKind::SegmentFacts);
        let directory = self
            .open_key_directory(identity, &key, false)
            .map_err(CacheReadError::Io)?;
        let final_name = format!("{}.ovf", key.hex());
        let file = open_regular_at(&directory, &final_name).map_err(CacheReadError::Io)?;
        SegmentFacts::from_reader_with_stats(file, identity, lineage, expected_catalog, bounds)
    }

    fn discard_fallback_for(&self, identity: &HeaderIdentity) {
        self.discard_fallback(FactKey::for_identity(identity, FileKind::SegmentFacts));
    }

    fn discard_fallback(&self, durable_key: FactKey) {
        self.with_fallback(|fallback| fallback.discard_durable(durable_key));
    }

    fn with_fallback<T>(&self, operation: impl FnOnce(&mut FallbackFactLru) -> T) -> T {
        let mut fallback = match self.fallback.lock() {
            Ok(fallback) => fallback,
            Err(poisoned) => poisoned.into_inner(),
        };
        operation(&mut fallback)
    }

    fn open_key_directory(
        &self,
        identity: &HeaderIdentity,
        key: &FactKey,
        create: bool,
    ) -> Result<File, io::Error> {
        if create {
            std::fs::create_dir_all(&self.cache_root)?;
        }
        let mut directory = File::open(&self.cache_root)?;
        if !directory.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "cache root is not a directory",
            ));
        }
        let scope = hex(&identity.source_scope_id.0);
        for component in ["overview", "v1", scope.as_str(), key.prefix().as_str()] {
            directory = open_child_directory(&directory, component, create)?;
        }
        Ok(directory)
    }
}

fn commit_temp(
    directory: &File,
    temp_name: &str,
    final_name: &str,
    facts: &SegmentFacts,
    expected_catalog: &[CatalogEntryDescriptor],
    key: &FactKey,
    bounds: &Bounds,
) -> Result<(), PersistError> {
    let temp = open_regular_at(directory, temp_name).map_err(PersistError::from_io)?;
    SegmentFacts::from_reader(
        temp,
        facts.identity(),
        facts.lineage(),
        expected_catalog,
        bounds,
    )
    .map_err(|_error| PersistError::Io)?;

    for _ in 0..2 {
        match rustix::fs::renameat_with(
            directory,
            temp_name,
            directory,
            final_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                directory.sync_all().map_err(PersistError::from_io)?;
                return Ok(());
            }
            Err(error) if error == rustix::io::Errno::EXIST => {
                if let Ok(existing) = open_regular_at(directory, final_name)
                    && SegmentFacts::from_reader(
                        existing,
                        facts.identity(),
                        facts.lineage(),
                        expected_catalog,
                        bounds,
                    )
                    .is_ok()
                {
                    return Ok(());
                }
                quarantine(directory, final_name, key)?;
            }
            Err(error) => return Err(PersistError::from_errno(error)),
        }
    }
    Err(PersistError::Io)
}

fn open_child_directory(parent: &File, name: &str, create: bool) -> Result<File, io::Error> {
    let mut created = false;
    if create {
        match rustix::fs::mkdirat(parent, name, DIR_MODE) {
            Ok(()) => created = true,
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => return Err(errno_to_io(error)),
        }
    }
    let child = match open_file_at(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(child) => child,
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                code == rustix::io::Errno::LOOP.raw_os_error()
                    || code == rustix::io::Errno::NOTDIR.raw_os_error()
            }) =>
        {
            return Err(errno_to_io(rustix::io::Errno::LOOP));
        }
        Err(error) => return Err(error),
    };
    if create {
        rustix::fs::fchmod(&child, DIR_MODE).map_err(errno_to_io)?;
    }
    if created {
        parent.sync_all()?;
    }
    Ok(child)
}

fn open_regular_at(directory: &File, name: &str) -> Result<File, io::Error> {
    let file = open_file_at(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache candidate is not a regular file",
        ));
    }
    Ok(file)
}

fn open_file_at(
    directory: &File,
    name: &str,
    flags: OFlags,
    mode: Mode,
) -> Result<File, io::Error> {
    rustix::fs::openat(directory, name, flags, mode)
        .map(File::from)
        .map_err(errno_to_io)
}

fn create_temp(directory: &File, key: &FactKey) -> Result<(String, File), PersistError> {
    for _ in 0..NAME_RETRIES {
        let sequence = PUBLISH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(".tmp-{}-{sequence}-{}", std::process::id(), key.prefix());
        match open_file_at(
            directory,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FILE_MODE,
        ) {
            Ok(file) => {
                rustix::fs::fchmod(&file, FILE_MODE).map_err(PersistError::from_errno)?;
                return Ok((name, file));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(PersistError::from_io(error)),
        }
    }
    Err(PersistError::Io)
}

fn write_synced(mut file: File, bytes: &[u8]) -> Result<(), PersistError> {
    file.write_all(bytes).map_err(PersistError::from_io)?;
    file.sync_all().map_err(PersistError::from_io)
}

fn quarantine(directory: &File, final_name: &str, key: &FactKey) -> Result<(), PersistError> {
    for _ in 0..NAME_RETRIES {
        let sequence = PUBLISH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let quarantine = format!(".bad-{}-{sequence}-{}", std::process::id(), key.prefix());
        match rustix::fs::renameat_with(
            directory,
            final_name,
            directory,
            quarantine,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                directory.sync_all().map_err(PersistError::from_io)?;
                return Ok(());
            }
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
            Err(error) => return Err(PersistError::from_errno(error)),
        }
    }
    Err(PersistError::Io)
}

fn path_exists_at(directory: &File, name: &str) -> Result<bool, PersistError> {
    match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_stat) => Ok(true),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
        Err(error) => Err(PersistError::from_errno(error)),
    }
}

fn unlink_ignoring_missing(directory: &File, name: &str) {
    match rustix::fs::unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::NOENT => {}
        Err(_error) => {}
    }
}

fn cache_rebuild_reason(error: &CacheReadError) -> CacheRebuildReason {
    match error {
        CacheReadError::Incompatible => CacheRebuildReason::Incompatible,
        CacheReadError::Corrupt => CacheRebuildReason::Corrupt,
        CacheReadError::WrongSource => CacheRebuildReason::WrongSource,
        CacheReadError::Oversized => CacheRebuildReason::Oversized,
        CacheReadError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            CacheRebuildReason::Missing
        }
        CacheReadError::Io(_) => CacheRebuildReason::Io,
    }
}

impl PersistError {
    const fn is_fallback_eligible(self) -> bool {
        matches!(
            self,
            Self::ReadOnlyFilesystem
                | Self::PermissionDenied
                | Self::NoSpace
                | Self::QuotaExceeded
                | Self::Busy
                | Self::Io
        )
    }

    fn from_errno(error: rustix::io::Errno) -> Self {
        if error == rustix::io::Errno::ROFS {
            Self::ReadOnlyFilesystem
        } else if matches!(error, rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) {
            Self::PermissionDenied
        } else if error == rustix::io::Errno::NOSPC {
            Self::NoSpace
        } else if error == rustix::io::Errno::DQUOT {
            Self::QuotaExceeded
        } else if error == rustix::io::Errno::LOOP {
            Self::UnsafePath
        } else {
            Self::Io
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the owned signature is used directly as an I/O Result map_err callback"
    )]
    fn from_io(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            io::ErrorKind::StorageFull => Self::NoSpace,
            _ => error
                .raw_os_error()
                .map(rustix::io::Errno::from_raw_os_error)
                .map_or(Self::Io, Self::from_errno),
        }
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::sync::{Arc, Barrier};

    use kronika_analytics::overview::{NamingContractId, SegmentLocator};
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};
    use tempfile::TempDir;

    use super::super::limits::LIMIT;
    use super::*;

    fn context() -> SegmentContext {
        context_for(b"store-a", 0x44)
    }

    fn context_for(namespace: &[u8], locator: u8) -> SegmentContext {
        SegmentContext::new(
            namespace.to_vec(),
            NamingContractId([0x33; 16]),
            SegmentLocator([locator; 32]),
        )
        .expect("valid context")
    }

    fn lifecycle_pgm(source_id: u64) -> Vec<u8> {
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
                source_id,
            },
        )
    }

    fn unit(bytes: &[u8]) -> PgmUnit<&[u8]> {
        PgmUnit::open(bytes).expect("open PGM")
    }

    fn built(bytes: &[u8]) -> SegmentFacts {
        SegmentFacts::extract(&unit(bytes), &context(), &LIMIT).expect("extract")
    }

    #[test]
    fn cold_build_and_cache_hit_report_exact_io_origins() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        let cold_unit = unit(&bytes);
        let cold = store
            .load_or_build(&cold_unit, &context(), &LIMIT)
            .expect("cold build");
        assert_eq!(cold.origin(), FactOrigin::Rebuilt);
        assert_eq!(cold.rebuild_reason(), Some(CacheRebuildReason::Missing));
        assert_eq!(cold.pgm_body_read_stats().read_calls, 1);
        assert!(cold.pgm_body_read_stats().stored_bytes_read > 0);
        assert_eq!(cold.persist_error(), None);

        let restarted_unit = unit(&bytes);
        restarted_unit
            .decode_overview_rows(0)
            .expect("independent shared-unit read");
        assert_ne!(
            restarted_unit.body_read_stats(),
            PgmBodyReadStats::default()
        );
        let warm = store
            .load_or_build(&restarted_unit, &context(), &LIMIT)
            .expect("restart-warm load");
        assert_eq!(warm.origin(), FactOrigin::CacheHit);
        assert_eq!(warm.pgm_body_read_stats(), PgmBodyReadStats::default());
        assert!(warm.fact_read_stats().is_some());
        assert_eq!(warm.facts().observations(), cold.facts().observations());
        assert_eq!(
            store.fallback_stats(),
            FallbackStats {
                misses: 1,
                ..FallbackStats::default()
            },
            "a durable hit neither consults nor duplicates the fallback"
        );
    }

    #[test]
    fn corrupt_target_is_quarantined_and_rebuilt() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        let facts = built(&bytes);
        let path = store.publish(&facts, &LIMIT).expect("publish");
        let mut damaged = std::fs::read(&path).expect("read facts");
        let last = damaged.len() - 1;
        damaged[last] ^= 0xff;
        std::fs::write(&path, damaged).expect("damage facts");

        let loaded = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("rebuild");
        assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
        assert_eq!(loaded.rebuild_reason(), Some(CacheRebuildReason::Corrupt));
        assert_eq!(loaded.persist_error(), None);
        store
            .read(&unit(&bytes), &context(), &LIMIT)
            .expect("replacement is valid");
        let quarantined = std::fs::read_dir(path.parent().expect("parent"))
            .expect("list cache directory")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".bad-"));
        assert!(quarantined, "invalid target must be quarantined");
    }

    #[test]
    fn wrong_source_at_the_expected_name_is_rejected() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes_a = lifecycle_pgm(7);
        let bytes_b = lifecycle_pgm(8);
        let path_a = store
            .publish(&built(&bytes_a), &LIMIT)
            .expect("publish source A");
        let path_b = store
            .publish(&built(&bytes_b), &LIMIT)
            .expect("publish source B");
        std::fs::copy(path_a, &path_b).expect("place wrong-source candidate");

        let loaded = store
            .load_or_build(&unit(&bytes_b), &context(), &LIMIT)
            .expect("rebuild source B");
        assert_eq!(
            loaded.rebuild_reason(),
            Some(CacheRebuildReason::WrongSource)
        );
        assert_eq!(loaded.facts().identity().pgm_source_id, 8);
        store
            .read(&unit(&bytes_b), &context(), &LIMIT)
            .expect("source B replacement");
    }

    #[test]
    fn symlink_candidate_is_quarantined_without_touching_target() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        let facts = built(&bytes);
        let path = store.publish(&facts, &LIMIT).expect("publish");
        std::fs::remove_file(&path).expect("remove fact target");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"source authority").expect("write victim");
        symlink(&victim, &path).expect("plant symlink candidate");

        let loaded = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("rebuild around symlink");
        assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
        assert_eq!(
            std::fs::read(&victim).expect("read victim"),
            b"source authority"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("replacement metadata")
                .file_type()
                .is_file()
        );
    }

    #[test]
    fn symlink_namespace_fails_closed_and_returns_rebuilt_facts() {
        let directory = TempDir::new().expect("cache directory");
        let outside = TempDir::new().expect("outside directory");
        symlink(outside.path(), directory.path().join("overview")).expect("plant namespace link");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        let loaded = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("source build remains available");
        assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
        assert!(loaded.persist_error().is_some());
        assert_eq!(
            std::fs::read_dir(outside.path())
                .expect("outside listing")
                .count(),
            0
        );
        assert_eq!(store.fallback_stats().inserts, 0);
        assert_eq!(store.fallback_stats().resident_entries, 0);
    }

    #[test]
    fn concurrent_builders_leave_one_valid_committed_file() {
        let directory = TempDir::new().expect("cache directory");
        let store = Arc::new(FactStore::new(directory.path()));
        let bytes = Arc::new(lifecycle_pgm(7));
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let unit = unit(&bytes);
                barrier.wait();
                store.load_or_build(&unit, &context(), &LIMIT)
            }));
        }
        barrier.wait();
        for worker in workers {
            let loaded = worker.join().expect("worker").expect("load");
            assert_eq!(loaded.facts().observations().len(), 2);
        }
        store
            .read(&unit(&bytes), &context(), &LIMIT)
            .expect("committed winner");
        let facts = built(&bytes);
        let path = placement(
            directory.path(),
            facts.identity().source_scope_id,
            &FactKey::for_identity(facts.identity(), FileKind::SegmentFacts),
        );
        let temps = std::fs::read_dir(path.parent().expect("parent"))
            .expect("list final directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
            .count();
        assert_eq!(temps, 0, "completed builders clean their temp files");
    }

    #[test]
    fn concurrent_publication_failures_leave_one_correct_fallback_entry() {
        let directory = TempDir::new().expect("cache parent");
        let cache_root = directory.path().join("not-a-directory");
        std::fs::write(&cache_root, b"blocks durable publication").expect("write cache blocker");
        let store = Arc::new(FactStore::new(cache_root.as_path()));
        let bytes = Arc::new(lifecycle_pgm(7));
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _worker in 0..2 {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let unit = unit(&bytes);
                barrier.wait();
                store.load_or_build(&unit, &context(), &LIMIT)
            }));
        }
        barrier.wait();

        for worker in workers {
            let loaded = worker.join().expect("worker").expect("load");
            assert_eq!(loaded.facts().observations().len(), 2);
            assert!(matches!(
                loaded.origin(),
                FactOrigin::Rebuilt | FactOrigin::FallbackHit
            ));
        }
        let stats = store.fallback_stats();
        assert_eq!(stats.resident_entries, 1);
        assert_eq!(
            stats.hits.checked_add(stats.publication_failure_fallbacks),
            Some(2)
        );
        assert_eq!(stats.inserts, stats.publication_failure_fallbacks);
    }

    #[test]
    fn stale_temp_does_not_block_publication() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        let facts = built(&bytes);
        let path = store.publish(&facts, &LIMIT).expect("publish");
        let stale = path.parent().expect("parent").join(".tmp-stale-owned");
        std::fs::write(&stale, b"torn").expect("write stale temp");
        std::fs::remove_file(&path).expect("remove committed target");

        let loaded = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("rebuild");
        assert_eq!(loaded.persist_error(), None);
        assert!(
            stale.exists(),
            "publication leaves an unrelated stale temp untouched"
        );
        store
            .read(&unit(&bytes), &context(), &LIMIT)
            .expect("new committed target");
    }

    #[test]
    fn publication_failure_returns_fresh_facts_then_serves_the_fallback() {
        let directory = TempDir::new().expect("cache parent");
        let cache_root = directory.path().join("not-a-directory");
        std::fs::write(&cache_root, b"blocks durable publication").expect("write cache blocker");
        let store = FactStore::new(cache_root.as_path());
        let bytes = lifecycle_pgm(7);
        let fresh = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("source build");
        assert_eq!(fresh.origin(), FactOrigin::Rebuilt);
        assert_eq!(fresh.persist_error(), Some(PersistError::Io));
        assert_eq!(fresh.facts().observations().len(), 2);
        let canonical_bytes = u64::try_from(
            fresh
                .facts()
                .encode(&LIMIT)
                .expect("encode admitted facts")
                .len(),
        )
        .expect("canonical length fits");
        assert_eq!(
            store.fallback_stats(),
            FallbackStats {
                misses: 1,
                inserts: 1,
                publication_failure_fallbacks: 1,
                resident_entries: 1,
                resident_segment_hours: 1,
                resident_bytes: canonical_bytes,
                ..FallbackStats::default()
            }
        );

        let fallback = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("fallback load");
        assert_eq!(fallback.origin(), FactOrigin::FallbackHit);
        assert_eq!(fallback.pgm_body_read_stats(), PgmBodyReadStats::default());
        assert!(Arc::ptr_eq(&fresh.shared_facts(), &fallback.shared_facts()));
        assert_eq!(store.fallback_stats().hits, 1);

        std::fs::remove_file(&cache_root).expect("remove cache blocker");
        std::fs::create_dir(&cache_root).expect("create durable cache root");
        store
            .publish(fresh.facts(), &LIMIT)
            .expect("publish admitted facts");
        let before_durable = store.fallback_stats();
        assert_eq!(before_durable.resident_entries, 0);
        let durable = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("durable load");
        assert_eq!(durable.origin(), FactOrigin::CacheHit);
        assert_eq!(
            store.fallback_stats(),
            before_durable,
            "durable lookup is preferred without a fallback lookup"
        );
    }

    #[test]
    fn fallback_identity_separates_locator_scope_and_source() {
        let directory = TempDir::new().expect("cache parent");
        let cache_root = directory.path().join("not-a-directory");
        std::fs::write(&cache_root, b"blocks durable publication").expect("write cache blocker");
        let store = FactStore::new(cache_root.as_path());
        let bytes = lifecycle_pgm(7);

        let first = store
            .load_or_build(&unit(&bytes), &context_for(b"store-a", 1), &LIMIT)
            .expect("first build");
        let other_locator = store
            .load_or_build(&unit(&bytes), &context_for(b"store-a", 2), &LIMIT)
            .expect("other locator build");
        let other_scope = store
            .load_or_build(&unit(&bytes), &context_for(b"store-b", 1), &LIMIT)
            .expect("other scope build");
        let other_source_bytes = lifecycle_pgm(8);
        let other_source = store
            .load_or_build(
                &unit(&other_source_bytes),
                &context_for(b"store-a", 1),
                &LIMIT,
            )
            .expect("other source build");

        for loaded in [&first, &other_locator, &other_scope, &other_source] {
            assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
            assert!(loaded.pgm_body_read_stats().read_calls > 0);
        }
        let stats = store.fallback_stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 4);
        assert_eq!(stats.inserts, 4);
        assert_eq!(stats.resident_entries, 4);
    }

    #[test]
    fn tighter_admission_does_not_reuse_a_looser_fallback_entry() {
        let directory = TempDir::new().expect("cache parent");
        let cache_root = directory.path().join("not-a-directory");
        std::fs::write(&cache_root, b"blocks durable publication").expect("write cache blocker");
        let store = FactStore::new(cache_root.as_path());
        let bytes = lifecycle_pgm(7);
        store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("populate fallback");
        let tighter = Bounds {
            items_per_block: 1,
            ..LIMIT
        };

        assert!(matches!(
            store.load_or_build(&unit(&bytes), &context(), &tighter),
            Err(BuildError::LimitExceeded)
        ));
        let stats = store.fallback_stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.inserts, 1);
        assert_eq!(stats.resident_entries, 1);
    }

    #[test]
    fn production_fallback_enforces_lru_hour_byte_and_oversized_budgets() {
        let hours_parent = TempDir::new().expect("hours cache parent");
        let hours_root = hours_parent.path().join("not-a-directory");
        std::fs::write(&hours_root, b"blocks durable publication").expect("write hours blocker");
        let hour_store = FactStore::with_fallback_config(
            hours_root.as_path(),
            FallbackConfig::new(2, crate::MAX_FALLBACK_BYTES).expect("hour config"),
        );
        let sources = [lifecycle_pgm(1), lifecycle_pgm(2), lifecycle_pgm(3)];
        for bytes in &sources[..2] {
            hour_store
                .load_or_build(&unit(bytes), &context(), &LIMIT)
                .expect("populate hour fallback");
        }
        assert_eq!(
            hour_store
                .load_or_build(&unit(&sources[0]), &context(), &LIMIT)
                .expect("refresh recency")
                .origin(),
            FactOrigin::FallbackHit
        );
        hour_store
            .load_or_build(&unit(&sources[2]), &context(), &LIMIT)
            .expect("evict least recent hour");
        assert_eq!(
            hour_store
                .load_or_build(&unit(&sources[0]), &context(), &LIMIT)
                .expect("recent entry remains")
                .origin(),
            FactOrigin::FallbackHit
        );
        assert_eq!(
            hour_store
                .load_or_build(&unit(&sources[1]), &context(), &LIMIT)
                .expect("evicted hour rebuilds")
                .origin(),
            FactOrigin::Rebuilt
        );
        assert!(hour_store.fallback_stats().evictions > 0);
        assert_eq!(hour_store.fallback_stats().resident_segment_hours, 2);

        let encoded_len = u64::try_from(
            built(&sources[0])
                .encode(&LIMIT)
                .expect("encode byte-budget fact")
                .len(),
        )
        .expect("encoded length fits");
        let two_entries = encoded_len.checked_mul(2).expect("two facts fit");
        let bytes_parent = TempDir::new().expect("bytes cache parent");
        let bytes_root = bytes_parent.path().join("not-a-directory");
        std::fs::write(&bytes_root, b"blocks durable publication").expect("write bytes blocker");
        let byte_store = FactStore::with_fallback_config(
            bytes_root.as_path(),
            FallbackConfig::new(10, two_entries).expect("byte config"),
        );
        for bytes in &sources {
            byte_store
                .load_or_build(&unit(bytes), &context(), &LIMIT)
                .expect("populate byte fallback");
        }
        let byte_stats = byte_store.fallback_stats();
        assert!(byte_stats.evictions > 0);
        assert!(byte_stats.resident_bytes <= two_entries);

        let oversized_parent = TempDir::new().expect("oversized cache parent");
        let oversized_root = oversized_parent.path().join("not-a-directory");
        std::fs::write(&oversized_root, b"blocks durable publication")
            .expect("write oversized blocker");
        let oversized_store = FactStore::with_fallback_config(
            oversized_root.as_path(),
            FallbackConfig::new(10, encoded_len - 1).expect("oversized config"),
        );
        let returned = oversized_store
            .load_or_build(&unit(&sources[0]), &context(), &LIMIT)
            .expect("oversized facts remain available");
        assert_eq!(returned.origin(), FactOrigin::Rebuilt);
        assert_eq!(oversized_store.fallback_stats().oversized, 1);
        assert_eq!(oversized_store.fallback_stats().resident_entries, 0);
    }

    #[test]
    fn source_failure_is_not_reported_as_cache_miss() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let mut bytes = lifecycle_pgm(7);
        let pristine = unit(&bytes);
        let offset =
            usize::try_from(pristine.catalog().entries[0].offset).expect("body offset fits");
        bytes[offset] ^= 0xff;
        let corrupt = unit(&bytes);

        assert!(matches!(
            store.load_or_build(&corrupt, &context(), &LIMIT),
            Err(BuildError::Source(_))
        ));
        assert_eq!(store.fallback_stats().inserts, 0);
        assert_eq!(store.fallback_stats().resident_entries, 0);
    }

    #[test]
    fn fact_and_key_directory_modes_are_owner_only() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let path = store
            .publish(&built(&lifecycle_pgm(7)), &LIMIT)
            .expect("publish");
        assert_eq!(
            std::fs::metadata(&path).expect("fact metadata").mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().expect("parent"))
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
