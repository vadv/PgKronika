//! The `pg_kronika-web` binary: serve the JSON API and embedded UI over a local
//! store directory.
//!
//! Configuration comes from the environment ([`WebConfig`]). This binary installs
//! the metrics recorder and the log subscriber, opens the store, runs the refresh
//! loop that republishes the snapshot once a second, and serves until a
//! termination signal drains in-flight requests.
#![allow(
    unused_crate_dependencies,
    reason = "this binary consumes the pg_kronika_web library; the package's other dependencies belong to the library and its tests"
)]
#![allow(
    clippy::multiple_crate_versions,
    reason = "metrics-exporter-prometheus and axum pull duplicate transitive versions outside our control"
)]
#![allow(
    clippy::cast_precision_loss,
    reason = "gauge values are small integer counts that fit an f64 exactly"
)]

use std::os::unix::ffi::OsStrExt as _;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kronika_reader::LocalDirSnapshot;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use pg_kronika_web::{
    AppState, AuthConfig, FORMAT_VERSION, OverviewConfig, REQUEST_DURATION_BUCKETS, WebConfig, app,
};
use tower_http::trace::TraceLayer;

/// Process-wide allocator: mimalloc, chosen for the allocation-heavy
/// snapshot-clone-per-request path. The `override` feature also routes C-side
/// malloc (zstd in the parquet decode path) through mimalloc.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// How often the refresh task re-scans the store directory.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Config first, before any I/O or logging: a bad environment must fail
    // immediately. The subscriber is not up yet, so this lone message goes to
    // stderr directly.
    let cfg = WebConfig::from_env().unwrap_or_else(|err| {
        eprintln!("configuration error: {err}");
        std::process::exit(2);
    });

    init_tracing(&cfg.log);
    let metrics_handle = install_metrics()?;

    let canonical_store = std::fs::canonicalize(&cfg.dir)
        .map_err(|err| format!("failed to resolve store at {}: {err}", cfg.dir.display()))?;
    let snapshot = LocalDirSnapshot::open(&canonical_store)
        .map_err(|err| format!("failed to open store at {}: {err}", cfg.dir.display()))?;
    let namespace = cfg
        .overview_namespace
        .clone()
        .unwrap_or_else(|| canonical_store.as_os_str().as_bytes().to_vec());
    let overview = OverviewConfig::new(cfg.overview_cache_dir.clone(), namespace);
    let state =
        AppState::with_overview_config(snapshot, now_unix_secs(), cfg.stale_after, overview)?;
    let auth = cfg
        .basic_auth
        .as_ref()
        .map(|(user, pass)| AuthConfig::new(user, pass));

    tracing::info!(
        addr = %cfg.addr,
        auth = auth.is_some(),
        stale_after_s = cfg.stale_after.as_secs(),
        version = env!("CARGO_PKG_VERSION"),
        format_version = FORMAT_VERSION,
        store = %cfg.dir.display(),
        overview_cache = %cfg.overview_cache_dir.display(),
        "pg_kronika-web starting"
    );

    let refresh = tokio::spawn(refresh_loop(state.clone()));

    let listener = tokio::net::TcpListener::bind(cfg.addr.as_str()).await?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let router = app(state, auth, metrics_handle).layer(TraceLayer::new_for_http());
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv() => {}
            }
        })
        .await?;

    refresh.abort();
    Ok(())
}

/// Seconds since the Unix epoch, or 0 if the clock predates it.
fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Install the JSON tracing subscriber at the level from `KRONIKA_WEB_LOG`.
///
/// An unparsable directive falls back to `info` rather than failing the start.
fn init_tracing(log: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(log)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();
}

/// Install the Prometheus recorder with the request-duration buckets and record
/// the immutable `build_info` gauge.
fn install_metrics() -> Result<PrometheusHandle, Box<dyn std::error::Error>> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("kronika_web_request_duration_seconds".to_owned()),
            REQUEST_DURATION_BUCKETS,
        )
        .map_err(|err| format!("invalid request-duration buckets: {err}"))?
        .install_recorder()
        .map_err(|err| format!("failed to install the metrics recorder: {err}"))?;
    metrics::gauge!(
        "kronika_web_build_info",
        "version" => env!("CARGO_PKG_VERSION"),
        "format_version" => FORMAT_VERSION.to_string(),
    )
    .set(1.0);
    Ok(handle)
}

/// Re-scan the store once a second and republish the snapshot.
///
/// The task owns a private mutable snapshot and publishes a fresh clone after
/// each successful scan. It advances the readiness counters and the store-health
/// gauges and counts refresh errors, so a stalled collector or a corrupt store
/// stays visible in `/metrics` and the logs.
#[allow(
    clippy::infinite_loop,
    reason = "the refresh task runs until the server aborts it on shutdown"
)]
async fn refresh_loop(state: AppState) {
    let mut snap = state.snapshot().as_ref().clone();
    let mut health = (snap.warnings().len(), snap.damages().len());
    if health != (0, 0) {
        tracing::warn!(
            warnings = health.0,
            damaged = health.1,
            "store opened with integrity issues"
        );
    }
    metrics::gauge!("kronika_web_store_warnings").set(health.0 as f64);
    metrics::gauge!("kronika_web_store_damages").set(health.1 as f64);

    loop {
        tokio::time::sleep(REFRESH_INTERVAL).await;
        state.refresh_loop_iterations.fetch_add(1, Relaxed);
        metrics::counter!("kronika_web_refresh_loop_iterations_total").increment(1);
        let fallback_snapshot = snap.clone();
        let worker_state = state.clone();
        let refreshed = tokio::task::spawn_blocking(move || {
            let scan = snap.refresh_incremental_delta();
            let publication = match &scan {
                Ok(delta) => match worker_state.republish_store_view(snap.clone(), delta) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        worker_state.publish_snapshot_with_last_timeline(snap.clone());
                        Err(error)
                    }
                },
                Err(_error) => Ok(()),
            };
            (snap, scan, publication)
        })
        .await;
        let (next_snapshot, scan, publication) = match refreshed {
            Ok(result) => result,
            Err(error) => {
                metrics::counter!("kronika_web_refresh_errors_total").increment(1);
                tracing::error!(%error, "overview refresh worker failed");
                snap = fallback_snapshot;
                continue;
            }
        };
        snap = next_snapshot;
        match scan {
            Ok(_delta) => {
                let current = (snap.warnings().len(), snap.damages().len());
                if current != health {
                    tracing::warn!(
                        warnings = current.0,
                        damaged = current.1,
                        "store health changed"
                    );
                    health = current;
                }
                metrics::gauge!("kronika_web_store_warnings").set(current.0 as f64);
                metrics::gauge!("kronika_web_store_damages").set(current.1 as f64);
                state.last_refresh.store(now_unix_secs(), Relaxed);
                metrics::gauge!("kronika_web_overview_view_generation")
                    .set(snap.view_generation() as f64);
                if let Err(error) = publication {
                    metrics::counter!("kronika_web_overview_refresh_errors_total").increment(1);
                    tracing::warn!(%error, "overview refresh retained the last timeline view");
                }
            }
            Err(err) => {
                metrics::counter!("kronika_web_refresh_errors_total").increment(1);
                tracing::warn!(%err, "store refresh failed");
            }
        }
    }
}
