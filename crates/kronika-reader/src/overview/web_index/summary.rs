use super::super::block::{BlockError, BlockKind, EncodableBlock};
use super::super::bytes::{ByteReader, ByteWriter};
use super::super::limits::Bounds;
use super::{IndexStatus, TimeGrid, bit_is_set, mask_len, validate_mask};

const SUMMARY_REVISION: u16 = 1;

/// Per-snapshot population and collection status of one UI view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSummary {
    view_code: u16,
    view_revision: u16,
    status: IndexStatus,
    snapshot_presence: Vec<u8>,
    populations: Vec<u64>,
    coverage_mask: Vec<u8>,
}

impl EncodableBlock for UiSummaryBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::UiSummary
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.views.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        self.snapshot_range()
    }

    fn encode(&self) -> Vec<u8> {
        self.encode_body()
    }
}

impl ViewSummary {
    /// Creates one view over the summary's shared timestamp table.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] when identity fields are zero, the mask can
    /// exceed the timestamp bound, or population count differs from its
    /// set-bit count.
    pub fn new(
        view_code: u16,
        view_revision: u16,
        status: IndexStatus,
        snapshot_presence: Vec<u8>,
        populations: Vec<u64>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if view_code == 0 || view_revision == 0 {
            return Err(BlockError::Malformed);
        }
        let maximum_mask = mask_len(
            usize::try_from(bounds.web_summary_timestamps)
                .map_err(|_error| BlockError::AboveBound)?,
        );
        if snapshot_presence.len() > maximum_mask {
            return Err(BlockError::AboveBound);
        }
        let set_bits = snapshot_presence
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum::<usize>();
        if populations.len() != set_bits {
            return Err(BlockError::Malformed);
        }
        Ok(Self {
            view_code,
            view_revision,
            status,
            snapshot_presence,
            populations,
            coverage_mask: Vec::new(),
        })
    }

    /// Stable view code.
    #[must_use]
    pub const fn view_code(&self) -> u16 {
        self.view_code
    }

    /// Projection revision used for this view.
    #[must_use]
    pub const fn view_revision(&self) -> u16 {
        self.view_revision
    }

    /// Final collection status.
    #[must_use]
    pub const fn status(&self) -> IndexStatus {
        self.status
    }

    /// Presence bits over the summary's shared timestamp table.
    #[must_use]
    pub fn snapshot_presence(&self) -> &[u8] {
        &self.snapshot_presence
    }

    /// Population values ordered by set bits in the shared timestamp mask.
    #[must_use]
    pub fn populations(&self) -> &[u64] {
        &self.populations
    }

    /// Bucket coverage derived from the shared timestamps.
    #[must_use]
    pub fn coverage_mask(&self) -> &[u8] {
        &self.coverage_mask
    }

    fn population_at_index(&self, index: usize) -> Option<u64> {
        if !bit_is_set(&self.snapshot_presence, index) {
            return None;
        }
        let population_index = (0..index)
            .filter(|candidate| bit_is_set(&self.snapshot_presence, *candidate))
            .count();
        self.populations.get(population_index).copied()
    }
}

/// Shared snapshot-time index and exact populations for all UI views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSummaryBlock {
    grid: Option<TimeGrid>,
    snapshot_times: Vec<i64>,
    views: Vec<ViewSummary>,
}

