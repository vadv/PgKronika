use axum::http::StatusCode;
use kronika_analytics::overview::MetricFactor;
use kronika_format::{FrameHeader, PartMeta, SectionInput, build_part};
use kronika_reader::LocalDirSnapshot;
use kronika_registry::pg_log::{PgLogErrorV1, PgLogLifecycleV1};
use kronika_registry::pg_stat_database::PgStatDatabaseV1;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{Section, Ts};
use serde_json::json;
use std::sync::Arc;

use super::{assert_problem, fixture_response, serve, serve_state, state_for_dir};
use crate::overview::cache::{Endpoint, ResponseKey};
use crate::{AppState, TimelineFlightRole};

/// Writes a sealed segment of `count` panic error groups a millisecond apart.
///
/// Each panic is a distinct notable observation, so the segment exercises
/// paginated notable retrieval.
fn write_panic_segment(dir: &std::path::Path, count: i64) {
    write_panic_segment_for(dir, "0.pgm", 7, 0, count);
}

fn write_panic_segment_for(
    dir: &std::path::Path,
    file: &str,
    source: u64,
    offset_us: i64,
    count: i64,
) {
    let rows: Vec<_> = (1..=count)
        .map(|index| PgLogErrorV1 {
            ts: Ts(offset_us + index * 1_000),
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
            min_ts: offset_us + 1_000,
            max_ts: offset_us + count * 1_000,
            source_id: source,
        },
    );
    std::fs::write(dir.join(file), &bytes).expect("write panic segment");
}

fn database_metric_row(ts_us: i64, deadlocks: i64) -> PgStatDatabaseV1 {
    PgStatDatabaseV1 {
        ts: Ts(ts_us),
        datid: 16_384,
        datname: None,
        numbackends: Some(3),
        xact_commit: 0,
        xact_rollback: 0,
        blks_read: 0,
        blks_hit: 0,
        tup_returned: 0,
        tup_fetched: 0,
        tup_inserted: 0,
        tup_updated: 0,
        tup_deleted: 0,
        conflicts: 0,
        temp_files: 0,
        temp_bytes: 0,
        deadlocks,
        blk_read_time: 0.0,
        blk_write_time: 0.0,
        stats_reset: None,
        frozen_xid_age: Some(1_000),
        min_mxid_age: Some(100),
        datconnlimit: Some(100),
        datallowconn: Some(true),
        datistemplate: Some(false),
    }
}

fn metric_reset_row(ts_us: i64) -> ResetMetadata {
    ResetMetadata {
        ts: Ts(ts_us),
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
        track_io_timing: Some(false),
        track_wal_io_timing: Some(false),
    }
}

fn database_coverage_row(ts_us: i64) -> SnapshotCoverageV1 {
    SnapshotCoverageV1 {
        ts: Ts(ts_us),
        source_type_id: 1_005_001,
        collector_pid: 42,
        collector_started_at: Ts(1),
        read_state: 0,
        visibility: 0,
        source_total: 1,
        collected: 1,
    }
}

fn write_database_metric_segment(dir: &std::path::Path) {
    let database =
        PgStatDatabaseV1::encode(&[database_metric_row(10, 7), database_metric_row(20, 8)])
            .expect("encode database metrics");
    let reset = ResetMetadata::encode(&[metric_reset_row(10), metric_reset_row(20)])
        .expect("encode reset metadata");
    let coverage =
        SnapshotCoverageV1::encode(&[database_coverage_row(10), database_coverage_row(20)])
            .expect("encode database coverage");
    let bytes = build_part(
        &[
            SectionInput {
                type_id: 1_005_001,
                rows: 2,
                body: &database,
            },
            SectionInput {
                type_id: 1_020_001,
                rows: 2,
                body: &reset,
            },
            SectionInput {
                type_id: 1_038_001,
                rows: 2,
                body: &coverage,
            },
        ],
        PartMeta {
            min_ts: 10,
            max_ts: 20,
            source_id: 7,
        },
    );
    std::fs::write(dir.join("database-metrics.pgm"), bytes).expect("write metric segment");
}

fn framed_lifecycle_part(ts_us: i64) -> Vec<u8> {
    let rows = [PgLogLifecycleV1 {
        ts: Ts(ts_us),
        kind: 0,
        pid: Some(42),
        signal: Some(9),
        shutdown_mode: None,
        message: None,
        query_detail: None,
        dict_dropped_fields: 0,
    }];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle");
    let part = build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: ts_us,
            max_ts: ts_us,
            source_id: 7,
        },
    );
    let mut framed = FrameHeader {
        part_len: u64::try_from(part.len()).expect("part length"),
    }
    .encode()
    .to_vec();
    framed.extend_from_slice(&part);
    framed
}

