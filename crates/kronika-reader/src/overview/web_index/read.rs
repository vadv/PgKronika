//! Selective web-index reads over an admitted OVF.

use kronika_format::ReadAt;

use super::{EntitySeriesBlock, UiSummaryBlock};
use crate::overview::block::BlockKind;
use crate::overview::container::{
    CacheReadError, FactFileReader, FactReadStats, HeaderIdentity, validate_block_descriptor,
};
use crate::overview::limits::Bounds;

impl<R: ReadAt> FactFileReader<R> {
    /// Reads, decodes, and validates the shared UI summary block.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when the required block is absent, corrupt,
    /// oversized, or contradicts its admitted directory descriptor.
    pub fn read_ui_summary(&mut self, bounds: &Bounds) -> Result<UiSummaryBlock, CacheReadError> {
        let entry = self
            .directory()
            .iter()
            .find(|entry| entry.block_kind == BlockKind::UiSummary.code() && entry.logical_id == 0)
            .copied()
            .ok_or(CacheReadError::Corrupt)?;
        let body = self
            .read_block(BlockKind::UiSummary, 0)?
            .ok_or(CacheReadError::Corrupt)?;
        let summary = UiSummaryBlock::decode(&body, bounds)?;
        validate_block_descriptor(&entry, &summary)?;
        Ok(summary)
    }

    /// Reads, decodes, and validates one independently addressed view series.
    ///
    /// # Errors
    ///
    /// Returns [`CacheReadError`] when the selected block is corrupt,
    /// oversized, or contradicts its view address or directory descriptor.
    pub fn read_entity_series(
        &mut self,
        view_code: u16,
        bounds: &Bounds,
    ) -> Result<Option<EntitySeriesBlock>, CacheReadError> {
        let logical_id = u32::from(view_code);
        let entry = self
            .directory()
            .iter()
            .find(|entry| {
                entry.block_kind == BlockKind::EntitySeries.code() && entry.logical_id == logical_id
            })
            .copied();
        let Some(body) = self.read_block(BlockKind::EntitySeries, logical_id)? else {
            return Ok(None);
        };
        let entry = entry.ok_or(CacheReadError::Corrupt)?;
        let series = EntitySeriesBlock::decode(&body, bounds)?;
        if series.view_code() != view_code {
            return Err(CacheReadError::Corrupt);
        }
        validate_block_descriptor(&entry, &series)?;
        Ok(Some(series))
    }
}

pub(crate) fn read_ui_summary<R: ReadAt>(
    reader: R,
    expected: &HeaderIdentity,
    bounds: &Bounds,
) -> Result<(UiSummaryBlock, FactReadStats), CacheReadError> {
    let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
    let summary = fact_reader.read_ui_summary(bounds)?;
    Ok((summary, fact_reader.stats()))
}

pub(crate) fn read_entity_series<R: ReadAt>(
    reader: R,
    expected: &HeaderIdentity,
    view_code: u16,
    bounds: &Bounds,
) -> Result<(Option<EntitySeriesBlock>, FactReadStats), CacheReadError> {
    let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
    let series = fact_reader.read_entity_series(view_code, bounds)?;
    Ok((series, fact_reader.stats()))
}
