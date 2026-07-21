use super::*;

fn write_archiver_with_node(
    dir: &std::path::Path,
    file: &str,
    node_self_id: &str,
    rows: &[PgStatArchiver],
    min_ts: i64,
    max_ts: i64,
) {
    let archiver = PgStatArchiver::encode(rows).expect("encode archiver");
    write_section_with_node(
        dir,
        file,
        node_self_id,
        1_008_001,
        u32::try_from(rows.len()).expect("fixture row count"),
        &archiver,
        min_ts,
        max_ts,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "fixture helper mirrors SectionInput and PartMeta fields"
)]
fn write_section_with_node(
    dir: &std::path::Path,
    file: &str,
    node_self_id: &str,
    type_id: u32,
    rows: u32,
    body: &[u8],
    min_ts: i64,
    max_ts: i64,
) {
    use kronika_format::DictLimits;
    use kronika_registry::instance_metadata::InstanceMetadata;

    let mut interner =
        kronika_writer::Interner::new(DictLimits::new(4096, 1 << 20).expect("dictionary limits"));
    let mut intern = |value: &str| {
        interner
            .intern(value.as_bytes())
            .map(|id| StrId(id.get()))
            .expect("intern fixture identity")
    };
    let metadata = InstanceMetadata {
        ts: Ts(min_ts),
        hostname: intern("db-host-7"),
        node_self_id: intern(node_self_id),
        pg_version_num: 170_000,
        kernel_version: intern("test-kernel"),
        pg_system_identifier: Some(7),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4096,
        boot_id: intern("test-boot"),
        btime: Ts(0),
    };
    for value in [
        "fixture-1",
        "fixture-2",
        "fixture-3",
        "fixture-4",
        "fixture-5",
    ] {
        let _ = intern(value);
    }
    let dictionary = kronika_writer::dict::encode(interner.window()).expect("encode dictionary");
    let metadata = InstanceMetadata::encode(&[metadata]).expect("encode metadata");
    let mut sections: Vec<SectionInput<'_>> = dictionary
        .iter()
        .map(|section| SectionInput {
            type_id: section.type_id,
            rows: section.rows,
            body: &section.body,
        })
        .collect();
    sections.push(SectionInput {
        type_id,
        rows,
        body,
    });
    sections.push(SectionInput {
        type_id: 1_021_001,
        rows: 1,
        body: &metadata,
    });
    let bytes = build_part(
        &sections,
        PartMeta {
            min_ts,
            max_ts,
            source_id: 7,
        },
    );
    std::fs::write(dir.join(file), bytes).expect("write segment");
}

fn fixture_str_id(value: &str) -> StrId {
    use kronika_format::DictLimits;
    let mut interner =
        kronika_writer::Interner::new(DictLimits::new(32, 4096).expect("dictionary limits"));
    interner
        .intern(value.as_bytes())
        .map(|id| StrId(id.get()))
        .expect("fixture string id")
}

fn write_archiver_with_identity(
    dir: &std::path::Path,
    rows: &[PgStatArchiver],
    min_ts: i64,
    max_ts: i64,
) {
    write_archiver_with_node(dir, "0.pgm", "node-7", rows, min_ts, max_ts);
}

fn archiver_rows(spiking: bool) -> Vec<PgStatArchiver> {
    const MINUTE: i64 = 60 * 1_000_000;
    let mut rows = Vec::new();
    let mut count = 0;
    for minute in 0..40 {
        count += if spiking && (20..25).contains(&minute) {
            50
        } else {
            1
        };
        rows.push(archiver_row(minute * MINUTE, count));
    }
    rows
}

/// The active lens ids the incidents endpoint advertises, in catalog order.
const ACTIVE_LENS_IDS: &[&str] = &[
    "PG-CACHE-010",
    "PG-WAL-009",
    "PG-TEMP-003",
    "PG-CHKPT-008",
    "PG-IO-011",
    "PG-HOT-007",
    "PG-ARCH-017",
    "OS-NET-028",
    "OS-CGRP-021",
    "PG-ANALYZE-004",
    "PG-CONN-014",
    "OS-MEM-022",
    "OS-WB-025",
    "PG-VACUUM-005",
    "PG-FREEZE-006",
    "PG-REPL-015",
    "PG-SLOT-016",
    "OS-CGMEM-023",
    "OS-FS-027",
    "PG-QRY-001",
    "PG-PLAN-002",
    "OS-CPU-020",
    "OS-BLOCK-024",
    "OS-IOWHO-026",
    "PG-HORIZON-013",
    "PG-SYNC-018",
    "PG-WAIT-019",
    "PG-LOCK-012",
    "PG-EVT-001",
    "PG-EVT-002",
    "PG-EVT-003",
    "PG-EVT-005",
    "PG-EVT-007",
    "PG-EVT-008",
    "PG-EVT-009",
    "PG-EVT-010",
    "PG-EVT-011",
    "PG-EVT-012",
    "PG-EVT-013",
    "PG-EVT-014",
];

