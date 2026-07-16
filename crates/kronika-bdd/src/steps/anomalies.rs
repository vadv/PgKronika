//! Detector step glue: timer-driven collector runs sealed as one segment,
//! checked through `/v1/anomalies` (load plateau) and `/v1/section/{name}/diff`
//! (GUC-gated timings).

use anyhow::{Context, Result, ensure};
use cucumber::{then, when};

use crate::BddWorld;
use crate::collector::Collector;
use crate::harness::web;

/// Run the collector on its one-second internal timer with no extra load,
/// then seal one continuous segment.
#[when(regex = r"^the collector ticks for (\d+) seconds and seals the segment$")]
async fn collector_ticks_calm(world: &mut BddWorld, seconds: u64) -> Result<()> {
    let cluster = world.harness.cluster()?;
    let mut extra_env = world.harness.collector_env().to_vec();
    extra_env.push(("KRONIKA_INTERVAL_S".to_owned(), "1".to_owned()));
    extra_env.push(("KRONIKA_PG_DATABASE_INTERVAL_S".to_owned(), "1".to_owned()));
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    let segment = collector.snapshot().await?;
    world.harness.set_collector_log(collector.stderr_captured());
    if let Some(dir) = collector.take_output_dir() {
        world.harness.retain_collector_output_dir(dir);
    }
    world.harness.set_segment(segment);
    Ok(())
}

/// Points of one diff column of one series, from `/v1/section/{name}/diff`.
async fn diff_column_points(
    world: &mut BddWorld,
    section: &str,
    column: &str,
) -> Result<Vec<serde_json::Value>> {
    let segment = world.harness.segment()?.clone();
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    let source = web::only_source(dir).await?;
    let diff = web::section_diff(dir, section, source).await?;
    let series = diff["series"]
        .as_array()
        .context("`series` is not an array")?;
    let with_points = series
        .iter()
        .filter_map(|s| s["columns"][column].as_array())
        .find(|points| points.len() > 1)
        .with_context(|| format!("no series with {column} points; diff: {diff}"))?;
    Ok(with_points.clone())
}

/// Assert every pair of the column (after the honest `FirstPoint`) reads
/// `not_collected`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r"^the web API diffs column (\w+) of section ([\w.+-]+) as not collected throughout$"
)]
async fn web_diffs_column_not_collected(
    world: &mut BddWorld,
    column: String,
    section: String,
) -> Result<()> {
    let points = diff_column_points(world, &section, &column).await?;
    ensure!(
        points[1..]
            .iter()
            .all(|point| point["nodata"] == "not_collected"),
        "every pair of {section}.{column} must read not_collected: {points:?}"
    );
    Ok(())
}

/// Assert the column still carries numeric rates (its gate did not fire).
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^the web API keeps rates for column (\w+) of section ([\w.+-]+)$")]
async fn web_keeps_rates(world: &mut BddWorld, column: String, section: String) -> Result<()> {
    let points = diff_column_points(world, &section, &column).await?;
    ensure!(
        points[1..].iter().all(|point| point["rate"].is_number()),
        "{section}.{column} must keep numeric rates: {points:?}"
    );
    Ok(())
}

