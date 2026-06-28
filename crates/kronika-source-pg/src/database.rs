//! `pg_stat_database` collection for types `1_005_001`..`1_005_004`.
//!
//! In PG 10-18 the `pg_stat_database` column set only grows: checksum columns
//! arrive in PG12, session statistics in PG14, and parallel-worker counters in
//! PG18. The major version selects both the SQL and the layout. Collection
//! returns owned rows; the caller interns `datname` into the segment dictionary.
//! The typed layout lives in `kronika-registry` (`PgStatDatabaseV1`..`V4`).

use kronika_registry::pg_stat_database::{
    PgStatDatabaseV1, PgStatDatabaseV2, PgStatDatabaseV3, PgStatDatabaseV4,
};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/database.rs */ ",
            $sql,
        )
    };
}

/// The `pg_stat_database` layout selected by the server major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseVersion {
    /// PG 10-11: type `1_005_001` (base layout).
    V1,
    /// PG 12-13: type `1_005_002` (adds checksum columns).
    V2,
    /// PG 14-17: type `1_005_003` (adds session statistics).
    V3,
    /// PG 18: type `1_005_004` (adds parallel-worker counters).
    V4,
}

/// Select the layout for a server major version.
///
/// Checksum columns arrived in PG12, session statistics in PG14, parallel-worker
/// counters in PG18.
#[must_use]
pub const fn database_version(major: u32) -> DatabaseVersion {
    if major >= 18 {
        DatabaseVersion::V4
    } else if major >= 14 {
        DatabaseVersion::V3
    } else if major >= 12 {
        DatabaseVersion::V2
    } else {
        DatabaseVersion::V1
    }
}

/// The SQL for one layout.
///
/// Each query carries the kronika marker and selects only the columns that
/// version stores. `ts` is one `statement_timestamp()` for the whole snapshot;
/// timestamp columns (`stats_reset`, `checksum_last_failure`) come back as unix
/// microseconds.
#[must_use]
pub const fn database_query(version: DatabaseVersion) -> &'static str {
    match version {
        DatabaseVersion::V1 => marked!(
            "SELECT datid, datname::text AS datname, numbackends, \
             xact_commit, xact_rollback, blks_read, blks_hit, \
             tup_returned, tup_fetched, tup_inserted, tup_updated, tup_deleted, \
             conflicts, temp_files, temp_bytes, deadlocks, \
             blk_read_time, blk_write_time, \
             (extract(epoch from stats_reset) * 1e6)::int8 AS stats_reset_us, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_database"
        ),
        DatabaseVersion::V2 => marked!(
            "SELECT datid, datname::text AS datname, numbackends, \
             xact_commit, xact_rollback, blks_read, blks_hit, \
             tup_returned, tup_fetched, tup_inserted, tup_updated, tup_deleted, \
             conflicts, temp_files, temp_bytes, deadlocks, \
             blk_read_time, blk_write_time, \
             (extract(epoch from stats_reset) * 1e6)::int8 AS stats_reset_us, \
             checksum_failures, \
             (extract(epoch from checksum_last_failure) * 1e6)::int8 AS checksum_last_failure_us, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_database"
        ),
        DatabaseVersion::V3 => marked!(
            "SELECT datid, datname::text AS datname, numbackends, \
             xact_commit, xact_rollback, blks_read, blks_hit, \
             tup_returned, tup_fetched, tup_inserted, tup_updated, tup_deleted, \
             conflicts, temp_files, temp_bytes, deadlocks, \
             blk_read_time, blk_write_time, \
             (extract(epoch from stats_reset) * 1e6)::int8 AS stats_reset_us, \
             checksum_failures, \
             (extract(epoch from checksum_last_failure) * 1e6)::int8 AS checksum_last_failure_us, \
             session_time, active_time, idle_in_transaction_time, \
             sessions, sessions_abandoned, sessions_fatal, sessions_killed, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_database"
        ),
        DatabaseVersion::V4 => marked!(
            "SELECT datid, datname::text AS datname, numbackends, \
             xact_commit, xact_rollback, blks_read, blks_hit, \
             tup_returned, tup_fetched, tup_inserted, tup_updated, tup_deleted, \
             conflicts, temp_files, temp_bytes, deadlocks, \
             blk_read_time, blk_write_time, \
             (extract(epoch from stats_reset) * 1e6)::int8 AS stats_reset_us, \
             checksum_failures, \
             (extract(epoch from checksum_last_failure) * 1e6)::int8 AS checksum_last_failure_us, \
             session_time, active_time, idle_in_transaction_time, \
             sessions, sessions_abandoned, sessions_fatal, sessions_killed, \
             parallel_workers_to_launch, parallel_workers_launched, \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_database"
        ),
    }
}

