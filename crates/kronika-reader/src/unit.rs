//! Decoder for one PGM container over any `ReadAt` source.
//!
//! The same code handles sealed segment files (`File`) and in-memory journal
//! parts (`&[u8]`).

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::sync::atomic::{AtomicU64, Ordering};

use kronika_format::{
    Catalog, Crc32c, DecodeError, ENTRY_LEN, Entry, FORMAT_VERSION, MAGIC, META_LEN,
    TAIL_INDEX_LEN, TailIndex, crc32c, validate_catalog_layout,
};
use kronika_registry::{
    Bytes, CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, DecodedSection, MAX_SECTION_BYTES,
    Row, VerifiedSection, decode_any, decode_rows,
};

use crate::{Dictionary, ReadError, Stored, decode_dictionary};

/// Upper bound on the catalog block, checked before allocation.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;
const CATALOG_READ_CHUNK_BYTES: usize = 64 * 1024;
const SCRUB_BUFFER_BYTES: usize = 64 * 1024;
const META_ENTRY_COUNT_AT: usize = 16;
const META_FORMAT_VERSION_AT: usize = 20;
const META_CRC_AT: usize = 24;
const META_WINDOW_COUNT_AT: usize = 28;
const _: () = assert!(
    CATALOG_READ_CHUNK_BYTES.is_multiple_of(ENTRY_LEN),
    "catalog read chunks must contain complete fixed-size entries"
);

/// Completed PGM section-body I/O performed by one open unit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PgmBodyReadStats {
    /// Number of section bodies read from the source.
    pub read_calls: u64,
    /// Stored section bytes read.
    pub stored_bytes_read: u64,
}

/// One CRC-verified PGM section selected by catalog ordinal.
pub struct OverviewSectionBody {
    catalog_ordinal: u32,
    descriptor: crate::ManifestEntryDescriptor,
    body: Bytes,
}

impl std::fmt::Debug for OverviewSectionBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverviewSectionBody")
            .field("catalog_ordinal", &self.catalog_ordinal)
            .field("descriptor", &self.descriptor)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl OverviewSectionBody {
    /// Segment-global catalog ordinal.
    #[must_use]
    pub const fn catalog_ordinal(&self) -> u32 {
        self.catalog_ordinal
    }

    /// Catalog metadata and exact body identity.
    #[must_use]
    pub const fn descriptor(&self) -> crate::ManifestEntryDescriptor {
        self.descriptor
    }

    /// CRC-verified section bytes.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        self.body.as_ref()
    }

    /// Consumes the wrapper and returns the verified bytes.
    #[must_use]
    pub fn into_body(self) -> Bytes {
        self.body
    }
}

/// A PGM container opened for reading over any [`kronika_format::ReadAt`] source.
///
/// Works for sealed segment files (`File`) and in-memory journal parts (`&[u8]`).
#[derive(Debug)]
pub struct PgmUnit<R: kronika_format::ReadAt> {
    reader: R,
    catalog: Catalog,
    source_file_len: u64,
    source_descriptor: crate::SourceDescriptor,
    body_read_calls: AtomicU64,
    body_bytes_read: AtomicU64,
}

impl<R: kronika_format::ReadAt> PgmUnit<R> {
    /// Open a PGM container and read its end catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] on I/O errors or invalid container framing.
    pub fn open(reader: R) -> Result<Self, ReadError> {
        let len = reader.byte_len()?;
        let opened = read_catalog_bytes(&reader, len)?;
        Ok(Self {
            reader,
            catalog: opened.catalog,
            source_file_len: len,
            source_descriptor: opened.source_descriptor,
            body_read_calls: AtomicU64::new(0),
            body_bytes_read: AtomicU64::new(0),
        })
    }

    /// The container's end catalog.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Descriptor of the exact file length, tail index, and catalog bytes.
    #[must_use]
    pub const fn source_descriptor(&self) -> crate::SourceDescriptor {
        self.source_descriptor
    }

    /// Exact PGM file length captured while opening the source.
    #[must_use]
    pub const fn source_file_len(&self) -> u64 {
        self.source_file_len
    }

