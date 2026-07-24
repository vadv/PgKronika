//! Directory-backed storage implementation.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use kronika_format::{
    Catalog, DEFAULT_MAX_PART_LEN, DEFAULT_RESYNC_CHUNK, DamageRegion, FORMAT_VERSION,
    FRAME_HEADER_LEN, JournalLimits, MAGIC, ReadAt, TAIL_INDEX_LEN, TailIndex,
    scan_journal_streaming_from, validate_part_catalog,
};

use crate::source::{ActivePart, LocalScan, SealedUnit, StoreError, StoreWarning};

/// Upper bound on the catalog block size; guards against corrupt tail indices.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;

/// A storage directory containing sealed `.pgm` segments and an `active.parts`
/// journal.
#[derive(Debug, Clone)]
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
    /// `active.parts` cannot be opened with an error other than `NotFound`.
    pub fn scan(&self) -> io::Result<LocalScan> {
        let mut warnings = Vec::new();

        // Scan active.parts before sealed files to narrow the seal window. This
        // ordering is not an atomic cross-file view; the snapshot layer can
        // deduplicate only copies whose catalogs are both observed.
        let journal_path = self.root.join("active.parts");
        let (active, damages, valid_len) = match File::open(&journal_path) {
            Ok(file) => {
                self.scan_journal_reader_from(&file, 0, Vec::new(), &journal_path, &mut warnings)?
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => (Vec::new(), Vec::new(), 0),
            Err(err) => return Err(err),
        };

        let sealed = self.list_sealed(&mut warnings)?;

        Ok(LocalScan {
            sealed,
            active,
            damages,
            warnings,
            valid_len,
        })
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
    /// - size `< last_valid_len` or the file is gone: a reset (truncate-in-place);
    ///   `prev_active` is dropped and the journal is scanned from `0`.
    ///
    /// Sealed `.pgm` files are always re-listed. Journal reads that hit
    /// `UnexpectedEof`/`NotFound` (a concurrent seal + reset) are downgraded to a
    /// warning, matching [`scan`](Self::scan).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory cannot be read, or if the journal
    /// cannot be stated or opened with an error other than `NotFound`.
    pub fn scan_from(
        &self,
        last_valid_len: u64,
        prev_active: Vec<ActivePart>,
    ) -> io::Result<LocalScan> {
        let mut warnings = Vec::new();
        let journal_path = self.root.join("active.parts");

        let (active, damages, valid_len) = match fs::metadata(&journal_path) {
            Ok(meta) => {
                let size = meta.len();
                if size == last_valid_len {
                    // Unchanged journal size: keep known parts, skip the body
                    // read. An equal-length rewrite is indistinguishable by size
                    // alone; decoding re-validates each unit's catalog, so a
                    // stale hit is caught there, not here.
                    (prev_active, Vec::new(), last_valid_len)
                } else {
                    let file = File::open(&journal_path)?;
                    if size > last_valid_len {
                        self.scan_journal_reader_from(
                            &file,
                            last_valid_len,
                            prev_active,
                            &journal_path,
                            &mut warnings,
                        )?
                    } else {
                        // size < last_valid_len: reset (truncate-in-place),
                        // drop known parts and rescan from the start.
                        self.scan_journal_reader_from(
                            &file,
                            0,
                            Vec::new(),
                            &journal_path,
                            &mut warnings,
                        )?
                    }
                }
            }
            // The journal vanished: treat as a reset to an empty journal.
            Err(err) if err.kind() == io::ErrorKind::NotFound => (Vec::new(), Vec::new(), 0),
            Err(err) => return Err(err),
        };

        let sealed = self.list_sealed(&mut warnings)?;

        Ok(LocalScan {
            sealed,
            active,
            damages,
            warnings,
            valid_len,
        })
    }

    /// List sealed `.pgm` segments, decoding each catalog from its tail.
    ///
    /// Unreadable `.pgm` files are skipped and recorded as [`StoreWarning`]s.
    fn list_sealed(&self, warnings: &mut Vec<StoreWarning>) -> io::Result<Vec<SealedUnit>> {
        let mut sealed = Vec::new();
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
        Ok(sealed)
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
    /// Returns [`StoreError::ActivePartTooLarge`] if the cached part reference
    /// exceeds the active part cap, or [`StoreError::Io`] if the journal file
    /// cannot be opened or the part bytes cannot be read.
    pub fn read_active_part(&self, p: &ActivePart) -> Result<Vec<u8>, StoreError> {
        let part_len = u64::try_from(p.part.len).unwrap_or(u64::MAX);
        if part_len > DEFAULT_MAX_PART_LEN {
            return Err(StoreError::ActivePartTooLarge {
                len: p.part.len,
                max: DEFAULT_MAX_PART_LEN,
            });
        }
        let journal_path = self.root.join("active.parts");
        let file = File::open(journal_path)?;
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
    /// A concurrent seal followed by `Journal::reset` can truncate the journal
    /// while the streaming scan or a per-part re-read is running; the resulting
    /// `UnexpectedEof`/`NotFound` means the live journal shrank under us. During
    /// the streaming scan this yields an empty live set; during per-part re-read
    /// it yields the parts already attached. In both cases the sealed-file scan
    /// still runs and a warning records the race. Other I/O errors propagate.
    #[expect(
        clippy::unused_self,
        reason = "method on LocalDir for API symmetry and unit-testing with a mock ReadAt"
    )]
    fn scan_journal_reader_from<R: ReadAt>(
        &self,
        reader: &R,
        start_at: u64,
        prev_active: Vec<ActivePart>,
        journal_path: &Path,
        warnings: &mut Vec<StoreWarning>,
    ) -> io::Result<(Vec<ActivePart>, Vec<DamageRegion>, u64)> {
        let report = match scan_journal_streaming_from(
            reader,
            start_at,
            JournalLimits::default(),
            DEFAULT_RESYNC_CHUNK,
        ) {
            Ok(report) => report,
            Err(err) if is_stale_journal(&err) => {
                warnings.push(StoreWarning {
                    path: journal_path.to_owned(),
                    reason: format!("live journal changed during scan: {err}"),
                });
                return Ok((Vec::new(), Vec::new(), start_at));
            }
            Err(err) => return Err(err),
        };

        let mut active = prev_active;
        active.reserve(report.parts.len());
        // The scan's valid_len assumes every part re-reads cleanly. If a per-part
        // re-read hits a concurrent shrink, roll the resumable offset back to the
        // failed frame's start so the next scan does not skip past it.
        let mut valid_len = report.valid_len as u64;
        for part_ref in report.parts {
            if part_ref.len as u64 > DEFAULT_MAX_PART_LEN {
                warnings.push(StoreWarning {
                    path: journal_path.to_owned(),
                    reason: format!(
                        "active part at offset {} exceeds the max part length",
                        part_ref.offset
                    ),
                });
                continue;
            }
            let mut buf = vec![0_u8; part_ref.len];
            match reader.read_exact_at(&mut buf, part_ref.offset as u64) {
                Ok(()) => {}
                Err(err) if is_stale_journal(&err) => {
                    warnings.push(StoreWarning {
                        path: journal_path.to_owned(),
                        reason: format!("live journal changed during scan: {err}"),
                    });
                    valid_len = (part_ref.offset - FRAME_HEADER_LEN) as u64;
                    break;
                }
                Err(err) => return Err(err),
            }
            match validate_part_catalog(&buf) {
                Ok(catalog) => active.push(ActivePart {
                    part: part_ref,
                    catalog,
                }),
                Err(err) => warnings.push(StoreWarning {
                    path: journal_path.to_owned(),
                    reason: format!(
                        "active part at offset {} passed the frame scan but failed catalog decode: {err}",
                        part_ref.offset
                    ),
                }),
            }
        }

        Ok((active, report.damages, valid_len))
    }
}

/// Whether an I/O error means the live journal shrank or vanished under us
/// (a concurrent seal + `Journal::reset`), rather than a real I/O failure.
fn is_stale_journal(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound
    )
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

    #[test]
    fn read_active_part_rejects_oversized_ref_before_allocation() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("active.parts"), b"").unwrap();
        let catalog = read_catalog(&part(1000, 42).as_slice()).expect("catalog");
        let oversized_len = usize::try_from(DEFAULT_MAX_PART_LEN).expect("part cap fits usize") + 1;
        let active = ActivePart {
            part: kronika_format::PartRef {
                offset: FRAME_HEADER_LEN,
                len: oversized_len,
            },
            catalog,
        };

        let err = LocalDir::open(dir.path())
            .unwrap()
            .read_active_part(&active)
            .unwrap_err();

        assert!(
            matches!(
                err,
                StoreError::ActivePartTooLarge { len, max }
                    if len == oversized_len && max == DEFAULT_MAX_PART_LEN
            ),
            "oversized active part must be rejected before allocation"
        );
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

    #[test]
    fn scan_keeps_sealed_when_active_journal_has_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("1000.pgm"), part(1000, 7)).unwrap();
        let unfinished_part = part(2000, 7);
        fs::write(
            dir.path().join("active.parts"),
            FrameHeader {
                part_len: u64::try_from(unfinished_part.len()).expect("part length fits u64"),
            }
            .encode(),
        )
        .unwrap();

        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1, "sealed discovery must still run");
        assert!(
            scan.active.is_empty(),
            "unfinished live frame must not become active"
        );
        assert_eq!(scan.damages.len(), 1, "torn live frame must be reported");
        assert!(
            scan.warnings.is_empty(),
            "torn tail is typed damage, not a file warning"
        );
    }

    // --- scan_from() incremental tests ---

    /// Wrap a part body in one journal frame.
    fn framed(part_bytes: &[u8]) -> Vec<u8> {
        let mut out = FrameHeader {
            part_len: part_bytes.len() as u64,
        }
        .encode()
        .to_vec();
        out.extend_from_slice(part_bytes);
        out
    }

    #[test]
    fn scan_from_unchanged_size_keeps_prev_and_reports_same_valid_len() {
        let dir = tempfile::tempdir().unwrap();
        let journal = framed(&part(1000, 7));
        fs::write(dir.path().join("active.parts"), &journal).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();

        let first = local.scan().unwrap();
        assert_eq!(first.active.len(), 1);
        assert_eq!(first.valid_len, journal.len() as u64);

        let prev_active = first.active;
        let again = local.scan_from(first.valid_len, prev_active).unwrap();
        assert_eq!(again.active.len(), 1, "unchanged journal keeps the part");
        assert_eq!(again.active[0].catalog.min_ts, 1000);
        assert_eq!(again.valid_len, journal.len() as u64);
    }

    #[test]
    fn scan_from_appends_only_the_new_tail_part() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let journal = framed(&part(1000, 7));
        fs::write(&journal_path, &journal).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();

        let first = local.scan().unwrap();
        let first_valid = first.valid_len;
        let first_offset = first.active[0].part.offset;

        // Append a second frame.
        let mut buf = fs::read(&journal_path).unwrap();
        buf.extend_from_slice(&framed(&part(3000, 7)));
        fs::write(&journal_path, &buf).unwrap();

        let scan = local.scan_from(first_valid, first.active).unwrap();
        assert_eq!(scan.active.len(), 2, "prev part kept, new tail appended");
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
        let mut two = framed(&part(1000, 7));
        two.extend_from_slice(&framed(&part(2000, 7)));
        fs::write(&journal_path, &two).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();

        let first = local.scan().unwrap();
        assert_eq!(first.active.len(), 2);
        let stale_valid = first.valid_len;

        // Truncate-in-place then write a smaller, different journal.
        let replacement = framed(&part(5000, 9));
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
        fs::write(&journal_path, framed(&part(1000, 7))).unwrap();
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
    fn scan_from_torn_tail_does_not_advance_valid_len() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let journal = framed(&part(1000, 7));
        fs::write(&journal_path, &journal).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();
        let first_valid = first.valid_len;

        // Append a header for a body that is not fully written yet.
        let next = part(3000, 7);
        let mut buf = fs::read(&journal_path).unwrap();
        let full = framed(&next);
        buf.extend_from_slice(&full[..full.len() - 3]); // truncated tail frame
        fs::write(&journal_path, &buf).unwrap();

        let scan = local.scan_from(first_valid, first.active).unwrap();
        assert_eq!(scan.active.len(), 1, "torn tail frame is not surfaced");
        assert_eq!(
            scan.valid_len, first_valid,
            "valid_len stays at the last complete frame"
        );

        // Finish the frame: the next incremental scan surfaces it.
        let mut done = journal;
        done.extend_from_slice(&full);
        fs::write(&journal_path, &done).unwrap();

        let after = local.scan_from(scan.valid_len, scan.active).unwrap();
        assert_eq!(after.active.len(), 2, "completed frame now surfaces");
        assert_eq!(after.active[1].catalog.min_ts, 3000);
        assert_eq!(after.valid_len, done.len() as u64);
    }

    #[test]
    fn scan_from_discovers_new_sealed_segment() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        fs::write(&journal_path, framed(&part(1000, 7))).unwrap();
        let local = LocalDir::open(dir.path()).unwrap();
        let first = local.scan().unwrap();
        assert_eq!(first.sealed.len(), 0);

        fs::write(dir.path().join("0500.pgm"), part(500, 7)).unwrap();

        let scan = local.scan_from(first.valid_len, first.active).unwrap();
        assert_eq!(scan.sealed.len(), 1, "new sealed .pgm is discovered");
        assert_eq!(scan.sealed[0].catalog.min_ts, 500);
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

    // scan_journal_reader returns empty active + warning when scan_journal_streaming
    // itself hits UnexpectedEof (e.g., the file shrank between byte_len and body
    // read inside the streaming scan — TOCTOU race with a concurrent seal/reset).
    #[test]
    fn scan_survives_journal_truncated_mid_frame() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        // The actual content is just a valid frame header; byte_len lies and
        // says there is a full body too — read_exact_at on body bytes fails.
        let p = part(2000, 7);
        let header = FrameHeader {
            part_len: p.len() as u64,
        }
        .encode();
        let reported_len = (FRAME_HEADER_LEN + p.len()) as u64;
        let mock = TruncatedAfterHeader {
            data: header.to_vec(),
            reported_len,
        };

        let local = LocalDir::open(dir.path()).unwrap();
        let mut warnings = Vec::new();
        let (active, _damages, _valid_len) = local
            .scan_journal_reader_from(&mock, 0, Vec::new(), &journal_path, &mut warnings)
            .expect("scan must not fail fatally on UnexpectedEof from streaming scan");

        assert!(
            active.is_empty(),
            "no active parts expected from truncated journal"
        );
        assert!(
            warnings.iter().any(|w| w.path == journal_path),
            "a warning must reference the journal path"
        );
    }

    // scan_journal_reader stays non-fatal when the streaming scan hits
    // UnexpectedEof on a LATER frame's body (a second frame whose body extends
    // past the shrunken journal): the streaming-scan error is caught, a warning
    // is emitted, and the caller still continues to the sealed-file pass.
    #[test]
    fn scan_survives_journal_truncated_at_second_frame() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");

        // Build a journal with one valid frame followed by a second frame
        // header with no body. The mock's data contains both headers and the
        // first body, but reported_len includes the second body too — so
        // scan_journal_streaming reads the first frame completely (valid), then
        // reads the second frame header but finds the body extends past the
        // actual data → UnexpectedEof inside streaming_frame_at's body read,
        // propagated as Err from scan_journal_streaming.
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

        let mut data = Vec::new();
        data.extend_from_slice(&header1);
        data.extend_from_slice(&p1);
        data.extend_from_slice(&header2);
        // p2 body is NOT in data; reported_len pretends it is.
        let reported_len = (data.len() as u64) + p2_fake_len;

        let mock = TruncatedAfterHeader { data, reported_len };

        let local = LocalDir::open(dir.path()).unwrap();
        let mut warnings = Vec::new();
        let (active, _damages, _valid_len) = local
            .scan_journal_reader_from(&mock, 0, Vec::new(), &journal_path, &mut warnings)
            .expect("scan must not fail fatally");

        // scan_journal_streaming sees the second frame's body is "present" per
        // reported_len but read_exact_at fails → Err(UnexpectedEof) →
        // scan_journal_reader catches it, emits warning, returns empty active.
        assert!(
            active.is_empty(),
            "truncated journal must yield no active parts"
        );
        assert!(
            warnings.iter().any(|w| w.path == journal_path),
            "scan must warn about the truncated journal"
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

    // Exercises the per-part-loop stale-catch: the streaming scan validates one
    // part (first read of each offset), then the loop's own read_exact_at of the
    // part body hits UnexpectedEof (second read of that offset). The part is
    // dropped with a warning and the scan does not fail.
    #[test]
    fn scan_survives_journal_shrink_after_streaming_scan() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("active.parts");
        let p = part(2000, 7);
        let mut data = Vec::new();
        data.extend_from_slice(
            &FrameHeader {
                part_len: p.len() as u64,
            }
            .encode(),
        );
        data.extend_from_slice(&p);
        let mock = ShrinksAfterScan {
            data,
            seen: std::cell::RefCell::new(std::collections::HashSet::new()),
        };

        let local = LocalDir::open(dir.path()).unwrap();
        let mut warnings = Vec::new();
        let (active, _damages, _valid_len) = local
            .scan_journal_reader_from(&mock, 0, Vec::new(), &journal_path, &mut warnings)
            .expect("scan must not fail fatally when the per-part re-read hits EOF");

        assert!(
            active.is_empty(),
            "the part must be dropped when its per-part re-read fails"
        );
        assert!(
            warnings.iter().any(|w| w.path == journal_path),
            "a warning must reference the journal path"
        );
    }
}
