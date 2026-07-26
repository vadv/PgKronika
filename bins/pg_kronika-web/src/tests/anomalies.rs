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
async fn segments_report_compact_rows_per_name_and_skip_dictionaries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archiver = PgStatArchiver::encode(&[
        archiver_row(1_000, 1),
        archiver_row(1_100, 2),
        archiver_row(1_200, 3),
    ])
    .expect("encode archiver");
    let bgwriter = BgwriterCheckpointer::encode(&[]).expect("encode bgwriter");
    let bytes = build_part(
        &[
            SectionInput {
                type_id: 1_008_001,
                rows: 3,
                body: &archiver,
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
        "compact section rows are reported and sections order by name"
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
    assert_eq!(
        top["signal_id"], "metric.robust_window_deviation.v1",
        "every anomaly result has a stable machine id"
    );
    assert_eq!(top["section"], "pg_stat_archiver");
    assert_eq!(top["column"], "archived_count");
    assert_eq!(top["direction"], "up");
    assert_eq!(top["series"], serde_json::json!({}), "singleton series");
    assert!(
        top["peak"]["m"].as_f64().expect("m is a number") > 3.5,
        "the peak clears the default threshold"
    );
    assert_eq!(
        top["parameters"],
        serde_json::json!({
            "reference_model": "rest_of_continuous_period",
            "retrospective": true,
            "threshold": 3.5,
            "eps_abs": 0.000_001,
            "eps_rel": 0.05,
            "min_reference_points": 20,
            "min_current_points": 3,
        }),
        "the detector parameters travel with the evidence"
    );

    let counters = &body["sections"]["pg_stat_archiver"];
    assert_eq!(counters["series_total"], 1);
    assert!(counters["evaluated"].as_u64().expect("evaluated") > 0);
    // Two cumulative columns contribute one honest FirstPoint each; the
    // three all-NULL gauge columns skip every one of the 40 rows.
    assert_eq!(counters["nodata_points"], 2 + 3 * 40);
    assert_eq!(body["skipped"], serde_json::json!([]));
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["status"], "signals_detected");
    assert_eq!(body["complete"], true);
    let scanned = body["sections"]
        .as_object()
        .expect("section counters")
        .len();
    assert_eq!(body["coverage"]["sections_requested"], scanned);
    assert_eq!(body["coverage"]["sections_scanned"], scanned);
    assert_eq!(body["coverage"]["sections_skipped"], 0);
    assert_eq!(body["coverage"]["plan_sections_analyzed"], 2);
    assert_eq!(
        body["truncation"],
        serde_json::json!({
            "section_episodes_dropped": 0,
            "global_episodes_dropped": 0,
            "episodes_dropped_total": 0,
            "section_plan_signals_dropped": 0,
            "global_plan_signals_dropped": 0,
            "plan_signals_dropped_total": 0,
        })
    );
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

fn write_two_section_spike_segment(dir: &std::path::Path) -> i64 {
    const MINUTE: i64 = 60 * 1_000_000;
    let mut archived_count = 0_i64;
    let mut archiver_rows = Vec::new();
    let mut database = Vec::new();
    for minute in 0..40_i32 {
        let in_spike = (20..25).contains(&minute);
        archived_count += if in_spike { 50 } else { 1 };
        archiver_rows.push(archiver_row(i64::from(minute) * MINUTE, archived_count));

        let mut row = db_row(i64::from(minute) * MINUTE, minute);
        row.numbackends = Some(if in_spike { 100 } else { 10 });
        database.push(row);
    }
    let to = 39 * MINUTE;
    let archiver_body = PgStatArchiver::encode(&archiver_rows).expect("encode archiver");
    let database_body = PgStatDatabaseV1::encode(&database).expect("encode database");
    let bytes = build_part(
        &[
            SectionInput {
                type_id: 1_008_001,
                rows: 40,
                body: &archiver_body,
            },
            SectionInput {
                type_id: 1_005_001,
                rows: 40,
                body: &database_body,
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
async fn global_episode_truncation_is_counted_and_makes_the_result_partial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_two_section_spike_segment(dir.path());

    let uri = format!("/v1/anomalies?source=7&from=0&to={to}&window=6m&step=2m&limit=1");
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["episodes"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["status"], "partial");
    assert_eq!(body["complete"], false);
    assert_eq!(body["truncation"]["section_episodes_dropped"], 0);
    assert_eq!(body["truncation"]["global_episodes_dropped"], 1);
    assert_eq!(body["truncation"]["episodes_dropped_total"], 1);
}

const PLAN_MINUTE_US: i64 = 60 * 1_000_000;

#[derive(Debug, Clone, Copy)]
enum OsscPlanFixture {
    ComputeQueryIdDisabled,
    DistributionAndBufferShift,
    PlanSetAddition,
    EvictionAndReentry,
    MissingSystemIdentity,
    StableAcrossReset,
}

fn ossc_plan_row(
    ts: i64,
    planid: i64,
    calls: i64,
    shared_blks_read: i64,
    first_call: i64,
) -> kronika_registry::pg_store_plans::PgStorePlansOsscV1 {
    kronika_registry::pg_store_plans::PgStorePlansOsscV1 {
        ts: Ts(ts),
        queryid: 7_777,
        planid,
        userid: 10,
        dbid: 5,
        datname: None,
        usename: None,
        plan: None,
        calls,
        total_time: 0.0,
        min_time: 1.0,
        max_time: 1.0,
        mean_time: 1.0,
        stddev_time: 0.0,
        rows: calls,
        shared_blks_hit: 0,
        shared_blks_read,
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
        first_call: Ts(first_call),
        last_call: Ts(ts),
    }
}

fn plan_reset_row(
    ts: i64,
    reset_at: Option<i64>,
    extension_version: StrId,
    compute_query_id: StrId,
) -> ResetMetadata {
    ResetMetadata {
        ts: Ts(ts),
        postmaster_start_time: Ts(1),
        pg_stat_database_reset_max_at: None,
        pg_stat_statements_reset_at: None,
        pg_store_plans_reset_at: reset_at.map(Ts),
        pg_stat_bgwriter_reset_at: None,
        pg_stat_checkpointer_reset_at: None,
        pg_stat_wal_reset_at: None,
        pg_stat_archiver_reset_at: None,
        pg_stat_io_reset_at: None,
        ext_pg_stat_statements_version: None,
        ext_pg_store_plans_version: Some(extension_version),
        compute_query_id: Some(compute_query_id),
        track_io_timing: Some(true),
        track_wal_io_timing: Some(false),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end fixture constructs all four persisted provenance contracts beside the plan rows"
)]
fn write_ossc_plan_anomaly_segment(dir: &std::path::Path, fixture: OsscPlanFixture) -> i64 {
    use kronika_format::DictLimits;
    use kronika_registry::instance_metadata::InstanceMetadata;
    use kronika_registry::pg_store_plans::PgStorePlansOsscV1;
    use kronika_registry::snapshot_coverage::SnapshotCoverageV1;

    const SNAPSHOTS: i64 = 60;
    const RESET_MINUTE: i64 = 30;
    let mut interner =
        kronika_writer::Interner::new(DictLimits::new(64, 4096).expect("dictionary limits"));
    let query_id_setting = if matches!(fixture, OsscPlanFixture::ComputeQueryIdDisabled) {
        "off"
    } else {
        "auto"
    };
    let (extension_version, compute_query_id, hostname, node_self_id, kernel_version, boot_id) = {
        let mut intern = |value: &str| {
            interner
                .intern(value.as_bytes())
                .map(|id| StrId(id.get()))
                .expect("intern plan fixture string")
        };
        (
            intern("1.10"),
            intern(query_id_setting),
            intern("plan-host"),
            intern("plan-node"),
            intern("test-kernel"),
            intern("test-boot"),
        )
    };

    let set_addition = matches!(fixture, OsscPlanFixture::PlanSetAddition);
    let eviction = matches!(fixture, OsscPlanFixture::EvictionAndReentry);
    let query_id_disabled = matches!(fixture, OsscPlanFixture::ComputeQueryIdDisabled);
    let mut calls = if set_addition {
        [100_i64, 0]
    } else {
        [100, 100]
    };
    let mut shared_reads = if set_addition {
        [100_i64, 0]
    } else {
        [100, 100]
    };
    let mut first_calls = [0_i64, 0];
    let mut plan_rows = Vec::with_capacity(usize::try_from(SNAPSHOTS * 2).expect("fixture rows"));
    let mut coverage_rows =
        Vec::with_capacity(usize::try_from(SNAPSHOTS).expect("fixture coverage"));

    for minute in 0..SNAPSHOTS {
        let ts = minute * PLAN_MINUTE_US;
        if minute != 0 {
            if eviction && minute == 40 {
                // A full zero-row snapshot below proves that both entries were
                // absent; no counter delta may bridge it.
            } else if eviction && minute == 41 {
                calls = [9, 1];
                shared_reads = [9, 1];
                first_calls = [40 * PLAN_MINUTE_US + 1, 40 * PLAN_MINUTE_US + 1];
            } else if matches!(fixture, OsscPlanFixture::StableAcrossReset)
                && minute == RESET_MINUTE
            {
                calls = [0, 0];
                shared_reads = [0, 0];
                first_calls = [ts, ts];
            } else {
                let shifted = matches!(
                    fixture,
                    OsscPlanFixture::DistributionAndBufferShift | OsscPlanFixture::PlanSetAddition
                ) && (40..=50).contains(&minute);
                let call_deltas = if shifted {
                    [4, 6]
                } else if set_addition && minute < 40 {
                    [10, 0]
                } else {
                    [9, 1]
                };
                if set_addition && minute == 40 {
                    first_calls[1] = (minute - 1) * PLAN_MINUTE_US + 1;
                }
                calls[0] += call_deltas[0];
                calls[1] += call_deltas[1];
                shared_reads[0] += if shifted {
                    call_deltas[0] * 10
                } else {
                    call_deltas[0]
                };
                shared_reads[1] += call_deltas[1];
            }
        }
        let empty_snapshot = query_id_disabled || (eviction && minute == 40);
        if !empty_snapshot {
            plan_rows.push(ossc_plan_row(
                ts,
                101,
                calls[0],
                shared_reads[0],
                first_calls[0],
            ));
        }
        if !empty_snapshot && (!set_addition || minute >= 40) {
            plan_rows.push(ossc_plan_row(
                ts,
                202,
                calls[1],
                shared_reads[1],
                first_calls[1],
            ));
        }
        let plans_in_snapshot = if empty_snapshot {
            0
        } else if set_addition && minute < 40 {
            1
        } else {
            2
        };
        coverage_rows.push(SnapshotCoverageV1 {
            ts: Ts(ts),
            source_type_id: 1_003_001,
            collector_pid: 42,
            collector_started_at: Ts(0),
            read_state: 0,
            visibility: 0,
            source_total: plans_in_snapshot,
            collected: plans_in_snapshot,
        });
    }

    let reset_rows = (0..SNAPSHOTS)
        .map(|minute| {
            let reset_at = if matches!(fixture, OsscPlanFixture::StableAcrossReset)
                && minute >= RESET_MINUTE
            {
                RESET_MINUTE * PLAN_MINUTE_US
            } else {
                1
            };
            plan_reset_row(
                minute * PLAN_MINUTE_US,
                Some(reset_at),
                extension_version,
                compute_query_id,
            )
        })
        .collect::<Vec<_>>();
    let metadata = InstanceMetadata {
        ts: Ts(0),
        hostname,
        node_self_id,
        pg_version_num: 150_000,
        kernel_version,
        pg_system_identifier: (!matches!(fixture, OsscPlanFixture::MissingSystemIdentity))
            .then_some(99),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4096,
        boot_id,
        btime: Ts(0),
    };

    let dictionary = kronika_writer::dict::encode(interner.window()).expect("encode dictionary");
    let plans = PgStorePlansOsscV1::encode(&plan_rows).expect("encode plan rows");
    let coverage = SnapshotCoverageV1::encode(&coverage_rows).expect("encode plan coverage");
    let resets = ResetMetadata::encode(&reset_rows).expect("encode plan reset metadata");
    let metadata = InstanceMetadata::encode(&[metadata]).expect("encode plan instance metadata");
    let mut sections: Vec<SectionInput<'_>> = dictionary
        .iter()
        .map(|section| SectionInput {
            type_id: section.type_id,
            rows: section.rows,
            body: &section.body,
        })
        .collect();
    sections.extend([
        SectionInput {
            type_id: 1_003_001,
            rows: u32::try_from(plan_rows.len()).expect("plan row count"),
            body: &plans,
        },
        SectionInput {
            type_id: 1_038_001,
            rows: u32::try_from(coverage_rows.len()).expect("coverage row count"),
            body: &coverage,
        },
        SectionInput {
            type_id: 1_020_001,
            rows: u32::try_from(reset_rows.len()).expect("reset row count"),
            body: &resets,
        },
        SectionInput {
            type_id: 1_021_001,
            rows: 1,
            body: &metadata,
        },
    ]);
    let to = (SNAPSHOTS - 1) * PLAN_MINUTE_US;
    let bytes = build_part(
        &sections,
        PartMeta {
            min_ts: 0,
            max_ts: to,
            source_id: 7,
        },
    );
    std::fs::write(dir.join("0.pgm"), bytes).expect("write plan fixture segment");
    to
}

fn vadv_plan_row(
    ts: i64,
    queryid_stat_statements: i64,
    calls: i64,
    shared_blks_read: i64,
) -> kronika_registry::pg_store_plans::PgStorePlansVadvV1 {
    kronika_registry::pg_store_plans::PgStorePlansVadvV1 {
        ts: Ts(ts),
        queryid_stat_statements,
        planid: 101,
        userid: 10,
        dbid: 5,
        datname: None,
        usename: None,
        plan: None,
        calls,
        slow_log_calls: 0,
        total_time: 0.0,
        min_time: 1.0,
        max_time: 1.0,
        mean_time: 1.0,
        stddev_time: 0.0,
        rows: calls,
        shared_blks_hit: 0,
        shared_blks_read,
        shared_blks_dirtied: 0,
        shared_blks_written: 0,
        local_blks_hit: 0,
        local_blks_read: 0,
        local_blks_dirtied: 0,
        local_blks_written: 0,
        temp_blks_read: 0,
        temp_blks_written: 0,
        blk_read_time: 0.0,
        blk_write_time: 0.0,
        first_call: Ts(0),
        last_call: Ts(ts),
        total_plan_time: 0.0,
        min_plan_time: 0.0,
        max_plan_time: 0.0,
        mean_plan_time: 0.0,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the vadv end-to-end fixture co-locates fork-specific rows with all analyzer provenance sections"
)]
fn write_vadv_plan_anomaly_segment(dir: &std::path::Path) -> i64 {
    use kronika_format::DictLimits;
    use kronika_registry::instance_metadata::InstanceMetadata;
    use kronika_registry::pg_store_plans::PgStorePlansVadvV1;
    use kronika_registry::snapshot_coverage::SnapshotCoverageV1;

    const SNAPSHOTS: i64 = 60;
    let mut interner =
        kronika_writer::Interner::new(DictLimits::new(64, 4096).expect("dictionary limits"));
    let (extension_version, compute_query_id, hostname, node_self_id, kernel_version, boot_id) = {
        let mut intern = |value: &str| {
            interner
                .intern(value.as_bytes())
                .map(|id| StrId(id.get()))
                .expect("intern vadv fixture string")
        };
        (
            intern("2.1"),
            intern("auto"),
            intern("vadv-host"),
            intern("vadv-node"),
            intern("test-kernel"),
            intern("test-boot"),
        )
    };

    let mut calls = 100_i64;
    let mut shared_reads = 100_i64;
    let mut plan_rows = Vec::with_capacity(usize::try_from(SNAPSHOTS).expect("fixture rows"));
    let mut coverage_rows =
        Vec::with_capacity(usize::try_from(SNAPSHOTS).expect("fixture coverage"));
    for minute in 0..SNAPSHOTS {
        let ts = minute * PLAN_MINUTE_US;
        if minute != 0 {
            calls += 10;
            shared_reads += if (40..=50).contains(&minute) { 100 } else { 10 };
        }
        let queryid_stat_statements = if minute < 30 { 7_777 } else { 8_888 };
        plan_rows.push(vadv_plan_row(
            ts,
            queryid_stat_statements,
            calls,
            shared_reads,
        ));
        coverage_rows.push(SnapshotCoverageV1 {
            ts: Ts(ts),
            source_type_id: 1_004_001,
            collector_pid: 42,
            collector_started_at: Ts(0),
            read_state: 0,
            visibility: 0,
            source_total: 1,
            collected: 1,
        });
    }
    let resets = (0..SNAPSHOTS)
        .map(|minute| {
            plan_reset_row(
                minute * PLAN_MINUTE_US,
                None,
                extension_version,
                compute_query_id,
            )
        })
        .collect::<Vec<_>>();
    let metadata = InstanceMetadata {
        ts: Ts(0),
        hostname,
        node_self_id,
        pg_version_num: 170_000,
        kernel_version,
        pg_system_identifier: Some(99),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4096,
        boot_id,
        btime: Ts(0),
    };

    let dictionary = kronika_writer::dict::encode(interner.window()).expect("encode dictionary");
    let plans = PgStorePlansVadvV1::encode(&plan_rows).expect("encode vadv plan rows");
    let coverage = SnapshotCoverageV1::encode(&coverage_rows).expect("encode plan coverage");
    let resets_body = ResetMetadata::encode(&resets).expect("encode plan reset metadata");
    let metadata = InstanceMetadata::encode(&[metadata]).expect("encode plan instance metadata");
    let mut sections: Vec<SectionInput<'_>> = dictionary
        .iter()
        .map(|section| SectionInput {
            type_id: section.type_id,
            rows: section.rows,
            body: &section.body,
        })
        .collect();
    sections.extend([
        SectionInput {
            type_id: 1_004_001,
            rows: u32::try_from(plan_rows.len()).expect("plan row count"),
            body: &plans,
        },
        SectionInput {
            type_id: 1_038_001,
            rows: u32::try_from(coverage_rows.len()).expect("coverage row count"),
            body: &coverage,
        },
        SectionInput {
            type_id: 1_020_001,
            rows: u32::try_from(resets.len()).expect("reset row count"),
            body: &resets_body,
        },
        SectionInput {
            type_id: 1_021_001,
            rows: 1,
            body: &metadata,
        },
    ]);
    let to = (SNAPSHOTS - 1) * PLAN_MINUTE_US;
    let bytes = build_part(
        &sections,
        PartMeta {
            min_ts: 0,
            max_ts: to,
            source_id: 7,
        },
    );
    std::fs::write(dir.join("0.pgm"), bytes).expect("write vadv plan fixture segment");
    to
}

#[tokio::test]
async fn plan_signals_follow_registry_storage_diff_and_http_with_typed_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to =
        write_ossc_plan_anomaly_segment(dir.path(), OsscPlanFixture::DistributionAndBufferShift);
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    let signals = body["plan_signals"].as_array().expect("plan_signals array");
    let distribution = signals
        .iter()
        .find(|signal| signal["signal_id"] == "pg.query.plan_distribution_shift.v1")
        .expect("distribution signal");
    assert_eq!(
        distribution["scope"],
        serde_json::json!({
            "dbid": 5,
            "userid": 10,
            "queryid": 7_777,
            "query_identity": "dbid_userid_core_queryid",
            "query_text_used": false,
        })
    );
    assert_eq!(distribution["parameters"]["count_basis"], "calls_delta");
    assert!(
        distribution["evidence"]["total_variation"]
            .as_f64()
            .expect("total variation")
            >= 0.20
    );
    assert_eq!(
        distribution["evidence"]["plans"].as_array().map(Vec::len),
        Some(2)
    );

    let buffer = signals
        .iter()
        .find(|signal| {
            signal["signal_id"] == "pg.plan.buffer_work_per_call_increase.v1"
                && signal["scope"]["planid"] == 101
                && signal["dimension"]["column"] == "shared_blks_read"
        })
        .expect("same-plan shared-read signal");
    assert_eq!(buffer["scope"]["queryid"], 7_777);
    assert_eq!(buffer["parameters"]["normalization"], "calls_delta");
    assert_eq!(buffer["dimension"]["unit"], "blocks_per_call");
    assert!(
        buffer["evidence"]["current_blocks_per_call"]
            .as_f64()
            .expect("current blocks per call")
            > buffer["evidence"]["reference_blocks_per_call"]
                .as_f64()
                .expect("reference blocks per call")
    );
    assert_eq!(
        buffer["interpretation"],
        "observed_same_plan_association_not_causation"
    );

    assert_eq!(body["status"], "signals_detected");
    assert_eq!(body["complete"], true);
    assert_eq!(body["coverage"]["plan_sections_analyzed"], 1);
    assert!(
        body["coverage"]["plan_positions_evaluated"]
            .as_u64()
            .expect("evaluated plan positions")
            > 0
    );
    assert_eq!(
        body["plan_analysis"]["pg_store_plans_ossc"]["status"],
        "complete"
    );
    assert_eq!(
        body["plan_analysis"]["pg_store_plans_ossc"]["quality"]["full_snapshots"],
        60
    );
}

#[tokio::test]
async fn vadv_exposes_same_plan_buffers_without_claiming_query_mixture_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_vadv_plan_anomaly_segment(dir.path());
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_vadv"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    let signals = body["plan_signals"].as_array().expect("plan signals");
    assert!(
        signals
            .iter()
            .all(|signal| signal["signal_id"] != "pg.query.plan_distribution_shift.v1")
    );
    let buffer = signals
        .iter()
        .find(|signal| {
            signal["signal_id"] == "pg.plan.buffer_work_per_call_increase.v1"
                && signal["scope"]["planid"] == 101
                && signal["dimension"]["column"] == "shared_blks_read"
        })
        .expect("vadv same-plan buffer signal");
    assert_eq!(buffer["scope"]["queryid"], serde_json::Value::Null);
    assert_eq!(buffer["scope"]["plan_identity"], "dbid_userid_planid");
    assert_eq!(buffer["scope"]["query_attribution"], "unavailable");
    let analysis = &body["plan_analysis"]["pg_store_plans_vadv"];
    assert_eq!(analysis["status"], "complete");
    assert_eq!(
        analysis["applicability"]["plan_distribution"],
        "not_applicable_queryid_not_in_identity"
    );
    assert_eq!(
        analysis["distribution"]["not_evaluated"]["not_applicable"],
        1
    );
}

#[tokio::test]
async fn plan_distribution_uses_calls_since_first_observation_for_a_new_plan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_ossc_plan_anomaly_segment(dir.path(), OsscPlanFixture::PlanSetAddition);
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    let distribution = body["plan_signals"]
        .as_array()
        .expect("plan signals")
        .iter()
        .find(|signal| signal["signal_id"] == "pg.query.plan_distribution_shift.v1")
        .expect("plan-set change produces distribution evidence");
    assert_eq!(
        distribution["evidence"]["current_newly_observed_planids"],
        serde_json::json!([202])
    );
    assert_eq!(
        body["plan_analysis"]["pg_store_plans_ossc"]["quality"]["plan_set_additions"],
        1
    );
    assert_eq!(
        body["plan_analysis"]["pg_store_plans_ossc"]["quality"]["membership_boundaries"],
        0
    );
}

#[tokio::test]
async fn full_empty_snapshot_breaks_evicted_plan_counters_before_reentry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_ossc_plan_anomaly_segment(dir.path(), OsscPlanFixture::EvictionAndReentry);
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plan_signals"], serde_json::json!([]));
    let quality = &body["plan_analysis"]["pg_store_plans_ossc"]["quality"];
    assert_eq!(quality["plan_set_removals"], 2);
    assert_eq!(quality["plan_set_additions"], 2);
    assert!(
        quality["membership_boundaries"]
            .as_u64()
            .expect("membership boundaries")
            > 0
    );
}

#[tokio::test]
async fn compute_query_id_off_is_explicitly_not_applicable_for_ossc_distribution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_ossc_plan_anomaly_segment(dir.path(), OsscPlanFixture::ComputeQueryIdDisabled);
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plan_signals"], serde_json::json!([]));
    let analysis = &body["plan_analysis"]["pg_store_plans_ossc"];
    assert_eq!(analysis["status"], "complete");
    assert_eq!(
        analysis["applicability"]["plan_distribution"],
        "not_applicable_compute_query_id_disabled"
    );
    assert_eq!(
        analysis["distribution"]["not_evaluated"]["not_applicable"],
        1
    );
}

