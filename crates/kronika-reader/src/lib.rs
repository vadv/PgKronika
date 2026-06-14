//! Segment read path.
//!
//! [`Segment::open`] reads a sealed `.pgm` file's end catalog; [`Segment::decode`]
//! reads one section body by its catalog range and decodes it. Reads are
//! positional and bounded — the catalog (the file tail) and one section at a
//! time — so opening and reading a segment never loads the whole file
//! (segment-format.md, "Reading from S3").
//!
//! This is where the registry's CRC trust boundary lands in production: the
//! reader passes [`kronika_format::crc32c`] into
//! [`VerifiedSection::verify`](kronika_registry::VerifiedSection::verify), so the
//! section bytes are checked against the catalog before the Parquet parser sees
//! them, and the registry stays free of a `kronika-format` dependency
//! (kronika-registry README, "Section Trait").

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use kronika_format::{Catalog, DecodeError, Entry, MAGIC, TAIL_INDEX_LEN, TailIndex, crc32c};
use kronika_registry::{
    Bytes, CodecError, DecodedSection, MAX_SECTION_BYTES, VerifiedSection, decode_any,
};

/// Upper bound on the end-catalog block, checked before it is read.
///
/// The catalog is the segment tail (target ~1 MiB; segment-format.md), so a far
/// larger `catalog_len` is a corrupt tail index. The bound stops a bad length
/// from allocating before the catalog CRC can reject it.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;

/// A sealed segment opened for reading.
///
/// Holds the open file and the decoded end catalog; section bodies are read on
/// demand by [`decode`](Segment::decode).
#[derive(Debug)]
pub struct Segment {
    file: File,
    catalog: Catalog,
}

/// Why a segment could not be opened or a section decoded.
#[derive(Debug)]
pub enum ReadError {
    /// A filesystem read failed.
    Io(std::io::Error),
    /// The file is shorter than a tail index.
    TooSmall {
        /// The file length found.
        len: u64,
    },
    /// The tail index did not decode.
    Tail(DecodeError),
    /// `catalog_len` does not fit between the magic and the tail index, or
    /// exceeds [`MAX_CATALOG_BYTES`].
    BadCatalogLen {
        /// `catalog_len` from the tail index.
        catalog_len: u32,
    },
    /// The catalog block did not decode (length, count, or CRC).
    Catalog(DecodeError),
    /// A catalog entry's length is above [`MAX_SECTION_BYTES`].
    SectionTooLarge {
        /// The section length claimed by the catalog.
        len: u64,
    },
    /// A section failed CRC verification or decoding.
    Codec(CodecError),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "segment io: {err}"),
            Self::TooSmall { len } => write!(f, "file of {len} bytes is too small for a segment"),
            Self::Tail(err) => write!(f, "segment tail index: {err}"),
            Self::BadCatalogLen { catalog_len } => {
                write!(f, "segment catalog_len {catalog_len} does not fit the file")
            }
            Self::Catalog(err) => write!(f, "segment catalog: {err}"),
            Self::SectionTooLarge { len } => write!(f, "section of {len} bytes is above the cap"),
            Self::Codec(err) => write!(f, "section decode: {err}"),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Tail(err) | Self::Catalog(err) => Some(err),
            Self::Codec(err) => Some(err),
            Self::TooSmall { .. } | Self::BadCatalogLen { .. } | Self::SectionTooLarge { .. } => {
                None
            }
        }
    }
}

