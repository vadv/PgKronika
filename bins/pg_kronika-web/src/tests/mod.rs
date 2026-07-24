use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_registry::incident_gauges::{
    PgFreezeHorizonV1, PgProcessCgroupMemoryV1, PgReplicationPhysicalV1,
    PgReplicationSlotRetentionV3, PgStorageMountV1, PgVacuumObservationV1,
};
use kronika_registry::os_meminfo::OsMeminfo;
use kronika_registry::pg_log::PgLogErrorV1;
use kronika_registry::pg_prepared_xacts::PgPreparedXacts;
use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_registry::pg_stat_database::PgStatDatabaseV1;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::{Section, StrId, Ts};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tower::ServiceExt;

use super::{AppState, AuthConfig, app};

mod anomalies;
mod auth_static;
mod incidents;
mod overview_timeline;
mod probes_metrics;
mod problems;
mod sections;
mod version_diff;

/// Process-global Prometheus recorder installed once for all tests.
///
/// `install_recorder` panics on the second call, so `OnceLock` ensures
/// it only runs once per test binary. All `app()` calls in tests share
/// this handle.
static TEST_RECORDER: OnceLock<PrometheusHandle> = OnceLock::new();

#[derive(Debug)]
struct CapturedResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: serde_json::Value,
}

impl CapturedResponse {
    fn media_type(&self) -> Option<&str> {
        self.headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    }
}

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
/// Returned to the caller are the response status and its body parsed as JSON;
/// callers that assert headers use [`fixture_captured`].
async fn fixture_response(uri: &str) -> (tempfile::TempDir, StatusCode, serde_json::Value) {
    let (dir, response) = fixture_captured(uri, &[]).await;
    (dir, response.status, response.body)
}

async fn fixture_captured(
    uri: &str,
    request_headers: &[(&str, &str)],
) -> (tempfile::TempDir, CapturedResponse) {
    fixture_request_captured(Method::GET, uri, request_headers).await
}

async fn fixture_request_captured(
    method: Method,
    uri: &str,
    request_headers: &[(&str, &str)],
) -> (tempfile::TempDir, CapturedResponse) {
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

    let mut request = Request::builder().method(method).uri(uri);
    for &(name, value) in request_headers {
        request = request.header(name, value);
    }
    let response = app(state, None, test_metrics_handle())
        .oneshot(request.body(Body::empty()).expect("build request"))
        .await
        .expect("route request");
    (dir, capture_json(response).await)
}

/// Open a snapshot over a caller-built `dir` and answer one request.
///
/// Unlike [`fixture_response`], the test writes its own segments into `dir`
/// first; this returns the response status and its JSON body.
async fn serve(dir: &std::path::Path, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = serve_captured(dir, uri, &[]).await;
    (response.status, response.body)
}

async fn serve_captured(
    dir: &std::path::Path,
    uri: &str,
    request_headers: &[(&str, &str)],
) -> CapturedResponse {
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir).expect("open snapshot");
    let state = AppState::new(snapshot);
    let mut request = Request::builder().uri(uri);
    for &(name, value) in request_headers {
        request = request.header(name, value);
    }
    let response = app(state, None, test_metrics_handle())
        .oneshot(request.body(Body::empty()).expect("build request"))
        .await
        .expect("route request");
    capture_json(response).await
}

async fn capture_json(response: Response) -> CapturedResponse {
    let status = response.status();
    let headers = response.headers().clone();
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    assert!(
        matches!(
            media_type,
            Some(value)
                if value.starts_with("application/json")
                    || value.starts_with("application/problem+json")
        ),
        "response must carry a JSON media type, got {media_type:?}"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).expect("body is valid JSON");
    CapturedResponse {
        status,
        headers,
        body,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers construct one-use JSON params fixtures inline"
)]
fn assert_problem(
    body: &serde_json::Value,
    status: StatusCode,
    code: &str,
    params: serde_json::Value,
) {
    let object = body.as_object().expect("problem body is an object");
    let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["code", "instance", "params", "status", "type"],
        "problem has exactly the normative fields"
    );
    assert_eq!(body["status"], u64::from(status.as_u16()));
    assert_eq!(body["code"], code);
    assert_eq!(body["params"], params);
    assert_eq!(
        body["type"],
        format!("https://pgkronika.dev/problems/{}", code.replace('_', "-"))
    );
    let instance = body["instance"].as_str().expect("problem instance string");
    let request_id = instance
        .strip_prefix("https://pgkronika.dev/problems/occurrences/")
        .expect("problem instance prefix");
    assert_eq!(request_id.len(), 32);
    assert!(
        request_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "request id is lowercase hexadecimal"
    );
    assert_eq!(request_id, request_id.to_ascii_lowercase());
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

/// Build an empty snapshot in a temp dir; return `(dir, snapshot)`.
fn empty_snapshot() -> (tempfile::TempDir, kronika_reader::LocalDirSnapshot) {
    let dir = tempfile::tempdir().expect("tempdir");
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
    (dir, snapshot)
}
