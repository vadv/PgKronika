//! `pg_store_plans` collection: vadv fork (`1_004_001`) and ossc upstream
//! (`1_003_001`).
//!
//! The extension exposes instance-wide per-plan rows through the
//! `pg_store_plans(showtext boolean)` set-returning function; the SQL objects
//! exist only in the database where `CREATE EXTENSION` ran, so the caller
//! discovers that database across the pool and pins the connection.
//!
//! Collection uses two SQL calls: enumerate top-N rows by `total_time` with
//! `showtext := false`, then fetch text for selected rows through
//! `pg_store_plans_get_plan` under the caller's per-cycle byte budget.
//!
//! `datname` and `usename` resolve through `LEFT JOIN`, so they are `None`
//! for an oid with no catalog row. Collection returns owned rows; the caller
//! interns the strings into the segment dictionary. The typed layout lives in
//! `kronika-registry` (`PgStorePlansVadvV1`).

use kronika_registry::pg_store_plans::{PgStorePlansOsscV1, PgStorePlansVadvV1};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the collector marker.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/store_plans.rs */ ",
            $sql,
        )
    };
}

/// One raw row of `pg_store_plans(false)`, before plan-text fetching.
///
/// `plan` starts as `None`; the caller fills it from
/// [`fetch_plan_text`] for the rows that fit the text budget.
#[derive(Debug, Clone)]
pub struct StorePlansRow {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// The extension's internal key slot; always `0` on this fork. Kept only
    /// to pass back into `pg_store_plans_get_plan`; never sealed.
    pub queryid: i64,
    /// Query id of the LAST statement that ran this plan; `0` when
    /// `compute_query_id` is off. Attribution, not identity.
    pub queryid_stat_statements: i64,
    /// Plan id derived from the normalized plan representation.
    pub planid: i64,
    /// Role oid the statements ran as.
    pub userid: u32,
    /// Database oid the statements ran in.
    pub dbid: u32,
    /// Database name resolved from `dbid`; `None` when unresolved.
    pub datname: Option<String>,
    /// Role name resolved from `userid`; `None` when unresolved.
    pub usename: Option<String>,
    /// Plan text; filled by the caller from [`fetch_plan_text`].
    pub plan: Option<String>,
    /// Executions accumulated for this plan entry.
    pub calls: i64,
    /// Executions recorded through `slow_statement_duration`.
    pub slow_log_calls: i64,
    /// Total execution time, milliseconds.
    pub total_time: f64,
    /// Minimum execution time, milliseconds.
    pub min_time: f64,
    /// Maximum execution time, milliseconds.
    pub max_time: f64,
    /// Mean execution time, milliseconds.
    pub mean_time: f64,
    /// Population standard deviation of execution time, milliseconds.
    pub stddev_time: f64,
    /// Rows retrieved or affected.
    pub rows: i64,
    /// Shared-block buffer hits.
    pub shared_blks_hit: i64,
    /// Shared blocks read.
    pub shared_blks_read: i64,
    /// Shared blocks dirtied.
    pub shared_blks_dirtied: i64,
    /// Shared blocks written.
    pub shared_blks_written: i64,
    /// Local-block buffer hits.
    pub local_blks_hit: i64,
    /// Local blocks read.
    pub local_blks_read: i64,
    /// Local blocks dirtied.
    pub local_blks_dirtied: i64,
    /// Local blocks written.
    pub local_blks_written: i64,
    /// Temp blocks read.
    pub temp_blks_read: i64,
    /// Temp blocks written.
    pub temp_blks_written: i64,
    /// Time reading blocks, milliseconds; `0` without `track_io_timing`.
    pub blk_read_time: f64,
    /// Time writing blocks, milliseconds; `0` without `track_io_timing`.
    pub blk_write_time: f64,
    /// When statistics for this entry began accumulating, unix microseconds.
    pub first_call: i64,
    /// When the entry was last executed, unix microseconds.
    pub last_call: i64,
    /// Total planning time, milliseconds; `0` without `track_planning`.
    pub total_plan_time: f64,
    /// Minimum planning time, milliseconds; `0` without `track_planning`.
    pub min_plan_time: f64,
    /// Maximum planning time, milliseconds; `0` without `track_planning`.
    pub max_plan_time: f64,
    /// Mean planning time, milliseconds; `0` without `track_planning`.
    pub mean_plan_time: f64,
}

