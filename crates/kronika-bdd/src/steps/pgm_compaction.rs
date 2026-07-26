//! Production lifecycle steps for the one current compact PGM contract.

use std::collections::BTreeSet;
use std::fs;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use cucumber::{then, when};
use kronika_reader::PgmUnit;
use kronika_registry::Cell;
use tokio::process::Command;
use tokio::time::timeout;

use crate::BddWorld;
use crate::collector::Collector;
use crate::harness::{pgm_compaction, web_lifecycle};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const INSTANCE_METADATA_TYPE_ID: u32 = 1_021_001;

#[when("the production collector recovers two completed windows after an abrupt stop")]
async fn collect_two_windows_and_recover(world: &mut BddWorld) -> Result<()> {
    let cluster = world.harness.cluster()?;
    let mut extra_env = world.harness.collector_env().to_vec();
    extra_env.extend([
        ("KRONIKA_INTERVAL_S".to_owned(), "0".to_owned()),
        ("KRONIKA_SEGMENT_MAX_AGE_S".to_owned(), "3600".to_owned()),
    ]);

    let mut collector = Collector::spawn_with_env(cluster, &extra_env).await?;
    collector.collect_window().await?;
    collector.collect_window().await?;
    let journal_path = collector.output_dir()?.join("active.parts");
    let journal_before = fs::read(&journal_path).context("read completed two-window journal")?;
    ensure!(
        !journal_before.is_empty(),
        "two acknowledged windows left an empty journal"
    );
    let out_dir = collector.kill_abruptly().await?;
    ensure!(
        fs::read(&journal_path).context("read journal after abrupt stop")? == journal_before,
        "abrupt process stop changed acknowledged journal bytes"
    );

    let recovered = Collector::spawn_with_env_in(cluster, &extra_env, out_dir).await?;
    let [segment] = recovered.recovered_seals() else {
        anyhow::bail!(
            "restart announced {} recovered segments, expected exactly one",
            recovered.recovered_seals().len()
        );
    };
    let segment = segment.clone();
    ensure!(
        fs::metadata(&journal_path)
            .context("stat journal after recovered seal")?
            .len()
            == 0,
        "successful recovered seal did not reset active.parts"
    );
    let sealed_bytes = fs::read(&segment).context("read recovered compact PGM")?;
    let first_restart_log = recovered.stderr_captured();
    let out_dir = recovered.stop_gracefully().await?;

    let restarted = Collector::spawn_with_env_in(cluster, &extra_env, out_dir).await?;
    ensure!(
        restarted.recovered_seals().is_empty(),
        "a clean binary restart tried to recover the reset journal again"
    );
    ensure!(
        fs::read(&segment).context("read compact PGM after clean binary restart")? == sealed_bytes,
        "clean collector restart changed the sealed PGM"
    );
    let second_restart_log = restarted.stderr_captured();
    let out_dir = restarted.stop_gracefully().await?;

    world.harness.set_collector_log(format!(
        "{first_restart_log}\n--- clean collector restart ---\n{second_restart_log}"
    ));
    world.harness.retain_collector_output_dir(out_dir);
    world.harness.set_segment(segment);
    Ok(())
}

#[then("the sealed file has the one current compact physical PGM contract")]
fn compact_physical_contract(world: &mut BddWorld) -> Result<()> {
    pgm_compaction::assert_current_compact_pgm(world.harness.segment()?)
}

