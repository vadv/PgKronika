use super::*;

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
    let archiver_b = PgStatArchiver::encode(&[archiver_row(1_200, 3)]).expect("encode archiver");
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
    let rows: Vec<PgStatDatabaseV1> = (0..4).map(|i| db_row(i64::from(i) * MINUTE, i)).collect();
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

    let uri = format!("/v1/anomalies?source=7&from=0&to={to}&window=1m&section=pg_stat_database");
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
