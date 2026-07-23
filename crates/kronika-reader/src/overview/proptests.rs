//! Property and fuzz coverage for the fact-file codec.
//!
//! Two invariants matter most: no untrusted byte string may drive a panic or an
//! unbounded allocation, and any value that encodes must decode back unchanged.
//! The generators feed adversarial identities, timestamps, and floats through
//! every decoder and through a full build-and-admit round trip.

use proptest::prelude::*;

use kronika_analytics::overview::{
    AlignmentId, CounterSample, GaugeSample, MetricSeriesId, NamingContractId, SegmentIdentity,
    SegmentLocator, SourceScopeId,
};

use super::block::{
    CounterSamplesBlock, EncodableBlock, EntityStatesBlock, GaugeSamplesBlock, LossCoverageBlock,
    ResetMarkersBlock, SourceManifestBlock, StringTableBlock,
};
use super::container::{BlockContent, FactFile, HeaderIdentity};
use super::limits::LIMIT;
use super::observations::EventObservationsBlock;

fn identity() -> HeaderIdentity {
    HeaderIdentity::from_current_contract(1, 7, 0, 10_000, 0, [0x11; 32], [0x22; 32])
}

fn lineage() -> SegmentIdentity {
    SegmentIdentity::sealed(
        SourceScopeId([1; 32]),
        NamingContractId([2; 16]),
        SegmentLocator([3; 32]),
        7,
        b"descriptor",
    )
}

fn arb_counter() -> impl Strategy<Value = CounterSample> {
    (
        any::<[u8; 16]>(),
        any::<[u8; 16]>(),
        any::<i64>(),
        any::<u64>(),
        any::<u64>(),
    )
        .prop_map(|(series, alignment, ts_us, value, epoch)| {
            CounterSample::new(
                MetricSeriesId(series),
                AlignmentId(alignment),
                ts_us,
                value,
                epoch,
            )
        })
}

fn arb_gauge() -> impl Strategy<Value = GaugeSample> {
    (any::<[u8; 16]>(), any::<i64>(), any::<f64>()).prop_filter_map("finite value", |(s, ts, v)| {
        GaugeSample::new(MetricSeriesId(s), ts, v)
    })
}

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4_096)
}

proptest! {
    #[test]
    fn admit_never_panics_on_arbitrary_bytes(bytes in arb_bytes()) {
        // The only contract is total: a typed result, never a panic or OOM.
        drop(FactFile::admit(&bytes, &identity(), &LIMIT));
    }

    #[test]
    fn block_decoders_never_panic_on_arbitrary_bytes(bytes in arb_bytes()) {
        drop(CounterSamplesBlock::decode(&bytes, &LIMIT));
        drop(GaugeSamplesBlock::decode(&bytes, &LIMIT));
        drop(LossCoverageBlock::decode(&bytes, &LIMIT));
        drop(ResetMarkersBlock::decode(&bytes, &LIMIT));
        drop(EntityStatesBlock::decode(&bytes, &LIMIT));
        drop(StringTableBlock::decode(&bytes, &LIMIT));
        drop(SourceManifestBlock::decode(&bytes, &LIMIT));
        drop(EventObservationsBlock::decode(&bytes, &lineage(), &LIMIT));
    }

    #[test]
    fn counter_samples_round_trip(samples in prop::collection::vec(arb_counter(), 0..96)) {
        if let Ok(block) = CounterSamplesBlock::new(samples, &LIMIT) {
            let decoded = CounterSamplesBlock::decode(&block.encode(), &LIMIT)
                .expect("re-decode of own output");
            prop_assert_eq!(decoded, block);
        }
    }

    #[test]
    fn gauge_samples_round_trip(samples in prop::collection::vec(arb_gauge(), 0..96)) {
        if let Ok(block) = GaugeSamplesBlock::new(samples, &LIMIT) {
            let decoded = GaugeSamplesBlock::decode(&block.encode(), &LIMIT)
                .expect("re-decode of own output");
            prop_assert_eq!(decoded, block);
        }
    }

    #[test]
    fn a_flipped_byte_in_a_valid_file_never_panics(
        seed in prop::collection::vec(arb_counter(), 0..16),
        index in 0_usize..1_024,
        xor in 1_u8..=255,
    ) {
        let Ok(block) = CounterSamplesBlock::new(seed, &LIMIT) else {
            return Ok(());
        };
        let blocks = vec![BlockContent::CounterSamples(Box::new(block))];
        let mut bytes = FactFile::build(&identity(), blocks, &LIMIT).expect("build");
        if index < bytes.len() {
            bytes[index] ^= xor;
        }
        // A single-byte corruption must never escape as a panic.
        drop(FactFile::admit(&bytes, &identity(), &LIMIT));
    }

    #[test]
    fn a_valid_build_always_admits_and_round_trips_its_blocks(
        counters in prop::collection::vec(arb_counter(), 0..48),
        gauges in prop::collection::vec(arb_gauge(), 0..48),
    ) {
        let mut blocks = Vec::new();
        let counter_block = CounterSamplesBlock::new(counters, &LIMIT).ok();
        let gauge_block = GaugeSamplesBlock::new(gauges, &LIMIT).ok();
        if let Some(block) = counter_block.clone() {
            blocks.push(BlockContent::CounterSamples(Box::new(block)));
        }
        if let Some(block) = gauge_block.clone() {
            blocks.push(BlockContent::GaugeSamples(Box::new(block)));
        }
        let bytes = FactFile::build(&identity(), blocks, &LIMIT).expect("build");
        let file = FactFile::admit(&bytes, &identity(), &LIMIT).expect("own build admits");

        if let Some(expected) = counter_block {
            let body = file
                .block_body(super::block::BlockKind::CounterSamples)
                .expect("counter block present");
            let decoded = CounterSamplesBlock::decode(body, &LIMIT).expect("decode");
            prop_assert_eq!(decoded, expected);
        }
        if let Some(expected) = gauge_block {
            let body = file
                .block_body(super::block::BlockKind::GaugeSamples)
                .expect("gauge block present");
            let decoded = GaugeSamplesBlock::decode(body, &LIMIT).expect("decode");
            prop_assert_eq!(decoded, expected);
        }
    }
}
