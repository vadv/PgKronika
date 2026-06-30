//! `pg_stat_user_tables` collection for types `1_013_001`..`1_013_004`.
//!
//! Per-table statistics, collected per database through the connection pool. In
//! PG 10-18 the column set only grows: `n_ins_since_vacuum` arrives in PG13;
//! `n_tup_newpage_upd` plus the `last_seq_scan`/`last_idx_scan` timestamps in
//! PG16; the four cumulative vacuum/analyze timing columns in PG18. The major
//! version selects both the SQL and the layout.
//!
//! Candidate selection is purely mechanical: the union of top-N tables by raw
//! columns (read activity, write volume, size, dead tuples, transaction-id age,
//! multixact age). Age axes let old tables be selected even without foreground
//! activity. The collector records and bounds its output; threshold decisions
//! belong to the analyzer. Collection returns owned rows; the caller interns the
//! strings into the segment dictionary. The typed layout is defined in
//! `kronika-registry` (`PgStatUserTablesV1`..`V4`).

use kronika_registry::pg_stat_user_tables::{
    PgStatUserTablesV1, PgStatUserTablesV2, PgStatUserTablesV3, PgStatUserTablesV4,
};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the kronika marker (SQL-transparency rule).
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/user_tables.rs */ ",
            $sql,
        )
    };
}

/// The `pg_stat_user_tables` layout selected by the server major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTablesVersion {
    /// PG 10-12: type `1_013_001` (base layout).
    V1,
    /// PG 13-15: type `1_013_002` (adds `n_ins_since_vacuum`).
    V2,
    /// PG 16-17: type `1_013_003` (adds `n_tup_newpage_upd`, `last_seq_scan`, `last_idx_scan`).
    V3,
    /// PG 18: type `1_013_004` (adds the four cumulative vacuum/analyze timing columns).
    V4,
}

/// Select the layout for a server major version.
///
/// `n_ins_since_vacuum` arrived in PG13; `n_tup_newpage_upd` and the
/// `last_seq_scan`/`last_idx_scan` timestamps in PG16; the four `total_*_time`
/// columns in PG18.
#[must_use]
pub const fn user_tables_version(major: u32) -> UserTablesVersion {
    if major >= 18 {
        UserTablesVersion::V4
    } else if major >= 16 {
        UserTablesVersion::V3
    } else if major >= 13 {
        UserTablesVersion::V2
    } else {
        UserTablesVersion::V1
    }
}

