//! Small machine-readable errors for the internal JSON API.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

const RETRY_AFTER_SECONDS: u64 = 1;

closed_string_enum! {
    /// Stable application-error codes exposed to API clients.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ErrorCode {
        Unauthorized => "unauthorized",
        RouteNotFound => "route_not_found",
        MethodNotAllowed => "method_not_allowed",
        MissingQueryParameter => "missing_query_parameter",
        InvalidQueryParameter => "invalid_query_parameter",
        UnknownQueryParameter => "unknown_query_parameter",
        DuplicateQueryParameter => "duplicate_query_parameter",
        InvalidQueryConstraint => "invalid_query_constraint",
        UnknownSection => "unknown_section",
        InvalidCursor => "invalid_cursor",
        CursorQueryMismatch => "cursor_query_mismatch",
        CursorExpired => "cursor_expired",
        ViewGone => "view_gone",
        EntityNotFound => "entity_not_found",
        QueryLimitExceeded => "query_limit_exceeded",
        CursorCapacityUnavailable => "cursor_capacity_unavailable",
        AnalyticCapacityUnavailable => "analytic_capacity_unavailable",
        ColdBuildOverloaded => "cold_build_overloaded",
        StoreReadFailed => "store_read_failed",
        InternalError => "internal_error",
    }
}

impl ErrorCode {
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::RouteNotFound | Self::UnknownSection | Self::EntityNotFound => {
                StatusCode::NOT_FOUND
            }
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::MissingQueryParameter
            | Self::InvalidQueryParameter
            | Self::UnknownQueryParameter
            | Self::DuplicateQueryParameter
            | Self::InvalidQueryConstraint
            | Self::InvalidCursor
            | Self::CursorQueryMismatch => StatusCode::BAD_REQUEST,
            Self::CursorExpired | Self::ViewGone => StatusCode::GONE,
            Self::QueryLimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::CursorCapacityUnavailable
            | Self::AnalyticCapacityUnavailable
            | Self::ColdBuildOverloaded => StatusCode::SERVICE_UNAVAILABLE,
            Self::StoreReadFailed | Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

closed_string_enum! {
    /// Query-parameter identifiers used by validation and API error params.
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum QueryParameter {
        At => "at",
        From => "from",
        To => "to",
        View => "view",
        Metric => "metric",
        Buckets => "buckets",
        Top => "top",
        Window => "window",
        Step => "step",
        Threshold => "threshold",
        EpsRel => "eps_rel",
        Epsilon => "epsilon",
        MaxClusterSpan => "max_cluster_span",
        Section => "section",
        Names => "names",
        Limit => "limit",
        Cursor => "cursor",
        MinSeverity => "min_severity",
        Kind => "kind",
        Span => "span",
        Preset => "preset",
        Database => "database",
        Q => "q",
        Sort => "sort",
        Order => "order",
        Columns => "columns",
        Include => "include",
    }
}

impl QueryParameter {
    pub(crate) const fn from_query_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"at" => Some(Self::At),
            b"from" => Some(Self::From),
            b"to" => Some(Self::To),
            b"view" => Some(Self::View),
            b"metric" => Some(Self::Metric),
            b"buckets" => Some(Self::Buckets),
            b"top" => Some(Self::Top),
            b"window" => Some(Self::Window),
            b"step" => Some(Self::Step),
            b"threshold" => Some(Self::Threshold),
            b"eps_rel" => Some(Self::EpsRel),
            b"epsilon" => Some(Self::Epsilon),
            b"max_cluster_span" => Some(Self::MaxClusterSpan),
            b"section" => Some(Self::Section),
            b"names" => Some(Self::Names),
            b"limit" => Some(Self::Limit),
            b"cursor" => Some(Self::Cursor),
            b"min_severity" => Some(Self::MinSeverity),
            b"kind" => Some(Self::Kind),
            b"span" => Some(Self::Span),
            b"preset" => Some(Self::Preset),
            b"database" => Some(Self::Database),
            b"q" => Some(Self::Q),
            b"sort" => Some(Self::Sort),
            b"order" => Some(Self::Order),
            b"columns" => Some(Self::Columns),
            b"include" => Some(Self::Include),
            _ => None,
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// Location accepted by `invalid_query_parameter`; raw query syntax is not a
/// parameter and cannot be used by missing/duplicate constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvalidParameterLocation {
    Query,
    Entity,
    Parameter(QueryParameter),
}

impl From<QueryParameter> for InvalidParameterLocation {
    fn from(parameter: QueryParameter) -> Self {
        Self::Parameter(parameter)
    }
}

impl Serialize for InvalidParameterLocation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Query => serializer.serialize_str("query"),
            Self::Entity => serializer.serialize_str("entity"),
            Self::Parameter(parameter) => parameter.serialize(serializer),
        }
    }
}

