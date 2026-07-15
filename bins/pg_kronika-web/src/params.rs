use axum::Json;
use axum::http::StatusCode;
use kronika_reader::{Cursor, QueryError};
use serde_json::{Value, json};

/// Rows returned when a request omits `limit`.
pub(crate) const DEFAULT_LIMIT: usize = 1_000;

/// Hard ceiling on `limit`, applied even when a request asks for more.
pub(crate) const MAX_LIMIT: usize = 10_000;

/// A `400 Bad Request` with a `{ "error", "detail" }` JSON body.
pub(crate) fn bad_request(detail: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "bad_request", "detail": detail })),
    )
}

/// Parse a required unsigned query parameter, or a `400` with a JSON body.
pub(crate) fn parse_u64(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<u64, (StatusCode, Json<Value>)> {
    params
        .get(key)
        .ok_or_else(|| bad_request(&format!("missing query parameter `{key}`")))?
        .parse()
        .map_err(|_err| bad_request(&format!("`{key}` must be an unsigned integer")))
}

/// Parse a required signed query parameter, or a `400` with a JSON body.
pub(crate) fn parse_i64(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<i64, (StatusCode, Json<Value>)> {
    params
        .get(key)
        .ok_or_else(|| bad_request(&format!("missing query parameter `{key}`")))?
        .parse()
        .map_err(|_err| bad_request(&format!("`{key}` must be an integer")))
}

/// Parse the optional `limit`: absent → [`DEFAULT_LIMIT`], present → clamped to
/// [`MAX_LIMIT`], unparseable → `400`.
pub(crate) fn parse_limit(
    params: &std::collections::HashMap<String, String>,
) -> Result<usize, (StatusCode, Json<Value>)> {
    parse_limit_default(params, DEFAULT_LIMIT)
}

/// Parse an optional duration parameter into microseconds: `250ms`, `90s`,
/// `15m`, `2h`, or a bare number of seconds. Absent → `default_us`; zero,
/// negative, or overflowing → `400`.
pub(crate) fn parse_duration_us(
    params: &std::collections::HashMap<String, String>,
    key: &str,
    default_us: i64,
) -> Result<i64, (StatusCode, Json<Value>)> {
    params.get(key).map_or(Ok(default_us), |raw| {
        duration_us(raw).ok_or_else(|| {
            bad_request(&format!(
                "`{key}` must be a positive duration like `250ms`, `90s`, `15m`, or `2h`"
            ))
        })
    })
}

/// A duration literal as microseconds, `None` when malformed or non-positive.
fn duration_us(raw: &str) -> Option<i64> {
    let suffixed =
        |suffix: &str, scale: i64| raw.strip_suffix(suffix).map(|digits| (digits, scale));
    // `ms` before `s`: a millisecond literal also ends in `s`.
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

/// Parse an optional float parameter; absent → `default`. Non-finite and
/// negative values are rejected: every caller's knob (a sigma threshold, a
/// relative floor) is meaningless below zero.
pub(crate) fn parse_f64_non_negative(
    params: &std::collections::HashMap<String, String>,
    key: &str,
    default: f64,
) -> Result<f64, (StatusCode, Json<Value>)> {
    params
        .get(key)
        .map_or(Ok(default), |raw| match raw.parse::<f64>() {
            Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
            _ => Err(bad_request(&format!(
                "`{key}` must be a non-negative finite number"
            ))),
        })
}

/// Parse the optional `limit` with a caller-chosen default, clamped to
/// [`MAX_LIMIT`]; unparseable → `400`.
pub(crate) fn parse_limit_default(
    params: &std::collections::HashMap<String, String>,
    default: usize,
) -> Result<usize, (StatusCode, Json<Value>)> {
    params.get("limit").map_or(Ok(default), |raw| {
        raw.parse::<usize>()
            .map(|limit| limit.min(MAX_LIMIT))
            .map_err(|_err| bad_request("`limit` must be a non-negative integer"))
    })
}

/// Parse the optional resume `cursor`: absent → `None`, present → decoded, or a
/// `400` when it is malformed or belongs to another source.
pub(crate) fn parse_cursor(
    params: &std::collections::HashMap<String, String>,
) -> Result<Option<Cursor>, (StatusCode, Json<Value>)> {
    params.get("cursor").map_or(Ok(None), |raw| {
        Cursor::decode(raw)
            .map(Some)
            .map_err(|err| query_error_response(&err))
    })
}

/// Map a reader [`QueryError`] to an HTTP status and a `{ error, detail }` body.
pub(crate) fn query_error_response(err: &QueryError) -> (StatusCode, Json<Value>) {
    let (status, code, detail) = match err {
        QueryError::UnknownSection(name) => (
            StatusCode::NOT_FOUND,
            "unknown_section",
            format!("no section named `{name}`"),
        ),
        QueryError::BadCursor(message) => (StatusCode::BAD_REQUEST, "bad_cursor", message.clone()),
        QueryError::Read(read) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "read_error",
            read.to_string(),
        ),
    };
    (status, Json(json!({ "error": code, "detail": detail })))
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LIMIT, MAX_LIMIT, duration_us, parse_f64_non_negative, parse_limit};

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
        let empty = std::collections::HashMap::new();
        assert_eq!(
            parse_f64_non_negative(&empty, "threshold", 3.5).ok(),
            Some(3.5)
        );

        let ok = std::collections::HashMap::from([("threshold".to_owned(), "2.0".to_owned())]);
        assert_eq!(
            parse_f64_non_negative(&ok, "threshold", 3.5).ok(),
            Some(2.0)
        );

        for bad in ["-1", "NaN", "inf", "abc"] {
            let params =
                std::collections::HashMap::from([("threshold".to_owned(), bad.to_owned())]);
            assert!(
                parse_f64_non_negative(&params, "threshold", 3.5).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn parse_limit_defaults_caps_and_rejects() {
        let empty = std::collections::HashMap::new();
        assert_eq!(
            parse_limit(&empty).ok(),
            Some(DEFAULT_LIMIT),
            "an absent limit uses the default"
        );

        let explicit = std::collections::HashMap::from([("limit".to_owned(), "50".to_owned())]);
        assert_eq!(
            parse_limit(&explicit).ok(),
            Some(50),
            "an explicit limit is honored"
        );

        let huge = std::collections::HashMap::from([("limit".to_owned(), "99999".to_owned())]);
        assert_eq!(
            parse_limit(&huge).ok(),
            Some(MAX_LIMIT),
            "a limit above the ceiling is clamped"
        );

        let bad = std::collections::HashMap::from([("limit".to_owned(), "-1".to_owned())]);
        assert!(
            parse_limit(&bad).is_err(),
            "a non-numeric limit is rejected"
        );
    }
}
