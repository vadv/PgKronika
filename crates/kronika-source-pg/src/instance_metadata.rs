//! The `PostgreSQL` half of section `1_021_001` (`instance_metadata`).
//!
//! The OS half comes from `kronika-source-os`; the collector binary joins the
//! two and writes one row per segment.

use tokio_postgres::Client;

/// Prefix a query literal with the collector marker.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/instance_metadata.rs */ ",
            $sql,
        )
    };
}

/// Server identity from the main connection.
#[derive(Debug, Clone, Copy)]
pub struct PgInstanceFacts {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// `server_version_num`, e.g. 170000 for PG17.
    pub pg_version_num: i32,
}

/// Read the collection timestamp and the server version number.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_pg_instance_facts(
    client: &Client,
) -> Result<PgInstanceFacts, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!(
                "SELECT \
                     (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                     current_setting('server_version_num')::int4 AS version_num"
            ),
            &[],
        )
        .await?;
    Ok(PgInstanceFacts {
        ts: row.get("ts_us"),
        pg_version_num: row.get("version_num"),
    })
}

/// Read the `pg_control` system identifier.
///
/// Kept separate from [`collect_pg_instance_facts`] because
/// `pg_control_system()` can be revoked from the collector's role; the caller
/// degrades this one value to `NULL` instead of losing the section.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the function is not executable or
/// the query fails.
pub async fn pg_system_identifier(client: &Client) -> Result<i64, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!("SELECT system_identifier FROM pg_control_system()"),
            &[],
        )
        .await?;
    Ok(row.get("system_identifier"))
}
