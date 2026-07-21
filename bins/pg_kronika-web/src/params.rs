//! Bounded query decoding and mapping of reader failures to API problems.

use std::collections::HashMap;

use kronika_reader::{Cursor, QueryError};

use crate::problem::{ApiProblem, ExpectedValue, LimitResource, QueryParameter, count_u64};

/// Rows returned when a request omits `limit`.
pub(crate) const DEFAULT_LIMIT: usize = 1_000;

/// Hard ceiling on `limit`, applied even when a request asks for more.
pub(crate) const MAX_LIMIT: usize = 10_000;

/// Maximum raw query length decoded into owned parameter pairs.
pub(crate) const MAX_QUERY_BYTES: usize = 8_192;

/// Maximum query pairs decoded before uniqueness and route checks.
pub(crate) const MAX_QUERY_PARAMETERS: usize = 32;

/// A validated query whose keys are known, allowed for the route, and unique.
#[derive(Debug)]
pub(crate) struct QueryParams {
    values: HashMap<QueryParameter, String>,
}

impl QueryParams {
    /// Decode and validate one raw query against the route allowlist.
    pub(crate) fn parse(raw: Option<&str>, allowed: &[QueryParameter]) -> Result<Self, ApiProblem> {
        let raw = raw.unwrap_or_default();
        if raw.len() > MAX_QUERY_BYTES {
            return Err(ApiProblem::query_limit_exceeded(
                LimitResource::QueryBytes,
                count_u64(MAX_QUERY_BYTES),
                Some(count_u64(raw.len())),
            ));
        }
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw).map_err(|_error| {
            ApiProblem::invalid_query_parameter(
                QueryParameter::Query,
                ExpectedValue::UrlEncodedQuery,
            )
        })?;
        if pairs.len() > MAX_QUERY_PARAMETERS {
            return Err(ApiProblem::query_limit_exceeded(
                LimitResource::QueryParameters,
                count_u64(MAX_QUERY_PARAMETERS),
                Some(count_u64(pairs.len())),
            ));
        }

        let mut values = HashMap::with_capacity(pairs.len());
        for (name, value) in pairs {
            let Some(parameter) = QueryParameter::from_query_name(&name) else {
                return Err(ApiProblem::unknown_query_parameter(&name));
            };
            if !allowed.contains(&parameter) {
                return Err(ApiProblem::unknown_query_parameter(&name));
            }
            if values.insert(parameter, value).is_some() {
                return Err(ApiProblem::duplicate_query_parameter(parameter));
            }
        }
        Ok(Self { values })
    }

    pub(crate) fn get(&self, parameter: QueryParameter) -> Option<&str> {
        self.values.get(&parameter).map(String::as_str)
    }
}

/// Parse a required unsigned query parameter.
pub(crate) fn parse_u64(
    params: &QueryParams,
    parameter: QueryParameter,
) -> Result<u64, ApiProblem> {
    params
        .get(parameter)
        .ok_or_else(|| ApiProblem::missing_query_parameter(parameter))?
        .parse()
        .map_err(|_error| ApiProblem::invalid_query_parameter(parameter, ExpectedValue::Uint64))
}

/// Parse a required signed query parameter.
pub(crate) fn parse_i64(
    params: &QueryParams,
    parameter: QueryParameter,
) -> Result<i64, ApiProblem> {
    params
        .get(parameter)
        .ok_or_else(|| ApiProblem::missing_query_parameter(parameter))?
        .parse()
        .map_err(|_error| ApiProblem::invalid_query_parameter(parameter, ExpectedValue::Int64))
}

/// Parse the optional `limit`: absent → [`DEFAULT_LIMIT`], present → clamped to
/// [`MAX_LIMIT`], unparseable → `400`.
pub(crate) fn parse_limit(params: &QueryParams) -> Result<usize, ApiProblem> {
    parse_limit_default(params, DEFAULT_LIMIT)
}

/// Parse an optional duration parameter into microseconds.
pub(crate) fn parse_duration_us(
    params: &QueryParams,
    parameter: QueryParameter,
    default_us: i64,
) -> Result<i64, ApiProblem> {
    params.get(parameter).map_or(Ok(default_us), |raw| {
        duration_us(raw).ok_or_else(|| {
            ApiProblem::invalid_query_parameter(parameter, ExpectedValue::PositiveDuration)
        })
    })
}

/// A duration literal as microseconds, `None` when malformed or non-positive.
fn duration_us(raw: &str) -> Option<i64> {
    let suffixed =
        |suffix: &str, scale: i64| raw.strip_suffix(suffix).map(|digits| (digits, scale));
    let (digits, scale) = suffixed("ms", 1_000)
        .or_else(|| suffixed("s", 1_000_000))
        .or_else(|| suffixed("m", 60 * 1_000_000))
        .or_else(|| suffixed("h", 3_600 * 1_000_000))
        .unwrap_or((raw, 1_000_000));
    let seconds: i64 = digits.parse().ok()?;
    if seconds <= 0 {
        return None;
    }
    seconds.checked_mul(scale)
}

/// Parse an optional non-negative finite float parameter.
pub(crate) fn parse_f64_non_negative(
    params: &QueryParams,
    parameter: QueryParameter,
    default: f64,
) -> Result<f64, ApiProblem> {
    params
        .get(parameter)
        .map_or(Ok(default), |raw| match raw.parse::<f64>() {
            Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
            _ => Err(ApiProblem::invalid_query_parameter(
                parameter,
                ExpectedValue::NonNegativeFiniteNumber,
            )),
        })
}

