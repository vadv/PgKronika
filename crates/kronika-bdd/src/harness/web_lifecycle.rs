//! End-to-end sibling-index lifecycle assertions over real web processes.

use std::fs;

use anyhow::{Context, Result, ensure};
use kronika_reader::{FallbackConfig, QUALIFICATION_PUBLISH_FAULT_ENV};
use serde_json::Value;

use super::web_process::{
    PublishBarrier, SidecarMismatch, WebCase, WebClient, corrupt_sidecar, fingerprint, metric,
    mismatched_sidecar,
};

/// Stable ordinary timeline payloads and canonical OVF from the first process.
#[derive(Debug, Clone)]
pub(crate) struct TimelineBaseline {
    overview: Value,
    events: Value,
    health: Value,
    sidecar: Vec<u8>,
}

#[derive(Debug)]
struct TimelineResponses {
    overview: Value,
    events: Value,
    health: Value,
}

/// Missing sibling → cold build → graceful restart → durable zero-PGM hit.
pub(crate) async fn establish_restart_baseline(
    segment: &std::path::Path,
) -> Result<TimelineBaseline> {
    let case = WebCase::from_segment(segment, "missing-restart")?;
    ensure!(
        !case.sidecar().exists(),
        "fresh lifecycle case unexpectedly has an OVF"
    );

    let first = case.spawn(&[]).await?;
    let first_responses = timeline(first.client(), &case).await?;
    let first_metrics = first.client().metrics().await?;
    ensure!(
        metric(&first_metrics, "kronika_web_overview_rebuilt_total", &[])? >= 1.0,
        "missing sidecar did not record a source rebuild"
    );
    ensure!(
        metric(
            &first_metrics,
            "overview_raw_fallback_total",
            &[("reason", "missing")]
        )? >= 1.0,
        "missing sidecar did not expose its rebuild reason"
    );
    ensure!(
        metric(&first_metrics, "overview_pgm_sections_decoded", &[])? > 0.0,
        "cold real-process request decoded no PGM sections"
    );
    let canonical = case.admitted_sidecar()?;
    let before_restart = fingerprint(&case.sidecar())?;
    case.assert_sources_preserved()?;
    first.stop().await?;

    let restarted = case.spawn(&[]).await?;
    let restart_responses = timeline(restarted.client(), &case).await?;
    assert_timeline_equal(&first_responses, &restart_responses)?;
    ensure!(
        fingerprint(&case.sidecar())? == before_restart,
        "restart rewrote a valid sibling OVF"
    );
    assert_durable_zero_pgm(&restarted.client().metrics().await?)?;
    ensure!(
        case.admitted_sidecar()? == canonical,
        "restart changed canonical sidecar bytes"
    );
    case.assert_sources_preserved()?;
    restarted.stop().await?;

    Ok(TimelineBaseline {
        overview: normalized(first_responses.overview),
        events: normalized(first_responses.events),
        health: normalized(first_responses.health),
        sidecar: canonical,
    })
}

/// Corrupt committed sibling is rejected, rebuilt atomically, then reused.
pub(crate) async fn assert_corrupt_recovery(
    segment: &std::path::Path,
    baseline: &TimelineBaseline,
) -> Result<()> {
    let case = WebCase::from_segment(segment, "corrupt-recovery")?;
    case.seed_sidecar(&corrupt_sidecar(&baseline.sidecar)?)?;
    let process = case.spawn(&[]).await?;
    let responses = timeline(process.client(), &case).await?;
    baseline.assert_responses(&responses)?;
    let metrics = process.client().metrics().await?;
    assert_rebuild_reason(&metrics, "corrupt")?;
    ensure!(
        case.admitted_sidecar()? == baseline.sidecar,
        "corrupt sibling was not replaced by the canonical OVF"
    );
    ensure!(
        case.publisher_artifacts()?.is_empty(),
        "successful corrupt recovery left a publisher artifact"
    );
    process.stop().await?;

    let stable = fingerprint(&case.sidecar())?;
    let restarted = case.spawn(&[]).await?;
    let overview = request_overview(restarted.client(), &case).await?;
    ensure!(
        normalized(overview) == baseline.overview,
        "post-recovery restart changed the ordinary overview"
    );
    assert_durable_zero_pgm(&restarted.client().metrics().await?)?;
    ensure!(
        fingerprint(&case.sidecar())? == stable,
        "post-recovery restart rewrote the canonical OVF"
    );
    case.assert_sources_preserved()?;
    restarted.stop().await
}

