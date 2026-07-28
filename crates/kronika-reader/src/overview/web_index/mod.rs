mod build;
mod read;
mod series;
mod summary;

pub(super) use build::{WebIndexBlocks, build_web_index};
pub(crate) use read::{read_entity_series, read_ui_summary};
pub use series::{
    EntityDictionaryEntry, EntityMetric, EntitySeries, EntitySeriesBlock, METRIC_FLAG_CANONICAL,
    MetricAggregation, MetricStatus,
};
pub use summary::{UiSummaryBlock, ViewSummary};

use super::block::BlockError;
use super::limits::Bounds;

const BASE_BUCKET_US: i64 = 60_000_000;

/// Final collection state of one web-index view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexStatus {
    /// Every declared input was indexed.
    Complete,
    /// Inputs were available and contained no rows.
    Empty,
    /// A collection gate prevented the view from being observed.
    Gated,
    /// The source type has no matching projection contract.
    UnsupportedType,
    /// A hard writer bound prevented a complete result.
    ResourceLimited,
}

impl IndexStatus {
    const fn code(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::Empty => 1,
            Self::Gated => 2,
            Self::UnsupportedType => 3,
            Self::ResourceLimited => 4,
        }
    }

    const fn from_code(code: u8) -> Result<Self, BlockError> {
        match code {
            0 => Ok(Self::Complete),
            1 => Ok(Self::Empty),
            2 => Ok(Self::Gated),
            3 => Ok(Self::UnsupportedType),
            4 => Ok(Self::ResourceLimited),
            _ => Err(BlockError::InvalidEnum),
        }
    }
}

/// UTC-aligned, bounded bucket grid shared by web-index blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeGrid {
    start_us: i64,
    bucket_width_s: u32,
    bucket_count: u16,
}

impl TimeGrid {
    /// Builds the narrowest minute-multiple grid that covers an inclusive range.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::Malformed`] for an inverted range or arithmetic
    /// overflow.
    pub fn for_range(first_ts_us: i64, last_ts_us: i64) -> Result<Self, BlockError> {
        if first_ts_us > last_ts_us {
            return Err(BlockError::Malformed);
        }
        let duration = last_ts_us
            .checked_sub(first_ts_us)
            .ok_or(BlockError::Malformed)?;
        let base_buckets = duration
            .checked_div(BASE_BUCKET_US)
            .and_then(|value| value.checked_add(1))
            .ok_or(BlockError::Malformed)?;
        let mut width_minutes = base_buckets
            .checked_add(255)
            .and_then(|value| value.checked_div(256))
            .ok_or(BlockError::Malformed)?
            .max(1);
        loop {
            let width_us = width_minutes
                .checked_mul(BASE_BUCKET_US)
                .ok_or(BlockError::Malformed)?;
            let start_us = first_ts_us
                .div_euclid(width_us)
                .checked_mul(width_us)
                .ok_or(BlockError::Malformed)?;
            let count = last_ts_us
                .checked_sub(start_us)
                .and_then(|span| span.checked_div(width_us))
                .and_then(|value| value.checked_add(1))
                .ok_or(BlockError::Malformed)?;
            if count <= 256 {
                let bucket_width_s =
                    u32::try_from(width_us / 1_000_000).map_err(|_error| BlockError::AboveBound)?;
                return Ok(Self {
                    start_us,
                    bucket_width_s,
                    bucket_count: u16::try_from(count).map_err(|_error| BlockError::AboveBound)?,
                });
            }
            width_minutes = width_minutes.checked_add(1).ok_or(BlockError::Malformed)?;
        }
    }

    pub(super) fn from_parts(
        start_us: i64,
        bucket_width_s: u32,
        bucket_count: u16,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if bucket_width_s == 0
            || !bucket_width_s.is_multiple_of(60)
            || bucket_count == 0
            || u64::from(bucket_count) > bounds.web_buckets
        {
            return Err(BlockError::AboveBound);
        }
        let grid = Self {
            start_us,
            bucket_width_s,
            bucket_count,
        };
        grid.bucket_end_us()?;
        Ok(grid)
    }

    /// Inclusive start of the first bucket, microseconds UTC.
    #[must_use]
    pub const fn start_us(self) -> i64 {
        self.start_us
    }

    /// Width of one bucket in seconds.
    #[must_use]
    pub const fn bucket_width_s(self) -> u32 {
        self.bucket_width_s
    }

    /// Number of buckets.
    #[must_use]
    pub const fn bucket_count(self) -> u16 {
        self.bucket_count
    }

    /// Exclusive end of the grid.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::Malformed`] when the end overflows `i64`.
    pub fn bucket_end_us(self) -> Result<i64, BlockError> {
        let width_us = i64::from(self.bucket_width_s)
            .checked_mul(1_000_000)
            .ok_or(BlockError::Malformed)?;
        width_us
            .checked_mul(i64::from(self.bucket_count))
            .and_then(|span| self.start_us.checked_add(span))
            .ok_or(BlockError::Malformed)
    }

    pub(super) fn bucket_index(self, ts_us: i64) -> Option<usize> {
        if ts_us < self.start_us {
            return None;
        }
        let width_us = i64::from(self.bucket_width_s).checked_mul(1_000_000)?;
        let index = ts_us.checked_sub(self.start_us)?.checked_div(width_us)?;
        let index = usize::try_from(index).ok()?;
        (index < usize::from(self.bucket_count)).then_some(index)
    }
}

pub(super) const fn mask_len(bits: usize) -> usize {
    bits.div_ceil(8)
}

pub(super) fn bit_is_set(mask: &[u8], index: usize) -> bool {
    mask.get(index / 8)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

pub(super) fn validate_mask(mask: &[u8], bits: usize) -> Result<(), BlockError> {
    if mask.len() != mask_len(bits) {
        return Err(BlockError::Malformed);
    }
    let used = bits % 8;
    if used != 0 && mask.last().is_some_and(|byte| byte & (!0_u8 << used) != 0) {
        return Err(BlockError::Malformed);
    }
    Ok(())
}
