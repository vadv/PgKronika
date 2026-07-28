//! Logical fact blocks and their canonical byte encodings.
//!
//! A block holds one kind of retained fact — counter samples, gauge samples,
//! coverage, observations — as a sorted, bounded, self-describing byte run.
//! Encoding follows the canonical rules: fixed-width little-endian scalars,
//! canonical `LEB128` counts and lengths, sorted-unique vectors, finite
//! floats, and a decoder that must consume the whole block.
//!
//! Each block round-trips through [`kronika_analytics::overview`] types without
//! reinterpreting them. Physical framing — offsets, CRC, compression — belongs
//! to the container, not here.

use kronika_analytics::overview::{
    AlignmentId, Applicability, BoundaryQuality, CadenceEpochId, CounterSample, Coverage,
    CoverageSpan, CoverageState, EntityKind, EntityRef, FactorCoverage, FactorId, GaugeSample,
    LossReason, MetricSeriesDescriptor, MetricSeriesId, MetricUnit, PeriodQuality,
    PhysicalCountSemantics, PopulationTotalQuality, ResetFamily, RetainedExactness,
    SourceCompleteness, SourcePopulation,
};

use super::bytes::{ByteError, ByteReader, ByteWriter};
use super::descriptors::{CatalogEntryDescriptor, ManifestEntryDescriptor};
use super::limits::Bounds;

/// A canonical block kind in the fact file.
///
/// Unknown kinds are handled by the container through the required-for-schema
/// flag, not by this enum. [`BlockKind::BASELINE`] distinguishes mandatory
/// singleton-or-partitioned blocks from known repeatable extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlockKind {
    /// Catalog inventory, PGM layout, and source/range provenance.
    SourceManifest,
    /// Retained source-shaped observations in canonical order.
    EventObservations,
    /// Policy-neutral normalized facts and their observation links.
    EventFacts,
    /// Coverage intervals, gaps, caps, and completeness quality.
    LossCoverage,
    /// Timestamped gauge values with series identity.
    GaugeSamples,
    /// Timestamped cumulative counter values with reset epoch.
    CounterSamples,
    /// Per-family reset, postmaster, and source epoch boundaries.
    ResetMarkers,
    /// Bounded entity snapshots for proven transitions.
    EntityStates,
    /// Bounded canonical text retained by explicit reference.
    StringTable,
    /// Shared web UI snapshot times, populations, and view status.
    UiSummary,
    /// Independently addressable top-K and heatmap series for one UI view.
    EntitySeries,
}

impl BlockKind {
    /// Required kinds in stable code order.
    pub const BASELINE: [Self; 10] = [
        Self::SourceManifest,
        Self::EventObservations,
        Self::EventFacts,
        Self::LossCoverage,
        Self::GaugeSamples,
        Self::CounterSamples,
        Self::ResetMarkers,
        Self::EntityStates,
        Self::StringTable,
        Self::UiSummary,
    ];

    /// Every understood kind in stable code order.
    pub const KNOWN: [Self; 11] = [
        Self::SourceManifest,
        Self::EventObservations,
        Self::EventFacts,
        Self::LossCoverage,
        Self::GaugeSamples,
        Self::CounterSamples,
        Self::ResetMarkers,
        Self::EntityStates,
        Self::StringTable,
        Self::UiSummary,
        Self::EntitySeries,
    ];

    /// The stable on-disk block-kind code.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::SourceManifest => 1,
            Self::EventObservations => 2,
            Self::EventFacts => 3,
            Self::LossCoverage => 4,
            Self::GaugeSamples => 5,
            Self::CounterSamples => 6,
            Self::ResetMarkers => 7,
            Self::EntityStates => 8,
            Self::StringTable => 9,
            Self::UiSummary => 10,
            Self::EntitySeries => 11,
        }
    }

    /// The kind for a code, or `None` for an unknown code.
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::SourceManifest),
            2 => Some(Self::EventObservations),
            3 => Some(Self::EventFacts),
            4 => Some(Self::LossCoverage),
            5 => Some(Self::GaugeSamples),
            6 => Some(Self::CounterSamples),
            7 => Some(Self::ResetMarkers),
            8 => Some(Self::EntityStates),
            9 => Some(Self::StringTable),
            10 => Some(Self::UiSummary),
            11 => Some(Self::EntitySeries),
            _ => None,
        }
    }
}

/// The block-body compression codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockCodec {
    /// Stored bytes equal decoded bytes.
    None,
    /// Zstandard-compressed stored bytes.
    Zstd,
}

/// A parsed set of block directory flags.
///
/// Bit 0 marks a required block, bit 1 canonical order, bit 2 a time range,
/// and bits 8..11 the codec. Other set bits are incompatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockFlags {
    /// Whether a reader that cannot decode this block must reject the file.
    pub required_for_schema: bool,
    /// Whether the block body is in canonical sort order.
    pub canonically_sorted: bool,
    /// Whether the directory timestamps describe the block's items.
    pub has_time_range: bool,
    /// The block-body codec.
    pub codec: BlockCodec,
}

impl BlockFlags {
    const REQUIRED_BIT: u16 = 1 << 0;
    const SORTED_BIT: u16 = 1 << 1;
    const TIME_RANGE_BIT: u16 = 1 << 2;
    const CODEC_SHIFT: u16 = 8;
    const CODEC_MASK: u16 = 0x0F00;
    const KNOWN_MASK: u16 =
        Self::REQUIRED_BIT | Self::SORTED_BIT | Self::TIME_RANGE_BIT | Self::CODEC_MASK;

    /// Encodes the flags to their raw directory field.
    #[must_use]
    pub const fn to_bits(self) -> u16 {
        let codec = match self.codec {
            BlockCodec::None => 0,
            BlockCodec::Zstd => 1,
        };
        let mut bits = codec << Self::CODEC_SHIFT;
        if self.required_for_schema {
            bits |= Self::REQUIRED_BIT;
        }
        if self.canonically_sorted {
            bits |= Self::SORTED_BIT;
        }
        if self.has_time_range {
            bits |= Self::TIME_RANGE_BIT;
        }
        bits
    }

    /// Parses raw directory flags, rejecting reserved bits and unknown codecs.
    ///
    /// # Errors
    /// Returns [`BlockError::InvalidFlags`] for any reserved bit set or an
    /// unknown codec value.
    pub const fn from_bits(bits: u16) -> Result<Self, BlockError> {
        if bits & !Self::KNOWN_MASK != 0 {
            return Err(BlockError::InvalidFlags);
        }
        let codec = match (bits & Self::CODEC_MASK) >> Self::CODEC_SHIFT {
            0 => BlockCodec::None,
            1 => BlockCodec::Zstd,
            _ => return Err(BlockError::InvalidFlags),
        };
        Ok(Self {
            required_for_schema: bits & Self::REQUIRED_BIT != 0,
            canonically_sorted: bits & Self::SORTED_BIT != 0,
            has_time_range: bits & Self::TIME_RANGE_BIT != 0,
            codec,
        })
    }
}

/// Why a logical block failed to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// A read needed more bytes than the block held.
    Truncated,
    /// A count or byte length exceeded a safety bound.
    AboveBound,
    /// A varint or structural field was malformed.
    Malformed,
    /// A vector expected in canonical order was out of order.
    Unsorted,
    /// A sorted-unique vector carried a duplicate key.
    Duplicate,
    /// A float field decoded to `NaN` or an infinity.
    NonFiniteFloat,
    /// The decoder left unread trailing bytes.
    TrailingBytes,
    /// An enum discriminant was outside its defined range.
    InvalidEnum,
    /// A directory flag word set a reserved bit or unknown codec.
    InvalidFlags,
    /// A decoded record was rejected by its analytics constructor.
    Reconstruct,
}

impl From<ByteError> for BlockError {
    fn from(value: ByteError) -> Self {
        match value {
            ByteError::Truncated => Self::Truncated,
            ByteError::AboveBound => Self::AboveBound,
            ByteError::VarintTooLong | ByteError::VarintNonCanonical => Self::Malformed,
            ByteError::TrailingBytes => Self::TrailingBytes,
            ByteError::NonFiniteFloat => Self::NonFiniteFloat,
        }
    }
}

/// A block that can be encoded and described for the directory.
pub(crate) trait EncodableBlock {
    /// The block kind, for the directory entry.
    fn kind(&self) -> BlockKind;
    /// Whether the encoded body is in canonical sort order.
    fn canonically_sorted(&self) -> bool;
    /// The number of logical items, for the directory entry.
    fn item_count(&self) -> u64;
    /// The inclusive timestamp range of the items, when they carry time.
    fn time_range(&self) -> Option<(i64, i64)>;
    /// The canonical decoded body bytes.
    fn encode(&self) -> Vec<u8>;
}

