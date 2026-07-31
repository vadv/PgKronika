use super::super::block::{BlockError, BlockKind, EncodableBlock};
use super::super::bytes::{ByteReader, ByteWriter};
use super::super::limits::Bounds;
use super::{IndexStatus, TimeGrid, bit_is_set, mask_len, validate_mask};

const SUMMARY_REVISION: u16 = 2;

#[cfg(test)]
std::thread_local! {
    static ENCODE_BODY_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Outcome of one physical source read represented in the UI summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionReadState {
    /// Every source row was retained.
    Complete,
    /// The collector retained a configured exact subset.
    SourceLimit,
    /// `PostgreSQL` permissions hid rows or rejected the read.
    Permission,
    /// The source could not be read.
    ReadFailure,
    /// A collector bound or loss made the total unsafe.
    CollectorLimitOrLoss,
}

impl CollectionReadState {
    const fn code(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::SourceLimit => 1,
            Self::Permission => 2,
            Self::ReadFailure => 3,
            Self::CollectorLimitOrLoss => 4,
        }
    }

    const fn from_code(code: u8) -> Result<Self, BlockError> {
        match code {
            0 => Ok(Self::Complete),
            1 => Ok(Self::SourceLimit),
            2 => Ok(Self::Permission),
            3 => Ok(Self::ReadFailure),
            4 => Ok(Self::CollectorLimitOrLoss),
            _ => Err(BlockError::InvalidEnum),
        }
    }
}

/// Visibility `PostgreSQL` gave the collector for one source read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionVisibility {
    /// The source was fully visible.
    Full,
    /// Permissions restricted the visible rows.
    Restricted,
    /// A failed or lossy read cannot prove visibility.
    Unknown,
}

impl CollectionVisibility {
    const fn code(self) -> u8 {
        match self {
            Self::Full => 0,
            Self::Restricted => 1,
            Self::Unknown => 2,
        }
    }

    const fn from_code(code: u8) -> Result<Self, BlockError> {
        match code {
            0 => Ok(Self::Full),
            1 => Ok(Self::Restricted),
            2 => Ok(Self::Unknown),
            _ => Err(BlockError::InvalidEnum),
        }
    }
}

/// Factual row counts and source-read state for one view snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionStatus {
    collected: u64,
    source_total: Option<u64>,
    read_state: CollectionReadState,
    visibility: CollectionVisibility,
}

impl CollectionStatus {
    /// Creates a collection status after validating all count/state invariants.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::Malformed`] when counts contradict the state or
    /// visibility.
    pub fn new(
        collected: u64,
        source_total: Option<u64>,
        read_state: CollectionReadState,
        visibility: CollectionVisibility,
    ) -> Result<Self, BlockError> {
        if source_total.is_some_and(|total| collected > total) {
            return Err(BlockError::Malformed);
        }
        let valid = match read_state {
            CollectionReadState::Complete => {
                matches!(visibility, CollectionVisibility::Full)
                    && matches!(source_total, Some(total) if collected == total)
            }
            CollectionReadState::SourceLimit => {
                matches!(visibility, CollectionVisibility::Full)
                    && matches!(source_total, Some(total) if collected < total)
            }
            CollectionReadState::Permission => {
                matches!(visibility, CollectionVisibility::Restricted)
            }
            CollectionReadState::ReadFailure | CollectionReadState::CollectorLimitOrLoss => {
                matches!(visibility, CollectionVisibility::Unknown) && source_total.is_none()
            }
        };
        if !valid {
            return Err(BlockError::Malformed);
        }
        Ok(Self {
            collected,
            source_total,
            read_state,
            visibility,
        })
    }

    /// Rows durably retained for the view snapshot.
    #[must_use]
    pub const fn collected(self) -> u64 {
        self.collected
    }

    /// Exact source row count, or `None` when it was not proven.
    #[must_use]
    pub const fn source_total(self) -> Option<u64> {
        self.source_total
    }

    /// Result of the source read.
    #[must_use]
    pub const fn read_state(self) -> CollectionReadState {
        self.read_state
    }

    /// Visibility of the physical source.
    #[must_use]
    pub const fn visibility(self) -> CollectionVisibility {
        self.visibility
    }
}

