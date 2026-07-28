use super::super::block::{BlockError, BlockKind, EncodableBlock};
use super::super::bytes::{ByteReader, ByteWriter};
use super::super::limits::Bounds;
use super::{IndexStatus, TimeGrid, bit_is_set, mask_len, validate_mask};

const SERIES_BLOCK_REVISION: u16 = 1;
const FORMAT_TOP_K: usize = 64;

#[cfg(test)]
std::thread_local! {
    static ENCODE_BODY_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Marks the metric used as the canonical sparkline for its view.
pub const METRIC_FLAG_CANONICAL: u16 = 1 << 0;
const KNOWN_METRIC_FLAGS: u16 = METRIC_FLAG_CANONICAL;

/// One typed identity and its display label in a view-local dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDictionaryEntry {
    key: Vec<u8>,
    label: String,
}

impl EntityDictionaryEntry {
    /// Creates a bounded dictionary entry.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::Malformed`] for an empty identity and
    /// [`BlockError::AboveBound`] when either field exceeds its byte cap.
    pub fn new(key: Vec<u8>, label: String, bounds: &Bounds) -> Result<Self, BlockError> {
        if key.is_empty() {
            return Err(BlockError::Malformed);
        }
        if exceeds_len(key.len(), bounds.web_identity_bytes)
            || exceeds_len(label.len(), bounds.web_label_bytes)
        {
            return Err(BlockError::AboveBound);
        }
        Ok(Self { key, label })
    }

    /// Canonical typed identity bytes.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Human-readable label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// How bucket values contribute to an exact range score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricAggregation {
    /// Add observed bucket values.
    Sum,
    /// Take the largest observed bucket value.
    Max,
}

impl MetricAggregation {
    const fn code(self) -> u8 {
        match self {
            Self::Sum => 0,
            Self::Max => 1,
        }
    }

    const fn from_code(code: u8) -> Result<Self, BlockError> {
        match code {
            0 => Ok(Self::Sum),
            1 => Ok(Self::Max),
            _ => Err(BlockError::InvalidEnum),
        }
    }
}

/// Final collection state of one metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricStatus {
    /// The metric and its top-K are complete.
    Complete,
    /// A collection gate prevented the metric from being observed.
    Gated,
    /// The source type has no matching metric projection.
    UnsupportedType,
    /// A hard writer bound prevented a complete metric.
    ResourceLimited,
}

impl MetricStatus {
    const fn code(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::Gated => 1,
            Self::UnsupportedType => 2,
            Self::ResourceLimited => 3,
        }
    }

    const fn from_code(code: u8) -> Result<Self, BlockError> {
        match code {
            0 => Ok(Self::Complete),
            1 => Ok(Self::Gated),
            2 => Ok(Self::UnsupportedType),
            3 => Ok(Self::ResourceLimited),
            _ => Err(BlockError::InvalidEnum),
        }
    }
}

/// Quantized bucket values for one entity and metric.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySeries {
    entity_ref: u16,
    exact_score: f64,
    max_bucket_value: f64,
    present_mask: Vec<u8>,
    quantized_values: Vec<u8>,
}

impl Eq for EntitySeries {}

impl EntitySeries {
    /// Creates one bounded series.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for non-finite or negative values, an oversized
    /// mask, or a quantized-value count different from the mask popcount.
    pub fn new(
        entity_ref: u16,
        exact_score: f64,
        max_bucket_value: f64,
        present_mask: Vec<u8>,
        quantized_values: Vec<u8>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        validate_nonnegative(exact_score)?;
        validate_nonnegative(max_bucket_value)?;
        let maximum_mask = mask_len(bound_to_usize(bounds.web_buckets)?);
        if present_mask.len() > maximum_mask {
            return Err(BlockError::AboveBound);
        }
        let present_count = mask_popcount(&present_mask)?;
        if quantized_values.len() != present_count {
            return Err(BlockError::Malformed);
        }
        if max_bucket_value == 0.0
            && (exact_score != 0.0 || quantized_values.iter().any(|value| *value != 0))
        {
            return Err(BlockError::Malformed);
        }
        if max_bucket_value > 0.0 && !quantized_values.contains(&u8::MAX) {
            return Err(BlockError::Malformed);
        }
        Ok(Self {
            entity_ref,
            exact_score,
            max_bucket_value,
            present_mask,
            quantized_values,
        })
    }

