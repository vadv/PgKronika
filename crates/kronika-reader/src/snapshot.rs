//! Read view over sealed files and active journal parts.
//!
//! Combines `LocalDir`'s sealed `.pgm` segments and `active.parts` journal into
//! one list, suppresses exact sealed/live duplicates, and decodes both through
//! `PgmUnit`.

use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use kronika_format::{Catalog, DamageRegion, Entry};
use kronika_registry::{DecodedSection, Row};
use kronika_store::{LocalDir, LocalScan, StoreError, StoreWarning};
use sha2::{Digest as _, Sha256};

use crate::refresh::{
    ByteRange, JournalDelta, JournalGenerationId, JournalIdentity, PartDescriptor, PartTransition,
    RefreshDelta, SealedLocator, SegmentDescriptor, classify_transition, part_id,
};
use crate::{
    Bounds, BuildError, Dictionary, FactLoad, FactStore, PgmUnit, ReadError, SegmentContext,
};

const JOURNAL_PREFIX_DOMAIN: &[u8] = b"pgk-overview-journal-prefix-v1\0";
const JOURNAL_HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JournalPrefixDigest([u8; 32]);

// Counts `open_unit` calls so batch tests can assert a unit is opened once.
// Thread-local, so parallel tests do not perturb each other; a test resets it
// to 0 before the call it measures.
#[cfg(test)]
thread_local! {
    pub(crate) static OPEN_UNIT_CALLS: std::cell::Cell<usize> =
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

    /// Decode one section as named-cell rows.
    ///
    /// `entry` must come from this unit's [`catalog`](Self::catalog).
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the section is a dictionary, out of bounds,
    /// fails CRC, or fails typed decode.
    pub fn decode_rows(&self, entry: &Entry) -> Result<Vec<Row>, ReadError> {
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
enum Handle {
    Sealed(usize),
    Active(usize),
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
    /// Last authoritative sealed-segment baseline, including entries preserved
    /// across a per-file or whole-listing warning.
    sealed_baseline: Vec<SegmentDescriptor>,
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
        /// Stable direct-child file-name identity requested by the caller.
        locator: SealedLocator,
    },
    /// A sealed locator now resolves to a different catalog descriptor.
    StaleDescriptor {
        /// Stable direct-child file-name identity requested by the caller.
        locator: SealedLocator,
    },
    /// The supplied extraction context does not carry the descriptor's locator.
    ContextLocatorMismatch {
        /// Stable direct-child file-name identity requested by the caller.
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
            Self::ContextLocatorMismatch { locator } => {
                write!(
                    f,
                    "segment context does not match sealed descriptor {locator:?}"
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
            | Self::StaleDescriptor { .. }
            | Self::ContextLocatorMismatch { .. } => None,
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
        let (scan, journal_identity, journal_prefix_digest) = full_scan_consistent(&dir, root)?;
        let last_valid_len = scan.valid_len;
        let sealed_baseline = sealed_descriptors(&scan)?;
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
            sealed_baseline,
            journal_descriptors_complete,
            delta_initialized: false,
            tail_pending,
        })
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
        let (scan, identity, prefix_digest) = full_scan_consistent(&self.dir, &self.root)?;
        let transition = if journal_descriptors_complete(&scan, &self.root) {
            self.verified_transition(identity)?
        } else {
            PartTransition::Uncertain
        };
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
        let previous_sealed = &self.sealed_baseline;
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

        let current_parts = part_descriptors(&scan, generation_id)?;
        let floor = if !bootstrap && transition == PartTransition::Append {
            previous_valid_len
        } else {
            0
        };
        let completed_parts = current_parts
            .iter()
            .copied()
            .filter(|descriptor| descriptor.part_id.frame_offset >= floor)
            .collect::<Vec<_>>();

        let current_tail_pending = tail_pending(current_identity, new_valid_len);
        if transition.preserves_generation() {
            scan.damages =
                merge_incremental_damages(&self.scan.damages, &scan.damages, previous_valid_len);
        }

        let sealed = sealed_delta(&scan, previous_sealed, &self.root)?;
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
        self.sealed_baseline = sealed.baseline;
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
    fn scan_incremental_consistent(
        &self,
    ) -> io::Result<(
        LocalScan,
        Option<JournalIdentity>,
        PartTransition,
        JournalPrefixDigest,
    )> {
        let identity_before = journal_identity(&self.root)?;
        let transition = self.verified_transition(identity_before)?;
        let scan = if transition.preserves_generation() {
            self.dir
                .scan_from(self.last_valid_len, self.scan.active.clone())?
        } else {
            self.dir.scan()?
        };
        let prefix_digest = match journal_prefix_digest(&self.root, scan.valid_len) {
            Ok(digest) => digest,
            Err(error) if is_journal_race(&error) => {
                return self.full_scan_after_race();
            }
            Err(error) => return Err(error),
        };
        let identity_after = journal_identity(&self.root)?;
        if identity_before == identity_after {
            let transition = if journal_descriptors_complete(&scan, &self.root) {
                transition
            } else {
                PartTransition::Uncertain
            };
            return Ok((scan, identity_after, transition, prefix_digest));
        }

        self.full_scan_after_race()
    }

    fn full_scan_after_race(
        &self,
    ) -> io::Result<(
        LocalScan,
        Option<JournalIdentity>,
        PartTransition,
        JournalPrefixDigest,
    )> {
        let (scan, identity, prefix_digest) = full_scan_consistent(&self.dir, &self.root)?;
        let transition = if journal_descriptors_complete(&scan, &self.root) {
            self.verified_transition(identity)?
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
        if transition == PartTransition::Append && same_inode_growth && self.last_valid_len > 0 {
            let observed = journal_prefix_digest(&self.root, self.last_valid_len)?;
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
        if transition.preserves_generation() {
            scan.damages =
                merge_incremental_damages(&self.scan.damages, &scan.damages, self.last_valid_len);
        }
        let generation = if transition.preserves_generation() {
            self.journal_generation
        } else {
            JournalGenerationId(bump(self.journal_generation.0)?)
        };
        let sealed = sealed_delta(&scan, &self.sealed_baseline, &self.root)?;
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
        self.sealed_baseline = sealed.baseline;
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
            .map(|h| match h {
                Handle::Sealed(i) => {
                    let c = &self.scan.sealed[i].catalog;
                    UnitMeta {
                        source_id: c.source_id,
                        min_ts: c.min_ts,
                        max_ts: c.max_ts,
                        live: false,
                    }
                }
                Handle::Active(i) => {
                    let c = &self.scan.active[i].catalog;
                    UnitMeta {
                        source_id: c.source_id,
                        min_ts: c.min_ts,
                        max_ts: c.max_ts,
                        live: true,
                    }
                }
            })
            .collect()
    }

    /// The catalog cached for unit `idx` in the same ordering as `units()`.
    ///
    /// Returns `None` when `idx` is out of range.
    #[must_use]
    pub fn unit_catalog(&self, idx: usize) -> Option<&Catalog> {
        let handle = self.handles().nth(idx)?;
        Some(match handle {
            Handle::Sealed(i) => &self.scan.sealed[i].catalog,
            Handle::Active(i) => &self.scan.active[i].catalog,
        })
    }

    /// Ordered sealed descriptors pinned by this snapshot.
    ///
    /// A descriptor preserved across a transient per-file scan warning remains
    /// in this baseline, but an exact load reports
    /// [`SealedFactError::DescriptorUnavailable`] until the file is readable
    /// again.
    #[must_use]
    pub fn sealed_descriptors(&self) -> &[SegmentDescriptor] {
        &self.sealed_baseline
    }

    /// Loads persistent overview facts for one sealed unit.
    ///
    /// The file is reopened and its catalog is compared with the pinned scan
    /// before cache lookup. A same-name replacement therefore yields
    /// [`SealedFactError::StaleSnapshot`] instead of facts for a different
    /// descriptor. Active journal parts are rejected.
    ///
    /// `context` must carry the locator assigned by the reader's sealed-segment
    /// registry; this method never derives it from row timestamps.
    ///
    /// # Errors
    ///
    /// Returns [`SealedFactError`] for an invalid/live unit, stale sealed file,
    /// source failure, unsupported event layout, or hard fact bound.
    pub fn load_sealed_facts(
        &self,
        idx: usize,
        store: &FactStore,
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<FactLoad, SealedFactError> {
        let handle = self
            .handles()
            .nth(idx)
            .ok_or(SealedFactError::UnitOutOfRange { unit_idx: idx })?;
        let Handle::Sealed(sealed_idx) = handle else {
            return Err(SealedFactError::LiveUnit { unit_idx: idx });
        };
        let sealed = &self.scan.sealed[sealed_idx];
        let file = self
            .dir
            .open_sealed(sealed)
            .map_err(ReadError::Io)
            .map_err(BuildError::from)?;
        let unit = PgmUnit::open(file).map_err(BuildError::from)?;
        if unit.catalog() != &sealed.catalog {
            return Err(SealedFactError::StaleSnapshot { unit_idx: idx });
        }
        store
            .load_or_build(&unit, context, bounds)
            .map_err(Into::into)
    }

    /// Loads overview facts for one exact reader-authored sealed descriptor.
    ///
    /// This avoids index lookup through the deduplicated query-unit view. The
    /// direct-child locator and catalog descriptor must both still match, and
    /// the extraction context must carry that exact locator.
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
        context: &SegmentContext,
        bounds: &Bounds,
    ) -> Result<FactLoad, SealedFactError> {
        if context.segment_locator().0 != *descriptor.locator.as_bytes() {
            return Err(SealedFactError::ContextLocatorMismatch {
                locator: descriptor.locator,
            });
        }

        let unit = self.open_sealed_by_descriptor(descriptor)?;
        store
            .load_or_build(&unit, context, bounds)
            .map_err(Into::into)
    }

    /// Opens one exact reader-authored sealed descriptor.
    ///
    /// The direct-child locator and catalog descriptor must both still match
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
        let sealed = self
            .scan
            .sealed
            .iter()
            .find(|sealed| {
                sealed
                    .path
                    .file_name()
                    .map(|name| SealedLocator::from_file_name_bytes(name.as_bytes()))
                    == Some(descriptor.locator)
            })
            .ok_or(SealedFactError::DescriptorUnavailable {
                locator: descriptor.locator,
            })?;
        if SegmentDescriptor::from_catalog(descriptor.locator, &sealed.catalog) != *descriptor {
            return Err(SealedFactError::StaleDescriptor {
                locator: descriptor.locator,
            });
        }

        let file = self.dir.open_sealed(sealed).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
            ) {
                SealedFactError::StaleDescriptor {
                    locator: descriptor.locator,
                }
            } else {
                SealedFactError::Build(BuildError::from(ReadError::Io(error)))
            }
        })?;
        let unit = PgmUnit::open(file).map_err(BuildError::from)?;
        if unit.catalog() != &sealed.catalog {
            return Err(SealedFactError::StaleDescriptor {
                locator: descriptor.locator,
            });
        }
        Ok(unit)
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
                    part_id: part_id(
                        self.journal_generation,
                        frame_offset,
                        body_len,
                        &active.catalog,
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
        let handle = self.handles().nth(idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unit index {idx} is out of range"),
            ))
        })?;
        match handle {
            Handle::Sealed(i) => {
                let su = &self.scan.sealed[i];
                let entry = su.catalog.entries.get(entry_idx).ok_or_else(|| {
                    ReadError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("entry index {entry_idx} is out of range for sealed unit {idx}"),
                    ))
                })?;
                let file = self.dir.open_sealed(su)?;
                PgmUnit::open(file)?.decode(entry)
            }
            Handle::Active(i) => {
                let ap = &self.scan.active[i];
                let cached_entry = ap.catalog.entries.get(entry_idx).ok_or_else(|| {
                    ReadError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("entry index {entry_idx} is out of range for active unit {idx}"),
                    ))
                })?;
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
                let unit = PgmUnit::open(bytes.as_slice())?;
                if unit.catalog() != &ap.catalog {
                    return Err(ReadError::StaleSnapshot { unit_idx: idx });
                }
                unit.decode(cached_entry)
            }
        }
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
        let handle = self.handles().nth(idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unit index {idx} is out of range"),
            ))
        })?;
        match handle {
            Handle::Sealed(i) => {
                let su = &self.scan.sealed[i];
                let entry = su.catalog.entries.get(entry_idx).ok_or_else(|| {
                    ReadError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("entry index {entry_idx} is out of range for sealed unit {idx}"),
                    ))
                })?;
                let file = self.dir.open_sealed(su)?;
                PgmUnit::open(file)?.decode_rows(entry)
            }
            Handle::Active(i) => {
                let ap = &self.scan.active[i];
                let cached_entry = ap.catalog.entries.get(entry_idx).ok_or_else(|| {
                    ReadError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("entry index {entry_idx} is out of range for active unit {idx}"),
                    ))
                })?;
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
                let unit = PgmUnit::open(bytes.as_slice())?;
                if unit.catalog() != &ap.catalog {
                    return Err(ReadError::StaleSnapshot { unit_idx: idx });
                }
                unit.decode_rows(cached_entry)
            }
        }
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
        let handle = self.handles().nth(idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unit index {idx} is out of range"),
            ))
        })?;
        match handle {
            Handle::Sealed(i) => {
                let su = &self.scan.sealed[i];
                let file = self.dir.open_sealed(su)?;
                PgmUnit::open(file)?.dictionary()
            }
            Handle::Active(i) => {
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
                let unit = PgmUnit::open(bytes.as_slice())?;
                if unit.catalog() != &ap.catalog {
                    return Err(ReadError::StaleSnapshot { unit_idx: idx });
                }
                unit.dictionary()
            }
        }
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
        #[cfg(test)]
        OPEN_UNIT_CALLS.with(|c| c.set(c.get() + 1));

        let handle = self.handles().nth(idx).ok_or_else(|| {
            ReadError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unit index {idx} is out of range"),
            ))
        })?;
        match handle {
            Handle::Sealed(i) => {
                let su = &self.scan.sealed[i];
                let file = self.dir.open_sealed(su)?;
                Ok(OpenUnit::Sealed(PgmUnit::open(file)?))
            }
            Handle::Active(i) => {
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

    /// Iterator over `Handle` values in the same order as `units()`.
    fn handles(&self) -> impl Iterator<Item = Handle> + '_ {
        let sealed_iter = (0..self.scan.sealed.len()).map(Handle::Sealed);

        let active_iter = self
            .scan
            .active
            .iter()
            .enumerate()
            .filter(|(_, ap)| !self.scan.sealed.iter().any(|su| su.catalog == ap.catalog))
            .map(|(i, _)| Handle::Active(i));

        sealed_iter.chain(active_iter)
    }
}