fn write_series_id(writer: &mut ByteWriter, id: MetricSeriesId) {
    writer.bytes(&id.0);
}

fn read_series_id(reader: &mut ByteReader<'_>) -> Result<MetricSeriesId, ByteError> {
    Ok(MetricSeriesId(reader.array()?))
}

fn write_alignment_id(writer: &mut ByteWriter, id: AlignmentId) {
    writer.bytes(&id.0);
}

fn read_alignment_id(reader: &mut ByteReader<'_>) -> Result<AlignmentId, ByteError> {
    Ok(AlignmentId(reader.array()?))
}

fn time_range_of(times: impl IntoIterator<Item = i64>) -> Option<(i64, i64)> {
    let mut iter = times.into_iter();
    let first = iter.next()?;
    Some(iter.fold((first, first), |(lo, hi), ts| (lo.min(ts), hi.max(ts))))
}

fn untyped_series_for_counters(samples: &[CounterSample]) -> Vec<MetricSeriesDescriptor> {
    untyped_series(samples.iter().map(|sample| sample.series_id()))
}

fn untyped_series_for_gauges(samples: &[GaugeSample]) -> Vec<MetricSeriesDescriptor> {
    untyped_series(samples.iter().map(|sample| sample.series_id()))
}

fn untyped_series(
    series_ids: impl IntoIterator<Item = MetricSeriesId>,
) -> Vec<MetricSeriesDescriptor> {
    let mut ids = series_ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .map(|series_id| MetricSeriesDescriptor {
            series_id,
            factor_id: FactorId(0),
            section_type_id: 0,
            unit: MetricUnit::Count,
            entity: None,
            reset_family: None,
        })
        .collect()
}

fn normalize_series(series: &mut [MetricSeriesDescriptor]) -> Result<(), BlockError> {
    series.sort_unstable_by_key(|descriptor| descriptor.series_id);
    if series
        .windows(2)
        .any(|pair| pair[0].series_id == pair[1].series_id)
    {
        return Err(BlockError::Duplicate);
    }
    Ok(())
}

fn validate_sample_series(
    series: &[MetricSeriesDescriptor],
    sample_ids: impl IntoIterator<Item = MetricSeriesId>,
) -> Result<(), BlockError> {
    let mut referenced = Vec::new();
    for id in sample_ids {
        if series
            .binary_search_by_key(&id, |descriptor| descriptor.series_id)
            .is_err()
        {
            return Err(BlockError::Reconstruct);
        }
        referenced.push(id);
    }
    referenced.sort_unstable();
    referenced.dedup();
    if referenced.len() != series.len()
        || referenced
            .iter()
            .zip(series)
            .any(|(id, descriptor)| *id != descriptor.series_id)
    {
        return Err(BlockError::Reconstruct);
    }
    Ok(())
}

fn write_series_descriptors(writer: &mut ByteWriter, series: &[MetricSeriesDescriptor]) {
    writer.uvarint(series.len() as u64);
    for descriptor in series {
        write_series_id(writer, descriptor.series_id);
        writer.u32_le(descriptor.factor_id.0);
        writer.u32_le(descriptor.section_type_id);
        writer.u8(descriptor.unit.code());
        match descriptor.entity {
            Some(entity) => {
                writer.u8(1);
                writer.u8(entity.kind.code());
                writer.bytes(&entity.id);
            }
            None => writer.u8(0),
        }
        match descriptor.reset_family {
            Some(family) => {
                writer.u8(1);
                writer.u8(family.code());
            }
            None => writer.u8(0),
        }
    }
}

fn read_series_descriptors(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Vec<MetricSeriesDescriptor>, BlockError> {
    let count = reader.uvarint(bounds.items_per_block)?;
    let mut series = Vec::with_capacity(count.min(4_096) as usize);
    for _ in 0..count {
        let series_id = read_series_id(reader)?;
        let factor_id = FactorId(reader.u32_le()?);
        let section_type_id = reader.u32_le()?;
        let unit = MetricUnit::from_code(reader.u8()?).ok_or(BlockError::InvalidEnum)?;
        let entity = match reader.u8()? {
            0 => None,
            1 => Some(EntityRef {
                kind: EntityKind::from_code(reader.u8()?).ok_or(BlockError::InvalidEnum)?,
                id: reader.array()?,
            }),
            _ => return Err(BlockError::InvalidEnum),
        };
        let reset_family = match reader.u8()? {
            0 => None,
            1 => Some(ResetFamily::from_code(reader.u8()?).ok_or(BlockError::InvalidEnum)?),
            _ => return Err(BlockError::InvalidEnum),
        };
        series.push(MetricSeriesDescriptor {
            series_id,
            factor_id,
            section_type_id,
            unit,
            entity,
            reset_family,
        });
    }
    for pair in series.windows(2) {
        match pair[0].series_id.cmp(&pair[1].series_id) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
            std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
        }
    }
    Ok(series)
}

/// A canonical, sorted-unique set of cumulative counter samples.
///
/// The canonical order is `(series_id, alignment_id, ts_us)`; two samples
/// sharing that key are a duplicate and never both retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterSamplesBlock {
    series: Vec<MetricSeriesDescriptor>,
    samples: Vec<CounterSample>,
}

impl CounterSamplesBlock {
    /// Normalizes samples into canonical sorted-unique order.
    ///
    /// # Errors
    /// Returns [`BlockError::Duplicate`] when two samples share the canonical
    /// key, and [`BlockError::AboveBound`] past the per-block item bound.
    pub fn new(samples: Vec<CounterSample>, bounds: &Bounds) -> Result<Self, BlockError> {
        let series = untyped_series_for_counters(&samples);
        Self::new_with_series(series, samples, bounds)
    }

    /// Normalizes typed series metadata and samples into canonical order.
    ///
    /// Every retained sample must reference exactly one descriptor. Production
    /// extraction uses this constructor so factor, unit, source scope, entity
    /// and reset family are never inferred by the disk codec.
    ///
    /// # Errors
    /// Returns [`BlockError`] for duplicate descriptors/samples, an unmapped
    /// sample, contradictory descriptor identity, or a configured bound.
    pub fn new_with_series(
        mut series: Vec<MetricSeriesDescriptor>,
        mut samples: Vec<CounterSample>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if samples.len() as u64 > bounds.items_per_block
            || series.len() as u64 > bounds.items_per_block
        {
            return Err(BlockError::AboveBound);
        }
        normalize_series(&mut series)?;
        samples.sort_unstable_by_key(counter_key);
        if samples
            .windows(2)
            .any(|w| counter_key(&w[0]) == counter_key(&w[1]))
        {
            return Err(BlockError::Duplicate);
        }
        validate_sample_series(&series, samples.iter().map(|sample| sample.series_id()))?;
        Ok(Self { series, samples })
    }

    /// Typed metadata of every referenced series.
    #[must_use]
    pub fn series(&self) -> &[MetricSeriesDescriptor] {
        &self.series
    }

    /// The canonical samples.
    #[must_use]
    pub fn samples(&self) -> &[CounterSample] {
        &self.samples
    }

    pub(super) fn resident_heap_bytes(&self) -> Option<usize> {
        self.series
            .capacity()
            .checked_mul(size_of::<MetricSeriesDescriptor>())?
            .checked_add(
                self.samples
                    .capacity()
                    .checked_mul(size_of::<CounterSample>())?,
            )
    }

    /// Decodes a counter-samples block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-order, out-of-bound, or
    /// trailing-byte body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Ok(Self {
                series: Vec::new(),
                samples: Vec::new(),
            });
        }
        let mut reader = ByteReader::new(body);
        let series = read_series_descriptors(&mut reader, bounds)?;
        let count = reader.uvarint(bounds.items_per_block)?;
        let mut samples = Vec::with_capacity(count.min(4_096) as usize);
        for _ in 0..count {
            let series_id = read_series_id(&mut reader)?;
            let alignment_id = read_alignment_id(&mut reader)?;
            let ts_us = reader.i64_le()?;
            let value = reader.u64_le()?;
            let reset_epoch = reader.u64_le()?;
            samples.push(CounterSample::new(
                series_id,
                alignment_id,
                ts_us,
                value,
                reset_epoch,
            ));
        }
        reader.finish()?;
        for w in samples.windows(2) {
            match counter_key(&w[0]).cmp(&counter_key(&w[1])) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
        validate_sample_series(&series, samples.iter().map(|sample| sample.series_id()))?;
        Ok(Self { series, samples })
    }
}

