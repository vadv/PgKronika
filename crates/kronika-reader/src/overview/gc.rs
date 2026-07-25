//! Fail-closed garbage collection and accounting for overview fact files.
//!
//! Only canonical committed identities inside `overview/v1` are eligible for
//! retention cleanup. Enumeration is descriptor-relative and never follows a
//! symlink. The collector completes a bounded inventory before it advances
//! grace or unlinks anything.

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::time::{Duration, SystemTime};

use kronika_analytics::overview::SourceScopeId;
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags};

use super::cache_owner::{open_child_directory, open_file_at};
use super::container::{FactFileReader, HeaderIdentity};
use super::descriptors::SourceDescriptor;
use super::factkey::{FactBuildKey, FactKey, FileKind, parse_hex_32};
use super::limits::LIMIT;

const HEADER_LEN: usize = 160;
const OWNER_LOCK_NAME: &str = ".owner.lock";
const FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const MAX_GC_ENTRIES: usize = 1_000_000;

/// Invalid destructive-cache configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcConfigError {
    /// The scan bound is zero or above the compiled hard maximum.
    EntryLimit,
    /// Fewer than two distinct view generations cannot establish grace.
    GenerationGrace,
    /// A configured hard quota is zero.
    Quota,
}

impl std::fmt::Display for GcConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EntryLimit => "GC entry limit is outside the supported range",
            Self::GenerationGrace => "GC grace must cover at least two distinct view generations",
            Self::Quota => "GC quotas must be greater than zero",
        })
    }
}

impl std::error::Error for GcConfigError {}

/// Bounded retention and optional hard-quota policy for one cache root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcConfig {
    max_entries: usize,
    grace_generations: u32,
    wall_grace: Duration,
    artifact_grace: Duration,
    max_logical_bytes: Option<u64>,
    max_files: Option<u64>,
}

impl GcConfig {
    /// Validates a GC policy.
    ///
    /// A quota is enforced only when explicitly configured.
    ///
    /// # Errors
    ///
    /// Returns [`GcConfigError`] for unsafe bounds or zero quotas.
    pub fn new(
        max_entries: usize,
        grace_generations: u32,
        wall_grace: Duration,
        artifact_grace: Duration,
        max_logical_bytes: Option<u64>,
        max_files: Option<u64>,
    ) -> Result<Self, GcConfigError> {
        if max_entries == 0 || max_entries > MAX_GC_ENTRIES {
            return Err(GcConfigError::EntryLimit);
        }
        if grace_generations < 2 {
            return Err(GcConfigError::GenerationGrace);
        }
        if max_logical_bytes == Some(0) || max_files == Some(0) {
            return Err(GcConfigError::Quota);
        }
        Ok(Self {
            max_entries,
            grace_generations,
            wall_grace,
            artifact_grace,
            max_logical_bytes,
            max_files,
        })
    }

    /// Maximum directory entries visited by one complete inventory.
    #[must_use]
    pub const fn max_entries(self) -> usize {
        self.max_entries
    }

    /// Distinct authoritative view generations required before deletion.
    #[must_use]
    pub const fn grace_generations(self) -> u32 {
        self.grace_generations
    }

    /// Minimum wall time from the first authoritative absence.
    #[must_use]
    pub const fn wall_grace(self) -> Duration {
        self.wall_grace
    }

    /// Minimum age for recognized abandoned temp and quarantine files.
    #[must_use]
    pub const fn artifact_grace(self) -> Duration {
        self.artifact_grace
    }

    /// Optional hard logical-byte quota for the complete namespace.
    #[must_use]
    pub const fn max_logical_bytes(self) -> Option<u64> {
        self.max_logical_bytes
    }

    /// Optional hard file-entry quota for the complete namespace.
    #[must_use]
    pub const fn max_files(self) -> Option<u64> {
        self.max_files
    }
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            grace_generations: 2,
            wall_grace: Duration::from_mins(2),
            artifact_grace: Duration::from_mins(10),
            max_logical_bytes: None,
            max_files: None,
        }
    }
}

/// Authoritative liveness input for one successfully published source view.
#[derive(Debug, Clone)]
pub struct GcMark {
    generation: u64,
    authoritative: bool,
    live: HashSet<FactBuildKey>,
}

