//! Read view over sealed files and active journal parts.
//!
//! Combines `LocalDir`'s sealed `.pgm` segments and `active.parts` journal into
//! one list, suppresses exact sealed/live duplicates, and decodes both through
//! `PgmUnit`.

use std::io;
use std::path::Path;

use kronika_format::{Catalog, DamageRegion, Entry};
use kronika_registry::{DecodedSection, Row};
use kronika_store::{LocalDir, LocalScan, StoreError, StoreWarning};

use crate::{Dictionary, PgmUnit, ReadError};

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

/// A point-in-time view of a `LocalDir` combining sealed and active units.
///
/// `open` calls `LocalDir::scan` with journal-first ordering. A part in the seal
/// window is visible either from `active.parts` before reset or from a sealed
/// `.pgm` after seal. `units()` drops an active part only when its catalog
/// exactly matches a sealed unit catalog.
#[derive(Debug)]
pub struct LocalDirSnapshot {
    dir: LocalDir,
    scan: LocalScan,
}

impl LocalDirSnapshot {
    /// Open a local directory and take an initial snapshot.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be opened or scanned.
    pub fn open(root: &Path) -> io::Result<Self> {
        let dir = LocalDir::open(root)?;
        let scan = dir.scan()?;
        Ok(Self { dir, scan })
    }

    /// Re-scan the directory, picking up new sealed files and journal appends.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be re-scanned.
    pub fn refresh(&mut self) -> io::Result<()> {
        self.scan = self.dir.scan()?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;

    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use super::*;

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
    fn middle_corruption_reported_through_snapshot_api_rest_served() {
        let dir = tempfile::tempdir().unwrap();
        let part1 = make_part(1000, 2000, 1);
        let part2 = make_part(3000, 4000, 1);
        let mut journal = framed(&part1);
        journal.extend_from_slice(b"GARBAGE_BYTES_HERE_THAT_ARE_NOT_A_VALID_FRAME");
        journal.extend_from_slice(&framed(&part2));
        fs::write(dir.path().join("active.parts"), &journal).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        assert_eq!(units.len(), 2, "both valid parts must be visible");
        assert!(
            !snap.damages().is_empty(),
            "corrupt region must be recorded as a damage"
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
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{StrId, Ts};
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
            sealed_rows[0]["archived_count"],
            Cell::I64(5),
            "archived_count cell"
        );
        // last_archived_wal carries a StrId.
        assert!(
            matches!(sealed_rows[0]["last_archived_wal"], Cell::StrId(_)),
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
        assert_eq!(rows[0]["archived_count"], Cell::I64(5));

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
}
