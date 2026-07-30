#![allow(
    dead_code,
    reason = "frame contracts are wired into projection and HTTP consumers in later tasks"
)]

pub(crate) mod cursor;
pub(crate) mod dto;
pub(crate) mod projection;
mod query;

use kronika_analytics::web_projection::{WebView, web_view_by_name};
use sha2::{Digest, Sha256};

use self::cursor::FrameCursor;
use super::catalog::ProjectionCatalog;
use crate::api_error::{ApiError, ExpectedValue, LimitResource, QueryParameter, count_u64};
use crate::params::{QueryParams, parse_duration_us, parse_i64};

pub(crate) const DEFAULT_FRAME_LIMIT: usize = 100;
pub(crate) const MAX_FRAME_LIMIT: usize = 200;
pub(crate) const DEFAULT_SPAN_US: i64 = 3_600_000_000;
pub(crate) const MAX_SPAN_US: i64 = 86_400_000_000;
pub(crate) const MAX_FILTER_BYTES: usize = 256;
pub(crate) const MAX_FRAME_CURSOR_BYTES: usize = 512;
pub(crate) const MAX_FRAME_RESPONSE_BYTES: usize = 1_048_576;

const FRAME_PARAMETERS: &[QueryParameter] = &[
    QueryParameter::At,
    QueryParameter::Span,
    QueryParameter::Preset,
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
    pub preset: &'static str,
    pub database: Option<String>,
    pub filter: Option<String>,
    pub sort: &'static str,
    pub descending: bool,
    pub limit: usize,
    pub cursor: Option<FrameCursor>,
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

        let preset = params
            .get(QueryParameter::Preset)
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
            })?;
        let sort = params
            .get(QueryParameter::Sort)
            .unwrap_or(preset.sort.column);
        let sort = view_spec
            .columns
            .iter()
            .find(|column| column.code == sort && preset.columns.contains(&column.code))
            .map(|column| column.code)
            .ok_or_else(|| {
                ApiError::invalid_query_parameter(
                    QueryParameter::Sort,
                    ExpectedValue::ProjectionCode,
                )
            })?;
        let order = params
            .get(QueryParameter::Order)
            .unwrap_or(preset.sort.order);
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

        let database = params.get(QueryParameter::Database).map(str::to_owned);
        let filter = params
            .get(QueryParameter::Q)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if filter
            .as_ref()
            .is_some_and(|value| value.len() > MAX_FILTER_BYTES)
        {
            return Err(ApiError::query_shape_limit_exceeded(
                LimitResource::Bytes,
                count_u64(MAX_FILTER_BYTES),
                filter.as_ref().map(|value| count_u64(value.len())),
            ));
        }

        let mut request = Self {
            view,
            at_us,
            span_us,
            preset: preset.code,
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
        update_string(&mut hasher, Some(self.preset));
        update_string(&mut hasher, self.database.as_deref());
        update_string(&mut hasher, self.filter.as_deref());
        update_string(&mut hasher, Some(self.sort));
        hasher.update([u8::from(self.descending)]);
        hasher.finalize().into()
    }
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