#[tokio::test]
async fn overview_returns_a_digest_over_a_valid_range() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/overview?source=7&from=0&to=1000000000").await;
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
    assert_eq!(digest["exactness"], "exact");

    let notable = &body["notable_preview"];
    assert!(
        notable["observations"].is_array(),
        "notable preview is an array"
    );
    assert_eq!(notable["omitted_count"], 0);
    assert!(notable["events_query_hash"].is_string());
}

#[tokio::test]
async fn timeline_meta_publishes_the_exact_incomplete_active_tail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut journal = framed_lifecycle_part(10);
    let from_offset = u64::try_from(journal.len()).expect("journal offset");
    let pending = FrameHeader { part_len: 128 }.encode();
    journal.extend_from_slice(&pending[..4]);
    let to_offset = u64::try_from(journal.len()).expect("pending offset");
    std::fs::write(dir.path().join("active.parts"), journal).expect("write active journal");

    let state = state_for_dir(dir.path());
    let (status, body) =
        serve_state(state, "/v1/timeline/events?source=7&from=0&to=100").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["meta"]["tail_pending"],
        json!({
            "from_offset_bytes": from_offset,
            "to_offset_bytes": to_offset,
        })
    );
    assert_eq!(body["events"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn overview_rejects_an_inverted_range() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/overview?source=7&from=1000&to=1000").await;
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
    let (_dir, status, body) = fixture_response("/v1/timeline/overview?source=7&from=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "missing_query_parameter");
}

#[tokio::test]
async fn timeline_requires_an_explicit_source_selection() {
    let (_dir, status, body) = fixture_response("/v1/timeline/overview?from=0&to=1000000000").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "missing_query_parameter");
    assert_eq!(body["params"]["parameter"], "source");
}

#[tokio::test]
async fn source_selection_isolated_and_repeated_event_sources_are_canonical() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment_for(dir.path(), "source-7.pgm", 7, 0, 2);
    write_panic_segment_for(dir.path(), "source-8.pgm", 8, 10_000, 3);
    let state = state_for_dir(dir.path());

    let (status, source_seven) = serve_state(
        state.clone(),
        "/v1/timeline/overview?source=7&from=0&to=1000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        source_seven["event_digest"]["retained_error_occurrence_count"],
        2
    );
    assert_eq!(source_seven["meta"]["sources"], json!([7]));

    let (status, source_eight) = serve_state(
        state.clone(),
        "/v1/timeline/overview?source=8&from=0&to=1000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        source_eight["event_digest"]["retained_error_occurrence_count"],
        3
    );
    assert_eq!(source_eight["meta"]["sources"], json!([8]));

    let (left_status, left) = serve_state(
        state.clone(),
        "/v1/timeline/events?source=8&source=7&from=0&to=1000000",
    )
    .await;
    let (right_status, right) = serve_state(
        state,
        "/v1/timeline/events?source=7&source=8&from=0&to=1000000",
    )
    .await;
    assert_eq!(left_status, StatusCode::OK);
    assert_eq!(right_status, StatusCode::OK);
    assert_eq!(left, right, "source order canonicalizes before projection");
    assert_eq!(left["meta"]["sources"], json!([7, 8]));
    assert_eq!(left["events"].as_array().expect("events").len(), 5);
}

