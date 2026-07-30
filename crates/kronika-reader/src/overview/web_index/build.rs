//! Bounded PGM-to-web-index projection.

#![allow(
    clippy::cast_precision_loss,
    reason = "the web index deliberately stores bounded approximate f64 summaries"
)]
#![allow(
    clippy::too_many_lines,
    reason = "projection functions mirror the declarative registry and keep one view formula local"
)]

use std::collections::{BTreeMap, BTreeSet};

use kronika_analytics::overview::{EventObservation, NotablePolicy};
use kronika_analytics::web_projection::{
    WebAggregation, WebFormula, WebInput, WebMetric, WebView, web_views,
};
use kronika_format::ReadAt;
use kronika_registry::{Cell, Row, registry};

use super::{
    CollectionReadState, CollectionStatus, CollectionVisibility, EntityDictionaryEntry,
    EntityMetric, EntitySeries, EntitySeriesBlock, IndexStatus, METRIC_FLAG_CANONICAL,
    MetricAggregation, MetricStatus, TimeGrid, UiSummaryBlock, ViewSummary, mask_len,
};
use crate::{Dictionary, PgmBodyReadStats, PgmUnit, Resolved};

use super::super::block::BlockError;
use super::super::facts::{BuildError, SourceError};
use super::super::limits::Bounds;

const TOP_K: usize = 64;
const COLLECTION_COVERAGE: &str = "collection_coverage";
const SNAPSHOT_COVERAGE: &str = "snapshot_coverage";

/// Populated web-index blocks derived from one PGM unit.
pub(crate) struct WebIndexBlocks {
    pub(crate) summary: UiSummaryBlock,
    pub(crate) series: Vec<EntitySeriesBlock>,
    pub(crate) pgm_body_read_stats: PgmBodyReadStats,
}

struct DecodedSection {
    catalog_ordinal: u32,
    type_id: u32,
    name: &'static str,
    rows: Vec<Row>,
}

#[derive(Clone, Copy)]
struct IndexedRow<'a> {
    catalog_ordinal: u32,
    row_ordinal: u32,
    type_id: u32,
    section: &'static str,
    row: &'a Row,
}

struct SummaryInput {
    view_code: u16,
    view_revision: u16,
    status: IndexStatus,
    populations: BTreeMap<i64, u64>,
    collections: BTreeMap<i64, CollectionStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CollectionFact {
    ts_us: i64,
    read_state: u8,
    visibility: u8,
    source_total: u64,
    collected: u64,
    total_exact: bool,
}

#[derive(Debug, Default)]
struct CollectionFacts {
    snapshot: Option<CollectionFact>,
    collection: Option<CollectionFact>,
}

#[derive(Debug, Clone, Copy)]
struct ViewCollection {
    source_type: u32,
    status: CollectionStatus,
}

type CollectionTimeline = BTreeMap<(u16, i64), ViewCollection>;

struct Candidate {
    key: Vec<u8>,
    label: String,
    buckets: Vec<Option<f64>>,
    score: f64,
}

struct EvaluatedMetric {
    metric: &'static WebMetric,
    status: MetricStatus,
    candidates: Vec<Candidate>,
}

type GroupedRows<'a> = BTreeMap<Vec<u8>, (String, Vec<IndexedRow<'a>>)>;

enum Evaluation {
    Complete(Vec<Candidate>),
    Unsupported,
}

/// Builds summary and view-addressed entity series from selected source rows.
pub(crate) fn build_web_index<R: ReadAt>(
    unit: &PgmUnit<R>,
    observations: &[EventObservation],
    min_ts: i64,
    max_ts: i64,
    bounds: &Bounds,
) -> Result<WebIndexBlocks, BuildError> {
    if !bounds.is_within_absolute_limits() {
        return Err(BuildError::LimitExceeded);
    }
    if min_ts > max_ts {
        return Ok(WebIndexBlocks {
            summary: UiSummaryBlock::empty(),
            series: Vec::new(),
            pgm_body_read_stats: PgmBodyReadStats::default(),
        });
    }
    let before = unit.body_read_stats();
    let available_sections = unit
        .catalog()
        .entries
        .iter()
        .filter_map(|entry| {
            registry()
                .iter()
                .find(|contract| contract.type_id.get() == entry.type_id)
                .map(|contract| contract.name)
        })
        .collect::<BTreeSet<_>>();
    let mut needed_sections = web_views()
        .iter()
        .filter(|view| view.name != "events")
        .flat_map(|view| view.inputs)
        .flat_map(|input| input.sections)
        .copied()
        .collect::<BTreeSet<_>>();
    needed_sections.extend([COLLECTION_COVERAGE, SNAPSHOT_COVERAGE]);
    let mut decoded = Vec::new();
    let mut decoded_rows = 0_u64;
    for (ordinal, entry) in unit.catalog().entries.iter().enumerate() {
        let Some(contract) = registry()
            .iter()
            .find(|contract| contract.type_id.get() == entry.type_id)
        else {
            continue;
        };
        if !needed_sections.contains(contract.name) {
            continue;
        }
        let catalog_ordinal = u32::try_from(ordinal).map_err(|_error| BuildError::Overflow)?;
        let (_descriptor, rows) = unit.decode_overview_rows(catalog_ordinal)?;
        decoded_rows = decoded_rows
            .checked_add(u64::try_from(rows.len()).map_err(|_error| BuildError::Overflow)?)
            .ok_or(BuildError::Overflow)?;
        if decoded_rows > bounds.items_per_block {
            return Err(BuildError::LimitExceeded);
        }
        decoded.push(DecodedSection {
            catalog_ordinal,
            type_id: entry.type_id,
            name: contract.name,
            rows,
        });
    }
    let dictionary = if decoded.iter().any(|section| {
        !matches!(section.name, COLLECTION_COVERAGE | SNAPSHOT_COVERAGE) && !section.rows.is_empty()
    }) {
        unit.dictionary()?
    } else {
        Dictionary::default()
    };
    let grid = TimeGrid::for_range(min_ts, max_ts).map_err(block_build_error)?;
    let collections = canonical_collections(&decoded)?;
    let (summary, summary_inputs) = build_summary(
        &decoded,
        &available_sections,
        &collections,
        observations,
        grid,
        bounds,
    )?;
    let mut series = Vec::new();
    for view in web_views() {
        if view.name == "events" {
            if !observations.is_empty() {
                series.push(build_event_series(view, observations, grid, bounds)?);
            }
            continue;
        }
        let primary = rows_for_input(&decoded, &view.inputs[0])?;
        if primary.is_empty() {
            continue;
        }
        series.push(build_view_series(
            view,
            &decoded,
            &available_sections,
            &dictionary,
            grid,
            primary,
            bounds,
        )?);
    }
    debug_assert_eq!(
        summary_inputs.len(),
        web_views().len(),
        "every registered view must contribute one summary input"
    );
    let after = unit.body_read_stats();
    let pgm_body_read_stats = PgmBodyReadStats {
        read_calls: after
            .read_calls
            .checked_sub(before.read_calls)
            .ok_or(BuildError::Internal)?,
        stored_bytes_read: after
            .stored_bytes_read
            .checked_sub(before.stored_bytes_read)
            .ok_or(BuildError::Internal)?,
    };
    Ok(WebIndexBlocks {
        summary,
        series,
        pgm_body_read_stats,
    })
}

