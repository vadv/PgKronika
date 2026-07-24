//! In-process web transport for BDD: build the JSON API router over a store
//! directory, query it without a socket, and compare a response row to the
//! values written in a `.feature`.
//!
//! This exercises the whole read path (collector output, reader, HTTP
//! serialization) against the same live-PostgreSQL oracle the direct-decode
//! steps use.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, header};
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

/// Captured in-process response, including the transport contract.
struct WebResponse {
    status: u16,
    headers: HeaderMap,
    body: Value,
}

impl WebResponse {
    fn media_type(&self) -> Option<&str> {
        self.headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    }
}

/// One in-process request against a fresh router over `dir`.
async fn request(dir: &Path, uri: &str, request_headers: &[(&str, &str)]) -> Result<WebResponse> {
    let snapshot = LocalDirSnapshot::open(dir).context("open the store snapshot")?;
    let state = AppState::new(snapshot).context("build the web state")?;
    let router = app(state, None, bdd_metrics_handle());
    request_with_router(&router, uri, request_headers).await
}

/// One request against an already-built router and its immutable publication.
async fn request_with_router(
    router: &Router,
    uri: &str,
    request_headers: &[(&str, &str)],
) -> Result<WebResponse> {
    let mut request = Request::builder().uri(uri);
    for &(name, value) in request_headers {
        request = request.header(name, value);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).context("build the request")?)
        .await
        .context("route the request")?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .context("read the response body")?
        .to_bytes();
    let body = serde_json::from_slice(&bytes).context("parse the JSON body")?;
    Ok(WebResponse {
        status,
        headers,
        body,
    })
}

/// The single source id the store holds, read through `/v1/sources`.
///
/// A BDD scenario collects one instance, so the store carries exactly one source.
pub(crate) async fn only_source(dir: &Path) -> Result<u64> {
    let response = request(dir, "/v1/sources", &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "/v1/sources returned status {}",
        response.status
    );
    let sources = response.body["sources"]
        .as_array()
        .context("`sources` is not an array")?;
    match sources.as_slice() {
        [source] => source["source_id"]
            .as_u64()
            .context("`source_id` is not a number"),
        other => bail!("expected exactly one source, got {}", other.len()),
    }
}

/// The single source's id and time span, read through `/v1/sources`.
pub(crate) async fn source_span(dir: &Path) -> Result<(u64, i64, i64)> {
    let response = request(dir, "/v1/sources", &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "/v1/sources returned status {}",
        response.status
    );
    let sources = response.body["sources"]
        .as_array()
        .context("`sources` is not an array")?;
    let [source] = sources.as_slice() else {
        bail!("expected exactly one source, got {}", sources.len());
    };
    Ok((
        source["source_id"]
            .as_u64()
            .context("`source_id` is not a number")?,
        source["min_ts"].as_i64().context("`min_ts` is not i64")?,
        source["max_ts"].as_i64().context("`max_ts` is not i64")?,
    ))
}

/// One section's diff over the whole period, for failure diagnostics.
pub(crate) async fn section_diff(dir: &Path, name: &str, source: u64) -> Result<Value> {
    let uri = format!(
        "/v1/section/{name}/diff?source={source}&from={}&to={}",
        i64::MIN,
        i64::MAX,
    );
    let response = request(dir, &uri, &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "/v1/section/{name}/diff returned status {}: {}",
        response.status,
        response.body
    );
    Ok(response.body)
}

/// The `/v1/anomalies` response over the store in `dir`.
pub(crate) async fn anomalies(dir: &Path, query: &str) -> Result<Value> {
    let response = request(dir, &format!("/v1/anomalies?{query}"), &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "/v1/anomalies returned status {}: {}",
        response.status,
        response.body
    );
    Ok(response.body)
}

/// Fetch one section's page for `source` over the widest possible window.
pub(crate) async fn section_page(dir: &Path, name: &str, source: u64) -> Result<Value> {
    let uri = format!(
        "/v1/section/{name}?source={source}&from={}&to={}&limit=10000",
        i64::MIN,
        i64::MAX,
    );
    let response = request(dir, &uri, &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "/v1/section/{name} returned status {}: {}",
        response.status,
        response.body
    );
    Ok(response.body)
}