    /// View-local dictionary reference.
    #[must_use]
    pub const fn entity_ref(&self) -> u16 {
        self.entity_ref
    }

    /// Exact score used for range ranking.
    #[must_use]
    pub const fn exact_score(&self) -> f64 {
        self.exact_score
    }

    /// Scale used to reconstruct quantized buckets.
    #[must_use]
    pub const fn max_bucket_value(&self) -> f64 {
        self.max_bucket_value
    }

    /// Presence bits over the block grid.
    #[must_use]
    pub fn present_mask(&self) -> &[u8] {
        &self.present_mask
    }

    /// Quantized values ordered by set bits in [`Self::present_mask`].
    #[must_use]
    pub fn quantized_values(&self) -> &[u8] {
        &self.quantized_values
    }

    /// Iterates observed bucket indexes and reconstructed values in one pass.
    pub fn observed_values(&self) -> impl Iterator<Item = (usize, f64)> + '_ {
        let mut value_index = 0_usize;
        (0..self.present_mask.len() * 8).filter_map(move |bucket| {
            if !bit_is_set(&self.present_mask, bucket) {
                return None;
            }
            let value = self
                .quantized_values
                .get(value_index)
                .map(|value| f64::from(*value) / 255.0 * self.max_bucket_value);
            value_index += 1;
            value.map(|value| (bucket, value))
        })
    }

    /// Reconstructs a bucket, preserving missing values as `None`.
    #[must_use]
    pub fn value_at(&self, bucket: usize) -> Option<f64> {
        if !bit_is_set(&self.present_mask, bucket) {
            return None;
        }
        let value_index = (0..bucket)
            .filter(|candidate| bit_is_set(&self.present_mask, *candidate))
            .count();
        self.quantized_values
            .get(value_index)
            .map(|value| f64::from(*value) / 255.0 * self.max_bucket_value)
    }
}

/// All retained entity series for one metric.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityMetric {
    metric_code: u16,
    metric_revision: u16,
    flags: u16,
    unit_code: u16,
    aggregation: MetricAggregation,
    status: MetricStatus,
    cutoff_score: f64,
    series: Vec<EntitySeries>,
}

impl Eq for EntityMetric {}

impl EntityMetric {
    /// Creates one canonical metric.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for invalid identity fields, reserved flags,
    /// unsafe scores, a non-canonical top-K, or partial series attached to a
    /// non-complete status.
    #[allow(
        clippy::too_many_arguments,
        reason = "arguments mirror one canonical metric record"
    )]
    pub fn new(
        metric_code: u16,
        metric_revision: u16,
        flags: u16,
        unit_code: u16,
        aggregation: MetricAggregation,
        status: MetricStatus,
        cutoff_score: f64,
        series: Vec<EntitySeries>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if metric_code == 0 || metric_revision == 0 || unit_code == 0 {
            return Err(BlockError::Malformed);
        }
        if flags & !KNOWN_METRIC_FLAGS != 0 {
            return Err(BlockError::InvalidFlags);
        }
        validate_nonnegative(cutoff_score)?;
        if exceeds_len(series.len(), bounds.web_series_per_metric) || series.len() > FORMAT_TOP_K {
            return Err(BlockError::AboveBound);
        }
        if status != MetricStatus::Complete && (!series.is_empty() || cutoff_score != 0.0) {
            return Err(BlockError::Malformed);
        }
        if series.len() < FORMAT_TOP_K && cutoff_score != 0.0 {
            return Err(BlockError::Malformed);
        }
        if series.len() == FORMAT_TOP_K
            && series
                .last()
                .is_none_or(|item| item.exact_score.to_bits() != cutoff_score.to_bits())
        {
            return Err(BlockError::Malformed);
        }
        validate_series_order(&series)?;
        Ok(Self {
            metric_code,
            metric_revision,
            flags,
            unit_code,
            aggregation,
            status,
            cutoff_score,
            series,
        })
    }

    /// Stable metric code.
    #[must_use]
    pub const fn metric_code(&self) -> u16 {
        self.metric_code
    }

    /// Metric projection revision.
    #[must_use]
    pub const fn metric_revision(&self) -> u16 {
        self.metric_revision
    }

    /// Metric flags.
    #[must_use]
    pub const fn flags(&self) -> u16 {
        self.flags
    }

    /// Stable unit code.
    #[must_use]
    pub const fn unit_code(&self) -> u16 {
        self.unit_code
    }

    /// Declared range-ranking aggregate.
    #[must_use]
    pub const fn aggregation(&self) -> MetricAggregation {
        self.aggregation
    }

    /// Final metric status.
    #[must_use]
    pub const fn status(&self) -> MetricStatus {
        self.status
    }

    /// Exact score of the Kth retained entity, or zero below K.
    #[must_use]
    pub const fn cutoff_score(&self) -> f64 {
        self.cutoff_score
    }

    /// Canonically ordered top-K series.
    #[must_use]
    pub fn series(&self) -> &[EntitySeries] {
        &self.series
    }
}