/// Snapshots the sealed segment descriptors visible in a scan, in scan order.
fn sealed_descriptors(scan: &LocalScan) -> io::Result<Vec<SegmentDescriptor>> {
    scan.sealed
        .iter()
        .map(|sealed| {
            let file_name = sealed.path.file_name().ok_or_else(|| {
                io::Error::other("sealed segment path has no direct-child file name")
            })?;
            Ok(SegmentDescriptor::from_catalog(
                SealedLocator::from_file_name_bytes(file_name.as_bytes()),
                &sealed.catalog,
            ))
        })
        .collect()
}

#[derive(Debug)]
struct SealedDeltaState {
    added: Vec<SegmentDescriptor>,
    removed: Vec<SegmentDescriptor>,
    baseline: Vec<SegmentDescriptor>,
}

fn sealed_delta(
    scan: &LocalScan,
    previous: &[SegmentDescriptor],
    root: &Path,
) -> io::Result<SealedDeltaState> {
    let current = sealed_descriptors(scan)?;
    let listing_authoritative = !scan.warnings.iter().any(|warning| warning.path == root);
    let unavailable = scan
        .warnings
        .iter()
        .filter_map(|warning| sealed_warning_locator(warning, root))
        .collect::<std::collections::BTreeSet<_>>();
    let added = difference(&current, previous);
    let current_locators = current
        .iter()
        .map(|descriptor| descriptor.locator)
        .collect::<std::collections::BTreeSet<_>>();
    let mut removed = Vec::new();
    let mut baseline = current;

    for descriptor in difference(previous, &baseline) {
        if current_locators.contains(&descriptor.locator) {
            removed.push(descriptor);
        } else if !listing_authoritative || unavailable.contains(&descriptor.locator) {
            baseline.push(descriptor);
        } else {
            removed.push(descriptor);
        }
    }

    Ok(SealedDeltaState {
        added,
        removed,
        baseline,
    })
}

