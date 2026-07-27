//! Directory-backed storage implementation.

#[cfg(test)]
use std::fs;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use kronika_format::FRAME_HEADER_LEN;
use kronika_format::{
    Catalog, ENTRY_LEN, FORMAT_VERSION, JOURNAL_HEADER_LEN, JournalHeader, JournalHeaderError,
    JournalLimits, JournalScanError, JournalState, MAGIC, MAX_JOURNAL_LEN, MAX_JOURNAL_PARTS,
    MAX_PART_LEN, META_LEN, PartRef, RESET_MARKER_LEN, ReadAt, ResetMarker, ScanReport,
    TAIL_INDEX_LEN, TailIndex, scan_journal_streaming_strict_from, validate_part_catalog,
};
use kronika_layout::{
    DataRoot, FileIdentity, LayoutError, LayoutLimits, LimitKind, SegmentArtifacts, SegmentId,
};

use crate::catalog_summary::{CatalogDigest, CatalogSummary, CatalogSummaryError};
use crate::source::{ActivePart, JournalScan, LocalScan, SealedUnit, StoreError};

/// Upper bound on the catalog block size; guards against corrupt tail indices.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
const ARC_ALLOCATION_OVERHEAD: usize = 2 * size_of::<usize>();
const ACTIVE_ARC_ALLOCATION_BYTES: usize = size_of::<Vec<ActivePart>>() + ARC_ALLOCATION_OVERHEAD;

