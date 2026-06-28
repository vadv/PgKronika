//! `PostgreSQL` collectors.
//!
//! Type `1_006_001` stores `pg_stat_bgwriter` data plus the checkpoint counters
//! that moved to `pg_stat_checkpointer` in `PostgreSQL` 17.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

use kronika_registry::{
    Ts, bgwriter_checkpointer::BgwriterCheckpointer, reset_metadata::ResetMetadata,
};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule): the
/// statement then shows in `pg_stat_activity` and the server log as kronika, its
/// version, and this source file.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/lib.rs */ ",
            $sql,
        )
    };
}

/// Major version from the `server_version` startup parameter, e.g. `"17.2"` ->
/// `17`.
///
/// The server reports `server_version` in the connection handshake, so the
/// caller reads it once from `Connection::parameter("server_version")` — no
/// query — and it cannot change while the connection lives (a major upgrade
/// restarts the server and drops the connection). Returns `None` if the
/// parameter is absent or has no leading version number.
#[must_use]
pub fn server_major(server_version: Option<&str>) -> Option<u32> {
    let text = server_version?.trim_start();
    let mut digits = String::new();
    for c in text.chars() {
        if !c.is_ascii_digit() {
            break;
        }
        digits.push(c);
    }
    digits.parse().ok()
}

/// Collect type `1_006_001` from a connected server of major version `major`.
///
/// `major` comes from the handshake (see [`server_major`]), so collection makes
/// one query and never asks the server for its version. `PostgreSQL` 17 split
/// the checkpoint counters into `pg_stat_checkpointer`; 17+ reads them there,
/// older versions from `pg_stat_bgwriter`. `ts` is the server's
/// `clock_timestamp()`, taken in the same query as the counters.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_bgwriter_checkpointer(
    client: &Client,
    major: u32,
) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    // PG17 is the only catalog boundary this type cares about.
    if major >= 17 {
        collect_pg17(client).await
    } else {
        collect_pre17(client).await
    }
}

/// `pg_stat_statements_info.stats_reset` is available in `pg_stat_statements`
/// 1.9 and newer.
fn pg_stat_statements_has_info(version: Option<&str>) -> bool {
    let Some(version) = version else {
        return false;
    };
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor) >= (1, 9)
}

/// Collect one `1_020_001` `reset_metadata` row for the segment being sealed.
///
/// The first query reads installed extension versions. The second query is
/// assembled from the views that exist on this server. `PostgreSQL` resolves
/// relation names before `CASE` branches run, so unsupported views must be left
/// out of the SQL text.
///
/// Dictionary-backed string fields are not written yet: extension versions and
/// `compute_query_id` stay `None` until dictionary support is wired into the
/// writer path.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_reset_metadata(
    client: &Client,
    major: u32,
) -> Result<ResetMetadata, tokio_postgres::Error> {
    let ext_rows = client
        .query(
            marked!(
                "SELECT extname, extversion FROM pg_extension \
                 WHERE extname IN ('pg_stat_statements', 'pg_store_plans')"
            ),
            &[],
        )
        .await?;
    let mut pgss_version: Option<String> = None;
    let mut has_store_plans = false;
    for row in &ext_rows {
        let name: String = row.get("extname");
        if name == "pg_stat_statements" {
            pgss_version = Some(row.get("extversion"));
        } else if name == "pg_store_plans" {
            has_store_plans = true;
        }
    }

    // Keep unsupported views out of the SQL text.
    let io = if major >= 16 {
        "(SELECT (extract(epoch from max(stats_reset)) * 1e6)::bigint FROM pg_stat_io)"
    } else {
        "NULL::bigint"
    };
    let pgss = if pg_stat_statements_has_info(pgss_version.as_deref()) {
        "(SELECT (extract(epoch from stats_reset) * 1e6)::bigint FROM pg_stat_statements_info)"
    } else {
        "NULL::bigint"
    };
    let store_plans = if has_store_plans {
        "(SELECT (extract(epoch from stats_reset) * 1e6)::bigint FROM pg_store_plans_info)"
    } else {
        "NULL::bigint"
    };
    let sql = [
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/lib.rs */ "
        ),
        // Required reset timestamps use postmaster start time when PostgreSQL
        // reports NULL because the stats have never been reset.
        "SELECT (extract(epoch from clock_timestamp()) * 1e6)::bigint AS ts_us, \
         (extract(epoch from pg_postmaster_start_time()) * 1e6)::bigint AS postmaster_us, \
         (SELECT (extract(epoch from coalesce(max(stats_reset), pg_postmaster_start_time())) * 1e6)::bigint FROM pg_stat_database) AS db_reset_us, \
         (SELECT (extract(epoch from stats_reset) * 1e6)::bigint FROM pg_stat_wal) AS wal_reset_us, \
         (SELECT (extract(epoch from coalesce(stats_reset, pg_postmaster_start_time())) * 1e6)::bigint FROM pg_stat_archiver) AS archiver_reset_us, ",
        io,
        " AS io_reset_us, ",
        pgss,
        " AS pgss_reset_us, ",
        store_plans,
        " AS store_plans_reset_us, \
         (current_setting('track_io_timing', true) = 'on') AS track_io_timing, \
         (current_setting('track_wal_io_timing', true) = 'on') AS track_wal_io_timing",
    ]
    .concat();
    let row = client.query_one(sql.as_str(), &[]).await?;

    Ok(ResetMetadata {
        ts: Ts(row.get("ts_us")),
        postmaster_start_time: Ts(row.get("postmaster_us")),
        pg_stat_database_reset_max_at: Ts(row.get("db_reset_us")),
        pg_stat_statements_reset_at: row.get::<_, Option<i64>>("pgss_reset_us").map(Ts),
        pg_store_plans_reset_at: row.get::<_, Option<i64>>("store_plans_reset_us").map(Ts),
        pg_stat_wal_reset_at: row.get::<_, Option<i64>>("wal_reset_us").map(Ts),
        pg_stat_archiver_reset_at: Ts(row.get("archiver_reset_us")),
        pg_stat_io_reset_at: row.get::<_, Option<i64>>("io_reset_us").map(Ts),
        // Filled after dictionary-backed string fields are available here.
        ext_pg_stat_statements_version: None,
        ext_pg_store_plans_version: None,
        compute_query_id: None,
        track_io_timing: row.get("track_io_timing"),
        track_wal_io_timing: row.get("track_wal_io_timing"),
    })
}