/// The SQL for one layout.
///
/// `$1` is the per-axis top-N row count. Candidate selection is purely
/// mechanical — the union of top-N tables by raw columns (read activity, write
/// volume, size, dead tuples, transaction-id age, multixact age). The write axis
/// orders by `n_tup_ins + n_tup_upd + n_tup_del` so a write-only table is kept
/// even on PG16+, where the activity axis is `GREATEST(last_seq_scan,
/// last_idx_scan)` (read recency). The collector records the highest-ranked
/// rows per axis and bounds its own output; threshold decisions belong to the
/// analyzer. `ts` is one `statement_timestamp()` for the snapshot; the `last_*`
/// columns come back as unix microseconds.
#[allow(
    clippy::too_many_lines,
    reason = "four full per-version SQL literals; splitting the match hurts readability"
)]
#[must_use]
pub const fn user_tables_query(version: UserTablesVersion) -> &'static str {
    match version {
        UserTablesVersion::V1 => marked!(
            "WITH candidates AS ( \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY COALESCE(seq_scan,0) + COALESCE(idx_scan,0) + COALESCE(n_tup_ins,0) \
                         + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY COALESCE(n_tup_ins,0) + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY c.relpages DESC LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables ORDER BY COALESCE(n_dead_tup, 0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY age(c.relfrozenxid) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY mxid_age(c.relminmxid) DESC LIMIT $1) \
             ) \
             SELECT \
               (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid, \
               t.relid, \
               t.schemaname::text AS schemaname, t.relname::text AS relname, \
               COALESCE(ts.spcname, 'pg_default')::text AS tablespace, \
               t.seq_scan, t.seq_tup_read, t.idx_scan, t.idx_tup_fetch, \
               t.n_tup_ins, t.n_tup_upd, t.n_tup_del, t.n_tup_hot_upd, \
               t.n_live_tup, t.n_dead_tup, t.n_mod_since_analyze, \
               t.vacuum_count, t.autovacuum_count, t.analyze_count, t.autoanalyze_count, \
               (extract(epoch from t.last_vacuum) * 1e6)::int8 AS last_vacuum_us, \
               (extract(epoch from t.last_autovacuum) * 1e6)::int8 AS last_autovacuum_us, \
               (extract(epoch from t.last_analyze) * 1e6)::int8 AS last_analyze_us, \
               (extract(epoch from t.last_autoanalyze) * 1e6)::int8 AS last_autoanalyze_us, \
               pg_relation_size(t.relid)::int8 AS main_fork_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_total_relation_size(cl.reltoastrelid)::int8 END AS toast_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_live_tuples(cl.reltoastrelid) END AS toast_n_live_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_dead_tuples(cl.reltoastrelid) END AS toast_n_dead_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN (extract(epoch from pg_stat_get_last_autovacuum_time(cl.reltoastrelid)) * 1e6)::int8 END AS toast_last_autovacuum_us, \
               age(cl.relfrozenxid)::int8 AS xid_age, mxid_age(cl.relminmxid)::int8 AS mxid_age, cl.reltuples::int8 AS reltuples, \
               io.heap_blks_read, io.heap_blks_hit, io.idx_blks_read, io.idx_blks_hit, \
               io.toast_blks_read, io.toast_blks_hit, io.tidx_blks_read, io.tidx_blks_hit, \
               (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_user_tables t \
             JOIN candidates cand ON cand.relid = t.relid \
             LEFT JOIN pg_class cl ON cl.oid = t.relid \
             LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace \
             LEFT JOIN pg_statio_user_tables io ON io.relid = t.relid"
        ),
        UserTablesVersion::V2 => marked!(
            "WITH candidates AS ( \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY COALESCE(seq_scan,0) + COALESCE(idx_scan,0) + COALESCE(n_tup_ins,0) \
                         + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY COALESCE(n_tup_ins,0) + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY c.relpages DESC LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables ORDER BY COALESCE(n_dead_tup, 0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY age(c.relfrozenxid) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY mxid_age(c.relminmxid) DESC LIMIT $1) \
             ) \
             SELECT \
               (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid, \
               t.relid, \
               t.schemaname::text AS schemaname, t.relname::text AS relname, \
               COALESCE(ts.spcname, 'pg_default')::text AS tablespace, \
               t.seq_scan, t.seq_tup_read, t.idx_scan, t.idx_tup_fetch, \
               t.n_tup_ins, t.n_tup_upd, t.n_tup_del, t.n_tup_hot_upd, \
               t.n_live_tup, t.n_dead_tup, t.n_mod_since_analyze, t.n_ins_since_vacuum, \
               t.vacuum_count, t.autovacuum_count, t.analyze_count, t.autoanalyze_count, \
               (extract(epoch from t.last_vacuum) * 1e6)::int8 AS last_vacuum_us, \
               (extract(epoch from t.last_autovacuum) * 1e6)::int8 AS last_autovacuum_us, \
               (extract(epoch from t.last_analyze) * 1e6)::int8 AS last_analyze_us, \
               (extract(epoch from t.last_autoanalyze) * 1e6)::int8 AS last_autoanalyze_us, \
               pg_relation_size(t.relid)::int8 AS main_fork_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_total_relation_size(cl.reltoastrelid)::int8 END AS toast_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_live_tuples(cl.reltoastrelid) END AS toast_n_live_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_dead_tuples(cl.reltoastrelid) END AS toast_n_dead_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN (extract(epoch from pg_stat_get_last_autovacuum_time(cl.reltoastrelid)) * 1e6)::int8 END AS toast_last_autovacuum_us, \
               age(cl.relfrozenxid)::int8 AS xid_age, mxid_age(cl.relminmxid)::int8 AS mxid_age, cl.reltuples::int8 AS reltuples, \
               io.heap_blks_read, io.heap_blks_hit, io.idx_blks_read, io.idx_blks_hit, \
               io.toast_blks_read, io.toast_blks_hit, io.tidx_blks_read, io.tidx_blks_hit, \
               (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_user_tables t \
             JOIN candidates cand ON cand.relid = t.relid \
             LEFT JOIN pg_class cl ON cl.oid = t.relid \
             LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace \
             LEFT JOIN pg_statio_user_tables io ON io.relid = t.relid"
        ),
        UserTablesVersion::V3 => marked!(
            "WITH candidates AS ( \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY GREATEST(last_seq_scan, last_idx_scan) DESC NULLS LAST LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY COALESCE(n_tup_ins,0) + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY c.relpages DESC LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables ORDER BY COALESCE(n_dead_tup, 0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY age(c.relfrozenxid) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY mxid_age(c.relminmxid) DESC LIMIT $1) \
             ) \
             SELECT \
               (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid, \
               t.relid, \
               t.schemaname::text AS schemaname, t.relname::text AS relname, \
               COALESCE(ts.spcname, 'pg_default')::text AS tablespace, \
               t.seq_scan, t.seq_tup_read, t.idx_scan, t.idx_tup_fetch, \
               t.n_tup_ins, t.n_tup_upd, t.n_tup_del, t.n_tup_hot_upd, t.n_tup_newpage_upd, \
               t.n_live_tup, t.n_dead_tup, t.n_mod_since_analyze, t.n_ins_since_vacuum, \
               t.vacuum_count, t.autovacuum_count, t.analyze_count, t.autoanalyze_count, \
               (extract(epoch from t.last_vacuum) * 1e6)::int8 AS last_vacuum_us, \
               (extract(epoch from t.last_autovacuum) * 1e6)::int8 AS last_autovacuum_us, \
               (extract(epoch from t.last_analyze) * 1e6)::int8 AS last_analyze_us, \
               (extract(epoch from t.last_autoanalyze) * 1e6)::int8 AS last_autoanalyze_us, \
               (extract(epoch from t.last_seq_scan) * 1e6)::int8 AS last_seq_scan_us, \
               (extract(epoch from t.last_idx_scan) * 1e6)::int8 AS last_idx_scan_us, \
               pg_relation_size(t.relid)::int8 AS main_fork_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_total_relation_size(cl.reltoastrelid)::int8 END AS toast_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_live_tuples(cl.reltoastrelid) END AS toast_n_live_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_dead_tuples(cl.reltoastrelid) END AS toast_n_dead_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN (extract(epoch from pg_stat_get_last_autovacuum_time(cl.reltoastrelid)) * 1e6)::int8 END AS toast_last_autovacuum_us, \
               age(cl.relfrozenxid)::int8 AS xid_age, mxid_age(cl.relminmxid)::int8 AS mxid_age, cl.reltuples::int8 AS reltuples, \
               io.heap_blks_read, io.heap_blks_hit, io.idx_blks_read, io.idx_blks_hit, \
               io.toast_blks_read, io.toast_blks_hit, io.tidx_blks_read, io.tidx_blks_hit, \
               (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_user_tables t \
             JOIN candidates cand ON cand.relid = t.relid \
             LEFT JOIN pg_class cl ON cl.oid = t.relid \
             LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace \
             LEFT JOIN pg_statio_user_tables io ON io.relid = t.relid"
        ),
        UserTablesVersion::V4 => marked!(
            "WITH candidates AS ( \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY GREATEST(last_seq_scan, last_idx_scan) DESC NULLS LAST LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables \
                  ORDER BY COALESCE(n_tup_ins,0) + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY c.relpages DESC LIMIT $1) \
               UNION \
               (SELECT relid FROM pg_stat_user_tables ORDER BY COALESCE(n_dead_tup, 0) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY age(c.relfrozenxid) DESC LIMIT $1) \
               UNION \
               (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid \
                  ORDER BY mxid_age(c.relminmxid) DESC LIMIT $1) \
             ) \
             SELECT \
               (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid, \
               t.relid, \
               t.schemaname::text AS schemaname, t.relname::text AS relname, \
               COALESCE(ts.spcname, 'pg_default')::text AS tablespace, \
               t.seq_scan, t.seq_tup_read, t.idx_scan, t.idx_tup_fetch, \
               t.n_tup_ins, t.n_tup_upd, t.n_tup_del, t.n_tup_hot_upd, t.n_tup_newpage_upd, \
               t.n_live_tup, t.n_dead_tup, t.n_mod_since_analyze, t.n_ins_since_vacuum, \
               t.vacuum_count, t.autovacuum_count, t.analyze_count, t.autoanalyze_count, \
               (extract(epoch from t.last_vacuum) * 1e6)::int8 AS last_vacuum_us, \
               (extract(epoch from t.last_autovacuum) * 1e6)::int8 AS last_autovacuum_us, \
               (extract(epoch from t.last_analyze) * 1e6)::int8 AS last_analyze_us, \
               (extract(epoch from t.last_autoanalyze) * 1e6)::int8 AS last_autoanalyze_us, \
               (extract(epoch from t.last_seq_scan) * 1e6)::int8 AS last_seq_scan_us, \
               (extract(epoch from t.last_idx_scan) * 1e6)::int8 AS last_idx_scan_us, \
               t.total_vacuum_time, t.total_autovacuum_time, t.total_analyze_time, t.total_autoanalyze_time, \
               pg_relation_size(t.relid)::int8 AS main_fork_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_total_relation_size(cl.reltoastrelid)::int8 END AS toast_bytes, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_live_tuples(cl.reltoastrelid) END AS toast_n_live_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_dead_tuples(cl.reltoastrelid) END AS toast_n_dead_tup, \
               CASE WHEN cl.reltoastrelid <> 0 THEN (extract(epoch from pg_stat_get_last_autovacuum_time(cl.reltoastrelid)) * 1e6)::int8 END AS toast_last_autovacuum_us, \
               age(cl.relfrozenxid)::int8 AS xid_age, mxid_age(cl.relminmxid)::int8 AS mxid_age, cl.reltuples::int8 AS reltuples, \
               io.heap_blks_read, io.heap_blks_hit, io.idx_blks_read, io.idx_blks_hit, \
               io.toast_blks_read, io.toast_blks_hit, io.tidx_blks_read, io.tidx_blks_hit, \
               (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_user_tables t \
             JOIN candidates cand ON cand.relid = t.relid \
             LEFT JOIN pg_class cl ON cl.oid = t.relid \
             LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace \
             LEFT JOIN pg_statio_user_tables io ON io.relid = t.relid"
        ),
    }
}

