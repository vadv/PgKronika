use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use kronika_reader::{section, sections as query_sections};
use kronika_registry::{DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, registry, section_name};
use serde_json::{Value, json};

use crate::AppState;
use crate::params::{
    bad_request, parse_cursor, parse_i64, parse_limit, parse_u64, query_error_response,
};
use crate::serialize::{column_class_name, column_type_name, page_to_json, semantics_name};

/// `GET /v1/version` — the API and container format versions this build serves.
///
/// Static body, `application/json`.
pub(crate) async fn version() -> Json<Value> {
    Json(json!({ "api": "v1", "format_version": crate::FORMAT_VERSION }))
}

/// `GET /v1/sources` — every source in the store and its overall time span.
pub(crate) async fn sources(State(state): State<AppState>) -> Json<Value> {
    let snapshot = state.snapshot.load_full();
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
    Json(json!({ "sources": sources }))
}

/// `GET /v1/sections` — static catalog of section types from the registry.
///
/// One entry per logical name: its semantics, sort key, and the union of its
/// versions' columns (first appearance across ascending `type_id`).
pub(crate) async fn sections() -> Json<Value> {
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
    Json(json!({ "sections": sections }))
}

/// `GET /v1/segments?source&from&to` — segments of `source` overlapping the
/// window, catalog metadata only (no section bodies decoded).
pub(crate) async fn segments(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let source = parse_u64(&params, "source")?;
    let from = parse_i64(&params, "from")?;
    let to = parse_i64(&params, "to")?;

    let snapshot = state.snapshot.load_full();
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
    Path(name): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let source = parse_u64(&params, "source")?;
    let from = parse_i64(&params, "from")?;
    let to = parse_i64(&params, "to")?;
    let limit = parse_limit(&params)?;
    let cursor = parse_cursor(&params)?;

    // section() takes `&mut`; clone the shared snapshot (catalog metadata, not
    // section bodies) and query the private copy.
    let mut snap = state.snapshot.load().as_ref().clone();
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
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let source = parse_u64(&params, "source")?;
    let from = parse_i64(&params, "from")?;
    let to = parse_i64(&params, "to")?;
    let limit = parse_limit(&params)?;
    let raw = params
        .get("names")
        .ok_or_else(|| bad_request("missing query parameter `names`"))?;
    let names: Vec<&str> = raw.split(',').filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return Err(bad_request("`names` must list at least one section"));
    }

    let mut snap = state.snapshot.load().as_ref().clone();
    let cursors = BTreeMap::new();
    match query_sections(&mut snap, source, from, to, &names, limit, &cursors) {
        Ok(pages) => {
            let object = pages
                .iter()
                .map(|(name, page)| (name.clone(), page_to_json(page)))
                .collect();
            Ok(Json(Value::Object(object)))
        }
        Err(err) => Err(query_error_response(&err)),
    }
}
