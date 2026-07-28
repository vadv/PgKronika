use std::fmt::Write as _;
use std::fs::File;
use std::path::Path;

use kronika_reader::{
    BlockCodec, BlockDirectoryEntry, BlockKind, EntitySeriesBlock, FactFileHeader, FactFileReader,
    IndexStatus, LIMIT, MetricAggregation, MetricStatus, TimeGrid, UiSummaryBlock,
};

use crate::model::{
    OvfBlockContentOutput, OvfBlockOutput, OvfEntityDictionaryOutput, OvfEntityMetricOutput,
    OvfEntitySeriesItemOutput, OvfEntitySeriesOutput, OvfGridOutput, OvfHeaderOutput,
    OvfObservedRangeOutput, OvfOutput, OvfUiSummaryOutput, OvfUiSummaryViewOutput,
};
use crate::{DumpError, Options};

pub(crate) fn inspect_file(
    file: File,
    path: &Path,
    options: Options,
) -> Result<OvfOutput, DumpError> {
    let mut reader = FactFileReader::inspect(file, &LIMIT)
        .map_err(|error| DumpError::input("inspect OVF metadata", error))?;
    let directory = reader.directory().to_vec();
    let mut blocks = Vec::with_capacity(directory.len());
    for entry in directory {
        let content = if options.rows {
            block_content(&mut reader, entry, options)?
        } else {
            None
        };
        blocks.push(block_output(&entry, content));
    }
    Ok(OvfOutput {
        kind: "ovf",
        path: path.display().to_string(),
        file_bytes: reader.header().file_len,
        header: header_output(reader.header()),
        blocks,
    })
}

fn header_output(header: &FactFileHeader) -> OvfHeaderOutput {
    let identity = &header.identity;
    OvfHeaderOutput {
        fact_schema_version: identity.fact_schema_version,
        extractor_semantics_version: identity.extractor_semantics_version,
        registry_contract_version: identity.registry_contract_version,
        source_format_version: identity.source_format_version,
        pgm_source_id: identity.pgm_source_id,
        source_min_ts_us: identity.source_min_ts_us,
        source_max_ts_us: identity.source_max_ts_us,
        source_file_len: identity.source_file_len,
        source_descriptor: hex(&identity.source_descriptor.0),
        fact_key: hex(identity.fact_key.as_bytes()),
        segment_lineage_id: hex(&identity.segment_lineage_id.0),
        directory_count: header.directory_count,
    }
}

fn block_output(
    entry: &BlockDirectoryEntry,
    content: Option<OvfBlockContentOutput>,
) -> OvfBlockOutput {
    OvfBlockOutput {
        kind: BlockKind::from_code(entry.block_kind).map(block_kind_name),
        kind_code: entry.block_kind,
        logical_id: entry.logical_id,
        schema_version: entry.block_schema_version,
        required: entry.flags.required_for_schema,
        sorted: entry.flags.canonically_sorted,
        has_time_range: entry.flags.has_time_range,
        codec: match entry.flags.codec {
            BlockCodec::None => "none",
            BlockCodec::Zstd => "zstd",
        },
        stored_bytes: entry.stored_len,
        decoded_bytes: entry.decoded_len,
        items: entry.item_count,
        min_ts_us: entry.flags.has_time_range.then_some(entry.min_ts_us),
        max_ts_us: entry.flags.has_time_range.then_some(entry.max_ts_us),
        content,
    }
}

fn block_content(
    reader: &mut FactFileReader<File>,
    entry: BlockDirectoryEntry,
    options: Options,
) -> Result<Option<OvfBlockContentOutput>, DumpError> {
    match BlockKind::from_code(entry.block_kind) {
        Some(BlockKind::UiSummary) => reader
            .read_ui_summary(&LIMIT)
            .map(|block| ui_summary_output(&block))
            .map(OvfBlockContentOutput::UiSummary)
            .map(Some)
            .map_err(|error| DumpError::input("read OVF ui_summary", error)),
        Some(BlockKind::EntitySeries) => {
            let view_code = u16::try_from(entry.logical_id)
                .map_err(|_error| DumpError::message("OVF view code does not fit u16"))?;
            let block = reader
                .read_entity_series(view_code, &LIMIT)
                .map_err(|error| DumpError::input("read OVF entity_series", error))?
                .ok_or_else(|| DumpError::message("OVF entity_series body is missing"))?;
            entity_series_output(&block, options.limit)
                .map(OvfBlockContentOutput::EntitySeries)
                .map(Some)
        }
        Some(
            BlockKind::SourceManifest
            | BlockKind::EventObservations
            | BlockKind::EventFacts
            | BlockKind::LossCoverage
            | BlockKind::GaugeSamples
            | BlockKind::CounterSamples
            | BlockKind::ResetMarkers
            | BlockKind::EntityStates
            | BlockKind::StringTable,
        )
        | None => Ok(None),
    }
}

