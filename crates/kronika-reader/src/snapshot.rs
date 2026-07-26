//! Read view over sealed files and active journal parts.
//!
//! Combines `LocalDir`'s sealed `.pgm` segments and `active.parts` journal into
//! one list, suppresses exact sealed/live duplicates, and decodes both through
//! `PgmUnit`.

use std::io::{self, Read as _};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kronika_format::{
    Catalog, DamageRegion, ENTRY_LEN, Entry, JOURNAL_HEADER_LEN, JournalHeader, JournalState,
    MAGIC, META_LEN, RESET_MARKER_LEN, ResetMarker, TAIL_INDEX_LEN,
};
use kronika_layout::SegmentId;
use kronika_registry::{DecodedSection, Row};
use kronika_store::{
    ActivePart, CatalogSummary, JournalScan, LocalDir, LocalScan, SealedUnit, StoreError,
    StoreWarning, catalog_digests, is_active_journal_scan_error,
};
use sha2::{Digest as _, Sha256};

use crate::refresh::{
    ByteRange, JournalDelta, JournalGenerationId, JournalIdentity, JournalPhase, PartDescriptor,
    PartTransition, RefreshDelta, SealedLocator, SegmentDescriptor,
    apply_committed_reset_transition, classify_transition, part_id_from_digest,
};
use crate::{
    Bounds, BuildError, Dictionary, FactLoad, FactStore, PgmUnit, ReadError, SegmentContext,
};

const JOURNAL_PREFIX_DOMAIN: &[u8] = b"pgk-overview-journal-prefix-v1\0";
const JOURNAL_HASH_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CONSISTENT_SCAN_ATTEMPTS: usize = 2;

fn segment_context(sealed: &SealedUnit) -> SegmentContext {
    SegmentContext::new(sealed.address)
}

fn journal_phase(scan: &LocalScan) -> JournalPhase {
    if scan.committed_reset {
        JournalPhase::CommittedReset
    } else if scan.active.is_empty() {
        JournalPhase::Empty
    } else {
        JournalPhase::Active
    }
}

fn journal_scan_phase(scan: &JournalScan) -> JournalPhase {
    if scan.committed_reset {
        JournalPhase::CommittedReset
    } else if scan.active.is_empty() {
        JournalPhase::Empty
    } else {
        JournalPhase::Active
    }
}

fn map_sealed_open_error(error: io::Error, unit_idx: usize) -> ReadError {
    if error.kind() == io::ErrorKind::Interrupted {
        ReadError::StaleSnapshot { unit_idx }
    } else {
        ReadError::Io(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JournalPrefixDigest([u8; 32]);

// Counts `open_unit` calls so batch tests can assert a unit is opened once.
// Thread-local, so parallel tests do not perturb each other; a test resets it
// to 0 before the call it measures.
#[cfg(test)]
thread_local! {
    pub(crate) static OPEN_UNIT_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(crate) static FORCED_STALE_OPEN_UNIT_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    pub(crate) static DECODE_ROWS_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// A unit opened once for decoding many sections.
///
/// Holds the underlying [`PgmUnit`] so the catalog, dictionary, and every
/// section come from one read of the unit's bytes. A sealed variant reads from
/// an immutable `.pgm` file; an active variant owns the journal bytes captured
/// at open time, after the staleness check has passed.
#[derive(Debug)]
pub enum OpenUnit {
    /// A sealed segment, backed by its immutable `.pgm` file.
    Sealed(PgmUnit<std::fs::File>),
    /// An active journal part, backed by the bytes read when the unit opened.
    Active(PgmUnit<Vec<u8>>),
}

impl OpenUnit {
    /// The unit's end catalog.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        match self {
            Self::Sealed(unit) => unit.catalog(),
            Self::Active(unit) => unit.catalog(),
        }
    }

    /// Decode one typed section.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the section is a dictionary, out of bounds,
    /// fails CRC, or fails typed decode.
    pub fn decode(&self, entry: &Entry) -> Result<DecodedSection, ReadError> {
        match self {
            Self::Sealed(unit) => unit.decode(entry),
            Self::Active(unit) => unit.decode(entry),
        }
    }

    /// Decode one section as named-cell rows.
    ///
    /// `entry` must come from this unit's [`catalog`](Self::catalog).
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the section is a dictionary, out of bounds,
    /// fails CRC, or fails typed decode.
    pub fn decode_rows(&self, entry: &Entry) -> Result<Vec<Row>, ReadError> {
        #[cfg(test)]
        DECODE_ROWS_CALLS.with(|calls| calls.set(calls.get() + 1));
        match self {
            Self::Sealed(unit) => unit.decode_rows(entry),
            Self::Active(unit) => unit.decode_rows(entry),
        }
    }

    /// Read the unit's dictionary sections into a `str_id` -> value map.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when a dictionary section cannot be read or decoded.
    pub fn dictionary(&self) -> Result<Dictionary, ReadError> {
        match self {
            Self::Sealed(unit) => unit.dictionary(),
            Self::Active(unit) => unit.dictionary(),
        }
    }
}

/// Metadata describing one unit (sealed or live) in the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitMeta {
    /// Source identifier from the unit's catalog.
    pub source_id: u64,
    /// Earliest timestamp in the unit.
    pub min_ts: i64,
    /// Latest timestamp in the unit.
    pub max_ts: i64,
    /// `true` when the unit is an active (not yet sealed) journal part.
    pub live: bool,
}

/// Internal index: points into `scan.sealed` or `scan.active`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum UnitHandle {
    Sealed(usize),
    Active(usize),
}

enum UnitCatalogHint<'a> {
    Sealed(&'a CatalogSummary),
    Active(&'a Catalog),
}

pub(crate) struct UnitDescriptor<'a> {
    pub(crate) index: usize,
    pub(crate) handle: UnitHandle,
    pub(crate) meta: UnitMeta,
    pub(crate) eager_open_bytes: u64,
    catalog_hint: UnitCatalogHint<'a>,
}

impl UnitDescriptor<'_> {
    pub(crate) fn may_contain_any_nonempty_type(&self, type_ids: &[u32]) -> bool {
        match self.catalog_hint {
            UnitCatalogHint::Sealed(summary) => summary.may_contain_any_nonempty_type(type_ids),
            UnitCatalogHint::Active(catalog) => catalog
                .entries
                .iter()
                .any(|entry| entry.rows != 0 && type_ids.contains(&entry.type_id)),
        }
    }
}