fn canonical_collections(decoded: &[DecodedSection]) -> Result<CollectionTimeline, BuildError> {
    let mut facts = BTreeMap::<(u32, i64), CollectionFacts>::new();
    for section in decoded {
        match section.name {
            SNAPSHOT_COVERAGE => {
                for row in &section.rows {
                    let source_type = coverage_u32(row, "section_type_id")?;
                    let read_state = coverage_u32(row, "read_state")?;
                    let visibility = coverage_u32(row, "visibility")?;
                    let source_total = u64::from(coverage_u32(row, "source_total")?);
                    let collected = u64::from(coverage_u32(row, "collected")?);
                    if read_state > 4
                        || visibility > 2
                        || collected > source_total
                        || read_state == 0 && collected != source_total
                        || read_state == 1 && collected >= source_total
                    {
                        return corrupt();
                    }
                    let fact = CollectionFact {
                        ts_us: row_ts(row)?,
                        read_state: u8::try_from(read_state).map_err(|_error| corrupt_error())?,
                        visibility: u8::try_from(visibility).map_err(|_error| corrupt_error())?,
                        source_total,
                        collected,
                        total_exact: read_state <= 1,
                    };
                    insert_collection_fact(&mut facts, source_type, fact, true)?;
                }
            }
            COLLECTION_COVERAGE => {
                for row in &section.rows {
                    let reason = coverage_u32(row, "reason")?;
                    if reason > 3 {
                        return corrupt();
                    }
                    let source_type = coverage_u32(row, "section_type_id")?;
                    let source_total = u64::from(coverage_u32(row, "total")?);
                    let collected = u64::from(coverage_u32(row, "collected")?);
                    if collected > source_total {
                        return corrupt();
                    }
                    let fact = CollectionFact {
                        ts_us: row_ts(row)?,
                        read_state: match reason {
                            2 => 2,
                            1 | 3 => 3,
                            _ => 1,
                        },
                        visibility: match reason {
                            2 => 1,
                            1 | 3 => 2,
                            _ => 0,
                        },
                        source_total,
                        collected,
                        total_exact: !coverage_bool(row, "unknown_total")?,
                    };
                    insert_collection_fact(&mut facts, source_type, fact, false)?;
                }
            }
            _ => {}
        }
    }

    let mut collections = BTreeMap::new();
    for ((source_type, ts), facts) in facts {
        let fact = merge_collection_facts(&facts)?;
        let Some(view) = collection_view(source_type) else {
            continue;
        };
        let status = CollectionStatus::new(
            fact.collected,
            fact.total_exact.then_some(fact.source_total),
            collection_read_state(fact.read_state)?,
            collection_visibility(fact.visibility)?,
        )
        .map_err(|_error| corrupt_error())?;
        if collections
            .insert(
                (view.code, ts),
                ViewCollection {
                    source_type,
                    status,
                },
            )
            .is_some()
        {
            return corrupt();
        }
    }
    Ok(collections)
}

fn insert_collection_fact(
    facts: &mut BTreeMap<(u32, i64), CollectionFacts>,
    source_type: u32,
    fact: CollectionFact,
    snapshot: bool,
) -> Result<(), BuildError> {
    let pair = facts.entry((source_type, fact.ts_us)).or_default();
    let slot = if snapshot {
        &mut pair.snapshot
    } else {
        &mut pair.collection
    };
    if slot.replace(fact).is_some() {
        return corrupt();
    }
    Ok(())
}

fn merge_collection_facts(facts: &CollectionFacts) -> Result<CollectionFact, BuildError> {
    let Some(snapshot) = facts.snapshot else {
        return facts.collection.ok_or_else(corrupt_error);
    };
    let Some(collection) = facts.collection else {
        return Ok(snapshot);
    };
    if snapshot.ts_us != collection.ts_us || snapshot.collected != collection.collected {
        return corrupt();
    }
    let source_total = match (snapshot.total_exact, collection.total_exact) {
        (true, true) if snapshot.source_total != collection.source_total => return corrupt(),
        (true, false) if collection.source_total > snapshot.source_total => return corrupt(),
        (false, true) if snapshot.source_total > collection.source_total => return corrupt(),
        (_, true) => collection.source_total,
        _ => snapshot.source_total.max(collection.source_total),
    };
    if snapshot.read_state == 0 && snapshot.collected != source_total
        || snapshot.read_state == 1 && snapshot.collected >= source_total
    {
        return corrupt();
    }
    Ok(CollectionFact {
        source_total,
        total_exact: collection.total_exact,
        ..snapshot
    })
}

fn collection_view(source_type: u32) -> Option<&'static WebView> {
    let source_name = registry()
        .iter()
        .find(|contract| contract.type_id.get() == source_type)?
        .name;
    web_views().iter().find(|view| {
        view.name != "events"
            && view
                .inputs
                .first()
                .is_some_and(|input| input.sections.contains(&source_name))
    })
}

const fn collection_read_state(code: u8) -> Result<CollectionReadState, BuildError> {
    match code {
        0 => Ok(CollectionReadState::Complete),
        1 => Ok(CollectionReadState::SourceLimit),
        2 => Ok(CollectionReadState::Permission),
        3 => Ok(CollectionReadState::ReadFailure),
        4 => Ok(CollectionReadState::CollectorLimitOrLoss),
        _ => corrupt(),
    }
}

const fn collection_visibility(code: u8) -> Result<CollectionVisibility, BuildError> {
    match code {
        0 => Ok(CollectionVisibility::Full),
        1 => Ok(CollectionVisibility::Restricted),
        2 => Ok(CollectionVisibility::Unknown),
        _ => corrupt(),
    }
}

fn coverage_u32(row: &Row, field: &str) -> Result<u32, BuildError> {
    match row.get(field) {
        Some(Cell::U32(value)) => Ok(*value),
        Some(Cell::I16(value)) => u32::try_from(*value).map_err(|_error| corrupt_error()),
        Some(Cell::I32(value)) => u32::try_from(*value).map_err(|_error| corrupt_error()),
        Some(Cell::I64(value)) => u32::try_from(*value).map_err(|_error| corrupt_error()),
        _ => corrupt(),
    }
}

fn coverage_bool(row: &Row, field: &str) -> Result<bool, BuildError> {
    match row.get(field) {
        Some(Cell::Bool(value)) => Ok(*value),
        _ => corrupt(),
    }
}

const fn corrupt<T>() -> Result<T, BuildError> {
    Err(corrupt_error())
}

const fn corrupt_error() -> BuildError {
    BuildError::Source(SourceError::Corrupt)
}

