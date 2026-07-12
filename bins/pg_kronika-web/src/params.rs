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
    params.get("limit").map_or(Ok(DEFAULT_LIMIT), |raw| {
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
    use super::{DEFAULT_LIMIT, MAX_LIMIT, parse_limit};

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