/// Read the installed `pg_store_plans` extension version on this connection.
///
/// Returns `None` when the extension is not installed in the connected
/// database.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the probe query fails.
pub async fn store_plans_extversion(
    client: &Client,
) -> Result<Option<String>, tokio_postgres::Error> {
    let row = client
        .query_opt(
            marked!("SELECT extversion FROM pg_extension WHERE extname = 'pg_store_plans'"),
            &[],
        )
        .await?;
    Ok(row.map(|row| row.get("extversion")))
}

/// Whether the installed extension exposes the vadv 2.x function signature.
///
/// The vadv fork declares `pg_store_plans(showtext boolean)`; the ossc
/// upstream declares a zero-argument function. A `false` result lets the caller
/// skip this layout instead of failing mid-collection.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the probe query fails.
pub async fn store_plans_is_vadv(client: &Client) -> Result<bool, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!("SELECT to_regprocedure('pg_store_plans(boolean)') IS NOT NULL AS vadv"),
            &[],
        )
        .await?;
    Ok(row.get("vadv"))
}

/// The enumeration query: top-N plan entries without plan texts.
const fn store_plans_query() -> &'static str {
    marked!(
        "SELECT \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
             (SELECT count(*) FROM pg_store_plans(false)) AS source_total, \
             s.queryid, \
             s.queryid_stat_statements, \
             s.planid, \
             s.userid, \
             s.dbid, \
             d.datname, \
             r.rolname AS usename, \
             s.calls, \
             s.slow_log_calls, \
             s.total_time, \
             s.min_time, \
             s.max_time, \
             s.mean_time, \
             s.stddev_time, \
             s.rows, \
             s.shared_blks_hit, \
             s.shared_blks_read, \
             s.shared_blks_dirtied, \
             s.shared_blks_written, \
             s.local_blks_hit, \
             s.local_blks_read, \
             s.local_blks_dirtied, \
             s.local_blks_written, \
             s.temp_blks_read, \
             s.temp_blks_written, \
             s.blk_read_time, \
             s.blk_write_time, \
             (extract(epoch from s.first_call) * 1e6)::int8 AS first_call_us, \
             (extract(epoch from s.last_call) * 1e6)::int8 AS last_call_us, \
             s.total_plan_time, \
             s.min_plan_time, \
             s.max_plan_time, \
             s.mean_plan_time \
         FROM pg_store_plans(false) s \
         LEFT JOIN pg_database d ON d.oid = s.dbid \
         LEFT JOIN pg_roles r ON r.oid = s.userid \
         ORDER BY s.total_time DESC \
         LIMIT $1"
    )
}

/// Collect top-N `pg_store_plans` rows without plan texts.
///
/// Runs against the one connection where the extension is installed;
/// `max_plans` is the top-N cap by `total_time`. Plan texts are fetched
/// separately per row through [`fetch_plan_text`].
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_store_plans(
    client: &Client,
    max_plans: i64,
) -> Result<(Vec<StorePlansRow>, u64), tokio_postgres::Error> {
    let rows = client.query(store_plans_query(), &[&max_plans]).await?;
    let source_total = rows
        .first()
        .map_or(0, |row| row.get::<_, i64>("source_total"));
    Ok((
        rows.iter().map(row_from_pg).collect(),
        u64::try_from(source_total).unwrap_or(0),
    ))
}

