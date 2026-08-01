use axum::http::StatusCode;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_layout::SegmentId;
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::collection_coverage::CollectionCoverageV1;
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{Section, StrId, Ts};

use super::{AppState, assert_api_error, serve, serve_state};

fn event_segment_bytes() -> Vec<u8> {
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
        },
    )
}

fn write_event_segment(directory: &std::path::Path) {
    crate::test_layout::write_named_pgm(directory, "events.pgm", &event_segment_bytes());
}

fn collection_segment_bytes() -> Vec<u8> {
    let snapshot = SnapshotCoverageV1::encode(&[
        SnapshotCoverageV1 {
            ts: Ts(100),
            section_type_id: 1_002_001,
            collector_pid: 42,
            collector_started_at: Ts(1),
            read_state: 0,
            visibility: 0,
            source_total: 5,
            collected: 5,
        },
        SnapshotCoverageV1 {
            ts: Ts(200),
            section_type_id: 1_002_001,
            collector_pid: 42,
            collector_started_at: Ts(1),
            read_state: 1,
            visibility: 0,
            source_total: 4_800,
            collected: 500,
        },
        SnapshotCoverageV1 {
            ts: Ts(300),
            section_type_id: 1_002_001,
            collector_pid: 42,
            collector_started_at: Ts(1),
            read_state: 2,
            visibility: 1,
            source_total: 10,
            collected: 5,
        },
        SnapshotCoverageV1 {
            ts: Ts(400),
            section_type_id: 1_002_001,
            collector_pid: 42,
            collector_started_at: Ts(1),
            read_state: 3,
            visibility: 2,
            source_total: 0,
            collected: 0,
        },
    ])
    .expect("encode snapshot coverage");
    let collection = CollectionCoverageV1::encode(&[
        CollectionCoverageV1 {
            ts: Ts(200),
            section_type_id: 1_002_001,
            total: 4_800,
            unknown_total: false,
            collected: 500,
            max_n: 500,
            order_by: StrId(1),
            cutoff_value: Some(1.0),
            reason: 0,
        },
        CollectionCoverageV1 {
            ts: Ts(300),
            section_type_id: 1_002_001,
            total: 10,
            unknown_total: false,
            collected: 5,
            max_n: 500,
            order_by: StrId(1),
            cutoff_value: None,
            reason: 2,
        },
        CollectionCoverageV1 {
            ts: Ts(400),
            section_type_id: 1_002_001,
            total: 0,
            unknown_total: true,
            collected: 0,
            max_n: 500,
            order_by: StrId(1),
            cutoff_value: None,
            reason: 1,
        },
    ])
    .expect("encode collection coverage");
    build_part(
        &[
            SectionInput {
                type_id: 1_023_001,
                rows: 3,
                body: &collection,
            },
            SectionInput {
                type_id: 1_038_001,
                rows: 4,
                body: &snapshot,
            },
        ],
        PartMeta {
            min_ts: 100,
            max_ts: 400,
        },
    )
}

fn write_collection_segment(directory: &std::path::Path) {
    crate::test_layout::write_named_pgm(directory, "collection.pgm", &collection_segment_bytes());
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
    write_event_segment(directory.path());
    publish_web_index(directory.path());

    let (status, body) = serve(directory.path(), "/v1/views/summary?at=1500").await;

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
    assert!(events["collection"].is_null());
    assert_eq!(body["quality"]["status"], "partial");
    assert_eq!(body["quality"]["snapshots"], 1);
}

#[tokio::test]
async fn ui_summary_over_an_unindexed_sealed_segment_degrades_without_failing() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path());
    // No publish_web_index: descriptor-first publication makes the sealed
    // descriptor visible right after the seal, while the sidecar is built
    // lazily by the first admitted timeline load.

    let (status, body) = serve(directory.path(), "/v1/views/summary?at=1500").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let views = body["views"].as_array().expect("views");
    assert_eq!(views.len(), 9);
    let events = views
        .iter()
        .find(|view| view["view"] == "events")
        .expect("events");
    assert_eq!(events["status"], "unavailable");
    assert!(events["snapshot_ts_us"].is_null());
    // A pending index borrows no false revision or resource verdict.
    assert_eq!(
        body["quality"]["unavailable_revision"],
        serde_json::json!([])
    );
    assert_eq!(body["quality"]["resource_limited"], serde_json::json!([]));
    assert_eq!(body["quality"]["status"], "partial");

    publish_web_index(directory.path());
    let (status, healed) = serve(directory.path(), "/v1/views/summary?at=1500").await;
    assert_eq!(status, StatusCode::OK, "{healed}");
    let healed_events = healed["views"]
        .as_array()
        .expect("views")
        .iter()
        .find(|view| view["view"] == "events")
        .expect("events");
    assert_eq!(healed_events["status"], "complete");
    assert_eq!(healed_events["population"], 1);
}

