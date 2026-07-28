use axum::http::StatusCode;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};

use super::{assert_problem, serve};

fn write_event_segment(directory: &std::path::Path, source: u64) {
    let body = PgLogLifecycleV1::encode(&[PgLogLifecycleV1 {
        ts: Ts(1_500),
        kind: 0,
        pid: Some(42),
        signal: Some(9),
        shutdown_mode: None,
        message: None,
        query_detail: None,
        dict_dropped_fields: 0,
    }])
    .expect("encode lifecycle");
    let bytes = build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: 1_500,
            max_ts: 1_500,
            source_id: source,
        },
    );
    crate::test_layout::write_named_pgm(directory, "events.pgm", &bytes);
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
    assert_eq!(events["notable"], false);
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
