//! Typed wire responses shared by handlers and the generated `OpenAPI` contract.

use std::collections::BTreeMap;

use kronika_reader::{
    DiffAt, DiffPoint, OutRow, Reason, Scalar, SectionPage, SeriesDiff, Value as CellValue,
};
use serde::Serialize;
use utoipa::{PartialSchema, ToSchema};

use crate::overview::dto::{CoverageSpanDto, TimelineMetaDto};

/// API and on-disk format versions served by this process.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct VersionResponse {
    pub(crate) api: &'static str,
    pub(crate) format_version: u32,
}

/// One physical column exposed by a logical section.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SectionColumnResponse {
    pub(crate) name: &'static str,
    #[serde(rename = "type")]
    pub(crate) value_type: &'static str,
    pub(crate) class: &'static str,
}

/// Registry metadata for one logical section.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SectionCatalogEntry {
    pub(crate) name: &'static str,
    pub(crate) semantics: &'static str,
    pub(crate) sort_key: &'static [&'static str],
    pub(crate) columns: Vec<SectionColumnResponse>,
}

/// Complete logical section catalog.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SectionsResponse {
    pub(crate) sections: Vec<SectionCatalogEntry>,
}

/// Row count for one logical section in a segment.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SegmentSectionResponse {
    pub(crate) name: &'static str,
    pub(crate) rows: u64,
}

/// Catalog-only metadata for one retained segment.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SegmentResponse {
    pub(crate) segment_id: String,
    pub(crate) min_ts: i64,
    pub(crate) max_ts: i64,
    pub(crate) sections: Vec<SegmentSectionResponse>,
}

/// Segments overlapping the requested time range.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SegmentsResponse {
    pub(crate) segments: Vec<SegmentResponse>,
}

/// Truncated blob representation used by section rows and identity keys.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct BlobValue {
    pub(crate) text: String,
    pub(crate) full_len: u64,
    pub(crate) truncated: bool,
}

/// Every scalar shape a registry column can expose on the JSON wire.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum ApiValue {
    Null,
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
    Text(String),
    Blob(BlobValue),
    ListI32(Vec<i32>),
}

impl PartialSchema for ApiValue {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::Ref;
        use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Type};

        AnyOfBuilder::new()
            .item(ObjectBuilder::new().schema_type(Type::Null))
            .item(i64::schema())
            .item(u64::schema())
            .item(f64::schema())
            .item(bool::schema())
            .item(String::schema())
            .item(Ref::from_schema_name(BlobValue::name()))
            .item(Vec::<i32>::schema())
            .description(Some(
                "Every scalar shape a registry column can expose on the JSON wire.",
            ))
            .into()
    }
}

impl ToSchema for ApiValue {
    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        schemas.push((BlobValue::name().into_owned(), BlobValue::schema()));
        BlobValue::schemas(schemas);
    }
}

impl From<&CellValue> for ApiValue {
    fn from(value: &CellValue) -> Self {
        match value {
            CellValue::Null => Self::Null,
            CellValue::I64(value) | CellValue::Ts(value) => Self::I64(*value),
            CellValue::U64(value) => Self::U64(*value),
            CellValue::F64(value) => Self::F64(*value),
            CellValue::Bool(value) => Self::Bool(*value),
            CellValue::Str(value) => Self::Text(value.clone()),
            CellValue::Blob {
                text,
                full_len,
                truncated,
            } => Self::Blob(BlobValue {
                text: text.clone(),
                full_len: *full_len,
                truncated: *truncated,
            }),
            CellValue::ListI32(value) => Self::ListI32(value.clone()),
        }
    }
}

/// One section row keyed by registry column name.
#[derive(Debug, Serialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = BTreeMap<String, ApiValue>)]
pub(crate) struct ApiRow(BTreeMap<String, ApiValue>);

impl From<&OutRow> for ApiRow {
    fn from(row: &OutRow) -> Self {
        Self(
            row.iter()
                .map(|(name, value)| (name.clone(), ApiValue::from(value)))
                .collect(),
        )
    }
}