#[tokio::test]
async fn summary_returns_notable_level_and_count() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path());
    publish_web_index(directory.path());

    let (status, body) = serve(directory.path(), "/v1/views/summary?at=1500").await;
    assert_eq!(status, StatusCode::OK);
    let events = body["views"]
        .as_array()
        .expect("views")
        .iter()
        .find(|view| view["view"] == "events")
        .expect("events");
    assert_eq!(events["notable_level"], "warning");
    assert_eq!(events["notable_count"], 2);
}

#[tokio::test]
async fn ui_summary_returns_factual_collection_states() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_collection_segment(directory.path());
    publish_web_index(directory.path());

    for (at, population, collection) in [
        (
            100,
            5,
            serde_json::json!({
                "collected": 5,
                "source_total": 5,
                "read_state": "complete",
                "visibility": "full",
            }),
        ),
        (
            200,
            500,
            serde_json::json!({
                "collected": 500,
                "source_total": 4_800,
                "read_state": "source_limit",
                "visibility": "full",
            }),
        ),
        (
            300,
            5,
            serde_json::json!({
                "collected": 5,
                "source_total": 10,
                "read_state": "permission",
                "visibility": "restricted",
            }),
        ),
        (
            400,
            0,
            serde_json::json!({
                "collected": 0,
                "source_total": null,
                "read_state": "read_failure",
                "visibility": "unknown",
            }),
        ),
    ] {
        let (status, body) = serve(directory.path(), &format!("/v1/views/summary?at={at}")).await;
        assert_eq!(status, StatusCode::OK);
        let statements = body["views"]
            .as_array()
            .expect("views")
            .iter()
            .find(|view| view["view"] == "statements")
            .expect("statements");
        assert_eq!(statements["snapshot_ts_us"], at.to_string());
        assert_eq!(statements["population"], population);
        assert_eq!(statements["status"], "complete");
        assert_eq!(statements["notable"], false);
        assert_eq!(statements["collection"], collection);
    }
}

#[tokio::test]
async fn ui_summary_rejects_unknown_parameters() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path());
    publish_web_index(directory.path());

    let (status, body) = serve(directory.path(), "/v1/views/summary?at=1500&extra=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_api_error(
        &body,
        status,
        "unknown_query_parameter",
        serde_json::json!({ "parameter": "extra" }),
    );
}

#[tokio::test]
async fn ui_heatmap_merges_the_selected_view_from_ovf() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_event_segment(directory.path());
    publish_web_index(directory.path());

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?view=events&metric=count&from=1500&to=1601&buckets=2&top=8",
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
    write_event_segment(directory.path());
    publish_web_index(directory.path());

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?view=missing&metric=count&from=0&to=1",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_api_error(
        &body,
        status,
        "invalid_query_parameter",
        serde_json::json!({ "parameter": "view", "expected": "projection_code" }),
    );

    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/heatmap?view=events&metric=count&from=0&to=86400000001",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_api_error(
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
        "/v1/timeline/heatmap?view=events&metric=count&from=60000000&to=120000000",
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
    let part = event_segment_bytes();
    crate::test_layout::write_journal(
        directory.path(),
        SegmentId::new(1_700_000_000_000_000).expect("segment id"),
        &[&part],
    );
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("open snapshot");
    let state = AppState::new(snapshot).expect("state");

    let (status, summary) = serve_state(state.clone(), "/v1/views/summary?at=1600").await;
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
        "/v1/timeline/heatmap?view=events&metric=count&from=0&to=60000000&buckets=2&top=8",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heatmap["quality"]["active_tail"], true);
    assert_eq!(heatmap["rows"][0]["score"]["lower"], 2.0);
    assert_eq!(heatmap["rows"][0]["values"][0], 2.0);
}
