pub(crate) mod cursor;
pub(crate) mod dto;
pub(crate) mod projection;
mod query;
pub(crate) mod spark;
pub(crate) mod threshold;

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::web_projection::{WebView, web_view_by_name};
use sha2::{Digest, Sha256};

use self::cursor::FrameCursor;
use self::query::FrameFilter;
use super::catalog::{ProjectionCatalog, Scope, ViewSpec};
use crate::api_error::{
    ApiError, ExpectedValue, LimitResource, QueryConstraint, QueryParameter, count_u64,
};
use crate::params::{QueryParams, parse_duration_us, parse_i64};

pub(crate) const DEFAULT_FRAME_LIMIT: usize = 100;
pub(crate) const MAX_FRAME_LIMIT: usize = 200;
pub(crate) const DEFAULT_SPAN_US: i64 = 3_600_000_000;
pub(crate) const MAX_SPAN_US: i64 = 86_400_000_000;
pub(crate) const MAX_FILTER_BYTES: usize = 256;
pub(crate) const MAX_FRAME_CURSOR_BYTES: usize = 512;
pub(crate) const MAX_FRAME_RESPONSE_BYTES: usize = 1_048_576;
pub(crate) const MAX_FRAME_COLUMNS: usize = 32;

const FRAME_PARAMETERS: &[QueryParameter] = &[
    QueryParameter::At,
    QueryParameter::Span,
    QueryParameter::Preset,
    QueryParameter::Columns,
    QueryParameter::Database,
    QueryParameter::Q,
    QueryParameter::Sort,
    QueryParameter::Order,
    QueryParameter::Limit,
    QueryParameter::Cursor,
];

#[derive(Debug)]
pub(crate) struct FrameRequest {
    pub view: &'static WebView,
    pub at_us: i64,
    pub span_us: i64,
    pub preset: Option<&'static str>,
    pub columns: Vec<&'static str>,
    pub database: Option<DatabaseFilter>,
    pub filter: Option<FrameFilter>,
    pub sort: &'static str,
    pub descending: bool,
    pub limit: usize,
    pub cursor: Option<FrameCursor>,
}

#[derive(Debug, Clone)]
pub(crate) struct DatabaseFilter {
    token: String,
    pub(crate) oid: u32,
}

