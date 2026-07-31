//! Property and fuzz coverage for the fact-file codec.
//!
//! Arbitrary bytes verify that decoders return without panicking; generated
//! values exercise canonical build-and-admit round trips.

use proptest::prelude::*;

use kronika_analytics::overview::{
    AlignmentId, CounterSample, GaugeSample, MetricSeriesId, SegmentIdentity,
};

use super::block::{
    CounterSamplesBlock, EncodableBlock, EntityStatesBlock, GaugeSamplesBlock, LossCoverageBlock,
    ResetMarkersBlock, SourceManifestBlock, StringTableBlock,
};
use super::container::{BlockContent, FactFile, HeaderIdentity};
use super::descriptors::{ManifestEntryDescriptor, SourceDescriptor};
use super::limits::LIMIT;
use super::observations::EventObservationsBlock;
use super::web_index::{
    CollectionReadState, CollectionStatus, CollectionVisibility, EntitySeriesBlock, IndexStatus,
    TimeGrid, UiSummaryBlock, ViewSummary,
};

fn identity() -> HeaderIdentity {
    let lineage = lineage();
    HeaderIdentity::from_current_contract(
        kronika_format::FORMAT_VERSION,
        i64::MIN,
        i64::MAX,
        4_096,
        SourceDescriptor([0x22; 32]),
        lineage.id(),
    )
}

fn lineage() -> SegmentIdentity {
    SegmentIdentity::sealed([0x22; 32])
}

fn manifest() -> BlockContent {
    BlockContent::SourceManifest(Box::new(
        SourceManifestBlock::new(
            kronika_format::FORMAT_VERSION,
            i64::MIN,
            i64::MAX,
            4_096,
            Vec::<ManifestEntryDescriptor>::new(),
            &LIMIT,
        )
        .expect("manifest"),
    ))
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

fn arb_collection_status() -> impl Strategy<Value = CollectionStatus> {
    prop_oneof![
        any::<u32>().prop_map(|collected| {
            CollectionStatus::new(
                u64::from(collected),
                Some(u64::from(collected)),
                CollectionReadState::Complete,
                CollectionVisibility::Full,
            )
            .expect("generated complete status")
        }),
        (any::<u32>(), 1_u32..=u32::MAX).prop_map(|(collected, missing)| {
            CollectionStatus::new(
                u64::from(collected),
                Some(u64::from(collected) + u64::from(missing)),
                CollectionReadState::SourceLimit,
                CollectionVisibility::Full,
            )
            .expect("generated source-limit status")
        }),
        (any::<u32>(), any::<bool>()).prop_map(|(collected, exact)| {
            CollectionStatus::new(
                u64::from(collected),
                exact.then_some(u64::from(collected)),
                CollectionReadState::Permission,
                CollectionVisibility::Restricted,
            )
            .expect("generated permission status")
        }),
        any::<u32>().prop_map(|collected| {
            CollectionStatus::new(
                u64::from(collected),
                None,
                CollectionReadState::ReadFailure,
                CollectionVisibility::Unknown,
            )
            .expect("generated read-failure status")
        }),
    ]
}

proptest! {
    #[test]
    fn admit_never_panics_on_arbitrary_bytes(bytes in arb_bytes()) {
        drop(FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT));
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
        let strings = StringTableBlock::new(Vec::new(), &LIMIT).expect("empty text table");
        drop(EventObservationsBlock::decode(
            &bytes,
            &lineage(),
            &strings,
            &LIMIT,
        ));
        drop(UiSummaryBlock::decode(&bytes, &LIMIT));
        drop(EntitySeriesBlock::decode(&bytes, &LIMIT));
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
    fn ui_summary_collection_status_round_trips(status in arb_collection_status()) {
        let view = ViewSummary::new_with_collection(
            1,
            1,
            IndexStatus::Complete,
            vec![1],
            vec![0],
            vec![status.collected()],
            vec![1],
            vec![status],
            &LIMIT,
        )
        .expect("generated view");
        let block = UiSummaryBlock::new(
            TimeGrid::for_range(100, 100).expect("grid"),
            vec![100],
            vec![view],
            &LIMIT,
        )
        .expect("generated summary");
        let decoded = UiSummaryBlock::decode(&block.encode(), &LIMIT).expect("decode own summary");
        prop_assert_eq!(decoded.collection_state_at(1, 100), Some((100, status)));
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
        let blocks = vec![
            manifest(),
            BlockContent::CounterSamples(Box::new(block)),
        ];
        let mut bytes = FactFile::build(&identity(), blocks, &LIMIT).expect("build");
        if index < bytes.len() {
            bytes[index] ^= xor;
        }
        drop(FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT));
    }

    #[test]
    fn a_valid_build_always_admits_and_round_trips_its_blocks(
        counters in prop::collection::vec(arb_counter(), 0..48),
        gauges in prop::collection::vec(arb_gauge(), 0..48),
    ) {
        let mut blocks = vec![manifest()];
        let counter_block = CounterSamplesBlock::new(counters, &LIMIT).ok();
        let gauge_block = GaugeSamplesBlock::new(gauges, &LIMIT).ok();
        if let Some(block) = counter_block.clone() {
            blocks.push(BlockContent::CounterSamples(Box::new(block)));
        }
        if let Some(block) = gauge_block.clone() {
            blocks.push(BlockContent::GaugeSamples(Box::new(block)));
        }
        let bytes = FactFile::build(&identity(), blocks, &LIMIT).expect("build");
        let file = FactFile::admit(&bytes, &identity(), &lineage(), &LIMIT)
            .expect("own build admits");

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