/// One raw `pg_stat_user_tables` row, a version-agnostic superset.
///
/// Numbers are owned directly; strings are interned by the caller. Columns
/// absent from the version, and catalog `NULL`s, are `None`. See
/// [`PgStatUserTablesV3`] for meaning.
#[derive(Debug, Clone)]
pub struct UserTablesRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Database oid of the connection.
    pub datid: u32,
    /// Table oid.
    pub relid: u32,
    /// Schema name.
    pub schemaname: String,
    /// Table name.
    pub relname: String,
    /// Tablespace name (`pg_default` for the default tablespace).
    pub tablespace: String,
    /// Sequential scans.
    pub seq_scan: i64,
    /// Live rows fetched by sequential scans.
    pub seq_tup_read: i64,
    /// Index scans; `None` when the table has no indexes.
    pub idx_scan: Option<i64>,
    /// Live rows fetched by index scans; `None` when the table has no indexes.
    pub idx_tup_fetch: Option<i64>,
    /// Rows inserted.
    pub n_tup_ins: i64,
    /// Rows updated.
    pub n_tup_upd: i64,
    /// Rows deleted.
    pub n_tup_del: i64,
    /// Rows HOT-updated.
    pub n_tup_hot_upd: i64,
    /// Rows updated to a new page (V3).
    pub n_tup_newpage_upd: Option<i64>,
    /// Estimated live rows.
    pub n_live_tup: i64,
    /// Estimated dead rows.
    pub n_dead_tup: i64,
    /// Rows modified since the last analyze.
    pub n_mod_since_analyze: i64,
    /// Rows inserted since the last vacuum (V2+).
    pub n_ins_since_vacuum: Option<i64>,
    /// Manual vacuums.
    pub vacuum_count: i64,
    /// Autovacuums.
    pub autovacuum_count: i64,
    /// Manual analyzes.
    pub analyze_count: i64,
    /// Autoanalyzes.
    pub autoanalyze_count: i64,
    /// Last manual vacuum, unix microseconds; `None` if never.
    pub last_vacuum: Option<i64>,
    /// Last autovacuum, unix microseconds; `None` if never.
    pub last_autovacuum: Option<i64>,
    /// Last manual analyze, unix microseconds; `None` if never.
    pub last_analyze: Option<i64>,
    /// Last autoanalyze, unix microseconds; `None` if never.
    pub last_autoanalyze: Option<i64>,
    /// Last sequential scan, unix microseconds (V3); `None` if never.
    pub last_seq_scan: Option<i64>,
    /// Last index scan, unix microseconds (V3); `None` if never.
    pub last_idx_scan: Option<i64>,
    /// Cumulative manual-vacuum time in milliseconds (V4); `None` for &lt;V4.
    pub total_vacuum_time: Option<f64>,
    /// Cumulative autovacuum time in milliseconds (V4); `None` for &lt;V4.
    pub total_autovacuum_time: Option<f64>,
    /// Cumulative manual-analyze time in milliseconds (V4); `None` for &lt;V4.
    pub total_analyze_time: Option<f64>,
    /// Cumulative autoanalyze time in milliseconds (V4); `None` for &lt;V4.
    pub total_autoanalyze_time: Option<f64>,
    /// Main-fork size in bytes.
    pub main_fork_bytes: i64,
    /// TOAST table + indexes size in bytes; `None` when no TOAST relation.
    pub toast_bytes: Option<i64>,
    /// TOAST live tuples; `None` when no TOAST relation.
    pub toast_n_live_tup: Option<i64>,
    /// TOAST dead tuples; `None` when no TOAST relation.
    pub toast_n_dead_tup: Option<i64>,
    /// Last TOAST autovacuum, unix microseconds; `None` when no TOAST or never.
    pub toast_last_autovacuum: Option<i64>,
    /// Age of `relfrozenxid` in transactions.
    pub xid_age: i64,
    /// Age of `relminmxid` in multixacts.
    pub mxid_age: i64,
    /// Planner row estimate (`pg_class.reltuples`).
    pub reltuples: i64,
    /// Heap block reads reported by `pg_statio_user_tables`.
    pub heap_blks_read: i64,
    /// Heap buffer hits.
    pub heap_blks_hit: i64,
    /// Index block reads; `None` when the table has no indexes.
    pub idx_blks_read: Option<i64>,
    /// Index buffer hits; `None` when the table has no indexes.
    pub idx_blks_hit: Option<i64>,
    /// TOAST block reads; `None` when no TOAST relation.
    pub toast_blks_read: Option<i64>,
    /// TOAST buffer hits; `None` when no TOAST relation.
    pub toast_blks_hit: Option<i64>,
    /// TOAST-index block reads; `None` when no TOAST relation.
    pub tidx_blks_read: Option<i64>,
    /// TOAST-index buffer hits; `None` when no TOAST relation.
    pub tidx_blks_hit: Option<i64>,
}