/// A read view of a `LocalDir` combining sealed and active units.
///
/// A directory scan is not an atomic cross-file snapshot. Journal-first
/// ordering and exact catalog deduplication narrow the seal window. A caller
/// may use the live-view completion boundary as publication evidence, but this
/// reader does not itself publish an atomic combined generation.
///
/// `Clone` copies the catalog metadata cache, not any section bodies; a web
/// handler clones a shared snapshot per request to call `&mut` query functions.
#[derive(Debug, Clone)]
pub struct LocalDirSnapshot {
    dir: LocalDir,
    scan: LocalScan,
    /// End of the last valid journal frame, carried across incremental refreshes.
    last_valid_len: u64,
    /// Root directory, retained so refreshes can restat the journal file.
    root: PathBuf,
    /// Monotone view generation, advanced by observable refresh changes.
    view_generation: u64,
    /// Current proven-continuous journal generation.
    journal_generation: JournalGenerationId,
    /// Journal file identity captured at the last scan, absent when the journal
    /// file is missing.
    journal_identity: Option<JournalIdentity>,
    /// Digest of the exact journal bytes through `last_valid_len`.
    journal_prefix_digest: JournalPrefixDigest,
    /// Whether the active descriptor set in `scan` is authoritative.
    journal_descriptors_complete: bool,
    /// Whether a delta consumer has received the current journal baseline.
    delta_initialized: bool,
    /// Unvalidated bytes after the current journal watermark.
    tail_pending: Option<ByteRange>,
}

/// Why a sealed snapshot unit could not produce persistent overview facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealedFactError {
    /// The unit index is outside the pinned snapshot.
    UnitOutOfRange {
        /// Requested unit index.
        unit_idx: usize,
    },
    /// The requested unit is an active journal part, not a sealed segment.
    LiveUnit {
        /// Requested unit index.
        unit_idx: usize,
    },
    /// The sealed file changed after this snapshot was scanned.
    StaleSnapshot {
        /// Requested unit index.
        unit_idx: usize,
    },
    /// The exact sealed descriptor is not readable in the current scan.
    DescriptorUnavailable {
        /// Stable `SegmentId` locator requested by the caller.
        locator: SealedLocator,
    },
    /// A sealed locator now resolves to a different catalog descriptor.
    StaleDescriptor {
        /// Stable `SegmentId` locator requested by the caller.
        locator: SealedLocator,
    },
    /// Source extraction or a hard fact bound failed.
    Build(BuildError),
}

impl std::fmt::Display for SealedFactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnitOutOfRange { unit_idx } => {
                write!(f, "unit index {unit_idx} is out of range")
            }
            Self::LiveUnit { unit_idx } => {
                write!(f, "unit {unit_idx} is live; sealed facts are unavailable")
            }
            Self::StaleSnapshot { unit_idx } => {
                write!(f, "sealed unit {unit_idx} changed; refresh the snapshot")
            }
            Self::DescriptorUnavailable { locator } => {
                write!(
                    f,
                    "sealed descriptor {locator:?} is unavailable in this scan"
                )
            }
            Self::StaleDescriptor { locator } => {
                write!(
                    f,
                    "sealed descriptor {locator:?} changed; refresh the snapshot"
                )
            }
            Self::Build(error) => write!(f, "sealed fact build failed: {error}"),
        }
    }
}

impl std::error::Error for SealedFactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::UnitOutOfRange { .. }
            | Self::LiveUnit { .. }
            | Self::StaleSnapshot { .. }
            | Self::DescriptorUnavailable { .. }
            | Self::StaleDescriptor { .. } => None,
        }
    }
}

impl From<BuildError> for SealedFactError {
    fn from(error: BuildError) -> Self {
        Self::Build(error)
    }
}

impl LocalDirSnapshot {
    /// Open a local directory and take an initial snapshot.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be opened or scanned.
    pub fn open(root: &Path) -> io::Result<Self> {
        let dir = LocalDir::open(root)?;
        let (scan, journal_identity, journal_prefix_digest) = full_scan_consistent(&dir, &[])?;
        let last_valid_len = scan.valid_len;
        let journal_descriptors_complete = journal_descriptors_complete(&scan, root);
        let tail_pending = tail_pending(journal_identity, last_valid_len);
        Ok(Self {
            dir,
            scan,
            last_valid_len,
            root: root.to_path_buf(),
            view_generation: 0,
            journal_generation: JournalGenerationId(0),
            journal_identity,
            journal_prefix_digest,
            journal_descriptors_complete,
            delta_initialized: false,
            tail_pending,
        })
    }

