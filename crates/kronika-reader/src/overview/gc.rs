//! Fail-closed retention and accounting for sibling overview sidecars.
//!
//! The collector scans the PgKronika-owned data directory without following
//! symlinks. It recognizes `.ovf` sidecars and publisher artifacts only in
//! their verified `YYYY/MM/DD` directories, completes a bounded inventory, and
//! only then advances grace or unlinks anything.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::time::{Duration, SystemTime};

use kronika_analytics::overview::SegmentLineageId;
use kronika_layout::{
    LayoutError, LayoutLimits, OverviewOwner, SegmentAddress, TemporaryKind, TemporaryObject,
    UtcDay,
};

use super::container::{FactFileReader, HeaderIdentity};
use super::descriptors::SourceDescriptor;
use super::factkey::{FactBuildKey, FactKey};
use super::limits::LIMIT;

const HEADER_LEN: usize = 192;
const MAX_GC_ENTRIES: usize = 1_000_000;

/// Invalid sidecar-retention configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcConfigError {
    /// The scan bound is zero or above the compiled hard maximum.
    EntryLimit,
    /// Fewer than two distinct authoritative generations cannot establish grace.
    GenerationGrace,
    /// A configured hard quota is zero.
    Quota,
}

impl std::fmt::Display for GcConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EntryLimit => "GC entry limit is outside the supported range",
            Self::GenerationGrace => {
                "GC grace must cover at least two distinct authoritative scan generations"
            }
            Self::Quota => "GC quotas must be greater than zero",
        })
    }
}

impl std::error::Error for GcConfigError {}

/// Bounded retention and optional byte/file policy for derived sidecars.
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
    /// Validates a retention policy. A quota is enforced only when configured.
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

    /// Maximum data-directory entries visited by one inventory.
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

    /// Minimum age for an abandoned publisher artifact or invalid sidecar.
    #[must_use]
    pub const fn artifact_grace(self) -> Duration {
        self.artifact_grace
    }

    /// Optional logical-byte quota for sidecars and publication artifacts.
    #[must_use]
    pub const fn max_logical_bytes(self) -> Option<u64> {
        self.max_logical_bytes
    }

    /// Optional file quota for sidecars and publication artifacts.
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
    #[must_use]
    pub fn unavailable(generation: u64) -> Self {
        Self {
            generation,
            authoritative: false,
            live: HashSet::new(),
        }
    }

    /// Source-view generation observed by this GC scan.
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
    /// This process could not establish the sole sidecar writer.
    OwnerUnavailable,
    /// At least one source or view component was unavailable.
    MarkUnavailable,
    /// The live set itself exceeded the configured bound.
    LiveSetCapped,
    /// The data directory could not be inventoried completely.
    ScanError,
    /// The configured entry cap was reached.
    ScanCapped,
}

/// File and byte accounting for one derived-file category.
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

/// Complete derived-file accounting from one successful bounded tree scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcUsage {
    /// Header-admitted sibling `.ovf` files.
    pub sidecars: GcCategoryUsage,
    /// Publisher temporary files and invalid `.ovf` files awaiting cleanup.
    pub temporary: GcCategoryUsage,
    /// The data-directory owner lock.
    pub locks: GcCategoryUsage,
}

impl GcUsage {
    /// Total accounted derived file entries.
    #[must_use]
    pub const fn total_files(self) -> u64 {
        self.sidecars
            .files
            .saturating_add(self.temporary.files)
            .saturating_add(self.locks.files)
    }

    /// Total logical bytes of accounted derived files.
    #[must_use]
    pub const fn total_logical_bytes(self) -> u64 {
        self.sidecars
            .logical_bytes
            .saturating_add(self.temporary.logical_bytes)
            .saturating_add(self.locks.logical_bytes)
    }

    /// Total allocated bytes of accounted derived files.
    #[must_use]
    pub const fn total_allocated_bytes(self) -> u64 {
        self.sidecars
            .allocated_bytes
            .saturating_add(self.temporary.allocated_bytes)
            .saturating_add(self.locks.allocated_bytes)
    }
}

