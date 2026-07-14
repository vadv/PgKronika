//! Serving-path benchmarks: the gather+sort+page work behind a section query.
//!
//! The fixtures are `pg_stat_statements` parts whose union spans three extension
//! layouts (V1/V4/V6), so the Null-fill for columns absent from a version's
//! layout is exercised. Two sort-key shapes are built: rows with unique `ts` and
//! rows that all share one `(dbid, userid, ts)` sort key, the latter forcing the
//! tie-break to compare every remaining union column.
//!
//! The global allocator is mimalloc, matching the web binary; the reader's own
//! test harness would otherwise measure against glibc malloc and the allocation
//! counts would not carry over to production.
//!
//! Fixtures live under a `tempfile::tempdir`, which on Linux is normally tmpfs or
//! page cache — these numbers reflect warm reads, not a cold disk.

#![allow(
    missing_docs,
    reason = "criterion_group!/criterion_main! expand to undocumented public items; a bench binary has no public API"
)]

use std::path::Path;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
use kronika_reader::{LocalDirSnapshot, section};
use kronika_registry::pg_stat_statements::{
    PgStatStatementsV1, PgStatStatementsV4, PgStatStatementsV6,
};
use kronika_registry::{Section, StrId, Ts};
use kronika_writer::{Interner, dict};

// Dependencies the reader library pulls in but this bench does not touch; naming
// them keeps `unused_crate_dependencies` quiet without editing the library.
use arrow_array as _;
use kronika_diff as _;
use kronika_store as _;
use parquet as _;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const SECTION: &str = "pg_stat_statements";
const SOURCE: u64 = 7;
/// Query text interned per row so the `str_id` resolve path returns a real
/// string; the row's `queryid` seeds it so distinct queries get distinct text.
fn query_text(queryid: i64) -> Vec<u8> {
    format!("SELECT * FROM t WHERE id = {queryid} AND col > 0 ORDER BY id").into_bytes()
}

