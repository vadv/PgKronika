//! Bounded `PGKOVF` framing, canonical block layout, and admission.
//!
//! The fixed header and directory are serialized field by field in
//! little-endian order. Full admission verifies physical framing and every
//! known logical block. [`FactFileReader`] exposes the same metadata checks for
//! positional sources and reads only requested block bodies.

use std::collections::BTreeMap;

use kronika_analytics::overview::{
    CONTAINER_VERSION, EXTRACTOR_SEMANTICS_VERSION, FACT_SCHEMA_VERSION, REGISTRY_CONTRACT_VERSION,
    SegmentIdentity, SourceScopeId,
};
use kronika_format::{ReadAt, crc32c};

use super::block::{
    BlockCodec, BlockError, BlockFlags, BlockKind, CounterSamplesBlock, EncodableBlock,
    EntityStatesBlock, GaugeSamplesBlock, LossCoverageBlock, ResetMarkersBlock,
    SourceManifestBlock, StringTableBlock,
};
use super::bytes::{ByteReader, ByteWriter};
use super::descriptors::SourceDescriptor;
use super::limits::Bounds;
use super::observations::EventObservationsBlock;

const MAGIC: [u8; 8] = *b"PGKOVF\0\0";
const HEADER_LEN: usize = 160;
const HEADER_LEN_U16: u16 = 160;
const HEADER_LEN_U64: u64 = 160;
const DIRECTORY_ENTRY_LEN: usize = 64;
const DIRECTORY_ENTRY_LEN_U16: u16 = 64;
const FILE_KIND_SEGMENT_FACTS: u16 = 1;
const DESCRIPTOR_KIND_CATALOG: u16 = 1;
const BLOCK_SCHEMA_VERSION: u16 = 1;
const HEADER_CRC_OFFSET: usize = 156;

/// Source and compatibility identity serialized in a fact-file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderIdentity {
    /// Logical fact-shape version.
    pub fact_schema_version: u32,
    /// PGM-to-fact extraction version.
    pub extractor_semantics_version: u32,
    /// Supported registry contract version.
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
    pub source_scope_id: SourceScopeId,
    /// Content-derived PGM descriptor.
    pub source_descriptor: SourceDescriptor,
}

impl HeaderIdentity {
    /// Builds an identity with version axes from the analytics contract.
    #[must_use]
    pub const fn from_current_contract(
        source_format_version: u32,
        pgm_source_id: u64,
        source_min_ts_us: i64,
        source_max_ts_us: i64,
        source_file_len: u64,
        source_scope_id: SourceScopeId,
        source_descriptor: SourceDescriptor,
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

/// Decoded fixed header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactFileHeader {
    /// Source and compatibility identity.
    pub identity: HeaderIdentity,
    /// Directory offset, fixed at 160 in version 1.
    pub directory_offset: u64,
    /// Directory entry count.
    pub directory_count: u32,
    /// Exact fact-file length.
    pub file_len: u64,
}

/// Decoded 64-byte directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDirectoryEntry {
    /// Stable block-kind code.
    pub block_kind: u32,
    /// Block schema version.
    pub block_schema_version: u16,
    /// Parsed flags.
    pub flags: BlockFlags,
    /// Stable factor or source ID, or zero for segment-wide data.
    pub logical_id: u32,
    /// Absolute offset of stored bytes.
    pub offset: u64,
    /// Stored byte length.
    pub stored_len: u64,
    /// Decoded byte length.
    pub decoded_len: u64,
    /// Logical item count.
    pub item_count: u32,
    /// CRC32C of stored bytes.
    pub block_crc32c: u32,
    /// Inclusive minimum item timestamp.
    pub min_ts_us: i64,
    /// Inclusive maximum item timestamp.
    pub max_ts_us: i64,
}

/// Work performed by a positional fact-file read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactReadStats {
    /// Positional body or metadata reads.
    pub read_calls: u64,
    /// Stored bytes read.
    pub stored_bytes_read: u64,
    /// Declared decoded bytes for successfully read bodies.
    pub decoded_bytes: u64,
}

/// Why a fact file could not be admitted.
#[derive(Debug)]
pub enum CacheReadError {
    /// A version, required schema, flag, or codec is unsupported.
    Incompatible,
    /// CRC, layout, or logical validation failed.
    Corrupt,
    /// Header provenance differs from the expected source.
    WrongSource,
    /// A hard safety limit was exceeded.
    Oversized,
    /// A positional read failed.
    Io(std::io::Error),
}

impl std::fmt::Display for CacheReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incompatible => f.write_str("fact file uses an incompatible contract"),
            Self::Corrupt => f.write_str("fact file is corrupt"),
            Self::WrongSource => f.write_str("fact file belongs to another source"),
            Self::Oversized => f.write_str("fact file exceeds a safety limit"),
            Self::Io(error) => write!(f, "fact file I/O: {error}"),
        }
    }
}

impl std::error::Error for CacheReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Incompatible | Self::Corrupt | Self::WrongSource | Self::Oversized => None,
        }
    }
}

impl From<std::io::Error> for CacheReadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<BlockError> for CacheReadError {
    fn from(error: BlockError) -> Self {
        match error {
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

/// One logical block supplied to the canonical builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockContent {
    /// Catalog inventory and source metadata.
    SourceManifest(Box<SourceManifestBlock>),
    /// Retained source observations.
    EventObservations(Box<EventObservationsBlock>),
    /// Policy-neutral facts block; container version 1 requires an empty body.
    EventFacts,
    /// Coverage and loss metadata.
    LossCoverage(Box<LossCoverageBlock>),
    /// Gauge samples.
    GaugeSamples(Box<GaugeSamplesBlock>),
    /// Counter samples.
    CounterSamples(Box<CounterSamplesBlock>),
    /// Counter reset markers.
    ResetMarkers(Box<ResetMarkersBlock>),
    /// Entity snapshots.
    EntityStates(Box<EntityStatesBlock>),
    /// Retained text and byte values.
    StringTable(Box<StringTableBlock>),
}

impl BlockContent {
    /// Stable block kind.
    #[must_use]
    pub fn kind(&self) -> BlockKind {
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

    fn encoded(&self) -> EncodedBlock {
        match self {
            Self::SourceManifest(block) => encoded_block(block.as_ref()),
            Self::EventObservations(block) => encoded_block(block.as_ref()),
            Self::EventFacts => EncodedBlock {
                body: Vec::new(),
                sorted: true,
                item_count: 0,
                time_range: None,
            },
            Self::LossCoverage(block) => encoded_block(block.as_ref()),
            Self::GaugeSamples(block) => encoded_block(block.as_ref()),
            Self::CounterSamples(block) => encoded_block(block.as_ref()),
            Self::ResetMarkers(block) => encoded_block(block.as_ref()),
            Self::EntityStates(block) => encoded_block(block.as_ref()),
            Self::StringTable(block) => encoded_block(block.as_ref()),
        }
    }
}

struct EncodedBlock {
    body: Vec<u8>,
    sorted: bool,
    item_count: u64,
    time_range: Option<(i64, i64)>,
}

fn encoded_block<B: EncodableBlock>(block: &B) -> EncodedBlock {
    EncodedBlock {
        body: block.encode(),
        sorted: block.canonically_sorted(),
        item_count: block.item_count(),
        time_range: block.time_range(),
    }
}

/// Fully admitted fact-file bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct FactFile {
    header: FactFileHeader,
    directory: Vec<BlockDirectoryEntry>,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for FactFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactFile")
            .field("header", &self.header)
            .field("directory", &self.directory)
            .field("stored_bytes", &self.bytes.len())
            .finish()
    }
}

impl FactFile {
    /// Decoded header.
    #[must_use]
    pub const fn header(&self) -> &FactFileHeader {
        &self.header
    }

    /// Decoded directory.
    #[must_use]
    pub fn directory(&self) -> &[BlockDirectoryEntry] {
        &self.directory
    }

