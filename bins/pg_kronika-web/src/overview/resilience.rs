//! Closed persistence-recovery orchestration and metrics.

use kronika_reader::{
    FactStore, PersistError, PersistFailureClass, PersistModeSnapshot, PersistenceProbeOutcome,
};

const PERSIST_REASONS: [&str; 12] = [
    "none",
    "read_only_filesystem",
    "permission_denied",
    "no_space",
    "quota_exceeded",
    "transient_io",
    "stale_filesystem",
    "invalid_facts",
    "busy",
    "unsafe_path",
    "invalid_cache_state",
    "io",
];
const PERSIST_CLASSES: [&str; 7] = [
    "none",
    "read_only",
    "permission",
    "capacity",
    "transient",
    "contended",
    "permanent",
];

pub(crate) fn run_due_probe(store: &FactStore) -> PersistenceProbeOutcome {
    store.probe_persistence()
}

pub(crate) fn record_probe_metrics(outcome: PersistenceProbeOutcome) {
    match outcome {
        PersistenceProbeOutcome::NotDue => {}
        PersistenceProbeOutcome::InFlight => {
            metrics::counter!(
                "kronika_web_overview_persist_probe_skipped_total",
                "reason" => "in_flight"
            )
            .increment(1);
        }
        PersistenceProbeOutcome::Succeeded => {
            metrics::counter!(
                "kronika_web_overview_persist_probe_attempts_total",
                "result" => "success"
            )
            .increment(1);
        }
        PersistenceProbeOutcome::Failed(error) => {
            metrics::counter!(
                "kronika_web_overview_persist_probe_attempts_total",
                "result" => "failure"
            )
            .increment(1);
            metrics::counter!(
                "kronika_web_overview_persist_probe_failures_total",
                "reason" => persist_reason(Some(error))
            )
            .increment(1);
        }
    }
}

pub(crate) fn record_persist_snapshot(snapshot: PersistModeSnapshot) {
    metrics::gauge!("kronika_web_overview_persist_mode")
        .set(super::super::persist_mode_code(snapshot.mode));
    metrics::gauge!("kronika_web_overview_persist_failures").set(f64::from(snapshot.failures));
    metrics::gauge!("kronika_web_overview_persist_retry_after_seconds")
        .set(snapshot.retry_after.as_secs_f64());
    metrics::gauge!("kronika_web_overview_persist_probe_in_flight")
        .set(f64::from(snapshot.probe_in_flight));

    let reason = persist_reason(snapshot.reason);
    for candidate in PERSIST_REASONS {
        metrics::gauge!(
            "kronika_web_overview_persist_reason",
            "reason" => candidate
        )
        .set(f64::from(candidate == reason));
    }
    let class = persist_class(snapshot.reason);
    for candidate in PERSIST_CLASSES {
        metrics::gauge!(
            "kronika_web_overview_persist_failure_class",
            "class" => candidate
        )
        .set(f64::from(candidate == class));
    }
}

const fn persist_reason(error: Option<PersistError>) -> &'static str {
    match error {
        None => "none",
        Some(PersistError::ReadOnlyFilesystem) => "read_only_filesystem",
        Some(PersistError::PermissionDenied) => "permission_denied",
        Some(PersistError::NoSpace) => "no_space",
        Some(PersistError::QuotaExceeded) => "quota_exceeded",
        Some(PersistError::TransientIo) => "transient_io",
        Some(PersistError::StaleFilesystem) => "stale_filesystem",
        Some(PersistError::InvalidFacts) => "invalid_facts",
        Some(PersistError::Busy) => "busy",
        Some(PersistError::UnsafePath) => "unsafe_path",
        Some(PersistError::InvalidCacheState) => "invalid_cache_state",
        Some(PersistError::Io) => "io",
    }
}

fn persist_class(error: Option<PersistError>) -> &'static str {
    match error.map(PersistError::class) {
        None => "none",
        Some(PersistFailureClass::ReadOnly) => "read_only",
        Some(PersistFailureClass::Permission) => "permission",
        Some(PersistFailureClass::Capacity) => "capacity",
        Some(PersistFailureClass::Transient) => "transient",
        Some(PersistFailureClass::Contended) => "contended",
        Some(PersistFailureClass::Permanent) => "permanent",
    }
}