type CounterKey = ([u8; 16], [u8; 16], i64);

const fn counter_key(sample: &CounterSample) -> CounterKey {
    (
        sample.series_id().0,
        sample.alignment_id().0,
        sample.ts_us(),
    )
}

impl EncodableBlock for CounterSamplesBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::CounterSamples
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.samples.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        time_range_of(self.samples.iter().map(|s| s.ts_us()))
    }

    fn encode(&self) -> Vec<u8> {
        if self.samples.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        write_series_descriptors(&mut writer, &self.series);
        writer.uvarint(self.samples.len() as u64);
        for sample in &self.samples {
            write_series_id(&mut writer, sample.series_id());
            write_alignment_id(&mut writer, sample.alignment_id());
            writer.i64_le(sample.ts_us());
            writer.u64_le(sample.value());
            writer.u64_le(sample.reset_epoch());
        }
        writer.into_bytes()
    }
}

/// A canonical, sorted-unique set of instantaneous gauge samples.
///
/// The canonical order is `(series_id, ts_us)`; two samples sharing that key
/// are a duplicate and never both retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaugeSamplesBlock {
    series: Vec<MetricSeriesDescriptor>,
    samples: Vec<GaugeSample>,
}

impl GaugeSamplesBlock {
    /// Normalizes samples into canonical sorted-unique order.
    ///
    /// # Errors
    /// Returns [`BlockError::Duplicate`] when two samples share the canonical
    /// key, and [`BlockError::AboveBound`] past the per-block item bound.
    pub fn new(samples: Vec<GaugeSample>, bounds: &Bounds) -> Result<Self, BlockError> {
        let series = untyped_series_for_gauges(&samples);
        Self::new_with_series(series, samples, bounds)
    }

    /// Normalizes typed series metadata and gauge samples.
    ///
    /// # Errors
    /// Returns [`BlockError`] for duplicate descriptors/samples, an unmapped
    /// sample, contradictory descriptor identity, or a configured bound.
    pub fn new_with_series(
        mut series: Vec<MetricSeriesDescriptor>,
        mut samples: Vec<GaugeSample>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if samples.len() as u64 > bounds.items_per_block
            || series.len() as u64 > bounds.items_per_block
        {
            return Err(BlockError::AboveBound);
        }
        normalize_series(&mut series)?;
        samples.sort_unstable_by_key(gauge_key);
        if samples
            .windows(2)
            .any(|w| gauge_key(&w[0]) == gauge_key(&w[1]))
        {
            return Err(BlockError::Duplicate);
        }
        validate_sample_series(&series, samples.iter().map(|sample| sample.series_id()))?;
        Ok(Self { series, samples })
    }

    /// Typed metadata of every referenced series.
    #[must_use]
    pub fn series(&self) -> &[MetricSeriesDescriptor] {
        &self.series
    }

    /// The canonical samples.
    #[must_use]
    pub fn samples(&self) -> &[GaugeSample] {
        &self.samples
    }

    pub(super) fn resident_heap_bytes(&self) -> Option<usize> {
        self.series
            .capacity()
            .checked_mul(size_of::<MetricSeriesDescriptor>())?
            .checked_add(
                self.samples
                    .capacity()
                    .checked_mul(size_of::<GaugeSample>())?,
            )
    }

    /// Decodes a gauge-samples block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, non-finite, out-of-order,
    /// out-of-bound, or trailing-byte body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Ok(Self {
                series: Vec::new(),
                samples: Vec::new(),
            });
        }
        let mut reader = ByteReader::new(body);
        let series = read_series_descriptors(&mut reader, bounds)?;
        let count = reader.uvarint(bounds.items_per_block)?;
        let mut samples = Vec::with_capacity(count.min(4_096) as usize);
        for _ in 0..count {
            let series_id = read_series_id(&mut reader)?;
            let ts_us = reader.i64_le()?;
            let value = reader.f64_finite()?;
            let sample =
                GaugeSample::new(series_id, ts_us, value).ok_or(BlockError::NonFiniteFloat)?;
            samples.push(sample);
        }
        reader.finish()?;
        for w in samples.windows(2) {
            match gauge_key(&w[0]).cmp(&gauge_key(&w[1])) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
        validate_sample_series(&series, samples.iter().map(|sample| sample.series_id()))?;
        Ok(Self { series, samples })
    }
}

const fn gauge_key(sample: &GaugeSample) -> ([u8; 16], i64) {
    (sample.series_id().0, sample.ts_us())
}

impl EncodableBlock for GaugeSamplesBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::GaugeSamples
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.samples.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        time_range_of(self.samples.iter().map(|sample| sample.ts_us()))
    }

    fn encode(&self) -> Vec<u8> {
        if self.samples.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        write_series_descriptors(&mut writer, &self.series);
        writer.uvarint(self.samples.len() as u64);
        for sample in &self.samples {
            write_series_id(&mut writer, sample.series_id());
            writer.i64_le(sample.ts_us());
            writer.f64_le(sample.value());
        }
        writer.into_bytes()
    }
}

const fn applicability_code(value: Applicability) -> u8 {
    match value {
        Applicability::Applicable => 0,
        Applicability::NotApplicable => 1,
        Applicability::Unsupported => 2,
    }
}

