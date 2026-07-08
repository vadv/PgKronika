//! Decoder for one PGM container over any `ReadAt` source.
//!
//! The same code handles sealed segment files (`File`) and in-memory journal
//! parts (`&[u8]`).

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;

use kronika_format::{Catalog, Entry, FORMAT_VERSION, MAGIC, TAIL_INDEX_LEN, TailIndex, crc32c};
use kronika_registry::{
    Bytes, CodecError, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, DecodedSection, MAX_SECTION_BYTES,
    VerifiedSection, decode_any,
};

use crate::{Dictionary, ReadError, Stored, decode_dictionary};

/// Upper bound on the catalog block, checked before allocation.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;

/// A PGM container opened for reading over any [`ReadAt`] source.
///
/// Works for sealed segment files (`File`) and in-memory journal parts (`&[u8]`).
#[derive(Debug)]
pub struct PgmUnit<R: kronika_format::ReadAt> {
    reader: R,
    catalog: Catalog,
}

impl<R: kronika_format::ReadAt> PgmUnit<R> {
    /// Open a PGM container and read its end catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] on I/O errors or invalid container framing.
    pub fn open(reader: R) -> Result<Self, ReadError> {
        let len = reader.byte_len()?;
        let catalog = read_catalog(&reader, len)?;
        Ok(Self { reader, catalog })
    }

    /// The container's end catalog.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
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
        decode_any(entry.type_id, self.verified_body(entry)?).map_err(ReadError::Codec)
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
            for (str_id, value) in
                decode_dictionary(body, entry.type_id).map_err(ReadError::Codec)?
            {
                match by_id.entry(str_id) {
                    MapEntry::Vacant(slot) => {
                        slot.insert(value);
                    }
                    // A later part may move the same id from `dict.strings` to
                    // `dict.blobs`; the blob carries truncation metadata, so it wins.
                    MapEntry::Occupied(mut slot) => {
                        if matches!(value, Stored::Blob { .. }) {
                            slot.insert(value);
                        }
                    }
                }
            }
        }
        Ok(Dictionary { by_id })
    }

    /// Read and CRC-check a section body.
    fn verified_body(&self, entry: &Entry) -> Result<VerifiedSection, ReadError> {
        let len = usize::try_from(entry.len)
            .ok()
            .filter(|&len| len <= MAX_SECTION_BYTES)
            .ok_or(ReadError::SectionTooLarge { len: entry.len })?;
        let mut body = vec![0_u8; len];
        self.reader.read_exact_at(&mut body, entry.offset)?;
        VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c).map_err(|source| {
            ReadError::Codec(CodecError::Section {
                type_id: entry.type_id,
                bytes_in: len,
                source: Box::new(source),
            })
        })
    }
}

/// Read and decode the end catalog from a `ReadAt` source.
pub(crate) fn read_catalog<R: kronika_format::ReadAt>(
    reader: &R,
    len: u64,
) -> Result<Catalog, ReadError> {
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

    let mut buf = vec![0_u8; tail.catalog_len as usize];
    reader.read_exact_at(&mut buf, catalog_at)?;
    let catalog = Catalog::decode(&buf).map_err(ReadError::Catalog)?;

    let mut magic = [0_u8; MAGIC.len()];
    reader.read_exact_at(&mut magic, 0)?;
    if magic != MAGIC {
        return Err(ReadError::BadMagic { actual: magic });
    }
    if catalog.format_version != FORMAT_VERSION {
        return Err(ReadError::UnsupportedFormat {
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
            return Err(ReadError::SectionOutOfBounds {
                type_id: entry.type_id,
            });
        }
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use super::*;

    /// Build a minimal, structurally valid PGM part with one real section.
    fn a_part() -> Vec<u8> {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode empty section");
        build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: 5,
                max_ts: 6,
                source_id: 1,
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

        let entry = &mem.catalog().entries[0];
        assert_eq!(
            mem.decode(entry).expect("decode mem").stats.rows,
            file.decode(entry).expect("decode file").stats.rows,
        );
    }

    #[test]
    fn corrupt_body_fails_crc_before_decode() {
        let mut bytes = a_part();
        let off = bytes.len() / 2;
        bytes[off] ^= 0xFF;
        let unit = PgmUnit::open(bytes.as_slice());
        // open may succeed (catalog at end, body is in the middle);
        // decode must fail on CRC.
        if let Ok(u) = unit {
            let entry = &u.catalog().entries[0];
            assert!(u.decode(entry).is_err(), "decode must fail on corrupt body");
        }
        // if open itself failed, that is also acceptable (catalog was corrupted)
    }
}
