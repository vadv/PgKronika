#![allow(
    dead_code,
    reason = "frame response DTOs are consumed by the projection and HTTP tasks"
)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::{Boundary, Classified, Comparison, Evidence, Level, NotClassifiedReason};
use serde::Serialize;
use utoipa::{PartialSchema, ToSchema};

use super::FrameRequest;
use super::projection::{ProjectedFrame, selected_columns};
use crate::ui::catalog::{Availability, ColumnSpec, ProjectionCatalog, ValueType};

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameResponse {
    pub view: &'static str,
    pub snapshot_ts_us: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_prev_ts_us: Option<String>,
    pub neighbors: FrameNeighborsDto,
    pub columns: Vec<FrameColumnDto>,
    pub rows: Vec<FrameRowDto>,
    pub page: FramePageDto,
    pub quality: FrameQualityDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameNeighborsDto {
    pub prev_us: Option<String>,
    pub next_us: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameColumnDto {
    pub code: &'static str,
    #[serde(rename = "type")]
    pub value_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold_metric: Option<&'static str>,
    /// Whether the column is materialized only for sort or field filtering.
    pub hidden: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameRowDto {
    pub entity: String,
    pub label: String,
    pub cells: Vec<FrameValue>,
    pub cell_statuses: Vec<CellStatusDto>,
    pub classifications: Vec<CellClassificationDto>,
    pub categorical_classifications: Vec<CategoricalClassificationDto>,
    pub spark: SparkDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub(crate) struct CellStatusDto {
    status: &'static str,
    #[schema(required = true)]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub(crate) struct CategoricalClassificationDto {
    column: &'static str,
    status: &'static str,
    #[schema(required = true)]
    code: Option<String>,
    #[schema(required = true)]
    level: Option<&'static str>,
    #[schema(required = true)]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum FrameValue {
    Null,
    Number(f64),
    Boolean(bool),
    String(String),
}

impl PartialSchema for FrameValue {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Type};

        AnyOfBuilder::new()
            .item(ObjectBuilder::new().schema_type(Type::Null))
            .item(f64::schema())
            .item(bool::schema())
            .item(String::schema())
            .description(Some(
                "A finite frame scalar; wide integers and timestamps use decimal strings.",
            ))
            .into()
    }
}

impl ToSchema for FrameValue {}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FramePageDto {
    pub returned: usize,
    pub matched: usize,
    pub next: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameQualityDto {
    pub status: &'static str,
    pub snapshots: usize,
    pub gaps: Vec<FrameGapDto>,
    pub gated: Vec<String>,
    pub unavailable_revision: Vec<String>,
    pub resource_limited: Vec<String>,
    pub active_tail: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameGapDto {
    pub from_us: String,
    pub to_us: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct CellClassificationDto {
    pub column: &'static str,
    pub metric: &'static str,
    pub result: ClassificationResultDto,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub(crate) enum ClassificationResultDto {
    Classified(ClassifiedResultDto),
    NotClassified(NotClassifiedResultDto),
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ClassifiedResultDto {
    status: &'static str,
    level: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    boundary: Option<BoundaryDto>,
    evidence: EvidenceDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct NotClassifiedResultDto {
    status: &'static str,
    reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
pub(crate) struct BoundaryDto {
    operator: &'static str,
    value: f64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EvidenceDto {
    Scalar {
        observed: f64,
    },
    Fraction {
        numerator: f64,
        denominator: f64,
        value: f64,
    },
    Limit {
        observed: f64,
        limit: f64,
    },
    RatioWithFloor {
        ratio: f64,
        count: f64,
        floor: BoundaryDto,
    },
    Age {
        epoch_seconds: f64,
        now_seconds: f64,
        age_seconds: f64,
    },
    FreeCapacity {
        available_bytes: f64,
        total_bytes: f64,
        available_fraction: f64,
        absolute_ceiling_bytes: BoundaryDto,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub(crate) struct SparkDto {
    pub values: Vec<Option<f64>>,
    pub complete: bool,
}

impl FrameResponse {
    pub(crate) fn from_projected(
        request: &FrameRequest,
        catalog: &ProjectionCatalog,
        frame: &ProjectedFrame,
    ) -> Result<Self, super::projection::ProjectionError> {
        let columns = selected_columns(request, catalog)?
            .into_iter()
            .map(|column| FrameColumnDto {
                code: column.code,
                value_type: value_type_spelling(column.value_type),
                unit: column.unit,
                threshold_metric: column.threshold_metric,
                hidden: !request.columns.contains(&column.code),
            })
            .collect::<Vec<_>>();
        let selected = selected_columns(request, catalog)?;
        let rows = frame
            .rows
            .iter()
            .map(|row| FrameRowDto {
                entity: URL_SAFE_NO_PAD.encode(&row.entity),
                label: row.label.clone(),
                cells: row.cells.clone(),
                cell_statuses: row
                    .cells
                    .iter()
                    .zip(&selected)
                    .map(|(value, column)| cell_status(value, column, frame))
                    .collect(),
                classifications: row
                    .classifications
                    .iter()
                    .map(|classification| CellClassificationDto {
                        column: classification.column,
                        metric: classification.metric_id.as_str(),
                        result: classification.result.into(),
                    })
                    .collect(),
                categorical_classifications: row
                    .cells
                    .iter()
                    .zip(&selected)
                    .filter_map(|(value, column)| {
                        categorical_classification(column.code, value, column)
                    })
                    .collect(),
                spark: row.spark.clone(),
            })
            .collect();
        let mut gated = frame.quality.gated.clone();
        let mut unavailable_revision = frame.quality.unavailable_revision.clone();
        let mut resource_limited = frame.quality.resource_limited.clone();
        for values in [&mut gated, &mut unavailable_revision, &mut resource_limited] {
            values.sort();
            values.dedup();
        }
        let complete = frame.quality.gaps.is_empty()
            && gated.is_empty()
            && unavailable_revision.is_empty()
            && resource_limited.is_empty()
            && frame.rows.iter().all(|row| row.spark.complete);
        Ok(Self {
            view: request.view.name,
            snapshot_ts_us: frame.snapshot_ts_us.to_string(),
            rate_prev_ts_us: frame.predecessor_ts_us.map(|value| value.to_string()),
            neighbors: FrameNeighborsDto {
                prev_us: frame.neighbors.previous.map(|value| value.to_string()),
                next_us: frame.neighbors.next.map(|value| value.to_string()),
            },
            columns,
            rows,
            page: FramePageDto {
                returned: frame.rows.len(),
                matched: frame.matched,
                next: frame.next.clone(),
            },
            quality: FrameQualityDto {
                status: if complete { "complete" } else { "partial" },
                snapshots: frame.quality.snapshots,
                gaps: frame
                    .quality
                    .gaps
                    .iter()
                    .map(|gap| FrameGapDto {
                        from_us: gap.from.to_string(),
                        to_us: gap.to.to_string(),
                    })
                    .collect(),
                gated,
                unavailable_revision,
                resource_limited,
                active_tail: frame.quality.active_tail,
            },
        })
    }
}

fn categorical_classification(
    column: &'static str,
    value: &FrameValue,
    spec: &ColumnSpec,
) -> Option<CategoricalClassificationDto> {
    if !matches!(
        column,
        "state"
            | "wait_event"
            | "replication_state"
            | "granted"
            | "lock_mode"
            | "lock_type"
            | "severity_code"
            | "category_code"
    ) {
        return None;
    }
    let code = match value {
        FrameValue::String(value) => value.clone(),
        FrameValue::Boolean(true) => "granted".to_owned(),
        FrameValue::Boolean(false) => "waiting".to_owned(),
        FrameValue::Null | FrameValue::Number(_) => {
            return Some(CategoricalClassificationDto {
                column,
                status: "not_classified",
                code: None,
                level: None,
                reason: spec.unavailable_reason.or(Some("not_observed")),
            });
        }
    };
    Some(CategoricalClassificationDto {
        column,
        status: "classified",
        level: Some(categorical_level(column, &code)),
        code: Some(code),
        reason: None,
    })
}

fn categorical_level(column: &str, code: &str) -> &'static str {
    match column {
        "severity_code" => match code {
            "panic" | "fatal" => "critical",
            "error" | "warning" => "warning",
            _ => "info",
        },
        "category_code" => {
            if code.contains("error")
                || code.contains("deadlock")
                || code.contains("lock")
                || code.contains("gap")
            {
                "warning"
            } else {
                "info"
            }
        }
        "state" => match code {
            "idle in transaction" | "idle in transaction (aborted)" => "warning",
            "disabled" => "critical",
            _ => "info",
        },
        "wait_event" => {
            if code.starts_with("Lock:") || code.starts_with("IO:") {
                "warning"
            } else {
                "info"
            }
        }
        "replication_state" => match code {
            "stopped" => "critical",
            "catchup" | "startup" | "backup" => "warning",
            _ => "info",
        },
        "granted" if code == "waiting" => "warning",
        _ => "info",
    }
}

fn cell_status(value: &FrameValue, column: &ColumnSpec, frame: &ProjectedFrame) -> CellStatusDto {
    if !matches!(value, FrameValue::Null) {
        return CellStatusDto {
            status: "available",
            reason: None,
        };
    }
    let reason = if column.availability != Availability::Available {
        column.unavailable_reason
    } else if !frame.quality.gaps.is_empty() {
        Some("producer_gap")
    } else if frame.predecessor_ts_us.is_none() && column.formula.is_some() {
        Some("missing_predecessor")
    } else {
        Some("not_observed")
    };
    CellStatusDto {
        status: "unavailable",
        reason,
    }
}

const fn value_type_spelling(value_type: ValueType) -> &'static str {
    match value_type {
        ValueType::I64 => "i64",
        ValueType::U64 => "u64",
        ValueType::F64 => "f64",
        ValueType::Bool => "bool",
        ValueType::Text => "text",
        ValueType::Timestamp => "timestamp",
    }
}

impl From<Classified> for ClassificationResultDto {
    fn from(classified: Classified) -> Self {
        match classified {
            Classified::Verdict(verdict) => Self::Classified(ClassifiedResultDto {
                status: "classified",
                level: level_spelling(verdict.level),
                boundary: verdict.boundary.map(BoundaryDto::from),
                evidence: EvidenceDto::from(verdict.evidence),
            }),
            Classified::NotClassified(reason) => Self::NotClassified(NotClassifiedResultDto {
                status: "not_classified",
                reason: reason_spelling(reason),
            }),
        }
    }
}

impl From<Boundary> for BoundaryDto {
    fn from(boundary: Boundary) -> Self {
        Self {
            operator: comparison_spelling(boundary.operator),
            value: boundary.value,
        }
    }
}

impl From<Evidence> for EvidenceDto {
    fn from(evidence: Evidence) -> Self {
        match evidence {
            Evidence::Scalar { observed } => Self::Scalar { observed },
            Evidence::Fraction {
                numerator,
                denominator,
                value,
            } => Self::Fraction {
                numerator,
                denominator,
                value,
            },
            Evidence::Limit { observed, limit } => Self::Limit { observed, limit },
            Evidence::RatioWithFloor {
                ratio,
                count,
                floor,
            } => Self::RatioWithFloor {
                ratio,
                count,
                floor: floor.into(),
            },
            Evidence::Age {
                epoch_seconds,
                now_seconds,
                age_seconds,
            } => Self::Age {
                epoch_seconds,
                now_seconds,
                age_seconds,
            },
            Evidence::FreeCapacity {
                available_bytes,
                total_bytes,
                available_fraction,
                absolute_ceiling_bytes,
            } => Self::FreeCapacity {
                available_bytes,
                total_bytes,
                available_fraction,
                absolute_ceiling_bytes: absolute_ceiling_bytes.into(),
            },
        }
    }
}

const fn level_spelling(level: Level) -> &'static str {
    match level {
        Level::Inactive => "inactive",
        Level::Ok => "ok",
        Level::Warning => "warning",
        Level::Critical => "critical",
    }
}

const fn comparison_spelling(comparison: Comparison) -> &'static str {
    match comparison {
        Comparison::Above => "above",
        Comparison::AtLeast => "at_least",
        Comparison::Below => "below",
        Comparison::AtMost => "at_most",
    }
}

const fn reason_spelling(reason: NotClassifiedReason) -> &'static str {
    match reason {
        NotClassifiedReason::Missing => "missing",
        NotClassifiedReason::NonFinite => "non_finite",
        NotClassifiedReason::OutOfDomain => "out_of_domain",
        NotClassifiedReason::InvalidDenominator => "invalid_denominator",
        NotClassifiedReason::NotApplicable => "not_applicable",
        NotClassifiedReason::InputShapeMismatch => "input_shape_mismatch",
    }
}
