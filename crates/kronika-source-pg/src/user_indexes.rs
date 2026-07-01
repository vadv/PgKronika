//! `pg_stat_user_indexes` collection for types `1_014_001`..`1_014_002`.
//!
//! Per-index statistics, collected per database through the connection pool. In
//! PG 10-18 the column set grows once: `last_idx_scan` arrives in PG16. PG17 and
//! PG18 add nothing to `pg_stat_all_indexes`. The major version selects both the
//! SQL and the layout.
//!
//! Candidate selection is purely mechanical: the union of top-N indexes by raw
//! columns (scan count, tuples read, size), so a heavily read or oversized index
//! is never dropped. The union also captures never-used indexes through the size
//! axis (`pg_class.relpages`), so the analyzer can flag them from the recorded
//! `relpages` and `idx_scan` without the collector passing a `WHERE idx_scan = 0`
//! verdict. Each axis breaks ties on `indexrelid` for a stable top-N. On PG16+ a
//! scan-recency axis (`last_idx_scan`) is added, filtered to indexes that have
//! been scanned so it never fills its slots with never-scanned indexes. Collection
//! returns owned rows; the caller interns the strings into the segment dictionary.
//! The typed layout lives in `kronika-registry` (`PgStatUserIndexesV1`..`V2`).

use kronika_registry::pg_stat_user_indexes::{PgStatUserIndexesV1, PgStatUserIndexesV2};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query (one or more literal fragments) with the kronika marker
/// (SQL-transparency rule). Multiple fragments let a shared literal, such as the
/// indexdef cap, be spliced into the middle of the query at compile time.
macro_rules! marked {
    ($($sql:expr),+ $(,)?) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/user_indexes.rs */ ",
            $($sql),+
        )
    };
}

/// Cap on the `pg_get_indexdef` text, bounding the String before tokio-postgres
/// materializes it (a partial index over a large expression can be arbitrarily
/// long). Consistent with query-text truncation elsewhere. The literal is shared
/// with the SQL through [`indexdef_max_len`].
macro_rules! indexdef_max_len {
    () => {
        "5000"
    };
}

/// The `pg_get_indexdef` text cap, as an integer for tests and callers.
#[must_use]
pub const fn indexdef_max_len() -> i64 {
    5000
}

/// The `pg_stat_user_indexes` layout selected by the server major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserIndexesVersion {
    /// PG 10-15: type `1_014_001` (base layout).
    V1,
    /// PG 16-18: type `1_014_002` (adds `last_idx_scan`).
    V2,
}

/// Select the layout for a server major version.
///
/// `last_idx_scan` arrived in PG16 and is the only catalog change through PG18.
#[must_use]
pub const fn user_indexes_version(major: u32) -> UserIndexesVersion {
    if major >= 16 {
        UserIndexesVersion::V2
    } else {
        UserIndexesVersion::V1
    }
}

/// Worst-case number of top-N axes any layout unions in `user_indexes_query`.
///
/// V2 (PG16+) has the most: `idx_scan`, `idx_tup_read`, `pg_class.relpages`, and
/// `last_idx_scan` recency — four `UNION` branches, each limited to the per-axis
/// top-N. Callers use this to bound `DEFAULT_MAX_DATABASES * axes * max_indexes`
/// against the section row cap.
pub const INDEX_TOPN_AXES: i64 = 4;