    /// PgKronika-owned directory containing the active view, sealed PGM files,
    /// and their sibling OVF sidecars.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.root
    }

    /// Re-scan the directory, picking up new sealed files and journal appends.
    ///
    /// This is a full re-scan of the journal from offset `0`. For steady-state
    /// polling prefer [`refresh_incremental`](Self::refresh_incremental).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be re-scanned.
    pub fn refresh(&mut self) -> io::Result<()> {
        let (scan, identity, transition, prefix_digest) = self.scan_full_consistent()?;
        self.install_baseline(scan, identity, prefix_digest, transition)?;
        Ok(())
    }

    /// Re-scan the store incrementally, reading only the journal tail.
    ///
    /// Uses the last known journal offset to skip already-validated frames: an
    /// unchanged journal is not re-read. Before an appended tail is accepted,
    /// the exact previously validated prefix is re-hashed; a mismatch forces a
    /// full scan and a new generation. A truncate-in-place reset rescans from
    /// the start. Sealed `.pgm` files are always re-listed.
    ///
    /// The decode-time staleness check in [`decode_unit`](Self::decode_unit) and
    /// friends remains the backstop against a part changing under a reader.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be re-scanned.
    pub fn refresh_incremental(&mut self) -> io::Result<()> {
        let (scan, identity, transition, prefix_digest) = self.scan_incremental_consistent()?;
        self.install_baseline(scan, identity, prefix_digest, transition)?;
        Ok(())
    }

    /// Re-scan incrementally and report the semantic delta of the scan.
    ///
    /// Beyond the file-length change this names the journal generation, the
    /// parts that completed since the last scan, the proven continuity class of
    /// the tail, any torn-tail bytes, and the sealed segments that appeared or
    /// disappeared. A transition that is not a proven append mints a new journal
    /// generation and re-lists every current part as completed, so the live
    /// builder folds it once under the new generation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be re-scanned or if a
    /// generation counter would overflow.
    pub fn refresh_incremental_delta(&mut self) -> io::Result<RefreshDelta> {
        let previous_valid_len = self.last_valid_len;
        let previous_sealed = Arc::clone(&self.scan.sealed);
        let previous_view_generation = self.view_generation;
        let bootstrap = !self.delta_initialized;

        let (mut scan, current_identity, transition, prefix_digest) =
            self.scan_incremental_consistent()?;
        let new_valid_len = scan.valid_len;
        let generation_id = if transition.preserves_generation() {
            self.journal_generation
        } else {
            JournalGenerationId(bump(self.journal_generation.0)?)
        };

        let current_parts: Arc<[PartDescriptor]> =
            Arc::from(part_descriptors(&scan, generation_id)?);
        let rebase_after_committed_reset = self.scan.committed_reset
            && !scan.committed_reset
            && !scan.active.is_empty()
            && transition.preserves_generation();
        let floor = if !bootstrap
            && transition == PartTransition::Append
            && !rebase_after_committed_reset
        {
            previous_valid_len
        } else {
            0
        };
        let completed_parts = if floor == 0 {
            Arc::clone(&current_parts)
        } else {
            Arc::from(
                current_parts
                    .iter()
                    .copied()
                    .filter(|descriptor| descriptor.part_id.frame_offset >= floor)
                    .collect::<Vec<_>>(),
            )
        };

        let current_tail_pending = tail_pending(current_identity, new_valid_len);
        if transition.preserves_generation() && !rebase_after_committed_reset {
            scan.damages =
                merge_incremental_damages(&self.scan.damages, &scan.damages, previous_valid_len);
        }

        let sealed = sealed_delta(&scan, previous_sealed.as_slice());
        let current_parts_complete = journal_descriptors_complete(&scan, &self.root);

        let changed = !completed_parts.is_empty()
            || !sealed.added.is_empty()
            || !sealed.removed.is_empty()
            || !same_sealed_units(&self.scan, &scan)
            || !same_warnings(&self.scan.warnings, &scan.warnings)
            || !transition.preserves_generation()
            || current_tail_pending != self.tail_pending
            || scan.damages != self.scan.damages
            || current_parts_complete != self.journal_descriptors_complete
            || (bootstrap
                && (current_tail_pending.is_some()
                    || !scan.damages.is_empty()
                    || !scan.active.is_empty()
                    || !current_parts_complete));
        let new_view_generation = if changed {
            bump(previous_view_generation)?
        } else {
            previous_view_generation
        };

        self.scan = scan;
        self.last_valid_len = new_valid_len;
        self.journal_generation = generation_id;
        self.journal_identity = current_identity;
        self.journal_prefix_digest = prefix_digest;
        self.journal_descriptors_complete = current_parts_complete;
        self.view_generation = new_view_generation;
        self.delta_initialized = true;
        self.tail_pending = current_tail_pending;

        Ok(RefreshDelta {
            previous_view_generation,
            new_view_generation,
            view_changed: changed,
            sealed_added: sealed.added,
            sealed_removed: sealed.removed,
            journal: JournalDelta {
                bootstrap,
                generation_id,
                previous_valid_len,
                new_valid_len,
                completed_parts,
                current_parts,
                current_parts_complete,
                transition,
                tail_pending: current_tail_pending,
                damages: self.scan.damages.clone(),
            },
        })
    }

    /// Scans from the current watermark when the journal identity permits it.
    fn scan_full_consistent(
        &self,
    ) -> io::Result<(
        LocalScan,
        Option<JournalIdentity>,
        PartTransition,
        JournalPrefixDigest,
    )> {
        let ((journal, base_transition, prefix_digest), identity) = with_stable_journal_identity(
            || journal_identity(&self.dir),
            |identity_before| {
                let transition = self
                    .verified_transition(identity_before)
                    .map_err(ScanAttemptError::journal)?;
                let journal = self.dir.scan_journal().map_err(ScanAttemptError::store)?;
                let prefix_digest = journal_prefix_digest(&self.dir, journal.valid_len)
                    .map_err(ScanAttemptError::journal)?;
                Ok((journal, transition, prefix_digest))
            },
        )?;
        let same_file = same_journal_file(self.journal_identity, identity);
        let same_committed_reset = same_file
            && self.scan.committed_reset
            && journal.committed_reset
            && self.journal_prefix_digest == prefix_digest;
        let transition = apply_committed_reset_transition(
            base_transition,
            same_file,
            journal_phase(&self.scan),
            journal_scan_phase(&journal),
            same_committed_reset,
        );
        let scan = self.dir.complete_scan_cached(journal, &self.scan.sealed)?;
        let transition = if journal_descriptors_complete(&scan, &self.root) {
            transition
        } else {
            PartTransition::Uncertain
        };
        Ok((scan, identity, transition, prefix_digest))
    }

    /// Scans from the current watermark when the journal identity permits it.
    fn scan_incremental_consistent(
        &self,
    ) -> io::Result<(
        LocalScan,
        Option<JournalIdentity>,
        PartTransition,
        JournalPrefixDigest,
    )> {
        let ((journal, base_transition, prefix_digest), identity) = with_stable_journal_identity(
            || journal_identity(&self.dir),
            |identity_before| {
                let transition = self
                    .verified_transition(identity_before)
                    .map_err(ScanAttemptError::journal)?;
                let journal = if transition.preserves_generation() && !self.scan.committed_reset {
                    self.dir
                        .scan_journal_from(self.last_valid_len, Arc::clone(&self.scan.active))
                } else {
                    self.dir.scan_journal()
                };
                let journal = journal.map_err(ScanAttemptError::store)?;
                let prefix_digest = journal_prefix_digest(&self.dir, journal.valid_len)
                    .map_err(ScanAttemptError::journal)?;
                Ok((journal, transition, prefix_digest))
            },
        )?;
        let same_file = same_journal_file(self.journal_identity, identity);
        let same_committed_reset = same_file
            && self.scan.committed_reset
            && journal.committed_reset
            && self.journal_prefix_digest == prefix_digest;
        let transition = apply_committed_reset_transition(
            base_transition,
            same_file,
            journal_phase(&self.scan),
            journal_scan_phase(&journal),
            same_committed_reset,
        );
        let scan = self.dir.complete_scan_cached(journal, &self.scan.sealed)?;
        let transition = if journal_descriptors_complete(&scan, &self.root) {
            transition
        } else {
            PartTransition::Uncertain
        };
        Ok((scan, identity, transition, prefix_digest))
    }

    fn verified_transition(&self, current: Option<JournalIdentity>) -> io::Result<PartTransition> {
        if !self.journal_descriptors_complete {
            return Ok(PartTransition::Uncertain);
        }
        let transition = classify_transition(self.journal_identity, current, self.last_valid_len);
        let same_inode_growth = matches!(
            (self.journal_identity, current),
            (Some(previous), Some(current))
                if previous.device == current.device
                    && previous.inode == current.inode
                    && current.len > previous.len
        );
        if transition == PartTransition::Append
            && same_inode_growth
            && self.last_valid_len > 0
            && !self.scan.committed_reset
            && !self.scan.active.is_empty()
        {
            let observed = journal_prefix_digest(&self.dir, self.last_valid_len)?;
            if observed != self.journal_prefix_digest {
                return Ok(PartTransition::Uncertain);
            }
        }
        Ok(transition)
    }

    /// Installs a scan taken outside the delta API and resets delta delivery.
    fn install_baseline(
        &mut self,
        mut scan: LocalScan,
        identity: Option<JournalIdentity>,
        prefix_digest: JournalPrefixDigest,
        transition: PartTransition,
    ) -> io::Result<()> {
        let rebase_after_committed_reset = self.scan.committed_reset
            && !scan.committed_reset
            && !scan.active.is_empty()
            && transition.preserves_generation();
        if transition.preserves_generation() && !rebase_after_committed_reset {
            scan.damages =
                merge_incremental_damages(&self.scan.damages, &scan.damages, self.last_valid_len);
        }
        let generation = if transition.preserves_generation() {
            self.journal_generation
        } else {
            JournalGenerationId(bump(self.journal_generation.0)?)
        };
        let sealed = sealed_delta(&scan, self.scan.sealed.as_slice());
        let current_parts_complete = journal_descriptors_complete(&scan, &self.root);
        let current_tail_pending = tail_pending(identity, scan.valid_len);
        let changed = !same_active_parts(&self.scan, &scan)
            || !same_sealed_units(&self.scan, &scan)
            || !same_warnings(&self.scan.warnings, &scan.warnings)
            || !sealed.added.is_empty()
            || !sealed.removed.is_empty()
            || scan.damages != self.scan.damages
            || current_tail_pending != self.tail_pending
            || current_parts_complete != self.journal_descriptors_complete
            || !transition.preserves_generation();
        let view_generation = if changed {
            bump(self.view_generation)?
        } else {
            self.view_generation
        };

        self.last_valid_len = scan.valid_len;
        self.scan = scan;
        self.journal_identity = identity;
        self.journal_prefix_digest = prefix_digest;
        self.journal_generation = generation;
        self.journal_descriptors_complete = current_parts_complete;
        self.view_generation = view_generation;
        self.delta_initialized = false;
        self.tail_pending = current_tail_pending;
        Ok(())
    }

    /// Current proven-continuous journal generation.
    #[must_use]
    pub const fn journal_generation(&self) -> JournalGenerationId {
        self.journal_generation
    }

    /// Current monotone view generation.
    #[must_use]
    pub const fn view_generation(&self) -> u64 {
        self.view_generation
    }

    /// Warnings emitted during the last scan (unreadable `.pgm` files, etc.).
    #[must_use]
    pub fn warnings(&self) -> &[StoreWarning] {
        &self.scan.warnings
    }

    /// Damaged byte ranges found while scanning `active.parts`.
    ///
    /// These ranges describe journal bytes the frame scanner could not validate.
    /// Valid parts before or after a damaged region remain visible through
    /// [`units`](Self::units).
    #[must_use]
    pub fn damages(&self) -> &[DamageRegion] {
        &self.scan.damages
    }

    /// Deduplicated list of units visible in this snapshot.
    ///
    /// Sealed units appear first, then surviving live parts. An active part is
    /// omitted only when a sealed unit has the same catalog. Time-range overlap
    /// is not enough to prove that a live part was sealed.
    #[must_use]
    pub fn units(&self) -> Vec<UnitMeta> {
        self.handles()
            .map(|handle| self.meta_for_handle(handle))
            .collect()
    }

    /// Read the catalog for unit `idx` in the same ordering as `units()`.
    ///
    /// Sealed catalogs are opened lazily and checked against the compact
    /// descriptor captured by this snapshot. Returns `Ok(None)` when `idx` is
    /// out of range.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the unit changed or its catalog cannot be
    /// opened and validated.
    pub fn unit_catalog(&self, idx: usize) -> Result<Option<Catalog>, ReadError> {
        let Some(handle) = self.handles().nth(idx) else {
            return Ok(None);
        };
        Ok(Some(self.open_unit_handle(idx, handle)?.catalog().clone()))
    }

    /// Iterates over ordered sealed descriptors pinned by this snapshot.
    ///
    /// Descriptors are derived from the compact sealed scan without retaining a
    /// second collection proportional to the number of segments.
    #[must_use]
    pub fn sealed_descriptors(
        &self,
    ) -> impl ExactSizeIterator<Item = SegmentDescriptor> + DoubleEndedIterator + '_ {
        self.scan.sealed.iter().map(descriptor_for_sealed)
    }

    /// Loads persistent overview facts for one sealed unit.
    ///
    /// The file is reopened and its catalog is compared with the pinned scan
    /// before cache lookup. A same-name replacement therefore yields
    /// [`SealedFactError::StaleSnapshot`] instead of facts for a different
    /// descriptor. Active journal parts are rejected.
    ///
    /// `context` must name the exact PGM address selected by the snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SealedFactError`] for an invalid/live unit, stale sealed file,
    /// source failure, unsupported event layout, or hard fact bound.
    pub fn load_sealed_facts(
        &self,
        idx: usize,
        store: &FactStore,
        bounds: &Bounds,
    ) -> Result<FactLoad, SealedFactError> {
        let handle = self
            .handles()
            .nth(idx)
            .ok_or(SealedFactError::UnitOutOfRange { unit_idx: idx })?;
        let UnitHandle::Sealed(sealed_idx) = handle else {
            return Err(SealedFactError::LiveUnit { unit_idx: idx });
        };
        let sealed = &self.scan.sealed[sealed_idx];
        let context = segment_context(sealed);
        let unit = self
            .open_checked_sealed(idx, sealed_idx)
            .map_err(|error| match error {
                ReadError::StaleSnapshot { .. } => SealedFactError::StaleSnapshot { unit_idx: idx },
                other => SealedFactError::Build(BuildError::from(other)),
            })?;
        store
            .load_or_build(&unit, &context, bounds)
            .map_err(Into::into)
    }

    /// Loads overview facts for one exact reader-authored sealed descriptor.
    ///
    /// This avoids index lookup through the deduplicated query-unit view. The
    /// verified segment address and catalog descriptor must both still match.
    ///
    /// # Errors
    ///
    /// Returns [`SealedFactError`] when the descriptor is unavailable or stale,
    /// the context carries another locator, or source extraction/admission
    /// fails.
    pub fn load_sealed_facts_by_descriptor(
        &self,
        descriptor: &SegmentDescriptor,
        store: &FactStore,
        bounds: &Bounds,
    ) -> Result<FactLoad, SealedFactError> {
        let context = self.sealed_context(descriptor)?;
        let unit = self.open_sealed_by_descriptor(descriptor)?;
        store
            .load_or_build(&unit, &context, bounds)
            .map_err(Into::into)
    }

    /// Derives sibling-sidecar addressing for one exact sealed descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`SealedFactError`] when the descriptor is unavailable or stale.
    pub fn sealed_context(
        &self,
        descriptor: &SegmentDescriptor,
    ) -> Result<SegmentContext, SealedFactError> {
        let sealed = self
            .scan
            .sealed
            .iter()
            .find(|sealed| SealedLocator::from_segment_id(sealed.address.id) == descriptor.locator)
            .ok_or(SealedFactError::DescriptorUnavailable {
                locator: descriptor.locator,
            })?;
        if SegmentDescriptor::from_summary(descriptor.locator, sealed.identity, &sealed.summary)
            != *descriptor
        {
            return Err(SealedFactError::StaleDescriptor {
                locator: descriptor.locator,
            });
        }
        Ok(segment_context(sealed))
    }

    /// Opens one exact reader-authored sealed descriptor.
    ///
    /// The `SegmentId` locator and catalog descriptor must both still match
    /// the pinned scan. This is the source-authority path used by seal
    /// reconciliation before durable fact publication.
    ///
    /// # Errors
    ///
    /// Returns [`SealedFactError`] when the descriptor is unavailable or stale,
    /// or the source PGM cannot be opened and validated.
    pub fn open_sealed_by_descriptor(
        &self,
        descriptor: &SegmentDescriptor,
    ) -> Result<PgmUnit<std::fs::File>, SealedFactError> {
        let (sealed_idx, sealed) = self
            .scan
            .sealed
            .iter()
            .enumerate()
            .find(|(_, sealed)| {
                SealedLocator::from_segment_id(sealed.address.id) == descriptor.locator
            })
            .ok_or(SealedFactError::DescriptorUnavailable {
                locator: descriptor.locator,
            })?;
        if SegmentDescriptor::from_summary(descriptor.locator, sealed.identity, &sealed.summary)
            != *descriptor
        {
            return Err(SealedFactError::StaleDescriptor {
                locator: descriptor.locator,
            });
        }
        self.open_checked_sealed(sealed_idx, sealed_idx)
            .map_err(|error| match error {
                ReadError::StaleSnapshot { .. } => SealedFactError::StaleDescriptor {
                    locator: descriptor.locator,
                },
                other => SealedFactError::Build(BuildError::from(other)),
            })
    }

    /// Opens one active journal part by its exact refresh descriptor.
    ///
    /// The descriptor is matched against the journal generation, byte range,
    /// and catalog digest rather than the deduplicated `units()` ordering.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the descriptor is absent, the journal moved
    /// after the scan, or the captured bytes fail PGM validation.
    pub fn open_active_part(
        &self,
        descriptor: &PartDescriptor,
    ) -> Result<PgmUnit<Vec<u8>>, ReadError> {
        let active_idx = self
            .scan
            .active
            .iter()
            .position(|active| {
                let Ok(frame_offset) = u64::try_from(active.part.offset) else {
                    return false;
                };
                let Ok(body_len) = u64::try_from(active.part.len) else {
                    return false;
                };
                PartDescriptor {
                    part_id: part_id_from_digest(
                        self.journal_generation,
                        frame_offset,
                        body_len,
                        active.catalog_digest,
                    ),
                    source_id: active.catalog.source_id,
                    min_ts: active.catalog.min_ts,
                    max_ts: active.catalog.max_ts,
                } == *descriptor
            })
            .ok_or_else(|| {
                ReadError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "active part {:?} is unavailable in journal generation {:?}",
                        descriptor.part_id, self.journal_generation
                    ),
                ))
            })?;
        let active = &self.scan.active[active_idx];
        let bytes = match self.dir.read_active_part(active) {
            Ok(bytes) => bytes,
            Err(StoreError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
                ) =>
            {
                return Err(ReadError::StaleSnapshot {
                    unit_idx: active_idx,
                });
            }
            Err(StoreError::Io(error)) => return Err(ReadError::Io(error)),
            Err(error) => return Err(ReadError::Store(error)),
        };
        let unit = PgmUnit::open(bytes)?;
        if unit.catalog() != &active.catalog {
            return Err(ReadError::StaleSnapshot {
                unit_idx: active_idx,
            });
        }
        Ok(unit)
    }

    /// Decode one section from the unit at position `idx` in `units()`.
    ///
    /// `entry_idx` indexes into `unit_catalog(idx).entries`. Both `idx` and
    /// `entry_idx` are bounds-checked; out-of-range values return an I/O error.
    ///
    /// For active (live) units the journal bytes are re-read and their catalog
    /// is compared against the cached one. If they differ the function returns
    /// [`ReadError::StaleSnapshot`]; the caller must call [`refresh`](Self::refresh)
    /// and retry.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the unit index or entry index is out of range,
    /// the unit cannot be opened, the section fails CRC or typed decode, or the
    /// active part's catalog has changed since the snapshot was taken.
    pub fn decode_unit(&self, idx: usize, entry_idx: usize) -> Result<DecodedSection, ReadError> {
        let unit = self.open_unit(idx)?;
        let entry = unit.catalog().entries.get(entry_idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("entry index {entry_idx} is out of range for unit {idx}"),
            ))
        })?;
        unit.decode(entry)
    }

    /// Decode one section as named-cell rows from the unit at position `idx`.
    ///
    /// Mirrors [`decode_unit`](Self::decode_unit) exactly — same bounds checks,
    /// staleness handling, and active-part re-read — but calls
    /// `PgmUnit::decode_rows` instead of `PgmUnit::decode`.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] for the same reasons as [`decode_unit`](Self::decode_unit).
    pub fn decode_unit_rows(&self, idx: usize, entry_idx: usize) -> Result<Vec<Row>, ReadError> {
        let unit = self.open_unit(idx)?;
        let entry = unit.catalog().entries.get(entry_idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("entry index {entry_idx} is out of range for unit {idx}"),
            ))
        })?;
        unit.decode_rows(entry)
    }

    /// Read the dictionary of the unit at position `idx` in `units()`.
    ///
    /// Opens the unit the same way [`decode_unit`](Self::decode_unit) does —
    /// sealed via a `File`, active by re-reading the journal bytes — and applies
    /// the same staleness check for live units.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the unit index is out of range, the unit
    /// cannot be opened, a dictionary section fails CRC or decode, or the
    /// active part's catalog changed since the snapshot was taken.
    pub fn unit_dictionary(&self, idx: usize) -> Result<Dictionary, ReadError> {
        self.open_unit(idx)?.dictionary()
    }

    /// Open the unit at position `idx` in `units()` for multi-section decoding.
    ///
    /// A sealed unit opens its immutable `.pgm` file. An active unit re-reads the
    /// journal bytes and compares the freshly parsed catalog against the cached
    /// one; a `NotFound`/`UnexpectedEof` read or a catalog mismatch means the
    /// journal moved on and yields [`ReadError::StaleSnapshot`]. The staleness
    /// check runs once, here; the returned [`OpenUnit`] then serves every section
    /// from the bytes captured at open time.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when `idx` is out of range, the unit cannot be
    /// opened, or the active part changed since the snapshot was taken.
    pub fn open_unit(&self, idx: usize) -> Result<OpenUnit, ReadError> {
        let handle = self.handles().nth(idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unit index {idx} is out of range"),
            ))
        })?;
        self.open_unit_handle(idx, handle)
    }

    fn open_checked_sealed(
        &self,
        unit_idx: usize,
        sealed_idx: usize,
    ) -> Result<PgmUnit<std::fs::File>, ReadError> {
        let sealed = &self.scan.sealed[sealed_idx];
        let file = self
            .dir
            .open_sealed(sealed)
            .map_err(|error| map_sealed_open_error(error, unit_idx))?;
        let identity_check = file.try_clone()?;
        let opened = PgmUnit::open(file);
        if let Err(error) = self.dir.validate_sealed_file(&identity_check, sealed) {
            return Err(map_sealed_open_error(error, unit_idx));
        }
        let unit = opened?;
        let catalog_len = u32::try_from(unit.catalog().encoded_len())
            .map_err(|_overflow| ReadError::CounterOverflow)?;
        let observed = CatalogSummary::from_catalog(unit.catalog(), catalog_len);
        if observed != *sealed.summary {
            return Err(ReadError::StaleSnapshot { unit_idx });
        }
        Ok(unit)
    }

    pub(crate) fn open_unit_handle(
        &self,
        idx: usize,
        handle: UnitHandle,
    ) -> Result<OpenUnit, ReadError> {
        #[cfg(test)]
        OPEN_UNIT_CALLS.with(|c| c.set(c.get() + 1));
        #[cfg(test)]
        if FORCED_STALE_OPEN_UNIT_CALLS.with(|calls| {
            let remaining = calls.get();
            calls.set(remaining.saturating_sub(1));
            remaining != 0
        }) {
            return Err(ReadError::StaleSnapshot { unit_idx: idx });
        }
        match handle {
            UnitHandle::Sealed(i) => self.open_checked_sealed(idx, i).map(OpenUnit::Sealed),
            UnitHandle::Active(i) => {
                let ap = &self.scan.active[i];
                let bytes = match self.dir.read_active_part(ap) {
                    Ok(b) => b,
                    Err(StoreError::Io(err))
                        if err.kind() == io::ErrorKind::NotFound
                            || err.kind() == io::ErrorKind::UnexpectedEof =>
                    {
                        return Err(ReadError::StaleSnapshot { unit_idx: idx });
                    }
                    Err(StoreError::Io(err)) => return Err(ReadError::Io(err)),
                    Err(err) => return Err(ReadError::Store(err)),
                };
                let unit = PgmUnit::open(bytes)?;
                if unit.catalog() != &ap.catalog {
                    return Err(ReadError::StaleSnapshot { unit_idx: idx });
                }
                Ok(OpenUnit::Active(unit))
            }
        }
    }

    pub(crate) fn unit_descriptors(&self) -> impl Iterator<Item = UnitDescriptor<'_>> {
        self.handles()
            .enumerate()
            .map(|(index, handle)| UnitDescriptor {
                index,
                handle,
                meta: self.meta_for_handle(handle),
                eager_open_bytes: match handle {
                    UnitHandle::Sealed(i) => u64::from(self.scan.sealed[i].summary.catalog_len)
                        .saturating_add(MAGIC.len() as u64)
                        .saturating_add(TAIL_INDEX_LEN as u64),
                    UnitHandle::Active(i) => {
                        u64::try_from(self.scan.active[i].part.len).unwrap_or(u64::MAX)
                    }
                },
                catalog_hint: match handle {
                    UnitHandle::Sealed(i) => {
                        UnitCatalogHint::Sealed(self.scan.sealed[i].summary.as_ref())
                    }
                    UnitHandle::Active(i) => UnitCatalogHint::Active(&self.scan.active[i].catalog),
                },
            })
    }

    fn meta_for_handle(&self, handle: UnitHandle) -> UnitMeta {
        match handle {
            UnitHandle::Sealed(i) => {
                let summary = self.scan.sealed[i].summary.as_ref();
                UnitMeta {
                    source_id: summary.source_id,
                    min_ts: summary.min_ts,
                    max_ts: summary.max_ts,
                    live: false,
                }
            }
            UnitHandle::Active(i) => {
                let catalog = &self.scan.active[i].catalog;
                UnitMeta {
                    source_id: catalog.source_id,
                    min_ts: catalog.min_ts,
                    max_ts: catalog.max_ts,
                    live: true,
                }
            }
        }
    }

    /// Iterator over handles in the same order as `units()`.
    fn handles(&self) -> impl Iterator<Item = UnitHandle> + '_ {
        let sealed_iter = (0..self.scan.sealed.len()).map(UnitHandle::Sealed);
        let suppress_active_generation = sealed_generation_matches_active(&self.scan);

        let active_iter = self
            .scan
            .active
            .iter()
            .enumerate()
            .filter(move |_| !suppress_active_generation)
            .map(|(i, _)| UnitHandle::Active(i));

        sealed_iter.chain(active_iter)
    }
}

