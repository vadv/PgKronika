//! Directory-backed storage implementation.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use kronika_format::{
    Catalog, FORMAT_VERSION, JournalLimits, MAGIC, ReadAt, TAIL_INDEX_LEN, TailIndex,
    scan_journal_streaming, validate_part_catalog,
};

use crate::source::{ActivePart, LocalScan, SealedUnit, StoreError, StoreWarning};

/// Upper bound on the catalog block size; guards against corrupt tail indices.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;

/// A storage directory containing sealed `.pgm` segments and an `active.parts`
/// journal.
#[derive(Debug)]
pub struct LocalDir {
    root: PathBuf,
}

impl LocalDir {
    /// Open a local directory as a segment store.
    ///
    /// Returns an error only if `root` cannot be read as a directory. Individual
    /// files inside the directory are validated lazily during [`scan`](Self::scan).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `root` is not a directory or cannot be accessed.
    pub fn open(root: &Path) -> io::Result<Self> {
        let meta = fs::metadata(root)?;
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "root is not a directory",
            ));
        }
        Ok(Self {
            root: root.to_owned(),
        })
    }

    /// Scan the directory for sealed segments and active journal parts.
    ///
    /// Sealed `.pgm` files whose catalog cannot be read are skipped and
    /// recorded as [`StoreWarning`]s. The `active.parts` journal is scanned
    /// streaming, keeping peak memory bounded to one part body.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory itself cannot be read, or if
    /// `active.parts` cannot be opened.
    pub fn scan(&self) -> io::Result<LocalScan> {
        let mut sealed = Vec::new();
        let mut warnings = Vec::new();

        // Collect and sort *.pgm file names deterministically.
        let mut pgm_paths: Vec<PathBuf> = fs::read_dir(&self.root)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                (path.extension().and_then(|e| e.to_str()) == Some("pgm")).then_some(path)
            })
            .collect();
        pgm_paths.sort();

        for path in pgm_paths {
            match read_catalog_from_path(&path) {
                Ok(catalog) => sealed.push(SealedUnit { path, catalog }),
                Err(err) => warnings.push(StoreWarning {
                    path,
                    reason: err.to_string(),
                }),
            }
        }

        // Scan active.parts if it exists.
        let journal_path = self.root.join("active.parts");
        let (active, damages) = if journal_path.exists() {
            let file = File::open(&journal_path)?;
            let report = scan_journal_streaming(&file, JournalLimits::default(), 1 << 20)?;

            let mut active_parts = Vec::new();
            for part_ref in report.parts {
                // Read just this part's bytes to validate its catalog.
                let mut buf = vec![0_u8; part_ref.len];
                file.read_exact_at(&mut buf, part_ref.offset as u64)?;
                if let Ok(catalog) = validate_part_catalog(&buf) {
                    active_parts.push(ActivePart {
                        part: part_ref,
                        catalog,
                    });
                }
            }
            (active_parts, report.damages)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(LocalScan {
            sealed,
            active,
            damages,
            warnings,
        })
    }

    /// Open a sealed segment file for raw byte access.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be opened.
    #[expect(
        clippy::unused_self,
        reason = "method is on LocalDir for API symmetry; the path comes from SealedUnit"
    )]
    pub fn open_sealed(&self, u: &SealedUnit) -> io::Result<File> {
        File::open(&u.path)
    }

    /// Read the bytes of one active part from the journal.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the journal file cannot be opened or the part
    /// bytes cannot be read.
    pub fn read_active_part(&self, p: &ActivePart) -> io::Result<Vec<u8>> {
        let journal_path = self.root.join("active.parts");
        let file = File::open(journal_path)?;
        let mut buf = vec![0_u8; p.part.len];
        file.read_exact_at(&mut buf, p.part.offset as u64)?;
        Ok(buf)
    }
}

/// Read and decode the end catalog from a `.pgm` file path.
fn read_catalog_from_path(path: &Path) -> Result<Catalog, StoreError> {
    let file = File::open(path)?;
    read_catalog(&file)
}

/// Read and decode the end catalog from any [`ReadAt`] source.
///
/// Reads only the tail index and catalog block; no section bodies are loaded.
///
/// # Errors
///
/// Returns [`StoreError`] when the source is too small, the magic bytes are
/// wrong, the format version is unsupported, the catalog block cannot be
/// located, the catalog bytes are corrupt, or a catalog entry points outside
/// the section area.
pub fn read_catalog<R: ReadAt>(reader: &R) -> Result<Catalog, StoreError> {
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

    let mut buf = vec![0_u8; tail.catalog_len as usize];
    reader.read_exact_at(&mut buf, catalog_at)?;
    let catalog = Catalog::decode(&buf).map_err(StoreError::Catalog)?;

    // Verify magic at offset 0.
    let mut magic = [0_u8; MAGIC.len()];
    reader.read_exact_at(&mut magic, 0)?;
    if magic != MAGIC {
        return Err(StoreError::BadMagic);
    }

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
                .is_some_and(|end| end <= catalog_at);
        if !in_bounds {
            return Err(StoreError::OutOfBounds);
        }
    }

    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};

    fn part(ts: i64, src: u64) -> Vec<u8> {
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: b"",
            }],
            PartMeta {
                min_ts: ts,
                max_ts: ts + 1,
                source_id: src,
            },
        )
    }

    #[test]
    fn scan_lists_sealed_and_active_with_cheap_catalog() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("1000.pgm"), part(1000, 7)).unwrap();
        let mut journal = Vec::new();
        for p in [part(2000, 7), part(3000, 7)] {
            journal.extend_from_slice(
                &FrameHeader {
                    part_len: p.len() as u64,
                }
                .encode(),
            );
            journal.extend_from_slice(&p);
        }
        fs::write(dir.path().join("active.parts"), &journal).unwrap();
        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1, "one sealed segment");
        assert_eq!(scan.sealed[0].catalog.min_ts, 1000, "sealed catalog min_ts");
        assert_eq!(scan.active.len(), 2, "two active parts");
        assert_eq!(
            scan.active[1].catalog.min_ts, 3000,
            "second active part min_ts"
        );
        assert!(scan.warnings.is_empty(), "no warnings for clean data");
    }

    #[test]
    fn corrupt_sealed_is_skipped_with_warning_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("1000.pgm"), part(1000, 7)).unwrap();
        fs::write(dir.path().join("bad.pgm"), b"not a pgm").unwrap();
        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1, "good segment still served");
        assert_eq!(scan.warnings.len(), 1, "bad segment produces one warning");
    }
}