impl GcMark {
    /// Creates a complete mark for one successful source-view generation.
    #[must_use]
    pub fn authoritative(generation: u64, live: impl IntoIterator<Item = FactBuildKey>) -> Self {
        Self {
            generation,
            authoritative: true,
            live: live.into_iter().collect(),
        }
    }

    /// Creates an explicitly non-authoritative mark.
    ///
    /// This is used when any source, snapshot, or promotion state is unknown.
    #[must_use]
    pub fn unavailable(generation: u64) -> Self {
        Self {
            generation,
            authoritative: false,
            live: HashSet::new(),
        }
    }

    /// Source-view generation represented by this mark.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the mark is complete enough to authorize grace advancement.
    #[must_use]
    pub const fn is_authoritative(&self) -> bool {
        self.authoritative
    }

    /// Number of complete live identities.
    #[must_use]
    pub fn live_len(&self) -> usize {
        self.live.len()
    }
}

/// Why an invocation made no destructive progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcSkipReason {
    /// This process could not establish the sole destructive root owner.
    OwnerUnavailable,
    /// At least one source or view component was unavailable.
    MarkUnavailable,
    /// The live set itself exceeded the configured bound.
    LiveSetCapped,
    /// The namespace could not be inventoried completely.
    ScanError,
    /// The configured entry cap was reached.
    ScanCapped,
}

/// File and byte accounting for one cache category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcCategoryUsage {
    /// Filesystem entries in the category.
    pub files: u64,
    /// Logical `st_size` bytes.
    pub logical_bytes: u64,
    /// Allocated bytes derived from `st_blocks`.
    pub allocated_bytes: u64,
}

impl GcCategoryUsage {
    const fn add(&mut self, logical_bytes: u64, allocated_bytes: u64) {
        self.files = self.files.saturating_add(1);
        self.logical_bytes = self.logical_bytes.saturating_add(logical_bytes);
        self.allocated_bytes = self.allocated_bytes.saturating_add(allocated_bytes);
    }

    const fn remove(&mut self, logical_bytes: u64, allocated_bytes: u64) {
        self.files = self.files.saturating_sub(1);
        self.logical_bytes = self.logical_bytes.saturating_sub(logical_bytes);
        self.allocated_bytes = self.allocated_bytes.saturating_sub(allocated_bytes);
    }
}

/// Complete category accounting from one successful namespace inventory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcUsage {
    /// Validated canonical committed facts.
    pub committed: GcCategoryUsage,
    /// Strictly recognized publisher temporary files.
    pub temporary: GcCategoryUsage,
    /// Strictly recognized quarantine files.
    pub quarantine: GcCategoryUsage,
    /// Owner and per-key lock files.
    pub locks: GcCategoryUsage,
    /// Anything not admitted into a known category.
    pub foreign: GcCategoryUsage,
}

impl GcUsage {
    /// Total accounted file entries.
    #[must_use]
    pub const fn total_files(self) -> u64 {
        self.committed
            .files
            .saturating_add(self.temporary.files)
            .saturating_add(self.quarantine.files)
            .saturating_add(self.locks.files)
            .saturating_add(self.foreign.files)
    }

    /// Total logical bytes.
    #[must_use]
    pub const fn total_logical_bytes(self) -> u64 {
        self.committed
            .logical_bytes
            .saturating_add(self.temporary.logical_bytes)
            .saturating_add(self.quarantine.logical_bytes)
            .saturating_add(self.locks.logical_bytes)
            .saturating_add(self.foreign.logical_bytes)
    }

    /// Total allocated bytes.
    #[must_use]
    pub const fn total_allocated_bytes(self) -> u64 {
        self.committed
            .allocated_bytes
            .saturating_add(self.temporary.allocated_bytes)
            .saturating_add(self.quarantine.allocated_bytes)
            .saturating_add(self.locks.allocated_bytes)
            .saturating_add(self.foreign.allocated_bytes)
    }
}