/// One raw `pg_stat_database` row, a version-agnostic superset.
///
/// Numbers are owned directly; `datname` is interned by the caller. Columns
/// absent from the version are `None`. See [`PgStatDatabaseV4`] for meaning.
#[derive(Debug, Clone)]
pub struct DatabaseRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Database oid (`0` for shared objects).
    pub datid: u32,
    /// Database name (`None` for the shared-objects row).
    pub datname: Option<String>,
    /// Backends connected to this database.
    pub numbackends: i32,
    /// Committed transactions.
    pub xact_commit: i64,
    /// Rolled-back transactions.
    pub xact_rollback: i64,
    /// Disk blocks read.
    pub blks_read: i64,
    /// Buffer hits.
    pub blks_hit: i64,
    /// Rows returned.
    pub tup_returned: i64,
    /// Rows fetched.
    pub tup_fetched: i64,
    /// Rows inserted.
    pub tup_inserted: i64,
    /// Rows updated.
    pub tup_updated: i64,
    /// Rows deleted.
    pub tup_deleted: i64,
    /// Recovery-conflict cancellations.
    pub conflicts: i64,
    /// Temporary files created.
    pub temp_files: i64,
    /// Bytes written to temporary files.
    pub temp_bytes: i64,
    /// Deadlocks detected.
    pub deadlocks: i64,
    /// Block read time, ms.
    pub blk_read_time: f64,
    /// Block write time, ms.
    pub blk_write_time: f64,
    /// Last statistics reset, unix microseconds; `None` if never.
    pub stats_reset: Option<i64>,
    /// Checksum failures (V2+).
    pub checksum_failures: Option<i64>,
    /// Last checksum failure, unix microseconds (V2+).
    pub checksum_last_failure: Option<i64>,
    /// Session time, ms (V3+).
    pub session_time: Option<f64>,
    /// Active session time, ms (V3+).
    pub active_time: Option<f64>,
    /// Idle-in-transaction time, ms (V3+).
    pub idle_in_transaction_time: Option<f64>,
    /// Sessions established (V3+).
    pub sessions: Option<i64>,
    /// Sessions abandoned (V3+).
    pub sessions_abandoned: Option<i64>,
    /// Sessions ended by a fatal error (V3+).
    pub sessions_fatal: Option<i64>,
    /// Sessions killed by an operator (V3+).
    pub sessions_killed: Option<i64>,
    /// Parallel workers planned (V4+).
    pub parallel_workers_to_launch: Option<i64>,
    /// Parallel workers launched (V4+).
    pub parallel_workers_launched: Option<i64>,
}

/// Intern an optional string, preserving `None`.
fn opt<E>(
    intern: &mut impl FnMut(&[u8]) -> Result<StrId, E>,
    value: Option<&str>,
) -> Result<Option<StrId>, E> {
    match value {
        Some(s) => Ok(Some(intern(s.as_bytes())?)),
        None => Ok(None),
    }
}