#[tokio::test]
async fn events_returns_a_machine_neutral_page() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/events?source=7&from=0&to=1000000000").await;
    assert_eq!(status, StatusCode::OK, "a valid range is served: {body}");
    // The bgwriter fixture retains no notable events; the page is empty and
    // final, and its exactness and policy version are still reported.
    assert_eq!(body["events"], json!([]));
    assert!(
        body["next_cursor"].is_null(),
        "an exhausted page has no cursor"
    );
    assert_eq!(body["notable_policy_version"], 2);
    assert_eq!(body["retained_exactness"], "exact");
    assert_eq!(body["omitted_by_response_filter"], 0);
    assert!(body["meta"].is_object(), "events carries timeline meta");
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture verifies the same metric, fact, and coverage axes across all endpoints"
)]
async fn metric_fact_and_full_coverage_axes_reach_all_timeline_responses() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_database_metric_segment(dir.path());
    let state = state_for_dir(dir.path());
    let query = "source=7&from=10&to=21";

    let (events_status, events) =
        serve_state(state.clone(), &format!("/v1/timeline/events?{query}")).await;
    let (overview_status, overview) =
        serve_state(state.clone(), &format!("/v1/timeline/overview?{query}")).await;
    let (health_status, health) =
        serve_state(state, &format!("/v1/timeline/health?{query}&step=11")).await;
    assert_eq!(events_status, StatusCode::OK, "{events}");
    assert_eq!(overview_status, StatusCode::OK, "{overview}");
    assert_eq!(health_status, StatusCode::OK, "{health}");
    assert_eq!(events["meta"]["response_schema_version"], 2);
    assert_eq!(events["notable_policy_version"], 2);

    let facts = events["events"].as_array().expect("events");
    let [fact] = facts.as_slice() else {
        panic!("expected one canonical deadlock fact, got {facts:?}");
    };
    assert_eq!(fact["event_kind"], "pg.database.deadlock_delta");
    assert_eq!(fact["notable_class"], "deadlock_observation");
    assert_eq!(fact["source_id"], 7);
    assert!(fact["source_type_id"].is_null());
    assert_eq!(fact["identity_quality"], "content_derived");
    assert_eq!(fact["sort_ts_us"], 20);
    assert_eq!(fact["occurred_at_us"], 20);
    assert_eq!(
        fact["observed_interval"],
        json!({"from_us": 20, "to_us": 21})
    );
    assert_eq!(fact["occurrence_count"], 1);
    assert_eq!(fact["evidence_quality"], "derived_exact");
    assert_eq!(fact["quality_flags"], 0);
    assert_eq!(fact["entity"]["kind"], "database");
    assert_eq!(fact["entity"]["id"].as_str().expect("entity id").len(), 22);
    assert_eq!(
        fact["payload"],
        json!({
            "kind": "pg.database.deadlock_delta",
            "factor_id": 100,
            "delta": 1,
            "duration_us": 10,
            "reset_epoch": fact["payload"]["reset_epoch"],
        })
    );
    assert!(fact["payload"]["reset_epoch"].is_u64());
    let evidence = fact["supporting_evidence"]
        .as_array()
        .expect("supporting evidence");
    assert_eq!(evidence.len(), 2);
    for item in evidence {
        assert_eq!(
            item.as_object().expect("evidence object").len(),
            5,
            "all nullable provenance fields stay explicit"
        );
        assert_eq!(
            item["observation_id"]
                .as_str()
                .expect("observation id")
                .len(),
            43
        );
        for field in [
            "section_body_id",
            "catalog_entry_ordinal",
            "row_ordinal",
            "dictionary_context_id",
        ] {
            assert!(item[field].is_null(), "{field} is not invented: {item}");
        }
    }

    assert_eq!(overview["coverage"], events["coverage"]);
    assert_eq!(health["coverage"], events["coverage"]);
    let coverage = events["coverage"].as_array().expect("factor coverage");
    let deadlocks = coverage
        .iter()
        .find(|entry| entry["factor_id"] == 100)
        .expect("deadlock coverage");
    assert_eq!(
        deadlocks,
        &json!({
            "factor_id": 100,
            "applicability": "applicable",
            "state": "complete",
            "interval": {"from_us": 10, "to_us": 21},
            "expected_period_us": 10,
            "period_quality": "observed_stable",
            "cadence_epoch_id": deadlocks["cadence_epoch_id"],
            "crosses_cadence_boundary": false,
            "present_samples": 2,
            "covered_duration_us": 11,
            "source_population": {
                "collected": 1,
                "total": 1,
                "total_quality": "exact",
            },
            "loss_reasons": [],
            "lost_count_lower_bound": null,
            "retained_exactness": "retained_exact",
            "source_completeness": "full",
            "physical_count_semantics": "not_applicable",
            "boundary_quality": "contained",
        })
    );
    assert_eq!(
        deadlocks["cadence_epoch_id"]
            .as_str()
            .expect("cadence epoch")
            .len(),
        22
    );
    let unsupported = coverage
        .iter()
        .find(|entry| entry["factor_id"] == 900)
        .expect("explicit unsupported factor");
    assert_eq!(unsupported["applicability"], "unsupported");
    assert_eq!(unsupported["state"], "not_collected");
    assert_eq!(unsupported["present_samples"], 0);
    assert_eq!(unsupported["covered_duration_us"], 0);
}

