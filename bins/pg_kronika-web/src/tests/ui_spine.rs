use super::*;
use kronika_reader::{FactStore, LIMIT, LocalDirSnapshot};
use kronika_registry::os_loadavg::OsLoadavg;
use kronika_registry::os_psi::OsPsi;
use kronika_registry::os_topology::OsTopology;

fn host_spine_fixture() -> tempfile::TempDir {
    let load_rows = [
        OsLoadavg {
            ts: Ts(60_000_000),
            load1: 2.0,
            load5: 1.0,
            load15: 0.5,
            running: 2,
            total: 10,
            scope: 0,
        },
        OsLoadavg {
            ts: Ts(120_000_000),
            load1: 4.0,
            load5: 2.0,
            load15: 1.0,
            running: 3,
            total: 10,
            scope: 0,
        },
    ];
    let psi_rows = [
        OsPsi {
            ts: Ts(60_000_000),
            resource: 2,
            some_avg10: 12.0,
            some_avg60: 6.0,
            some_avg300: 3.0,
            some_total: 100,
            full_avg10: Some(1.0),
            full_avg60: Some(0.5),
            full_avg300: Some(0.1),
            full_total: Some(10),
            scope: 0,
        },
        OsPsi {
            ts: Ts(120_000_000),
            resource: 2,
            some_avg10: 34.0,
            some_avg60: 17.0,
            some_avg300: 8.0,
            some_total: 200,
            full_avg10: Some(2.0),
            full_avg60: Some(1.0),
            full_avg300: Some(0.2),
            full_total: Some(20),
            scope: 0,
        },
    ];
    let topology_rows = [60_000_000, 120_000_000]
        .into_iter()
        .flat_map(|timestamp| {
            (0..4).map(move |cpu_id| OsTopology {
                ts: Ts(timestamp),
                cpu_id,
                model_name: StrId(1),
                mhz_max: Some(3_600.0),
                core_id: cpu_id / 2,
                socket_id: 0,
                scope: 0,
            })
        })
        .collect::<Vec<_>>();
    let load = OsLoadavg::encode(&load_rows).expect("encode load");
    let psi = OsPsi::encode(&psi_rows).expect("encode PSI");
    let topology = OsTopology::encode(&topology_rows).expect("encode topology");
    let bytes = build_part(
        &[
            SectionInput {
                type_id: 1_105_001,
                rows: 2,
                body: &load,
            },
            SectionInput {
                type_id: 1_107_001,
                rows: 2,
                body: &psi,
            },
            SectionInput {
                type_id: 1_113_001,
                rows: 8,
                body: &topology,
            },
        ],
        PartMeta {
            min_ts: 60_000_000,
            max_ts: 120_000_000,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    crate::test_layout::write_named_pgm(directory.path(), "host-spine.pgm", &bytes);
    let snapshot = LocalDirSnapshot::open(directory.path()).expect("snapshot");
    let store = FactStore::new(directory.path());
    for descriptor in snapshot.sealed_descriptors() {
        snapshot
            .load_sealed_facts_by_descriptor(&descriptor, &store, &LIMIT)
            .expect("publish host web index");
    }
    directory
}

#[tokio::test]
async fn spine_returns_aligned_host_series_from_the_hidden_ovf_view() {
    let directory = host_spine_fixture();
    kronika_reader::qualification_reset_open_unit_calls();
    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/spine?from=60000000&to=180000000&buckets=2",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        kronika_reader::qualification_open_unit_calls(),
        0,
        "spine must read only indexed OVF blocks"
    );
    assert_eq!(body["grid"]["bucket_count"], 2);
    assert_eq!(body["series"][0]["code"], "load_per_cpu");
    let load = body["series"][0]["values"].as_array().expect("load values");
    assert!((load[0].as_f64().expect("load value") - 0.5).abs() < 0.01);
    assert_eq!(load[1], 1.0);
    assert_eq!(body["series"][1]["code"], "psi_io_some");
    assert_eq!(body["series"][1]["values"], serde_json::json!([12.0, 34.0]));
    assert_eq!(
        body["series"][0]["value_statuses"][0]["status"],
        "available"
    );
}

#[tokio::test]
async fn spine_distinguishes_observed_value_missing_sample_and_producer_gap() {
    let directory = host_spine_fixture();
    let (status, body) = serve(
        directory.path(),
        "/v1/timeline/spine?from=60000000&to=240000000&buckets=6",
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let psi = &body["series"][1];
    let values = psi["values"].as_array().expect("psi values");
    let statuses = psi["value_statuses"]
        .as_array()
        .expect("psi value statuses");
    assert_eq!(values[0], 12.0);
    assert_eq!(statuses[0]["status"], "available");
    assert_eq!(statuses[0]["reason"], serde_json::Value::Null);
    assert_eq!(values[1], serde_json::Value::Null);
    assert_eq!(statuses[1]["status"], "unavailable");
    assert_eq!(statuses[1]["reason"], "no_sample");
    assert_eq!(values[5], serde_json::Value::Null);
    assert_eq!(statuses[5]["status"], "unavailable");
    assert_eq!(statuses[5]["reason"], "producer_gap");
}

#[tokio::test]
async fn spine_rejects_invalid_query_shapes_before_index_reads() {
    for uri in [
        "/v1/timeline/spine?to=10",
        "/v1/timeline/spine?from=1",
        "/v1/timeline/spine?from=1&from=2&to=3",
        "/v1/timeline/spine?from=1&to=2&source=legacy",
        "/v1/timeline/spine?from=2&to=1",
        "/v1/timeline/spine?from=0&to=90000000000",
        "/v1/timeline/spine?from=1&to=2&buckets=0",
        "/v1/timeline/spine?from=1&to=2&buckets=513",
    ] {
        let (_directory, status, body) = fixture_response(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
}
