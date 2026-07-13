//! In-process web transport for BDD: build the JSON API router over a store
//! directory, query it without a socket, and compare a response row to the
//! values written in a `.feature`.
//!
//! This exercises the whole read path (collector output, reader, HTTP
//! serialization) against the same live-PostgreSQL oracle the direct-decode
//! steps use.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use kronika_reader::LocalDirSnapshot;
use kronika_registry::Cell;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use pg_kronika_web::{AppState, app};
use serde_json::Value;
use tower::ServiceExt as _;

use crate::harness::assert_row::KeyMatch;
use crate::harness::expected::{ExpectedColumn, ExpectedValue};

/// Process-global Prometheus handle for BDD harness requests.
static BDD_RECORDER: OnceLock<PrometheusHandle> = OnceLock::new();

fn bdd_metrics_handle() -> PrometheusHandle {
    BDD_RECORDER
        .get_or_init(|| {
            PrometheusBuilder::new()
                .install_recorder()
                .expect("install global Prometheus recorder for BDD harness")
        })
        .clone()
}

/// One in-process request against a fresh router over `dir`; returns the HTTP
/// status and the parsed JSON body.
async fn request(dir: &Path, uri: &str) -> Result<(u16, Value)> {
    let snapshot = LocalDirSnapshot::open(dir).context("open the store snapshot")?;
    let router = app(AppState::new(snapshot), None, bdd_metrics_handle());
    let response = router
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .context("build the request")?,
        )
        .await
        .context("route the request")?;
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .context("read the response body")?
        .to_bytes();
    let body = serde_json::from_slice(&bytes).context("parse the JSON body")?;
    Ok((status, body))
}

/// The single source id the store holds, read through `/v1/sources`.
///
/// A BDD scenario collects one instance, so the store carries exactly one source.
pub(crate) async fn only_source(dir: &Path) -> Result<u64> {
    let (status, body) = request(dir, "/v1/sources").await?;
    anyhow::ensure!(status == 200, "/v1/sources returned status {status}");
    let sources = body["sources"]
        .as_array()
        .context("`sources` is not an array")?;
    match sources.as_slice() {
        [source] => source["source_id"]
            .as_u64()
            .context("`source_id` is not a number"),
        other => bail!("expected exactly one source, got {}", other.len()),
    }
}

/// Fetch one section's page for `source` over the widest possible window.
pub(crate) async fn section_page(dir: &Path, name: &str, source: u64) -> Result<Value> {
    let uri = format!(
        "/v1/section/{name}?source={source}&from={}&to={}&limit=10000",
        i64::MIN,
        i64::MAX,
    );
    let (status, body) = request(dir, &uri).await?;
    anyhow::ensure!(
        status == 200,
        "/v1/section/{name} returned status {status}: {body}"
    );
    Ok(body)
}

/// Assert the page holds exactly one row whose named columns match `expected`.
pub(crate) fn assert_one_row(page: &Value, expected: &[ExpectedColumn]) -> Result<()> {
    let rows = page["rows"].as_array().context("`rows` is not an array")?;
    let [row] = rows.as_slice() else {
        bail!("expected exactly one row, got {}; page: {page}", rows.len());
    };
    assert_columns(row, expected)
}

/// Assert the page holds a row matching every key, whose columns match `expected`.
pub(crate) fn assert_row_where(
    page: &Value,
    keys: &[KeyMatch],
    expected: &[ExpectedColumn],
) -> Result<()> {
    let rows = page["rows"].as_array().context("`rows` is not an array")?;
    let row = rows
        .iter()
        .find(|row| keys.iter().all(|key| json_matches_key(row, key)))
        .with_context(|| format!("no web row matched {keys:?}; page: {page}"))?;
    assert_columns(row, expected)
}

/// Report every column of `row` that differs from `expected`.
fn assert_columns(row: &Value, expected: &[ExpectedColumn]) -> Result<()> {
    let diffs: Vec<String> = expected
        .iter()
        .filter_map(|col| column_diff(col, &row[col.name.as_str()]))
        .collect();
    if diffs.is_empty() {
        return Ok(());
    }
    bail!("web row did not match:\n{}\nrow: {row}", diffs.join("\n"))
}

/// Whether a JSON row satisfies one key, resolving strings the way the API does.
fn json_matches_key(row: &Value, key: &KeyMatch) -> bool {
    match key {
        KeyMatch::Cell { column, cell } => json_matches_cell(&row[column.as_str()], cell),
        KeyMatch::Str { column, value } => row[column.as_str()].as_str() == Some(value.as_str()),
    }
}

/// A mismatch line if the JSON `actual` differs from the expected value.
fn column_diff(col: &ExpectedColumn, actual: &Value) -> Option<String> {
    let (matches, want) = match &col.value {
        ExpectedValue::Cell(cell) => (json_matches_cell(actual, cell), format!("{cell:?}")),
        ExpectedValue::Str(want) => (actual.as_str() == Some(want.as_str()), format!("{want:?}")),
        ExpectedValue::AtLeast(floor) => (
            actual.as_i64().is_some_and(|v| v >= *floor),
            format!(">= {floor}"),
        ),
    };
    (!matches).then(|| format!("  {}: expected {want}, got {actual}", col.name))
}

