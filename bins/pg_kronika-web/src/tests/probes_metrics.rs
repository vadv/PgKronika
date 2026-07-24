use super::*;

/// Drive one probe request and return `(status, body)`.
async fn probe(state: AppState, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route request");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).expect("body is valid JSON");
    (status, value)
}

#[tokio::test]
async fn healthz_returns_200_ok() {
    let (_dir, snapshot) = empty_snapshot();
    let state = AppState::new(snapshot).expect("state");
    let (status, body) = probe(state, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "ok"}));
}

#[tokio::test]
async fn readyz_fresh_snapshot_returns_200_ready() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let (_dir, snapshot) = empty_snapshot();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // last_refresh = now; stale_after = 10s => age == 0, not stale
    let state =
        AppState::with_readiness(snapshot, now, std::time::Duration::from_secs(10)).expect("state");
    let (status, body) = probe(state, "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ready"], serde_json::json!(true));
}

#[tokio::test]
async fn readyz_stale_snapshot_returns_503_not_ready() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let (_dir, snapshot) = empty_snapshot();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // last_refresh = now - 3600; stale_after = 10s => age = 3600, stale
    let state = AppState::with_readiness(
        snapshot,
        now.saturating_sub(3600),
        std::time::Duration::from_secs(10),
    )
    .expect("state");
    let (status, body) = probe(state, "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["ready"], serde_json::json!(false));
}

#[tokio::test]
async fn metrics_endpoint_lists_metric_names_after_traffic() {
    let handle = test_metrics_handle();

    // track_metrics registers the request counter lazily — on the first
    // increment, which runs after a handler returns. Warm it with one
    // request so the scrape sees it without leaning on sibling tests.
    let (_warm_dir, warm_snapshot) = empty_snapshot();
    app(
        AppState::new(warm_snapshot).expect("state"),
        None,
        handle.clone(),
    )
    .oneshot(
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .expect("build request"),
    )
    .await
    .expect("route warmup request");

    let (_dir, snapshot) = empty_snapshot();
    let response = app(AppState::new(snapshot).expect("state"), None, handle)
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route request");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/metrics must return 200"
    );

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let body = String::from_utf8_lossy(&bytes);

    assert!(
        body.contains("kronika_web_requests_total"),
        "/metrics body must contain kronika_web_requests_total"
    );
    assert!(
        body.lines()
            .any(|line| line == "kronika_web_data_age_seconds NaN"),
        "an empty store must expose data age as unavailable"
    );
    assert!(
        body.contains("kronika_web_reader_age_seconds"),
        "/metrics body must contain kronika_web_reader_age_seconds"
    );
    assert!(
        body.contains("kronika_web_units_total"),
        "/metrics body must contain kronika_web_units_total"
    );
}