/// Every current header identity axis is rejected under its production class.
pub(crate) async fn assert_stale_identity_recovery(
    segment: &std::path::Path,
    baseline: &TimelineBaseline,
) -> Result<()> {
    for mismatch in SidecarMismatch::ALL {
        let case = WebCase::from_segment(segment, mismatch.label())?;
        let stale = mismatched_sidecar(&baseline.sidecar, mismatch)?;
        ensure!(
            stale != baseline.sidecar,
            "{} mismatch did not change the candidate",
            mismatch.label()
        );
        case.seed_sidecar(&stale)?;
        let process = case.spawn(&[]).await?;
        let responses = timeline(process.client(), &case).await?;
        baseline.assert_responses(&responses)?;
        assert_rebuild_reason(
            &process.client().metrics().await?,
            mismatch.rebuild_reason(),
        )?;
        ensure!(
            case.admitted_sidecar()? == baseline.sidecar,
            "{} mismatch was not replaced canonically",
            mismatch.label()
        );
        ensure!(
            case.publisher_artifacts()?.is_empty(),
            "{} recovery left a publisher artifact",
            mismatch.label()
        );
        case.assert_sources_preserved()?;
        process.stop().await?;
    }
    Ok(())
}

/// A process killed at the pre-rename barrier leaves no admitted partial OVF;
/// a new process ignores both that residue and a pre-existing foreign residue.
pub(crate) async fn assert_interrupted_publication_recovery(
    segment: &std::path::Path,
    baseline: &TimelineBaseline,
) -> Result<()> {
    let case = WebCase::from_segment(segment, "interrupted-publication")?;
    let preexisting = case.data_dir().join(".pgkronika-overview.tmp-preexisting");
    fs::write(&preexisting, b"not an admitted sidecar")
        .context("seed interrupted-publication residue")?;
    let barrier = PublishBarrier::bind(case.control_path("publication.sock")?)?;
    let environment = barrier.environment();
    let process = case.spawn(&[environment]).await?;
    let client = process.client();
    let target = overview_target(&case);
    let pending = tokio::spawn(async move { client.get_json(&target).await });
    let lease = barrier.arrive().await?;
    let generated = case.data_dir().join(&lease.temporary_name);
    ensure!(
        generated.is_file(),
        "barrier announced no synced temporary sidecar"
    );
    ensure!(
        !case.sidecar().exists(),
        "partial publication became visible as canonical before rename"
    );
    process.crash().await?;
    drop(lease);
    ensure!(
        pending
            .await
            .context("join interrupted HTTP request")?
            .is_err(),
        "HTTP request unexpectedly survived the killed web process"
    );
    ensure!(
        !case.sidecar().exists(),
        "killed publication left an admitted canonical OVF"
    );
    ensure!(
        generated.is_file() && preexisting.is_file(),
        "simulated crash did not retain both temporary residues"
    );

    let recovered = case.spawn(&[]).await?;
    let responses = timeline(recovered.client(), &case).await?;
    baseline.assert_responses(&responses)?;
    assert_rebuild_reason(&recovered.client().metrics().await?, "missing")?;
    ensure!(
        case.admitted_sidecar()? == baseline.sidecar,
        "new process did not recover the canonical OVF after a crash"
    );
    case.assert_sources_preserved()?;
    recovered.stop().await
}