    /// Completed section-body I/O since this unit was opened.
    #[must_use]
    pub fn body_read_stats(&self) -> PgmBodyReadStats {
        PgmBodyReadStats {
            read_calls: self.body_read_calls.load(Ordering::Relaxed),
            stored_bytes_read: self.body_bytes_read.load(Ordering::Relaxed),
        }
    }

    /// Reads one section by its segment-global catalog ordinal.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] for an invalid ordinal, unsafe length, I/O error,
    /// or body CRC mismatch.
    pub fn read_overview_section(
        &self,
        catalog_ordinal: u32,
    ) -> Result<OverviewSectionBody, ReadError> {
        let index = usize::try_from(catalog_ordinal).map_err(|_error| {
            ReadError::CatalogOrdinalOutOfRange {
                ordinal: catalog_ordinal,
            }
        })?;
        let entry = self
            .catalog
            .entries
            .get(index)
            .ok_or(ReadError::CatalogOrdinalOutOfRange {
                ordinal: catalog_ordinal,
            })?;
        let body = self.verified_body(entry)?.into_bytes();
        let descriptor = crate::ManifestEntryDescriptor::from_verified(entry, body.as_ref());
        Ok(OverviewSectionBody {
            catalog_ordinal,
            descriptor,
            body,
        })
    }

    /// Streams and CRC-checks one section without materializing its body.
    ///
    /// The fixed scratch buffer bounds scrub RSS independently of section
    /// length. The ordinal is segment-global and comes from the opened
    /// catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] for an invalid ordinal, unsafe length, I/O error,
    /// counter overflow, or body CRC mismatch.
    pub fn scrub_overview_section(&self, catalog_ordinal: u32) -> Result<(), ReadError> {
        let index = usize::try_from(catalog_ordinal).map_err(|_error| {
            ReadError::CatalogOrdinalOutOfRange {
                ordinal: catalog_ordinal,
            }
        })?;
        let entry = self
            .catalog
            .entries
            .get(index)
            .ok_or(ReadError::CatalogOrdinalOutOfRange {
                ordinal: catalog_ordinal,
            })?;
        let len = usize::try_from(entry.len)
            .ok()
            .filter(|&len| len <= MAX_SECTION_BYTES)
            .ok_or(ReadError::SectionTooLarge { len: entry.len })?;
        self.body_read_calls
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_value| ReadError::CounterOverflow)?;