/// Build a `1_005_004` row (PG18 layout), interning `datname`.
///
/// # Errors
/// Returns the interner's error if `datname` cannot be interned.
pub fn to_v4<E>(
    row: &DatabaseRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatDatabaseV4, E> {
    Ok(PgStatDatabaseV4 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        numbackends: row.numbackends,
        xact_commit: row.xact_commit,
        xact_rollback: row.xact_rollback,
        blks_read: row.blks_read,
        blks_hit: row.blks_hit,
        tup_returned: row.tup_returned,
        tup_fetched: row.tup_fetched,
        tup_inserted: row.tup_inserted,
        tup_updated: row.tup_updated,
        tup_deleted: row.tup_deleted,
        conflicts: row.conflicts,
        temp_files: row.temp_files,
        temp_bytes: row.temp_bytes,
        deadlocks: row.deadlocks,
        blk_read_time: row.blk_read_time,
        blk_write_time: row.blk_write_time,
        stats_reset: row.stats_reset.map(Ts),
        checksum_failures: row.checksum_failures.unwrap_or(0),
        checksum_last_failure: row.checksum_last_failure.map(Ts),
        session_time: row.session_time.unwrap_or(0.0),
        active_time: row.active_time.unwrap_or(0.0),
        idle_in_transaction_time: row.idle_in_transaction_time.unwrap_or(0.0),
        sessions: row.sessions.unwrap_or(0),
        sessions_abandoned: row.sessions_abandoned.unwrap_or(0),
        sessions_fatal: row.sessions_fatal.unwrap_or(0),
        sessions_killed: row.sessions_killed.unwrap_or(0),
        parallel_workers_to_launch: row.parallel_workers_to_launch.unwrap_or(0),
        parallel_workers_launched: row.parallel_workers_launched.unwrap_or(0),
    })
}

/// Build a `1_005_003` row (PG14-17 layout, no parallel-worker counters).
///
/// # Errors
/// Returns the interner's error if `datname` cannot be interned.
pub fn to_v3<E>(
    row: &DatabaseRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatDatabaseV3, E> {
    Ok(PgStatDatabaseV3 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        numbackends: row.numbackends,
        xact_commit: row.xact_commit,
        xact_rollback: row.xact_rollback,
        blks_read: row.blks_read,
        blks_hit: row.blks_hit,
        tup_returned: row.tup_returned,
        tup_fetched: row.tup_fetched,
        tup_inserted: row.tup_inserted,
        tup_updated: row.tup_updated,
        tup_deleted: row.tup_deleted,
        conflicts: row.conflicts,
        temp_files: row.temp_files,
        temp_bytes: row.temp_bytes,
        deadlocks: row.deadlocks,
        blk_read_time: row.blk_read_time,
        blk_write_time: row.blk_write_time,
        stats_reset: row.stats_reset.map(Ts),
        checksum_failures: row.checksum_failures.unwrap_or(0),
        checksum_last_failure: row.checksum_last_failure.map(Ts),
        session_time: row.session_time.unwrap_or(0.0),
        active_time: row.active_time.unwrap_or(0.0),
        idle_in_transaction_time: row.idle_in_transaction_time.unwrap_or(0.0),
        sessions: row.sessions.unwrap_or(0),
        sessions_abandoned: row.sessions_abandoned.unwrap_or(0),
        sessions_fatal: row.sessions_fatal.unwrap_or(0),
        sessions_killed: row.sessions_killed.unwrap_or(0),
    })
}

/// Build a `1_005_002` row (PG12-13 layout, checksum but no session columns).
///
/// # Errors
/// Returns the interner's error if `datname` cannot be interned.
pub fn to_v2<E>(
    row: &DatabaseRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatDatabaseV2, E> {
    Ok(PgStatDatabaseV2 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        numbackends: row.numbackends,
        xact_commit: row.xact_commit,
        xact_rollback: row.xact_rollback,
        blks_read: row.blks_read,
        blks_hit: row.blks_hit,
        tup_returned: row.tup_returned,
        tup_fetched: row.tup_fetched,
        tup_inserted: row.tup_inserted,
        tup_updated: row.tup_updated,
        tup_deleted: row.tup_deleted,
        conflicts: row.conflicts,
        temp_files: row.temp_files,
        temp_bytes: row.temp_bytes,
        deadlocks: row.deadlocks,
        blk_read_time: row.blk_read_time,
        blk_write_time: row.blk_write_time,
        stats_reset: row.stats_reset.map(Ts),
        checksum_failures: row.checksum_failures.unwrap_or(0),
        checksum_last_failure: row.checksum_last_failure.map(Ts),
    })
}

