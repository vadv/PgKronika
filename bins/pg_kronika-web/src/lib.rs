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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::MatchedPath;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use kronika_reader::{FallbackConfig, LocalDirSnapshot};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, TryAcquireError};
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
pub use overview::live::OverviewBuildError;
pub use startup::WebConfig;

/// Container format version this build serves, mirrored into `/v1/version`.
pub const FORMAT_VERSION: u32 = 1;

/// Histogram buckets, in seconds, for `kronika_web_request_duration_seconds`.
pub const REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Atomically published store metadata and timeline facts.
#[derive(Debug)]
pub(crate) struct PublishedStoreView {
    snapshot: Arc<LocalDirSnapshot>,
    timeline: Arc<overview::view::IndexView>,
}

type TimelineFlightResult = Result<Arc<[u8]>, problem::ApiProblem>;

#[derive(Debug)]
pub(crate) struct TimelineFlight {
    result: Mutex<Option<TimelineFlightResult>>,
    notify: Notify,
}

impl TimelineFlight {
    fn pending() -> Self {
        Self {
            result: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    pub(crate) async fn wait(&self) -> TimelineFlightResult {
        loop {
            let notified = self.notify.notified();
            let result = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(result) = result {
                return result;
            }
            notified.await;
        }
    }
}

#[derive(Debug)]
pub(crate) enum TimelineFlightRole {
    Leader(Arc<TimelineFlight>),
    Follower(Arc<TimelineFlight>),
}

/// Explicit overview storage and memory policy.
#[derive(Debug, Clone)]
pub struct OverviewConfig {
    /// Durable fact-cache root.
    pub cache_root: PathBuf,
    /// Stable normalized store/deployment identity.
    pub namespace: Vec<u8>,
    /// Bounded durable-publication fallback.
    pub fallback: FallbackConfig,
    /// Logical serialized-response cache byte ceiling.
    pub response_cache_bytes: usize,
    /// Secondary serialized-response entry ceiling.
    pub response_cache_entries: usize,
    /// Maximum simultaneously pinned event views.
    pub cursor_max_views: usize,
    /// Logical byte ceiling for pinned event views.
    pub cursor_max_bytes: usize,
    /// Lifetime of one event cursor and its pinned view.
    pub cursor_ttl: Duration,
}

impl OverviewConfig {
    /// Builds the default bounded policy for an explicit cache root and
    /// namespace.
    #[must_use]
    pub fn new(cache_root: PathBuf, namespace: Vec<u8>) -> Self {
        Self {
            cache_root,
            namespace,
            fallback: FallbackConfig::default(),
            response_cache_bytes: RESPONSE_CACHE_BYTES,
            response_cache_entries: RESPONSE_CACHE_ENTRIES,
            cursor_max_views: 64,
            cursor_max_bytes: 512 * 1024 * 1024,
            cursor_ttl: Duration::from_mins(5),
        }
    }
}

/// Startup failure before a coherent store/timeline pair exists.
#[derive(Debug)]
pub enum StateBuildError {
    /// The initial incremental scan failed.
    Snapshot(std::io::Error),
    /// Overview configuration or assembly failed.
    Overview(OverviewBuildError),
    /// Cursor registry configuration or authentication-key setup failed.
    CursorRegistry(std::io::Error),
}

impl std::fmt::Display for StateBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Snapshot(error) => write!(f, "initial store refresh: {error}"),
            Self::Overview(error) => write!(f, "initial overview build: {error}"),
            Self::CursorRegistry(error) => write!(f, "cursor registry: {error}"),
        }
    }
}

impl std::error::Error for StateBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Snapshot(error) | Self::CursorRegistry(error) => Some(error),
            Self::Overview(error) => Some(error),
        }
    }
}

/// Shared router state: one coherent store view and readiness counters.
///
/// All fields use `Arc` so `Clone` is cheap; the router clones this per request.
#[derive(Debug, Clone)]
pub struct AppState {
    published: Arc<ArcSwap<PublishedStoreView>>,
    /// Unix timestamp (seconds) of the last successful snapshot refresh.
    pub last_refresh: Arc<AtomicU64>,
    /// Number of completed refresh loop iterations (successful or not).
    pub refresh_loop_iterations: Arc<AtomicU64>,
    /// Age threshold after which the store is considered stale.
    pub stale_after: Duration,
    analytic_requests: Arc<Semaphore>,
    timeline_flights: Arc<Mutex<HashMap<overview::cache::ResponseKey, Arc<TimelineFlight>>>>,
    cursor_registry: Arc<overview::cursor::CursorRegistry>,
    overview: Arc<Mutex<overview::OverviewIndex>>,
    /// Byte-bounded cache of exact serialized timeline responses.
    pub(crate) response_cache: overview::cache::ResponseCache,
}