        let mut checksum = Crc32c::new();
        let mut scratch = vec![0_u8; SCRUB_BUFFER_BYTES].into_boxed_slice();
        let mut consumed = 0_usize;
        while consumed < len {
            let chunk_len = (len - consumed).min(scratch.len());
            let offset = entry
                .offset
                .checked_add(u64::try_from(consumed).map_err(|_error| ReadError::CounterOverflow)?)
                .ok_or(ReadError::CounterOverflow)?;
            self.reader
                .read_exact_at(&mut scratch[..chunk_len], offset)?;
            checksum.update(&scratch[..chunk_len]);
            self.body_bytes_read
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(u64::try_from(chunk_len).ok()?)
                })
                .map_err(|_value| ReadError::CounterOverflow)?;
            consumed = consumed
                .checked_add(chunk_len)
                .ok_or(ReadError::CounterOverflow)?;
        }

        let got = checksum.finalize();
        if got != entry.crc32c {
            return Err(ReadError::Codec(CodecError::Section {
                type_id: entry.type_id,
                bytes_in: len,
                source: Box::new(CodecError::SectionCrcMismatch {
                    expected: entry.crc32c,
                    got,
                }),
            }));
        }
        Ok(())
    }

    /// Reads, CRC-checks, and decodes one section without rereading its body.
    ///
    /// Returns the verified manifest descriptor with the named-cell rows.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] for an invalid ordinal, unsafe length, I/O error,
    /// CRC failure, typed decode failure, or row-count mismatch.
    pub fn decode_overview_rows(
        &self,
        catalog_ordinal: u32,
    ) -> Result<(crate::ManifestEntryDescriptor, Vec<Row>), ReadError> {
        let index = usize::try_from(catalog_ordinal).map_err(|_error| {
            ReadError::CatalogOrdinalOutOfRange {
                ordinal: catalog_ordinal,
            }
        })?;
        let entry = self
            .catalog
            .entries
            .get(index)
            .ok_or(ReadError::CatalogOrdinalOutOfRange {
                ordinal: catalog_ordinal,
            })?;
        if matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
            return Err(ReadError::DictionarySection {
                type_id: entry.type_id,
            });
        }
        let verified = self.verified_body(entry)?;
        let body = verified.clone().into_bytes();
        let descriptor = crate::ManifestEntryDescriptor::from_verified(entry, body.as_ref());
        let rows = decode_rows(entry.type_id, verified).map_err(ReadError::Codec)?;
        let decoded = u64::try_from(rows.len()).map_err(|_error| ReadError::CounterOverflow)?;
        Self::validate_catalog_row_count(entry, decoded)?;
        Ok((descriptor, rows))
    }

    /// Read and decode one section by its catalog `entry`.
    ///
    /// Rejects dictionary sections; call [`dictionary`](Self::dictionary) for those.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the section is a dictionary, out of bounds,
    /// fails CRC, or fails typed decode.
    pub fn decode(&self, entry: &Entry) -> Result<DecodedSection, ReadError> {
        if matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
            return Err(ReadError::DictionarySection {
                type_id: entry.type_id,
            });
        }
        let decoded =
            decode_any(entry.type_id, self.verified_body(entry)?).map_err(ReadError::Codec)?;
        let rows =
            u64::try_from(decoded.stats.rows).map_err(|_error| ReadError::CounterOverflow)?;
        Self::validate_catalog_row_count(entry, rows)?;
        Ok(decoded)
    }

    /// Read and decode one section as named-cell rows.
    ///
    /// Rejects dictionary sections; call [`dictionary`](Self::dictionary) for those.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when the section is a dictionary, out of bounds,
    /// fails CRC, or fails typed decode.
    pub fn decode_rows(&self, entry: &Entry) -> Result<Vec<Row>, ReadError> {
        if matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
            return Err(ReadError::DictionarySection {
                type_id: entry.type_id,
            });
        }
        let rows =
            decode_rows(entry.type_id, self.verified_body(entry)?).map_err(ReadError::Codec)?;
        let decoded = u64::try_from(rows.len()).map_err(|_error| ReadError::CounterOverflow)?;
        Self::validate_catalog_row_count(entry, decoded)?;
        Ok(rows)
    }

    /// Read the container's dictionary sections into a `str_id` -> bytes map.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] when a dictionary section cannot be read or decoded.
    pub fn dictionary(&self) -> Result<Dictionary, ReadError> {
        let mut by_id: HashMap<u64, Stored> = HashMap::new();
        for entry in &self.catalog.entries {
            if !matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
                continue;
            }
            let body = self.verified_body(entry)?.into_bytes();
            let (decoded, rows) =
                decode_dictionary(body, entry.type_id).map_err(ReadError::Codec)?;
            Self::validate_catalog_row_count(entry, rows)?;
            for (str_id, value) in decoded {
                match by_id.entry(str_id) {
                    MapEntry::Vacant(slot) => {
                        slot.insert(value);
                    }
                    MapEntry::Occupied(_) => {
                        return Err(ReadError::DictionaryConflict { str_id });
                    }
                }
            }
        }
        Ok(Dictionary { by_id })
    }

    pub(crate) fn validate_catalog_row_count(entry: &Entry, decoded: u64) -> Result<(), ReadError> {
        if decoded == u64::from(entry.rows) {
            Ok(())
        } else {
            Err(ReadError::CatalogRowCountMismatch {
                type_id: entry.type_id,
                declared: entry.rows,
                decoded,
            })
        }
    }

    /// Read and CRC-check a section body.
    pub(crate) fn verified_body(&self, entry: &Entry) -> Result<VerifiedSection, ReadError> {
        let len = usize::try_from(entry.len)
            .ok()
            .filter(|&len| len <= MAX_SECTION_BYTES)
            .ok_or(ReadError::SectionTooLarge { len: entry.len })?;
        let mut body = vec![0_u8; len];
        self.reader.read_exact_at(&mut body, entry.offset)?;
        self.body_read_calls
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_value| ReadError::CounterOverflow)?;
        self.body_bytes_read
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(entry.len)
            })
            .map_err(|_value| ReadError::CounterOverflow)?;
        VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c).map_err(|source| {
            ReadError::Codec(CodecError::Section {
                type_id: entry.type_id,
                bytes_in: len,
                source: Box::new(source),
            })
        })
    }
}