const fn applicability_from(code: u8) -> Result<Applicability, BlockError> {
    match code {
        0 => Ok(Applicability::Applicable),
        1 => Ok(Applicability::NotApplicable),
        2 => Ok(Applicability::Unsupported),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn period_quality_code(value: PeriodQuality) -> u8 {
    match value {
        PeriodQuality::PersistedConfigEpoch => 0,
        PeriodQuality::ObservedStable => 1,
        PeriodQuality::AssumedCurrentConfig => 2,
        PeriodQuality::Unknown => 3,
    }
}

const fn period_quality_from(code: u8) -> Result<PeriodQuality, BlockError> {
    match code {
        0 => Ok(PeriodQuality::PersistedConfigEpoch),
        1 => Ok(PeriodQuality::ObservedStable),
        2 => Ok(PeriodQuality::AssumedCurrentConfig),
        3 => Ok(PeriodQuality::Unknown),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn source_completeness_code(value: SourceCompleteness) -> u8 {
    match value {
        SourceCompleteness::Full => 0,
        SourceCompleteness::BoundedSubset => 1,
        SourceCompleteness::Unknown => 2,
    }
}

const fn source_completeness_from(code: u8) -> Result<SourceCompleteness, BlockError> {
    match code {
        0 => Ok(SourceCompleteness::Full),
        1 => Ok(SourceCompleteness::BoundedSubset),
        2 => Ok(SourceCompleteness::Unknown),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn retained_exactness_code(value: RetainedExactness) -> u8 {
    match value {
        RetainedExactness::Exact => 0,
        RetainedExactness::LowerBound => 1,
        RetainedExactness::Unknown => 2,
    }
}

const fn retained_exactness_from(code: u8) -> Result<RetainedExactness, BlockError> {
    match code {
        0 => Ok(RetainedExactness::Exact),
        1 => Ok(RetainedExactness::LowerBound),
        2 => Ok(RetainedExactness::Unknown),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn physical_count_code(value: PhysicalCountSemantics) -> u8 {
    match value {
        PhysicalCountSemantics::Exact => 0,
        PhysicalCountSemantics::LowerBound => 1,
        PhysicalCountSemantics::Unknown => 2,
        PhysicalCountSemantics::NotApplicable => 3,
    }
}

const fn physical_count_from(code: u8) -> Result<PhysicalCountSemantics, BlockError> {
    match code {
        0 => Ok(PhysicalCountSemantics::Exact),
        1 => Ok(PhysicalCountSemantics::LowerBound),
        2 => Ok(PhysicalCountSemantics::Unknown),
        3 => Ok(PhysicalCountSemantics::NotApplicable),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_coverage(writer: &mut ByteWriter, coverage: &Coverage) {
    writer.uvarint(coverage.spans().len() as u64);
    for span in coverage.spans() {
        writer.i64_le(span.start_us());
        writer.i64_le(span.end_us());
    }
}

fn read_coverage(reader: &mut ByteReader<'_>, bound: u64) -> Result<Coverage, BlockError> {
    let count = reader.uvarint(bound)?;
    let mut spans = Vec::with_capacity(count.min(4_096) as usize);
    for _ in 0..count {
        let from_us = reader.i64_le()?;
        let to_us = reader.i64_le()?;
        let span = CoverageSpan::new(from_us, to_us).ok_or(BlockError::Malformed)?;
        if let Some(previous) = spans.last() {
            // Canonical coverage is sorted and strictly disjoint: two adjacent
            // or overlapping spans would have merged into one.
            let previous: &CoverageSpan = previous;
            if span.start_us() <= previous.end_us() {
                return Err(BlockError::Unsorted);
            }
        }
        spans.push(span);
    }
    Ok(Coverage::from_spans(spans))
}

/// Coverage, known gaps, and the retained completeness quality of a segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossCoverageBlock {
    covered: Coverage,
    known_gaps: Coverage,
    applicability: Applicability,
    period_quality: PeriodQuality,
    source_completeness: SourceCompleteness,
    retained_exactness: RetainedExactness,
    physical_count: PhysicalCountSemantics,
    dropped_lower_bound: u64,
    factor_coverage: Vec<FactorCoverage>,
}

impl LossCoverageBlock {
    /// Builds a coverage block, bounding the span counts.
    ///
    /// # Errors
    /// Returns [`BlockError::AboveBound`] when either span set exceeds the
    /// coverage-span bound.
    #[allow(
        clippy::too_many_arguments,
        reason = "the block mirrors the stored coverage record"
    )]
    pub fn new(
        covered: Coverage,
        known_gaps: Coverage,
        applicability: Applicability,
        period_quality: PeriodQuality,
        source_completeness: SourceCompleteness,
        retained_exactness: RetainedExactness,
        physical_count: PhysicalCountSemantics,
        dropped_lower_bound: u64,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        Self::new_with_factors(
            covered,
            known_gaps,
            applicability,
            period_quality,
            source_completeness,
            retained_exactness,
            physical_count,
            dropped_lower_bound,
            Vec::new(),
            bounds,
        )
    }

    /// Builds segment coverage together with canonical per-factor evidence.
    ///
    /// Factor records are sorted by `(factor_id, interval)` and must be unique.
    /// Their variable-sized loss lists share the block item bound.
    ///
    /// # Errors
    /// Returns [`BlockError`] for duplicate or malformed factor records, or
    /// when a span/item limit is exceeded.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor mirrors the persisted coverage contract"
    )]
    pub fn new_with_factors(
        covered: Coverage,
        known_gaps: Coverage,
        applicability: Applicability,
        period_quality: PeriodQuality,
        source_completeness: SourceCompleteness,
        retained_exactness: RetainedExactness,
        physical_count: PhysicalCountSemantics,
        dropped_lower_bound: u64,
        mut factor_coverage: Vec<FactorCoverage>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if covered.spans().len() as u64 > bounds.coverage_spans
            || known_gaps.spans().len() as u64 > bounds.coverage_spans
        {
            return Err(BlockError::AboveBound);
        }
        normalize_factor_coverage(&mut factor_coverage, bounds)?;
        Ok(Self {
            covered,
            known_gaps,
            applicability,
            period_quality,
            source_completeness,
            retained_exactness,
            physical_count,
            dropped_lower_bound,
            factor_coverage,
        })
    }

    /// The covered spans.
    #[must_use]
    pub const fn covered(&self) -> &Coverage {
        &self.covered
    }

    /// The known collection gaps.
    #[must_use]
    pub const fn known_gaps(&self) -> &Coverage {
        &self.known_gaps
    }

    /// Whether this factor applies to the source.
    #[must_use]
    pub const fn applicability(&self) -> Applicability {
        self.applicability
    }

    /// Provenance quality of the collection period.
    #[must_use]
    pub const fn period_quality(&self) -> PeriodQuality {
        self.period_quality
    }

    /// Completeness of the source population.
    #[must_use]
    pub const fn source_completeness(&self) -> SourceCompleteness {
        self.source_completeness
    }

    /// Exactness of retained values.
    #[must_use]
    pub const fn retained_exactness(&self) -> RetainedExactness {
        self.retained_exactness
    }

    /// Meaning of the physical count.
    #[must_use]
    pub const fn physical_count(&self) -> PhysicalCountSemantics {
        self.physical_count
    }

    /// The proven lower bound on dropped records.
    #[must_use]
    pub const fn dropped_lower_bound(&self) -> u64 {
        self.dropped_lower_bound
    }

    /// Canonical per-factor applicability, cadence, population and loss proof.
    #[must_use]
    pub fn factor_coverage(&self) -> &[FactorCoverage] {
        &self.factor_coverage
    }

    pub(super) fn resident_factor_bytes(&self) -> Option<usize> {
        self.factor_coverage
            .capacity()
            .checked_mul(size_of::<FactorCoverage>())?
            .checked_add(
                self.factor_coverage
                    .iter()
                    .try_fold(0_usize, |total, coverage| {
                        total.checked_add(
                            coverage
                                .loss_reasons
                                .capacity()
                                .checked_mul(size_of::<LossReason>())?,
                        )
                    })?,
            )
    }

    /// Decodes a coverage block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-order, out-of-bound,
    /// invalid-enum, or trailing-byte body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        let mut covered_budget = bounds.coverage_spans;
        let mut gap_budget = bounds.coverage_spans;
        Self::decode_with_span_budgets(body, bounds, &mut covered_budget, &mut gap_budget)
    }

    pub(super) fn decode_with_span_budgets(
        body: &[u8],
        bounds: &Bounds,
        covered_budget: &mut u64,
        gap_budget: &mut u64,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        let mut reader = ByteReader::new(body);
        let covered = read_coverage(&mut reader, *covered_budget)?;
        *covered_budget = covered_budget
            .checked_sub(covered.spans().len() as u64)
            .ok_or(BlockError::AboveBound)?;
        let known_gaps = read_coverage(&mut reader, *gap_budget)?;
        *gap_budget = gap_budget
            .checked_sub(known_gaps.spans().len() as u64)
            .ok_or(BlockError::AboveBound)?;
        let applicability = applicability_from(reader.u8()?)?;
        let period_quality = period_quality_from(reader.u8()?)?;
        let source_completeness = source_completeness_from(reader.u8()?)?;
        let retained_exactness = retained_exactness_from(reader.u8()?)?;
        let physical_count = physical_count_from(reader.u8()?)?;
        let dropped_lower_bound = reader.u64_le()?;
        let factor_count = reader.uvarint(bounds.items_per_block)?;
        let mut factor_coverage = Vec::with_capacity(factor_count.min(4_096) as usize);
        let mut loss_budget = bounds.items_per_block;
        for _ in 0..factor_count {
            factor_coverage.push(read_factor_coverage(&mut reader, bounds, &mut loss_budget)?);
        }
        reader.finish()?;
        validate_canonical_factor_coverage(&factor_coverage)?;
        Self::new_with_factors(
            covered,
            known_gaps,
            applicability,
            period_quality,
            source_completeness,
            retained_exactness,
            physical_count,
            dropped_lower_bound,
            factor_coverage,
            bounds,
        )
    }
}

fn validate_canonical_factor_coverage(coverage: &[FactorCoverage]) -> Result<(), BlockError> {
    for pair in coverage.windows(2) {
        match factor_coverage_key(&pair[0]).cmp(&factor_coverage_key(&pair[1])) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
            std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
        }
    }
    for item in coverage {
        for pair in item.loss_reasons.windows(2) {
            match pair[0].cmp(&pair[1]) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
    }
    Ok(())
}

impl EncodableBlock for LossCoverageBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::LossCoverage
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        (self.covered.spans().len() + self.known_gaps.spans().len() + self.factor_coverage.len())
            as u64
            + 1
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        let starts = self
            .covered
            .spans()
            .iter()
            .chain(self.known_gaps.spans())
            .map(|span| span.start_us());
        let ends = self
            .covered
            .spans()
            .iter()
            .chain(self.known_gaps.spans())
            .map(|span| span.end_us());
        let end_exclusive = ends.max()?;
        Some((starts.min()?, end_exclusive.checked_sub(1)?))
    }

    fn encode(&self) -> Vec<u8> {
        let mut writer = ByteWriter::new();
        write_coverage(&mut writer, &self.covered);
        write_coverage(&mut writer, &self.known_gaps);
        writer.u8(applicability_code(self.applicability));
        writer.u8(period_quality_code(self.period_quality));
        writer.u8(source_completeness_code(self.source_completeness));
        writer.u8(retained_exactness_code(self.retained_exactness));
        writer.u8(physical_count_code(self.physical_count));
        writer.u64_le(self.dropped_lower_bound);
        writer.uvarint(self.factor_coverage.len() as u64);
        for coverage in &self.factor_coverage {
            write_factor_coverage(&mut writer, coverage);
        }
        writer.into_bytes()
    }
}

type FactorCoverageKey = (u32, i64, i64);

const fn factor_coverage_key(coverage: &FactorCoverage) -> FactorCoverageKey {
    (
        coverage.factor_id.0,
        coverage.interval.start_us(),
        coverage.interval.end_us(),
    )
}

fn normalize_factor_coverage(
    coverage: &mut [FactorCoverage],
    bounds: &Bounds,
) -> Result<(), BlockError> {
    if coverage.len() as u64 > bounds.items_per_block {
        return Err(BlockError::AboveBound);
    }
    coverage.sort_unstable_by_key(factor_coverage_key);
    if coverage
        .windows(2)
        .any(|pair| factor_coverage_key(&pair[0]) == factor_coverage_key(&pair[1]))
    {
        return Err(BlockError::Duplicate);
    }
    let mut loss_count = 0_u64;
    for item in coverage {
        if item.expected_period_us == Some(0)
            || item.covered_duration_us > item.interval.duration_us()
            || item.source_population.is_some_and(|population| {
                population
                    .total
                    .is_some_and(|total| population.collected > total)
                    || population.total_quality == PopulationTotalQuality::Exact
                        && population.total.is_none()
            })
            || item
                .lost_count_lower_bound
                .is_some_and(|lost| lost > 0 && item.loss_reasons.is_empty())
        {
            return Err(BlockError::Malformed);
        }
        item.loss_reasons.sort_unstable();
        if item.loss_reasons.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(BlockError::Duplicate);
        }
        loss_count = loss_count
            .checked_add(item.loss_reasons.len() as u64)
            .ok_or(BlockError::AboveBound)?;
        if loss_count > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
    }
    Ok(())
}

fn write_factor_coverage(writer: &mut ByteWriter, coverage: &FactorCoverage) {
    writer.u32_le(coverage.factor_id.0);
    writer.u8(applicability_code(coverage.applicability));
    writer.u8(coverage_state_code(coverage.state));
    writer.i64_le(coverage.interval.start_us());
    writer.i64_le(coverage.interval.end_us());
    write_optional_u64(writer, coverage.expected_period_us);
    writer.u8(period_quality_code(coverage.period_quality));
    match coverage.cadence_epoch_id {
        Some(id) => {
            writer.u8(1);
            writer.bytes(&id.0);
        }
        None => writer.u8(0),
    }
    writer.u8(u8::from(coverage.crosses_cadence_boundary));
    writer.u64_le(coverage.present_samples);
    writer.u64_le(coverage.covered_duration_us);
    match coverage.source_population {
        Some(population) => {
            writer.u8(1);
            writer.u64_le(population.collected);
            write_optional_u64(writer, population.total);
            writer.u8(population_quality_code(population.total_quality));
        }
        None => writer.u8(0),
    }
    writer.uvarint(coverage.loss_reasons.len() as u64);
    for reason in &coverage.loss_reasons {
        writer.u8(reason.code());
    }
    write_optional_u64(writer, coverage.lost_count_lower_bound);
    writer.u8(retained_exactness_code(coverage.retained_exactness));
    writer.u8(source_completeness_code(coverage.source_completeness));
    writer.u8(physical_count_code(coverage.physical_count_semantics));
    writer.u8(boundary_quality_code(coverage.boundary_quality));
}

fn read_factor_coverage(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
    loss_budget: &mut u64,
) -> Result<FactorCoverage, BlockError> {
    let factor_id = FactorId(reader.u32_le()?);
    let applicability = applicability_from(reader.u8()?)?;
    let state = coverage_state_from(reader.u8()?)?;
    let interval =
        CoverageSpan::new(reader.i64_le()?, reader.i64_le()?).ok_or(BlockError::Malformed)?;
    let expected_period_us = read_optional_u64(reader)?;
    let period_quality = period_quality_from(reader.u8()?)?;
    let cadence_epoch_id = match reader.u8()? {
        0 => None,
        1 => Some(CadenceEpochId(reader.array()?)),
        _ => return Err(BlockError::InvalidEnum),
    };
    let crosses_cadence_boundary = read_bool(reader)?;
    let present_samples = reader.u64_le()?;
    let covered_duration_us = reader.u64_le()?;
    let source_population = match reader.u8()? {
        0 => None,
        1 => Some(SourcePopulation {
            collected: reader.u64_le()?,
            total: read_optional_u64(reader)?,
            total_quality: population_quality_from(reader.u8()?)?,
        }),
        _ => return Err(BlockError::InvalidEnum),
    };
    let loss_count = reader.uvarint((*loss_budget).min(bounds.items_per_block))?;
    *loss_budget = loss_budget
        .checked_sub(loss_count)
        .ok_or(BlockError::AboveBound)?;
    let mut loss_reasons = Vec::with_capacity(loss_count.min(64) as usize);
    for _ in 0..loss_count {
        loss_reasons.push(LossReason::from_code(reader.u8()?).ok_or(BlockError::InvalidEnum)?);
    }
    let lost_count_lower_bound = read_optional_u64(reader)?;
    let retained_exactness = retained_exactness_from(reader.u8()?)?;
    let source_completeness = source_completeness_from(reader.u8()?)?;
    let physical_count_semantics = physical_count_from(reader.u8()?)?;
    let boundary_quality = boundary_quality_from(reader.u8()?)?;
    Ok(FactorCoverage {
        factor_id,
        applicability,
        state,
        interval,
        expected_period_us,
        period_quality,
        cadence_epoch_id,
        crosses_cadence_boundary,
        present_samples,
        covered_duration_us,
        source_population,
        loss_reasons,
        lost_count_lower_bound,
        retained_exactness,
        source_completeness,
        physical_count_semantics,
        boundary_quality,
    })
}

fn write_optional_u64(writer: &mut ByteWriter, value: Option<u64>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.u64_le(value);
        }
        None => writer.u8(0),
    }
}