fn build_summary(
    decoded: &[DecodedSection],
    available_sections: &BTreeSet<&'static str>,
    collections: &CollectionTimeline,
    observations: &[EventObservation],
    grid: TimeGrid,
    bounds: &Bounds,
) -> Result<(UiSummaryBlock, Vec<SummaryInput>), BuildError> {
    let mut all_times = BTreeSet::new();
    let mut inputs = Vec::with_capacity(web_views().len());
    let notable_policy = NotablePolicy::v1();
    let notable_buckets = observations
        .iter()
        .filter(|observation| notable_policy.classify(observation).is_some())
        .filter_map(|observation| grid.bucket_index(observation.time().sort_ts_us))
        .collect::<BTreeSet<_>>();
    for view in web_views() {
        let mut populations = BTreeMap::<i64, u64>::new();
        let mut population_types = BTreeMap::<i64, u32>::new();
        if view.name == "events" {
            for observation in observations {
                let ts = observation.time().sort_ts_us;
                all_times.insert(ts);
                let population = populations.entry(ts).or_default();
                *population = population.checked_add(1).ok_or(BuildError::Overflow)?;
            }
        } else {
            for row in rows_for_input(decoded, &view.inputs[0])? {
                let ts = row_ts(row.row)?;
                all_times.insert(ts);
                let population = populations.entry(ts).or_default();
                *population = population.checked_add(1).ok_or(BuildError::Overflow)?;
                match population_types.entry(ts) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(row.type_id);
                    }
                    std::collections::btree_map::Entry::Occupied(slot)
                        if *slot.get() == row.type_id => {}
                    std::collections::btree_map::Entry::Occupied(_) => {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                }
            }
        }
        let mut view_collections = BTreeMap::new();
        for ((_, ts), collection) in
            collections.range((view.code, i64::MIN)..=(view.code, i64::MAX))
        {
            all_times.insert(*ts);
            match populations.get(ts) {
                Some(population) if *population != collection.status.collected() => {
                    return Err(BuildError::Source(SourceError::Corrupt));
                }
                None => {
                    populations.insert(*ts, collection.status.collected());
                }
                Some(_) => {}
            }
            match population_types.get(ts) {
                Some(actual_type) if *actual_type != collection.source_type => {
                    return Err(BuildError::Source(SourceError::Corrupt));
                }
                None => {
                    population_types.insert(*ts, collection.source_type);
                }
                Some(_) => {}
            }
            view_collections.insert(*ts, collection.status);
        }
        let available = input_available(available_sections, &view.inputs[0]);
        let status = if !available && view_collections.is_empty() {
            IndexStatus::Gated
        } else if populations.is_empty() {
            IndexStatus::Empty
        } else {
            IndexStatus::Complete
        };
        inputs.push(SummaryInput {
            view_code: view.code,
            view_revision: view.revision,
            status,
            populations,
            collections: view_collections,
        });
    }
    let snapshot_times = all_times.into_iter().collect::<Vec<_>>();
    if snapshot_times.len() as u64 > bounds.web_summary_timestamps {
        return Err(BuildError::LimitExceeded);
    }
    let mut views = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let mut presence = vec![0_u8; mask_len(snapshot_times.len())];
        let mut notable = vec![0_u8; presence.len()];
        let mut collection_presence = vec![0_u8; presence.len()];
        let mut populations = Vec::with_capacity(input.populations.len());
        let mut collections = Vec::with_capacity(input.collections.len());
        for (index, ts) in snapshot_times.iter().enumerate() {
            if let Some(population) = input.populations.get(ts) {
                presence[index / 8] |= 1 << (index % 8);
                if grid
                    .bucket_index(*ts)
                    .is_some_and(|bucket| notable_buckets.contains(&bucket))
                {
                    notable[index / 8] |= 1 << (index % 8);
                }
                populations.push(*population);
            }
            if let Some(collection) = input.collections.get(ts) {
                collection_presence[index / 8] |= 1 << (index % 8);
                collections.push(*collection);
            }
        }
        views.push(
            ViewSummary::new_with_collection(
                input.view_code,
                input.view_revision,
                input.status,
                presence,
                notable,
                populations,
                collection_presence,
                collections,
                bounds,
            )
            .map_err(block_build_error)?,
        );
    }
    let summary =
        UiSummaryBlock::new(grid, snapshot_times, views, bounds).map_err(block_build_error)?;
    Ok((summary, inputs))
}

