//! Revisioned entity-detail and bounded history admission.

mod cursor;

use std::collections::BTreeSet;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::web_projection::{WebView, web_view_by_name};
use kronika_reader::{Gap, LocalDirSnapshot, WebIndexReadError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use self::cursor::EntityHistoryCursor;
use super::catalog::{Availability, ColumnSpec, ProjectionCatalog};
use super::frame::dto::FrameValue;
use super::frame::projection::{FrameError, FrameLimits, FrameQuality, project_entity_at};
use super::snapshot::read_summary_tolerant;
use crate::api_error::{
    ApiError, ExpectedValue, InvalidParameterLocation, LimitResource, QueryConstraint,
    QueryParameter, count_u64,
};
use crate::params::{QueryParams, parse_i64};

const MAX_ENTITY_TOKEN_BYTES: usize = 256;
const MAX_HISTORY_COLUMNS: usize = 32;
const MAX_HISTORY_SPAN_US: i64 = 6 * 60 * 60 * 1_000_000;
const DEFAULT_HISTORY_LIMIT: usize = 500;
const MAX_HISTORY_LIMIT: usize = 2_000;
pub(crate) const MAX_HISTORY_SEGMENTS: usize = 32;
const ENTITY_PARAMETERS: &[QueryParameter] = &[
    QueryParameter::At,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Columns,
    QueryParameter::Include,
    QueryParameter::Limit,
    QueryParameter::Cursor,
];

#[derive(Debug)]
pub(crate) struct EntityRequest {
    pub(crate) view: &'static WebView,
    pub(crate) entity: Vec<u8>,
    pub(crate) encoded_entity: String,
    pub(crate) mode: EntityMode,
}

#[derive(Debug)]
pub(crate) enum EntityMode {
    Point {
        at_us: i64,
        include_related: bool,
    },
    History {
        from_us: i64,
        to_us: i64,
        columns: Vec<&'static str>,
        limit: usize,
        cursor: Option<EntityHistoryCursor>,
        fingerprint: [u8; 32],
    },
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub(crate) enum EntityResponse {
    Point(EntityPointResponse),
    History(EntityHistoryResponse),
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct EntityPointResponse {
    mode: &'static str,
    view: &'static str,
    entity: String,
    /// Human row label from the same projection as frame rows — the entity
    /// token above is routing material, never display text.
    label: String,
    snapshot_ts_us: String,
    fields: Vec<EntityFieldDto>,
    related: Vec<RelatedEntityDto>,
    quality: EntityQualityDto,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct EntityHistoryResponse {
    mode: &'static str,
    view: &'static str,
    entity: String,
    /// Label of the first observed snapshot; empty when the entity never
    /// appears in the window.
    label: String,
    columns: Vec<&'static str>,
    snapshots: Vec<EntitySnapshotDto>,
    page: EntityPageDto,
    quality: EntityQualityDto,
}

#[derive(Debug, Serialize, ToSchema)]
struct EntityFieldDto {
    code: &'static str,
    value: FrameValue,
    status: &'static str,
    #[schema(required = true)]
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct EntitySnapshotDto {
    ts_us: String,
    values: Vec<FrameValue>,
    statuses: Vec<&'static str>,
    reasons: Vec<Option<&'static str>>,
}

#[derive(Debug, Serialize, ToSchema)]
struct RelatedEntityDto {
    relation: &'static str,
    view: &'static str,
    entity: String,
    provenance: RelationProvenanceDto,
}

#[derive(Debug, Serialize, ToSchema)]
struct RelationProvenanceDto {
    kind: &'static str,
    fields: Vec<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct EntityPageDto {
    #[schema(required = true)]
    next: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct EntityQualityDto {
    status: &'static str,
    gaps: Vec<EntityGapDto>,
    gated: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
struct EntityGapDto {
    from_us: String,
    to_us: String,
}

#[derive(Debug)]
pub(crate) enum EntityError {
    Frame(FrameError),
    WebIndex(WebIndexReadError),
    Cursor,
    Gone,
    SelectedSegments { observed: usize },
}

impl fmt::Display for EntityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "entity projection failed: {error}"),
            Self::WebIndex(error) => write!(f, "entity summary read failed: {error}"),
            Self::Cursor => f.write_str("entity history cursor encoding failed"),
            Self::Gone => f.write_str("entity is absent from the selected snapshot"),
            Self::SelectedSegments { observed } => {
                write!(f, "entity history selected {observed} segments")
            }
        }
    }
}

impl std::error::Error for EntityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::WebIndex(error) => Some(error),
            Self::Frame(_) | Self::Cursor | Self::Gone | Self::SelectedSegments { .. } => None,
        }
    }
}

impl From<FrameError> for EntityError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<WebIndexReadError> for EntityError {
    fn from(error: WebIndexReadError) -> Self {
        Self::WebIndex(error)
    }
}

impl EntityRequest {
    #[allow(
        clippy::too_many_lines,
        reason = "all entity token and query-shape admission runs before any storage access"
    )]
    pub(crate) fn parse(
        view_name: &str,
        encoded_entity: &str,
        raw_query: Option<&str>,
        catalog: &ProjectionCatalog,
    ) -> Result<Self, ApiError> {
        let view = web_view_by_name(view_name).ok_or_else(|| {
            ApiError::invalid_query_parameter(QueryParameter::View, ExpectedValue::ProjectionCode)
        })?;
        let view_spec = catalog
            .views()
            .iter()
            .find(|candidate| candidate.code == view_name)
            .ok_or_else(|| {
                ApiError::invalid_query_parameter(
                    QueryParameter::View,
                    ExpectedValue::ProjectionCode,
                )
            })?;
        let entity = decode_entity(encoded_entity, view_spec.identity_revision)?;
        let params = QueryParams::parse(raw_query, ENTITY_PARAMETERS)?;
        let has_at = params.get(QueryParameter::At).is_some();
        let has_from = params.get(QueryParameter::From).is_some();
        let has_to = params.get(QueryParameter::To).is_some();
        let has_columns = params.get(QueryParameter::Columns).is_some();
        let has_include = params.get(QueryParameter::Include).is_some();
        let has_limit = params.get(QueryParameter::Limit).is_some();
        let has_cursor = params.get(QueryParameter::Cursor).is_some();
        let point_shape =
            has_at && !has_from && !has_to && !has_columns && !has_limit && !has_cursor;
        let history_shape = !has_at && !has_include && has_from && has_to && has_columns;
        if !point_shape && !history_shape {
            return Err(ApiError::invalid_query_constraint(
                QueryConstraint::PointOrHistory,
            ));
        }

        let mode = if point_shape {
            let include_related =
                params
                    .get(QueryParameter::Include)
                    .map_or(Ok(false), |include| {
                        if include == "related" {
                            Ok(true)
                        } else {
                            Err(ApiError::invalid_query_parameter(
                                QueryParameter::Include,
                                ExpectedValue::ProjectionCode,
                            ))
                        }
                    })?;
            EntityMode::Point {
                at_us: parse_i64(&params, QueryParameter::At)?,
                include_related,
            }
        } else {
            if !view_spec.capabilities.history {
                return Err(ApiError::invalid_query_constraint(
                    QueryConstraint::HistorySupported,
                ));
            }
            let from_us = parse_i64(&params, QueryParameter::From)?;
            let to_us = parse_i64(&params, QueryParameter::To)?;
            let span = to_us
                .checked_sub(from_us)
                .filter(|span| *span > 0)
                .ok_or_else(|| ApiError::invalid_query_constraint(QueryConstraint::FromBeforeTo))?;
            if span > MAX_HISTORY_SPAN_US {
                return Err(ApiError::query_shape_limit_exceeded(
                    LimitResource::QuerySpanUs,
                    MAX_HISTORY_SPAN_US.unsigned_abs(),
                    Some(span.unsigned_abs()),
                ));
            }
            let columns = parse_columns(
                params
                    .get(QueryParameter::Columns)
                    .expect("history shape requires columns"),
                view_spec,
            )?;
            let limit =
                params
                    .get(QueryParameter::Limit)
                    .map_or(Ok(DEFAULT_HISTORY_LIMIT), |raw| {
                        raw.parse::<usize>().map_err(|_error| {
                            ApiError::invalid_query_parameter(
                                QueryParameter::Limit,
                                ExpectedValue::PositiveInteger,
                            )
                        })
                    })?;
            if limit == 0 {
                return Err(ApiError::invalid_query_parameter(
                    QueryParameter::Limit,
                    ExpectedValue::PositiveInteger,
                ));
            }
            if limit > MAX_HISTORY_LIMIT {
                return Err(ApiError::query_shape_limit_exceeded(
                    LimitResource::Rows,
                    count_u64(MAX_HISTORY_LIMIT),
                    Some(count_u64(limit)),
                ));
            }
            let fingerprint = history_fingerprint(view, &entity, from_us, to_us, &columns);
            let cursor = params
                .get(QueryParameter::Cursor)
                .map(EntityHistoryCursor::decode)
                .transpose()
                .map_err(|_error| ApiError::invalid_cursor())?;
            if cursor.is_some_and(|cursor| {
                cursor.view_code() != view.code
                    || cursor.view_revision() != view.revision
                    || cursor.range_start_us() != from_us
                    || cursor.range_end_us() != to_us
                    || cursor.fingerprint() != fingerprint
            }) {
                return Err(ApiError::cursor_query_mismatch());
            }
            EntityMode::History {
                from_us,
                to_us,
                columns,
                limit,
                cursor,
                fingerprint,
            }
        };
        Ok(Self {
            view,
            entity,
            encoded_entity: encoded_entity.to_owned(),
            mode,
        })
    }
}

