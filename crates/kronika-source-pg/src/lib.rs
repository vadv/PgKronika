//! `PostgreSQL` collectors.
//!
//! Collectors are split by on-disk schema. When `PostgreSQL` changes a catalog
//! shape across major versions, each shape gets its own `type_id` and collector
//! (see the `type_id` rule in the registry README). The caller reads the major
//! version once from the handshake ([`server_major`]) and calls the matching
//! collector: `1_006_001`/`1_006_002` for background-writer stats and
//! `1_020_001`/`1_020_002` for reset context.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

use kronika_registry::{
    Ts,
    bgwriter_checkpointer::{Bgwriter, BgwriterCheckpointer},
    reset_metadata::{ResetMetadata, ResetMetadataIo},
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

/// Collect type `1_006_001` from `pg_stat_bgwriter` on `PostgreSQL` 15–16.
///
/// `ts` is the server's `clock_timestamp()`, taken in the same query. Use this
/// for majors below 17; PG17 split the view, so 17+ uses
/// [`collect_checkpointer`] and a different `type_id`.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_bgwriter(client: &Client) -> Result<Bgwriter, tokio_postgres::Error> {
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
    Ok(Bgwriter {
        ts: Ts(row.get("ts_us")),
        checkpoints_timed: row.get("checkpoints_timed"),
        checkpoints_req: row.get("checkpoints_req"),
        checkpoint_write_time: row.get("checkpoint_write_time"),
        checkpoint_sync_time: row.get("checkpoint_sync_time"),
        buffers_checkpoint: row.get("buffers_checkpoint"),
        buffers_clean: row.get("buffers_clean"),
        maxwritten_clean: row.get("maxwritten_clean"),
        buffers_backend: row.get("buffers_backend"),
        buffers_backend_fsync: row.get("buffers_backend_fsync"),
        buffers_alloc: row.get("buffers_alloc"),
        stats_reset: Ts(row.get("stats_reset_us")),
    })
}

/// Collect type `1_006_002` from `pg_stat_checkpointer` + `pg_stat_bgwriter` on
/// `PostgreSQL` 17+.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_checkpointer(
    client: &Client,
) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!(
                "SELECT c.num_timed, c.num_requested, c.restartpoints_timed, c.restartpoints_req, \
                 c.restartpoints_done, c.write_time, c.sync_time, c.buffers_written, \
                 b.buffers_clean, b.maxwritten_clean, b.buffers_alloc, \
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
        num_timed: row.get("num_timed"),
        num_requested: row.get("num_requested"),
        restartpoints_timed: row.get("restartpoints_timed"),
        restartpoints_req: row.get("restartpoints_req"),
        restartpoints_done: row.get("restartpoints_done"),
        write_time: row.get("write_time"),
        sync_time: row.get("sync_time"),
        buffers_written: row.get("buffers_written"),
        buffers_clean: row.get("buffers_clean"),
        maxwritten_clean: row.get("maxwritten_clean"),
        buffers_alloc: row.get("buffers_alloc"),
        bgwriter_stats_reset: Ts(row.get("bgwriter_reset_us")),
        checkpointer_stats_reset: Ts(row.get("checkpointer_reset_us")),
    })
}

/// `pg_stat_statements_info` (which carries `stats_reset`) exists since the
/// extension version 1.9.
fn pg_stat_statements_has_info(version: Option<&str>) -> bool {
    let Some(version) = version else {
        return false;
    };
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor) >= (1, 9)
}

/// Optional reset sources exposed by this instance.
///
/// Extension presence is install configuration, not a `PostgreSQL` version
/// schema difference, so it gates subqueries inside the reset-context collector
/// instead of selecting a different `type_id`.
async fn detect_reset_extensions(client: &Client) -> Result<(bool, bool), tokio_postgres::Error> {
    let rows = client
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
    for row in &rows {
        let name: String = row.get("extname");
        if name == "pg_stat_statements" {
            pgss_version = Some(row.get("extversion"));
        } else if name == "pg_store_plans" {
            has_store_plans = true;
        }
    }
    Ok((
        pg_stat_statements_has_info(pgss_version.as_deref()),
        has_store_plans,
    ))
}

