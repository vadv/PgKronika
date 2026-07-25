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

#[test]
fn selected_segment_policy_exports_one_static_rejection_series_and_its_effective_limit() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);
    write_bgwriter_segment(dir.path(), "two.pgm", 7, 0, 1);

    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            let snapshot =
                kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
            let mut config = OverviewConfig::new(
                dir.path().join(".overview-cache"),
                dir.path().as_os_str().as_encoded_bytes().to_vec(),
            );
            config.max_selected_segments = 1;
            let state = AppState::with_overview_config(
                snapshot,
                0,
                std::time::Duration::from_secs(10),
                config,
            )
            .expect("state");
            let response = app(state, None, handle.clone())
                .oneshot(
                    Request::builder()
                        .uri("/v1/timeline/overview?source=7&from=0&to=2")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("route");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        });
    });

    let rendered = handle.render();
    assert!(
        rendered
            .lines()
            .any(|line| line == "kronika_web_timeline_selected_segments_limit 1"),
        "{rendered}"
    );
    let rejection_lines = rendered
        .lines()
        .filter(|line| line.starts_with("kronika_web_timeline_query_limit_rejections_total{"))
        .collect::<Vec<_>>();
    assert_eq!(
        rejection_lines,
        ["kronika_web_timeline_query_limit_rejections_total{resource=\"selected_segments\"} 1"]
    );
    for forbidden in [
        "kronika_web_timeline_response_cache_misses_total",
        "kronika_web_timeline_singleflight_leaders_total",
        "kronika_web_timeline_capacity_rejections_total",
        "kronika_web_overview_cold_work_rejections_total",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "shape admission must precede cache, response flight, analytic and cold work:\n{rendered}"
        );
    }
}
