//! Read view over sealed files and active journal parts.
//!
//! Combines `LocalDir`'s sealed `.pgm` segments and `active.parts` journal into
//! one list, drops active parts that a sealed unit already covers, and decodes
//! both through `PgmUnit`.

use std::io;
use std::path::Path;

use kronika_format::Catalog;
use kronika_registry::DecodedSection;
use kronika_store::{LocalDir, LocalScan, StoreWarning};

use crate::{PgmUnit, ReadError};

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
/// `.pgm` after seal. `units()` drops an active part whose `[min_ts, max_ts]`
/// range is fully covered by a sealed unit with the same `source_id`.
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

    /// Deduplicated list of units visible in this snapshot.
    ///
    /// Sealed units appear first, then surviving live parts. An active part is
    /// omitted when a sealed unit of the same `source_id` covers its entire
    /// `[min_ts, max_ts]` range.
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
                let bytes = self.dir.read_active_part(ap)?;
                let unit = PgmUnit::open(bytes.as_slice())?;
                if unit.catalog() != &ap.catalog {
                    return Err(ReadError::StaleSnapshot { unit_idx: idx });
                }
                unit.decode(cached_entry)
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
            .filter(|(_, ap)| {
                let ac = &ap.catalog;
                !self.scan.sealed.iter().any(|su| {
                    let sc = &su.catalog;
                    sc.source_id == ac.source_id && sc.min_ts <= ac.min_ts && ac.max_ts <= sc.max_ts
                })
            })
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
    fn sealed_covering_part_is_deduped_no_double() {
        let dir = tempfile::tempdir().unwrap();
        let part = make_part(1000, 2000, 42);
        // Write the same data as a sealed .pgm.
        fs::write(dir.path().join("1000.pgm"), &part).unwrap();
        // And as an active part covering the same range.
        let journal = framed(&part);
        fs::write(dir.path().join("active.parts"), &journal).unwrap();

        let snap = LocalDirSnapshot::open(dir.path()).unwrap();
        let units = snap.units();
        assert_eq!(units.len(), 1, "covered active part must be deduped");
        assert!(!units[0].live, "surviving unit must be the sealed one");
        assert_eq!(units[0].source_id, 42);
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
    fn middle_corruption_reported_rest_served() {
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
            !snap.scan.damages.is_empty(),
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
        assert!(snap.scan.damages.is_empty());
    }
}