/// The SQL for one layout; `$1` is the per-axis top-N row count. See the module
/// doc for the candidate-selection rationale.
#[must_use]
pub const fn user_indexes_query(version: UserIndexesVersion) -> &'static str {
    match version {
        UserIndexesVersion::V1 => marked!(
            "WITH candidates AS ( \
               (SELECT indexrelid FROM pg_stat_user_indexes ORDER BY COALESCE(idx_scan, 0) DESC, indexrelid LIMIT $1) \
               UNION \
               (SELECT indexrelid FROM pg_stat_user_indexes ORDER BY COALESCE(idx_tup_read, 0) DESC, indexrelid LIMIT $1) \
               UNION \
               (SELECT i.indexrelid FROM pg_stat_user_indexes i JOIN pg_class c ON c.oid = i.indexrelid \
                  ORDER BY c.relpages DESC, i.indexrelid LIMIT $1) \
             ) \
             SELECT \
               (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid, \
               i.indexrelid, i.relid, \
               i.schemaname::text AS schemaname, i.relname::text AS relname, \
               i.indexrelname::text AS indexrelname, \
               COALESCE(ts.spcname, (SELECT spcname FROM pg_catalog.pg_tablespace WHERE oid = (SELECT dattablespace FROM pg_catalog.pg_database WHERE datname = current_database())))::text AS tablespace, \
               i.idx_scan, i.idx_tup_read, i.idx_tup_fetch, \
               pg_relation_size(i.indexrelid)::int8 AS main_fork_bytes, \
               COALESCE(ix.indisunique, false) AS indisunique, \
               COALESCE(ix.indisprimary, false) AS indisprimary, \
               COALESCE(ix.indisvalid, false) AS indisvalid, \
               COALESCE(ix.indisexclusion, false) AS indisexclusion, \
               COALESCE(ix.indisready, false) AS indisready, \
               COALESCE(am.amname, '')::text AS amname, \
               COALESCE(left(pg_get_indexdef(i.indexrelid), ",
            indexdef_max_len!(),
            "), '')::text AS indexdef, \
               COALESCE(io.idx_blks_read, 0) AS idx_blks_read, COALESCE(io.idx_blks_hit, 0) AS idx_blks_hit, \
               (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_user_indexes i \
             JOIN candidates cand ON cand.indexrelid = i.indexrelid \
             LEFT JOIN pg_class cl ON cl.oid = i.indexrelid \
             LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace \
             LEFT JOIN pg_am am ON am.oid = cl.relam \
             LEFT JOIN pg_index ix ON ix.indexrelid = i.indexrelid \
             LEFT JOIN pg_statio_user_indexes io ON io.indexrelid = i.indexrelid"
        ),
        UserIndexesVersion::V2 => marked!(
            "WITH candidates AS ( \
               (SELECT indexrelid FROM pg_stat_user_indexes ORDER BY COALESCE(idx_scan, 0) DESC, indexrelid LIMIT $1) \
               UNION \
               (SELECT indexrelid FROM pg_stat_user_indexes ORDER BY COALESCE(idx_tup_read, 0) DESC, indexrelid LIMIT $1) \
               UNION \
               (SELECT i.indexrelid FROM pg_stat_user_indexes i JOIN pg_class c ON c.oid = i.indexrelid \
                  ORDER BY c.relpages DESC, i.indexrelid LIMIT $1) \
               UNION \
               (SELECT indexrelid FROM pg_stat_user_indexes WHERE last_idx_scan IS NOT NULL \
                  ORDER BY last_idx_scan DESC, indexrelid LIMIT $1) \
             ) \
             SELECT \
               (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())::oid AS datid, \
               i.indexrelid, i.relid, \
               i.schemaname::text AS schemaname, i.relname::text AS relname, \
               i.indexrelname::text AS indexrelname, \
               COALESCE(ts.spcname, (SELECT spcname FROM pg_catalog.pg_tablespace WHERE oid = (SELECT dattablespace FROM pg_catalog.pg_database WHERE datname = current_database())))::text AS tablespace, \
               i.idx_scan, i.idx_tup_read, i.idx_tup_fetch, \
               pg_relation_size(i.indexrelid)::int8 AS main_fork_bytes, \
               (extract(epoch from i.last_idx_scan) * 1e6)::int8 AS last_idx_scan_us, \
               COALESCE(ix.indisunique, false) AS indisunique, \
               COALESCE(ix.indisprimary, false) AS indisprimary, \
               COALESCE(ix.indisvalid, false) AS indisvalid, \
               COALESCE(ix.indisexclusion, false) AS indisexclusion, \
               COALESCE(ix.indisready, false) AS indisready, \
               COALESCE(am.amname, '')::text AS amname, \
               COALESCE(left(pg_get_indexdef(i.indexrelid), ",
            indexdef_max_len!(),
            "), '')::text AS indexdef, \
               COALESCE(io.idx_blks_read, 0) AS idx_blks_read, COALESCE(io.idx_blks_hit, 0) AS idx_blks_hit, \
               (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us \
             FROM pg_stat_user_indexes i \
             JOIN candidates cand ON cand.indexrelid = i.indexrelid \
             LEFT JOIN pg_class cl ON cl.oid = i.indexrelid \
             LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace \
             LEFT JOIN pg_am am ON am.oid = cl.relam \
             LEFT JOIN pg_index ix ON ix.indexrelid = i.indexrelid \
             LEFT JOIN pg_statio_user_indexes io ON io.indexrelid = i.indexrelid"
        ),
    }
}

