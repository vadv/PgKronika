//! Lookup and publication of disposable segment fact files.
//!
//! Sidecar rejection triggers a PGM rebuild. Source failures remain source
//! failures, and persistence failures return the computed facts with a typed
//! diagnostic.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, Write as _};
#[cfg(feature = "qualification")]
use std::io::{BufRead as _, BufReader};
use std::num::NonZeroU64;
#[cfg(feature = "qualification")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(feature = "qualification")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(any(test, feature = "qualification"))]
use std::collections::VecDeque;

use kronika_analytics::overview::SegmentIdentity;
use kronika_format::ReadAt;
use kronika_layout::{DataRoot, LayoutError, LayoutLimits, OverviewOwner};

use super::container::{CacheReadError, FactReadStats, HeaderIdentity};
use super::descriptors::CatalogEntryDescriptor;
use super::factkey::{FactBuildKey, FactKey, FileKind};
use super::facts::{BuildError, SegmentContext, SegmentFacts};
use super::fallback::{FallbackConfig, FallbackFactKey, FallbackFactLru, FallbackStats};
use super::gc::{GcAdmissionError, GcConfig, GcMark, GcOutcome, GcSkipReason, GcState};
use super::limits::Bounds;
use super::persist_mode::{PersistModeSnapshot, PersistState, Reservation};
use crate::unit::{PgmBodyReadStats, PgmUnit};

/// Qualification-only Unix socket used to stop a real web process immediately
/// before the atomic sidecar rename.
#[cfg(feature = "qualification")]
#[doc(hidden)]
pub const QUALIFICATION_PUBLISH_BARRIER_ENV: &str =
    "KRONIKA_WEB_QUALIFICATION_PUBLISH_BARRIER_SOCKET";

/// Qualification-only publication fault consumed by the first sidecar write.
#[cfg(feature = "qualification")]
#[doc(hidden)]
pub const QUALIFICATION_PUBLISH_FAULT_ENV: &str = "KRONIKA_WEB_QUALIFICATION_PUBLISH_FAULT";

/// Barrier message sent after a fully synced temporary sidecar is ready.
#[cfg(feature = "qualification")]
#[doc(hidden)]
pub const QUALIFICATION_PUBLISH_BARRIER_READY: &str = "before_commit";

/// Exact release line accepted by the qualification publication barrier.
#[cfg(feature = "qualification")]
#[doc(hidden)]
pub const QUALIFICATION_PUBLISH_BARRIER_RELEASE: &str = "continue\n";

static STORE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "qualification")]
#[derive(Debug)]
struct QualificationPublishBarrier {
    socket: Option<PathBuf>,
    armed: AtomicBool,
}

#[cfg(feature = "qualification")]
impl QualificationPublishBarrier {
    fn from_env() -> Self {
        let socket = std::env::var_os(QUALIFICATION_PUBLISH_BARRIER_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self {
            armed: AtomicBool::new(socket.is_some()),
            socket,
        }
    }

    fn wait_before_commit(&self, temporary_name: &str) -> Result<(), PersistError> {
        if !self.armed.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let socket = self.socket.as_deref().ok_or(PersistError::TransientIo)?;
        let mut stream = UnixStream::connect(socket).map_err(PersistError::from_io)?;
        writeln!(
            stream,
            "{QUALIFICATION_PUBLISH_BARRIER_READY} {temporary_name}"
        )
        .map_err(PersistError::from_io)?;
        stream.flush().map_err(PersistError::from_io)?;
        let mut release = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut release)
            .map_err(PersistError::from_io)?;
        if release == QUALIFICATION_PUBLISH_BARRIER_RELEASE {
            Ok(())
        } else {
            Err(PersistError::TransientIo)
        }
    }
}

/// Why a fact-file write could not be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistError {
    /// The target filesystem is mounted read-only.
    ReadOnlyFilesystem,
    /// The process lacks permission to publish the sidecar.
    PermissionDenied,
    /// The filesystem has no free blocks.
    NoSpace,
    /// The filesystem quota is exhausted.
    QuotaExceeded,
    /// A retryable filesystem operation failed.
    TransientIo,
    /// A network or remote filesystem handle became stale.
    StaleFilesystem,
    /// Computed facts could not be encoded under the configured bounds.
    InvalidFacts,
    /// Another owner or in-process publication holds the required reservation.
    Busy,
    /// A source or sidecar name resolved through an unsafe file type.
    UnsafePath,
    /// The data directory or sibling sidecar has an invalid structure.
    InvalidSidecarState,
    /// An unclassified I/O failure occurred and requires operator diagnosis.
    Io,
}

#[cfg(feature = "qualification")]
fn qualification_publish_faults_from_env() -> VecDeque<PersistError> {
    let Some(raw) = std::env::var_os(QUALIFICATION_PUBLISH_FAULT_ENV) else {
        return VecDeque::new();
    };
    raw.to_str()
        .and_then(|value| match value {
            "read_only_filesystem" => Some(PersistError::ReadOnlyFilesystem),
            "permission_denied" => Some(PersistError::PermissionDenied),
            "no_space" => Some(PersistError::NoSpace),
            "quota_exceeded" => Some(PersistError::QuotaExceeded),
            "transient_io" => Some(PersistError::TransientIo),
            "stale_filesystem" => Some(PersistError::StaleFilesystem),
            "invalid_facts" => Some(PersistError::InvalidFacts),
            "busy" => Some(PersistError::Busy),
            "unsafe_path" => Some(PersistError::UnsafePath),
            "invalid_sidecar_state" => Some(PersistError::InvalidSidecarState),
            "io" => Some(PersistError::Io),
            _ => None,
        })
        .into_iter()
        .collect()
}

