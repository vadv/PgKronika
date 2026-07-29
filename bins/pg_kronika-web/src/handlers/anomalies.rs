//! `GET /v1/anomalies` — cross-section anomaly episodes over a period.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use kronika_reader::{
    LocalDirSnapshot, LogicalSection, QueryError, SectionPage, diff_section, gauge_section,
    logical_section, section as query_section,
};
use kronika_registry::{ColumnClass, SectionClass, registry};
use serde_json::{Value, json};

use crate::AppState;
use crate::anomaly::{EpisodeHit, MAX_SCORE_WORK, ScanCounts, ScanParams, rank, scan_section};
use crate::params::{
    QueryParams, parse_duration_us, parse_f64_non_negative, parse_i64, parse_limit_default,
    query_error_response_without_cursor,
};
use crate::plan_anomaly::{
    PLAN_CONTEXT_SECTIONS, PlanContext, PlanScan, PlanSignal, is_plan_section, rank_plan_signals,
    scan_plan_section,
};
use crate::problem::{ApiProblem, LimitResource, QueryConstraint, QueryParameter};
use crate::reason::{ApiReason, MaterializationResource};
use crate::serialize::episode_to_json;

use super::v1::{DIFF_MAX_ROWS, Gates};

/// Default window length: one hour.
const WINDOW_DEFAULT_US: i64 = 3_600 * 1_000_000;
/// Default episode cutoff, in robust sigmas.
const THRESHOLD_DEFAULT: f64 = 3.5;
/// Default relative floor as a fraction of the reference median.
const EPS_REL_DEFAULT: f64 = 0.05;
/// Default cap on returned episodes.
const LIMIT_DEFAULT: usize = 50;
/// Cap on sliding-window positions per series. `DIFF_MAX_ROWS` bounds the
/// row axis; this bounds the other one, so a huge period over a tiny `step`
/// cannot allocate unbounded score profiles.
const MAX_POSITIONS: i64 = 10_000;

const ANOMALY_PARAMS: &[QueryParameter] = &[
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Window,
    QueryParameter::Step,
    QueryParameter::Threshold,
    QueryParameter::EpsRel,
    QueryParameter::Limit,
    QueryParameter::Section,
];

type ErrorResponse = ApiProblem;

struct SectionScan {
    identity: Vec<&'static str>,
    hits: Vec<EpisodeHit>,
    counts: ScanCounts,
    work: usize,
    plan: Option<PlanScan>,
}

/// Names of every section the detector scans: snapshot and event sections
/// with at least one scorable (Cumulative or Gauge) column. Dictionaries are
/// not timelines and charts are derived views of the same raw data.
pub(crate) fn scannable_sections() -> Vec<&'static str> {
    let mut names = std::collections::BTreeSet::new();
    for contract in registry() {
        if contract.deprecated {
            continue;
        }
        if !matches!(
            contract.type_id.section_class(),
            Some(SectionClass::Snapshot | SectionClass::Event)
        ) {
            continue;
        }
        if contract
            .columns
            .iter()
            .any(|column| column.class.eps_abs().is_some())
        {
            names.insert(contract.name);
        }
    }
    names.into_iter().collect()
}

/// Shape one section's honesty counters for the response.
fn counts_to_json(counts: &ScanCounts) -> Value {
    json!({
        "series_total": counts.series_total,
        "evaluated": counts.evaluated,
        "not_evaluated": {
            "ref_too_small": counts.ref_too_small,
            "cur_too_small": counts.cur_too_small,
            "all_no_data": counts.all_no_data,
            "non_finite": counts.non_finite,
            "discontinuity": counts.discontinuity,
        },
        "nodata_points": counts.nodata_points,
        "episodes_truncated": counts.episodes_truncated,
    })
}

const fn response_status(
    complete: bool,
    signals_present: bool,
    series_total: u64,
    generic_evaluated: u64,
    plan_evaluated: u64,
) -> &'static str {
    if !complete {
        "partial"
    } else if signals_present {
        "signals_detected"
    } else if series_total == 0 && plan_evaluated == 0 {
        "no_data"
    } else if generic_evaluated == 0 && plan_evaluated == 0 {
        "insufficient_data"
    } else {
        "calm"
    }
}

