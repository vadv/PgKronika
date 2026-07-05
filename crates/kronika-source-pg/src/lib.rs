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

macro_rules! pg_row_mapper {
    (
        $cols:ident($version:ident : $version_ty:ty) => $row_ty:ident {
            $(
                $field:ident : $ty:ty = $column:tt $(if $condition:expr)?
            ),+ $(,)?
        }
    ) => {
        #[derive(Debug)]
        #[allow(
            dead_code,
            reason = "some generated fields are read only by generated row decoding"
        )]
        struct $cols {
            $(
                $field: pg_row_mapper!(@col_ty $ty $(, $condition)?),
            )+
        }

        impl $cols {
            #[allow(
                dead_code,
                reason = "tests exercise new_from_names; collectors call new"
            )]
            fn new(
                $version: $version_ty,
                columns: &[tokio_postgres::Column],
            ) -> Result<Self, crate::PgRowError> {
                Self::new_from_names($version, columns.iter().map(tokio_postgres::Column::name))
            }

            fn new_from_names<I, S>(
                $version: $version_ty,
                column_names: I,
            ) -> Result<Self, crate::PgRowError>
            where
                I: IntoIterator<Item = S>,
                S: AsRef<str>,
            {
                let column_names: Vec<S> = column_names.into_iter().collect();
                Ok(Self {
                    $(
                        $field: pg_row_mapper!(
                            @init
                            $version,
                            &column_names,
                            stringify!($row_ty),
                            stringify!($field),
                            $column,
                            $ty
                            $(, $condition)?
                        )?,
                    )+
                })
            }

            #[allow(
                dead_code,
                reason = "used after collector row mappings are converted"
            )]
            fn read(&self, row: &tokio_postgres::Row) -> Result<$row_ty, crate::PgRowError> {
                Ok($row_ty {
                    $(
                        $field: pg_row_mapper!(@read self, row, $field $(, $condition)?),
                    )+
                })
            }
        }
    };
    (@col_ty $ty:ty) => {
        crate::pg_row::PgCol<$ty>
    };
    (@col_ty $ty:ty, $condition:expr) => {
        Option<crate::pg_row::PgCol<$ty>>
    };
    (@column $version:ident, $column:literal) => {
        $column
    };
    (@column $version:ident, { $column:expr }) => {
        $column
    };
    (@init $version:ident, $columns:expr, $row:expr, $field:expr, $column:tt, $ty:ty) => {{
        let column = pg_row_mapper!(@column $version, $column);
        crate::pg_row::PgCol::<$ty>::required(
            $row,
            $field,
            column,
            ($columns).iter().map(AsRef::as_ref),
        )
    }};
    (@init $version:ident, $columns:expr, $row:expr, $field:expr, $column:tt, $ty:ty, $condition:expr) => {{
        if $condition {
            let column = pg_row_mapper!(@column $version, $column);
            crate::pg_row::PgCol::<$ty>::required(
                $row,
                $field,
                column,
                ($columns).iter().map(AsRef::as_ref),
            )
                .map(Some)
        } else {
            Ok(None)
        }
    }};
    (@read $self:ident, $row:ident, $field:ident) => {
        $self.$field.get($row)?
    };
    (@read $self:ident, $row:ident, $field:ident, $condition:expr) => {
        match &$self.$field {
            Some(col) => col.get($row)?,
            None => None,
        }
    };
}

mod pg_row;
pub use pg_row::{PgCollectError, PgRowError};

mod activity;
pub use activity::{
    ActivityRow, ActivityVersion, activity_query, activity_version, collect_activity, to_v1, to_v2,
    to_v3,
};

pub mod archiver;
pub mod database;
pub mod instance_metadata;
pub mod io;
pub mod locks;

pub mod pool;
pub mod wal;

pub mod prepared_xacts;

pub mod progress_vacuum;

pub mod replication_details;
pub mod replication_instance;
pub mod reset_metadata;
pub mod settings;

pub mod statements;
pub mod store_plans;

pub mod user_indexes;
pub mod user_tables;

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

/// The snapshot timestamp: server time in unix microseconds.
///
/// The segment file is named after this value, so it comes from one tiny
/// query instead of piggybacking on whichever section happens to be due.
///
/// # Errors
/// Returns the underlying [`tokio_postgres::Error`] if the server cannot be
/// queried.
pub async fn snapshot_ts(client: &Client) -> Result<Ts, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!("SELECT (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us"),
            &[],
        )
        .await?;
    Ok(Ts(row.get("ts_us")))
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