/// Read a raw row from a result row using the version's column set.
fn row_from_pg(row: &tokio_postgres::Row, version: UserTablesVersion) -> UserTablesRow {
    let has_insert_vacuum = matches!(
        version,
        UserTablesVersion::V2 | UserTablesVersion::V3 | UserTablesVersion::V4
    );
    let has_pg16 = matches!(version, UserTablesVersion::V3 | UserTablesVersion::V4);
    let has_pg18 = matches!(version, UserTablesVersion::V4);
    UserTablesRow {
        ts: row.get("ts_us"),
        datid: row.get("datid"),
        relid: row.get("relid"),
        schemaname: row.get("schemaname"),
        relname: row.get("relname"),
        tablespace: row.get("tablespace"),
        seq_scan: row.get("seq_scan"),
        seq_tup_read: row.get("seq_tup_read"),
        idx_scan: row.get("idx_scan"),
        idx_tup_fetch: row.get("idx_tup_fetch"),
        n_tup_ins: row.get("n_tup_ins"),
        n_tup_upd: row.get("n_tup_upd"),
        n_tup_del: row.get("n_tup_del"),
        n_tup_hot_upd: row.get("n_tup_hot_upd"),
        n_tup_newpage_upd: has_pg16.then(|| row.get("n_tup_newpage_upd")),
        n_live_tup: row.get("n_live_tup"),
        n_dead_tup: row.get("n_dead_tup"),
        n_mod_since_analyze: row.get("n_mod_since_analyze"),
        n_ins_since_vacuum: has_insert_vacuum.then(|| row.get("n_ins_since_vacuum")),
        vacuum_count: row.get("vacuum_count"),
        autovacuum_count: row.get("autovacuum_count"),
        analyze_count: row.get("analyze_count"),
        autoanalyze_count: row.get("autoanalyze_count"),
        last_vacuum: row.get("last_vacuum_us"),
        last_autovacuum: row.get("last_autovacuum_us"),
        last_analyze: row.get("last_analyze_us"),
        last_autoanalyze: row.get("last_autoanalyze_us"),
        last_seq_scan: has_pg16.then(|| row.get("last_seq_scan_us")).flatten(),
        last_idx_scan: has_pg16.then(|| row.get("last_idx_scan_us")).flatten(),
        total_vacuum_time: has_pg18.then(|| row.get("total_vacuum_time")),
        total_autovacuum_time: has_pg18.then(|| row.get("total_autovacuum_time")),
        total_analyze_time: has_pg18.then(|| row.get("total_analyze_time")),
        total_autoanalyze_time: has_pg18.then(|| row.get("total_autoanalyze_time")),
        main_fork_bytes: row.get("main_fork_bytes"),
        toast_bytes: row.get("toast_bytes"),
        toast_n_live_tup: row.get("toast_n_live_tup"),
        toast_n_dead_tup: row.get("toast_n_dead_tup"),
        toast_last_autovacuum: row.get("toast_last_autovacuum_us"),
        xid_age: row.get("xid_age"),
        mxid_age: row.get("mxid_age"),
        reltuples: row.get("reltuples"),
        heap_blks_read: row.get("heap_blks_read"),
        heap_blks_hit: row.get("heap_blks_hit"),
        idx_blks_read: row.get("idx_blks_read"),
        idx_blks_hit: row.get("idx_blks_hit"),
        toast_blks_read: row.get("toast_blks_read"),
        toast_blks_hit: row.get("toast_blks_hit"),
        tidx_blks_read: row.get("tidx_blks_read"),
        tidx_blks_hit: row.get("tidx_blks_hit"),
    }
}