impl UiSummaryBlock {
    /// Canonical empty summary used when a segment has no indexed views.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            grid: None,
            snapshot_times: Vec::new(),
            views: Vec::new(),
        }
    }

    /// Builds a canonical summary and derives per-view bucket coverage.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for an unsafe bound, timestamps outside the grid,
    /// non-canonical timestamps, duplicate views, invalid masks, or an encoded
    /// body above the summary byte cap.
    pub fn new(
        grid: TimeGrid,
        snapshot_times: Vec<i64>,
        mut views: Vec<ViewSummary>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        let timestamp_count =
            u64::try_from(snapshot_times.len()).map_err(|_error| BlockError::AboveBound)?;
        if timestamp_count > bounds.web_summary_timestamps {
            return Err(BlockError::AboveBound);
        }
        let view_count = u64::try_from(views.len()).map_err(|_error| BlockError::AboveBound)?;
        if view_count > bounds.web_summary_views {
            return Err(BlockError::AboveBound);
        }
        if views.is_empty() {
            return if snapshot_times.is_empty() {
                Ok(Self::empty())
            } else {
                Err(BlockError::Malformed)
            };
        }
        if snapshot_times.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(BlockError::Unsorted);
        }
        if snapshot_times
            .iter()
            .any(|timestamp| grid.bucket_index(*timestamp).is_none())
        {
            return Err(BlockError::Malformed);
        }

        let presence_len = mask_len(snapshot_times.len());
        let coverage_len = mask_len(usize::from(grid.bucket_count()));
        for view in &mut views {
            validate_mask(&view.snapshot_presence, snapshot_times.len())?;
            if view.snapshot_presence.len() != presence_len {
                return Err(BlockError::Malformed);
            }
            view.coverage_mask = vec![0_u8; coverage_len];
            for (index, timestamp) in snapshot_times.iter().enumerate() {
                if bit_is_set(&view.snapshot_presence, index) {
                    let bucket = grid.bucket_index(*timestamp).ok_or(BlockError::Malformed)?;
                    view.coverage_mask[bucket / 8] |= 1 << (bucket % 8);
                }
            }
        }
        views.sort_unstable_by_key(ViewSummary::view_code);
        if views
            .windows(2)
            .any(|pair| pair[0].view_code == pair[1].view_code)
        {
            return Err(BlockError::Duplicate);
        }

        let block = Self {
            grid: Some(grid),
            snapshot_times,
            views,
        };
        let encoded_len =
            u64::try_from(block.encode_body().len()).map_err(|_error| BlockError::AboveBound)?;
        if encoded_len > bounds.web_summary_decoded_bytes {
            return Err(BlockError::AboveBound);
        }
        Ok(block)
    }

    /// Decodes and validates one canonical summary body.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for malformed bytes or any declared value above
    /// the supplied bounds.
    pub fn decode(body: &[u8], bounds: &Bounds) -> Result<Self, BlockError> {
        if body.is_empty() {
            return Ok(Self::empty());
        }
        if body.len() as u64 > bounds.web_summary_decoded_bytes {
            return Err(BlockError::AboveBound);
        }
        let mut reader = ByteReader::new(body);
        if reader.u16_le()? != SUMMARY_REVISION {
            return Err(BlockError::InvalidEnum);
        }
        let grid =
            TimeGrid::from_parts(reader.i64_le()?, reader.u32_le()?, reader.u16_le()?, bounds)?;
        let timestamp_count = reader.u32_le()?;
        if u64::from(timestamp_count) > bounds.web_summary_timestamps {
            return Err(BlockError::AboveBound);
        }
        let timestamp_capacity =
            usize::try_from(timestamp_count).map_err(|_error| BlockError::AboveBound)?;
        let mut snapshot_times = Vec::with_capacity(timestamp_capacity);
        for index in 0..timestamp_count {
            let delta = reader.uvarint(i64::MAX as u64)?;
            let delta = i64::try_from(delta).map_err(|_error| BlockError::AboveBound)?;
            if index > 0 && delta == 0 {
                return Err(BlockError::Unsorted);
            }
            let base = snapshot_times
                .last()
                .copied()
                .unwrap_or_else(|| grid.start_us());
            let timestamp = base.checked_add(delta).ok_or(BlockError::Malformed)?;
            snapshot_times.push(timestamp);
        }

        let view_count = reader.u16_le()?;
        if u64::from(view_count) > bounds.web_summary_views {
            return Err(BlockError::AboveBound);
        }
        let view_capacity = usize::from(view_count);
        let presence_len = mask_len(snapshot_times.len());
        let coverage_len = mask_len(usize::from(grid.bucket_count()));
        let mut views = Vec::with_capacity(view_capacity);
        let mut stored_coverages = Vec::with_capacity(view_capacity);
        for _ in 0..view_count {
            let view_code = reader.u16_le()?;
            let view_revision = reader.u16_le()?;
            let status = IndexStatus::from_code(reader.u8()?)?;
            let presence = reader.take(presence_len)?.to_vec();
            validate_mask(&presence, snapshot_times.len())?;
            let population_count = reader.u32_le()?;
            let expected_count = presence.iter().map(|byte| byte.count_ones()).sum::<u32>();
            if population_count != expected_count {
                return Err(BlockError::Malformed);
            }
            let population_capacity =
                usize::try_from(population_count).map_err(|_error| BlockError::AboveBound)?;
            let mut populations = Vec::with_capacity(population_capacity);
            for _ in 0..population_count {
                populations.push(reader.uvarint(u64::MAX)?);
            }
            let coverage = reader.take(coverage_len)?.to_vec();
            validate_mask(&coverage, usize::from(grid.bucket_count()))?;
            views.push(ViewSummary::new(
                view_code,
                view_revision,
                status,
                presence,
                populations,
                bounds,
            )?);
            stored_coverages.push(coverage);
        }
        reader.finish()?;
        if views
            .windows(2)
            .any(|pair| pair[0].view_code >= pair[1].view_code)
        {
            return Err(BlockError::Unsorted);
        }
        let block = Self::new(grid, snapshot_times, views, bounds)?;
        if block
            .views
            .iter()
            .zip(stored_coverages)
            .any(|(view, stored)| view.coverage_mask != stored)
        {
            return Err(BlockError::Malformed);
        }
        Ok(block)
    }

    /// Shared timestamp table.
    #[must_use]
    pub fn snapshot_times(&self) -> &[i64] {
        &self.snapshot_times
    }

    /// Canonically ordered view summaries.
    #[must_use]
    pub fn views(&self) -> &[ViewSummary] {
        &self.views
    }

    /// Last observed population of `view_code` at or before `at_us`.
    #[must_use]
    pub fn population_at(&self, view_code: u16, at_us: i64) -> Option<u64> {
        self.snapshot_at(view_code, at_us)
            .map(|(_timestamp, population)| population)
    }

    /// Last exact snapshot timestamp and population at or before `at_us`.
    #[must_use]
    pub fn snapshot_at(&self, view_code: u16, at_us: i64) -> Option<(i64, u64)> {
        let view = self
            .views
            .binary_search_by_key(&view_code, ViewSummary::view_code)
            .ok()
            .and_then(|index| self.views.get(index))?;
        let upper = self
            .snapshot_times
            .partition_point(|timestamp| *timestamp <= at_us);
        (0..upper).rev().find_map(|index| {
            view.population_at_index(index)
                .map(|population| (self.snapshot_times[index], population))
        })
    }

    /// Shared bucket grid, or `None` for the canonical empty summary.
    #[must_use]
    pub const fn grid(&self) -> Option<TimeGrid> {
        self.grid
    }

    pub(super) fn snapshot_range(&self) -> Option<(i64, i64)> {
        self.snapshot_times
            .first()
            .copied()
            .zip(self.snapshot_times.last().copied())
    }

    pub(in crate::overview) fn resident_heap_bytes(&self) -> Option<usize> {
        let snapshot_times = self
            .snapshot_times
            .capacity()
            .checked_mul(size_of::<i64>())?;
        let view_slots = self
            .views
            .capacity()
            .checked_mul(size_of::<ViewSummary>())?;
        let view_heap = self.views.iter().try_fold(0_usize, |total, view| {
            total
                .checked_add(view.snapshot_presence.capacity())?
                .checked_add(view.populations.capacity().checked_mul(size_of::<u64>())?)?
                .checked_add(view.coverage_mask.capacity())
        })?;

        snapshot_times
            .checked_add(view_slots)?
            .checked_add(view_heap)
    }

    pub(super) fn encode_body(&self) -> Vec<u8> {
        let Some(grid) = self.grid else {
            return Vec::new();
        };
        let mut writer = ByteWriter::new();
        writer.u16_le(SUMMARY_REVISION);
        writer.i64_le(grid.start_us());
        writer.u32_le(grid.bucket_width_s());
        writer.u16_le(grid.bucket_count());
        writer.u32_le(len_u32(self.snapshot_times.len()));
        for (index, timestamp) in self.snapshot_times.iter().enumerate() {
            let base = if index == 0 {
                grid.start_us()
            } else {
                self.snapshot_times[index - 1]
            };
            writer.uvarint(nonnegative_i64(timestamp - base));
        }
        writer.u16_le(len_u16(self.views.len()));
        for view in &self.views {
            writer.u16_le(view.view_code);
            writer.u16_le(view.view_revision);
            writer.u8(view.status.code());
            writer.bytes(&view.snapshot_presence);
            writer.u32_le(len_u32(view.populations.len()));
            for population in &view.populations {
                writer.uvarint(*population);
            }
            writer.bytes(&view.coverage_mask);
        }
        writer.into_bytes()
    }
}