/// A V6 row with every counter/timing field defaulted; only `ts`, `queryid`, and
/// the interned `query` id vary.
const fn statements_v6_row(
    ts: i64,
    dbid: u32,
    userid: u32,
    queryid: i64,
    query: StrId,
) -> PgStatStatementsV6 {
    PgStatStatementsV6 {
        ts: Ts(ts),
        queryid: Some(queryid),
        userid,
        dbid,
        toplevel: false,
        datname: None,
        usename: None,
        query: Some(query),
        calls: 0,
        rows: 0,
        plans: 0,
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
        wal_records: 0,
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

/// A V4 row (extension 1.10): no `wal_buffers_full`, no parallel-worker columns,
/// no `local_blk_*_time`, no `jit_deform_*`, no `stats_since`. All fields
/// defaulted except the varying keys.
const fn statements_v4_row(
    ts: i64,
    dbid: u32,
    userid: u32,
    queryid: i64,
    query: StrId,
) -> PgStatStatementsV4 {
    PgStatStatementsV4 {
        ts: Ts(ts),
        queryid: Some(queryid),
        userid,
        dbid,
        toplevel: false,
        datname: None,
        usename: None,
        query: Some(query),
        calls: 0,
        rows: 0,
        plans: 0,
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
        blk_read_time: 0.0,
        blk_write_time: 0.0,
        temp_blk_read_time: 0.0,
        temp_blk_write_time: 0.0,
        wal_records: 0,
        wal_fpi: 0,
        wal_bytes: 0,
        jit_functions: 0,
        jit_generation_time: 0.0,
        jit_inlining_count: 0,
        jit_inlining_time: 0.0,
        jit_optimization_count: 0,
        jit_optimization_time: 0.0,
        jit_emission_count: 0,
        jit_emission_time: 0.0,
    }
}

/// A V1 row (extension 1.6/1.7): legacy timing names, no planning/WAL/JIT/
/// `toplevel` columns. All fields defaulted except the varying keys.
const fn statements_v1_row(
    ts: i64,
    dbid: u32,
    userid: u32,
    queryid: i64,
    query: StrId,
) -> PgStatStatementsV1 {
    PgStatStatementsV1 {
        ts: Ts(ts),
        queryid: Some(queryid),
        userid,
        dbid,
        datname: None,
        usename: None,
        query: Some(query),
        calls: 0,
        rows: 0,
        total_time: 0.0,
        min_time: 0.0,
        max_time: 0.0,
        mean_time: 0.0,
        stddev_time: 0.0,
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
        blk_read_time: 0.0,
        blk_write_time: 0.0,
    }
}

/// How a fixture assigns `ts` across its rows.
#[derive(Clone, Copy)]
enum SortKeyShape {
    /// Each row gets a distinct `ts`, so the sort key alone orders every row.
    Unique,
    /// Every row shares one `(dbid, userid, ts)`, so the tie-break must walk the
    /// remaining union columns to order them — the O(W^2) worst case.
    Tied,
}

/// Split `n` rows across the three layout versions, roughly 60/25/15 so the union
/// spans all three and the bulk sits on the widest (V6).
const fn split_counts(n: usize) -> (usize, usize, usize) {
    let v1 = n / 6;
    let v4 = n / 4;
    let v6 = n - v1 - v4;
    (v6, v4, v1)
}

/// Build a one-part segment of `n` statements rows in `dir/1000.pgm`.
///
/// The part carries a real dictionary (interned query text) so the `str_id`
/// resolve path returns strings rather than nulls. `ts` values follow `shape`.
fn build_fixture(dir: &Path, n: usize, shape: SortKeyShape) {
    let mut interner = Interner::new(DictLimits::new(1 << 16, 1 << 24).expect("dict limits"));

    // A `ts` per row: monotonic for Unique, constant for Tied. The tie-break
    // needs a shared full sort key `(dbid, userid, ts)`, so Tied also fixes dbid
    // and userid; only the interned `query` column then separates rows.
    let base_ts = 1_000_i64;
    let row_ts = |i: i64| -> i64 {
        match shape {
            SortKeyShape::Unique => base_ts + i,
            SortKeyShape::Tied => base_ts,
        }
    };

    let (n_v6, n_v4, n_v1) = split_counts(n);

    let mut v6 = Vec::with_capacity(n_v6);
    let mut v4 = Vec::with_capacity(n_v4);
    let mut v1 = Vec::with_capacity(n_v1);
    for i in 0..n {
        // `n` stays under 10_000, so `i` fits every target integer width.
        let idx = i64::try_from(i).expect("row index fits i64");
        let (d, u) = dbid_userid(i, shape);
        // Distinct text per row: the dictionary holds `n` entries and the
        // tie-break has a differing union column to order Tied rows on.
        let str_id = interner
            .intern(&query_text(idx))
            .expect("intern query text");
        let q = StrId(str_id.get());
        if i < n_v6 {
            v6.push(statements_v6_row(row_ts(idx), d, u, idx, q));
        } else if i < n_v6 + n_v4 {
            v4.push(statements_v4_row(row_ts(idx), d, u, idx, q));
        } else {
            v1.push(statements_v1_row(row_ts(idx), d, u, idx, q));
        }
    }

    let body_v6 = PgStatStatementsV6::encode(&v6).expect("encode v6");
    let body_v4 = PgStatStatementsV4::encode(&v4).expect("encode v4");
    let body_v1 = PgStatStatementsV1::encode(&v1).expect("encode v1");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    let mut inputs: Vec<SectionInput<'_>> = dict_sections
        .iter()
        .map(|s| SectionInput {
            type_id: s.type_id,
            rows: s.rows,
            body: &s.body,
        })
        .collect();
    inputs.push(SectionInput {
        type_id: 1_002_006,
        rows: u32::try_from(n_v6).expect("v6 count fits u32"),
        body: &body_v6,
    });
    inputs.push(SectionInput {
        type_id: 1_002_004,
        rows: u32::try_from(n_v4).expect("v4 count fits u32"),
        body: &body_v4,
    });
    inputs.push(SectionInput {
        type_id: 1_002_001,
        rows: u32::try_from(n_v1).expect("v1 count fits u32"),
        body: &body_v1,
    });

    let min_ts = base_ts;
    let max_ts = base_ts + i64::try_from(n).expect("row count fits i64");
    let part = build_part(
        &inputs,
        PartMeta {
            min_ts,
            max_ts,
            source_id: SOURCE,
        },
    );
    std::fs::write(dir.join("1000.pgm"), &part).expect("write part");
}

/// The `(dbid, userid)` for row `i` under `shape`. A free function so it can be
/// called inside the row loop above without borrowing `shape` through a closure.
fn dbid_userid(i: usize, shape: SortKeyShape) -> (u32, u32) {
    match shape {
        // `i` stays under 10_000, so the cast never truncates.
        SortKeyShape::Unique => {
            let i = u32::try_from(i).expect("row index fits u32");
            (i % 8 + 1, i % 4 + 1)
        }
        SortKeyShape::Tied => (1, 1),
    }
}

/// Full section query: gather across the segment, sort, and page.
fn bench_section_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("section_query");
    for &n in &[100_usize, 10_000] {
        for (label, shape) in [
            ("unique", SortKeyShape::Unique),
            ("tied", SortKeyShape::Tied),
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            build_fixture(tmp.path(), n, shape);
            let snap = LocalDirSnapshot::open(tmp.path()).expect("open snapshot");

            // Warm the page cache and the decode path once before measuring.
            {
                let mut warm = snap.clone();
                let warmed = section(
                    &mut warm,
                    SECTION,
                    SOURCE,
                    i64::MIN,
                    i64::MAX,
                    100_000,
                    None,
                )
                .expect("warm section");
                black_box(warmed);
            }

            let id = BenchmarkId::from_parameter(format!("{n}/{label}"));
            group.bench_function(id, |b| {
                b.iter(|| {
                    // `section` takes `&mut` only for the stale-retry `refresh`;
                    // decode is `&self`, so a fresh clone per iteration keeps the
                    // read pure without touching the fixture on disk.
                    let mut s = snap.clone();
                    let page = section(&mut s, SECTION, SOURCE, i64::MIN, i64::MAX, 100_000, None)
                        .expect("section");
                    black_box(page);
                });
            });
        }
    }
    group.finish();
}

/// Repeated incremental refresh on an unchanged store: the steady-state poll tick
/// that should do almost no work when nothing changed.
///
/// The appended-tail variant (sealing a new `.pgm` between iterations) is not
/// covered here: mutating the fixture inside a criterion sample would fold file
/// I/O into the measurement. It belongs in a separate harness.
fn bench_refresh_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_incremental");
    for &n in &[100_usize, 10_000] {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_fixture(tmp.path(), n, SortKeyShape::Unique);
        let mut snap = LocalDirSnapshot::open(tmp.path()).expect("open snapshot");

        let id = BenchmarkId::from_parameter(format!("{n}/unchanged"));
        group.bench_function(id, |b| {
            b.iter(|| {
                snap.refresh_incremental().expect("refresh_incremental");
                black_box(&snap);
            });
        });
    }
    group.finish();
}