fn read_optional_u64(reader: &mut ByteReader<'_>) -> Result<Option<u64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64_le()?)),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn read_bool(reader: &mut ByteReader<'_>) -> Result<bool, BlockError> {
    match reader.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn coverage_state_code(value: CoverageState) -> u8 {
    match value {
        CoverageState::Complete => 0,
        CoverageState::Partial => 1,
        CoverageState::Gap => 2,
        CoverageState::Unknown => 3,
        CoverageState::NotCollected => 4,
    }
}

const fn coverage_state_from(code: u8) -> Result<CoverageState, BlockError> {
    match code {
        0 => Ok(CoverageState::Complete),
        1 => Ok(CoverageState::Partial),
        2 => Ok(CoverageState::Gap),
        3 => Ok(CoverageState::Unknown),
        4 => Ok(CoverageState::NotCollected),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn population_quality_code(value: PopulationTotalQuality) -> u8 {
    match value {
        PopulationTotalQuality::Exact => 0,
        PopulationTotalQuality::LowerBound => 1,
        PopulationTotalQuality::Unknown => 2,
    }
}

const fn population_quality_from(code: u8) -> Result<PopulationTotalQuality, BlockError> {
    match code {
        0 => Ok(PopulationTotalQuality::Exact),
        1 => Ok(PopulationTotalQuality::LowerBound),
        2 => Ok(PopulationTotalQuality::Unknown),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn boundary_quality_code(value: BoundaryQuality) -> u8 {
    match value {
        BoundaryQuality::Contained => 0,
        BoundaryQuality::EndpointAttributedCrossBoundary => 1,
        BoundaryQuality::ModeledHold => 2,
        BoundaryQuality::Mixed => 3,
        BoundaryQuality::Unknown => 4,
    }
}

const fn boundary_quality_from(code: u8) -> Result<BoundaryQuality, BlockError> {
    match code {
        0 => Ok(BoundaryQuality::Contained),
        1 => Ok(BoundaryQuality::EndpointAttributedCrossBoundary),
        2 => Ok(BoundaryQuality::ModeledHold),
        3 => Ok(BoundaryQuality::Mixed),
        4 => Ok(BoundaryQuality::Unknown),
        _ => Err(BlockError::InvalidEnum),
    }
}

/// One per-series reset epoch boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetMarker {
    /// The series whose counter reset.
    pub series_id: MetricSeriesId,
    /// The timestamp at which the reset epoch changed.
    pub ts_us: i64,
    /// The epoch value in effect from this timestamp.
    pub reset_epoch: u64,
}

/// A canonical, sorted-unique set of reset markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetMarkersBlock {
    markers: Vec<ResetMarker>,
}

impl ResetMarkersBlock {
    /// Normalizes reset markers into canonical sorted-unique order.
    ///
    /// # Errors
    /// Returns [`BlockError::Duplicate`] for two markers with the same
    /// `(series_id, ts_us)`, and [`BlockError::AboveBound`] past the item
    /// bound.
    pub fn new(mut markers: Vec<ResetMarker>, bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if markers.len() as u64 > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
        markers.sort_unstable_by_key(reset_key);
        if markers
            .windows(2)
            .any(|w| reset_key(&w[0]) == reset_key(&w[1]))
        {
            return Err(BlockError::Duplicate);
        }
        Ok(Self { markers })
    }

    /// The canonical markers.
    #[must_use]
    pub fn markers(&self) -> &[ResetMarker] {
        &self.markers
    }

    pub(super) const fn resident_heap_bytes(&self) -> Option<usize> {
        self.markers
            .capacity()
            .checked_mul(size_of::<ResetMarker>())
    }

    /// Decodes a reset-markers block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-order, out-of-bound, or
    /// trailing-byte body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Ok(Self {
                markers: Vec::new(),
            });
        }
        let mut reader = ByteReader::new(body);
        let count = reader.uvarint(bounds.items_per_block)?;
        let mut markers = Vec::with_capacity(count.min(4_096) as usize);
        for _ in 0..count {
            let series_id = read_series_id(&mut reader)?;
            let ts_us = reader.i64_le()?;
            let reset_epoch = reader.u64_le()?;
            markers.push(ResetMarker {
                series_id,
                ts_us,
                reset_epoch,
            });
        }
        reader.finish()?;
        for w in markers.windows(2) {
            match reset_key(&w[0]).cmp(&reset_key(&w[1])) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
        Ok(Self { markers })
    }
}