/// Work and accounting from one GC request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcOutcome {
    /// Directory entries visited by a complete or failed bounded scan.
    pub scanned: u64,
    /// Validated facts protected by the mark or recent publication.
    pub live: u64,
    /// Validated non-live facts still inside generation or wall grace.
    pub pending: u64,
    /// All files unlinked by this invocation.
    pub deleted: u64,
    /// Validated committed facts unlinked.
    pub deleted_finals: u64,
    /// Recognized stale temp or quarantine files unlinked.
    pub deleted_artifacts: u64,
    /// Logical bytes measured from an opened inode immediately before unlink.
    pub freed_bytes: u64,
    /// Allocated bytes measured from that same inode.
    pub freed_allocated_bytes: u64,
    /// Whether the entire namespace inventory completed.
    pub scan_complete: bool,
    /// Whether an authoritative mark permitted destructive work.
    pub sweep_authorized: bool,
    /// Explicit reason destructive work was skipped.
    pub skip_reason: Option<GcSkipReason>,
    /// Whether the post-sweep namespace still exceeds a configured hard quota.
    pub quota_exceeded: bool,
    /// Complete post-sweep category accounting, when `scan_complete` is true.
    pub usage: GcUsage,
}

#[derive(Debug, Default)]
pub(super) struct GcState {
    pending: HashMap<FactBuildKey, PendingEntry>,
    recently_published: HashMap<FactBuildKey, SystemTime>,
    last_authoritative_mark: Option<GcMark>,
    usage: Option<GcUsage>,
    quota_exceeded: bool,
}

impl GcState {
    pub(super) fn record_publication(&mut self, key: FactBuildKey, now: SystemTime) {
        self.pending.remove(&key);
        self.recently_published.insert(key, now);
    }

    #[allow(
        dead_code,
        reason = "consumed by the typed one-GC recovery path in the next logical slice"
    )]
    pub(super) fn last_authoritative_mark(&self) -> Option<GcMark> {
        self.last_authoritative_mark.clone()
    }

    #[allow(
        dead_code,
        reason = "consumed by the typed one-GC recovery path in the next logical slice"
    )]
    pub(super) const fn quota_exceeded(&self) -> bool {
        self.quota_exceeded
    }

    pub(super) fn update_usage(&mut self, usage: GcUsage, config: GcConfig) {
        self.quota_exceeded = exceeds_quota(usage, config, 0, 0);
        self.usage = Some(usage);
    }
}

#[derive(Debug)]
struct PendingEntry {
    first_absent_at: SystemTime,
    observed_generations: u32,
    last_generation: u64,
}

#[derive(Debug)]
struct Inventory {
    scanned: u64,
    usage: GcUsage,
    finals: Vec<FinalCandidate>,
    artifacts: Vec<ArtifactCandidate>,
}

