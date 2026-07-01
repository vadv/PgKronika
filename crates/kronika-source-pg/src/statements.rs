//! `pg_stat_statements` collection for types `1_002_001`..`1_002_006`.
//!
//! An instance-wide extension view: one row per `(userid, dbid, queryid)` and,
//! from extension 1.9, also per `toplevel`. The `dbid` distinguishes databases,
//! so one query against the view (from any database that has the extension
//! installed) returns every database's statements. The layout is chosen by the
//! *extension* version reported by `pg_extension`, not the server major, because
//! the extension can be pinned independently of the server.
//!
//! Candidate selection is purely mechanical: the union of top-N statements by
//! `total_exec_time` and by `calls`, so a heavy-by-time and a heavy-by-frequency
//! statement are both kept. The collector records and bounds its output; judging
//! whether a value is dangerous is the analyzer's job.
//!
//! `queryid` and `query` are nullable: `queryid` is `NULL` when
//! `compute_query_id` is off, and `query` is `NULL` for a caller without the
//! privilege to read another role's statement text. `datname` and `usename` are
//! resolved through `LEFT JOIN`, so they are `None` for an oid with no catalog
//! row. Collection returns owned rows; the caller interns the strings into the
//! segment dictionary. The typed layout lives in `kronika-registry`
//! (`PgStatStatementsV1`..`V6`).

use kronika_registry::pg_stat_statements::{
    PgStatStatementsV1, PgStatStatementsV2, PgStatStatementsV3, PgStatStatementsV4,
    PgStatStatementsV5, PgStatStatementsV6,
};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/statements.rs */ ",
            $sql,
        )
    };
}

/// Statement text truncation, in bytes, applied inline in the SELECT.
const QUERY_TRUNCATE: usize = 5000;

/// The `pg_stat_statements` layout selected by the extension version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementsVersion {
    /// Extension &le; 1.7 (PG10-12): type `1_002_001`, the legacy timing names.
    V1,
    /// Extension 1.8 (PG13): type `1_002_002`, exec/plan split plus WAL.
    V2,
    /// Extension 1.9 (PG14): type `1_002_003`, adds `toplevel`.
    V3,
    /// Extension 1.10 (PG15-16): type `1_002_004`, adds temp timing and JIT.
    V4,
    /// Extension 1.11 (PG17): type `1_002_005`, `shared_blk_*_time` rename and
    /// the `local_blk_*_time`, `jit_deform_*`, and `*_stats_since` columns.
    V5,
    /// Extension 1.12 (PG18): type `1_002_006`, adds `wal_buffers_full` and the
    /// parallel-worker counters.
    V6,
}

/// Select the layout for a `pg_stat_statements` extension version string.
///
/// `extversion` is the `"major.minor"` value from `pg_extension.extversion`.
/// Anything below 1.8 maps to the legacy layout; anything at or above 1.12 maps
/// to the newest. A string that does not parse as `major.minor` is treated as
/// the legacy layout, the conservative choice for an unknown build.
#[must_use]
pub fn statements_version(extversion: &str) -> StatementsVersion {
    let (major, minor) = parse_ext_version(extversion).unwrap_or((1, 0));
    match (major, minor) {
        (1, 0..=7) => StatementsVersion::V1,
        (1, 8) => StatementsVersion::V2,
        (1, 9) => StatementsVersion::V3,
        (1, 10) => StatementsVersion::V4,
        (1, 11) => StatementsVersion::V5,
        // 1.12 and up, and any future 2.x, take the newest known layout.
        _ if (major, minor) >= (1, 12) => StatementsVersion::V6,
        // major 0 or an unexpectedly low pair: fall back to the legacy layout.
        _ => StatementsVersion::V1,
    }
}

/// Parse `"major.minor"` into `(major, minor)`; `None` if either part is absent
/// or non-numeric. A bare `"1"` yields minor `0`.
fn parse_ext_version(extversion: &str) -> Option<(u32, u32)> {
    let mut parts = extversion.trim().split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = match parts.next() {
        Some(text) => text.parse::<u32>().ok()?,
        None => 0,
    };
    Some((major, minor))
}

