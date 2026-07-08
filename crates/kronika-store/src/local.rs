//! Directory-backed storage implementation.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use kronika_format::{
    Catalog, DEFAULT_MAX_PART_LEN, FORMAT_VERSION, JournalLimits, MAGIC, ReadAt, TAIL_INDEX_LEN,
    TailIndex, scan_journal_streaming, validate_part_catalog,
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

        // Scan active.parts before sealed files. During seal, this captures the
        // journal copy before reset; after seal, the later sealed-file scan
        // finds the .pgm copy and the snapshot layer deduplicates.
        let journal_path = self.root.join("active.parts");
        let (active, damages) = match File::open(&journal_path) {
            Ok(file) => {
                let report = scan_journal_streaming(&file, JournalLimits::default(), 1 << 20)?;

                let mut active_parts = Vec::new();
                for part_ref in report.parts {
                    // Keep allocation bounded even if scanner limits change.
                    if part_ref.len as u64 > DEFAULT_MAX_PART_LEN {
                        warnings.push(StoreWarning {
                            path: journal_path.clone(),
                            reason: format!(
                                "part at offset {} claims len {} which exceeds DEFAULT_MAX_PART_LEN",
                                part_ref.offset, part_ref.len
                            ),
                        });
                        continue;
                    }

                    // Read one bounded part to attach its catalog to the scan result.
                    let mut buf = vec![0_u8; part_ref.len];
                    file.read_exact_at(&mut buf, part_ref.offset as u64)?;

                    // The streaming scanner already checked section CRCs. Re-checking
                    // the catalog keeps the scan result self-contained; a mismatch is
                    // reported instead of silently dropping the part.
                    match validate_part_catalog(&buf) {
                        Ok(catalog) => active_parts.push(ActivePart {
                            part: part_ref,
                            catalog,
                        }),
                        Err(err) => warnings.push(StoreWarning {
                            path: journal_path.clone(),
                            reason: format!(
                                "part at offset {} passed scanner but failed catalog decode: {err}",
                                part_ref.offset
                            ),
                        }),
                    }
                }
                (active_parts, report.damages)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(err) => return Err(err),
        };

        // A part sealed after the journal scan appears in the sealed-file pass.
        let mut pgm_paths: Vec<PathBuf> = Vec::new();
        for entry_result in fs::read_dir(&self.root)? {
            match entry_result {
                Ok(entry) => {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("pgm") {
                        pgm_paths.push(path);
                    }
                }
                Err(err) => warnings.push(StoreWarning {
                    path: self.root.clone(),
                    reason: format!("read_dir entry error: {err}"),
                }),
            }
        }
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
    use kronika_format::{
        ENTRY_LEN, FrameHeader, META_LEN, PartMeta, SectionInput, build_part, crc32c,
    };

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

    // --- scan() behavioral tests ---

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
