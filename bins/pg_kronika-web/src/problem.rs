//! Locale-neutral RFC 9457 application errors.

use std::fmt::Write as _;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::ser::SerializeMap as _;
use sha2::{Digest as _, Sha256};

const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";
const REQUEST_ID_HEADER: &str = "x-request-id";
const RETRY_AFTER_SECONDS: u64 = 1;
const MAX_PUBLIC_TOKEN_BYTES: usize = 64;

closed_string_enum! {
    /// Stable application-error codes exposed to API clients.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ProblemCode {
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
        ViewGone => "view_gone",
        QueryLimitExceeded => "query_limit_exceeded",
        AnalyticCapacityUnavailable => "analytic_capacity_unavailable",
        StoreReadFailed => "store_read_failed",
        InternalError => "internal_error",
    }
}

impl ProblemCode {
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::RouteNotFound | Self::UnknownSection => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::MissingQueryParameter
            | Self::InvalidQueryParameter
            | Self::UnknownQueryParameter
            | Self::DuplicateQueryParameter
            | Self::InvalidQueryConstraint
            | Self::InvalidCursor
            | Self::CursorQueryMismatch => StatusCode::BAD_REQUEST,
            Self::ViewGone => StatusCode::GONE,
            Self::QueryLimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::AnalyticCapacityUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::StoreReadFailed | Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) const fn type_uri(self) -> &'static str {
        match self {
            Self::Unauthorized => "https://pgkronika.dev/problems/unauthorized",
            Self::RouteNotFound => "https://pgkronika.dev/problems/route-not-found",
            Self::MethodNotAllowed => "https://pgkronika.dev/problems/method-not-allowed",
            Self::MissingQueryParameter => "https://pgkronika.dev/problems/missing-query-parameter",
            Self::InvalidQueryParameter => "https://pgkronika.dev/problems/invalid-query-parameter",
            Self::UnknownQueryParameter => "https://pgkronika.dev/problems/unknown-query-parameter",
            Self::DuplicateQueryParameter => {
                "https://pgkronika.dev/problems/duplicate-query-parameter"
            }
            Self::InvalidQueryConstraint => {
                "https://pgkronika.dev/problems/invalid-query-constraint"
            }
            Self::UnknownSection => "https://pgkronika.dev/problems/unknown-section",
            Self::InvalidCursor => "https://pgkronika.dev/problems/invalid-cursor",
            Self::CursorQueryMismatch => "https://pgkronika.dev/problems/cursor-query-mismatch",
            Self::ViewGone => "https://pgkronika.dev/problems/view-gone",
            Self::QueryLimitExceeded => "https://pgkronika.dev/problems/query-limit-exceeded",
            Self::AnalyticCapacityUnavailable => {
                "https://pgkronika.dev/problems/analytic-capacity-unavailable"
            }
            Self::StoreReadFailed => "https://pgkronika.dev/problems/store-read-failed",
            Self::InternalError => "https://pgkronika.dev/problems/internal-error",
        }
    }
}

closed_string_enum! {
    /// Bounded query-parameter identifiers allowed in Problem params.
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum QueryParameter {
        Source => "source",
        From => "from",
        To => "to",
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
    }
}

impl QueryParameter {
    pub(crate) const fn from_query_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"source" => Some(Self::Source),
            b"from" => Some(Self::From),
            b"to" => Some(Self::To),
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
            Self::Parameter(parameter) => parameter.serialize(serializer),
        }
    }
}

closed_string_enum! {
    /// Expected machine types for invalid query parameters.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ExpectedValue {
        UrlEncodedQuery => "url_encoded_query",
        Uint64 => "uint64",
        Int64 => "int64",
        PositiveDuration => "positive_duration",
        NonNegativeFiniteNumber => "non_negative_finite_number",
        NonNegativeInteger => "non_negative_integer",
        SectionList => "section_list",
        Severity => "severity",
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
        IdentityBytes => "identity_bytes",
        SeriesPoints => "series_points",
        Episodes => "episodes",
        Clusters => "clusters",
        IncidentKeyBytes => "incident_key_bytes",
        TotalIncidentKeyBytes => "total_incident_key_bytes",
    }
}

#[derive(Debug, Clone, Copy)]
struct EmptyParams;