/// Build a `1_013_004` row (PG18 layout), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v4<E>(
    row: &UserTablesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserTablesV4, E> {
    Ok(PgStatUserTablesV4 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        seq_scan: row.seq_scan,
        seq_tup_read: row.seq_tup_read,
        idx_scan: row.idx_scan,
        idx_tup_fetch: row.idx_tup_fetch,
        n_tup_ins: row.n_tup_ins,
        n_tup_upd: row.n_tup_upd,
        n_tup_del: row.n_tup_del,
        n_tup_hot_upd: row.n_tup_hot_upd,
        n_tup_newpage_upd: row.n_tup_newpage_upd.unwrap_or(0),
        n_live_tup: row.n_live_tup,
        n_dead_tup: row.n_dead_tup,
        n_mod_since_analyze: row.n_mod_since_analyze,
        n_ins_since_vacuum: row.n_ins_since_vacuum.unwrap_or(0),
        vacuum_count: row.vacuum_count,
        autovacuum_count: row.autovacuum_count,
        analyze_count: row.analyze_count,
        autoanalyze_count: row.autoanalyze_count,
        last_vacuum: row.last_vacuum.map(Ts),
        last_autovacuum: row.last_autovacuum.map(Ts),
        last_analyze: row.last_analyze.map(Ts),
        last_autoanalyze: row.last_autoanalyze.map(Ts),
        last_seq_scan: row.last_seq_scan.map(Ts),
        last_idx_scan: row.last_idx_scan.map(Ts),
        total_vacuum_time: row.total_vacuum_time.unwrap_or(0.0),
        total_autovacuum_time: row.total_autovacuum_time.unwrap_or(0.0),
        total_analyze_time: row.total_analyze_time.unwrap_or(0.0),
        total_autoanalyze_time: row.total_autoanalyze_time.unwrap_or(0.0),
        main_fork_bytes: row.main_fork_bytes,
        toast_bytes: row.toast_bytes,
        toast_n_live_tup: row.toast_n_live_tup,
        toast_n_dead_tup: row.toast_n_dead_tup,
        toast_last_autovacuum: row.toast_last_autovacuum.map(Ts),
        xid_age: row.xid_age,
        mxid_age: row.mxid_age,
        reltuples: row.reltuples,
        heap_blks_read: row.heap_blks_read,
        heap_blks_hit: row.heap_blks_hit,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
        toast_blks_read: row.toast_blks_read,
        toast_blks_hit: row.toast_blks_hit,
        tidx_blks_read: row.tidx_blks_read,
        tidx_blks_hit: row.tidx_blks_hit,
    })
}