/// One raw `pg_stat_user_indexes` row, a version-agnostic superset.
///
/// Numbers are owned directly; strings are interned by the caller. Columns
/// absent from the version are `None`. See [`PgStatUserIndexesV2`] for meaning.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent pg_index flag column, not interdependent state"
)]
#[derive(Debug, Clone)]
pub struct UserIndexesRow {
    /// Snapshot time, unix microseconds.
    pub ts: i64,
    /// Database oid of the connection.
    pub datid: u32,
    /// Index oid.
    pub indexrelid: u32,
    /// Table oid the index belongs to.
    pub relid: u32,
    /// Schema name.
    pub schemaname: String,
    /// Table name.
    pub relname: String,
    /// Index name.
    pub indexrelname: String,
    /// Tablespace name; the current database default when `reltablespace = 0`.
    pub tablespace: String,
    /// Index scans.
    pub idx_scan: i64,
    /// Index entries returned by scans.
    pub idx_tup_read: i64,
    /// Live table rows fetched by simple index scans using this index.
    pub idx_tup_fetch: i64,
    /// Main-fork size in bytes.
    pub main_fork_bytes: i64,
    /// Last index scan, unix microseconds (V2); `None` if never.
    pub last_idx_scan: Option<i64>,
    /// Whether the index enforces uniqueness.
    pub indisunique: bool,
    /// Whether the index is a primary key.
    pub indisprimary: bool,
    /// Whether the index is valid for queries.
    pub indisvalid: bool,
    /// Whether the index enforces an exclusion constraint.
    pub indisexclusion: bool,
    /// Whether the index is ready for inserts.
    pub indisready: bool,
    /// Access method name.
    pub amname: String,
    /// `pg_get_indexdef` reconstruction of the index definition.
    pub indexdef: String,
    /// Shared-buffer misses for index blocks.
    pub idx_blks_read: i64,
    /// Shared-buffer hits for index blocks.
    pub idx_blks_hit: i64,
}

/// Read a raw row from a result row using the version's column set.
fn row_from_pg(row: &tokio_postgres::Row, version: UserIndexesVersion) -> UserIndexesRow {
    let has_pg16 = matches!(version, UserIndexesVersion::V2);
    UserIndexesRow {
        ts: row.get("ts_us"),
        datid: row.get("datid"),
        indexrelid: row.get("indexrelid"),
        relid: row.get("relid"),
        schemaname: row.get("schemaname"),
        relname: row.get("relname"),
        indexrelname: row.get("indexrelname"),
        tablespace: row.get("tablespace"),
        idx_scan: row.get("idx_scan"),
        idx_tup_read: row.get("idx_tup_read"),
        idx_tup_fetch: row.get("idx_tup_fetch"),
        main_fork_bytes: row.get("main_fork_bytes"),
        last_idx_scan: has_pg16.then(|| row.get("last_idx_scan_us")).flatten(),
        indisunique: row.get("indisunique"),
        indisprimary: row.get("indisprimary"),
        indisvalid: row.get("indisvalid"),
        indisexclusion: row.get("indisexclusion"),
        indisready: row.get("indisready"),
        amname: row.get("amname"),
        indexdef: row.get("indexdef"),
        idx_blks_read: row.get("idx_blks_read"),
        idx_blks_hit: row.get("idx_blks_hit"),
    }
}

