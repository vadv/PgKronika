//! Production lifecycle steps for the one current compact PGM contract.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result, ensure};
use cucumber::{then, when};
use kronika_reader::PgmUnit;
use kronika_registry::Cell;

use crate::BddWorld;
use crate::collector::Collector;
use crate::harness::{pgm_compaction, web_lifecycle};

const INSTANCE_METADATA_TYPE_ID: u32 = 1_021_001;

#[when("the production collector recovers at least two completed windows after an abrupt stop")]
async fn collect_two_windows_and_recover(world: &mut BddWorld) -> Result<()> {
    let cluster = world.harness.cluster()?;
    let mut extra_env = world.harness.collector_env().to_vec();
    extra_env.extend([
        ("KRONIKA_INTERVAL_S".to_owned(), "1".to_owned()),
        ("KRONIKA_PG_DATABASE_INTERVAL_S".to_owned(), "0".to_owned()),
        ("KRONIKA_INSTANCE_INTERVAL_S".to_owned(), "0".to_owned()),
        ("KRONIKA_SEGMENT_MAX_AGE_S".to_owned(), "3600".to_owned()),
    ]);

    let collector = Collector::spawn_with_env(cluster, &extra_env).await?;
    let first = super::scheduler::wait_journal_grows(&collector, 0).await?;
    super::scheduler::wait_journal_grows(&collector, first).await?;
    let journal_path = collector.output_dir()?.join("active.parts");
    let journal_before = fs::read(&journal_path).context("read completed two-window journal")?;
    ensure!(
        !journal_before.is_empty(),
        "completed windows left an empty journal"
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
