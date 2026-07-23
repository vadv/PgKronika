//! The `PGKOVF` container: fixed header, block directory, and admission.
//!
//! The header is exactly 160 bytes and the directory entry exactly 64, both
//! serialized field by field in little-endian order — never through native
//! struct layout. Admission walks the format contract's ordered checks: header
//! magic and CRC, checked directory arithmetic, directory CRC, expected
//! identity, canonical layout with non-overlapping contiguous extents, per-block
//! bounds, and per-block CRC. Any failure yields a typed [`CacheReadError`] so
//! an untrusted file falls back to a raw rebuild instead of panicking.
//!
//! Physical admission stops at verified block bytes. Turning those bytes into
//! logical facts is the block codec's job, because some blocks — observations —
//! need the segment lineage as context.

use kronika_analytics::overview::{
    CONTAINER_VERSION, EXTRACTOR_SEMANTICS_VERSION, FACT_SCHEMA_VERSION, REGISTRY_CONTRACT_VERSION,
};
use kronika_format::crc32c;

use super::block::{
    BlockCodec, BlockError, BlockFlags, BlockKind, CounterSamplesBlock, EntityStatesBlock,
    GaugeSamplesBlock, LossCoverageBlock, ResetMarkersBlock, SourceManifestBlock, StringTableBlock,
};
use super::bytes::{ByteReader, ByteWriter};
use super::limits::Bounds;
use super::observations::EventObservationsBlock;

/// The container magic, `PGKOVF` padded to eight bytes.
const MAGIC: [u8; 8] = *b"PGKOVF\0\0";
/// The fixed header length.
const HEADER_LEN: usize = 160;
/// The fixed header length as the header's own `u16` field.
const HEADER_LEN_U16: u16 = 160;
/// The fixed header length as a `u64` offset.
const HEADER_LEN_U64: u64 = 160;
/// The fixed directory-entry length as the header's own `u16` field.
const DIRECTORY_ENTRY_LEN_U16: u16 = 64;
/// The fixed directory-entry length as a `u64` stride.
const DIRECTORY_ENTRY_LEN_U64: u64 = 64;
/// The only file kind in version 1: sealed segment facts.
const FILE_KIND_SEGMENT_FACTS: u16 = 1;
/// The only descriptor kind in version 1: the PGM catalog descriptor.
const DESCRIPTOR_KIND_CATALOG: u16 = 1;
/// The block-schema version written for every block in version 1.
const BLOCK_SCHEMA_VERSION: u16 = 1;

/// The identity a reader expects a fact file to carry.
///
/// Admission compares every field against the header. A source-scope,
/// descriptor, source-ID, range, format, or length mismatch is a wrong-source
/// error; a fact-schema, extractor, or registry mismatch is incompatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderIdentity {
    /// Logical fact-shape version.
    pub fact_schema_version: u32,
    /// PGM-to-fact extraction and normalization version.
    pub extractor_semantics_version: u32,
    /// Supported type and layout contract version.
    pub registry_contract_version: u32,
    /// PGM container version.
    pub source_format_version: u32,
    /// PGM source ID provenance.
    pub pgm_source_id: u64,
    /// Inclusive minimum PGM timestamp.
    pub source_min_ts_us: i64,
    /// Inclusive maximum PGM timestamp.
    pub source_max_ts_us: i64,
    /// Exact PGM file length.
    pub source_file_len: u64,
    /// Dataset and deployment scope.
    pub source_scope_id: [u8; 32],
    /// Content-bound PGM descriptor.
    pub source_descriptor: [u8; 32],
}

impl HeaderIdentity {
    /// Builds an identity whose version axes come from the analytics contract.
    #[must_use]
    pub const fn from_current_contract(
        source_format_version: u32,
        pgm_source_id: u64,
        source_min_ts_us: i64,
        source_max_ts_us: i64,
        source_file_len: u64,
        source_scope_id: [u8; 32],
        source_descriptor: [u8; 32],
    ) -> Self {
        Self {
            fact_schema_version: FACT_SCHEMA_VERSION,
            extractor_semantics_version: EXTRACTOR_SEMANTICS_VERSION,
            registry_contract_version: REGISTRY_CONTRACT_VERSION,
            source_format_version,
            pgm_source_id,
            source_min_ts_us,
            source_max_ts_us,
            source_file_len,
            source_scope_id,
            source_descriptor,
        }
    }
}

/// The decoded fixed header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactFileHeader {
    /// The identity fields the header carries.
    pub identity: HeaderIdentity,
    /// Offset of the block directory, always 160 in version 1.
    pub directory_offset: u64,
    /// Number of directory entries.
    pub directory_count: u32,
    /// Exact fact-file length.
    pub file_len: u64,
}