/// Whether a JSON value equals a decoded [`Cell`] as the API serializes it.
fn json_matches_cell(actual: &Value, cell: &Cell) -> bool {
    match cell {
        Cell::Null => actual.is_null(),
        Cell::Bool(b) => actual.as_bool() == Some(*b),
        Cell::I16(n) => actual.as_i64() == Some(i64::from(*n)),
        Cell::I32(n) => actual.as_i64() == Some(i64::from(*n)),
        Cell::I64(n) | Cell::Ts(n) => actual.as_i64() == Some(*n),
        Cell::U32(n) => actual.as_u64() == Some(u64::from(*n)),
        Cell::U64(n) => actual.as_u64() == Some(*n),
        Cell::F64(f) => actual.as_f64().is_some_and(|v| (v - *f).abs() <= 1e-6),
        Cell::ListI32(items) => actual.as_array().is_some_and(|array| {
            array.len() == items.len()
                && array
                    .iter()
                    .zip(items)
                    .all(|(value, item)| value.as_i64() == Some(i64::from(*item)))
        }),
        // A StrId serializes as its resolved string; string expectations use
        // ExpectedValue::Str, never a Cell.
        Cell::StrId(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{column_diff, json_matches_cell, json_matches_key};
    use crate::harness::assert_row::KeyMatch;
    use crate::harness::expected::{ExpectedColumn, ExpectedValue};
    use kronika_registry::Cell;
    use serde_json::json;

    #[test]
    fn json_matches_cell_covers_scalars_lists_and_null() {
        assert!(json_matches_cell(&json!(null), &Cell::Null), "null");
        assert!(json_matches_cell(&json!(true), &Cell::Bool(true)), "bool");
        assert!(json_matches_cell(&json!(-5), &Cell::I64(-5)), "i64");
        assert!(
            json_matches_cell(&json!(1_700_000_000_i64), &Cell::Ts(1_700_000_000)),
            "ts is a number"
        );
        assert!(
            json_matches_cell(&json!(-5), &Cell::I16(-5)),
            "i16 widens to a number"
        );
        assert!(json_matches_cell(&json!(3), &Cell::I32(3)), "i32");
        assert!(json_matches_cell(&json!(7), &Cell::U32(7)), "u32");
        assert!(
            json_matches_cell(&json!(u64::MAX), &Cell::U64(u64::MAX)),
            "a u64 above i64::MAX survives"
        );
        assert!(json_matches_cell(&json!(1.5), &Cell::F64(1.5)), "f64");
        assert!(
            json_matches_cell(&json!([1, 2, 3]), &Cell::ListI32(vec![1, 2, 3])),
            "list of i32"
        );
        assert!(
            !json_matches_cell(&json!(1), &Cell::I64(2)),
            "a wrong number is a mismatch"
        );
        assert!(
            !json_matches_cell(&json!("s"), &Cell::StrId(1)),
            "a StrId is never matched as a cell"
        );
    }

    #[test]
    fn column_diff_reports_string_and_floor_mismatches() {
        let str_col = ExpectedColumn {
            name: "last_archived_wal".to_owned(),
            value: ExpectedValue::Str("00000001".to_owned()),
        };
        assert!(
            column_diff(&str_col, &json!("00000001")).is_none(),
            "a matching string agrees"
        );
        assert!(
            column_diff(&str_col, &json!(null)).is_some(),
            "null is not the expected string"
        );

        let floor_col = ExpectedColumn {
            name: "archived_count".to_owned(),
            value: ExpectedValue::AtLeast(10),
        };
        assert!(
            column_diff(&floor_col, &json!(11)).is_none(),
            "at or above the floor agrees"
        );
        assert!(
            column_diff(&floor_col, &json!(9)).is_some(),
            "below the floor is a diff"
        );
    }

    #[test]
    fn json_matches_key_matches_scalar_and_resolved_string_keys() {
        let row = json!({ "datname": "kronika_db", "datid": 5 });
        assert!(
            json_matches_key(
                &row,
                &KeyMatch::Str {
                    column: "datname".to_owned(),
                    value: "kronika_db".to_owned(),
                }
            ),
            "a string key matches the resolved column"
        );
        assert!(
            json_matches_key(
                &row,
                &KeyMatch::Cell {
                    column: "datid".to_owned(),
                    cell: Cell::U32(5),
                }
            ),
            "a scalar key matches the number"
        );
        assert!(
            !json_matches_key(
                &row,
                &KeyMatch::Str {
                    column: "datname".to_owned(),
                    value: "other_db".to_owned(),
                }
            ),
            "a different string does not match"
        );
    }
}