/// The SQL for one layout.
///
/// `$1` is the per-axis top-N row count. Candidate selection is purely
/// mechanical — the union of top-N statements by `total_exec_time` (or, on the
/// legacy layout, `total_time`) and by `calls`. `query` is truncated inline to
/// [`QUERY_TRUNCATE`] bytes and interned; `datname`/`usename` are resolved with a
/// `LEFT JOIN`. `ts` is one `statement_timestamp()` for the whole snapshot; the
/// `*_stats_since` columns come back as unix microseconds. The collector only
/// records the most extreme rows per axis and bounds its own output; no
/// threshold and no "danger" verdict appears in the SQL.
#[allow(
    clippy::too_many_lines,
    reason = "six full per-version SQL literals; splitting the match hurts readability"
)]
#[must_use]
pub fn statements_query(version: StatementsVersion) -> String {
    // The legacy layout orders its time axis by `total_time`; every later layout
    // renamed it to `total_exec_time`.
    let time_axis = match version {
        StatementsVersion::V1 => "total_time",
        _ => "total_exec_time",
    };
    let candidates = format!(
        "WITH candidates AS ( \
           (SELECT userid, dbid, queryid FROM pg_stat_statements ORDER BY {time_axis} DESC NULLS LAST LIMIT $1) \
           UNION \
           (SELECT userid, dbid, queryid FROM pg_stat_statements ORDER BY calls DESC NULLS LAST LIMIT $1) \
         ) "
    );
    // The join back to pg_stat_statements uses IS NOT DISTINCT FROM so a NULL
    // queryid (compute_query_id off) still matches its own candidate row.
    let join = "s JOIN candidates c \
         ON c.userid = s.userid AND c.dbid = s.dbid \
         AND c.queryid IS NOT DISTINCT FROM s.queryid \
         LEFT JOIN pg_database d ON d.oid = s.dbid \
         LEFT JOIN pg_roles r ON r.oid = s.userid";
    let text = format!("LEFT(s.query, {QUERY_TRUNCATE}) AS query");
    let ts = "(extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us";
    let ident = format!(
        "s.queryid, s.userid, s.dbid, d.datname::text AS datname, r.rolname::text AS usename, {text}"
    );
    let body = match version {
        StatementsVersion::V1 => format!(
            "{candidates}SELECT {ident}, \
               s.calls, s.rows, \
               s.total_time, s.min_time, s.max_time, s.mean_time, s.stddev_time, \
               s.shared_blks_hit, s.shared_blks_read, s.shared_blks_dirtied, s.shared_blks_written, \
               s.local_blks_hit, s.local_blks_read, s.local_blks_dirtied, s.local_blks_written, \
               s.temp_blks_read, s.temp_blks_written, \
               s.blk_read_time, s.blk_write_time, \
               {ts} \
             FROM pg_stat_statements {join}"
        ),
        StatementsVersion::V2 => format!(
            "{candidates}SELECT {ident}, \
               s.calls, s.rows, s.plans, \
               s.total_exec_time, s.total_plan_time, \
               s.min_exec_time, s.max_exec_time, s.mean_exec_time, s.stddev_exec_time, \
               s.min_plan_time, s.max_plan_time, s.mean_plan_time, s.stddev_plan_time, \
               s.shared_blks_hit, s.shared_blks_read, s.shared_blks_dirtied, s.shared_blks_written, \
               s.local_blks_hit, s.local_blks_read, s.local_blks_dirtied, s.local_blks_written, \
               s.temp_blks_read, s.temp_blks_written, \
               s.blk_read_time, s.blk_write_time, \
               s.wal_records, s.wal_fpi, s.wal_bytes::int8 AS wal_bytes, \
               {ts} \
             FROM pg_stat_statements {join}"
        ),
        StatementsVersion::V3 => format!(
            "{candidates}SELECT {ident}, s.toplevel, \
               s.calls, s.rows, s.plans, \
               s.total_exec_time, s.total_plan_time, \
               s.min_exec_time, s.max_exec_time, s.mean_exec_time, s.stddev_exec_time, \
               s.min_plan_time, s.max_plan_time, s.mean_plan_time, s.stddev_plan_time, \
               s.shared_blks_hit, s.shared_blks_read, s.shared_blks_dirtied, s.shared_blks_written, \
               s.local_blks_hit, s.local_blks_read, s.local_blks_dirtied, s.local_blks_written, \
               s.temp_blks_read, s.temp_blks_written, \
               s.blk_read_time, s.blk_write_time, \
               s.wal_records, s.wal_fpi, s.wal_bytes::int8 AS wal_bytes, \
               {ts} \
             FROM pg_stat_statements {join}"
        ),
        StatementsVersion::V4 => format!(
            "{candidates}SELECT {ident}, s.toplevel, \
               s.calls, s.rows, s.plans, \
               s.total_exec_time, s.total_plan_time, \
               s.min_exec_time, s.max_exec_time, s.mean_exec_time, s.stddev_exec_time, \
               s.min_plan_time, s.max_plan_time, s.mean_plan_time, s.stddev_plan_time, \
               s.shared_blks_hit, s.shared_blks_read, s.shared_blks_dirtied, s.shared_blks_written, \
               s.local_blks_hit, s.local_blks_read, s.local_blks_dirtied, s.local_blks_written, \
               s.temp_blks_read, s.temp_blks_written, \
               s.blk_read_time, s.blk_write_time, s.temp_blk_read_time, s.temp_blk_write_time, \
               s.wal_records, s.wal_fpi, s.wal_bytes::int8 AS wal_bytes, \
               s.jit_functions, s.jit_generation_time, s.jit_inlining_count, s.jit_inlining_time, \
               s.jit_optimization_count, s.jit_optimization_time, \
               s.jit_emission_count, s.jit_emission_time, \
               {ts} \
             FROM pg_stat_statements {join}"
        ),
        StatementsVersion::V5 => format!(
            "{candidates}SELECT {ident}, s.toplevel, \
               s.calls, s.rows, s.plans, \
               s.total_exec_time, s.total_plan_time, \
               s.min_exec_time, s.max_exec_time, s.mean_exec_time, s.stddev_exec_time, \
               s.min_plan_time, s.max_plan_time, s.mean_plan_time, s.stddev_plan_time, \
               s.shared_blks_hit, s.shared_blks_read, s.shared_blks_dirtied, s.shared_blks_written, \
               s.local_blks_hit, s.local_blks_read, s.local_blks_dirtied, s.local_blks_written, \
               s.temp_blks_read, s.temp_blks_written, \
               s.shared_blk_read_time, s.shared_blk_write_time, \
               s.local_blk_read_time, s.local_blk_write_time, \
               s.temp_blk_read_time, s.temp_blk_write_time, \
               s.wal_records, s.wal_fpi, s.wal_bytes::int8 AS wal_bytes, \
               s.jit_functions, s.jit_generation_time, s.jit_inlining_count, s.jit_inlining_time, \
               s.jit_optimization_count, s.jit_optimization_time, \
               s.jit_emission_count, s.jit_emission_time, \
               s.jit_deform_count, s.jit_deform_time, \
               (extract(epoch from s.stats_since) * 1e6)::int8 AS stats_since_us, \
               (extract(epoch from s.minmax_stats_since) * 1e6)::int8 AS minmax_stats_since_us, \
               {ts} \
             FROM pg_stat_statements {join}"
        ),
        StatementsVersion::V6 => format!(
            "{candidates}SELECT {ident}, s.toplevel, \
               s.calls, s.rows, s.plans, \
               s.total_exec_time, s.total_plan_time, \
               s.min_exec_time, s.max_exec_time, s.mean_exec_time, s.stddev_exec_time, \
               s.min_plan_time, s.max_plan_time, s.mean_plan_time, s.stddev_plan_time, \
               s.shared_blks_hit, s.shared_blks_read, s.shared_blks_dirtied, s.shared_blks_written, \
               s.local_blks_hit, s.local_blks_read, s.local_blks_dirtied, s.local_blks_written, \
               s.temp_blks_read, s.temp_blks_written, \
               s.shared_blk_read_time, s.shared_blk_write_time, \
               s.local_blk_read_time, s.local_blk_write_time, \
               s.temp_blk_read_time, s.temp_blk_write_time, \
               s.wal_records, s.wal_fpi, s.wal_bytes::int8 AS wal_bytes, s.wal_buffers_full, \
               s.jit_functions, s.jit_generation_time, s.jit_inlining_count, s.jit_inlining_time, \
               s.jit_optimization_count, s.jit_optimization_time, \
               s.jit_emission_count, s.jit_emission_time, \
               s.jit_deform_count, s.jit_deform_time, \
               s.parallel_workers_to_launch, s.parallel_workers_launched, \
               (extract(epoch from s.stats_since) * 1e6)::int8 AS stats_since_us, \
               (extract(epoch from s.minmax_stats_since) * 1e6)::int8 AS minmax_stats_since_us, \
               {ts} \
             FROM pg_stat_statements {join}"
        ),
    };
    format!("{}{body}", marked!(""))
}