/// Whether sealing every active part for one segment produces this sealed catalog.
///
/// The sealer relocates section bodies into one PGM, so a multi-part active
/// catalog cannot be compared to the sealed catalog one part at a time.
fn sealed_generation_matches_active(scan: &LocalScan) -> bool {
    let Some(segment_id) = scan.active.first().map(|active| active.segment_id) else {
        return false;
    };
    let Some(sealed) = scan
        .sealed
        .iter()
        .find(|sealed| sealed.address.id == segment_id)
    else {
        return false;
    };
    aggregate_catalog_matches(&sealed.summary, &scan.active, segment_id)
}

/// Mirrors the catalog transformation performed by `kronika_writer::seal`.
fn aggregate_catalog_matches(
    sealed: &CatalogSummary,
    active: &[ActivePart],
    segment_id: SegmentId,
) -> bool {
    if active.is_empty() {
        return false;
    }
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;
    let format_version = active[0].catalog.format_version;
    let mut entry_count = 0_usize;
    let mut body_end = MAGIC.len() as u64;

    for active_part in active {
        if active_part.segment_id != segment_id {
            return false;
        }
        let catalog = &active_part.catalog;
        if catalog.format_version != format_version {
            return false;
        }
        min_ts = min_ts.min(catalog.min_ts);
        max_ts = max_ts.max(catalog.max_ts);
        if catalog.source_id != 0 {
            if source_id != 0 && source_id != catalog.source_id {
                return false;
            }
            source_id = catalog.source_id;
        }
        entry_count = match entry_count.checked_add(catalog.entries.len()) {
            Some(count) => count,
            None => return false,
        };
        for entry in &catalog.entries {
            body_end = match body_end.checked_add(entry.len) {
                Some(next) => next,
                None => return false,
            };
        }
    }

    if min_ts > max_ts {
        min_ts = 0;
        max_ts = 0;
    }
    let Ok(entry_count_u32) = u32::try_from(entry_count) else {
        return false;
    };
    let Some(catalog_len) = entry_count
        .checked_mul(ENTRY_LEN)
        .and_then(|entries_len| META_LEN.checked_add(entries_len))
        .and_then(|len| u32::try_from(len).ok())
    else {
        return false;
    };
    let mut relocated_offset = MAGIC.len() as u64;
    let relocated_entries = active
        .iter()
        .flat_map(|part| part.catalog.entries.iter().copied())
        .map(|entry| {
            let relocated = Entry {
                offset: relocated_offset,
                ..entry
            };
            relocated_offset = relocated_offset
                .checked_add(entry.len)
                .expect("aggregate body length was checked above");
            relocated
        });
    let (logical_digest, layout_digest) = catalog_digests(
        source_id,
        min_ts,
        max_ts,
        format_version,
        entry_count_u32,
        relocated_entries,
    );

    sealed.min_ts == min_ts
        && sealed.max_ts == max_ts
        && sealed.source_id == source_id
        && sealed.entry_count == entry_count_u32
        && sealed.format_version == format_version
        && sealed.catalog_len == catalog_len
        && sealed.logical_digest == logical_digest
        && sealed.layout_digest == layout_digest
        && body_end >= MAGIC.len() as u64
}

