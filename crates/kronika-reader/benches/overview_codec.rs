//! Release benchmarks for full admission and selective block reads.

#![allow(
    missing_docs,
    reason = "criterion macros generate public items in this benchmark binary"
)]

use arrow_array as _;
use arrow_schema as _;
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use kronika_analytics::overview::{
    AlignmentId, CounterSample, GaugeSample, MetricSeriesId, NamingContractId, SegmentIdentity,
    SegmentLocator, SourceScopeId,
};
use kronika_format::{PartMeta, ReadAt, SectionInput, build_part};
use kronika_reader::{
    BlockContent, BlockKind, CounterSamplesBlock, FactFile, FactFileReader, GaugeSamplesBlock,
    HeaderIdentity, LIMIT, SegmentContext, SegmentFacts, SourceDescriptor, SourceManifestBlock,
};
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::{Section, Ts};
use kronika_store as _;
use kronika_writer as _;
use mimalloc::MiMalloc;
use parquet as _;
use proptest as _;
use rustix as _;
use sha2 as _;
use tempfile as _;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn fixture() -> (Vec<u8>, HeaderIdentity, SegmentIdentity) {
    let identity = HeaderIdentity::from_current_contract(
        1,
        7,
        0,
        9_999,
        1_048_576,
        SourceScopeId([0x11; 32]),
        SourceDescriptor([0x22; 32]),
    );
    let lineage = SegmentIdentity::sealed(
        identity.source_scope_id,
        NamingContractId([0x33; 16]),
        SegmentLocator([0x44; 32]),
        1_006_001,
        b"first",
    );
    let manifest =
        SourceManifestBlock::new(7, 1, 0, 9_999, 1_048_576, Vec::new(), &LIMIT).expect("manifest");
    let gauges = (0_i64..10_000)
        .map(|timestamp| {
            GaugeSample::new(MetricSeriesId([1; 16]), timestamp, 1.0).expect("finite gauge")
        })
        .collect();
    let counters = (0_i64..100)
        .map(|timestamp| {
            CounterSample::new(
                MetricSeriesId([2; 16]),
                AlignmentId([2; 16]),
                timestamp,
                u64::try_from(timestamp).expect("nonnegative timestamp"),
                1,
            )
        })
        .collect();
    let gauges = GaugeSamplesBlock::new(gauges, &LIMIT).expect("gauge samples");
    let counters = CounterSamplesBlock::new(counters, &LIMIT).expect("counter samples");
    let bytes = FactFile::build(
        &identity,
        vec![
            BlockContent::SourceManifest(Box::new(manifest)),
            BlockContent::GaugeSamples(Box::new(gauges)),
            BlockContent::CounterSamples(Box::new(counters)),
        ],
        &LIMIT,
    )
    .expect("fact file");
    (bytes, identity, lineage)
}

fn segment_context() -> SegmentContext {
    SegmentContext::new(
        b"benchmark-store".to_vec(),
        NamingContractId([0x33; 16]),
        SegmentLocator([0x44; 32]),
    )
    .expect("valid segment context")
}

fn pgm_fixture() -> Vec<u8> {
    let rows: Vec<_> = (0..2_048)
        .map(|index| PgLogLifecycleV1 {
            ts: Ts(1_000_000 + i64::from(index)),
            kind: 2,
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dict_dropped_fields: 0,
        })
        .collect();
    let body = PgLogLifecycleV1::encode(&rows).expect("encode lifecycle rows");
    build_part(
        &[SectionInput {
            type_id: 1_028_001,
            rows: u32::try_from(rows.len()).expect("row count"),
            body: &body,
        }],
        PartMeta {
            min_ts: 1_000_000,
            max_ts: 1_002_047,
            source_id: 7,
        },
    )
}