/// Stable operational class for a sidecar-persistence failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistFailureClass {
    /// The backing filesystem is mounted read-only.
    ReadOnly,
    /// The process lacks permission to publish the sidecar.
    Permission,
    /// Free space or configured quota is exhausted.
    Capacity,
    /// A bounded retry may recover from the I/O failure.
    Transient,
    /// Another owner currently has the required lock or reservation.
    Contended,
    /// Retrying without an operator or code change is unsafe.
    Permanent,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::ReadOnlyFilesystem => "data filesystem is read-only",
            Self::PermissionDenied => "sidecar write permission denied",
            Self::NoSpace => "data filesystem is out of space",
            Self::QuotaExceeded => "derived-file quota is exhausted",
            Self::TransientIo => "sidecar filesystem I/O failed transiently",
            Self::StaleFilesystem => "data filesystem handle is stale",
            Self::InvalidFacts => "computed facts cannot be encoded",
            Self::Busy => "sidecar publication is held by another owner",
            Self::UnsafePath => "source or sidecar path contains an unsafe file type",
            Self::InvalidSidecarState => "data directory or sibling sidecar is invalid",
            Self::Io => "sidecar I/O failed",
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
    fact_write_bytes: u64,
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

    /// Canonical fact bytes committed by this load.
    #[must_use]
    pub const fn fact_write_bytes(&self) -> u64 {
        self.fact_write_bytes
    }

    /// PGM body reads performed by this load.
    #[must_use]
    pub const fn pgm_body_read_stats(&self) -> PgmBodyReadStats {
        self.pgm_body_read_stats
    }
}

/// Disposable per-segment fact-sidecar store used by the reader.
#[derive(Debug, Clone)]
pub struct FactStore {
    data_dir: PathBuf,
    fallback: Arc<Mutex<FallbackFactLru>>,
    gc_config: GcConfig,
    gc: Arc<Mutex<GcState>>,
    owner: Arc<Mutex<Option<Arc<OverviewOwner>>>>,
    publication_gate: Arc<Mutex<()>>,
    /// Write mode and retry backoff for sibling sidecars.
    persist: Arc<Mutex<PersistState>>,
    #[cfg(any(test, feature = "qualification"))]
    test_publish_faults: Arc<Mutex<VecDeque<PersistError>>>,
    #[cfg(any(test, feature = "qualification"))]
    test_publish_attempts: Arc<AtomicU64>,
    #[cfg(any(test, feature = "qualification"))]
    test_recovery_gc_attempts: Arc<AtomicU64>,
    #[cfg(feature = "qualification")]
    qualification_publish_barrier: Arc<QualificationPublishBarrier>,
}

struct PersistAttempt {
    state: Arc<Mutex<PersistState>>,
    reservation: u64,
    completed: bool,
}

impl PersistAttempt {
    const fn new(state: Arc<Mutex<PersistState>>, reservation: u64) -> Self {
        Self {
            state,
            reservation,
            completed: false,
        }
    }

    fn finish<T>(mut self, result: Result<T, PersistError>) -> Result<T, PersistError> {
        let state_result = match &result {
            Ok(_) => Ok(()),
            Err(error) => Err(*error),
        };
        lock_unpoisoned(&self.state).finish(self.reservation, state_result, Instant::now());
        self.completed = true;
        result
    }
}

impl Drop for PersistAttempt {
    fn drop(&mut self) {
        if !self.completed {
            lock_unpoisoned(&self.state).cancel(self.reservation, Instant::now());
        }
    }
}

/// Result of checking whether a backed-off store can write again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceProbeOutcome {
    /// No standing failure is due for a probe.
    NotDue,
    /// Another publication or recovery probe owns the reservation.
    InFlight,
    /// The probe durably created and removed its sentinel file.
    Succeeded,
    /// The probe failed with a typed persistence error.
    Failed(PersistError),
}