#[cfg(test)]
std::thread_local! {
    static CATALOG_SUMMARY_READS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

/// A storage directory containing sealed `.pgm` segments and an `active.parts`
/// journal.
#[derive(Debug, Clone)]
pub struct LocalDir {
    root: DataRoot,
    limits: LayoutLimits,
}

impl LocalDir {
    /// Open a local directory as a segment store.
    ///
    /// This opens only the root directory descriptor. The complete owned-tree
    /// grammar and segment contents are validated during
    /// [`scan`](Self::scan) or [`complete_scan`](Self::complete_scan).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `root` is not a directory or cannot be accessed.
    pub fn open(root: &Path) -> io::Result<Self> {
        let root = DataRoot::open(root).map_err(layout_io)?;
        let limits = LayoutLimits::default();
        Ok(Self { root, limits })
    }

    /// Scan the directory for sealed segments and active journal parts.
    ///
    /// Every discovered `.pgm` catalog and the complete version-1
    /// `active.parts` journal must be valid. The journal is scanned streaming,
    /// keeping peak memory bounded to one part body.
    ///
    /// # Errors
    ///
    /// Returns an I/O error for an invalid tree, malformed PGM, malformed
    /// journal, or an inaccessible object. No partial segment set is returned.
    pub fn scan(&self) -> io::Result<LocalScan> {
        let journal = self.scan_journal()?;
        self.complete_scan(journal)
    }

    /// Scan only `active.parts`.
    ///
    /// The returned journal view is complete and can be captured inside a
    /// short file-identity handshake. Sealed-tree traversal is deliberately
    /// deferred to [`complete_scan`](Self::complete_scan).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the journal is malformed, changes during the
    /// scan, or cannot be read.
    pub fn scan_journal(&self) -> io::Result<JournalScan> {
        let journal_path = self.root.diagnostic_path().join("active.parts");
        let journal = self
            .root
            .open_active_journal()
            .map_err(layout_io)
            .map_err(active_journal_io)?;
        journal.map_or_else(
            || empty_journal_scan(self.limits.max_metadata_bytes),
            |file| {
                self.scan_journal_reader_from(
                    &file,
                    JOURNAL_HEADER_LEN as u64,
                    Arc::new(Vec::new()),
                    &journal_path,
                )
                .map_err(active_journal_io)
            },
        )
    }

    /// Incrementally re-scan the store from a known journal offset.
    ///
    /// `last_valid_len` is the end of the last valid frame seen by the previous
    /// scan; `prev_active` are the active parts that scan already validated.
    /// The journal is stat-gated:
    ///
    /// - size `== last_valid_len`: unchanged; `prev_active` is kept as is and the
    ///   journal body is not re-read.
    /// - size `> last_valid_len`: only `[last_valid_len, size)` is scanned and the
    ///   new parts are appended to `prev_active`.
    /// - size `< last_valid_len` or the file is gone: a reset;
    ///   `prev_active` is dropped and the journal is scanned from its v1 header.
    ///
    /// Sealed `.pgm` files are always re-listed. A concurrent journal reset is
    /// reported as a retryable I/O error; structural damage remains fatal.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the tree, a PGM, or the journal cannot be read
    /// and validated completely.
    pub fn scan_from<A>(&self, last_valid_len: u64, prev_active: A) -> io::Result<LocalScan>
    where
        A: Into<Arc<Vec<ActivePart>>>,
    {
        let journal = self.scan_journal_from(last_valid_len, prev_active)?;
        self.complete_scan(journal)
    }

    /// Incrementally scan only `active.parts`.
    ///
    /// This has the same journal resume semantics as [`scan_from`](Self::scan_from)
    /// without traversing the sealed tree.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the journal is malformed, changes during the
    /// scan, or cannot be read.
    pub fn scan_journal_from<A>(
        &self,
        last_valid_len: u64,
        prev_active: A,
    ) -> io::Result<JournalScan>
    where
        A: Into<Arc<Vec<ActivePart>>>,
    {
        let prev_active = prev_active.into();
        let journal_path = self.root.diagnostic_path().join("active.parts");
        let journal = self
            .root
            .open_active_journal()
            .map_err(layout_io)
            .map_err(active_journal_io)?;
        journal.map_or_else(
            || empty_journal_scan(self.limits.max_metadata_bytes),
            |file| {
                self.scan_journal_reader_from(&file, last_valid_len, prev_active, &journal_path)
                    .map_err(active_journal_io)
            },
        )
    }

    /// Complete a captured journal scan with one strict sealed-tree traversal.
    ///
    /// The journal is not opened again. This keeps potentially long sealed
    /// traversal outside a journal identity handshake while retaining the
    /// exact journal state already validated by [`scan_journal`](Self::scan_journal).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the owned tree or a sealed PGM is invalid, or if
    /// directory plus active-result metadata exceed the shared layout budget.
    pub fn complete_scan(&self, journal: JournalScan) -> io::Result<LocalScan> {
        self.complete_scan_cached(journal, &[])
    }

    /// Complete a journal scan while reusing unchanged sealed catalog summaries.
    ///
    /// `previous_sealed` must be the sorted sealed result of an earlier scan of
    /// this store. A summary is reused only when both its address and complete
    /// filesystem identity match the current strict layout result. All other
    /// PGM catalogs are reopened and validated.
    ///
    /// The merge is linear in the current and previous sealed counts and does
    /// not build an auxiliary map.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the tree or a PGM is invalid or changes while
    /// being scanned, or if retained scan metadata would exceed the shared
    /// layout budget. No partial scan is returned.
    pub fn complete_scan_cached(
        &self,
        journal: JournalScan,
        previous_sealed: &[SealedUnit],
    ) -> io::Result<LocalScan> {
        let layout = self.root.scan(self.limits).map_err(layout_io)?;
        let segment_count = layout.segments.len();
        let new_summaries = count_new_summaries(&layout.segments, previous_sealed);
        ensure_scan_metadata_budget(
            layout.metadata_bytes,
            journal.metadata_bytes,
            previous_sealed.len(),
            segment_count,
            new_summaries,
            self.limits.max_metadata_bytes,
        )?;
        let mut retained_during_build = accounted_scan_metadata_bytes(
            layout.metadata_bytes,
            journal.metadata_bytes,
            previous_sealed.len(),
            segment_count,
            0,
        )
        .ok_or_else(|| metadata_limit_io(self.limits.max_metadata_bytes))?;

        let mut sealed = Vec::with_capacity(segment_count);
        let mut previous_at = 0_usize;
        for artifact in layout.segments {
            advance_previous(previous_sealed, &mut previous_at, artifact.address);
            if let Some(previous) = previous_sealed.get(previous_at).filter(|previous| {
                (previous.address, previous.identity) == (artifact.address, artifact.pgm_identity)
            }) {
                sealed.push(previous.clone());
                continue;
            }

            let file = self.open_pinned_pgm(artifact.address, artifact.pgm_identity)?;
            let summary =
                read_catalog_summary(&file, retained_during_build, self.limits.max_metadata_bytes)
                    .map_err(store_io)?;
            require_file_identity(
                &file,
                artifact.pgm_identity,
                artifact.address,
                "after reading its catalog",
            )?;
            retained_during_build = retained_during_build
                .checked_add(summary_allocation_bytes())
                .ok_or_else(|| metadata_limit_io(self.limits.max_metadata_bytes))?;
            sealed.push(SealedUnit {
                address: artifact.address,
                identity: artifact.pgm_identity,
                summary: Arc::new(summary),
            });
        }

        Ok(LocalScan {
            sealed: Arc::new(sealed),
            active: journal.active,
            damages: journal.damages,
            warnings: Vec::new(),
            valid_len: journal.valid_len,
            committed_reset: journal.committed_reset,
        })
    }

    /// Open a sealed segment file for raw byte access.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be opened or no longer has the
    /// filesystem identity pinned by the scan.
    pub fn open_sealed(&self, u: &SealedUnit) -> io::Result<File> {
        let file = self.root.open_pgm(u.address).map_err(layout_io)?;
        self.validate_sealed_file(&file, u)?;
        Ok(file)
    }

    /// Validate an already-open sealed PGM against its pinned scan identity.
    ///
    /// Readers call this again after parsing the catalog so an in-place change
    /// during positional reads becomes a retryable stale-snapshot error without
    /// reopening the path.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::Interrupted`] when the descriptor no longer
    /// names the exact file captured by `u`.
    #[expect(
        clippy::unused_self,
        reason = "identity validation belongs to the LocalDir sealed-open contract"
    )]
    pub fn validate_sealed_file(&self, file: &File, u: &SealedUnit) -> io::Result<()> {
        require_file_identity(file, u.identity, u.address, "while it was open")
    }

    /// Opens the root-level active journal for snapshot identity and prefix
    /// checks.
    ///
    /// # Errors
    ///
    /// Returns an error when the existing journal is unsafe or unreadable.
    pub fn open_active(&self) -> io::Result<Option<File>> {
        self.root.open_active_journal().map_err(layout_io)
    }

    /// Read the bytes of one active part from the journal.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::ActivePartTooLarge`] if the cached part reference
    /// exceeds the active part cap, or [`StoreError::Io`] if the journal file
    /// cannot be opened or the part bytes cannot be read.
    pub fn read_active_part(&self, p: &ActivePart) -> Result<Vec<u8>, StoreError> {
        let part_len = u64::try_from(p.part.len).unwrap_or(u64::MAX);
        if part_len > MAX_PART_LEN {
            return Err(StoreError::ActivePartTooLarge {
                len: p.part.len,
                max: MAX_PART_LEN,
            });
        }
        let file = self
            .root
            .open_active_journal()?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "active.parts is absent"))?;
        validate_active_part_reference(&file, p)?;
        let mut buf = vec![0_u8; p.part.len];
        file.read_exact_at(&mut buf, p.part.offset as u64)?;
        Ok(buf)
    }

    /// Scan an already-open `active.parts` journal from `start_at` into active
    /// parts, damage regions, and the resumable `valid_len`.
    ///
    /// Newly validated parts are appended to `prev_active`; pass an empty vector
    /// for a full scan. Frame offsets from the scan are absolute, so a caller
    /// resuming at `start_at > 0` gets parts that reference their true journal
    /// position.
    ///
    /// A concurrent completed-segment publication followed by `Journal::reset`
    /// can shrink the journal during scanning. Such an
    /// `UnexpectedEof`/`NotFound` becomes a retryable `Interrupted` error; no
    /// partial active or sealed set is returned. Other I/O errors propagate.
    #[expect(
        clippy::rc_buffer,
        reason = "incremental append needs Arc::make_mut while unchanged scans retain the Vec allocation"
    )]
    fn scan_journal_reader_from<R: ReadAt>(
        &self,
        reader: &R,
        start_at: u64,
        prev_active: Arc<Vec<ActivePart>>,
        journal_path: &Path,
    ) -> io::Result<JournalScan> {
        self.scan_journal_reader_bounded_from(
            reader,
            start_at,
            prev_active,
            journal_path,
            MAX_JOURNAL_PARTS,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "journal admission, validation, and retained-budget accounting are one transaction"
    )]
    #[expect(
        clippy::rc_buffer,
        reason = "incremental append needs Arc::make_mut while unchanged scans retain the Vec allocation"
    )]
    fn scan_journal_reader_bounded_from<R: ReadAt>(
        &self,
        reader: &R,
        start_at: u64,
        prev_active: Arc<Vec<ActivePart>>,
        journal_path: &Path,
        max_parts: usize,
    ) -> io::Result<JournalScan> {
        let plan = match read_journal_plan(reader) {
            Ok(plan) => plan,
            Err(error) if is_stale_journal(&error) => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    format!(
                        "{} changed while reading its header: {error}",
                        journal_path.display()
                    ),
                ));
            }
            Err(error) => return Err(error),
        };
        let Some(segment_id) = plan.segment_id else {
            if ACTIVE_ARC_ALLOCATION_BYTES > self.limits.max_metadata_bytes {
                return Err(metadata_limit_io(self.limits.max_metadata_bytes));
            }
            return Ok(JournalScan {
                active: Arc::new(Vec::new()),
                damages: Vec::new(),
                valid_len: plan.valid_len,
                committed_reset: false,
                metadata_bytes: ACTIVE_ARC_ALLOCATION_BYTES,
            });
        };
        let body_reader = PrefixReader {
            inner: reader,
            len: plan.scan_len,
        };
        let previous_matches = prev_active
            .first()
            .is_none_or(|part| part.segment_id == segment_id);
        let can_resume = !plan.committed_reset
            && previous_matches
            && start_at >= JOURNAL_HEADER_LEN as u64
            && start_at <= plan.scan_len;
        let start_at = if can_resume {
            start_at
        } else {
            JOURNAL_HEADER_LEN as u64
        };
        let prev_active = if can_resume {
            prev_active
        } else {
            Arc::new(Vec::new())
        };
        let Some(remaining_parts) = max_parts.checked_sub(prev_active.len()) else {
            return Err(active_part_limit_io(journal_path, max_parts));
        };
        let report = scan_journal_frames(
            &body_reader,
            start_at,
            remaining_parts,
            max_parts,
            journal_path,
        )?;

        let mut active = prev_active;
        let mut active_metadata = active_metadata_bytes(&active, active.capacity())?;
        let report_metadata = scan_report_metadata_bytes(&report)?;
        if active_metadata
            .checked_add(report_metadata)
            .is_none_or(|peak| peak > self.limits.max_metadata_bytes)
        {
            return Err(metadata_limit_io(self.limits.max_metadata_bytes));
        }
        if !report.damages.is_empty()
            || u64::try_from(report.valid_len).unwrap_or(u64::MAX) != plan.scan_len
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} contains torn or damaged version-1 frames",
                    journal_path.display()
                ),
            ));
        }
        let appending_to_shared_baseline =
            !plan.committed_reset && !report.parts.is_empty() && Arc::strong_count(&active) > 1;
        let previous_active_metadata = if appending_to_shared_baseline {
            active_metadata
        } else {
            0
        };
        if !plan.committed_reset {
            reserve_active_slots(
                &mut active,
                report.parts.len(),
                active_metadata,
                report_metadata,
                previous_active_metadata,
                self.limits.max_metadata_bytes,
            )?;
            active_metadata = active_metadata_bytes(&active, active.capacity())?;
        }
        for part_ref in report.parts {
            if part_ref.len as u64 > MAX_PART_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "active part at offset {} exceeds the max part length",
                        part_ref.offset
                    ),
                ));
            }
            let part_metadata = match active_part_catalog_metadata_bytes(&body_reader, part_ref) {
                Ok(metadata) => metadata,
                Err(error) if is_stale_journal(&error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        format!("{} changed during scan: {error}", journal_path.display()),
                    ));
                }
                Err(error) => return Err(error),
            };
            ensure_active_part_budget(
                previous_active_metadata
                    .checked_add(active_metadata)
                    .and_then(|peak| peak.checked_add(report_metadata))
                    .ok_or_else(|| metadata_limit_io(self.limits.max_metadata_bytes))?,
                part_metadata,
                part_ref.len,
                self.limits.max_metadata_bytes,
            )?;
            let mut buf = vec![0_u8; part_ref.len];
            match body_reader.read_exact_at(&mut buf, part_ref.offset as u64) {
                Ok(()) => {}
                Err(err) if is_stale_journal(&err) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        format!("{} changed during scan: {err}", journal_path.display()),
                    ));
                }
                Err(err) => return Err(err),
            }
            let catalog = validate_part_catalog(&buf).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "active part at offset {} failed catalog decode: {error}",
                        part_ref.offset
                    ),
                )
            })?;
            let catalog_digest = CatalogDigest::from_catalog(&catalog);
            if !plan.committed_reset {
                Arc::make_mut(&mut active).push(ActivePart {
                    segment_id,
                    part: part_ref,
                    catalog,
                    catalog_digest,
                });
                active_metadata = active_metadata_bytes(&active, active.capacity())?;
                if previous_active_metadata
                    .checked_add(active_metadata)
                    .and_then(|peak| peak.checked_add(report_metadata))
                    .is_none_or(|peak| peak > self.limits.max_metadata_bytes)
                {
                    return Err(metadata_limit_io(self.limits.max_metadata_bytes));
                }
            }
        }

        Ok(JournalScan {
            active,
            damages: Vec::new(),
            valid_len: plan.valid_len,
            committed_reset: plan.committed_reset,
            metadata_bytes: previous_active_metadata
                .checked_add(active_metadata)
                .ok_or_else(|| metadata_limit_io(self.limits.max_metadata_bytes))?,
        })
    }

    fn open_pinned_pgm(
        &self,
        address: kronika_layout::SegmentAddress,
        expected: FileIdentity,
    ) -> io::Result<File> {
        let file = self.root.open_pgm(address).map_err(layout_io)?;
        require_file_identity(&file, expected, address, "after opening it")?;
        Ok(file)
    }
}

fn advance_previous(
    previous: &[SealedUnit],
    previous_at: &mut usize,
    current: kronika_layout::SegmentAddress,
) {
    while previous
        .get(*previous_at)
        .is_some_and(|sealed| sealed.address < current)
    {
        *previous_at += 1;
    }
}

fn count_new_summaries(current: &[SegmentArtifacts], previous: &[SealedUnit]) -> usize {
    let mut previous_at = 0_usize;
    current
        .iter()
        .filter(|artifact| {
            advance_previous(previous, &mut previous_at, artifact.address);
            !previous.get(previous_at).is_some_and(|sealed| {
                (sealed.address, sealed.identity) == (artifact.address, artifact.pgm_identity)
            })
        })
        .count()
}

