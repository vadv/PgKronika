use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kronika_reader::{
    PersistError, PersistMode, PersistModeSnapshot, PersistenceProbeOutcome, PgmUnit,
};
use metrics_exporter_prometheus::PrometheusBuilder;
use tower::ServiceExt as _;

use crate::overview::resilience::{record_persist_snapshot, record_probe_metrics};
use crate::{AppState, OverviewConfig, app};

use super::{test_metrics_handle, write_bgwriter_segment};

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
        OverviewConfig::new(
            dir.path().join(".overview-cache"),
            b"last-good-descriptor-authority".to_vec(),
        ),
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
    let segment_path = dir.path().join("one.pgm");
    let cache_root = dir.path().join(".overview-cache");
    let namespace = b"durable-first-restart".to_vec();
    write_bgwriter_segment(dir.path(), "one.pgm", 7, 0, 1);

    let first_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("first snapshot");
    let first_state = AppState::with_overview_config(
        first_snapshot,
        0,
        Duration::from_secs(10),
        OverviewConfig::new(cache_root.clone(), namespace.clone()),
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

    let mut bytes = std::fs::read(&segment_path).expect("read segment");
    let body_offset = {
        let unit = PgmUnit::open(bytes.as_slice()).expect("open segment catalog");
        let entry = unit.catalog().entries.first().expect("section entry");
        assert_ne!(entry.len, 0, "fixture section body is non-empty");
        usize::try_from(entry.offset).expect("section offset fits usize")
    };
    bytes[body_offset] ^= 0xff;
    std::fs::write(&segment_path, bytes).expect("corrupt only the source section body");

    let restarted_snapshot =
        kronika_reader::LocalDirSnapshot::open(dir.path()).expect("restart snapshot");
    let restarted_state = AppState::with_overview_config(
        restarted_snapshot,
        0,
        Duration::from_secs(10),
        OverviewConfig::new(cache_root, namespace),
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