impl FactStore {
    /// Creates a fact store in the PgKronika-owned data directory.
    #[must_use]
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::with_fallback_config(data_dir, FallbackConfig::default())
    }

    /// Creates a fact store with validated process-local fallback budgets.
    #[must_use]
    pub fn with_fallback_config(
        data_dir: impl Into<PathBuf>,
        fallback_config: FallbackConfig,
    ) -> Self {
        Self::with_configs(data_dir, fallback_config, GcConfig::default())
    }

    /// Creates a fact store with validated fallback and GC/quota policies.
    #[must_use]
    pub fn with_configs(
        data_dir: impl Into<PathBuf>,
        fallback_config: FallbackConfig,
        gc_config: GcConfig,
    ) -> Self {
        let data_dir = data_dir.into();
        let jitter_seed = store_jitter_seed(&data_dir);
        Self {
            data_dir,
            fallback: Arc::new(Mutex::new(FallbackFactLru::new(fallback_config))),
            gc_config,
            gc: Arc::new(Mutex::new(GcState::default())),
            owner: Arc::new(Mutex::new(None)),
            publication_gate: Arc::new(Mutex::new(())),
            persist: Arc::new(Mutex::new(PersistState::new(jitter_seed))),
            #[cfg(any(test, feature = "qualification"))]
            test_publish_faults: Arc::new(Mutex::new({
                #[cfg(feature = "qualification")]
                {
                    qualification_publish_faults_from_env()
                }
                #[cfg(not(feature = "qualification"))]
                {
                    VecDeque::new()
                }
            })),
            #[cfg(any(test, feature = "qualification"))]
            test_publish_attempts: Arc::new(AtomicU64::new(0)),
            #[cfg(any(test, feature = "qualification"))]
            test_recovery_gc_attempts: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "qualification")]
            qualification_publish_barrier: Arc::new(QualificationPublishBarrier::from_env()),
        }
    }

    /// The current persistence write mode and backoff diagnostics.
    #[must_use]
    pub fn persist_mode(&self) -> PersistModeSnapshot {
        self.with_persist(|persist| persist.snapshot(Instant::now()))
    }

    /// Runs the single due recovery probe without changing durable-read paths.
    ///
    /// At most one caller receives the due reservation. A capacity failure may
    /// first run one bounded GC pass from the last complete authoritative mark.
    #[must_use]
    pub fn probe_persistence(&self) -> PersistenceProbeOutcome {
        let now = Instant::now();
        let (reservation, standing_reason) = self
            .with_persist(|persist| (persist.reserve_due_probe(now), persist.standing_reason()));
        let reservation = match reservation {
            Reservation::Acquired(reservation) => reservation,
            Reservation::InFlight => return PersistenceProbeOutcome::InFlight,
            Reservation::Backoff(_) | Reservation::NotNeeded => {
                return PersistenceProbeOutcome::NotDue;
            }
        };
        let attempt = PersistAttempt::new(Arc::clone(&self.persist), reservation);
        if standing_reason.class() == PersistFailureClass::Capacity {
            let _recovered = self.recover_capacity_once();
        }
        match attempt.finish(self.probe_once()) {
            Ok(()) => PersistenceProbeOutcome::Succeeded,
            Err(error) => PersistenceProbeOutcome::Failed(error),
        }
    }

    fn with_persist<T>(&self, operation: impl FnOnce(&mut PersistState) -> T) -> T {
        let mut persist = match self.persist.lock() {
            Ok(persist) => persist,
            Err(poisoned) => poisoned.into_inner(),
        };
        operation(&mut persist)
    }

    fn acquire_owner(&self) -> Result<Arc<OverviewOwner>, PersistError> {
        let mut owner = lock_unpoisoned(&self.owner);
        if let Some(owner) = owner.as_ref() {
            return Ok(Arc::clone(owner));
        }
        let root = DataRoot::open(&self.data_dir).map_err(PersistError::from_layout)?;
        let acquired = Arc::new(
            root.acquire_overview(LayoutLimits::default())
                .map_err(PersistError::from_layout)?,
        );
        *owner = Some(Arc::clone(&acquired));
        drop(owner);
        Ok(acquired)
    }

    /// Performs bounded GC from one complete typed source-view mark.
    ///
    /// A non-authoritative mark, owner contention, incomplete scan, or entry
    /// cap produces no deletion and does not advance grace.
    #[must_use]
    pub fn collect_garbage(&self, mark: &GcMark) -> GcOutcome {
        let _publication = lock_unpoisoned(&self.publication_gate);
        let owner = match self.acquire_owner() {
            Ok(owner) => owner,
            Err(_error) => {
                return GcOutcome {
                    skip_reason: Some(GcSkipReason::OwnerUnavailable),
                    ..GcOutcome::default()
                };
            }
        };
        let mut gc = lock_unpoisoned(&self.gc);
        super::gc::collect(&owner, mark, &mut gc, self.gc_config, SystemTime::now())
    }

    /// PgKronika-owned data directory containing PGM and OVF siblings.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Returns fallback lifetime counters and exact current residency.
    #[must_use]
    pub fn fallback_stats(&self) -> FallbackStats {
        self.with_fallback(|fallback| fallback.stats())
    }

    /// Active GC and optional hard-quota policy.
    #[must_use]
    pub const fn gc_config(&self) -> GcConfig {
        self.gc_config
    }

    /// Looks up durable or admitted fallback facts without extracting PGM
    /// section bodies or attempting publication.
    ///
    /// A cache rejection is reported as `Ok(None)`: callers may then enter
    /// cold-work admission and call [`Self::load_or_build`], which rechecks the
    /// durable/fallback layers to close races with another process.
    ///
    /// # Errors
    /// Returns [`BuildError`] only when the catalog cannot establish the
    /// expected source identity needed for fallback lookup.
    pub fn load_cached<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<Option<FactLoad>, BuildError> {
        match self.read_with_stats(unit, context, bounds) {
            Ok((facts, stats)) => {
                let facts = Arc::new(facts);
                self.discard_fallback_for(facts.identity(), facts.lineage());
                Ok(Some(FactLoad {
                    facts,
                    origin: FactOrigin::CacheHit,
                    rebuild_reason: None,
                    persist_error: None,
                    fact_read_stats: Some(stats),
                    fact_write_bytes: 0,
                    pgm_body_read_stats: PgmBodyReadStats::default(),
                }))
            }
            Err(cache_error) => {
                let rebuild_reason = cache_rebuild_reason(&cache_error);
                let (identity, lineage) = SegmentFacts::provenance(unit)?;
                let fallback_key = FallbackFactKey::for_expected(&identity, &lineage);
                Ok(self
                    .with_fallback(|fallback| fallback.get(&fallback_key, *bounds))
                    .map(|facts| FactLoad {
                        facts,
                        origin: FactOrigin::FallbackHit,
                        rebuild_reason: Some(rebuild_reason),
                        persist_error: None,
                        fact_read_stats: None,
                        fact_write_bytes: 0,
                        pgm_body_read_stats: PgmBodyReadStats::default(),
                    }))
            }
        }
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
                self.discard_fallback_for(facts.identity(), facts.lineage());
                Ok(FactLoad {
                    facts,
                    origin: FactOrigin::CacheHit,
                    rebuild_reason: None,
                    persist_error: None,
                    fact_read_stats: Some(stats),
                    fact_write_bytes: 0,
                    pgm_body_read_stats: PgmBodyReadStats::default(),
                })
            }
            Err(cache_error) => {
                let rebuild_reason = cache_rebuild_reason(&cache_error);
                let (identity, lineage) = SegmentFacts::provenance(unit)?;
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
                        fact_write_bytes: 0,
                        pgm_body_read_stats: PgmBodyReadStats::default(),
                    });
                }

                let (facts, pgm_body_read_stats) = SegmentFacts::extract_with_stats(unit, bounds)?;
                let (facts, persist_error, fact_write_bytes) =
                    self.admit_publish_or_fallback(&facts, context, bounds)?;
                Ok(FactLoad {
                    facts,
                    origin: FactOrigin::Rebuilt,
                    rebuild_reason: Some(rebuild_reason),
                    persist_error,
                    fact_read_stats: None,
                    fact_write_bytes,
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

    /// Atomically publishes `facts` as the named PGM's sibling `.ovf`.
    ///
    /// A stale sidecar is replaced at the same path. Source and sidecar opens
    /// never follow symlinks.
    ///
    /// # Errors
    ///
    /// Returns [`PersistError`] when the cache cannot be mutated safely.
    pub fn publish(
        &self,
        facts: &SegmentFacts,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<PathBuf, PersistError> {
        let bytes = facts
            .encode(bounds)
            .map_err(|_error| PersistError::InvalidFacts)?;
        let path = self.persist_encoded(facts, context, &bytes, bounds)?;
        self.discard_fallback_for(facts.identity(), facts.lineage());
        Ok(path)
    }

    pub(super) fn admit_publish_or_fallback(
        &self,
        facts: &SegmentFacts,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<(Arc<SegmentFacts>, Option<PersistError>, u64), BuildError> {
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

        let persist_error = self
            .persist_encoded(&admitted, context, &bytes, bounds)
            .err();
        let fallback_key = FallbackFactKey::for_facts(&admitted);
        match persist_error {
            None => self.discard_fallback(fallback_key),
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
                        context,
                        admitted.identity(),
                        admitted.lineage(),
                        &expected_catalog,
                        bounds,
                    )
                    .is_ok()
                {
                    self.discard_fallback(fallback_key);
                }
            }
            Some(_error) => {}
        }
        let fact_write_bytes = if persist_error.is_none() {
            canonical_byte_len.get()
        } else {
            0
        };
        Ok((admitted, persist_error, fact_write_bytes))
    }

    fn persist_encoded(
        &self,
        facts: &SegmentFacts,
        context: &SegmentContext,
        bytes: &[u8],
        bounds: &Bounds,
    ) -> Result<PathBuf, PersistError> {
        let reservation = self.with_persist(|persist| persist.reserve_write(Instant::now()));
        let reservation = match reservation {
            Reservation::Acquired(reservation) => reservation,
            Reservation::Backoff(error) => return Err(error),
            Reservation::InFlight => return Err(PersistError::Busy),
            Reservation::NotNeeded => return Err(PersistError::Io),
        };
        PersistAttempt::new(Arc::clone(&self.persist), reservation)
            .finish(self.publish_with_capacity_recovery(facts, context, bytes, bounds))
    }

    fn publish_with_capacity_recovery(
        &self,
        facts: &SegmentFacts,
        context: &SegmentContext,
        bytes: &[u8],
        bounds: &Bounds,
    ) -> Result<PathBuf, PersistError> {
        match self.publish_encoded(facts, context, bytes, bounds) {
            Err(error)
                if error.class() == PersistFailureClass::Capacity
                    && self.recover_capacity_once() =>
            {
                self.publish_encoded(facts, context, bytes, bounds)
            }
            outcome => outcome,
        }
    }

    fn recover_capacity_once(&self) -> bool {
        let mark = lock_unpoisoned(&self.gc).last_authoritative_mark();
        let Some(mark) = mark else {
            return false;
        };
        #[cfg(any(test, feature = "qualification"))]
        self.test_recovery_gc_attempts
            .fetch_add(1, Ordering::Relaxed);
        let outcome = self.collect_garbage(&mark);
        outcome.scan_complete && outcome.sweep_authorized
    }

    fn publish_encoded(
        &self,
        facts: &SegmentFacts,
        context: &SegmentContext,
        bytes: &[u8],
        bounds: &Bounds,
    ) -> Result<PathBuf, PersistError> {
        #[cfg(any(test, feature = "qualification"))]
        {
            self.test_publish_attempts.fetch_add(1, Ordering::Relaxed);
            let injected = lock_unpoisoned(&self.test_publish_faults).pop_front();
            if let Some(error) = injected {
                return Err(error);
            }
        }
        let _publication = lock_unpoisoned(&self.publication_gate);
        let owner = self.acquire_owner()?;
        let key = FactKey::for_identity(facts.identity(), FileKind::SegmentFacts);
        let lineage = facts.lineage().id();
        let build_key = FactBuildKey::new(key, lineage);
        validate_sibling_source(owner.root(), context, facts.identity())
            .map_err(PersistError::from_io)?;
        let final_path = owner
            .root()
            .diagnostic_file_path(context.address(), kronika_layout::FileKind::Ovf);
        let expected_catalog = facts.catalog_descriptors();
        if let Ok(Some(existing)) = owner.root().open_ovf(context.address())
            && SegmentFacts::from_reader(
                existing,
                facts.identity(),
                facts.lineage(),
                &expected_catalog,
                bounds,
            )
            .is_ok()
        {
            lock_unpoisoned(&self.gc).record_cache_hit(build_key, SystemTime::now());
            return Ok(final_path);
        }

        if self.gc_config.max_logical_bytes().is_some() || self.gc_config.max_files().is_some() {
            let incoming =
                u64::try_from(bytes.len()).map_err(|_error| PersistError::QuotaExceeded)?;
            let mut gc = lock_unpoisoned(&self.gc);
            match super::gc::admit_publication(&owner, &mut gc, self.gc_config, incoming) {
                Ok(()) => {}
                Err(GcAdmissionError::Capped) => return Err(PersistError::QuotaExceeded),
                Err(GcAdmissionError::Incomplete) => return Err(PersistError::TransientIo),
            }
        }

        let mut temporary = owner
            .create_ovf_temp(context.address())
            .map_err(PersistError::from_layout)?;
        temporary
            .file_mut()
            .write_all(bytes)
            .map_err(PersistError::from_io)?;
        let candidate = temporary
            .try_clone_file()
            .map_err(PersistError::from_layout)?;
        let candidate_metadata = candidate.metadata().map_err(PersistError::from_io)?;
        let published_usage = super::gc::FileUsage {
            logical_bytes: candidate_metadata.len(),
            allocated_bytes: std::os::unix::fs::MetadataExt::blocks(&candidate_metadata)
                .saturating_mul(512),
        };
        SegmentFacts::from_reader(
            candidate,
            facts.identity(),
            facts.lineage(),
            &expected_catalog,
            bounds,
        )
        .map_err(|_error| PersistError::InvalidSidecarState)?;
        #[cfg(feature = "qualification")]
        self.qualification_publish_barrier
            .wait_before_commit(temporary.temp_name())?;

        temporary.publish().map_err(PersistError::from_layout)?;
        lock_unpoisoned(&self.gc).record_publication(
            context.address(),
            build_key,
            published_usage,
            SystemTime::now(),
        );
        Ok(final_path)
    }

    fn probe_once(&self) -> Result<(), PersistError> {
        let _publication = lock_unpoisoned(&self.publication_gate);
        let owner = self.acquire_owner()?;
        let snapshot = owner
            .root()
            .scan(LayoutLimits::default())
            .map_err(PersistError::from_layout)?;
        let address = snapshot
            .segments
            .last()
            .map(|segment| segment.address)
            .ok_or(PersistError::InvalidSidecarState)?;
        let mut probe = owner
            .create_probe_temp(address)
            .map_err(PersistError::from_layout)?;
        probe
            .file_mut()
            .write_all(b"p")
            .map_err(PersistError::from_io)?;
        probe.finish_probe().map_err(PersistError::from_layout)
    }

    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn inject_publish_faults(&self, faults: impl IntoIterator<Item = PersistError>) {
        *lock_unpoisoned(&self.test_publish_faults) = faults.into_iter().collect();
        self.test_publish_attempts.store(0, Ordering::Relaxed);
        self.test_recovery_gc_attempts.store(0, Ordering::Relaxed);
    }

    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn test_publish_attempts(&self) -> u64 {
        self.test_publish_attempts.load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn test_recovery_gc_attempts(&self) -> u64 {
        self.test_recovery_gc_attempts.load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "qualification"))]
    pub(super) fn force_persistence_probe_due(&self) {
        self.with_persist(|persist| persist.force_due(Instant::now()));
    }

    /// Installs a deterministic persistence-failure sequence for the test-only
    /// qualification executable.
    ///
    /// This hook is not present in production builds.
    #[cfg(feature = "qualification")]
    #[doc(hidden)]
    pub fn qualification_inject_publish_faults(
        &self,
        faults: impl IntoIterator<Item = PersistError>,
    ) {
        self.inject_publish_faults(faults);
    }

    /// Makes the standing retry probe immediately due in a qualification build.
    /// This hook is not present in production builds.
    #[cfg(feature = "qualification")]
    #[doc(hidden)]
    pub fn qualification_force_persistence_probe_due(&self) {
        self.force_persistence_probe_due();
    }

    /// Exact publication-attempt count for a qualification build.
    #[cfg(feature = "qualification")]
    #[doc(hidden)]
    #[must_use]
    pub fn qualification_publish_attempts(&self) -> u64 {
        self.test_publish_attempts()
    }

    /// Exact capacity-recovery GC attempt count for a qualification build.
    #[cfg(feature = "qualification")]
    #[doc(hidden)]
    #[must_use]
    pub fn qualification_recovery_gc_attempts(&self) -> u64 {
        self.test_recovery_gc_attempts()
    }

    fn read_with_stats<R: ReadAt>(
        &self,
        unit: &PgmUnit<R>,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<(SegmentFacts, FactReadStats), CacheReadError> {
        let (identity, lineage) =
            SegmentFacts::provenance(unit).map_err(|_error| CacheReadError::Corrupt)?;
        let expected_catalog: Vec<_> = unit
            .catalog()
            .entries
            .iter()
            .map(CatalogEntryDescriptor::of)
            .collect();
        self.read_expected_with_stats(context, &identity, &lineage, &expected_catalog, bounds)
    }

    fn read_expected_with_stats(
        &self,
        context: &SegmentContext,
        identity: &HeaderIdentity,
        lineage: &SegmentIdentity,
        expected_catalog: &[CatalogEntryDescriptor],
        bounds: &Bounds,
    ) -> Result<(SegmentFacts, FactReadStats), CacheReadError> {
        let root = DataRoot::open(&self.data_dir).map_err(cache_layout_error)?;
        validate_sibling_source(&root, context, identity).map_err(CacheReadError::Io)?;
        let file = root
            .open_ovf(context.address())
            .map_err(cache_layout_error)?
            .ok_or_else(|| {
                CacheReadError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "overview sidecar is absent",
                ))
            })?;
        SegmentFacts::from_reader_with_stats(file, identity, lineage, expected_catalog, bounds)
    }

    fn discard_fallback_for(&self, identity: &HeaderIdentity, lineage: &SegmentIdentity) {
        self.discard_fallback(FallbackFactKey::for_expected(identity, lineage));
    }

    fn discard_fallback(&self, fallback_key: FallbackFactKey) {
        self.with_fallback(|fallback| fallback.discard_durable(fallback_key));
    }

    fn with_fallback<T>(&self, operation: impl FnOnce(&mut FallbackFactLru) -> T) -> T {
        let mut fallback = match self.fallback.lock() {
            Ok(fallback) => fallback,
            Err(poisoned) => poisoned.into_inner(),
        };
        operation(&mut fallback)
    }
}