/// Highest server-classified notable level for one exact view snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotableLevel {
    /// No notable observations in the snapshot bucket.
    None,
    /// Informational evidence that does not require urgent action.
    Info,
    /// Actionable degradation or contention evidence.
    Warning,
    /// Critical availability, integrity, or capacity evidence.
    Critical,
}

impl NotableLevel {
    const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Info => 1,
            Self::Warning => 2,
            Self::Critical => 3,
        }
    }

    const fn from_code(code: u8) -> Result<Self, BlockError> {
        match code {
            0 => Ok(Self::None),
            1 => Ok(Self::Info),
            2 => Ok(Self::Warning),
            3 => Ok(Self::Critical),
            _ => Err(BlockError::InvalidEnum),
        }
    }
}

/// Stored notable classification for one exact view snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notability {
    level: NotableLevel,
    count: u64,
}

impl Notability {
    /// Creates a validated notability pair.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::Malformed`] when `none` has a non-zero count or a
    /// non-`none` level has a zero count.
    pub const fn new(level: NotableLevel, count: u64) -> Result<Self, BlockError> {
        if matches!(
            (level, count),
            (NotableLevel::None, 1..)
                | (
                    NotableLevel::Info | NotableLevel::Warning | NotableLevel::Critical,
                    0
                )
        ) {
            return Err(BlockError::Malformed);
        }
        Ok(Self { level, count })
    }

    /// Highest stored notable level.
    #[must_use]
    pub const fn level(self) -> NotableLevel {
        self.level
    }

    /// Number of notable observations in the snapshot bucket.
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }
}