impl Serialize for EmptyParams {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_map(Some(0))?.end()
    }
}

#[derive(Debug, Clone, Serialize)]
struct ParameterParams {
    parameter: QueryParameter,
}

#[derive(Debug, Clone, Serialize)]
struct InvalidParameterParams {
    parameter: InvalidParameterLocation,
    expected: ExpectedValue,
}

#[derive(Debug, Clone, Serialize)]
struct UnknownParameterParams {
    parameter: String,
}

#[derive(Debug, Clone, Serialize)]
struct ConstraintParams {
    constraint: QueryConstraint,
}

#[derive(Debug, Clone, Serialize)]
struct SectionParams {
    section: String,
}

#[derive(Debug, Clone, Serialize)]
struct LimitParams {
    resource: LimitResource,
    limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct CapacityParams {
    retry_after_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ProblemParams {
    Empty(EmptyParams),
    Parameter(ParameterParams),
    InvalidParameter(InvalidParameterParams),
    UnknownParameter(UnknownParameterParams),
    Constraint(ConstraintParams),
    Section(SectionParams),
    Limit(LimitParams),
    Capacity(CapacityParams),
}

/// A closed, machine-only RFC 9457 response.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApiProblem {
    #[serde(rename = "type")]
    type_uri: &'static str,
    status: u16,
    code: ProblemCode,
    params: ProblemParams,
    instance: String,
    #[serde(skip)]
    request_id: String,
    #[serde(skip)]
    allow: Option<&'static str>,
}

impl ApiProblem {
    fn new(code: ProblemCode, params: ProblemParams) -> Self {
        let request_id = next_request_id();
        Self {
            type_uri: code.type_uri(),
            status: code.status().as_u16(),
            code,
            params,
            instance: format!("https://pgkronika.dev/problems/occurrences/{request_id}"),
            request_id,
            allow: None,
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self::empty(ProblemCode::Unauthorized)
    }

    pub(crate) fn route_not_found() -> Self {
        Self::empty(ProblemCode::RouteNotFound)
    }

    pub(crate) fn method_not_allowed(allow: &'static str) -> Self {
        let mut problem = Self::empty(ProblemCode::MethodNotAllowed);
        problem.allow = Some(allow);
        problem
    }

    pub(crate) fn missing_query_parameter(parameter: QueryParameter) -> Self {
        Self::new(
            ProblemCode::MissingQueryParameter,
            ProblemParams::Parameter(ParameterParams { parameter }),
        )
    }

    pub(crate) fn invalid_query_parameter(
        parameter: impl Into<InvalidParameterLocation>,
        expected: ExpectedValue,
    ) -> Self {
        Self::new(
            ProblemCode::InvalidQueryParameter,
            ProblemParams::InvalidParameter(InvalidParameterParams {
                parameter: parameter.into(),
                expected,
            }),
        )
    }

    pub(crate) fn unknown_query_parameter(parameter: &str) -> Self {
        Self::new(
            ProblemCode::UnknownQueryParameter,
            ProblemParams::UnknownParameter(UnknownParameterParams {
                parameter: bounded_public_token(parameter),
            }),
        )
    }

    pub(crate) fn duplicate_query_parameter(parameter: QueryParameter) -> Self {
        Self::new(
            ProblemCode::DuplicateQueryParameter,
            ProblemParams::Parameter(ParameterParams { parameter }),
        )
    }

    pub(crate) fn invalid_query_constraint(constraint: QueryConstraint) -> Self {
        Self::new(
            ProblemCode::InvalidQueryConstraint,
            ProblemParams::Constraint(ConstraintParams { constraint }),
        )
    }

    pub(crate) fn unknown_section(section: &str) -> Self {
        Self::new(
            ProblemCode::UnknownSection,
            ProblemParams::Section(SectionParams {
                section: bounded_public_token(section),
            }),
        )
    }

    pub(crate) fn invalid_cursor() -> Self {
        Self::empty(ProblemCode::InvalidCursor)
    }

    pub(crate) fn cursor_query_mismatch() -> Self {
        Self::empty(ProblemCode::CursorQueryMismatch)
    }

    pub(crate) fn view_gone() -> Self {
        Self::empty(ProblemCode::ViewGone)
    }

    pub(crate) fn query_limit_exceeded(
        resource: LimitResource,
        limit: u64,
        observed: Option<u64>,
    ) -> Self {
        Self::new(
            ProblemCode::QueryLimitExceeded,
            ProblemParams::Limit(LimitParams {
                resource,
                limit,
                observed,
            }),
        )
    }

    pub(crate) fn analytic_capacity_unavailable() -> Self {
        Self::new(
            ProblemCode::AnalyticCapacityUnavailable,
            ProblemParams::Capacity(CapacityParams {
                retry_after_seconds: RETRY_AFTER_SECONDS,
            }),
        )
    }

    pub(crate) fn store_read_failed() -> Self {
        Self::empty(ProblemCode::StoreReadFailed)
    }

    pub(crate) fn internal_error() -> Self {
        Self::empty(ProblemCode::InternalError)
    }

    fn empty(code: ProblemCode) -> Self {
        Self::new(code, ProblemParams::Empty(EmptyParams))
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    #[cfg(test)]
    pub(crate) const fn code(&self) -> ProblemCode {
        self.code
    }
}

impl IntoResponse for ApiProblem {
    fn into_response(mut self) -> Response {
        let status = self.code.status();
        let code = self.code;
        let request_id = match HeaderValue::from_str(&self.request_id) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    event = "api_request_id_header_invalid",
                    error = %error,
                    "generated request id violated the header contract"
                );
                "invalid".clone_into(&mut self.request_id);
                "https://pgkronika.dev/problems/occurrences/invalid".clone_into(&mut self.instance);
                HeaderValue::from_static("invalid")
            }
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(
                event = "api_problem_response",
                request_id = self.request_id.as_str(),
                code = code.as_str(),
                status = status.as_u16(),
                "API problem response"
            );
        } else {
            tracing::debug!(
                event = "api_problem_response",
                request_id = self.request_id.as_str(),
                code = code.as_str(),
                status = status.as_u16(),
                "API problem response"
            );
        }
        let allow = self.allow.take();
        let mut response = (status, Json(self)).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_MEDIA_TYPE),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(REQUEST_ID_HEADER, request_id);
        match code {
            ProblemCode::Unauthorized => {
                headers.insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Basic realm=\"pg_kronika-web\""),
                );
            }
            ProblemCode::MethodNotAllowed => {
                if let Some(allow) = allow {
                    headers.insert(header::ALLOW, HeaderValue::from_static(allow));
                }
            }
            ProblemCode::AnalyticCapacityUnavailable => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECONDS));
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