/// A recoverable write failure serves honest facts from bounded memory, a
/// later process publishes durably, and the following process is a cold restart
/// hit with zero PGM body work.
pub(crate) async fn assert_publication_failure_recovery(
    segment: &std::path::Path,
    baseline: &TimelineBaseline,
) -> Result<()> {
    let case = WebCase::from_segment(segment, "publication-failure")?;
    let failed = case
        .spawn(&[(QUALIFICATION_PUBLISH_FAULT_ENV, "transient_io")])
        .await?;
    let responses = timeline(failed.client(), &case).await?;
    baseline.assert_responses(&responses)?;
    ensure!(
        !case.sidecar().exists(),
        "injected publication failure unexpectedly committed an OVF"
    );
    let metrics = failed.client().metrics().await?;
    ensure!(
        metric(
            &metrics,
            "overview_persist_failures_total",
            &[("reason", "transient_io")]
        )? >= 1.0,
        "recoverable publication failure was not classified as transient"
    );
    ensure!(
        metric(
            &metrics,
            "overview_cache_entries",
            &[("class", "publication_fallback")]
        )? > 0.5
            && metric(
                &metrics,
                "overview_cache_entries",
                &[("class", "publication_fallback")]
            )? < 1.5,
        "recoverable failure did not retain one bounded fallback entry"
    );
    let fallback_bytes = metric(
        &metrics,
        "overview_cache_bytes",
        &[("class", "publication_fallback")],
    )?;
    let fallback_limit = u32::try_from(FallbackConfig::default().bytes())
        .context("default fallback bound fits the Prometheus exact-integer range")?;
    ensure!(
        fallback_bytes > 0.0 && fallback_bytes <= f64::from(fallback_limit),
        "fallback residency {fallback_bytes} exceeds its configured byte bound"
    );
    failed.stop().await?;

    let recovered = case.spawn(&[]).await?;
    let recovered_responses = timeline(recovered.client(), &case).await?;
    baseline.assert_responses(&recovered_responses)?;
    assert_rebuild_reason(&recovered.client().metrics().await?, "missing")?;
    ensure!(
        case.admitted_sidecar()? == baseline.sidecar,
        "recovered writer did not publish the canonical sidecar"
    );
    recovered.stop().await?;

    let stable = fingerprint(&case.sidecar())?;
    let durable = case.spawn(&[]).await?;
    let overview = request_overview(durable.client(), &case).await?;
    ensure!(
        normalized(overview) == baseline.overview,
        "durable process returned a different recovered overview"
    );
    assert_durable_zero_pgm(&durable.client().metrics().await?)?;
    ensure!(
        fingerprint(&case.sidecar())? == stable,
        "durable process rewrote the recovered OVF"
    );
    case.assert_sources_preserved()?;
    durable.stop().await
}

/// A cursor is process-local and becomes the public 410 `cursor_expired`
/// problem after restart, while all ordinary timeline payloads stay equal.
pub(crate) async fn assert_cursor_restart_contract(
    segment: &std::path::Path,
    baseline: &TimelineBaseline,
) -> Result<()> {
    let case = WebCase::from_segment(segment, "cursor-restart")?;
    case.seed_sidecar(&baseline.sidecar)?;
    let first = case.spawn(&[]).await?;
    let ordinary = timeline(first.client(), &case).await?;
    baseline.assert_responses(&ordinary)?;
    let cursor_response = first
        .client()
        .get_json(&events_target(&case, 1, None))
        .await?;
    let cursor = cursor_response["next_cursor"]
        .as_str()
        .context("one-row real-process events page has no cursor")?
        .to_owned();
    first.stop().await?;

    let stable = fingerprint(&case.sidecar())?;
    let restarted = case.spawn(&[]).await?;
    let restarted_ordinary = timeline(restarted.client(), &case).await?;
    baseline.assert_responses(&restarted_ordinary)?;
    let response = restarted
        .client()
        .get(&events_target(&case, 1, Some(&cursor)))
        .await?;
    let problem = response.json_status(410)?;
    ensure!(
        problem["code"] == "cursor_expired",
        "prior-process cursor returned the wrong problem: {problem}"
    );
    ensure!(
        problem["type"] == "https://pgkronika.dev/problems/cursor-expired",
        "prior-process cursor returned the wrong problem type: {problem}"
    );
    assert_durable_zero_pgm(&restarted.client().metrics().await?)?;
    ensure!(
        fingerprint(&case.sidecar())? == stable,
        "cursor restart rewrote the valid OVF"
    );
    case.assert_sources_preserved()?;
    restarted.stop().await
}