    /// First admitted body of `kind`.
    #[must_use]
    pub fn block_body(&self, kind: BlockKind) -> Option<&[u8]> {
        self.directory
            .iter()
            .find(|entry| entry.block_kind == kind.code())
            .and_then(|entry| {
                let start = usize::try_from(entry.offset).ok()?;
                let length = usize::try_from(entry.stored_len).ok()?;
                let end = start.checked_add(length)?;
                self.bytes.get(start..end)
            })
    }

    /// Builds one canonical version-1 fact file.
    ///
    /// `SOURCE_MANIFEST` is mandatory because its metadata must match the
    /// header. Other missing baseline kinds are inserted as canonical empty
    /// blocks.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] for duplicate kinds, inconsistent source
    /// metadata, or a safety-limit violation.
    #[allow(
        clippy::too_many_lines,
        reason = "the fixed header-directory-body assembly keeps offset and checksum state local"
    )]
    pub fn build(
        identity: &HeaderIdentity,
        blocks: Vec<BlockContent>,
        bounds: &Bounds,
    ) -> Result<Vec<u8>, CacheReadError> {
        validate_api_inputs(identity, bounds)?;

        let mut present = [false; BlockKind::ALL.len()];
        let mut selected = Vec::with_capacity(BlockKind::ALL.len());
        for block in blocks {
            let index = baseline_index(block.kind());
            if present[index] {
                return Err(CacheReadError::Corrupt);
            }
            present[index] = true;
            selected.push(block);
        }
        if !present[baseline_index(BlockKind::SourceManifest)] {
            return Err(CacheReadError::Corrupt);
        }
        let observation_strings = selected.iter().find_map(|block| match block {
            BlockContent::EventObservations(observations) => {
                Some(observations.string_table().clone())
            }
            _ => None,
        });
        if let Some(supplied) = selected.iter().find_map(|block| match block {
            BlockContent::StringTable(strings) => Some(strings.as_ref()),
            _ => None,
        }) && observation_strings
            .as_ref()
            .is_some_and(|derived| derived != supplied)
        {
            return Err(CacheReadError::Corrupt);
        }
        for (index, was_present) in present.into_iter().enumerate() {
            if !was_present {
                let kind = BlockKind::ALL[index];
                if kind == BlockKind::StringTable {
                    selected.push(BlockContent::StringTable(Box::new(
                        observation_strings
                            .clone()
                            .unwrap_or(StringTableBlock::new(Vec::new(), bounds)?),
                    )));
                } else {
                    selected.push(empty_block(kind, bounds)?);
                }
            }
        }
        selected.sort_by_key(|block| block.kind().code());

        if let Some(BlockContent::SourceManifest(manifest)) = selected.first() {
            verify_manifest_identity(manifest, identity)?;
        } else {
            return Err(CacheReadError::Corrupt);
        }

        let count = u32::try_from(selected.len()).map_err(|_error| CacheReadError::Oversized)?;
        let directory_bytes = directory_bytes(count, bounds)?;
        let mut offset = HEADER_LEN_U64
            .checked_add(directory_bytes)
            .ok_or(CacheReadError::Oversized)?;
        let mut decoded_sum = 0_u64;
        let mut directory = ByteWriter::new();
        let mut bodies = Vec::with_capacity(selected.len());

        for block in &selected {
            let encoded = block.encoded();
            let stored_len = encoded.body.len() as u64;
            if stored_len > bounds.stored_block_len || stored_len > bounds.decoded_block_len {
                return Err(CacheReadError::Oversized);
            }
            decoded_sum = decoded_sum
                .checked_add(stored_len)
                .ok_or(CacheReadError::Oversized)?;
            if decoded_sum > bounds.decoded_file_bytes {
                return Err(CacheReadError::Oversized);
            }
            let item_count =
                u32::try_from(encoded.item_count).map_err(|_error| CacheReadError::Oversized)?;
            if u64::from(item_count) > bounds.items_per_block {
                return Err(CacheReadError::Oversized);
            }
            if (item_count == 0) != encoded.body.is_empty() {
                return Err(CacheReadError::Corrupt);
            }
            let (min_ts_us, max_ts_us) = encoded.time_range.unwrap_or((0, 0));
            if let Some((minimum, maximum)) = encoded.time_range
                && (minimum > maximum
                    || minimum < identity.source_min_ts_us
                    || maximum > identity.source_max_ts_us)
            {
                return Err(CacheReadError::Corrupt);
            }
            let flags = BlockFlags {
                required_for_schema: true,
                canonically_sorted: encoded.sorted,
                has_time_range: encoded.time_range.is_some(),
                codec: BlockCodec::None,
            };
            write_directory_entry(
                &mut directory,
                block.kind().code(),
                flags,
                offset,
                stored_len,
                item_count,
                crc32c(&encoded.body),
                min_ts_us,
                max_ts_us,
            );
            offset = offset
                .checked_add(stored_len)
                .ok_or(CacheReadError::Oversized)?;
            bodies.push(encoded.body);
        }
        if offset > bounds.fact_file_len {
            return Err(CacheReadError::Oversized);
        }

        let directory = directory.into_bytes();
        let header = encode_header(identity, count, offset, crc32c(&directory));
        let capacity = usize::try_from(offset).map_err(|_error| CacheReadError::Oversized)?;
        let mut output = Vec::with_capacity(capacity);
        output.extend_from_slice(&header);
        output.extend_from_slice(&directory);
        for body in bodies {
            output.extend_from_slice(&body);
        }
        if output.len() != capacity {
            return Err(CacheReadError::Corrupt);
        }
        Ok(output)
    }

    /// Admits complete bytes and validates all known logical blocks.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] for physical corruption, incompatible
    /// schemas, wrong provenance, unsafe lengths, or a logical block that
    /// contradicts its directory metadata.
    pub fn admit(
        bytes: &[u8],
        expected: &HeaderIdentity,
        lineage: &SegmentIdentity,
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        validate_api_inputs(expected, bounds)?;
        if lineage.source_scope_id() != expected.source_scope_id {
            return Err(CacheReadError::WrongSource);
        }
        if bytes.len() as u64 > bounds.fact_file_len {
            return Err(CacheReadError::Oversized);
        }
        if bytes.len() < HEADER_LEN {
            return Err(CacheReadError::Corrupt);
        }
        let (header, directory_crc) = decode_header(&bytes[..HEADER_LEN])?;
        verify_identity(&header.identity, expected)?;
        if header.file_len != bytes.len() as u64 {
            return Err(CacheReadError::Corrupt);
        }
        let directory_end = checked_directory_end(&header, bounds)?;
        let directory_end_usize =
            usize::try_from(directory_end).map_err(|_error| CacheReadError::Corrupt)?;
        let directory_bytes = &bytes[HEADER_LEN..directory_end_usize];
        if crc32c(directory_bytes) != directory_crc {
            return Err(CacheReadError::Corrupt);
        }
        let directory = decode_directory(directory_bytes, header.directory_count, bounds)?;
        validate_directory(&directory, &header, directory_end, bounds)?;

        let mut bodies = Vec::with_capacity(directory.len());
        for entry in &directory {
            let start = usize::try_from(entry.offset).map_err(|_error| CacheReadError::Corrupt)?;
            let length =
                usize::try_from(entry.stored_len).map_err(|_error| CacheReadError::Corrupt)?;
            let end = start.checked_add(length).ok_or(CacheReadError::Corrupt)?;
            let body = bytes.get(start..end).ok_or(CacheReadError::Corrupt)?;
            if crc32c(body) != entry.block_crc32c {
                return Err(CacheReadError::Corrupt);
            }
            bodies.push(body);
        }
        validate_logical_blocks(&directory, &bodies, &header.identity, lineage, bounds)?;

        Ok(Self {
            header,
            directory,
            bytes: bytes.to_vec(),
        })
    }
}

/// Metadata-admitted fact file over a positional byte source.
#[derive(Debug)]
pub struct FactFileReader<R: ReadAt> {
    reader: R,
    header: FactFileHeader,
    directory: Vec<BlockDirectoryEntry>,
    stats: FactReadStats,
}

