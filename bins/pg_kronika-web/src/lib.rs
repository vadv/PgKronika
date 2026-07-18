//! JSON API over a local store directory, served by an axum router.
//!
//! Handlers clone the shared snapshot (catalog metadata, not section bodies)
//! and run the reader's `&mut` queries on the private copy. A background task
//! refreshes the shared snapshot once a second; tests skip it, so the router
//! stays deterministic.
#![allow(
    clippy::multiple_crate_versions,
    reason = "metrics-exporter-prometheus and axum pull duplicate transitive versions outside our control"
)]

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
    reason = "finding, evidence, and lens types are exercised by engine tests but the HTTP \
              endpoint currently exposes clustering only"
)]
mod incident;
mod incident_input;
mod incident_response;
mod params;
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
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
            last_refresh: Arc::new(AtomicU64::new(now)),
            refresh_loop_iterations: Arc::new(AtomicU64::new(0)),
            stale_after: Duration::from_secs(10),
            analytic_requests: Arc::new(Semaphore::new(1)),
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
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
            last_refresh: Arc::new(AtomicU64::new(last_refresh_secs)),
            refresh_loop_iterations: Arc::new(AtomicU64::new(0)),
            stale_after,
            analytic_requests: Arc::new(Semaphore::new(1)),
        }
    }

    /// Reserve the server's single heavy-analysis slot without queuing.
    pub(crate) fn try_acquire_analytic(&self) -> Result<OwnedSemaphorePermit, TryAcquireError> {
        Arc::clone(&self.analytic_requests).try_acquire_owned()
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
mod tests {
    use std::sync::OnceLock;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::Ts;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_prepared_xacts::PgPreparedXacts;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::pg_stat_database::PgStatDatabaseV1;
    use kronika_registry::reset_metadata::ResetMetadata;
    use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
    use tower::ServiceExt;

    use super::{AppState, AuthConfig, app};

    /// Process-global Prometheus recorder installed once for all tests.
    ///
    /// `install_recorder` panics on the second call, so `OnceLock` ensures
    /// it only runs once per test binary. All `app()` calls in tests share
    /// this handle.
    static TEST_RECORDER: OnceLock<PrometheusHandle> = OnceLock::new();

    fn test_metrics_handle() -> PrometheusHandle {
        TEST_RECORDER
            .get_or_init(|| {
                PrometheusBuilder::new()
                    .set_buckets_for_metric(
                        Matcher::Full("kronika_web_request_duration_seconds".to_owned()),
                        super::REQUEST_DURATION_BUCKETS,
                    )
                    .expect("histogram buckets are valid")
                    .install_recorder()
                    .expect("install global Prometheus recorder")
            })
            .clone()
    }

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

        let response = app(state, None, test_metrics_handle())
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

    #[tokio::test]
    async fn section_diff_resolves_identity_and_columns_for_a_known_section() {
        // The fixture holds no pg_stat_statements data, so the diff is empty —
        // but the route, the registry column resolution, and the response shape
        // are all exercised.
        let (_dir, status, body) =
            fixture_response("/v1/section/pg_stat_statements/diff?source=7&from=0&to=9000").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["section"], "pg_stat_statements");
        assert_eq!(
            body["identity"],
            serde_json::json!(["queryid", "userid", "dbid", "toplevel"]),
            "identity resolves as the union of the section's versions"
        );
        assert_eq!(body["series"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn batch_diff_serves_each_requested_section_keyed_by_name() {
        // The fixture holds no rows for either section, so the series are empty —
        // but the batch route, name resolution, and per-name shape are exercised.
        let (_dir, status, body) = fixture_response(
            "/v1/sections/batch/diff?source=7&from=0&to=9000&names=pg_stat_wal,pg_stat_io",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pg_stat_wal"]["section"], "pg_stat_wal");
        assert_eq!(body["pg_stat_wal"]["series"], serde_json::json!([]));
        assert_eq!(body["pg_stat_io"]["section"], "pg_stat_io");
        assert_eq!(body["pg_stat_io"]["series"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn batch_diff_rejects_a_missing_names_parameter() {
        let (_dir, status, _body) =
            fixture_response("/v1/sections/batch/diff?source=7&from=0&to=9000").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
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

    /// Open a snapshot over a caller-built `dir` and answer one request.
    ///
    /// Unlike [`fixture_response`], the test writes its own segments into `dir`
    /// first; this returns the response status and its JSON body.
    async fn serve(dir: &std::path::Path, uri: &str) -> (StatusCode, serde_json::Value) {
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir).expect("open snapshot");
        let state = AppState::new(snapshot);
        let response = app(state, None, test_metrics_handle())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let value = serde_json::from_slice(&bytes).expect("body is valid JSON");
        (status, value)
    }

    /// Write an empty `pg_stat_bgwriter + pg_stat_checkpointer` segment.
    fn write_bgwriter_segment(
        dir: &std::path::Path,
        file: &str,
        source: u64,
        min_ts: i64,
        max_ts: i64,
    ) {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        );
        std::fs::write(dir.join(file), &bytes).expect("write segment");
    }

    /// One `pg_stat_archiver` row with every optional column left NULL.
    fn archiver_row(ts: i64, archived: i64) -> PgStatArchiver {
        PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }
    }

    #[tokio::test]
    async fn sources_fold_each_source_into_one_span() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);
        write_bgwriter_segment(dir.path(), "3000.pgm", 7, 3_000, 4_000);
        write_bgwriter_segment(dir.path(), "1500.pgm", 42, 1_500, 2_500);

        let (status, body) = serve(dir.path(), "/v1/sources").await;
        assert_eq!(status, StatusCode::OK, "sources responds 200");
        assert_eq!(
            body,
            serde_json::json!({ "sources": [
                { "source_id": 7, "min_ts": 1_000, "max_ts": 4_000, "segments": 2 },
                { "source_id": 42, "min_ts": 1_500, "max_ts": 2_500, "segments": 1 }
            ] }),
            "each source folds its units into one span, ordered by source_id"
        );
    }

    #[tokio::test]
    async fn sections_catalog_describes_archiver_from_the_registry() {
        // The catalog is static: it comes from the registry, not the fixture.
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        let (status, body) = serve(dir.path(), "/v1/sections").await;
        assert_eq!(status, StatusCode::OK, "sections responds 200");
        let archiver = body["sections"]
            .as_array()
            .expect("sections is an array")
            .iter()
            .find(|section| section["name"] == "pg_stat_archiver")
            .expect("pg_stat_archiver is in the catalog");
        assert_eq!(
            archiver["semantics"], "snapshot_full",
            "archiver is a full snapshot"
        );
        assert_eq!(
            archiver["sort_key"],
            serde_json::json!(["ts"]),
            "archiver sorts by ts"
        );
        let columns = archiver["columns"].as_array().expect("columns array");
        assert!(
            columns.contains(&serde_json::json!({ "name": "ts", "type": "ts", "class": "t" })),
            "ts is a timestamp-class ts column"
        );
        assert!(
            columns.contains(
                &serde_json::json!({ "name": "archived_count", "type": "i64", "class": "c" })
            ),
            "archived_count is a cumulative i64 counter"
        );
    }

    #[tokio::test]
    async fn segments_sum_rows_per_name_and_skip_dictionaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archiver_a = PgStatArchiver::encode(&[archiver_row(1_000, 1), archiver_row(1_100, 2)])
            .expect("encode archiver");
        let archiver_b =
            PgStatArchiver::encode(&[archiver_row(1_200, 3)]).expect("encode archiver");
        let bgwriter = BgwriterCheckpointer::encode(&[]).expect("encode bgwriter");
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: 1_008_001,
                    rows: 2,
                    body: &archiver_a,
                },
                SectionInput {
                    type_id: 1_008_001,
                    rows: 1,
                    body: &archiver_b,
                },
                SectionInput {
                    type_id: 1_006_001,
                    rows: 0,
                    body: &bgwriter,
                },
            ],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("1000.pgm"), &bytes).expect("write segment");

        let (status, body) = serve(dir.path(), "/v1/segments?source=7&from=0&to=3000").await;
        assert_eq!(status, StatusCode::OK, "segments responds 200");
        assert_eq!(
            body,
            serde_json::json!({ "segments": [
                { "segment_id": "1000", "source_id": 7, "min_ts": 1_000, "max_ts": 2_000,
                  "sections": [
                    { "name": "pg_stat_archiver", "rows": 3 },
                    { "name": "pg_stat_bgwriter + pg_stat_checkpointer", "rows": 0 }
                  ] }
            ] }),
            "repeated type_ids of one name sum their rows; sections order by name"
        );
    }

    /// Forty archiver snapshots a minute apart: the counter climbs by one per
    /// minute except minutes 20..25, where it climbs by fifty — a rate spike
    /// against a calm reference. Returns the last snapshot time.
    fn write_archiver_spike_segment(dir: &std::path::Path) -> i64 {
        const MINUTE: i64 = 60 * 1_000_000;
        let mut rows = Vec::new();
        let mut count = 0;
        for minute in 0..40 {
            count += if (20..25).contains(&minute) { 50 } else { 1 };
            rows.push(archiver_row(minute * MINUTE, count));
        }
        let to = 39 * MINUTE;
        let body = PgStatArchiver::encode(&rows).expect("encode archiver");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_008_001,
                rows: 40,
                body: &body,
            }],
            PartMeta {
                min_ts: 0,
                max_ts: to,
                source_id: 7,
            },
        );
        std::fs::write(dir.join("0.pgm"), &bytes).expect("write segment");
        to
    }

    #[tokio::test]
    async fn anomalies_rank_the_archiver_spike_first_and_count_honestly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_archiver_spike_segment(dir.path());

        let uri = format!("/v1/anomalies?source=7&from=0&to={to}&window=6m&step=2m");
        let (status, body) = serve(dir.path(), &uri).await;
        assert_eq!(status, StatusCode::OK, "anomalies responds 200");

        let episodes = body["episodes"].as_array().expect("episodes is an array");
        assert!(!episodes.is_empty(), "the spike must surface as an episode");
        let top = &episodes[0];
        assert_eq!(top["section"], "pg_stat_archiver");
        assert_eq!(top["column"], "archived_count");
        assert_eq!(top["direction"], "up");
        assert_eq!(top["series"], serde_json::json!({}), "singleton series");
        assert!(
            top["peak"]["m"].as_f64().expect("m is a number") > 3.5,
            "the peak clears the default threshold"
        );

        let counters = &body["sections"]["pg_stat_archiver"];
        assert_eq!(counters["series_total"], 1);
        assert!(counters["evaluated"].as_u64().expect("evaluated") > 0);
        // Two cumulative columns contribute one honest FirstPoint each; the
        // three all-NULL gauge columns skip every one of the 40 rows.
        assert_eq!(counters["nodata_points"], 2 + 3 * 40);
        assert_eq!(body["skipped"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn anomalies_scan_every_scannable_section_without_a_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_archiver_spike_segment(dir.path());

        let uri = format!("/v1/anomalies?source=7&from=0&to={to}&window=6m");
        let (_status, body) = serve(dir.path(), &uri).await;
        let sections = body["sections"].as_object().expect("sections object");
        assert!(
            sections.len() > 1,
            "an unfiltered scan reports counters for every scannable section"
        );
        assert!(sections.contains_key("pg_stat_archiver"));
    }

    fn db_row(ts: i64, tick: i32) -> PgStatDatabaseV1 {
        PgStatDatabaseV1 {
            ts: Ts(ts),
            datid: 5,
            datname: None,
            numbackends: None,
            xact_commit: i64::from(tick) * 10,
            xact_rollback: 0,
            blks_read: i64::from(tick) * 100,
            blks_hit: i64::from(tick) * 1_000,
            tup_returned: 0,
            tup_fetched: 0,
            tup_inserted: 0,
            tup_updated: 0,
            tup_deleted: 0,
            conflicts: 0,
            temp_files: 0,
            temp_bytes: 0,
            deadlocks: 0,
            blk_read_time: 2.5 * f64::from(tick),
            blk_write_time: 0.5 * f64::from(tick),
            stats_reset: None,
            frozen_xid_age: None,
            min_mxid_age: None,
            datconnlimit: None,
            datallowconn: None,
            datistemplate: None,
        }
    }

    fn reset_row(ts: i64, track_io_timing: Option<bool>) -> ResetMetadata {
        ResetMetadata {
            ts: Ts(ts),
            postmaster_start_time: Ts(1),
            pg_stat_database_reset_max_at: None,
            pg_stat_statements_reset_at: None,
            pg_store_plans_reset_at: None,
            pg_stat_bgwriter_reset_at: None,
            pg_stat_checkpointer_reset_at: None,
            pg_stat_wal_reset_at: None,
            pg_stat_archiver_reset_at: None,
            pg_stat_io_reset_at: None,
            ext_pg_stat_statements_version: None,
            ext_pg_store_plans_version: None,
            compute_query_id: None,
            track_io_timing,
            track_wal_io_timing: None,
        }
    }

    fn write_gated_db_segment(dir: &std::path::Path) -> i64 {
        const MINUTE: i64 = 60 * 1_000_000;
        let rows: Vec<PgStatDatabaseV1> =
            (0..4).map(|i| db_row(i64::from(i) * MINUTE, i)).collect();
        let meta: Vec<ResetMetadata> = (0..4).map(|i| reset_row(i * MINUTE, Some(false))).collect();
        let to = 3 * MINUTE;
        let db_body = PgStatDatabaseV1::encode(&rows).expect("encode pg_stat_database");
        let meta_body = ResetMetadata::encode(&meta).expect("encode reset_metadata");
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: 1_005_001,
                    rows: 4,
                    body: &db_body,
                },
                SectionInput {
                    type_id: 1_020_001,
                    rows: 4,
                    body: &meta_body,
                },
            ],
            PartMeta {
                min_ts: 0,
                max_ts: to,
                source_id: 7,
            },
        );
        std::fs::write(dir.join("0.pgm"), &bytes).expect("write segment");
        to
    }

    #[tokio::test]
    async fn diff_reports_not_collected_while_track_io_timing_is_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_gated_db_segment(dir.path());

        let uri = format!("/v1/section/pg_stat_database/diff?source=7&from=0&to={to}");
        let (status, body) = serve(dir.path(), &uri).await;
        assert_eq!(status, StatusCode::OK, "diff responds 200");

        let series = body["series"].as_array().expect("series array");
        let db = series
            .iter()
            .find(|s| s["key"]["datid"] == 5)
            .expect("datid 5 series present");

        let timing = db["columns"]["blk_read_time"]
            .as_array()
            .expect("blk_read_time points");
        assert!(
            timing[1..]
                .iter()
                .all(|point| point["nodata"] == "not_collected"),
            "timings measured under a disabled GUC must read not_collected: {timing:?}"
        );

        let blocks = db["columns"]["blks_read"]
            .as_array()
            .expect("blks_read points");
        assert!(
            blocks[1..].iter().all(|point| point["rate"].is_number()),
            "an ungated counter keeps its rates: {blocks:?}"
        );
    }

    #[tokio::test]
    async fn batch_diff_applies_collection_gates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_gated_db_segment(dir.path());
        let uri = format!("/v1/sections/batch/diff?source=7&from=0&to={to}&names=pg_stat_database");
        let (status, body) = serve(dir.path(), &uri).await;
        assert_eq!(status, StatusCode::OK);
        let points = body["pg_stat_database"]["series"][0]["columns"]["blk_read_time"]
            .as_array()
            .expect("blk_read_time points");
        assert!(
            points[1..]
                .iter()
                .all(|point| point["nodata"] == "not_collected")
        );
    }

    #[tokio::test]
    async fn anomalies_count_gated_timings_as_nodata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_gated_db_segment(dir.path());

        let uri =
            format!("/v1/anomalies?source=7&from=0&to={to}&window=1m&section=pg_stat_database");
        let (status, body) = serve(dir.path(), &uri).await;
        assert_eq!(status, StatusCode::OK, "anomalies responds 200");
        let counters = &body["sections"]["pg_stat_database"];
        assert!(
            counters["nodata_points"].as_u64().expect("nodata_points") >= 6,
            "gated pairs must land in nodata_points: {counters}"
        );
    }

    #[tokio::test]
    async fn anomalies_reject_degenerate_parameters() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        for uri in [
            // window wider than the period
            "/v1/anomalies?source=7&from=0&to=1000&window=1h",
            // from at/after to
            "/v1/anomalies?source=7&from=5&to=5",
            // malformed knobs
            "/v1/anomalies?source=7&from=0&to=9000000000&window=0s",
            "/v1/anomalies?source=7&from=0&to=9000000000&threshold=-1",
            "/v1/anomalies?source=7&from=0&to=9000000000&eps_rel=NaN",
            // a huge period over a tiny step: the position cap must reject it
            // before anything allocates
            "/v1/anomalies?source=7&from=0&to=900000000000000000&window=1h&step=1s",
        ] {
            let (status, _body) = serve(dir.path(), uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} must be rejected");
        }

        let (status, _body) = serve(
            dir.path(),
            "/v1/anomalies?source=7&from=0&to=9000000000&section=nope",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown section is a 404");
    }

    fn write_archiver_with_node(
        dir: &std::path::Path,
        file: &str,
        node_self_id: &str,
        rows: &[PgStatArchiver],
        min_ts: i64,
        max_ts: i64,
    ) {
        use kronika_format::DictLimits;
        use kronika_registry::StrId;
        use kronika_registry::instance_metadata::InstanceMetadata;

        let mut interner = kronika_writer::Interner::new(
            DictLimits::new(4096, 1 << 20).expect("dictionary limits"),
        );
        let mut intern = |value: &str| {
            interner
                .intern(value.as_bytes())
                .map(|id| StrId(id.get()))
                .expect("intern fixture identity")
        };
        let metadata = InstanceMetadata {
            ts: Ts(min_ts),
            hostname: intern("db-host-7"),
            node_self_id: intern(node_self_id),
            pg_version_num: 170_000,
            kernel_version: intern("test-kernel"),
            pg_system_identifier: Some(7),
            clock_ticks_per_sec: 100,
            page_size_bytes: 4096,
            boot_id: intern("test-boot"),
            btime: Ts(0),
        };
        let dictionary =
            kronika_writer::dict::encode(interner.window()).expect("encode dictionary");
        let archiver = PgStatArchiver::encode(rows).expect("encode archiver");
        let metadata = InstanceMetadata::encode(&[metadata]).expect("encode metadata");
        let mut sections: Vec<SectionInput<'_>> = dictionary
            .iter()
            .map(|section| SectionInput {
                type_id: section.type_id,
                rows: section.rows,
                body: &section.body,
            })
            .collect();
        sections.push(SectionInput {
            type_id: 1_008_001,
            rows: u32::try_from(rows.len()).expect("fixture row count"),
            body: &archiver,
        });
        sections.push(SectionInput {
            type_id: 1_021_001,
            rows: 1,
            body: &metadata,
        });
        let bytes = build_part(
            &sections,
            PartMeta {
                min_ts,
                max_ts,
                source_id: 7,
            },
        );
        std::fs::write(dir.join(file), bytes).expect("write segment");
    }

    fn write_archiver_with_identity(
        dir: &std::path::Path,
        rows: &[PgStatArchiver],
        min_ts: i64,
        max_ts: i64,
    ) {
        write_archiver_with_node(dir, "0.pgm", "node-7", rows, min_ts, max_ts);
    }

    fn archiver_rows(spiking: bool) -> Vec<PgStatArchiver> {
        const MINUTE: i64 = 60 * 1_000_000;
        let mut rows = Vec::new();
        let mut count = 0;
        for minute in 0..40 {
            count += if spiking && (20..25).contains(&minute) {
                50
            } else {
                1
            };
            rows.push(archiver_row(minute * MINUTE, count));
        }
        rows
    }

    #[tokio::test]
    async fn incidents_surface_a_spike_and_stay_empty_when_calm() {
        let to = 39 * 60 * 1_000_000;
        let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m");

        let spiking = tempfile::tempdir().expect("tempdir");
        let spike_rows = archiver_rows(true);
        write_archiver_with_node(
            spiking.path(),
            "0.pgm",
            "node-7",
            &spike_rows[..21],
            0,
            20 * 60 * 1_000_000,
        );
        write_archiver_with_node(
            spiking.path(),
            "1.pgm",
            "node-7",
            &spike_rows[20..],
            20 * 60 * 1_000_000,
            to,
        );
        let (status, body) = serve(spiking.path(), &uri).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "incidents 200; got {status}: {body}"
        );

        for field in [
            "complete",
            "clustering_complete",
            "analysis_status",
            "incidents",
            "coverage_by_section",
            "data_age_seconds",
            "catalog",
            "data_quality",
            "skipped",
        ] {
            assert!(body.get(field).is_some(), "response carries {field}");
        }
        assert_eq!(body["complete"], false);
        assert_eq!(body["clustering_complete"], true);
        assert_eq!(body["analysis_status"], "incidents_detected");
        assert_eq!(body["catalog"]["status"], "dormant");
        assert_eq!(body["catalog"]["diagnosis_available"], false);
        assert_eq!(body["catalog"]["applied"], serde_json::json!([]));
        let dormant = body["catalog"]["dormant"]
            .as_array()
            .expect("catalog lists dormant lenses");
        assert_eq!(dormant.len(), 28, "the full lens catalog is declared");
        assert!(
            dormant
                .iter()
                .any(|entry| entry["lens_id"] == "PG-LOCK-012"),
            "the lock lens is catalogued as dormant"
        );
        let incidents = body["incidents"].as_array().expect("incidents is an array");
        assert!(
            !incidents.is_empty(),
            "the spike must cluster into an incident"
        );
        assert_eq!(incidents[0]["findings"], serde_json::json!([]));
        assert_eq!(incidents[0]["evaluation_complete"], false);
        assert_eq!(incidents[0]["finding_evaluation_status"], "not_available");
        let members = incidents[0]["members"]
            .as_array()
            .expect("members is an array");
        assert!(
            members.iter().any(|member| {
                member["logical_section"] == "pg_stat_archiver"
                    && member["column"] == "archived_count"
            }),
            "an incident member is the real archiver spike series"
        );

        let calm = tempfile::tempdir().expect("tempdir");
        write_archiver_with_identity(calm.path(), &archiver_rows(false), 0, to);
        let (status, body) = serve(calm.path(), &uri).await;
        assert_eq!(status, StatusCode::OK, "calm 200; got {status}: {body}");
        assert_eq!(
            body["incidents"],
            serde_json::json!([]),
            "no anomaly means no incident"
        );
        assert_eq!(body["analysis_status"], "calm");
        assert_eq!(body["clustering_complete"], true);
        assert_eq!(body["complete"], false);
        for field in ["catalog", "data_quality", "skipped", "coverage_by_section"] {
            assert!(
                body.get(field).is_some(),
                "an empty response still carries {field}"
            );
        }
    }

    #[tokio::test]
    async fn incidents_reject_degenerate_parameters() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        for uri in [
            "/v1/incidents?source=7&from=5&to=5",
            "/v1/incidents?source=7&from=0&to=1000&window=1h",
            "/v1/incidents?source=7&from=0&to=9000000000&window=0s",
            "/v1/incidents?source=7&from=0&to=9000000000&threshold=-1",
            "/v1/incidents?source=7&from=0&to=9000000000&eps_rel=NaN",
            "/v1/incidents?source=7&from=-9223372036854775808&to=9223372036854775807",
            "/v1/incidents?source=7&from=0&to=3600000000&max_cluster_span=2h",
            "/v1/incidents?source=7&from=0&to=9000000000&unknown=1",
            "/v1/incidents?source=7&source=8&from=0&to=9000000000",
        ] {
            let (status, _body) = serve(dir.path(), uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} must be rejected");
        }

        for uri in [
            "/v1/incidents?source=7&from=0&to=86400000000&window=1s&step=1s",
            "/v1/incidents?source=7&from=0&to=90000000000",
        ] {
            let (status, _body) = serve(dir.path(), uri).await;
            assert_eq!(
                status,
                StatusCode::PAYLOAD_TOO_LARGE,
                "{uri} must hit a hard cap"
            );
        }
    }

    #[tokio::test]
    async fn incidents_distinguish_no_data_and_identity_quality() {
        const MINUTE: i64 = 60 * 1_000_000;
        let no_data = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(no_data.path(), "0.pgm", 7, 0, MINUTE);
        let (status, body) = serve(
            no_data.path(),
            "/v1/incidents?source=8&from=600000000&to=1200000000",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["analysis_status"], "no_data");
        assert_eq!(body["complete"], false);
        assert_eq!(body["data_age_seconds"], serde_json::Value::Null);

        let missing = tempfile::tempdir().expect("tempdir");
        let to = write_archiver_spike_segment(missing.path());
        let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m");
        let (status, body) = serve(missing.path(), &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["analysis_status"], "missing_node_identity");
        assert_eq!(body["complete"], false);
        assert_eq!(body["incidents"], serde_json::json!([]));

        let conflicting = tempfile::tempdir().expect("tempdir");
        let rows = archiver_rows(true);
        write_archiver_with_node(
            conflicting.path(),
            "0.pgm",
            "node-a",
            &rows[..21],
            0,
            20 * MINUTE,
        );
        write_archiver_with_node(
            conflicting.path(),
            "1.pgm",
            "node-b",
            &rows[20..],
            20 * MINUTE,
            39 * MINUTE,
        );
        let (status, body) = serve(conflicting.path(), &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["analysis_status"], "conflicting_node_identity");
        assert_eq!(body["complete"], false);
    }

    #[tokio::test]
    async fn analytic_endpoints_share_fail_fast_admission() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "0.pgm", 7, 0, 10 * 60 * 1_000_000);
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let state = AppState::new(snapshot);
        let _permit = state
            .try_acquire_analytic()
            .expect("reserve the shared analytic slot");

        for uri in [
            "/v1/incidents?source=7&from=0&to=600000000&window=1m&step=1m",
            "/v1/anomalies?source=7&from=0&to=600000000&window=1m&step=1m",
        ] {
            let response = app(state.clone(), None, test_metrics_handle())
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("build request"),
                )
                .await
                .expect("route request");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
                Some("1"),
                "{uri} advertises a valid retry delay",
            );
        }
    }

    #[tokio::test]
    async fn incident_read_failure_is_sanitized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = 39 * 60 * 1_000_000;
        write_archiver_with_identity(dir.path(), &archiver_rows(true), 0, to);
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let state = AppState::new(snapshot);
        std::fs::remove_file(dir.path().join("0.pgm")).expect("remove fixture after snapshot");
        let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m");
        let response = app(state, None, test_metrics_handle())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON error");
        assert_eq!(body["error"], "store_read_failed");
        let rendered = String::from_utf8_lossy(&bytes);
        assert!(!rendered.contains("0.pgm"));
        assert!(!rendered.contains(dir.path().to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn segments_outside_the_window_are_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "5000.pgm", 7, 5_000, 6_000);

        let (status, body) = serve(dir.path(), "/v1/segments?source=7&from=0&to=1000").await;
        assert_eq!(status, StatusCode::OK, "segments responds 200");
        assert_eq!(
            body,
            serde_json::json!({ "segments": [] }),
            "a window before every unit yields no segments"
        );
    }

    #[tokio::test]
    async fn segments_missing_a_required_parameter_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        let (status, body) = serve(dir.path(), "/v1/segments?from=0&to=1000").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a missing source is a client error"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn segments_non_numeric_parameter_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        let (status, body) = serve(dir.path(), "/v1/segments?source=abc&from=0&to=1000").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a non-numeric source is a client error"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("unsigned integer")),
            "the detail explains the parse failure, distinct from a missing parameter"
        );
    }

    /// Write a `pg_stat_archiver` segment holding `rows`.
    fn write_archiver_segment(
        dir: &std::path::Path,
        file: &str,
        source: u64,
        min_ts: i64,
        max_ts: i64,
        rows: &[PgStatArchiver],
    ) {
        let body = PgStatArchiver::encode(rows).expect("encode archiver");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_008_001,
                rows: u32::try_from(rows.len()).expect("row count fits u32"),
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        );
        std::fs::write(dir.join(file), &bytes).expect("write segment");
    }

    #[tokio::test]
    async fn section_serializes_rows_over_a_covered_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5), archiver_row(1_100, 6)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=1000&to=2000&limit=10",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "section responds 200");
        assert_eq!(
            body,
            serde_json::json!({
                "section": "pg_stat_archiver",
                "source_id": 7,
                "rows": [
                    { "ts": 1_000, "archived_count": 5, "last_archived_wal": null, "last_archived_time": null, "failed_count": 0, "last_failed_wal": null, "last_failed_time": null, "stats_reset": null },
                    { "ts": 1_100, "archived_count": 6, "last_archived_wal": null, "last_archived_time": null, "failed_count": 0, "last_failed_wal": null, "last_failed_time": null, "stats_reset": null }
                ],
                "gaps": [],
                "next_cursor": null
            }),
            "rows serialize on union columns; a fully covered window has no gaps"
        );
    }

    #[tokio::test]
    async fn section_reports_a_gap_for_an_uncovered_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=5000&to=6000&limit=10",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "section responds 200");
        assert_eq!(
            body["rows"],
            serde_json::json!([]),
            "an uncovered window has no rows"
        );
        assert_eq!(
            body["gaps"],
            serde_json::json!([{ "from": 5_000, "to": 6_000 }]),
            "the whole uncovered window is one gap"
        );
        assert_eq!(
            body["next_cursor"],
            serde_json::json!(null),
            "an exhausted stream carries no cursor"
        );
    }

    #[tokio::test]
    async fn section_unknown_name_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/does_not_exist?source=7&from=0&to=3000",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown section is 404");
        assert_eq!(
            body["error"], "unknown_section",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn section_bad_parameter_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=abc&from=0&to=3000",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a non-numeric source is 400"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn section_cursor_pages_across_segment_boundaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );
        write_archiver_segment(
            dir.path(),
            "3000.pgm",
            7,
            3_000,
            4_000,
            &[archiver_row(3_000, 2)],
        );

        let (status, page1) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=0&to=5000&limit=1",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "page one responds 200");
        assert_eq!(
            page1["rows"].as_array().map(Vec::len),
            Some(1),
            "the limit caps page one at one row"
        );
        assert_eq!(
            page1["rows"][0]["ts"],
            serde_json::json!(1_000),
            "page one is the earliest row"
        );
        let cursor = page1["next_cursor"]
            .as_str()
            .expect("a full page carries a resume cursor");

        let (status, page2) = serve(
            dir.path(),
            &format!(
                "/v1/section/pg_stat_archiver?source=7&from=0&to=5000&limit=1&cursor={cursor}"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "page two responds 200");
        assert_eq!(
            page2["rows"][0]["ts"],
            serde_json::json!(3_000),
            "page two resumes at the next segment's row, no duplicate"
        );
    }

    #[tokio::test]
    async fn section_malformed_cursor_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=0&to=5000&cursor=notavalidcursor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a malformed cursor is a client error"
        );
        assert_eq!(
            body["error"], "bad_cursor",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn sections_batch_returns_a_page_per_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archiver = PgStatArchiver::encode(&[archiver_row(1_000, 1), archiver_row(1_100, 2)])
            .expect("encode archiver");
        let prepared = PgPreparedXacts::encode(&[]).expect("encode prepared_xacts");
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: 1_008_001,
                    rows: 2,
                    body: &archiver,
                },
                SectionInput {
                    type_id: 1_010_001,
                    rows: 0,
                    body: &prepared,
                },
            ],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("1000.pgm"), &bytes).expect("write segment");

        let (status, body) = serve(
            dir.path(),
            "/v1/sections/batch?source=7&from=1000&to=2000&names=pg_stat_archiver,pg_prepared_xacts&limit=10",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "batch responds 200");
        assert_eq!(
            body["pg_stat_archiver"]["rows"].as_array().map(Vec::len),
            Some(2),
            "the archiver page carries its rows"
        );
        assert_eq!(
            body["pg_stat_archiver"]["section"], "pg_stat_archiver",
            "each page names its section"
        );
        assert_eq!(
            body["pg_prepared_xacts"]["rows"],
            serde_json::json!([]),
            "a section with no rows is still present in the batch"
        );
    }

    #[tokio::test]
    async fn sections_batch_without_names_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );

        let (status, body) =
            serve(dir.path(), "/v1/sections/batch?source=7&from=1000&to=2000").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "batch without names is a client error"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn sections_batch_with_only_separators_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/sections/batch?source=7&from=1000&to=2000&names=,,",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a names list of only separators names no section"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    /// Build an empty snapshot in a temp dir; return `(dir, snapshot)`.
    fn empty_snapshot() -> (tempfile::TempDir, kronika_reader::LocalDirSnapshot) {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        (dir, snapshot)
    }

    /// Drive one probe request and return `(status, body)`.
    async fn probe(state: AppState, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = app(state, None, test_metrics_handle())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let value = serde_json::from_slice(&bytes).expect("body is valid JSON");
        (status, value)
    }

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let (_dir, snapshot) = empty_snapshot();
        let state = AppState::new(snapshot);
        let (status, body) = probe(state, "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"status": "ok"}));
    }

    #[tokio::test]
    async fn readyz_fresh_snapshot_returns_200_ready() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let (_dir, snapshot) = empty_snapshot();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // last_refresh = now; stale_after = 10s => age == 0, not stale
        let state = AppState::with_readiness(snapshot, now, std::time::Duration::from_secs(10));
        let (status, body) = probe(state, "/readyz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ready"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn readyz_stale_snapshot_returns_503_not_ready() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let (_dir, snapshot) = empty_snapshot();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // last_refresh = now - 3600; stale_after = 10s => age = 3600, stale
        let state = AppState::with_readiness(
            snapshot,
            now.saturating_sub(3600),
            std::time::Duration::from_secs(10),
        );
        let (status, body) = probe(state, "/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["ready"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn metrics_endpoint_lists_metric_names_after_traffic() {
        let handle = test_metrics_handle();

        // track_metrics registers the request counter lazily — on the first
        // increment, which runs after a handler returns. Warm it with one
        // request so the scrape sees it without leaning on sibling tests.
        let (_warm_dir, warm_snapshot) = empty_snapshot();
        app(AppState::new(warm_snapshot), None, handle.clone())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route warmup request");

        let (_dir, snapshot) = empty_snapshot();
        let response = app(AppState::new(snapshot), None, handle)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/metrics must return 200"
        );

        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let body = String::from_utf8_lossy(&bytes);

        assert!(
            body.contains("kronika_web_requests_total"),
            "/metrics body must contain kronika_web_requests_total"
        );
        assert!(
            body.lines()
                .any(|line| line == "kronika_web_data_age_seconds NaN"),
            "an empty store must expose data age as unavailable"
        );
        assert!(
            body.contains("kronika_web_reader_age_seconds"),
            "/metrics body must contain kronika_web_reader_age_seconds"
        );
        assert!(
            body.contains("kronika_web_units_total"),
            "/metrics body must contain kronika_web_units_total"
        );
    }

    /// A valid `Authorization: Basic` header for `user:pass`.
    fn basic_header(user: &str, pass: &str) -> String {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        format!("Basic {encoded}")
    }

    /// Drive one request over an empty snapshot with the given auth setting and
    /// optional `Authorization` header; return the response status.
    async fn auth_status(auth: Option<AuthConfig>, uri: &str, header: Option<&str>) -> StatusCode {
        let (_dir, snapshot) = empty_snapshot();
        let mut builder = Request::builder().uri(uri);
        if let Some(value) = header {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        app(AppState::new(snapshot), auth, test_metrics_handle())
            .oneshot(builder.body(Body::empty()).expect("build request"))
            .await
            .expect("route request")
            .status()
    }

    #[tokio::test]
    async fn auth_disabled_leaves_v1_open() {
        assert_eq!(
            auth_status(None, "/v1/version", None).await,
            StatusCode::OK,
            "with auth off, /v1 needs no credentials"
        );
    }

    #[tokio::test]
    async fn auth_enabled_rejects_v1_without_credentials() {
        for uri in [
            "/v1/version",
            "/v1/incidents?source=7&from=0&to=600000000&window=1m",
        ] {
            assert_eq!(
                auth_status(Some(AuthConfig::new("u", "p")), uri, None).await,
                StatusCode::UNAUTHORIZED,
                "with auth on, {uri} without a header is 401",
            );
        }
    }

    #[tokio::test]
    async fn auth_enabled_accepts_correct_credentials() {
        let header = basic_header("u", "p");
        assert_eq!(
            auth_status(
                Some(AuthConfig::new("u", "p")),
                "/v1/version",
                Some(header.as_str())
            )
            .await,
            StatusCode::OK,
            "the right credential opens /v1"
        );
    }

    #[tokio::test]
    async fn auth_enabled_rejects_wrong_password() {
        let header = basic_header("u", "wrong");
        assert_eq!(
            auth_status(
                Some(AuthConfig::new("u", "p")),
                "/v1/version",
                Some(header.as_str())
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "a wrong password is 401"
        );
    }

    #[tokio::test]
    async fn auth_enabled_keeps_probes_and_metrics_public() {
        for uri in ["/healthz", "/readyz", "/metrics"] {
            assert_eq!(
                auth_status(Some(AuthConfig::new("u", "p")), uri, None).await,
                StatusCode::OK,
                "{uri} stays public under auth"
            );
        }
    }

    /// Drive one request over an empty snapshot; return status, content type and
    /// the raw body (static responses are HTML, not JSON).
    async fn raw_request(
        auth: Option<AuthConfig>,
        uri: &str,
        header: Option<&str>,
    ) -> (StatusCode, String, Vec<u8>) {
        let (_dir, snapshot) = empty_snapshot();
        let mut builder = Request::builder().uri(uri);
        if let Some(value) = header {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = app(AppState::new(snapshot), auth, test_metrics_handle())
            .oneshot(builder.body(Body::empty()).expect("build request"))
            .await
            .expect("route request");
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes()
            .to_vec();
        (status, content_type, body)
    }

    #[tokio::test]
    async fn static_serves_index_html() {
        let (status, content_type, body) = raw_request(None, "/index.html", None).await;
        assert_eq!(status, StatusCode::OK, "/index.html is served");
        assert!(
            content_type.starts_with("text/html"),
            "index.html is HTML, got {content_type}"
        );
        assert!(!body.is_empty(), "the shell has a body");
    }

    #[tokio::test]
    async fn static_root_serves_spa_shell() {
        let (status, content_type, _body) = raw_request(None, "/", None).await;
        assert_eq!(status, StatusCode::OK, "/ falls back to the SPA shell");
        assert!(content_type.starts_with("text/html"), "the shell is HTML");
    }

    #[tokio::test]
    async fn static_unknown_ui_path_serves_spa_shell() {
        let (status, content_type, _body) = raw_request(None, "/dashboard/live", None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an unknown UI path falls back to the shell"
        );
        assert!(content_type.starts_with("text/html"), "the shell is HTML");
    }

    #[tokio::test]
    async fn static_unknown_v1_path_is_json_404() {
        let (status, content_type, body) = raw_request(None, "/v1/does-not-exist", None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an unknown /v1 path is a 404"
        );
        assert!(
            content_type.starts_with("application/json"),
            "the 404 is JSON, not the HTML shell"
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        assert_eq!(value["error"], "not_found", "the error names the fault");
    }

    #[tokio::test]
    async fn auth_enabled_protects_static() {
        // Security-critical: with auth on, the UI is behind it too, not just
        // /v1. This fails if the auth layer misses the static fallback.
        let (status, _ct, _body) =
            raw_request(Some(AuthConfig::new("u", "p")), "/index.html", None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "the static UI is behind auth when it is enabled"
        );
    }

    #[tokio::test]
    async fn auth_enabled_allows_static_with_credentials() {
        let header = basic_header("u", "p");
        let (status, content_type, _body) = raw_request(
            Some(AuthConfig::new("u", "p")),
            "/index.html",
            Some(header.as_str()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the right credential opens the UI");
        assert!(content_type.starts_with("text/html"), "the shell is HTML");
    }
}