fn row_from_pg(row: &tokio_postgres::Row) -> StorePlansRow {
    StorePlansRow {
        ts: row.get("ts_us"),
        queryid: row.get("queryid"),
        queryid_stat_statements: row.get("queryid_stat_statements"),
        planid: row.get("planid"),
        userid: row.get("userid"),
        dbid: row.get("dbid"),
        datname: row.get("datname"),
        usename: row.get("usename"),
        plan: None,
        calls: row.get("calls"),
        slow_log_calls: row.get("slow_log_calls"),
        total_time: row.get("total_time"),
        min_time: row.get("min_time"),
        max_time: row.get("max_time"),
        mean_time: row.get("mean_time"),
        stddev_time: row.get("stddev_time"),
        rows: row.get("rows"),
        shared_blks_hit: row.get("shared_blks_hit"),
        shared_blks_read: row.get("shared_blks_read"),
        shared_blks_dirtied: row.get("shared_blks_dirtied"),
        shared_blks_written: row.get("shared_blks_written"),
        local_blks_hit: row.get("local_blks_hit"),
        local_blks_read: row.get("local_blks_read"),
        local_blks_dirtied: row.get("local_blks_dirtied"),
        local_blks_written: row.get("local_blks_written"),
        temp_blks_read: row.get("temp_blks_read"),
        temp_blks_written: row.get("temp_blks_written"),
        blk_read_time: row.get("blk_read_time"),
        blk_write_time: row.get("blk_write_time"),
        first_call: row.get("first_call_us"),
        last_call: row.get("last_call_us"),
        total_plan_time: row.get("total_plan_time"),
        min_plan_time: row.get("min_plan_time"),
        max_plan_time: row.get("max_plan_time"),
        mean_plan_time: row.get("mean_plan_time"),
    }
}

/// Fetch one plan text through `pg_store_plans_get_plan`, truncated to
/// `max_len` bytes.
///
/// Returns `None` when the entry vanished between enumeration and this call
/// (deallocated under memory pressure or reset).
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn fetch_plan_text(
    client: &Client,
    row: &StorePlansRow,
    max_len: i32,
) -> Result<Option<String>, tokio_postgres::Error> {
    let out = client
        .query_one(
            marked!(
                "SELECT left(pg_store_plans_textplan(pg_store_plans_get_plan(\
                     $1::oid, $2::oid, $3, $4)), $5::int4) AS plan"
            ),
            &[&row.userid, &row.dbid, &row.queryid, &row.planid, &max_len],
        )
        .await?;
    Ok(out.get("plan"))
}

/// Build a `1_004_001` row, interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_vadv_v1<E>(
    row: &StorePlansRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStorePlansVadvV1, E> {
    Ok(PgStorePlansVadvV1 {
        ts: Ts(row.ts),
        queryid_stat_statements: row.queryid_stat_statements,
        planid: row.planid,
        userid: row.userid,
        dbid: row.dbid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        plan: opt(&mut intern, row.plan.as_deref())?,
        calls: row.calls,
        slow_log_calls: row.slow_log_calls,
        total_time: row.total_time,
        min_time: row.min_time,
        max_time: row.max_time,
        mean_time: row.mean_time,
        stddev_time: row.stddev_time,
        rows: row.rows,
        shared_blks_hit: row.shared_blks_hit,
        shared_blks_read: row.shared_blks_read,
        shared_blks_dirtied: row.shared_blks_dirtied,
        shared_blks_written: row.shared_blks_written,
        local_blks_hit: row.local_blks_hit,
        local_blks_read: row.local_blks_read,
        local_blks_dirtied: row.local_blks_dirtied,
        local_blks_written: row.local_blks_written,
        temp_blks_read: row.temp_blks_read,
        temp_blks_written: row.temp_blks_written,
        blk_read_time: row.blk_read_time,
        blk_write_time: row.blk_write_time,
        first_call: Ts(row.first_call),
        last_call: Ts(row.last_call),
        total_plan_time: row.total_plan_time,
        min_plan_time: row.min_plan_time,
        max_plan_time: row.max_plan_time,
        mean_plan_time: row.mean_plan_time,
    })
}

fn opt<E>(
    intern: &mut impl FnMut(&[u8]) -> Result<StrId, E>,
    value: Option<&str>,
) -> Result<Option<StrId>, E> {
    value.map(|s| intern(s.as_bytes())).transpose()
}