/// Half-open range where section data is unavailable.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct GapResponse {
    pub(crate) from: i64,
    pub(crate) to: i64,
}

/// One bounded page of decoded section rows.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SectionPageResponse {
    pub(crate) section: String,
    pub(crate) rows: Vec<ApiRow>,
    pub(crate) gaps: Vec<GapResponse>,
    #[schema(required = true)]
    pub(crate) next_cursor: Option<String>,
}

impl From<&SectionPage> for SectionPageResponse {
    fn from(page: &SectionPage) -> Self {
        Self {
            section: page.section.clone(),
            rows: page.rows.iter().map(ApiRow::from).collect(),
            gaps: page
                .gaps
                .iter()
                .map(|gap| GapResponse {
                    from: gap.from,
                    to: gap.to,
                })
                .collect(),
            next_cursor: page
                .next_cursor
                .as_ref()
                .map(kronika_reader::Cursor::encode),
        }
    }
}

/// Section pages keyed by requested logical section name.
#[derive(Debug, Serialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = BTreeMap<String, SectionPageResponse>)]
pub(crate) struct SectionsBatchResponse(pub(crate) BTreeMap<String, SectionPageResponse>);

/// Stable reason for a missing diff point.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoDataReason {
    Reset,
    Gap,
    FirstPoint,
    Anomaly,
    NotCollected,
}

impl From<Reason> for NoDataReason {
    fn from(reason: Reason) -> Self {
        match reason {
            Reason::Reset => Self::Reset,
            Reason::Gap => Self::Gap,
            Reason::FirstPoint => Self::FirstPoint,
            Reason::Anomaly => Self::Anomaly,
            Reason::NotCollected => Self::NotCollected,
        }
    }
}

/// Finite numeric delta; non-finite floating-point values become `null`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum DiffScalar {
    Null,
    Integer(i64),
    Number(f64),
}

impl PartialSchema for DiffScalar {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Type};

        AnyOfBuilder::new()
            .item(ObjectBuilder::new().schema_type(Type::Null))
            .item(i64::schema())
            .item(f64::schema())
            .description(Some(
                "Finite numeric delta; non-finite floating-point values become null.",
            ))
            .into()
    }
}

impl ToSchema for DiffScalar {}

impl From<Scalar> for DiffScalar {
    #[allow(
        clippy::cast_precision_loss,
        reason = "the existing wire contract falls back to f64 above i64::MAX"
    )]
    fn from(value: Scalar) -> Self {
        match value {
            Scalar::Int(value) => {
                i64::try_from(value).map_or_else(|_| Self::finite(value as f64), Self::Integer)
            }
            Scalar::Float(value) => Self::finite(value),
        }
    }
}

impl DiffScalar {
    const fn finite(value: f64) -> Self {
        if value.is_finite() {
            Self::Number(value)
        } else {
            Self::Null
        }
    }
}

/// A measured delta and rate between two observations.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DiffValuePoint {
    pub(crate) ts: i64,
    pub(crate) delta: DiffScalar,
    #[schema(required = true)]
    pub(crate) rate: Option<f64>,
    pub(crate) dt_micros: i64,
}

/// An observation where no valid delta can be derived.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DiffNoDataPoint {
    pub(crate) ts: i64,
    pub(crate) nodata: NoDataReason,
}

/// One timestamped diff result.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub(crate) enum DiffPointResponse {
    Value(DiffValuePoint),
    NoData(DiffNoDataPoint),
}

impl From<&DiffAt> for DiffPointResponse {
    fn from(at: &DiffAt) -> Self {
        match at.point {
            DiffPoint::Value {
                delta,
                rate,
                dt_micros,
            } => Self::Value(DiffValuePoint {
                ts: at.ts,
                delta: DiffScalar::from(delta),
                rate: rate.is_finite().then_some(rate),
                dt_micros,
            }),
            DiffPoint::NoData { reason } => Self::NoData(DiffNoDataPoint {
                ts: at.ts,
                nodata: NoDataReason::from(reason),
            }),
        }
    }
}

