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
use kronika_format as _;
use kronika_reader::{
    BlockContent, BlockKind, CounterSamplesBlock, FactFile, FactFileReader, GaugeSamplesBlock,
    HeaderIdentity, LIMIT, SourceDescriptor, SourceManifestBlock,
};
use kronika_registry as _;
use kronika_store as _;
use kronika_writer as _;
use mimalloc::MiMalloc;
use parquet as _;
use proptest as _;
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
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