/// One independently addressable web-index view.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySeriesBlock {
    view_code: u16,
    view_revision: u16,
    identity_revision: u16,
    status: IndexStatus,
    observed_range: (i64, i64),
    grid: TimeGrid,
    coverage_mask: Vec<u8>,
    dictionary: Vec<EntityDictionaryEntry>,
    metrics: Vec<EntityMetric>,
}

impl Eq for EntitySeriesBlock {}

impl EntitySeriesBlock {
    /// Creates a canonical, bounded view block.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] when any identity, range, mask, dictionary,
    /// metric, series reference, ordering rule, or decoded-byte cap is invalid.
    #[allow(
        clippy::too_many_arguments,
        reason = "arguments mirror one independently addressable view block"
    )]
    pub fn new(
        view_code: u16,
        view_revision: u16,
        identity_revision: u16,
        status: IndexStatus,
        observed_range: (i64, i64),
        grid: TimeGrid,
        coverage_mask: Vec<u8>,
        dictionary: Vec<EntityDictionaryEntry>,
        metrics: Vec<EntityMetric>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        Self::from_parts(
            view_code,
            view_revision,
            identity_revision,
            status,
            observed_range,
            grid,
            coverage_mask,
            dictionary,
            metrics,
            bounds,
            None,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "arguments mirror one independently addressable view block"
    )]
    fn from_parts(
        view_code: u16,
        view_revision: u16,
        identity_revision: u16,
        status: IndexStatus,
        observed_range: (i64, i64),
        grid: TimeGrid,
        coverage_mask: Vec<u8>,
        dictionary: Vec<EntityDictionaryEntry>,
        metrics: Vec<EntityMetric>,
        bounds: &Bounds,
        decoded_len: Option<usize>,
    ) -> Result<Self, BlockError> {
        if view_code == 0 || view_revision == 0 || identity_revision == 0 {
            return Err(BlockError::Malformed);
        }
        if !matches!(status, IndexStatus::Complete | IndexStatus::ResourceLimited) {
            return Err(BlockError::Malformed);
        }
        validate_observed_range(observed_range, grid, &coverage_mask)?;
        if exceeds_len(dictionary.len(), bounds.web_dictionary_entries)
            || exceeds_len(metrics.len(), bounds.web_metrics_per_view)
        {
            return Err(BlockError::AboveBound);
        }
        validate_dictionary_order(&dictionary)?;
        validate_metric_order(&metrics)?;
        if metrics.is_empty() {
            return Err(BlockError::Malformed);
        }
        validate_series_masks(&metrics, grid)?;
        validate_entity_refs(&metrics, dictionary.len())?;
        validate_dictionary_usage(&metrics, dictionary.len())?;

        let block = Self {
            view_code,
            view_revision,
            identity_revision,
            status,
            observed_range,
            grid,
            coverage_mask,
            dictionary,
            metrics,
        };
        let decoded_len = decoded_len.unwrap_or_else(|| block.encode_body().len());
        if exceeds_len(decoded_len, bounds.web_series_decoded_bytes) {
            return Err(BlockError::AboveBound);
        }
        Ok(block)
    }

    /// Decodes and fully validates one entity-series body.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for malformed bytes or any declared value above
    /// the supplied bounds.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if exceeds_len(body.len(), bounds.web_series_decoded_bytes) {
            return Err(BlockError::AboveBound);
        }
        let mut reader = ByteReader::new(body);
        if reader.u16_le()? != SERIES_BLOCK_REVISION {
            return Err(BlockError::InvalidEnum);
        }
        let view_code = reader.u16_le()?;
        let view_revision = reader.u16_le()?;
        let identity_revision = reader.u16_le()?;
        let status = IndexStatus::from_code(reader.u8()?)?;
        let observed_range = (reader.i64_le()?, reader.i64_le()?);
        let grid =
            TimeGrid::from_parts(reader.i64_le()?, reader.u32_le()?, reader.u16_le()?, bounds)?;
        let mask_bytes = mask_len(usize::from(grid.bucket_count()));
        let coverage_mask = reader.take(mask_bytes)?.to_vec();
        let dictionary_count = reader.u16_le()?;
        if u64::from(dictionary_count) > bounds.web_dictionary_entries {
            return Err(BlockError::AboveBound);
        }
        let metric_count = reader.u16_le()?;
        if u64::from(metric_count) > bounds.web_metrics_per_view {
            return Err(BlockError::AboveBound);
        }

        let mut dictionary = Vec::with_capacity(usize::from(dictionary_count));
        for _ in 0..dictionary_count {
            let key = reader.length_prefixed(bounds.web_identity_bytes)?.to_vec();
            let label_bytes = reader.length_prefixed(bounds.web_label_bytes)?;
            let label = std::str::from_utf8(label_bytes)
                .map_err(|_error| BlockError::Malformed)?
                .to_owned();
            dictionary.push(EntityDictionaryEntry::new(key, label, bounds)?);
        }

        let mut metrics = Vec::with_capacity(usize::from(metric_count));
        for _ in 0..metric_count {
            let metric_code = reader.u16_le()?;
            let metric_revision = reader.u16_le()?;
            let flags = reader.u16_le()?;
            let unit_code = reader.u16_le()?;
            let aggregation = MetricAggregation::from_code(reader.u8()?)?;
            let metric_status = MetricStatus::from_code(reader.u8()?)?;
            let series_count = reader.u16_le()?;
            if u64::from(series_count) > bounds.web_series_per_metric
                || usize::from(series_count) > FORMAT_TOP_K
            {
                return Err(BlockError::AboveBound);
            }
            let cutoff_score = reader.f64_finite()?;
            let mut series = Vec::with_capacity(usize::from(series_count));
            for _ in 0..series_count {
                let entity_ref = reader.uvarint(bounds.web_dictionary_entries)?;
                let entity_ref =
                    u16::try_from(entity_ref).map_err(|_error| BlockError::AboveBound)?;
                let exact_score = reader.f64_finite()?;
                let max_bucket_value = reader.f64_finite()?;
                let present_mask = reader.take(mask_bytes)?.to_vec();
                validate_mask(&present_mask, usize::from(grid.bucket_count()))?;
                let value_count = mask_popcount(&present_mask)?;
                let quantized_values = reader.take(value_count)?.to_vec();
                series.push(EntitySeries::new(
                    entity_ref,
                    exact_score,
                    max_bucket_value,
                    present_mask,
                    quantized_values,
                    bounds,
                )?);
            }
            metrics.push(EntityMetric::new(
                metric_code,
                metric_revision,
                flags,
                unit_code,
                aggregation,
                metric_status,
                cutoff_score,
                series,
                bounds,
            )?);
        }
        reader.finish()?;
        Self::from_parts(
            view_code,
            view_revision,
            identity_revision,
            status,
            observed_range,
            grid,
            coverage_mask,
            dictionary,
            metrics,
            bounds,
            Some(body.len()),
        )
    }

    /// Stable view code, also used as the directory logical ID.
    #[must_use]
    pub const fn view_code(&self) -> u16 {
        self.view_code
    }

    /// View projection revision.
    #[must_use]
    pub const fn view_revision(&self) -> u16 {
        self.view_revision
    }

    /// Typed entity identity revision.
    #[must_use]
    pub const fn identity_revision(&self) -> u16 {
        self.identity_revision
    }

    /// Final view status.
    #[must_use]
    pub const fn status(&self) -> IndexStatus {
        self.status
    }

    /// Exact observed timestamp range represented by this block.
    #[must_use]
    pub const fn observed_range(&self) -> (i64, i64) {
        self.observed_range
    }

    /// UTC-aligned bucket grid.
    #[must_use]
    pub const fn grid(&self) -> TimeGrid {
        self.grid
    }

    /// View snapshot coverage over the block grid.
    #[must_use]
    pub fn coverage_mask(&self) -> &[u8] {
        &self.coverage_mask
    }

    /// View-local typed identity dictionary.
    #[must_use]
    pub fn dictionary(&self) -> &[EntityDictionaryEntry] {
        &self.dictionary
    }

    /// Canonically ordered metrics.
    #[must_use]
    pub fn metrics(&self) -> &[EntityMetric] {
        &self.metrics
    }

    pub(in crate::overview) fn resident_heap_bytes(&self) -> Option<usize> {
        let dictionary_slots = self
            .dictionary
            .capacity()
            .checked_mul(size_of::<EntityDictionaryEntry>())?;
        let dictionary_heap = self.dictionary.iter().try_fold(0_usize, |total, entry| {
            total
                .checked_add(entry.key.capacity())?
                .checked_add(entry.label.capacity())
        })?;
        let metric_slots = self
            .metrics
            .capacity()
            .checked_mul(size_of::<EntityMetric>())?;
        let metric_heap = self.metrics.iter().try_fold(0_usize, |total, metric| {
            let series_slots = metric
                .series
                .capacity()
                .checked_mul(size_of::<EntitySeries>())?;
            let series_heap = metric.series.iter().try_fold(0_usize, |subtotal, series| {
                subtotal
                    .checked_add(series.present_mask.capacity())?
                    .checked_add(series.quantized_values.capacity())
            })?;
            total.checked_add(series_slots)?.checked_add(series_heap)
        })?;

        self.coverage_mask
            .capacity()
            .checked_add(dictionary_slots)?
            .checked_add(dictionary_heap)?
            .checked_add(metric_slots)?
            .checked_add(metric_heap)
    }

    pub(super) fn encode_body(&self) -> Vec<u8> {
        #[cfg(test)]
        ENCODE_BODY_CALLS.with(|calls| calls.set(calls.get() + 1));

        let mut writer = ByteWriter::new();
        writer.u16_le(SERIES_BLOCK_REVISION);
        writer.u16_le(self.view_code);
        writer.u16_le(self.view_revision);
        writer.u16_le(self.identity_revision);
        writer.u8(self.status.code());
        writer.i64_le(self.observed_range.0);
        writer.i64_le(self.observed_range.1);
        writer.i64_le(self.grid.start_us());
        writer.u32_le(self.grid.bucket_width_s());
        writer.u16_le(self.grid.bucket_count());
        writer.bytes(&self.coverage_mask);
        writer.u16_le(len_u16(self.dictionary.len()));
        writer.u16_le(len_u16(self.metrics.len()));
        for entry in &self.dictionary {
            writer.length_prefixed(&entry.key);
            writer.length_prefixed(entry.label.as_bytes());
        }
        for metric in &self.metrics {
            writer.u16_le(metric.metric_code);
            writer.u16_le(metric.metric_revision);
            writer.u16_le(metric.flags);
            writer.u16_le(metric.unit_code);
            writer.u8(metric.aggregation.code());
            writer.u8(metric.status.code());
            writer.u16_le(len_u16(metric.series.len()));
            writer.f64_le(metric.cutoff_score);
            for series in &metric.series {
                writer.uvarint(u64::from(series.entity_ref));
                writer.f64_le(series.exact_score);
                writer.f64_le(series.max_bucket_value);
                writer.bytes(&series.present_mask);
                writer.bytes(&series.quantized_values);
            }
        }
        writer.into_bytes()
    }
}

