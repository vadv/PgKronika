use super::*;

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
    assert_problem(
        &body,
        status,
        "missing_query_parameter",
        serde_json::json!({ "parameter": "source" }),
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
    assert_problem(
        &body,
        status,
        "invalid_query_parameter",
        serde_json::json!({ "parameter": "source", "expected": "uint64" }),
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
    assert_problem(
        &body,
        status,
        "unknown_section",
        serde_json::json!({ "section": "does_not_exist" }),
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
    assert_problem(
        &body,
        status,
        "invalid_query_parameter",
        serde_json::json!({ "parameter": "source", "expected": "uint64" }),
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
        &format!("/v1/section/pg_stat_archiver?source=7&from=0&to=5000&limit=1&cursor={cursor}"),
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
    assert_problem(&body, status, "invalid_cursor", serde_json::json!({}));
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

    let (status, body) = serve(dir.path(), "/v1/sections/batch?source=7&from=1000&to=2000").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "batch without names is a client error"
    );
    assert_problem(
        &body,
        status,
        "missing_query_parameter",
        serde_json::json!({ "parameter": "names" }),
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
    assert_problem(
        &body,
        status,
        "invalid_query_parameter",
        serde_json::json!({ "parameter": "names", "expected": "section_list" }),
    );
}