pub(crate) fn entity(
    snapshot: &LocalDirSnapshot,
    request: &EntityRequest,
    catalog: &ProjectionCatalog,
) -> Result<EntityResponse, EntityError> {
    match &request.mode {
        EntityMode::Point {
            at_us,
            include_related,
        } => entity_point(snapshot, request, catalog, *at_us, *include_related)
            .map(EntityResponse::Point),
        EntityMode::History {
            from_us,
            to_us,
            columns,
            limit,
            cursor,
            fingerprint,
        } => entity_history(
            snapshot,
            request,
            catalog,
            HistoryArgs {
                from_us: *from_us,
                to_us: *to_us,
                columns,
                limit: *limit,
                cursor: *cursor,
                fingerprint: *fingerprint,
            },
        )
        .map(EntityResponse::History),
    }
}

fn entity_point(
    snapshot: &LocalDirSnapshot,
    request: &EntityRequest,
    catalog: &ProjectionCatalog,
    at_us: i64,
    include_related: bool,
) -> Result<EntityPointResponse, EntityError> {
    let view = catalog_view(catalog, request.view.name)?;
    let columns = view.columns.iter().collect::<Vec<_>>();
    let projected = project_entity_at(
        snapshot,
        request.view,
        at_us,
        &request.entity,
        &columns,
        catalog,
        include_related,
        FrameLimits::default(),
    )?;
    let row = projected.row.as_ref().ok_or(EntityError::Gone)?;
    let fields = columns
        .iter()
        .zip(&row.values)
        .map(|(column, (_code, value))| {
            let (status, reason) = field_status(column, value, &projected.quality);
            EntityFieldDto {
                code: column.code,
                value: value.clone(),
                status,
                reason,
            }
        })
        .collect();
    let related = projected
        .relations
        .iter()
        .map(|relation| RelatedEntityDto {
            relation: relation.relation,
            view: relation.view,
            entity: URL_SAFE_NO_PAD.encode(&relation.entity),
            provenance: RelationProvenanceDto {
                kind: "field_equality",
                fields: relation.fields.clone(),
            },
        })
        .collect();
    Ok(EntityPointResponse {
        mode: "point",
        view: request.view.name,
        entity: request.encoded_entity.clone(),
        label: row.label.clone(),
        snapshot_ts_us: projected.snapshot_ts_us.to_string(),
        fields,
        related,
        quality: quality_dto(&projected.quality),
    })
}