#[tokio::test]
async fn all_supported_factor_families_reach_every_timeline_endpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("all-families.pgm"),
        kronika_reader::qualification_all_family_pgm(),
    )
    .expect("write all-family fixture");
    let state = state_for_dir(dir.path());
    let query = "source=7&from=10&to=41";

    let (overview_status, overview) =
        serve_state(state.clone(), &format!("/v1/timeline/overview?{query}")).await;
    let (events_status, events) =
        serve_state(state.clone(), &format!("/v1/timeline/events?{query}&limit=1000")).await;
    let (health_status, health) =
        serve_state(state, &format!("/v1/timeline/health?{query}&step=10")).await;
    assert_eq!(overview_status, StatusCode::OK, "{overview}");
    assert_eq!(events_status, StatusCode::OK, "{events}");
    assert_eq!(health_status, StatusCode::OK, "{health}");

    for body in [&overview, &events, &health] {
        assert_eq!(body["meta"]["sources"], json!([7]));
        assert_eq!(body["meta"]["available_sources"], json!([7]));
        assert_eq!(body["meta"]["tail_pending"], serde_json::Value::Null);
        assert_eq!(
            body["meta"]["fact_set_id"], overview["meta"]["fact_set_id"],
            "every endpoint must use the same immutable fact set"
        );
    }
    assert_eq!(overview["coverage"], events["coverage"]);
    assert_eq!(events["coverage"], health["coverage"]);

    let coverage = events["coverage"].as_array().expect("factor coverage");
    let expected_ids = MetricFactor::ALL
        .into_iter()
        .map(|factor| u64::from(factor.id().0))
        .collect::<Vec<_>>();
    let actual_ids = coverage
        .iter()
        .map(|entry| entry["factor_id"].as_u64().expect("numeric factor id"))
        .collect::<Vec<_>>();
    assert_eq!(
        actual_ids, expected_ids,
        "the HTTP coverage contract must enumerate every stable factor exactly once"
    );
    for (factor, entry) in MetricFactor::ALL.into_iter().zip(coverage) {
        let supported = factor.id().0 < 900;
        assert_eq!(
            entry["applicability"],
            if supported {
                "applicable"
            } else {
                "unsupported"
            },
            "wrong applicability for {}",
            factor.wire_code()
        );
        assert!(entry["interval"]["from_us"].is_i64());
        assert!(entry["interval"]["to_us"].is_i64());
        assert!(entry["loss_reasons"].is_array());
        assert!(entry["retained_exactness"].is_string());
        assert!(entry["source_completeness"].is_string());
        assert!(entry["physical_count_semantics"].is_string());
        if supported {
            assert!(
                entry["present_samples"]
                    .as_u64()
                    .is_some_and(|samples| samples > 0),
                "{} has no retained HTTP sample",
                factor.wire_code()
            );
        } else {
            assert_eq!(entry["state"], "not_collected");
            assert_eq!(entry["present_samples"], 0);
            assert!(entry["source_population"].is_null());
        }
    }

    let event_rows = events["events"].as_array().expect("event facts");
    assert!(!event_rows.is_empty(), "all-family events must reach HTTP");
    for event in event_rows {
        assert!(event["event_id"].is_string());
        assert!(event["event_instance_id"].is_string());
        assert!(event["event_kind"].is_string());
        assert_eq!(event["source_id"], 7);
        if let Some(factor_id) = event["payload"]["factor_id"].as_u64() {
            assert!(
                expected_ids.contains(&factor_id),
                "event payload published an unknown factor: {event}"
            );
            assert!(
                event["entity"].is_object(),
                "factor event lost its source-qualified entity: {event}"
            );
        }
    }

    let points = health["points"].as_array().expect("health points");
    assert!(!points.is_empty());
    assert!(
        points.windows(2).all(|pair| {
            pair[0]["interval"]["to_us"] == pair[1]["interval"]["from_us"]
        }),
        "health buckets must form a non-interpolated half-open partition"
    );
}

#[tokio::test]
async fn events_rejects_a_forged_cursor() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/events?source=7&from=0&to=1000&cursor=not_a_real_cursor")
            .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_cursor");
}

