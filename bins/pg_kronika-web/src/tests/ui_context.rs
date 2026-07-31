use kronika_format::DictLimits;
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::instance_metadata::InstanceMetadata;
use kronika_registry::os_topology::OsTopology;
use kronika_registry::replication_instance::ReplicationInstance;
use kronika_registry::replication_replicas::ReplicationReplicasV1;
use kronika_writer::{Interner, dict};

use super::*;

fn context_fixture() -> tempfile::TempDir {
    context_fixture_impl(false, true, None)
}

fn standby_fixture() -> tempfile::TempDir {
    context_fixture_impl(true, false, Some(5))
}

#[allow(
    clippy::too_many_lines,
    reason = "the fixture keeps one exact multi-section context snapshot visible in one place"
)]
fn context_fixture_impl(
    is_in_recovery: bool,
    include_replicas: bool,
    replay_lag_s: Option<i64>,
) -> tempfile::TempDir {
    const TS_US: i64 = 1_500;
    let mut interner = Interner::new(DictLimits::new(128, 64 * 1024).expect("dictionary limits"));
    let mut intern = |value: &str| {
        interner
            .intern(value.as_bytes())
            .map(|id| StrId(id.get()))
            .expect("intern context fixture")
    };
    let hostname = intern("orders-db");
    let kernel = intern("6.8.0");
    let boot_id = intern("boot-1");
    let database = intern("orders");
    let synchronous_standby_names = intern("");
    let synchronous_commit = intern("on");
    let replica_user = intern("replicator");
    let application_name = intern("standby-a");
    let client_addr = intern("10.0.0.2");
    let streaming = intern("streaming");
    let async_state = intern("async");
    let model = intern("fixture-cpu");

    let instance = InstanceMetadata {
        ts: Ts(TS_US),
        hostname,
        pg_version_num: 170_000,
        kernel_version: kernel,
        pg_system_identifier: Some(7_300_000_000_000_000_000),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4_096,
        boot_id,
        btime: Ts(1_000),
    };
    let database = PgStatDatabaseV1 {
        ts: Ts(TS_US),
        datid: 16_384,
        datname: Some(database),
        numbackends: Some(3),
        xact_commit: 100,
        xact_rollback: 1,
        blks_read: 10,
        blks_hit: 90,
        tup_returned: 100,
        tup_fetched: 90,
        tup_inserted: 10,
        tup_updated: 5,
        tup_deleted: 1,
        conflicts: 0,
        temp_files: 0,
        temp_bytes: 0,
        deadlocks: 0,
        blk_read_time: 0.0,
        blk_write_time: 0.0,
        stats_reset: None,
        frozen_xid_age: Some(100),
        min_mxid_age: Some(10),
        datconnlimit: Some(-1),
        datallowconn: Some(true),
        datistemplate: Some(false),
    };
    let replication = ReplicationInstance {
        ts: Ts(TS_US),
        is_in_recovery,
        timeline_id: 1,
        synchronous_standby_names,
        synchronous_commit,
        wal_receiver_status: None,
        sender_host: None,
        sender_port: None,
        slot_name: None,
        streaming_replicas: i32::from(!is_in_recovery),
        replay_lag_s,
        standby_receive_lsn: None,
        standby_replay_lsn: None,
        standby_last_replay_at: None,
        current_wal_lsn: Some(1_000),
        latest_end_lsn: None,
        latest_end_time: None,
        received_tli: None,
    };
    let replica = ReplicationReplicasV1 {
        ts: Ts(TS_US),
        pid: 4_810,
        usename: replica_user,
        application_name,
        client_addr: Some(client_addr),
        state: streaming,
        sync_state: async_state,
        sync_priority: Some(0),
        sent_lsn: Some(1_000),
        write_lsn: Some(990),
        flush_lsn: Some(980),
        replay_lsn: Some(970),
        write_lag_us: Some(100_000),
        flush_lag_us: Some(200_000),
        replay_lag_us: Some(400_000),
    };
    let topology = (0..4)
        .map(|cpu_id| OsTopology {
            ts: Ts(TS_US),
            cpu_id,
            model_name: model,
            mhz_max: Some(3_600.0),
            core_id: cpu_id / 2,
            socket_id: 0,
            scope: 0,
        })
        .collect::<Vec<_>>();

    let instance_body = InstanceMetadata::encode(&[instance]).expect("encode instance");
    let database_body = PgStatDatabaseV1::encode(&[database]).expect("encode database");
    let replication_body = ReplicationInstance::encode(&[replication]).expect("encode replication");
    let replicas_body = ReplicationReplicasV1::encode(&[replica]).expect("encode replicas");
    let topology_body = OsTopology::encode(&topology).expect("encode topology");
    let dictionary = dict::encode(interner.window()).expect("encode dictionary");
    let mut sections = vec![
        SectionInput {
            type_id: 1_005_001,
            rows: 1,
            body: &database_body,
        },
        SectionInput {
            type_id: 1_015_001,
            rows: 1,
            body: &replication_body,
        },
        SectionInput {
            type_id: 1_021_001,
            rows: 1,
            body: &instance_body,
        },
        SectionInput {
            type_id: 1_113_001,
            rows: 4,
            body: &topology_body,
        },
    ];
    if include_replicas {
        sections.push(SectionInput {
            type_id: 1_016_001,
            rows: 1,
            body: &replicas_body,
        });
    }
    sections.extend(dictionary.iter().map(|section| SectionInput {
        type_id: section.type_id,
        rows: section.rows,
        body: &section.body,
    }));
    sections.sort_unstable_by_key(|section| section.type_id);
    let bytes = build_part(
        &sections,
        PartMeta {
            min_ts: TS_US,
            max_ts: TS_US,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "context.pgm", &bytes);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish context web index");
    }
    directory
}

