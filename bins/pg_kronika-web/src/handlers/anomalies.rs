//! `GET /v1/anomalies` — cross-section anomaly episodes over a period.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use kronika_reader::{
    LocalDirSnapshot, LogicalSection, QueryError, diff_section, gauge_section, logical_section,
    section as query_section,
};
use kronika_registry::{ColumnClass, SectionClass, registry};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::anomaly::{EpisodeHit, MAX_SCORE_WORK, ScanCounts, ScanParams, rank, scan_section};
use crate::params::{
    bad_request, parse_duration_us, parse_f64_non_negative, parse_i64, parse_limit_default,
    parse_u64, query_error_response,
};
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

static ANOMALY_REQUESTS: Semaphore = Semaphore::const_new(1);

/// The error half of a handler result: an HTTP status and a JSON body.
type ErrorResponse = (StatusCode, Json<Value>);

struct SectionScan {
    identity: Vec<&'static str>,
    hits: Vec<EpisodeHit>,
    counts: ScanCounts,
    work: usize,
}

/// Names of every section the detector scans: snapshot and event sections
/// with at least one scorable (Cumulative or Gauge) column. Dictionaries are
/// not timelines and charts are derived views of the same raw data.
fn scannable_sections() -> Vec<&'static str> {
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
        },
        "nodata_points": counts.nodata_points,
    })
}

