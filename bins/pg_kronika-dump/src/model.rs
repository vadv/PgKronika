use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Output {
    Tree(TreeOutput),
    Pgm(PgmOutput),
    Ovf(OvfOutput),
    Journal(JournalOutput),
}

#[derive(Debug, Serialize)]
pub(crate) struct WindowsOutput {
    pub(crate) count: Option<u64>,
    pub(crate) first_us: Option<i64>,
    pub(crate) last_us: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DictionaryOutput {
    pub(crate) entries: u64,
    pub(crate) stored_bytes: u64,
    pub(crate) decoded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) decode_skipped: Option<&'static str>,
    pub(crate) share_of_file: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SectionOutput {
    pub(crate) type_id: String,
    pub(crate) type_name: Option<&'static str>,
    pub(crate) rows: u64,
    pub(crate) stored_bytes: u64,
    pub(crate) decoded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) decode_skipped: Option<&'static str>,
    pub(crate) ratio: Option<f64>,
    pub(crate) share_of_file: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rows_data: Option<Vec<Map<String, Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rows_skipped: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TotalsOutput {
    pub(crate) stored_bytes: u64,
    pub(crate) decoded_bytes: Option<u64>,
    pub(crate) ratio: Option<f64>,
}

#[derive(Debug)]
pub(crate) struct UnitStats {
    pub(crate) windows: WindowsOutput,
    pub(crate) dictionary: DictionaryOutput,
    pub(crate) sections: Vec<SectionOutput>,
    pub(crate) totals: TotalsOutput,
}

#[derive(Debug, Serialize)]
pub(crate) struct PgmOutput {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    pub(crate) segment_id: Option<i64>,
    pub(crate) file_bytes: u64,
    pub(crate) windows: WindowsOutput,
    pub(crate) dictionary: DictionaryOutput,
    pub(crate) sections: Vec<SectionOutput>,
    pub(crate) totals: TotalsOutput,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfHeaderOutput {
    pub(crate) fact_schema_version: u32,
    pub(crate) extractor_semantics_version: u32,
    pub(crate) registry_contract_version: u32,
    pub(crate) source_format_version: u32,
    pub(crate) source_min_ts_us: i64,
    pub(crate) source_max_ts_us: i64,
    pub(crate) source_file_len: u64,
    pub(crate) source_descriptor: String,
    pub(crate) fact_key: String,
    pub(crate) segment_lineage_id: String,
    pub(crate) directory_count: u32,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfBlockOutput {
    pub(crate) kind: Option<&'static str>,
    pub(crate) kind_code: u32,
    pub(crate) logical_id: u32,
    pub(crate) schema_version: u16,
    pub(crate) required: bool,
    pub(crate) sorted: bool,
    pub(crate) has_time_range: bool,
    pub(crate) codec: &'static str,
    pub(crate) stored_bytes: u64,
    pub(crate) decoded_bytes: u64,
    pub(crate) items: u32,
    pub(crate) min_ts_us: Option<i64>,
    pub(crate) max_ts_us: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<OvfBlockContentOutput>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum OvfBlockContentOutput {
    UiSummary(OvfUiSummaryOutput),
    EntitySeries(OvfEntitySeriesOutput),
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct OvfGridOutput {
    pub(crate) start_us: i64,
    pub(crate) bucket_width_s: u32,
    pub(crate) bucket_count: u16,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfUiSummaryViewOutput {
    pub(crate) view_code: u16,
    pub(crate) view_revision: u16,
    pub(crate) status: &'static str,
    pub(crate) populations: Vec<Option<u64>>,
    pub(crate) notable: Vec<Option<bool>>,
    pub(crate) coverage: Vec<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfUiSummaryOutput {
    pub(crate) grid: Option<OvfGridOutput>,
    pub(crate) snapshot_times_us: Vec<i64>,
    pub(crate) views: Vec<OvfUiSummaryViewOutput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfEntityDictionaryOutput {
    pub(crate) entity_ref: u16,
    pub(crate) key: String,
    pub(crate) label: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfEntitySeriesItemOutput {
    pub(crate) entity_ref: u16,
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) exact_score: f64,
    pub(crate) max_bucket_value: f64,
    pub(crate) values: Vec<Option<f64>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfEntityMetricOutput {
    pub(crate) metric_code: u16,
    pub(crate) metric_revision: u16,
    pub(crate) flags: u16,
    pub(crate) unit_code: u16,
    pub(crate) aggregation: &'static str,
    pub(crate) status: &'static str,
    pub(crate) cutoff_score: f64,
    pub(crate) series: Vec<OvfEntitySeriesItemOutput>,
    pub(crate) truncated: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfObservedRangeOutput {
    pub(crate) first_us: i64,
    pub(crate) last_us: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfEntitySeriesOutput {
    pub(crate) view_code: u16,
    pub(crate) view_revision: u16,
    pub(crate) identity_revision: u16,
    pub(crate) status: &'static str,
    pub(crate) observed_range: OvfObservedRangeOutput,
    pub(crate) grid: OvfGridOutput,
    pub(crate) coverage: Vec<bool>,
    pub(crate) dictionary: Vec<OvfEntityDictionaryOutput>,
    pub(crate) metrics: Vec<OvfEntityMetricOutput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OvfOutput {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    pub(crate) file_bytes: u64,
    pub(crate) header: OvfHeaderOutput,
    pub(crate) blocks: Vec<OvfBlockOutput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JournalHeaderOutput {
    pub(crate) state: &'static str,
    pub(crate) segment_id: Option<i64>,
    pub(crate) recorded_body_len: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JournalFrameOutput {
    pub(crate) offset: u64,
    pub(crate) part_bytes: u64,
    pub(crate) windows: u64,
    pub(crate) crc_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dictionary: Option<DictionaryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sections: Option<Vec<SectionOutput>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JournalDamageOutput {
    pub(crate) offset: u64,
    pub(crate) kind: &'static str,
    pub(crate) detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct RecoverableOutput {
    pub(crate) frames: u64,
    pub(crate) windows: u64,
    pub(crate) first_us: Option<i64>,
    pub(crate) last_us: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JournalOutput {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    pub(crate) header: JournalHeaderOutput,
    pub(crate) physical_bytes: u64,
    pub(crate) frames: Vec<JournalFrameOutput>,
    pub(crate) valid_prefix_bytes: u64,
    pub(crate) damage: Option<JournalDamageOutput>,
    pub(crate) recoverable: RecoverableOutput,
}

#[derive(Debug, Serialize)]
pub(crate) struct TreeJournalOutput {
    pub(crate) state: &'static str,
    pub(crate) segment_id: Option<i64>,
    pub(crate) frames: u64,
    pub(crate) bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) damage: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QuarantineOutput {
    pub(crate) id: String,
    pub(crate) reason: &'static str,
    pub(crate) bytes: u64,
    pub(crate) file_type: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct TreeSegmentOutput {
    pub(crate) segment_id: i64,
    pub(crate) pgm_bytes: u64,
    pub(crate) ovf: bool,
    pub(crate) sections: u64,
    pub(crate) stored_bytes: u64,
    pub(crate) decoded_bytes: Option<u64>,
    pub(crate) ratio: Option<f64>,
    pub(crate) windows: Option<u64>,
    pub(crate) first_window_us: Option<i64>,
    pub(crate) last_window_us: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TreeDayOutput {
    pub(crate) day: String,
    pub(crate) segments: Vec<TreeSegmentOutput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TreeTotalsOutput {
    pub(crate) segments: u64,
    pub(crate) pgm_bytes: u64,
    pub(crate) stored_bytes: u64,
    pub(crate) decoded_bytes: Option<u64>,
    pub(crate) ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TreeOutput {
    pub(crate) kind: &'static str,
    pub(crate) root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
    pub(crate) journal: TreeJournalOutput,
    pub(crate) quarantine: Vec<QuarantineOutput>,
    pub(crate) days: Vec<TreeDayOutput>,
    pub(crate) totals: TreeTotalsOutput,
}
