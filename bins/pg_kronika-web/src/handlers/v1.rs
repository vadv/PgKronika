use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, RawQuery, State};
use kronika_reader::{
    GateReading, LogicalSection, QueryError, SectionPage, SeriesDiff, apply_collection_gating,
    diff_section, gate_readings, logical_section, section, sections as query_sections,
};
use kronika_registry::{
    ColumnClass, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, registry, section_name,
};
use serde_json::{Value, json};

use crate::AppState;
use crate::params::{
    QueryParams, parse_cursor, parse_i64, parse_limit, parse_u64, query_error_response,
    query_error_response_without_cursor,
};
use crate::problem::{ApiProblem, ExpectedValue, LimitResource, QueryParameter, count_u64};
use crate::serialize::{
    column_class_name, column_type_name, page_to_json, semantics_name, series_diff_to_json,
};

/// Cap on rows read for one diff response; a wider window is rejected so a
/// single request cannot pull an unbounded section into memory.
pub(crate) const DIFF_MAX_ROWS: usize = 262_144;

const RANGE_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
];
const PAGE_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Limit,
    QueryParameter::Cursor,
];
const BATCH_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Names,
    QueryParameter::Limit,
];
const BATCH_DIFF_PARAMS: &[QueryParameter] = &[
    QueryParameter::Source,
    QueryParameter::From,
    QueryParameter::To,
    QueryParameter::Names,
];

/// `GET /v1/version` — the API and container format versions this build serves.
///
/// Static body, `application/json`.
pub(crate) async fn version(RawQuery(raw): RawQuery) -> Result<Json<Value>, ApiProblem> {
    QueryParams::parse(raw.as_deref(), &[])?;
    Ok(Json(
        json!({ "api": "v1", "format_version": crate::FORMAT_VERSION }),
    ))
}