/// Build a `1_013_003` row (PG16-17 layout, no PG18 timing columns).
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v3<E>(
    row: &UserTablesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserTablesV3, E> {
    Ok(PgStatUserTablesV3 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        seq_scan: row.seq_scan,
        seq_tup_read: row.seq_tup_read,
        idx_scan: row.idx_scan,
        idx_tup_fetch: row.idx_tup_fetch,
        n_tup_ins: row.n_tup_ins,
        n_tup_upd: row.n_tup_upd,
        n_tup_del: row.n_tup_del,
        n_tup_hot_upd: row.n_tup_hot_upd,
        n_tup_newpage_upd: row.n_tup_newpage_upd.unwrap_or(0),
        n_live_tup: row.n_live_tup,
        n_dead_tup: row.n_dead_tup,
        n_mod_since_analyze: row.n_mod_since_analyze,
        n_ins_since_vacuum: row.n_ins_since_vacuum.unwrap_or(0),
        vacuum_count: row.vacuum_count,
        autovacuum_count: row.autovacuum_count,
        analyze_count: row.analyze_count,
        autoanalyze_count: row.autoanalyze_count,
        last_vacuum: row.last_vacuum.map(Ts),
        last_autovacuum: row.last_autovacuum.map(Ts),
        last_analyze: row.last_analyze.map(Ts),
        last_autoanalyze: row.last_autoanalyze.map(Ts),
        last_seq_scan: row.last_seq_scan.map(Ts),
        last_idx_scan: row.last_idx_scan.map(Ts),
        main_fork_bytes: row.main_fork_bytes,
        toast_bytes: row.toast_bytes,
        toast_n_live_tup: row.toast_n_live_tup,
        toast_n_dead_tup: row.toast_n_dead_tup,
        toast_last_autovacuum: row.toast_last_autovacuum.map(Ts),
        xid_age: row.xid_age,
        mxid_age: row.mxid_age,
        reltuples: row.reltuples,
        heap_blks_read: row.heap_blks_read,
        heap_blks_hit: row.heap_blks_hit,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
        toast_blks_read: row.toast_blks_read,
        toast_blks_hit: row.toast_blks_hit,
        tidx_blks_read: row.tidx_blks_read,
        tidx_blks_hit: row.tidx_blks_hit,
    })
}

/// Build a `1_013_002` row (PG13-15 layout, no PG16 columns).
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v2<E>(
    row: &UserTablesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserTablesV2, E> {
    Ok(PgStatUserTablesV2 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        seq_scan: row.seq_scan,
        seq_tup_read: row.seq_tup_read,
        idx_scan: row.idx_scan,
        idx_tup_fetch: row.idx_tup_fetch,
        n_tup_ins: row.n_tup_ins,
        n_tup_upd: row.n_tup_upd,
        n_tup_del: row.n_tup_del,
        n_tup_hot_upd: row.n_tup_hot_upd,
        n_live_tup: row.n_live_tup,
        n_dead_tup: row.n_dead_tup,
        n_mod_since_analyze: row.n_mod_since_analyze,
        n_ins_since_vacuum: row.n_ins_since_vacuum.unwrap_or(0),
        vacuum_count: row.vacuum_count,
        autovacuum_count: row.autovacuum_count,
        analyze_count: row.analyze_count,
        autoanalyze_count: row.autoanalyze_count,
        last_vacuum: row.last_vacuum.map(Ts),
        last_autovacuum: row.last_autovacuum.map(Ts),
        last_analyze: row.last_analyze.map(Ts),
        last_autoanalyze: row.last_autoanalyze.map(Ts),
        main_fork_bytes: row.main_fork_bytes,
        toast_bytes: row.toast_bytes,
        toast_n_live_tup: row.toast_n_live_tup,
        toast_n_dead_tup: row.toast_n_dead_tup,
        toast_last_autovacuum: row.toast_last_autovacuum.map(Ts),
        xid_age: row.xid_age,
        mxid_age: row.mxid_age,
        reltuples: row.reltuples,
        heap_blks_read: row.heap_blks_read,
        heap_blks_hit: row.heap_blks_hit,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
        toast_blks_read: row.toast_blks_read,
        toast_blks_hit: row.toast_blks_hit,
        tidx_blks_read: row.tidx_blks_read,
        tidx_blks_hit: row.tidx_blks_hit,
    })
}

