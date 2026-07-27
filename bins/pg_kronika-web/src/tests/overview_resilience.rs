use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_reader::{
    PersistError, PersistMode, PersistModeSnapshot, PersistenceProbeOutcome, PgmUnit,
};
use kronika_registry::pg_log::PgLogErrorV1;
use kronika_registry::{Section, Ts};
use metrics_exporter_prometheus::PrometheusBuilder;
use tower::ServiceExt as _;

use crate::overview::resilience::{record_persist_snapshot, record_probe_metrics};
use crate::{AppState, OverviewConfig, app};

use super::{capture_json, test_metrics_handle, write_bgwriter_segment};

fn write_overview_event_segment(dir: &std::path::Path) -> std::path::PathBuf {
    let body = PgLogErrorV1::encode(&[PgLogErrorV1 {
        ts: Ts(1),
        severity: 2,
        category: 9,
        sqlstate: None,
        pattern: None,
        count: 1,
        sample: None,
        detail: None,
        hint: None,
        context: None,
        statement: None,
        database: None,
        username: None,
        dict_dropped_fields: 0,
    }])
    .expect("encode overview event");
    let bytes = build_part(
        &[SectionInput {
            type_id: 1_022_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: 0,
            max_ts: 1,
            source_id: 7,
        },
    );
    crate::test_layout::write_named_pgm(dir, "one.pgm", &bytes)
}

fn corrupt_first_section_body(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("read segment");
    let body_offset = {
        let unit = PgmUnit::open(bytes.as_slice()).expect("open segment catalog");
        let entry = unit.catalog().entries.first().expect("section entry");
        assert_ne!(entry.len, 0, "fixture section body is non-empty");
        usize::try_from(entry.offset).expect("section offset fits usize")
    };
    bytes[body_offset] ^= 0xff;
    std::fs::write(path, bytes).expect("corrupt only the source section body");
}

#[tokio::test]
async fn a_metadata_only_fallback_keeps_the_snapshot_that_authorized_its_descriptors() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);
    let initial_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("initial snapshot");
    let state = AppState::with_overview_config(
        initial_snapshot,
        0,
        Duration::from_secs(10),
        &OverviewConfig::new(),
    )
    .expect("state");

    write_bgwriter_segment(dir.path(), "two.pgm", 7, 10, 11);
    let fresh_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("fresh snapshot");
    assert_eq!(fresh_snapshot.units().len(), 2);
    state.publish_snapshot_with_last_timeline(fresh_snapshot);

    assert_eq!(state.snapshot().units().len(), 2);
    let (timeline_snapshot, descriptors) = state.overview_request_view();
    assert_eq!(timeline_snapshot.units().len(), 1);
    assert_eq!(descriptors.entries().len(), 1);

    let response = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_restart_uses_the_durable_fact_before_reading_a_now_corrupt_section_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let segment_path = write_overview_event_segment(dir.path());

    let first_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("first snapshot");
    let first_state = AppState::with_overview_config(
        first_snapshot,
        0,
        Duration::from_secs(10),
        &OverviewConfig::new(),
    )
    .expect("first state");
    let first = app(first_state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("first request"),
        )
        .await
        .expect("first route");
    assert_eq!(first.status(), StatusCode::OK);

    corrupt_first_section_body(&segment_path);

    let restarted_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("restart snapshot");
    let restarted_state = AppState::with_overview_config(
        restarted_snapshot,
        0,
        Duration::from_secs(10),
        &OverviewConfig::new(),
    )
    .expect("restarted state");
    let restarted = app(restarted_state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("restart request"),
        )
        .await
        .expect("restart route");
    assert_eq!(
        restarted.status(),
        StatusCode::OK,
        "a durable exact-key hit must not decode the changed source body"
    );
}

#[tokio::test]
async fn a_source_read_failure_returns_an_uncached_explicit_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let segment_path = write_overview_event_segment(dir.path());
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("snapshot");
    let state = AppState::with_overview_config(
        snapshot,
        0,
        Duration::from_secs(10),
        &OverviewConfig::new(),
    )
    .expect("state");
    corrupt_first_section_body(&segment_path);

    let response = app(state.clone(), None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("route");
    let response = capture_json(response).await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "partial view remains queryable"
    );
    assert_eq!(response.body["meta"]["source_status"], "gap");
    assert_eq!(
        response.body["meta"]["source_freshness"][0]["source_status"],
        "gap"
    );
    assert_eq!(
        response.body["meta"]["loss"][0]["known_gaps"],
        serde_json::json!([{ "from_us": 0, "to_us": 2 }])
    );
    assert_eq!(
        state.response_cache.len(),
        0,
        "a partial fact-set identity must never populate the complete response cache key"
    );
}