/// One raw `pg_stat_statements` row, a version-agnostic superset.
///
/// Numbers are owned directly; strings are interned by the caller. Columns
/// absent from the version, and catalog `NULL`s, are `None`. Timing fields carry
/// the value under its version's name (the legacy `total_time` and the later
/// `total_exec_time` share [`Self::total_time`]). See [`PgStatStatementsV6`] for
/// meaning.
#[derive(Debug, Clone)]
pub struct StatementsRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Query id; `None` when `compute_query_id` is off.
    pub queryid: Option<i64>,
    /// Role oid the statement ran as.
    pub userid: u32,
    /// Database oid the statement ran in.
    pub dbid: u32,
    /// Whether the statement ran at the top level (V3+); `None` for &lt;V3.
    pub toplevel: Option<bool>,
    /// Database name resolved from `dbid`; `None` when unresolved.
    pub datname: Option<String>,
    /// Role name resolved from `userid`; `None` when unresolved.
    pub usename: Option<String>,
    /// Statement text (truncated); `None` on insufficient privilege.
    pub query: Option<String>,
    /// Times the statement was executed.
    pub calls: i64,
    /// Rows retrieved or affected.
    pub rows: i64,
    /// Times the statement was planned (V2+); `None` for &lt;V2.
    pub plans: Option<i64>,
    /// Total time (`total_time` on &lt;V2, `total_exec_time` on V2+), ms.
    pub total_time: f64,
    /// Total planning time (V2+), ms; `None` for &lt;V2.
    pub total_plan_time: Option<f64>,
    /// Minimum time (`min_time`/`min_exec_time`), ms.
    pub min_time: f64,
    /// Maximum time (`max_time`/`max_exec_time`), ms.
    pub max_time: f64,
    /// Mean time (`mean_time`/`mean_exec_time`), ms.
    pub mean_time: f64,
    /// Std-dev of time (`stddev_time`/`stddev_exec_time`), ms.
    pub stddev_time: f64,
    /// Minimum planning time (V2+), ms; `None` for &lt;V2.
    pub min_plan_time: Option<f64>,
    /// Maximum planning time (V2+), ms; `None` for &lt;V2.
    pub max_plan_time: Option<f64>,
    /// Mean planning time (V2+), ms; `None` for &lt;V2.
    pub mean_plan_time: Option<f64>,
    /// Std-dev of planning time (V2+), ms; `None` for &lt;V2.
    pub stddev_plan_time: Option<f64>,
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
    /// Shared/legacy block read time (`blk_read_time` on &lt;V5,
    /// `shared_blk_read_time` on V5+), ms.
    pub blk_read_time: f64,
    /// Shared/legacy block write time, ms. See [`Self::blk_read_time`].
    pub blk_write_time: f64,
    /// Local block read time (V5+), ms; `None` for &lt;V5.
    pub local_blk_read_time: Option<f64>,
    /// Local block write time (V5+), ms; `None` for &lt;V5.
    pub local_blk_write_time: Option<f64>,
    /// Temp block read time (V4+), ms; `None` for &lt;V4.
    pub temp_blk_read_time: Option<f64>,
    /// Temp block write time (V4+), ms; `None` for &lt;V4.
    pub temp_blk_write_time: Option<f64>,
    /// WAL records (V2+); `None` for &lt;V2.
    pub wal_records: Option<i64>,
    /// WAL full-page images (V2+); `None` for &lt;V2.
    pub wal_fpi: Option<i64>,
    /// WAL bytes (V2+); `None` for &lt;V2.
    pub wal_bytes: Option<i64>,
    /// Full-WAL-buffer waits (V6+); `None` for &lt;V6.
    pub wal_buffers_full: Option<i64>,
    /// JIT-compiled functions (V4+); `None` for &lt;V4.
    pub jit_functions: Option<i64>,
    /// JIT generation time (V4+), ms; `None` for &lt;V4.
    pub jit_generation_time: Option<f64>,
    /// JIT inlining passes (V4+); `None` for &lt;V4.
    pub jit_inlining_count: Option<i64>,
    /// JIT inlining time (V4+), ms; `None` for &lt;V4.
    pub jit_inlining_time: Option<f64>,
    /// JIT optimization passes (V4+); `None` for &lt;V4.
    pub jit_optimization_count: Option<i64>,
    /// JIT optimization time (V4+), ms; `None` for &lt;V4.
    pub jit_optimization_time: Option<f64>,
    /// JIT emissions (V4+); `None` for &lt;V4.
    pub jit_emission_count: Option<i64>,
    /// JIT emission time (V4+), ms; `None` for &lt;V4.
    pub jit_emission_time: Option<f64>,
    /// JIT deform passes (V5+); `None` for &lt;V5.
    pub jit_deform_count: Option<i64>,
    /// JIT deform time (V5+), ms; `None` for &lt;V5.
    pub jit_deform_time: Option<f64>,
    /// Parallel workers planned (V6+); `None` for &lt;V6.
    pub parallel_workers_to_launch: Option<i64>,
    /// Parallel workers launched (V6+); `None` for &lt;V6.
    pub parallel_workers_launched: Option<i64>,
    /// When this row's statistics began accumulating (V5+), unix microseconds.
    pub stats_since: Option<i64>,
    /// When this row's min/max statistics were last reset (V5+), unix microseconds.
    pub minmax_stats_since: Option<i64>,
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