/// Run the collector on its one-second internal timer so every snapshot
/// lands in one continuous segment (signal-sealed single-snapshot segments
/// leave coverage gaps between them, and the diff refuses to fold across a
/// gap). A calm phase builds the reference; a plateau of `tps` short
/// implicit transactions per second then holds for `load_seconds`. The
/// plateau length matters: the detector scores the window median, so a
/// single-point spike stays quiet. One SIGUSR2 seals the whole run.
#[when(
    regex = r"^the collector ticks for (\d+) calm seconds, carries (\d+) transactions per second for (\d+) seconds, and seals the segment$"
)]
async fn collector_ticks_with_tail_load(
    world: &mut BddWorld,
    calm_seconds: u64,
    tps: usize,
    load_seconds: usize,
) -> Result<()> {
    let cluster = world.harness.cluster()?;
    let mut extra_env = world.harness.collector_env().to_vec();
    extra_env.push(("KRONIKA_INTERVAL_S".to_owned(), "1".to_owned()));
    // The database source keeps its own cadence; without this the run seals
    // ten-second deltas and the reference never reaches its 20-point gate.
    extra_env.push(("KRONIKA_PG_DATABASE_INTERVAL_S".to_owned(), "1".to_owned()));
    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;

    let dsn = world.harness.database_dsn()?;
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect the load session")?;
    let driver = tokio::spawn(async move {
        let _completion = connection.await;
    });

    tokio::time::sleep(std::time::Duration::from_secs(calm_seconds)).await;
    let committed_before = own_xact_commit(&client).await?;
    // One connection per plateau second: a busy backend parks its counters
    // in pending state (the idle flush is throttled and
    // `pg_stat_force_next_flush` does not break through under load), which
    // would land the whole plateau as one lump after the load ends. A
    // backend exit flushes unconditionally, so per-second sessions give the
    // collector a per-tick rate.
    for _ in 0..load_seconds {
        let second_started = std::time::Instant::now();
        let (loader, loader_connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
            .await
            .context("connect a plateau-second session")?;
        let loader_driver = tokio::spawn(async move {
            let _completion = loader_connection.await;
        });
        for _ in 0..tps {
            loader
                .execute("SELECT 1", &[])
                .await
                .context("run a load transaction")?;
        }
        drop(loader);
        loader_driver.await.context("close the plateau session")?;
        tokio::time::sleep(
            std::time::Duration::from_secs(1).saturating_sub(second_started.elapsed()),
        )
        .await;
    }
    // Flushed pending counters reach the shared stats with a lag; wait until
    // the whole plateau is visible (bounded, no fixed sleep) so the sealed
    // segment carries it and a slow flush fails loudly instead of flaking.
    let expected = i64::try_from(tps * load_seconds).context("load size fits i64")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut committed_after = own_xact_commit(&client).await?;
    while committed_after - committed_before < expected {
        ensure!(
            std::time::Instant::now() < deadline,
            "the load must be visible in pg_stat_database before sealing: \
             xact_commit went {committed_before} -> {committed_after}, expected +{expected}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        committed_after = own_xact_commit(&client).await?;
    }
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let segment = collector.snapshot().await?;
    driver.abort();

    world.harness.set_collector_log(collector.stderr_captured());
    if let Some(dir) = collector.take_output_dir() {
        world.harness.retain_collector_output_dir(dir);
    }
    world.harness.set_segment(segment);
    Ok(())
}

/// The scenario database's own `xact_commit`, read on the load session.
async fn own_xact_commit(client: &tokio_postgres::Client) -> Result<i64> {
    let row = client
        .query_one(
            "SELECT xact_commit FROM pg_stat_database WHERE datname = current_database()",
            &[],
        )
        .await
        .context("read own xact_commit")?;
    Ok(row.get::<_, i64>(0))
}

/// Assert `/v1/anomalies` reports an up episode for the section and column,
/// peaking in the second half of the period.
///
/// The window is a sixth of the period: over a ~30-snapshot run that is
/// four-five points per window, enough for the plateau points to own the
/// window median while the reference keeps its 20-point sufficiency gate.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r"^the web API reports an anomaly episode in section ([\w.+-]+) column (\w+) at the period's end$"
)]
async fn web_reports_anomaly_episode(
    world: &mut BddWorld,
    section: String,
    column: String,
) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let dir = segment
        .parent()
        .context("the sealed segment has no parent directory")?;
    let (source, min_ts, max_ts) = web::source_span(dir).await?;
    let period = max_ts - min_ts;
    // A sixth of the period: four-five points per window over a ~30-snapshot
    // run, so the burst points own the window median while the reference
    // keeps its 20-point sufficiency gate. Milliseconds, not seconds: the
    // whole BDD period is a few seconds long.
    let window_ms = (period / 6 / 1_000).max(1);

    let query = format!("source={source}&from={min_ts}&to={max_ts}&window={window_ms}ms&limit=200");
    let body = web::anomalies(dir, &query).await?;
    let episodes = body["episodes"]
        .as_array()
        .context("`episodes` is not an array")?;

    let found = episodes.iter().find(|episode| {
        episode["section"] == section.as_str() && episode["column"] == column.as_str()
    });
    let Some(episode) = found else {
        // The series itself explains a miss better than the episode list.
        let diff = web::section_diff(dir, &section, source).await?;
        anyhow::bail!("no episode for {section}.{column}; diff: {diff}; body: {body}");
    };
    ensure!(
        episode["direction"] == "up",
        "the burst must point up: {episode}"
    );
    let peak_ts = episode["peak_ts"]
        .as_i64()
        .context("`peak_ts` is not i64")?;
    ensure!(
        peak_ts > min_ts + period / 2,
        "the episode must peak in the period's second half (peak_ts {peak_ts}, \
         period [{min_ts}, {max_ts}]): {episode}"
    );
    Ok(())
}