#[tokio::test]
async fn scheduled_source_scrub_prevents_a_durable_fact_from_masking_damage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let segment_path = write_overview_event_segment(dir.path());
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("snapshot");
    let mut config = OverviewConfig::new();
    config.source_scrub_interval = Duration::from_millis(10);
    let state = AppState::with_overview_config(snapshot, 0, Duration::from_secs(10), &config)
        .expect("state");

    let complete = app(state.clone(), None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/overview?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("complete request"),
        )
        .await
        .expect("complete route");
    let complete = capture_json(complete).await;
    assert_eq!(complete.status, StatusCode::OK);
    assert_eq!(
        complete.body["meta"]["source_status"],
        "complete_for_contract"
    );
    let cached_complete_responses = state.response_cache.len();
    assert_eq!(cached_complete_responses, 1);

    corrupt_first_section_body(&segment_path);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut snapshot = (*state.snapshot()).clone();
    let delta = snapshot
        .refresh_incremental_delta()
        .expect("same-catalog source refresh");
    state
        .republish_store_view(snapshot, &delta)
        .expect("scrubbed publication");
    let plan = state
        .select_overview(
            state.overview_view(),
            &[7],
            kronika_analytics::overview::CoverageSpan::new(0, 2).expect("range"),
        )
        .expect("damaged source selection");
    assert!(
        plan.sealed_gap(),
        "scrub damage becomes an unavailable descriptor"
    );
    let mut repeated_snapshot = (*state.snapshot()).clone();
    let repeated_delta = repeated_snapshot
        .refresh_incremental_delta()
        .expect("repeated invalid source refresh");
    assert!(repeated_delta.sealed_removed.is_empty());
    state
        .republish_store_view(repeated_snapshot, &repeated_delta)
        .expect("repeated damaged publication");
    let repeated_plan = state
        .select_overview(
            state.overview_view(),
            &[7],
            kronika_analytics::overview::CoverageSpan::new(0, 2).expect("range"),
        )
        .expect("persistently damaged source selection");
    assert!(
        repeated_plan.sealed_gap(),
        "the unavailable descriptor remains while the invalid-PGM warning persists"
    );

    let damaged = app(state.clone(), None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri("/v1/timeline/events?source=7&from=0&to=2")
                .body(Body::empty())
                .expect("damaged request"),
        )
        .await
        .expect("damaged route");
    let damaged = capture_json(damaged).await;
    assert_eq!(damaged.status, StatusCode::OK);
    assert_eq!(damaged.body["meta"]["source_status"], "gap");
    assert_eq!(
        damaged.body["meta"]["source_freshness"][0]["source_status"],
        "gap"
    );
    assert_eq!(
        damaged.body["meta"]["loss"][0]["known_gaps"],
        serde_json::json!([{ "from_us": 0, "to_us": 2 }])
    );
    assert_eq!(
        state.response_cache.len(),
        cached_complete_responses,
        "the partial response is not cached and the old durable fact is not consulted"
    );
}

#[test]
fn persistence_metrics_use_closed_labels_and_reset_every_gauge() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        record_persist_snapshot(PersistModeSnapshot {
            mode: PersistMode::UnavailableBackoff,
            failures: 4,
            reason: Some(PersistError::NoSpace),
            retry_after: Duration::from_secs(42),
            probe_in_flight: true,
        });
        record_probe_metrics(PersistenceProbeOutcome::InFlight);
        record_probe_metrics(PersistenceProbeOutcome::Succeeded);
        record_probe_metrics(PersistenceProbeOutcome::Failed(
            PersistError::StaleFilesystem,
        ));
        record_persist_snapshot(PersistModeSnapshot {
            mode: PersistMode::ReadWrite,
            failures: 0,
            reason: None,
            retry_after: Duration::ZERO,
            probe_in_flight: false,
        });
    });

    let rendered = handle.render();
    assert_eq!(
        metric_series(&rendered, "kronika_web_overview_persist_reason{"),
        12
    );
    assert_eq!(
        metric_series(&rendered, "kronika_web_overview_persist_failure_class{"),
        7
    );
    assert_metric(&rendered, "kronika_web_overview_persist_mode 0");
    assert_metric(&rendered, "kronika_web_overview_persist_failures 0");
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_retry_after_seconds 0",
    );
    assert_metric(&rendered, "kronika_web_overview_persist_probe_in_flight 0");
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_reason{reason=\"none\"} 1",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_reason{reason=\"no_space\"} 0",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_failure_class{class=\"none\"} 1",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_failure_class{class=\"capacity\"} 0",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_probe_attempts_total{result=\"success\"} 1",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_probe_attempts_total{result=\"failure\"} 1",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_probe_failures_total{reason=\"stale_filesystem\"} 1",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persist_probe_skipped_total{reason=\"in_flight\"} 1",
    );
}

fn metric_series(rendered: &str, prefix: &str) -> usize {
    rendered
        .lines()
        .filter(|line| line.starts_with(prefix))
        .count()
}

fn assert_metric(rendered: &str, exact_line: &str) {
    assert!(
        rendered.lines().any(|line| line == exact_line),
        "missing metric line `{exact_line}` in:\n{rendered}"
    );
}
