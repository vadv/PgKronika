use std::time::Duration;

use kronika_reader::{PersistError, PersistMode, PersistModeSnapshot, PersistenceProbeOutcome};
use metrics_exporter_prometheus::PrometheusBuilder;

use crate::overview::live::OverviewDiagnostics;
use crate::overview::resilience::{record_persist_snapshot, record_probe_metrics};
use crate::record_overview_diagnostics;

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
        record_overview_diagnostics(OverviewDiagnostics {
            durable_hits: 2,
            fallback_hits: 3,
            rebuilt: 4,
            promotions: 5,
            persistence_failures: 6,
            sealed_failures: 7,
        });
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
    assert_metric(
        &rendered,
        "# TYPE kronika_web_overview_persistence_failures_total counter",
    );
    assert_metric(
        &rendered,
        "kronika_web_overview_persistence_failures_total 6",
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