/// Build a `1_002_006` row (extension 1.12, PG18), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v6<E>(
    row: &StatementsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatStatementsV6, E> {
    Ok(PgStatStatementsV6 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        userid: row.userid,
        dbid: row.dbid,
        toplevel: row.toplevel.unwrap_or(true),
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        calls: row.calls,
        rows: row.rows,
        plans: row.plans.unwrap_or(0),
        total_exec_time: row.total_time,
        total_plan_time: row.total_plan_time.unwrap_or(0.0),
        min_exec_time: row.min_time,
        max_exec_time: row.max_time,
        mean_exec_time: row.mean_time,
        stddev_exec_time: row.stddev_time,
        min_plan_time: row.min_plan_time.unwrap_or(0.0),
        max_plan_time: row.max_plan_time.unwrap_or(0.0),
        mean_plan_time: row.mean_plan_time.unwrap_or(0.0),
        stddev_plan_time: row.stddev_plan_time.unwrap_or(0.0),
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
        shared_blk_read_time: row.blk_read_time,
        shared_blk_write_time: row.blk_write_time,
        local_blk_read_time: row.local_blk_read_time.unwrap_or(0.0),
        local_blk_write_time: row.local_blk_write_time.unwrap_or(0.0),
        temp_blk_read_time: row.temp_blk_read_time.unwrap_or(0.0),
        temp_blk_write_time: row.temp_blk_write_time.unwrap_or(0.0),
        wal_records: row.wal_records.unwrap_or(0),
        wal_fpi: row.wal_fpi.unwrap_or(0),
        wal_bytes: row.wal_bytes.unwrap_or(0),
        wal_buffers_full: row.wal_buffers_full.unwrap_or(0),
        jit_functions: row.jit_functions.unwrap_or(0),
        jit_generation_time: row.jit_generation_time.unwrap_or(0.0),
        jit_inlining_count: row.jit_inlining_count.unwrap_or(0),
        jit_inlining_time: row.jit_inlining_time.unwrap_or(0.0),
        jit_optimization_count: row.jit_optimization_count.unwrap_or(0),
        jit_optimization_time: row.jit_optimization_time.unwrap_or(0.0),
        jit_emission_count: row.jit_emission_count.unwrap_or(0),
        jit_emission_time: row.jit_emission_time.unwrap_or(0.0),
        jit_deform_count: row.jit_deform_count.unwrap_or(0),
        jit_deform_time: row.jit_deform_time.unwrap_or(0.0),
        parallel_workers_to_launch: row.parallel_workers_to_launch.unwrap_or(0),
        parallel_workers_launched: row.parallel_workers_launched.unwrap_or(0),
        stats_since: row.stats_since.map(Ts),
        minmax_stats_since: row.minmax_stats_since.map(Ts),
    })
}