async fn assert_calm_incidents(uri: &str, to: i64) {
    let calm = tempfile::tempdir().expect("tempdir");
    write_archiver_with_identity(calm.path(), &archiver_rows(false), 0, to);
    let (status, body) = serve(calm.path(), uri).await;
    assert_eq!(status, StatusCode::OK, "calm 200; got {status}: {body}");
    assert_eq!(
        body["incidents"],
        serde_json::json!([]),
        "no anomaly means no incident"
    );
    assert_eq!(body["analysis_status"], "calm");
    assert_eq!(body["clustering_complete"], true);
    assert_eq!(body["complete"], false);
    for field in ["catalog", "data_quality", "skipped", "coverage_by_section"] {
        assert!(
            body.get(field).is_some(),
            "an empty response still carries {field}"
        );
    }
}

#[tokio::test]
async fn incidents_surface_a_spike_and_stay_empty_when_calm() {
    let to = 39 * 60 * 1_000_000;
    let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m");

    let spiking = tempfile::tempdir().expect("tempdir");
    let spike_rows = archiver_rows(true);
    write_archiver_with_node(
        spiking.path(),
        "0.pgm",
        "node-7",
        &spike_rows[..21],
        0,
        20 * 60 * 1_000_000,
    );
    write_archiver_with_node(
        spiking.path(),
        "1.pgm",
        "node-7",
        &spike_rows[20..],
        20 * 60 * 1_000_000,
        to,
    );
    let (status, body) = serve(spiking.path(), &uri).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "incidents 200; got {status}: {body}"
    );

    for field in [
        "complete",
        "clustering_complete",
        "analysis_status",
        "incidents",
        "coverage_by_section",
        "data_age_seconds",
        "catalog",
        "data_quality",
        "skipped",
    ] {
        assert!(body.get(field).is_some(), "response carries {field}");
    }
    assert_eq!(body["complete"], false);
    assert_eq!(body["clustering_complete"], true);
    assert_eq!(body["analysis_status"], "incidents_detected");
    assert_eq!(body["catalog"]["status"], "partial");
    assert_eq!(body["catalog"]["diagnosis_available"], true);
    assert_eq!(
        body["catalog"]["applied"],
        serde_json::json!(ACTIVE_LENS_IDS)
    );
    let dormant = body["catalog"]["dormant"]
        .as_array()
        .expect("catalog lists dormant lenses");
    assert_eq!(dormant.len(), 0, "all 28 core lenses are active");
    assert!(
        dormant
            .iter()
            .all(|entry| entry["lens_id"] != "PG-LOCK-012"),
        "the lock lens is now active, not dormant"
    );
    assert!(dormant.is_empty());
    let incidents = body["incidents"].as_array().expect("incidents is an array");
    assert!(
        !incidents.is_empty(),
        "the spike must cluster into an incident"
    );
    assert_eq!(incidents[0]["findings"], serde_json::json!([]));
    assert_eq!(incidents[0]["evaluation_complete"], true);
    assert_eq!(incidents[0]["finding_evaluation_status"], "complete");
    let members = incidents[0]["members"]
        .as_array()
        .expect("members is an array");
    assert!(
        members.iter().any(|member| {
            member["logical_section"] == "pg_stat_archiver" && member["column"] == "archived_count"
        }),
        "an incident member is the real archiver spike series"
    );

    assert_calm_incidents(&uri, to).await;
}