/// Build a `1_013_001` row (PG10-12 base layout, no `n_ins_since_vacuum`).
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v1<E>(
    row: &UserTablesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserTablesV1, E> {
    Ok(PgStatUserTablesV1 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        seq_scan: row.seq_scan,
        seq_tup_read: row.seq_tup_read,
        idx_scan: row.idx_scan,
        idx_tup_fetch: row.idx_tup_fetch,
        n_tup_ins: row.n_tup_ins,
        n_tup_upd: row.n_tup_upd,
        n_tup_del: row.n_tup_del,
        n_tup_hot_upd: row.n_tup_hot_upd,
        n_live_tup: row.n_live_tup,
        n_dead_tup: row.n_dead_tup,
        n_mod_since_analyze: row.n_mod_since_analyze,
        vacuum_count: row.vacuum_count,
        autovacuum_count: row.autovacuum_count,
        analyze_count: row.analyze_count,
        autoanalyze_count: row.autoanalyze_count,
        last_vacuum: row.last_vacuum.map(Ts),
        last_autovacuum: row.last_autovacuum.map(Ts),
        last_analyze: row.last_analyze.map(Ts),
        last_autoanalyze: row.last_autoanalyze.map(Ts),
        main_fork_bytes: row.main_fork_bytes,
        toast_bytes: row.toast_bytes,
        toast_n_live_tup: row.toast_n_live_tup,
        toast_n_dead_tup: row.toast_n_dead_tup,
        toast_last_autovacuum: row.toast_last_autovacuum.map(Ts),
        xid_age: row.xid_age,
        mxid_age: row.mxid_age,
        reltuples: row.reltuples,
        heap_blks_read: row.heap_blks_read,
        heap_blks_hit: row.heap_blks_hit,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
        toast_blks_read: row.toast_blks_read,
        toast_blks_hit: row.toast_blks_hit,
        tidx_blks_read: row.tidx_blks_read,
        tidx_blks_hit: row.tidx_blks_hit,
    })
}

/// Collect a `pg_stat_user_tables` snapshot for one database connection.
///
/// Returns the layout version and raw rows; the caller interns the strings and
/// builds the typed rows. `max_tables` is the per-axis top-N row count.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_user_tables(
    client: &Client,
    major: u32,
    max_tables: i64,
) -> Result<(UserTablesVersion, Vec<UserTablesRow>), tokio_postgres::Error> {
    let version = user_tables_version(major);
    let rows = client
        .query(user_tables_query(version), &[&max_tables])
        .await?;
    let parsed = rows.iter().map(|row| row_from_pg(row, version)).collect();
    Ok((version, parsed))
}

