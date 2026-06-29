//! `pg_prepared_xacts` per-database aggregate collection for type `1_010_001`.
//!
//! Two-phase-commit transactions (`PREPARE TRANSACTION`) awaiting resolution,
//! summarized per database: how many are prepared and the oldest one's
//! wall-clock age. The view is cluster-wide and tags each transaction with its
//! database; grouping by database keeps the database that a forgotten 2PC blocks
//! vacuum in. Returns no rows when nothing is prepared (the default, since
//! `max_prepared_transactions` is 0). The caller interns `datname`.

use kronika_registry::pg_prepared_xacts::PgPreparedXacts;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/prepared_xacts.rs */ ",
            $sql,
        )
    };
}

/// One raw per-database `pg_prepared_xacts` aggregate row.
///
/// `datname` is owned here and interned by the caller; numbers are owned
/// directly. See [`PgPreparedXacts`] for meaning.
#[derive(Debug, Clone)]
pub struct PreparedXactsRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Database holding these prepared transactions.
    pub datname: String,
    /// Prepared transactions in this database.
    pub prepared_count: i64,
    /// Age of the oldest prepared transaction in this database, microseconds.
    pub max_age_us: i64,
}

/// Build the typed `1_010_001` row, interning `datname`.
///
/// # Errors
/// Returns the interner's error if `datname` cannot be interned.
pub fn to_prepared_xacts<E>(
    row: &PreparedXactsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgPreparedXacts, E> {
    Ok(PgPreparedXacts {
        ts: Ts(row.ts),
        datname: intern(row.datname.as_bytes())?,
        prepared_count: row.prepared_count,
        max_age_us: row.max_age_us,
    })
}

/// Collect the per-database `pg_prepared_xacts` aggregate.
///
/// Groups by database, so each row names the database holding the prepared
/// transactions; `min(prepared)` within a group is never `NULL` (the group
/// exists only because it has at least one prepared transaction). `ts` is the
/// snapshot's `statement_timestamp()`, taken in the same query.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_prepared_xacts(
    client: &Client,
) -> Result<Vec<PreparedXactsRow>, tokio_postgres::Error> {
    let rows = client
        .query(
            marked!(
                "SELECT database::text AS datname, \
                 count(*)::int8 AS prepared_count, \
                 (extract(epoch from (statement_timestamp() - min(prepared))) * 1e6)::int8 \
                 AS max_age_us, \
                 (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
                 FROM pg_prepared_xacts GROUP BY database"
            ),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| PreparedXactsRow {
            ts: row.get("ts_us"),
            datname: row.get("datname"),
            prepared_count: row.get("prepared_count"),
            max_age_us: row.get("max_age_us"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{PreparedXactsRow, to_prepared_xacts};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_prepared_xacts expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    #[test]
    fn maps_every_field_and_interns_datname() {
        let r = PreparedXactsRow {
            ts: 2_000,
            datname: "appdb".to_owned(),
            prepared_count: 3,
            max_age_us: 4_200_000,
        };
        let typed = to_prepared_xacts(&r, fake_intern).expect("infallible intern");
        assert_eq!(typed.ts.0, 2_000);
        assert_eq!(typed.prepared_count, 3);
        assert_eq!(typed.max_age_us, 4_200_000);
        assert_eq!(typed.datname, fake_intern(b"appdb").unwrap());
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        let r = PreparedXactsRow {
            ts: 1,
            datname: "db".to_owned(),
            prepared_count: 1,
            max_age_us: 1,
        };
        assert_eq!(to_prepared_xacts(&r, boom), Err("full"));
    }
}