#[derive(Clone, Copy)]
struct HistoryArgs<'a> {
    from_us: i64,
    to_us: i64,
    columns: &'a [&'static str],
    limit: usize,
    cursor: Option<EntityHistoryCursor>,
    fingerprint: [u8; 32],
}

#[allow(
    clippy::too_many_lines,
    reason = "history keeps bounded descriptor admission, slot enumeration and cursor tiling explicit"
)]
fn entity_history(
    snapshot: &LocalDirSnapshot,
    request: &EntityRequest,
    catalog: &ProjectionCatalog,
    args: HistoryArgs<'_>,
) -> Result<EntityHistoryResponse, EntityError> {
    let view = catalog_view(catalog, request.view.name)?;
    let columns = args
        .columns
        .iter()
        .map(|code| {
            view.columns
                .iter()
                .find(|column| column.code == *code)
                .expect("admission validated history columns")
        })
        .collect::<Vec<_>>();
    // The predecessor of the first in-window snapshot may live one segment
    // earlier; the expansion keeps it inside the 32-segment admission.
    let expanded_from = args
        .from_us
        .saturating_sub(request.view.max_rate_gap_us.unwrap_or(0));
    let descriptors = snapshot
        .sealed_descriptors()
        .filter(|descriptor| descriptor.max_ts >= expanded_from && descriptor.min_ts < args.to_us)
        .collect::<Vec<_>>();
    if descriptors.len() > MAX_HISTORY_SEGMENTS {
        return Err(EntityError::SelectedSegments {
            observed: descriptors.len(),
        });
    }
    // The grid is the view's own presence bitmap: a scheduled miss of the
    // view's collection interval never becomes a row, let alone a gap.
    let mut timestamps = BTreeSet::new();
    let mut combined_quality = FrameQuality::default();
    for descriptor in &descriptors {
        let Some(summary) = read_summary_tolerant(snapshot, descriptor)? else {
            continue;
        };
        let Some(view_summary) = summary
            .views()
            .iter()
            .find(|candidate| candidate.view_code() == request.view.code)
        else {
            continue;
        };
        timestamps.extend(
            summary
                .snapshot_times()
                .iter()
                .copied()
                .enumerate()
                .filter(|(index, timestamp)| {
                    *timestamp >= args.from_us
                        && *timestamp < args.to_us
                        && view_summary
                            .snapshot_presence()
                            .get(index / 8)
                            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
                })
                .map(|(_index, timestamp)| timestamp),
        );
    }
    // True producer gaps are coverage holes in the admitted sealed span.
    combined_quality.gaps.extend(coverage_gaps(
        &descriptors,
        args.from_us,
        args.to_us,
        request.view.max_rate_gap_us,
    ));
    if let Some(cursor) = args.cursor {
        timestamps.retain(|timestamp| *timestamp > cursor.last_ts_us());
    }
    let selected = timestamps
        .into_iter()
        .take(args.limit.saturating_add(1))
        .collect::<Vec<_>>();
    let has_more = selected.len() > args.limit;
    let mut label = String::new();
    let mut snapshots = Vec::with_capacity(selected.len().min(args.limit));
    for timestamp in selected.iter().take(args.limit).copied() {
        let projected = project_entity_at(
            snapshot,
            request.view,
            timestamp,
            &request.entity,
            &columns,
            catalog,
            false,
            FrameLimits::default(),
        )?;
        merge_quality(&mut combined_quality, &projected.quality);
        if projected.snapshot_ts_us != projected.requested_ts_us {
            // Defensive: the presence grid resolves exactly by construction.
            combined_quality.gaps.push(Gap {
                from: timestamp,
                to: timestamp,
            });
            snapshots.push(EntitySnapshotDto {
                ts_us: timestamp.to_string(),
                values: vec![FrameValue::Null; columns.len()],
                statuses: vec!["unavailable"; columns.len()],
                reasons: vec![Some("producer_gap"); columns.len()],
            });
            continue;
        }
        let Some(row) = projected.row else {
            // The view was collected at this snapshot; the entity itself is
            // absent (top-N eviction, exited process) — not a producer gap.
            snapshots.push(EntitySnapshotDto {
                ts_us: timestamp.to_string(),
                values: vec![FrameValue::Null; columns.len()],
                statuses: vec!["unavailable"; columns.len()],
                reasons: vec![Some("not_observed"); columns.len()],
            });
            continue;
        };
        if label.is_empty() {
            label.clone_from(&row.label);
        }
        let mut values = Vec::with_capacity(columns.len());
        let mut statuses = Vec::with_capacity(columns.len());
        let mut reasons = Vec::with_capacity(columns.len());
        for (column, (_code, value)) in columns.iter().zip(row.values) {
            let (status, reason) = field_status(column, &value, &projected.quality);
            values.push(value);
            statuses.push(status);
            reasons.push(reason);
        }
        snapshots.push(EntitySnapshotDto {
            ts_us: timestamp.to_string(),
            values,
            statuses,
            reasons,
        });
    }
    combined_quality.gaps.sort_by_key(|gap| (gap.from, gap.to));
    combined_quality.gaps.dedup();
    let next = if has_more {
        let encoded = snapshots
            .last()
            .and_then(|snapshot| snapshot.ts_us.parse().ok())
            .and_then(|last_ts_us| {
                EntityHistoryCursor::new(
                    request.view.code,
                    request.view.revision,
                    args.from_us,
                    args.to_us,
                    last_ts_us,
                    args.fingerprint,
                )
                .ok()
            })
            .and_then(|cursor| cursor.encode().ok())
            .ok_or(EntityError::Cursor)?;
        Some(encoded)
    } else {
        None
    };
    Ok(EntityHistoryResponse {
        mode: "history",
        view: request.view.name,
        entity: request.encoded_entity.clone(),
        label,
        columns: args.columns.to_vec(),
        snapshots,
        page: EntityPageDto { next },
        quality: quality_dto(&combined_quality),
    })
}

