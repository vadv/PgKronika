use axum::http::StatusCode;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::pg_log::PgLogErrorV1;
use kronika_registry::{Section, Ts};
use serde_json::json;
use std::sync::Arc;

use super::{assert_problem, fixture_response, serve};
use crate::overview::cache::{Endpoint, ResponseKey};
use crate::{AppState, TimelineFlightRole};

/// Writes a sealed segment of `count` panic error groups a millisecond apart.
///
/// Each panic is a distinct notable observation, so the segment exercises
/// paginated notable retrieval.
fn write_panic_segment(dir: &std::path::Path, count: i64) {
    let rows: Vec<_> = (1..=count)
        .map(|index| PgLogErrorV1 {
            ts: Ts(index * 1_000),
            severity: 2,
            category: 9,
            sqlstate: None,
            pattern: None,
            count: 1,
            sample: None,
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: None,
            username: None,
            dict_dropped_fields: 0,
        })
        .collect();
    let body = PgLogErrorV1::encode(&rows).expect("encode panic errors");
    let bytes = build_part(
        &[SectionInput {
            type_id: 1_022_001,
            rows: u32::try_from(count).expect("small fixture count"),
            body: &body,
        }],
        PartMeta {
            min_ts: 1_000,
            max_ts: count * 1_000,
            source_id: 7,
        },
    );
    std::fs::write(dir.join("0.pgm"), &bytes).expect("write panic segment");
}

#[tokio::test]
async fn overview_returns_a_digest_over_a_valid_range() {
    let (_dir, status, body) = fixture_response("/v1/timeline/overview?from=0&to=1000000000").await;
    assert_eq!(status, StatusCode::OK, "a valid range is served: {body}");

    let object = body.as_object().expect("overview body is an object");
    for field in [
        "meta",
        "event_digest",
        "notable_preview",
        "health_summary",
        "coverage",
    ] {
        assert!(object.contains_key(field), "response carries {field}");
    }

    let meta = &body["meta"];
    assert_eq!(
        meta["requested_range"],
        json!({ "from_us": 0, "to_us": 1_000_000_000 })
    );
    assert_eq!(meta["source_status"], "complete_for_contract");
    assert!(
        meta["fact_set_id"].is_string(),
        "fact set id is a base64url string"
    );
    assert!(
        meta["view_generation"].is_number(),
        "view generation is present"
    );

    let digest = &body["event_digest"];
    assert_eq!(
        digest["by_severity"]
            .as_array()
            .expect("by_severity array")
            .len(),
        5,
        "the five marginal severities are always present"
    );
    assert_eq!(
        digest["by_category"]
            .as_array()
            .expect("by_category array")
            .len(),
        11,
        "the eleven marginal categories are always present"
    );
    assert_eq!(digest["exactness"], "retained_exact");

    let notable = &body["notable_preview"];
    assert!(
        notable["observations"].is_array(),
        "notable preview is an array"
    );
    assert_eq!(notable["omitted_count"], 0);
    assert!(notable["events_query_hash"].is_string());
}

#[tokio::test]
async fn overview_rejects_an_inverted_range() {
    let (_dir, status, body) = fixture_response("/v1/timeline/overview?from=1000&to=1000").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_problem(
        &body,
        StatusCode::BAD_REQUEST,
        "invalid_query_constraint",
        json!({ "constraint": "from_before_to" }),
    );
}

#[tokio::test]
async fn overview_requires_the_range_bounds() {
    let (_dir, status, body) = fixture_response("/v1/timeline/overview?from=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "missing_query_parameter");
}

#[tokio::test]
async fn events_returns_a_machine_neutral_page() {
    let (_dir, status, body) = fixture_response("/v1/timeline/events?from=0&to=1000000000").await;
    assert_eq!(status, StatusCode::OK, "a valid range is served: {body}");
    // The bgwriter fixture retains no notable events; the page is empty and
    // final, and its exactness and policy version are still reported.
    assert_eq!(body["events"], json!([]));
    assert!(
        body["next_cursor"].is_null(),
        "an exhausted page has no cursor"
    );
    assert_eq!(body["notable_policy_version"], 1);
    assert_eq!(body["retained_exactness"], "retained_exact");
    assert_eq!(body["omitted_by_response_filter"], 0);
    assert!(body["meta"].is_object(), "events carries timeline meta");
}

#[tokio::test]
async fn events_rejects_a_forged_cursor() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/events?from=0&to=1000&cursor=not_a_real_cursor").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_cursor");
}