/// Work and accounting from one retention request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcOutcome {
    /// Data-directory entries visited by the bounded scan.
    pub scanned: u64,
    /// Validated sidecars protected by the mark or recent publication.
    pub live: u64,
    /// Validated non-live sidecars still inside generation or wall grace.
    pub pending: u64,
    /// All derived files unlinked by this invocation.
    pub deleted: u64,
    /// Validated sidecars unlinked.
    pub deleted_sidecars: u64,
    /// Stale publisher artifacts or invalid sidecars unlinked.
    pub deleted_artifacts: u64,
    /// Logical bytes measured from opened inodes that were then unlinked.
    pub unlinked_logical_bytes: u64,
    /// Allocated bytes measured from those same unlinked inodes.
    pub unlinked_allocated_bytes: u64,
    /// Whether the complete owned-tree scan finished.
    pub scan_complete: bool,
    /// Whether an authoritative mark permitted destructive work.
    pub sweep_authorized: bool,
    /// Explicit reason destructive work was skipped.
    pub skip_reason: Option<GcSkipReason>,
    /// Whether derived files still exceed a configured hard quota.
    pub quota_exceeded: bool,
    /// Complete post-sweep derived-file accounting.
    pub usage: GcUsage,
}

#[derive(Debug, Default)]
pub(super) struct GcState {
    pending: HashMap<FactBuildKey, PendingEntry>,
    recently_published: HashMap<FactBuildKey, SystemTime>,
    last_authoritative_mark: Option<GcMark>,
    quota: Option<QuotaState>,
}

impl GcState {
    pub(super) fn record_cache_hit(&mut self, key: FactBuildKey, now: SystemTime) {
        self.pending.remove(&key);
        self.recently_published.insert(key, now);
    }

    pub(super) fn record_publication(
        &mut self,
        address: SegmentAddress,
        key: FactBuildKey,
        usage: FileUsage,
        now: SystemTime,
    ) {
        self.record_cache_hit(key, now);
        let Some(quota) = self.quota.as_mut() else {
            return;
        };
        if let Some(previous) = quota.sidecars.remove(&address) {
            quota
                .usage
                .sidecars
                .remove(previous.logical_bytes, previous.allocated_bytes);
        }
        if let Some(previous) = quota.invalid_overviews.remove(&address) {
            quota
                .usage
                .temporary
                .remove(previous.logical_bytes, previous.allocated_bytes);
        }
        quota
            .usage
            .sidecars
            .add(usage.logical_bytes, usage.allocated_bytes);
        quota.sidecars.insert(address, usage);
    }