fn validate_sibling_source(
    root: &DataRoot,
    context: &SegmentContext,
    expected: &HeaderIdentity,
) -> io::Result<()> {
    let source = root.open_pgm(context.address()).map_err(layout_to_io)?;
    let unit = PgmUnit::open(source)
        .map_err(|_error| io::Error::new(io::ErrorKind::InvalidData, "invalid sibling PGM"))?;
    let (actual, _lineage) = SegmentFacts::provenance(&unit)
        .map_err(|_error| io::Error::new(io::ErrorKind::InvalidData, "invalid sibling PGM"))?;
    if actual != *expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sibling PGM does not match overview facts",
        ));
    }
    Ok(())
}

fn layout_to_io(error: LayoutError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn cache_layout_error(error: LayoutError) -> CacheReadError {
    CacheReadError::Io(layout_to_io(error))
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
    /// Stable recovery policy class for this failure.
    #[must_use]
    pub const fn class(self) -> PersistFailureClass {
        match self {
            Self::ReadOnlyFilesystem => PersistFailureClass::ReadOnly,
            Self::PermissionDenied => PersistFailureClass::Permission,
            Self::NoSpace | Self::QuotaExceeded => PersistFailureClass::Capacity,
            Self::TransientIo | Self::StaleFilesystem => PersistFailureClass::Transient,
            Self::Busy => PersistFailureClass::Contended,
            Self::InvalidFacts | Self::UnsafePath | Self::InvalidSidecarState | Self::Io => {
                PersistFailureClass::Permanent
            }
        }
    }

    pub(super) const fn is_backoff_eligible(self) -> bool {
        matches!(
            self.class(),
            PersistFailureClass::ReadOnly
                | PersistFailureClass::Permission
                | PersistFailureClass::Capacity
                | PersistFailureClass::Transient
        )
    }

    const fn is_fallback_eligible(self) -> bool {
        self.is_backoff_eligible() || matches!(self.class(), PersistFailureClass::Contended)
    }

    fn from_layout(error: LayoutError) -> Self {
        match error {
            LayoutError::OwnerContended { .. } => Self::Busy,
            LayoutError::SymlinkNotAllowed { .. } => Self::UnsafePath,
            LayoutError::Io(error) => Self::from_io(error),
            LayoutError::TraversalLimitExceeded { .. } => Self::TransientIo,
            _ => Self::InvalidSidecarState,
        }
    }

    pub(super) fn from_errno(error: rustix::io::Errno) -> Self {
        if error == rustix::io::Errno::ROFS {
            Self::ReadOnlyFilesystem
        } else if matches!(error, rustix::io::Errno::ACCESS | rustix::io::Errno::PERM) {
            Self::PermissionDenied
        } else if error == rustix::io::Errno::NOSPC {
            Self::NoSpace
        } else if error == rustix::io::Errno::DQUOT {
            Self::QuotaExceeded
        } else if error == rustix::io::Errno::STALE {
            Self::StaleFilesystem
        } else if error == rustix::io::Errno::LOOP {
            Self::UnsafePath
        } else if matches!(
            error,
            rustix::io::Errno::IO
                | rustix::io::Errno::AGAIN
                | rustix::io::Errno::BUSY
                | rustix::io::Errno::INTR
                | rustix::io::Errno::TIMEDOUT
        ) {
            Self::TransientIo
        } else {
            Self::Io
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the owned signature is used directly as an I/O Result map_err callback"
    )]
    pub(super) fn from_io(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            io::ErrorKind::StorageFull => Self::NoSpace,
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                Self::TransientIo
            }
            io::ErrorKind::InvalidData | io::ErrorKind::NotADirectory => Self::InvalidSidecarState,
            _ => error
                .raw_os_error()
                .map(rustix::io::Errno::from_raw_os_error)
                .map_or(Self::Io, Self::from_errno),
        }
    }
}