#[tokio::test]
async fn a_cursor_walks_the_retained_set_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 5);

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..8 {
        let uri = cursor.as_ref().map_or_else(
            || "/v1/timeline/events?from=0&to=1000000&limit=2".to_owned(),
            |token| format!("/v1/timeline/events?from=0&to=1000000&limit=2&cursor={token}"),
        );
        let (status, body) = serve(dir.path(), &uri).await;
        assert_eq!(status, StatusCode::OK, "page served: {body}");
        for event in body["events"].as_array().expect("events array") {
            seen.push(event["event_id"].as_str().expect("event id").to_owned());
        }
        match body["next_cursor"].as_str() {
            Some(token) => cursor = Some(token.to_owned()),
            None => break,
        }
    }

    assert_eq!(
        seen.len(),
        5,
        "every retained notable event is visited once"
    );
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 5, "no event is duplicated across pages");
}

#[tokio::test]
async fn parsed_panic_does_not_assert_a_trusted_health_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 1);
    let (status, body) = serve(
        dir.path(),
        "/v1/timeline/health?from=0&to=1000000&step=1000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health served: {body}");
    assert_eq!(body["health_policy_version"], 1);
    let points = body["points"].as_array().expect("points array");
    assert!(
        points
            .iter()
            .all(|point| point["overall_state"] != "critical"),
        "parsed log text cannot prove a catastrophic floor"
    );
    assert!(
        points
            .iter()
            .all(|point| point["continuous_score"].is_null()),
        "an uncovered required domain is never a false green"
    );
    assert!(
        points.iter().all(|point| point["floor_evidence"]
            .as_array()
            .expect("floor evidence")
            .is_empty()),
        "untrusted panic text is not published as floor evidence"
    );
}

#[tokio::test]
async fn health_of_an_empty_range_is_unknown_not_green() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/health?from=0&to=1000&step=1000").await;
    assert_eq!(status, StatusCode::OK, "health served: {body}");
    let points = body["points"].as_array().expect("points array");
    assert!(!points.is_empty(), "the range is partitioned into points");
    assert!(
        points
            .iter()
            .all(|point| point["overall_state"] == "unknown"),
        "no-data buckets are unknown"
    );
    assert!(
        points
            .iter()
            .all(|point| point["continuous_score"].is_null()),
        "a gap is never a false green"
    );
}

#[tokio::test]
async fn a_cursor_presented_to_a_changed_query_is_a_mismatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 3);
    let (status, first) = serve(dir.path(), "/v1/timeline/events?from=0&to=1000000&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    let cursor = first["next_cursor"].as_str().expect("a first-page cursor");
    // A cursor is bound to the query that issued it; a changed range is a 400
    // mismatch, distinct from a 410 gone view (unit-covered in the cursor codec).
    let changed = format!("/v1/timeline/events?from=0&to=2000000&limit=1&cursor={cursor}");
    let (status, body) = serve(dir.path(), &changed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "cursor_query_mismatch");
}

#[tokio::test]
async fn identical_timeline_misses_share_one_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("snapshot");
    let state = AppState::new(snapshot).expect("state");
    let key = ResponseKey {
        endpoint: Endpoint::Events,
        response_schema_version: 1,
        fact_set_id: [7; 32],
        from_us: 0,
        to_us: 10,
        step_us: None,
        notable_policy_version: 1,
        health_policy_version: 1,
        filters: "limit=2".to_owned(),
        page: None,
    };
    let leader = match state.timeline_flight(&key) {
        TimelineFlightRole::Leader(flight) => flight,
        TimelineFlightRole::Follower(_) => panic!("first request must lead"),
    };
    let follower = match state.timeline_flight(&key) {
        TimelineFlightRole::Follower(flight) => flight,
        TimelineFlightRole::Leader(_) => panic!("same key must join"),
    };
    assert!(Arc::ptr_eq(&leader, &follower));

    let expected: Arc<[u8]> = br#"{"events":[]}"#.as_slice().into();
    state.finish_timeline_flight(&key, &leader, Ok(Arc::clone(&expected)));
    assert_eq!(follower.wait().await.expect("flight result"), expected);
    assert!(matches!(
        state.timeline_flight(&key),
        TimelineFlightRole::Leader(_)
    ));
}