/// Byte budget for the exact response cache: 64 MiB.
const RESPONSE_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Secondary response-cache ceiling for small bodies.
const RESPONSE_CACHE_ENTRIES: usize = 4_096;

fn default_overview_config() -> OverviewConfig {
    static INSTANCE: AtomicU64 = AtomicU64::new(0);
    let instance = INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    OverviewConfig::new(
        std::env::temp_dir().join(format!(
            "pgkronika-overview-test-{}-{instance}",
            std::process::id()
        )),
        format!("test-store-{}-{instance}", std::process::id()).into_bytes(),
    )
}

impl AppState {
    /// Wrap a snapshot in shared state with default readiness values.
    ///
    /// `last_refresh` is initialised to the current wall-clock second so that
    /// `/readyz` reports ready immediately after startup. `stale_after` defaults
    /// to 10 s, matching the refresh loop cadence.
    ///
    /// # Errors
    ///
    /// Returns a typed startup error when the initial snapshot refresh or
    /// overview assembly cannot produce one coherent published view.
    pub fn new(snapshot: LocalDirSnapshot) -> Result<Self, StateBuildError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self::with_overview_config(
            snapshot,
            now,
            Duration::from_secs(10),
            default_overview_config(),
        )
    }

    /// Construct state with an explicit `last_refresh` and `stale_after`.
    ///
    /// The server passes the configured staleness threshold and the current
    /// time; tests use it to drive `/readyz` from an injected `last_refresh`.
    ///
    /// # Errors
    ///
    /// Returns a typed startup error when the initial snapshot refresh or
    /// overview assembly cannot produce one coherent published view.
    pub fn with_readiness(
        snapshot: LocalDirSnapshot,
        last_refresh_secs: u64,
        stale_after: Duration,
    ) -> Result<Self, StateBuildError> {
        Self::with_overview_config(
            snapshot,
            last_refresh_secs,
            stale_after,
            default_overview_config(),
        )
    }

    /// Constructs state from an explicit production overview policy.
    ///
    /// # Errors
    ///
    /// Returns a typed startup error when the snapshot cannot refresh or the
    /// configured writer cannot publish its initial view.
    pub fn with_overview_config(
        mut snapshot: LocalDirSnapshot,
        last_refresh_secs: u64,
        stale_after: Duration,
        config: OverviewConfig,
    ) -> Result<Self, StateBuildError> {
        let OverviewConfig {
            cache_root,
            namespace,
            fallback,
            response_cache_bytes,
            response_cache_entries,
            cursor_max_views,
            cursor_max_bytes,
            cursor_ttl,
        } = config;
        let delta = snapshot
            .refresh_incremental_delta()
            .map_err(StateBuildError::Snapshot)?;
        let mut overview = overview::OverviewIndex::new(cache_root, namespace, fallback)
            .map_err(StateBuildError::Overview)?;
        let timeline = overview
            .assemble(&snapshot, &delta)
            .map_err(StateBuildError::Overview)?;
        let cursor_registry =
            overview::cursor::CursorRegistry::new(overview::cursor::CursorConfig {
                max_views: cursor_max_views,
                max_bytes: cursor_max_bytes,
                ttl_secs: cursor_ttl.as_secs(),
            })
            .map_err(StateBuildError::CursorRegistry)?;
        let published = PublishedStoreView {
            snapshot: Arc::new(snapshot),
            timeline: Arc::new(timeline),
        };
        Ok(Self {
            published: Arc::new(ArcSwap::from_pointee(published)),
            last_refresh: Arc::new(AtomicU64::new(last_refresh_secs)),
            refresh_loop_iterations: Arc::new(AtomicU64::new(0)),
            stale_after,
            analytic_requests: Arc::new(Semaphore::new(1)),
            timeline_flights: Arc::new(Mutex::new(HashMap::new())),
            cursor_registry: Arc::new(cursor_registry),
            overview: Arc::new(Mutex::new(overview)),
            response_cache: overview::cache::ResponseCache::new(
                response_cache_bytes,
                response_cache_entries,
            ),
        })
    }

    /// Reserve the server's single heavy-analysis slot without queuing.
    pub(crate) fn try_acquire_analytic(&self) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        Arc::clone(&self.analytic_requests).try_acquire_owned()
    }

    pub(crate) fn timeline_flight(&self, key: &overview::cache::ResponseKey) -> TimelineFlightRole {
        let mut flights = self
            .timeline_flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(flight) = flights.get(key) {
            metrics::counter!("kronika_web_timeline_singleflight_joins_total").increment(1);
            return TimelineFlightRole::Follower(Arc::clone(flight));
        }
        let flight = Arc::new(TimelineFlight::pending());
        flights.insert(key.clone(), Arc::clone(&flight));
        drop(flights);
        metrics::counter!("kronika_web_timeline_singleflight_leaders_total").increment(1);
        TimelineFlightRole::Leader(flight)
    }

    pub(crate) fn finish_timeline_flight(
        &self,
        key: &overview::cache::ResponseKey,
        flight: &Arc<TimelineFlight>,
        result: TimelineFlightResult,
    ) {
        *flight
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        flight.notify.notify_waiters();
        let mut flights = self
            .timeline_flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flights
            .get(key)
            .is_some_and(|registered| Arc::ptr_eq(registered, flight))
        {
            flights.remove(key);
        }
    }

    pub(crate) fn cursor_registry(&self) -> &overview::cursor::CursorRegistry {
        &self.cursor_registry
    }

    /// Reclaims timeline cursor views whose TTL has elapsed.
    ///
    /// The refresh loop calls this independently of store/timeline build
    /// success, so a failing collector cannot prolong pinned-view retention.
    pub fn prune_timeline_cursors(&self, now_secs: u64) {
        self.cursor_registry.prune(now_secs);
    }

    /// Current snapshot from one coherent publication.
    #[must_use]
    pub fn snapshot(&self) -> Arc<LocalDirSnapshot> {
        Arc::clone(&self.published.load().snapshot)
    }

    pub(crate) fn overview_view(&self) -> Arc<overview::view::IndexView> {
        Arc::clone(&self.published.load().timeline)
    }

    /// Generation of the timeline view currently published to HTTP readers.
    #[must_use]
    pub fn overview_view_generation(&self) -> u64 {
        self.published.load().timeline.view_generation()
    }

    /// Builds and publishes one coherent store/timeline pair.
    ///
    /// # Errors
    ///
    /// Returns a typed build error without changing the published pair when
    /// live folding or writer access fails.
    #[allow(
        clippy::cast_precision_loss,
        reason = "Prometheus gauges use f64; integer counters remain exact through 2^53"
    )]
    pub fn republish_store_view(
        &self,
        snapshot: LocalDirSnapshot,
        delta: &kronika_reader::RefreshDelta,
    ) -> Result<(), OverviewBuildError> {
        let mut overview = self
            .overview
            .lock()
            .map_err(|_error| OverviewBuildError::WriterPoisoned)?;
        let timeline = overview.assemble_with_live(&snapshot, delta)?;
        let diagnostics = overview.diagnostics();
        let gc = overview.collect_fact_garbage();
        drop(overview);
        if let Some(gc) = gc {
            metrics::counter!("kronika_web_overview_gc_deleted_total").increment(gc.deleted);
            metrics::counter!("kronika_web_overview_gc_freed_bytes_total")
                .increment(gc.freed_bytes);
            metrics::gauge!("kronika_web_overview_gc_pending").set(gc.pending as f64);
        }
        metrics::gauge!("kronika_web_overview_durable_hits_total")
            .set(diagnostics.durable_hits as f64);
        metrics::gauge!("kronika_web_overview_fallback_hits_total")
            .set(diagnostics.fallback_hits as f64);
        metrics::gauge!("kronika_web_overview_rebuilt_total").set(diagnostics.rebuilt as f64);
        metrics::gauge!("kronika_web_overview_promotions_total").set(diagnostics.promotions as f64);
        metrics::gauge!("kronika_web_overview_persistence_failures_total")
            .set(diagnostics.persistence_failures as f64);
        metrics::gauge!("kronika_web_overview_sealed_failures_total")
            .set(diagnostics.sealed_failures as f64);
        metrics::gauge!("kronika_web_overview_data_through_us")
            .set(timeline.data_through_us().unwrap_or_default() as f64);
        self.cursor_registry.prune(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        self.published.store(Arc::new(PublishedStoreView {
            snapshot: Arc::new(snapshot),
            timeline: Arc::new(timeline),
        }));
        Ok(())
    }

    /// Publishes a fresh metadata snapshot with the last usable timeline view.
    pub fn publish_snapshot_with_last_timeline(&self, snapshot: LocalDirSnapshot) {
        let timeline = self.overview_view();
        self.published.store(Arc::new(PublishedStoreView {
            snapshot: Arc::new(snapshot),
            timeline,
        }));
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