/// Parse the optional `limit` with a caller-chosen default and hard ceiling.
pub(crate) fn parse_limit_default(
    params: &QueryParams,
    default: usize,
) -> Result<usize, ApiProblem> {
    params
        .get(QueryParameter::Limit)
        .map_or(Ok(default), |raw| {
            raw.parse::<usize>()
                .map(|limit| limit.min(MAX_LIMIT))
                .map_err(|_error| {
                    ApiProblem::invalid_query_parameter(
                        QueryParameter::Limit,
                        ExpectedValue::NonNegativeInteger,
                    )
                })
        })
}

/// Parse an optional opaque resume cursor.
pub(crate) fn parse_cursor(params: &QueryParams) -> Result<Option<Cursor>, ApiProblem> {
    params.get(QueryParameter::Cursor).map_or(Ok(None), |raw| {
        Cursor::decode(raw)
            .map(Some)
            .map_err(|_error| ApiProblem::invalid_cursor())
    })
}

/// Map a reader failure without exposing paths, values, or error chains.
pub(crate) fn query_error_response(error: &QueryError) -> ApiProblem {
    match error {
        QueryError::UnknownSection(name) => ApiProblem::unknown_section(name),
        QueryError::BadCursor(_) => ApiProblem::invalid_cursor(),
        QueryError::Read(read) => {
            let problem = ApiProblem::store_read_failed();
            tracing::error!(
                event = "api_store_read_failed",
                request_id = problem.request_id(),
                error = %read,
                "store query failed"
            );
            problem
        }
        QueryError::ResultTooLarge { max_cells } => {
            ApiProblem::query_limit_exceeded(LimitResource::Cells, count_u64(*max_cells), None)
        }
        QueryError::MaterializedBytesTooLarge { max_bytes } => {
            ApiProblem::query_limit_exceeded(LimitResource::Bytes, count_u64(*max_bytes), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_LIMIT, MAX_LIMIT, MAX_QUERY_BYTES, MAX_QUERY_PARAMETERS, QueryParams, duration_us,
        parse_f64_non_negative, parse_limit,
    };
    use crate::problem::{ProblemCode, QueryParameter};

    fn params(raw: &str, allowed: &[QueryParameter]) -> QueryParams {
        QueryParams::parse(Some(raw), allowed).expect("valid query")
    }

    #[test]
    fn durations_parse_with_suffixes_and_reject_degenerate_values() {
        assert_eq!(duration_us("250ms"), Some(250 * 1_000));
        assert_eq!(duration_us("90s"), Some(90 * 1_000_000));
        assert_eq!(duration_us("15m"), Some(15 * 60 * 1_000_000));
        assert_eq!(duration_us("2h"), Some(2 * 3_600 * 1_000_000));
        assert_eq!(duration_us("45"), Some(45 * 1_000_000), "bare seconds");
        for bad in [
            "0s",
            "0ms",
            "-5m",
            "",
            "m",
            "ms",
            "1.5h",
            "1d",
            "999999999999999999h",
        ] {
            assert_eq!(duration_us(bad), None, "{bad:?} must be rejected");
        }
    }

    #[test]
    fn floats_default_when_absent_and_reject_non_finite_or_negative() {
        let empty = params("", &[QueryParameter::Threshold]);
        assert_eq!(
            parse_f64_non_negative(&empty, QueryParameter::Threshold, 3.5).ok(),
            Some(3.5)
        );

        let explicit = params("threshold=2.0", &[QueryParameter::Threshold]);
        assert_eq!(
            parse_f64_non_negative(&explicit, QueryParameter::Threshold, 3.5).ok(),
            Some(2.0)
        );

        for bad in ["-1", "NaN", "inf", "abc"] {
            let query = format!("threshold={bad}");
            let params = params(&query, &[QueryParameter::Threshold]);
            assert!(
                parse_f64_non_negative(&params, QueryParameter::Threshold, 3.5).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn parse_limit_defaults_caps_and_rejects() {
        let empty = params("", &[QueryParameter::Limit]);
        assert_eq!(
            parse_limit(&empty).ok(),
            Some(DEFAULT_LIMIT),
            "an absent limit uses the default"
        );

        let explicit = params("limit=50", &[QueryParameter::Limit]);
        assert_eq!(
            parse_limit(&explicit).ok(),
            Some(50),
            "an explicit limit is honored"
        );

        let huge = params("limit=99999", &[QueryParameter::Limit]);
        assert_eq!(
            parse_limit(&huge).ok(),
            Some(MAX_LIMIT),
            "a limit above the ceiling is clamped"
        );

        let bad = params("limit=-1", &[QueryParameter::Limit]);
        assert!(
            parse_limit(&bad).is_err(),
            "a non-numeric limit is rejected"
        );
    }

    #[test]
    fn raw_query_bounds_are_enforced_before_decoding() {
        let oversized = "x".repeat(MAX_QUERY_BYTES + 1);
        let byte_error = QueryParams::parse(Some(&oversized), &[]).expect_err("byte ceiling");
        assert_eq!(byte_error.code(), ProblemCode::QueryLimitExceeded);

        let too_many = std::iter::repeat_n("source=1", MAX_QUERY_PARAMETERS + 1)
            .collect::<Vec<_>>()
            .join("&");
        let pair_error = QueryParams::parse(Some(&too_many), &[QueryParameter::Source])
            .expect_err("pair ceiling");
        assert_eq!(pair_error.code(), ProblemCode::QueryLimitExceeded);
    }
}
