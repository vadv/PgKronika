//! Reset and interpretation context for section `1_020_001`.
//!
//! The base read runs on the main connection: postmaster start time, the
//! per-view `stats_reset` timestamps that exist on this server major, and the
//! GUCs that change how counter and timing columns must be read. The
//! `pg_stat_statements` and `pg_store_plans` reset timestamps live in
//! extension-owned info views, present only in the database where the
//! extension is installed, so the caller reads them through the same
//! discovered connections it collects those sections from.

use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the collector marker.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/reset_metadata.rs */ ",
            $sql,
        )
    };
}

/// The main-connection part of `reset_metadata`, before extension info views.
#[derive(Debug, Clone)]
pub struct ResetBase {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// Postmaster start time, unix microseconds.
    pub postmaster_start_time: i64,
    /// Max `stats_reset` across `pg_stat_database`; `None` until any
    /// database-level reset happened.
    pub pg_stat_database_reset_max_at: Option<i64>,
    /// `pg_stat_bgwriter.stats_reset`.
    pub pg_stat_bgwriter_reset_at: Option<i64>,
    /// `pg_stat_checkpointer.stats_reset`; `None` before PG17.
    pub pg_stat_checkpointer_reset_at: Option<i64>,
    /// `pg_stat_wal.stats_reset`; `None` before PG14.
    pub pg_stat_wal_reset_at: Option<i64>,
    /// `pg_stat_archiver.stats_reset`.
    pub pg_stat_archiver_reset_at: Option<i64>,
    /// Max `stats_reset` across `pg_stat_io`; `None` before PG16.
    pub pg_stat_io_reset_at: Option<i64>,
    /// `compute_query_id` GUC as the server shows it; `None` before PG14.
    pub compute_query_id: Option<String>,
    /// `track_io_timing` in this collector session.
    pub track_io_timing: Option<bool>,
    /// `track_wal_io_timing` in this collector session; `None` before PG14.
    pub track_wal_io_timing: Option<bool>,
}

/// Collect the main-connection part of `reset_metadata`.
///
/// Version-gated views (`pg_stat_wal`, `pg_stat_io`, `pg_stat_checkpointer`)
/// are read only on majors that have them; their fields stay `None` elsewhere.
///
/// # Errors
/// Returns the first [`tokio_postgres::Error`] from the underlying queries.
pub async fn collect_reset_base(
    client: &Client,
    major: u32,
) -> Result<ResetBase, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!(
                "SELECT \
                     (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                     (extract(epoch from pg_postmaster_start_time()) * 1e6)::int8 \
                         AS postmaster_start_us, \
                     (SELECT (extract(epoch from max(stats_reset)) * 1e6)::int8 \
                        FROM pg_stat_database) AS db_reset_max_us, \
                     (SELECT (extract(epoch from stats_reset) * 1e6)::int8 \
                        FROM pg_stat_bgwriter) AS bgwriter_reset_us, \
                     (SELECT (extract(epoch from stats_reset) * 1e6)::int8 \
                        FROM pg_stat_archiver) AS archiver_reset_us, \
                     current_setting('compute_query_id', true) AS compute_query_id, \
                     current_setting('track_io_timing', true) AS track_io_timing, \
                     current_setting('track_wal_io_timing', true) AS track_wal_io_timing"
            ),
            &[],
        )
        .await?;

    let checkpointer = if major >= 17 {
        scalar_reset_us(
            client,
            marked!(
                "SELECT (extract(epoch from stats_reset) * 1e6)::int8 AS reset_us \
                 FROM pg_stat_checkpointer"
            ),
        )
        .await?
    } else {
        None
    };
    let wal = if major >= 14 {
        scalar_reset_us(
            client,
            marked!(
                "SELECT (extract(epoch from stats_reset) * 1e6)::int8 AS reset_us \
                 FROM pg_stat_wal"
            ),
        )
        .await?
    } else {
        None
    };
    let io = if major >= 16 {
        scalar_reset_us(
            client,
            marked!(
                "SELECT (extract(epoch from max(stats_reset)) * 1e6)::int8 AS reset_us \
                 FROM pg_stat_io"
            ),
        )
        .await?
    } else {
        None
    };

    Ok(ResetBase {
        ts: row.get("ts_us"),
        postmaster_start_time: row.get("postmaster_start_us"),
        pg_stat_database_reset_max_at: row.get("db_reset_max_us"),
        pg_stat_bgwriter_reset_at: row.get("bgwriter_reset_us"),
        pg_stat_checkpointer_reset_at: checkpointer,
        pg_stat_wal_reset_at: wal,
        pg_stat_archiver_reset_at: row.get("archiver_reset_us"),
        pg_stat_io_reset_at: io,
        compute_query_id: row.get("compute_query_id"),
        track_io_timing: row
            .get::<_, Option<String>>("track_io_timing")
            .as_deref()
            .and_then(parse_bool_guc),
        track_wal_io_timing: row
            .get::<_, Option<String>>("track_wal_io_timing")
            .as_deref()
            .and_then(parse_bool_guc),
    })
}

