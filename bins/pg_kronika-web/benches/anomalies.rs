//! End-to-end `GET /v1/anomalies` benchmark.
//!
//! The fixture covers two hours at 10-second cadence with 200 statement
//! series, plus archiver and bgwriter singletons.

#![allow(
    missing_docs,
    reason = "criterion_group!/criterion_main! expand to undocumented public items; a bench binary has no public API"
)]

use kronika_writer as _;
use std::path::Path;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::Request;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use http_body_util::BodyExt;
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_registry::pg_stat_statements::PgStatStatementsV6;
use kronika_registry::{Section, Ts};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use pg_kronika_web::{AppState, app};
use tower::ServiceExt;

// Dependencies the web library pulls in but this bench does not touch; naming
// them keeps `unused_crate_dependencies` quiet without editing the library.
use arc_swap as _;
use base64 as _;
use form_urlencoded as _;
use kronika_analytics as _;
use kronika_reader as _;
use metrics as _;
use rust_embed as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use subtle as _;
use tower_http as _;
use tracing as _;
use tracing_subscriber as _;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const SOURCE: u64 = 7;
const SECOND: i64 = 1_000_000;
/// Snapshot cadence: one collection every 10 s for two hours.
const STEP: i64 = 10 * SECOND;
const SNAPSHOTS: i64 = 720;
const SERIES: i64 = 200;

/// Process-global Prometheus recorder; `install_recorder` panics on a second
/// call, so every router in this bench shares one handle.
static RECORDER: OnceLock<PrometheusHandle> = OnceLock::new();

fn metrics_handle() -> PrometheusHandle {
    RECORDER
        .get_or_init(|| {
            PrometheusBuilder::new()
                .set_buckets_for_metric(
                    Matcher::Full("kronika_web_request_duration_seconds".to_owned()),
                    pg_kronika_web::REQUEST_DURATION_BUCKETS,
                )
                .expect("histogram buckets are valid")
                .install_recorder()
                .expect("install global Prometheus recorder")
        })
        .clone()
}

/// A V6 statements row with every counter defaulted and no interned strings
/// (`query`/`datname`/`usename` are nullable), so the fixture needs no
/// dictionary. Counters climb with `tick` so the diff fold sees real deltas.
const fn statements_row(ts: i64, queryid: i64, tick: i64) -> PgStatStatementsV6 {
    PgStatStatementsV6 {
        ts: Ts(ts),
        queryid: Some(queryid),
        userid: 1,
        dbid: 1,
        toplevel: true,
        datname: None,
        usename: None,
        query: None,
        calls: tick * 5,
        rows: tick * 50,
        plans: tick,
        total_exec_time: 0.0,
        total_plan_time: 0.0,
        min_exec_time: 0.0,
        max_exec_time: 0.0,
        mean_exec_time: 0.0,
        stddev_exec_time: 0.0,
        min_plan_time: 0.0,
        max_plan_time: 0.0,
        mean_plan_time: 0.0,
        stddev_plan_time: 0.0,
        shared_blks_hit: tick * 100,
        shared_blks_read: tick * 10,
        shared_blks_dirtied: tick,
        shared_blks_written: tick,
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
        wal_records: tick * 2,
        wal_fpi: 0,
        wal_bytes: 0,
        wal_buffers_full: 0,
        jit_functions: 0,
        jit_generation_time: 0.0,
        jit_inlining_count: 0,
        jit_inlining_time: 0.0,
        jit_optimization_count: 0,
        jit_optimization_time: 0.0,
        jit_emission_count: 0,
        jit_emission_time: 0.0,
        jit_deform_count: 0,
        jit_deform_time: 0.0,
        parallel_workers_to_launch: 0,
        parallel_workers_launched: 0,
        stats_since: None,
        minmax_stats_since: None,
    }
}

/// Snapshots per segment: ten-minute segments at the 10 s cadence, matching
/// how the collector seals parts (and staying under the 65 536 row cap).
const SNAPSHOTS_PER_SEGMENT: i64 = 60;