#[cfg(test)]
mod tests {
    use super::{StorePlansRow, store_plans_query, to_vadv_v1};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the mapper accepts a fallible interner"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn sample_row() -> StorePlansRow {
        StorePlansRow {
            ts: 2_000,
            queryid: 41,
            queryid_stat_statements: 4_242,
            planid: -9,
            userid: 10,
            dbid: 5,
            datname: Some("appdb".to_owned()),
            usename: Some("alice".to_owned()),
            plan: None,
            calls: 100,
            slow_log_calls: 2,
            total_time: 1_234.5,
            min_time: 0.5,
            max_time: 40.0,
            mean_time: 12.3,
            stddev_time: 3.1,
            rows: 5_000,
            shared_blks_hit: 90_000,
            shared_blks_read: 4_000,
            shared_blks_dirtied: 50,
            shared_blks_written: 30,
            local_blks_hit: 1,
            local_blks_read: 2,
            local_blks_dirtied: 3,
            local_blks_written: 4,
            temp_blks_read: 5,
            temp_blks_written: 6,
            blk_read_time: 12.5,
            blk_write_time: 3.0,
            first_call: 1_000,
            last_call: 1_999,
            total_plan_time: 7.5,
            min_plan_time: 0.1,
            max_plan_time: 2.0,
            mean_plan_time: 0.6,
        }
    }

    #[test]
    fn enumeration_query_reads_without_texts_and_caps_by_total_time() {
        let q = store_plans_query();
        assert!(q.contains("FROM pg_store_plans(false) s"), "{q}");
        assert!(q.contains("ORDER BY s.total_time DESC"), "{q}");
        assert!(q.contains("LIMIT $1"), "{q}");
        assert!(q.contains("s.queryid_stat_statements"), "{q}");
        assert!(
            !q.contains("s.plan,") && !q.contains("s.plan "),
            "enumeration must not fetch plan texts: {q}"
        );
    }

    #[test]
    fn to_vadv_v1_interns_strings_and_keeps_null_plan() {
        let row = sample_row();
        let typed = to_vadv_v1(&row, fake_intern).unwrap();
        assert_eq!(typed.queryid_stat_statements, 4_242);
        assert_eq!(typed.plan, None, "unfetched plan stays NULL");
        assert!(typed.datname.is_some());
        assert!((typed.total_time - 1_234.5).abs() < f64::EPSILON);
        assert_eq!(typed.first_call.0, 1_000);
    }

    #[test]
    fn to_vadv_v1_interns_fetched_plan_text() {
        let mut row = sample_row();
        row.plan = Some("Seq Scan on appdb_t".to_owned());
        let typed = to_vadv_v1(&row, fake_intern).unwrap();
        assert!(typed.plan.is_some());
        assert_ne!(typed.plan, typed.datname, "distinct strings intern apart");
    }
}

/// Whether the installed extension exposes the ossc zero-argument signature.
///
/// The upstream declares `pg_store_plans()`; the vadv fork replaces it with
/// the one-argument form. Probing both signatures routes each fork to its own
/// layout; neither probe matching means an unusable installation.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the probe query fails.
pub async fn store_plans_is_ossc(client: &Client) -> Result<bool, tokio_postgres::Error> {
    let row = client
        .query_one(
            marked!("SELECT to_regprocedure('pg_store_plans()') IS NOT NULL AS ossc"),
            &[],
        )
        .await?;
    Ok(row.get("ossc"))
}