fn descriptor_for_sealed(sealed: &SealedUnit) -> SegmentDescriptor {
    SegmentDescriptor::from_summary(
        SealedLocator::from_segment_id(sealed.address.id),
        sealed.identity,
        &sealed.summary,
    )
}

#[derive(Debug)]
struct SealedDeltaState {
    added: Vec<SegmentDescriptor>,
    removed: Vec<SegmentDescriptor>,
}

fn sealed_delta(scan: &LocalScan, previous: &[SealedUnit]) -> SealedDeltaState {
    let current = scan.sealed.as_slice();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut current_index = 0;
    let mut previous_index = 0;

    while let (Some(current_unit), Some(previous_unit)) =
        (current.get(current_index), previous.get(previous_index))
    {
        match current_unit.address.id.cmp(&previous_unit.address.id) {
            std::cmp::Ordering::Less => {
                added.push(descriptor_for_sealed(current_unit));
                current_index += 1;
            }
            std::cmp::Ordering::Equal => {
                let current_descriptor = descriptor_for_sealed(current_unit);
                let previous_descriptor = descriptor_for_sealed(previous_unit);
                if current_descriptor != previous_descriptor {
                    removed.push(previous_descriptor);
                    added.push(current_descriptor);
                }
                current_index += 1;
                previous_index += 1;
            }
            std::cmp::Ordering::Greater => {
                removed.push(descriptor_for_sealed(previous_unit));
                previous_index += 1;
            }
        }
    }
    for unit in &current[current_index..] {
        added.push(descriptor_for_sealed(unit));
    }
    for unit in &previous[previous_index..] {
        removed.push(descriptor_for_sealed(unit));
    }

    SealedDeltaState { added, removed }
}