/// Build the typical period as sealed ten-minute segments: statements 200
/// series per snapshot plus the two singletons, 12 `.pgm` parts in `dir`.
fn build_fixture(dir: &Path) {
    let per_segment = usize::try_from(SERIES * SNAPSHOTS_PER_SEGMENT).expect("fixture fits");
    for segment in 0..(SNAPSHOTS / SNAPSHOTS_PER_SEGMENT) {
        let mut statements = Vec::with_capacity(per_segment);
        let mut archiver = Vec::new();
        let mut bgwriter = Vec::new();
        let first_tick = segment * SNAPSHOTS_PER_SEGMENT;
        for tick in first_tick..first_tick + SNAPSHOTS_PER_SEGMENT {
            let ts = tick * STEP;
            for queryid in 0..SERIES {
                statements.push(statements_row(ts, queryid, tick));
            }
            archiver.push(PgStatArchiver {
                ts: Ts(ts),
                archived_count: tick,
                last_archived_wal: None,
                last_archived_time: None,
                failed_count: 0,
                last_failed_wal: None,
                last_failed_time: None,
                stats_reset: None,
            });
            bgwriter.push(BgwriterCheckpointer {
                ts: Ts(ts),
                checkpoints_timed: tick / 30,
                checkpoints_req: 0,
                checkpoint_write_time: 0.0,
                checkpoint_sync_time: 0.0,
                buffers_checkpoint: tick * 64,
                restartpoints_timed: None,
                restartpoints_req: None,
                restartpoints_done: None,
                buffers_clean: tick * 8,
                maxwritten_clean: 0,
                buffers_backend: Some(tick * 2),
                buffers_backend_fsync: Some(0),
                buffers_alloc: tick * 100,
                bgwriter_stats_reset: Ts(0),
                checkpointer_stats_reset: None,
            });
        }

        let statements_body = PgStatStatementsV6::encode(&statements).expect("encode statements");
        let archiver_body = PgStatArchiver::encode(&archiver).expect("encode archiver");
        let bgwriter_body = BgwriterCheckpointer::encode(&bgwriter).expect("encode bgwriter");
        let min_ts = first_tick * STEP;
        let part = build_part(
            &[
                SectionInput {
                    type_id: 1_002_006,
                    rows: u32::try_from(statements.len()).expect("row count fits u32"),
                    body: &statements_body,
                },
                SectionInput {
                    type_id: 1_008_001,
                    rows: u32::try_from(archiver.len()).expect("fits"),
                    body: &archiver_body,
                },
                SectionInput {
                    type_id: 1_006_001,
                    rows: u32::try_from(bgwriter.len()).expect("fits"),
                    body: &bgwriter_body,
                },
            ],
            PartMeta {
                min_ts,
                max_ts: (first_tick + SNAPSHOTS_PER_SEGMENT) * STEP,
                source_id: SOURCE,
            },
        );
        std::fs::write(dir.join(format!("{min_ts}.pgm")), &part).expect("write part");
    }
}

/// The full endpoint over the typical period, and the one-section variant.
fn bench_anomalies_endpoint(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("tempdir");
    build_fixture(tmp.path());
    let snapshot = kronika_reader::LocalDirSnapshot::open(tmp.path()).expect("open snapshot");
    let state = AppState::new(snapshot);
    let router = app(state, None, metrics_handle());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let to = SNAPSHOTS * STEP;
    // The default knobs (1h window, w/4 step) give ~5 positions over the
    // two-hour period — the typical UI request the budget is set for. The
    // 5m/75s variant slides 92 positions and is the stress case.
    let default_knobs = format!("/v1/anomalies?source={SOURCE}&from=0&to={to}");
    let stress = format!("/v1/anomalies?source={SOURCE}&from=0&to={to}&window=5m");
    let one = format!("{stress}&section=pg_stat_statements");

    let mut group = c.benchmark_group("anomalies_endpoint");
    group.sample_size(10);
    for (label, uri) in [
        ("full_scan_default", &default_knobs),
        ("full_scan_5m_stress", &stress),
        ("one_section_5m_stress", &one),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                let body = rt.block_on(async {
                    let response = router
                        .clone()
                        .oneshot(
                            Request::builder()
                                .uri(uri.as_str())
                                .body(Body::empty())
                                .expect("build request"),
                        )
                        .await
                        .expect("route request");
                    assert!(response.status().is_success(), "scan must succeed");
                    response.into_body().collect().await.expect("read body")
                });
                black_box(body);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_anomalies_endpoint);
criterion_main!(benches);