/// Build `m` single-row sealed segments (`1000.pgm`, `1001.pgm`, …).
///
/// Opening the dir yields `m` sealed units, so `LocalDirSnapshot::clone` copies
/// `m` catalogs — the per-request cost the data handlers pay on the happy path
/// today.
fn build_many_segments(dir: &Path, m: usize) {
    for i in 0..m {
        let mut interner = Interner::new(DictLimits::new(1 << 16, 1 << 24).expect("dict limits"));
        let idx = i64::try_from(i).expect("segment index fits i64");
        let str_id = interner
            .intern(&query_text(idx))
            .expect("intern query text");
        let row = statements_v6_row(1000 + idx, 1, 1, idx, StrId(str_id.get()));
        let body = PgStatStatementsV6::encode(&[row]).expect("encode v6");
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
        let mut inputs: Vec<SectionInput<'_>> = dict_sections
            .iter()
            .map(|s| SectionInput {
                type_id: s.type_id,
                rows: s.rows,
                body: &s.body,
            })
            .collect();
        inputs.push(SectionInput {
            type_id: 1_002_006,
            rows: 1,
            body: &body,
        });
        let part = build_part(
            &inputs,
            PartMeta {
                min_ts: 1000 + idx,
                max_ts: 1000 + idx,
                source_id: SOURCE,
            },
        );
        std::fs::write(dir.join(format!("{}.pgm", 1000 + i)), &part).expect("write part");
    }
}

/// Per-request snapshot clone as the sealed-segment count grows. Data handlers
/// clone the whole catalog set on every request today; P2 removes it from the
/// happy path, so this sizes what P2 saves.
fn bench_snapshot_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_clone");
    for &m in &[100_usize, 1000, 5000] {
        let tmp = tempfile::tempdir().expect("tempdir");
        build_many_segments(tmp.path(), m);
        let snap = LocalDirSnapshot::open(tmp.path()).expect("open snapshot");
        let id = BenchmarkId::from_parameter(format!("{m}_segments"));
        group.bench_function(id, |b| {
            b.iter(|| {
                black_box(snap.clone());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_section_query,
    bench_refresh_incremental,
    bench_snapshot_clone
);
criterion_main!(benches);