/// Verify the fixed log fixture through one shared timeline publication.
///
/// The router is built once so the overview preview, `/events`, and `/health`
/// all read the same immutable fact set and cursor registry. This is the
/// end-to-end collector → PGM → reader → HTTP assertion used by the `PostgreSQL`
/// 15–18 feature matrix.
pub(crate) async fn assert_timeline_pg_log_contract(
    dir: &Path,
    from_us: i64,
    to_us: i64,
) -> Result<()> {
    anyhow::ensure!(from_us < to_us, "timeline fixture range is empty");
    let snapshot = LocalDirSnapshot::open(dir).context("open the store snapshot")?;
    let state = AppState::new(snapshot).context("build the timeline web state")?;
    let router = app(state, None, bdd_metrics_handle());

    let sources = request_with_router(&router, "/v1/sources", &[]).await?;
    anyhow::ensure!(
        sources.status == 200,
        "/v1/sources returned status {}: {}",
        sources.status,
        sources.body
    );
    let source_rows = sources.body["sources"]
        .as_array()
        .context("`sources` is not an array")?;
    let [source_row] = source_rows.as_slice() else {
        bail!("expected exactly one source, got {}", source_rows.len());
    };
    let source = source_row["source_id"]
        .as_u64()
        .context("`source_id` is not a number")?;

    assert_timeline_source_required(&router, from_us, to_us).await?;

    let overview = timeline_ok(
        &router,
        &format!("/v1/timeline/overview?source={source}&from={from_us}&to={to_us}"),
        "overview",
    )
    .await?;
    let events = timeline_ok(
        &router,
        &format!("/v1/timeline/events?source={source}&from={from_us}&to={to_us}"),
        "events",
    )
    .await?;
    let health = timeline_ok(
        &router,
        &format!(
            "/v1/timeline/health?source={source}&from={from_us}&to={to_us}&step={}",
            to_us
                .checked_sub(from_us)
                .context("timeline fixture range subtraction overflowed")?
        ),
        "health",
    )
    .await?;

    for (name, body) in [
        ("overview", &overview),
        ("events", &events),
        ("health", &health),
    ] {
        assert_source_meta(name, body, source)?;
    }
    assert_same_publication(&overview, &events, &health)?;
    assert_digest_reconciles(&overview)?;
    assert_shared_event_facts(&overview, &events, source)?;
    assert_parsed_panic_and_child_termination_do_not_set_health_floor(&health)?;
    Ok(())
}

async fn assert_timeline_source_required(router: &Router, from_us: i64, to_us: i64) -> Result<()> {
    let uris = [
        format!("/v1/timeline/overview?from={from_us}&to={to_us}"),
        format!("/v1/timeline/events?from={from_us}&to={to_us}"),
        format!("/v1/timeline/health?from={from_us}&to={to_us}"),
    ];
    for uri in uris {
        let response = request_with_router(router, &uri, &[]).await?;
        anyhow::ensure!(
            response.status == 400,
            "{uri} without source returned status {}: {}",
            response.status,
            response.body
        );
        anyhow::ensure!(
            response.body["code"] == "missing_query_parameter",
            "{uri} did not reject the missing source: {}",
            response.body
        );
        anyhow::ensure!(
            response.body["params"]["parameter"] == "source",
            "{uri} reported the wrong missing parameter: {}",
            response.body
        );
    }
    Ok(())
}

async fn timeline_ok(router: &Router, uri: &str, label: &str) -> Result<Value> {
    let response = request_with_router(router, uri, &[]).await?;
    anyhow::ensure!(
        response.status == 200,
        "timeline {label} returned status {}: {}",
        response.status,
        response.body
    );
    Ok(response.body)
}