#[tokio::test]
async fn incidents_publish_numeric_gauge_evidence_from_reader_input() {
    const MINUTE: i64 = 60 * 1_000_000;
    let rows: Vec<OsMeminfo> = (0..40)
        .map(|minute| OsMeminfo {
            ts: Ts(i64::from(minute) * MINUTE),
            mem_total: 1_000_000,
            mem_free: None,
            mem_available: Some(if (20..25).contains(&minute) {
                10_000
            } else {
                500_000
            }),
            buffers: None,
            cached: None,
            swap_total: None,
            swap_free: None,
            active: None,
            inactive: None,
            dirty: Some(1_000),
            writeback: Some(500),
            slab: None,
            s_reclaimable: None,
            s_unreclaim: None,
            anon_pages: None,
            mapped: None,
            shmem: None,
            page_tables: None,
            commit_limit: None,
            committed_as: None,
            huge_pages_total: None,
            huge_pages_free: None,
            hugepagesize: None,
            scope: 0,
        })
        .collect();
    let body = OsMeminfo::encode(&rows).expect("encode os_meminfo");
    let dir = tempfile::tempdir().expect("tempdir");
    let to = 39 * MINUTE;
    write_section_with_node(
        dir.path(),
        "0.pgm",
        "node-7",
        1_104_001,
        u32::try_from(rows.len()).expect("row count"),
        &body,
        0,
        to,
    );

    let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m&section=os_meminfo");
    let (status, response) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let finding = response["incidents"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|incident| incident["findings"].as_array().into_iter().flatten())
        .find(|finding| finding["lens_id"] == "OS-MEM-022")
        .expect("low MemAvailable finding");
    assert_eq!(finding["role"], "coincident");
    assert_eq!(finding["confidence"], "low");
    assert_eq!(finding["scope"]["identity"], serde_json::json!([]));
    let evidence = &finding["evidence"][0];
    assert_eq!(evidence["type"], "gauge");
    assert_eq!(evidence["claim"], "observed_threshold_crossing");
    assert_eq!(evidence["measurement"]["kind"], "ratio");
    assert_eq!(evidence["measurement"]["numerator_name"], "mem_available");
    assert_eq!(evidence["measurement"]["numerator"], 10_000.0);
    assert_eq!(evidence["measurement"]["numerator_unit"], "KiB");
    assert_eq!(evidence["measurement"]["denominator_name"], "mem_total");
    assert_eq!(evidence["measurement"]["denominator"], 1_000_000.0);
    assert_eq!(evidence["measurement"]["denominator_unit"], "KiB");
    assert_eq!(evidence["measurement"]["value"], 0.01);
    assert_eq!(evidence["measurement"]["headroom"], 990_000.0);
    assert_eq!(evidence["measurement"]["operand_unit"], "KiB");
    assert_eq!(evidence["unit"], "ratio");
    assert_eq!(evidence["threshold"]["operator"], "below");
    assert_eq!(evidence["threshold"]["value"], 0.05);
    assert!(evidence["observed_at_us"].is_i64());
    assert!(
        evidence["sample_count"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );
    assert_eq!(evidence["entity"]["logical_section"], "os_meminfo");
    assert_eq!(evidence["entity"]["identity"], serde_json::json!([]));
}

async fn assert_contract_lens(
    type_id: u32,
    section: &str,
    lens_id: &str,
    rows: usize,
    body: &[u8],
    to: i64,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    write_section_with_node(
        dir.path(),
        "0.pgm",
        "node-7",
        type_id,
        u32::try_from(rows).expect("row count"),
        body,
        0,
        to,
    );
    let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m&section={section}");
    let (status, response) = serve(dir.path(), &uri).await;
    assert_eq!(status, StatusCode::OK, "{section}: {response}");
    assert!(
        response["incidents"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|incident| incident["findings"].as_array().into_iter().flatten())
            .any(|finding| finding["lens_id"] == lens_id),
        "{section} must reach {lens_id}: {response}"
    );
    let capability = response["catalog"]["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["lens_id"] == lens_id)
        .expect("capability entry");
    assert_eq!(capability["status"], "available");
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one integration test exercises six compact versioned row fixtures through HTTP"
)]
async fn six_versioned_gauge_contracts_reach_http_findings() {
    const MINUTE: i64 = 60 * 1_000_000;
    let to = 39 * MINUTE;
    let spike = |minute: i32| (20..25).contains(&minute);

    let freeze: Vec<_> = (0..40)
        .map(|minute| PgFreezeHorizonV1 {
            ts: Ts(i64::from(minute) * MINUTE),
            datid: 7,
            datname: fixture_str_id("fixture-1"),
            relid: 42,
            schemaname: fixture_str_id("fixture-2"),
            relname: fixture_str_id("fixture-3"),
            xid_age: if spike(minute) { 95 } else { 10 },
            xid_limit: 100,
            xid_is_toast: false,
            mxid_age: 10,
            mxid_limit: 100,
            mxid_is_toast: false,
        })
        .collect();
    let body = PgFreezeHorizonV1::encode(&freeze).expect("freeze encode");
    assert_contract_lens(
        1_031_001,
        "pg_freeze_horizon",
        "PG-FREEZE-006",
        freeze.len(),
        &body,
        to,
    )
    .await;

    let vacuum: Vec<_> = (0..40)
        .map(|minute| PgVacuumObservationV1 {
            ts: Ts(i64::from(minute) * MINUTE),
            pid: 42,
            session_start_key: 1,
            query_start_key: 1,
            datid: 7,
            datname: fixture_str_id("fixture-1"),
            relid: 42,
            phase: fixture_str_id("fixture-2"),
            backend_type: fixture_str_id("fixture-3"),
            activity_present: true,
            is_autovacuum: Some(true),
            backend_start: Some(Ts(1)),
            query_start: Some(Ts(1)),
            elapsed_us: Some(if spike(minute) {
                360_000_000
            } else {
                60_000_000
            }),
            clock_valid: Some(true),
        })
        .collect();
    let body = PgVacuumObservationV1::encode(&vacuum).expect("vacuum encode");
    assert_contract_lens(
        1_032_001,
        "pg_vacuum_observation",
        "PG-VACUUM-005",
        vacuum.len(),
        &body,
        to,
    )
    .await;

    let replication: Vec<_> = (0..40)
        .map(|minute| PgReplicationPhysicalV1 {
            ts: Ts(i64::from(minute) * MINUTE),
            pid: 42,
            backend_start_key: 1,
            application_name: fixture_str_id("fixture-1"),
            slot_name: fixture_str_id("fixture-2"),
            slot_type: fixture_str_id("fixture-3"),
            state: fixture_str_id("fixture-4"),
            sync_state: fixture_str_id("fixture-5"),
            scope_code: 1,
            state_code: 3,
            current_to_sent_bytes: Some(0),
            sent_to_write_bytes: Some(0),
            write_to_flush_bytes: Some(0),
            flush_to_replay_bytes: Some(if spike(minute) { 100_000_000 } else { 1_000 }),
            write_lag_us: None,
            flush_lag_us: None,
            replay_lag_us: None,
        })
        .collect();
    let body = PgReplicationPhysicalV1::encode(&replication).expect("replication encode");
    assert_contract_lens(
        1_033_001,
        "pg_replication_physical",
        "PG-REPL-015",
        replication.len(),
        &body,
        to,
    )
    .await;

    let slots: Vec<_> = (0..40)
        .map(|minute| {
            let retained = if spike(minute) {
                90_000_000
            } else {
                10_000_000
            };
            PgReplicationSlotRetentionV3 {
                ts: Ts(i64::from(minute) * MINUTE),
                slot_name: fixture_str_id("fixture-1"),
                slot_type: fixture_str_id("fixture-2"),
                wal_status: fixture_str_id("fixture-3"),
                invalidation_reason: fixture_str_id("fixture-4"),
                active: false,
                active_pid: None,
                restart_lsn: Some(1),
                retained_bytes: Some(retained),
                safe_wal_size: Some(100_000_000 - retained),
                max_slot_wal_keep_size_bytes: Some(100_000_000),
                wal_status_code: 1,
                is_in_recovery: false,
                conflicting: Some(false),
                invalidation_code: 0,
            }
        })
        .collect();
    assert_eq!(
        kronika_reader::logical_section("pg_replication_slot_retention")
            .expect("slot logical section")
            .diff_key(),
        vec!["slot_name"]
    );
    let body = PgReplicationSlotRetentionV3::encode(&slots).expect("slot encode");
    assert_contract_lens(
        1_034_003,
        "pg_replication_slot_retention",
        "PG-SLOT-016",
        slots.len(),
        &body,
        to,
    )
    .await;

    let storage: Vec<_> = (0..40)
        .map(|minute| PgStorageMountV1 {
            ts: Ts(i64::from(minute) * MINUTE),
            role: 1,
            path_hash_hi: 1,
            path_hash_lo: 2,
            mount_hash_hi: 3,
            mount_hash_lo: 4,
            mount_namespace: 5,
            mapping_state: 1,
            total_bytes: Some(100_000_000),
            available_bytes: Some(if spike(minute) { 5_000_000 } else { 50_000_000 }),
        })
        .collect();
    let body = PgStorageMountV1::encode(&storage).expect("storage encode");
    assert_contract_lens(
        1_036_001,
        "pg_storage_mount",
        "OS-FS-027",
        storage.len(),
        &body,
        to,
    )
    .await;

    let cgroup: Vec<_> = (0..40)
        .map(|minute| PgProcessCgroupMemoryV1 {
            ts: Ts(i64::from(minute) * MINUTE),
            process_hash_hi: 1,
            process_hash_lo: 2,
            cgroup_hash_hi: 3,
            cgroup_hash_lo: 4,
            hierarchy: 2,
            mapping_state: 1,
            current_bytes: Some(if spike(minute) {
                95_000_000
            } else {
                50_000_000
            }),
            max_bytes: Some(100_000_000),
            max_unlimited: false,
        })
        .collect();
    let body = PgProcessCgroupMemoryV1::encode(&cgroup).expect("cgroup encode");
    assert_contract_lens(
        1_037_001,
        "pg_process_cgroup_memory",
        "OS-CGMEM-023",
        cgroup.len(),
        &body,
        to,
    )
    .await;
}

#[tokio::test]
async fn incidents_reject_degenerate_parameters() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

    for uri in [
        "/v1/incidents?source=7&from=5&to=5",
        "/v1/incidents?source=7&from=0&to=1000&window=1h",
        "/v1/incidents?source=7&from=0&to=9000000000&window=0s",
        "/v1/incidents?source=7&from=0&to=9000000000&threshold=-1",
        "/v1/incidents?source=7&from=0&to=9000000000&eps_rel=NaN",
        "/v1/incidents?source=7&from=-9223372036854775808&to=9223372036854775807",
        "/v1/incidents?source=7&from=0&to=3600000000&max_cluster_span=2h",
        "/v1/incidents?source=7&from=0&to=9000000000&unknown=1",
        "/v1/incidents?source=7&source=8&from=0&to=9000000000",
    ] {
        let (status, _body) = serve(dir.path(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} must be rejected");
    }

    for uri in [
        "/v1/incidents?source=7&from=0&to=86400000000&window=1s&step=1s",
        "/v1/incidents?source=7&from=0&to=90000000000",
    ] {
        let (status, _body) = serve(dir.path(), uri).await;
        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{uri} must hit a hard cap"
        );
    }
}

