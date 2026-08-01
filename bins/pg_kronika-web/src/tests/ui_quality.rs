use kronika_format::DictLimits;
use kronika_layout::{ProducerStatus, RetentionStatus, write_producer_status};
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::pg_store_plans::PgStorePlansOsscV1;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_writer::{Interner, dict};

use super::*;

fn quality_fixture() -> tempfile::TempDir {
    quality_fixture_with(Some(
        ProducerStatus::running(42, 500, 1_600, Some(RetentionStatus::fixed(1 << 30)))
            .stopped(1_700),
    ))
}

fn quality_fixture_with(status: Option<ProducerStatus>) -> tempfile::TempDir {
    let rows = [
        PgLogLifecycleV1 {
            ts: Ts(1_200),
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
    ];
    let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle");
    let bytes = build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: 2,
            body: &body,
        }],
        PartMeta {
            min_ts: 1_200,
            max_ts: 1_600,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "quality.pgm", &bytes);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish quality web index");
    }
    if let Some(status) = status {
        write_producer_status(directory.path(), &status).expect("publish producer status");
    }
    directory
}

#[tokio::test]
async fn data_quality_distinguishes_gaps_and_proven_stopped_producer() {
    let directory = quality_fixture();
    let (status, body) = serve(directory.path(), "/v1/data/quality?from=1000&to=2000").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["producer"]["state"], "stopped");
    assert_eq!(body["coverage"]["observed_snapshots"], 2);
    assert_eq!(body["gaps"][0]["reason"], "unknown");
    assert_eq!(body["freshness"]["data_through_us"], "1600");
    assert_eq!(body["status"], "partial");
}

#[tokio::test]
async fn data_quality_reports_stale_data_with_running_producer() {
    let directory = quality_fixture_with(Some(ProducerStatus::running(42, 500, 99_900_000, None)));
    let (status, body) = serve(directory.path(), "/v1/data/quality?from=1000&to=100000000").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["producer"]["state"], "running");
    assert_eq!(body["freshness"]["state"], "stale");
    assert_eq!(body["status"], "stale");
}

#[tokio::test]
async fn data_quality_surfaces_permission_degradation_at_the_top_status() {
    let directory = quality_coverage_fixture(0);
    let (status, body) = serve(directory.path(), "/v1/data/quality?from=1200&to=2000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "fresh");

    let directory = quality_coverage_fixture(2);
    let (status, body) = serve(directory.path(), "/v1/data/quality?from=1200&to=2000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "partial");
}

fn quality_coverage_fixture(later_read_state: u8) -> tempfile::TempDir {
    let plan_row = |ts: i64, calls: i64| PgStorePlansOsscV1 {
        ts: Ts(ts),
        queryid: 7,
        planid: 99,
        userid: 10,
        dbid: 20,
        datname: None,
        usename: None,
        plan: None,
        calls,
        total_time: f64::from(i32::try_from(calls).expect("fixture calls fit i32")) * 5.0,
        min_time: 0.0,
        max_time: 0.0,
        mean_time: 0.0,
        stddev_time: 0.0,
        rows: calls,
        shared_blks_hit: 0,
        shared_blks_read: 0,
        shared_blks_dirtied: 0,
        shared_blks_written: 0,
        local_blks_hit: 0,
        local_blks_read: 0,
        local_blks_dirtied: 0,
        local_blks_written: 0,
        temp_blks_read: 0,
        temp_blks_written: 0,
        shared_blk_read_time: 0.0,
        shared_blk_write_time: 0.0,
        local_blk_read_time: 0.0,
        local_blk_write_time: 0.0,
        temp_blk_read_time: 0.0,
        temp_blk_write_time: 0.0,
        first_call: Ts(1_000),
        last_call: Ts(ts),
    };
    let coverage_row = |ts: i64, read_state: u8, visibility: u8| SnapshotCoverageV1 {
        ts: Ts(ts),
        section_type_id: 1_003_001,
        collector_pid: 42,
        collector_started_at: Ts(1),
        read_state,
        visibility,
        source_total: if read_state == 0 { 1 } else { 10 },
        collected: 1,
    };
    let plans = PgStorePlansOsscV1::encode(&[plan_row(1_200, 3), plan_row(1_600, 4)])
        .expect("encode plans");
    let coverage = SnapshotCoverageV1::encode(&[
        coverage_row(1_200, 0, 0),
        coverage_row(1_600, later_read_state, u8::from(later_read_state != 0)),
    ])
    .expect("encode coverage");
    let interner = Interner::new(DictLimits::new(16, 4_096).expect("dictionary limits"));
    let dictionary = dict::encode(interner.window()).expect("encode dictionary");
    let mut sections = vec![
        SectionInput {
            type_id: 1_003_001,
            rows: 2,
            body: &plans,
        },
        SectionInput {
            type_id: 1_038_001,
            rows: 2,
            body: &coverage,
        },
    ];
    sections.extend(dictionary.iter().map(|section| SectionInput {
        type_id: section.type_id,
        rows: section.rows,
        body: &section.body,
    }));
    sections.sort_unstable_by_key(|section| section.type_id);
    let bytes = build_part(
        &sections,
        PartMeta {
            min_ts: 1_200,
            max_ts: 1_600,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "quality-coverage.pgm", &bytes);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish quality web index");
    }
    directory
}

#[tokio::test]
async fn data_quality_rejects_invalid_query_shapes_before_index_reads() {
    for uri in [
        "/v1/data/quality",
        "/v1/data/quality?from=1",
        "/v1/data/quality?to=1",
        "/v1/data/quality?from=1&from=2&to=3",
        "/v1/data/quality?from=1&to=2&source=legacy",
        "/v1/data/quality?from=2&to=1",
        "/v1/data/quality?from=0&to=90000000000",
    ] {
        let (_directory, status, body) = fixture_response(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
}