/// Per-identity series with dynamically named cumulative columns.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct DiffSeriesResponse {
    pub(crate) key: BTreeMap<String, ApiValue>,
    pub(crate) columns: BTreeMap<String, Vec<DiffPointResponse>>,
}

impl DiffSeriesResponse {
    pub(crate) fn collect(identity: &[&str], series: &[SeriesDiff]) -> Vec<Self> {
        series
            .iter()
            .map(|series| Self {
                key: identity
                    .iter()
                    .zip(&series.key)
                    .map(|(name, value)| ((*name).to_owned(), ApiValue::from(value)))
                    .collect(),
                columns: series
                    .columns
                    .iter()
                    .map(|column| {
                        (
                            column.name.clone(),
                            column.points.iter().map(DiffPointResponse::from).collect(),
                        )
                    })
                    .collect(),
            })
            .collect()
    }
}

/// Diff of one logical section over a requested range.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct SectionDiffResponse {
    pub(crate) section: String,
    pub(crate) identity: Vec<String>,
    pub(crate) series: Vec<DiffSeriesResponse>,
}

/// Section diffs keyed by requested logical section name.
#[derive(Debug, Serialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = BTreeMap<String, SectionDiffResponse>)]
pub(crate) struct SectionsBatchDiffResponse(pub(crate) BTreeMap<String, SectionDiffResponse>);

/// Contribution of one health domain to a timeline point.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthDomainResponse {
    pub(crate) domain: String,
    pub(crate) penalty: f64,
    pub(crate) driving_factor_ids: Vec<u32>,
}

/// A fact that imposes a hard health-state floor.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthFloorEvidenceResponse {
    pub(crate) class: String,
    pub(crate) supporting_fact_id: String,
}

/// Population evidence attached to one factor coverage record.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthSourcePopulationResponse {
    pub(crate) collected: u64,
    pub(crate) total: u64,
    pub(crate) total_quality: String,
}

/// Coverage and quality evidence for one health factor.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthFactorCoverageResponse {
    pub(crate) factor_id: u32,
    pub(crate) applicability: String,
    pub(crate) state: String,
    pub(crate) interval: CoverageSpanDto,
    #[schema(required = true)]
    pub(crate) expected_period_us: Option<u64>,
    pub(crate) period_quality: String,
    #[schema(required = true)]
    pub(crate) cadence_epoch_id: Option<String>,
    pub(crate) crosses_cadence_boundary: bool,
    pub(crate) present_samples: u64,
    pub(crate) covered_duration_us: u64,
    #[schema(required = true)]
    pub(crate) source_population: Option<HealthSourcePopulationResponse>,
    pub(crate) loss_reasons: Vec<String>,
    #[schema(required = true)]
    pub(crate) lost_count_lower_bound: Option<u64>,
    pub(crate) retained_exactness: String,
    pub(crate) source_completeness: String,
    pub(crate) physical_count_semantics: String,
    pub(crate) boundary_quality: String,
}

/// One downsampled point of the timeline health line.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthPointResponse {
    pub(crate) interval: CoverageSpanDto,
    #[schema(required = true)]
    pub(crate) continuous_score: Option<f64>,
    #[schema(required = true)]
    pub(crate) overall_score: Option<f64>,
    pub(crate) overall_state: String,
    pub(crate) health_policy_version: u32,
    pub(crate) factor_set_id: String,
    pub(crate) domains: Vec<HealthDomainResponse>,
    pub(crate) floor_evidence: Vec<HealthFloorEvidenceResponse>,
    pub(crate) coverage: Vec<HealthFactorCoverageResponse>,
}

/// Worst and latest health points calculated for an overview window.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthSummaryResponse {
    #[schema(required = true)]
    pub(crate) worst_point: Option<HealthPointResponse>,
    #[schema(required = true)]
    pub(crate) latest_point: Option<HealthPointResponse>,
}