#[tokio::test]
async fn context_returns_instance_databases_replication_and_cpu_from_one_snapshot() {
    let directory = context_fixture();
    let (status, body) = serve(directory.path(), "/v1/ui/context?at=1600").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["snapshot_ts_us"], "1500");
    assert_eq!(body["instance"]["hostname"], "orders-db");
    assert_eq!(body["instance"]["role"], "primary");
    assert_eq!(body["host"]["logical_cpu_count"], 4);
    assert_eq!(body["databases"][0]["oid"], 16_384);
    assert_eq!(body["databases"][0]["name"], "orders");
    assert!(body["databases"][0]["entity"].as_str().is_some());
    assert_eq!(body["replication"]["instance"]["streaming_replicas"], 1);
    assert_eq!(body["replication"]["replicas"][0]["replay_lag_us"], 400_000);
    assert_eq!(
        body["instance"]["pg_system_identifier_reason"],
        serde_json::Value::Null
    );
    assert_eq!(body["quality"]["status"], "complete");
    assert_eq!(body["quality"]["gated"], serde_json::json!([]));
}

#[tokio::test]
async fn context_on_standby_reports_role_without_invented_replication() {
    let directory = standby_fixture();
    let (status, body) = serve(directory.path(), "/v1/ui/context?at=1600").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["instance"]["role"], "standby");
    assert_eq!(body["instance"]["role_reason"], serde_json::Value::Null);
    assert_eq!(body["replication"]["instance"]["replay_lag_us"], 5_000_000);
    assert_eq!(
        body["replication"]["instance"]["replay_lag_reason"],
        serde_json::Value::Null
    );
    assert_eq!(body["replication"]["replicas"], serde_json::json!([]));
    assert_eq!(body["quality"]["status"], "complete");
    assert_eq!(body["quality"]["gated"], serde_json::json!([]));
}

#[tokio::test]
async fn context_rejects_unknown_duplicate_and_missing_at_before_io() {
    for uri in [
        "/v1/ui/context",
        "/v1/ui/context?at=1&at=2",
        "/v1/ui/context?at=1&source=legacy",
        "/v1/ui/context?at=not-a-timestamp",
    ] {
        let (_directory, status, body) = fixture_response(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
}