fn journal_descriptors_complete(scan: &LocalScan, root: &Path) -> bool {
    let journal_path = root.join("active.parts");
    !scan
        .warnings
        .iter()
        .any(|warning| warning.path == journal_path)
}

fn part_descriptors(
    scan: &LocalScan,
    generation: JournalGenerationId,
) -> io::Result<Vec<PartDescriptor>> {
    scan.active
        .iter()
        .map(|active| {
            let frame_offset = u64::try_from(active.part.offset)
                .map_err(|_error| io::Error::other("journal part offset overflow"))?;
            let body_len = u64::try_from(active.part.len)
                .map_err(|_error| io::Error::other("journal part length overflow"))?;
            Ok(PartDescriptor {
                part_id: part_id_from_digest(
                    generation,
                    frame_offset,
                    body_len,
                    active.catalog_digest,
                ),
                source_id: active.catalog.source_id,
                min_ts: active.catalog.min_ts,
                max_ts: active.catalog.max_ts,
            })
        })
        .collect()
}

/// Performs a full scan while the journal identity remains stable.
fn full_scan_consistent(
    dir: &LocalDir,
    previous_sealed: &[SealedUnit],
) -> io::Result<(LocalScan, Option<JournalIdentity>, JournalPrefixDigest)> {
    let ((journal, prefix_digest), identity) = with_stable_journal_identity(
        || journal_identity(dir),
        |_identity_before| {
            let journal = dir.scan_journal().map_err(ScanAttemptError::store)?;
            let prefix_digest =
                journal_prefix_digest(dir, journal.valid_len).map_err(ScanAttemptError::journal)?;
            Ok((journal, prefix_digest))
        },
    )?;
    let scan = dir.complete_scan_cached(journal, previous_sealed)?;
    Ok((scan, identity, prefix_digest))
}