fn parse_scan_params(params: &QueryParams) -> Result<(ScanParams, usize), ErrorResponse> {
    let from = parse_i64(params, QueryParameter::From)?;
    let to = parse_i64(params, QueryParameter::To)?;
    if from >= to {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::FromBeforeTo,
        ));
    }
    let window = parse_duration_us(params, QueryParameter::Window, WINDOW_DEFAULT_US)?;
    let step = parse_duration_us(params, QueryParameter::Step, (window / 4).max(1))?;
    let threshold = parse_f64_non_negative(params, QueryParameter::Threshold, THRESHOLD_DEFAULT)?;
    let eps_rel = parse_f64_non_negative(params, QueryParameter::EpsRel, EPS_REL_DEFAULT)?;
    let limit = parse_limit_default(params, LIMIT_DEFAULT)?;
    if from.checked_add(window).is_none_or(|first| first > to) {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::WindowWithinInterval,
        ));
    }
    let positions = to
        .checked_sub(from)
        .and_then(|span| span.checked_sub(window))
        .map(|scannable| scannable / step + 2);
    let Some(positions) = positions else {
        return Err(ApiProblem::invalid_query_constraint(
            QueryConstraint::FiniteScan,
        ));
    };
    if positions > MAX_POSITIONS {
        return Err(ApiProblem::query_limit_exceeded(
            LimitResource::WindowPositions,
            u64::try_from(MAX_POSITIONS).unwrap_or(u64::MAX),
            u64::try_from(positions).ok(),
        ));
    }
    Ok((
        ScanParams {
            from,
            to,
            window,
            step,
            threshold,
            eps_rel,
        },
        limit,
    ))
}

/// `GET /v1/anomalies?from&to` returns ranked anomaly episodes.
///
/// Optional parameters are `window`, `step`, `threshold`, `eps_rel`, `limit`,
/// and `section`. Oversized sections are reported in `skipped`.
pub(crate) async fn anomalies(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let params = match QueryParams::parse(raw.as_deref(), ANOMALY_PARAMS) {
        Ok(params) => params,
        Err(problem) => return problem.into_response(),
    };
    let request = match validate_request(&params) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let Ok(permit) = state.try_acquire_analytic() else {
        return ApiProblem::analytic_capacity_unavailable().into_response();
    };

    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        run(&state, request)
    })
    .await
    {
        Ok(Ok(body)) => body.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(join) => {
            let problem = ApiProblem::internal_error();
            tracing::error!(
                event = "api_analytic_worker_failed",
                request_id = problem.request_id(),
                error = ?join,
                "anomaly worker failed"
            );
            problem.into_response()
        }
    }
}

struct AnomalyRequest {
    scan: ScanParams,
    limit: usize,
    names: Vec<&'static str>,
}

fn validate_request(params: &QueryParams) -> Result<AnomalyRequest, ErrorResponse> {
    let (scan, limit) = parse_scan_params(params)?;
    let names = match params.get(QueryParameter::Section) {
        Some(name) => {
            let logical = logical_section(name).ok_or_else(|| ApiProblem::unknown_section(name))?;
            vec![logical.name]
        }
        None => scannable_sections(),
    };
    Ok(AnomalyRequest { scan, limit, names })
}