/// The directory has one operation owner: a second real process rebuilds
/// honestly into bounded fallback, reports deterministic `busy` contention,
/// and never corrupts the first process's eventual atomic publication.
pub(crate) async fn assert_writer_contention(
    segment: &std::path::Path,
    baseline: &TimelineBaseline,
) -> Result<()> {
    let case = WebCase::from_segment(segment, "writer-contention")?;
    let barrier = PublishBarrier::bind(case.control_path("owner.sock")?)?;
    let environment = barrier.environment();
    let owner = case.spawn(&[environment]).await?;
    let owner_client = owner.client();
    let target = overview_target(&case);
    let owner_request = tokio::spawn(async move { owner_client.get_json(&target).await });
    let lease = barrier.arrive().await?;
    ensure!(
        !case.sidecar().exists(),
        "owner published before the deterministic contention point"
    );

    let contender = case.spawn(&[]).await?;
    let contender_overview = request_overview(contender.client(), &case).await?;
    ensure!(
        normalized(contender_overview) == baseline.overview,
        "contended process returned false or incomplete overview data"
    );
    ensure!(
        !case.sidecar().exists(),
        "contended process replaced the blocked owner's target"
    );
    let contender_metrics = contender.client().metrics().await?;
    ensure!(
        metric(
            &contender_metrics,
            "overview_persist_failures_total",
            &[("reason", "busy")]
        )? >= 1.0,
        "second process did not report deterministic owner contention"
    );
    ensure!(
        metric(
            &contender_metrics,
            "overview_cache_entries",
            &[("class", "publication_fallback")]
        )? > 0.5
            && metric(
                &contender_metrics,
                "overview_cache_entries",
                &[("class", "publication_fallback")]
            )? < 1.5,
        "contended process did not retain its honest bounded fallback"
    );

    lease.release().await?;
    let owner_overview = owner_request.await.context("join owner HTTP request")??;
    ensure!(
        normalized(owner_overview) == baseline.overview,
        "owner process returned a different overview after release"
    );
    ensure!(
        case.admitted_sidecar()? == baseline.sidecar,
        "owner contention produced non-canonical sidecar bytes"
    );
    let stable = fingerprint(&case.sidecar())?;
    contender.stop().await?;
    owner.stop().await?;

    let durable = case.spawn(&[]).await?;
    let overview = request_overview(durable.client(), &case).await?;
    ensure!(
        normalized(overview) == baseline.overview,
        "post-contention durable read changed the overview"
    );
    assert_durable_zero_pgm(&durable.client().metrics().await?)?;
    ensure!(
        fingerprint(&case.sidecar())? == stable,
        "post-contention process rewrote the winner's OVF"
    );
    case.assert_sources_preserved()?;
    durable.stop().await
}

impl TimelineBaseline {
    fn assert_responses(&self, responses: &TimelineResponses) -> Result<()> {
        ensure!(
            normalized(responses.overview.clone()) == self.overview,
            "real-process overview changed across lifecycle state"
        );
        ensure!(
            normalized(responses.events.clone()) == self.events,
            "real-process events changed across lifecycle state"
        );
        ensure!(
            normalized(responses.health.clone()) == self.health,
            "real-process health changed across lifecycle state"
        );
        Ok(())
    }
}

fn assert_timeline_equal(expected: &TimelineResponses, actual: &TimelineResponses) -> Result<()> {
    ensure!(
        normalized(expected.overview.clone()) == normalized(actual.overview.clone()),
        "overview changed across a graceful real-process restart"
    );
    ensure!(
        normalized(expected.events.clone()) == normalized(actual.events.clone()),
        "events changed across a graceful real-process restart"
    );
    ensure!(
        normalized(expected.health.clone()) == normalized(actual.health.clone()),
        "health changed across a graceful real-process restart"
    );
    Ok(())
}