/// One decoded 64-byte block directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDirectoryEntry {
    /// Stable block-kind code.
    pub block_kind: u32,
    /// Block schema version.
    pub block_schema_version: u16,
    /// Parsed block flags.
    pub flags: BlockFlags,
    /// Stable factor or source ID, or zero for segment-wide.
    pub logical_id: u32,
    /// Absolute offset of the stored block bytes.
    pub offset: u64,
    /// Stored block length, bytes.
    pub stored_len: u64,
    /// Decoded block length, bytes.
    pub decoded_len: u64,
    /// Logical item count.
    pub item_count: u32,
    /// CRC32C of the stored block bytes.
    pub block_crc32c: u32,
    /// Inclusive minimum item timestamp.
    pub min_ts_us: i64,
    /// Inclusive maximum item timestamp.
    pub max_ts_us: i64,
}

/// Why a fact file could not be admitted.
#[derive(Debug)]
pub enum CacheReadError {
    /// The fact file does not exist.
    Missing,
    /// The framing, versions, flags, or codec are from an unreadable contract.
    Incompatible,
    /// A CRC, structure, order, or logical invariant failed.
    Corrupt,
    /// The header identity does not match the expected source.
    WrongSource,
    /// A safety bound was exceeded.
    Oversized,
    /// A filesystem read failed.
    Io(std::io::Error),
}

impl std::fmt::Display for CacheReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "fact file is missing"),
            Self::Incompatible => write!(f, "fact file is from an unreadable contract"),
            Self::Corrupt => write!(f, "fact file is corrupt"),
            Self::WrongSource => write!(f, "fact file names a different source"),
            Self::Oversized => write!(f, "fact file exceeds a safety bound"),
            Self::Io(error) => write!(f, "fact file io: {error}"),
        }
    }
}

impl std::error::Error for CacheReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Missing
            | Self::Incompatible
            | Self::Corrupt
            | Self::WrongSource
            | Self::Oversized => None,
        }
    }
}

impl From<BlockError> for CacheReadError {
    fn from(value: BlockError) -> Self {
        match value {
            BlockError::AboveBound => Self::Oversized,
            BlockError::InvalidFlags => Self::Incompatible,
            BlockError::Truncated
            | BlockError::Malformed
            | BlockError::Unsorted
            | BlockError::Duplicate
            | BlockError::NonFiniteFloat
            | BlockError::TrailingBytes
            | BlockError::InvalidEnum
            | BlockError::Reconstruct => Self::Corrupt,
        }
    }
}

/// One canonical logical block, ready to be laid out in a fact file.
///
/// `EventFacts` carries no analytics type yet, so it is always an empty
/// required block; see the module gap report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockContent {
    /// The catalog inventory and source metadata.
    SourceManifest(Box<SourceManifestBlock>),
    /// Retained observations.
    EventObservations(Box<EventObservationsBlock>),
    /// Policy-neutral facts; empty until analytics defines the type.
    EventFacts,
    /// Coverage and completeness quality.
    LossCoverage(Box<LossCoverageBlock>),
    /// Gauge samples.
    GaugeSamples(Box<GaugeSamplesBlock>),
    /// Counter samples.
    CounterSamples(Box<CounterSamplesBlock>),
    /// Reset markers.
    ResetMarkers(Box<ResetMarkersBlock>),
    /// Entity snapshots.
    EntityStates(Box<EntityStatesBlock>),
    /// Retained text table.
    StringTable(Box<StringTableBlock>),
}

impl BlockContent {
    /// The block kind.
    #[must_use]
    pub fn kind(&self) -> BlockKind {
        use super::block::EncodableBlock;
        match self {
            Self::SourceManifest(block) => block.kind(),
            Self::EventObservations(block) => block.kind(),
            Self::EventFacts => BlockKind::EventFacts,
            Self::LossCoverage(block) => block.kind(),
            Self::GaugeSamples(block) => block.kind(),
            Self::CounterSamples(block) => block.kind(),
            Self::ResetMarkers(block) => block.kind(),
            Self::EntityStates(block) => block.kind(),
            Self::StringTable(block) => block.kind(),
        }
    }

    fn encoded(&self) -> (Vec<u8>, bool, u64, Option<(i64, i64)>) {
        match self {
            Self::SourceManifest(block) => descriptor_of(block.as_ref()),
            Self::EventObservations(block) => descriptor_of(block.as_ref()),
            Self::EventFacts => (Vec::new(), true, 0, None),
            Self::LossCoverage(block) => descriptor_of(block.as_ref()),
            Self::GaugeSamples(block) => descriptor_of(block.as_ref()),
            Self::CounterSamples(block) => descriptor_of(block.as_ref()),
            Self::ResetMarkers(block) => descriptor_of(block.as_ref()),
            Self::EntityStates(block) => descriptor_of(block.as_ref()),
            Self::StringTable(block) => descriptor_of(block.as_ref()),
        }
    }
}

