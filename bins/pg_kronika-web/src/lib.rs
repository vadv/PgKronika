//! Bounded JSON API and embedded UI over a local PGM directory.
//!
//! Handlers clone the shared snapshot (catalog metadata, not section bodies)
//! and run the reader's `&mut` queries on the private copy. A background task
//! refreshes the shared snapshot once a second; tests skip it, so the router
//! stays deterministic.
//!
//! [`app`] exposes public liveness, readiness, and Prometheus endpoints plus a
//! UI and `/v1` routes for catalogs, rows, diffs, anomalies, and incident
//! clusters. Optional [`AuthConfig`] protects the UI and `/v1`; probes and
//! metrics remain public. TLS and proxy authentication are deployment concerns.
//!
//! Reader and adapter ceilings bound rows, materialized cells, scan positions,
//! scoring work, identity bytes, and output. One semaphore slot admits heavy
//! anomaly or incident work; another request receives `503` instead of
//! queueing. Incident diagnosis is a first slice: the endpoint clusters
//! episodes, runs the active diagnostic lenses, still reports `complete=false`,
//! and returns their findings with a partly dormant lens catalog.
#![allow(
    clippy::multiple_crate_versions,
    reason = "metrics-exporter-prometheus and axum pull duplicate transitive versions outside our control"
)]

macro_rules! closed_string_enum {
    (
        $(#[$meta:meta])*
        $visibility:vis enum $name:ident {
            $($variant:ident => $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $visibility enum $name {
            $($variant),+
        }

        impl $name {
            #[allow(
                dead_code,
                reason = "closed-enum count is part of the generated registry contract"
            )]
            $visibility const COUNT: usize = [$(stringify!($variant)),+].len();

            #[cfg(test)]
            $visibility const ALL: [Self; Self::COUNT] = [
                $(Self::$variant),+
            ];

            $visibility const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }
    };
}

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::MatchedPath;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use kronika_reader::LocalDirSnapshot;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
// These crates are used by the binary target. Keep the imports here so the
// library target satisfies `unused_crate_dependencies`.
use mimalloc as _;
use tokio as _;
use tower_http as _;
use tracing as _;
use tracing_subscriber as _;

// criterion is used only by the `anomalies` bench; anchored for the
// `unused_crate_dependencies` lint, which checks each target separately.
#[cfg(test)]
use criterion as _;

mod anomaly;
mod auth;
pub(crate) mod handlers;
#[allow(
    dead_code,
    reason = "reserved bounded evidence variants are exercised by engine tests even when a \
              production catalog does not construct them"
)]
mod incident;
mod incident_input;
mod incident_response;
pub(crate) mod overview;
mod params;
mod problem;
mod reason;
mod serialize;
pub(crate) mod startup;

pub use auth::AuthConfig;
use auth::require_basic_auth;
pub use startup::WebConfig;

/// Container format version this build serves, mirrored into `/v1/version`.
pub const FORMAT_VERSION: u32 = 1;

/// Histogram buckets, in seconds, for `kronika_web_request_duration_seconds`.
pub const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Shared router state: the store snapshot and readiness counters.
///
/// All fields use `Arc` so `Clone` is cheap; the router clones this per request.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The current store snapshot, replaced wholesale on each refresh.
    pub snapshot: Arc<ArcSwap<LocalDirSnapshot>>,
    /// Unix timestamp (seconds) of the last successful snapshot refresh.
    pub last_refresh: Arc<AtomicU64>,
    /// Number of completed refresh loop iterations (successful or not).
    pub refresh_loop_iterations: Arc<AtomicU64>,
    /// Age threshold after which the store is considered stale.
    pub stale_after: Duration,
    analytic_requests: Arc<Semaphore>,
    overview: overview::OverviewIndex,
    /// The atomic overview index view, republished by the refresh cycle.
    pub(crate) overview_view: Arc<ArcSwap<overview::view::IndexView>>,
    /// Byte-bounded cache of exact serialized timeline responses.
    pub(crate) response_cache: overview::cache::ResponseCache,
}

/// Byte budget for the exact response cache: 64 MiB.
const RESPONSE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// The persistent overview fact store rooted for this process.
///
/// The cache is content-addressed, so a shared per-process root is safe; a
/// write failure degrades to a bounded memory fallback without losing
/// correctness.
fn default_overview_index() -> overview::OverviewIndex {
    let cache_root = std::env::temp_dir().join("pgkronika-overview-cache");
    overview::OverviewIndex::new(cache_root, b"pgkronika".to_vec())
}