#[derive(Debug)]
struct FinalCandidate {
    scope_name: String,
    prefix_name: String,
    final_name: String,
    key: FactBuildKey,
    device: u64,
    inode: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactKind {
    Temporary,
    Quarantine,
}

#[derive(Debug)]
struct ArtifactCandidate {
    scope_name: String,
    prefix_name: String,
    name: String,
    kind: ArtifactKind,
    device: u64,
    inode: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    modified: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanError {
    Capped,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GcAdmissionError {
    Capped,
    Incomplete,
}

/// Performs one bounded mark/sweep under the caller's root-owner and
/// publication gate.
pub(super) fn collect(
    namespace: &File,
    mark: &GcMark,
    state: &mut GcState,
    config: GcConfig,
    now: SystemTime,
) -> GcOutcome {
    if !mark.authoritative {
        return GcOutcome {
            skip_reason: Some(GcSkipReason::MarkUnavailable),
            ..GcOutcome::default()
        };
    }
    if mark.live.len() > config.max_entries {
        return GcOutcome {
            skip_reason: Some(GcSkipReason::LiveSetCapped),
            ..GcOutcome::default()
        };
    }

    let inventory = match scan_namespace(namespace, config.max_entries) {
        Ok(inventory) => inventory,
        Err((ScanError::Capped, scanned)) => {
            return GcOutcome {
                scanned,
                skip_reason: Some(GcSkipReason::ScanCapped),
                ..GcOutcome::default()
            };
        }
        Err((ScanError::Io, scanned)) => {
            return GcOutcome {
                scanned,
                skip_reason: Some(GcSkipReason::ScanError),
                ..GcOutcome::default()
            };
        }
    };

    state.last_authoritative_mark = Some(mark.clone());
    let mut outcome = GcOutcome {
        scanned: inventory.scanned,
        scan_complete: true,
        sweep_authorized: true,
        usage: inventory.usage,
        ..GcOutcome::default()
    };

    delete_stale_artifacts(namespace, &inventory, config, now, &mut outcome);

    let seen: HashSet<_> = inventory
        .finals
        .iter()
        .map(|candidate| candidate.key)
        .collect();
    state.pending.retain(|key, _entry| seen.contains(key));
    state.recently_published.retain(|key, published_at| {
        seen.contains(key)
            && elapsed_since(*published_at, now).is_some_and(|age| age < config.wall_grace)
    });

    for candidate in &inventory.finals {
        if mark.live.contains(&candidate.key)
            || state.recently_published.contains_key(&candidate.key)
        {
            outcome.live = outcome.live.saturating_add(1);
            state.pending.remove(&candidate.key);
            continue;
        }

        let pending = state.pending.entry(candidate.key).or_insert(PendingEntry {
            first_absent_at: now,
            observed_generations: 0,
            last_generation: mark.generation,
        });
        if pending.observed_generations == 0 || pending.last_generation != mark.generation {
            pending.observed_generations = pending.observed_generations.saturating_add(1);
            pending.last_generation = mark.generation;
        }
        let old_enough =
            elapsed_since(pending.first_absent_at, now).is_some_and(|age| age >= config.wall_grace);
        if pending.observed_generations < config.grace_generations || !old_enough {
            outcome.pending = outcome.pending.saturating_add(1);
            continue;
        }

        match delete_final(namespace, candidate) {
            Ok(Some((logical_bytes, allocated_bytes))) => {
                outcome.deleted = outcome.deleted.saturating_add(1);
                outcome.deleted_finals = outcome.deleted_finals.saturating_add(1);
                outcome.freed_bytes = outcome.freed_bytes.saturating_add(logical_bytes);
                outcome.freed_allocated_bytes = outcome
                    .freed_allocated_bytes
                    .saturating_add(allocated_bytes);
                outcome
                    .usage
                    .committed
                    .remove(logical_bytes, allocated_bytes);
                state.pending.remove(&candidate.key);
            }
            Ok(None) | Err(()) => {
                outcome.pending = outcome.pending.saturating_add(1);
            }
        }
    }

    outcome.quota_exceeded = exceeds_quota(outcome.usage, config, 0, 0);
    state.update_usage(outcome.usage, config);
    outcome
}

/// Completes exact namespace accounting and checks publication peak usage.
pub(super) fn admit_publication(
    namespace: &File,
    config: GcConfig,
    incoming_logical_bytes: u64,
) -> Result<GcUsage, GcAdmissionError> {
    let inventory =
        scan_namespace(namespace, config.max_entries).map_err(|(error, _scanned)| match error {
            ScanError::Capped => GcAdmissionError::Capped,
            ScanError::Io => GcAdmissionError::Incomplete,
        })?;
    if exceeds_quota(inventory.usage, config, incoming_logical_bytes, 1) {
        Err(GcAdmissionError::Capped)
    } else {
        Ok(inventory.usage)
    }
}

fn exceeds_quota(
    usage: GcUsage,
    config: GcConfig,
    additional_logical_bytes: u64,
    additional_files: u64,
) -> bool {
    let logical = usage
        .total_logical_bytes()
        .checked_add(additional_logical_bytes);
    let files = usage.total_files().checked_add(additional_files);
    config
        .max_logical_bytes
        .is_some_and(|limit| logical.is_none_or(|value| value > limit))
        || config
            .max_files
            .is_some_and(|limit| files.is_none_or(|value| value > limit))
}

fn scan_namespace(namespace: &File, max_entries: usize) -> Result<Inventory, (ScanError, u64)> {
    let mut inventory = Inventory {
        scanned: 0,
        usage: GcUsage::default(),
        finals: Vec::new(),
        artifacts: Vec::new(),
    };
    match scan_scopes(namespace, max_entries, &mut inventory) {
        Ok(()) => Ok(inventory),
        Err(error) => Err((error, inventory.scanned)),
    }
}

fn scan_scopes(
    namespace: &File,
    max_entries: usize,
    inventory: &mut Inventory,
) -> Result<(), ScanError> {
    for entry in directory_entries(namespace)? {
        let entry = entry.map_err(|_error| ScanError::Io)?;
        let name = entry.file_name();
        if is_dot_entry(name) {
            continue;
        }
        observe(inventory, max_entries)?;
        let stat = stat_entry(namespace, name)?;
        if name.to_bytes() == OWNER_LOCK_NAME.as_bytes() {
            account(&mut inventory.usage.locks, &stat);
            continue;
        }
        let Some(scope_name) = canonical_component(name, 64) else {
            account(&mut inventory.usage.foreign, &stat);
            if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                return Err(ScanError::Io);
            }
            continue;
        };
        if parse_hex_32(&scope_name).is_none()
            || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        {
            account(&mut inventory.usage.foreign, &stat);
            continue;
        }
        let scope =
            open_child_directory(namespace, &scope_name, false).map_err(|_error| ScanError::Io)?;
        scan_prefixes(&scope, &scope_name, max_entries, inventory)?;
    }
    Ok(())
}

fn scan_prefixes(
    scope: &File,
    scope_name: &str,
    max_entries: usize,
    inventory: &mut Inventory,
) -> Result<(), ScanError> {
    for entry in directory_entries(scope)? {
        let entry = entry.map_err(|_error| ScanError::Io)?;
        let name = entry.file_name();
        if is_dot_entry(name) {
            continue;
        }
        observe(inventory, max_entries)?;
        let stat = stat_entry(scope, name)?;
        let Some(prefix_name) = canonical_component(name, 2) else {
            account(&mut inventory.usage.foreign, &stat);
            if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                return Err(ScanError::Io);
            }
            continue;
        };
        if !is_lower_hex(&prefix_name)
            || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        {
            account(&mut inventory.usage.foreign, &stat);
            continue;
        }
        let prefix =
            open_child_directory(scope, &prefix_name, false).map_err(|_error| ScanError::Io)?;
        scan_files(&prefix, scope_name, &prefix_name, max_entries, inventory)?;
    }
    Ok(())
}

fn scan_files(
    directory: &File,
    scope_name: &str,
    prefix_name: &str,
    max_entries: usize,
    inventory: &mut Inventory,
) -> Result<(), ScanError> {
    let scope = SourceScopeId(parse_hex_32(scope_name).ok_or(ScanError::Io)?);
    for entry in directory_entries(directory)? {
        let entry = entry.map_err(|_error| ScanError::Io)?;
        let name = entry.file_name();
        if is_dot_entry(name) {
            continue;
        }
        observe(inventory, max_entries)?;
        let stat = stat_entry(directory, name)?;
        let Some(name) = name.to_str().ok().map(ToOwned::to_owned) else {
            account(&mut inventory.usage.foreign, &stat);
            continue;
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            account(&mut inventory.usage.foreign, &stat);
            if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
                return Err(ScanError::Io);
            }
            continue;
        }

        if let Some(key) = FactBuildKey::from_final_name(&name)
            && key.fact_key().prefix() == prefix_name
            && let Ok(candidate) =
                validate_final(directory, scope_name, prefix_name, &name, key, scope)
        {
            inventory
                .usage
                .committed
                .add(candidate.logical_bytes, candidate.allocated_bytes);
            inventory.finals.push(candidate);
            continue;
        }

        if let Some(kind) = artifact_kind(&name, prefix_name)
            && let Ok(candidate) =
                validate_artifact(directory, scope_name, prefix_name, &name, kind)
        {
            match kind {
                ArtifactKind::Temporary => inventory
                    .usage
                    .temporary
                    .add(candidate.logical_bytes, candidate.allocated_bytes),
                ArtifactKind::Quarantine => inventory
                    .usage
                    .quarantine
                    .add(candidate.logical_bytes, candidate.allocated_bytes),
            }
            inventory.artifacts.push(candidate);
            continue;
        }

        if is_lock_name(&name, prefix_name) {
            account(&mut inventory.usage.locks, &stat);
        } else {
            account(&mut inventory.usage.foreign, &stat);
        }
    }
    Ok(())
}