fn descriptor_of<B: super::block::EncodableBlock>(
    block: &B,
) -> (Vec<u8>, bool, u64, Option<(i64, i64)>) {
    (
        block.encode(),
        block.canonically_sorted(),
        block.item_count(),
        block.time_range(),
    )
}

/// A physically admitted fact file: verified header, directory, and block bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFile {
    header: FactFileHeader,
    directory: Vec<BlockDirectoryEntry>,
    bodies: Vec<Vec<u8>>,
}

impl FactFile {
    /// The decoded header.
    #[must_use]
    pub const fn header(&self) -> &FactFileHeader {
        &self.header
    }

    /// The decoded directory.
    #[must_use]
    pub fn directory(&self) -> &[BlockDirectoryEntry] {
        &self.directory
    }

    /// The verified stored bytes of the first block of `kind`, if present.
    #[must_use]
    pub fn block_body(&self, kind: BlockKind) -> Option<&[u8]> {
        self.directory
            .iter()
            .position(|entry| entry.block_kind == kind.code())
            .map(|index| self.bodies[index].as_slice())
    }

    /// Builds a canonical fact file from a set of blocks.
    ///
    /// Every required baseline kind is inserted empty when absent, so the
    /// result always admits. Blocks are laid out contiguously in canonical
    /// kind order after the directory.
    ///
    /// # Errors
    /// Returns [`CacheReadError::Oversized`] when the block set, directory, or
    /// file would exceed a safety bound.
    pub fn build(
        identity: &HeaderIdentity,
        blocks: Vec<BlockContent>,
        bounds: &Bounds,
    ) -> Result<Vec<u8>, CacheReadError> {
        let mut present = [false; BlockKind::ALL.len()];
        let mut chosen: Vec<BlockContent> = Vec::with_capacity(BlockKind::ALL.len());
        for block in blocks {
            let index = baseline_index(block.kind());
            if present[index] {
                // One block per baseline kind in the version-1 builder.
                return Err(CacheReadError::Corrupt);
            }
            present[index] = true;
            chosen.push(block);
        }
        for (index, &was_present) in present.iter().enumerate() {
            if !was_present {
                chosen.push(empty_block(BlockKind::ALL[index]));
            }
        }
        chosen.sort_by_key(|block| block.kind().code());

        let count = u32::try_from(chosen.len()).map_err(|_error| CacheReadError::Oversized)?;
        if count == 0 || count > bounds.directory_entries {
            return Err(CacheReadError::Oversized);
        }
        let directory_bytes = u64::from(count)
            .checked_mul(DIRECTORY_ENTRY_LEN_U64)
            .ok_or(CacheReadError::Oversized)?;
        if directory_bytes > bounds.directory_bytes {
            return Err(CacheReadError::Oversized);
        }

        let mut offset = HEADER_LEN_U64 + directory_bytes;
        let mut directory = ByteWriter::new();
        let mut body_bytes = ByteWriter::new();
        for block in &chosen {
            let (body, sorted, item_count, range) = block.encoded();
            let stored_len = body.len() as u64;
            if stored_len > bounds.stored_block_len || stored_len > bounds.decoded_block_len {
                return Err(CacheReadError::Oversized);
            }
            let item_count =
                u32::try_from(item_count).map_err(|_error| CacheReadError::Oversized)?;
            let (min_ts, max_ts) =
                range.unwrap_or((identity.source_min_ts_us, identity.source_max_ts_us));
            let flags = BlockFlags {
                required_for_schema: true,
                canonically_sorted: sorted,
                codec: BlockCodec::None,
            };
            write_directory_entry(
                &mut directory,
                block.kind().code(),
                flags,
                offset,
                stored_len,
                item_count,
                crc32c(&body),
                min_ts,
                max_ts,
            );
            offset = offset
                .checked_add(stored_len)
                .ok_or(CacheReadError::Oversized)?;
            body_bytes.bytes(&body);
        }
        let file_len = offset;
        if file_len > bounds.fact_file_len {
            return Err(CacheReadError::Oversized);
        }

        let directory = directory.into_bytes();
        let directory_crc = crc32c(&directory);
        let header = encode_header(identity, count, file_len, directory_crc);

        let capacity = usize::try_from(file_len).map_err(|_error| CacheReadError::Oversized)?;
        let mut out = Vec::with_capacity(capacity);
        out.extend_from_slice(&header);
        out.extend_from_slice(&directory);
        out.extend_from_slice(&body_bytes.into_bytes());
        debug_assert_eq!(out.len(), capacity, "layout must match file_len");
        Ok(out)
    }