closed_string_enum! {
    /// Expected machine types for invalid query parameters.
    #[allow(
        dead_code,
        reason = "frame-only expected values become runtime-used when the frame route is registered"
    )]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ExpectedValue {
        UrlEncodedQuery => "url_encoded_query",
        Uint64 => "uint64",
        Int64 => "int64",
        PositiveDuration => "positive_duration",
        PositiveInteger => "positive_integer",
        NonNegativeFiniteNumber => "non_negative_finite_number",
        NonNegativeInteger => "non_negative_integer",
        SectionList => "section_list",
        Severity => "severity",
        ProjectionCode => "projection_code",
        SortOrder => "sort_order",
        FilterExpression => "filter_expression",
        EntityToken => "entity_token",
        ProjectionColumnList => "projection_column_list",
    }
}

closed_string_enum! {
    /// Cross-parameter constraints enforced before a query runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum QueryConstraint {
        FromBeforeTo => "from_before_to",
        WindowWithinInterval => "window_within_interval",
        EpsilonNotGreaterThanMaxClusterSpan => "epsilon_not_greater_than_max_cluster_span",
        MaxClusterSpanWithinInterval => "max_cluster_span_within_interval",
        FiniteScan => "finite_scan",
        PointOrHistory => "point_or_history",
        HistorySupported => "history_supported",
        PresetOrColumns => "preset_or_columns",
    }
}

closed_string_enum! {
    /// Resource dimensions used by `query_limit_exceeded`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum LimitResource {
        QueryBytes => "query_bytes",
        QueryParameters => "query_parameters",
        QuerySpanUs => "query_span_us",
        WindowPositions => "window_positions",
        Rows => "rows",
        Cells => "cells",
        Bytes => "bytes",
        Units => "units",
        Sections => "sections",
        SeriesPoints => "series_points",
        Episodes => "episodes",
        Clusters => "clusters",
        IncidentKeyBytes => "incident_key_bytes",
        TotalIncidentKeyBytes => "total_incident_key_bytes",
        SelectedSegments => "selected_segments",
    }
}

/// A small machine-readable API error.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ApiError {
    #[schema(value_type = String)]
    code: ErrorCode,
    #[schema(value_type = Object)]
    params: serde_json::Value,
    #[serde(skip)]
    status: StatusCode,
    #[serde(skip)]
    allow: Option<&'static str>,
    #[serde(skip)]
    retry_after_seconds: Option<u64>,
}