fn validate_final(
    directory: &File,
    scope_name: &str,
    prefix_name: &str,
    final_name: &str,
    key: FactBuildKey,
    scope: SourceScopeId,
) -> Result<FinalCandidate, ()> {
    let mut file = open_regular_at(directory, final_name).map_err(|_error| ())?;
    let metadata = file.metadata().map_err(|_error| ())?;
    let mut header = [0_u8; HEADER_LEN];
    file.read_exact(&mut header).map_err(|_error| ())?;
    let identity = identity_from_header(&header).ok_or(())?;
    let reader = FactFileReader::open(file, &identity, &LIMIT).map_err(|_error| ())?;
    if reader.header().identity.source_scope_id != scope
        || FactKey::for_identity(&identity, FileKind::SegmentFacts) != key.fact_key()
    {
        return Err(());
    }
    Ok(FinalCandidate {
        scope_name: scope_name.to_owned(),
        prefix_name: prefix_name.to_owned(),
        final_name: final_name.to_owned(),
        key,
        device: metadata.dev(),
        inode: metadata.ino(),
        logical_bytes: metadata.len(),
        allocated_bytes: metadata.blocks().saturating_mul(512),
    })
}

fn validate_artifact(
    directory: &File,
    scope_name: &str,
    prefix_name: &str,
    name: &str,
    kind: ArtifactKind,
) -> Result<ArtifactCandidate, ()> {
    let file = open_regular_at(directory, name).map_err(|_error| ())?;
    let metadata = file.metadata().map_err(|_error| ())?;
    Ok(ArtifactCandidate {
        scope_name: scope_name.to_owned(),
        prefix_name: prefix_name.to_owned(),
        name: name.to_owned(),
        kind,
        device: metadata.dev(),
        inode: metadata.ino(),
        logical_bytes: metadata.len(),
        allocated_bytes: metadata.blocks().saturating_mul(512),
        modified: metadata.modified().map_err(|_error| ())?,
    })
}