/// Timeline health response with its calculation and evidence metadata.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the timeline policy serializer"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct HealthResponse {
    pub(crate) meta: TimelineMetaDto,
    pub(crate) health_policy_version: u32,
    pub(crate) factor_set_ids: Vec<String>,
    pub(crate) points: Vec<HealthPointResponse>,
    pub(crate) coverage: Vec<HealthFactorCoverageResponse>,
}

/// Machine-readable reason carried inside a successful partial response.
#[allow(
    dead_code,
    reason = "schema projection shared by JSON builders for successful analytic responses"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct SuccessReasonResponse {
    pub(crate) kind: String,
    #[schema(value_type = Object)]
    pub(crate) params: serde_json::Value,
}

/// Counts explaining why a generic anomaly series was not evaluated.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalyNotEvaluatedResponse {
    pub(crate) ref_too_small: u64,
    pub(crate) cur_too_small: u64,
    pub(crate) all_no_data: u64,
    pub(crate) non_finite: u64,
    pub(crate) discontinuity: u64,
}

/// Per-section generic anomaly scan accounting.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalySectionResponse {
    pub(crate) series_total: u64,
    pub(crate) evaluated: u64,
    pub(crate) not_evaluated: AnomalyNotEvaluatedResponse,
    pub(crate) nodata_points: u64,
    pub(crate) episodes_truncated: u64,
}

/// Request-wide anomaly scan coverage.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalyCoverageResponse {
    pub(crate) sections_requested: usize,
    pub(crate) sections_scanned: u64,
    pub(crate) sections_skipped: usize,
    pub(crate) plan_sections_analyzed: u64,
    pub(crate) plan_positions_evaluated: u64,
}

/// Bounded-result counters for dropped anomaly evidence.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalyTruncationResponse {
    pub(crate) section_episodes_dropped: u64,
    pub(crate) global_episodes_dropped: u64,
    pub(crate) episodes_dropped_total: u64,
    pub(crate) section_plan_signals_dropped: u64,
    pub(crate) global_plan_signals_dropped: u64,
    pub(crate) plan_signals_dropped_total: u64,
}

/// Detector parameters repeated with each generic anomaly episode.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalyEpisodeParametersResponse {
    pub(crate) reference_model: String,
    pub(crate) retrospective: bool,
    pub(crate) threshold: f64,
    #[schema(required = true)]
    pub(crate) eps_abs: Option<f64>,
    pub(crate) eps_rel: f64,
    pub(crate) min_reference_points: usize,
    pub(crate) min_current_points: usize,
}

/// Values behind the peak anomaly verdict.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalyPeakResponse {
    #[schema(required = true)]
    pub(crate) m: Option<f64>,
    #[schema(required = true)]
    pub(crate) med_cur: Option<f64>,
    #[schema(required = true)]
    pub(crate) med_ref: Option<f64>,
    #[schema(required = true)]
    pub(crate) mad_ref: Option<f64>,
    #[schema(required = true)]
    pub(crate) sigma_used: Option<f64>,
    pub(crate) n_cur: usize,
    pub(crate) n_ref: usize,
}

/// One ranked generic anomaly episode.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalyEpisodeResponse {
    pub(crate) signal_id: String,
    pub(crate) section: String,
    pub(crate) series: BTreeMap<String, ApiValue>,
    pub(crate) column: String,
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) peak_ts: i64,
    pub(crate) direction: String,
    pub(crate) parameters: AnomalyEpisodeParametersResponse,
    pub(crate) peak: AnomalyPeakResponse,
}

/// A section omitted from a successful, explicitly partial anomaly response.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomalySkippedSectionResponse {
    pub(crate) section: String,
    pub(crate) reason: SuccessReasonResponse,
}

/// Complete response contract of the cross-section anomaly scan.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the anomaly engine"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct AnomaliesResponse {
    pub(crate) schema_version: u32,
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) window_us: i64,
    pub(crate) step_us: i64,
    pub(crate) threshold: f64,
    pub(crate) eps_rel: f64,
    pub(crate) limit: usize,
    pub(crate) status: String,
    pub(crate) complete: bool,
    pub(crate) coverage: AnomalyCoverageResponse,
    pub(crate) truncation: AnomalyTruncationResponse,
    pub(crate) episodes: Vec<AnomalyEpisodeResponse>,
    #[schema(value_type = Vec<Object>)]
    pub(crate) plan_signals: Vec<serde_json::Value>,
    pub(crate) sections: BTreeMap<String, AnomalySectionResponse>,
    #[schema(value_type = Object)]
    pub(crate) plan_analysis: BTreeMap<String, serde_json::Value>,
    pub(crate) skipped: Vec<AnomalySkippedSectionResponse>,
}