#[derive(Debug)]
struct CountingReader {
    bytes: Vec<u8>,
    reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    bytes_read: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ReadAt for CountingReader {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        self.bytes.as_slice().read_exact_at(buf, offset)?;
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.bytes_read
            .fetch_add(buf.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn byte_len(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the benchmark groups share one admitted fixture and its measured byte counts"
)]
fn benchmark(criterion: &mut Criterion) {
    let (bytes, identity, lineage) = fixture();
    let mut probe =
        FactFileReader::open(bytes.as_slice(), &identity, &LIMIT).expect("open selective probe");
    probe
        .read_blocks(BlockKind::CounterSamples)
        .expect("read counter probe");
    let stats = probe.stats();
    drop(probe);
    assert_eq!(
        stats.read_calls, 3,
        "metadata plus one selected body require three reads"
    );
    assert!(
        stats.stored_bytes_read < bytes.len() as u64 / 10,
        "the probe must skip the large gauge body"
    );
    println!(
        "overview selective probe: file_bytes={} read_calls={} stored_bytes_read={} decoded_bytes={}",
        bytes.len(),
        stats.read_calls,
        stats.stored_bytes_read,
        stats.decoded_bytes,
    );

    {
        let mut full = criterion.benchmark_group("overview_full_admission");
        full.throughput(Throughput::Bytes(bytes.len() as u64));
        full.bench_function("10k_gauges_100_counters", |bencher| {
            bencher.iter(|| {
                black_box(
                    FactFile::admit(
                        black_box(&bytes),
                        black_box(&identity),
                        black_box(&lineage),
                        &LIMIT,
                    )
                    .expect("admit"),
                );
            });
        });
        full.finish();
    }

    {
        let mut selective = criterion.benchmark_group("overview_selective_read");
        selective.throughput(Throughput::Bytes(stats.stored_bytes_read));
        selective.bench_function("open_and_read_counter_body", |bencher| {
            bencher.iter(|| {
                let mut reader =
                    FactFileReader::open(black_box(bytes.as_slice()), &identity, &LIMIT)
                        .expect("open");
                black_box(
                    reader
                        .read_blocks(BlockKind::CounterSamples)
                        .expect("counter block"),
                );
            });
        });
        selective.finish();
    }

    let pgm = pgm_fixture();
    let pgm_reads = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let pgm_bytes_read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let unit = kronika_reader::PgmUnit::open(CountingReader {
        bytes: pgm,
        reads: std::sync::Arc::clone(&pgm_reads),
        bytes_read: std::sync::Arc::clone(&pgm_bytes_read),
    })
    .expect("open PGM fixture");
    pgm_reads.store(0, std::sync::atomic::Ordering::Relaxed);
    pgm_bytes_read.store(0, std::sync::atomic::Ordering::Relaxed);
    let facts = SegmentFacts::extract(&unit, &segment_context(), &LIMIT).expect("extract facts");
    let fact_catalog = facts.catalog_descriptors();
    let cold_counters = (
        pgm_reads.load(std::sync::atomic::Ordering::Relaxed),
        pgm_bytes_read.load(std::sync::atomic::Ordering::Relaxed),
    );
    let fact_bytes = facts.encode(&LIMIT).expect("encode facts");
    let mut warm_probe = FactFileReader::open(fact_bytes.as_slice(), facts.identity(), &LIMIT)
        .expect("open fact probe");
    for kind in [
        BlockKind::SourceManifest,
        BlockKind::StringTable,
        BlockKind::EventObservations,
        BlockKind::LossCoverage,
    ] {
        warm_probe.read_blocks(kind).expect("read fact block");
    }
    let warm_stats = warm_probe.stats();
    println!(
        "overview sealed-fact probe: pgm_bytes={} cold_body_reads={} cold_body_bytes={} fact_bytes={} warm_fact_reads={} warm_fact_bytes={}",
        unit.source_file_len(),
        cold_counters.0,
        cold_counters.1,
        fact_bytes.len(),
        warm_stats.read_calls,
        warm_stats.stored_bytes_read,
    );

    {
        let mut cold = criterion.benchmark_group("overview_segment_cold");
        cold.throughput(Throughput::Elements(2_048));
        cold.bench_function("extract_2048_lifecycle_rows", |bencher| {
            bencher.iter(|| {
                black_box(
                    SegmentFacts::extract(black_box(&unit), black_box(&segment_context()), &LIMIT)
                        .expect("extract"),
                );
            });
        });
        cold.finish();
    }

    {
        let mut restart = criterion.benchmark_group("overview_segment_restart_warm");
        restart.throughput(Throughput::Bytes(fact_bytes.len() as u64));
        restart.bench_function("read_2048_lifecycle_facts", |bencher| {
            bencher.iter(|| {
                black_box(
                    SegmentFacts::from_reader(
                        black_box(fact_bytes.as_slice()),
                        facts.identity(),
                        facts.lineage(),
                        &fact_catalog,
                        &LIMIT,
                    )
                    .expect("warm read"),
                );
            });
        });
        restart.finish();
    }
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