/// Build a `1_005_001` row (PG10-11 base layout).
///
/// # Errors
/// Returns the interner's error if `datname` cannot be interned.
pub fn to_v1<E>(
    row: &DatabaseRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatDatabaseV1, E> {
    Ok(PgStatDatabaseV1 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        numbackends: row.numbackends,
        xact_commit: row.xact_commit,
        xact_rollback: row.xact_rollback,
        blks_read: row.blks_read,
        blks_hit: row.blks_hit,
        tup_returned: row.tup_returned,
        tup_fetched: row.tup_fetched,
        tup_inserted: row.tup_inserted,
        tup_updated: row.tup_updated,
        tup_deleted: row.tup_deleted,
        conflicts: row.conflicts,
        temp_files: row.temp_files,
        temp_bytes: row.temp_bytes,
        deadlocks: row.deadlocks,
        blk_read_time: row.blk_read_time,
        blk_write_time: row.blk_write_time,
        stats_reset: row.stats_reset.map(Ts),
    })
}

/// Read a raw row from a result row using the version's column set.
fn row_from_pg(row: &tokio_postgres::Row, version: DatabaseVersion) -> DatabaseRow {
    let has_checksum = matches!(
        version,
        DatabaseVersion::V2 | DatabaseVersion::V3 | DatabaseVersion::V4
    );
    let has_session = matches!(version, DatabaseVersion::V3 | DatabaseVersion::V4);
    let has_parallel = matches!(version, DatabaseVersion::V4);
    DatabaseRow {
        ts: row.get("ts_us"),
        datid: row.get("datid"),
        datname: row.get("datname"),
        numbackends: row.get("numbackends"),
        xact_commit: row.get("xact_commit"),
        xact_rollback: row.get("xact_rollback"),
        blks_read: row.get("blks_read"),
        blks_hit: row.get("blks_hit"),
        tup_returned: row.get("tup_returned"),
        tup_fetched: row.get("tup_fetched"),
        tup_inserted: row.get("tup_inserted"),
        tup_updated: row.get("tup_updated"),
        tup_deleted: row.get("tup_deleted"),
        conflicts: row.get("conflicts"),
        temp_files: row.get("temp_files"),
        temp_bytes: row.get("temp_bytes"),
        deadlocks: row.get("deadlocks"),
        blk_read_time: row.get("blk_read_time"),
        blk_write_time: row.get("blk_write_time"),
        stats_reset: row.get("stats_reset_us"),
        checksum_failures: if has_checksum {
            row.get::<_, Option<i64>>("checksum_failures")
        } else {
            None
        },
        checksum_last_failure: if has_checksum {
            row.get("checksum_last_failure_us")
        } else {
            None
        },
        session_time: has_session.then(|| row.get("session_time")),
        active_time: has_session.then(|| row.get("active_time")),
        idle_in_transaction_time: has_session.then(|| row.get("idle_in_transaction_time")),
        sessions: has_session.then(|| row.get("sessions")),
        sessions_abandoned: has_session.then(|| row.get("sessions_abandoned")),
        sessions_fatal: has_session.then(|| row.get("sessions_fatal")),
        sessions_killed: has_session.then(|| row.get("sessions_killed")),
        parallel_workers_to_launch: has_parallel.then(|| row.get("parallel_workers_to_launch")),
        parallel_workers_launched: has_parallel.then(|| row.get("parallel_workers_launched")),
    }
}

/// Collect a full `pg_stat_database` snapshot. Returns the layout version and
/// raw rows; the caller interns `datname` and builds the typed rows.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_database(
    client: &Client,
    major: u32,
) -> Result<(DatabaseVersion, Vec<DatabaseRow>), tokio_postgres::Error> {
    let version = database_version(major);
    let rows = client.query(database_query(version), &[]).await?;
    let parsed = rows.iter().map(|row| row_from_pg(row, version)).collect();
    Ok((version, parsed))
}

