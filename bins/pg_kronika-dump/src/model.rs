use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Output {
    Tree(TreeOutput),
    Pgm(PgmOutput),
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