struct OpenedCatalog {
    catalog: Catalog,
    source_descriptor: crate::SourceDescriptor,
}

fn read_catalog_bytes<R: kronika_format::ReadAt>(
    reader: &R,
    len: u64,
) -> Result<OpenedCatalog, ReadError> {
    let tail_at = len
        .checked_sub(TAIL_INDEX_LEN as u64)
        .ok_or(ReadError::TooSmall { len })?;
    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    reader.read_exact_at(&mut tail_bytes, tail_at)?;
    let tail = TailIndex::decode(tail_bytes).map_err(ReadError::Tail)?;

    let catalog_len = u64::from(tail.catalog_len);
    let bad_len = || ReadError::BadCatalogLen {
        catalog_len: tail.catalog_len,
    };
    if catalog_len > MAX_CATALOG_BYTES {
        return Err(bad_len());
    }
    let catalog_at = tail_at.checked_sub(catalog_len).ok_or_else(bad_len)?;
    if catalog_at < MAGIC.len() as u64 {
        return Err(bad_len());
    }

    let catalog_len = usize::try_from(catalog_len).map_err(|_overflow| bad_len())?;
    let entries_bytes = admitted_entries_bytes(catalog_len).map_err(ReadError::Catalog)?;
    let meta_at = catalog_at
        .checked_add(u64::try_from(entries_bytes).map_err(|_overflow| bad_len())?)
        .ok_or_else(bad_len)?;
    let mut meta = [0_u8; META_LEN];
    reader.read_exact_at(&mut meta, meta_at)?;

    let entry_count = entries_bytes / ENTRY_LEN;
    let stored_count = u32_at(&meta, META_ENTRY_COUNT_AT);
    let derived_count = u32::try_from(entry_count).map_err(|_overflow| {
        ReadError::Catalog(DecodeError::BadCatalogLen {
            actual: catalog_len,
        })
    })?;
    if stored_count != derived_count {
        return Err(ReadError::Catalog(DecodeError::EntryCountMismatch {
            stored: stored_count,
            derived: derived_count,
        }));
    }

    let max_decoded_bytes = usize::try_from(MAX_CATALOG_BYTES).map_err(|_overflow| bad_len())?;
    entry_count
        .checked_mul(size_of::<Entry>())
        .filter(|&bytes| bytes <= max_decoded_bytes)
        .ok_or_else(bad_len)?;
    let mut entries = Vec::new();
    if entries.try_reserve_exact(entry_count).is_err() {
        return Err(bad_len());
    }

    let mut catalog_crc = Crc32c::new();
    let mut scratch = vec![0_u8; entries_bytes.min(CATALOG_READ_CHUNK_BYTES)].into_boxed_slice();
    let mut consumed = 0_usize;
    while consumed < entries_bytes {
        let chunk_len = (entries_bytes - consumed).min(scratch.len());
        let offset = catalog_at
            .checked_add(u64::try_from(consumed).map_err(|_overflow| bad_len())?)
            .ok_or_else(bad_len)?;
        let chunk = &mut scratch[..chunk_len];
        reader.read_exact_at(chunk, offset)?;
        catalog_crc.update(chunk);
        entries.extend(chunk.chunks_exact(ENTRY_LEN).map(decode_catalog_entry));
        consumed = consumed.checked_add(chunk_len).ok_or_else(bad_len)?;
    }

    catalog_crc.update(&meta[..META_CRC_AT]);
    catalog_crc.update(&[0_u8; 4]);
    catalog_crc.update(&meta[META_CRC_AT + 4..]);
    let stored_crc = u32_at(&meta, META_CRC_AT);
    let computed_crc = catalog_crc.finalize();
    if stored_crc != computed_crc {
        return Err(ReadError::Catalog(DecodeError::BadCrc {
            stored: stored_crc,
            computed: computed_crc,
        }));
    }

    let mut magic = [0_u8; MAGIC.len()];
    reader.read_exact_at(&mut magic, 0)?;
    if magic != MAGIC {
        return Err(ReadError::BadMagic { actual: magic });
    }
    let format_version = u32_at(&meta, META_FORMAT_VERSION_AT);
    if format_version != FORMAT_VERSION {
        return Err(ReadError::UnsupportedFormat {
            version: format_version,
        });
    }
    let catalog = Catalog {
        entries,
        min_ts: i64_at(&meta, 0),
        max_ts: i64_at(&meta, 8),
        format_version,
        window_count: u32_at(&meta, META_WINDOW_COUNT_AT),
    };
    validate_catalog_layout(&catalog, catalog_at).map_err(ReadError::Layout)?;
    let source_descriptor = crate::SourceDescriptor::from_catalog(&catalog);
    Ok(OpenedCatalog {
        catalog,
        source_descriptor,
    })
}