/// Build the reset-context query for the selected schema.
///
/// `with_io` adds the PG16+ `pg_stat_io` reset. Required reset timestamps
/// coalesce to postmaster start time so a fresh cluster does not produce NULL
/// for non-nullable contract fields.
fn reset_metadata_sql(pgss_has_info: bool, has_store_plans: bool, with_io: bool) -> String {
    let pgss = if pgss_has_info {
        "(SELECT (extract(epoch from stats_reset) * 1e6)::bigint FROM pg_stat_statements_info)"
    } else {
        "NULL::bigint"
    };
    let store_plans = if has_store_plans {
        "(SELECT (extract(epoch from stats_reset) * 1e6)::bigint FROM pg_store_plans_info)"
    } else {
        "NULL::bigint"
    };
    let io = if with_io {
        ", (SELECT (extract(epoch from coalesce(max(stats_reset), pg_postmaster_start_time())) * 1e6)::bigint FROM pg_stat_io) AS io_reset_us"
    } else {
        ""
    };
    [
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/lib.rs */ "
        ),
        "SELECT (extract(epoch from clock_timestamp()) * 1e6)::bigint AS ts_us, \
         (extract(epoch from pg_postmaster_start_time()) * 1e6)::bigint AS postmaster_us, \
         (SELECT (extract(epoch from coalesce(max(stats_reset), pg_postmaster_start_time())) * 1e6)::bigint FROM pg_stat_database) AS db_reset_us, \
         (SELECT (extract(epoch from coalesce(stats_reset, pg_postmaster_start_time())) * 1e6)::bigint FROM pg_stat_wal) AS wal_reset_us, \
         (SELECT (extract(epoch from coalesce(stats_reset, pg_postmaster_start_time())) * 1e6)::bigint FROM pg_stat_archiver) AS archiver_reset_us, ",
        pgss,
        " AS pgss_reset_us, ",
        store_plans,
        " AS store_plans_reset_us",
        io,
    ]
    .concat()
}

/// Collect type `1_020_001` (reset context, `PostgreSQL` 15 — no `pg_stat_io`).
///
/// One row per segment. See the registry README for why the io reset is a
/// separate `type_id` rather than a nullable column.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_reset_metadata(
    client: &Client,
) -> Result<ResetMetadata, tokio_postgres::Error> {
    let (pgss_has_info, has_store_plans) = detect_reset_extensions(client).await?;
    let sql = reset_metadata_sql(pgss_has_info, has_store_plans, false);
    let row = client.query_one(sql.as_str(), &[]).await?;
    Ok(ResetMetadata {
        ts: Ts(row.get("ts_us")),
        postmaster_start_time: Ts(row.get("postmaster_us")),
        pg_stat_database_reset_max_at: Ts(row.get("db_reset_us")),
        pg_stat_wal_reset_at: Ts(row.get("wal_reset_us")),
        pg_stat_archiver_reset_at: Ts(row.get("archiver_reset_us")),
        pg_stat_statements_reset_at: row.get::<_, Option<i64>>("pgss_reset_us").map(Ts),
        pg_store_plans_reset_at: row.get::<_, Option<i64>>("store_plans_reset_us").map(Ts),
    })
}

/// Collect type `1_020_002` (reset context, `PostgreSQL` 16+ — adds the
/// `pg_stat_io` reset). One row per segment.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_reset_metadata_io(
    client: &Client,
) -> Result<ResetMetadataIo, tokio_postgres::Error> {
    let (pgss_has_info, has_store_plans) = detect_reset_extensions(client).await?;
    let sql = reset_metadata_sql(pgss_has_info, has_store_plans, true);
    let row = client.query_one(sql.as_str(), &[]).await?;
    Ok(ResetMetadataIo {
        ts: Ts(row.get("ts_us")),
        postmaster_start_time: Ts(row.get("postmaster_us")),
        pg_stat_database_reset_max_at: Ts(row.get("db_reset_us")),
        pg_stat_wal_reset_at: Ts(row.get("wal_reset_us")),
        pg_stat_archiver_reset_at: Ts(row.get("archiver_reset_us")),
        pg_stat_io_reset_at: Ts(row.get("io_reset_us")),
        pg_stat_statements_reset_at: row.get::<_, Option<i64>>("pgss_reset_us").map(Ts),
        pg_store_plans_reset_at: row.get::<_, Option<i64>>("store_plans_reset_us").map(Ts),
    })
}