impl FrameRequest {
    #[allow(
        clippy::too_many_lines,
        reason = "the admission sequence is kept together so every field is validated before I/O"
    )]
    pub(crate) fn parse(
        view_name: &str,
        raw_query: Option<&str>,
        catalog: &ProjectionCatalog,
    ) -> Result<Self, ApiError> {
        let Some(view) = web_view_by_name(view_name) else {
            return Err(ApiError::invalid_query_parameter(
                QueryParameter::View,
                ExpectedValue::ProjectionCode,
            ));
        };
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
        let params = QueryParams::parse(raw_query, FRAME_PARAMETERS)?;
        let at_us = parse_i64(&params, QueryParameter::At)?;
        let span_us = parse_duration_us(&params, QueryParameter::Span, DEFAULT_SPAN_US)?;
        if span_us > MAX_SPAN_US {
            return Err(ApiError::query_shape_limit_exceeded(
                LimitResource::QuerySpanUs,
                MAX_SPAN_US.unsigned_abs(),
                Some(span_us.unsigned_abs()),
            ));
        }

        let preset_parameter = params.get(QueryParameter::Preset);
        let columns_parameter = params.get(QueryParameter::Columns);
        if preset_parameter.is_some() && columns_parameter.is_some() {
            return Err(ApiError::invalid_query_constraint(
                QueryConstraint::PresetOrColumns,
            ));
        }
        let preset = if columns_parameter.is_some() {
            None
        } else {
            Some(
                preset_parameter
                    .map_or_else(
                        || view_spec.presets.first(),
                        |code| {
                            view_spec
                                .presets
                                .iter()
                                .find(|candidate| candidate.code == code)
                        },
                    )
                    .ok_or_else(|| {
                        ApiError::invalid_query_parameter(
                            QueryParameter::Preset,
                            ExpectedValue::ProjectionCode,
                        )
                    })?,
            )
        };
        let columns = match (columns_parameter, preset) {
            (Some(raw), None) => parse_frame_columns(raw, view_spec)?,
            (None, Some(preset)) => preset
                .columns
                .iter()
                .filter_map(|code| {
                    view_spec
                        .columns
                        .iter()
                        .find(|column| column.code == *code && !column.lazy)
                        .map(|column| column.code)
                })
                .collect(),
            _ => unreachable!("preset and columns admission is exhaustive"),
        };
        if columns.is_empty() {
            return Err(ApiError::invalid_query_parameter(
                QueryParameter::Columns,
                ExpectedValue::ProjectionColumnList,
            ));
        }
        let sort = params
            .get(QueryParameter::Sort)
            .or_else(|| preset.map(|preset| preset.sort.column))
            .unwrap_or(columns[0]);
        let sort = view_spec
            .columns
            .iter()
            .find(|column| column.code == sort && !column.lazy)
            .map(|column| column.code)
            .ok_or_else(|| {
                ApiError::invalid_query_parameter(
                    QueryParameter::Sort,
                    ExpectedValue::ProjectionCode,
                )
            })?;
        let order = params
            .get(QueryParameter::Order)
            .or_else(|| preset.map(|preset| preset.sort.order))
            .unwrap_or("asc");
        let descending = match order {
            "asc" => false,
            "desc" => true,
            _ => {
                return Err(ApiError::invalid_query_parameter(
                    QueryParameter::Order,
                    ExpectedValue::SortOrder,
                ));
            }
        };

        let limit = params
            .get(QueryParameter::Limit)
            .map_or(Ok(DEFAULT_FRAME_LIMIT), |raw| {
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
        if limit > MAX_FRAME_LIMIT {
            return Err(ApiError::query_shape_limit_exceeded(
                LimitResource::Rows,
                count_u64(MAX_FRAME_LIMIT),
                Some(count_u64(limit)),
            ));
        }

        let database = params
            .get(QueryParameter::Database)
            .map(|raw| parse_database_filter(raw, view_spec.scope))
            .transpose()?;
        let filter = params
            .get(QueryParameter::Q)
            .map(|value| {
                if value.len() > MAX_FILTER_BYTES {
                    return Err(ApiError::query_shape_limit_exceeded(
                        LimitResource::Bytes,
                        count_u64(MAX_FILTER_BYTES),
                        Some(count_u64(value.len())),
                    ));
                }
                FrameFilter::parse(value, view_spec)
            })
            .transpose()?;

        let mut request = Self {
            view,
            at_us,
            span_us,
            preset: preset.map(|preset| preset.code),
            columns,
            database,
            filter,
            sort,
            descending,
            limit,
            cursor: None,
        };
        if let Some(encoded) = params.get(QueryParameter::Cursor) {
            if encoded.len() > MAX_FRAME_CURSOR_BYTES {
                return Err(ApiError::query_shape_limit_exceeded(
                    LimitResource::Bytes,
                    count_u64(MAX_FRAME_CURSOR_BYTES),
                    Some(count_u64(encoded.len())),
                ));
            }
            let cursor =
                FrameCursor::decode(encoded).map_err(|_error| ApiError::invalid_cursor())?;
            if cursor.view_code() != request.view.code
                || cursor.view_revision() != request.view.revision
                || cursor.query_fingerprint() != request.query_fingerprint()
            {
                return Err(ApiError::cursor_query_mismatch());
            }
            request.cursor = Some(cursor);
        }
        Ok(request)
    }

    pub(crate) fn query_fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"pgkronika-frame-query-v1");
        hasher.update(self.view.code.to_le_bytes());
        hasher.update(self.view.revision.to_le_bytes());
        hasher.update(self.at_us.to_le_bytes());
        hasher.update(self.span_us.to_le_bytes());
        update_string(&mut hasher, self.preset);
        for column in &self.columns {
            update_string(&mut hasher, Some(column));
        }
        update_string(
            &mut hasher,
            self.database
                .as_ref()
                .map(|database| database.token.as_str()),
        );
        update_string(
            &mut hasher,
            self.filter.as_ref().map(FrameFilter::canonical),
        );
        update_string(&mut hasher, Some(self.sort));
        hasher.update([u8::from(self.descending)]);
        hasher.finalize().into()
    }
}

fn parse_frame_columns(raw: &str, view: &ViewSpec) -> Result<Vec<&'static str>, ApiError> {
    let mut seen = BTreeSet::new();
    let mut columns = Vec::new();
    for code in raw.split(',') {
        let Some(column) = view
            .columns
            .iter()
            .find(|column| column.code == code && !column.lazy)
        else {
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
    if columns.is_empty() || columns.len() > MAX_FRAME_COLUMNS {
        return Err(ApiError::query_shape_limit_exceeded(
            LimitResource::Cells,
            count_u64(MAX_FRAME_COLUMNS),
            Some(count_u64(columns.len())),
        ));
    }
    Ok(columns)
}

fn parse_database_filter(raw: &str, scope: Scope) -> Result<DatabaseFilter, ApiError> {
    if scope != Scope::Database {
        return Err(ApiError::invalid_query_parameter(
            QueryParameter::Database,
            ExpectedValue::EntityToken,
        ));
    }
    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_error| {
        ApiError::invalid_query_parameter(QueryParameter::Database, ExpectedValue::EntityToken)
    })?;
    let revision = bytes
        .get(..2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes);
    let has_system_identifier = bytes.get(2).copied();
    let oid_offset = match has_system_identifier {
        Some(0) if bytes.len() == 7 => 3,
        Some(1) if bytes.len() == 15 => 11,
        _ => {
            return Err(ApiError::invalid_query_parameter(
                QueryParameter::Database,
                ExpectedValue::EntityToken,
            ));
        }
    };
    if revision != Some(1) {
        return Err(ApiError::invalid_query_parameter(
            QueryParameter::Database,
            ExpectedValue::EntityToken,
        ));
    }
    let oid = bytes
        .get(oid_offset..oid_offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .filter(|oid| *oid != 0)
        .ok_or_else(|| {
            ApiError::invalid_query_parameter(QueryParameter::Database, ExpectedValue::EntityToken)
        })?;
    Ok(DatabaseFilter {
        token: raw.to_owned(),
        oid,
    })
}

fn update_string(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update([0]),
    }
}