impl<R: ReadAt> FactFileReader<R> {
    /// Reads only the fixed header and bounded directory.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when metadata or source identity is invalid.
    pub fn open(
        reader: R,
        expected: &HeaderIdentity,
        bounds: &Bounds,
    ) -> Result<Self, CacheReadError> {
        validate_api_inputs(expected, bounds)?;
        let file_len = reader.byte_len()?;
        if file_len > bounds.fact_file_len {
            return Err(CacheReadError::Oversized);
        }
        if file_len < HEADER_LEN_U64 {
            return Err(CacheReadError::Corrupt);
        }

        let mut header_bytes = [0_u8; HEADER_LEN];
        reader.read_exact_at(&mut header_bytes, 0)?;
        let (header, directory_crc) = decode_header(&header_bytes)?;
        verify_identity(&header.identity, expected)?;
        if header.file_len != file_len {
            return Err(CacheReadError::Corrupt);
        }
        let directory_end = checked_directory_end(&header, bounds)?;
        let length = directory_end
            .checked_sub(HEADER_LEN_U64)
            .ok_or(CacheReadError::Corrupt)?;
        let length = usize::try_from(length).map_err(|_error| CacheReadError::Oversized)?;
        let mut directory_bytes = vec![0_u8; length];
        reader.read_exact_at(&mut directory_bytes, HEADER_LEN_U64)?;
        if crc32c(&directory_bytes) != directory_crc {
            return Err(CacheReadError::Corrupt);
        }
        let directory = decode_directory(&directory_bytes, header.directory_count, bounds)?;
        validate_directory(&directory, &header, directory_end, bounds)?;
        let metadata_bytes = HEADER_LEN_U64
            .checked_add(length as u64)
            .ok_or(CacheReadError::Oversized)?;
        Ok(Self {
            reader,
            header,
            directory,
            stats: FactReadStats {
                read_calls: 2,
                stored_bytes_read: metadata_bytes,
                decoded_bytes: 0,
            },
        })
    }

    /// Decoded header.
    #[must_use]
    pub const fn header(&self) -> &FactFileHeader {
        &self.header
    }

    /// Decoded directory.
    #[must_use]
    pub fn directory(&self) -> &[BlockDirectoryEntry] {
        &self.directory
    }

    /// Current positional-read counters.
    #[must_use]
    pub const fn stats(&self) -> FactReadStats {
        self.stats
    }

    /// Reads and CRC-checks every stored body of `kind`.
    ///
    /// Unknown or unselected bodies are not read. Call the corresponding
    /// logical block decoder before using returned bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] for an I/O error, CRC failure, or counter
    /// overflow.
    pub fn read_blocks(&mut self, kind: BlockKind) -> Result<Vec<Vec<u8>>, CacheReadError> {
        self.read_matching_blocks(kind, None)
    }

    /// Reads blocks of `kind` that overlap the half-open range `[from, to)`.
    ///
    /// Non-temporal blocks are always selected. An empty or inverted query
    /// range selects no temporal blocks.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] under the same conditions as
    /// [`Self::read_blocks`].
    pub fn read_blocks_in_range(
        &mut self,
        kind: BlockKind,
        from: i64,
        to: i64,
    ) -> Result<Vec<Vec<u8>>, CacheReadError> {
        self.read_matching_blocks(kind, Some((from, to)))
    }

    fn read_matching_blocks(
        &mut self,
        kind: BlockKind,
        range: Option<(i64, i64)>,
    ) -> Result<Vec<Vec<u8>>, CacheReadError> {
        let selected: Vec<_> = self
            .directory
            .iter()
            .copied()
            .filter(|entry| entry.block_kind == kind.code())
            .filter(|entry| {
                range.is_none_or(|(from, to)| {
                    !entry.flags.has_time_range
                        || (from < to && entry.max_ts_us >= from && entry.min_ts_us < to)
                })
            })
            .collect();
        let mut bodies = Vec::with_capacity(selected.len());
        for entry in selected {
            let length =
                usize::try_from(entry.stored_len).map_err(|_error| CacheReadError::Oversized)?;
            let mut body = vec![0_u8; length];
            if !body.is_empty() {
                self.reader.read_exact_at(&mut body, entry.offset)?;
                self.stats.read_calls = self
                    .stats
                    .read_calls
                    .checked_add(1)
                    .ok_or(CacheReadError::Oversized)?;
                self.stats.stored_bytes_read = self
                    .stats
                    .stored_bytes_read
                    .checked_add(entry.stored_len)
                    .ok_or(CacheReadError::Oversized)?;
            }
            if crc32c(&body) != entry.block_crc32c {
                return Err(CacheReadError::Corrupt);
            }
            self.stats.decoded_bytes = self
                .stats
                .decoded_bytes
                .checked_add(entry.decoded_len)
                .ok_or(CacheReadError::Oversized)?;
            bodies.push(body);
        }
        Ok(bodies)
    }
}