#[tokio::test]
async fn events_rejects_zero_and_over_maximum_page_limits() {
    for (limit, expected_code, expected_status) in [
        (0, "invalid_query_parameter", StatusCode::BAD_REQUEST),
        (1_001, "query_limit_exceeded", StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let (_dir, status, body) = fixture_response(&format!(
            "/v1/timeline/events?source=7&from=0&to=1000000000&limit={limit}"
        ))
        .await;
        assert_eq!(status, expected_status, "{body}");
        assert_eq!(body["code"], expected_code);
    }
}

#[tokio::test]
async fn a_cursor_walks_the_retained_set_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 5);
    let state = state_for_dir(dir.path());

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..8 {
        let uri = cursor.as_ref().map_or_else(
            || "/v1/timeline/events?source=7&from=0&to=1000000&limit=2".to_owned(),
            |token| {
                format!("/v1/timeline/events?source=7&from=0&to=1000000&limit=2&cursor={token}")
            },
        );
        let (status, body) = serve_state(state.clone(), &uri).await;
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
async fn preview_and_events_share_typed_fact_ids_and_canonical_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 5);
    let state = state_for_dir(dir.path());
    let (overview_status, overview) = serve_state(
        state.clone(),
        "/v1/timeline/overview?source=7&from=0&to=1000000",
    )
    .await;
    let (events_status, events) =
        serve_state(state, "/v1/timeline/events?source=7&from=0&to=1000000").await;
    assert_eq!(overview_status, StatusCode::OK);
    assert_eq!(events_status, StatusCode::OK);
    assert_eq!(
        overview["notable_preview"]["observations"], events["events"],
        "both routes use the same typed EventFact projection"
    );
    let facts = events["events"].as_array().expect("events");
    let positions = facts
        .iter()
        .map(|fact| {
            assert_eq!(fact["source_id"], 7);
            assert!(fact["payload"].is_object());
            assert!(fact["supporting_evidence"].is_array());
            (
                fact["sort_ts_us"].as_i64().expect("sort timestamp"),
                fact["event_id"].as_str().expect("event id").to_owned(),
                fact["event_instance_id"]
                    .as_str()
                    .expect("instance id")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "facts use the declared strict three-part order"
    );
}

#[tokio::test]
async fn a_capped_preview_is_the_exact_first_events_page() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 105);
    let state = state_for_dir(dir.path());
    let (overview_status, overview) = serve_state(
        state.clone(),
        "/v1/timeline/overview?source=7&from=0&to=1000000",
    )
    .await;
    let (events_status, events) = serve_state(
        state,
        "/v1/timeline/events?source=7&from=0&to=1000000&limit=100",
    )
    .await;
    assert_eq!(overview_status, StatusCode::OK, "{overview}");
    assert_eq!(events_status, StatusCode::OK, "{events}");
    assert_eq!(overview["notable_preview"]["omitted_count"], 5);
    assert_eq!(
        overview["notable_preview"]["observations"], events["events"],
        "preview uses the canonical page order, not a distinct ranking"
    );
    assert!(events["next_cursor"].is_string());
}

#[tokio::test]
async fn duplicate_segment_contents_do_not_invent_path_based_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment_for(dir.path(), "first.pgm", 7, 0, 1);
    write_panic_segment_for(dir.path(), "second.pgm", 7, 0, 1);
    let (status, body) = serve(dir.path(), "/v1/timeline/events?source=7&from=0&to=1000000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = body["events"].as_array().expect("events");
    assert_eq!(
        events.len(),
        1,
        "identical PGM content is not distinguished by its directory path"
    );
}

#[tokio::test]
async fn digest_equations_and_source_quality_reconcile_independently() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 5);
    let (status, body) = serve(
        dir.path(),
        "/v1/timeline/overview?source=7&from=0&to=1000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let digest = &body["event_digest"];
    let total = digest["retained_error_occurrence_count"]
        .as_u64()
        .expect("total");
    let sum_array = |field: &str| {
        digest[field]
            .as_array()
            .expect("count axis")
            .iter()
            .map(|value| value.as_u64().expect("count"))
            .sum::<u64>()
    };
    assert_eq!(sum_array("by_severity"), total);
    assert_eq!(sum_array("by_category"), total);
    let sqlstate_total = digest["sqlstate_missing_count"].as_u64().expect("missing")
        + digest["sqlstate_other_count"].as_u64().expect("other")
        + digest["by_sqlstate"]
            .as_array()
            .expect("sqlstate top")
            .iter()
            .map(|entry| entry["count"].as_u64().expect("count"))
            .sum::<u64>();
    assert_eq!(sqlstate_total, total);
    let joint_total = digest["joint_other_count"].as_u64().expect("joint other")
        + digest["joint_top"]
            .as_array()
            .expect("joint top")
            .iter()
            .map(|entry| entry["count"].as_u64().expect("count"))
            .sum::<u64>();
    assert_eq!(joint_total, total);
    assert_eq!(digest["retained_error_group_count"], 5);
    assert_eq!(digest["retained_observation_row_count"], 5);

    let freshness = &body["meta"]["source_freshness"][0];
    assert_eq!(freshness["source_completeness"], "bounded_subset");
    assert_eq!(freshness["retained_exactness"], "exact");
    assert_eq!(freshness["physical_count_semantics"], "lower_bound");
    assert_eq!(body["meta"]["loss"][0]["known_gaps"], json!([]));
    assert_eq!(body["meta"]["loss"][0]["dropped_count_lower_bound"], 0);
}