fn ensure_scan_metadata_budget(
    layout_metadata: usize,
    journal_metadata: usize,
    previous_segments: usize,
    current_segments: usize,
    new_summaries: usize,
    limit: usize,
) -> io::Result<()> {
    let Some(accounted) = accounted_scan_metadata_bytes(
        layout_metadata,
        journal_metadata,
        previous_segments,
        current_segments,
        new_summaries,
    ) else {
        return Err(metadata_limit_io(limit));
    };
    if accounted > limit {
        return Err(metadata_limit_io(limit));
    }
    Ok(())
}

fn accounted_scan_metadata_bytes(
    layout_metadata: usize,
    journal_metadata: usize,
    previous_segments: usize,
    current_segments: usize,
    new_summaries: usize,
) -> Option<usize> {
    let previous_capacity = previous_segments.checked_mul(size_of::<SealedUnit>())?;
    let current_capacity = current_segments.checked_mul(size_of::<SealedUnit>())?;
    let collection_count = usize::from(previous_segments != 0).checked_add(1)?;
    let sealed_collections = collection_count
        .checked_mul(size_of::<Vec<SealedUnit>>().checked_add(ARC_ALLOCATION_OVERHEAD)?)?;
    let unique_summaries = previous_segments.checked_add(new_summaries)?;
    let summary_allocations = unique_summaries.checked_mul(summary_allocation_bytes())?;
    layout_metadata
        .checked_add(journal_metadata)?
        .checked_add(previous_capacity)?
        .checked_add(current_capacity)?
        .checked_add(sealed_collections)?
        .checked_add(summary_allocations)
}

const fn summary_allocation_bytes() -> usize {
    size_of::<CatalogSummary>() + ARC_ALLOCATION_OVERHEAD
}

fn require_file_identity(
    file: &File,
    expected: FileIdentity,
    address: kronika_layout::SegmentAddress,
    phase: &str,
) -> io::Result<()> {
    let actual = FileIdentity::from_file(file)?;
    if actual == expected {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::Interrupted,
        format!(
            "sealed PGM {} changed {phase}; retry the scan",
            address.pgm_name()
        ),
    ))
}

fn empty_journal_scan(metadata_limit: usize) -> io::Result<JournalScan> {
    if ACTIVE_ARC_ALLOCATION_BYTES > metadata_limit {
        return Err(metadata_limit_io(metadata_limit));
    }
    Ok(JournalScan {
        active: Arc::new(Vec::new()),
        damages: Vec::new(),
        valid_len: 0,
        committed_reset: false,
        metadata_bytes: ACTIVE_ARC_ALLOCATION_BYTES,
    })
}

fn active_part_catalog_metadata_bytes<R: ReadAt>(reader: &R, part: PartRef) -> io::Result<usize> {
    let minimum = MAGIC
        .len()
        .checked_add(META_LEN)
        .and_then(|bytes| bytes.checked_add(TAIL_INDEX_LEN))
        .ok_or_else(metadata_size_overflow)?;
    if part.len < minimum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("active part at offset {} is too short", part.offset),
        ));
    }
    let tail_offset = part
        .offset
        .checked_add(part.len - TAIL_INDEX_LEN)
        .ok_or_else(metadata_size_overflow)?;
    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    reader.read_exact_at(
        &mut tail_bytes,
        u64::try_from(tail_offset)
            .map_err(|_overflow| io::Error::from(io::ErrorKind::UnexpectedEof))?,
    )?;
    let tail = TailIndex::decode(tail_bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "active part at offset {} has an invalid tail index: {error}",
                part.offset
            ),
        )
    })?;
    let catalog_len =
        usize::try_from(tail.catalog_len).map_err(|_overflow| metadata_size_overflow())?;
    let Some(entries_bytes) = catalog_len.checked_sub(META_LEN) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "active part at offset {} has an invalid catalog length",
                part.offset
            ),
        ));
    };
    if !entries_bytes.is_multiple_of(ENTRY_LEN)
        || catalog_len
            .checked_add(TAIL_INDEX_LEN + MAGIC.len())
            .is_none_or(|minimum_part_len| minimum_part_len > part.len)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "active part at offset {} has an invalid catalog length",
                part.offset
            ),
        ));
    }
    let entry_count = entries_bytes / ENTRY_LEN;
    entry_count
        .checked_mul(size_of::<kronika_format::Entry>())
        .ok_or_else(metadata_size_overflow)
}

fn scan_report_metadata_bytes(report: &ScanReport) -> io::Result<usize> {
    report
        .parts
        .capacity()
        .checked_mul(size_of::<PartRef>())
        .and_then(|parts| {
            report
                .damages
                .capacity()
                .checked_mul(size_of::<kronika_format::DamageRegion>())
                .and_then(|damages| parts.checked_add(damages))
        })
        .ok_or_else(metadata_size_overflow)
}

fn ensure_active_part_budget(
    retained_metadata: usize,
    part_metadata: usize,
    transient_part_bytes: usize,
    limit: usize,
) -> io::Result<()> {
    let admitted = retained_metadata
        .checked_add(part_metadata)
        .and_then(|retained| retained.checked_add(transient_part_bytes))
        .is_some_and(|peak| peak <= limit);
    if admitted {
        Ok(())
    } else {
        Err(metadata_limit_io(limit))
    }
}

fn active_metadata_bytes(active: &[ActivePart], active_capacity: usize) -> io::Result<usize> {
    let struct_bytes = active_capacity
        .checked_mul(size_of::<ActivePart>())
        .and_then(|bytes| bytes.checked_add(ACTIVE_ARC_ALLOCATION_BYTES))
        .ok_or_else(metadata_size_overflow)?;
    active.iter().try_fold(struct_bytes, |total, part| {
        let entries = part
            .catalog
            .entries
            .capacity()
            .checked_mul(size_of::<kronika_format::Entry>())
            .ok_or_else(metadata_size_overflow)?;
        total
            .checked_add(entries)
            .ok_or_else(metadata_size_overflow)
    })
}

#[expect(
    clippy::rc_buffer,
    reason = "capacity admission and copy-on-write apply to the retained Vec allocation"
)]
fn reserve_active_slots(
    active: &mut Arc<Vec<ActivePart>>,
    additional: usize,
    retained_metadata: usize,
    report_metadata: usize,
    previous_active_metadata: usize,
    limit: usize,
) -> io::Result<()> {
    let final_len = active
        .len()
        .checked_add(additional)
        .ok_or_else(metadata_size_overflow)?;
    if additional != 0 {
        let clone_peak = previous_active_metadata
            .checked_add(retained_metadata)
            .and_then(|peak| peak.checked_add(report_metadata))
            .is_some_and(|peak| peak <= limit);
        if !clone_peak {
            return Err(metadata_limit_io(limit));
        }
        Arc::make_mut(active);
    }
    if final_len > active.capacity() {
        // `Vec` reallocation can retain the old allocation until the replacement
        // has succeeded, so admit both allocations before asking the allocator.
        let replacement_allocation = final_len
            .checked_mul(size_of::<ActivePart>())
            .ok_or_else(metadata_size_overflow)?;
        let admitted = previous_active_metadata
            .checked_add(retained_metadata)
            .and_then(|peak| peak.checked_add(report_metadata))
            .and_then(|peak| peak.checked_add(replacement_allocation))
            .is_some_and(|peak| peak <= limit);
        if !admitted {
            return Err(metadata_limit_io(limit));
        }
        Arc::make_mut(active)
            .try_reserve_exact(additional)
            .map_err(|_error| metadata_limit_io(limit))?;
    }
    let retained_after = active_metadata_bytes(active, active.capacity())?;
    if previous_active_metadata
        .checked_add(retained_after)
        .and_then(|peak| peak.checked_add(report_metadata))
        .is_none_or(|peak| peak > limit)
    {
        return Err(metadata_limit_io(limit));
    }
    Ok(())
}

fn metadata_size_overflow() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "active journal metadata size overflow",
    )
}

fn metadata_limit_io(limit: usize) -> io::Error {
    layout_io(LayoutError::TraversalLimitExceeded {
        kind: LimitKind::MetadataBytes,
        limit,
    })
}

const fn metadata_limit_store(limit: usize) -> StoreError {
    StoreError::Layout(LayoutError::TraversalLimitExceeded {
        kind: LimitKind::MetadataBytes,
        limit,
    })
}

#[derive(Debug, Clone, Copy)]
struct JournalReadPlan {
    segment_id: Option<SegmentId>,
    /// Prefix containing the old frames that must validate.
    scan_len: u64,
    /// Complete physical state proven valid by this plan.
    valid_len: u64,
    committed_reset: bool,
}

struct PrefixReader<'a, R> {
    inner: &'a R,
    len: u64,
}

impl<R: ReadAt> ReadAt for PrefixReader<'_, R> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let buf_len = u64::try_from(buf.len())
            .map_err(|_overflow| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let end = offset
            .checked_add(buf_len)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        if end > self.len {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        self.inner.read_exact_at(buf, offset)
    }

    fn byte_len(&self) -> io::Result<u64> {
        Ok(self.len)
    }
}

#[derive(Debug)]
struct ActiveJournalScanError(io::Error);

impl std::fmt::Display for ActiveJournalScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "active journal scan: {}", self.0)
    }
}

impl std::error::Error for ActiveJournalScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn active_journal_io(error: io::Error) -> io::Error {
    let kind = error.kind();
    io::Error::new(kind, ActiveJournalScanError(error))
}

/// Whether an error returned by [`LocalDir::scan`] originated while reading
/// `active.parts`, rather than while listing or validating sealed segments.
#[must_use]
pub fn is_active_journal_scan_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync + 'static>::is::<ActiveJournalScanError>)
}

/// Whether an I/O error means the live journal shrank or vanished under us
/// (a concurrent seal + `Journal::reset`), rather than a real I/O failure.
fn is_stale_journal(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound
    )
}

