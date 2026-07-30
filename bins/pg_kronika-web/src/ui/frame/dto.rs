#![allow(
    dead_code,
    reason = "frame response DTOs are consumed by the projection and HTTP tasks"
)]

use kronika_analytics::{Boundary, Classified, Comparison, Evidence, Level, NotClassifiedReason};
use serde::Serialize;
use utoipa::{PartialSchema, ToSchema};

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
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct FrameRowDto {
    pub entity: String,
    pub label: String,
    pub cells: Vec<FrameValue>,
    pub classifications: Vec<CellClassificationDto>,
    pub spark: SparkDto,
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