impl EncodableBlock for EntitySeriesBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::EntitySeries
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.metrics.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        Some(self.observed_range)
    }

    fn encode(&self) -> Vec<u8> {
        self.encode_body()
    }
}

fn exceeds_len(len: usize, bound: u64) -> bool {
    u64::try_from(len).map_or(true, |value| value > bound)
}

fn bound_to_usize(bound: u64) -> Result<usize, BlockError> {
    usize::try_from(bound).map_err(|_error| BlockError::AboveBound)
}

fn len_u16(len: usize) -> u16 {
    match u16::try_from(len) {
        Ok(value) => value,
        Err(_error) => unreachable!("constructor validated the u16 wire length"),
    }
}

fn mask_popcount(mask: &[u8]) -> Result<usize, BlockError> {
    mask.iter().try_fold(0_usize, |count, byte| {
        count
            .checked_add(byte.count_ones() as usize)
            .ok_or(BlockError::AboveBound)
    })
}

fn validate_nonnegative(value: f64) -> Result<(), BlockError> {
    if !value.is_finite() {
        Err(BlockError::NonFiniteFloat)
    } else if value < 0.0 || (value == 0.0 && value.is_sign_negative()) {
        Err(BlockError::Malformed)
    } else {
        Ok(())
    }
}

fn validate_series_order(series: &[EntitySeries]) -> Result<(), BlockError> {
    for (index, left) in series.iter().enumerate() {
        if series[..index]
            .iter()
            .any(|seen| seen.entity_ref == left.entity_ref)
        {
            return Err(BlockError::Duplicate);
        }
        if let Some(right) = series.get(index + 1) {
            match left.exact_score.total_cmp(&right.exact_score) {
                std::cmp::Ordering::Less => return Err(BlockError::Unsorted),
                std::cmp::Ordering::Equal if left.entity_ref >= right.entity_ref => {
                    return Err(BlockError::Unsorted);
                }
                std::cmp::Ordering::Equal | std::cmp::Ordering::Greater => {}
            }
        }
    }
    Ok(())
}