async fn timeline(client: WebClient, case: &WebCase) -> Result<TimelineResponses> {
    // The overview is deliberately the first HTTP request after readiness: it
    // is the request that must build or durably admit the sibling.
    let overview = request_overview(client, case).await?;
    let events = client.get_json(&events_target(case, 10_000, None)).await?;
    let health = client.get_json(&health_target(case)).await?;
    let healthz = client.get("/healthz").await?;
    ensure!(
        healthz.status == 200,
        "/healthz returned {}",
        healthz.status
    );
    let readyz = client.get("/readyz").await?;
    ensure!(readyz.status == 200, "/readyz returned {}", readyz.status);
    let sources = client.get_json("/v1/sources").await?;
    let source_rows = sources["sources"]
        .as_array()
        .context("/v1/sources has no sources array")?;
    ensure!(
        source_rows
            .iter()
            .any(|row| row["source_id"].as_u64() == Some(case.source_id())),
        "/v1/sources omitted the fixture source {}",
        case.source_id()
    );
    assert_same_fact_set(&overview, &events, &health)?;
    Ok(TimelineResponses {
        overview,
        events,
        health,
    })
}

async fn request_overview(client: WebClient, case: &WebCase) -> Result<Value> {
    client.get_json(&overview_target(case)).await
}

fn overview_target(case: &WebCase) -> String {
    format!(
        "/v1/timeline/overview?source={}&from={}&to={}",
        case.source_id(),
        case.range_start_us(),
        case.to_us()
    )
}

fn health_target(case: &WebCase) -> String {
    let step = case
        .to_us()
        .checked_sub(case.range_start_us())
        .expect("lifecycle range is nonempty");
    format!(
        "/v1/timeline/health?source={}&from={}&to={}&step={step}",
        case.source_id(),
        case.range_start_us(),
        case.to_us()
    )
}

fn events_target(case: &WebCase, limit: usize, cursor: Option<&str>) -> String {
    let mut target = format!(
        "/v1/timeline/events?source={}&from={}&to={}&limit={limit}",
        case.source_id(),
        case.range_start_us(),
        case.to_us()
    );
    if let Some(cursor) = cursor {
        target.push_str("&cursor=");
        target.push_str(cursor);
    }
    target
}

fn assert_same_fact_set(overview: &Value, events: &Value, health: &Value) -> Result<()> {
    let fact_set = overview["meta"]["fact_set_id"]
        .as_str()
        .context("overview has no fact_set_id")?;
    ensure!(
        events["meta"]["fact_set_id"] == fact_set,
        "events used a different fact set"
    );
    ensure!(
        health["meta"]["fact_set_id"] == fact_set,
        "health used a different fact set"
    );
    Ok(())
}

fn assert_rebuild_reason(metrics: &str, reason: &str) -> Result<()> {
    ensure!(
        metric(
            metrics,
            "overview_raw_fallback_total",
            &[("reason", reason)]
        )? >= 1.0,
        "real process did not report rebuild reason {reason}"
    );
    ensure!(
        metric(metrics, "kronika_web_overview_rebuilt_total", &[])? >= 1.0,
        "real process did not report a completed rebuild"
    );
    ensure!(
        metric(metrics, "overview_pgm_sections_decoded", &[])? > 0.0,
        "real process rebuilt without recorded PGM section decodes"
    );
    Ok(())
}

fn assert_durable_zero_pgm(metrics: &str) -> Result<()> {
    ensure!(
        metric(metrics, "kronika_web_overview_durable_hits_total", &[])? >= 1.0,
        "restart did not record a durable sidecar hit"
    );
    ensure!(
        metric(metrics, "overview_pgm_sections_decoded", &[])? == 0.0,
        "durable restart decoded a PGM section body"
    );
    ensure!(
        metric(metrics, "overview_pgm_body_read_bytes", &[])? == 0.0,
        "durable restart read PGM section-body bytes"
    );
    ensure!(
        metric(
            metrics,
            "overview_fact_lookup_total",
            &[("layer", "l1"), ("result", "hit"), ("reason", "none")]
        )? >= 1.0,
        "restart did not expose the durable L1 lookup hit"
    );
    Ok(())
}

fn normalized(mut value: Value) -> Value {
    remove_process_tokens(&mut value);
    value
}

fn remove_process_tokens(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("next_cursor");
            for child in object.values_mut() {
                remove_process_tokens(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                remove_process_tokens(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}
