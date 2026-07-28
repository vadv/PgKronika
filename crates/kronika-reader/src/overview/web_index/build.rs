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

use kronika_analytics::overview::EventObservation;
use kronika_analytics::web_projection::{
    WebAggregation, WebFormula, WebInput, WebMetric, WebView, web_views,
};
use kronika_format::ReadAt;
use kronika_registry::{Cell, Row, registry};

use super::{
    EntityDictionaryEntry, EntityMetric, EntitySeries, EntitySeriesBlock, IndexStatus,
    METRIC_FLAG_CANONICAL, MetricAggregation, MetricStatus, TimeGrid, UiSummaryBlock, ViewSummary,
    mask_len,
};
use crate::{Dictionary, PgmBodyReadStats, PgmUnit, Resolved};

use super::super::block::BlockError;
use super::super::facts::{BuildError, SourceError};
use super::super::limits::Bounds;

const TOP_K: usize = 64;

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
}

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
    let needed_sections = web_views()
        .iter()
        .filter(|view| view.name != "events")
        .flat_map(|view| view.inputs)
        .flat_map(|input| input.sections)
        .copied()
        .collect::<BTreeSet<_>>();
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
    let dictionary = if decoded.iter().any(|section| !section.rows.is_empty()) {
        unit.dictionary()?
    } else {
        Dictionary::default()
    };
    let grid = TimeGrid::for_range(min_ts, max_ts).map_err(block_build_error)?;
    let (summary, summary_inputs) =
        build_summary(&decoded, &available_sections, observations, grid, bounds)?;
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

fn build_summary(
    decoded: &[DecodedSection],
    available_sections: &BTreeSet<&'static str>,
    observations: &[EventObservation],
    grid: TimeGrid,
    bounds: &Bounds,
) -> Result<(UiSummaryBlock, Vec<SummaryInput>), BuildError> {
    let mut all_times = BTreeSet::new();
    let mut inputs = Vec::with_capacity(web_views().len());
    for view in web_views() {
        let mut populations = BTreeMap::<i64, u64>::new();
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
            }
        }
        let available = input_available(available_sections, &view.inputs[0]);
        let status = if !available {
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
        });
    }
    let snapshot_times = all_times.into_iter().collect::<Vec<_>>();
    if snapshot_times.len() as u64 > bounds.web_summary_timestamps {
        return Err(BuildError::LimitExceeded);
    }
    let mut views = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let mut presence = vec![0_u8; mask_len(snapshot_times.len())];
        let mut populations = Vec::with_capacity(input.populations.len());
        for (index, ts) in snapshot_times.iter().enumerate() {
            if let Some(population) = input.populations.get(ts) {
                presence[index / 8] |= 1 << (index % 8);
                populations.push(*population);
            }
        }
        views.push(
            ViewSummary::new(
                input.view_code,
                input.view_revision,
                input.status,
                presence,
                populations,
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
    let mut candidates = observations
        .iter()
        .map(|observation| {
            let mut buckets = empty_buckets(grid);
            insert_bucket(
                &mut buckets,
                grid,
                observation.time().sort_ts_us,
                observation.occurrence_count() as f64,
                metric.aggregation,
            )?;
            candidate(
                observation.observation_id().0.to_vec(),
                bounded_label(observation.payload().kind_code(), 160),
                buckets,
                metric.aggregation,
            )
        })
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
    use kronika_registry::{Cell, Row, registry};

    use super::{candidate, encode_candidate};
    use crate::overview::limits::LIMIT;

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
}