/// Read a single optional microsecond timestamp; absent rows collapse to `None`.
async fn scalar_reset_us(client: &Client, sql: &str) -> Result<Option<i64>, tokio_postgres::Error> {
    let row = client.query_opt(sql, &[]).await?;
    Ok(row.and_then(|r| r.get("reset_us")))
}

/// `pg_stat_statements_info.stats_reset`, or `None` when the info view does
/// not exist on this connection (extension absent or older than 1.9).
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] when the probe or the read fails,
/// e.g. without SELECT privilege on the view.
pub async fn statements_reset_at(client: &Client) -> Result<Option<i64>, tokio_postgres::Error> {
    info_view_reset_at(
        client,
        marked!("SELECT to_regclass('pg_stat_statements_info') IS NOT NULL AS present"),
        marked!(
            "SELECT (extract(epoch from stats_reset) * 1e6)::int8 AS reset_us \
             FROM pg_stat_statements_info"
        ),
    )
    .await
}

/// `pg_store_plans_info.stats_reset`, or `None` when the info view does not
/// exist on this connection: the vadv fork does not ship it, and neither does
/// a database without the extension.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] when the probe or the read fails.
pub async fn store_plans_reset_at(client: &Client) -> Result<Option<i64>, tokio_postgres::Error> {
    info_view_reset_at(
        client,
        marked!("SELECT to_regclass('pg_store_plans_info') IS NOT NULL AS present"),
        marked!(
            "SELECT (extract(epoch from stats_reset) * 1e6)::int8 AS reset_us \
             FROM pg_store_plans_info"
        ),
    )
    .await
}

/// Probe an extension info view by name, then read its reset timestamp.
async fn info_view_reset_at(
    client: &Client,
    probe_sql: &str,
    read_sql: &str,
) -> Result<Option<i64>, tokio_postgres::Error> {
    let probe = client.query_one(probe_sql, &[]).await?;
    if !probe.get::<_, bool>("present") {
        return Ok(None);
    }
    scalar_reset_us(client, read_sql).await
}