#[allow(
    clippy::too_many_lines,
    reason = "the request-global bounded ranking and completeness fold must update episodes and plan signals together"
)]
fn run(state: &AppState, request: AnomalyRequest) -> Result<Json<Value>, ErrorResponse> {
    let AnomalyRequest { scan, limit, names } = request;
    let (from, to) = (scan.from, scan.to);
    let (window, step) = (scan.window, scan.step);

    let mut snap = state.snapshot().as_ref().clone();
    let logicals: Vec<LogicalSection> = names
        .iter()
        .filter_map(|&name| logical_section(name))
        .collect();
    let plan_requested = names.iter().any(|name| is_plan_section(name));
    let supporting = load_supporting_pages(&mut snap, &logicals, plan_requested, from, to)?;
    let gates = Gates::from_pages(&logicals, &supporting);
    let plan_context = plan_requested.then(|| PlanContext::from_pages(&supporting));
    let mut hits: Vec<(&'static str, EpisodeHit)> = Vec::new();
    let mut plan_signals = Vec::<PlanSignal>::new();
    let mut identities: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    let mut sections_out = serde_json::Map::new();
    let mut plan_analysis = serde_json::Map::new();
    let mut skipped = Vec::new();
    let mut remaining_work = MAX_SCORE_WORK;
    let mut sections_scanned = 0_u64;
    let mut series_total = 0_u64;
    let mut evaluated = 0_u64;
    let mut section_episodes_dropped = 0_u64;
    let mut global_episodes_dropped = 0_u64;
    let mut section_plan_signals_dropped = 0_u64;
    let mut global_plan_signals_dropped = 0_u64;
    let mut plan_sections_analyzed = 0_u64;
    let mut plan_positions_evaluated = 0_u64;
    let mut plan_complete = true;

    for &name in &names {
        match scan_one_section(
            &mut snap,
            name,
            &scan,
            remaining_work,
            limit,
            &gates,
            &supporting,
            plan_context.as_ref(),
        )? {
            Ok(mut section) => {
                remaining_work -= section.work;
                hits.extend(section.hits.into_iter().map(|hit| (name, hit)));
                global_episodes_dropped = global_episodes_dropped
                    .saturating_add(u64::try_from(rank(&mut hits, limit)).unwrap_or(u64::MAX));
                identities.insert(name, section.identity);
                sections_scanned = sections_scanned.saturating_add(1);
                series_total = series_total.saturating_add(section.counts.series_total);
                evaluated = evaluated.saturating_add(section.counts.evaluated);
                section_episodes_dropped =
                    section_episodes_dropped.saturating_add(section.counts.episodes_truncated);
                sections_out.insert(name.to_owned(), counts_to_json(&section.counts));
                if let Some(mut plan) = section.plan.take() {
                    plan_sections_analyzed = plan_sections_analyzed.saturating_add(1);
                    plan_complete &= plan.complete();
                    plan_positions_evaluated =
                        plan_positions_evaluated.saturating_add(plan.evaluated());
                    section_plan_signals_dropped =
                        section_plan_signals_dropped.saturating_add(plan.signals_truncated());
                    plan_signals.extend(plan.take_signals());
                    global_plan_signals_dropped = global_plan_signals_dropped.saturating_add(
                        u64::try_from(rank_plan_signals(&mut plan_signals, limit))
                            .unwrap_or(u64::MAX),
                    );
                    plan_analysis.insert(name.to_owned(), plan.to_json());
                }
            }
            Err(reason) => {
                skipped.push(json!({
                    "section": name,
                    "reason": reason,
                }));
            }
        }
    }

    global_episodes_dropped = global_episodes_dropped
        .saturating_add(u64::try_from(rank(&mut hits, limit)).unwrap_or(u64::MAX));
    global_plan_signals_dropped = global_plan_signals_dropped.saturating_add(
        u64::try_from(rank_plan_signals(&mut plan_signals, limit)).unwrap_or(u64::MAX),
    );
    let episodes: Vec<Value> = hits
        .iter()
        .map(|(name, hit)| {
            let empty: &[&'static str] = &[];
            let identity = identities.get(name).map_or(empty, Vec::as_slice);
            episode_to_json(name, identity, hit, &scan)
        })
        .collect();
    let plan_signals: Vec<Value> = plan_signals
        .iter()
        .map(|signal| signal.to_json(&scan))
        .collect();
    let episodes_dropped_total = section_episodes_dropped.saturating_add(global_episodes_dropped);
    let plan_signals_dropped_total =
        section_plan_signals_dropped.saturating_add(global_plan_signals_dropped);
    let complete = skipped.is_empty()
        && episodes_dropped_total == 0
        && plan_signals_dropped_total == 0
        && plan_complete;
    let status = response_status(
        complete,
        !episodes.is_empty() || !plan_signals.is_empty(),
        series_total,
        evaluated,
        plan_positions_evaluated,
    );

    Ok(Json(json!({
        "schema_version": 1,
        "from": from,
        "to": to,
        "window_us": window,
        "step_us": step,
        "threshold": scan.threshold,
        "eps_rel": scan.eps_rel,
        "limit": limit,
        "status": status,
        "complete": complete,
        "coverage": {
            "sections_requested": names.len(),
            "sections_scanned": sections_scanned,
            "sections_skipped": skipped.len(),
            "plan_sections_analyzed": plan_sections_analyzed,
            "plan_positions_evaluated": plan_positions_evaluated,
        },
        "truncation": {
            "section_episodes_dropped": section_episodes_dropped,
            "global_episodes_dropped": global_episodes_dropped,
            "episodes_dropped_total": episodes_dropped_total,
            "section_plan_signals_dropped": section_plan_signals_dropped,
            "global_plan_signals_dropped": global_plan_signals_dropped,
            "plan_signals_dropped_total": plan_signals_dropped_total,
        },
        "episodes": episodes,
        "plan_signals": plan_signals,
        "sections": Value::Object(sections_out),
        "plan_analysis": Value::Object(plan_analysis),
        "skipped": skipped,
    })))
}

fn load_supporting_pages(
    snap: &mut LocalDirSnapshot,
    logicals: &[LogicalSection],
    include_plan_context: bool,
    from: i64,
    to: i64,
) -> Result<BTreeMap<String, SectionPage>, ErrorResponse> {
    let mut pages = BTreeMap::new();
    let mut names: std::collections::BTreeSet<_> = Gates::sections(logicals).into_iter().collect();
    if include_plan_context {
        names.extend(PLAN_CONTEXT_SECTIONS);
    }
    for name in names {
        let page = query_section(snap, name, from, to, DIFF_MAX_ROWS, None)
            .map_err(|err| query_error_response_without_cursor(&err))?;
        pages.insert(name.to_owned(), page);
    }
    Ok(pages)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one section scan shares the request snapshot, budgets, decoded support pages, and provenance"
)]
fn scan_one_section(
    snap: &mut LocalDirSnapshot,
    name: &'static str,
    scan: &ScanParams,
    remaining_work: usize,
    hit_limit: usize,
    gates: &Gates,
    supporting: &BTreeMap<String, SectionPage>,
    plan_context: Option<&PlanContext>,
) -> Result<Result<SectionScan, ApiReason>, ErrorResponse> {
    let owned_page;
    let page = if let Some(page) = supporting.get(name) {
        page
    } else {
        owned_page = match query_section(snap, name, scan.from, scan.to, DIFF_MAX_ROWS, None) {
            Ok(page) => page,
            Err(QueryError::ResultTooLarge { max_cells }) => {
                return Ok(Err(ApiReason::materialization_limit(
                    MaterializationResource::Cells,
                    max_cells,
                )));
            }
            Err(QueryError::MaterializedBytesTooLarge { max_bytes }) => {
                return Ok(Err(ApiReason::materialization_limit(
                    MaterializationResource::Bytes,
                    max_bytes,
                )));
            }
            Err(err) => return Err(query_error_response_without_cursor(&err)),
        };
        &owned_page
    };
    if page.next_cursor.is_some() {
        return Ok(Err(ApiReason::incomplete_page()));
    }
    let logical = logical_section(name).ok_or_else(|| {
        query_error_response_without_cursor(&QueryError::UnknownSection(name.to_owned()))
    })?;
    let identity = logical.diff_key();
    let (cumulative, gauges) = scorable_columns(&logical);
    let mut diffs = diff_section(&identity, &cumulative, &page.rows, &page.gaps);
    gates.apply(&logical, &mut diffs);
    let gauge_series = gauge_section(&identity, &gauges, &page.rows);
    let (hits, counts, anomaly_work) =
        match scan_section(&diffs, &gauge_series, scan, remaining_work, hit_limit) {
            Ok(scanned) => scanned,
            Err(limit) => {
                return Ok(Err(ApiReason::scoring_work_budget(
                    limit.required,
                    limit.available,
                )));
            }
        };
    let plan = plan_context.and_then(|context| {
        scan_plan_section(
            name,
            page,
            &diffs,
            context,
            scan,
            remaining_work.saturating_sub(anomaly_work),
            hit_limit,
        )
    });
    let work = anomaly_work.saturating_add(plan.as_ref().map_or(0, PlanScan::work));
    Ok(Ok(SectionScan {
        identity,
        hits,
        counts,
        work,
        plan,
    }))
}

/// Split a logical section's columns into cumulative and gauge name lists.
fn scorable_columns(logical: &LogicalSection) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut cumulative = Vec::new();
    let mut gauges = Vec::new();
    for column in &logical.columns {
        match column.class {
            ColumnClass::Cumulative => cumulative.push(column.name),
            ColumnClass::Gauge => gauges.push(column.name),
            ColumnClass::Label | ColumnClass::Timestamp => {}
        }
    }
    (cumulative, gauges)
}

#[cfg(test)]
mod tests {
    use super::response_status;

    #[test]
    fn specialized_results_participate_in_top_level_status() {
        assert_eq!(response_status(true, true, 1, 0, 1), "signals_detected");
        assert_eq!(response_status(true, false, 1, 0, 1), "calm");
        assert_eq!(response_status(true, false, 1, 0, 0), "insufficient_data");
        assert_eq!(response_status(true, false, 0, 0, 0), "no_data");
        assert_eq!(response_status(false, true, 1, 1, 1), "partial");
    }
}