fn validate_dictionary_order(dictionary: &[EntityDictionaryEntry]) -> Result<(), BlockError> {
    for pair in dictionary.windows(2) {
        match pair[0].key.cmp(&pair[1].key) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
            std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
        }
    }
    Ok(())
}

fn validate_metric_order(metrics: &[EntityMetric]) -> Result<(), BlockError> {
    for pair in metrics.windows(2) {
        match pair[0].metric_code.cmp(&pair[1].metric_code) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
            std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
        }
    }
    Ok(())
}

fn validate_entity_refs(metrics: &[EntityMetric], dictionary_len: usize) -> Result<(), BlockError> {
    if metrics
        .iter()
        .flat_map(|metric| &metric.series)
        .any(|series| usize::from(series.entity_ref) >= dictionary_len)
    {
        return Err(BlockError::Malformed);
    }
    Ok(())
}

fn validate_series_masks(metrics: &[EntityMetric], grid: TimeGrid) -> Result<(), BlockError> {
    let bucket_count = usize::from(grid.bucket_count());
    for series in metrics.iter().flat_map(|metric| &metric.series) {
        validate_mask(&series.present_mask, bucket_count)?;
    }
    Ok(())
}

fn validate_dictionary_usage(
    metrics: &[EntityMetric],
    dictionary_len: usize,
) -> Result<(), BlockError> {
    for entity_ref in 0..dictionary_len {
        if !metrics
            .iter()
            .flat_map(|metric| &metric.series)
            .any(|series| usize::from(series.entity_ref) == entity_ref)
        {
            return Err(BlockError::Malformed);
        }
    }
    Ok(())
}