/// Build a `1_014_002` row (PG16-18 layout), interning the strings.
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v2<E>(
    row: &UserIndexesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserIndexesV2, E> {
    Ok(PgStatUserIndexesV2 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        indexrelid: row.indexrelid,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        indexrelname: intern(row.indexrelname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        idx_scan: row.idx_scan,
        idx_tup_read: row.idx_tup_read,
        idx_tup_fetch: row.idx_tup_fetch,
        main_fork_bytes: row.main_fork_bytes,
        last_idx_scan: row.last_idx_scan.map(Ts),
        indisunique: row.indisunique,
        indisprimary: row.indisprimary,
        indisvalid: row.indisvalid,
        indisexclusion: row.indisexclusion,
        indisready: row.indisready,
        amname: intern(row.amname.as_bytes())?,
        indexdef: intern(row.indexdef.as_bytes())?,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
    })
}

/// Build a `1_014_001` row (PG10-15 base layout, no `last_idx_scan`).
///
/// # Errors
/// Returns the interner's error if a string cannot be interned.
pub fn to_v1<E>(
    row: &UserIndexesRow,
    datname: &str,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgStatUserIndexesV1, E> {
    Ok(PgStatUserIndexesV1 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(datname.as_bytes())?,
        indexrelid: row.indexrelid,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        indexrelname: intern(row.indexrelname.as_bytes())?,
        tablespace: intern(row.tablespace.as_bytes())?,
        idx_scan: row.idx_scan,
        idx_tup_read: row.idx_tup_read,
        idx_tup_fetch: row.idx_tup_fetch,
        main_fork_bytes: row.main_fork_bytes,
        indisunique: row.indisunique,
        indisprimary: row.indisprimary,
        indisvalid: row.indisvalid,
        indisexclusion: row.indisexclusion,
        indisready: row.indisready,
        amname: intern(row.amname.as_bytes())?,
        indexdef: intern(row.indexdef.as_bytes())?,
        idx_blks_read: row.idx_blks_read,
        idx_blks_hit: row.idx_blks_hit,
    })
}

/// Collect a `pg_stat_user_indexes` snapshot for one database connection.
///
/// Returns the layout version and raw rows; the caller interns the strings and
/// builds the typed rows. `max_indexes` is the per-axis top-N row count.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_user_indexes(
    client: &Client,
    major: u32,
    max_indexes: i64,
) -> Result<(UserIndexesVersion, Vec<UserIndexesRow>), tokio_postgres::Error> {
    let version = user_indexes_version(major);
    let rows = client
        .query(user_indexes_query(version), &[&max_indexes])
        .await?;
    let parsed = rows.iter().map(|row| row_from_pg(row, version)).collect();
    Ok((version, parsed))
}