fn delete_stale_artifacts(
    namespace: &File,
    inventory: &Inventory,
    config: GcConfig,
    now: SystemTime,
    outcome: &mut GcOutcome,
) {
    for artifact in &inventory.artifacts {
        let old_enough =
            elapsed_since(artifact.modified, now).is_some_and(|age| age >= config.artifact_grace);
        if !old_enough {
            continue;
        }
        let Ok(scope) = open_child_directory(namespace, &artifact.scope_name, false) else {
            continue;
        };
        let Ok(directory) = open_child_directory(&scope, &artifact.prefix_name, false) else {
            continue;
        };
        let Ok(file) = open_regular_at(&directory, &artifact.name) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if metadata.dev() != artifact.device || metadata.ino() != artifact.inode {
            continue;
        }
        if rustix::fs::unlinkat(&directory, &artifact.name, AtFlags::empty()).is_err() {
            continue;
        }
        let logical_bytes = metadata.len();
        let allocated_bytes = metadata.blocks().saturating_mul(512);
        outcome.deleted = outcome.deleted.saturating_add(1);
        outcome.deleted_artifacts = outcome.deleted_artifacts.saturating_add(1);
        outcome.freed_bytes = outcome.freed_bytes.saturating_add(logical_bytes);
        outcome.freed_allocated_bytes = outcome
            .freed_allocated_bytes
            .saturating_add(allocated_bytes);
        match artifact.kind {
            ArtifactKind::Temporary => outcome
                .usage
                .temporary
                .remove(logical_bytes, allocated_bytes),
            ArtifactKind::Quarantine => outcome
                .usage
                .quarantine
                .remove(logical_bytes, allocated_bytes),
        }
    }
}

fn delete_final(namespace: &File, candidate: &FinalCandidate) -> Result<Option<(u64, u64)>, ()> {
    let scope =
        open_child_directory(namespace, &candidate.scope_name, false).map_err(|_error| ())?;
    let directory =
        open_child_directory(&scope, &candidate.prefix_name, false).map_err(|_error| ())?;
    let lock_name = format!(
        ".lock-{}-{}",
        candidate.key.fact_key().hex(),
        hex(&candidate.key.segment_lineage_id().0)
    );
    let lock = open_file_at(
        &directory,
        &lock_name,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        FILE_MODE,
    )
    .map_err(|_error| ())?;
    rustix::fs::fchmod(&lock, FILE_MODE).map_err(|_error| ())?;
    match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => return Ok(None),
        Err(_) => return Err(()),
    }

    let scope_id = SourceScopeId(parse_hex_32(&candidate.scope_name).ok_or(())?);
    let reopened = validate_final(
        &directory,
        &candidate.scope_name,
        &candidate.prefix_name,
        &candidate.final_name,
        candidate.key,
        scope_id,
    )?;
    if reopened.device != candidate.device || reopened.inode != candidate.inode {
        return Ok(None);
    }
    let file = open_regular_at(&directory, &candidate.final_name).map_err(|_error| ())?;
    let metadata = file.metadata().map_err(|_error| ())?;
    if metadata.dev() != candidate.device || metadata.ino() != candidate.inode {
        return Ok(None);
    }
    rustix::fs::unlinkat(&directory, &candidate.final_name, AtFlags::empty())
        .map_err(|_error| ())?;
    directory.sync_all().map_err(|_error| ())?;
    Ok(Some((
        metadata.len(),
        metadata.blocks().saturating_mul(512),
    )))
}