fn build_view_series(
    view: &'static WebView,
    decoded: &[DecodedSection],
    available_sections: &BTreeSet<&'static str>,
    dictionary: &Dictionary,
    grid: TimeGrid,
    primary: Vec<IndexedRow<'_>>,
    bounds: &Bounds,
) -> Result<EntitySeriesBlock, BuildError> {
    let mut evaluated = Vec::with_capacity(view.metrics.len());
    for metric in view.metrics {
        let requirements_available = metric.requires.iter().all(|required| {
            input_by_code(view, required)
                .is_some_and(|input| input_available(available_sections, input))
        });
        if !requirements_available || metric.requires.len() != 1 {
            evaluated.push(EvaluatedMetric {
                metric,
                status: MetricStatus::Gated,
                candidates: Vec::new(),
            });
            continue;
        }
        let input = input_by_code(view, metric.requires[0]).ok_or(BuildError::Internal)?;
        let rows = rows_for_input(decoded, input)?;
        let evaluation = evaluate_metric(view, metric, &rows, dictionary, grid)?;
        let (status, mut candidates) = match evaluation {
            Evaluation::Complete(candidates) => (MetricStatus::Complete, candidates),
            Evaluation::Unsupported => (MetricStatus::UnsupportedType, Vec::new()),
        };
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
        candidates.truncate(TOP_K);
        evaluated.push(EvaluatedMetric {
            metric,
            status,
            candidates,
        });
    }

    let mut labels = BTreeMap::<Vec<u8>, String>::new();
    for candidate in evaluated.iter().flat_map(|metric| metric.candidates.iter()) {
        labels.insert(candidate.key.clone(), candidate.label.clone());
    }
    if labels.len() as u64 > bounds.web_dictionary_entries {
        return Err(BuildError::LimitExceeded);
    }
    let dictionary_entries = labels
        .iter()
        .map(|(key, label)| EntityDictionaryEntry::new(key.clone(), label.clone(), bounds))
        .collect::<Result<Vec<_>, _>>()
        .map_err(block_build_error)?;
    let references = labels
        .keys()
        .enumerate()
        .map(|(index, key)| {
            u16::try_from(index)
                .map(|entity_ref| (key.clone(), entity_ref))
                .map_err(|_error| BuildError::LimitExceeded)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut metrics = Vec::with_capacity(evaluated.len());
    for evaluated_metric in evaluated {
        let mut series = evaluated_metric
            .candidates
            .into_iter()
            .map(|candidate| {
                let entity_ref = *references.get(&candidate.key).ok_or(BuildError::Internal)?;
                encode_candidate(entity_ref, candidate, bounds)
            })
            .collect::<Result<Vec<_>, _>>()?;
        series.sort_by(|left, right| {
            right
                .exact_score()
                .total_cmp(&left.exact_score())
                .then_with(|| left.entity_ref().cmp(&right.entity_ref()))
        });
        let cutoff_score = if series.len() == TOP_K {
            series.last().map_or(0.0, EntitySeries::exact_score)
        } else {
            0.0
        };
        let flags = if evaluated_metric.metric.canonical {
            METRIC_FLAG_CANONICAL
        } else {
            0
        };
        metrics.push(
            EntityMetric::new(
                evaluated_metric.metric.code,
                evaluated_metric.metric.revision,
                flags,
                evaluated_metric.metric.unit.code(),
                match evaluated_metric.metric.aggregation {
                    WebAggregation::Sum => MetricAggregation::Sum,
                    WebAggregation::Max => MetricAggregation::Max,
                },
                evaluated_metric.status,
                cutoff_score,
                series,
                bounds,
            )
            .map_err(block_build_error)?,
        );
    }

    let observed_range =
        primary
            .iter()
            .try_fold((i64::MAX, i64::MIN), |(first, last), indexed| {
                let ts = row_ts(indexed.row)?;
                Ok::<_, BuildError>((first.min(ts), last.max(ts)))
            })?;
    let mut coverage_mask = vec![0_u8; mask_len(usize::from(grid.bucket_count()))];
    for indexed in primary {
        let bucket = grid
            .bucket_index(row_ts(indexed.row)?)
            .ok_or(BuildError::Internal)?;
        coverage_mask[bucket / 8] |= 1 << (bucket % 8);
    }
    EntitySeriesBlock::new(
        view.code,
        view.revision,
        view.identity_revision,
        IndexStatus::Complete,
        observed_range,
        grid,
        coverage_mask,
        dictionary_entries,
        metrics,
        bounds,
    )
    .map_err(block_build_error)
}

fn build_event_series(
    view: &'static WebView,
    observations: &[EventObservation],
    grid: TimeGrid,
    bounds: &Bounds,
) -> Result<EntitySeriesBlock, BuildError> {
    let metric = view.metrics.first().ok_or(BuildError::Internal)?;
    let mut categories = BTreeMap::<Vec<u8>, (String, Vec<Option<f64>>)>::new();
    for observation in observations {
        let category = observation.payload().kind_code();
        let category_len =
            u16::try_from(category.len()).map_err(|_error| BuildError::LimitExceeded)?;
        let mut key = Vec::with_capacity(4 + category.len());
        key.extend_from_slice(&view.identity_revision.to_le_bytes());
        key.extend_from_slice(&category_len.to_le_bytes());
        key.extend_from_slice(category.as_bytes());
        let entry = categories
            .entry(key)
            .or_insert_with(|| (bounded_label(category, 160), empty_buckets(grid)));
        insert_bucket(
            &mut entry.1,
            grid,
            observation.time().sort_ts_us,
            observation.occurrence_count() as f64,
            metric.aggregation,
        )?;
    }
    let mut candidates = categories
        .into_iter()
        .map(|(key, (label, buckets))| candidate(key, label, buckets, metric.aggregation))
        .collect::<Result<Vec<_>, BuildError>>()?;
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.key.cmp(&right.key))
    });
    candidates.truncate(TOP_K);

    let labels = candidates
        .iter()
        .map(|candidate| (candidate.key.clone(), candidate.label.clone()))
        .collect::<BTreeMap<_, _>>();
    let dictionary = labels
        .iter()
        .map(|(key, label)| EntityDictionaryEntry::new(key.clone(), label.clone(), bounds))
        .collect::<Result<Vec<_>, _>>()
        .map_err(block_build_error)?;
    let references = labels
        .keys()
        .enumerate()
        .map(|(index, key)| {
            u16::try_from(index)
                .map(|entity_ref| (key.clone(), entity_ref))
                .map_err(|_error| BuildError::LimitExceeded)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut series = candidates
        .into_iter()
        .map(|candidate| {
            let entity_ref = *references.get(&candidate.key).ok_or(BuildError::Internal)?;
            encode_candidate(entity_ref, candidate, bounds)
        })
        .collect::<Result<Vec<_>, _>>()?;
    series.sort_by(|left, right| {
        right
            .exact_score()
            .total_cmp(&left.exact_score())
            .then_with(|| left.entity_ref().cmp(&right.entity_ref()))
    });
    let cutoff_score = if series.len() == TOP_K {
        series.last().map_or(0.0, EntitySeries::exact_score)
    } else {
        0.0
    };
    let entity_metric = EntityMetric::new(
        metric.code,
        metric.revision,
        METRIC_FLAG_CANONICAL,
        metric.unit.code(),
        MetricAggregation::Sum,
        MetricStatus::Complete,
        cutoff_score,
        series,
        bounds,
    )
    .map_err(block_build_error)?;
    let observed_range =
        observations
            .iter()
            .fold((i64::MAX, i64::MIN), |(first, last), observation| {
                let ts = observation.time().sort_ts_us;
                (first.min(ts), last.max(ts))
            });
    let mut coverage_mask = vec![0_u8; mask_len(usize::from(grid.bucket_count()))];
    for observation in observations {
        let bucket = grid
            .bucket_index(observation.time().sort_ts_us)
            .ok_or(BuildError::Internal)?;
        coverage_mask[bucket / 8] |= 1 << (bucket % 8);
    }
    EntitySeriesBlock::new(
        view.code,
        view.revision,
        view.identity_revision,
        IndexStatus::Complete,
        observed_range,
        grid,
        coverage_mask,
        dictionary,
        vec![entity_metric],
        bounds,
    )
    .map_err(block_build_error)
}

fn evaluate_metric(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
) -> Result<Evaluation, BuildError> {
    match metric.formula {
        WebFormula::PositiveDeltaSum {
            field_sets, scale, ..
        } => evaluate_deltas(
            view, metric, rows, dictionary, grid, field_sets, scale, false,
        ),
        WebFormula::PositiveDeltaRate { field_sets, .. } => {
            evaluate_deltas(view, metric, rows, dictionary, grid, field_sets, 1.0, true)
        }
        WebFormula::GaugeRatio {
            numerator,
            denominator,
            ..
        } => evaluate_gauge_ratio(view, metric, rows, dictionary, grid, numerator, denominator),
        WebFormula::EventCount { .. } => evaluate_events(view, metric, rows, dictionary, grid),
        WebFormula::ActiveFraction { .. } => {
            evaluate_active_fraction(view, metric, rows, dictionary, grid)
        }
        WebFormula::ActivityWait { .. } => {
            evaluate_activity_wait(view, metric, rows, dictionary, grid)
        }
        WebFormula::LockDuration { .. } => {
            evaluate_lock_duration(view, metric, rows, dictionary, grid)
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the evaluator receives the complete declarative delta formula"
)]
fn evaluate_deltas(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
    field_sets: &[&[&str]],
    scale: f64,
    rate: bool,
) -> Result<Evaluation, BuildError> {
    let compatible = rows
        .iter()
        .copied()
        .filter(|indexed| compatible_field_set(indexed.row, field_sets).is_some())
        .collect::<Vec<_>>();
    if compatible.is_empty() {
        return Ok(Evaluation::Unsupported);
    }
    let mut grouped = group_rows(view, &compatible, dictionary)?;
    let mut candidates = Vec::new();
    for (key, (label, entity_rows)) in &mut grouped {
        entity_rows.sort_by_key(|indexed| row_ts(indexed.row).unwrap_or(i64::MIN));
        let mut buckets = empty_buckets(grid);
        let mut previous = None;
        for indexed in entity_rows {
            let ts = row_ts(indexed.row)?;
            let Some(fields) = compatible_field_set(indexed.row, field_sets) else {
                continue;
            };
            let Some(current) = sum_fields(indexed.row, fields) else {
                continue;
            };
            if let Some((previous_ts, previous_value)) = previous
                && ts > previous_ts
                && current >= previous_value
            {
                let mut value = (current - previous_value) * scale;
                if rate {
                    value /= (ts - previous_ts) as f64 / 1_000_000.0;
                }
                insert_bucket(&mut buckets, grid, ts, value, metric.aggregation)?;
            }
            previous = Some((ts, current));
        }
        if buckets.iter().any(Option::is_some) {
            candidates.push(candidate(
                key.clone(),
                label.clone(),
                buckets,
                metric.aggregation,
            )?);
        }
    }
    Ok(Evaluation::Complete(candidates))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the evaluator receives the complete declarative ratio formula"
)]
fn evaluate_gauge_ratio(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
    numerator: &str,
    denominator: &str,
) -> Result<Evaluation, BuildError> {
    let compatible = rows
        .iter()
        .copied()
        .filter(|indexed| {
            indexed.row.contract().column(numerator).is_some()
                && indexed.row.contract().column(denominator).is_some()
        })
        .collect::<Vec<_>>();
    if compatible.is_empty() {
        return Ok(Evaluation::Unsupported);
    }
    let mut entities = BTreeMap::<Vec<u8>, (String, Vec<Option<f64>>)>::new();
    for indexed in compatible {
        let (key, label) = identity_and_label(view, indexed, dictionary)?;
        let Some(numerator_value) = cell_number(indexed.row.get(numerator)) else {
            continue;
        };
        let Some(mut denominator_value) = cell_number(indexed.row.get(denominator)) else {
            continue;
        };
        if view.name == "tables" {
            denominator_value += numerator_value;
        }
        let value = numerator_value / denominator_value.max(1.0);
        let entry = entities
            .entry(key)
            .or_insert_with(|| (label.clone(), empty_buckets(grid)));
        entry.0 = label;
        insert_bucket(
            &mut entry.1,
            grid,
            row_ts(indexed.row)?,
            value,
            metric.aggregation,
        )?;
    }
    Ok(Evaluation::Complete(
        entities
            .into_iter()
            .map(|(key, (label, buckets))| candidate(key, label, buckets, metric.aggregation))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn evaluate_events(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
) -> Result<Evaluation, BuildError> {
    let mut candidates = Vec::with_capacity(rows.len());
    for indexed in rows {
        let (key, label) = identity_and_label(view, *indexed, dictionary)?;
        let mut buckets = empty_buckets(grid);
        insert_bucket(
            &mut buckets,
            grid,
            row_ts(indexed.row)?,
            1.0,
            metric.aggregation,
        )?;
        candidates.push(candidate(key, label, buckets, metric.aggregation)?);
    }
    Ok(Evaluation::Complete(candidates))
}

fn evaluate_active_fraction(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
) -> Result<Evaluation, BuildError> {
    if rows
        .iter()
        .all(|indexed| indexed.row.contract().column("state").is_none())
    {
        return Ok(Evaluation::Unsupported);
    }
    let bucket_count = usize::from(grid.bucket_count());
    let mut entities = BTreeMap::<Vec<u8>, (String, Vec<u64>, Vec<u64>)>::new();
    for indexed in rows {
        let (key, label) = identity_and_label(view, *indexed, dictionary)?;
        let bucket = grid
            .bucket_index(row_ts(indexed.row)?)
            .ok_or(BuildError::Internal)?;
        let entry = entities.entry(key).or_insert_with(|| {
            (
                label.clone(),
                vec![0_u64; bucket_count],
                vec![0_u64; bucket_count],
            )
        });
        entry.0 = label;
        entry.2[bucket] = entry.2[bucket].checked_add(1).ok_or(BuildError::Overflow)?;
        if cell_is_string(indexed.row.get("state"), dictionary, b"active") {
            entry.1[bucket] = entry.1[bucket].checked_add(1).ok_or(BuildError::Overflow)?;
        }
    }
    let candidates = entities
        .into_iter()
        .map(|(key, (label, active, observed))| {
            let buckets = active
                .into_iter()
                .zip(observed)
                .map(|(active, observed)| {
                    (observed != 0).then_some(active as f64 / observed as f64)
                })
                .collect();
            candidate(key, label, buckets, metric.aggregation)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Evaluation::Complete(candidates))
}

fn evaluate_activity_wait(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
) -> Result<Evaluation, BuildError> {
    if rows
        .iter()
        .all(|indexed| indexed.row.contract().column("wait_event").is_none())
    {
        return Ok(Evaluation::Unsupported);
    }
    let mut grouped = group_rows(view, rows, dictionary)?;
    let mut candidates = Vec::new();
    for (key, (label, entity_rows)) in &mut grouped {
        entity_rows.sort_by_key(|indexed| row_ts(indexed.row).unwrap_or(i64::MIN));
        let mut buckets = empty_buckets(grid);
        let mut previous_ts = None;
        for indexed in entity_rows {
            let ts = row_ts(indexed.row)?;
            if let Some(previous) = previous_ts
                && ts > previous
                && !matches!(indexed.row.get("wait_event"), None | Some(Cell::Null))
            {
                insert_bucket(
                    &mut buckets,
                    grid,
                    ts,
                    (ts - previous) as f64,
                    metric.aggregation,
                )?;
            }
            previous_ts = Some(ts);
        }
        if buckets.iter().any(Option::is_some) {
            candidates.push(candidate(
                key.clone(),
                label.clone(),
                buckets,
                metric.aggregation,
            )?);
        }
    }
    Ok(Evaluation::Complete(candidates))
}

fn evaluate_lock_duration(
    view: &'static WebView,
    metric: &'static WebMetric,
    rows: &[IndexedRow<'_>],
    dictionary: &Dictionary,
    grid: TimeGrid,
) -> Result<Evaluation, BuildError> {
    let mut entities = BTreeMap::<Vec<u8>, (String, Vec<Option<f64>>)>::new();
    for indexed in rows {
        let ts = row_ts(indexed.row)?;
        let start = ["waitstart", "xact_start", "query_start"]
            .iter()
            .find_map(|field| cell_timestamp(indexed.row.get(field)));
        let Some(start) = start.filter(|start| *start <= ts) else {
            continue;
        };
        let (key, label) = identity_and_label(view, *indexed, dictionary)?;
        let entry = entities
            .entry(key)
            .or_insert_with(|| (label.clone(), empty_buckets(grid)));
        entry.0 = label;
        insert_bucket(
            &mut entry.1,
            grid,
            ts,
            (ts - start) as f64,
            metric.aggregation,
        )?;
    }
    Ok(Evaluation::Complete(
        entities
            .into_iter()
            .map(|(key, (label, buckets))| candidate(key, label, buckets, metric.aggregation))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn group_rows<'a>(
    view: &WebView,
    rows: &[IndexedRow<'a>],
    dictionary: &Dictionary,
) -> Result<GroupedRows<'a>, BuildError> {
    let mut grouped = GroupedRows::new();
    for indexed in rows {
        let (key, label) = identity_and_label(view, *indexed, dictionary)?;
        let entry = grouped
            .entry(key)
            .or_insert_with(|| (label.clone(), Vec::new()));
        entry.0 = label;
        entry.1.push(*indexed);
    }
    Ok(grouped)
}

fn identity_and_label(
    view: &WebView,
    indexed: IndexedRow<'_>,
    dictionary: &Dictionary,
) -> Result<(Vec<u8>, String), BuildError> {
    if view.name == "events" {
        let mut key = Vec::new();
        key.extend_from_slice(&view.identity_revision.to_le_bytes());
        key.extend_from_slice(&indexed.type_id.to_le_bytes());
        key.extend_from_slice(&indexed.catalog_ordinal.to_le_bytes());
        key.extend_from_slice(&indexed.row_ordinal.to_le_bytes());
        key.extend_from_slice(&row_ts(indexed.row)?.to_le_bytes());
        return Ok((key, bounded_label(indexed.section, 160)));
    }
    let fallback_fields;
    let fields: &[&str] = match view.name {
        "activity" | "locks" => &["pid", "backend_start"],
        "processes" => &["pid", "starttime"],
        "vacuum" => &["pid", "datid", "relid"],
        _ if !indexed.row.contract().identity.is_empty() => indexed.row.contract().identity,
        _ => {
            fallback_fields = indexed
                .row
                .contract()
                .sort_key
                .iter()
                .copied()
                .filter(|field| *field != "ts")
                .collect::<Vec<_>>();
            &fallback_fields
        }
    };
    let mut key = Vec::new();
    key.extend_from_slice(&view.identity_revision.to_le_bytes());
    for field in fields {
        let cell = indexed.row.get(field).ok_or(BuildError::Internal)?;
        encode_cell(&mut key, cell)?;
    }
    let label = view_label(view.name, indexed.row, dictionary).unwrap_or_else(|| hex_label(&key));
    Ok((key, bounded_label(&label, 160)))
}

fn view_label(view: &str, row: &Row, dictionary: &Dictionary) -> Option<String> {
    let fields: &[&str] = match view {
        "activity" => &["pid", "application_name"],
        "statements" => &["queryid"],
        "plans" => &["planid"],
        "tables" => &["schemaname", "relname"],
        "indexes" => &["schemaname", "indexrelname"],
        "vacuum" => &["pid", "relid"],
        "processes" => &["comm", "pid"],
        "locks" => &["pid", "lock_target"],
        _ => return None,
    };
    let values = fields
        .iter()
        .filter_map(|field| display_cell(row.get(field)?, dictionary))
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join(" / "))
}

fn rows_for_input<'a>(
    decoded: &'a [DecodedSection],
    input: &WebInput,
) -> Result<Vec<IndexedRow<'a>>, BuildError> {
    let mut rows = Vec::new();
    for section in decoded
        .iter()
        .filter(|section| input.sections.contains(&section.name))
    {
        for (row_ordinal, row) in section.rows.iter().enumerate() {
            rows.push(IndexedRow {
                catalog_ordinal: section.catalog_ordinal,
                row_ordinal: u32::try_from(row_ordinal).map_err(|_error| BuildError::Overflow)?,
                type_id: section.type_id,
                section: section.name,
                row,
            });
        }
    }
    Ok(rows)
}

fn input_by_code<'a>(view: &'a WebView, code: &str) -> Option<&'a WebInput> {
    view.inputs.iter().find(|input| input.code == code)
}

fn input_available(available_sections: &BTreeSet<&'static str>, input: &WebInput) -> bool {
    input
        .sections
        .iter()
        .any(|section| available_sections.contains(section))
}

fn row_ts(row: &Row) -> Result<i64, BuildError> {
    match row.get("ts") {
        Some(Cell::Ts(ts)) => Ok(*ts),
        _ => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn empty_buckets(grid: TimeGrid) -> Vec<Option<f64>> {
    vec![None; usize::from(grid.bucket_count())]
}

fn insert_bucket(
    buckets: &mut [Option<f64>],
    grid: TimeGrid,
    ts: i64,
    value: f64,
    aggregation: WebAggregation,
) -> Result<(), BuildError> {
    if !value.is_finite() || value < 0.0 {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    let bucket = grid.bucket_index(ts).ok_or(BuildError::Internal)?;
    buckets[bucket] = Some(match (buckets[bucket], aggregation) {
        (Some(current), WebAggregation::Sum) => {
            let value = current + value;
            if !value.is_finite() {
                return Err(BuildError::Overflow);
            }
            value
        }
        (Some(current), WebAggregation::Max) => current.max(value),
        (None, _) => value,
    });
    Ok(())
}

fn candidate(
    key: Vec<u8>,
    label: String,
    buckets: Vec<Option<f64>>,
    aggregation: WebAggregation,
) -> Result<Candidate, BuildError> {
    let observed = buckets.iter().flatten().copied();
    let score = match aggregation {
        WebAggregation::Sum => observed.sum::<f64>(),
        WebAggregation::Max => observed.fold(0.0, f64::max),
    };
    if !score.is_finite() || score < 0.0 {
        return Err(BuildError::Overflow);
    }
    Ok(Candidate {
        key,
        label,
        buckets,
        score,
    })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the rounded value is explicitly clamped to the complete u8 range"
)]
fn encode_candidate(
    entity_ref: u16,
    candidate: Candidate,
    bounds: &Bounds,
) -> Result<EntitySeries, BuildError> {
    let mut present_mask = vec![0_u8; mask_len(candidate.buckets.len())];
    let max_bucket_value = candidate
        .buckets
        .iter()
        .flatten()
        .copied()
        .fold(0.0, f64::max);
    let mut quantized = Vec::new();
    for (index, value) in candidate.buckets.into_iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        present_mask[index / 8] |= 1 << (index % 8);
        let encoded = if max_bucket_value == 0.0 {
            0
        } else {
            (value / max_bucket_value * 255.0).round().clamp(0.0, 255.0) as u8
        };
        quantized.push(encoded);
    }
    EntitySeries::new(
        entity_ref,
        candidate.score,
        max_bucket_value,
        present_mask,
        quantized,
        bounds,
    )
    .map_err(block_build_error)
}

fn sum_fields(row: &Row, fields: &[&str]) -> Option<f64> {
    fields.iter().try_fold(0.0, |total, field| {
        let value = match row.get(field)? {
            Cell::Null => 0.0,
            cell => cell_number(Some(cell))?,
        };
        let sum = total + value;
        sum.is_finite().then_some(sum)
    })
}

fn compatible_field_set<'a>(row: &Row, field_sets: &'a [&[&str]]) -> Option<&'a [&'a str]> {
    field_sets.iter().copied().find(|fields| {
        fields
            .iter()
            .all(|field| row.contract().column(field).is_some())
    })
}

fn cell_number(cell: Option<&Cell>) -> Option<f64> {
    let value = match cell? {
        Cell::I16(value) => f64::from(*value),
        Cell::I32(value) => f64::from(*value),
        Cell::I64(value) | Cell::Ts(value) => *value as f64,
        Cell::U32(value) => f64::from(*value),
        Cell::U64(value) => *value as f64,
        Cell::F64(value) => *value,
        Cell::Bool(value) => f64::from(*value),
        Cell::StrId(_) | Cell::ListI32(_) | Cell::Null => return None,
    };
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn cell_timestamp(cell: Option<&Cell>) -> Option<i64> {
    match cell? {
        Cell::Ts(value) => Some(*value),
        _ => None,
    }
}

fn cell_is_string(cell: Option<&Cell>, dictionary: &Dictionary, expected: &[u8]) -> bool {
    let Some(Cell::StrId(str_id)) = cell else {
        return false;
    };
    match dictionary.resolve(*str_id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => bytes == expected,
        None => false,
    }
}

fn display_cell(cell: &Cell, dictionary: &Dictionary) -> Option<String> {
    match cell {
        Cell::I16(value) => Some(value.to_string()),
        Cell::I32(value) => Some(value.to_string()),
        Cell::I64(value) | Cell::Ts(value) => Some(value.to_string()),
        Cell::U32(value) => Some(value.to_string()),
        Cell::U64(value) => Some(value.to_string()),
        Cell::F64(value) => value.is_finite().then(|| value.to_string()),
        Cell::Bool(value) => Some(value.to_string()),
        Cell::StrId(str_id) => match dictionary.resolve(*str_id) {
            Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            None => Some(format!("{str_id:016x}")),
        },
        Cell::ListI32(values) => Some(
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
        Cell::Null => None,
    }
}

fn encode_cell(output: &mut Vec<u8>, cell: &Cell) -> Result<(), BuildError> {
    match cell {
        Cell::I16(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::I32(value) => {
            output.push(2);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::I64(value) => {
            output.push(3);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::U32(value) => {
            output.push(4);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::U64(value) => {
            output.push(5);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::F64(value) if value.is_finite() => {
            output.push(6);
            output.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        Cell::Bool(value) => {
            output.push(7);
            output.push(u8::from(*value));
        }
        Cell::Ts(value) => {
            output.push(8);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::StrId(value) => {
            output.push(9);
            output.extend_from_slice(&value.to_le_bytes());
        }
        Cell::ListI32(values) => {
            output.push(10);
            output.extend_from_slice(
                &u16::try_from(values.len())
                    .map_err(|_error| BuildError::LimitExceeded)?
                    .to_le_bytes(),
            );
            for value in values {
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        Cell::Null => output.push(0),
        Cell::F64(_) => return Err(BuildError::Source(SourceError::Corrupt)),
    }
    Ok(())
}

fn bounded_label(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut bounded = String::with_capacity(maximum);
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > maximum {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn hex_label(key: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(key.len().saturating_mul(2));
    for byte in key {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    value
}

const fn block_build_error(error: BlockError) -> BuildError {
    match error {
        BlockError::AboveBound => BuildError::LimitExceeded,
        _ => BuildError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use kronika_analytics::web_projection::{WebAggregation, web_view_by_name};
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_registry::collection_coverage::CollectionCoverageV1;
    use kronika_registry::pg_stat_user_indexes::PgStatUserIndexesV1;
    use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
    use kronika_registry::{Cell, Row, Section, StrId, Ts, registry};

    use super::{build_web_index, candidate, encode_candidate};
    use crate::PgmUnit;
    use crate::overview::facts::{BuildError, SourceError};
    use crate::overview::limits::LIMIT;
    use crate::overview::web_index::{CollectionReadState, CollectionVisibility, UiSummaryBlock};

    const COVERAGE_TS: i64 = 100;

    fn snapshot_coverage(
        section_type_id: u32,
        read_state: u8,
        visibility: u8,
        source_total: u32,
    ) -> SnapshotCoverageV1 {
        SnapshotCoverageV1 {
            ts: Ts(COVERAGE_TS),
            section_type_id,
            collector_pid: 42,
            collector_started_at: Ts(1),
            read_state,
            visibility,
            source_total,
            collected: 0,
        }
    }

    fn collection_coverage(
        section_type_id: u32,
        total: u32,
        unknown_total: bool,
        reason: u8,
    ) -> CollectionCoverageV1 {
        CollectionCoverageV1 {
            ts: Ts(COVERAGE_TS),
            section_type_id,
            total,
            unknown_total,
            collected: 0,
            max_n: 500,
            order_by: StrId(1),
            cutoff_value: None,
            reason,
        }
    }

    fn coverage_pgm(
        snapshots: &[SnapshotCoverageV1],
        collections: &[CollectionCoverageV1],
    ) -> Vec<u8> {
        coverage_pgm_with_sources(Vec::new(), snapshots, collections)
    }

    fn coverage_pgm_with_sources(
        mut bodies: Vec<(u32, u32, Vec<u8>)>,
        snapshots: &[SnapshotCoverageV1],
        collections: &[CollectionCoverageV1],
    ) -> Vec<u8> {
        if !collections.is_empty() {
            bodies.push((
                1_023_001,
                u32::try_from(collections.len()).expect("small collection fixture"),
                CollectionCoverageV1::encode(collections).expect("encode collection coverage"),
            ));
        }
        if !snapshots.is_empty() {
            bodies.push((
                1_038_001,
                u32::try_from(snapshots.len()).expect("small snapshot fixture"),
                SnapshotCoverageV1::encode(snapshots).expect("encode snapshot coverage"),
            ));
        }
        bodies.sort_unstable_by_key(|(type_id, _, _)| *type_id);
        let inputs = bodies
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect::<Vec<_>>();
        build_part(
            &inputs,
            PartMeta {
                min_ts: COVERAGE_TS,
                max_ts: COVERAGE_TS,
            },
        )
    }

    fn index_row() -> PgStatUserIndexesV1 {
        PgStatUserIndexesV1 {
            ts: Ts(COVERAGE_TS),
            datid: 1,
            datname: StrId(1),
            indexrelid: 2,
            relid: 3,
            schemaname: StrId(2),
            relname: StrId(3),
            indexrelname: StrId(4),
            tablespace: StrId(5),
            idx_scan: 1,
            idx_tup_read: 2,
            idx_tup_fetch: 3,
            main_fork_bytes: 8_192,
            indisunique: false,
            indisprimary: false,
            indisvalid: true,
            indisexclusion: false,
            indisready: true,
            amname: StrId(6),
            indexdef: StrId(7),
            idx_blks_read: 4,
            idx_blks_hit: 5,
        }
    }

    fn build_summary(bytes: &[u8]) -> Result<UiSummaryBlock, BuildError> {
        let unit =
            PgmUnit::open(bytes).map_err(|_error| BuildError::Source(SourceError::Corrupt))?;
        build_web_index(&unit, &[], COVERAGE_TS, COVERAGE_TS, &LIMIT).map(|blocks| blocks.summary)
    }

    #[test]
    fn quantization_preserves_observed_zero_and_missing_as_distinct() {
        let candidate = candidate(
            vec![1],
            "one".to_owned(),
            vec![Some(0.0), None, Some(10.0)],
            WebAggregation::Sum,
        )
        .expect("candidate");
        let series = encode_candidate(0, candidate, &LIMIT).expect("series");
        assert_eq!(series.present_mask(), &[0b0000_0101]);
        assert_eq!(series.quantized_values(), &[0, 255]);
        assert_eq!(series.value_at(0), Some(0.0));
        assert_eq!(series.value_at(1), None);
        assert_eq!(series.value_at(2), Some(10.0));
    }

    #[test]
    fn statements_registry_formula_requires_real_counter_fields() {
        let statements = web_view_by_name("statements").expect("statements");
        let contract = registry()
            .iter()
            .find(|contract| contract.name == "pg_stat_statements")
            .expect("statement contract");
        let row = Row::new(contract, vec![Cell::Null; contract.columns.len()]);
        let time = statements.metrics.first().expect("time metric");
        let kronika_analytics::web_projection::WebFormula::PositiveDeltaSum { field_sets, .. } =
            time.formula
        else {
            panic!("time is a positive delta");
        };
        assert!(field_sets.iter().any(|fields| {
            fields
                .iter()
                .all(|field| row.contract().column(field).is_some())
        }));
    }

    #[test]
    fn physical_coverage_maps_to_all_four_collection_views() {
        for (source_type, view_code) in [
            (1_002_001, 2),
            (1_003_001, 3),
            (1_004_001, 3),
            (1_013_001, 4),
            (1_014_001, 5),
        ] {
            let snapshot = snapshot_coverage(source_type, 1, 0, 10);
            let collection = collection_coverage(source_type, 10, false, 0);
            let bytes = coverage_pgm(&[snapshot], &[collection]);
            let summary = build_summary(&bytes).expect("build collection summary");
            let (_, status) = summary
                .collection_state_at(view_code, COVERAGE_TS)
                .expect("mapped collection status");

            assert_eq!(status.collected(), 0);
            assert_eq!(status.source_total(), Some(10));
            assert_eq!(status.read_state(), CollectionReadState::SourceLimit);
            assert_eq!(status.visibility(), CollectionVisibility::Full);
        }
    }

    #[test]
    fn complete_failure_and_collector_loss_remain_factual() {
        for (snapshot, collection, expected_state, expected_total) in [
            (
                snapshot_coverage(1_002_001, 0, 0, 0),
                None,
                CollectionReadState::Complete,
                Some(0),
            ),
            (
                snapshot_coverage(1_002_001, 3, 2, 0),
                Some(collection_coverage(1_002_001, 0, true, 1)),
                CollectionReadState::ReadFailure,
                None,
            ),
            (
                snapshot_coverage(1_002_001, 4, 2, u32::MAX),
                Some(collection_coverage(1_002_001, u32::MAX, true, 3)),
                CollectionReadState::CollectorLimitOrLoss,
                None,
            ),
        ] {
            let collections = collection.into_iter().collect::<Vec<_>>();
            let bytes = coverage_pgm(&[snapshot], &collections);
            let summary = build_summary(&bytes).expect("build factual state");
            let (_, status) = summary
                .collection_state_at(2, COVERAGE_TS)
                .expect("statement status");

            assert_eq!(status.source_total(), expected_total);
            assert_eq!(status.read_state(), expected_state);
        }
    }

    #[test]
    fn duplicate_and_revision_conflicting_coverage_is_corrupt() {
        let duplicate = snapshot_coverage(1_002_001, 0, 0, 0);
        let bytes = coverage_pgm(&[duplicate, duplicate], &[]);
        assert!(matches!(
            build_summary(&bytes),
            Err(BuildError::Source(SourceError::Corrupt))
        ));

        let ossc = snapshot_coverage(1_003_001, 0, 0, 0);
        let vadv = snapshot_coverage(1_004_001, 0, 0, 0);
        let bytes = coverage_pgm(&[ossc, vadv], &[]);
        assert!(matches!(
            build_summary(&bytes),
            Err(BuildError::Source(SourceError::Corrupt))
        ));
    }

    #[test]
    fn source_population_must_equal_collected_and_match_its_revision() {
        let source = PgStatUserIndexesV1::encode(&[index_row()]).expect("encode source row");
        let source_section = vec![(1_014_001, 1, source.clone())];
        let snapshot = SnapshotCoverageV1 {
            collected: 1,
            ..snapshot_coverage(1_014_001, 1, 0, 10)
        };
        let collection = CollectionCoverageV1 {
            collected: 1,
            ..collection_coverage(1_014_001, 10, false, 0)
        };
        let bytes = coverage_pgm_with_sources(source_section, &[snapshot], &[collection]);
        let summary = build_summary(&bytes).expect("matching source population");
        assert_eq!(summary.population_at(5, COVERAGE_TS), Some(1));
        assert_eq!(
            summary
                .collection_state_at(5, COVERAGE_TS)
                .map(|(_, status)| status.collected()),
            Some(1)
        );

        let mismatched = coverage_pgm_with_sources(
            vec![(1_014_001, 1, source.clone())],
            &[snapshot_coverage(1_014_001, 1, 0, 10)],
            &[collection_coverage(1_014_001, 10, false, 0)],
        );
        assert!(matches!(
            build_summary(&mismatched),
            Err(BuildError::Source(SourceError::Corrupt))
        ));

        let wrong_revision = SnapshotCoverageV1 {
            collected: 1,
            ..snapshot_coverage(1_014_002, 1, 0, 10)
        };
        let bytes = coverage_pgm_with_sources(vec![(1_014_001, 1, source)], &[wrong_revision], &[]);
        assert!(matches!(
            build_summary(&bytes),
            Err(BuildError::Source(SourceError::Corrupt))
        ));
    }
}
