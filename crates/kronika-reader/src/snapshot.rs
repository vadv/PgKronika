//! `LocalDirSnapshot`: a unified read view over sealed and active units.
//!
//! Combines `LocalDir`'s sealed `.pgm` segments and `active.parts` journal
//! into one list, deduplicating active parts that a sealed unit already covers,
//! and decoding both via `PgmUnit`.

use std::io;
use std::path::Path;

use kronika_format::Entry;
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
/// `open` calls `LocalDir::scan` with the journal-first ordering, so a part
/// mid-seal is captured in `active` (before the `.pgm` is written) or in
/// `sealed` (after), never lost.  `units()` then deduplicates: an active part
/// whose `[min_ts, max_ts]` is fully covered by a sealed unit of the same
/// `source_id` is dropped — the sealed file is the authoritative record.
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
    /// Sealed units appear first, then surviving live parts.  An active part is
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

    /// Decode one section from the unit at position `idx` in `units()`.
    ///
    /// `idx` indexes the same ordering as `units()`: sealed units first, then
    /// surviving live parts in journal order.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the unit cannot be opened or the section
    /// fails CRC or typed decode.
    pub fn decode_unit(&self, idx: usize, entry: &Entry) -> Result<DecodedSection, ReadError> {
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
                PgmUnit::open(file)?.decode(entry)
            }
            Handle::Active(i) => {
                let ap = &self.scan.active[i];
                let bytes = self.dir.read_active_part(ap)?;
                PgmUnit::open(bytes.as_slice())?.decode(entry)
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
}
