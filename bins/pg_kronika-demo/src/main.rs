//! Demo stand for PGM/OVF size evaluation.
//!
//! Boots a throwaway `PostgreSQL` 17 cluster, seeds a saturated OLTP profile,
//! runs `pg_kronika-collector` (and the web viewer when available) against it
//! under synthetic load, then builds `.ovf` fact files from the sealed
//! segments and reports size breakdowns.
//!
//! Subcommands:
//!
//! - `stand` — the full in-container run: boot, seed, load, collect, measure.
//!   With `DEMO_DURATION_MIN=0` the stand serves until `SIGTERM`/`SIGINT`
//!   (the `docker stop` path) and measures during shutdown.
//! - `load` — seed and drive load against an external cluster (`KRONIKA_PG_DSN`).
//! - `measure` — rebuild `.ovf` files and print the size report for an
//!   existing segments directory.
//! - `clean` — remove everything under the stand root (the data volume is
//!   owned by the container user, so the host cannot).
//! - `self-check` — verify the `PostgreSQL` runtime this stand needs.
//!
//! Tunables are read from `DEMO_*` environment variables; see [`config`].

#![allow(
    clippy::multiple_crate_versions,
    reason = "kronika-reader pulls duplicate transitive versions outside our control"
)]

mod cluster;
mod collector;
mod config;
mod load;
mod measure;
mod schema;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the tokio runtime")?;
    let command = std::env::args().nth(1).unwrap_or_default();
    match command.as_str() {
        "stand" => runtime.block_on(run_stand()),
        "load" => runtime.block_on(run_load_only()),
        "measure" => run_measure(),
        "clean" => run_clean(),
        "self-check" => cluster::self_check(),
        other => {
            bail!(
                "unknown subcommand {other:?}; expected stand, load, measure, clean, or self-check"
            )
        }
    }
}

/// Removes everything under the stand root; the mount point itself stays.
fn run_clean() -> Result<()> {
    let config = config::Config::from_env()?;
    let entries = std::fs::read_dir(&config.root)
        .with_context(|| format!("list {}", config.root.display()))?;
    for entry in entries {
        let path = entry.context("read a stand root entry")?.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.with_context(|| format!("remove {}", path.display()))?;
    }
    println!("clean: stand root emptied");
    Ok(())
}

/// The full stand: cluster, seed, collector, web, load, then measurement.
async fn run_stand() -> Result<()> {
    let config = config::Config::from_env()?;
    let paths = config::StandPaths::under(&config.root);
    paths.create()?;
    // Handlers must exist before boot/seed: PID 1 without them ignores the
    // docker-stop SIGTERM for the whole startup phase.
    let stop = stop_signal(&config)?;

    let cluster = cluster::Cluster::boot(&config, &paths).await?;
    let client = cluster.connect().await?;
    schema::seed(client.client(), &config).await?;

    let dsn = cluster.dsn().to_owned();
    let collector = collector::Collector::spawn(&dsn, &paths, &config)?;
    let web = collector::spawn_web(&paths, &config)?;
    if config.duration.is_zero() {
        println!("stand: ready, serving until SIGTERM (docker stop)");
    } else {
        println!(
            "stand: ready, load for {} min",
            config.duration.as_secs() / 60
        );
    }
    load::run(&dsn, &config, stop).await?;

    println!("stand: shutting down, sealing segments");
    collector.stop().await?;
    collector::seal_tail(&dsn, &paths, &config).await?;
    drop(web);
    drop(client);
    cluster.stop().await;

    let report = measure::measure(&paths.segments, &paths.ovf_cache, config.chart_series)?;
    let rendered = measure::render(&report);
    println!("{rendered}");
    let json =
        serde_json::to_string_pretty(&measure::to_json(&report)).context("encode report.json")?;
    std::fs::write(&paths.report, &json)
        .with_context(|| format!("write {}", paths.report.display()))?;
    println!("stand: report written to {}", paths.report.display());
    Ok(())
}

/// A future that resolves when the load phase must end: after the configured
/// duration, or on `SIGTERM`/`SIGINT` when the stand serves indefinitely.
/// Termination signals also cut a bounded run short.
///
/// Signal handlers are installed here, synchronously, so a signal arriving
/// before the future is polled is still delivered.
fn stop_signal(config: &config::Config) -> Result<impl Future<Output = ()> + Send + use<>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).context("install the SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install the SIGINT handler")?;
    let duration = config.duration;
    Ok(async move {
        let wait_duration = async {
            if duration.is_zero() {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(duration).await;
            }
        };
        tokio::select! {
            () = wait_duration => (),
            _ = sigterm.recv() => (),
            _ = sigint.recv() => (),
        }
    })
}

/// Seed and load an external cluster; no collector, no measurement.
async fn run_load_only() -> Result<()> {
    let config = config::Config::from_env()?;
    let stop = stop_signal(&config)?;
    let dsn = std::env::var("KRONIKA_PG_DSN").context("KRONIKA_PG_DSN is not set")?;
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect for seeding")?;
    let driver = tokio::spawn(connection);
    schema::seed(&client, &config).await?;
    drop(client);
    driver.abort();
    load::run(&dsn, &config, stop).await
}

/// Rebuild `.ovf` files for an existing segments directory and print sizes.
fn run_measure() -> Result<()> {
    let config = config::Config::from_env()?;
    let paths = config::StandPaths::under(&config.root);
    let report = measure::measure(&paths.segments, &paths.ovf_cache, config.chart_series)?;
    let rendered = measure::render(&report);
    println!("{rendered}");
    Ok(())
}
