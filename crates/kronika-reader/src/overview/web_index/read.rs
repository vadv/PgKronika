//! Selective web-index reads over an admitted OVF.

use kronika_format::ReadAt;

use super::{EntitySeriesBlock, UiSummaryBlock};
use crate::overview::block::BlockKind;
use crate::overview::container::{
    CacheReadError, FactFileReader, FactReadStats, HeaderIdentity, validate_block_descriptor,
};
use crate::overview::limits::Bounds;

pub(crate) fn read_ui_summary<R: ReadAt>(
    reader: R,
    expected: &HeaderIdentity,
    bounds: &Bounds,
) -> Result<(UiSummaryBlock, FactReadStats), CacheReadError> {
    let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
    let entry = fact_reader
        .directory()
        .iter()
        .find(|entry| entry.block_kind == BlockKind::UiSummary.code() && entry.logical_id == 0)
        .copied()
        .ok_or(CacheReadError::Corrupt)?;
    let body = fact_reader
        .read_block(BlockKind::UiSummary, 0)?
        .ok_or(CacheReadError::Corrupt)?;
    let summary = UiSummaryBlock::decode(&body, bounds)?;
    validate_block_descriptor(&entry, &summary)?;
    Ok((summary, fact_reader.stats()))
}

pub(crate) fn read_entity_series<R: ReadAt>(
    reader: R,
    expected: &HeaderIdentity,
    view_code: u16,
    bounds: &Bounds,
) -> Result<(Option<EntitySeriesBlock>, FactReadStats), CacheReadError> {
    let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
    let logical_id = u32::from(view_code);
    let entry = fact_reader
        .directory()
        .iter()
        .find(|entry| {
            entry.block_kind == BlockKind::EntitySeries.code() && entry.logical_id == logical_id
        })
        .copied();
    let Some(body) = fact_reader.read_block(BlockKind::EntitySeries, logical_id)? else {
        return Ok((None, fact_reader.stats()));
    };
    let entry = entry.ok_or(CacheReadError::Corrupt)?;
    let series = EntitySeriesBlock::decode(&body, bounds)?;
    if series.view_code() != view_code {
        return Err(CacheReadError::Corrupt);
    }
    validate_block_descriptor(&entry, &series)?;
    Ok((Some(series), fact_reader.stats()))
}