fn directory_entries(
    directory: &File,
) -> Result<impl Iterator<Item = rustix::io::Result<rustix::fs::DirEntry>>, ScanError> {
    rustix::fs::Dir::read_from(directory).map_err(|_error| ScanError::Io)
}

fn observe(inventory: &mut Inventory, max_entries: usize) -> Result<(), ScanError> {
    inventory.scanned = inventory.scanned.saturating_add(1);
    if inventory.scanned > u64::try_from(max_entries).unwrap_or(u64::MAX) {
        Err(ScanError::Capped)
    } else {
        Ok(())
    }
}

fn stat_entry(directory: &File, name: &CStr) -> Result<rustix::fs::Stat, ScanError> {
    rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_error| ScanError::Io)
}

fn account(category: &mut GcCategoryUsage, stat: &rustix::fs::Stat) {
    let logical_bytes = u64::try_from(stat.st_size).unwrap_or(0);
    let blocks = u64::try_from(stat.st_blocks).unwrap_or(0);
    category.add(logical_bytes, blocks.saturating_mul(512));
}

fn canonical_component(name: &CStr, length: usize) -> Option<String> {
    let name = name.to_str().ok()?;
    (name.len() == length && is_lower_hex(name)).then(|| name.to_owned())
}

fn is_dot_entry(name: &CStr) -> bool {
    matches!(name.to_bytes(), b"." | b"..")
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn artifact_kind(name: &str, prefix: &str) -> Option<ArtifactKind> {
    for (tag, kind) in [
        (".tmp-", ArtifactKind::Temporary),
        (".bad-", ArtifactKind::Quarantine),
    ] {
        let Some(tail) = name.strip_prefix(tag) else {
            continue;
        };
        let mut parts = tail.split('-');
        let pid = parts.next()?;
        let sequence = parts.next()?;
        let named_prefix = parts.next()?;
        if parts.next().is_none()
            && !pid.is_empty()
            && pid.bytes().all(|byte| byte.is_ascii_digit())
            && !sequence.is_empty()
            && sequence.bytes().all(|byte| byte.is_ascii_digit())
            && named_prefix == prefix
        {
            return Some(kind);
        }
    }
    None
}

fn is_lock_name(name: &str, prefix: &str) -> bool {
    let Some(tail) = name.strip_prefix(".lock-") else {
        return false;
    };
    let Some((key, lineage)) = tail.split_once('-') else {
        return false;
    };
    tail.matches('-').count() == 1
        && FactKey::from_hex(key).is_some_and(|key| key.prefix() == prefix)
        && parse_hex_32(lineage).is_some()
}

fn open_regular_at(directory: &File, name: &str) -> io::Result<File> {
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

fn identity_from_header(header: &[u8; HEADER_LEN]) -> Option<HeaderIdentity> {
    Some(HeaderIdentity {
        fact_schema_version: u32_at(header, 16)?,
        extractor_semantics_version: u32_at(header, 20)?,
        registry_contract_version: u32_at(header, 24)?,
        source_format_version: u32_at(header, 28)?,
        pgm_source_id: u64_at(header, 32)?,
        source_min_ts_us: i64_at(header, 40)?,
        source_max_ts_us: i64_at(header, 48)?,
        source_file_len: u64_at(header, 56)?,
        source_scope_id: SourceScopeId(header.get(64..96)?.try_into().ok()?),
        source_descriptor: SourceDescriptor(header.get(96..128)?.try_into().ok()?),
    })
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn i64_at(bytes: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn elapsed_since(earlier: SystemTime, now: SystemTime) -> Option<Duration> {
    now.duration_since(earlier).ok()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests;
