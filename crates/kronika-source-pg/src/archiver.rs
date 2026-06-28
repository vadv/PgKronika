//! `pg_stat_archiver` collection for type `1_008_001`.
//!
//! One row, stable across PG 10-18; the caller interns the WAL file names. The
//! typed layout lives in `kronika-registry` (`PgStatArchiver`).

use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/archiver.rs */ ",
            $sql,
        )
    };
}

const QUERY: &str = marked!(
    "SELECT archived_count, last_archived_wal, \
     (extract(epoch from last_archived_time) * 1e6)::int8 AS last_archived_time_us, \
     failed_count, last_failed_wal, \
     (extract(epoch from last_failed_time) * 1e6)::int8 AS last_failed_time_us, \
     (extract(epoch from stats_reset) * 1e6)::int8 AS stats_reset_us, \
     (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
     FROM pg_stat_archiver"
);

/// One raw `pg_stat_archiver` row. WAL names are owned; the caller interns them.
/// See [`PgStatArchiver`] for column meaning.
#[derive(Debug, Clone)]
pub struct ArchiverRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// WAL files archived.
    pub archived_count: i64,
    /// Last archived WAL file name.
    pub last_archived_wal: Option<String>,
    /// Last archive time, unix microseconds.
    pub last_archived_time: Option<i64>,
    /// Failed archive attempts.
    pub failed_count: i64,
    /// WAL file of the last failed attempt.
    pub last_failed_wal: Option<String>,
    /// Last failed attempt time, unix microseconds.
    pub last_failed_time: Option<i64>,
    /// Last reset, unix microseconds.
    pub stats_reset: Option<i64>,
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

/// Build the typed `1_008_001` row, interning WAL file names.
///
/// # Errors
/// Returns the interner's error if a WAL name cannot be interned.
pub fn to_archiver<E>(
    row: &ArchiverRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatArchiver, E> {
    Ok(PgStatArchiver {
        ts: Ts(row.ts),
        archived_count: row.archived_count,
        last_archived_wal: opt(&mut intern, row.last_archived_wal.as_deref())?,
        last_archived_time: row.last_archived_time.map(Ts),
        failed_count: row.failed_count,
        last_failed_wal: opt(&mut intern, row.last_failed_wal.as_deref())?,
        last_failed_time: row.last_failed_time.map(Ts),
        stats_reset: row.stats_reset.map(Ts),
    })
}

/// Collect the single `pg_stat_archiver` row (present on every PG 10-18).
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_archiver(client: &Client) -> Result<ArchiverRow, tokio_postgres::Error> {
    let row = client.query_one(QUERY, &[]).await?;
    Ok(ArchiverRow {
        ts: row.get("ts_us"),
        archived_count: row.get("archived_count"),
        last_archived_wal: row.get("last_archived_wal"),
        last_archived_time: row.get("last_archived_time_us"),
        failed_count: row.get("failed_count"),
        last_failed_wal: row.get("last_failed_wal"),
        last_failed_time: row.get("last_failed_time_us"),
        stats_reset: row.get("stats_reset_us"),
    })
}

#[cfg(test)]
mod tests {
    use super::{ArchiverRow, to_archiver};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_archiver expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn sample_row(archived: bool, failed: bool) -> ArchiverRow {
        ArchiverRow {
            ts: 2_000,
            archived_count: 100,
            last_archived_wal: archived.then(|| "000000010000000000000005".to_owned()),
            last_archived_time: archived.then_some(1_500),
            failed_count: 2,
            last_failed_wal: failed.then(|| "000000010000000000000006".to_owned()),
            last_failed_time: failed.then_some(1_750),
            stats_reset: Some(1_000),
        }
    }

    #[test]
    fn interns_wal_names_and_maps_times() {
        let r = to_archiver(&sample_row(true, true), fake_intern).expect("intern");
        assert_eq!(r.ts.0, 2_000);
        assert_eq!(r.archived_count, 100);
        assert_eq!(
            r.last_archived_wal,
            Some(fake_intern(b"000000010000000000000005").unwrap())
        );
        assert_eq!(r.last_archived_time.map(|t| t.0), Some(1_500));
        assert_eq!(
            r.last_failed_wal,
            Some(fake_intern(b"000000010000000000000006").unwrap())
        );
        assert_eq!(r.last_failed_time.map(|t| t.0), Some(1_750));
    }

    #[test]
    fn handles_null_wal_names() {
        let r = to_archiver(&sample_row(false, false), fake_intern).expect("intern");
        assert_eq!(r.last_archived_wal, None);
        assert_eq!(r.last_archived_time, None);
        assert_eq!(r.last_failed_wal, None);
        assert_eq!(r.last_failed_time, None);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_archiver(&sample_row(false, true), boom), Err("full"));
    }
}