fn sealed_warning_locator(warning: &StoreWarning, root: &Path) -> Option<SealedLocator> {
    if warning.path.parent() != Some(root)
        || warning
            .path
            .extension()
            .is_none_or(|extension| extension.as_bytes() != b"pgm")
    {
        return None;
    }
    warning
        .path
        .file_name()
        .map(|name| SealedLocator::from_file_name_bytes(name.as_bytes()))
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
                part_id: part_id(generation, frame_offset, body_len, &active.catalog),
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
    root: &Path,
) -> io::Result<(LocalScan, Option<JournalIdentity>, JournalPrefixDigest)> {
    for _attempt in 0..2 {
        let identity_before = journal_identity(root)?;
        let scan = dir.scan()?;
        let prefix_digest = match journal_prefix_digest(root, scan.valid_len) {
            Ok(digest) => digest,
            Err(error) if is_journal_race(&error) => continue,
            Err(error) => return Err(error),
        };
        let identity_after = journal_identity(root)?;
        if identity_before == identity_after {
            return Ok((scan, identity_after, prefix_digest));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "active.parts changed during two consecutive scans",
    ))
}

fn is_journal_race(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
    )
}

fn journal_prefix_digest(root: &Path, valid_len: u64) -> io::Result<JournalPrefixDigest> {
    let mut hasher = Sha256::new();
    hasher.update(JOURNAL_PREFIX_DOMAIN);
    if valid_len == 0 {
        return Ok(JournalPrefixDigest(hasher.finalize().into()));
    }

    let mut file = std::fs::File::open(root.join("active.parts"))?;
    let mut remaining = valid_len;
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

/// Reads the observable identity of `active.parts`.
fn journal_identity(root: &Path) -> io::Result<Option<JournalIdentity>> {
    let metadata = match std::fs::metadata(root.join("active.parts")) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
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

/// Descriptors present in `left` but not in `right`, preserving `left` order.
fn difference(left: &[SegmentDescriptor], right: &[SegmentDescriptor]) -> Vec<SegmentDescriptor> {
    let mut remaining = std::collections::BTreeMap::<SegmentDescriptor, usize>::new();
    for descriptor in right {
        *remaining.entry(*descriptor).or_default() += 1;
    }
    let mut difference = Vec::new();
    for descriptor in left {
        match remaining.get_mut(descriptor) {
            Some(count) if *count > 0 => *count -= 1,
            _ => difference.push(*descriptor),
        }
    }
    difference
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
            .zip(&current.active)
            .all(|(left, right)| left.part == right.part && left.catalog == right.catalog)
}

fn same_sealed_units(previous: &LocalScan, current: &LocalScan) -> bool {
    previous.sealed.len() == current.sealed.len()
        && previous
            .sealed
            .iter()
            .zip(&current.sealed)
            .all(|(left, right)| left.path == right.path && left.catalog == right.catalog)
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