/// `GET /v1/sources` — every source in the store and its overall time span.
pub(crate) async fn sources(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiProblem> {
    QueryParams::parse(raw.as_deref(), &[])?;
    let snapshot = state.snapshot();
    let mut spans: BTreeMap<u64, (i64, i64, usize)> = BTreeMap::new();
    for unit in snapshot.units() {
        let span = spans
            .entry(unit.source_id)
            .or_insert((unit.min_ts, unit.max_ts, 0));
        span.0 = span.0.min(unit.min_ts);
        span.1 = span.1.max(unit.max_ts);
        span.2 += 1;
    }
    let sources: Vec<Value> = spans
        .into_iter()
        .map(|(source_id, (min_ts, max_ts, segments))| {
            json!({ "source_id": source_id, "min_ts": min_ts, "max_ts": max_ts, "segments": segments })
        })
        .collect();
    Ok(Json(json!({ "sources": sources })))
}

/// `GET /v1/sections` — static catalog of section types from the registry.
///
/// One entry per logical name: its semantics, sort key, and the union of its
/// versions' columns (first appearance across ascending `type_id`).
pub(crate) async fn sections(RawQuery(raw): RawQuery) -> Result<Json<Value>, ApiProblem> {
    QueryParams::parse(raw.as_deref(), &[])?;
    let mut by_name: BTreeMap<&'static str, Vec<&'static kronika_registry::TypeContract>> =
        BTreeMap::new();
    for contract in registry() {
        by_name.entry(contract.name).or_default().push(contract);
    }
    let sections: Vec<Value> = by_name
        .into_iter()
        .map(|(name, mut contracts)| {
            contracts.sort_by_key(|contract| contract.type_id.get());
            let mut seen = std::collections::HashSet::new();
            let mut columns = Vec::new();
            for contract in &contracts {
                for column in contract.columns {
                    if seen.insert(column.name) {
                        columns.push(json!({
                            "name": column.name,
                            "type": column_type_name(column.ty),
                            "class": column_class_name(column.class),
                        }));
                    }
                }
            }
            json!({
                "name": name,
                "semantics": semantics_name(contracts[0].semantics),
                "sort_key": contracts[0].sort_key,
                "columns": columns,
            })
        })
        .collect();
    Ok(Json(json!({ "sections": sections })))
}

/// `GET /v1/segments?source&from&to` — segments of `source` overlapping the
/// window, catalog metadata only (no section bodies decoded).
pub(crate) async fn segments(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiProblem> {
    let params = QueryParams::parse(raw.as_deref(), RANGE_PARAMS)?;
    let source = parse_u64(&params, QueryParameter::Source)?;
    let from = parse_i64(&params, QueryParameter::From)?;
    let to = parse_i64(&params, QueryParameter::To)?;

    let snapshot = state.snapshot();
    let units = snapshot.units();
    let mut out = Vec::new();
    for (idx, unit) in units.iter().enumerate() {
        if unit.source_id != source || unit.max_ts < from || unit.min_ts > to {
            continue;
        }
        let Some(catalog) = snapshot.unit_catalog(idx) else {
            continue;
        };
        let mut rows_by_name: BTreeMap<&'static str, u64> = BTreeMap::new();
        for entry in &catalog.entries {
            if matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
                continue;
            }
            let Some(name) = section_name(entry.type_id) else {
                continue;
            };
            *rows_by_name.entry(name).or_insert(0) += u64::from(entry.rows);
        }
        let sections: Vec<Value> = rows_by_name
            .into_iter()
            .map(|(name, rows)| json!({ "name": name, "rows": rows }))
            .collect();
        out.push(json!({
            "segment_id": unit.min_ts.to_string(),
            "source_id": unit.source_id,
            "min_ts": unit.min_ts,
            "max_ts": unit.max_ts,
            "sections": sections,
        }));
    }
    Ok(Json(json!({ "segments": out })))
}

/// `GET /v1/section/{name}?source&from&to&limit` — one section's rows over the
/// window, decoded and serialized to JSON.
///
/// The reader does the query (ts filter, sort, union columns, gaps); this
/// handler parses params and shapes the result. A stale snapshot degrades to
/// gaps inside the reader, so it stays a `200`.
pub(crate) async fn section_data(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiProblem> {
    let Path(name) = path.map_err(|_rejection| ApiProblem::unknown_section("invalid"))?;
    let params = QueryParams::parse(raw.as_deref(), PAGE_PARAMS)?;
    let source = parse_u64(&params, QueryParameter::Source)?;
    let from = parse_i64(&params, QueryParameter::From)?;
    let to = parse_i64(&params, QueryParameter::To)?;
    let limit = parse_limit(&params)?;
    let cursor = parse_cursor(&params)?;

    // section() takes `&mut`; clone the shared snapshot (catalog metadata, not
    // section bodies) and query the private copy.
    let mut snap = state.snapshot().as_ref().clone();
    match section(&mut snap, &name, source, from, to, limit, cursor) {
        Ok(page) => Ok(Json(page_to_json(&page))),
        Err(err) => Err(query_error_response(&err)),
    }
}

/// `GET /v1/sections/batch?source&from&to&names=a,b,c&limit` — several sections
/// for one source over one window, each as its own page keyed by name.
///
/// One decode of each overlapping segment serves every requested section, so a
/// multi-metric view costs one pass, not one per section. An unknown name fails
/// the whole request.
pub(crate) async fn sections_batch(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiProblem> {
    let params = QueryParams::parse(raw.as_deref(), BATCH_PARAMS)?;
    let source = parse_u64(&params, QueryParameter::Source)?;
    let from = parse_i64(&params, QueryParameter::From)?;
    let to = parse_i64(&params, QueryParameter::To)?;
    let limit = parse_limit(&params)?;
    let raw = params
        .get(QueryParameter::Names)
        .ok_or_else(|| ApiProblem::missing_query_parameter(QueryParameter::Names))?;
    let names: Vec<&str> = raw.split(',').filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return Err(ApiProblem::invalid_query_parameter(
            QueryParameter::Names,
            ExpectedValue::SectionList,
        ));
    }

    let mut snap = state.snapshot().as_ref().clone();
    let cursors = BTreeMap::new();
    match query_sections(&mut snap, source, from, to, &names, limit, &cursors) {
        Ok(pages) => {
            let object = pages
                .iter()
                .map(|(name, page)| (name.clone(), page_to_json(page)))
                .collect();
            Ok(Json(Value::Object(object)))
        }
        Err(err) => Err(query_error_response_without_cursor(&err)),
    }
}

/// The error half of a handler result: one closed Problem Details response.
type ErrorResponse = ApiProblem;

/// One section's diff as a JSON object (`section`, `identity`, `series`).
type DiffObject = serde_json::Map<String, Value>;

/// Gate timelines for the request's gated columns, keyed by the gate's
/// section and column names.
///
/// Missing or truncated gate pages yield an unknown timeline.
pub(crate) struct Gates {
    readings: BTreeMap<(&'static str, &'static str), Vec<GateReading>>,
}

impl Gates {
    /// Names of the sections the logical sections' gates live in.
    pub(crate) fn sections(logicals: &[LogicalSection]) -> Vec<&'static str> {
        let mut names = std::collections::BTreeSet::new();
        for logical in logicals {
            for column in &logical.columns {
                if let Some(gate) = column.gated_by {
                    for reference in gate.references() {
                        names.insert(reference.section);
                    }
                }
            }
        }
        names.into_iter().collect()
    }

    /// Build the timelines the logical sections need from fetched pages.
    pub(crate) fn from_pages(
        logicals: &[LogicalSection],
        pages: &BTreeMap<String, SectionPage>,
    ) -> Self {
        let mut readings = BTreeMap::new();
        for logical in logicals {
            for column in &logical.columns {
                let Some(gate) = column.gated_by else {
                    continue;
                };
                for reference in gate.references() {
                    readings
                        .entry((reference.section, reference.column))
                        .or_insert_with(|| {
                            pages
                                .get(reference.section)
                                .filter(|page| page.next_cursor.is_none())
                                .map(|page| {
                                    let mut values = gate_readings(&page.rows, reference.column);
                                    values.extend(page.gaps.iter().map(|gap| (gap.from, None)));
                                    values.sort_by_key(|reading| reading.0);
                                    values
                                })
                                .unwrap_or_default()
                        });
                }
            }
        }
        Self { readings }
    }

    /// Rewrite the gated columns of one section's folded series.
    pub(crate) fn apply(&self, logical: &LogicalSection, series: &mut [SeriesDiff]) {
        let identity = logical.diff_key();
        for column in &logical.columns {
            let Some(gate) = column.gated_by else {
                continue;
            };
            apply_collection_gating(series, column.name, &identity, gate, |selected| {
                self.readings
                    .get(&(selected.section, selected.column))
                    .map_or(&[][..], Vec::as_slice)
            });
        }
    }
}

/// Fold one section's page into its diff JSON.
///
/// The series key is the declared identity, or the sort key minus `ts` when a
/// section is unmarked, so multi-row sections do not collapse into one series. A
/// window whose rows exceed one page is rejected so the response stays bounded.
fn section_diff_object(
    logical: &LogicalSection,
    page: &SectionPage,
    gates: &Gates,
) -> Result<DiffObject, ErrorResponse> {
    if page.next_cursor.is_some() {
        return Err(ApiProblem::query_limit_exceeded(
            LimitResource::Rows,
            count_u64(DIFF_MAX_ROWS),
            None,
        ));
    }
    let identity = logical.diff_key();
    let cumulative: Vec<&str> = logical
        .columns
        .iter()
        .filter(|column| column.class == ColumnClass::Cumulative)
        .map(|column| column.name)
        .collect();
    let mut series = diff_section(&identity, &cumulative, &page.rows, &page.gaps);
    gates.apply(logical, &mut series);
    let mut object = serde_json::Map::new();
    object.insert("section".to_owned(), json!(logical.name));
    object.insert("identity".to_owned(), json!(identity));
    object.insert("series".to_owned(), series_diff_to_json(&identity, &series));
    Ok(object)
}

/// `GET /v1/section/{name}/diff?source&from&to` — per-entity deltas and rates
/// over a window.
///
/// Resolves the section's identity and cumulative columns from the registry,
/// reads the window in one page, and folds each series through the diff core.
pub(crate) async fn section_diff(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ErrorResponse> {
    let Path(name) = path.map_err(|_rejection| ApiProblem::unknown_section("invalid"))?;
    let params = QueryParams::parse(raw.as_deref(), RANGE_PARAMS)?;
    let source = parse_u64(&params, QueryParameter::Source)?;
    let from = parse_i64(&params, QueryParameter::From)?;
    let to = parse_i64(&params, QueryParameter::To)?;

    let logical = logical_section(&name).ok_or_else(|| {
        query_error_response_without_cursor(&QueryError::UnknownSection(name.clone()))
    })?;
    let mut snap = state.snapshot().as_ref().clone();
    let page = section(&mut snap, &name, source, from, to, DIFF_MAX_ROWS, None)
        .map_err(|err| query_error_response_without_cursor(&err))?;
    let mut gate_pages = BTreeMap::new();
    for gate_section in Gates::sections(std::slice::from_ref(&logical)) {
        let gate_page = section(
            &mut snap,
            gate_section,
            source,
            from,
            to,
            DIFF_MAX_ROWS,
            None,
        )
        .map_err(|err| query_error_response_without_cursor(&err))?;
        gate_pages.insert(gate_section.to_owned(), gate_page);
    }
    let gates = Gates::from_pages(std::slice::from_ref(&logical), &gate_pages);

    let mut object = section_diff_object(&logical, &page, &gates)?;
    object.insert("source_id".to_owned(), json!(source));
    Ok(Json(Value::Object(object)))
}

/// `GET /v1/sections/batch/diff?source&from&to&names=a,b,c` — diffs for several
/// sections over one window, each keyed by name.
///
/// One decode of each overlapping segment serves every requested section, so a
/// multi-metric diff costs one segment pass, not one per section. An unknown name
/// fails the whole request.
pub(crate) async fn sections_batch_diff(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ErrorResponse> {
    let params = QueryParams::parse(raw.as_deref(), BATCH_DIFF_PARAMS)?;
    let source = parse_u64(&params, QueryParameter::Source)?;
    let from = parse_i64(&params, QueryParameter::From)?;
    let to = parse_i64(&params, QueryParameter::To)?;
    let raw = params
        .get(QueryParameter::Names)
        .ok_or_else(|| ApiProblem::missing_query_parameter(QueryParameter::Names))?;
    let names: Vec<&str> = raw.split(',').filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return Err(ApiProblem::invalid_query_parameter(
            QueryParameter::Names,
            ExpectedValue::SectionList,
        ));
    }
    let logicals: Vec<LogicalSection> = names
        .iter()
        .map(|&name| {
            logical_section(name).ok_or_else(|| {
                query_error_response_without_cursor(&QueryError::UnknownSection(name.to_owned()))
            })
        })
        .collect::<Result<_, _>>()?;

    let mut snap = state.snapshot().as_ref().clone();
    let cursors = BTreeMap::new();
    // Gate sections ride the same gather; requested names stay first so the
    // response loop below only walks them.
    let mut fetch_names = names.clone();
    for gate_section in Gates::sections(&logicals) {
        if !fetch_names.contains(&gate_section) {
            fetch_names.push(gate_section);
        }
    }
    let pages = query_sections(
        &mut snap,
        source,
        from,
        to,
        &fetch_names,
        DIFF_MAX_ROWS,
        &cursors,
    )
    .map_err(|err| query_error_response_without_cursor(&err))?;
    let gates = Gates::from_pages(&logicals, &pages);

    let mut out = serde_json::Map::new();
    for (logical, &name) in logicals.iter().zip(&names) {
        let page = pages.get(name).ok_or_else(|| {
            query_error_response_without_cursor(&QueryError::UnknownSection(name.to_owned()))
        })?;
        out.insert(
            name.to_owned(),
            Value::Object(section_diff_object(logical, page, &gates)?),
        );
    }
    Ok(Json(Value::Object(out)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kronika_reader::{ColumnDiff, DiffAt, DiffPoint, Reason, Scalar, SeriesDiff, Value};

    use super::Gates;

    const SEC: i64 = 1_000_000;

    fn series(object: &str, column: &str) -> SeriesDiff {
        SeriesDiff {
            key: vec![
                Value::Str("client backend".to_owned()),
                Value::Str(object.to_owned()),
                Value::Str("normal".to_owned()),
            ],
            columns: vec![ColumnDiff {
                name: column.to_owned(),
                points: vec![DiffAt {
                    ts: SEC,
                    point: DiffPoint::Value {
                        delta: Scalar::Float(1.0),
                        rate: 1.0,
                        dt_micros: SEC,
                    },
                }],
            }],
        }
    }

    fn not_collected(series: &SeriesDiff) -> bool {
        matches!(
            series.columns[0].points[0].point,
            DiffPoint::NoData {
                reason: Reason::NotCollected
            }
        )
    }

    fn gates(io: bool, wal: bool) -> Gates {
        Gates {
            readings: BTreeMap::from([
                (("reset_metadata", "track_io_timing"), vec![(0, Some(io))]),
                (
                    ("reset_metadata", "track_wal_io_timing"),
                    vec![(0, Some(wal))],
                ),
            ]),
        }
    }

    #[test]
    fn pg18_wal_and_relation_rows_use_different_timing_gates() {
        let logical = kronika_reader::logical_section("pg_stat_io").expect("registered section");

        let mut io_on = vec![series("relation", "read_time"), series("wal", "read_time")];
        gates(true, false).apply(&logical, &mut io_on);
        assert!(!not_collected(&io_on[0]));
        assert!(not_collected(&io_on[1]));

        let mut wal_on = vec![series("relation", "read_time"), series("wal", "read_time")];
        gates(false, true).apply(&logical, &mut wal_on);
        assert!(not_collected(&wal_on[0]));
        assert!(!not_collected(&wal_on[1]));
    }

    #[test]
    fn pg18_wal_writeback_time_still_uses_track_io_timing() {
        let logical = kronika_reader::logical_section("pg_stat_io").expect("registered section");
        let mut wal = vec![series("wal", "writeback_time")];
        gates(false, true).apply(&logical, &mut wal);
        assert!(not_collected(&wal[0]));
    }

    #[test]
    fn unresolved_row_selector_is_not_collected() {
        let logical = kronika_reader::logical_section("pg_stat_io").expect("registered section");
        let mut unknown = vec![series("wal", "read_time")];
        unknown[0].key[1] = Value::Null;
        gates(true, true).apply(&logical, &mut unknown);
        assert!(not_collected(&unknown[0]));
    }
}