fn active_part_limit_io(journal_path: &Path, limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} contains more than the allowed {limit} active parts",
            journal_path.display()
        ),
    )
}

fn scan_journal_frames<R: ReadAt>(
    reader: &R,
    start_at: u64,
    remaining_parts: usize,
    total_part_limit: usize,
    journal_path: &Path,
) -> io::Result<ScanReport> {
    match scan_journal_streaming_strict_from(
        reader,
        start_at,
        JournalLimits::default(),
        remaining_parts,
    ) {
        Ok(report) => Ok(report),
        Err(JournalScanError::Io(error)) if is_stale_journal(&error) => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("{} changed during scan: {error}", journal_path.display()),
        )),
        Err(JournalScanError::Io(error)) => Err(error),
        Err(JournalScanError::PartLimitExceeded { .. }) => {
            Err(active_part_limit_io(journal_path, total_part_limit))
        }
    }
}

fn validate_active_part_reference<R: ReadAt>(reader: &R, active: &ActivePart) -> io::Result<()> {
    let file_len = reader.byte_len()?;
    validate_journal_len(file_len)?;
    if file_len < JOURNAL_HEADER_LEN as u64 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let mut header_bytes = [0_u8; JOURNAL_HEADER_LEN];
    reader.read_exact_at(&mut header_bytes, 0)?;
    if committed_reset_plan(reader, file_len, header_bytes)?.is_some() {
        return Err(stale_active_generation());
    }
    let header = JournalHeader::decode(header_bytes).map_err(journal_header_io)?;
    let JournalState::Active { segment_id } = header.state else {
        return Err(stale_active_generation());
    };
    if segment_id != active.segment_id.get() {
        return Err(stale_active_generation());
    }

    let committed_end = (JOURNAL_HEADER_LEN as u64)
        .checked_add(header.body_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "journal length overflow"))?;
    let part_offset = u64::try_from(active.part.offset)
        .map_err(|_overflow| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    let part_len = u64::try_from(active.part.len)
        .map_err(|_overflow| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    let part_end = part_offset
        .checked_add(part_len)
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    if part_end > committed_end {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    Ok(())
}

fn stale_active_generation() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "active part belongs to another journal generation",
    )
}

fn read_journal_plan<R: ReadAt>(reader: &R) -> io::Result<JournalReadPlan> {
    let file_len = reader.byte_len()?;
    validate_journal_len(file_len)?;
    if file_len == 0 {
        // A crash before the writer's first header write; the file provably
        // holds no frames, so it reads as an empty journal.
        return Ok(JournalReadPlan {
            segment_id: None,
            scan_len: 0,
            valid_len: 0,
            committed_reset: false,
        });
    }
    if file_len < JOURNAL_HEADER_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("active.parts has a torn {file_len}-byte header"),
        ));
    }
    let mut bytes = [0_u8; JOURNAL_HEADER_LEN];
    reader.read_exact_at(&mut bytes, 0)?;
    if let Some(plan) = committed_reset_plan(reader, file_len, bytes)? {
        return Ok(plan);
    }
    let header = JournalHeader::decode(bytes).map_err(journal_header_io)?;
    let actual = file_len - JOURNAL_HEADER_LEN as u64;
    if header.body_len != actual {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "active.parts header records {} body bytes, file contains {actual}",
                header.body_len
            ),
        ));
    }
    match header.state {
        JournalState::Empty if actual == 0 => Ok(JournalReadPlan {
            segment_id: None,
            scan_len: file_len,
            valid_len: file_len,
            committed_reset: false,
        }),
        JournalState::Empty => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty active.parts has trailing frames",
        )),
        JournalState::Active { segment_id } => {
            if actual == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "active active.parts has no complete first frame",
                ));
            }
            Ok(JournalReadPlan {
                segment_id: Some(SegmentId::new(segment_id).map_err(layout_io)?),
                scan_len: file_len,
                valid_len: file_len,
                committed_reset: false,
            })
        }
    }
}

fn validate_journal_len(file_len: u64) -> io::Result<()> {
    if file_len > MAX_JOURNAL_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "active.parts length {file_len} exceeds the version-1 limit of {MAX_JOURNAL_LEN} bytes"
            ),
        ));
    }
    Ok(())
}

fn committed_reset_plan<R: ReadAt>(
    reader: &R,
    file_len: u64,
    header_bytes: [u8; JOURNAL_HEADER_LEN],
) -> io::Result<Option<JournalReadPlan>> {
    let marker_len = RESET_MARKER_LEN as u64;
    let Some(marker_at) = file_len.checked_sub(marker_len) else {
        return Ok(None);
    };
    if marker_at <= JOURNAL_HEADER_LEN as u64 {
        return Ok(None);
    }
    let mut marker_bytes = [0_u8; RESET_MARKER_LEN];
    reader.read_exact_at(&mut marker_bytes, marker_at)?;
    let Some(marker) = ResetMarker::decode(marker_bytes) else {
        return Ok(None);
    };
    if marker.previous_len != marker_at
        || marker.expected_previous_header().is_none()
        || marker.classify_header_transition(header_bytes).is_none()
    {
        return Ok(None);
    }
    let Ok(segment_id) = SegmentId::new(marker.previous_segment_id) else {
        return Ok(None);
    };
    Ok(Some(JournalReadPlan {
        segment_id: Some(segment_id),
        scan_len: marker.previous_len,
        valid_len: file_len,
        committed_reset: true,
    }))
}

