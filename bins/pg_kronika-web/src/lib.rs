//! JSON API over a local store directory, served by an axum router.
//!
//! Handlers clone the shared snapshot (catalog metadata, not section bodies)
//! and run the reader's `&mut` queries on the private copy. A background task
//! refreshes the shared snapshot once a second; tests skip it, so the router
//! stays deterministic.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use axum::Router;
use axum::routing::get;
use kronika_reader::LocalDirSnapshot;
// The binary target and the `#[tokio::test]` harness need the async runtime; the
// library's handlers are runtime-agnostic and never name it.
use tokio as _;

pub(crate) mod handlers;
mod params;
mod serialize;
pub(crate) mod startup;

/// Container format version this build serves, mirrored into `/v1/version`.
pub(crate) const FORMAT_VERSION: u32 = 1;

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
        }
    }

    /// Construct state with explicit readiness values — intended for tests.
    ///
    /// Use this when a test needs to control `last_refresh` or `stale_after`
    /// (e.g. to assert `/readyz` 503 behaviour).
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
        }
    }
}

/// Build the request router over `state`.
///
/// Pure: no sockets, no background tasks. Tests call this directly and drive it
/// with `tower::ServiceExt::oneshot`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/version", get(handlers::v1::version))
        .route("/v1/sources", get(handlers::v1::sources))
        .route("/v1/sections", get(handlers::v1::sections))
        .route("/v1/segments", get(handlers::v1::segments))
        .route("/v1/section/{name}", get(handlers::v1::section_data))
        .route("/v1/sections/batch", get(handlers::v1::sections_batch))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::Section;
    use kronika_registry::Ts;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_prepared_xacts::PgPreparedXacts;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
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

    /// Open a snapshot over a caller-built `dir` and answer one request.
    ///
    /// Unlike [`fixture_response`], the test writes its own segments into `dir`
    /// first; this returns the response status and its JSON body.
    async fn serve(dir: &std::path::Path, uri: &str) -> (StatusCode, serde_json::Value) {
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir).expect("open snapshot");
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
}