const fn reset_key(marker: &ResetMarker) -> ([u8; 16], i64) {
    (marker.series_id.0, marker.ts_us)
}

impl EncodableBlock for ResetMarkersBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::ResetMarkers
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.markers.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        time_range_of(self.markers.iter().map(|marker| marker.ts_us))
    }

    fn encode(&self) -> Vec<u8> {
        if self.markers.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        writer.uvarint(self.markers.len() as u64);
        for marker in &self.markers {
            write_series_id(&mut writer, marker.series_id);
            writer.i64_le(marker.ts_us);
            writer.u64_le(marker.reset_epoch);
        }
        writer.into_bytes()
    }
}

/// One bounded entity snapshot retained for proven transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityStateRecord {
    /// The entity series identity.
    pub series_id: MetricSeriesId,
    /// The snapshot timestamp.
    pub ts_us: i64,
    /// The retained state discriminant.
    pub state_code: u32,
    /// The population total the snapshot proves.
    pub population_total: u64,
}

/// A canonical, sorted-unique set of entity snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityStatesBlock {
    records: Vec<EntityStateRecord>,
}

impl EntityStatesBlock {
    /// Normalizes entity snapshots into canonical sorted-unique order.
    ///
    /// # Errors
    /// Returns [`BlockError::Duplicate`] for two records with the same
    /// `(series_id, ts_us)`, and [`BlockError::AboveBound`] past the item
    /// bound.
    pub fn new(mut records: Vec<EntityStateRecord>, bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if records.len() as u64 > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
        records.sort_unstable_by_key(entity_key);
        if records
            .windows(2)
            .any(|w| entity_key(&w[0]) == entity_key(&w[1]))
        {
            return Err(BlockError::Duplicate);
        }
        Ok(Self { records })
    }

    /// The canonical records.
    #[must_use]
    pub fn records(&self) -> &[EntityStateRecord] {
        &self.records
    }

    pub(super) const fn resident_heap_bytes(&self) -> Option<usize> {
        self.records
            .capacity()
            .checked_mul(size_of::<EntityStateRecord>())
    }

    /// Decodes an entity-states block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-order, out-of-bound, or
    /// trailing-byte body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Ok(Self {
                records: Vec::new(),
            });
        }
        let mut reader = ByteReader::new(body);
        let count = reader.uvarint(bounds.items_per_block)?;
        let mut records = Vec::with_capacity(count.min(4_096) as usize);
        for _ in 0..count {
            let series_id = read_series_id(&mut reader)?;
            let ts_us = reader.i64_le()?;
            let state_code = reader.u32_le()?;
            let population_total = reader.u64_le()?;
            records.push(EntityStateRecord {
                series_id,
                ts_us,
                state_code,
                population_total,
            });
        }
        reader.finish()?;
        for w in records.windows(2) {
            match entity_key(&w[0]).cmp(&entity_key(&w[1])) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
        Ok(Self { records })
    }
}

const fn entity_key(record: &EntityStateRecord) -> ([u8; 16], i64) {
    (record.series_id.0, record.ts_us)
}

impl EncodableBlock for EntityStatesBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::EntityStates
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.records.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        time_range_of(self.records.iter().map(|record| record.ts_us))
    }

    fn encode(&self) -> Vec<u8> {
        if self.records.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        writer.uvarint(self.records.len() as u64);
        for record in &self.records {
            write_series_id(&mut writer, record.series_id);
            writer.i64_le(record.ts_us);
            writer.u32_le(record.state_code);
            writer.u64_le(record.population_total);
        }
        writer.into_bytes()
    }
}

/// A canonical, sorted-unique table of retained text and byte values.
///
/// A retained text reference is the position of its value in the sorted table.
#[derive(Clone, PartialEq, Eq)]
pub struct StringTableBlock {
    entries: Vec<Box<[u8]>>,
}

impl std::fmt::Debug for StringTableBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stored_bytes = self
            .entries
            .iter()
            .fold(0_usize, |total, entry| total.saturating_add(entry.len()));
        f.debug_struct("StringTableBlock")
            .field("entries", &self.entries.len())
            .field("stored_bytes", &stored_bytes)
            .finish()
    }
}

impl StringTableBlock {
    /// Normalizes values into a sorted-unique bounded table.
    ///
    /// # Errors
    /// Returns [`BlockError::AboveBound`] when a value exceeds the pattern
    /// bound, the table exceeds the decoded string bound, or the count exceeds
    /// the item bound.
    pub fn new(values: Vec<Box<[u8]>>, bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if values.len() as u64 > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
        let mut entries = values;
        entries.sort_unstable();
        entries.dedup();
        let mut total: u64 = 0;
        for entry in &entries {
            if entry.len() as u64 > bounds.pattern_bytes {
                return Err(BlockError::AboveBound);
            }
            total = total
                .checked_add(entry.len() as u64)
                .ok_or(BlockError::AboveBound)?;
        }
        if total > bounds.string_table_bytes {
            return Err(BlockError::AboveBound);
        }
        Ok(Self { entries })
    }

    /// The canonical values.
    #[must_use]
    pub fn values(&self) -> &[Box<[u8]>] {
        &self.entries
    }

    /// Decodes a string-table block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-order, duplicate,
    /// out-of-bound, or trailing-byte body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Ok(Self {
                entries: Vec::new(),
            });
        }
        let mut reader = ByteReader::new(body);
        let count = reader.uvarint(bounds.items_per_block)?;
        let mut entries: Vec<Box<[u8]>> = Vec::with_capacity(count.min(4_096) as usize);
        let mut total: u64 = 0;
        for _ in 0..count {
            let value = reader.length_prefixed(bounds.pattern_bytes)?;
            total = total
                .checked_add(value.len() as u64)
                .ok_or(BlockError::AboveBound)?;
            if total > bounds.string_table_bytes {
                return Err(BlockError::AboveBound);
            }
            if let Some(previous) = entries.last() {
                match value.cmp(previous.as_ref()) {
                    std::cmp::Ordering::Greater => {}
                    std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                    std::cmp::Ordering::Less => return Err(BlockError::Unsorted),
                }
            }
            entries.push(value.into());
        }
        reader.finish()?;
        Ok(Self { entries })
    }
}

impl EncodableBlock for StringTableBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::StringTable
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.entries.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        None
    }

    fn encode(&self) -> Vec<u8> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        writer.uvarint(self.entries.len() as u64);
        for entry in &self.entries {
            writer.length_prefixed(entry);
        }
        writer.into_bytes()
    }
}

/// The catalog inventory and source/range metadata of a segment.
///
/// Entries stay in catalog order: order is part of the segment's provenance,
/// so this block is not reordered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceManifestBlock {
    source_format_version: u32,
    source_min_ts_us: i64,
    source_max_ts_us: i64,
    source_file_len: u64,
    entries: Vec<ManifestEntryDescriptor>,
}