fn journal_header_io(error: JournalHeaderError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn layout_io(error: LayoutError) -> io::Error {
    match error {
        LayoutError::Io(source) => source,
        structural => io::Error::new(io::ErrorKind::InvalidData, structural),
    }
}

fn store_io(error: StoreError) -> io::Error {
    let kind = match error {
        StoreError::Io(ref source) => source.kind(),
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, error)
}

struct EncodedCatalog {
    bytes: Vec<u8>,
    body_end: u64,
}

fn read_encoded_catalog<R: ReadAt>(
    reader: &R,
    metadata_budget: Option<(usize, usize)>,
) -> Result<EncodedCatalog, StoreError> {
    let len = reader.byte_len()?;

    let tail_at = len
        .checked_sub(TAIL_INDEX_LEN as u64)
        .ok_or(StoreError::TooSmall)?;

    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    reader.read_exact_at(&mut tail_bytes, tail_at)?;
    let tail = TailIndex::decode(tail_bytes).map_err(|_decode_err| StoreError::TooSmall)?;

    let catalog_len = u64::from(tail.catalog_len);
    if catalog_len > MAX_CATALOG_BYTES {
        return Err(StoreError::BadCatalogLen);
    }
    let catalog_at = tail_at
        .checked_sub(catalog_len)
        .ok_or(StoreError::BadCatalogLen)?;
    if catalog_at < MAGIC.len() as u64 {
        return Err(StoreError::BadCatalogLen);
    }
    if let Some((retained, limit)) = metadata_budget {
        let catalog_bytes =
            usize::try_from(catalog_len).map_err(|_overflow| StoreError::BadCatalogLen)?;
        if retained
            .checked_add(catalog_bytes)
            .is_none_or(|peak| peak > limit)
        {
            return Err(metadata_limit_store(limit));
        }
    }

    let mut buf = vec![0_u8; tail.catalog_len as usize];
    reader.read_exact_at(&mut buf, catalog_at)?;
    Ok(EncodedCatalog {
        bytes: buf,
        body_end: catalog_at,
    })
}

fn read_catalog_summary<R: ReadAt>(
    reader: &R,
    retained_metadata: usize,
    metadata_limit: usize,
) -> Result<CatalogSummary, StoreError> {
    #[cfg(test)]
    CATALOG_SUMMARY_READS.with(|reads| reads.set(reads.get().saturating_add(1)));

    let encoded = read_encoded_catalog(reader, Some((retained_metadata, metadata_limit)))?;
    let summary = CatalogSummary::from_encoded(&encoded.bytes, encoded.body_end)
        .map_err(summary_store_error)?;
    verify_pgm_magic(reader)?;
    Ok(summary)
}

const fn summary_store_error(error: CatalogSummaryError) -> StoreError {
    match error {
        CatalogSummaryError::Decode(error) => StoreError::Catalog(error),
        CatalogSummaryError::UnsupportedFormat { version } => {
            StoreError::UnsupportedFormat { version }
        }
        CatalogSummaryError::LengthOverflow => StoreError::BadCatalogLen,
        CatalogSummaryError::EntryOutOfBounds { .. } => StoreError::OutOfBounds,
    }
}

fn verify_pgm_magic<R: ReadAt>(reader: &R) -> Result<(), StoreError> {
    let mut magic = [0_u8; MAGIC.len()];
    reader.read_exact_at(&mut magic, 0)?;
    if magic == MAGIC {
        Ok(())
    } else {
        Err(StoreError::BadMagic)
    }
}

/// Read and decode the end catalog from any [`ReadAt`] source.
///
/// Reads only the tail index and catalog block; no section bodies are loaded.
/// Directory discovery uses a compact summary instead; this function remains
/// available for callers that explicitly need an owned catalog.
///
/// # Errors
///
/// Returns [`StoreError`] when the source is too small, the magic bytes are
/// wrong, the format version is unsupported, the catalog block cannot be
/// located, the catalog bytes are corrupt, or a catalog entry points outside
/// the section area.
pub fn read_catalog<R: ReadAt>(reader: &R) -> Result<Catalog, StoreError> {
    let encoded = read_encoded_catalog(reader, None)?;
    let catalog = Catalog::decode(&encoded.bytes).map_err(StoreError::Catalog)?;
    verify_pgm_magic(reader)?;
    if catalog.format_version != FORMAT_VERSION {
        return Err(StoreError::UnsupportedFormat {
            version: catalog.format_version,
        });
    }

    for entry in &catalog.entries {
        let in_bounds = entry.offset >= MAGIC.len() as u64
            && entry
                .offset
                .checked_add(entry.len)
                .is_some_and(|end| end <= encoded.body_end);
        if !in_bounds {
            return Err(StoreError::OutOfBounds);
        }
    }

    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part, crc32c};
    use kronika_layout::SegmentAddress;

    fn part(ts: i64, src: u64) -> Vec<u8> {
        part_with_body(ts, src, b"")
    }

    fn part_with_body(ts: i64, src: u64, body: &[u8]) -> Vec<u8> {
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body,
            }],
            PartMeta {
                min_ts: ts,
                max_ts: ts + 1,
                source_id: src,
            },
        )
    }

    fn segment_path(root: &Path, raw_id: i64) -> std::path::PathBuf {
        let address =
            SegmentAddress::new(SegmentId::new(raw_id).expect("representable segment id"))
                .expect("segment address");
        let day = root
            .join(address.day.year_component())
            .join(address.day.month_component())
            .join(address.day.day_component());
        fs::create_dir_all(&day).expect("create test UTC day");
        day.join(address.pgm_name())
    }

    fn write_segment(root: &Path, raw_id: i64, bytes: impl AsRef<[u8]>) {
        fs::write(segment_path(root, raw_id), bytes).expect("write test segment");
    }

    fn frame(part_bytes: &[u8]) -> Vec<u8> {
        let mut out = FrameHeader {
            part_len: part_bytes.len() as u64,
        }
        .encode()
        .to_vec();
        out.extend_from_slice(part_bytes);
        out
    }

    fn journal(raw_id: i64, parts: &[Vec<u8>]) -> Vec<u8> {
        let body = parts
            .iter()
            .flat_map(|part| frame(part))
            .collect::<Vec<_>>();
        let header = JournalHeader {
            state: JournalState::Active { segment_id: raw_id },
            body_len: body.len() as u64,
        };
        let mut bytes = header.encode().to_vec();
        bytes.extend_from_slice(&body);
        bytes
    }

    fn write_journal(root: &Path, raw_id: i64, parts: &[Vec<u8>]) {
        fs::write(root.join("active.parts"), journal(raw_id, parts))
            .expect("write version-1 journal");
    }

    fn append_journal_part(path: &Path, raw_id: i64, part_bytes: &[u8]) -> Vec<u8> {
        let mut bytes = fs::read(path).expect("read journal");
        bytes.extend_from_slice(&frame(part_bytes));
        let body_len = bytes.len() - JOURNAL_HEADER_LEN;
        bytes[..JOURNAL_HEADER_LEN].copy_from_slice(
            &JournalHeader {
                state: JournalState::Active { segment_id: raw_id },
                body_len: body_len as u64,
            }
            .encode(),
        );
        fs::write(path, &bytes).expect("append journal part");
        bytes
    }

    fn write_empty_journal(root: &Path) {
        fs::write(root.join("active.parts"), JournalHeader::EMPTY.encode())
            .expect("write empty version-1 journal");
    }

    fn reset_catalog_summary_reads() {
        CATALOG_SUMMARY_READS.with(|reads| reads.set(0));
    }

    fn catalog_summary_reads() -> usize {
        CATALOG_SUMMARY_READS.with(std::cell::Cell::get)
    }

    #[derive(Clone, Copy)]
    enum CommittedHeaderPhase {
        Previous,
        Empty,
        Torn,
    }

    fn committed_reset_journal(
        raw_id: i64,
        parts: &[Vec<u8>],
        phase: CommittedHeaderPhase,
    ) -> Vec<u8> {
        let mut bytes = journal(raw_id, parts);
        let previous_len = bytes.len() as u64;
        let previous_header: [u8; JOURNAL_HEADER_LEN] = bytes[..JOURNAL_HEADER_LEN]
            .try_into()
            .expect("complete previous header");
        bytes.extend_from_slice(
            &ResetMarker::new(previous_len, raw_id)
                .expect("non-empty journal reset marker")
                .encode(),
        );
        match phase {
            CommittedHeaderPhase::Previous => {}
            CommittedHeaderPhase::Empty => {
                bytes[..JOURNAL_HEADER_LEN].copy_from_slice(&JournalHeader::EMPTY.encode());
            }
            CommittedHeaderPhase::Torn => {
                let empty = JournalHeader::EMPTY.encode();
                let split = JOURNAL_HEADER_LEN / 2;
                bytes[..split].copy_from_slice(&empty[..split]);
                bytes[split..JOURNAL_HEADER_LEN]
                    .copy_from_slice(&previous_header[split..JOURNAL_HEADER_LEN]);
            }
        }
        bytes
    }

    /// Build a minimal valid part with no sections and a specific `format_version`.
    ///
    /// Layout: `MAGIC(4)` | catalog block (`META_LEN` bytes) | tail index (`TAIL_INDEX_LEN` bytes)
    fn minimal_part_with_version(format_version: u32) -> Vec<u8> {
        let catalog = Catalog {
            entries: vec![],
            min_ts: 0,
            max_ts: 0,
            source_id: 0,
            format_version,
        };
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&catalog.encode());
        out
    }

    /// Locate the tail index within a part buffer.
    ///
    /// Returns the byte offset of the 8-byte tail index at the end of the buffer.
    fn tail_offset(buf: &[u8]) -> usize {
        buf.len() - TAIL_INDEX_LEN
    }

    /// Locate the catalog block start within a part buffer.
    ///
    /// The `catalog_len` stored in the tail index tells us where the catalog starts.
    fn catalog_offset(buf: &[u8]) -> usize {
        let tail_at = tail_offset(buf);
        let catalog_len =
            u32::from_le_bytes(buf[tail_at..tail_at + 4].try_into().unwrap()) as usize;
        tail_at - catalog_len
    }

    /// Offset of `format_version` within the meta block (28 bytes into meta).
    fn format_version_offset(buf: &[u8]) -> usize {
        // meta block is the last META_LEN bytes of the catalog block (before the tail)
        let cat_start = catalog_offset(buf);
        let cat_end = tail_offset(buf);
        let cat_len = cat_end - cat_start;
        let entry_count = (cat_len - META_LEN) / ENTRY_LEN;
        cat_start + entry_count * ENTRY_LEN + 28 // 28 = offset of format_version within meta
    }

    /// Offset of crc32c within the meta block (32 bytes into meta).
    fn meta_crc_offset(buf: &[u8]) -> usize {
        let cat_start = catalog_offset(buf);
        let cat_end = tail_offset(buf);
        let cat_len = cat_end - cat_start;
        let entry_count = (cat_len - META_LEN) / ENTRY_LEN;
        cat_start + entry_count * ENTRY_LEN + 32 // 32 = META_CRC_OFFSET
    }

    /// Recompute catalog CRC and patch it into `buf` at the crc field position.
    fn repatch_catalog_crc(buf: &mut [u8]) {
        let crc_at = meta_crc_offset(buf);
        let tail_at = tail_offset(buf);
        // Zero the crc field before computing.
        buf[crc_at..crc_at + 4].copy_from_slice(&0_u32.to_le_bytes());
        let crc = crc32c(&buf[catalog_offset(buf)..tail_at]);
        buf[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
    }

    // --- read_catalog branch tests ---

    #[test]
    fn read_catalog_too_small_buffer_shorter_than_tail() {
        // A buffer shorter than TAIL_INDEX_LEN cannot hold a tail index.
        let buf: &[u8] = &[0_u8; TAIL_INDEX_LEN - 1];
        assert!(
            matches!(read_catalog(&buf), Err(StoreError::TooSmall)),
            "buffer shorter than tail index must return TooSmall"
        );
    }

    #[test]
    fn read_catalog_too_small_bad_tail_magic() {
        // Exactly TAIL_INDEX_LEN bytes with wrong magic: TailIndex::decode fails
        // → mapped to TooSmall.
        let buf = [0_u8; TAIL_INDEX_LEN];
        assert!(
            matches!(read_catalog(&buf.as_slice()), Err(StoreError::TooSmall)),
            "tail with wrong magic must return TooSmall"
        );
    }

    #[test]
    fn read_catalog_bad_catalog_len_exceeds_max() {
        // Tail index with catalog_len > MAX_CATALOG_BYTES (64 MiB).
        // Build tail manually: catalog_len as u32 LE + MAGIC.
        // MAX_CATALOG_BYTES = 64 MiB = 0x0400_0000; adding 1 gives 0x0400_0001, which fits u32.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "MAX_CATALOG_BYTES + 1 = 64 MiB + 1 < u32::MAX; truncation is impossible"
        )]
        let huge_len: u32 = (MAX_CATALOG_BYTES + 1) as u32;
        let mut buf = vec![0_u8; TAIL_INDEX_LEN + 100];
        let tail_at = buf.len() - TAIL_INDEX_LEN;
        buf[tail_at..tail_at + 4].copy_from_slice(&huge_len.to_le_bytes());
        buf[tail_at + 4..tail_at + 8].copy_from_slice(&MAGIC);
        assert!(
            matches!(
                read_catalog(&buf.as_slice()),
                Err(StoreError::BadCatalogLen)
            ),
            "catalog_len > MAX_CATALOG_BYTES must return BadCatalogLen"
        );
    }

    #[test]
    fn read_catalog_bad_catalog_len_catalog_overlaps_magic() {
        // catalog_len so large that catalog_at would land before the magic.
        // The file is: MAGIC(4) + tail_index(8) = 12 bytes.
        // Set catalog_len = 9 so catalog_at = 12 - 8 - 9 = -5 (underflow → BadCatalogLen).
        let catalog_len: u32 = 9;
        let mut buf = vec![0_u8; 12];
        buf[0..4].copy_from_slice(&MAGIC);
        let tail_at = 4;
        buf[tail_at..tail_at + 4].copy_from_slice(&catalog_len.to_le_bytes());
        buf[tail_at + 4..tail_at + 8].copy_from_slice(&MAGIC);
        assert!(
            matches!(
                read_catalog(&buf.as_slice()),
                Err(StoreError::BadCatalogLen)
            ),
            "catalog extending past magic must return BadCatalogLen"
        );
    }

    #[test]
    fn read_catalog_catalog_decode_error() {
        // Valid tail + catalog bytes that fail Catalog::decode (all-zeroes meta has bad CRC).
        // File: MAGIC(4) | catalog_block(META_LEN=40, all zeroes) | tail(8)
        let catalog_len = u32::try_from(META_LEN).expect("META_LEN fits u32");
        let total = MAGIC.len() + META_LEN + TAIL_INDEX_LEN;
        let mut buf = vec![0_u8; total];
        buf[0..4].copy_from_slice(&MAGIC);
        let tail_at = MAGIC.len() + META_LEN;
        buf[tail_at..tail_at + 4].copy_from_slice(&catalog_len.to_le_bytes());
        buf[tail_at + 4..tail_at + 8].copy_from_slice(&MAGIC);
        // catalog_at = MAGIC.len() = 4, catalog block is all zeroes — CRC mismatch.
        assert!(
            matches!(read_catalog(&buf.as_slice()), Err(StoreError::Catalog(_))),
            "corrupt catalog block must return Catalog(DecodeError)"
        );
    }

    #[test]
    fn read_catalog_bad_magic() {
        // Valid tail + valid catalog, but byte 0 is not MAGIC.
        let mut buf = minimal_part_with_version(FORMAT_VERSION);
        buf[0] ^= 0xFF; // corrupt first byte
        assert!(
            matches!(read_catalog(&buf.as_slice()), Err(StoreError::BadMagic)),
            "wrong magic at offset 0 must return BadMagic"
        );
    }

    #[test]
    fn read_catalog_unsupported_format_version() {
        // Valid part except format_version != FORMAT_VERSION.
        // Patch format_version to 99, then recompute catalog CRC.
        let mut buf = minimal_part_with_version(FORMAT_VERSION);
        let fv_at = format_version_offset(&buf);
        buf[fv_at..fv_at + 4].copy_from_slice(&99_u32.to_le_bytes());
        repatch_catalog_crc(&mut buf);
        assert!(
            matches!(
                read_catalog(&buf.as_slice()),
                Err(StoreError::UnsupportedFormat { version: 99 })
            ),
            "unknown format_version must return UnsupportedFormat"
        );
    }

    #[test]
    fn read_catalog_out_of_bounds_entry() {
        // Build a part with one section, then patch that entry's offset to point
        // into the catalog block (past catalog_at), triggering OutOfBounds.
        let section_body = b"data";
        let mut buf = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 1,
                body: section_body,
            }],
            PartMeta {
                min_ts: 1,
                max_ts: 2,
                source_id: 0,
            },
        );
        // Entry layout in catalog block: type_id(4) flags(4) offset(8) len(8) rows(4) crc32c(4)
        // offset field starts at byte 8 within the first entry.
        let cat_start = catalog_offset(&buf);
        let entry_offset_field = cat_start + 8;
        // Set offset to a value past catalog_start (i.e., into the catalog block itself).
        let bad_offset = cat_start as u64 + 1;
        buf[entry_offset_field..entry_offset_field + 8].copy_from_slice(&bad_offset.to_le_bytes());
        // Recompute catalog CRC so Catalog::decode succeeds.
        repatch_catalog_crc(&mut buf);
        assert!(
            matches!(read_catalog(&buf.as_slice()), Err(StoreError::OutOfBounds)),
            "entry pointing into catalog block must return OutOfBounds"
        );
    }

    #[test]
    fn read_catalog_happy_path() {
        // Confirm a correctly built part round-trips through read_catalog.
        let buf = part(1000, 42);
        let catalog = read_catalog(&buf.as_slice()).expect("valid part must decode");
        assert_eq!(catalog.min_ts, 1000);
        assert_eq!(catalog.source_id, 42);
    }

    #[test]
    fn catalog_summary_transient_bytes_are_admitted_before_allocation() {
        let buf = part(1000, 42);
        let error = read_catalog_summary(&buf.as_slice(), 1, 1).unwrap_err();
        assert!(matches!(
            error,
            StoreError::Layout(LayoutError::TraversalLimitExceeded {
                kind: LimitKind::MetadataBytes,
                limit: 1,
            })
        ));
    }

    #[test]
    fn active_part_budget_includes_retained_report_and_transient_body() {
        assert!(ensure_active_part_budget(100, 20, 30, 150).is_ok());
        let error = ensure_active_part_budget(100, 20, 30, 149).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn active_vector_capacity_and_reallocation_peak_are_budgeted() {
        let mut active = Arc::new(Vec::<ActivePart>::with_capacity(1));
        let retained = active_metadata_bytes(&active, active.capacity()).unwrap();
        assert_eq!(
            retained,
            size_of::<ActivePart>() + ACTIVE_ARC_ALLOCATION_BYTES
        );

        let replacement = 2 * size_of::<ActivePart>();
        let limit = retained + replacement;
        let error = reserve_active_slots(&mut active, 2, retained, 0, 0, limit - 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(active.capacity(), 1, "rejection happens before allocation");

        reserve_active_slots(&mut active, 2, retained, 0, 0, limit).unwrap();
        assert!(active.capacity() >= 2);
        assert_eq!(
            active_metadata_bytes(&active, active.capacity()).unwrap(),
            active.capacity() * size_of::<ActivePart>() + ACTIVE_ARC_ALLOCATION_BYTES
        );
    }

    #[test]
    fn shared_active_clone_is_admitted_before_copy_on_write() {
        let catalog = read_catalog(&part(1000, 42).as_slice()).expect("catalog");
        let catalog_digest = CatalogDigest::from_catalog(&catalog);
        let mut active = Arc::new(vec![ActivePart {
            segment_id: SegmentId::new(1_000).unwrap(),
            part: PartRef {
                offset: JOURNAL_HEADER_LEN + FRAME_HEADER_LEN,
                len: 1,
            },
            catalog,
            catalog_digest,
        }]);
        let retained = active_metadata_bytes(&active, active.capacity()).unwrap();
        let previous = Arc::clone(&active);
        let clone_peak = retained.checked_mul(2).expect("test metadata fits");

        let error = reserve_active_slots(&mut active, 1, retained, 0, retained, clone_peak - 1)
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            Arc::ptr_eq(&active, &previous),
            "the shared baseline must remain untouched when its clone is not admitted"
        );
    }

    #[test]
    fn million_part_clone_lower_bound_exceeds_the_default_metadata_budget() {
        let one_baseline = MAX_JOURNAL_PARTS
            .checked_mul(size_of::<ActivePart>())
            .and_then(|bytes| bytes.checked_add(ACTIVE_ARC_ALLOCATION_BYTES))
            .expect("v1 active baseline size fits usize");
        let clone_peak = one_baseline
            .checked_mul(2)
            .expect("v1 active clone peak fits usize");
        let limit = LayoutLimits::default().max_metadata_bytes;

        assert!(
            one_baseline <= limit,
            "one minimal maximum-count baseline remains representable"
        );
        assert!(
            clone_peak > limit,
            "a live clone makes copy-on-write inadmissible before catalog-entry allocations"
        );
    }

    #[test]
    fn read_active_part_rejects_oversized_ref_before_allocation() {
        let dir = tempfile::tempdir().unwrap();
        write_empty_journal(dir.path());
        let catalog = read_catalog(&part(1000, 42).as_slice()).expect("catalog");
        let catalog_digest = CatalogDigest::from_catalog(&catalog);
        let oversized_len = usize::try_from(MAX_PART_LEN).expect("part cap fits usize") + 1;
        let active = ActivePart {
            segment_id: SegmentId::new(1_000).unwrap(),
            part: PartRef {
                offset: JOURNAL_HEADER_LEN + FRAME_HEADER_LEN,
                len: oversized_len,
            },
            catalog,
            catalog_digest,
        };

        let err = LocalDir::open(dir.path())
            .unwrap()
            .read_active_part(&active)
            .unwrap_err();

        assert!(
            matches!(
                err,
                StoreError::ActivePartTooLarge { len, max }
                    if len == oversized_len && max == MAX_PART_LEN
            ),
            "oversized active part must be rejected before allocation"
        );
    }

    #[test]
    fn read_active_part_rejects_the_same_offset_and_catalog_in_a_new_segment() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = part(1000, 42);
        write_journal(dir.path(), 1_000, std::slice::from_ref(&bytes));
        let local = LocalDir::open(dir.path()).unwrap();
        let scan = local.scan().unwrap();
        let active = scan.active[0].clone();

        write_journal(dir.path(), 2_000, std::slice::from_ref(&bytes));
        let error = local.read_active_part(&active).unwrap_err();
        assert!(
            matches!(error, StoreError::Io(source) if source.kind() == io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn read_active_part_ignores_an_uncommitted_append_tail() {
        let dir = tempfile::tempdir().unwrap();
        let first = part(1000, 42);
        let later = part(2000, 42);
        write_journal(dir.path(), 1_000, std::slice::from_ref(&first));
        let journal_path = dir.path().join("active.parts");
        let local = LocalDir::open(dir.path()).unwrap();
        let scan = local.scan().unwrap();
        let active = scan.active[0].clone();

        let mut with_uncommitted_tail = fs::read(&journal_path).unwrap();
        with_uncommitted_tail.extend_from_slice(&frame(&later));
        fs::write(journal_path, with_uncommitted_tail).unwrap();

        assert_eq!(local.read_active_part(&active).unwrap(), first);
    }

    // --- scan() behavioral tests ---

    #[test]
    fn scan_lists_sealed_and_active_with_cheap_catalog() {
        let dir = tempfile::tempdir().unwrap();
        write_segment(dir.path(), 1_000, part(1000, 7));
        write_journal(dir.path(), 2_000, &[part(2000, 7), part(3000, 7)]);
        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1, "one sealed segment");
        assert_eq!(scan.sealed[0].summary.min_ts, 1000, "sealed summary min_ts");
        assert_eq!(scan.active.len(), 2, "two active parts");
        assert_eq!(
            scan.active[1].catalog.min_ts, 3000,
            "second active part min_ts"
        );
        assert_eq!(
            scan.active[1].catalog_digest,
            CatalogDigest::from_catalog(&scan.active[1].catalog),
            "the validated catalog digest is retained with the active part"
        );
        assert!(scan.warnings.is_empty(), "no warnings for clean data");
    }

    #[test]
    fn cached_scan_reuses_summary_without_reading_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        write_segment(dir.path(), 1_000, part(1000, 7));
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();

        reset_catalog_summary_reads();
        let second = local
            .complete_scan_cached(local.scan_journal().unwrap(), first.sealed.as_slice())
            .unwrap();

        assert_eq!(
            catalog_summary_reads(),
            0,
            "an identity-equal PGM must not have its catalog reread"
        );
        assert!(Arc::ptr_eq(
            &first.sealed[0].summary,
            &second.sealed[0].summary
        ));
        let cloned = second.clone();
        assert!(
            Arc::ptr_eq(&second.sealed, &cloned.sealed),
            "snapshot clone must share the sealed collection"
        );
    }

    #[test]
    fn same_inode_metadata_change_invalidates_cached_and_opened_units() {
        let dir = tempfile::tempdir().unwrap();
        let path = segment_path(dir.path(), 1_000);
        fs::write(&path, part(1000, 7)).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();
        let pinned = first.sealed[0].clone();
        let opened = local.open_sealed(&pinned).unwrap();

        fs::write(&path, part_with_body(1000, 7, b"changed")).unwrap();
        let current_identity = FileIdentity::from_file(&File::open(&path).unwrap()).unwrap();
        assert_eq!(pinned.identity.device, current_identity.device);
        assert_eq!(pinned.identity.inode, current_identity.inode);
        assert_ne!(pinned.identity, current_identity);
        assert_eq!(
            local
                .validate_sealed_file(&opened, &pinned)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            local.open_sealed(&pinned).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );

        reset_catalog_summary_reads();
        let second = local
            .complete_scan_cached(local.scan_journal().unwrap(), first.sealed.as_slice())
            .unwrap();
        assert_eq!(catalog_summary_reads(), 1);
        assert!(!Arc::ptr_eq(
            &first.sealed[0].summary,
            &second.sealed[0].summary
        ));
        assert_eq!(second.sealed[0].identity, current_identity);
    }

    #[test]
    fn changed_identity_does_not_reuse_a_corrupt_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = segment_path(dir.path(), 1_000);
        let valid = part(1000, 7);
        fs::write(&path, &valid).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();

        let mut corrupt = valid;
        let corrupt_at = catalog_offset(&corrupt);
        corrupt[corrupt_at] ^= 0xff;
        fs::write(path, corrupt).unwrap();
        reset_catalog_summary_reads();
        let error = local
            .complete_scan_cached(local.scan_journal().unwrap(), first.sealed.as_slice())
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(catalog_summary_reads(), 1);
    }

    #[test]
    fn five_year_fixed_summary_scan_fits_the_metadata_budget() {
        const SEGMENTS: usize = 5 * 35_040;
        const PERMANENT_FILES: usize = 5 * 70_080;
        const CALENDAR_DIRECTORIES: usize = 5 * (1 + 12 + 365);
        const ENTRY_ACCOUNTING: usize = 128;
        const MAX_FILE_NAME_BYTES: usize = 24;
        const LIMIT: usize = 128 * 1024 * 1024;

        let layout_metadata = PERMANENT_FILES * (ENTRY_ACCOUNTING + MAX_FILE_NAME_BYTES)
            + CALENDAR_DIRECTORIES * (ENTRY_ACCOUNTING + 4)
            + SEGMENTS * size_of::<SegmentArtifacts>();
        let cold =
            accounted_scan_metadata_bytes(layout_metadata, 0, 0, SEGMENTS, SEGMENTS).unwrap();
        let unchanged_refresh =
            accounted_scan_metadata_bytes(layout_metadata, 0, SEGMENTS, SEGMENTS, 0).unwrap();
        let replaced_refresh =
            accounted_scan_metadata_bytes(layout_metadata, 0, SEGMENTS, SEGMENTS, SEGMENTS)
                .unwrap();

        assert!(cold <= LIMIT, "cold five-year scan accounts {cold} bytes");
        assert!(
            unchanged_refresh <= LIMIT,
            "unchanged five-year refresh accounts {unchanged_refresh} bytes"
        );
        assert!(
            replaced_refresh > LIMIT,
            "a full same-name replacement must fail before duplicating every summary"
        );
    }

    #[test]
    fn journal_above_the_v1_physical_limit_is_rejected_before_scanning() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let file = File::create(&journal_path).unwrap();
        file.set_len(MAX_JOURNAL_LEN as u64 + 1).unwrap();

        let error = LocalDir::open(dir.path()).unwrap().scan().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("version-1 limit"));
        assert_eq!(
            fs::metadata(journal_path).unwrap().len(),
            MAX_JOURNAL_LEN as u64 + 1
        );
    }

    #[test]
    fn active_part_count_limit_is_stable_invalid_data() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let bytes = journal(2_000, &[part(2000, 7), part(3000, 7)]);
        let local = LocalDir::open(dir.path()).unwrap();

        let error = local
            .scan_journal_reader_bounded_from(
                &bytes,
                JOURNAL_HEADER_LEN as u64,
                Arc::new(Vec::new()),
                &journal_path,
                1,
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("allowed 1 active parts"));
    }

    #[test]
    fn incremental_scan_counts_cached_parts_toward_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let first_bytes = journal(2_000, &[part(2000, 7)]);
        let complete_bytes = journal(2_000, &[part(2000, 7), part(3000, 7)]);
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local
            .scan_journal_reader_bounded_from(
                &first_bytes,
                JOURNAL_HEADER_LEN as u64,
                Arc::new(Vec::new()),
                &journal_path,
                1,
            )
            .unwrap();
        let first_valid_len = first.valid_len;

        let error = local
            .scan_journal_reader_bounded_from(
                &complete_bytes,
                first_valid_len,
                first.active,
                &journal_path,
                1,
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("allowed 1 active parts"));
    }

    #[test]
    fn corrupt_sealed_aborts_the_complete_scan() {
        let dir = tempfile::tempdir().unwrap();
        write_segment(dir.path(), 1_000, part(1000, 7));
        write_segment(dir.path(), 2_000, b"not a pgm");
        assert!(
            LocalDir::open(dir.path()).unwrap().scan().is_err(),
            "a partial sealed set must never be returned"
        );
    }

    #[test]
    fn zero_length_active_journal_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        write_segment(dir.path(), 1_000, part(1000, 7));
        fs::write(dir.path().join("active.parts"), []).unwrap();
        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1);
        assert!(scan.active.is_empty());
        assert!(scan.damages.is_empty());
    }

    #[test]
    fn torn_active_journal_aborts_the_complete_scan() {
        let dir = tempfile::tempdir().unwrap();
        write_segment(dir.path(), 1_000, part(1000, 7));
        let unfinished_part = part(2000, 7);
        let body = FrameHeader {
            part_len: u64::try_from(unfinished_part.len()).expect("part length fits u64"),
        }
        .encode();
        let mut bytes = JournalHeader {
            state: JournalState::Active { segment_id: 2_000 },
            body_len: body.len() as u64,
        }
        .encode()
        .to_vec();
        bytes.extend_from_slice(&body);
        fs::write(dir.path().join("active.parts"), bytes).unwrap();
        assert!(
            LocalDir::open(dir.path()).unwrap().scan().is_err(),
            "stable journal damage is fatal"
        );
    }

    #[test]
    fn committed_reset_phases_are_logically_empty_after_validating_the_old_body() {
        for phase in [
            CommittedHeaderPhase::Previous,
            CommittedHeaderPhase::Empty,
            CommittedHeaderPhase::Torn,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let bytes = committed_reset_journal(2_000, &[part(2000, 7)], phase);
            fs::write(dir.path().join("active.parts"), &bytes).unwrap();

            let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
            assert!(scan.active.is_empty());
            assert_eq!(
                scan.valid_len,
                bytes.len() as u64,
                "the complete committed state is a valid logical reset boundary"
            );
            assert!(scan.damages.is_empty());
        }
    }

    #[test]
    fn committed_reset_marker_does_not_hide_corrupt_old_frames() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes =
            committed_reset_journal(2_000, &[part(2000, 7)], CommittedHeaderPhase::Previous);
        bytes[JOURNAL_HEADER_LEN + FRAME_HEADER_LEN] ^= 0xff;
        fs::write(dir.path().join("active.parts"), bytes).unwrap();

        let error = LocalDir::open(dir.path()).unwrap().scan().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(is_active_journal_scan_error(&error));
    }

    #[test]
    fn scan_error_origin_distinguishes_active_journal_from_sealed_pgm() {
        let active_dir = tempfile::tempdir().unwrap();
        fs::write(
            active_dir.path().join("active.parts"),
            b"stable malformed journal",
        )
        .unwrap();
        let active_error = LocalDir::open(active_dir.path())
            .unwrap()
            .scan()
            .unwrap_err();
        assert!(is_active_journal_scan_error(&active_error));

        let sealed_dir = tempfile::tempdir().unwrap();
        write_segment(sealed_dir.path(), 2_000, b"stable malformed PGM");
        let sealed_error = LocalDir::open(sealed_dir.path())
            .unwrap()
            .scan()
            .unwrap_err();
        assert!(!is_active_journal_scan_error(&sealed_error));
    }

    #[test]
    fn scan_from_unchanged_size_keeps_prev_and_reports_same_valid_len() {
        let dir = tempfile::tempdir().unwrap();
        let journal = journal(1_000, &[part(1000, 7)]);
        fs::write(dir.path().join("active.parts"), &journal).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();

        let first = local.scan().unwrap();
        assert_eq!(first.active.len(), 1);
        assert_eq!(first.valid_len, journal.len() as u64);

        let prev_active = Arc::clone(&first.active);
        let again = local
            .scan_from(first.valid_len, Arc::clone(&prev_active))
            .unwrap();
        assert_eq!(again.active.len(), 1, "unchanged journal keeps the part");
        assert_eq!(again.active[0].catalog.min_ts, 1000);
        assert_eq!(again.valid_len, journal.len() as u64);
        assert!(
            Arc::ptr_eq(&again.active, &prev_active),
            "an unchanged scan must retain the validated active allocation"
        );
    }

    #[test]
    fn scan_from_appends_only_the_new_tail_part() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let journal = journal(1_000, &[part(1000, 7)]);
        fs::write(&journal_path, &journal).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();

        let first = local.scan().unwrap();
        let first_valid = first.valid_len;
        let first_offset = first.active[0].part.offset;
        let previous = Arc::clone(&first.active);

        // Append a second frame.
        let buf = append_journal_part(&journal_path, 1_000, &part(3000, 7));

        let scan = local
            .scan_from(first_valid, Arc::clone(&first.active))
            .unwrap();
        assert_eq!(scan.active.len(), 2, "prev part kept, new tail appended");
        assert!(
            !Arc::ptr_eq(&scan.active, &previous),
            "appending must leave the previous snapshot allocation immutable"
        );
        assert_eq!(
            previous.len(),
            1,
            "the previous snapshot keeps its exact view"
        );
        assert_eq!(
            scan.active[0].part.offset, first_offset,
            "the first part keeps its original offset"
        );
        assert_eq!(scan.active[0].catalog.min_ts, 1000);
        assert_eq!(scan.active[1].catalog.min_ts, 3000);
        assert_eq!(scan.valid_len, buf.len() as u64);
    }

    #[test]
    fn scan_from_size_shrink_resets_and_rescans_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        // Two frames make the initial journal larger than one replacement frame.
        let two = journal(1_000, &[part(1000, 7), part(2000, 7)]);
        fs::write(&journal_path, &two).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();

        let first = local.scan().unwrap();
        assert_eq!(first.active.len(), 2);
        let stale_valid = first.valid_len;

        // Truncate-in-place then write a smaller, different journal.
        let replacement = journal(5_000, &[part(5000, 9)]);
        assert!(
            (replacement.len() as u64) < stale_valid,
            "replacement is smaller"
        );
        fs::write(&journal_path, &replacement).unwrap();

        let scan = local.scan_from(stale_valid, first.active).unwrap();
        assert_eq!(scan.active.len(), 1, "reset yields exactly the new journal");
        assert_eq!(
            scan.active[0].catalog.min_ts, 5000,
            "stale parts are dropped, only the new part surfaces"
        );
        assert_eq!(scan.valid_len, replacement.len() as u64);
    }

    #[test]
    fn scan_from_missing_journal_resets_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, journal(1_000, &[part(1000, 7)])).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();

        fs::remove_file(&journal_path).unwrap();

        let scan = local.scan_from(first.valid_len, first.active).unwrap();
        assert!(
            scan.active.is_empty(),
            "removed journal empties the live set"
        );
        assert_eq!(scan.valid_len, 0, "valid_len resets to zero");
        assert!(scan.damages.is_empty());
    }

    #[test]
    fn scan_from_torn_tail_fails_without_returning_partial_state() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let journal = journal(1_000, &[part(1000, 7)]);
        fs::write(&journal_path, &journal).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();
        let first_valid = first.valid_len;

        // Append a header for a body that is not fully written yet.
        let next = part(3000, 7);
        let mut buf = fs::read(&journal_path).unwrap();
        let full = frame(&next);
        buf.extend_from_slice(&full[..full.len() - 3]); // truncated tail frame
        let body_len = buf.len() - JOURNAL_HEADER_LEN;
        buf[..JOURNAL_HEADER_LEN].copy_from_slice(
            &JournalHeader {
                state: JournalState::Active { segment_id: 1_000 },
                body_len: body_len as u64,
            }
            .encode(),
        );
        fs::write(&journal_path, &buf).unwrap();

        assert_eq!(
            local
                .scan_from(first_valid, first.active)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn scan_from_discovers_new_sealed_segment() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, journal(1_000, &[part(1000, 7)])).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();
        assert_eq!(first.sealed.len(), 0);

        write_segment(dir.path(), 500, part(500, 7));

        let scan = local.scan_from(first.valid_len, first.active).unwrap();
        assert_eq!(scan.sealed.len(), 1, "new sealed .pgm is discovered");
        assert_eq!(scan.sealed[0].summary.min_ts, 500);
        assert_eq!(scan.active.len(), 1, "active part is preserved");
    }

    // A mock ReadAt that claims a large byte_len but returns UnexpectedEof on
    // body reads — simulating a file that was truncated between byte_len() and
    // the first read_exact_at call (TOCTOU race with a concurrent seal/reset).
    struct TruncatedAfterHeader {
        data: Vec<u8>,
        /// Reported size is larger than `data.len()`, causing body reads to fail.
        reported_len: u64,
    }

    impl ReadAt for TruncatedAfterHeader {
        fn byte_len(&self) -> io::Result<u64> {
            Ok(self.reported_len)
        }
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(buf.len());
            if end > self.data.len() {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            buf.copy_from_slice(&self.data[start..end]);
            Ok(())
        }
    }

    // A concurrent shrink is reported as transient interruption. It must not
    // turn a partial prefix into an authoritative live set.
    #[test]
    fn scan_reports_journal_truncated_mid_frame_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let p = part(2000, 7);
        let frame_header = FrameHeader {
            part_len: p.len() as u64,
        }
        .encode();
        let body_len = (FRAME_HEADER_LEN + p.len()) as u64;
        let mut data = JournalHeader {
            state: JournalState::Active { segment_id: 2_000 },
            body_len,
        }
        .encode()
        .to_vec();
        data.extend_from_slice(&frame_header);
        let reported_len = JOURNAL_HEADER_LEN as u64 + body_len;
        let mock = TruncatedAfterHeader { data, reported_len };

        let local = LocalDir::open(dir.path()).unwrap();
        assert_eq!(
            local
                .scan_journal_reader_from(
                    &mock,
                    JOURNAL_HEADER_LEN as u64,
                    Arc::new(Vec::new()),
                    &journal_path,
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn scan_reports_journal_truncated_at_second_frame_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");

        let p1 = part(2000, 7);
        let header1 = FrameHeader {
            part_len: p1.len() as u64,
        }
        .encode();
        let p2_fake_len = 512_u64; // claimed body size, not present in data
        let header2 = FrameHeader {
            part_len: p2_fake_len,
        }
        .encode();

        let body_len = (FRAME_HEADER_LEN + p1.len()) as u64 + FRAME_HEADER_LEN as u64 + p2_fake_len;
        let mut data = JournalHeader {
            state: JournalState::Active { segment_id: 2_000 },
            body_len,
        }
        .encode()
        .to_vec();
        data.extend_from_slice(&header1);
        data.extend_from_slice(&p1);
        data.extend_from_slice(&header2);
        let reported_len = JOURNAL_HEADER_LEN as u64 + body_len;

        let mock = TruncatedAfterHeader { data, reported_len };

        let local = LocalDir::open(dir.path()).unwrap();
        assert_eq!(
            local
                .scan_journal_reader_from(
                    &mock,
                    JOURNAL_HEADER_LEN as u64,
                    Arc::new(Vec::new()),
                    &journal_path,
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    // A mock ReadAt that serves each offset once, then returns UnexpectedEof on
    // any re-read of the same offset — simulating the journal shrinking AFTER
    // the streaming scan validated a part but BEFORE scan_journal_reader's own
    // per-part re-read (the second stale-catch point, inside the loop).
    struct ShrinksAfterScan {
        data: Vec<u8>,
        seen: std::cell::RefCell<std::collections::HashSet<u64>>,
    }

    impl ReadAt for ShrinksAfterScan {
        fn byte_len(&self) -> io::Result<u64> {
            Ok(self.data.len() as u64)
        }
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
            if !self.seen.borrow_mut().insert(offset) {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(buf.len());
            if end > self.data.len() {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            buf.copy_from_slice(&self.data[start..end]);
            Ok(())
        }
    }

    #[test]
    fn scan_reports_journal_shrink_after_streaming_scan_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let p = part(2000, 7);
        let data = journal(2_000, &[p]);
        let mock = ShrinksAfterScan {
            data,
            seen: std::cell::RefCell::new(std::collections::HashSet::new()),
        };

        let local = LocalDir::open(dir.path()).unwrap();
        assert_eq!(
            local
                .scan_journal_reader_from(
                    &mock,
                    JOURNAL_HEADER_LEN as u64,
                    Arc::new(Vec::new()),
                    &journal_path,
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }
}