#[derive(Debug)]
struct ScanAttemptError {
    source: io::Error,
    retry_if_identity_changed: bool,
}

impl ScanAttemptError {
    fn journal(source: io::Error) -> Self {
        let retry_if_identity_changed = is_transition_read_error(&source);
        Self {
            source,
            retry_if_identity_changed,
        }
    }

    fn store(source: io::Error) -> Self {
        let retry_if_identity_changed =
            is_active_journal_scan_error(&source) && is_transition_read_error(&source);
        Self {
            source,
            retry_if_identity_changed,
        }
    }
}

fn is_transition_read_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidData
            | io::ErrorKind::Interrupted
            | io::ErrorKind::NotFound
            | io::ErrorKind::UnexpectedEof
    )
}

fn with_stable_journal_identity<T>(
    mut identity: impl FnMut() -> io::Result<Option<JournalIdentity>>,
    mut operation: impl FnMut(Option<JournalIdentity>) -> Result<T, ScanAttemptError>,
) -> io::Result<(T, Option<JournalIdentity>)> {
    for _attempt in 0..MAX_CONSISTENT_SCAN_ATTEMPTS {
        let identity_before = identity()?;
        let result = match operation(identity_before) {
            Err(failure) if !failure.retry_if_identity_changed => return Err(failure.source),
            result => result,
        };
        let identity_after = match identity() {
            Ok(identity) => identity,
            Err(identity_error) => {
                return match result {
                    Ok(_) => Err(identity_error),
                    Err(failure) => Err(failure.source),
                };
            }
        };
        match result {
            Ok(value) if identity_before == identity_after => {
                return Ok((value, identity_after));
            }
            Err(failure) if identity_before == identity_after => return Err(failure.source),
            Ok(_) | Err(_) => {}
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "active.parts changed during {MAX_CONSISTENT_SCAN_ATTEMPTS} consecutive scan attempts"
        ),
    ))
}