/// One raw row of the ossc `pg_store_plans` view.
///
/// The upstream view carries the plan text inline, so collection is a single
/// query with a server-side per-row truncation; the caller applies the byte
/// budget afterwards by dropping tail texts.
#[derive(Debug, Clone)]
pub struct StorePlansOsscRow {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// Core query id, part of the entry identity.
    pub queryid: i64,
    /// Plan id derived from the normalized plan representation.
    pub planid: i64,
    /// Role oid the statements ran as.
    pub userid: u32,
    /// Database oid the statements ran in.
    pub dbid: u32,
    /// Database name resolved from `dbid`; `None` when unresolved.
    pub datname: Option<String>,
    /// Role name resolved from `userid`; `None` when unresolved.
    pub usename: Option<String>,
    /// Plan text, server-truncated; the caller may drop it under the budget.
    pub plan: Option<String>,
    /// Executions accumulated for this plan entry.
    pub calls: i64,
    /// Total execution time, milliseconds.
    pub total_time: f64,
    /// Minimum execution time, milliseconds.
    pub min_time: f64,
    /// Maximum execution time, milliseconds.
    pub max_time: f64,
    /// Mean execution time, milliseconds.
    pub mean_time: f64,
    /// Population standard deviation of execution time, milliseconds.
    pub stddev_time: f64,
    /// Rows retrieved or affected.
    pub rows: i64,
    /// Shared-block buffer hits.
    pub shared_blks_hit: i64,
    /// Shared blocks read.
    pub shared_blks_read: i64,
    /// Shared blocks dirtied.
    pub shared_blks_dirtied: i64,
    /// Shared blocks written.
    pub shared_blks_written: i64,
    /// Local-block buffer hits.
    pub local_blks_hit: i64,
    /// Local blocks read.
    pub local_blks_read: i64,
    /// Local blocks dirtied.
    pub local_blks_dirtied: i64,
    /// Local blocks written.
    pub local_blks_written: i64,
    /// Temp blocks read.
    pub temp_blks_read: i64,
    /// Temp blocks written.
    pub temp_blks_written: i64,
    /// Time reading shared blocks, milliseconds.
    pub shared_blk_read_time: f64,
    /// Time writing shared blocks, milliseconds.
    pub shared_blk_write_time: f64,
    /// Time reading local blocks, milliseconds.
    pub local_blk_read_time: f64,
    /// Time writing local blocks, milliseconds.
    pub local_blk_write_time: f64,
    /// Time reading temp blocks, milliseconds.
    pub temp_blk_read_time: f64,
    /// Time writing temp blocks, milliseconds.
    pub temp_blk_write_time: f64,
    /// When statistics for this entry began accumulating, unix microseconds.
    pub first_call: i64,
    /// When the entry was last executed, unix microseconds.
    pub last_call: i64,
}

/// The ossc collection query: top-N rows with server-truncated plan texts.
///
/// The upstream SRF still copies its stored plan texts into the server-side
/// tuplestore regardless of the projection; selecting `left(...)` bounds what
/// crosses the network and what the collector allocates.
const fn store_plans_ossc_query() -> &'static str {
    marked!(
        "SELECT \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
             (SELECT count(*) FROM pg_store_plans) AS source_total, \
             s.queryid, \
             s.planid, \
             s.userid, \
             s.dbid, \
             d.datname, \
             r.rolname AS usename, \
             left(s.plan, $2::int4) AS plan, \
             s.calls, \
             s.total_time, \
             s.min_time, \
             s.max_time, \
             s.mean_time, \
             s.stddev_time, \
             s.rows, \
             s.shared_blks_hit, \
             s.shared_blks_read, \
             s.shared_blks_dirtied, \
             s.shared_blks_written, \
             s.local_blks_hit, \
             s.local_blks_read, \
             s.local_blks_dirtied, \
             s.local_blks_written, \
             s.temp_blks_read, \
             s.temp_blks_written, \
             s.shared_blk_read_time, \
             s.shared_blk_write_time, \
             s.local_blk_read_time, \
             s.local_blk_write_time, \
             s.temp_blk_read_time, \
             s.temp_blk_write_time, \
             (extract(epoch from s.first_call) * 1e6)::int8 AS first_call_us, \
             (extract(epoch from s.last_call) * 1e6)::int8 AS last_call_us \
         FROM pg_store_plans s \
         LEFT JOIN pg_database d ON d.oid = s.dbid \
         LEFT JOIN pg_roles r ON r.oid = s.userid \
         ORDER BY s.total_time DESC \
         LIMIT $1"
    )
}

/// The numeric-only ossc query: plan texts never cross the network.
///
/// Used when the plan-text budget is zero; the `plan` column is projected as
/// `NULL` so no text is transferred or allocated by the collector.
const fn store_plans_ossc_numeric_query() -> &'static str {
    marked!(
        "SELECT \
             (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
             (SELECT count(*) FROM pg_store_plans) AS source_total, \
             s.queryid, \
             s.planid, \
             s.userid, \
             s.dbid, \
             d.datname, \
             r.rolname AS usename, \
             NULL::text AS plan, \
             s.calls, \
             s.total_time, \
             s.min_time, \
             s.max_time, \
             s.mean_time, \
             s.stddev_time, \
             s.rows, \
             s.shared_blks_hit, \
             s.shared_blks_read, \
             s.shared_blks_dirtied, \
             s.shared_blks_written, \
             s.local_blks_hit, \
             s.local_blks_read, \
             s.local_blks_dirtied, \
             s.local_blks_written, \
             s.temp_blks_read, \
             s.temp_blks_written, \
             s.shared_blk_read_time, \
             s.shared_blk_write_time, \
             s.local_blk_read_time, \
             s.local_blk_write_time, \
             s.temp_blk_read_time, \
             s.temp_blk_write_time, \
             (extract(epoch from s.first_call) * 1e6)::int8 AS first_call_us, \
             (extract(epoch from s.last_call) * 1e6)::int8 AS last_call_us \
         FROM pg_store_plans s \
         LEFT JOIN pg_database d ON d.oid = s.dbid \
         LEFT JOIN pg_roles r ON r.oid = s.userid \
         ORDER BY s.total_time DESC \
         LIMIT $1"
    )
}

