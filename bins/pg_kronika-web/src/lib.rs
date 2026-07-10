//! Serves data to humans and agents: web UI, MCP server, JSON API.
//!
//! This binary hosts an axum router over a near-real-time view of a local
//! store directory. The router is built by [`app`] from an [`AppState`], which
//! holds the shared snapshot behind an [`ArcSwap`]. Request handlers clone the
//! current snapshot (catalog metadata only, not section bodies) and call the
//! reader's `&mut` query functions on their private copy. In production a
//! background task refreshes the shared snapshot once a second; tests build the
//! state directly and never start that task, so the router stays deterministic.

use std::collections::BTreeMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use kronika_reader::{
    Cursor, LocalDirSnapshot, OutRow, QueryError, SectionPage, Value as CellValue, section,
    sections as query_sections,
};
use kronika_registry::{
    ColumnClass, ColumnType, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, Semantics, TypeContract,
    registry, section_name,
};
use serde_json::{Value, json};
// The binary target and the `#[tokio::test]` harness need the async runtime; the
// library's handlers are runtime-agnostic and never name it.
use tokio as _;

/// Container format version this build serves, mirrored into `/v1/version`.
const FORMAT_VERSION: u32 = 1;

/// Rows returned when a request omits `limit`.
const DEFAULT_LIMIT: usize = 1_000;

/// Hard ceiling on `limit`, applied even when a request asks for more.
const MAX_LIMIT: usize = 10_000;

/// Shared router state.
///
/// The snapshot is swapped atomically by the background refresh task; handlers
/// load the current pointer and clone it for their own `&mut` queries.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The current store snapshot, replaced wholesale on each refresh.
    pub snapshot: Arc<ArcSwap<LocalDirSnapshot>>,
}

impl AppState {
    /// Wrap an already-open snapshot in swappable shared state.
    #[must_use]
    pub fn new(snapshot: LocalDirSnapshot) -> Self {
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
        }
    }
}

/// Build the request router over `state`.
///
/// Pure: no sockets, no background tasks. Tests call this directly and drive it
/// with `tower::ServiceExt::oneshot`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/version", get(version))
        .route("/v1/sources", get(sources))
        .route("/v1/sections", get(sections))
        .route("/v1/segments", get(segments))
        .route("/v1/section/{name}", get(section_data))
        .route("/v1/sections/batch", get(sections_batch))
        .with_state(state)
}

/// `GET /v1/version` — the API and container format versions this build serves.
///
/// The body is static: `{"api":"v1","format_version":1}` with an
/// `application/json` content type.
async fn version() -> Json<Value> {
    Json(json!({ "api": "v1", "format_version": FORMAT_VERSION }))
}