#[tokio::test]
async fn incidents_distinguish_no_data_and_identity_quality() {
    const MINUTE: i64 = 60 * 1_000_000;
    let no_data = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(no_data.path(), "0.pgm", 7, 0, MINUTE);
    let (status, body) = serve(
        no_data.path(),
        "/v1/incidents?source=8&from=600000000&to=1200000000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["analysis_status"], "no_data");
    assert_eq!(body["complete"], false);
    assert_eq!(body["data_age_seconds"], serde_json::Value::Null);

    let missing = tempfile::tempdir().expect("tempdir");
    let to = write_archiver_spike_segment(missing.path());
    let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m");
    let (status, body) = serve(missing.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["analysis_status"], "missing_node_identity");
    assert_eq!(body["complete"], false);
    assert_eq!(body["incidents"], serde_json::json!([]));

    let conflicting = tempfile::tempdir().expect("tempdir");
    let rows = archiver_rows(true);
    write_archiver_with_node(
        conflicting.path(),
        "0.pgm",
        "node-a",
        &rows[..21],
        0,
        20 * MINUTE,
    );
    write_archiver_with_node(
        conflicting.path(),
        "1.pgm",
        "node-b",
        &rows[20..],
        20 * MINUTE,
        39 * MINUTE,
    );
    let (status, body) = serve(conflicting.path(), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["analysis_status"], "conflicting_node_identity");
    assert_eq!(body["complete"], false);
}

#[tokio::test]
async fn analytic_endpoints_share_fail_fast_admission() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_bgwriter_segment(dir.path(), "0.pgm", 7, 0, 10 * 60 * 1_000_000);
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
    let state = AppState::new(snapshot);
    let _permit = state
        .try_acquire_analytic()
        .expect("reserve the shared analytic slot");

    for uri in [
        "/v1/incidents?source=7&from=0&to=600000000&window=1m&step=1m",
        "/v1/anomalies?source=7&from=0&to=600000000&window=1m&step=1m",
    ] {
        let response = app(state.clone(), None, test_metrics_handle())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1"),
            "{uri} advertises a valid retry delay",
        );
    }
}

#[tokio::test]
async fn incident_read_failure_is_sanitized() {
    let dir = tempfile::tempdir().expect("tempdir");
    let to = 39 * 60 * 1_000_000;
    write_archiver_with_identity(dir.path(), &archiver_rows(true), 0, to);
    let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
    let state = AppState::new(snapshot);
    std::fs::remove_file(dir.path().join("0.pgm")).expect("remove fixture after snapshot");
    let uri = format!("/v1/incidents?source=7&from=0&to={to}&window=6m&step=2m");
    let response = app(state, None, test_metrics_handle())
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route request");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON error");
    assert_problem(
        &body,
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_read_failed",
        serde_json::json!({}),
    );
    let rendered = String::from_utf8_lossy(&bytes);
    assert!(!rendered.contains("0.pgm"));
    assert!(!rendered.contains(dir.path().to_string_lossy().as_ref()));
}