/// Collect top-N ossc `pg_store_plans` rows.
///
/// With `text_cap = Some(chars)` the server truncates each plan text per row;
/// with `None` the query projects `NULL` plans and no text crosses the
/// network. The caller enforces the byte budget afterwards.
///
/// Rows whose `queryid`/`planid` came back `NULL` are privilege-masked by the
/// upstream (the collector lacks `pg_read_all_stats` for other roles' rows);
/// they carry no identity and are dropped, and their count is returned so the
/// caller can log the degradation.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_store_plans_ossc(
    client: &Client,
    max_plans: i64,
    text_cap: Option<i32>,
) -> Result<(Vec<StorePlansOsscRow>, usize, u64), tokio_postgres::Error> {
    let rows = match text_cap {
        Some(cap) => {
            client
                .query(store_plans_ossc_query(), &[&max_plans, &cap])
                .await?
        }
        None => {
            client
                .query(store_plans_ossc_numeric_query(), &[&max_plans])
                .await?
        }
    };
    let source_total = rows
        .first()
        .map_or(0, |row| row.get::<_, i64>("source_total"));
    let mut out = Vec::with_capacity(rows.len());
    let mut masked = 0_usize;
    for row in &rows {
        match ossc_row_from_pg(row) {
            Some(parsed) => out.push(parsed),
            None => masked += 1,
        }
    }
    Ok((out, masked, u64::try_from(source_total).unwrap_or(0)))
}

fn ossc_row_from_pg(row: &tokio_postgres::Row) -> Option<StorePlansOsscRow> {
    // NULL identity columns mean the upstream masked another role's entry.
    let queryid: Option<i64> = row.get("queryid");
    let planid: Option<i64> = row.get("planid");
    Some(StorePlansOsscRow {
        ts: row.get("ts_us"),
        queryid: queryid?,
        planid: planid?,
        userid: row.get("userid"),
        dbid: row.get("dbid"),
        datname: row.get("datname"),
        usename: row.get("usename"),
        plan: row.get("plan"),
        calls: row.get("calls"),
        total_time: row.get("total_time"),
        min_time: row.get("min_time"),
        max_time: row.get("max_time"),
        mean_time: row.get("mean_time"),
        stddev_time: row.get("stddev_time"),
        rows: row.get("rows"),
        shared_blks_hit: row.get("shared_blks_hit"),
        shared_blks_read: row.get("shared_blks_read"),
        shared_blks_dirtied: row.get("shared_blks_dirtied"),
        shared_blks_written: row.get("shared_blks_written"),
        local_blks_hit: row.get("local_blks_hit"),
        local_blks_read: row.get("local_blks_read"),
        local_blks_dirtied: row.get("local_blks_dirtied"),
        local_blks_written: row.get("local_blks_written"),
        temp_blks_read: row.get("temp_blks_read"),
        temp_blks_written: row.get("temp_blks_written"),
        shared_blk_read_time: row.get("shared_blk_read_time"),
        shared_blk_write_time: row.get("shared_blk_write_time"),
        local_blk_read_time: row.get("local_blk_read_time"),
        local_blk_write_time: row.get("local_blk_write_time"),
        temp_blk_read_time: row.get("temp_blk_read_time"),
        temp_blk_write_time: row.get("temp_blk_write_time"),
        first_call: row.get("first_call_us"),
        last_call: row.get("last_call_us"),
    })
}

