use super::*;

/// A valid `Authorization: Basic` header for `user:pass`.
fn basic_header(user: &str, pass: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
    format!("Basic {encoded}")
}

/// Drive one request over an empty snapshot with the given auth setting and
/// optional `Authorization` header; return the response status.
async fn auth_status(auth: Option<AuthConfig>, uri: &str, header: Option<&str>) -> StatusCode {
    let (_dir, snapshot) = empty_snapshot();
    let mut builder = Request::builder().uri(uri);
    if let Some(value) = header {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    app(
        AppState::new(snapshot).expect("state"),
        auth,
        test_metrics_handle(),
    )
    .oneshot(builder.body(Body::empty()).expect("build request"))
    .await
    .expect("route request")
    .status()
}

#[tokio::test]
async fn auth_disabled_leaves_v1_open() {
    assert_eq!(
        auth_status(None, "/v1/version", None).await,
        StatusCode::OK,
        "with auth off, /v1 needs no credentials"
    );
}

#[tokio::test]
async fn auth_enabled_rejects_v1_without_credentials() {
    for uri in [
        "/v1/version",
        "/v1/incidents?source=7&from=0&to=600000000&window=1m",
    ] {
        assert_eq!(
            auth_status(Some(AuthConfig::new("u", "p")), uri, None).await,
            StatusCode::UNAUTHORIZED,
            "with auth on, {uri} without a header is 401",
        );
    }
}

#[tokio::test]
async fn auth_enabled_accepts_correct_credentials() {
    let header = basic_header("u", "p");
    assert_eq!(
        auth_status(
            Some(AuthConfig::new("u", "p")),
            "/v1/version",
            Some(header.as_str())
        )
        .await,
        StatusCode::OK,
        "the right credential opens /v1"
    );
}

#[tokio::test]
async fn auth_enabled_rejects_wrong_password() {
    let header = basic_header("u", "wrong");
    assert_eq!(
        auth_status(
            Some(AuthConfig::new("u", "p")),
            "/v1/version",
            Some(header.as_str())
        )
        .await,
        StatusCode::UNAUTHORIZED,
        "a wrong password is 401"
    );
}

#[tokio::test]
async fn auth_enabled_keeps_probes_and_metrics_public() {
    for uri in ["/healthz", "/readyz", "/metrics"] {
        assert_eq!(
            auth_status(Some(AuthConfig::new("u", "p")), uri, None).await,
            StatusCode::OK,
            "{uri} stays public under auth"
        );
    }
}

/// Drive one request over an empty snapshot; return status, content type and
/// the raw body (static responses are HTML, not JSON).
async fn raw_request(
    auth: Option<AuthConfig>,
    uri: &str,
    header: Option<&str>,
) -> (StatusCode, String, Vec<u8>) {
    let (_dir, snapshot) = empty_snapshot();
    let mut builder = Request::builder().uri(uri);
    if let Some(value) = header {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    let response = app(
        AppState::new(snapshot).expect("state"),
        auth,
        test_metrics_handle(),
    )
    .oneshot(builder.body(Body::empty()).expect("build request"))
    .await
    .expect("route request");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes()
        .to_vec();
    (status, content_type, body)
}

#[tokio::test]
async fn static_serves_index_html() {
    let (status, content_type, body) = raw_request(None, "/index.html", None).await;
    assert_eq!(status, StatusCode::OK, "/index.html is served");
    assert!(
        content_type.starts_with("text/html"),
        "index.html is HTML, got {content_type}"
    );
    assert!(!body.is_empty(), "the shell has a body");
}

#[tokio::test]
async fn static_root_serves_spa_shell() {
    let (status, content_type, _body) = raw_request(None, "/", None).await;
    assert_eq!(status, StatusCode::OK, "/ falls back to the SPA shell");
    assert!(content_type.starts_with("text/html"), "the shell is HTML");
}

#[tokio::test]
async fn static_unknown_ui_path_serves_spa_shell() {
    let (status, content_type, _body) = raw_request(None, "/dashboard/live", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an unknown UI path falls back to the shell"
    );
    assert!(content_type.starts_with("text/html"), "the shell is HTML");
}

#[tokio::test]
async fn static_unknown_v1_path_is_problem_details() {
    let (status, content_type, body) = raw_request(None, "/v1/does-not-exist", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown /v1 path is a 404"
    );
    assert!(
        content_type.starts_with("application/problem+json"),
        "the 404 is Problem Details, not the HTML shell"
    );
    let value: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    assert_problem(&value, status, "route_not_found", serde_json::json!({}));
}

#[tokio::test]
async fn auth_enabled_protects_static() {
    // Security-critical: with auth on, the UI is behind it too, not just
    // /v1. This fails if the auth layer misses the static fallback.
    let (status, _ct, _body) =
        raw_request(Some(AuthConfig::new("u", "p")), "/index.html", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the static UI is behind auth when it is enabled"
    );
}

#[tokio::test]
async fn auth_enabled_allows_static_with_credentials() {
    let header = basic_header("u", "p");
    let (status, content_type, _body) = raw_request(
        Some(AuthConfig::new("u", "p")),
        "/index.html",
        Some(header.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the right credential opens the UI");
    assert!(content_type.starts_with("text/html"), "the shell is HTML");
}