/// Build a `1_002_005` row (extension 1.11, PG17), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v5<E>(
    row: &StatementsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatStatementsV5, E> {
    Ok(PgStatStatementsV5 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        userid: row.userid,
        dbid: row.dbid,
        toplevel: row.toplevel.unwrap_or(true),
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        calls: row.calls,
        rows: row.rows,
        plans: row.plans.unwrap_or(0),
        total_exec_time: row.total_time,
        total_plan_time: row.total_plan_time.unwrap_or(0.0),
        min_exec_time: row.min_time,
        max_exec_time: row.max_time,
        mean_exec_time: row.mean_time,
        stddev_exec_time: row.stddev_time,
        min_plan_time: row.min_plan_time.unwrap_or(0.0),
        max_plan_time: row.max_plan_time.unwrap_or(0.0),
        mean_plan_time: row.mean_plan_time.unwrap_or(0.0),
        stddev_plan_time: row.stddev_plan_time.unwrap_or(0.0),
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
        shared_blk_read_time: row.blk_read_time,
        shared_blk_write_time: row.blk_write_time,
        local_blk_read_time: row.local_blk_read_time.unwrap_or(0.0),
        local_blk_write_time: row.local_blk_write_time.unwrap_or(0.0),
        temp_blk_read_time: row.temp_blk_read_time.unwrap_or(0.0),
        temp_blk_write_time: row.temp_blk_write_time.unwrap_or(0.0),
        wal_records: row.wal_records.unwrap_or(0),
        wal_fpi: row.wal_fpi.unwrap_or(0),
        wal_bytes: row.wal_bytes.unwrap_or(0),
        jit_functions: row.jit_functions.unwrap_or(0),
        jit_generation_time: row.jit_generation_time.unwrap_or(0.0),
        jit_inlining_count: row.jit_inlining_count.unwrap_or(0),
        jit_inlining_time: row.jit_inlining_time.unwrap_or(0.0),
        jit_optimization_count: row.jit_optimization_count.unwrap_or(0),
        jit_optimization_time: row.jit_optimization_time.unwrap_or(0.0),
        jit_emission_count: row.jit_emission_count.unwrap_or(0),
        jit_emission_time: row.jit_emission_time.unwrap_or(0.0),
        jit_deform_count: row.jit_deform_count.unwrap_or(0),
        jit_deform_time: row.jit_deform_time.unwrap_or(0.0),
        stats_since: row.stats_since.map(Ts),
        minmax_stats_since: row.minmax_stats_since.map(Ts),
    })
}

/// Build a `1_002_004` row (extension 1.10, PG15-16), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v4<E>(
    row: &StatementsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatStatementsV4, E> {
    Ok(PgStatStatementsV4 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        userid: row.userid,
        dbid: row.dbid,
        toplevel: row.toplevel.unwrap_or(true),
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        calls: row.calls,
        rows: row.rows,
        plans: row.plans.unwrap_or(0),
        total_exec_time: row.total_time,
        total_plan_time: row.total_plan_time.unwrap_or(0.0),
        min_exec_time: row.min_time,
        max_exec_time: row.max_time,
        mean_exec_time: row.mean_time,
        stddev_exec_time: row.stddev_time,
        min_plan_time: row.min_plan_time.unwrap_or(0.0),
        max_plan_time: row.max_plan_time.unwrap_or(0.0),
        mean_plan_time: row.mean_plan_time.unwrap_or(0.0),
        stddev_plan_time: row.stddev_plan_time.unwrap_or(0.0),
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
        temp_blk_read_time: row.temp_blk_read_time.unwrap_or(0.0),
        temp_blk_write_time: row.temp_blk_write_time.unwrap_or(0.0),
        wal_records: row.wal_records.unwrap_or(0),
        wal_fpi: row.wal_fpi.unwrap_or(0),
        wal_bytes: row.wal_bytes.unwrap_or(0),
        jit_functions: row.jit_functions.unwrap_or(0),
        jit_generation_time: row.jit_generation_time.unwrap_or(0.0),
        jit_inlining_count: row.jit_inlining_count.unwrap_or(0),
        jit_inlining_time: row.jit_inlining_time.unwrap_or(0.0),
        jit_optimization_count: row.jit_optimization_count.unwrap_or(0),
        jit_optimization_time: row.jit_optimization_time.unwrap_or(0.0),
        jit_emission_count: row.jit_emission_count.unwrap_or(0),
        jit_emission_time: row.jit_emission_time.unwrap_or(0.0),
    })
}

/// Build a `1_002_003` row (extension 1.9, PG14), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v3<E>(
    row: &StatementsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatStatementsV3, E> {
    Ok(PgStatStatementsV3 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        userid: row.userid,
        dbid: row.dbid,
        toplevel: row.toplevel.unwrap_or(true),
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        calls: row.calls,
        rows: row.rows,
        plans: row.plans.unwrap_or(0),
        total_exec_time: row.total_time,
        total_plan_time: row.total_plan_time.unwrap_or(0.0),
        min_exec_time: row.min_time,
        max_exec_time: row.max_time,
        mean_exec_time: row.mean_time,
        stddev_exec_time: row.stddev_time,
        min_plan_time: row.min_plan_time.unwrap_or(0.0),
        max_plan_time: row.max_plan_time.unwrap_or(0.0),
        mean_plan_time: row.mean_plan_time.unwrap_or(0.0),
        stddev_plan_time: row.stddev_plan_time.unwrap_or(0.0),
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
        wal_records: row.wal_records.unwrap_or(0),
        wal_fpi: row.wal_fpi.unwrap_or(0),
        wal_bytes: row.wal_bytes.unwrap_or(0),
    })
}

