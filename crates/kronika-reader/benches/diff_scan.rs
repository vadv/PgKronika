//! Anomaly-path benchmarks: the series fold behind a period scan and the
//! batch gather that feeds it.
//!
//! `diff_section` is measured in isolation over synthetic rows (its `BTreeMap`
//! grouping and per-cell name lookups dominate when a scan folds thousands of
//! series), and the batch `sections` gather is measured against per-section
//! queries over the same fixture (one segment decode should serve every
//! section of a scan).
//!
//! The global allocator is mimalloc, matching the web binary (see serving.rs).

#![allow(
    missing_docs,
    reason = "criterion_group!/criterion_main! expand to undocumented public items; a bench binary has no public API"
)]

use std::collections::BTreeMap;
use std::path::Path;

// proptest is used by the lib's overview fuzz tests; anchored here so this
// bench target does not trip `unused_crate_dependencies`.
use proptest as _;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_reader::{LocalDirSnapshot, OutRow, Value, diff_section, section, sections};
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_registry::{Section, Ts};

// Dependencies the reader library pulls in but this bench does not touch; naming
// them keeps `unused_crate_dependencies` quiet without editing the library.
use arrow_array as _;
use kronika_analytics as _;
use kronika_store as _;
use kronika_writer as _;
use parquet as _;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const SOURCE: u64 = 7;
const SECOND: i64 = 1_000_000;
/// Snapshot cadence of the synthetic fixtures: one row per series each 10 s.
const STEP: i64 = 10 * SECOND;
const CUMULATIVE: [&str; 5] = ["c0", "c1", "c2", "c3", "c4"];

/// One synthetic snapshot row: identity `id`, five climbing counters.
fn synthetic_row(ts: i64, id: i64, tick: i64) -> OutRow {
    let mut row: OutRow = vec![
        ("ts".to_owned(), Value::Ts(ts)),
        ("id".to_owned(), Value::I64(id)),
    ];
    for (k, name) in CUMULATIVE.iter().enumerate() {
        let k = i64::try_from(k).expect("column index fits i64");
        row.push(((*name).to_owned(), Value::I64(tick * (k + 1))));
    }
    row
}

/// `series x snapshots` rows in collection order (all series per tick).
fn synthetic_rows(series: i64, snapshots: i64) -> Vec<OutRow> {
    let mut rows = Vec::with_capacity(usize::try_from(series * snapshots).expect("fixture fits"));
    for tick in 0..snapshots {
        for id in 0..series {
            rows.push(synthetic_row(tick * STEP, id, tick));
        }
    }
    rows
}

/// The per-series fold in isolation: group by identity, sort, diff every pair.
fn bench_diff_section_fold(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_section_fold");
    for &(series, snapshots) in &[(10_i64, 120_i64), (200, 120), (1_000, 120)] {
        let rows = synthetic_rows(series, snapshots);
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{series}x{snapshots}")),
            &rows,
            |b, rows| {
                b.iter(|| {
                    black_box(diff_section(
                        black_box(&["id"]),
                        black_box(&CUMULATIVE),
                        rows,
                        &[],
                    ))
                });
            },
        );
    }
    group.finish();
}

/// A two-hour, 10-second-cadence archiver + bgwriter segment (720 rows each).
fn build_two_section_fixture(dir: &Path) {
    let snapshots = 720;
    let mut archiver = Vec::with_capacity(snapshots);
    let mut bgwriter = Vec::with_capacity(snapshots);
    for tick in 0..snapshots {
        let idx = i64::try_from(tick).expect("tick fits i64");
        let ts = idx * STEP;
        archiver.push(PgStatArchiver {
            ts: Ts(ts),
            archived_count: idx,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        });
        bgwriter.push(BgwriterCheckpointer {
            ts: Ts(ts),
            checkpoints_timed: idx / 30,
            checkpoints_req: 0,
            #[allow(clippy::cast_precision_loss, reason = "bench ticks stay small")]
            checkpoint_write_time: 0.5 * idx as f64,
            #[allow(clippy::cast_precision_loss, reason = "bench ticks stay small")]
            checkpoint_sync_time: 0.1 * idx as f64,
            buffers_checkpoint: idx * 64,
            restartpoints_timed: None,
            restartpoints_req: None,
            restartpoints_done: None,
            buffers_clean: idx * 8,
            maxwritten_clean: 0,
            buffers_backend: Some(idx * 2),
            buffers_backend_fsync: Some(0),
            buffers_alloc: idx * 100,
            bgwriter_stats_reset: Ts(0),
            checkpointer_stats_reset: None,
        });
    }
    let archiver_body = PgStatArchiver::encode(&archiver).expect("encode archiver");
    let bgwriter_body = BgwriterCheckpointer::encode(&bgwriter).expect("encode bgwriter");
    let rows = u32::try_from(snapshots).expect("row count fits u32");
    let part = build_part(
        &[
            SectionInput {
                type_id: 1_008_001,
                rows,
                body: &archiver_body,
            },
            SectionInput {
                type_id: 1_006_001,
                rows,
                body: &bgwriter_body,
            },
        ],
        PartMeta {
            min_ts: 0,
            max_ts: i64::try_from(snapshots).expect("fits") * STEP,
            source_id: SOURCE,
        },
    );
    std::fs::write(dir.join("0.pgm"), &part).expect("write part");
}

const TWO_SECTIONS: [&str; 2] = [
    "pg_stat_archiver",
    "pg_stat_bgwriter + pg_stat_checkpointer",
];

/// One batch gather versus one query per section over the same segment.
fn bench_batch_vs_single(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("tempdir");
    build_two_section_fixture(tmp.path());
    let snap = LocalDirSnapshot::open(tmp.path()).expect("open snapshot");

    let mut group = c.benchmark_group("batch_gather");
    group.bench_function("sections_batch", |b| {
        b.iter(|| {
            let mut snap = snap.clone();
            let cursors = BTreeMap::new();
            let pages = sections(
                &mut snap,
                SOURCE,
                i64::MIN,
                i64::MAX,
                &TWO_SECTIONS,
                1_000_000,
                &cursors,
            )
            .expect("batch query");
            black_box(pages);
        });
    });
    group.bench_function("section_per_name", |b| {
        b.iter(|| {
            for name in TWO_SECTIONS {
                let mut snap = snap.clone();
                let page = section(&mut snap, name, SOURCE, i64::MIN, i64::MAX, 1_000_000, None)
                    .expect("single query");
                black_box(page);
            }
        });
    });
    group.finish();
}

criterion_group!(benches, bench_diff_section_fold, bench_batch_vs_single);
criterion_main!(benches);
