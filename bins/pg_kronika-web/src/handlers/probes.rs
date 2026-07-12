use std::sync::atomic::Ordering::Relaxed;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::json;

use crate::AppState;
use crate::startup::staleness;

/// `GET /healthz` — liveness probe.
///
/// Always returns 200 while the process is up.
pub(crate) async fn healthz() -> Json<serde_json::Value> {
    Json(json!({"status": "ok"}))
}

/// `GET /readyz` — readiness probe.
///
/// Returns 200 with `ready: true` when the snapshot is fresh, 503 with
/// `ready: false` when it exceeds `stale_after`.
pub(crate) async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last = state.last_refresh.load(Relaxed);
    let seconds_since_refresh = now.saturating_sub(last);
    let is_stale = staleness(now, last, state.stale_after);
    if is_stale {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false, "seconds_since_refresh": seconds_since_refresh})),
        )
    } else {
        (
            StatusCode::OK,
            Json(json!({"ready": true, "seconds_since_refresh": seconds_since_refresh})),
        )
    }
}