/// `GET /v1/sources` — every source in the store and its overall time span.
async fn sources(State(state): State<AppState>) -> Json<Value> {
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
async fn sections() -> Json<Value> {
    let mut by_name: BTreeMap<&'static str, Vec<&'static TypeContract>> = BTreeMap::new();
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
async fn segments(
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
/// Delegates the query (ts filter, sort, union columns, gap accounting) to the
/// reader; this handler only parses parameters and shapes the result. A stale
/// snapshot degrades to gaps inside the reader, so it stays a `200` here.
async fn section_data(
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
async fn sections_batch(
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

/// Parse a required unsigned query parameter, or a `400` with a JSON body.
fn parse_u64(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<u64, (StatusCode, Json<Value>)> {
    params
        .get(key)
        .ok_or_else(|| bad_request(&format!("missing query parameter `{key}`")))?
        .parse()
        .map_err(|_err| bad_request(&format!("`{key}` must be an unsigned integer")))
}

/// Parse a required signed query parameter, or a `400` with a JSON body.
fn parse_i64(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<i64, (StatusCode, Json<Value>)> {
    params
        .get(key)
        .ok_or_else(|| bad_request(&format!("missing query parameter `{key}`")))?
        .parse()
        .map_err(|_err| bad_request(&format!("`{key}` must be an integer")))
}

/// A `400 Bad Request` with a `{ "error", "detail" }` JSON body.
fn bad_request(detail: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "bad_request", "detail": detail })),
    )
}

/// Parse the optional `limit`: absent → [`DEFAULT_LIMIT`], present → clamped to
/// [`MAX_LIMIT`], unparseable → `400`.
fn parse_limit(
    params: &std::collections::HashMap<String, String>,
) -> Result<usize, (StatusCode, Json<Value>)> {
    params.get("limit").map_or(Ok(DEFAULT_LIMIT), |raw| {
        raw.parse::<usize>()
            .map(|limit| limit.min(MAX_LIMIT))
            .map_err(|_err| bad_request("`limit` must be a non-negative integer"))
    })
}

/// Parse the optional resume `cursor`: absent → `None`, present → decoded, or a
/// `400` when it is malformed or belongs to another source.
fn parse_cursor(
    params: &std::collections::HashMap<String, String>,
) -> Result<Option<Cursor>, (StatusCode, Json<Value>)> {
    params.get("cursor").map_or(Ok(None), |raw| {
        Cursor::decode(raw)
            .map(Some)
            .map_err(|err| query_error_response(&err))
    })
}

/// Map one reader [`CellValue`] to its JSON form (see the API contract).
fn value_to_json(value: &CellValue) -> Value {
    match value {
        CellValue::Null => Value::Null,
        CellValue::I64(n) => (*n).into(),
        CellValue::U64(n) => (*n).into(),
        CellValue::F64(n) => (*n).into(),
        CellValue::Bool(b) => (*b).into(),
        CellValue::Ts(t) => (*t).into(),
        CellValue::Str(s) => Value::String(s.clone()),
        CellValue::Blob {
            text,
            full_len,
            truncated,
        } => json!({ "text": text.as_str(), "full_len": *full_len, "truncated": *truncated }),
        CellValue::ListI32(items) => Value::from(items.clone()),
    }
}

/// Shape one output row as a JSON object keyed by column name.
fn row_to_json(row: &OutRow) -> Value {
    let object = row
        .iter()
        .map(|(name, value)| (name.clone(), value_to_json(value)))
        .collect();
    Value::Object(object)
}

/// Shape a [`SectionPage`] as the `/v1/section` response body.
fn page_to_json(page: &SectionPage) -> Value {
    let rows: Vec<Value> = page.rows.iter().map(row_to_json).collect();
    let gaps: Vec<Value> = page
        .gaps
        .iter()
        .map(|gap| json!({ "from": gap.from, "to": gap.to }))
        .collect();
    let next_cursor = page
        .next_cursor
        .as_ref()
        .map_or(Value::Null, |cursor| Value::String(cursor.encode()));
    json!({
        "section": page.section,
        "source_id": page.source_id,
        "rows": rows,
        "gaps": gaps,
        "next_cursor": next_cursor,
    })
}

/// Map a reader [`QueryError`] to an HTTP status and a `{ error, detail }` body.
fn query_error_response(err: &QueryError) -> (StatusCode, Json<Value>) {
    let (status, code, detail) = match err {
        QueryError::UnknownSection(name) => (
            StatusCode::NOT_FOUND,
            "unknown_section",
            format!("no section named `{name}`"),
        ),
        QueryError::BadCursor(message) => (StatusCode::BAD_REQUEST, "bad_cursor", message.clone()),
        QueryError::Read(read) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "read_error",
            read.to_string(),
        ),
    };
    (status, Json(json!({ "error": code, "detail": detail })))
}

/// Stable wire name for a column's on-disk type.
const fn column_type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::I8 => "i8",
        ColumnType::I16 => "i16",
        ColumnType::I32 => "i32",
        ColumnType::I64 => "i64",
        ColumnType::U8 => "u8",
        ColumnType::U16 => "u16",
        ColumnType::U32 => "u32",
        ColumnType::U64 => "u64",
        ColumnType::F32 => "f32",
        ColumnType::F64 => "f64",
        ColumnType::Bool => "bool",
        ColumnType::Ts => "ts",
        ColumnType::StrId => "str",
        ColumnType::ListI32 => "list_i32",
    }
}

/// Stable wire name for a column's role: cumulative / gauge / label / timestamp.
const fn column_class_name(class: ColumnClass) -> &'static str {
    match class {
        ColumnClass::Cumulative => "c",
        ColumnClass::Gauge => "g",
        ColumnClass::Label => "l",
        ColumnClass::Timestamp => "t",
    }
}