/// Producer gaps are coverage holes in the admitted sealed span, wider than
/// the view's rate tolerance; rollover seams and a still-open tail are not gaps.
fn coverage_gaps(
    descriptors: &[kronika_reader::SegmentDescriptor],
    from_us: i64,
    to_us: i64,
    max_rate_gap_us: Option<i64>,
) -> Vec<Gap> {
    let tolerance = max_rate_gap_us.unwrap_or(0);
    let mut spans = descriptors
        .iter()
        .map(|descriptor| (descriptor.min_ts, descriptor.max_ts))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    let mut gaps = Vec::new();
    let mut covered_to = from_us;
    for (minimum, maximum) in spans {
        let start = minimum.max(from_us);
        let end = maximum.saturating_add(1).min(to_us);
        if start > covered_to && start - covered_to > tolerance {
            gaps.push(Gap {
                from: covered_to,
                to: start,
            });
        }
        covered_to = covered_to.max(end);
    }
    if to_us - covered_to > tolerance {
        gaps.push(Gap {
            from: covered_to,
            to: to_us,
        });
    }
    gaps
}

fn catalog_view<'a>(
    catalog: &'a ProjectionCatalog,
    code: &str,
) -> Result<&'a super::catalog::ViewSpec, EntityError> {
    catalog
        .views()
        .iter()
        .find(|view| view.code == code)
        .ok_or(EntityError::Gone)
}

