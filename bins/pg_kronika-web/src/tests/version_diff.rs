use super::*;

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