/// Build a `1_002_002` row (extension 1.8, PG13), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v2<E>(
    row: &StatementsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatStatementsV2, E> {
    Ok(PgStatStatementsV2 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        userid: row.userid,
        dbid: row.dbid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        calls: row.calls,
        rows: row.rows,
        plans: row.plans.unwrap_or(0),
        total_exec_time: row.total_time,
        total_plan_time: row.total_plan_time.unwrap_or(0.0),
        min_exec_time: row.min_time,
        max_exec_time: row.max_time,
        mean_exec_time: row.mean_time,
        stddev_exec_time: row.stddev_time,
        min_plan_time: row.min_plan_time.unwrap_or(0.0),
        max_plan_time: row.max_plan_time.unwrap_or(0.0),
        mean_plan_time: row.mean_plan_time.unwrap_or(0.0),
        stddev_plan_time: row.stddev_plan_time.unwrap_or(0.0),
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
        wal_records: row.wal_records.unwrap_or(0),
        wal_fpi: row.wal_fpi.unwrap_or(0),
        wal_bytes: row.wal_bytes.unwrap_or(0),
    })
}

/// Build a `1_002_001` row (extension &le; 1.7, PG10-12), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v1<E>(
    row: &StatementsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatStatementsV1, E> {
    Ok(PgStatStatementsV1 {
        ts: Ts(row.ts),
        queryid: row.queryid,
        userid: row.userid,
        dbid: row.dbid,
        datname: opt(&mut intern, row.datname.as_deref())?,
        usename: opt(&mut intern, row.usename.as_deref())?,
        query: opt(&mut intern, row.query.as_deref())?,
        calls: row.calls,
        rows: row.rows,
        total_time: row.total_time,
        min_time: row.min_time,
        max_time: row.max_time,
        mean_time: row.mean_time,
        stddev_time: row.stddev_time,
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
    })
}

/// Read a raw row from a result row using the version's column set.
fn row_from_pg(row: &tokio_postgres::Row, version: StatementsVersion) -> StatementsRow {
    let has_split = !matches!(version, StatementsVersion::V1);
    let has_toplevel = matches!(
        version,
        StatementsVersion::V3
            | StatementsVersion::V4
            | StatementsVersion::V5
            | StatementsVersion::V6
    );
    let has_temp_time = matches!(
        version,
        StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
    );
    let has_jit = has_temp_time;
    let has_pg17 = matches!(version, StatementsVersion::V5 | StatementsVersion::V6);
    let has_pg18 = matches!(version, StatementsVersion::V6);
    StatementsRow {
        ts: row.get("ts_us"),
        queryid: row.get("queryid"),
        userid: row.get("userid"),
        dbid: row.get("dbid"),
        toplevel: has_toplevel.then(|| row.get("toplevel")),
        datname: row.get("datname"),
        usename: row.get("usename"),
        query: row.get("query"),
        calls: row.get("calls"),
        rows: row.get("rows"),
        plans: has_split.then(|| row.get("plans")),
        // The legacy column is `total_time`; V2+ renamed it to `total_exec_time`.
        total_time: match version {
            StatementsVersion::V1 => row.get("total_time"),
            _ => row.get("total_exec_time"),
        },
        total_plan_time: has_split.then(|| row.get("total_plan_time")),
        min_time: match version {
            StatementsVersion::V1 => row.get("min_time"),
            _ => row.get("min_exec_time"),
        },
        max_time: match version {
            StatementsVersion::V1 => row.get("max_time"),
            _ => row.get("max_exec_time"),
        },
        mean_time: match version {
            StatementsVersion::V1 => row.get("mean_time"),
            _ => row.get("mean_exec_time"),
        },
        stddev_time: match version {
            StatementsVersion::V1 => row.get("stddev_time"),
            _ => row.get("stddev_exec_time"),
        },
        min_plan_time: has_split.then(|| row.get("min_plan_time")),
        max_plan_time: has_split.then(|| row.get("max_plan_time")),
        mean_plan_time: has_split.then(|| row.get("mean_plan_time")),
        stddev_plan_time: has_split.then(|| row.get("stddev_plan_time")),
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
        // Pre-1.11 the pair is `blk_read_time`; 1.11+ renamed it to
        // `shared_blk_read_time`.
        blk_read_time: if has_pg17 {
            row.get("shared_blk_read_time")
        } else {
            row.get("blk_read_time")
        },
        blk_write_time: if has_pg17 {
            row.get("shared_blk_write_time")
        } else {
            row.get("blk_write_time")
        },
        local_blk_read_time: has_pg17.then(|| row.get("local_blk_read_time")),
        local_blk_write_time: has_pg17.then(|| row.get("local_blk_write_time")),
        temp_blk_read_time: has_temp_time.then(|| row.get("temp_blk_read_time")),
        temp_blk_write_time: has_temp_time.then(|| row.get("temp_blk_write_time")),
        wal_records: has_split.then(|| row.get("wal_records")),
        wal_fpi: has_split.then(|| row.get("wal_fpi")),
        wal_bytes: has_split.then(|| row.get("wal_bytes")),
        wal_buffers_full: has_pg18.then(|| row.get("wal_buffers_full")),
        jit_functions: has_jit.then(|| row.get("jit_functions")),
        jit_generation_time: has_jit.then(|| row.get("jit_generation_time")),
        jit_inlining_count: has_jit.then(|| row.get("jit_inlining_count")),
        jit_inlining_time: has_jit.then(|| row.get("jit_inlining_time")),
        jit_optimization_count: has_jit.then(|| row.get("jit_optimization_count")),
        jit_optimization_time: has_jit.then(|| row.get("jit_optimization_time")),
        jit_emission_count: has_jit.then(|| row.get("jit_emission_count")),
        jit_emission_time: has_jit.then(|| row.get("jit_emission_time")),
        jit_deform_count: has_pg17.then(|| row.get("jit_deform_count")),
        jit_deform_time: has_pg17.then(|| row.get("jit_deform_time")),
        parallel_workers_to_launch: has_pg18.then(|| row.get("parallel_workers_to_launch")),
        parallel_workers_launched: has_pg18.then(|| row.get("parallel_workers_launched")),
        stats_since: has_pg17.then(|| row.get("stats_since_us")).flatten(),
        minmax_stats_since: has_pg17.then(|| row.get("minmax_stats_since_us")).flatten(),
    }
}

