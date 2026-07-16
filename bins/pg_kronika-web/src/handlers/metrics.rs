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

    let max_ts_micros = units.iter().map(|u| u.max_ts).max();
    metrics::gauge!("kronika_web_data_age_seconds").set(data_age_seconds(now_secs, max_ts_micros));

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

/// Returns resident memory in bytes from `/proc/self/status`.
fn read_rss_bytes() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_rss_bytes(&contents)
}

fn data_age_seconds(now_secs: u64, max_ts_micros: Option<i64>) -> f64 {
    let Some(max_ts_micros) = max_ts_micros.filter(|&ts| ts > 0) else {
        return f64::NAN;
    };
    #[allow(clippy::cast_sign_loss, reason = "the timestamp is positive")]
    let data_secs = (max_ts_micros / 1_000_000) as u64;
    #[allow(
        clippy::cast_precision_loss,
        reason = "real data ages fit in the f64 integer mantissa"
    )]
    let age = now_secs.saturating_sub(data_secs) as f64;
    age
}

fn parse_rss_bytes(contents: &str) -> Option<u64> {
    let line = contents.lines().find(|line| line.starts_with("VmRSS:"))?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "VmRSS:" {
        return None;
    }
    let kib: u64 = fields.next()?.parse().ok()?;
    if fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    kib.checked_mul(1024)
}

/// Counts entries in `/proc/self/fd` (each is an open file descriptor).
fn count_open_fds() -> Option<u64> {
    let entries = std::fs::read_dir("/proc/self/fd").ok()?;
    let count = entries.filter_map(Result::ok).count();
    // Subtract the dirfd opened by read_dir itself.
    Some(count.saturating_sub(1) as u64)
}

#[cfg(test)]
mod tests {
    use super::{data_age_seconds, parse_rss_bytes};

    #[test]
    fn missing_data_age_is_not_reported_as_zero() {
        assert!(data_age_seconds(100, None).is_nan());
        assert!(data_age_seconds(100, Some(0)).is_nan());
    }

    #[test]
    fn data_age_uses_the_latest_unit_timestamp() {
        assert!((data_age_seconds(100, Some(95_000_000)) - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rss_uses_the_kernel_reported_kibibytes() {
        let status = "Name:\tpg_kronika-web\nVmRSS:\t123 kB\nThreads:\t1\n";
        assert_eq!(parse_rss_bytes(status), Some(123 * 1024));
    }

    #[test]
    fn malformed_or_overflowing_rss_is_unavailable() {
        assert_eq!(parse_rss_bytes("VmRSS: unknown kB\n"), None);
        assert_eq!(parse_rss_bytes("VmRSS: 1 MB\n"), None);
        assert_eq!(parse_rss_bytes("VmRSS: 18446744073709551615 kB\n"), None);
    }
}