impl From<std::io::Error> for ReadError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl Segment {
    /// Open a sealed segment and read its end catalog.
    ///
    /// Reads only the tail index and the catalog block (positional, bounded by
    /// [`MAX_CATALOG_BYTES`]); section bodies are read later by
    /// [`decode`](Segment::decode).
    ///
    /// # Errors
    ///
    /// [`ReadError::TooSmall`], [`ReadError::Tail`], [`ReadError::BadCatalogLen`],
    /// or [`ReadError::Catalog`] if the file is not a valid segment;
    /// [`ReadError::Io`] on a read failure.
    pub fn open(path: &Path) -> Result<Self, ReadError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let catalog = read_catalog(&file, len)?;
        Ok(Self { file, catalog })
    }

    /// The segment's end catalog.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Read and decode one section by its catalog `entry`.
    ///
    /// Reads the body at `entry.offset` (bounded by [`MAX_SECTION_BYTES`]),
    /// verifies it against `entry.crc32c` with the format checksum, then decodes
    /// it through the registry. `entry` must come from this segment's
    /// [`catalog`](Segment::catalog).
    ///
    /// # Errors
    ///
    /// [`ReadError::SectionTooLarge`] if the entry length is over the cap;
    /// [`ReadError::Codec`] on a CRC mismatch, an unknown `type_id`, or a decode
    /// failure; [`ReadError::Io`] on a read failure.
    pub fn decode(&self, entry: &Entry) -> Result<DecodedSection, ReadError> {
        let len = usize::try_from(entry.len)
            .ok()
            .filter(|&len| len <= MAX_SECTION_BYTES)
            .ok_or(ReadError::SectionTooLarge { len: entry.len })?;

        let mut body = vec![0_u8; len];
        self.file.read_exact_at(&mut body, entry.offset)?;

        let verified = VerifiedSection::verify(Bytes::from(body), entry.crc32c, crc32c)
            .map_err(ReadError::Codec)?;
        decode_any(entry.type_id, verified).map_err(ReadError::Codec)
    }
}

/// Read and decode the end catalog from the file's tail.
fn read_catalog(file: &File, len: u64) -> Result<Catalog, ReadError> {
    let tail_at = len
        .checked_sub(TAIL_INDEX_LEN as u64)
        .ok_or(ReadError::TooSmall { len })?;
    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    file.read_exact_at(&mut tail_bytes, tail_at)?;
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
    file.read_exact_at(&mut buf, catalog_at)?;
    Catalog::decode(&buf).map_err(ReadError::Catalog)
}

#[cfg(test)]
mod tests {
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;

    use super::{ReadError, Segment};

    /// Write a one-section segment to a temp file. A chartless segment is
    /// structurally a PGM part, so `build_part` writes a valid one.
    fn segment_with(
        body: &[u8],
        type_id: u32,
        rows: u32,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let bytes = build_part(
            &[SectionInput {
                type_id,
                rows,
                body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("143000.pgm");
        std::fs::write(&path, &bytes).expect("write segment");
        (dir, path)
    }

    #[test]
    fn opens_a_segment_and_decodes_a_section() {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode empty section");
        let (_dir, path) = segment_with(&body, 1_006_001, 0);

        let segment = Segment::open(&path).expect("open");
        assert_eq!(segment.catalog().source_id, 7);
        assert_eq!(segment.catalog().entries.len(), 1);

        let entry = segment.catalog().entries[0];
        let decoded = segment.decode(&entry).expect("decode");
        assert_eq!(decoded.stats.type_id, 1_006_001);
        assert_eq!(decoded.stats.rows, 0);
    }

    #[test]
    fn a_corrupted_section_body_fails_the_crc_check() {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode");
        let (_dir, path) = segment_with(&body, 1_006_001, 0);

        // Flip a byte inside the section body, just past the segment magic.
        let mut bytes = std::fs::read(&path).expect("read");
        bytes[6] ^= 0x01;
        std::fs::write(&path, &bytes).expect("rewrite");

        let segment = Segment::open(&path).expect("the catalog is intact");
        let entry = segment.catalog().entries[0];
        // The injected crc32c rejects the tampered body before Parquet sees it.
        assert!(matches!(segment.decode(&entry), Err(ReadError::Codec(_))));
    }

    #[test]
    fn a_too_small_file_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tiny.pgm");
        std::fs::write(&path, [0_u8; 4]).expect("write");
        assert!(matches!(
            Segment::open(&path),
            Err(ReadError::TooSmall { len: 4 })
        ));
    }
}