#[cfg(test)]
mod tests {
    use super::{
        UserTablesVersion, to_v1, to_v2, to_v3, to_v4, user_tables_query, user_tables_version,
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

    fn sample_row(relid: u32, has_idx: bool, has_toast: bool) -> super::UserTablesRow {
        super::UserTablesRow {
            ts: 2_000,
            datid: 5,
            relid,
            schemaname: "public".to_owned(),
            relname: "accounts".to_owned(),
            tablespace: "pg_default".to_owned(),
            seq_scan: 10,
            seq_tup_read: 1_000,
            idx_scan: has_idx.then_some(7),
            idx_tup_fetch: has_idx.then_some(700),
            n_tup_ins: 50,
            n_tup_upd: 30,
            n_tup_del: 10,
            n_tup_hot_upd: 5,
            n_tup_newpage_upd: Some(0),
            n_live_tup: 900,
            n_dead_tup: 40,
            n_mod_since_analyze: 70,
            n_ins_since_vacuum: Some(20),
            vacuum_count: 1,
            autovacuum_count: 3,
            analyze_count: 1,
            autoanalyze_count: 2,
            last_vacuum: None,
            last_autovacuum: None,
            last_analyze: None,
            last_autoanalyze: None,
            last_seq_scan: None,
            last_idx_scan: None,
            total_vacuum_time: Some(12.5),
            total_autovacuum_time: Some(340.0),
            total_analyze_time: Some(7.5),
            total_autoanalyze_time: Some(21.0),
            main_fork_bytes: 8_192,
            toast_bytes: has_toast.then_some(16_384),
            toast_n_live_tup: has_toast.then_some(3),
            toast_n_dead_tup: has_toast.then_some(1),
            toast_last_autovacuum: None,
            xid_age: 100_000_000,
            mxid_age: 5_000_000,
            reltuples: 900,
            heap_blks_read: 400,
            heap_blks_hit: 90_000,
            idx_blks_read: has_idx.then_some(40),
            idx_blks_hit: has_idx.then_some(9_000),
            toast_blks_read: has_toast.then_some(2),
            toast_blks_hit: has_toast.then_some(20),
            tidx_blks_read: has_toast.then_some(1),
            tidx_blks_hit: has_toast.then_some(10),
        }
    }

    #[test]
    fn version_follows_catalog_changes() {
        assert_eq!(user_tables_version(10), UserTablesVersion::V1);
        assert_eq!(user_tables_version(12), UserTablesVersion::V1);
        assert_eq!(user_tables_version(13), UserTablesVersion::V2);
        assert_eq!(user_tables_version(15), UserTablesVersion::V2);
        assert_eq!(user_tables_version(16), UserTablesVersion::V3);
        assert_eq!(user_tables_version(17), UserTablesVersion::V3);
        assert_eq!(user_tables_version(18), UserTablesVersion::V4);
    }

    #[test]
    fn query_has_version_specific_columns_and_marker() {
        assert!(!user_tables_query(UserTablesVersion::V1).contains("n_ins_since_vacuum"));
        assert!(user_tables_query(UserTablesVersion::V2).contains("n_ins_since_vacuum"));
        assert!(!user_tables_query(UserTablesVersion::V2).contains("n_tup_newpage_upd"));
        assert!(user_tables_query(UserTablesVersion::V3).contains("n_tup_newpage_upd"));
        assert!(user_tables_query(UserTablesVersion::V3).contains("last_seq_scan"));
        assert!(!user_tables_query(UserTablesVersion::V3).contains("total_vacuum_time"));
        assert!(user_tables_query(UserTablesVersion::V4).contains("total_vacuum_time"));
        assert!(user_tables_query(UserTablesVersion::V4).contains("total_autoanalyze_time"));
        for v in [
            UserTablesVersion::V1,
            UserTablesVersion::V2,
            UserTablesVersion::V3,
            UserTablesVersion::V4,
        ] {
            let q = user_tables_query(v);
            assert!(q.contains("pg_kronika"));
            assert!(q.contains("pg_stat_user_tables"));
            assert!(q.contains("LEFT JOIN pg_statio_user_tables"));
            assert!(q.contains("AS main_fork_bytes"));
            // Candidate selection is mechanical top-N by raw columns, including
            // transaction-id and multixact age. No thresholds or GUC-based
            // verdicts belong in the SQL.
            assert!(q.contains("ORDER BY age(c.relfrozenxid) DESC"));
            assert!(q.contains("ORDER BY mxid_age(c.relminmxid) DESC"));
            // The write axis keeps a write-only table even when the activity axis
            // is read recency (PG16+).
            assert!(q.contains(
                "ORDER BY COALESCE(n_tup_ins,0) + COALESCE(n_tup_upd,0) + COALESCE(n_tup_del,0) DESC"
            ));
            assert!(!q.contains("current_setting"));
        }
    }

    #[test]
    fn to_v4_keeps_timing_and_main_fork_bytes() {
        let r = to_v4(&sample_row(5, true, true), "appdb", fake_intern).expect("infallible intern");
        assert_eq!(r.relid, 5);
        assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
        assert!((r.total_autovacuum_time - 340.0).abs() < f64::EPSILON);
        assert!((r.total_analyze_time - 7.5).abs() < f64::EPSILON);
        assert_eq!(r.main_fork_bytes, 8_192);
        assert_eq!(r.n_ins_since_vacuum, 20);
    }

    #[test]
    fn to_v3_maps_nulls_interns_strings_and_injects_datname() {
        let r = to_v3(
            &sample_row(
                /*relid*/ 5, /*has_idx*/ false, /*has_toast*/ false,
            ),
            "appdb",
            fake_intern,
        )
        .expect("infallible intern");
        assert_eq!(r.relid, 5);
        assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
        assert_eq!(r.idx_scan, None); // no indexes
        assert_eq!(r.idx_blks_read, None);
        assert_eq!(r.toast_bytes, None); // no TOAST
        assert_eq!(r.last_vacuum, None); // never vacuumed in the sample
        assert_eq!(r.n_tup_newpage_upd, 0);
        assert_eq!(r.xid_age, 100_000_000);
    }

    #[test]
    fn to_v2_drops_pg16_columns_but_keeps_insert_vacuum() {
        let r = to_v2(&sample_row(5, true, true), "appdb", fake_intern).expect("infallible intern");
        assert_eq!(r.relid, 5);
        assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
        assert_eq!(r.idx_scan, Some(7));
        assert_eq!(r.toast_bytes, Some(16_384));
        assert_eq!(r.n_ins_since_vacuum, 20);
    }

    #[test]
    fn to_v1_drops_insert_vacuum() {
        let r =
            to_v1(&sample_row(5, false, false), "appdb", fake_intern).expect("infallible intern");
        assert_eq!(r.relid, 5);
        assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
        assert_eq!(r.idx_scan, None);
        assert_eq!(r.reltuples, 900);
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(
            to_v3(&sample_row(5, true, true), "appdb", boom),
            Err("full")
        );
    }
}
