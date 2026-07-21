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

/// Stable application-error codes exposed to API clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProblemCode {
    Unauthorized,
    RouteNotFound,
    MethodNotAllowed,
    MissingQueryParameter,
    InvalidQueryParameter,
    UnknownQueryParameter,
    DuplicateQueryParameter,
    InvalidQueryConstraint,
    UnknownSection,
    InvalidCursor,
    QueryLimitExceeded,
    AnalyticCapacityUnavailable,
    StoreReadFailed,
    InternalError,
}

impl Serialize for ProblemCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl ProblemCode {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 14] = [
        Self::Unauthorized,
        Self::RouteNotFound,
        Self::MethodNotAllowed,
        Self::MissingQueryParameter,
        Self::InvalidQueryParameter,
        Self::UnknownQueryParameter,
        Self::DuplicateQueryParameter,
        Self::InvalidQueryConstraint,
        Self::UnknownSection,
        Self::InvalidCursor,
        Self::QueryLimitExceeded,
        Self::AnalyticCapacityUnavailable,
        Self::StoreReadFailed,
        Self::InternalError,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::RouteNotFound => "route_not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::MissingQueryParameter => "missing_query_parameter",
            Self::InvalidQueryParameter => "invalid_query_parameter",
            Self::UnknownQueryParameter => "unknown_query_parameter",
            Self::DuplicateQueryParameter => "duplicate_query_parameter",
            Self::InvalidQueryConstraint => "invalid_query_constraint",
            Self::UnknownSection => "unknown_section",
            Self::InvalidCursor => "invalid_cursor",
            Self::QueryLimitExceeded => "query_limit_exceeded",
            Self::AnalyticCapacityUnavailable => "analytic_capacity_unavailable",
            Self::StoreReadFailed => "store_read_failed",
            Self::InternalError => "internal_error",
        }
    }

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
            | Self::InvalidCursor => StatusCode::BAD_REQUEST,
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
            Self::QueryLimitExceeded => "https://pgkronika.dev/problems/query-limit-exceeded",
            Self::AnalyticCapacityUnavailable => {
                "https://pgkronika.dev/problems/analytic-capacity-unavailable"
            }
            Self::StoreReadFailed => "https://pgkronika.dev/problems/store-read-failed",
            Self::InternalError => "https://pgkronika.dev/problems/internal-error",
        }
    }
}

/// Bounded query-parameter identifiers allowed in Problem params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum QueryParameter {
    Query,
    Source,
    From,
    To,
    Window,
    Step,
    Threshold,
    EpsRel,
    Epsilon,
    MaxClusterSpan,
    Section,
    Names,
    Limit,
    Cursor,
}

impl Serialize for QueryParameter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl QueryParameter {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 14] = [
        Self::Query,
        Self::Source,
        Self::From,
        Self::To,
        Self::Window,
        Self::Step,
        Self::Threshold,
        Self::EpsRel,
        Self::Epsilon,
        Self::MaxClusterSpan,
        Self::Section,
        Self::Names,
        Self::Limit,
        Self::Cursor,
    ];

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
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Source => "source",
            Self::From => "from",
            Self::To => "to",
            Self::Window => "window",
            Self::Step => "step",
            Self::Threshold => "threshold",
            Self::EpsRel => "eps_rel",
            Self::Epsilon => "epsilon",
            Self::MaxClusterSpan => "max_cluster_span",
            Self::Section => "section",
            Self::Names => "names",
            Self::Limit => "limit",
            Self::Cursor => "cursor",
        }
    }
}

/// Expected machine types for invalid query parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExpectedValue {
    UrlEncodedQuery,
    Uint64,
    Int64,
    PositiveDuration,
    NonNegativeFiniteNumber,
    NonNegativeInteger,
    SectionList,
}

impl ExpectedValue {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 7] = [
        Self::UrlEncodedQuery,
        Self::Uint64,
        Self::Int64,
        Self::PositiveDuration,
        Self::NonNegativeFiniteNumber,
        Self::NonNegativeInteger,
        Self::SectionList,
    ];
}

/// Cross-parameter constraints enforced before a query runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueryConstraint {
    FromBeforeTo,
    WindowWithinInterval,
    EpsilonNotGreaterThanMaxClusterSpan,
    MaxClusterSpanWithinInterval,
    FiniteScan,
}

impl QueryConstraint {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [
        Self::FromBeforeTo,
        Self::WindowWithinInterval,
        Self::EpsilonNotGreaterThanMaxClusterSpan,
        Self::MaxClusterSpanWithinInterval,
        Self::FiniteScan,
    ];
}