/// Read the installed `pg_stat_statements` extension version on this connection.
///
/// Returns `None` when the extension is not installed in the connected database.
/// The version string is `pg_extension.extversion` (e.g. `"1.11"`); pass it to
/// [`statements_version`] to select the layout.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the probe query fails.
pub async fn statements_extversion(
    client: &Client,
) -> Result<Option<String>, tokio_postgres::Error> {
    let row = client
        .query_opt(
            marked!("SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements'"),
            &[],
        )
        .await?;
    Ok(row.map(|row| row.get("extversion")))
}

/// Collect a `pg_stat_statements` snapshot from one connection.
///
/// The view is instance-wide, so this runs once against whichever connection has
/// the extension installed. Returns the raw rows; the caller interns the strings
/// and builds the typed rows. `max_statements` is the per-axis top-N row count.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_statements(
    client: &Client,
    version: StatementsVersion,
    max_statements: i64,
) -> Result<Vec<StatementsRow>, tokio_postgres::Error> {
    let rows = client
        .query(&statements_query(version), &[&max_statements])
        .await?;
    Ok(rows.iter().map(|row| row_from_pg(row, version)).collect())
}

#[cfg(test)]
mod tests {
    use super::{
        StatementsRow, StatementsVersion, statements_query, statements_version, to_v1, to_v2, to_v6,
    };
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

    fn sample_row() -> StatementsRow {
        StatementsRow {
            ts: 2_000,
            queryid: Some(777),
            userid: 10,
            dbid: 5,
            toplevel: Some(true),
            datname: Some("appdb".to_owned()),
            usename: Some("alice".to_owned()),
            query: Some("select 1".to_owned()),
            calls: 100,
            rows: 5_000,
            plans: Some(90),
            total_time: 1_234.5,
            total_plan_time: Some(12.5),
            min_time: 0.5,
            max_time: 40.0,
            mean_time: 12.3,
            stddev_time: 3.1,
            min_plan_time: Some(0.1),
            max_plan_time: Some(1.0),
            mean_plan_time: Some(0.2),
            stddev_plan_time: Some(0.05),
            shared_blks_hit: 90_000,
            shared_blks_read: 4_000,
            shared_blks_dirtied: 50,
            shared_blks_written: 30,
            local_blks_hit: 0,
            local_blks_read: 0,
            local_blks_dirtied: 0,
            local_blks_written: 0,
            temp_blks_read: 0,
            temp_blks_written: 0,
            blk_read_time: 12.5,
            blk_write_time: 3.0,
            local_blk_read_time: Some(1.0),
            local_blk_write_time: Some(0.5),
            temp_blk_read_time: Some(2.0),
            temp_blk_write_time: Some(1.5),
            wal_records: Some(42),
            wal_fpi: Some(3),
            wal_bytes: Some(8_192),
            wal_buffers_full: Some(1),
            jit_functions: Some(0),
            jit_generation_time: Some(0.0),
            jit_inlining_count: Some(0),
            jit_inlining_time: Some(0.0),
            jit_optimization_count: Some(0),
            jit_optimization_time: Some(0.0),
            jit_emission_count: Some(0),
            jit_emission_time: Some(0.0),
            jit_deform_count: Some(0),
            jit_deform_time: Some(0.0),
            parallel_workers_to_launch: Some(4),
            parallel_workers_launched: Some(3),
            stats_since: Some(1_500),
            minmax_stats_since: Some(1_800),
        }
    }

    #[test]
    fn version_follows_extension_version() {
        assert_eq!(statements_version("1.6"), StatementsVersion::V1);
        assert_eq!(statements_version("1.7"), StatementsVersion::V1);
        assert_eq!(statements_version("1.8"), StatementsVersion::V2);
        assert_eq!(statements_version("1.9"), StatementsVersion::V3);
        assert_eq!(statements_version("1.10"), StatementsVersion::V4);
        assert_eq!(statements_version("1.11"), StatementsVersion::V5);
        assert_eq!(statements_version("1.12"), StatementsVersion::V6);
        // A future minor maps to the newest known layout.
        assert_eq!(statements_version("1.13"), StatementsVersion::V6);
        // A future major likewise maps to the newest known layout.
        assert_eq!(statements_version("2.0"), StatementsVersion::V6);
    }

    #[test]
    fn version_of_unparseable_string_is_legacy() {
        assert_eq!(statements_version(""), StatementsVersion::V1);
        assert_eq!(statements_version("garbage"), StatementsVersion::V1);
        // A bare major parses with minor 0, which is the legacy layout.
        assert_eq!(statements_version("1"), StatementsVersion::V1);
    }

