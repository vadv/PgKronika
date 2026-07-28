use std::fmt::Write as _;
use std::fs::File;
use std::path::Path;

use kronika_reader::{
    BlockCodec, BlockDirectoryEntry, BlockKind, FactFileHeader, FactFileReader, LIMIT,
};

use crate::model::{OvfBlockOutput, OvfHeaderOutput, OvfOutput};
use crate::{DumpError, Options};

pub(crate) fn inspect_file(
    file: File,
    path: &Path,
    _options: Options,
) -> Result<OvfOutput, DumpError> {
    let reader = FactFileReader::inspect(file, &LIMIT)
        .map_err(|error| DumpError::input("inspect OVF metadata", error))?;
    Ok(OvfOutput {
        kind: "ovf",
        path: path.display().to_string(),
        file_bytes: reader.header().file_len,
        header: header_output(reader.header()),
        blocks: reader.directory().iter().map(block_output).collect(),
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

fn block_output(entry: &BlockDirectoryEntry) -> OvfBlockOutput {
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