fn ui_summary_output(block: &UiSummaryBlock) -> OvfUiSummaryOutput {
    let timestamp_count = block.snapshot_times().len();
    let views = block
        .views()
        .iter()
        .map(|view| {
            let mut population_index = 0_usize;
            let populations = (0..timestamp_count)
                .map(|index| {
                    if !bit_is_set(view.snapshot_presence(), index) {
                        return None;
                    }
                    let population = view.populations().get(population_index).copied();
                    population_index += 1;
                    population
                })
                .collect();
            let notable = (0..timestamp_count)
                .map(|index| {
                    bit_is_set(view.snapshot_presence(), index)
                        .then(|| bit_is_set(view.notable_presence(), index))
                })
                .collect();
            let coverage_count = block
                .grid()
                .map_or(0, |grid| usize::from(grid.bucket_count()));
            OvfUiSummaryViewOutput {
                view_code: view.view_code(),
                view_revision: view.view_revision(),
                status: index_status_name(view.status()),
                populations,
                notable,
                coverage: expand_mask(view.coverage_mask(), coverage_count),
            }
        })
        .collect();
    OvfUiSummaryOutput {
        grid: block.grid().map(grid_output),
        snapshot_times_us: block.snapshot_times().to_vec(),
        views,
    }
}

fn entity_series_output(
    block: &EntitySeriesBlock,
    limit: usize,
) -> Result<OvfEntitySeriesOutput, DumpError> {
    let dictionary: Vec<_> = block
        .dictionary()
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            Ok(OvfEntityDictionaryOutput {
                entity_ref: u16::try_from(index)
                    .map_err(|_error| DumpError::message("OVF entity_ref does not fit u16"))?,
                key: hex(entry.key()),
                label: entry.label().to_owned(),
            })
        })
        .collect::<Result<_, DumpError>>()?;
    let bucket_count = usize::from(block.grid().bucket_count());
    let metrics = block
        .metrics()
        .iter()
        .map(|metric| {
            let series = metric
                .series()
                .iter()
                .take(limit)
                .map(|series| {
                    let entity = dictionary
                        .get(usize::from(series.entity_ref()))
                        .ok_or_else(|| {
                            DumpError::message("OVF entity_ref is outside dictionary")
                        })?;
                    Ok(OvfEntitySeriesItemOutput {
                        entity_ref: series.entity_ref(),
                        key: entity.key.clone(),
                        label: entity.label.clone(),
                        exact_score: series.exact_score(),
                        max_bucket_value: series.max_bucket_value(),
                        values: (0..bucket_count)
                            .map(|bucket| series.value_at(bucket))
                            .collect(),
                    })
                })
                .collect::<Result<_, DumpError>>()?;
            Ok(OvfEntityMetricOutput {
                metric_code: metric.metric_code(),
                metric_revision: metric.metric_revision(),
                flags: metric.flags(),
                unit_code: metric.unit_code(),
                aggregation: aggregation_name(metric.aggregation()),
                status: metric_status_name(metric.status()),
                cutoff_score: metric.cutoff_score(),
                series,
                truncated: metric.series().len() > limit,
            })
        })
        .collect::<Result<_, DumpError>>()?;
    let (first_us, last_us) = block.observed_range();
    Ok(OvfEntitySeriesOutput {
        view_code: block.view_code(),
        view_revision: block.view_revision(),
        identity_revision: block.identity_revision(),
        status: index_status_name(block.status()),
        observed_range: OvfObservedRangeOutput { first_us, last_us },
        grid: grid_output(block.grid()),
        coverage: expand_mask(block.coverage_mask(), bucket_count),
        dictionary,
        metrics,
    })
}

const fn grid_output(grid: TimeGrid) -> OvfGridOutput {
    OvfGridOutput {
        start_us: grid.start_us(),
        bucket_width_s: grid.bucket_width_s(),
        bucket_count: grid.bucket_count(),
    }
}

fn expand_mask(mask: &[u8], count: usize) -> Vec<bool> {
    (0..count).map(|index| bit_is_set(mask, index)).collect()
}

fn bit_is_set(mask: &[u8], index: usize) -> bool {
    mask.get(index / 8)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

const fn index_status_name(status: IndexStatus) -> &'static str {
    match status {
        IndexStatus::Complete => "complete",
        IndexStatus::Empty => "empty",
        IndexStatus::Gated => "gated",
        IndexStatus::UnsupportedType => "unsupported_type",
        IndexStatus::ResourceLimited => "resource_limited",
    }
}

const fn metric_status_name(status: MetricStatus) -> &'static str {
    match status {
        MetricStatus::Complete => "complete",
        MetricStatus::Gated => "gated",
        MetricStatus::UnsupportedType => "unsupported_type",
        MetricStatus::ResourceLimited => "resource_limited",
    }
}

const fn aggregation_name(aggregation: MetricAggregation) -> &'static str {
    match aggregation {
        MetricAggregation::Sum => "sum",
        MetricAggregation::Max => "max",
    }
}

const fn block_kind_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::SourceManifest => "source_manifest",
        BlockKind::EventObservations => "event_observations",
        BlockKind::EventFacts => "event_facts",
        BlockKind::LossCoverage => "loss_coverage",
        BlockKind::GaugeSamples => "gauge_samples",
        BlockKind::CounterSamples => "counter_samples",
        BlockKind::ResetMarkers => "reset_markers",
        BlockKind::EntityStates => "entity_states",
        BlockKind::StringTable => "string_table",
        BlockKind::UiSummary => "ui_summary",
        BlockKind::EntitySeries => "entity_series",
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