const fn validate_api_inputs(
    identity: &HeaderIdentity,
    bounds: &Bounds,
) -> Result<(), CacheReadError> {
    if !bounds.is_within_absolute_limits() {
        return Err(CacheReadError::Oversized);
    }
    if identity.fact_schema_version != FACT_SCHEMA_VERSION
        || identity.extractor_semantics_version != EXTRACTOR_SEMANTICS_VERSION
        || identity.registry_contract_version != REGISTRY_CONTRACT_VERSION
        || identity.source_format_version != kronika_format::FORMAT_VERSION
    {
        return Err(CacheReadError::Incompatible);
    }
    if identity.source_min_ts_us > identity.source_max_ts_us {
        return Err(CacheReadError::Corrupt);
    }
    Ok(())
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
    writer.bytes(&identity.source_scope_id.0);
    writer.bytes(&identity.source_descriptor.0);
    writer.u64_le(HEADER_LEN_U64);
    writer.u32_le(directory_count);
    writer.u16_le(DIRECTORY_ENTRY_LEN_U16);
    writer.u16_le(DESCRIPTOR_KIND_CATALOG);
    writer.u64_le(file_len);
    writer.u32_le(directory_crc32c);
    writer.u32_le(0);
    let bytes = writer.into_bytes();
    let mut header = [0_u8; HEADER_LEN];
    header.copy_from_slice(&bytes);
    let checksum = crc32c(&header);
    header[HEADER_CRC_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    header
}

fn decode_header(bytes: &[u8]) -> Result<(FactFileHeader, u32), CacheReadError> {
    if bytes.len() != HEADER_LEN {
        return Err(CacheReadError::Corrupt);
    }
    let mut checksum_input = [0_u8; HEADER_LEN];
    checksum_input.copy_from_slice(bytes);
    let stored_checksum = u32::from_le_bytes(
        bytes[HEADER_CRC_OFFSET..HEADER_LEN]
            .try_into()
            .map_err(|_error| CacheReadError::Corrupt)?,
    );
    checksum_input[HEADER_CRC_OFFSET..HEADER_LEN].fill(0);
    if crc32c(&checksum_input) != stored_checksum {
        return Err(CacheReadError::Corrupt);
    }

    let mut reader = ByteReader::new(bytes);
    let magic: [u8; 8] = reader.array().map_err(|_error| CacheReadError::Corrupt)?;
    if magic != MAGIC {
        return Err(CacheReadError::Incompatible);
    }
    let corrupt = |_error| CacheReadError::Corrupt;
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
        source_scope_id: SourceScopeId(reader.array().map_err(corrupt)?),
        source_descriptor: SourceDescriptor(reader.array().map_err(corrupt)?),
    };
    if identity.fact_schema_version != FACT_SCHEMA_VERSION
        || identity.extractor_semantics_version != EXTRACTOR_SEMANTICS_VERSION
        || identity.registry_contract_version != REGISTRY_CONTRACT_VERSION
        || identity.source_format_version != kronika_format::FORMAT_VERSION
    {
        return Err(CacheReadError::Incompatible);
    }
    let directory_offset = reader.u64_le().map_err(corrupt)?;
    let directory_count = reader.u32_le().map_err(corrupt)?;
    let directory_entry_len = reader.u16_le().map_err(corrupt)?;
    let descriptor_kind = reader.u16_le().map_err(corrupt)?;
    let file_len = reader.u64_le().map_err(corrupt)?;
    let directory_crc32c = reader.u32_le().map_err(corrupt)?;
    let _header_crc32c = reader.u32_le().map_err(corrupt)?;
    reader.finish().map_err(corrupt)?;
    if directory_entry_len != DIRECTORY_ENTRY_LEN_U16 || descriptor_kind != DESCRIPTOR_KIND_CATALOG
    {
        return Err(CacheReadError::Incompatible);
    }
    if identity.source_min_ts_us > identity.source_max_ts_us {
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
    actual: &HeaderIdentity,
    expected: &HeaderIdentity,
) -> Result<(), CacheReadError> {
    if actual.fact_schema_version != expected.fact_schema_version
        || actual.extractor_semantics_version != expected.extractor_semantics_version
        || actual.registry_contract_version != expected.registry_contract_version
    {
        return Err(CacheReadError::Incompatible);
    }
    if actual.source_scope_id != expected.source_scope_id
        || actual.source_descriptor != expected.source_descriptor
        || actual.pgm_source_id != expected.pgm_source_id
        || actual.source_min_ts_us != expected.source_min_ts_us
        || actual.source_max_ts_us != expected.source_max_ts_us
        || actual.source_format_version != expected.source_format_version
        || actual.source_file_len != expected.source_file_len
    {
        return Err(CacheReadError::WrongSource);
    }
    Ok(())
}

fn directory_bytes(count: u32, bounds: &Bounds) -> Result<u64, CacheReadError> {
    if count == 0 || count > bounds.directory_entries {
        return Err(CacheReadError::Oversized);
    }
    let bytes = u64::from(count)
        .checked_mul(DIRECTORY_ENTRY_LEN as u64)
        .ok_or(CacheReadError::Oversized)?;
    if bytes > bounds.directory_bytes {
        return Err(CacheReadError::Oversized);
    }
    Ok(bytes)
}

fn checked_directory_end(header: &FactFileHeader, bounds: &Bounds) -> Result<u64, CacheReadError> {
    if header.directory_offset != HEADER_LEN_U64 {
        return Err(CacheReadError::Corrupt);
    }
    let bytes = directory_bytes(header.directory_count, bounds)?;
    let end = header
        .directory_offset
        .checked_add(bytes)
        .ok_or(CacheReadError::Oversized)?;
    if end > header.file_len {
        return Err(CacheReadError::Corrupt);
    }
    Ok(end)
}

fn decode_directory(
    bytes: &[u8],
    count: u32,
    bounds: &Bounds,
) -> Result<Vec<BlockDirectoryEntry>, CacheReadError> {
    let expected_len = usize::try_from(directory_bytes(count, bounds)?)
        .map_err(|_error| CacheReadError::Oversized)?;
    if bytes.len() != expected_len {
        return Err(CacheReadError::Corrupt);
    }
    let mut reader = ByteReader::new(bytes);
    let capacity = usize::try_from(count).map_err(|_error| CacheReadError::Oversized)?;
    let mut entries = Vec::with_capacity(capacity);
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
        if flags.codec != BlockCodec::None {
            return Err(CacheReadError::Incompatible);
        }
        if let Some(kind) = BlockKind::from_code(block_kind) {
            if block_schema_version != BLOCK_SCHEMA_VERSION {
                return Err(CacheReadError::Incompatible);
            }
            if !flags.required_for_schema || flags.canonically_sorted != expected_sorted(kind) {
                return Err(CacheReadError::Corrupt);
            }
        } else if flags.required_for_schema {
            return Err(CacheReadError::Incompatible);
        }
        if stored_len > bounds.stored_block_len || decoded_len > bounds.decoded_block_len {
            return Err(CacheReadError::Oversized);
        }
        if stored_len != decoded_len {
            return Err(CacheReadError::Corrupt);
        }
        if u64::from(item_count) > bounds.items_per_block {
            return Err(CacheReadError::Oversized);
        }
        if item_count == 0 {
            if stored_len != 0
                || decoded_len != 0
                || flags.has_time_range
                || min_ts_us != 0
                || max_ts_us != 0
            {
                return Err(CacheReadError::Corrupt);
            }
        } else if flags.has_time_range {
            if stored_len == 0 || min_ts_us > max_ts_us {
                return Err(CacheReadError::Corrupt);
            }
        } else if stored_len == 0 || min_ts_us != 0 || max_ts_us != 0 {
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

fn validate_directory(
    directory: &[BlockDirectoryEntry],
    header: &FactFileHeader,
    directory_end: u64,
    bounds: &Bounds,
) -> Result<(), CacheReadError> {
    let mut cursor = directory_end;
    let mut previous_order: Option<(u32, u32, i64)> = None;
    let mut previous_range: BTreeMap<(u32, u32), Option<i64>> = BTreeMap::new();
    let mut item_sums: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    let mut decoded_sum = 0_u64;

    for entry in directory {
        let order = (entry.block_kind, entry.logical_id, entry.min_ts_us);
        if previous_order.is_some_and(|previous| order <= previous) {
            return Err(CacheReadError::Corrupt);
        }
        previous_order = Some(order);
        if entry.offset != cursor {
            return Err(CacheReadError::Corrupt);
        }
        cursor = cursor
            .checked_add(entry.stored_len)
            .ok_or(CacheReadError::Oversized)?;
        if cursor > header.file_len {
            return Err(CacheReadError::Corrupt);
        }
        decoded_sum = decoded_sum
            .checked_add(entry.decoded_len)
            .ok_or(CacheReadError::Oversized)?;
        if decoded_sum > bounds.decoded_file_bytes {
            return Err(CacheReadError::Oversized);
        }
        let key = (entry.block_kind, entry.logical_id);
        let item_sum = item_sums.entry(key).or_default();
        *item_sum = item_sum
            .checked_add(u64::from(entry.item_count))
            .ok_or(CacheReadError::Oversized)?;
        if *item_sum > bounds.items_per_block {
            return Err(CacheReadError::Oversized);
        }
        match previous_range.insert(key, entry.flags.has_time_range.then_some(entry.max_ts_us)) {
            Some(Some(previous_max))
                if !entry.flags.has_time_range || entry.min_ts_us <= previous_max =>
            {
                return Err(CacheReadError::Corrupt);
            }
            Some(None) => return Err(CacheReadError::Corrupt),
            _ => {}
        }
        if entry.flags.has_time_range
            && (entry.min_ts_us < header.identity.source_min_ts_us
                || entry.max_ts_us > header.identity.source_max_ts_us)
        {
            return Err(CacheReadError::Corrupt);
        }
    }
    if cursor != header.file_len {
        return Err(CacheReadError::Corrupt);
    }
    verify_required_baseline(directory)
}

fn verify_required_baseline(directory: &[BlockDirectoryEntry]) -> Result<(), CacheReadError> {
    for kind in BlockKind::ALL {
        let count = directory
            .iter()
            .filter(|entry| entry.block_kind == kind.code())
            .count();
        if count == 0 {
            return Err(CacheReadError::Corrupt);
        }
        if matches!(
            kind,
            BlockKind::SourceManifest | BlockKind::EventFacts | BlockKind::StringTable
        ) && count != 1
        {
            return Err(CacheReadError::Corrupt);
        }
    }
    Ok(())
}

fn validate_logical_blocks(
    directory: &[BlockDirectoryEntry],
    bodies: &[&[u8]],
    identity: &HeaderIdentity,
    lineage: &SegmentIdentity,
    bounds: &Bounds,
) -> Result<(), CacheReadError> {
    let manifest_index = directory
        .iter()
        .position(|entry| entry.block_kind == BlockKind::SourceManifest.code())
        .ok_or(CacheReadError::Corrupt)?;
    let manifest = SourceManifestBlock::decode(bodies[manifest_index], bounds)?;
    verify_manifest_identity(&manifest, identity)?;
    let strings_index = directory
        .iter()
        .position(|entry| entry.block_kind == BlockKind::StringTable.code())
        .ok_or(CacheReadError::Corrupt)?;
    let strings = StringTableBlock::decode(bodies[strings_index], bounds)?;
    let mut referenced_strings = Vec::new();

    for (entry, body) in directory.iter().zip(bodies) {
        let Some(kind) = BlockKind::from_code(entry.block_kind) else {
            continue;
        };
        let logical = match kind {
            BlockKind::SourceManifest => logical_descriptor(&manifest),
            BlockKind::EventObservations => {
                let block = EventObservationsBlock::decode(body, lineage, &strings, bounds)?;
                validate_observation_provenance(&block, &manifest)?;
                referenced_strings.extend(block.string_table().values().iter().cloned());
                logical_descriptor(&block)
            }
            BlockKind::EventFacts => {
                if !body.is_empty() {
                    return Err(CacheReadError::Corrupt);
                }
                LogicalDescriptor {
                    sorted: true,
                    item_count: 0,
                    time_range: None,
                }
            }
            BlockKind::LossCoverage => {
                logical_descriptor(&LossCoverageBlock::decode(body, bounds)?)
            }
            BlockKind::GaugeSamples => {
                logical_descriptor(&GaugeSamplesBlock::decode(body, bounds)?)
            }
            BlockKind::CounterSamples => {
                logical_descriptor(&CounterSamplesBlock::decode(body, bounds)?)
            }
            BlockKind::ResetMarkers => {
                logical_descriptor(&ResetMarkersBlock::decode(body, bounds)?)
            }
            BlockKind::EntityStates => {
                logical_descriptor(&EntityStatesBlock::decode(body, bounds)?)
            }
            BlockKind::StringTable => logical_descriptor(&StringTableBlock::decode(body, bounds)?),
        };
        if logical.sorted != entry.flags.canonically_sorted
            || logical.item_count != u64::from(entry.item_count)
            || logical.time_range.is_some() != entry.flags.has_time_range
        {
            return Err(CacheReadError::Corrupt);
        }
        match logical.time_range {
            Some((minimum, maximum))
                if minimum != entry.min_ts_us || maximum != entry.max_ts_us =>
            {
                return Err(CacheReadError::Corrupt);
            }
            None if entry.min_ts_us != 0 || entry.max_ts_us != 0 => {
                return Err(CacheReadError::Corrupt);
            }
            _ => {}
        }
    }
    let referenced_strings = StringTableBlock::new(referenced_strings, bounds)?;
    if referenced_strings != strings {
        return Err(CacheReadError::Corrupt);
    }
    Ok(())
}

struct LogicalDescriptor {
    sorted: bool,
    item_count: u64,
    time_range: Option<(i64, i64)>,
}

fn logical_descriptor<B: EncodableBlock>(block: &B) -> LogicalDescriptor {
    LogicalDescriptor {
        sorted: block.canonically_sorted(),
        item_count: block.item_count(),
        time_range: block.time_range(),
    }
}

fn verify_manifest_identity(
    manifest: &SourceManifestBlock,
    identity: &HeaderIdentity,
) -> Result<(), CacheReadError> {
    if manifest.source_id() != identity.pgm_source_id
        || manifest.source_format_version() != identity.source_format_version
        || manifest.source_time_range() != (identity.source_min_ts_us, identity.source_max_ts_us)
        || manifest.source_file_len() != identity.source_file_len
    {
        return Err(CacheReadError::WrongSource);
    }
    Ok(())
}

fn validate_observation_provenance(
    observations: &EventObservationsBlock,
    manifest: &SourceManifestBlock,
) -> Result<(), CacheReadError> {
    for observation in observations.observations() {
        let provenance = observation.provenance();
        let ordinal = usize::try_from(provenance.catalog_entry_ordinal)
            .map_err(|_error| CacheReadError::Corrupt)?;
        let entry = manifest
            .entries()
            .get(ordinal)
            .ok_or(CacheReadError::Corrupt)?;
        if observation.source_type_id() != entry.catalog.type_id
            || Some(provenance.section_body_id) != entry.section_body_id
            || provenance.row_ordinal >= entry.catalog.rows
        {
            return Err(CacheReadError::Corrupt);
        }
    }
    Ok(())
}

fn expected_sorted(kind: BlockKind) -> bool {
    kind != BlockKind::SourceManifest
}

#[allow(
    clippy::too_many_arguments,
    reason = "arguments mirror the fixed directory entry"
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

fn empty_block(kind: BlockKind, bounds: &Bounds) -> Result<BlockContent, CacheReadError> {
    use kronika_analytics::overview::{
        Applicability, Coverage, PeriodQuality, PhysicalCountSemantics, RetainedExactness,
        SourceCompleteness,
    };

    match kind {
        BlockKind::SourceManifest => Err(CacheReadError::Corrupt),
        BlockKind::EventObservations => Ok(BlockContent::EventObservations(Box::new(
            EventObservationsBlock::new(Vec::new(), bounds)?,
        ))),
        BlockKind::EventFacts => Ok(BlockContent::EventFacts),
        BlockKind::LossCoverage => Ok(BlockContent::LossCoverage(Box::new(
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
            )?,
        ))),
        BlockKind::GaugeSamples => Ok(BlockContent::GaugeSamples(Box::new(
            GaugeSamplesBlock::new(Vec::new(), bounds)?,
        ))),
        BlockKind::CounterSamples => Ok(BlockContent::CounterSamples(Box::new(
            CounterSamplesBlock::new(Vec::new(), bounds)?,
        ))),
        BlockKind::ResetMarkers => Ok(BlockContent::ResetMarkers(Box::new(
            ResetMarkersBlock::new(Vec::new(), bounds)?,
        ))),
        BlockKind::EntityStates => Ok(BlockContent::EntityStates(Box::new(
            EntityStatesBlock::new(Vec::new(), bounds)?,
        ))),
        BlockKind::StringTable => Ok(BlockContent::StringTable(Box::new(StringTableBlock::new(
            Vec::new(),
            bounds,
        )?))),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use kronika_analytics::overview::{
        AlignmentId, CounterSample, DroppedFieldCount, EventObservation, EvidenceQuality,
        GaugeSample, LifecyclePayload, MetricSeriesId, NamingContractId, ObservationPayload,
        ObservationProvenance, ObservationShape, ObservationTime, QualityFlags, SectionBodyId,
        SegmentLocator, TimeQuality,
    };

    use super::super::descriptors::{CatalogEntryDescriptor, ManifestEntryDescriptor};
    use super::super::limits::LIMIT;
    use super::*;

    fn identity() -> HeaderIdentity {
        HeaderIdentity::from_current_contract(
            1,
            7,
            1_000,
            2_000,
            4_096,
            SourceScopeId([0x11; 32]),
            SourceDescriptor([0x22; 32]),
        )
    }

    fn lineage() -> SegmentIdentity {
        SegmentIdentity::sealed(
            identity().source_scope_id,
            NamingContractId([0x33; 16]),
            SegmentLocator([0x44; 32]),
            1_006_001,
            b"first",
        )
    }

    fn event_lineage() -> SegmentIdentity {
        SegmentIdentity::sealed(
            identity().source_scope_id,
            NamingContractId([0x33; 16]),
            SegmentLocator([0x44; 32]),
            1_028_001,
            b"event",
        )
    }

    fn manifest() -> SourceManifestBlock {
        SourceManifestBlock::new(
            7,
            1,
            1_000,
            2_000,
            4_096,
            vec![ManifestEntryDescriptor {
                catalog: CatalogEntryDescriptor {
                    type_id: 1_006_001,
                    flags: 0,
                    body_len: 12,
                    rows: 1,
                    body_crc32c: 7,
                },
                section_body_id: None,
            }],
            &LIMIT,
        )
        .expect("manifest")
    }

    fn sample_blocks() -> Vec<BlockContent> {
        let counter =
            CounterSample::new(MetricSeriesId([1; 16]), AlignmentId([1; 16]), 1_500, 5, 1);
        vec![
            BlockContent::SourceManifest(Box::new(manifest())),
            BlockContent::CounterSamples(Box::new(
                CounterSamplesBlock::new(vec![counter], &LIMIT).expect("counter"),
            )),
        ]
    }

    fn counter_file(samples: Vec<CounterSample>) -> Vec<u8> {
        FactFile::build(
            &identity(),
            vec![
                BlockContent::SourceManifest(Box::new(manifest())),
                BlockContent::CounterSamples(Box::new(
                    CounterSamplesBlock::new(samples, &LIMIT).expect("counter samples"),
                )),
            ],
            &LIMIT,
        )
        .expect("build counter file")
    }

    fn ready_observation() -> EventObservation {
        EventObservation::new(
            event_lineage(),
            1_028_001,
            ObservationProvenance {
                segment_locator: Some(SegmentLocator([0x44; 32])),
                section_body_id: SectionBodyId([0x66; 32]),
                catalog_entry_ordinal: 0,
                row_ordinal: 0,
                dictionary_context_id: kronika_analytics::overview::DictionaryContextId([0x77; 32]),
                source_locator: None,
            },
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: 1_500,
                occurred_at_us: Some(1_500),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ReadyObserved(Box::new(LifecyclePayload {
                pid: None,
                signal: None,
                shutdown_mode: None,
                message: Some("database system is ready".into()),
                query_detail: None,
                dropped_field_count: DroppedFieldCount(0),
            })),
            EvidenceQuality::Structured,
            QualityFlags(0),
            None,
        )
        .expect("observation")
    }

    fn event_manifest() -> SourceManifestBlock {
        SourceManifestBlock::new(
            7,
            1,
            1_000,
            2_000,
            4_096,
            vec![ManifestEntryDescriptor {
                catalog: CatalogEntryDescriptor {
                    type_id: 1_028_001,
                    flags: 0,
                    body_len: 128,
                    rows: 1,
                    body_crc32c: 9,
                },
                section_body_id: Some(SectionBodyId([0x66; 32])),
            }],
            &LIMIT,
        )
        .expect("event manifest")
    }

    fn observation_file() -> Vec<u8> {
        let observations = EventObservationsBlock::new(vec![ready_observation()], &LIMIT)
            .expect("observation block");
        FactFile::build(
            &identity(),
            vec![
                BlockContent::SourceManifest(Box::new(event_manifest())),
                BlockContent::EventObservations(Box::new(observations)),
            ],
            &LIMIT,
        )
        .expect("build observation file")
    }

    fn valid_file() -> Vec<u8> {
        FactFile::build(&identity(), sample_blocks(), &LIMIT).expect("build")
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("field"))
    }

    fn u64_at(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("field"))
    }

    fn reseal_header(bytes: &mut [u8]) {
        bytes[HEADER_CRC_OFFSET..HEADER_LEN].fill(0);
        let checksum = crc32c(&bytes[..HEADER_LEN]);
        bytes[HEADER_CRC_OFFSET..HEADER_LEN].copy_from_slice(&checksum.to_le_bytes());
    }

    fn reseal_directory(bytes: &mut [u8]) {
        let count = u32_at(bytes, 136) as usize;
        let end = HEADER_LEN + count * DIRECTORY_ENTRY_LEN;
        let checksum = crc32c(&bytes[HEADER_LEN..end]);
        bytes[152..156].copy_from_slice(&checksum.to_le_bytes());
        reseal_header(bytes);
    }

    fn entry_field(index: usize, offset: usize) -> usize {
        HEADER_LEN + index * DIRECTORY_ENTRY_LEN + offset
    }

    fn entry_index(bytes: &[u8], kind: BlockKind) -> usize {
        let count = u32_at(bytes, 136) as usize;
        (0..count)
            .find(|&index| u32_at(bytes, entry_field(index, 0)) == kind.code())
            .expect("block kind is present")
    }

    fn entry_body_range(bytes: &[u8], index: usize) -> std::ops::Range<usize> {
        let offset =
            usize::try_from(u64_at(bytes, entry_field(index, 16))).expect("body offset fits usize");
        let length =
            usize::try_from(u64_at(bytes, entry_field(index, 24))).expect("body length fits usize");
        offset..offset + length
    }

    fn reseal_block(bytes: &mut [u8], index: usize) {
        let body = entry_body_range(bytes, index);
        let checksum = crc32c(&bytes[body]);
        let checksum_at = entry_field(index, 44);
        bytes[checksum_at..checksum_at + 4].copy_from_slice(&checksum.to_le_bytes());
        reseal_directory(bytes);
    }

    fn counter_partition(
        offset: u64,
        item_count: u32,
        min_ts_us: i64,
        max_ts_us: i64,
    ) -> BlockDirectoryEntry {
        BlockDirectoryEntry {
            block_kind: BlockKind::CounterSamples.code(),
            block_schema_version: BLOCK_SCHEMA_VERSION,
            flags: BlockFlags {
                required_for_schema: true,
                canonically_sorted: true,
                has_time_range: true,
                codec: BlockCodec::None,
            },
            logical_id: 1,
            offset,
            stored_len: 1,
            decoded_len: 1,
            item_count,
            block_crc32c: 0,
            min_ts_us,
            max_ts_us,
        }
    }

    fn append_empty_optional_block(bytes: &mut Vec<u8>) {
        let old_count = u32_at(bytes, 136) as usize;
        let old_directory_end = HEADER_LEN + old_count * DIRECTORY_ENTRY_LEN;
        let old_file_len = u64::from_le_bytes(bytes[144..152].try_into().expect("file length"));
        bytes.splice(
            old_directory_end..old_directory_end,
            [0_u8; DIRECTORY_ENTRY_LEN],
        );
        for index in 0..old_count {
            let at = entry_field(index, 16);
            let old_offset = u64::from_le_bytes(bytes[at..at + 8].try_into().expect("offset"));
            bytes[at..at + 8]
                .copy_from_slice(&(old_offset + DIRECTORY_ENTRY_LEN as u64).to_le_bytes());
        }
        let optional = entry_field(old_count, 0);
        bytes[optional..optional + 4].copy_from_slice(&9_999_u32.to_le_bytes());
        bytes[optional + 4..optional + 6].copy_from_slice(&77_u16.to_le_bytes());
        bytes[optional + 16..optional + 24]
            .copy_from_slice(&(old_file_len + DIRECTORY_ENTRY_LEN as u64).to_le_bytes());
        bytes[optional + 44..optional + 48].copy_from_slice(&crc32c(&[]).to_le_bytes());
        let new_count = u32::try_from(old_count + 1).expect("directory count fits u32");
        bytes[136..140].copy_from_slice(&new_count.to_le_bytes());
        bytes[144..152].copy_from_slice(&(old_file_len + DIRECTORY_ENTRY_LEN as u64).to_le_bytes());
        reseal_directory(bytes);
    }

    #[test]
    fn canonical_build_admits_and_empty_blocks_have_zero_lengths() {
        let bytes = valid_file();
        let admitted = FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT).expect("admit");
        assert_eq!(admitted.header().file_len, bytes.len() as u64);
        for entry in admitted.directory() {
            if entry.item_count == 0 {
                assert_eq!(entry.stored_len, 0);
                assert_eq!(entry.decoded_len, 0);
                assert!(!entry.flags.has_time_range);
                assert_eq!((entry.min_ts_us, entry.max_ts_us), (0, 0));
            }
        }
    }

    #[test]
    fn header_crc_covers_the_zeroed_checksum_field() {
        let mut bytes = valid_file();
        bytes[HEADER_CRC_OFFSET] ^= 1;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_rejects_a_stale_directory_crc() {
        let mut bytes = valid_file();
        bytes[entry_field(0, 0)] ^= 1;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_rejects_a_stale_block_crc() {
        let mut bytes = valid_file();
        let counter = entry_index(&bytes, BlockKind::CounterSamples);
        let body = entry_body_range(&bytes, counter);
        bytes[body.start] ^= 1;
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_distinguishes_wrong_source_from_incompatible_versions() {
        let bytes = valid_file();
        let mut wrong_scope = identity();
        wrong_scope.source_scope_id.0[0] ^= 1;
        let mut wrong_descriptor = identity();
        wrong_descriptor.source_descriptor.0[0] ^= 1;
        for expected in [&wrong_scope, &wrong_descriptor] {
            assert!(matches!(
                FactFile::admit(&bytes, expected, &lineage(), &LIMIT),
                Err(CacheReadError::WrongSource)
            ));
        }

        let mut incompatible = identity();
        incompatible.fact_schema_version += 1;
        assert!(matches!(
            FactFile::admit(&bytes, &incompatible, &lineage(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    #[allow(
        clippy::format_collect,
        reason = "a compact hex string keeps the 160-byte golden header reviewable"
    )]
    fn canonical_header_matches_the_golden_vector() {
        let header = encode_header(&identity(), 9, 1_234, 0x1122_3344);
        let encoded: String = header.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            encoded,
            concat!(
                "50474b4f564600000100a0000100000001000000010000000100000001000000",
                "0700000000000000e803000000000000d0070000000000000010000000000000",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222222222222222222222222222",
                "a0000000000000000900000040000100d2040000000000004433221144b2353b",
            )
        );
    }

    #[test]
    fn known_schema_flags_and_time_metadata_are_enforced() {
        for (entry, field, value) in [(0, 4, 2_u8), (0, 6, 0_u8), (5, 6, 3_u8)] {
            let mut bytes = valid_file();
            bytes[entry_field(entry, field)] = value;
            reseal_directory(&mut bytes);
            assert!(
                FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT).is_err(),
                "entry {entry}, field {field}"
            );
        }
    }

    #[test]
    fn admission_rejects_resealed_unsorted_counter_samples() {
        let series = MetricSeriesId([1; 16]);
        let alignment = AlignmentId([1; 16]);
        let mut bytes = counter_file(vec![
            CounterSample::new(series, alignment, 1_500, 5, 1),
            CounterSample::new(series, alignment, 1_600, 6, 1),
        ]);
        let counter = entry_index(&bytes, BlockKind::CounterSamples);
        let body = entry_body_range(&bytes, counter);
        let record_len = (body.len() - 1) / 2;
        let first = bytes[body.start + 1..body.start + 1 + record_len].to_vec();
        let second = bytes[body.start + 1 + record_len..body.start + 1 + 2 * record_len].to_vec();
        bytes[body.start + 1..body.start + 1 + record_len].copy_from_slice(&second);
        bytes[body.start + 1 + record_len..body.start + 1 + 2 * record_len].copy_from_slice(&first);
        reseal_block(&mut bytes, counter);

        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_rejects_resealed_duplicate_counter_samples() {
        let series = MetricSeriesId([1; 16]);
        let alignment = AlignmentId([1; 16]);
        let mut bytes = counter_file(vec![
            CounterSample::new(series, alignment, 1_500, 5, 1),
            CounterSample::new(series, alignment, 1_600, 6, 1),
        ]);
        let counter = entry_index(&bytes, BlockKind::CounterSamples);
        let body = entry_body_range(&bytes, counter);
        let record_len = (body.len() - 1) / 2;
        let first = bytes[body.start + 1..body.start + 1 + record_len].to_vec();
        bytes[body.start + 1 + record_len..body.start + 1 + 2 * record_len].copy_from_slice(&first);
        reseal_block(&mut bytes, counter);

        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_rejects_a_resealed_non_finite_gauge() {
        let gauge = GaugeSamplesBlock::new(
            vec![GaugeSample::new(MetricSeriesId([2; 16]), 1_500, 1.0).expect("finite gauge")],
            &LIMIT,
        )
        .expect("gauge block");
        let mut bytes = FactFile::build(
            &identity(),
            vec![
                BlockContent::SourceManifest(Box::new(manifest())),
                BlockContent::GaugeSamples(Box::new(gauge)),
            ],
            &LIMIT,
        )
        .expect("build gauge file");
        let gauge = entry_index(&bytes, BlockKind::GaugeSamples);
        let body = entry_body_range(&bytes, gauge);
        let value = body.start + 1 + 16 + 8;
        bytes[value..value + 8].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
        reseal_block(&mut bytes, gauge);

        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_rejects_a_resealed_invalid_coverage_enum() {
        let mut bytes = valid_file();
        let coverage = entry_index(&bytes, BlockKind::LossCoverage);
        let body = entry_body_range(&bytes, coverage);
        bytes[body.start + 2] = u8::MAX;
        reseal_block(&mut bytes, coverage);

        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn admission_rejects_directory_item_count_mismatch() {
        let mut bytes = valid_file();
        let at = entry_field(5, 40);
        bytes[at..at + 4].copy_from_slice(&2_u32.to_le_bytes());
        reseal_directory(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn source_manifest_must_match_the_header() {
        let wrong = SourceManifestBlock::new(8, 1, 1_000, 2_000, 4_096, Vec::new(), &LIMIT)
            .expect("manifest");
        assert!(matches!(
            FactFile::build(
                &identity(),
                vec![BlockContent::SourceManifest(Box::new(wrong))],
                &LIMIT
            ),
            Err(CacheReadError::WrongSource)
        ));
    }

    #[test]
    fn admission_rejects_a_resealed_manifest_header_mismatch() {
        let mut bytes = valid_file();
        let manifest = entry_index(&bytes, BlockKind::SourceManifest);
        let body = entry_body_range(&bytes, manifest);
        bytes[body.start] ^= 1;
        reseal_block(&mut bytes, manifest);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::WrongSource)
        ));
    }

    #[test]
    fn caller_cannot_expand_absolute_limits() {
        let expanded = Bounds {
            fact_file_len: LIMIT.fact_file_len + 1,
            ..LIMIT
        };
        assert!(matches!(
            FactFile::admit(&valid_file(), &identity(), &lineage(), &expanded),
            Err(CacheReadError::Oversized)
        ));
    }

    #[test]
    fn directory_aggregate_bounds_and_partition_ranges_are_enforced() {
        let directory_end = HEADER_LEN_U64 + 2 * DIRECTORY_ENTRY_LEN as u64;
        let header = FactFileHeader {
            identity: identity(),
            directory_offset: HEADER_LEN_U64,
            directory_count: 2,
            file_len: directory_end + 2,
        };
        let disjoint = [
            counter_partition(directory_end, 1, 1_000, 1_200),
            counter_partition(directory_end + 1, 1, 1_300, 1_600),
        ];

        let decoded_bound = Bounds {
            decoded_file_bytes: 1,
            ..LIMIT
        };
        assert!(matches!(
            validate_directory(&disjoint, &header, directory_end, &decoded_bound),
            Err(CacheReadError::Oversized)
        ));

        let item_bound = Bounds {
            items_per_block: 1,
            ..LIMIT
        };
        assert!(matches!(
            validate_directory(&disjoint, &header, directory_end, &item_bound),
            Err(CacheReadError::Oversized)
        ));

        let overlapping = [
            counter_partition(directory_end, 1, 1_000, 1_500),
            counter_partition(directory_end + 1, 1, 1_500, 1_600),
        ];
        assert!(matches!(
            validate_directory(&overlapping, &header, directory_end, &LIMIT),
            Err(CacheReadError::Corrupt)
        ));
    }

    #[test]
    fn unknown_required_blocks_are_incompatible() {
        let mut bytes = valid_file();
        bytes[entry_field(0, 0)..entry_field(0, 0) + 4].copy_from_slice(&9_999_u32.to_le_bytes());
        reseal_directory(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn unknown_optional_blocks_are_skipped() {
        let mut bytes = valid_file();
        append_empty_optional_block(&mut bytes);
        let admitted =
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT).expect("optional block");
        assert_eq!(admitted.directory().len(), BlockKind::ALL.len() + 1);
        assert_eq!(
            admitted.directory().last().expect("entry").block_kind,
            9_999
        );
    }

    #[test]
    fn zstd_is_rejected_before_any_decompression_allocation() {
        let mut bytes = valid_file();
        let flags_at = entry_field(0, 6);
        let flags = u16::from_le_bytes(bytes[flags_at..flags_at + 2].try_into().expect("flags"));
        bytes[flags_at..flags_at + 2].copy_from_slice(&(flags | 0x0100).to_le_bytes());
        reseal_directory(&mut bytes);
        assert!(matches!(
            FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT),
            Err(CacheReadError::Incompatible)
        ));
    }

    #[test]
    fn encoding_is_deterministic() {
        assert_eq!(valid_file(), valid_file());
    }

    #[test]
    fn observations_use_manifest_provenance_and_text_references() {
        let bytes = observation_file();
        let admitted =
            FactFile::admit(&bytes, &identity(), &event_lineage(), &LIMIT).expect("admit");
        let observation_body = admitted
            .block_body(BlockKind::EventObservations)
            .expect("observation body");
        assert!(
            !observation_body
                .windows(b"database system is ready".len())
                .any(|window| window == b"database system is ready")
        );
        let strings = StringTableBlock::decode(
            admitted
                .block_body(BlockKind::StringTable)
                .expect("string table"),
            &LIMIT,
        )
        .expect("decode strings");
        assert_eq!(
            strings.values(),
            &[Box::from(b"database system is ready".as_slice())]
        );
    }

    #[test]
    fn admission_rejects_resealed_observation_provenance_mismatches() {
        const SOURCE_TYPE_OFFSET: usize = 1;
        const SECTION_BODY_ID_OFFSET: usize = 38;
        const CATALOG_ORDINAL_OFFSET: usize = 70;
        const ROW_ORDINAL_OFFSET: usize = 74;

        #[derive(Debug, Clone, Copy)]
        enum Mutation {
            SourceType,
            SectionBody,
            CatalogOrdinal,
            RowOrdinal,
        }

        for mutation in [
            Mutation::SourceType,
            Mutation::SectionBody,
            Mutation::CatalogOrdinal,
            Mutation::RowOrdinal,
        ] {
            let mut bytes = observation_file();
            let observations = entry_index(&bytes, BlockKind::EventObservations);
            let body = entry_body_range(&bytes, observations);
            match mutation {
                Mutation::SourceType => bytes
                    [body.start + SOURCE_TYPE_OFFSET..body.start + SOURCE_TYPE_OFFSET + 4]
                    .copy_from_slice(&1_027_001_u32.to_le_bytes()),
                Mutation::SectionBody => bytes[body.start + SECTION_BODY_ID_OFFSET] ^= 1,
                Mutation::CatalogOrdinal => bytes
                    [body.start + CATALOG_ORDINAL_OFFSET..body.start + CATALOG_ORDINAL_OFFSET + 4]
                    .copy_from_slice(&1_u32.to_le_bytes()),
                Mutation::RowOrdinal => bytes
                    [body.start + ROW_ORDINAL_OFFSET..body.start + ROW_ORDINAL_OFFSET + 4]
                    .copy_from_slice(&1_u32.to_le_bytes()),
            }
            reseal_block(&mut bytes, observations);
            assert!(
                matches!(
                    FactFile::admit(&bytes, &identity(), &event_lineage(), &LIMIT),
                    Err(CacheReadError::Corrupt)
                ),
                "mutation {mutation:?} was admitted"
            );
        }
    }

    #[derive(Debug)]
    struct CountingReader<'a> {
        bytes: &'a [u8],
        calls: Rc<Cell<u64>>,
        read_bytes: Rc<Cell<u64>>,
    }

    impl ReadAt for CountingReader<'_> {
        fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
            self.calls.set(self.calls.get() + 1);
            self.read_bytes
                .set(self.read_bytes.get() + buf.len() as u64);
            self.bytes.read_exact_at(buf, offset)
        }

        fn byte_len(&self) -> std::io::Result<u64> {
            Ok(self.bytes.len() as u64)
        }
    }

    #[test]
    fn positional_reader_reads_only_metadata_and_selected_bodies() {
        let bytes = valid_file();
        let calls = Rc::new(Cell::new(0));
        let read_bytes = Rc::new(Cell::new(0));
        let reader = CountingReader {
            bytes: &bytes,
            calls: Rc::clone(&calls),
            read_bytes: Rc::clone(&read_bytes),
        };
        let mut file = FactFileReader::open(reader, &identity(), &LIMIT).expect("open");
        let metadata_bytes = HEADER_LEN_U64 + BlockKind::ALL.len() as u64 * 64;
        assert_eq!(file.stats().read_calls, 2);
        assert_eq!(file.stats().stored_bytes_read, metadata_bytes);
        assert_eq!(calls.get(), file.stats().read_calls);
        assert_eq!(read_bytes.get(), file.stats().stored_bytes_read);

        let bodies = file
            .read_blocks(BlockKind::CounterSamples)
            .expect("selected body");
        assert_eq!(bodies.len(), 1);
        let counter = file
            .directory()
            .iter()
            .find(|entry| entry.block_kind == BlockKind::CounterSamples.code())
            .expect("entry");
        assert_eq!(
            file.stats().stored_bytes_read,
            metadata_bytes + counter.stored_len
        );
        assert_eq!(file.stats().decoded_bytes, counter.decoded_len);
        assert_eq!(calls.get(), file.stats().read_calls);
        assert_eq!(read_bytes.get(), file.stats().stored_bytes_read);
        assert!(file.stats().stored_bytes_read < bytes.len() as u64);

        let before = file.stats();
        let outside = file
            .read_blocks_in_range(BlockKind::CounterSamples, 1_600, 1_700)
            .expect("range read");
        assert!(outside.is_empty());
        assert_eq!(file.stats(), before);
        assert_eq!(calls.get(), file.stats().read_calls);
        assert_eq!(read_bytes.get(), file.stats().stored_bytes_read);
    }

    #[test]
    fn positional_reader_verifies_the_selected_body_crc() {
        let mut bytes = valid_file();
        let counter = entry_index(&bytes, BlockKind::CounterSamples);
        let body = entry_body_range(&bytes, counter);
        bytes[body.start] ^= 1;

        let calls = Rc::new(Cell::new(0));
        let read_bytes = Rc::new(Cell::new(0));
        let reader = CountingReader {
            bytes: &bytes,
            calls: Rc::clone(&calls),
            read_bytes: Rc::clone(&read_bytes),
        };
        let mut file = FactFileReader::open(reader, &identity(), &LIMIT).expect("metadata");
        let metadata_bytes = HEADER_LEN_U64 + BlockKind::ALL.len() as u64 * 64;
        assert_eq!(calls.get(), 2);
        assert_eq!(read_bytes.get(), metadata_bytes);

        let counter_len = file.directory()[counter].stored_len;
        assert!(matches!(
            file.read_blocks(BlockKind::CounterSamples),
            Err(CacheReadError::Corrupt)
        ));
        assert_eq!(calls.get(), 3);
        assert_eq!(read_bytes.get(), metadata_bytes + counter_len);
        assert_eq!(file.stats().read_calls, calls.get());
        assert_eq!(file.stats().stored_bytes_read, read_bytes.get());
        assert_eq!(file.stats().decoded_bytes, 0);
        assert!(file.stats().stored_bytes_read < bytes.len() as u64);
    }

    #[test]
    fn arbitrary_truncation_returns_an_error() {
        let bytes = valid_file();
        for end in 0..bytes.len() {
            assert!(
                FactFile::admit(&bytes[..end], &identity(), &lineage(), &LIMIT).is_err(),
                "accepted prefix of {end} bytes"
            );
        }
    }
}