/// One half-open interval in an incident response.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentIntervalResponse {
    pub(crate) from: i64,
    pub(crate) to: i64,
}

/// One anomaly episode referenced by a clustered incident.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentMemberResponse {
    pub(crate) logical_section: String,
    pub(crate) column: String,
    pub(crate) identity: Vec<ApiValue>,
    pub(crate) from: i64,
    pub(crate) to: i64,
}

/// Scope to which an incident finding applies.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentFindingScopeResponse {
    pub(crate) logical_section: String,
    pub(crate) column: String,
    pub(crate) identity: Vec<ApiValue>,
}

/// Symbolic or structured evidence attached to an incident finding.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug)]
pub(crate) enum IncidentEvidenceResponse {}

impl PartialSchema for IncidentEvidenceResponse {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder};

        AnyOfBuilder::new()
            .item(String::schema())
            .item(ObjectBuilder::new())
            .description(Some(
                "Symbolic or structured evidence attached to an incident finding.",
            ))
            .into()
    }
}

impl ToSchema for IncidentEvidenceResponse {}

/// One diagnostic finding and its bounded supporting evidence.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentFindingResponse {
    pub(crate) lens_id: String,
    pub(crate) role: String,
    pub(crate) confidence: String,
    pub(crate) scope: IncidentFindingScopeResponse,
    pub(crate) evidence: Vec<IncidentEvidenceResponse>,
}

/// One cluster of related anomaly episodes.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentResponse {
    pub(crate) interval: IncidentIntervalResponse,
    pub(crate) incident_key: String,
    pub(crate) members: Vec<IncidentMemberResponse>,
    pub(crate) findings: Vec<IncidentFindingResponse>,
    pub(crate) evaluation_complete: bool,
    pub(crate) finding_evaluation_status: String,
}

/// Complete response contract of incident clustering and diagnosis.
#[allow(
    dead_code,
    reason = "schema projection for JSON assembled by the incident response builder"
)]
#[derive(Debug, ToSchema)]
pub(crate) struct IncidentsResponse {
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) complete: bool,
    pub(crate) clustering_complete: bool,
    pub(crate) analysis_status: String,
    pub(crate) incidents: Vec<IncidentResponse>,
    #[schema(value_type = Object)]
    pub(crate) coverage_by_section: BTreeMap<String, serde_json::Value>,
    #[schema(required = true)]
    pub(crate) data_age_seconds: Option<u64>,
    #[schema(value_type = Object)]
    pub(crate) catalog: serde_json::Value,
    #[schema(value_type = Object)]
    pub(crate) log: serde_json::Value,
    #[schema(value_type = Object)]
    pub(crate) data_quality: serde_json::Value,
    #[schema(value_type = Object)]
    pub(crate) skipped: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use kronika_reader::Value as CellValue;

    use super::ApiValue;
    use crate::serialize::value_to_json;

    #[test]
    fn api_value_preserves_the_existing_json_wire_shapes() {
        let values = [
            CellValue::Null,
            CellValue::I64(-5),
            CellValue::U64(5),
            CellValue::F64(1.5),
            CellValue::Bool(true),
            CellValue::Ts(1_234),
            CellValue::Str("x".to_owned()),
            CellValue::Blob {
                text: "ab".to_owned(),
                full_len: 10,
                truncated: true,
            },
            CellValue::ListI32(vec![1, 2, 3]),
        ];

        for value in values {
            assert_eq!(
                serde_json::to_value(ApiValue::from(&value)).expect("serialize API value"),
                value_to_json(&value)
            );
        }
    }
}