#[cfg(test)]
mod tests {
    use super::{
        UserIndexesRow, UserIndexesVersion, indexdef_max_len, to_v1, to_v2, user_indexes_query,
        user_indexes_version,
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

    fn sample_row(indexrelid: u32, scanned: bool) -> UserIndexesRow {
        UserIndexesRow {
            ts: 2_000,
            datid: 5,
            indexrelid,
            relid: indexrelid - 1,
            schemaname: "public".to_owned(),
            relname: "accounts".to_owned(),
            indexrelname: "accounts_pkey".to_owned(),
            tablespace: "pg_default".to_owned(),
            idx_scan: 120,
            idx_tup_read: 3_400,
            idx_tup_fetch: 3_000,
            main_fork_bytes: 16_384,
            last_idx_scan: scanned.then_some(1_900),
            indisunique: true,
            indisprimary: true,
            indisvalid: true,
            indisexclusion: false,
            indisready: true,
            amname: "btree".to_owned(),
            indexdef: "CREATE UNIQUE INDEX accounts_pkey ON public.accounts USING btree (id)"
                .to_owned(),
            idx_blks_read: 40,
            idx_blks_hit: 9_000,
        }
    }

    #[test]
    fn version_follows_catalog_changes() {
        assert_eq!(user_indexes_version(10), UserIndexesVersion::V1);
        assert_eq!(user_indexes_version(15), UserIndexesVersion::V1);
        assert_eq!(user_indexes_version(16), UserIndexesVersion::V2);
        assert_eq!(user_indexes_version(17), UserIndexesVersion::V2);
        assert_eq!(user_indexes_version(18), UserIndexesVersion::V2);
    }

    #[test]
    fn query_has_version_specific_columns_and_marker() {
        assert!(!user_indexes_query(UserIndexesVersion::V1).contains("last_idx_scan"));
        assert!(user_indexes_query(UserIndexesVersion::V2).contains("last_idx_scan"));
        // The recency axis skips never-scanned indexes so it does not fill its
        // slots arbitrarily; they are still caught by the relpages/size axis.
        assert!(
            user_indexes_query(UserIndexesVersion::V2).contains("WHERE last_idx_scan IS NOT NULL")
        );
        for v in [UserIndexesVersion::V1, UserIndexesVersion::V2] {
            let q = user_indexes_query(v);
            assert!(q.contains("pg_kronika"));
            assert!(q.contains("pg_stat_user_indexes"));
            assert!(q.contains("LEFT JOIN pg_statio_user_indexes"));
            assert!(q.contains("LEFT JOIN pg_am"));
            assert!(q.contains("LEFT JOIN pg_index"));
            assert!(q.contains("pg_get_indexdef"));
            assert!(q.contains("AS main_fork_bytes"));
            assert!(q.contains("ORDER BY COALESCE(idx_scan, 0) DESC"));
            assert!(q.contains("ORDER BY COALESCE(idx_tup_read, 0) DESC"));
            assert!(q.contains("ORDER BY c.relpages DESC"));
            // The indexdef String is bounded in SQL before materialization.
            assert!(q.contains("left(pg_get_indexdef"));
            assert!(q.contains("5000"));
            // The database default tablespace is the fallback, not a pg_default
            // literal, so an object with reltablespace = 0 in a non-pg_default
            // default is labelled correctly.
            assert!(q.contains("dattablespace"));
            assert!(!q.contains("'pg_default'"));
            // The two exclusion/ready flags are recorded alongside the others.
            assert!(q.contains("indisexclusion"));
            assert!(q.contains("indisready"));
            // A statio race cannot surface a NULL that panics the i64 decode.
            assert!(q.contains("COALESCE(io.idx_blks_read, 0)"));
            assert!(q.contains("COALESCE(io.idx_blks_hit, 0)"));
            // Every axis breaks ties on indexrelid for a deterministic top-N.
            assert!(q.contains(", indexrelid LIMIT $1"));
            // No threshold "big-unused" verdict, no GUC-based branch in the SQL.
            assert!(!q.contains("idx_scan = 0"));
            assert!(!q.contains("current_setting"));
        }
    }

    #[test]
    fn indexdef_cap_matches_sql_literal() {
        // The integer cap and the literal spliced into the SQL must agree.
        assert_eq!(indexdef_max_len(), 5000);
        assert!(
            user_indexes_query(UserIndexesVersion::V1).contains(&indexdef_max_len().to_string())
        );
    }

    #[test]
    fn to_v2_keeps_scan_recency_and_flags() {
        let r = to_v2(&sample_row(16_385, true), "appdb", fake_intern).expect("infallible intern");
        assert_eq!(r.indexrelid, 16_385);
        assert_eq!(r.relid, 16_384);
        assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
        assert_eq!(r.last_idx_scan.map(|t| t.0), Some(1_900));
        assert!(r.indisprimary);
        assert!(!r.indisexclusion);
        assert!(r.indisready);
        assert_eq!(r.amname, fake_intern(b"btree").unwrap());
        assert_eq!(r.main_fork_bytes, 16_384);
    }

    #[test]
    fn to_v2_maps_never_scanned_to_none() {
        let r = to_v2(&sample_row(16_385, false), "appdb", fake_intern).expect("infallible intern");
        assert_eq!(r.last_idx_scan, None);
    }

    #[test]
    fn to_v1_drops_scan_recency() {
        let r = to_v1(&sample_row(16_385, true), "appdb", fake_intern).expect("infallible intern");
        assert_eq!(r.indexrelid, 16_385);
        assert_eq!(r.datname, fake_intern(b"appdb").unwrap());
        assert_eq!(r.idx_blks_hit, 9_000);
        assert_eq!(
            r.indexdef,
            fake_intern(sample_row(16_385, true).indexdef.as_bytes()).unwrap()
        );
    }

    #[test]
    fn intern_failure_propagates() {
        fn boom(_b: &[u8]) -> Result<StrId, &'static str> {
            Err("full")
        }
        assert_eq!(to_v2(&sample_row(16_385, true), "appdb", boom), Err("full"));
    }
}