impl ApiError {
    const fn new(code: ErrorCode, params: serde_json::Value) -> Self {
        Self {
            code,
            params,
            status: code.status(),
            allow: None,
            retry_after_seconds: None,
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self::empty(ErrorCode::Unauthorized)
    }

    pub(crate) fn route_not_found() -> Self {
        Self::empty(ErrorCode::RouteNotFound)
    }

    pub(crate) fn method_not_allowed(allow: &'static str) -> Self {
        let mut error = Self::empty(ErrorCode::MethodNotAllowed);
        error.allow = Some(allow);
        error
    }

    pub(crate) fn missing_query_parameter(parameter: QueryParameter) -> Self {
        Self::new(
            ErrorCode::MissingQueryParameter,
            serde_json::json!({ "parameter": parameter }),
        )
    }

    pub(crate) fn invalid_query_parameter(
        parameter: impl Into<InvalidParameterLocation>,
        expected: ExpectedValue,
    ) -> Self {
        Self::new(
            ErrorCode::InvalidQueryParameter,
            serde_json::json!({
                "parameter": parameter.into(),
                "expected": expected,
            }),
        )
    }

    pub(crate) fn unknown_query_parameter(parameter: &str) -> Self {
        Self::new(
            ErrorCode::UnknownQueryParameter,
            serde_json::json!({ "parameter": parameter }),
        )
    }

    pub(crate) fn duplicate_query_parameter(parameter: QueryParameter) -> Self {
        Self::new(
            ErrorCode::DuplicateQueryParameter,
            serde_json::json!({ "parameter": parameter }),
        )
    }

    pub(crate) fn invalid_query_constraint(constraint: QueryConstraint) -> Self {
        Self::new(
            ErrorCode::InvalidQueryConstraint,
            serde_json::json!({ "constraint": constraint }),
        )
    }

    pub(crate) fn unknown_section(section: &str) -> Self {
        Self::new(
            ErrorCode::UnknownSection,
            serde_json::json!({ "section": section }),
        )
    }

    pub(crate) fn invalid_cursor() -> Self {
        Self::empty(ErrorCode::InvalidCursor)
    }

    pub(crate) fn cursor_query_mismatch() -> Self {
        Self::empty(ErrorCode::CursorQueryMismatch)
    }

    pub(crate) fn cursor_expired() -> Self {
        Self::empty(ErrorCode::CursorExpired)
    }

    pub(crate) fn view_gone() -> Self {
        Self::empty(ErrorCode::ViewGone)
    }

    pub(crate) fn entity_not_found() -> Self {
        Self::empty(ErrorCode::EntityNotFound)
    }

    pub(crate) fn query_limit_exceeded(
        resource: LimitResource,
        limit: u64,
        observed: Option<u64>,
    ) -> Self {
        let mut params = serde_json::json!({
            "resource": resource,
            "limit": limit,
        });
        if let Some(observed) = observed {
            params["observed"] = observed.into();
        }
        Self::new(ErrorCode::QueryLimitExceeded, params)
    }

    pub(crate) fn query_shape_limit_exceeded(
        resource: LimitResource,
        limit: u64,
        observed: Option<u64>,
    ) -> Self {
        let mut error = Self::query_limit_exceeded(resource, limit, observed);
        error.status = StatusCode::BAD_REQUEST;
        error
    }

    pub(crate) fn cursor_capacity_unavailable() -> Self {
        Self::empty(ErrorCode::CursorCapacityUnavailable)
    }

    pub(crate) fn analytic_capacity_unavailable() -> Self {
        let mut error = Self::new(
            ErrorCode::AnalyticCapacityUnavailable,
            serde_json::json!({ "retry_after_seconds": RETRY_AFTER_SECONDS }),
        );
        error.retry_after_seconds = Some(RETRY_AFTER_SECONDS);
        error
    }

    pub(crate) fn cold_build_overloaded(retry_after_seconds: u64) -> Self {
        let mut error = Self::new(
            ErrorCode::ColdBuildOverloaded,
            serde_json::json!({ "retry_after_seconds": retry_after_seconds }),
        );
        error.retry_after_seconds = Some(retry_after_seconds);
        error
    }

    pub(crate) fn store_read_failed() -> Self {
        Self::empty(ErrorCode::StoreReadFailed)
    }

    pub(crate) fn internal_error() -> Self {
        Self::empty(ErrorCode::InternalError)
    }

    fn empty(code: ErrorCode) -> Self {
        Self::new(code, serde_json::json!({}))
    }

    #[cfg(test)]
    pub(crate) const fn code(&self) -> ErrorCode {
        self.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        let code = self.code;
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(
                event = "api_error_response",
                code = code.as_str(),
                status = status.as_u16(),
                "API error response"
            );
        } else {
            tracing::debug!(
                event = "api_error_response",
                code = code.as_str(),
                status = status.as_u16(),
                "API error response"
            );
        }
        let allow = self.allow;
        let retry_after_seconds = self.retry_after_seconds;
        let mut response = (status, Json(self)).into_response();
        let headers = response.headers_mut();
        match code {
            ErrorCode::Unauthorized => {
                headers.insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Basic realm=\"pg_kronika-web\""),
                );
            }
            ErrorCode::MethodNotAllowed => {
                if let Some(allow) = allow {
                    headers.insert(header::ALLOW, HeaderValue::from_static(allow));
                }
            }
            ErrorCode::AnalyticCapacityUnavailable | ErrorCode::ColdBuildOverloaded => {
                if let Some(seconds) = retry_after_seconds {
                    headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
                }
            }
            _ => {}
        }
        response
    }
}

/// Saturating conversion for externally reported collection counts.
pub(crate) fn count_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