fn len_u16(len: usize) -> u16 {
    match u16::try_from(len) {
        Ok(value) => value,
        Err(_error) => unreachable!("constructor validated the u16 wire length"),
    }
}

fn len_u32(len: usize) -> u32 {
    match u32::try_from(len) {
        Ok(value) => value,
        Err(_error) => unreachable!("constructor validated the u32 wire length"),
    }
}

fn nonnegative_i64(value: i64) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(_error) => unreachable!("constructor validated increasing timestamps"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{IndexStatus, TimeGrid};
    use super::{UiSummaryBlock, ViewSummary};
    use crate::overview::block::BlockError;
    use crate::overview::limits::LIMIT;

    #[test]
    fn ui_summary_round_trips_shared_snapshot_times() {
        let grid = TimeGrid::for_range(0, 120_000_000).expect("grid");
        let first = ViewSummary::new(
            1,
            4,
            IndexStatus::Complete,
            vec![0b0000_0101],
            vec![10, 12],
            &LIMIT,
        )
        .expect("first view");
        let second = ViewSummary::new(
            2,
            7,
            IndexStatus::Complete,
            vec![0b0000_0010],
            vec![3],
            &LIMIT,
        )
        .expect("second view");
        let block = UiSummaryBlock::new(
            grid,
            vec![0, 60_000_000, 120_000_000],
            vec![second, first],
            &LIMIT,
        )
        .expect("summary");

        let decoded = UiSummaryBlock::decode(&block.encode_body(), &LIMIT).expect("decode");

        assert_eq!(decoded, block);
        assert_eq!(decoded.views()[0].view_code(), 1);
        assert_eq!(decoded.snapshot_times(), &[0, 60_000_000, 120_000_000]);
    }

    #[test]
    fn ui_summary_population_belongs_to_each_present_snapshot() {
        let grid = TimeGrid::for_range(0, 50_000_000).expect("grid");
        let view = ViewSummary::new(
            1,
            1,
            IndexStatus::Complete,
            vec![0b0000_0011],
            vec![7, 11],
            &LIMIT,
        )
        .expect("view");
        let block = UiSummaryBlock::new(grid, vec![10_000_000, 50_000_000], vec![view], &LIMIT)
            .expect("summary");

        assert_eq!(block.population_at(1, 10_000_000), Some(7));
        assert_eq!(block.population_at(1, 49_999_999), Some(7));
        assert_eq!(block.population_at(1, 50_000_000), Some(11));
        assert_eq!(block.snapshot_at(1, 49_999_999), Some((10_000_000, 7)));
    }

    #[test]
    fn ui_summary_rejects_presence_population_mismatch() {
        assert_eq!(
            ViewSummary::new(
                1,
                1,
                IndexStatus::Complete,
                vec![0b0000_0101],
                vec![10],
                &LIMIT,
            ),
            Err(BlockError::Malformed)
        );
    }

    #[test]
    fn adaptive_grid_never_truncates_a_long_segment() {
        let last = 300 * 60_000_000;
        let grid = TimeGrid::for_range(0, last).expect("grid");

        assert!(grid.bucket_count() <= 256);
        assert_eq!(grid.bucket_width_s() % 60, 0);
        assert!(grid.bucket_end_us().expect("grid end") > last);
    }

    #[test]
    fn ui_summary_reports_all_owned_heap_capacity() {
        let grid = TimeGrid::for_range(0, 60_000_000).expect("grid");
        let view = ViewSummary::new(
            1,
            1,
            IndexStatus::Complete,
            vec![0b0000_0011],
            vec![7, 11],
            &LIMIT,
        )
        .expect("view");
        let block =
            UiSummaryBlock::new(grid, vec![0, 60_000_000], vec![view], &LIMIT).expect("summary");
        let expected = block.snapshot_times.capacity() * size_of::<i64>()
            + block.views.capacity() * size_of::<ViewSummary>()
            + block
                .views
                .iter()
                .map(|view| {
                    view.snapshot_presence.capacity()
                        + view.populations.capacity() * size_of::<u64>()
                        + view.coverage_mask.capacity()
                })
                .sum::<usize>();

        assert_eq!(block.resident_heap_bytes(), Some(expected));
    }
}