/// Per-snapshot population and collection status of one UI view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSummary {
    view_code: u16,
    view_revision: u16,
    status: IndexStatus,
    snapshot_presence: Vec<u8>,
    notable_presence: Vec<u8>,
    populations: Vec<u64>,
    notability: Vec<Notability>,
    collection_presence: Vec<u8>,
    collections: Vec<CollectionStatus>,
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
        notable_presence: Vec<u8>,
        populations: Vec<u64>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        let maximum_mask = mask_len(
            usize::try_from(bounds.web_summary_timestamps)
                .map_err(|_error| BlockError::AboveBound)?,
        );
        if snapshot_presence.len() > maximum_mask {
            return Err(BlockError::AboveBound);
        }
        let collection_presence = vec![0_u8; snapshot_presence.len()];
        Self::new_with_collection(
            view_code,
            view_revision,
            status,
            snapshot_presence,
            notable_presence,
            populations,
            collection_presence,
            Vec::new(),
            bounds,
        )
    }

    /// Creates one view with per-snapshot collection status.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for invalid identities, masks, counts, state
    /// invariants, or population values that disagree with `collected`.
    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments are the complete bounded wire representation of one view"
    )]
    pub fn new_with_collection(
        view_code: u16,
        view_revision: u16,
        status: IndexStatus,
        snapshot_presence: Vec<u8>,
        notable_presence: Vec<u8>,
        populations: Vec<u64>,
        collection_presence: Vec<u8>,
        collections: Vec<CollectionStatus>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        let notability =
            notability_from_presence(&snapshot_presence, &notable_presence, populations.len())?;
        Self::new_with_collection_and_notability(
            view_code,
            view_revision,
            status,
            snapshot_presence,
            notable_presence,
            populations,
            notability,
            collection_presence,
            collections,
            bounds,
        )
    }

    /// Creates one view with exact per-snapshot notability and collection status.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for invalid identities, masks, aligned counts, or
    /// collection state invariants.
    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments are the complete bounded wire representation of one view"
    )]
    pub fn new_with_collection_and_notability(
        view_code: u16,
        view_revision: u16,
        status: IndexStatus,
        snapshot_presence: Vec<u8>,
        notable_presence: Vec<u8>,
        populations: Vec<u64>,
        notability: Vec<Notability>,
        collection_presence: Vec<u8>,
        collections: Vec<CollectionStatus>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if view_code == 0 || view_revision == 0 {
            return Err(BlockError::Malformed);
        }
        let maximum_mask = mask_len(
            usize::try_from(bounds.web_summary_timestamps)
                .map_err(|_error| BlockError::AboveBound)?,
        );
        if snapshot_presence.len() > maximum_mask
            || notable_presence.len() > maximum_mask
            || collection_presence.len() > maximum_mask
        {
            return Err(BlockError::AboveBound);
        }
        if snapshot_presence.len() != notable_presence.len()
            || snapshot_presence.len() != collection_presence.len()
            || notable_presence
                .iter()
                .zip(&snapshot_presence)
                .any(|(notable, present)| notable & !present != 0)
            || collection_presence
                .iter()
                .zip(&snapshot_presence)
                .any(|(collection, present)| collection & !present != 0)
        {
            return Err(BlockError::Malformed);
        }
        let set_bits = snapshot_presence
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum::<usize>();
        if populations.len() != set_bits || notability.len() != set_bits {
            return Err(BlockError::Malformed);
        }
        let collection_bits = collection_presence
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum::<usize>();
        if collections.len() != collection_bits {
            return Err(BlockError::Malformed);
        }
        let mut population_index = 0;
        let mut collection_index = 0;
        let mask_bits = snapshot_presence
            .len()
            .checked_mul(8)
            .ok_or(BlockError::AboveBound)?;
        for index in 0..mask_bits {
            let population = if bit_is_set(&snapshot_presence, index) {
                let value = populations
                    .get(population_index)
                    .copied()
                    .ok_or(BlockError::Malformed)?;
                let notable = notability
                    .get(population_index)
                    .copied()
                    .ok_or(BlockError::Malformed)?;
                if bit_is_set(&notable_presence, index) != (notable.level() != NotableLevel::None) {
                    return Err(BlockError::Malformed);
                }
                population_index += 1;
                Some(value)
            } else {
                None
            };
            if bit_is_set(&collection_presence, index) {
                let collection = collections
                    .get(collection_index)
                    .copied()
                    .ok_or(BlockError::Malformed)?;
                collection_index += 1;
                if population != Some(collection.collected()) {
                    return Err(BlockError::Malformed);
                }
            }
        }
        Ok(Self {
            view_code,
            view_revision,
            status,
            snapshot_presence,
            notable_presence,
            populations,
            notability,
            collection_presence,
            collections,
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

    /// Notable bits over the summary's shared timestamp table.
    #[must_use]
    pub fn notable_presence(&self) -> &[u8] {
        &self.notable_presence
    }

    /// Population values ordered by set bits in the shared timestamp mask.
    #[must_use]
    pub fn populations(&self) -> &[u64] {
        &self.populations
    }

    /// Notable classifications aligned with `populations`.
    #[must_use]
    pub fn notability(&self) -> &[Notability] {
        &self.notability
    }

    /// Collection-status presence bits over the shared timestamp table.
    #[must_use]
    pub fn collection_presence(&self) -> &[u8] {
        &self.collection_presence
    }

    /// Collection statuses ordered by set bits in `collection_presence`.
    #[must_use]
    pub fn collections(&self) -> &[CollectionStatus] {
        &self.collections
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

    fn collection_at_index(&self, index: usize) -> Option<CollectionStatus> {
        if !bit_is_set(&self.collection_presence, index) {
            return None;
        }
        let collection_index = (0..index)
            .filter(|candidate| bit_is_set(&self.collection_presence, *candidate))
            .count();
        self.collections.get(collection_index).copied()
    }

    fn notability_at_index(&self, index: usize) -> Option<Notability> {
        if !bit_is_set(&self.snapshot_presence, index) {
            return None;
        }
        let population_index = (0..index)
            .filter(|candidate| bit_is_set(&self.snapshot_presence, *candidate))
            .count();
        self.notability.get(population_index).copied()
    }
}

fn notability_from_presence(
    snapshot_presence: &[u8],
    notable_presence: &[u8],
    population_count: usize,
) -> Result<Vec<Notability>, BlockError> {
    let mut result = Vec::with_capacity(population_count);
    let mask_bits = snapshot_presence
        .len()
        .checked_mul(8)
        .ok_or(BlockError::AboveBound)?;
    for index in 0..mask_bits {
        if bit_is_set(snapshot_presence, index) {
            result.push(if bit_is_set(notable_presence, index) {
                Notability::new(NotableLevel::Warning, 1)?
            } else {
                Notability::new(NotableLevel::None, 0)?
            });
        }
    }
    if result.len() != population_count {
        return Err(BlockError::Malformed);
    }
    Ok(result)
}

/// Shared snapshot-time index and exact populations for all UI views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSummaryBlock {
    grid: Option<TimeGrid>,
    snapshot_times: Vec<i64>,
    views: Vec<ViewSummary>,
}

/// Exact snapshots adjacent to the selected snapshot of one UI view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotNeighbors {
    /// Previous present snapshot of the same view.
    pub previous: Option<i64>,
    /// Greatest present snapshot at or before the requested timestamp.
    pub current: i64,
    /// Next present snapshot of the same view.
    pub next: Option<i64>,
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
        views: Vec<ViewSummary>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        Self::from_parts(grid, snapshot_times, views, bounds, None)
    }

    fn from_parts(
        grid: TimeGrid,
        snapshot_times: Vec<i64>,
        mut views: Vec<ViewSummary>,
        bounds: &Bounds,
        decoded_len: Option<usize>,
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
            validate_mask(&view.collection_presence, snapshot_times.len())?;
            if view.snapshot_presence.len() != presence_len
                || view.collection_presence.len() != presence_len
            {
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
        let encoded_len = decoded_len.unwrap_or_else(|| block.encode_body().len());
        let encoded_len = u64::try_from(encoded_len).map_err(|_error| BlockError::AboveBound)?;
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
        let revision = reader.u16_le()?;
        if revision != SUMMARY_REVISION {
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
            let notable = reader.take(presence_len)?.to_vec();
            validate_mask(&notable, snapshot_times.len())?;
            let population_count = reader.u32_le()?;
            let expected_count = presence.iter().map(|byte| byte.count_ones()).sum::<u32>();
            if population_count != expected_count {
                return Err(BlockError::Malformed);
            }
            let population_capacity =
                usize::try_from(population_count).map_err(|_error| BlockError::AboveBound)?;
            let mut populations = Vec::with_capacity(population_capacity);
            let mut notability = Vec::with_capacity(population_capacity);
            for _ in 0..population_count {
                populations.push(reader.uvarint(u64::MAX)?);
                notability.push(decode_notability(&mut reader)?);
            }
            let (collection_presence, collections) =
                decode_collections(&mut reader, presence_len, snapshot_times.len())?;
            let coverage = reader.take(coverage_len)?.to_vec();
            validate_mask(&coverage, usize::from(grid.bucket_count()))?;
            views.push(ViewSummary::new_with_collection_and_notability(
                view_code,
                view_revision,
                status,
                presence,
                notable,
                populations,
                notability,
                collection_presence,
                collections,
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
        let block = Self::from_parts(grid, snapshot_times, views, bounds, Some(body.len()))?;
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
        self.snapshot_state_at(view_code, at_us)
            .map(|(timestamp, population, _notable)| (timestamp, population))
    }

    /// Exact previous, current, and next snapshots for `view_code`.
    #[must_use]
    pub fn snapshot_neighbors(&self, view_code: u16, at_us: i64) -> Option<SnapshotNeighbors> {
        let view = self
            .views
            .binary_search_by_key(&view_code, ViewSummary::view_code)
            .ok()
            .and_then(|index| self.views.get(index))?;
        let upper = self
            .snapshot_times
            .partition_point(|timestamp| *timestamp <= at_us);
        let current_index = (0..upper)
            .rev()
            .find(|index| bit_is_set(&view.snapshot_presence, *index))?;
        let previous = (0..current_index)
            .rev()
            .find(|index| bit_is_set(&view.snapshot_presence, *index))
            .map(|index| self.snapshot_times[index]);
        let next = (current_index + 1..self.snapshot_times.len())
            .find(|index| bit_is_set(&view.snapshot_presence, *index))
            .map(|index| self.snapshot_times[index]);

        Some(SnapshotNeighbors {
            previous,
            current: self.snapshot_times[current_index],
            next,
        })
    }

    /// Last exact snapshot timestamp, population, and notable state.
    #[must_use]
    pub fn snapshot_state_at(&self, view_code: u16, at_us: i64) -> Option<(i64, u64, bool)> {
        self.snapshot_notability_at(view_code, at_us)
            .map(|(timestamp, population, notability)| {
                (
                    timestamp,
                    population,
                    notability.level() != NotableLevel::None,
                )
            })
    }

    /// Last exact snapshot timestamp, population, and stored notability.
    #[must_use]
    pub fn snapshot_notability_at(
        &self,
        view_code: u16,
        at_us: i64,
    ) -> Option<(i64, u64, Notability)> {
        let view = self
            .views
            .binary_search_by_key(&view_code, ViewSummary::view_code)
            .ok()
            .and_then(|index| self.views.get(index))?;
        let upper = self
            .snapshot_times
            .partition_point(|timestamp| *timestamp <= at_us);
        (0..upper).rev().find_map(|index| {
            view.population_at_index(index).and_then(|population| {
                view.notability_at_index(index)
                    .map(|notability| (self.snapshot_times[index], population, notability))
            })
        })
    }

    /// Last collection status of `view_code` at or before `at_us`.
    #[must_use]
    pub fn collection_state_at(
        &self,
        view_code: u16,
        at_us: i64,
    ) -> Option<(i64, CollectionStatus)> {
        let view = self
            .views
            .binary_search_by_key(&view_code, ViewSummary::view_code)
            .ok()
            .and_then(|index| self.views.get(index))?;
        let upper = self
            .snapshot_times
            .partition_point(|timestamp| *timestamp <= at_us);
        (0..upper).rev().find_map(|index| {
            view.collection_at_index(index)
                .map(|status| (self.snapshot_times[index], status))
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
                .checked_add(view.notable_presence.capacity())?
                .checked_add(view.populations.capacity().checked_mul(size_of::<u64>())?)?
                .checked_add(
                    view.notability
                        .capacity()
                        .checked_mul(size_of::<Notability>())?,
                )?
                .checked_add(view.collection_presence.capacity())?
                .checked_add(
                    view.collections
                        .capacity()
                        .checked_mul(size_of::<CollectionStatus>())?,
                )?
                .checked_add(view.coverage_mask.capacity())
        })?;

        snapshot_times
            .checked_add(view_slots)?
            .checked_add(view_heap)
    }

    pub(super) fn encode_body(&self) -> Vec<u8> {
        #[cfg(test)]
        ENCODE_BODY_CALLS.with(|calls| calls.set(calls.get() + 1));

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
            writer.bytes(&view.notable_presence);
            writer.u32_le(len_u32(view.populations.len()));
            for (population, notability) in view.populations.iter().zip(&view.notability) {
                writer.uvarint(*population);
                writer.u8(notability.level().code());
                writer.uvarint(notability.count());
            }
            writer.bytes(&view.collection_presence);
            for collection in &view.collections {
                writer.uvarint(collection.collected());
                write_optional_uvarint(&mut writer, collection.source_total());
                writer.u8(collection.read_state().code());
                writer.u8(collection.visibility().code());
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

fn write_optional_uvarint(writer: &mut ByteWriter, value: Option<u64>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.uvarint(value);
        }
        None => writer.u8(0),
    }
}

fn read_optional_uvarint(reader: &mut ByteReader<'_>) -> Result<Option<u64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.uvarint(u64::MAX)?)),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn decode_notability(reader: &mut ByteReader<'_>) -> Result<Notability, BlockError> {
    Notability::new(
        NotableLevel::from_code(reader.u8()?)?,
        reader.uvarint(u64::MAX)?,
    )
}

fn decode_collections(
    reader: &mut ByteReader<'_>,
    presence_len: usize,
    timestamp_count: usize,
) -> Result<(Vec<u8>, Vec<CollectionStatus>), BlockError> {
    let presence = reader.take(presence_len)?.to_vec();
    validate_mask(&presence, timestamp_count)?;
    let count = presence
        .iter()
        .map(|byte| byte.count_ones() as usize)
        .sum::<usize>();
    let mut collections = Vec::with_capacity(count);
    for _ in 0..count {
        collections.push(CollectionStatus::new(
            reader.uvarint(u64::MAX)?,
            read_optional_uvarint(reader)?,
            CollectionReadState::from_code(reader.u8()?)?,
            CollectionVisibility::from_code(reader.u8()?)?,
        )?);
    }
    Ok((presence, collections))
}

#[cfg(test)]
mod tests {
    use super::super::{IndexStatus, TimeGrid};
    use super::{
        CollectionReadState, CollectionStatus, CollectionVisibility, ENCODE_BODY_CALLS, Notability,
        NotableLevel, SnapshotNeighbors, UiSummaryBlock, ViewSummary,
    };
    use crate::overview::block::BlockError;
    use crate::overview::bytes::ByteWriter;
    use crate::overview::limits::LIMIT;

    fn old_summary_body_without_collection() -> Vec<u8> {
        let mut writer = ByteWriter::new();
        writer.u16_le(1);
        writer.i64_le(0);
        writer.u32_le(60);
        writer.u16_le(1);
        writer.u32_le(1);
        writer.uvarint(100);
        writer.u16_le(1);
        writer.u16_le(1);
        writer.u16_le(1);
        writer.u8(IndexStatus::Complete.code());
        writer.u8(1);
        writer.u8(0);
        writer.u32_le(1);
        writer.uvarint(500);
        writer.u8(1);
        writer.into_bytes()
    }

    fn raw_summary_body(collection_mask: u8, read_state: u8, visibility: u8) -> Vec<u8> {
        let mut writer = ByteWriter::new();
        writer.u16_le(2);
        writer.i64_le(0);
        writer.u32_le(60);
        writer.u16_le(1);
        writer.u32_le(1);
        writer.uvarint(100);
        writer.u16_le(1);
        writer.u16_le(1);
        writer.u16_le(1);
        writer.u8(IndexStatus::Complete.code());
        writer.u8(1);
        writer.u8(0);
        writer.u32_le(1);
        writer.uvarint(500);
        writer.u8(NotableLevel::None.code());
        writer.uvarint(0);
        writer.u8(collection_mask);
        writer.uvarint(500);
        writer.u8(1);
        writer.uvarint(4_800);
        writer.u8(read_state);
        writer.u8(visibility);
        writer.u8(1);
        writer.into_bytes()
    }

    fn summary_with_presence(
        snapshot_times: &[i64],
        view_code: u16,
        presence: &[bool],
    ) -> UiSummaryBlock {
        let grid = TimeGrid::for_range(
            *snapshot_times.first().expect("first timestamp"),
            *snapshot_times.last().expect("last timestamp"),
        )
        .expect("grid");
        let mut mask = vec![0_u8; presence.len().div_ceil(8)];
        let mut populations = Vec::new();
        for (index, present) in presence.iter().copied().enumerate() {
            if present {
                mask[index / 8] |= 1 << (index % 8);
                populations.push(index as u64);
            }
        }
        let view = ViewSummary::new(
            view_code,
            1,
            IndexStatus::Complete,
            mask.clone(),
            vec![0; mask.len()],
            populations,
            &LIMIT,
        )
        .expect("view");
        UiSummaryBlock::new(grid, snapshot_times.to_vec(), vec![view], &LIMIT).expect("summary")
    }

    #[test]
    fn ui_summary_round_trips_shared_snapshot_times() {
        let grid = TimeGrid::for_range(0, 120_000_000).expect("grid");
        let first = ViewSummary::new(
            1,
            4,
            IndexStatus::Complete,
            vec![0b0000_0101],
            vec![0b0000_0100],
            vec![10, 12],
            &LIMIT,
        )
        .expect("first view");
        let second = ViewSummary::new(
            2,
            7,
            IndexStatus::Complete,
            vec![0b0000_0010],
            vec![0],
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
    fn ui_summary_revision_two_round_trips_collection_and_notability() {
        let status = CollectionStatus::new(
            500,
            Some(4_800),
            CollectionReadState::SourceLimit,
            CollectionVisibility::Full,
        )
        .expect("valid source limit");
        assert_eq!(status.collected(), 500);
        assert_eq!(status.source_total(), Some(4_800));
        assert_eq!(status.read_state(), CollectionReadState::SourceLimit);
        assert_eq!(status.visibility(), CollectionVisibility::Full);
        let view = ViewSummary::new_with_collection(
            1,
            1,
            IndexStatus::Complete,
            vec![1],
            vec![0],
            vec![500],
            vec![1],
            vec![status],
            &LIMIT,
        )
        .expect("view with collection");
        let block = UiSummaryBlock::new(
            TimeGrid::for_range(100, 100).expect("grid"),
            vec![100],
            vec![view],
            &LIMIT,
        )
        .expect("summary");
        let body = block.encode_body();

        assert_eq!(u16::from_le_bytes([body[0], body[1]]), 2);
        let decoded = UiSummaryBlock::decode(&body, &LIMIT).expect("revision two");
        assert_eq!(decoded.collection_state_at(1, 100), Some((100, status)));
        assert_eq!(
            decoded.snapshot_notability_at(1, 100),
            Some((
                100,
                500,
                Notability::new(NotableLevel::None, 0).expect("notability"),
            ))
        );

        assert!(
            UiSummaryBlock::decode(&old_summary_body_without_collection(), &LIMIT).is_err(),
            "the undeployed old layout is intentionally unsupported"
        );
    }

    #[test]
    fn collection_status_rejects_non_factual_state_combinations() {
        let invalid = [
            (
                500,
                Some(400),
                CollectionReadState::SourceLimit,
                CollectionVisibility::Full,
            ),
            (
                500,
                Some(501),
                CollectionReadState::Complete,
                CollectionVisibility::Full,
            ),
            (
                500,
                Some(500),
                CollectionReadState::SourceLimit,
                CollectionVisibility::Full,
            ),
            (
                500,
                Some(500),
                CollectionReadState::ReadFailure,
                CollectionVisibility::Unknown,
            ),
            (
                500,
                Some(500),
                CollectionReadState::CollectorLimitOrLoss,
                CollectionVisibility::Unknown,
            ),
            (
                500,
                None,
                CollectionReadState::Permission,
                CollectionVisibility::Full,
            ),
        ];
        for (collected, total, read_state, visibility) in invalid {
            assert_eq!(
                CollectionStatus::new(collected, total, read_state, visibility),
                Err(BlockError::Malformed)
            );
        }
    }

    #[test]
    fn collection_presence_requires_the_same_snapshot_population() {
        let status = CollectionStatus::new(
            5,
            Some(10),
            CollectionReadState::SourceLimit,
            CollectionVisibility::Full,
        )
        .expect("valid status");
        assert_eq!(
            ViewSummary::new_with_collection(
                1,
                1,
                IndexStatus::Complete,
                vec![1],
                vec![0],
                vec![6],
                vec![1],
                vec![status],
                &LIMIT,
            ),
            Err(BlockError::Malformed)
        );
        assert_eq!(
            ViewSummary::new_with_collection(
                1,
                1,
                IndexStatus::Complete,
                vec![1],
                vec![0],
                vec![5],
                vec![2],
                vec![status],
                &LIMIT,
            ),
            Err(BlockError::Malformed)
        );
    }

    #[test]
    fn ui_summary_rejects_malformed_collection_wire_data() {
        assert_eq!(
            UiSummaryBlock::decode(
                &raw_summary_body(
                    0b0000_0010,
                    CollectionReadState::SourceLimit.code(),
                    CollectionVisibility::Full.code(),
                ),
                &LIMIT,
            ),
            Err(BlockError::Malformed)
        );
        assert_eq!(
            UiSummaryBlock::decode(
                &raw_summary_body(1, 99, CollectionVisibility::Full.code()),
                &LIMIT,
            ),
            Err(BlockError::InvalidEnum)
        );
        assert_eq!(
            UiSummaryBlock::decode(
                &raw_summary_body(1, CollectionReadState::SourceLimit.code(), 99,),
                &LIMIT,
            ),
            Err(BlockError::InvalidEnum)
        );

        let body = raw_summary_body(
            1,
            CollectionReadState::SourceLimit.code(),
            CollectionVisibility::Full.code(),
        );
        let tight = crate::overview::limits::Bounds {
            web_summary_decoded_bytes: body.len() as u64 - 1,
            ..LIMIT
        };
        assert_eq!(
            UiSummaryBlock::decode(&body, &tight),
            Err(BlockError::AboveBound)
        );

        let mut unknown_revision = raw_summary_body(
            1,
            CollectionReadState::SourceLimit.code(),
            CollectionVisibility::Full.code(),
        );
        unknown_revision[..2].copy_from_slice(&3_u16.to_le_bytes());
        assert_eq!(
            UiSummaryBlock::decode(&unknown_revision, &LIMIT),
            Err(BlockError::InvalidEnum)
        );
    }

    #[test]
    fn ui_summary_decode_does_not_reencode_a_validated_body() {
        let grid = TimeGrid::for_range(0, 0).expect("grid");
        let view = ViewSummary::new(
            1,
            1,
            IndexStatus::Complete,
            vec![1],
            vec![0],
            vec![1],
            &LIMIT,
        )
        .expect("view");
        let block = UiSummaryBlock::new(grid, vec![0], vec![view], &LIMIT).expect("summary");
        let body = block.encode_body();
        ENCODE_BODY_CALLS.with(|calls| calls.set(0));

        UiSummaryBlock::decode(&body, &LIMIT).expect("decode");

        ENCODE_BODY_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    }

    #[test]
    fn ui_summary_population_belongs_to_each_present_snapshot() {
        let grid = TimeGrid::for_range(0, 50_000_000).expect("grid");
        let view = ViewSummary::new(
            1,
            1,
            IndexStatus::Complete,
            vec![0b0000_0011],
            vec![0b0000_0010],
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
        assert_eq!(
            block.snapshot_state_at(1, 10_000_000),
            Some((10_000_000, 7, false))
        );
        assert_eq!(
            block.snapshot_state_at(1, 50_000_000),
            Some((50_000_000, 11, true))
        );
    }

    #[test]
    fn neighbors_skip_timestamps_where_the_view_is_absent() {
        let block = summary_with_presence(
            &[10_000_000, 20_000_000, 30_000_000, 40_000_000],
            1,
            &[true, false, true, true],
        );

        assert_eq!(
            block.snapshot_neighbors(1, 35_000_000),
            Some(SnapshotNeighbors {
                previous: Some(10_000_000),
                current: 30_000_000,
                next: Some(40_000_000),
            })
        );
        assert_eq!(block.snapshot_neighbors(1, 9_999_999), None);
    }

    #[test]
    fn neighbors_select_exact_boundaries_and_keep_missing_sides_optional() {
        let block = summary_with_presence(
            &[10_000_000, 20_000_000, 30_000_000],
            1,
            &[true, false, true],
        );

        assert_eq!(
            block.snapshot_neighbors(1, 10_000_000),
            Some(SnapshotNeighbors {
                previous: None,
                current: 10_000_000,
                next: Some(30_000_000),
            })
        );
        assert_eq!(
            block.snapshot_neighbors(1, 30_000_000),
            Some(SnapshotNeighbors {
                previous: Some(10_000_000),
                current: 30_000_000,
                next: None,
            })
        );
    }

    #[test]
    fn neighbors_reject_unknown_or_snapshotless_views() {
        let block = summary_with_presence(
            &[10_000_000, 20_000_000, 30_000_000],
            1,
            &[false, false, false],
        );

        assert_eq!(block.snapshot_neighbors(1, 30_000_000), None);
        assert_eq!(block.snapshot_neighbors(99, 30_000_000), None);
    }

    #[test]
    fn ui_summary_rejects_presence_population_mismatch() {
        assert_eq!(
            ViewSummary::new(
                1,
                1,
                IndexStatus::Complete,
                vec![0b0000_0101],
                vec![0],
                vec![10],
                &LIMIT,
            ),
            Err(BlockError::Malformed)
        );
    }

    #[test]
    fn ui_summary_rejects_notable_without_a_snapshot() {
        assert_eq!(
            ViewSummary::new(
                1,
                1,
                IndexStatus::Complete,
                vec![0b0000_0001],
                vec![0b0000_0010],
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
            vec![0],
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
                        + view.notable_presence.capacity()
                        + view.populations.capacity() * size_of::<u64>()
                        + view.notability.capacity() * size_of::<Notability>()
                        + view.collection_presence.capacity()
                        + view.collections.capacity() * size_of::<CollectionStatus>()
                        + view.coverage_mask.capacity()
                })
                .sum::<usize>();

        assert_eq!(block.resident_heap_bytes(), Some(expected));
    }
}
