use axum::http::StatusCode;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_layout::SegmentId;
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};

use super::{AppState, assert_problem, serve, serve_state};

fn event_segment_bytes(source: u64) -> Vec<u8> {
    let body = PgLogLifecycleV1::encode(&[
        PgLogLifecycleV1 {
            ts: Ts(1_500),
            kind: 0,
            pid: Some(42),
            signal: Some(9),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        },
        PgLogLifecycleV1 {
            ts: Ts(1_600),
            kind: 0,
            pid: Some(43),
            signal: Some(15),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        },
    ])
    .expect("encode lifecycle");
    build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 2,
            body: &body,
        }],
        PartMeta {
            min_ts: 1_500,
            max_ts: 1_600,
            source_id: source,
        },
    )
}

fn write_event_segment(directory: &std::path::Path, source: u64) {
    crate::test_layout::write_named_pgm(directory, "events.pgm", &event_segment_bytes(source));
}

fn publish_web_index(directory: &std::path::Path) {
    let snapshot = LocalDirSnapshot::open(directory).expect("open snapshot");
    let store = FactStore::new(directory);
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish web index");
    }
}

#[tokio::test]
async fn ui_summary_returns_exact_event_population_and_all_views() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path(), 7);
    publish_web_index(directory.path());

    let (status, body) = serve(directory.path(), "/v1/views/summary?source=7&at=1500").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["at_us"], "1500");
    assert_eq!(body["views"].as_array().map(Vec::len), Some(9));
    let events = body["views"]
        .as_array()
        .expect("views")
        .iter()
        .find(|view| view["view"] == "events")
        .expect("events");
    assert_eq!(events["snapshot_ts_us"], "1500");
    assert_eq!(events["population"], 1);
    assert_eq!(events["status"], "complete");
    assert_eq!(events["notable"], true);
    assert_eq!(body["quality"]["status"], "partial");
    assert_eq!(body["quality"]["snapshots"], 1);
}

#[tokio::test]
async fn ui_summary_rejects_unknown_source_and_parameters() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path(), 7);
    publish_web_index(directory.path());

    let (status, body) = serve(directory.path(), "/v1/views/summary?source=8&at=1500").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_problem(
        &body,
        status,
        "unknown_source",
        serde_json::json!({ "source": 8 }),
    );

    let (status, body) = serve(
        directory.path(),
        "/v1/views/summary?source=7&at=1500&extra=1",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_problem(
        &body,
        status,
        "unknown_query_parameter",
        serde_json::json!({ "parameter": "extra" }),
    );
}

#[tokio::test]
async fn ui_heatmap_merges_the_selected_view_from_ovf() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path(), 7);
    publish_web_index(directory.path());

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?source=7&view=events&metric=count&from=1500&to=1601&buckets=2&top=8",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["grid"]["from_us"], "1500");
    assert_eq!(body["grid"]["to_us"], "1601");
    assert_eq!(body["grid"]["bucket_count"], 2);
    assert_eq!(body["ranking"]["exact"], true);
    assert_eq!(body["ranking"]["unseen_upper"], 0.0);
    assert_eq!(body["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["rows"][0]["unit"], "count");
    assert_eq!(body["rows"][0]["score"]["lower"], 2.0);
    assert_eq!(body["rows"][0]["score"]["upper"], 2.0);
    assert_eq!(body["rows"][0]["values"][0], 2.0);
    assert!(body["rows"][0]["values"][1].is_null());
}

#[tokio::test]
async fn ui_heatmap_enforces_range_and_projection_contracts() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path(), 7);
    publish_web_index(directory.path());

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?source=7&view=missing&metric=count&from=0&to=1",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_problem(
        &body,
        status,
        "invalid_query_parameter",
        serde_json::json!({ "parameter": "view", "expected": "projection_code" }),
    );

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?source=7&view=events&metric=count&from=0&to=86400000001",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_problem(
        &body,
        status,
        "query_limit_exceeded",
        serde_json::json!({
            "resource": "query_span_us",
            "limit": 86_400_000_000_u64,
            "observed": 86_400_000_001_u64,
        }),
    );

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?source=7&view=events&metric=count&from=60000000&to=120000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rows"], serde_json::json!([]));
    assert_eq!(body["ranking"]["exact"], false);
    assert_eq!(
        body["quality"]["gaps"],
        serde_json::json!([{ "from_us": "60000000", "to_us": "120000000" }])
    );
}

#[tokio::test]
async fn ui_summary_and_heatmap_include_the_current_active_tail() {
    let directory = tempfile::tempdir().expect("tempdir");
    let part = event_segment_bytes(7);
    crate::test_layout::write_journal(
        directory.path(),
        SegmentId::new(1_700_000_000_000_000).expect("segment id"),
        &[&part],
    );
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("open snapshot");
    let state = AppState::new(snapshot).expect("state");

    let (status, summary) = serve_state(state.clone(), "/v1/views/summary?source=7&at=1600").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["quality"]["active_tail"], true);
    let events = summary["views"]
        .as_array()
        .expect("views")
        .iter()
        .find(|view| view["view"] == "events")
        .expect("events");
    assert_eq!(events["population"], 1);
    assert_eq!(events["notable"], true);

    let (status, heatmap) = serve_state(
        state,
        "/v1/timeline/heatmap?source=7&view=events&metric=count&from=0&to=60000000&buckets=2&top=8",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heatmap["quality"]["active_tail"], true);
    assert_eq!(heatmap["rows"][0]["score"]["lower"], 2.0);
    assert_eq!(heatmap["rows"][0]["values"][0], 2.0);
}