fn store_jitter_seed(data_dir: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    data_dir.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
        .hash(&mut hasher);
    STORE_SEQUENCE
        .fetch_add(1, Ordering::Relaxed)
        .hash(&mut hasher);
    hasher.finish()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, symlink};
    use std::sync::Barrier;

    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::pg_log::PgLogLifecycleV1;
    use kronika_registry::{Section, Ts};
    use tempfile::TempDir;

    use super::super::limits::LIMIT;
    use super::super::qualification_fixture::all_family_fixture;
    use super::*;

    fn context() -> SegmentContext {
        context_for("segment")
    }

    fn context_for(stem: &str) -> SegmentContext {
        SegmentContext::new(crate::test_layout::named_address(stem))
    }

    fn write_pgm(directory: &TempDir, context: &SegmentContext, bytes: &[u8]) {
        crate::test_layout::write_pgm(directory.path(), context.address(), bytes);
    }

    fn pgm_path(directory: &TempDir, context: &SegmentContext) -> PathBuf {
        crate::test_layout::file_path(
            directory.path(),
            context.address(),
            kronika_layout::FileKind::Pgm,
        )
    }

    fn sidecar_path(directory: &TempDir, context: &SegmentContext) -> PathBuf {
        crate::test_layout::file_path(
            directory.path(),
            context.address(),
            kronika_layout::FileKind::Ovf,
        )
    }

    fn stale_ovf_temp_path(directory: &TempDir, context: &SegmentContext) -> PathBuf {
        crate::test_layout::day_path(directory.path(), context.address())
            .join(format!("{}.ovf.12.34.tmp", context.address().id))
    }

    fn overview_temp_count(directory: &TempDir, context: &SegmentContext) -> usize {
        let prefix = format!("{}.ovf.", context.address().id);
        std::fs::read_dir(crate::test_layout::day_path(
            directory.path(),
            context.address(),
        ))
        .expect("list UTC day")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with(&prefix)
                && Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension == "tmp")
        })
        .count()
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
        SegmentFacts::extract(&unit(bytes), &LIMIT).expect("extract")
    }

    #[test]
    fn dropped_persist_attempt_releases_its_reservation() {
        let now = Instant::now();
        let state = Arc::new(Mutex::new(PersistState::new(7)));
        let reserved = lock_unpoisoned(&state).reserve_write(now);
        let reservation = match reserved {
            Reservation::Acquired(reservation) => reservation,
            other => panic!("expected reservation, got {other:?}"),
        };
        drop(PersistAttempt::new(Arc::clone(&state), reservation));
        assert!(matches!(
            lock_unpoisoned(&state).reserve_write(now),
            Reservation::Acquired(_)
        ));
    }

    #[test]
    fn backoff_suppresses_a_second_publication_attempt() {
        let temp = TempDir::new().expect("cache directory");
        let store = FactStore::new(temp.path());
        store.inject_publish_faults([PersistError::TransientIo]);
        let facts = built(&lifecycle_pgm(7));

        let (_first, first_error, _first_write_bytes) = store
            .admit_publish_or_fallback(&facts, &context(), &LIMIT)
            .expect("first admit");
        assert_eq!(first_error, Some(PersistError::TransientIo));
        assert_eq!(
            store.persist_mode().failures,
            1,
            "the first failure arms backoff"
        );

        // The second call reports the standing reason without entering
        // `publish_encoded`, so no new publication failure is recorded.
        let (_second, second_error, _second_write_bytes) = store
            .admit_publish_or_fallback(&facts, &context(), &LIMIT)
            .expect("second admit");
        assert!(
            second_error.is_some(),
            "a suppressed write still reports the standing reason"
        );
        assert_eq!(
            store.persist_mode().failures,
            1,
            "a suppressed write does not enter publication"
        );
        assert_eq!(store.test_publish_attempts(), 1);
    }

    #[test]
    fn cold_build_and_cache_hit_report_exact_io_origins() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        write_pgm(&directory, &context(), &bytes);
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
    fn identical_pgms_use_distinct_same_stem_sidecars_and_stay_restart_warm() {
        let directory = TempDir::new().expect("data directory");
        let bytes = lifecycle_pgm(7);
        let first_context = context_for("first");
        let second_context = context_for("second");
        write_pgm(&directory, &first_context, &bytes);
        write_pgm(&directory, &second_context, &bytes);
        let store = FactStore::new(directory.path());

        let first = store
            .load_or_build(&unit(&bytes), &first_context, &LIMIT)
            .expect("first occurrence");
        let second = store
            .load_or_build(&unit(&bytes), &second_context, &LIMIT)
            .expect("second occurrence");
        assert_eq!(first.origin(), FactOrigin::Rebuilt);
        assert_eq!(second.origin(), FactOrigin::Rebuilt);
        assert_eq!(first.facts().identity(), second.facts().identity());
        assert_eq!(first.facts().lineage(), second.facts().lineage());

        let first_path = sidecar_path(&directory, &first_context);
        let second_path = sidecar_path(&directory, &second_context);
        assert_ne!(first_path, second_path);
        assert!(first_path.is_file());
        assert!(second_path.is_file());

        let restarted = FactStore::new(directory.path());
        for context in [&first_context, &second_context] {
            let warm = restarted
                .load_or_build(&unit(&bytes), context, &LIMIT)
                .expect("restart-warm occurrence");
            assert_eq!(warm.origin(), FactOrigin::CacheHit);
            assert_eq!(warm.pgm_body_read_stats(), PgmBodyReadStats::default());
        }
    }

    #[test]
    fn corrupt_sidecar_is_atomically_replaced_at_the_same_path() {
        let directory = TempDir::new().expect("data directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        write_pgm(&directory, &context(), &bytes);
        let facts = built(&bytes);
        let path = store.publish(&facts, &context(), &LIMIT).expect("publish");
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
        assert_eq!(overview_temp_count(&directory, &context()), 0);
    }

    #[test]
    fn wrong_source_at_the_expected_name_is_rejected() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        let bytes_a = lifecycle_pgm(7);
        let bytes_b = lifecycle_pgm(8);
        let context_a = context_for("source-a");
        let context_b = context_for("source-b");
        write_pgm(&directory, &context_a, &bytes_a);
        write_pgm(&directory, &context_b, &bytes_b);
        let path_a = store
            .publish(&built(&bytes_a), &context_a, &LIMIT)
            .expect("publish source A");
        let path_b = store
            .publish(&built(&bytes_b), &context_b, &LIMIT)
            .expect("publish source B");
        std::fs::copy(path_a, &path_b).expect("place wrong-source candidate");

        let loaded = store
            .load_or_build(&unit(&bytes_b), &context_b, &LIMIT)
            .expect("rebuild source B");
        assert_eq!(
            loaded.rebuild_reason(),
            Some(CacheRebuildReason::WrongSource)
        );
        assert_eq!(loaded.facts().identity().pgm_source_id, 8);
        store
            .read(&unit(&bytes_b), &context_b, &LIMIT)
            .expect("source B replacement");
    }

    #[test]
    fn oversized_candidate_is_rebuilt_and_atomically_replaced() {
        let directory = TempDir::new().expect("data directory");
        let store = FactStore::new(directory.path());
        let target_bytes = lifecycle_pgm(7);
        let target_facts = built(&target_bytes);
        let target_encoded = target_facts
            .encode(&LIMIT)
            .expect("encode target facts under absolute bounds");
        write_pgm(&directory, &context(), &target_bytes);

        let large_source = all_family_fixture().sealed_bytes();
        let large_encoded = built(&large_source)
            .encode(&LIMIT)
            .expect("encode deliberately larger candidate");
        assert!(
            large_encoded.len() > target_encoded.len(),
            "qualification candidate must exceed the target fact file"
        );
        let sidecar = sidecar_path(&directory, &context());
        std::fs::write(&sidecar, &large_encoded).expect("seed oversized candidate");

        let mut tight = LIMIT;
        tight.fact_file_len =
            u64::try_from(target_encoded.len()).expect("target fact length fits u64");
        let source_before = std::fs::read(pgm_path(&directory, &context())).expect("read source");
        let loaded = store
            .load_or_build(&unit(&target_bytes), &context(), &tight)
            .expect("rebuild target below the configured bound");

        assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
        assert_eq!(loaded.rebuild_reason(), Some(CacheRebuildReason::Oversized));
        assert_eq!(loaded.persist_error(), None);
        assert_eq!(loaded.facts(), &target_facts);
        assert!(
            std::fs::metadata(&sidecar)
                .expect("replacement metadata")
                .len()
                <= tight.fact_file_len,
            "replacement must satisfy the same admission bound"
        );
        store
            .read(&unit(&target_bytes), &context(), &tight)
            .expect("replacement is restart-admissible");
        assert_eq!(
            std::fs::read(pgm_path(&directory, &context())).expect("reread source"),
            source_before,
            "rebuild must not alter the source PGM"
        );
    }

    #[test]
    fn symlink_sidecar_fails_closed_without_touching_target() {
        let directory = TempDir::new().expect("data directory");
        let outside = TempDir::new().expect("outside directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        write_pgm(&directory, &context(), &bytes);
        let facts = built(&bytes);
        let path = store.publish(&facts, &context(), &LIMIT).expect("publish");
        std::fs::remove_file(&path).expect("remove fact target");
        let victim = outside.path().join("victim");
        std::fs::write(&victim, b"source authority").expect("write victim");
        symlink(&victim, &path).expect("plant symlink candidate");

        let loaded = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("rebuild around symlink");
        assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
        assert_eq!(loaded.persist_error(), Some(PersistError::UnsafePath));
        assert_eq!(
            std::fs::read(&victim).expect("read victim"),
            b"source authority"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("replacement metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn symlink_pgm_is_rejected_without_touching_its_target() {
        let directory = TempDir::new().expect("data directory");
        let outside = TempDir::new().expect("outside directory");
        let bytes = lifecycle_pgm(7);
        let outside_pgm = outside.path().join("source.pgm");
        std::fs::write(&outside_pgm, &bytes).expect("write outside PGM");
        let pgm = pgm_path(&directory, &context());
        symlink(&outside_pgm, pgm).expect("plant PGM symlink");
        let store = FactStore::new(directory.path());
        let loaded = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("source build remains available");
        assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
        assert_eq!(
            loaded.persist_error(),
            Some(PersistError::InvalidSidecarState)
        );
        assert_eq!(std::fs::read(&outside_pgm).expect("outside PGM"), bytes);
        assert_eq!(store.fallback_stats().inserts, 0);
        assert_eq!(store.fallback_stats().resident_entries, 0);
    }

    #[test]
    fn concurrent_builders_leave_one_valid_committed_file() {
        let directory = TempDir::new().expect("data directory");
        let bytes = Arc::new(lifecycle_pgm(7));
        write_pgm(&directory, &context(), &bytes);
        let store = Arc::new(FactStore::new(directory.path()));
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
        let path = sidecar_path(&directory, &context());
        assert!(path.is_file());
        assert_eq!(
            overview_temp_count(&directory, &context()),
            0,
            "completed builders clean their temp files"
        );
    }

    #[test]
    fn concurrent_publication_failures_leave_one_correct_fallback_entry() {
        let directory = TempDir::new().expect("cache directory");
        let store = Arc::new(FactStore::new(directory.path()));
        store.inject_publish_faults([PersistError::TransientIo]);
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
        let directory = TempDir::new().expect("data directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        write_pgm(&directory, &context(), &bytes);
        let facts = built(&bytes);
        let path = store.publish(&facts, &context(), &LIMIT).expect("publish");
        let stale = stale_ovf_temp_path(&directory, &context());
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
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        store.inject_publish_faults([PersistError::TransientIo]);
        let bytes = lifecycle_pgm(7);
        write_pgm(&directory, &context(), &bytes);
        let fresh = store
            .load_or_build(&unit(&bytes), &context(), &LIMIT)
            .expect("source build");
        assert_eq!(fresh.origin(), FactOrigin::Rebuilt);
        assert_eq!(fresh.persist_error(), Some(PersistError::TransientIo));
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

        store.force_persistence_probe_due();
        store
            .publish(fresh.facts(), &context(), &LIMIT)
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
    fn fallback_identity_is_path_independent_and_separates_sources() {
        let directory = TempDir::new().expect("data directory");
        let store = FactStore::new(directory.path());
        store.inject_publish_faults([PersistError::TransientIo]);
        let bytes = lifecycle_pgm(7);

        let first = store
            .load_or_build(&unit(&bytes), &context_for("first"), &LIMIT)
            .expect("first build");
        let same_source = store
            .load_or_build(&unit(&bytes), &context_for("second"), &LIMIT)
            .expect("same source fallback");
        let other_source_bytes = lifecycle_pgm(8);
        let other_source = store
            .load_or_build(&unit(&other_source_bytes), &context_for("third"), &LIMIT)
            .expect("other source build");

        for loaded in [&first, &other_source] {
            assert_eq!(loaded.origin(), FactOrigin::Rebuilt);
            assert!(loaded.pgm_body_read_stats().read_calls > 0);
        }
        assert_eq!(same_source.origin(), FactOrigin::FallbackHit);
        let stats = store.fallback_stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.inserts, 2);
        assert_eq!(stats.resident_entries, 2);
    }

    #[test]
    fn tighter_admission_does_not_reuse_a_looser_fallback_entry() {
        let directory = TempDir::new().expect("cache directory");
        let store = FactStore::new(directory.path());
        store.inject_publish_faults([PersistError::TransientIo]);
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
        let hours_root = TempDir::new().expect("hours cache directory");
        let hour_store = FactStore::with_fallback_config(
            hours_root.path(),
            FallbackConfig::new(2, crate::MAX_FALLBACK_BYTES).expect("hour config"),
        );
        hour_store.inject_publish_faults([PersistError::TransientIo]);
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
        let bytes_root = TempDir::new().expect("bytes cache directory");
        let byte_store = FactStore::with_fallback_config(
            bytes_root.path(),
            FallbackConfig::new(10, two_entries).expect("byte config"),
        );
        byte_store.inject_publish_faults([PersistError::TransientIo]);
        for bytes in &sources {
            byte_store
                .load_or_build(&unit(bytes), &context(), &LIMIT)
                .expect("populate byte fallback");
        }
        let byte_stats = byte_store.fallback_stats();
        assert!(byte_stats.evictions > 0);
        assert!(byte_stats.resident_bytes <= two_entries);

        let oversized_root = TempDir::new().expect("oversized cache directory");
        let oversized_store = FactStore::with_fallback_config(
            oversized_root.path(),
            FallbackConfig::new(10, encoded_len - 1).expect("oversized config"),
        );
        oversized_store.inject_publish_faults([PersistError::TransientIo]);
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
    fn sidecar_mode_respects_data_policy_and_owner_lock_stays_private() {
        let directory = TempDir::new().expect("data directory");
        let store = FactStore::new(directory.path());
        let bytes = lifecycle_pgm(7);
        write_pgm(&directory, &context(), &bytes);
        let path = store
            .publish(&built(&bytes), &context(), &LIMIT)
            .expect("publish");
        let sidecar_mode = std::fs::metadata(&path).expect("fact metadata").mode() & 0o777;
        assert_eq!(sidecar_mode & 0o600, 0o600, "owner can read and write");
        assert_eq!(sidecar_mode & 0o117, 0, "no execute or world permissions");
        assert_eq!(
            sidecar_mode & !0o640,
            0,
            "umask may remove, but cannot add permissions beyond 0640"
        );
        assert_eq!(
            std::fs::metadata(directory.path().join(".pgkronika-overview.owner.lock"))
                .expect("owner lock metadata")
                .mode()
                & 0o777,
            0o600
        );
    }
}
