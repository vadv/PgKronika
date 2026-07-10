//! Serves data to humans and agents: web UI, MCP server, JSON API.
//!
//! This binary hosts an axum router over a near-real-time view of a local
//! store directory. The router is built by [`app`] from an [`AppState`], which
//! holds the shared snapshot behind an [`ArcSwap`]. Request handlers clone the
//! current snapshot (catalog metadata only, not section bodies) and call the
//! reader's `&mut` query functions on their private copy. In production a
//! background task refreshes the shared snapshot once a second; tests build the
//! state directly and never start that task, so the router stays deterministic.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::routing::get;
use axum::{Json, Router};
use kronika_reader::LocalDirSnapshot;
use serde_json::json;

/// Container format version this build serves, mirrored into `/v1/version`.
const FORMAT_VERSION: u32 = 1;

/// How often the production refresh task re-scans the store directory.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Environment variable naming the store directory served by `main`.
const DIR_ENV: &str = "KRONIKA_WEB_DIR";

/// Shared router state.
///
/// The snapshot is swapped atomically by the background refresh task; handlers
/// load the current pointer and clone it for their own `&mut` queries.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The current store snapshot, replaced wholesale on each refresh.
    pub snapshot: Arc<ArcSwap<LocalDirSnapshot>>,
}

impl AppState {
    /// Wrap an already-open snapshot in swappable shared state.
    #[must_use]
    pub fn new(snapshot: LocalDirSnapshot) -> Self {
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
        }
    }
}

/// Build the request router over `state`.
///
/// Pure: no sockets, no background tasks. Tests call this directly and drive it
/// with `tower::ServiceExt::oneshot`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/version", get(version))
        .with_state(state)
}

/// `GET /v1/version` — the API and container format versions this build serves.
///
/// The body is static: `{"api":"v1","format_version":1}` with an
/// `application/json` content type.
async fn version() -> Json<serde_json::Value> {
    Json(json!({ "api": "v1", "format_version": FORMAT_VERSION }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args_os().nth(1).map_or_else(
        || std::env::var_os(DIR_ENV).map(std::path::PathBuf::from),
        |arg| Some(std::path::PathBuf::from(arg)),
    );
    let Some(dir) = dir else {
        eprintln!("usage: pg_kronika-web <dir>   (or set {DIR_ENV})");
        std::process::exit(2);
    };

    let snapshot = LocalDirSnapshot::open(&dir)?;
    let state = AppState::new(snapshot);

    // The refresh task owns its own mutable snapshot and publishes a fresh
    // clone after each incremental scan. The timer policy lives here, in the
    // binary, not in the reader library.
    let shared = Arc::clone(&state.snapshot);
    tokio::spawn(async move {
        let mut snap = shared.load().as_ref().clone();
        loop {
            tokio::time::sleep(REFRESH_INTERVAL).await;
            match snap.refresh_incremental() {
                Ok(()) => shared.store(Arc::new(snap.clone())),
                Err(err) => eprintln!("refresh failed: {err}"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 8080)).await?;
    axum::serve(listener, app(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use tower::ServiceExt;

    use super::{AppState, app};

    /// Build an [`AppState`] over a temp directory holding one `build_part`
    /// segment, then answer one request against `app(state)` in-process.
    ///
    /// Returned to the caller are the response status and its body parsed as
    /// JSON, so later tasks reuse the same fixture-to-response path.
    async fn fixture_response(uri: &str) -> (tempfile::TempDir, StatusCode, serde_json::Value) {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode empty section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("143000.pgm"), &bytes).expect("write segment");

        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let state = AppState::new(snapshot);

        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        let status = response.status();
        let json_body = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        assert!(json_body, "response must carry an application/json body");
        let collected = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let value: serde_json::Value =
            serde_json::from_slice(&collected).expect("body is valid JSON");
        (dir, status, value)
    }

    #[tokio::test]
    async fn version_returns_the_api_and_format_versions() {
        let (_dir, status, body) = fixture_response("/v1/version").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            serde_json::json!({ "api": "v1", "format_version": 1 }),
            "version body must match the committed shape exactly"
        );
    }

    #[tokio::test]
    async fn golden_harness_serves_version_over_a_fixture_directory() {
        // The harness proves the fixture -> AppState -> oneshot path works, so
        // later tasks can drive real query handlers through the same helper.
        let (dir, status, _body) = fixture_response("/v1/version").await;
        assert!(
            dir.path().exists(),
            "the fixture directory outlives the request"
        );
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn snapshot_arc_swap_round_trips_and_clone_stays_queryable() {
        // Serving-model smoke: no background task. Construct a snapshot, publish
        // it through ArcSwap, then clone the loaded pointer and run a `&mut`
        // query against the clone.
        let dir = tempfile::tempdir().expect("tempdir");
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("143000.pgm"), &bytes).expect("write segment");

        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let state = AppState::new(snapshot);

        let mut snap = state.snapshot.load().as_ref().clone();
        let page = kronika_reader::section(
            &mut snap,
            "pg_stat_bgwriter + pg_stat_checkpointer",
            7,
            i64::MIN,
            i64::MAX,
            10,
            None,
        );
        assert!(
            page.is_ok(),
            "a cloned snapshot must answer a section query: {:?}",
            page.err()
        );
    }
}