impl SourceManifestBlock {
    /// Builds a manifest, bounding the catalog inventory length.
    ///
    /// # Errors
    /// Returns [`BlockError::AboveBound`] past the directory-entry bound.
    pub fn new(
        source_format_version: u32,
        source_min_ts_us: i64,
        source_max_ts_us: i64,
        source_file_len: u64,
        entries: Vec<ManifestEntryDescriptor>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if entries.len() as u64 > u64::from(bounds.directory_entries) {
            return Err(BlockError::AboveBound);
        }
        if source_min_ts_us > source_max_ts_us {
            return Err(BlockError::Malformed);
        }
        Ok(Self {
            source_format_version,
            source_min_ts_us,
            source_max_ts_us,
            source_file_len,
            entries,
        })
    }

    /// The catalog inventory, in catalog order.
    #[must_use]
    pub fn entries(&self) -> &[ManifestEntryDescriptor] {
        &self.entries
    }

    /// The PGM file length recorded for the segment.
    #[must_use]
    pub const fn source_file_len(&self) -> u64 {
        self.source_file_len
    }

    /// PGM container version recorded for the segment.
    #[must_use]
    pub const fn source_format_version(&self) -> u32 {
        self.source_format_version
    }

    /// Inclusive PGM timestamp range.
    #[must_use]
    pub const fn source_time_range(&self) -> (i64, i64) {
        (self.source_min_ts_us, self.source_max_ts_us)
    }

    /// Decodes a source-manifest block body.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-bound, or trailing-byte
    /// body.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        let mut reader = ByteReader::new(body);
        let source_format_version = reader.u32_le()?;
        let source_min_ts_us = reader.i64_le()?;
        let source_max_ts_us = reader.i64_le()?;
        let source_file_len = reader.u64_le()?;
        if source_min_ts_us > source_max_ts_us {
            return Err(BlockError::Malformed);
        }
        let count = reader.uvarint(u64::from(bounds.directory_entries))?;
        let mut entries = Vec::with_capacity(count.min(4_096) as usize);
        for _ in 0..count {
            let type_id = reader.u32_le()?;
            let flags = reader.u32_le()?;
            let body_len = reader.u64_le()?;
            let rows = reader.u32_le()?;
            let body_crc32c = reader.u32_le()?;
            let section_body_id = match reader.u8()? {
                0 => None,
                1 => Some(kronika_analytics::overview::SectionBodyId(reader.array()?)),
                _ => return Err(BlockError::Malformed),
            };
            entries.push(ManifestEntryDescriptor {
                catalog: CatalogEntryDescriptor {
                    type_id,
                    flags,
                    body_len,
                    rows,
                    body_crc32c,
                },
                section_body_id,
            });
        }
        reader.finish()?;
        Ok(Self {
            source_format_version,
            source_min_ts_us,
            source_max_ts_us,
            source_file_len,
            entries,
        })
    }
}