#[then("both stored windows retain the exact PostgreSQL major through the current reader")]
async fn exact_major_and_two_windows(world: &mut BddWorld) -> Result<()> {
    let dsn = world.harness.database_dsn()?;
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect exact-major oracle")?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let version = client
        .query_one("SHOW server_version_num", &[])
        .await
        .context("read server_version_num")?
        .get::<_, String>(0)
        .parse::<i32>()
        .context("parse server_version_num")?;
    driver.abort();

    let unit = PgmUnit::open(
        fs::File::open(world.harness.segment()?).context("open compact PGM for major oracle")?,
    )
    .context("open current reader for major oracle")?;
    let entry = unit
        .catalog()
        .entries
        .iter()
        .find(|entry| entry.type_id == INSTANCE_METADATA_TYPE_ID)
        .context("compact PGM has no instance_metadata section")?;
    let rows = unit
        .decode_rows(entry)
        .context("decode compact instance_metadata")?;
    let mut timestamps = BTreeSet::new();
    for row in &rows {
        match row.get("pg_version_num") {
            Some(Cell::I32(actual)) => ensure!(
                *actual == version,
                "stored pg_version_num {actual} differs from live oracle {version}"
            ),
            other => anyhow::bail!("stored pg_version_num is {other:?}, expected i32"),
        }
        match row.get("ts") {
            Some(Cell::Ts(ts)) => {
                timestamps.insert(*ts);
            }
            other => anyhow::bail!("stored instance_metadata ts is {other:?}"),
        }
    }
    ensure!(
        timestamps.len() >= 2,
        "compact instance_metadata has {} distinct windows, expected at least two",
        timestamps.len()
    );
    Ok(())
}

#[then(
    "real web processes preserve section diff overview anomaly and incident semantics through OVF restart"
)]
async fn real_web_semantics_and_ovf_restart(world: &mut BddWorld) -> Result<()> {
    let segment = world.harness.segment()?.clone();
    let baseline = web_lifecycle::establish_restart_baseline(&segment).await?;
    world.harness.set_web_lifecycle_baseline(baseline);
    Ok(())
}

#[then("collector and web reject an obsolete internal PGM without changing it")]
async fn obsolete_internal_pgm_is_fail_fast(_world: &mut BddWorld) -> Result<()> {
    let dir = tempfile::tempdir().context("create obsolete-store fixture")?;
    let path = dir.path().join("retained.pgm");
    let bytes = b"PGM1 retained pre-compaction source";
    fs::write(&path, bytes).context("write obsolete-store fixture")?;

    let collector_bin =
        std::env::var("KRONIKA_COLLECTOR_BIN").context("KRONIKA_COLLECTOR_BIN is not set")?;
    let mut collector = Command::new(collector_bin);
    collector
        .env("KRONIKA_PG_DSN", "postgresql://invalid.invalid/unused")
        .env("KRONIKA_OUT_DIR", dir.path())
        .env("KRONIKA_INTERVAL_S", "0")
        .stdin(Stdio::null());
    let collector_stderr = rejected_process(collector, "collector").await?;
    ensure!(
        collector_stderr.contains("pre-compaction")
            || collector_stderr.contains("obsolete")
            || collector_stderr.contains("current PGM contract"),
        "collector rejection did not identify the incompatible PGM: {collector_stderr}"
    );

    let web_bin = std::env::var("KRONIKA_WEB_BIN").context("KRONIKA_WEB_BIN is not set")?;
    let mut web = Command::new(web_bin);
    web.env("KRONIKA_WEB_DIR", dir.path())
        .env("KRONIKA_WEB_ADDR", "127.0.0.1:0")
        .stdin(Stdio::null());
    let web_stderr = rejected_process(web, "web").await?;
    ensure!(
        web_stderr.contains("pre-compaction")
            || web_stderr.contains("obsolete")
            || web_stderr.contains("current PGM contract"),
        "web rejection did not identify the incompatible PGM: {web_stderr}"
    );
    ensure!(
        fs::read(&path).context("reread obsolete PGM")? == bytes,
        "a rejecting binary changed the obsolete PGM bytes"
    );
    Ok(())
}

async fn rejected_process(mut command: Command, label: &str) -> Result<String> {
    let output = timeout(PROCESS_TIMEOUT, command.output())
        .await
        .with_context(|| format!("{label} did not reject the obsolete store in time"))?
        .with_context(|| format!("run {label} against obsolete store"))?;
    ensure!(
        !output.status.success(),
        "{label} accepted an obsolete internal PGM"
    );
    ensure!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "ready" || line.starts_with("pg_kronika-web ready ")),
        "{label} announced readiness for an obsolete internal PGM"
    );
    Ok(String::from_utf8_lossy(&output.stderr).into_owned())
}