#[tokio::test]
async fn missing_system_identifier_makes_plan_evidence_partial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_ossc_plan_anomaly_segment(dir.path(), OsscPlanFixture::MissingSystemIdentity);
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "partial");
    assert_eq!(body["complete"], false);
    assert_eq!(body["plan_signals"], serde_json::json!([]));
    let analysis = &body["plan_analysis"]["pg_store_plans_ossc"];
    assert_eq!(analysis["status"], "partial");
    assert!(
        analysis["quality"]["instance_identity_unavailable_intervals"]
            .as_u64()
            .expect("missing system identity count")
            > 0
    );
}

#[tokio::test]
async fn plan_signals_do_not_bridge_a_real_counter_reset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = write_ossc_plan_anomaly_segment(dir.path(), OsscPlanFixture::StableAcrossReset);
    let uri = format!(
        "/v1/anomalies?source=7&from=0&to={to}&window=10m&step=2m&limit=200&section=pg_store_plans_ossc"
    );
    let (status, body) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plan_signals"], serde_json::json!([]));
    assert!(
        body["plan_analysis"]["pg_store_plans_ossc"]["quality"]["reset_boundaries"]
            .as_u64()
            .expect("reset boundary count")
            > 0
    );
    assert!(
        body["plan_analysis"]["pg_store_plans_ossc"]["distribution"]["not_evaluated"]
            ["discontinuity"]
            .as_u64()
            .expect("distribution discontinuities")
            > 0
    );
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
    ] {
        let (status, _body) = serve(dir.path(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} must be rejected");
    }

    let (status, body) = serve(
        dir.path(),
        "/v1/anomalies?source=7&from=0&to=900000000000000000&window=1h&step=1s",
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_problem(
        &body,
        status,
        "query_limit_exceeded",
        serde_json::json!({
            "resource": "window_positions",
            "limit": 10_000,
            "observed": 899_999_996_402_u64,
        }),
    );

    let (status, body) = serve(
        dir.path(),
        "/v1/anomalies?source=7&from=0&to=9000000000&section=nope",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown section is a 404");
    assert_problem(
        &body,
        status,
        "unknown_section",
        serde_json::json!({ "section": "nope" }),
    );
}