const fn admitted_entries_bytes(catalog_len: usize) -> Result<usize, DecodeError> {
    let Some(entries_bytes) = catalog_len.checked_sub(META_LEN) else {
        return Err(DecodeError::BadCatalogLen {
            actual: catalog_len,
        });
    };
    if !entries_bytes.is_multiple_of(ENTRY_LEN) {
        return Err(DecodeError::BadCatalogLen {
            actual: catalog_len,
        });
    }
    Ok(entries_bytes)
}

fn decode_catalog_entry(bytes: &[u8]) -> Entry {
    Entry {
        type_id: u32_at(bytes, 0),
        flags: u32_at(bytes, 4),
        offset: u64_at(bytes, 8),
        len: u64_at(bytes, 16),
        rows: u32_at(bytes, 24),
        crc32c: u32_at(bytes, 28),
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("fixed catalog field"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("fixed catalog field"))
}

fn i64_at(bytes: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(bytes[at..at + 8].try_into().expect("fixed catalog field"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use kronika_format::{DictLimits, PartMeta, ReadAt, SectionInput, build_part};
    use kronika_registry::os_loadavg::OsLoadavg;
    use kronika_registry::{Section, Ts};
    use kronika_writer::{Interner, dict};

    use super::*;

    const OVERSIZED_CATALOG_LEN: u32 = 64 * 1024 * 1024 + 1;

    #[derive(Default)]
    struct ReadObservation {
        calls: Cell<usize>,
        max_len: Cell<usize>,
    }

    struct CountingReader<'a> {
        bytes: &'a [u8],
        observation: Rc<ReadObservation>,
    }

    impl ReadAt for CountingReader<'_> {
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
            self.observation
                .calls
                .set(self.observation.calls.get().saturating_add(1));
            self.observation
                .max_len
                .set(self.observation.max_len.get().max(buf.len()));
            self.bytes.read_exact_at(buf, offset)
        }

        fn byte_len(&self) -> std::io::Result<u64> {
            Ok(self.bytes.len() as u64)
        }
    }

    struct OversizedCatalogReader {
        len: u64,
        observation: Rc<ReadObservation>,
    }

    impl ReadAt for OversizedCatalogReader {
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
            self.observation
                .calls
                .set(self.observation.calls.get().saturating_add(1));
            self.observation
                .max_len
                .set(self.observation.max_len.get().max(buf.len()));
            assert_eq!(offset, self.len - TAIL_INDEX_LEN as u64);
            assert_eq!(buf.len(), TAIL_INDEX_LEN);
            buf.copy_from_slice(
                &TailIndex {
                    catalog_len: OVERSIZED_CATALOG_LEN,
                }
                .encode(),
            );
            Ok(())
        }

        fn byte_len(&self) -> std::io::Result<u64> {
            Ok(self.len)
        }
    }

    struct ZeroSizedReader;

    impl ReadAt for ZeroSizedReader {
        fn read_exact_at(&self, _buf: &mut [u8], _offset: u64) -> std::io::Result<()> {
            unreachable!("structural size test never reads")
        }

        fn byte_len(&self) -> std::io::Result<u64> {
            unreachable!("structural size test never reads")
        }
    }

    fn data_body() -> Vec<u8> {
        OsLoadavg::encode(&[OsLoadavg {
            ts: Ts(5),
            load1: 0.15,
            load5: 0.10,
            load15: 0.05,
            running: 2,
            total: 345,
            scope: 0,
        }])
        .expect("encode data section")
    }

    /// Build a minimal, structurally valid PGM part with one real section.
    fn a_part() -> Vec<u8> {
        let body = data_body();
        build_part(
            &[SectionInput {
                type_id: OsLoadavg::CONTRACT.type_id.get(),
                rows: 1,
                body: &body,
            }],
            PartMeta {
                min_ts: 5,
                max_ts: 6,
            },
        )
    }

    fn large_catalog_part(entry_count: usize) -> Vec<u8> {
        let sections = (0..entry_count)
            .map(|index| SectionInput {
                type_id: 2_000_000 + u32::try_from(index).expect("test entry count fits u32"),
                rows: 1,
                body: b"x",
            })
            .collect::<Vec<_>>();
        build_part(
            &sections,
            PartMeta {
                min_ts: 5,
                max_ts: 6,
            },
        )
    }

    #[test]
    fn same_bytes_decode_via_file_and_memory() {
        let bytes = a_part();

        // In-memory path.
        let mem = PgmUnit::open(bytes.as_slice()).expect("open in-memory");

        // File path.
        let f = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(f.path(), &bytes).expect("write to file");
        let file = PgmUnit::open(std::fs::File::open(f.path()).expect("open file"))
            .expect("open PgmUnit from file");

        assert_eq!(mem.catalog(), file.catalog());
        assert_eq!(mem.source_descriptor(), file.source_descriptor());
        assert_eq!(mem.source_file_len(), bytes.len() as u64);
        assert_eq!(
            mem.source_descriptor(),
            crate::SourceDescriptor::from_catalog(mem.catalog())
        );

        let entry = &mem.catalog().entries[0];
        mem.scrub_overview_section(0)
            .expect("streaming scrub accepts the valid body");
        assert_eq!(
            mem.body_read_stats(),
            PgmBodyReadStats {
                read_calls: 1,
                stored_bytes_read: entry.len,
            }
        );
        let overview = mem.read_overview_section(0).expect("overview body");
        assert_eq!(overview.catalog_ordinal(), 0);
        assert_eq!(overview.body().len() as u64, entry.len);
        assert_eq!(
            overview.descriptor().section_body_id,
            Some(crate::section_body_id(entry.type_id, overview.body()))
        );
        assert!(matches!(
            mem.read_overview_section(1),
            Err(ReadError::CatalogOrdinalOutOfRange { ordinal: 1 })
        ));
        assert!(matches!(
            mem.scrub_overview_section(1),
            Err(ReadError::CatalogOrdinalOutOfRange { ordinal: 1 })
        ));
        assert_eq!(
            mem.decode(entry).expect("decode mem").stats.rows,
            file.decode(entry).expect("decode file").stats.rows,
        );
    }

    #[test]
    fn corrupt_overview_body_fails_crc_before_decode() {
        let mut bytes = a_part();
        let body_offset = {
            let unit = PgmUnit::open(bytes.as_slice()).expect("open pristine part");
            usize::try_from(unit.catalog().entries[0].offset).expect("body offset fits usize")
        };
        bytes[body_offset] ^= 0xFF;

        let unit = PgmUnit::open(bytes.as_slice()).expect("body corruption leaves metadata valid");
        assert!(matches!(
            unit.scrub_overview_section(0),
            Err(ReadError::Codec(_))
        ));
        assert_eq!(
            unit.body_read_stats(),
            PgmBodyReadStats {
                read_calls: 1,
                stored_bytes_read: unit.catalog().entries[0].len,
            },
            "the scrub verifies the complete stored body with bounded scratch space"
        );
        assert!(matches!(
            unit.read_overview_section(0),
            Err(ReadError::Codec(_))
        ));
        assert!(matches!(
            unit.decode(&unit.catalog().entries[0]),
            Err(ReadError::Codec(_))
        ));
    }

    #[test]
    fn open_rejects_duplicate_physical_sections() {
        let body = data_body();
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: OsLoadavg::CONTRACT.type_id.get(),
                    rows: 1,
                    body: &body,
                },
                SectionInput {
                    type_id: OsLoadavg::CONTRACT.type_id.get(),
                    rows: 1,
                    body: &body,
                },
            ],
            PartMeta {
                min_ts: 5,
                max_ts: 6,
            },
        );

        assert!(matches!(
            PgmUnit::open(bytes.as_slice()),
            Err(ReadError::Layout(_))
        ));
    }

    #[test]
    fn streaming_catalog_matches_monolithic_decode_across_chunk_boundary() {
        let bytes = large_catalog_part(CATALOG_READ_CHUNK_BYTES / ENTRY_LEN + 1);
        let tail_at = bytes.len() - TAIL_INDEX_LEN;
        let tail_bytes: [u8; TAIL_INDEX_LEN] = bytes[tail_at..].try_into().expect("tail bytes");
        let tail = TailIndex::decode(tail_bytes).expect("tail");
        let catalog_at =
            tail_at - usize::try_from(tail.catalog_len).expect("catalog length fits usize");
        let expected_catalog =
            Catalog::decode(&bytes[catalog_at..tail_at]).expect("monolithic catalog decode");
        let expected_descriptor = crate::SourceDescriptor::from_catalog(&expected_catalog);
        let observation = Rc::new(ReadObservation::default());

        let unit = PgmUnit::open(CountingReader {
            bytes: &bytes,
            observation: Rc::clone(&observation),
        })
        .expect("streaming open");

        assert_eq!(unit.catalog(), &expected_catalog);
        assert_eq!(unit.source_descriptor(), expected_descriptor);
        assert!(
            observation.max_len.get() <= CATALOG_READ_CHUNK_BYTES,
            "largest positional read was {} bytes",
            observation.max_len.get()
        );
        assert_eq!(
            observation.calls.get(),
            5,
            "tail, meta, two entry chunks, and leading magic"
        );
    }

    #[test]
    fn corrupt_catalog_crc_is_rejected_after_streaming_read() {
        let mut bytes = large_catalog_part(CATALOG_READ_CHUNK_BYTES / ENTRY_LEN + 1);
        let tail_at = bytes.len() - TAIL_INDEX_LEN;
        let tail_bytes: [u8; TAIL_INDEX_LEN] = bytes[tail_at..].try_into().expect("tail bytes");
        let tail = TailIndex::decode(tail_bytes).expect("tail");
        let catalog_at =
            tail_at - usize::try_from(tail.catalog_len).expect("catalog length fits usize");
        bytes[catalog_at] ^= 0xFF;

        assert!(matches!(
            PgmUnit::open(bytes.as_slice()),
            Err(ReadError::Catalog(DecodeError::BadCrc { .. }))
        ));
    }

    #[test]
    fn dictionary_rejects_cross_placement_overlap() {
        let limits = DictLimits::new(4_096, 1 << 20).expect("dictionary limits");
        let mut strings = Interner::new(limits);
        let str_id = strings.intern(b"same-id").expect("string value");
        let strings = dict::encode(strings.window()).expect("strings section");
        let mut blobs = Interner::new(limits);
        assert_eq!(blobs.intern_blob(b"same-id").expect("blob value"), str_id);
        let blobs = dict::encode(blobs.window()).expect("blobs section");
        let sections = [
            SectionInput {
                type_id: strings[0].type_id,
                rows: strings[0].rows,
                body: &strings[0].body,
            },
            SectionInput {
                type_id: blobs[0].type_id,
                rows: blobs[0].rows,
                body: &blobs[0].body,
            },
        ];
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts: i64::MAX,
                max_ts: i64::MIN,
            },
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("canonical catalog");
        assert!(matches!(
            unit.dictionary(),
            Err(ReadError::DictionaryConflict { str_id: got }) if got == str_id.get()
        ));
    }

    #[test]
    fn all_data_decode_paths_reject_catalog_row_mismatch() {
        let body = data_body();
        let bytes = build_part(
            &[SectionInput {
                type_id: OsLoadavg::CONTRACT.type_id.get(),
                rows: 2,
                body: &body,
            }],
            PartMeta {
                min_ts: 5,
                max_ts: 6,
            },
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open mismatched catalog");
        let entry = &unit.catalog().entries[0];
        let mismatch = |result: Result<(), ReadError>| {
            assert!(matches!(
                result,
                Err(ReadError::CatalogRowCountMismatch {
                    type_id,
                    declared: 2,
                    decoded: 1,
                }) if type_id == OsLoadavg::CONTRACT.type_id.get()
            ));
        };

        mismatch(unit.decode(entry).map(|_| ()));
        mismatch(unit.decode_rows(entry).map(|_| ()));
        mismatch(unit.decode_overview_rows(0).map(|_| ()));
    }

    #[test]
    fn dictionary_rejects_catalog_row_mismatch() {
        let limits = DictLimits::new(4_096, 1 << 20).expect("dictionary limits");
        let mut interner = Interner::new(limits);
        interner.intern(b"one").expect("intern value");
        let encoded = dict::encode(interner.window()).expect("dictionary section");
        let section = &encoded[0];
        let bytes = build_part(
            &[SectionInput {
                type_id: section.type_id,
                rows: section.rows + 1,
                body: &section.body,
            }],
            PartMeta {
                min_ts: i64::MAX,
                max_ts: i64::MIN,
            },
        );
        let unit = PgmUnit::open(bytes.as_slice()).expect("open mismatched dictionary catalog");

        assert!(matches!(
            unit.dictionary(),
            Err(ReadError::CatalogRowCountMismatch {
                type_id,
                declared: 2,
                decoded: 1,
            }) if type_id == section.type_id
        ));
    }

    #[test]
    fn catalog_admission_rejects_over_64_mib_before_large_read_or_allocation() {
        let observation = Rc::new(ReadObservation::default());
        let reader = OversizedCatalogReader {
            len: MAX_CATALOG_BYTES + 1 + TAIL_INDEX_LEN as u64 + MAGIC.len() as u64,
            observation: Rc::clone(&observation),
        };

        assert!(matches!(
            PgmUnit::open(reader),
            Err(ReadError::BadCatalogLen { catalog_len })
                if catalog_len == OVERSIZED_CATALOG_LEN
        ));
        assert_eq!(observation.calls.get(), 1, "only the tail is read");
        assert_eq!(observation.max_len.get(), TAIL_INDEX_LEN);
    }

    #[test]
    fn open_state_retains_descriptor_instead_of_raw_catalog() {
        let fixed_state = size_of::<Catalog>()
            + size_of::<u64>()
            + size_of::<crate::SourceDescriptor>()
            + 2 * size_of::<AtomicU64>();
        assert_eq!(size_of::<PgmUnit<ZeroSizedReader>>(), fixed_state);
        assert_eq!(size_of::<Entry>(), ENTRY_LEN);
        assert_eq!(CATALOG_READ_CHUNK_BYTES, 64 * 1024);

        let max_catalog_bytes =
            usize::try_from(MAX_CATALOG_BYTES).expect("64 MiB fits every supported target");
        let max_valid_len = (max_catalog_bytes - META_LEN) / ENTRY_LEN * ENTRY_LEN + META_LEN;
        let max_entries =
            admitted_entries_bytes(max_valid_len).expect("largest aligned catalog") / ENTRY_LEN;
        assert!(
            max_entries
                .checked_mul(size_of::<Entry>())
                .is_some_and(|bytes| bytes <= max_catalog_bytes)
        );
    }
}