/// Resource dimensions used by `query_limit_exceeded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LimitResource {
    QueryBytes,
    QueryParameters,
    QuerySpanUs,
    WindowPositions,
    Rows,
    Cells,
    Bytes,
    Units,
    Sections,
    IdentityBytes,
    SeriesPoints,
    Episodes,
    Clusters,
    IncidentKeyBytes,
    TotalIncidentKeyBytes,
}

impl LimitResource {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 15] = [
        Self::QueryBytes,
        Self::QueryParameters,
        Self::QuerySpanUs,
        Self::WindowPositions,
        Self::Rows,
        Self::Cells,
        Self::Bytes,
        Self::Units,
        Self::Sections,
        Self::IdentityBytes,
        Self::SeriesPoints,
        Self::Episodes,
        Self::Clusters,
        Self::IncidentKeyBytes,
        Self::TotalIncidentKeyBytes,
    ];
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

#[derive(Debug, Serialize)]
struct ParameterParams {
    parameter: QueryParameter,
}

#[derive(Debug, Serialize)]
struct InvalidParameterParams {
    parameter: QueryParameter,
    expected: ExpectedValue,
}

#[derive(Debug, Serialize)]
struct UnknownParameterParams {
    parameter: String,
}

#[derive(Debug, Serialize)]
struct ConstraintParams {
    constraint: QueryConstraint,
}

#[derive(Debug, Serialize)]
struct SectionParams {
    section: String,
}

#[derive(Debug, Serialize)]
struct LimitParams {
    resource: LimitResource,
    limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CapacityParams {
    retry_after_seconds: u64,
}

#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
pub(crate) struct ApiProblem {
    #[serde(rename = "type")]
    type_uri: &'static str,
    status: u16,
    code: ProblemCode,
    params: ProblemParams,
    instance: String,
    #[serde(skip)]
    request_id: String,
}

impl ApiProblem {
    fn new(code: ProblemCode, params: ProblemParams) -> Self {
        let request_id = next_request_id();
        Self {
            type_uri: code.type_uri(),
            status: code.status().as_u16(),
            code,
            params,
            instance: format!("urn:pgkronika:request:{request_id}"),
            request_id,
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self::empty(ProblemCode::Unauthorized)
    }

    pub(crate) fn route_not_found() -> Self {
        Self::empty(ProblemCode::RouteNotFound)
    }

    pub(crate) fn method_not_allowed() -> Self {
        Self::empty(ProblemCode::MethodNotAllowed)
    }

    pub(crate) fn missing_query_parameter(parameter: QueryParameter) -> Self {
        Self::new(
            ProblemCode::MissingQueryParameter,
            ProblemParams::Parameter(ParameterParams { parameter }),
        )
    }

    pub(crate) fn invalid_query_parameter(
        parameter: QueryParameter,
        expected: ExpectedValue,
    ) -> Self {
        Self::new(
            ProblemCode::InvalidQueryParameter,
            ProblemParams::InvalidParameter(InvalidParameterParams {
                parameter,
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
    fn into_response(self) -> Response {
        let status = self.code.status();
        let code = self.code;
        let request_id = self.request_id.clone();
        tracing::info!(
            event = "api_problem_response",
            request_id = request_id.as_str(),
            code = code.as_str(),
            status = status.as_u16(),
            "API problem response"
        );
        let mut response = (status, Json(self)).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_MEDIA_TYPE),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let request_id = HeaderValue::from_str(&request_id)
            .unwrap_or_else(|_error| HeaderValue::from_static("invalid"));
        headers.insert(REQUEST_ID_HEADER, request_id);
        match code {
            ProblemCode::Unauthorized => {
                headers.insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Basic realm=\"pg_kronika-web\""),
                );
            }
            ProblemCode::MethodNotAllowed => {
                headers.insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            }
            ProblemCode::AnalyticCapacityUnavailable => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
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
    static PROCESS_NONCE: LazyLock<[u8; 32]> = LazyLock::new(process_nonce);
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(*PROCESS_NONCE);
    hasher.update(sequence.to_be_bytes());
    hasher.update(now.to_be_bytes());
    let digest = hasher.finalize();
    let mut request_id = String::with_capacity(32);
    for byte in &digest[..16] {
        let _ = write!(request_id, "{byte:02x}");
    }
    request_id
}

fn process_nonce() -> [u8; 32] {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(started.to_be_bytes());
    hasher.update(std::process::id().to_be_bytes());
    hasher.finalize().into()
}