    #[test]
    fn query_has_version_specific_columns_and_marker() {
        // The time axis uses the legacy name only on V1.
        assert!(statements_query(StatementsVersion::V1).contains("ORDER BY total_time DESC"));
        assert!(!statements_query(StatementsVersion::V1).contains("total_exec_time"));
        assert!(statements_query(StatementsVersion::V2).contains("ORDER BY total_exec_time DESC"));

        assert!(!statements_query(StatementsVersion::V2).contains("s.toplevel"));
        assert!(statements_query(StatementsVersion::V3).contains("s.toplevel"));

        assert!(!statements_query(StatementsVersion::V3).contains("temp_blk_read_time"));
        assert!(statements_query(StatementsVersion::V4).contains("temp_blk_read_time"));
        assert!(statements_query(StatementsVersion::V4).contains("jit_emission_time"));

        assert!(!statements_query(StatementsVersion::V4).contains("shared_blk_read_time"));
        assert!(statements_query(StatementsVersion::V5).contains("shared_blk_read_time"));
        assert!(statements_query(StatementsVersion::V5).contains("jit_deform_count"));
        assert!(statements_query(StatementsVersion::V5).contains("stats_since"));

        assert!(!statements_query(StatementsVersion::V5).contains("wal_buffers_full"));
        assert!(statements_query(StatementsVersion::V6).contains("wal_buffers_full"));
        assert!(statements_query(StatementsVersion::V6).contains("parallel_workers_launched"));

        for v in [
            StatementsVersion::V1,
            StatementsVersion::V2,
            StatementsVersion::V3,
            StatementsVersion::V4,
            StatementsVersion::V5,
            StatementsVersion::V6,
        ] {
            let q = statements_query(v);
            assert!(q.contains("pg_kronika"));
            assert!(q.contains("pg_stat_statements"));
            assert!(q.contains("LEFT(s.query, 5000)"));
            assert!(q.contains("LEFT JOIN pg_database"));
            assert!(q.contains("LEFT JOIN pg_roles"));
            // Candidate selection is mechanical top-N by raw columns, two axes.
            assert!(q.contains("ORDER BY calls DESC"));
            // A NULL queryid still matches its candidate row.
            assert!(q.contains("IS NOT DISTINCT FROM"));
            // No threshold verdict, no GUC-based branch in the SQL.
            assert!(!q.contains("current_setting"));
        }
    }

    #[test]
    fn to_v6_maps_every_column_and_interns_strings() {
        let r = to_v6(&sample_row(), fake_intern).expect("infallible intern");
        assert_eq!(r.ts.0, 2_000);
        assert_eq!(r.queryid, Some(777));
        assert_eq!(r.userid, 10);
        assert_eq!(r.dbid, 5);
        assert!(r.toplevel);
        assert_eq!(r.datname, Some(fake_intern(b"appdb").unwrap()));
        assert_eq!(r.usename, Some(fake_intern(b"alice").unwrap()));
        assert_eq!(r.query, Some(fake_intern(b"select 1").unwrap()));
        assert!((r.total_exec_time - 1_234.5).abs() < f64::EPSILON);
        assert!((r.shared_blk_read_time - 12.5).abs() < f64::EPSILON);
        assert!((r.local_blk_read_time - 1.0).abs() < f64::EPSILON);
        assert_eq!(r.wal_buffers_full, 1);
        assert_eq!(r.parallel_workers_launched, 3);
        assert_eq!(r.stats_since.map(|t| t.0), Some(1_500));
    }

    #[test]
    fn to_v6_defaults_missing_optionals_to_zero() {
        // A pre-1.12 raw row (planning/jit/parallel absent) still builds a V6
        // struct, defaulting the absent counters to 0 rather than dropping them.
        let mut row = sample_row();
        row.plans = None;
        row.total_plan_time = None;
        row.wal_buffers_full = None;
        row.parallel_workers_to_launch = None;
        row.parallel_workers_launched = None;
        row.jit_deform_count = None;
        let r = to_v6(&row, fake_intern).expect("intern");
        assert_eq!(r.plans, 0);
        assert!((r.total_plan_time - 0.0).abs() < f64::EPSILON);
        assert_eq!(r.wal_buffers_full, 0);
        assert_eq!(r.parallel_workers_launched, 0);
        assert_eq!(r.jit_deform_count, 0);
    }

    #[test]
    fn to_v2_keeps_exec_plan_split_and_wal() {
        let r = to_v2(&sample_row(), fake_intern).expect("intern");
        assert_eq!(r.plans, 90);
        assert!((r.total_plan_time - 12.5).abs() < f64::EPSILON);
        assert_eq!(r.wal_records, 42);
        assert_eq!(r.wal_bytes, 8_192);
        assert_eq!(r.datname, Some(fake_intern(b"appdb").unwrap()));
    }

    #[test]
    fn to_v1_maps_legacy_timing_and_preserves_null_query() {
        let mut row = sample_row();
        row.query = None;
        row.queryid = None;
        let r = to_v1(&row, fake_intern).expect("intern");
        assert!((r.total_time - 1_234.5).abs() < f64::EPSILON);
        assert!((r.mean_time - 12.3).abs() < f64::EPSILON);
        assert_eq!(r.query, None);
        assert_eq!(r.queryid, None);
        assert_eq!(r.userid, 10);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_v6(&sample_row(), boom), Err("full"));
    }
}