#[cfg(test)]
mod tests {
    use super::{DatabaseRow, DatabaseVersion, database_query, database_version, to_v1, to_v4};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_v* expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn sample_row(datid: u32) -> DatabaseRow {
        DatabaseRow {
            ts: 2_000,
            datid,
            datname: if datid == 0 {
                None
            } else {
                Some("appdb".to_owned())
            },
            numbackends: 4,
            xact_commit: 100,
            xact_rollback: 2,
            blks_read: 4_000,
            blks_hit: 90_000,
            tup_returned: 500,
            tup_fetched: 400,
            tup_inserted: 50,
            tup_updated: 30,
            tup_deleted: 10,
            conflicts: 0,
            temp_files: 1,
            temp_bytes: 8_192,
            deadlocks: 0,
            blk_read_time: 12.5,
            blk_write_time: 3.0,
            stats_reset: Some(1_500),
            checksum_failures: Some(0),
            checksum_last_failure: None,
            session_time: Some(1_000.0),
            active_time: Some(250.0),
            idle_in_transaction_time: Some(50.0),
            sessions: Some(7),
            sessions_abandoned: Some(1),
            sessions_fatal: Some(0),
            sessions_killed: Some(0),
            parallel_workers_to_launch: Some(9),
            parallel_workers_launched: Some(8),
        }
    }

    #[test]
    fn version_follows_catalog_changes() {
        assert_eq!(database_version(10), DatabaseVersion::V1);
        assert_eq!(database_version(11), DatabaseVersion::V1);
        assert_eq!(database_version(12), DatabaseVersion::V2);
        assert_eq!(database_version(13), DatabaseVersion::V2);
        assert_eq!(database_version(14), DatabaseVersion::V3);
        assert_eq!(database_version(17), DatabaseVersion::V3);
        assert_eq!(database_version(18), DatabaseVersion::V4);
    }

    #[test]
    fn query_includes_version_specific_columns() {
        assert!(!database_query(DatabaseVersion::V1).contains("checksum_failures"));
        assert!(database_query(DatabaseVersion::V2).contains("checksum_failures"));
        assert!(!database_query(DatabaseVersion::V2).contains("session_time"));
        assert!(database_query(DatabaseVersion::V3).contains("session_time"));
        assert!(!database_query(DatabaseVersion::V3).contains("parallel_workers_launched"));
        assert!(database_query(DatabaseVersion::V4).contains("parallel_workers_launched"));
        for v in [
            DatabaseVersion::V1,
            DatabaseVersion::V2,
            DatabaseVersion::V3,
            DatabaseVersion::V4,
        ] {
            assert!(database_query(v).contains("pg_stat_database"));
            assert!(database_query(v).contains("pg_kronika"));
        }
    }

    #[test]
    fn to_v4_maps_every_column_and_interns_datname() {
        let r = to_v4(&sample_row(5), fake_intern).expect("infallible intern");
        assert_eq!(r.ts.0, 2_000);
        assert_eq!(r.datid, 5);
        assert_eq!(r.datname, Some(fake_intern(b"appdb").unwrap()));
        assert_eq!(r.numbackends, 4);
        assert!((r.blk_read_time - 12.5).abs() < f64::EPSILON);
        assert_eq!(r.checksum_failures, 0);
        assert_eq!(r.checksum_last_failure, None);
        assert_eq!(r.parallel_workers_launched, 8);
    }

    #[test]
    fn to_v4_shared_row_has_null_datname() {
        let r = to_v4(&sample_row(0), fake_intern).expect("intern");
        assert_eq!(r.datid, 0);
        assert_eq!(r.datname, None);
    }

    #[test]
    fn to_v1_maps_the_base_layout() {
        let r = to_v1(&sample_row(5), fake_intern).expect("intern");
        assert_eq!(r.datid, 5);
        assert_eq!(r.datname, Some(fake_intern(b"appdb").unwrap()));
        assert_eq!(r.xact_commit, 100);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_v4(&sample_row(5), boom), Err("full"));
    }
}