    pub(super) fn last_authoritative_mark(&self) -> Option<GcMark> {
        self.last_authoritative_mark.clone()
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
    days: Vec<UtcDay>,
    sidecars: Vec<SidecarCandidate>,
    artifacts: Vec<ArtifactCandidate>,
}

impl Inventory {
    fn quota_state(&self) -> QuotaState {
        let sidecars = self
            .sidecars
            .iter()
            .map(|candidate| (candidate.address, candidate.usage))
            .collect();
        let invalid_overviews = self
            .artifacts
            .iter()
            .filter_map(|candidate| match candidate.location {
                ArtifactLocation::Overview(address) => Some((address, candidate.usage)),
                ArtifactLocation::Temporary(_) => None,
            })
            .collect();
        QuotaState {
            usage: self.usage,
            sidecars,
            invalid_overviews,
        }
    }
}

#[derive(Debug)]
struct SidecarCandidate {
    address: SegmentAddress,
    key: FactBuildKey,
    device: u64,
    inode: u64,
    usage: FileUsage,
}

#[derive(Debug)]
struct ArtifactCandidate {
    location: ArtifactLocation,
    device: u64,
    inode: u64,
    usage: FileUsage,
    modified: SystemTime,
}

#[derive(Debug, Clone)]
enum ArtifactLocation {
    Overview(SegmentAddress),
    Temporary(TemporaryObject),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FileUsage {
    pub(super) logical_bytes: u64,
    pub(super) allocated_bytes: u64,
}

#[derive(Debug, Default)]
struct QuotaState {
    usage: GcUsage,
    sidecars: HashMap<SegmentAddress, FileUsage>,
    invalid_overviews: HashMap<SegmentAddress, FileUsage>,
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

/// Performs one bounded mark/sweep under the caller's owner and publication gate.
pub(super) fn collect(
    owner: &OverviewOwner,
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

    let inventory = match scan_directory(owner, config.max_entries) {
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

    let mut quota = inventory.quota_state();
    let prune_days = inventory_days(&inventory);
    state.last_authoritative_mark = Some(mark.clone());
    let mut outcome = GcOutcome {
        scanned: inventory.scanned,
        scan_complete: true,
        sweep_authorized: true,
        usage: inventory.usage,
        ..GcOutcome::default()
    };

    delete_stale_artifacts(owner, &inventory, config, now, &mut outcome, &mut quota);

    let seen: HashSet<_> = inventory
        .sidecars
        .iter()
        .map(|candidate| candidate.key)
        .collect();
    state.pending.retain(|key, _entry| seen.contains(key));
    state.recently_published.retain(|key, published_at| {
        seen.contains(key)
            && elapsed_since(*published_at, now).is_some_and(|age| age < config.wall_grace)
    });

    for candidate in &inventory.sidecars {
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

        match delete_sidecar(owner, candidate) {
            Ok(Some((logical_bytes, allocated_bytes))) => {
                outcome.deleted = outcome.deleted.saturating_add(1);
                outcome.deleted_sidecars = outcome.deleted_sidecars.saturating_add(1);
                outcome.unlinked_logical_bytes =
                    outcome.unlinked_logical_bytes.saturating_add(logical_bytes);
                outcome.unlinked_allocated_bytes = outcome
                    .unlinked_allocated_bytes
                    .saturating_add(allocated_bytes);
                outcome
                    .usage
                    .sidecars
                    .remove(logical_bytes, allocated_bytes);
                quota.usage.sidecars.remove(logical_bytes, allocated_bytes);
                quota.sidecars.remove(&candidate.address);
                state.pending.remove(&candidate.key);
            }
            Ok(None) | Err(()) => outcome.pending = outcome.pending.saturating_add(1),
        }
    }

    outcome.quota_exceeded = exceeds_quota(outcome.usage, config, 0, 0);
    quota.usage = outcome.usage;
    state.quota = Some(quota);
    for day in prune_days {
        owner.prune_empty_day(day).ok();
    }
    outcome
}

fn inventory_days(inventory: &Inventory) -> HashSet<UtcDay> {
    inventory
        .days
        .iter()
        .copied()
        .chain(
            inventory
                .sidecars
                .iter()
                .map(|candidate| candidate.address.day),
        )
        .chain(
            inventory
                .artifacts
                .iter()
                .map(|candidate| match &candidate.location {
                    ArtifactLocation::Overview(address) => address.day,
                    ArtifactLocation::Temporary(temporary) => temporary.address.day,
                }),
        )
        .collect()
}

/// Initializes quota accounting once per owner lifetime and checks the
/// temporary-file publication peak.
pub(super) fn admit_publication(
    owner: &OverviewOwner,
    state: &mut GcState,
    config: GcConfig,
    incoming_logical_bytes: u64,
) -> Result<(), GcAdmissionError> {
    if state.quota.is_none() {
        let inventory =
            scan_directory(owner, config.max_entries).map_err(|(error, _scanned)| match error {
                ScanError::Capped => GcAdmissionError::Capped,
                ScanError::Io => GcAdmissionError::Incomplete,
            })?;
        state.quota = Some(inventory.quota_state());
    }
    let usage = state
        .quota
        .as_ref()
        .expect("quota inventory was initialized")
        .usage;
    if exceeds_quota(usage, config, incoming_logical_bytes, 1) {
        Err(GcAdmissionError::Capped)
    } else {
        Ok(())
    }
}

fn exceeds_quota(
    usage: GcUsage,
    config: GcConfig,
    additional_logical_bytes: u64,
    additional_files: u64,
) -> bool {
    let logical = usage
        .sidecars
        .logical_bytes
        .saturating_add(usage.temporary.logical_bytes)
        .checked_add(additional_logical_bytes);
    let files = usage
        .sidecars
        .files
        .saturating_add(usage.temporary.files)
        .checked_add(additional_files);
    config
        .max_logical_bytes
        .is_some_and(|limit| logical.is_none_or(|value| value > limit))
        || config
            .max_files
            .is_some_and(|limit| files.is_none_or(|value| value > limit))
}

fn scan_directory(
    owner: &OverviewOwner,
    max_entries: usize,
) -> Result<Inventory, (ScanError, u64)> {
    let limits = LayoutLimits {
        max_visited_entries: max_entries,
        max_entries_per_day: max_entries,
        max_segments: max_entries,
        ..LayoutLimits::default()
    };
    let snapshot = owner
        .root()
        .scan(limits)
        .map_err(|error| (scan_error(&error), 0))?;
    let mut inventory = Inventory {
        scanned: u64::try_from(snapshot.visited_entries).unwrap_or(u64::MAX),
        usage: GcUsage::default(),
        days: snapshot.days,
        sidecars: Vec::new(),
        artifacts: Vec::new(),
    };

    for segment in snapshot.segments {
        if segment.ovf_bytes.is_none() {
            continue;
        }
        let file = owner
            .root()
            .open_ovf(segment.address)
            .map_err(|_error| (ScanError::Io, inventory.scanned))?
            .ok_or((ScanError::Io, inventory.scanned))?;
        if let Ok(candidate) = validate_sidecar(file, segment.address) {
            inventory.usage.sidecars.add(
                candidate.usage.logical_bytes,
                candidate.usage.allocated_bytes,
            );
            inventory.sidecars.push(candidate);
        } else {
            let file = owner
                .root()
                .open_ovf(segment.address)
                .map_err(|_error| (ScanError::Io, inventory.scanned))?
                .ok_or((ScanError::Io, inventory.scanned))?;
            let candidate = validate_artifact(&file, ArtifactLocation::Overview(segment.address))
                .map_err(|()| (ScanError::Io, inventory.scanned))?;
            inventory.usage.temporary.add(
                candidate.usage.logical_bytes,
                candidate.usage.allocated_bytes,
            );
            inventory.artifacts.push(candidate);
        }
    }

    for address in snapshot.orphan_overviews {
        let file = owner
            .root()
            .open_ovf(address)
            .map_err(|_error| (ScanError::Io, inventory.scanned))?
            .ok_or((ScanError::Io, inventory.scanned))?;
        let candidate = validate_artifact(&file, ArtifactLocation::Overview(address))
            .map_err(|()| (ScanError::Io, inventory.scanned))?;
        inventory.usage.temporary.add(
            candidate.usage.logical_bytes,
            candidate.usage.allocated_bytes,
        );
        inventory.artifacts.push(candidate);
    }

    for temporary in snapshot.temporaries {
        if temporary.kind == TemporaryKind::Pgm {
            continue;
        }
        let file = owner
            .root()
            .open_temporary(&temporary)
            .map_err(|_error| (ScanError::Io, inventory.scanned))?;
        let candidate = validate_artifact(&file, ArtifactLocation::Temporary(temporary))
            .map_err(|()| (ScanError::Io, inventory.scanned))?;
        inventory.usage.temporary.add(
            candidate.usage.logical_bytes,
            candidate.usage.allocated_bytes,
        );
        inventory.artifacts.push(candidate);
    }

    Ok(inventory)
}

fn validate_sidecar(mut file: File, address: SegmentAddress) -> Result<SidecarCandidate, ()> {
    let metadata = file.metadata().map_err(|_error| ())?;
    let mut header = [0_u8; HEADER_LEN];
    file.read_exact(&mut header).map_err(|_error| ())?;
    let identity = identity_from_header(&header).ok_or(())?;
    let reader = FactFileReader::open(file, &identity, &LIMIT).map_err(|_error| ())?;
    let admitted = reader.header().identity;
    let key = FactBuildKey::new(admitted.fact_key, admitted.segment_lineage_id);
    Ok(SidecarCandidate {
        address,
        key,
        device: metadata.dev(),
        inode: metadata.ino(),
        usage: file_usage(&metadata),
    })
}

fn validate_artifact(file: &File, location: ArtifactLocation) -> Result<ArtifactCandidate, ()> {
    let metadata = file.metadata().map_err(|_error| ())?;
    Ok(ArtifactCandidate {
        location,
        device: metadata.dev(),
        inode: metadata.ino(),
        usage: file_usage(&metadata),
        modified: metadata.modified().map_err(|_error| ())?,
    })
}

fn delete_stale_artifacts(
    owner: &OverviewOwner,
    inventory: &Inventory,
    config: GcConfig,
    now: SystemTime,
    outcome: &mut GcOutcome,
    quota: &mut QuotaState,
) {
    for artifact in &inventory.artifacts {
        let old_enough =
            elapsed_since(artifact.modified, now).is_some_and(|age| age >= config.artifact_grace);
        if !old_enough {
            continue;
        }
        let removed = match &artifact.location {
            ArtifactLocation::Overview(address) => {
                owner.remove_ovf_if_identity(*address, artifact.device, artifact.inode)
            }
            ArtifactLocation::Temporary(temporary) => {
                owner.remove_temporary_if_identity(temporary, artifact.device, artifact.inode)
            }
        };
        if !matches!(removed, Ok(true)) {
            continue;
        }
        let logical_bytes = artifact.usage.logical_bytes;
        let allocated_bytes = artifact.usage.allocated_bytes;
        outcome.deleted = outcome.deleted.saturating_add(1);
        outcome.deleted_artifacts = outcome.deleted_artifacts.saturating_add(1);
        outcome.unlinked_logical_bytes =
            outcome.unlinked_logical_bytes.saturating_add(logical_bytes);
        outcome.unlinked_allocated_bytes = outcome
            .unlinked_allocated_bytes
            .saturating_add(allocated_bytes);
        outcome
            .usage
            .temporary
            .remove(logical_bytes, allocated_bytes);
        quota.usage.temporary.remove(logical_bytes, allocated_bytes);
        if let ArtifactLocation::Overview(address) = &artifact.location {
            quota.invalid_overviews.remove(address);
        }
    }
}

fn delete_sidecar(
    owner: &OverviewOwner,
    candidate: &SidecarCandidate,
) -> Result<Option<(u64, u64)>, ()> {
    let file = owner
        .root()
        .open_ovf(candidate.address)
        .map_err(|_error| ())?
        .ok_or(())?;
    let reopened = validate_sidecar(file, candidate.address)?;
    if reopened.key != candidate.key
        || reopened.device != candidate.device
        || reopened.inode != candidate.inode
    {
        return Ok(None);
    }
    if !owner
        .remove_ovf_if_identity(candidate.address, candidate.device, candidate.inode)
        .map_err(|_error| ())?
    {
        return Ok(None);
    }
    Ok(Some((
        candidate.usage.logical_bytes,
        candidate.usage.allocated_bytes,
    )))
}

const fn scan_error(error: &LayoutError) -> ScanError {
    match error {
        LayoutError::InvalidLimits { .. } | LayoutError::TraversalLimitExceeded { .. } => {
            ScanError::Capped
        }
        _ => ScanError::Io,
    }
}

fn file_usage(metadata: &std::fs::Metadata) -> FileUsage {
    FileUsage {
        logical_bytes: metadata.len(),
        allocated_bytes: metadata.blocks().saturating_mul(512),
    }
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
        source_descriptor: SourceDescriptor(header.get(64..96)?.try_into().ok()?),
        fact_key: FactKey::from_bytes(header.get(96..128)?.try_into().ok()?),
        segment_lineage_id: SegmentLineageId(header.get(128..160)?.try_into().ok()?),
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

#[cfg(test)]
mod tests;