fn journal_prefix_digest(dir: &LocalDir, valid_len: u64) -> io::Result<JournalPrefixDigest> {
    let mut hasher = Sha256::new();
    hasher.update(JOURNAL_PREFIX_DOMAIN);
    hasher.update(valid_len.to_le_bytes());
    if valid_len == 0 {
        return Ok(JournalPrefixDigest(hasher.finalize().into()));
    }

    if valid_len < JOURNAL_HEADER_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal prefix ends inside the version-1 header",
        ));
    }
    let mut file = dir
        .open_active()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "active.parts is absent"))?;
    let mut header_bytes = [0_u8; JOURNAL_HEADER_LEN];
    file.read_exact(&mut header_bytes)?;
    if is_committed_reset_state(&file, valid_len, header_bytes)? {
        hasher.update([0]);
    } else {
        let header = JournalHeader::decode(header_bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        match header.state {
            JournalState::Empty => hasher.update([0]),
            JournalState::Active { segment_id } => {
                hasher.update([1]);
                hasher.update(segment_id.to_le_bytes());
            }
        }
    }
    let mut remaining = valid_len - JOURNAL_HEADER_LEN as u64;
    let mut buffer = vec![0_u8; JOURNAL_HASH_BUFFER_BYTES].into_boxed_slice();
    let buffer_len = u64::try_from(buffer.len())
        .map_err(|_error| io::Error::other("journal hash buffer length overflow"))?;
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(buffer_len))
            .map_err(|_error| io::Error::other("journal hash range overflow"))?;
        file.read_exact(&mut buffer[..wanted])?;
        hasher.update(&buffer[..wanted]);
        let consumed = u64::try_from(wanted)
            .map_err(|_error| io::Error::other("journal hash read length overflow"))?;
        remaining -= consumed;
    }
    Ok(JournalPrefixDigest(hasher.finalize().into()))
}

fn is_committed_reset_state(
    file: &std::fs::File,
    valid_len: u64,
    header_bytes: [u8; JOURNAL_HEADER_LEN],
) -> io::Result<bool> {
    let marker_len = RESET_MARKER_LEN as u64;
    let Some(marker_at) = valid_len.checked_sub(marker_len) else {
        return Ok(false);
    };
    if marker_at <= JOURNAL_HEADER_LEN as u64 {
        return Ok(false);
    }
    let mut marker_bytes = [0_u8; RESET_MARKER_LEN];
    file.read_exact_at(&mut marker_bytes, marker_at)?;
    let Some(marker) = ResetMarker::decode(marker_bytes) else {
        return Ok(false);
    };
    Ok(marker.previous_len == marker_at
        && SegmentId::new(marker.previous_segment_id).is_ok()
        && marker.expected_previous_header().is_some()
        && marker.classify_header_transition(header_bytes).is_some())
}

/// Reads the observable identity of `active.parts`.
fn journal_identity(dir: &LocalDir) -> io::Result<Option<JournalIdentity>> {
    let Some(file) = dir.open_active()? else {
        return Ok(None);
    };
    let metadata = file.metadata()?;
    let mtime_ns = timestamp_ns(metadata.mtime(), metadata.mtime_nsec())?;
    let ctime_ns = timestamp_ns(metadata.ctime(), metadata.ctime_nsec())?;
    Ok(Some(JournalIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        mtime_ns,
        ctime_ns,
    }))
}

const fn same_journal_file(
    previous: Option<JournalIdentity>,
    current: Option<JournalIdentity>,
) -> bool {
    matches!(
        (previous, current),
        (Some(previous), Some(current))
            if previous.device == current.device && previous.inode == current.inode
    )
}

fn timestamp_ns(seconds: i64, nanoseconds: i64) -> io::Result<i128> {
    i128::from(seconds)
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i128::from(nanoseconds)))
        .ok_or_else(|| io::Error::other("filesystem timestamp overflow"))
}

fn tail_pending(identity: Option<JournalIdentity>, valid_len: u64) -> Option<ByteRange> {
    identity.and_then(|identity| {
        (identity.len > valid_len).then_some(ByteRange {
            start: valid_len,
            end: identity.len,
        })
    })
}

/// Advances a monotone generation counter, refusing to wrap silently.
fn bump(value: u64) -> io::Result<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| io::Error::other("generation counter overflow"))
}

fn merge_incremental_damages(
    previous: &[DamageRegion],
    current: &[DamageRegion],
    previous_valid_len: u64,
) -> Vec<DamageRegion> {
    let mut merged: Vec<_> = previous
        .iter()
        .copied()
        .filter(|damage| (damage.from as u128) < u128::from(previous_valid_len))
        .collect();
    for damage in current {
        if !merged.contains(damage) {
            merged.push(*damage);
        }
    }
    merged.sort_by_key(|damage| damage.from);
    merged
}

fn same_active_parts(previous: &LocalScan, current: &LocalScan) -> bool {
    previous.active.len() == current.active.len()
        && previous
            .active
            .iter()
            .zip(current.active.iter())
            .all(|(left, right)| left.part == right.part && left.catalog == right.catalog)
}

fn same_sealed_units(previous: &LocalScan, current: &LocalScan) -> bool {
    previous.sealed.len() == current.sealed.len()
        && previous
            .sealed
            .iter()
            .zip(current.sealed.iter())
            .all(|(left, right)| {
                left.address == right.address
                    && left.identity == right.identity
                    && left.summary == right.summary
            })
}

fn same_warnings(previous: &[StoreWarning], current: &[StoreWarning]) -> bool {
    previous.len() == current.len()
        && previous
            .iter()
            .zip(current)
            .all(|(left, right)| left.path == right.path && left.reason == right.reason)
}

#[cfg(test)]
mod tests;
