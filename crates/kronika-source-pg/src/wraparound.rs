//! `wraparound` collection for type `1_018_001`.
//!
//! Per-database transaction-ID and multixact age toward wraparound, read from
//! `pg_database`. Every database is included — a frozen idle database such as
//! `template0` can hold the cluster's largest age. Collection returns owned rows;
//! the caller interns `datname` into the segment dictionary.

use kronika_registry::wraparound::WraparoundAge;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/wraparound.rs */ ",
            $sql,
        )
    };
}

/// One raw `wraparound` row; `datname` is owned and interned by the caller.
#[derive(Debug, Clone)]
pub struct WraparoundRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Database name.
    pub datname: String,
    /// `age(datfrozenxid)` for this database.
    pub age: i64,
    /// `mxid_age(datminmxid)` for this database.
    pub mxid_age: i64,
}

/// Build a `1_018_001` row, interning `datname`.
///
/// # Errors
/// Returns the interner's error if `datname` cannot be interned.
pub fn to_wraparound<E>(
    row: &WraparoundRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<WraparoundAge, E> {
    Ok(WraparoundAge {
        ts: Ts(row.ts),
        datname: intern(row.datname.as_bytes())?,
        age: row.age,
        mxid_age: row.mxid_age,
    })
}

/// Collect both wraparound ages for every database.
///
/// All databases are read, including ones that disallow connections: their
/// `datfrozenxid` / `datminmxid` still age and bound the cluster's wraparound
/// headroom on each axis. `ts` is one `statement_timestamp()` for the whole
/// snapshot.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_wraparound(
    client: &Client,
) -> Result<Vec<WraparoundRow>, tokio_postgres::Error> {
    let rows = client
        .query(
            marked!(
                "SELECT datname, age(datfrozenxid)::int8 AS age, \
                 mxid_age(datminmxid)::int8 AS mxid_age, \
                 (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
                 FROM pg_database"
            ),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| WraparoundRow {
            ts: row.get("ts_us"),
            datname: row.get("datname"),
            age: row.get("age"),
            mxid_age: row.get("mxid_age"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{WraparoundRow, to_wraparound};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_wraparound expects"
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
    fn to_wraparound_interns_datname_and_keeps_ages() {
        let r = WraparoundRow {
            ts: 2_000,
            datname: "template0".to_owned(),
            age: 150_000_000,
            mxid_age: 5_000_000,
        };
        let out = to_wraparound(&r, fake_intern).expect("intern");
        assert_eq!(out.ts.0, 2_000);
        assert_eq!(out.datname, fake_intern(b"template0").unwrap());
        assert_eq!(out.age, 150_000_000);
        assert_eq!(out.mxid_age, 5_000_000);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        let r = WraparoundRow {
            ts: 1,
            datname: "appdb".to_owned(),
            age: 1,
            mxid_age: 1,
        };
        assert_eq!(to_wraparound(&r, boom), Err("full"));
    }
}