fn assert_source_meta(label: &str, body: &Value, source: u64) -> Result<()> {
    let meta = body["meta"]
        .as_object()
        .with_context(|| format!("{label} `meta` is not an object"))?;
    anyhow::ensure!(
        meta.get("sources") == Some(&serde_json::json!([source])),
        "{label} did not retain the selected source: {}",
        body["meta"]
    );
    anyhow::ensure!(
        meta.get("available_sources") == Some(&serde_json::json!([source])),
        "{label} did not report the selected source as available: {}",
        body["meta"]
    );
    anyhow::ensure!(
        meta.get("source_status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "unavailable"),
        "{label} source status is unavailable: {}",
        body["meta"]
    );

    let freshness = meta
        .get("source_freshness")
        .and_then(Value::as_array)
        .with_context(|| format!("{label} `source_freshness` is not an array"))?;
    let [freshness] = freshness.as_slice() else {
        bail!(
            "{label} expected one source freshness record, got {}",
            freshness.len()
        );
    };
    anyhow::ensure!(freshness["source_id"] == source);
    anyhow::ensure!(
        freshness["source_scope_id"]
            .as_str()
            .is_some_and(|scope| !scope.is_empty())
    );
    anyhow::ensure!(freshness["source_completeness"] == "bounded_subset");
    anyhow::ensure!(freshness["retained_exactness"] == "exact");
    anyhow::ensure!(freshness["physical_count_semantics"] == "lower_bound");

    let loss = meta
        .get("loss")
        .and_then(Value::as_array)
        .with_context(|| format!("{label} `loss` is not an array"))?;
    let [loss] = loss.as_slice() else {
        bail!(
            "{label} expected one source loss record, got {}",
            loss.len()
        );
    };
    anyhow::ensure!(loss["source_id"] == source);
    anyhow::ensure!(loss["known_gaps"].is_array());
    anyhow::ensure!(
        loss["dropped_count_lower_bound"].is_null() || loss["dropped_count_lower_bound"].is_u64()
    );
    Ok(())
}

fn assert_same_publication(overview: &Value, events: &Value, health: &Value) -> Result<()> {
    let fact_set_id = overview["meta"]["fact_set_id"]
        .as_str()
        .context("overview fact_set_id is not a string")?;
    anyhow::ensure!(!fact_set_id.is_empty(), "overview fact_set_id is empty");
    for (label, body) in [("events", events), ("health", health)] {
        anyhow::ensure!(
            body["meta"]["fact_set_id"] == fact_set_id,
            "{label} used a different fact set: {}",
            body["meta"]
        );
        anyhow::ensure!(
            body["meta"]["view_generation"] == overview["meta"]["view_generation"],
            "{label} used a different view generation: {}",
            body["meta"]
        );
    }
    Ok(())
}

fn assert_digest_reconciles(overview: &Value) -> Result<()> {
    let digest = &overview["event_digest"];
    let total = json_u64(
        &digest["retained_error_occurrence_count"],
        "retained_error_occurrence_count",
    )?;
    anyhow::ensure!(
        total == 3,
        "fixed fixture retained {total} error occurrences instead of 3: {digest}"
    );
    anyhow::ensure!(
        digest["retained_error_group_count"] == 3,
        "fixed fixture did not retain three error groups: {digest}"
    );
    anyhow::ensure!(
        json_u64(
            &digest["retained_observation_row_count"],
            "retained_observation_row_count"
        )? >= total
    );
    anyhow::ensure!(digest["exactness"] == "exact");

    let severity_total = checked_json_array_sum(&digest["by_severity"], "by_severity")?;
    let category_total = checked_json_array_sum(&digest["by_category"], "by_category")?;
    let sqlstate_top = checked_json_entry_sum(&digest["by_sqlstate"], "count", "by_sqlstate")?;
    let sqlstate_total = json_u64(&digest["sqlstate_missing_count"], "sqlstate_missing_count")?
        .checked_add(json_u64(
            &digest["sqlstate_other_count"],
            "sqlstate_other_count",
        )?)
        .and_then(|sum| sum.checked_add(sqlstate_top))
        .context("SQLSTATE digest count overflowed")?;
    let joint_top = checked_json_entry_sum(&digest["joint_top"], "count", "joint_top")?;
    let joint_total = json_u64(&digest["joint_other_count"], "joint_other_count")?
        .checked_add(joint_top)
        .context("joint digest count overflowed")?;

    anyhow::ensure!(
        [severity_total, category_total, sqlstate_total, joint_total]
            .into_iter()
            .all(|sum| sum == total),
        "overview count axes do not reconcile to {total}: {digest}"
    );
    anyhow::ensure!(digest["lifecycle"]["crashes"] == 1);
    let signals = digest["lifecycle"]["signals"]
        .as_array()
        .context("lifecycle signals is not an array")?;
    anyhow::ensure!(
        signals
            .iter()
            .any(|signal| signal["signal"] == 9 && signal["count"] == 1),
        "fixed child termination is absent from lifecycle counts: {digest}"
    );
    Ok(())
}

fn checked_json_array_sum(value: &Value, label: &str) -> Result<u64> {
    let values = value
        .as_array()
        .with_context(|| format!("`{label}` is not an array"))?;
    values.iter().try_fold(0_u64, |sum, value| {
        sum.checked_add(json_u64(value, label)?)
            .with_context(|| format!("`{label}` count overflowed"))
    })
}

fn checked_json_entry_sum(value: &Value, field: &str, label: &str) -> Result<u64> {
    let entries = value
        .as_array()
        .with_context(|| format!("`{label}` is not an array"))?;
    entries.iter().try_fold(0_u64, |sum, entry| {
        sum.checked_add(json_u64(&entry[field], label)?)
            .with_context(|| format!("`{label}` count overflowed"))
    })
}

fn json_u64(value: &Value, label: &str) -> Result<u64> {
    value
        .as_u64()
        .with_context(|| format!("`{label}` is not an unsigned integer: {value}"))
}

fn assert_shared_event_facts(overview: &Value, events: &Value, source: u64) -> Result<()> {
    let preview = overview["notable_preview"]["observations"]
        .as_array()
        .context("overview notable observations is not an array")?;
    let page = events["events"]
        .as_array()
        .context("events page is not an array")?;
    anyhow::ensure!(
        preview == page,
        "overview preview and /events projected different EventFacts:\npreview={preview:?}\nevents={page:?}"
    );
    anyhow::ensure!(
        preview.len() == 3,
        "fixed fixture produced {} notable facts instead of 3: {preview:?}",
        preview.len()
    );
    anyhow::ensure!(overview["notable_preview"]["omitted_count"] == 0);
    anyhow::ensure!(events["next_cursor"].is_null());
    anyhow::ensure!(events["omitted_by_response_filter"] == 0);
    anyhow::ensure!(events["retained_exactness"] == "exact");
    anyhow::ensure!(events["source_completeness"] == "bounded_subset");
    anyhow::ensure!(events["physical_count_semantics"] == "lower_bound");

    let mut seen_event_ids = BTreeSet::new();
    let mut seen_instance_ids = BTreeSet::new();
    for fact in preview {
        let (event_id, instance_id) = assert_event_fact(fact, source)?;
        anyhow::ensure!(!event_id.is_empty() && seen_event_ids.insert(event_id));
        anyhow::ensure!(!instance_id.is_empty() && seen_instance_ids.insert(instance_id));
    }

    let classes = preview
        .iter()
        .filter_map(|fact| fact["notable_class"].as_str())
        .collect::<BTreeSet<_>>();
    let expected_classes = BTreeSet::from([
        "deadlock_observation",
        "panic_severity_observation",
        "server_child_sigkill",
    ]);
    anyhow::ensure!(
        classes == expected_classes,
        "fixed fixture produced unexpected notable classes: {classes:?}"
    );
    Ok(())
}

fn assert_event_fact(fact: &Value, source: u64) -> Result<(&str, &str)> {
    let expected_fields = BTreeSet::from([
        "event_id",
        "event_instance_id",
        "source_id",
        "source_scope_id",
        "source_type_id",
        "identity_quality",
        "sort_ts_us",
        "occurred_at_us",
        "occurrence_count",
        "event_kind",
        "notable_class",
        "evidence_quality",
        "quality_flags",
        "payload",
        "supporting_evidence",
        "loss",
    ]);
    let object = fact.as_object().context("EventFact is not an object")?;
    let fields = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    anyhow::ensure!(
        fields == expected_fields,
        "EventFact fields changed: {fields:?}"
    );
    let event_id = fact["event_id"]
        .as_str()
        .context("EventFact.event_id is not a string")?;
    let instance_id = fact["event_instance_id"]
        .as_str()
        .context("EventFact.event_instance_id is not a string")?;
    anyhow::ensure!(fact["source_id"] == source);
    anyhow::ensure!(
        fact["source_scope_id"]
            .as_str()
            .is_some_and(|scope| !scope.is_empty())
    );
    anyhow::ensure!(fact["source_type_id"].is_u64());
    anyhow::ensure!(fact["identity_quality"] == "content_derived");
    anyhow::ensure!(fact["sort_ts_us"].is_i64());
    anyhow::ensure!(fact["occurred_at_us"].is_i64() || fact["occurred_at_us"].is_null());
    anyhow::ensure!(
        fact["occurrence_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    anyhow::ensure!(
        fact["payload"]["kind"] == fact["event_kind"],
        "typed payload kind disagrees with EventFact kind: {fact}"
    );
    anyhow::ensure!(fact["notable_class"].is_string());
    let expected_evidence_quality = match fact["event_kind"].as_str() {
        Some("pg.lifecycle.child_signal_termination") => "parsed",
        _ => "heuristic",
    };
    anyhow::ensure!(
        fact["evidence_quality"] == expected_evidence_quality,
        "fixed log fact carried the wrong evidence quality: {fact}"
    );
    anyhow::ensure!(fact["quality_flags"].is_u64());
    anyhow::ensure!(fact["loss"].is_null() || fact["loss"].is_object());
    assert_supporting_evidence(fact)?;
    Ok((event_id, instance_id))
}

fn assert_supporting_evidence(fact: &Value) -> Result<()> {
    let expected_fields = BTreeSet::from([
        "observation_id",
        "section_body_id",
        "catalog_entry_ordinal",
        "row_ordinal",
        "dictionary_context_id",
        "segment_locator",
    ]);
    let evidence = fact["supporting_evidence"]
        .as_array()
        .context("supporting_evidence is not an array")?;
    let [evidence] = evidence.as_slice() else {
        bail!(
            "EventFact expected one supporting observation, got {}: {fact}",
            evidence.len()
        );
    };
    let fields = evidence
        .as_object()
        .context("supporting evidence is not an object")?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        fields == expected_fields,
        "supporting evidence fields changed: {fields:?}"
    );
    for field in ["observation_id", "section_body_id", "dictionary_context_id"] {
        anyhow::ensure!(
            evidence[field]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "supporting evidence `{field}` is empty: {evidence}"
        );
    }
    anyhow::ensure!(evidence["catalog_entry_ordinal"].is_u64());
    anyhow::ensure!(evidence["row_ordinal"].is_u64());
    anyhow::ensure!(
        evidence["segment_locator"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    Ok(())
}

fn assert_parsed_panic_and_child_termination_do_not_set_health_floor(health: &Value) -> Result<()> {
    let points = health["points"]
        .as_array()
        .context("health points is not an array")?;
    anyhow::ensure!(
        !points.is_empty(),
        "health response did not cover the fixed fixture range"
    );
    for point in points {
        anyhow::ensure!(
            point["floor_evidence"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "parsed PANIC or child termination created trusted floor evidence: {point}"
        );
        anyhow::ensure!(
            point["overall_state"] != "critical",
            "parsed PANIC or child termination created critical health: {point}"
        );
    }
    Ok(())
}

/// Verify that language preferences cannot change a Problem representation.
pub(crate) async fn assert_locale_neutral_problem(dir: &Path) -> Result<()> {
    const URI: &str = "/v1/segments?source=not-a-number&from=0&to=1";
    let english = request(dir, URI, &[("accept-language", "en")]).await?;
    let russian = request(dir, URI, &[("accept-language", "ru-RU, ru;q=0.9")]).await?;

    for response in [&english, &russian] {
        anyhow::ensure!(
            response.status == 400,
            "problem status was {}",
            response.status
        );
        anyhow::ensure!(
            response.media_type() == Some("application/problem+json"),
            "problem media type was {:?}",
            response.media_type()
        );
        anyhow::ensure!(
            response
                .headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                == Some("no-store"),
            "problem response did not disable caching"
        );
        anyhow::ensure!(response.headers.get(header::CONTENT_LANGUAGE).is_none());
        anyhow::ensure!(response.headers.get(header::VARY).is_none());

        let object = response
            .body
            .as_object()
            .context("problem body is not an object")?;
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        anyhow::ensure!(
            keys == ["code", "instance", "params", "status", "type"],
            "unexpected problem fields: {keys:?}"
        );
        anyhow::ensure!(response.body["status"] == 400);
        anyhow::ensure!(response.body["code"] == "invalid_query_parameter");
        anyhow::ensure!(
            response.body["params"]
                == serde_json::json!({ "parameter": "source", "expected": "uint64" })
        );
        let instance = response.body["instance"]
            .as_str()
            .context("problem instance is not a string")?;
        let request_id = response
            .headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .context("problem response has no request id")?;
        anyhow::ensure!(
            instance == format!("https://pgkronika.dev/problems/occurrences/{request_id}")
        );
        anyhow::ensure!(
            request_id.len() == 32
                && request_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "request id is not 32 lowercase hex characters"
        );
    }

    let mut english_body = english.body;
    let mut russian_body = russian.body;
    english_body
        .as_object_mut()
        .context("English problem is not an object")?
        .remove("instance");
    russian_body
        .as_object_mut()
        .context("Russian problem is not an object")?
        .remove("instance");
    anyhow::ensure!(
        english_body == russian_body,
        "Accept-Language changed the problem"
    );

    let oversized_uri = format!("/v1/version?{}", "x".repeat(8_193));
    let oversized = request(dir, &oversized_uri, &[]).await?;
    anyhow::ensure!(oversized.status == 413);
    anyhow::ensure!(oversized.body["code"] == "query_limit_exceeded");
    anyhow::ensure!(
        oversized.body["params"]
            == serde_json::json!({
                "resource": "query_bytes",
                "limit": 8_192,
                "observed": 8_193,
            })
    );
    Ok(())
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