/// Map a boolean GUC as `current_setting` renders it; anything else is `None`.
fn parse_bool_guc(value: &str) -> Option<bool> {
    match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Extension context resolved by the caller from its discovered sources.
///
/// A `None` version means this snapshot is not reading that extension: absent,
/// unreadable, or an unrecognized fork. It does not mean "not installed
/// anywhere".
#[derive(Debug, Clone, Default)]
pub struct ResetExtensions {
    /// `pg_stat_statements` extension version on the statements source.
    pub statements_version: Option<String>,
    /// `pg_stat_statements_info.stats_reset`, unix microseconds.
    pub statements_reset_at: Option<i64>,
    /// `pg_store_plans` extension version on the plans source.
    pub store_plans_version: Option<String>,
    /// `pg_store_plans_info.stats_reset`, unix microseconds.
    pub store_plans_reset_at: Option<i64>,
}

/// Assemble the registry row, interning the label strings.
///
/// # Errors
/// Propagates the interner error when the dictionary is full.
pub fn to_reset_metadata<E>(
    base: &ResetBase,
    ext: &ResetExtensions,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<ResetMetadata, E> {
    let mut intern_opt = |value: &Option<String>| -> Result<Option<StrId>, E> {
        value.as_deref().map(|s| intern(s.as_bytes())).transpose()
    };
    Ok(ResetMetadata {
        ts: Ts(base.ts),
        postmaster_start_time: Ts(base.postmaster_start_time),
        pg_stat_database_reset_max_at: base.pg_stat_database_reset_max_at.map(Ts),
        pg_stat_statements_reset_at: ext.statements_reset_at.map(Ts),
        pg_store_plans_reset_at: ext.store_plans_reset_at.map(Ts),
        pg_stat_bgwriter_reset_at: base.pg_stat_bgwriter_reset_at.map(Ts),
        pg_stat_checkpointer_reset_at: base.pg_stat_checkpointer_reset_at.map(Ts),
        pg_stat_wal_reset_at: base.pg_stat_wal_reset_at.map(Ts),
        pg_stat_archiver_reset_at: base.pg_stat_archiver_reset_at.map(Ts),
        pg_stat_io_reset_at: base.pg_stat_io_reset_at.map(Ts),
        ext_pg_stat_statements_version: intern_opt(&ext.statements_version)?,
        ext_pg_store_plans_version: intern_opt(&ext.store_plans_version)?,
        compute_query_id: intern_opt(&base.compute_query_id)?,
        track_io_timing: base.track_io_timing,
        track_wal_io_timing: base.track_wal_io_timing,
    })
}

#[cfg(test)]
mod tests {
    use super::{ResetBase, ResetExtensions, parse_bool_guc, to_reset_metadata};
    use kronika_registry::{StrId, Ts};

    #[test]
    fn parse_bool_guc_maps_only_the_server_renderings() {
        assert_eq!(parse_bool_guc("on"), Some(true));
        assert_eq!(parse_bool_guc("off"), Some(false));
        assert_eq!(parse_bool_guc("true"), None);
        assert_eq!(parse_bool_guc(""), None);
    }

    fn base() -> ResetBase {
        ResetBase {
            ts: 2_000_000,
            postmaster_start_time: 1_700_000_000_000_000,
            pg_stat_database_reset_max_at: None,
            pg_stat_bgwriter_reset_at: Some(1_700_000_000_000_001),
            pg_stat_checkpointer_reset_at: None,
            pg_stat_wal_reset_at: Some(1_700_000_000_000_002),
            pg_stat_archiver_reset_at: Some(1_700_000_000_000_003),
            pg_stat_io_reset_at: None,
            compute_query_id: Some("auto".to_owned()),
            track_io_timing: Some(true),
            track_wal_io_timing: None,
        }
    }

    #[test]
    fn to_reset_metadata_interns_labels_and_keeps_nulls() {
        let ext = ResetExtensions {
            statements_version: Some("1.12".to_owned()),
            statements_reset_at: Some(1_700_000_000_000_004),
            store_plans_version: None,
            store_plans_reset_at: None,
        };
        let mut next = 0_u64;
        let row = to_reset_metadata::<()>(&base(), &ext, |_| {
            next += 1;
            Ok(StrId(next))
        })
        .expect("interner never fails here");
        assert_eq!(row.ts, Ts(2_000_000));
        assert_eq!(row.pg_stat_database_reset_max_at, None);
        assert_eq!(
            row.pg_stat_statements_reset_at,
            Some(Ts(1_700_000_000_000_004))
        );
        assert_eq!(row.pg_store_plans_reset_at, None);
        assert_eq!(row.ext_pg_stat_statements_version, Some(StrId(1)));
        assert_eq!(row.ext_pg_store_plans_version, None);
        assert_eq!(row.compute_query_id, Some(StrId(2)));
        assert_eq!(row.track_io_timing, Some(true));
        assert_eq!(row.track_wal_io_timing, None);
    }
}
