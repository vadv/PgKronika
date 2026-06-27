//! `PostgreSQL` collectors.
//!
//! Type `1_006_001` stores `pg_stat_bgwriter` data plus the checkpoint counters
//! that moved to `pg_stat_checkpointer` in `PostgreSQL` 17.
#![allow(
    clippy::multiple_crate_versions,
    reason = "tokio-postgres and the registry's arrow/parquet stack pull duplicate transitive versions outside our control"
)]

use kronika_registry::{Ts, bgwriter_checkpointer::BgwriterCheckpointer};
use tokio_postgres::Client;

const PG17: i32 = 170_000;

/// Collect type `1_006_001` from a connected server, stamping the row with `ts`.
///
/// `PostgreSQL` 17+ reads checkpoint counters from `pg_stat_checkpointer`.
/// Earlier versions read them from `pg_stat_bgwriter`; columns they do not
/// expose are returned as `None`.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn collect_bgwriter_checkpointer(
    client: &Client,
    ts: Ts,
) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    let version_num: i32 = client
        .query_one("SELECT current_setting('server_version_num')::int", &[])
        .await?
        .get(0);
    if version_num >= PG17 {
        collect_pg17(client, ts).await
    } else {
        collect_pre17(client, ts).await
    }
}

/// `PostgreSQL` 16 and earlier: all counters come from `pg_stat_bgwriter`.
async fn collect_pre17(
    client: &Client,
    ts: Ts,
) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT checkpoints_timed, checkpoints_req, checkpoint_write_time, \
             checkpoint_sync_time, buffers_checkpoint, buffers_clean, maxwritten_clean, \
             buffers_backend, buffers_backend_fsync, buffers_alloc, \
             (extract(epoch from stats_reset) * 1e6)::bigint AS stats_reset_us \
             FROM pg_stat_bgwriter",
            &[],
        )
        .await?;
    Ok(BgwriterCheckpointer {
        ts,
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
async fn collect_pg17(
    client: &Client,
    ts: Ts,
) -> Result<BgwriterCheckpointer, tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT c.num_timed, c.num_requested, c.write_time, c.sync_time, \
             c.buffers_written, c.restartpoints_timed, c.restartpoints_req, \
             c.restartpoints_done, b.buffers_clean, b.maxwritten_clean, b.buffers_alloc, \
             (extract(epoch from b.stats_reset) * 1e6)::bigint AS bgwriter_reset_us, \
             (extract(epoch from c.stats_reset) * 1e6)::bigint AS checkpointer_reset_us \
             FROM pg_stat_bgwriter b, pg_stat_checkpointer c",
            &[],
        )
        .await?;
    Ok(BgwriterCheckpointer {
        ts,
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