/// `PostgreSQL` 16 and earlier: all counters come from `pg_stat_bgwriter`.
async fn collect_pre17(client: &Client) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!(
                "SELECT checkpoints_timed, checkpoints_req, checkpoint_write_time, \
                 checkpoint_sync_time, buffers_checkpoint, buffers_clean, maxwritten_clean, \
                 buffers_backend, buffers_backend_fsync, buffers_alloc, \
                 (extract(epoch from stats_reset) * 1e6)::bigint AS stats_reset_us, \
                 (extract(epoch from clock_timestamp()) * 1e6)::bigint AS ts_us \
                 FROM pg_stat_bgwriter"
            ),
            &[],
        )
        .await?;
    Ok(BgwriterCheckpointer {
        ts: Ts(row.get("ts_us")),
        checkpoints_timed: row.get("checkpoints_timed"),
        checkpoints_req: row.get("checkpoints_req"),
        checkpoint_write_time: row.get("checkpoint_write_time"),
        checkpoint_sync_time: row.get("checkpoint_sync_time"),
        buffers_checkpoint: row.get("buffers_checkpoint"),
        restartpoints_timed: None,
        restartpoints_req: None,
        restartpoints_done: None,
        buffers_clean: row.get("buffers_clean"),
        maxwritten_clean: row.get("maxwritten_clean"),
        buffers_backend: Some(row.get("buffers_backend")),
        buffers_backend_fsync: Some(row.get("buffers_backend_fsync")),
        buffers_alloc: row.get("buffers_alloc"),
        bgwriter_stats_reset: Ts(row.get("stats_reset_us")),
        checkpointer_stats_reset: None,
    })
}

/// `PostgreSQL` 17+: checkpoint counters moved to `pg_stat_checkpointer`.
async fn collect_pg17(client: &Client) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!(
                "SELECT c.num_timed, c.num_requested, c.write_time, c.sync_time, \
                 c.buffers_written, c.restartpoints_timed, c.restartpoints_req, \
                 c.restartpoints_done, b.buffers_clean, b.maxwritten_clean, b.buffers_alloc, \
                 (extract(epoch from b.stats_reset) * 1e6)::bigint AS bgwriter_reset_us, \
                 (extract(epoch from c.stats_reset) * 1e6)::bigint AS checkpointer_reset_us, \
                 (extract(epoch from clock_timestamp()) * 1e6)::bigint AS ts_us \
                 FROM pg_stat_bgwriter b, pg_stat_checkpointer c"
            ),
            &[],
        )
        .await?;
    Ok(BgwriterCheckpointer {
        ts: Ts(row.get("ts_us")),
        checkpoints_timed: row.get("num_timed"),
        checkpoints_req: row.get("num_requested"),
        checkpoint_write_time: row.get("write_time"),
        checkpoint_sync_time: row.get("sync_time"),
        buffers_checkpoint: row.get("buffers_written"),
        restartpoints_timed: Some(row.get("restartpoints_timed")),
        restartpoints_req: Some(row.get("restartpoints_req")),
        restartpoints_done: Some(row.get("restartpoints_done")),
        buffers_clean: row.get("buffers_clean"),
        maxwritten_clean: row.get("maxwritten_clean"),
        buffers_backend: None,
        buffers_backend_fsync: None,
        buffers_alloc: row.get("buffers_alloc"),
        bgwriter_stats_reset: Ts(row.get("bgwriter_reset_us")),
        checkpointer_stats_reset: Some(Ts(row.get("checkpointer_reset_us"))),
    })
}