fn validate_observed_range(
    observed_range: (i64, i64),
    grid: TimeGrid,
    coverage_mask: &[u8],
) -> Result<(), BlockError> {
    validate_mask(coverage_mask, usize::from(grid.bucket_count()))?;
    if observed_range.0 > observed_range.1 {
        return Err(BlockError::Malformed);
    }
    let first_bucket = grid
        .bucket_index(observed_range.0)
        .ok_or(BlockError::Malformed)?;
    let last_bucket = grid
        .bucket_index(observed_range.1)
        .ok_or(BlockError::Malformed)?;
    let first_covered = (0..usize::from(grid.bucket_count()))
        .find(|index| bit_is_set(coverage_mask, *index))
        .ok_or(BlockError::Malformed)?;
    let last_covered = (0..usize::from(grid.bucket_count()))
        .rfind(|index| bit_is_set(coverage_mask, *index))
        .ok_or(BlockError::Malformed)?;
    if first_bucket != first_covered || last_bucket != last_covered {
        return Err(BlockError::Malformed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ENCODE_BODY_CALLS, EntityDictionaryEntry, EntityMetric, EntitySeries, EntitySeriesBlock,
        METRIC_FLAG_CANONICAL, MetricAggregation, MetricStatus,
    };
    use crate::overview::block::BlockError;
    use crate::overview::limits::LIMIT;
    use crate::overview::web_index::{IndexStatus, TimeGrid};

    fn dictionary_entry(key: u8) -> EntityDictionaryEntry {
        EntityDictionaryEntry::new(vec![key], format!("entity-{key}"), &LIMIT)
            .expect("dictionary entry")
    }

    fn complete_metric(series: Vec<EntitySeries>) -> EntityMetric {
        EntityMetric::new(
            1,
            1,
            METRIC_FLAG_CANONICAL,
            1,
            MetricAggregation::Sum,
            MetricStatus::Complete,
            0.0,
            series,
            &LIMIT,
        )
        .expect("metric")
    }

    #[test]
    fn entity_series_round_trips_missing_and_observed_zero() {
        let grid = TimeGrid::for_range(0, 60_000_000).expect("grid");
        let series =
            EntitySeries::new(0, 0.0, 0.0, vec![0b0000_0010], vec![0], &LIMIT).expect("series");
        let block = EntitySeriesBlock::new(
            1,
            3,
            2,
            IndexStatus::Complete,
            (0, 60_000_000),
            grid,
            vec![0b0000_0011],
            vec![dictionary_entry(1)],
            vec![complete_metric(vec![series])],
            &LIMIT,
        )
        .expect("block");

        let decoded = EntitySeriesBlock::decode(&block.encode_body(), &LIMIT).expect("decode");

        assert_eq!(decoded, block);
        assert_eq!(decoded.metrics()[0].series()[0].value_at(0), None);
        assert_eq!(decoded.metrics()[0].series()[0].value_at(1), Some(0.0));
        assert_eq!(
            decoded.metrics()[0].series()[0]
                .observed_values()
                .collect::<Vec<_>>(),
            vec![(1, 0.0)]
        );
    }

    #[test]
    fn entity_series_decode_does_not_reencode_a_validated_body() {
        let grid = TimeGrid::for_range(0, 0).expect("grid");
        let series = EntitySeries::new(0, 1.0, 1.0, vec![1], vec![255], &LIMIT).expect("series");
        let block = EntitySeriesBlock::new(
            1,
            1,
            1,
            IndexStatus::Complete,
            (0, 0),
            grid,
            vec![1],
            vec![dictionary_entry(1)],
            vec![complete_metric(vec![series])],
            &LIMIT,
        )
        .expect("block");
        let body = block.encode_body();
        ENCODE_BODY_CALLS.with(|calls| calls.set(0));

        EntitySeriesBlock::decode(&body, &LIMIT).expect("decode");

        ENCODE_BODY_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    }

    #[test]
    fn entity_series_rejects_duplicate_dictionary_keys() {
        let grid = TimeGrid::for_range(0, 0).expect("grid");
        let result = EntitySeriesBlock::new(
            1,
            1,
            1,
            IndexStatus::Complete,
            (0, 0),
            grid,
            vec![1],
            vec![dictionary_entry(1), dictionary_entry(1)],
            Vec::new(),
            &LIMIT,
        );

        assert_eq!(result, Err(BlockError::Duplicate));
    }

    #[test]
    fn entity_series_rejects_series_outside_top_k() {
        let series = (0_u16..65)
            .map(|entity_ref| {
                let score = f64::from(65 - entity_ref);
                EntitySeries::new(entity_ref, score, score, vec![1], vec![255], &LIMIT)
                    .expect("series")
            })
            .collect();

        let result = EntityMetric::new(
            1,
            1,
            0,
            1,
            MetricAggregation::Max,
            MetricStatus::Complete,
            1.0,
            series,
            &LIMIT,
        );

        assert_eq!(result, Err(BlockError::AboveBound));
    }

    #[test]
    fn entity_series_rejects_non_finite_or_negative_scores() {
        assert_eq!(
            EntitySeries::new(0, f64::NAN, 1.0, vec![1], vec![1], &LIMIT),
            Err(BlockError::NonFiniteFloat)
        );
        assert_eq!(
            EntitySeries::new(0, -1.0, 1.0, vec![1], vec![1], &LIMIT),
            Err(BlockError::Malformed)
        );
        assert_eq!(
            EntitySeries::new(0, 1.0, -1.0, vec![1], vec![1], &LIMIT),
            Err(BlockError::Malformed)
        );
    }

    #[test]
    fn entity_series_rejects_an_unanchored_positive_scale() {
        assert_eq!(
            EntitySeries::new(0, 1.0, 10.0, vec![1], vec![1], &LIMIT),
            Err(BlockError::Malformed)
        );
    }

    #[test]
    fn resource_limited_metric_carries_no_partial_series() {
        let series = EntitySeries::new(0, 1.0, 1.0, vec![1], vec![255], &LIMIT).expect("series");

        let result = EntityMetric::new(
            1,
            1,
            0,
            1,
            MetricAggregation::Sum,
            MetricStatus::ResourceLimited,
            1.0,
            vec![series],
            &LIMIT,
        );

        assert_eq!(result, Err(BlockError::Malformed));
    }

    #[test]
    fn entity_series_reports_all_owned_heap_capacity() {
        let grid = TimeGrid::for_range(0, 60_000_000).expect("grid");
        let series = EntitySeries::new(0, 1.0, 1.0, vec![0b0000_0011], vec![255, 0], &LIMIT)
            .expect("series");
        let block = EntitySeriesBlock::new(
            1,
            1,
            1,
            IndexStatus::Complete,
            (0, 60_000_000),
            grid,
            vec![0b0000_0011],
            vec![dictionary_entry(1)],
            vec![complete_metric(vec![series])],
            &LIMIT,
        )
        .expect("block");
        let expected = block.coverage_mask.capacity()
            + block.dictionary.capacity() * size_of::<EntityDictionaryEntry>()
            + block
                .dictionary
                .iter()
                .map(|entry| entry.key.capacity() + entry.label.capacity())
                .sum::<usize>()
            + block.metrics.capacity() * size_of::<EntityMetric>()
            + block
                .metrics
                .iter()
                .map(|metric| {
                    metric.series.capacity() * size_of::<EntitySeries>()
                        + metric
                            .series
                            .iter()
                            .map(|series| {
                                series.present_mask.capacity() + series.quantized_values.capacity()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>();

        assert_eq!(block.resident_heap_bytes(), Some(expected));
    }
}