impl AppState {
    /// Wrap a snapshot in shared state with default readiness values.
    ///
    /// `last_refresh` is initialised to the current wall-clock second so that
    /// `/readyz` reports ready immediately after startup. `stale_after` defaults
    /// to 10 s, matching the refresh loop cadence.
    #[must_use]
    pub fn new(snapshot: LocalDirSnapshot) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let overview = default_overview_index();
        let initial_view = Arc::new(ArcSwap::from_pointee(overview.assemble(&snapshot)));
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
            last_refresh: Arc::new(AtomicU64::new(now)),
            refresh_loop_iterations: Arc::new(AtomicU64::new(0)),
            stale_after: Duration::from_secs(10),
            analytic_requests: Arc::new(Semaphore::new(1)),
            overview,
            overview_view: initial_view,
            response_cache: overview::cache::ResponseCache::new(RESPONSE_CACHE_BYTES),
        }
    }

    /// Construct state with an explicit `last_refresh` and `stale_after`.
    ///
    /// The server passes the configured staleness threshold and the current
    /// time; tests use it to drive `/readyz` from an injected `last_refresh`.
    #[must_use]
    pub fn with_readiness(
        snapshot: LocalDirSnapshot,
        last_refresh_secs: u64,
        stale_after: Duration,
    ) -> Self {
        let overview = default_overview_index();
        let initial_view = Arc::new(ArcSwap::from_pointee(overview.assemble(&snapshot)));
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
            last_refresh: Arc::new(AtomicU64::new(last_refresh_secs)),
            refresh_loop_iterations: Arc::new(AtomicU64::new(0)),
            stale_after,
            analytic_requests: Arc::new(Semaphore::new(1)),
            overview,
            overview_view: initial_view,
            response_cache: overview::cache::ResponseCache::new(RESPONSE_CACHE_BYTES),
        }
    }

    /// Reserve the server's single heavy-analysis slot without queuing.
    pub(crate) fn try_acquire_analytic(&self) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        Arc::clone(&self.analytic_requests).try_acquire_owned()
    }

    /// Reassemble the overview index view and publish it atomically.
    ///
    /// The refresh cycle is the single writer of the published view. It folds
    /// the delta's active parts into the live generation, so live events become
    /// visible without a request ever decoding PGM bodies.
    pub fn republish_overview(
        &self,
        snapshot: &LocalDirSnapshot,
        delta: &kronika_reader::RefreshDelta,
    ) {
        let view = self.overview.assemble_with_live(snapshot, delta);
        self.overview_view.store(Arc::new(view));
    }
}

/// Per-request metrics middleware.
///
/// Increments `kronika_web_requests_total` and records
/// `kronika_web_request_duration_seconds` after the inner handler responds.
/// Tracks in-flight requests via `kronika_web_inflight_requests` gauge.
/// Path labels come from `MatchedPath` to avoid high-cardinality URIs.
async fn track_metrics(req: Request<axum::body::Body>, next: Next) -> Response {
    let matched = req
        .extensions()
        .get::<MatchedPath>()
        .map(|mp| mp.as_str().to_owned());
    let method = req.method().as_str().to_owned();
    let (method_label, path_label) = startup::metric_labels(&method, matched.as_deref());

    metrics::gauge!("kronika_web_inflight_requests").increment(1.0);
    let start = Instant::now();

    let response = next.run(req).await;

    let elapsed = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    metrics::gauge!("kronika_web_inflight_requests").decrement(1.0);
    metrics::counter!(
        "kronika_web_requests_total",
        "method" => method_label.clone(),
        "path" => path_label,
        "status" => status
    )
    .increment(1);
    metrics::histogram!(
        "kronika_web_request_duration_seconds",
        "method" => method_label,
        "path" => path_label
    )
    .record(elapsed);

    response
}

/// Build the request router over `state`.
///
/// `auth` gates everything except the public probes and `/metrics` — the `/v1`
/// API and the embedded UI alike: `Some` requires the Basic credential, `None`
/// leaves them open. Pure: no sockets, no background tasks. Tests drive it with
/// `tower::ServiceExt::oneshot`.
pub fn app(state: AppState, auth: Option<AuthConfig>, metrics_handle: PrometheusHandle) -> Router {
    use axum::Extension;

    let public = Router::new()
        .route("/healthz", get(handlers::probes::healthz))
        .route("/readyz", get(handlers::probes::readyz))
        .route("/metrics", get(handlers::metrics::metrics_handler));

    let mut protected = Router::new()
        .route("/v1/version", get(handlers::v1::version))
        .route("/v1/timeline/overview", get(overview::handlers::overview))
        .route("/v1/timeline/events", get(overview::handlers::events))
        .route("/v1/timeline/health", get(overview::handlers::health))
        .route("/v1/anomalies", get(handlers::anomalies::anomalies))
        .route("/v1/incidents", get(handlers::incidents::incidents))
        .route("/v1/sources", get(handlers::v1::sources))
        .route("/v1/sections", get(handlers::v1::sections))
        .route("/v1/segments", get(handlers::v1::segments))
        .route("/v1/section/{name}", get(handlers::v1::section_data))
        .route("/v1/section/{name}/diff", get(handlers::v1::section_diff))
        .route("/v1/sections/batch", get(handlers::v1::sections_batch))
        .route(
            "/v1/sections/batch/diff",
            get(handlers::v1::sections_batch_diff),
        )
        .method_not_allowed_fallback(|| async {
            problem::ApiProblem::method_not_allowed("GET, HEAD")
        })
        .fallback(handlers::static_::static_handler);
    if let Some(cfg) = auth {
        // `layer`, not `route_layer`: auth must also cover the static fallback,
        // which `route_layer` (matched routes only) leaves open.
        protected = protected.layer(middleware::from_fn_with_state(cfg, require_basic_auth));
    }

    public
        .merge(protected)
        .layer(Extension(metrics_handle))
        .layer(middleware::from_fn(track_metrics))
        .with_state(state)
}

#[cfg(test)]
mod tests;