const fn field_status(
    column: &ColumnSpec,
    value: &FrameValue,
    quality: &FrameQuality,
) -> (&'static str, Option<&'static str>) {
    if !matches!(value, FrameValue::Null) {
        return ("available", None);
    }
    if !quality.gaps.is_empty() {
        return ("unavailable", Some("producer_gap"));
    }
    match column.availability {
        Availability::NotCollected => ("not_collected", Some("not_collected")),
        Availability::UnsupportedType => ("unavailable", Some("unsupported_type")),
        Availability::Gated => ("unavailable", Some("not_collected")),
        Availability::Available => ("unavailable", Some("unknown")),
    }
}

fn quality_dto(quality: &FrameQuality) -> EntityQualityDto {
    let mut gated = quality.gated.clone();
    gated.extend(quality.unavailable_revision.iter().cloned());
    gated.extend(quality.resource_limited.iter().cloned());
    gated.sort();
    gated.dedup();
    let complete = quality.gaps.is_empty() && gated.is_empty();
    EntityQualityDto {
        status: if complete { "complete" } else { "partial" },
        gaps: quality
            .gaps
            .iter()
            .map(|gap| EntityGapDto {
                from_us: gap.from.to_string(),
                to_us: gap.to.to_string(),
            })
            .collect(),
        gated,
    }
}