#[tokio::test]
async fn an_unknown_source_is_unavailable_not_an_exact_empty_population() {
    let (_dir, status, body) =
        fixture_response("/v1/timeline/events?source=999&from=0&to=1000000000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["events"], json!([]));
    assert_eq!(body["meta"]["source_status"], "unavailable");
    assert_eq!(body["retained_exactness"], "unknown");
    assert_eq!(body["source_completeness"], "unknown");
    assert_eq!(body["physical_count_semantics"], "not_applicable");
    assert!(body["meta"]["loss"][0]["dropped_count_lower_bound"].is_null());
}

#[tokio::test]
async fn a_cursor_resolves_its_pinned_view_after_a_new_publication() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment_for(dir.path(), "first.pgm", 7, 0, 5);
    let state = state_for_dir(dir.path());
    let (first_status, first) = serve_state(
        state.clone(),
        "/v1/timeline/events?source=7&from=0&to=1000000&limit=2",
    )
    .await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("continuation")
        .to_owned();

    write_panic_segment_for(dir.path(), "second.pgm", 7, 10_000, 2);
    let mut snapshot: LocalDirSnapshot = (*state.snapshot()).clone();
    let delta = snapshot.refresh_incremental_delta().expect("refresh delta");
    state
        .republish_store_view(snapshot, &delta)
        .expect("publish newer view");

    let (continued_status, continued) = serve_state(
        state.clone(),
        &format!("/v1/timeline/events?source=7&from=0&to=1000000&limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(continued_status, StatusCode::OK, "{continued}");
    assert_eq!(
        continued["meta"]["fact_set_id"], first["meta"]["fact_set_id"],
        "continuation resolves the immutable pinned generation"
    );

    let (fresh_status, fresh) = serve_state(
        state,
        "/v1/timeline/events?source=7&from=0&to=1000000&limit=100",
    )
    .await;
    assert_eq!(fresh_status, StatusCode::OK, "{fresh}");
    assert_ne!(fresh["meta"]["fact_set_id"], first["meta"]["fact_set_id"]);
    assert_eq!(fresh["events"].as_array().expect("fresh events").len(), 7);
}

#[tokio::test]
async fn parsed_panic_does_not_assert_a_trusted_health_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_panic_segment(dir.path(), 1);
    let (status, body) = serve(
        dir.path(),
        "/v1/timeline/health?source=7&from=0&to=1000000&step=1000000",
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
        fixture_response("/v1/timeline/health?source=7&from=0&to=1000&step=1000").await;
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
    let state = state_for_dir(dir.path());
    let (status, first) = serve_state(
        state.clone(),
        "/v1/timeline/events?source=7&from=0&to=1000000&limit=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cursor = first["next_cursor"].as_str().expect("a first-page cursor");
    // A cursor is bound to the query that issued it; a changed range is a 400
    // mismatch, distinct from a 410 response for an unavailable pinned view.
    let changed = format!("/v1/timeline/events?source=7&from=0&to=2000000&limit=1&cursor={cursor}");
    let (status, body) = serve_state(state.clone(), &changed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "cursor_query_mismatch");

    let changed_source =
        format!("/v1/timeline/events?source=8&from=0&to=1000000&limit=1&cursor={cursor}");
    let (status, body) = serve_state(state, &changed_source).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "cursor_query_mismatch");
}

#[tokio::test]
async fn identical_timeline_misses_share_one_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let snapshot = LocalDirSnapshot::open(dir.path()).expect("snapshot");
    let state = AppState::new(snapshot).expect("state");
    let key = ResponseKey {
        endpoint: Endpoint::Events,
        response_schema_version: 2,
        fact_set_id: [7; 32],
        from_us: 0,
        to_us: 10,
        step_us: None,
        notable_policy_version: 2,
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