    /// Admits fact-file bytes against an expected identity.
    ///
    /// # Errors
    /// Returns [`CacheReadError`] for any framing, CRC, identity, bound, or
    /// layout violation.
    pub fn admit(
        bytes: &[u8],
        expected: &HeaderIdentity,
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        if bytes.len() as u64 > bounds.fact_file_len {
            return Err(CacheReadError::Oversized);
        }
        if bytes.len() < HEADER_LEN {
            return Err(CacheReadError::Corrupt);
        }
        let (header, directory_crc32c) = decode_header(&bytes[..HEADER_LEN])?;
        verify_identity(&header.identity, expected)?;
        if header.identity.source_min_ts_us > header.identity.source_max_ts_us {
            return Err(CacheReadError::Corrupt);
        }
        if header.file_len != bytes.len() as u64 {
            return Err(CacheReadError::Corrupt);
        }
        if header.directory_offset != HEADER_LEN_U64 {
            return Err(CacheReadError::Corrupt);
        }
        if header.directory_count == 0 || header.directory_count > bounds.directory_entries {
            return Err(CacheReadError::Oversized);
        }
        let directory_bytes = u64::from(header.directory_count)
            .checked_mul(DIRECTORY_ENTRY_LEN_U64)
            .ok_or(CacheReadError::Oversized)?;
        if directory_bytes > bounds.directory_bytes {
            return Err(CacheReadError::Oversized);
        }
        let directory_end = (HEADER_LEN_U64)
            .checked_add(directory_bytes)
            .ok_or(CacheReadError::Oversized)?;
        if directory_end > header.file_len {
            return Err(CacheReadError::Corrupt);
        }
        let directory_end_usize =
            usize::try_from(directory_end).map_err(|_error| CacheReadError::Corrupt)?;
        let directory_slice = &bytes[HEADER_LEN..directory_end_usize];
        if crc32c(directory_slice) != directory_crc32c {
            return Err(CacheReadError::Corrupt);
        }

        let directory = decode_directory(directory_slice, header.directory_count, bounds)?;
        verify_layout(&directory, directory_end, header.file_len, bounds)?;
        verify_required_baseline(&directory)?;

        let mut bodies = Vec::with_capacity(directory.len());
        for entry in &directory {
            let start = usize::try_from(entry.offset).map_err(|_error| CacheReadError::Corrupt)?;
            let stored =
                usize::try_from(entry.stored_len).map_err(|_error| CacheReadError::Corrupt)?;
            let end = start.checked_add(stored).ok_or(CacheReadError::Corrupt)?;
            let body = &bytes[start..end];
            if crc32c(body) != entry.block_crc32c {
                return Err(CacheReadError::Corrupt);
            }
            let flags = entry.flags;
            if flags.codec != BlockCodec::None {
                // Version 1 reads and writes only the identity codec; a
                // compressed body is not decodable by this build.
                return Err(CacheReadError::Incompatible);
            }
            if entry.stored_len != entry.decoded_len {
                return Err(CacheReadError::Corrupt);
            }
            bodies.push(body.to_vec());
        }

        Ok(Self {
            header,
            directory,
            bodies,
        })
    }
}

fn encode_header(
    identity: &HeaderIdentity,
    directory_count: u32,
    file_len: u64,
    directory_crc32c: u32,
) -> [u8; HEADER_LEN] {
    let mut writer = ByteWriter::new();
    writer.bytes(&MAGIC);
    writer.u16_le(CONTAINER_VERSION);
    writer.u16_le(HEADER_LEN_U16);
    writer.u16_le(FILE_KIND_SEGMENT_FACTS);
    writer.u16_le(0);
    writer.u32_le(identity.fact_schema_version);
    writer.u32_le(identity.extractor_semantics_version);
    writer.u32_le(identity.registry_contract_version);
    writer.u32_le(identity.source_format_version);
    writer.u64_le(identity.pgm_source_id);
    writer.i64_le(identity.source_min_ts_us);
    writer.i64_le(identity.source_max_ts_us);
    writer.u64_le(identity.source_file_len);
    writer.bytes(&identity.source_scope_id);
    writer.bytes(&identity.source_descriptor);
    writer.u64_le(HEADER_LEN_U64);
    writer.u32_le(directory_count);
    writer.u16_le(DIRECTORY_ENTRY_LEN_U16);
    writer.u16_le(DESCRIPTOR_KIND_CATALOG);
    writer.u64_le(file_len);
    writer.u32_le(directory_crc32c);
    writer.u32_le(0);
    let mut header = [0_u8; HEADER_LEN];
    header.copy_from_slice(&writer.into_bytes());
    let header_crc = crc32c(&header[..HEADER_LEN - 4]);
    header[HEADER_LEN - 4..].copy_from_slice(&header_crc.to_le_bytes());
    header
}