/// `GET /v1/anomalies?source&from&to` — every anomaly episode of the period,
/// across all sections of the source, ranked by peak score.
///
/// Optional knobs and their defaults: `window=1h` (sliding window length),
/// `step` (position stride, `window/4`), `threshold=3.5` (episode cutoff in
/// robust sigmas), `eps_rel=0.05` (relative scale floor), `limit=50` (episode
/// cap after ranking), `section=<name>` (restrict the scan to one section).
///
/// Cumulative columns are scored over derivative rates, gauge columns over
/// raw readings; MAD units make peak `|m|` comparable across sections, so
/// one ranked list serves the whole source. A section whose period exceeds
/// the row cap lands in `skipped` and the rest of the scan proceeds.
/// Parse and validate every scan knob of the request.
fn parse_scan_params(
    params: &std::collections::HashMap<String, String>,
) -> Result<(ScanParams, usize), ErrorResponse> {
    let from = parse_i64(params, "from")?;
    let to = parse_i64(params, "to")?;
    if from >= to {
        return Err(bad_request("`from` must be before `to`"));
    }
    let window = parse_duration_us(params, "window", WINDOW_DEFAULT_US)?;
    let step = parse_duration_us(params, "step", (window / 4).max(1))?;
    let threshold = parse_f64_non_negative(params, "threshold", THRESHOLD_DEFAULT)?;
    let eps_rel = parse_f64_non_negative(params, "eps_rel", EPS_REL_DEFAULT)?;
    let limit = parse_limit_default(params, LIMIT_DEFAULT)?;
    if from.checked_add(window).is_none_or(|first| first > to) {
        return Err(bad_request("`window` must fit inside [from, to]"));
    }
    let positions = to
        .checked_sub(from)
        .and_then(|span| span.checked_sub(window))
        .map(|scannable| scannable / step + 2);
    if positions.is_none_or(|count| count > MAX_POSITIONS) {
        return Err(bad_request(
            "the period and `step` produce too many window positions; \
             widen `step` or narrow [from, to]",
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

pub(crate) async fn anomalies(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ErrorResponse> {
    let _permit = ANOMALY_REQUESTS.acquire().await.map_err(|_closed| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "anomaly_scanner_unavailable",
                "detail": "the anomaly scanner is unavailable",
            })),
        )
    })?;
    let source = parse_u64(&params, "source")?;
    let (scan, limit) = parse_scan_params(&params)?;
    let (from, to) = (scan.from, scan.to);
    let (window, step) = (scan.window, scan.step);

    let names: Vec<&'static str> = match params.get("section") {
        Some(name) => {
            let logical = logical_section(name)
                .ok_or_else(|| query_error_response(&QueryError::UnknownSection(name.clone())))?;
            vec![logical.name]
        }
        None => scannable_sections(),
    };

    let mut snap = state.snapshot.load().as_ref().clone();
    let logicals: Vec<LogicalSection> = names.iter().filter_map(|&name| logical_section(name)).collect();
    let gates = load_gates(&mut snap, &logicals, source, from, to)?;
    let mut hits: Vec<(&'static str, EpisodeHit)> = Vec::new();
    let mut identities: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    let mut sections_out = serde_json::Map::new();
    let mut skipped = Vec::new();
    let mut remaining_work = MAX_SCORE_WORK;

    for &name in &names {
        match scan_one_section(
            &mut snap,
            name,
            source,
            &scan,
            remaining_work,
            limit,
            &gates,
        )? {
            Ok(section) => {
                remaining_work -= section.work;
                hits.extend(section.hits.into_iter().map(|hit| (name, hit)));
                rank(&mut hits, limit);
                identities.insert(name, section.identity);
                sections_out.insert(name.to_owned(), counts_to_json(&section.counts));
            }
            Err(reason) => {
                skipped.push(json!({
                    "section": name,
                    "reason": reason,
                }));
            }
        }
    }

    rank(&mut hits, limit);
    let episodes: Vec<Value> = hits
        .iter()
        .map(|(name, hit)| {
            let empty: &[&'static str] = &[];
            let identity = identities.get(name).map_or(empty, Vec::as_slice);
            episode_to_json(name, identity, hit)
        })
        .collect();

    Ok(Json(json!({
        "source_id": source,
        "from": from,
        "to": to,
        "window_us": window,
        "step_us": step,
        "threshold": scan.threshold,
        "eps_rel": scan.eps_rel,
        "limit": limit,
        "episodes": episodes,
        "sections": Value::Object(sections_out),
        "skipped": skipped,
    })))
}

fn load_gates(
    snap: &mut LocalDirSnapshot,
    logicals: &[LogicalSection],
    source: u64,
    from: i64,
    to: i64,
) -> Result<Gates, ErrorResponse> {
    let mut pages = BTreeMap::new();
    for name in Gates::sections(logicals) {
        let page = query_section(snap, name, source, from, to, DIFF_MAX_ROWS, None)
            .map_err(|err| query_error_response(&err))?;
        pages.insert(name.to_owned(), page);
    }
    Ok(Gates::from_pages(logicals, &pages))
}

fn scan_one_section(
    snap: &mut LocalDirSnapshot,
    name: &'static str,
    source: u64,
    scan: &ScanParams,
    remaining_work: usize,
    hit_limit: usize,
    gates: &Gates,
) -> Result<Result<SectionScan, String>, ErrorResponse> {
    let page = match query_section(snap, name, source, scan.from, scan.to, DIFF_MAX_ROWS, None) {
        Ok(page) => page,
        Err(QueryError::ResultTooLarge { max_cells }) => {
            return Ok(Err(format!(
                "the period exceeds the {max_cells}-cell materialization limit; narrow it"
            )));
        }
        Err(err) => return Err(query_error_response(&err)),
    };
    if page.next_cursor.is_some() {
        return Ok(Err(
            "the period has too many rows to scan in one pass; narrow it".to_owned(),
        ));
    }
    let logical = logical_section(name)
        .ok_or_else(|| query_error_response(&QueryError::UnknownSection(name.to_owned())))?;
    let identity = logical.diff_key();
    let (cumulative, gauges) = scorable_columns(&logical);
    let mut diffs = diff_section(&identity, &cumulative, &page.rows, &page.gaps);
    gates.apply(&logical, &mut diffs);
    let gauge_series = gauge_section(&identity, &gauges, &page.rows);
    let (hits, counts, work) =
        match scan_section(&diffs, &gauge_series, scan, remaining_work, hit_limit) {
            Ok(scanned) => scanned,
            Err(limit) => {
                return Ok(Err(format!(
                    "scoring requires {} point-position pairs; {} remain in the request budget",
                    limit.required, limit.available
                )));
            }
        };
    Ok(Ok(SectionScan {
        identity,
        hits,
        counts,
        work,
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