impl EncodableBlock for SourceManifestBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::SourceManifest
    }

    fn canonically_sorted(&self) -> bool {
        false
    }

    fn item_count(&self) -> u64 {
        self.entries.len() as u64 + 1
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        None
    }

    fn encode(&self) -> Vec<u8> {
        let mut writer = ByteWriter::new();
        writer.u32_le(self.source_format_version);
        writer.i64_le(self.source_min_ts_us);
        writer.i64_le(self.source_max_ts_us);
        writer.u64_le(self.source_file_len);
        writer.uvarint(self.entries.len() as u64);
        for entry in &self.entries {
            writer.u32_le(entry.catalog.type_id);
            writer.u32_le(entry.catalog.flags);
            writer.u64_le(entry.catalog.body_len);
            writer.u32_le(entry.catalog.rows);
            writer.u32_le(entry.catalog.body_crc32c);
            match entry.section_body_id {
                Some(section_body_id) => {
                    writer.u8(1);
                    writer.bytes(&section_body_id.0);
                }
                None => writer.u8(0),
            }
        }
        writer.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "fixtures assert exactly representable float values"
    )]

    use super::*;

    const BOUNDS: Bounds = super::super::limits::LIMIT;

    fn series(value: u8) -> MetricSeriesId {
        MetricSeriesId([value; 16])
    }

    fn alignment(value: u8) -> AlignmentId {
        AlignmentId([value; 16])
    }

    fn counter(series_byte: u8, ts_us: i64, value: u64) -> CounterSample {
        CounterSample::new(series(series_byte), alignment(1), ts_us, value, 1)
    }

    fn gauge(series_byte: u8, ts_us: i64, value: f64) -> GaugeSample {
        GaugeSample::new(series(series_byte), ts_us, value).expect("finite fixture")
    }

    #[test]
    fn block_kind_codes_round_trip_and_reject_unknown() {
        for kind in BlockKind::KNOWN {
            assert_eq!(BlockKind::from_code(kind.code()), Some(kind));
        }
        assert_eq!(BlockKind::from_code(0), None);
        assert_eq!(BlockKind::from_code(12), None);
    }

    #[test]
    fn block_flags_round_trip_and_reject_reserved_bits() {
        let flags = BlockFlags {
            required_for_schema: true,
            canonically_sorted: true,
            has_time_range: true,
            codec: BlockCodec::None,
        };
        assert_eq!(BlockFlags::from_bits(flags.to_bits()), Ok(flags));

        let zstd = BlockFlags {
            required_for_schema: false,
            canonically_sorted: true,
            has_time_range: false,
            codec: BlockCodec::Zstd,
        };
        assert_eq!(BlockFlags::from_bits(zstd.to_bits()), Ok(zstd));

        assert_eq!(BlockFlags::from_bits(1 << 4), Err(BlockError::InvalidFlags));
        assert_eq!(
            BlockFlags::from_bits(0x0F00),
            Err(BlockError::InvalidFlags),
            "codec 15 is unknown"
        );
    }

    #[test]
    fn counter_samples_round_trip_in_canonical_order() {
        let block = CounterSamplesBlock::new(
            vec![counter(2, 30, 9), counter(1, 10, 5), counter(1, 20, 7)],
            &BOUNDS,
        )
        .expect("valid block");
        let decoded = CounterSamplesBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        // Canonical order sorts by series then timestamp.
        assert_eq!(decoded.samples()[0].ts_us(), 10);
        assert_eq!(decoded.samples()[2].series_id(), series(2));
    }

    #[test]
    fn an_empty_counter_block_round_trips() {
        let block = CounterSamplesBlock::new(vec![], &BOUNDS).expect("empty is valid");
        let decoded = CounterSamplesBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.time_range(), None);
    }

    #[test]
    fn counter_block_rejects_a_duplicate_canonical_key() {
        assert_eq!(
            CounterSamplesBlock::new(vec![counter(1, 10, 5), counter(1, 10, 6)], &BOUNDS),
            Err(BlockError::Duplicate)
        );
    }

    #[test]
    fn a_truncated_counter_body_is_rejected() {
        let block = CounterSamplesBlock::new(vec![counter(1, 10, 5)], &BOUNDS).expect("valid");
        let mut body = block.encode();
        body.truncate(body.len() - 1);
        assert_eq!(
            CounterSamplesBlock::decode(&body, &BOUNDS),
            Err(BlockError::Truncated)
        );
    }

    #[test]
    fn trailing_bytes_after_a_counter_block_are_rejected() {
        let block = CounterSamplesBlock::new(vec![counter(1, 10, 5)], &BOUNDS).expect("valid");
        let mut body = block.encode();
        body.push(0);
        assert_eq!(
            CounterSamplesBlock::decode(&body, &BOUNDS),
            Err(BlockError::TrailingBytes)
        );
    }

    #[test]
    fn an_oversized_counter_count_is_rejected_before_allocation() {
        let tight = Bounds {
            items_per_block: 1,
            ..BOUNDS
        };
        let block = CounterSamplesBlock::new(vec![counter(1, 10, 5)], &tight).expect("one fits");
        let mut body = block.encode();
        // Rewrite the leading count varint to a value above the bound.
        body[0] = 9;
        assert_eq!(
            CounterSamplesBlock::decode(&body, &tight),
            Err(BlockError::AboveBound)
        );
    }

    #[test]
    fn gauge_samples_round_trip_and_reject_non_finite_bytes() {
        let block = GaugeSamplesBlock::new(
            vec![gauge(1, 20, 2.5), gauge(1, 10, 1.0), gauge(2, 5, -3.0)],
            &BOUNDS,
        )
        .expect("valid block");
        let decoded = GaugeSamplesBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);

        // Corrupt the first sample's value bytes to a NaN payload.
        let mut body = block.encode();
        let value_at = body.len() - 8;
        body[value_at..].copy_from_slice(&f64::NAN.to_le_bytes());
        assert_eq!(
            GaugeSamplesBlock::decode(&body, &BOUNDS),
            Err(BlockError::NonFiniteFloat)
        );
    }

    #[test]
    fn gauge_block_normalizes_negative_zero() {
        let block = GaugeSamplesBlock::new(vec![gauge(1, 10, -0.0)], &BOUNDS).expect("valid");
        let decoded = GaugeSamplesBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded.samples()[0].value(), 0.0);
    }

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid span")
    }

    fn reset(series_byte: u8, ts_us: i64, epoch: u64) -> ResetMarker {
        ResetMarker {
            series_id: series(series_byte),
            ts_us,
            reset_epoch: epoch,
        }
    }

    #[test]
    fn loss_coverage_round_trips_with_quality_and_gaps() {
        let covered = Coverage::from_spans(vec![span(0, 10), span(20, 30)]);
        let gaps = Coverage::from_spans(vec![span(10, 20)]);
        let block = LossCoverageBlock::new(
            covered,
            gaps,
            Applicability::Applicable,
            PeriodQuality::ObservedStable,
            SourceCompleteness::BoundedSubset,
            RetainedExactness::LowerBound,
            PhysicalCountSemantics::Exact,
            42,
            &BOUNDS,
        )
        .expect("valid");
        let decoded = LossCoverageBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.dropped_lower_bound(), 42);
        assert_eq!(decoded.covered().covered_duration_us(), 20);
    }

    #[test]
    fn loss_coverage_rejects_an_invalid_enum_byte() {
        let block = LossCoverageBlock::new(
            Coverage::empty(),
            Coverage::empty(),
            Applicability::Applicable,
            PeriodQuality::Unknown,
            SourceCompleteness::Unknown,
            RetainedExactness::Unknown,
            PhysicalCountSemantics::Unknown,
            0,
            &BOUNDS,
        )
        .expect("valid");
        let mut body = block.encode();
        // Two empty coverage sets encode as two zero varints; the applicability
        // byte follows at index 2.
        body[2] = 0x7F;
        assert_eq!(
            LossCoverageBlock::decode(&body, &BOUNDS),
            Err(BlockError::InvalidEnum)
        );
    }

    #[test]
    fn loss_coverage_decode_rejects_unsorted_spans_on_disk() {
        let mut body = vec![2_u8];
        body.extend_from_slice(&20_i64.to_le_bytes());
        body.extend_from_slice(&30_i64.to_le_bytes());
        body.extend_from_slice(&0_i64.to_le_bytes());
        body.extend_from_slice(&10_i64.to_le_bytes());
        assert_eq!(
            LossCoverageBlock::decode(&body, &BOUNDS),
            Err(BlockError::Unsorted)
        );
    }

    fn factor_record(factor: u32, loss_reasons: Vec<LossReason>) -> FactorCoverage {
        FactorCoverage {
            factor_id: FactorId(factor),
            applicability: Applicability::Applicable,
            state: CoverageState::Unknown,
            interval: span(0, 10),
            expected_period_us: None,
            period_quality: PeriodQuality::Unknown,
            cadence_epoch_id: None,
            crosses_cadence_boundary: false,
            present_samples: 0,
            covered_duration_us: 0,
            source_population: None,
            lost_count_lower_bound: (!loss_reasons.is_empty()).then_some(1),
            loss_reasons,
            retained_exactness: RetainedExactness::Unknown,
            source_completeness: SourceCompleteness::Unknown,
            physical_count_semantics: PhysicalCountSemantics::NotApplicable,
            boundary_quality: BoundaryQuality::Unknown,
        }
    }

    fn encoded_loss_coverage(factors: &[FactorCoverage]) -> Vec<u8> {
        let mut writer = ByteWriter::new();
        write_coverage(&mut writer, &Coverage::empty());
        write_coverage(&mut writer, &Coverage::empty());
        writer.u8(applicability_code(Applicability::Applicable));
        writer.u8(period_quality_code(PeriodQuality::Unknown));
        writer.u8(source_completeness_code(SourceCompleteness::Unknown));
        writer.u8(retained_exactness_code(RetainedExactness::Unknown));
        writer.u8(physical_count_code(PhysicalCountSemantics::Unknown));
        writer.u64_le(0);
        writer.uvarint(factors.len() as u64);
        for factor in factors {
            write_factor_coverage(&mut writer, factor);
        }
        writer.into_bytes()
    }

    #[test]
    fn loss_coverage_decode_rejects_unsorted_factor_records() {
        let body =
            encoded_loss_coverage(&[factor_record(2, Vec::new()), factor_record(1, Vec::new())]);
        assert_eq!(
            LossCoverageBlock::decode(&body, &BOUNDS),
            Err(BlockError::Unsorted)
        );
    }

    #[test]
    fn loss_coverage_decode_rejects_unsorted_loss_reasons() {
        let body = encoded_loss_coverage(&[factor_record(
            1,
            vec![
                LossReason::VisibilityRestricted,
                LossReason::PermissionDenied,
            ],
        )]);
        assert_eq!(
            LossCoverageBlock::decode(&body, &BOUNDS),
            Err(BlockError::Unsorted)
        );
    }

    #[test]
    fn reset_markers_round_trip_in_canonical_order() {
        let block =
            ResetMarkersBlock::new(vec![reset(2, 50, 3), reset(1, 10, 1)], &BOUNDS).expect("valid");
        let decoded = ResetMarkersBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.markers()[0].series_id, series(1));
    }

    #[test]
    fn reset_markers_reject_a_duplicate_key() {
        assert_eq!(
            ResetMarkersBlock::new(vec![reset(1, 10, 1), reset(1, 10, 2)], &BOUNDS),
            Err(BlockError::Duplicate)
        );
    }

    #[test]
    fn a_truncated_reset_body_is_rejected() {
        let block = ResetMarkersBlock::new(vec![reset(1, 10, 1)], &BOUNDS).expect("valid");
        let mut body = block.encode();
        body.truncate(body.len() - 1);
        assert_eq!(
            ResetMarkersBlock::decode(&body, &BOUNDS),
            Err(BlockError::Truncated)
        );
    }

    #[test]
    fn entity_states_round_trip() {
        let block = EntityStatesBlock::new(
            vec![
                EntityStateRecord {
                    series_id: series(2),
                    ts_us: 30,
                    state_code: 9,
                    population_total: 200,
                },
                EntityStateRecord {
                    series_id: series(1),
                    ts_us: 10,
                    state_code: 7,
                    population_total: 100,
                },
            ],
            &BOUNDS,
        )
        .expect("valid");
        let decoded = EntityStatesBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.records()[0].series_id, series(1));
    }

    #[test]
    fn string_table_round_trips_sorted_and_deduped() {
        let block = StringTableBlock::new(
            vec![
                Box::from(b"beta".as_slice()),
                Box::from(b"alpha".as_slice()),
                Box::from(b"alpha".as_slice()),
            ],
            &BOUNDS,
        )
        .expect("valid");
        let decoded = StringTableBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.values().len(), 2);
        assert_eq!(decoded.values()[0].as_ref(), b"alpha");
    }

    #[test]
    fn string_table_rejects_a_pattern_above_the_bound() {
        let tight = Bounds {
            pattern_bytes: 3,
            ..BOUNDS
        };
        assert_eq!(
            StringTableBlock::new(vec![Box::from(b"toolong".as_slice())], &tight),
            Err(BlockError::AboveBound)
        );
    }

    #[test]
    fn string_table_decode_rejects_unsorted_entries() {
        let body = vec![2_u8, 1, b'b', 1, b'a'];
        assert_eq!(
            StringTableBlock::decode(&body, &BOUNDS),
            Err(BlockError::Unsorted)
        );
    }

    #[test]
    fn source_manifest_round_trips_in_catalog_order() {
        let entries = vec![
            ManifestEntryDescriptor {
                catalog: CatalogEntryDescriptor {
                    type_id: 1_022_001,
                    flags: 0,
                    body_len: 4_096,
                    rows: 12,
                    body_crc32c: 0xAAAA,
                },
                section_body_id: Some(kronika_analytics::overview::SectionBodyId([1; 32])),
            },
            ManifestEntryDescriptor {
                catalog: CatalogEntryDescriptor {
                    type_id: 1_028_001,
                    flags: 0,
                    body_len: 8,
                    rows: 1,
                    body_crc32c: 0xBBBB,
                },
                section_body_id: None,
            },
        ];
        let block =
            SourceManifestBlock::new(1, 1_000, 2_000, 65_536, entries, &BOUNDS).expect("valid");
        let decoded = SourceManifestBlock::decode(&block.encode(), &BOUNDS).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.entries()[0].catalog.type_id, 1_022_001);
        assert_eq!(decoded.source_file_len(), 65_536);
    }
}