fn merge_quality(target: &mut FrameQuality, source: &FrameQuality) {
    target.snapshots = target.snapshots.saturating_add(source.snapshots);
    target.gaps.extend(source.gaps.iter().copied());
    target.gated.extend(source.gated.iter().cloned());
    target
        .unavailable_revision
        .extend(source.unavailable_revision.iter().cloned());
    target
        .resource_limited
        .extend(source.resource_limited.iter().cloned());
}

fn decode_entity(encoded: &str, identity_revision: u16) -> Result<Vec<u8>, ApiError> {
    if encoded.is_empty() || encoded.len() > MAX_ENTITY_TOKEN_BYTES * 2 {
        return Err(ApiError::invalid_query_parameter(
            InvalidParameterLocation::Entity,
            ExpectedValue::EntityToken,
        ));
    }
    let entity = URL_SAFE_NO_PAD.decode(encoded).map_err(|_error| {
        ApiError::invalid_query_parameter(
            InvalidParameterLocation::Entity,
            ExpectedValue::EntityToken,
        )
    })?;
    if entity.len() < 3 || entity.len() > MAX_ENTITY_TOKEN_BYTES {
        return Err(ApiError::invalid_query_parameter(
            InvalidParameterLocation::Entity,
            ExpectedValue::EntityToken,
        ));
    }
    let revision = entity
        .get(..2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_le_bytes);
    if revision != Some(identity_revision) {
        return Err(ApiError::invalid_query_parameter(
            InvalidParameterLocation::Entity,
            ExpectedValue::EntityToken,
        ));
    }
    Ok(entity)
}

fn parse_columns(
    raw: &str,
    view: &super::catalog::ViewSpec,
) -> Result<Vec<&'static str>, ApiError> {
    let mut seen = BTreeSet::new();
    let mut columns = Vec::new();
    for code in raw.split(',') {
        let Some(column) = view.columns.iter().find(|column| column.code == code) else {
            return Err(ApiError::invalid_query_parameter(
                QueryParameter::Columns,
                ExpectedValue::ProjectionColumnList,
            ));
        };
        if code.is_empty() || !seen.insert(column.code) {
            return Err(ApiError::invalid_query_parameter(
                QueryParameter::Columns,
                ExpectedValue::ProjectionColumnList,
            ));
        }
        columns.push(column.code);
    }
    if columns.is_empty() || columns.len() > MAX_HISTORY_COLUMNS {
        return Err(ApiError::query_shape_limit_exceeded(
            LimitResource::Cells,
            count_u64(MAX_HISTORY_COLUMNS),
            Some(count_u64(columns.len())),
        ));
    }
    Ok(columns)
}

fn history_fingerprint(
    view: &WebView,
    entity: &[u8],
    from_us: i64,
    to_us: i64,
    columns: &[&str],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pgkronika-entity-history-v1");
    hasher.update(view.code.to_le_bytes());
    hasher.update(view.revision.to_le_bytes());
    hasher.update(from_us.to_le_bytes());
    hasher.update(to_us.to_le_bytes());
    hasher.update((entity.len() as u64).to_le_bytes());
    hasher.update(entity);
    for column in columns {
        hasher.update((column.len() as u64).to_le_bytes());
        hasher.update(column.as_bytes());
    }
    hasher.finalize().into()
}