/// Build a `1_003_001` row, interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_ossc_v1<E>(
    row: &StorePlansOsscRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStorePlansOsscV1, E> {
    Ok(PgStorePlansOsscV1 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        planid: row.planid,
        userid: row.userid,
        dbid: row.dbid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        plan: opt(&mut intern, row.plan.as_deref())?,
        calls: row.calls,
        total_time: row.total_time,
        min_time: row.min_time,
        max_time: row.max_time,
        mean_time: row.mean_time,
        stddev_time: row.stddev_time,
        rows: row.rows,
        shared_blks_hit: row.shared_blks_hit,
        shared_blks_read: row.shared_blks_read,
        shared_blks_dirtied: row.shared_blks_dirtied,
        shared_blks_written: row.shared_blks_written,
        local_blks_hit: row.local_blks_hit,
        local_blks_read: row.local_blks_read,
        local_blks_dirtied: row.local_blks_dirtied,
        local_blks_written: row.local_blks_written,
        temp_blks_read: row.temp_blks_read,
        temp_blks_written: row.temp_blks_written,
        shared_blk_read_time: row.shared_blk_read_time,
        shared_blk_write_time: row.shared_blk_write_time,
        local_blk_read_time: row.local_blk_read_time,
        local_blk_write_time: row.local_blk_write_time,
        temp_blk_read_time: row.temp_blk_read_time,
        temp_blk_write_time: row.temp_blk_write_time,
        first_call: Ts(row.first_call),
        last_call: Ts(row.last_call),
    })
}

#[cfg(test)]
mod ossc_tests {
    use super::{StorePlansOsscRow, store_plans_ossc_query, to_ossc_v1};
    use kronika_registry::StrId;
    use std::convert::Infallible;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the fallible interner signature to_ossc_v1 expects"
    )]
    fn fake_intern(bytes: &[u8]) -> Result<StrId, Infallible> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(StrId(h | 1))
    }

    fn sample_row() -> StorePlansOsscRow {
        StorePlansOsscRow {
            ts: 2_000,
            queryid: 4_242,
            planid: -9,
            userid: 10,
            dbid: 5,
            datname: Some("appdb".to_owned()),
            usename: Some("alice".to_owned()),
            plan: Some("Seq Scan on t".to_owned()),
            calls: 100,
            total_time: 1_234.5,
            min_time: 0.5,
            max_time: 40.0,
            mean_time: 12.3,
            stddev_time: 3.1,
            rows: 5_000,
            shared_blks_hit: 1,
            shared_blks_read: 2,
            shared_blks_dirtied: 3,
            shared_blks_written: 4,
            local_blks_hit: 5,
            local_blks_read: 6,
            local_blks_dirtied: 7,
            local_blks_written: 8,
            temp_blks_read: 9,
            temp_blks_written: 10,
            shared_blk_read_time: 1.5,
            shared_blk_write_time: 2.5,
            local_blk_read_time: 3.5,
            local_blk_write_time: 4.5,
            temp_blk_read_time: 5.5,
            temp_blk_write_time: 6.5,
            first_call: 1_000,
            last_call: 1_999,
        }
    }

    #[test]
    fn ossc_query_truncates_on_the_server_and_caps_by_total_time() {
        let q = store_plans_ossc_query();
        assert!(q.contains("FROM pg_store_plans s"), "{q}");
        assert!(q.contains("left(s.plan, $2::int4) AS plan"), "{q}");
        assert!(q.contains("ORDER BY s.total_time DESC"), "{q}");
        assert!(q.contains("LIMIT $1"), "{q}");
        assert!(
            !q.contains("pg_store_plans(false)"),
            "the upstream view has no showtext argument: {q}"
        );
    }

    #[test]
    fn to_ossc_v1_interns_strings_and_keeps_split_timings() {
        let typed = to_ossc_v1(&sample_row(), fake_intern).unwrap();
        assert_eq!(typed.queryid, 4_242);
        assert!(typed.plan.is_some());
        assert!((typed.temp_blk_write_time - 6.5).abs() < f64::EPSILON);
    }

    #[test]
    fn to_ossc_v1_keeps_dropped_plan_null() {
        let mut row = sample_row();
        row.plan = None;
        let typed = to_ossc_v1(&row, fake_intern).unwrap();
        assert_eq!(typed.plan, None, "a budget-dropped plan stays NULL");
    }
}