/// Stable wire name for a section's collection semantics.
const fn semantics_name(semantics: Semantics) -> &'static str {
    match semantics {
        Semantics::SnapshotFull => "snapshot_full",
        Semantics::ConditionalFull => "conditional_full",
        Semantics::EventStream => "event_stream",
        Semantics::Changed => "changed",
        Semantics::OnChange => "on_change",
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use kronika_format::{PartMeta, SectionInput, build_part};
    use kronika_reader::Value as CellValue;
    use kronika_registry::Section;
    use kronika_registry::Ts;
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::pg_prepared_xacts::PgPreparedXacts;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use tower::ServiceExt;

    use super::{AppState, app, parse_limit, value_to_json};

    /// Build an [`AppState`] over a temp directory holding one `build_part`
    /// segment, then answer one request against `app(state)` in-process.
    ///
    /// Returned to the caller are the response status and its body parsed as
    /// JSON, so later tasks reuse the same fixture-to-response path.
    async fn fixture_response(uri: &str) -> (tempfile::TempDir, StatusCode, serde_json::Value) {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode empty section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("143000.pgm"), &bytes).expect("write segment");

        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let state = AppState::new(snapshot);

        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        let status = response.status();
        let json_body = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        assert!(json_body, "response must carry an application/json body");
        let collected = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let value: serde_json::Value =
            serde_json::from_slice(&collected).expect("body is valid JSON");
        (dir, status, value)
    }

    #[tokio::test]
    async fn version_returns_the_api_and_format_versions() {
        let (_dir, status, body) = fixture_response("/v1/version").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            serde_json::json!({ "api": "v1", "format_version": 1 }),
            "version body must match the committed shape exactly"
        );
    }

    #[tokio::test]
    async fn golden_harness_serves_version_over_a_fixture_directory() {
        // The harness proves the fixture -> AppState -> oneshot path works, so
        // later tasks can drive real query handlers through the same helper.
        let (dir, status, _body) = fixture_response("/v1/version").await;
        assert!(
            dir.path().exists(),
            "the fixture directory outlives the request"
        );
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn snapshot_arc_swap_round_trips_and_clone_stays_queryable() {
        // Serving-model smoke: no background task. Construct a snapshot, publish
        // it through ArcSwap, then clone the loaded pointer and run a `&mut`
        // query against the clone.
        let dir = tempfile::tempdir().expect("tempdir");
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("143000.pgm"), &bytes).expect("write segment");

        let snapshot = kronika_reader::LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let state = AppState::new(snapshot);

        let mut snap = state.snapshot.load().as_ref().clone();
        let page = kronika_reader::section(
            &mut snap,
            "pg_stat_bgwriter + pg_stat_checkpointer",
            7,
            i64::MIN,
            i64::MAX,
            10,
            None,
        );
        assert!(
            page.is_ok(),
            "a cloned snapshot must answer a section query: {:?}",
            page.err()
        );
    }

    /// Open a snapshot over a caller-built `dir` and answer one request.
    ///
    /// Unlike [`fixture_response`], the test writes its own segments into `dir`
    /// first; this returns the response status and its JSON body.
    async fn serve(dir: &std::path::Path, uri: &str) -> (StatusCode, serde_json::Value) {
        let snapshot = kronika_reader::LocalDirSnapshot::open(dir).expect("open snapshot");
        let state = AppState::new(snapshot);
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        let value = serde_json::from_slice(&bytes).expect("body is valid JSON");
        (status, value)
    }

    /// Write an empty `pg_stat_bgwriter + pg_stat_checkpointer` segment.
    fn write_bgwriter_segment(
        dir: &std::path::Path,
        file: &str,
        source: u64,
        min_ts: i64,
        max_ts: i64,
    ) {
        let body = BgwriterCheckpointer::encode(&[]).expect("encode section");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_006_001,
                rows: 0,
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        );
        std::fs::write(dir.join(file), &bytes).expect("write segment");
    }

    /// One `pg_stat_archiver` row with every optional column left NULL.
    fn archiver_row(ts: i64, archived: i64) -> PgStatArchiver {
        PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        }
    }

    #[tokio::test]
    async fn sources_fold_each_source_into_one_span() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);
        write_bgwriter_segment(dir.path(), "3000.pgm", 7, 3_000, 4_000);
        write_bgwriter_segment(dir.path(), "1500.pgm", 42, 1_500, 2_500);

        let (status, body) = serve(dir.path(), "/v1/sources").await;
        assert_eq!(status, StatusCode::OK, "sources responds 200");
        assert_eq!(
            body,
            serde_json::json!({ "sources": [
                { "source_id": 7, "min_ts": 1_000, "max_ts": 4_000, "segments": 2 },
                { "source_id": 42, "min_ts": 1_500, "max_ts": 2_500, "segments": 1 }
            ] }),
            "each source folds its units into one span, ordered by source_id"
        );
    }

    #[tokio::test]
    async fn sections_catalog_describes_archiver_from_the_registry() {
        // The catalog is static: it comes from the registry, not the fixture.
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        let (status, body) = serve(dir.path(), "/v1/sections").await;
        assert_eq!(status, StatusCode::OK, "sections responds 200");
        let archiver = body["sections"]
            .as_array()
            .expect("sections is an array")
            .iter()
            .find(|section| section["name"] == "pg_stat_archiver")
            .expect("pg_stat_archiver is in the catalog");
        assert_eq!(
            archiver["semantics"], "snapshot_full",
            "archiver is a full snapshot"
        );
        assert_eq!(
            archiver["sort_key"],
            serde_json::json!(["ts"]),
            "archiver sorts by ts"
        );
        let columns = archiver["columns"].as_array().expect("columns array");
        assert!(
            columns.contains(&serde_json::json!({ "name": "ts", "type": "ts", "class": "t" })),
            "ts is a timestamp-class ts column"
        );
        assert!(
            columns.contains(
                &serde_json::json!({ "name": "archived_count", "type": "i64", "class": "c" })
            ),
            "archived_count is a cumulative i64 counter"
        );
    }

    #[tokio::test]
    async fn segments_sum_rows_per_name_and_skip_dictionaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archiver_a = PgStatArchiver::encode(&[archiver_row(1_000, 1), archiver_row(1_100, 2)])
            .expect("encode archiver");
        let archiver_b =
            PgStatArchiver::encode(&[archiver_row(1_200, 3)]).expect("encode archiver");
        let bgwriter = BgwriterCheckpointer::encode(&[]).expect("encode bgwriter");
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: 1_008_001,
                    rows: 2,
                    body: &archiver_a,
                },
                SectionInput {
                    type_id: 1_008_001,
                    rows: 1,
                    body: &archiver_b,
                },
                SectionInput {
                    type_id: 1_006_001,
                    rows: 0,
                    body: &bgwriter,
                },
            ],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("1000.pgm"), &bytes).expect("write segment");

        let (status, body) = serve(dir.path(), "/v1/segments?source=7&from=0&to=3000").await;
        assert_eq!(status, StatusCode::OK, "segments responds 200");
        assert_eq!(
            body,
            serde_json::json!({ "segments": [
                { "segment_id": "1000", "source_id": 7, "min_ts": 1_000, "max_ts": 2_000,
                  "sections": [
                    { "name": "pg_stat_archiver", "rows": 3 },
                    { "name": "pg_stat_bgwriter + pg_stat_checkpointer", "rows": 0 }
                  ] }
            ] }),
            "repeated type_ids of one name sum their rows; sections order by name"
        );
    }

    #[tokio::test]
    async fn segments_outside_the_window_are_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "5000.pgm", 7, 5_000, 6_000);

        let (status, body) = serve(dir.path(), "/v1/segments?source=7&from=0&to=1000").await;
        assert_eq!(status, StatusCode::OK, "segments responds 200");
        assert_eq!(
            body,
            serde_json::json!({ "segments": [] }),
            "a window before every unit yields no segments"
        );
    }

    #[tokio::test]
    async fn segments_missing_a_required_parameter_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        let (status, body) = serve(dir.path(), "/v1/segments?from=0&to=1000").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a missing source is a client error"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn segments_non_numeric_parameter_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_bgwriter_segment(dir.path(), "1000.pgm", 7, 1_000, 2_000);

        let (status, body) = serve(dir.path(), "/v1/segments?source=abc&from=0&to=1000").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a non-numeric source is a client error"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("unsigned integer")),
            "the detail explains the parse failure, distinct from a missing parameter"
        );
    }

    /// Write a `pg_stat_archiver` segment holding `rows`.
    fn write_archiver_segment(
        dir: &std::path::Path,
        file: &str,
        source: u64,
        min_ts: i64,
        max_ts: i64,
        rows: &[PgStatArchiver],
    ) {
        let body = PgStatArchiver::encode(rows).expect("encode archiver");
        let bytes = build_part(
            &[SectionInput {
                type_id: 1_008_001,
                rows: u32::try_from(rows.len()).expect("row count fits u32"),
                body: &body,
            }],
            PartMeta {
                min_ts,
                max_ts,
                source_id: source,
            },
        );
        std::fs::write(dir.join(file), &bytes).expect("write segment");
    }

    #[test]
    fn value_to_json_maps_every_variant() {
        assert_eq!(
            value_to_json(&CellValue::Null),
            serde_json::json!(null),
            "null"
        );
        assert_eq!(
            value_to_json(&CellValue::I64(-5)),
            serde_json::json!(-5),
            "i64"
        );
        assert_eq!(
            value_to_json(&CellValue::U64(5)),
            serde_json::json!(5),
            "u64"
        );
        assert_eq!(
            value_to_json(&CellValue::F64(1.5)),
            serde_json::json!(1.5),
            "f64"
        );
        assert_eq!(
            value_to_json(&CellValue::Bool(true)),
            serde_json::json!(true),
            "bool"
        );
        assert_eq!(
            value_to_json(&CellValue::Ts(1_234)),
            serde_json::json!(1_234),
            "ts serializes as a number"
        );
        assert_eq!(
            value_to_json(&CellValue::Str("x".to_owned())),
            serde_json::json!("x"),
            "str"
        );
        assert_eq!(
            value_to_json(&CellValue::Blob {
                text: "ab".to_owned(),
                full_len: 10,
                truncated: true,
            }),
            serde_json::json!({ "text": "ab", "full_len": 10, "truncated": true }),
            "blob carries text, full_len and truncated"
        );
        assert_eq!(
            value_to_json(&CellValue::ListI32(vec![1, 2, 3])),
            serde_json::json!([1, 2, 3]),
            "list of i32"
        );
    }

    #[test]
    fn parse_limit_defaults_caps_and_rejects() {
        let empty = std::collections::HashMap::new();
        assert_eq!(
            parse_limit(&empty).ok(),
            Some(1_000),
            "an absent limit uses the default"
        );

        let explicit = std::collections::HashMap::from([("limit".to_owned(), "50".to_owned())]);
        assert_eq!(
            parse_limit(&explicit).ok(),
            Some(50),
            "an explicit limit is honored"
        );

        let huge = std::collections::HashMap::from([("limit".to_owned(), "99999".to_owned())]);
        assert_eq!(
            parse_limit(&huge).ok(),
            Some(10_000),
            "a limit above the ceiling is clamped"
        );

        let bad = std::collections::HashMap::from([("limit".to_owned(), "-1".to_owned())]);
        assert!(
            parse_limit(&bad).is_err(),
            "a non-numeric limit is rejected"
        );
    }

    #[tokio::test]
    async fn section_serializes_rows_over_a_covered_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5), archiver_row(1_100, 6)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=1000&to=2000&limit=10",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "section responds 200");
        assert_eq!(
            body,
            serde_json::json!({
                "section": "pg_stat_archiver",
                "source_id": 7,
                "rows": [
                    { "ts": 1_000, "archived_count": 5, "last_archived_wal": null, "last_archived_time": null, "failed_count": 0, "last_failed_wal": null, "last_failed_time": null, "stats_reset": null },
                    { "ts": 1_100, "archived_count": 6, "last_archived_wal": null, "last_archived_time": null, "failed_count": 0, "last_failed_wal": null, "last_failed_time": null, "stats_reset": null }
                ],
                "gaps": [],
                "next_cursor": null
            }),
            "rows serialize on union columns; a fully covered window has no gaps"
        );
    }

    #[tokio::test]
    async fn section_reports_a_gap_for_an_uncovered_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=5000&to=6000&limit=10",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "section responds 200");
        assert_eq!(
            body["rows"],
            serde_json::json!([]),
            "an uncovered window has no rows"
        );
        assert_eq!(
            body["gaps"],
            serde_json::json!([{ "from": 5_000, "to": 6_000 }]),
            "the whole uncovered window is one gap"
        );
        assert_eq!(
            body["next_cursor"],
            serde_json::json!(null),
            "an exhausted stream carries no cursor"
        );
    }

    #[tokio::test]
    async fn section_unknown_name_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/does_not_exist?source=7&from=0&to=3000",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown section is 404");
        assert_eq!(
            body["error"], "unknown_section",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn section_bad_parameter_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 5)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=abc&from=0&to=3000",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a non-numeric source is 400"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn section_cursor_pages_across_segment_boundaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );
        write_archiver_segment(
            dir.path(),
            "3000.pgm",
            7,
            3_000,
            4_000,
            &[archiver_row(3_000, 2)],
        );

        let (status, page1) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=0&to=5000&limit=1",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "page one responds 200");
        assert_eq!(
            page1["rows"].as_array().map(Vec::len),
            Some(1),
            "the limit caps page one at one row"
        );
        assert_eq!(
            page1["rows"][0]["ts"],
            serde_json::json!(1_000),
            "page one is the earliest row"
        );
        let cursor = page1["next_cursor"]
            .as_str()
            .expect("a full page carries a resume cursor");

        let (status, page2) = serve(
            dir.path(),
            &format!(
                "/v1/section/pg_stat_archiver?source=7&from=0&to=5000&limit=1&cursor={cursor}"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "page two responds 200");
        assert_eq!(
            page2["rows"][0]["ts"],
            serde_json::json!(3_000),
            "page two resumes at the next segment's row, no duplicate"
        );
    }

    #[tokio::test]
    async fn section_malformed_cursor_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/section/pg_stat_archiver?source=7&from=0&to=5000&cursor=notavalidcursor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a malformed cursor is a client error"
        );
        assert_eq!(
            body["error"], "bad_cursor",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn sections_batch_returns_a_page_per_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archiver = PgStatArchiver::encode(&[archiver_row(1_000, 1), archiver_row(1_100, 2)])
            .expect("encode archiver");
        let prepared = PgPreparedXacts::encode(&[]).expect("encode prepared_xacts");
        let bytes = build_part(
            &[
                SectionInput {
                    type_id: 1_008_001,
                    rows: 2,
                    body: &archiver,
                },
                SectionInput {
                    type_id: 1_010_001,
                    rows: 0,
                    body: &prepared,
                },
            ],
            PartMeta {
                min_ts: 1_000,
                max_ts: 2_000,
                source_id: 7,
            },
        );
        std::fs::write(dir.path().join("1000.pgm"), &bytes).expect("write segment");

        let (status, body) = serve(
            dir.path(),
            "/v1/sections/batch?source=7&from=1000&to=2000&names=pg_stat_archiver,pg_prepared_xacts&limit=10",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "batch responds 200");
        assert_eq!(
            body["pg_stat_archiver"]["rows"].as_array().map(Vec::len),
            Some(2),
            "the archiver page carries its rows"
        );
        assert_eq!(
            body["pg_stat_archiver"]["section"], "pg_stat_archiver",
            "each page names its section"
        );
        assert_eq!(
            body["pg_prepared_xacts"]["rows"],
            serde_json::json!([]),
            "a section with no rows is still present in the batch"
        );
    }

    #[tokio::test]
    async fn sections_batch_without_names_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );

        let (status, body) =
            serve(dir.path(), "/v1/sections/batch?source=7&from=1000&to=2000").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "batch without names is a client error"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }

    #[tokio::test]
    async fn sections_batch_with_only_separators_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_archiver_segment(
            dir.path(),
            "1000.pgm",
            7,
            1_000,
            2_000,
            &[archiver_row(1_000, 1)],
        );

        let (status, body) = serve(
            dir.path(),
            "/v1/sections/batch?source=7&from=1000&to=2000&names=,,",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a names list of only separators names no section"
        );
        assert_eq!(
            body["error"], "bad_request",
            "the error body names the fault"
        );
    }
}
