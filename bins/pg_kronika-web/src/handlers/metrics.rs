//! `GET /metrics` — Prometheus scrape endpoint.
//!
//! On each scrape: reads process RSS and open-fd count from `/proc/self/*`,
//! updates data-age and unit-count gauges from the live snapshot, then
//! delegates rendering to the `PrometheusHandle`.

use std::sync::atomic::Ordering::Relaxed;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::AppState;

/// `GET /metrics` — render the Prometheus scrape page.
///
/// Updates on-scrape gauges before rendering so values are always fresh.
pub(crate) async fn metrics_handler(
    State(state): State<AppState>,
    Extension(handle): Extension<PrometheusHandle>,
) -> impl IntoResponse {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Snapshot-derived gauges.
    let snapshot = state.snapshot.load_full();
    let units = snapshot.units();
    let unit_count = units.len();
    // Gauge precision: unit counts never exceed 2^52 in practice.
    #[allow(
        clippy::cast_precision_loss,
        reason = "unit count never exceeds 2^52 in practice"
    )]
    let unit_count_f = unit_count as f64;
    metrics::gauge!("kronika_web_units_total").set(unit_count_f);

    let max_ts_micros = units.iter().map(|u| u.max_ts).max().unwrap_or(0);
    if max_ts_micros > 0 {
        #[allow(
            clippy::cast_sign_loss,
            reason = "max_ts is checked to be positive before this cast"
        )]
        let data_secs = (max_ts_micros / 1_000_000) as u64;
        let age = now_secs.saturating_sub(data_secs);
        // Age in seconds fits well within f64 precision.
        #[allow(
            clippy::cast_precision_loss,
            reason = "age in seconds fits well within f64 mantissa for any real data age"
        )]
        metrics::gauge!("kronika_web_data_age_seconds").set(age as f64);
    } else {
        metrics::gauge!("kronika_web_data_age_seconds").set(0.0);
    }

    // Reader-age gauge: seconds since the last successful snapshot refresh.
    let last = state.last_refresh.load(Relaxed);
    let reader_age = now_secs.saturating_sub(last);
    #[allow(
        clippy::cast_precision_loss,
        reason = "reader age in seconds fits well within f64 mantissa"
    )]
    metrics::gauge!("kronika_web_reader_age_seconds").set(reader_age as f64);

    // Process-level gauges from /proc.
    set_process_metrics();

    let body = handle.render();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4"),
        )],
        body,
    )
}

/// Read RSS and open-fd count from `/proc/self/*` and update gauges.
///
/// Errors are silently ignored so a missing `/proc` entry does not crash
/// the scrape endpoint.
fn set_process_metrics() {
    if let Some(rss) = read_rss_bytes() {
        #[allow(
            clippy::cast_precision_loss,
            reason = "RSS bytes may exceed f64 mantissa but the loss is acceptable for monitoring"
        )]
        metrics::gauge!("process_resident_memory_bytes").set(rss as f64);
    }
    if let Some(fds) = count_open_fds() {
        #[allow(
            clippy::cast_precision_loss,
            reason = "fd count never exceeds 2^52 in practice"
        )]
        metrics::gauge!("process_open_fds").set(fds as f64);
    }
}

/// Returns resident memory in bytes from `/proc/self/statm`.
///
/// Field index 1 (0-based) is the resident set size in pages; page size
/// is 4096 bytes on all Linux targets this binary supports (x86-64, aarch64).
fn read_rss_bytes() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = contents.split_whitespace().nth(1)?.parse().ok()?;
    // Page size is 4096 on all supported Linux targets (x86-64 and aarch64).
    Some(rss_pages * 4096)
}

/// Counts entries in `/proc/self/fd` (each is an open file descriptor).
fn count_open_fds() -> Option<u64> {
    let entries = std::fs::read_dir("/proc/self/fd").ok()?;
    let count = entries.filter_map(Result::ok).count();
    // Subtract the dirfd opened by read_dir itself.
    Some(count.saturating_sub(1) as u64)
}
