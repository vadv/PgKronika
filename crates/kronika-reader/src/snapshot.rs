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
            Self::Build(error) => write!(f, "sealed fact build failed: {error}"),
        }
    }
}

impl std::error::Error for SealedFactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::UnitOutOfRange { .. } | Self::LiveUnit { .. } | Self::StaleSnapshot { .. } => {
                None
            }
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
        || !warning
            .path
            .extension()
            .is_some_and(|extension| extension.as_bytes() == b"pgm")
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
    let mut buffer = [0_u8; JOURNAL_HASH_BUFFER_BYTES];
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
mod tests {
    use std::fs::{self, FileTimes};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::time::{Duration, UNIX_EPOCH};

    use kronika_analytics::overview::{NamingContractId, SegmentLocator};
    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::Ts;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_log::PgLogLifecycleV1;

    use super::*;
    use crate::{FactOrigin, LIMIT};

    /// Build one minimal valid part with a real section.
    fn make_part(min_ts: i64, max_ts: i64, source_id: u64) -> Vec<u8> {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id,
            },
        )
    }

    /// Wrap `part_bytes` in a journal frame.
    fn framed(part_bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &FrameHeader {
                part_len: part_bytes.len() as u64,
            }
            .encode(),
        );
        buf.extend_from_slice(part_bytes);
        buf
    }

    fn lifecycle_part(source_id: u64) -> Vec<u8> {
        let rows = [PgLogLifecycleV1 {
            ts: Ts(1_500),
            kind: 0,
            pid: Some(42),
            signal: Some(9),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        }];
        let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle");
        build_part(
            &[SectionInput {
                type_id: 1_028_001,
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_500,
                max_ts: 1_500,
                source_id,
            },
        )
    }

    fn fact_context() -> SegmentContext {
        SegmentContext::new(
            b"snapshot-store".to_vec(),
            NamingContractId([0x11; 16]),
            SegmentLocator([0x22; 32]),
        )
        .expect("valid context")
    }

    #[test]
    fn sealed_snapshot_cache_hit_after_reopen_reads_no_pgm_bodies() {
        let source = tempfile::tempdir().expect("source directory");
        let cache = tempfile::tempdir().expect("cache directory");
        fs::write(source.path().join("1500.pgm"), lifecycle_part(7)).expect("write segment");
        let store = FactStore::new(cache.path());

        let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
        let cold = snapshot
            .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
            .expect("cold facts");
        assert_eq!(cold.origin(), FactOrigin::Rebuilt);
        assert_eq!(cold.pgm_body_read_stats().read_calls, 1);

        let restarted = LocalDirSnapshot::open(source.path()).expect("restart snapshot");
        let warm = restarted
            .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
            .expect("warm facts");
        assert_eq!(warm.origin(), FactOrigin::CacheHit);
        assert_eq!(warm.pgm_body_read_stats().read_calls, 0);
        assert_eq!(warm.facts().observations(), cold.facts().observations());
    }

    #[test]
    fn same_name_replacement_invalidates_pinned_snapshot() {
        let source = tempfile::tempdir().expect("source directory");
        let cache = tempfile::tempdir().expect("cache directory");
        let path = source.path().join("1500.pgm");
        fs::write(&path, lifecycle_part(7)).expect("write first segment");
        let pinned = LocalDirSnapshot::open(source.path()).expect("open pinned snapshot");
        let store = FactStore::new(cache.path());
        pinned
            .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
            .expect("first facts");

        fs::write(&path, lifecycle_part(8)).expect("replace segment");
        assert!(matches!(
            pinned.load_sealed_facts(0, &store, &fact_context(), &LIMIT),
            Err(SealedFactError::StaleSnapshot { unit_idx: 0 })
        ));

        let refreshed = LocalDirSnapshot::open(source.path()).expect("refresh snapshot");
        let replacement = refreshed
            .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
            .expect("replacement facts");
        assert_eq!(replacement.facts().identity().pgm_source_id, 8);
        assert_eq!(replacement.origin(), FactOrigin::Rebuilt);
    }

    #[test]
    fn removed_source_is_not_resurrected_by_an_orphan_fact_file() {
        let source = tempfile::tempdir().expect("source directory");
        let cache = tempfile::tempdir().expect("cache directory");
        let path = source.path().join("1500.pgm");
        fs::write(&path, lifecycle_part(7)).expect("write segment");
        let store = FactStore::new(cache.path());
        LocalDirSnapshot::open(source.path())
            .expect("open snapshot")
            .load_sealed_facts(0, &store, &fact_context(), &LIMIT)
            .expect("build facts");
        fs::remove_file(path).expect("remove authoritative segment");

        let after_retention = LocalDirSnapshot::open(source.path()).expect("rescan source");
        assert!(after_retention.units().is_empty());
        assert!(matches!(
            after_retention.load_sealed_facts(0, &store, &fact_context(), &LIMIT),
            Err(SealedFactError::UnitOutOfRange { unit_idx: 0 })
        ));
        let orphan_exists = walk_files(cache.path())
            .iter()
            .any(|path| path.extension().and_then(|value| value.to_str()) == Some("ovf"));
        assert!(
            orphan_exists,
            "source retention does not remove disposable cache files"
        );
    }

    #[test]
    fn active_part_is_rejected_by_sealed_fact_loader() {
        let source = tempfile::tempdir().expect("source directory");
        let cache = tempfile::tempdir().expect("cache directory");
        fs::write(
            source.path().join("active.parts"),
            framed(&lifecycle_part(7)),
        )
        .expect("write active part");
        let snapshot = LocalDirSnapshot::open(source.path()).expect("open snapshot");
        assert!(matches!(
            snapshot.load_sealed_facts(0, &FactStore::new(cache.path()), &fact_context(), &LIMIT),
            Err(SealedFactError::LiveUnit { unit_idx: 0 })
        ));
    }

    fn walk_files(root: &Path) -> Vec<PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).expect("walk cache") {
                let path = entry.expect("cache entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    #[test]
    fn live_part_is_visible_before_seal() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 1);
        let journal: Vec<u8> = framed(&part);
        fs::write(dir.path().join("active.parts"), &journal).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        assert_eq!(units.len(), 1, "one live part expected");
        assert!(units[0].live, "part must be marked live");
        assert_eq!(units[0].source_id, 1);
        assert_eq!(units[0].min_ts, 1000);
        assert_eq!(units[0].max_ts, 2000);
    }

    #[test]
    fn exact_sealed_active_catalog_is_deduped_no_double() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 42);
        // Write the same data as a sealed .pgm.
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();
        // And as the exact same active part.
        let journal = framed(&part);
        fs::write(dir.path().join("active.parts"), &journal).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        assert_eq!(units.len(), 1, "exact active duplicate must be deduped");
        assert!(!units[0].live, "surviving unit must be the sealed one");
        assert_eq!(units[0].source_id, 42);
    }

    #[test]
    fn overlapping_active_part_is_not_deduped_by_range_only() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = make_part(1000, 5000, 42);
        let active = make_part(2000, 3000, 42);
        fs::write(dir.path().join("1000.pgm"), &sealed).unwrap();
        fs::write(dir.path().join("active.parts"), framed(&active)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        assert_eq!(
            units.len(),
            2,
            "range overlap must not hide a distinct live part"
        );
        assert!(
            units
                .iter()
                .any(|u| !u.live && u.min_ts == 1000 && u.max_ts == 5000),
            "sealed unit must remain visible"
        );
        assert!(
            units
                .iter()
                .any(|u| u.live && u.min_ts == 2000 && u.max_ts == 3000),
            "overlapping live unit must remain visible"
        );
    }

    #[test]
    fn refresh_picks_up_appended_part() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let journal = framed(&part1);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, &journal).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);

        // Append a second part.
        let part2 = make_part(3000, 4000, 1);
        let mut journal_buf = fs::read(&journal_path).unwrap();
        journal_buf.extend_from_slice(&framed(&part2));
        fs::write(&journal_path, &journal_buf).unwrap();

        snap.refresh().unwrap();
        assert_eq!(snap.units().len(), 2, "refresh must surface the new part");
    }

    #[test]
    fn refresh_incremental_surfaces_appended_part_and_keeps_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let journal_path = dir.path().join("active.parts");
        let journal = framed(&part1);
        fs::write(&journal_path, &journal).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);
        let first_offset = snap.scan.active[0].part.offset;
        let valid_before = snap.last_valid_len;
        assert_eq!(valid_before, journal.len() as u64);

        // Append a second part.
        let part2 = make_part(3000, 4000, 1);
        let mut buf = fs::read(&journal_path).unwrap();
        let appended = framed(&part2);
        buf.extend_from_slice(&appended);
        fs::write(&journal_path, &buf).unwrap();

        snap.refresh_incremental().unwrap();
        assert_eq!(
            snap.units().len(),
            2,
            "incremental refresh surfaces the new part"
        );
        assert_eq!(
            snap.scan.active[0].part.offset, first_offset,
            "the first part is carried over, not re-scanned"
        );
        assert_eq!(
            snap.last_valid_len,
            valid_before + appended.len() as u64,
            "valid_len advances by exactly the appended frame"
        );
    }

    #[test]
    fn refresh_incremental_noop_when_journal_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part1)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let first_offset = snap.scan.active[0].part.offset;
        let valid_before = snap.last_valid_len;

        snap.refresh_incremental().unwrap();

        assert_eq!(
            snap.units().len(),
            1,
            "unchanged journal keeps its one unit"
        );
        assert_eq!(
            snap.scan.active[0].part.offset, first_offset,
            "the part is carried unchanged, the journal body is not re-read"
        );
        assert_eq!(
            snap.last_valid_len, valid_before,
            "valid_len is unchanged on a noop refresh"
        );
    }

    #[test]
    fn refresh_incremental_reset_when_journal_truncated_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part1)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);

        // Truncate-in-place to zero (a reset).
        fs::write(&journal_path, b"").unwrap();

        snap.refresh_incremental().unwrap();
        assert!(snap.units().is_empty(), "reset clears the live parts");
        assert_eq!(snap.last_valid_len, 0, "valid_len resets to zero");
    }

    #[test]
    fn refresh_incremental_torn_tail_holds_then_completes() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let journal_path = dir.path().join("active.parts");
        let journal = framed(&part1);
        fs::write(&journal_path, &journal).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let valid_before = snap.last_valid_len;

        // Append a truncated (incomplete) second frame.
        let part2 = make_part(3000, 4000, 1);
        let full = framed(&part2);
        let mut buf = journal.clone();
        buf.extend_from_slice(&full[..full.len() - 3]);
        fs::write(&journal_path, &buf).unwrap();

        snap.refresh_incremental().unwrap();
        assert_eq!(snap.units().len(), 1, "torn tail must not surface a part");
        assert_eq!(
            snap.last_valid_len, valid_before,
            "valid_len does not move past the last complete frame"
        );

        // Complete the frame; the next incremental refresh sees it.
        let mut done = journal;
        done.extend_from_slice(&full);
        fs::write(&journal_path, &done).unwrap();

        snap.refresh_incremental().unwrap();
        assert_eq!(
            snap.units().len(),
            2,
            "completed frame is surfaced next tick"
        );
        assert_eq!(snap.last_valid_len, done.len() as u64);
    }

    #[test]
    fn refresh_incremental_discovers_new_sealed_segment() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(2000, 3000, 1);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part1)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);
        assert!(snap.units()[0].live);

        // A new sealed segment appears in the directory.
        let sealed = make_part(500, 1000, 1);
        fs::write(dir.path().join("0500.pgm"), &sealed).unwrap();

        snap.refresh_incremental().unwrap();
        let units = snap.units();
        assert_eq!(units.len(), 2, "new sealed segment is discovered");
        assert!(
            units.iter().any(|u| !u.live && u.min_ts == 500),
            "the sealed unit is visible"
        );
        assert!(
            units.iter().any(|u| u.live && u.min_ts == 2000),
            "the live part remains visible"
        );
    }

    #[test]
    fn middle_corruption_reported_through_snapshot_api_rest_served() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let part2 = make_part(3000, 4000, 1);
        let mut journal = framed(&part1);
        journal.extend_from_slice(b"GARBAGE_BYTES_HERE_THAT_ARE_NOT_A_VALID_FRAME");
        journal.extend_from_slice(&framed(&part2));
        fs::write(dir.path().join("active.parts"), &journal).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        assert_eq!(units.len(), 2, "both valid parts must be visible");
        assert!(
            !snap.damages().is_empty(),
            "corrupt region must be recorded as a damage"
        );
        let damage = snap.damages().to_vec();
        snap.refresh_incremental().expect("incremental refresh");
        assert_eq!(
            snap.damages(),
            damage,
            "a tail-only refresh retains earlier middle damage"
        );
    }

    #[test]
    fn decode_unit_sealed_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);
        assert!(!snap.units()[0].live);

        let catalog = snap.unit_catalog(0).expect("catalog for unit 0");
        assert!(!catalog.entries.is_empty());

        let decoded = snap.decode_unit(0, 0).expect("decode sealed unit");
        assert_eq!(decoded.stats.type_id, 1_006_001);
    }

    #[test]
    fn decode_unit_active_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        let journal = framed(&part);
        fs::write(dir.path().join("active.parts"), &journal).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);
        assert!(snap.units()[0].live);

        let catalog = snap.unit_catalog(0).expect("catalog for unit 0");
        assert!(!catalog.entries.is_empty());

        let decoded = snap.decode_unit(0, 0).expect("decode active unit");
        assert_eq!(decoded.stats.type_id, 1_006_001);
    }

    #[test]
    fn decode_unit_out_of_range_unit_idx() {
        let dir = tempfile::tempdir().unwrap();
        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let err = snap.decode_unit(99, 0).unwrap_err();
        assert!(
            matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput),
            "out-of-range unit index must return InvalidInput"
        );
    }

    #[test]
    fn decode_unit_out_of_range_entry_idx() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // Entry 0 exists, entry 99 does not.
        let err = snap.decode_unit(0, 99).unwrap_err();
        assert!(
            matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput),
            "out-of-range entry index must return InvalidInput"
        );
    }

    #[test]
    fn decode_unit_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let part_a = make_part(1000, 2000, 1);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part_a)).unwrap();

        // Snapshot taken while part_a is live.
        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);

        // Replace the journal with a different part (different timestamps).
        let part_b = make_part(5000, 6000, 2);
        fs::write(&journal_path, framed(&part_b)).unwrap();

        // The cached offset now maps to bytes belonging to a different part.
        let err = snap.decode_unit(0, 0).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "replaced journal must trigger StaleSnapshot, got: {err}"
        );
    }

    #[test]
    fn missing_active_parts_is_empty_live_journal() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        // Write only a sealed file — no active.parts.
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        // The sealed unit is present; no live parts (active.parts absent).
        assert_eq!(units.len(), 1);
        assert!(!units[0].live);
        assert!(snap.scan.active.is_empty());
        assert!(snap.damages().is_empty());
    }

    // ---- decode_unit_rows / unit_dictionary tests ----

    use kronika_format::DictLimits;
    use kronika_registry::Cell;
    use kronika_registry::StrId;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_writer::Interner;
    use kronika_writer::dict;

    /// Build a part with one `pg_stat_archiver` row (carrying a `StrId`) and
    /// the corresponding `dict.strings` section. Returns the part bytes and the
    /// interned `str_id` for the WAL file name.
    fn make_archiver_part_with_dict(min_ts: i64, max_ts: i64, source_id: u64) -> (Vec<u8>, u64) {
        let mut interner = Interner::new(DictLimits::new(256, 1 << 20).expect("limits"));
        let wal_id = interner
            .intern(b"000000010000000000000001")
            .expect("intern");

        let archiver_body = PgStatArchiver::encode(&[PgStatArchiver {
            ts: Ts(min_ts),
            archived_count: 5,
            last_archived_wal: Some(StrId(wal_id.get())),
            last_archived_time: Some(Ts(min_ts - 1000)),
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }])
        .expect("encode archiver");

        let dict_sections = dict::encode(interner.window()).expect("encode dict");
        // Collect owned bodies so all SectionInput borrows can point to them.
        let dict_owned: Vec<(u32, u32, Vec<u8>)> = dict_sections
            .into_iter()
            .map(|s| (s.type_id, s.rows, s.body))
            .collect();

        let mut all: Vec<SectionInput<'_>> = vec![SectionInput {
            type_id: 1_008_001,
            rows: 1,
            body: &archiver_body,
        }];
        for (type_id, rows, body) in &dict_owned {
            all.push(SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            });
        }

        let bytes = build_part(
            &all,
            PartMeta {
                min_ts,
                max_ts,
                source_id,
            },
        );
        (bytes, wal_id.get())
    }

    #[test]
    fn decode_unit_rows_sealed_and_active_match() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, _wal_id) = make_archiver_part_with_dict(1000, 2000, 9);

        // Write as sealed.
        fs::write(dir.path().join("1000.pgm"), &part_bytes).unwrap();
        let sealed_snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(!sealed_snap.units()[0].live);
        // pg_stat_archiver is entry 0 (first non-dict section, but dict sections
        // come after data sections in our fixture, so entry 0 is archiver).
        let catalog = sealed_snap.unit_catalog(0).expect("catalog");
        let archiver_entry_idx = catalog
            .entries
            .iter()
            .position(|e| e.type_id == 1_008_001)
            .expect("archiver entry");
        let sealed_rows = sealed_snap
            .decode_unit_rows(0, archiver_entry_idx)
            .expect("decode sealed rows");

        // Write same bytes as active part.
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part_bytes)).unwrap();
        let active_snap = LocalDirSnapshot::open(dir.path()).unwrap();
        // The active part is deduped by the sealed unit, so only 1 unit total.
        // The sealed unit is at index 0. Write only active (remove sealed).
        fs::remove_file(dir.path().join("1000.pgm")).unwrap();
        let active_snap2 = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(active_snap2.units()[0].live);
        let catalog2 = active_snap2.unit_catalog(0).expect("catalog");
        let archiver_entry_idx2 = catalog2
            .entries
            .iter()
            .position(|e| e.type_id == 1_008_001)
            .expect("archiver entry");
        let active_rows = active_snap2
            .decode_unit_rows(0, archiver_entry_idx2)
            .expect("decode active rows");

        assert_eq!(
            sealed_rows, active_rows,
            "sealed and active paths yield identical named-cell rows"
        );
        assert_eq!(sealed_rows.len(), 1, "one row decoded");
        assert_eq!(
            sealed_rows[0].get("archived_count"),
            Some(&Cell::I64(5)),
            "archived_count cell"
        );
        // last_archived_wal carries a StrId.
        assert!(
            matches!(
                sealed_rows[0].get("last_archived_wal"),
                Some(&Cell::StrId(_))
            ),
            "last_archived_wal is a StrId cell"
        );
        // Suppress the active_snap binding unused warning.
        drop(active_snap);
    }

    #[test]
    fn unit_dictionary_resolves_interned_str_id() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
        fs::write(dir.path().join("1000.pgm"), &part_bytes).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let dict = snap.unit_dictionary(0).expect("unit dictionary");
        assert!(!dict.is_empty(), "at least one entry in the dictionary");
        let resolved = dict.resolve(wal_id).expect("wal_id is in the dictionary");
        assert_eq!(
            resolved,
            crate::Resolved::String(b"000000010000000000000001"),
            "str_id resolves to the interned WAL name"
        );
    }

    #[test]
    fn unit_dictionary_active_resolves_str_id() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part_bytes)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);
        let dict = snap.unit_dictionary(0).expect("unit dictionary for active");
        let resolved = dict.resolve(wal_id).expect("wal_id resolved");
        assert_eq!(
            resolved,
            crate::Resolved::String(b"000000010000000000000001"),
        );
    }

    #[test]
    fn decode_unit_rows_stale_after_journal_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, _) = make_archiver_part_with_dict(1000, 2000, 9);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part_bytes)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);
        let archiver_entry_idx = snap
            .unit_catalog(0)
            .expect("catalog")
            .entries
            .iter()
            .position(|e| e.type_id == 1_008_001)
            .expect("archiver entry");

        fs::remove_file(&journal_path).unwrap();

        let err = snap.decode_unit_rows(0, archiver_entry_idx).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "removed journal must return StaleSnapshot for decode_unit_rows, got: {err}"
        );
    }

    #[test]
    fn unit_dictionary_stale_after_journal_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, _) = make_archiver_part_with_dict(1000, 2000, 9);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part_bytes)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);

        fs::remove_file(&journal_path).unwrap();

        let err = snap.unit_dictionary(0).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "removed journal must return StaleSnapshot for unit_dictionary, got: {err}"
        );
    }

    // When active.parts disappears (removed or truncated to zero) between
    // snapshot time and decode_unit time, read_active_part returns NotFound or
    // UnexpectedEof. decode_unit must map that to StaleSnapshot, not ReadError::Io.
    #[test]
    fn decode_unit_active_truncated_after_snapshot_returns_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);
        assert!(snap.units()[0].live);

        // Remove the journal file to simulate post-seal reset.
        fs::remove_file(&journal_path).unwrap();

        let err = snap.decode_unit(0, 0).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "removed journal must return StaleSnapshot, got: {err}"
        );
    }

    #[test]
    fn decode_unit_active_zero_truncated_after_snapshot_returns_stale_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);
        assert!(snap.units()[0].live);

        fs::write(&journal_path, b"").unwrap();

        let err = snap.decode_unit(0, 0).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "truncated journal must return StaleSnapshot, got: {err}"
        );
    }

    // ---- open_unit / OpenUnit tests ----

    #[test]
    fn open_unit_sealed_decodes_rows_and_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
        fs::write(dir.path().join("1000.pgm"), &part_bytes).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let unit = snap.open_unit(0).expect("open sealed unit");
        assert!(matches!(unit, OpenUnit::Sealed(_)));
        assert_eq!(unit.catalog().source_id, 9);

        let archiver = unit
            .catalog()
            .entries
            .iter()
            .find(|e| e.type_id == 1_008_001)
            .expect("archiver entry");
        let rows = unit.decode_rows(archiver).expect("decode rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("archived_count"), Some(&Cell::I64(5)));

        let dict = unit.dictionary().expect("dictionary");
        assert_eq!(
            dict.resolve(wal_id).expect("resolve"),
            crate::Resolved::String(b"000000010000000000000001")
        );
    }

    #[test]
    fn open_unit_active_decodes_rows_and_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let (part_bytes, wal_id) = make_archiver_part_with_dict(1000, 2000, 9);
        fs::write(dir.path().join("active.parts"), framed(&part_bytes)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);
        let unit = snap.open_unit(0).expect("open active unit");
        assert!(matches!(unit, OpenUnit::Active(_)));
        assert_eq!(unit.catalog().source_id, 9);

        let archiver = unit
            .catalog()
            .entries
            .iter()
            .find(|e| e.type_id == 1_008_001)
            .expect("archiver entry");
        let rows = unit.decode_rows(archiver).expect("decode rows");
        assert_eq!(rows.len(), 1);

        let dict = unit.dictionary().expect("dictionary");
        assert_eq!(
            dict.resolve(wal_id).expect("resolve"),
            crate::Resolved::String(b"000000010000000000000001")
        );
    }

    #[test]
    fn open_unit_out_of_range_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let err = snap.open_unit(99).unwrap_err();
        assert!(
            matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::InvalidInput),
            "out-of-range unit index must return InvalidInput"
        );
    }

    #[test]
    fn open_unit_active_stale_after_journal_removed() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert!(snap.units()[0].live);

        fs::remove_file(&journal_path).unwrap();

        let err = snap.open_unit(0).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "removed journal must return StaleSnapshot, got: {err}"
        );
    }

    #[test]
    fn open_unit_active_stale_when_journal_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let part_a = make_part(1000, 2000, 1);
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part_a)).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        assert_eq!(snap.units().len(), 1);

        // Replace with a different part so the cached catalog no longer matches.
        let part_b = make_part(5000, 6000, 2);
        fs::write(&journal_path, framed(&part_b)).unwrap();

        let err = snap.open_unit(0).unwrap_err();
        assert!(
            matches!(err, ReadError::StaleSnapshot { unit_idx: 0 }),
            "replaced journal must trigger StaleSnapshot, got: {err}"
        );
    }

    #[test]
    fn open_unit_increments_the_test_counter() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 7);
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        OPEN_UNIT_CALLS.with(|c| c.set(0));
        drop(snap.open_unit(0).expect("open"));
        drop(snap.open_unit(0).expect("open"));
        assert_eq!(OPEN_UNIT_CALLS.with(std::cell::Cell::get), 2);
    }

    #[test]
    fn refresh_delta_reports_appended_part_as_completed_under_the_same_generation() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&make_part(1000, 2000, 1))).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let generation_before = snap.journal_generation();
        let initial = snap.refresh_incremental_delta().expect("initial delta");
        assert!(initial.journal.bootstrap);
        assert_eq!(initial.journal.completed_parts.len(), 1);
        assert_eq!(
            initial.journal.current_parts,
            initial.journal.completed_parts
        );
        assert!(initial.journal.current_parts_complete);
        assert_eq!(initial.journal.completed_parts[0].min_ts, 1000);

        let appended = framed(&make_part(3000, 4000, 1));
        let mut buf = fs::read(&journal_path).unwrap();
        buf.extend_from_slice(&appended);
        fs::write(&journal_path, &buf).unwrap();

        let delta = snap.refresh_incremental_delta().expect("delta");
        assert!(!delta.journal.bootstrap);
        assert_eq!(delta.journal.transition, PartTransition::Append);
        assert_eq!(delta.journal.generation_id, generation_before);
        assert_eq!(
            delta.journal.completed_parts.len(),
            1,
            "only the newly appended part is completed"
        );
        assert_eq!(
            delta.journal.current_parts.len(),
            2,
            "completion evidence includes the old and appended parts"
        );
        assert_eq!(delta.journal.current_parts[0].min_ts, 1000);
        assert_eq!(delta.journal.current_parts[1].min_ts, 3000);
        let final_part = delta.journal.current_parts.last().expect("final part");
        assert_eq!(
            final_part.part_id.frame_offset + final_part.part_id.body_len,
            delta.journal.new_valid_len,
            "the complete descriptor set reaches the validated watermark"
        );
        assert!(delta.journal.current_parts_complete);
        assert_eq!(delta.journal.completed_parts[0].min_ts, 3000);
        assert!(delta.new_view_generation > delta.previous_view_generation);
        assert!(!delta.requires_live_rebuild());
    }

    #[test]
    fn refresh_delta_redelivery_is_idempotent_and_reports_no_new_parts() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&make_part(1000, 2000, 1))).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let initial = snap.refresh_incremental_delta().expect("initial delta");
        assert_eq!(initial.journal.completed_parts.len(), 1);
        let mut buf = fs::read(&journal_path).unwrap();
        buf.extend_from_slice(&framed(&make_part(3000, 4000, 1)));
        fs::write(&journal_path, &buf).unwrap();

        let first = snap.refresh_incremental_delta().expect("first delta");
        assert_eq!(first.journal.completed_parts.len(), 1);
        let generation = first.journal.generation_id;

        let second = snap.refresh_incremental_delta().expect("second delta");
        assert!(
            second.journal.completed_parts.is_empty(),
            "an unchanged journal re-delivers no parts"
        );
        assert_eq!(
            second.journal.current_parts.len(),
            2,
            "completion evidence remains whole on a no-op refresh"
        );
        assert_eq!(second.journal.generation_id, generation);
        assert_eq!(second.new_view_generation, first.new_view_generation);
    }

    #[test]
    fn refresh_delta_truncation_resets_and_mints_a_new_generation() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&make_part(1000, 2000, 1))).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let generation_before = snap.journal_generation();

        fs::write(&journal_path, b"").unwrap();

        let delta = snap.refresh_incremental_delta().expect("delta");
        assert_eq!(delta.journal.transition, PartTransition::Reset);
        assert_ne!(delta.journal.generation_id, generation_before);
        assert_eq!(delta.journal.new_valid_len, 0);
        assert!(delta.journal.completed_parts.is_empty());
        assert!(delta.journal.current_parts.is_empty());
        assert!(delta.journal.current_parts_complete);
        assert!(delta.requires_live_rebuild());
    }

    #[test]
    fn refresh_delta_torn_tail_reports_pending_bytes_and_holds_the_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let base = framed(&make_part(1000, 2000, 1));
        fs::write(&journal_path, &base).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let valid_before = snap.last_valid_len;
        let initial = snap.refresh_incremental_delta().expect("initial delta");
        assert_eq!(initial.journal.completed_parts.len(), 1);

        let full = framed(&make_part(3000, 4000, 1));
        let mut buf = base;
        buf.extend_from_slice(&full[..full.len() - 3]);
        let torn_len = buf.len() as u64;
        fs::write(&journal_path, &buf).unwrap();

        let delta = snap.refresh_incremental_delta().expect("delta");
        assert_eq!(delta.journal.transition, PartTransition::Append);
        assert!(
            delta.journal.completed_parts.is_empty(),
            "a torn frame surfaces no completed part"
        );
        assert_eq!(
            delta.journal.current_parts.len(),
            1,
            "the whole validated prefix remains the completion target"
        );
        assert!(delta.journal.current_parts_complete);
        assert_eq!(delta.journal.new_valid_len, valid_before, "watermark holds");
        assert_eq!(
            delta.journal.tail_pending,
            Some(ByteRange {
                start: valid_before,
                end: torn_len,
            })
        );
    }

    #[test]
    fn refresh_delta_reports_a_newly_sealed_segment() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("active.parts"),
            framed(&make_part(2000, 3000, 1)),
        )
        .unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let initial = snap.refresh_incremental_delta().expect("initial delta");
        assert_eq!(initial.journal.completed_parts.len(), 1);
        fs::write(dir.path().join("0500.pgm"), make_part(500, 1000, 1)).unwrap();

        let delta = snap.refresh_incremental_delta().expect("delta");
        assert_eq!(delta.sealed_added.len(), 1);
        assert_eq!(delta.sealed_added[0].min_ts, 500);
        assert!(delta.sealed_removed.is_empty());
    }

    #[test]
    fn first_delta_delivers_parts_found_during_open() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("active.parts"),
            framed(&make_part(1000, 2000, 1)),
        )
        .unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let first = snap.refresh_incremental_delta().expect("first delta");
        let second = snap.refresh_incremental_delta().expect("second delta");

        assert_eq!(first.journal.completed_parts.len(), 1);
        assert_eq!(first.journal.completed_parts[0].min_ts, 1000);
        assert!(second.journal.completed_parts.is_empty());
        assert_eq!(second.new_view_generation, first.new_view_generation);
    }

    #[test]
    fn equal_length_rewrite_discards_the_cached_active_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let first_part = framed(&make_part(1000, 2000, 1));
        let replacement = framed(&make_part(3000, 4000, 1));
        assert_eq!(first_part.len(), replacement.len());
        fs::write(&journal_path, first_part).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let initial = snap.refresh_incremental_delta().expect("initial delta");
        let initial_generation = initial.journal.generation_id;

        fs::write(&journal_path, replacement).unwrap();
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&journal_path)
            .unwrap();
        file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
            .unwrap();

        let delta = snap.refresh_incremental_delta().expect("replacement delta");
        assert_eq!(delta.journal.transition, PartTransition::Uncertain);
        assert_ne!(delta.journal.generation_id, initial_generation);
        assert_eq!(delta.journal.completed_parts.len(), 1);
        assert_eq!(delta.journal.completed_parts[0].min_ts, 3000);
        assert_eq!(snap.units()[0].min_ts, 3000);
    }

    #[test]
    fn same_inode_growth_with_a_rewritten_prefix_forces_a_full_generation() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let first = framed(&make_part(1000, 2000, 1));
        fs::write(&journal_path, &first).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let initial = snap.refresh_incremental_delta().expect("initial delta");
        let initial_generation = initial.journal.generation_id;
        let initial_inode = fs::metadata(&journal_path).unwrap().ino();

        let replacement = framed(&make_part(3000, 4000, 1));
        assert_eq!(first.len(), replacement.len());
        let mut rewritten_and_grown = replacement;
        rewritten_and_grown.extend_from_slice(&framed(&make_part(5000, 6000, 1)));
        fs::write(&journal_path, rewritten_and_grown).unwrap();
        assert_eq!(fs::metadata(&journal_path).unwrap().ino(), initial_inode);

        let delta = snap.refresh_incremental_delta().expect("growth delta");
        assert_eq!(delta.journal.transition, PartTransition::Uncertain);
        assert_ne!(delta.journal.generation_id, initial_generation);
        assert_eq!(delta.journal.completed_parts.len(), 2);
        assert_eq!(delta.journal.current_parts, delta.journal.completed_parts);
        assert_eq!(delta.journal.current_parts[0].min_ts, 3000);
        assert_eq!(delta.journal.current_parts[1].min_ts, 5000);
    }

    #[test]
    fn an_incomplete_active_baseline_forces_full_descriptor_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let valid_part = make_part(1000, 2000, 1);
        fs::write(&journal_path, framed(&valid_part)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        snap.scan.active.clear();
        snap.scan.warnings.push(StoreWarning {
            path: journal_path,
            reason: "simulated scan race omitted the validated prefix".to_owned(),
        });
        snap.journal_descriptors_complete = false;
        snap.delta_initialized = true;
        assert!(!snap.journal_descriptors_complete);
        assert!(snap.units().is_empty());

        let recovered = snap
            .refresh_incremental_delta()
            .expect("descriptor recovery");
        assert_eq!(recovered.journal.transition, PartTransition::Uncertain);
        assert!(recovered.journal.current_parts_complete);
        assert_eq!(recovered.journal.completed_parts.len(), 1);
        assert_eq!(
            recovered.journal.current_parts,
            recovered.journal.completed_parts
        );
        assert!(snap.warnings().is_empty());
    }

    #[test]
    fn unreadable_sealed_file_is_not_removed_until_absence_is_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        let sealed_path = dir.path().join("1000.pgm");
        let valid_segment = make_part(1000, 2000, 1);
        fs::write(&sealed_path, &valid_segment).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        snap.refresh_incremental_delta().expect("initial delta");

        fs::write(&sealed_path, b"not a pgm segment").unwrap();
        let unavailable = snap
            .refresh_incremental_delta()
            .expect("warning-bearing delta");
        assert!(unavailable.sealed_removed.is_empty());
        assert!(
            snap.warnings()
                .iter()
                .any(|warning| warning.path == sealed_path)
        );
        assert!(
            unavailable.new_view_generation > unavailable.previous_view_generation,
            "warning-visible raw state advances the view generation"
        );

        fs::write(&sealed_path, &valid_segment).unwrap();
        let recovered = snap.refresh_incremental_delta().expect("readable recovery");
        assert!(recovered.sealed_added.is_empty());
        assert!(recovered.sealed_removed.is_empty());
        assert!(recovered.new_view_generation > recovered.previous_view_generation);
        assert!(snap.warnings().is_empty());

        fs::write(&sealed_path, b"not a pgm segment").unwrap();
        let unavailable_again = snap
            .refresh_incremental_delta()
            .expect("second warning-bearing delta");
        assert!(unavailable_again.sealed_removed.is_empty());

        fs::remove_file(&sealed_path).unwrap();
        let absent = snap
            .refresh_incremental_delta()
            .expect("authoritative absence");
        assert_eq!(
            absent.sealed_removed.len(),
            1,
            "the preserved descriptor is removed exactly once"
        );
        let repeated = snap.refresh_incremental_delta().expect("repeated absence");
        assert!(repeated.sealed_removed.is_empty());
    }

    #[test]
    fn same_name_sealed_replacement_reports_remove_and_add() {
        let dir = tempfile::tempdir().unwrap();
        let sealed_path = dir.path().join("1000.pgm");
        fs::write(&sealed_path, make_part(1000, 2000, 1)).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        snap.refresh_incremental_delta().expect("initial delta");
        fs::write(&sealed_path, make_part(3000, 4000, 1)).unwrap();

        let delta = snap.refresh_incremental_delta().expect("replacement delta");
        assert_eq!(delta.sealed_removed.len(), 1);
        assert_eq!(delta.sealed_added.len(), 1);
        assert_eq!(
            delta.sealed_removed[0].locator, delta.sealed_added[0].locator,
            "the stable file-name locator connects replacement identities"
        );
        assert_ne!(
            delta.sealed_removed[0].catalog_digest,
            delta.sealed_added[0].catalog_digest
        );
    }

    #[test]
    fn root_warning_suppresses_removal_but_journal_warning_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = PgmUnit::open(make_part(1000, 2000, 1).as_slice())
            .expect("unit")
            .catalog()
            .clone();
        let previous = SegmentDescriptor::from_catalog(
            SealedLocator::from_file_name_bytes(b"1000.pgm"),
            &catalog,
        );
        let root_warning = LocalScan {
            sealed: Vec::new(),
            active: Vec::new(),
            damages: Vec::new(),
            warnings: vec![StoreWarning {
                path: dir.path().to_path_buf(),
                reason: "read_dir entry unavailable".to_owned(),
            }],
            valid_len: 0,
        };
        let preserved = sealed_delta(&root_warning, &[previous], dir.path()).expect("delta");
        assert!(preserved.removed.is_empty());
        assert_eq!(preserved.baseline, vec![previous]);

        let replacement_catalog = PgmUnit::open(make_part(3000, 4000, 1).as_slice())
            .expect("replacement unit")
            .catalog()
            .clone();
        let visible_replacement = LocalScan {
            sealed: vec![kronika_store::SealedUnit {
                path: dir.path().join("1000.pgm"),
                catalog: replacement_catalog,
            }],
            warnings: root_warning.warnings.clone(),
            ..root_warning.clone()
        };
        let replaced =
            sealed_delta(&visible_replacement, &[previous], dir.path()).expect("replacement");
        assert_eq!(replaced.removed, vec![previous]);
        assert_eq!(replaced.added.len(), 1);
        assert_eq!(replaced.baseline, replaced.added);

        let journal_warning = LocalScan {
            warnings: vec![StoreWarning {
                path: dir.path().join("active.parts"),
                reason: "journal unavailable".to_owned(),
            }],
            ..root_warning
        };
        let removed = sealed_delta(&journal_warning, &[previous], dir.path()).expect("delta");
        assert_eq!(removed.removed, vec![previous]);
        assert!(removed.baseline.is_empty());
        assert!(!journal_descriptors_complete(&journal_warning, dir.path()));
    }

    #[test]
    fn failed_refresh_preserves_the_previous_delta_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let mut bytes = framed(&make_part(1000, 2000, 1));
        fs::write(&journal_path, &bytes).unwrap();

        let mut snap = LocalDirSnapshot::open(dir.path()).unwrap();
        snap.refresh_incremental_delta().expect("initial delta");
        bytes.extend_from_slice(&framed(&make_part(3000, 4000, 1)));
        fs::write(&journal_path, bytes).unwrap();

        let original_mode = fs::metadata(dir.path()).unwrap().permissions().mode();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o000)).unwrap();
        let failed = snap.refresh_incremental_delta();
        fs::set_permissions(
            dir.path(),
            fs::Permissions::from_mode(original_mode & 0o7777),
        )
        .unwrap();

        assert_eq!(
            failed.expect_err("permission error").kind(),
            io::ErrorKind::PermissionDenied
        );
        let recovered = snap.refresh_incremental_delta().expect("recovered delta");
        assert_eq!(recovered.journal.completed_parts.len(), 1);
        assert_eq!(recovered.journal.completed_parts[0].min_ts, 3000);
    }
}