fn bounded_public_token(value: &str) -> String {
    if value.is_empty()
        || value.len() > MAX_PUBLIC_TOKEN_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return "invalid".to_owned();
    }
    value.to_owned()
}

fn next_request_id() -> String {
    static PROCESS_NONCE: LazyLock<u64> = LazyLock::new(process_nonce);
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut request_id = String::with_capacity(32);
    let _ = write!(request_id, "{:016x}{sequence:016x}", *PROCESS_NONCE);
    request_id
}

fn process_nonce() -> u64 {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(started.to_be_bytes());
    hasher.update(std::process::id().to_be_bytes());
    let digest = hasher.finalize();
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::{MAX_PUBLIC_TOKEN_BYTES, bounded_public_token};

    #[test]
    fn public_tokens_enforce_the_exact_grammar_and_byte_bound() {
        assert_eq!(bounded_public_token("Az09_.-"), "Az09_.-");
        assert_eq!(
            bounded_public_token(&"a".repeat(MAX_PUBLIC_TOKEN_BYTES)),
            "a".repeat(MAX_PUBLIC_TOKEN_BYTES)
        );
        assert_eq!(
            bounded_public_token(&"a".repeat(MAX_PUBLIC_TOKEN_BYTES + 1)),
            "invalid"
        );
        assert_eq!(bounded_public_token(""), "invalid");
        assert_eq!(bounded_public_token("секрет"), "invalid");

        for byte in 0_u8..=127 {
            let token = char::from(byte).to_string();
            let allowed = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-');
            assert_eq!(
                bounded_public_token(&token),
                if allowed { token } else { "invalid".to_owned() },
                "ASCII byte {byte}"
            );
        }
    }
}