/// Parses and CRC-checks the fixed header, returning it and the directory CRC.
fn decode_header(bytes: &[u8]) -> Result<(FactFileHeader, u32), CacheReadError> {
    let mut reader = ByteReader::new(bytes);
    let magic: [u8; 8] = reader.array().map_err(|_error| CacheReadError::Corrupt)?;
    if magic != MAGIC {
        return Err(CacheReadError::Incompatible);
    }
    let corrupt = |_error: super::bytes::ByteError| CacheReadError::Corrupt;
    let container_version = reader.u16_le().map_err(corrupt)?;
    let header_len = reader.u16_le().map_err(corrupt)?;
    let file_kind = reader.u16_le().map_err(corrupt)?;
    let header_flags = reader.u16_le().map_err(corrupt)?;
    if container_version != CONTAINER_VERSION
        || header_len != HEADER_LEN_U16
        || file_kind != FILE_KIND_SEGMENT_FACTS
        || header_flags != 0
    {
        return Err(CacheReadError::Incompatible);
    }
    let identity = HeaderIdentity {
        fact_schema_version: reader.u32_le().map_err(corrupt)?,
        extractor_semantics_version: reader.u32_le().map_err(corrupt)?,
        registry_contract_version: reader.u32_le().map_err(corrupt)?,
        source_format_version: reader.u32_le().map_err(corrupt)?,
        pgm_source_id: reader.u64_le().map_err(corrupt)?,
        source_min_ts_us: reader.i64_le().map_err(corrupt)?,
        source_max_ts_us: reader.i64_le().map_err(corrupt)?,
        source_file_len: reader.u64_le().map_err(corrupt)?,
        source_scope_id: reader.array().map_err(corrupt)?,
        source_descriptor: reader.array().map_err(corrupt)?,
    };
    let directory_offset = reader.u64_le().map_err(corrupt)?;
    let directory_count = reader.u32_le().map_err(corrupt)?;
    let directory_entry_len = reader.u16_le().map_err(corrupt)?;
    let descriptor_kind = reader.u16_le().map_err(corrupt)?;
    let file_len = reader.u64_le().map_err(corrupt)?;
    let directory_crc32c = reader.u32_le().map_err(corrupt)?;
    let stored_header_crc = reader.u32_le().map_err(corrupt)?;
    if directory_entry_len != DIRECTORY_ENTRY_LEN_U16 || descriptor_kind != DESCRIPTOR_KIND_CATALOG
    {
        return Err(CacheReadError::Incompatible);
    }
    if crc32c(&bytes[..HEADER_LEN - 4]) != stored_header_crc {
        return Err(CacheReadError::Corrupt);
    }
    Ok((
        FactFileHeader {
            identity,
            directory_offset,
            directory_count,
            file_len,
        },
        directory_crc32c,
    ))
}

fn verify_identity(
    header: &HeaderIdentity,
    expected: &HeaderIdentity,
) -> Result<(), CacheReadError> {
    if header.fact_schema_version != expected.fact_schema_version
        || header.extractor_semantics_version != expected.extractor_semantics_version
        || header.registry_contract_version != expected.registry_contract_version
    {
        return Err(CacheReadError::Incompatible);
    }
    if header.source_scope_id != expected.source_scope_id
        || header.source_descriptor != expected.source_descriptor
        || header.pgm_source_id != expected.pgm_source_id
        || header.source_min_ts_us != expected.source_min_ts_us
        || header.source_max_ts_us != expected.source_max_ts_us
        || header.source_format_version != expected.source_format_version
        || header.source_file_len != expected.source_file_len
    {
        return Err(CacheReadError::WrongSource);
    }
    Ok(())
}

fn decode_directory(
    bytes: &[u8],
    count: u32,
    bounds: &Bounds,
) -> Result<Vec<BlockDirectoryEntry>, CacheReadError> {
    let mut reader = ByteReader::new(bytes);
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let block_kind = reader.u32_le().map_err(|_error| CacheReadError::Corrupt)?;
        let block_schema_version = reader.u16_le().map_err(|_error| CacheReadError::Corrupt)?;
        let raw_flags = reader.u16_le().map_err(|_error| CacheReadError::Corrupt)?;
        let logical_id = reader.u32_le().map_err(|_error| CacheReadError::Corrupt)?;
        let reserved = reader.u32_le().map_err(|_error| CacheReadError::Corrupt)?;
        let offset = reader.u64_le().map_err(|_error| CacheReadError::Corrupt)?;
        let stored_len = reader.u64_le().map_err(|_error| CacheReadError::Corrupt)?;
        let decoded_len = reader.u64_le().map_err(|_error| CacheReadError::Corrupt)?;
        let item_count = reader.u32_le().map_err(|_error| CacheReadError::Corrupt)?;
        let block_crc32c = reader.u32_le().map_err(|_error| CacheReadError::Corrupt)?;
        let min_ts_us = reader.i64_le().map_err(|_error| CacheReadError::Corrupt)?;
        let max_ts_us = reader.i64_le().map_err(|_error| CacheReadError::Corrupt)?;
        if reserved != 0 {
            return Err(CacheReadError::Corrupt);
        }
        let flags = BlockFlags::from_bits(raw_flags)?;
        if BlockKind::from_code(block_kind).is_none() && flags.required_for_schema {
            // An unknown required block makes the whole file incompatible.
            return Err(CacheReadError::Incompatible);
        }
        if stored_len > bounds.stored_block_len || decoded_len > bounds.decoded_block_len {
            return Err(CacheReadError::Oversized);
        }
        if u64::from(item_count) > bounds.items_per_block {
            return Err(CacheReadError::Oversized);
        }
        if min_ts_us > max_ts_us {
            return Err(CacheReadError::Corrupt);
        }
        entries.push(BlockDirectoryEntry {
            block_kind,
            block_schema_version,
            flags,
            logical_id,
            offset,
            stored_len,
            decoded_len,
            item_count,
            block_crc32c,
            min_ts_us,
            max_ts_us,
        });
    }
    reader.finish().map_err(|_error| CacheReadError::Corrupt)?;
    Ok(entries)
}

fn verify_layout(
    directory: &[BlockDirectoryEntry],
    directory_end: u64,
    file_len: u64,
    bounds: &Bounds,
) -> Result<(), CacheReadError> {
    let _ = bounds;
    let mut cursor = directory_end;
    let mut previous_order: Option<(u32, u32, i64)> = None;
    for entry in directory {
        let order = (entry.block_kind, entry.logical_id, entry.min_ts_us);
        if let Some(previous) = previous_order
            && order <= previous
        {
            return Err(CacheReadError::Corrupt);
        }
        previous_order = Some(order);
        // Contiguous, non-overlapping extents with no gaps or trailing bytes.
        if entry.offset != cursor {
            return Err(CacheReadError::Corrupt);
        }
        cursor = entry
            .offset
            .checked_add(entry.stored_len)
            .ok_or(CacheReadError::Oversized)?;
        if cursor > file_len {
            return Err(CacheReadError::Corrupt);
        }
    }
    if cursor != file_len {
        return Err(CacheReadError::Corrupt);
    }
    Ok(())
}

fn verify_required_baseline(directory: &[BlockDirectoryEntry]) -> Result<(), CacheReadError> {
    for kind in BlockKind::ALL {
        if !directory
            .iter()
            .any(|entry| entry.block_kind == kind.code())
        {
            return Err(CacheReadError::Corrupt);
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the writer mirrors the fixed 64-byte directory entry"
)]
fn write_directory_entry(
    writer: &mut ByteWriter,
    block_kind: u32,
    flags: BlockFlags,
    offset: u64,
    stored_len: u64,
    item_count: u32,
    block_crc32c: u32,
    min_ts_us: i64,
    max_ts_us: i64,
) {
    writer.u32_le(block_kind);
    writer.u16_le(BLOCK_SCHEMA_VERSION);
    writer.u16_le(flags.to_bits());
    writer.u32_le(0);
    writer.u32_le(0);
    writer.u64_le(offset);
    writer.u64_le(stored_len);
    writer.u64_le(stored_len);
    writer.u32_le(item_count);
    writer.u32_le(block_crc32c);
    writer.i64_le(min_ts_us);
    writer.i64_le(max_ts_us);
}

const fn baseline_index(kind: BlockKind) -> usize {
    match kind {
        BlockKind::SourceManifest => 0,
        BlockKind::EventObservations => 1,
        BlockKind::EventFacts => 2,
        BlockKind::LossCoverage => 3,
        BlockKind::GaugeSamples => 4,
        BlockKind::CounterSamples => 5,
        BlockKind::ResetMarkers => 6,
        BlockKind::EntityStates => 7,
        BlockKind::StringTable => 8,
    }
}

fn empty_block(kind: BlockKind) -> BlockContent {
    let bounds = super::limits::LIMIT;
    match kind {
        BlockKind::SourceManifest => BlockContent::SourceManifest(Box::new(
            SourceManifestBlock::new(0, 0, 0, 0, 0, Vec::new(), &bounds)
                .expect("empty manifest is valid"),
        )),
        BlockKind::EventObservations => BlockContent::EventObservations(Box::new(
            EventObservationsBlock::new(Vec::new(), &bounds).expect("empty observations are valid"),
        )),
        BlockKind::EventFacts => BlockContent::EventFacts,
        BlockKind::LossCoverage => {
            BlockContent::LossCoverage(Box::new(empty_loss_coverage(&bounds)))
        }
        BlockKind::GaugeSamples => BlockContent::GaugeSamples(Box::new(
            GaugeSamplesBlock::new(Vec::new(), &bounds).expect("empty gauges are valid"),
        )),
        BlockKind::CounterSamples => BlockContent::CounterSamples(Box::new(
            CounterSamplesBlock::new(Vec::new(), &bounds).expect("empty counters are valid"),
        )),
        BlockKind::ResetMarkers => BlockContent::ResetMarkers(Box::new(
            ResetMarkersBlock::new(Vec::new(), &bounds).expect("empty resets are valid"),
        )),
        BlockKind::EntityStates => BlockContent::EntityStates(Box::new(
            EntityStatesBlock::new(Vec::new(), &bounds).expect("empty entities are valid"),
        )),
        BlockKind::StringTable => BlockContent::StringTable(Box::new(
            StringTableBlock::new(Vec::new(), &bounds).expect("empty string table is valid"),
        )),
    }
}

fn empty_loss_coverage(bounds: &Bounds) -> LossCoverageBlock {
    use kronika_analytics::overview::{
        Applicability, Coverage, PeriodQuality, PhysicalCountSemantics, RetainedExactness,
        SourceCompleteness,
    };
    LossCoverageBlock::new(
        Coverage::empty(),
        Coverage::empty(),
        Applicability::Applicable,
        PeriodQuality::Unknown,
        SourceCompleteness::Unknown,
        RetainedExactness::Unknown,
        PhysicalCountSemantics::Unknown,
        0,
        bounds,
    )
    .expect("empty coverage is valid")
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{AlignmentId, CounterSample, GaugeSample, MetricSeriesId};

    use super::super::limits::LIMIT;
    use super::*;

    fn identity() -> HeaderIdentity {
        HeaderIdentity::from_current_contract(1, 7, 1_000, 2_000, 4_096, [0x11; 32], [0x22; 32])
    }

    fn sample_blocks() -> Vec<BlockContent> {
        let counter = CounterSample::new(MetricSeriesId([1; 16]), AlignmentId([1; 16]), 10, 5, 1);
        let gauge = GaugeSample::new(MetricSeriesId([2; 16]), 20, 2.5).expect("finite");
        vec![
            BlockContent::CounterSamples(Box::new(
                CounterSamplesBlock::new(vec![counter], &LIMIT).expect("counter block"),
            )),
            BlockContent::GaugeSamples(Box::new(
                GaugeSamplesBlock::new(vec![gauge], &LIMIT).expect("gauge block"),
            )),
            BlockContent::StringTable(Box::new(
                StringTableBlock::new(vec![Box::from(b"pattern".as_slice())], &LIMIT)
                    .expect("string table"),
            )),
        ]
    }

    fn valid_file() -> Vec<u8> {
        FactFile::build(&identity(), sample_blocks(), &LIMIT).expect("valid build")
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        let field: [u8; 4] = bytes[at..at + 4].try_into().expect("four bytes");
        u32::from_le_bytes(field)
    }

    /// Recomputes the directory and header CRCs so a mutated field passes CRC
    /// and the semantic check under test is what actually fires.
    fn reseal(bytes: &mut [u8]) {
        let count = u32_at(bytes, 136) as usize;
        let directory_end = HEADER_LEN + count * DIRECTORY_ENTRY_LEN_U16 as usize;
        let directory_crc = crc32c(&bytes[HEADER_LEN..directory_end]);
        bytes[152..156].copy_from_slice(&directory_crc.to_le_bytes());
        reseal_header(bytes);
    }

    /// Recomputes only the header CRC, for a field checked before the directory.
    fn reseal_header(bytes: &mut [u8]) {
        let header_crc = crc32c(&bytes[..HEADER_LEN - 4]);
        bytes[HEADER_LEN - 4..HEADER_LEN].copy_from_slice(&header_crc.to_le_bytes());
    }

    fn entry_field(index: usize, field_offset: usize) -> usize {
        HEADER_LEN + index * DIRECTORY_ENTRY_LEN_U16 as usize + field_offset
    }

    #[test]
    fn a_built_file_round_trips_through_admission() {
        let bytes = valid_file();
        let file = FactFile::admit(&bytes, &identity(), &LIMIT).expect("admits");
        assert_eq!(
            usize::try_from(file.header().directory_count).expect("count fits usize"),
            BlockKind::ALL.len()
        );
        assert_eq!(file.header().file_len, bytes.len() as u64);

        let body = file
            .block_body(BlockKind::CounterSamples)
            .expect("counter block present");
        let decoded = CounterSamplesBlock::decode(body, &LIMIT).expect("decode");
        assert_eq!(decoded.samples()[0].value(), 5);
    }

    #[test]
    fn every_required_baseline_block_is_present() {
        let bytes = valid_file();
        let file = FactFile::admit(&bytes, &identity(), &LIMIT).expect("admits");
        for kind in BlockKind::ALL {
            assert!(
                file.block_body(kind).is_some(),
                "missing baseline block {kind:?}"
            );
        }
    }

    #[test]
    fn a_bad_magic_is_incompatible() {
        let mut bytes = valid_file();
        bytes[0] ^= 0xFF;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn an_unknown_container_version_is_incompatible() {
        let mut bytes = valid_file();
        bytes[8] = 2;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn a_wrong_file_kind_or_flags_or_descriptor_is_incompatible() {
        for at in [12_usize, 14, 142] {
            let mut bytes = valid_file();
            bytes[at] = 9;
            reseal(&mut bytes);
            assert!(
                matches!(
                    FactFile::admit(&bytes, &identity(), &LIMIT),
                    Err(CacheReadError::Incompatible)
                ),
                "byte {at} should be incompatible"
            );
        }
    }

    #[test]
    fn a_wrong_directory_entry_len_is_incompatible() {
        let mut bytes = valid_file();
        bytes[140] = 65;
        reseal(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn a_header_crc_mismatch_is_corrupt() {
        let mut bytes = valid_file();
        // Flip a byte inside the identity region without resealing the CRC.
        bytes[64] ^= 0x01;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn a_directory_crc_mismatch_is_corrupt() {
        let mut bytes = valid_file();
        // Flip a byte inside the first directory entry, leaving the CRC stale.
        let at = entry_field(0, 48);
        bytes[at] ^= 0x01;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn a_block_crc_mismatch_is_corrupt() {
        let mut bytes = valid_file();
        // The last byte belongs to the final block body.
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn a_truncated_header_is_corrupt() {
        let bytes = valid_file();
        assert!(matches!(
            FactFile::admit(&bytes[..100], &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn a_trailing_byte_or_wrong_file_len_is_corrupt() {
        let mut bytes = valid_file();
        bytes.push(0);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn a_wrong_source_scope_or_descriptor_is_wrong_source() {
        let bytes = valid_file();
        let mut expected = identity();
        expected.source_scope_id = [0x99; 32];
        assert!(matches!(
            FactFile::admit(&bytes, &expected, &LIMIT),
            Err(CacheReadError::WrongSource)
        ));
    }

    #[test]
    fn a_version_axis_mismatch_is_incompatible() {
        let bytes = valid_file();
        let mut expected = identity();
        expected.fact_schema_version += 1;
        assert!(matches!(
            FactFile::admit(&bytes, &expected, &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn a_file_above_the_length_bound_is_oversized() {
        let bytes = valid_file();
        let bounds = Bounds {
            fact_file_len: 64,
            ..LIMIT
        };
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &bounds),
            Err(CacheReadError::Oversized)
        ));
    }

    #[test]
    fn a_directory_count_above_the_bound_is_oversized() {
        let mut bytes = valid_file();
        // A count above the bound is rejected before any directory read, so only
        // the header CRC needs to stay valid.
        bytes[136..140].copy_from_slice(&5_000_u32.to_le_bytes());
        reseal_header(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Oversized)
        ));
    }

    #[test]
    fn a_nonzero_reserved_field_is_corrupt() {
        let mut bytes = valid_file();
        let at = entry_field(0, 12);
        bytes[at] = 1;
        reseal(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn an_unknown_block_flag_bit_is_incompatible() {
        let mut bytes = valid_file();
        let at = entry_field(0, 6);
        // Set a reserved flag bit (bit 4).
        bytes[at] |= 0x10;
        reseal(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn a_compressed_codec_flag_is_incompatible_in_this_build() {
        let mut bytes = valid_file();
        let at = entry_field(0, 6);
        // Set the codec nibble (bits 8..12) to Zstd on the first entry.
        let flags = u16::from_le_bytes([bytes[at], bytes[at + 1]]) | 0x0100;
        bytes[at..at + 2].copy_from_slice(&flags.to_le_bytes());
        reseal(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn an_unknown_required_block_kind_is_incompatible() {
        let mut bytes = valid_file();
        let at = entry_field(0, 0);
        bytes[at..at + 4].copy_from_slice(&9_999_u32.to_le_bytes());
        reseal(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn an_overlapping_block_offset_is_corrupt() {
        let mut bytes = valid_file();
        // Shift the second entry's offset back so its extent overlaps the first.
        let at = entry_field(1, 16);
        bytes[at..at + 8].copy_from_slice(&HEADER_LEN_U64.to_le_bytes());
        reseal(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn a_min_ts_above_max_ts_in_the_header_is_corrupt() {
        // Build a file whose header range is inverted, then admit with the same
        // identity so the range check, not the identity check, fires.
        let mut inverted = identity();
        inverted.source_min_ts_us = 5_000;
        inverted.source_max_ts_us = 1_000;
        let bytes = FactFile::build(&inverted, sample_blocks(), &LIMIT).expect("build");
        assert!(matches!(
            FactFile::admit(&bytes, &inverted, &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn build_rejects_two_blocks_of_the_same_kind() {
        let counter = CounterSample::new(MetricSeriesId([1; 16]), AlignmentId([1; 16]), 10, 5, 1);
        let duplicate = vec![
            BlockContent::CounterSamples(Box::new(
                CounterSamplesBlock::new(vec![counter], &LIMIT).expect("block"),
            )),
            BlockContent::CounterSamples(Box::new(
                CounterSamplesBlock::new(vec![], &LIMIT).expect("block"),
            )),
        ];
        assert!(matches!(
            FactFile::build(&identity(), duplicate, &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }
}
