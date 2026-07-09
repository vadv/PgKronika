//! Type `1_004_001`: `pg_store_plans`, the vadv fork (extension 2.x).
//!
//! Per-plan execution counters, one row per `(userid, dbid, planid)` — the
//! extension's real entry identity: it hashes the normalized plan and passes
//! a zero query id into the key, so statements sharing a plan shape aggregate
//! into one entry. The statistics are instance-wide, read from
//! the one database where `CREATE EXTENSION pg_store_plans` ran. The vadv fork
//! and the ossc upstream expose different column sets and different plan-text
//! access paths, so they are separate type families (`1_004` vadv, `1_003`
//! ossc), not one layout with optional columns.
//!
//! `queryid_stat_statements` is best-effort attribution, not identity: the
//! extension overwrites it on every execution, so the value names the LAST
//! statement that ran this plan. Joining to `1_002` through it is valid only
//! under that caveat, and it stays `0` unless `compute_query_id = on`.
//!
//! `planid` identifies rows only within one instance, one server major, and
//! one extension version; it is not a portable identifier.
//! Timing columns are `f64`, so the layout derives `PartialEq` but not `Eq`.

use crate::{Section, StrId, Ts};

/// Type `1_004_001`: `pg_store_plans` (vadv fork, extension 2.x).
///
/// One row per plan entry of `pg_store_plans(false)`, top-N by `total_time`;
/// the row identity is `(userid, dbid, planid)`.
/// The `*_blk_*_time` columns are `0` when `track_io_timing` is off — an
/// unmeasured zero is indistinguishable from a true zero. The `*_plan_time`
/// columns are `0` without `pg_store_plans.track_planning`.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_004_001,
    name = "pg_store_plans_vadv",
    semantics = snapshot_full,
    sort_key("dbid", "userid", "planid")
)]
pub struct PgStorePlansVadvV1 {
    /// Collection time, unix microseconds; one value for all rows of a read.
    #[column(t)]
    pub ts: Ts,
    /// `pg_stat_statements` query id of the LAST statement that ran this
    /// plan (overwritten by the extension per execution); `0` when
    /// `compute_query_id` is off. Best-effort bridge to section `1_002`, not
    /// part of the row identity.
    #[column(l)]
    pub queryid_stat_statements: i64,
    /// Plan id derived from the normalized plan representation.
    #[column(l)]
    pub planid: i64,
    /// Role oid the statements ran as.
    #[column(l)]
    pub userid: u32,
    /// Database oid the statements ran in.
    #[column(l)]
    pub dbid: u32,
    /// Database name resolved from `dbid`; `None` when `dbid` has no
    /// `pg_database` row.
    #[column(l)]
    pub datname: Option<StrId>,
    /// Role name resolved from `userid`; `None` when `userid` has no
    /// `pg_roles` row.
    #[column(l)]
    pub usename: Option<StrId>,
    /// Plan text fetched via `pg_store_plans_get_plan`; `None` when the
    /// per-cycle plan-text budget was exhausted before this row.
    #[column(l)]
    pub plan: Option<StrId>,
    /// Executions accumulated for this plan entry.
    #[column(c)]
    pub calls: i64,
    /// Executions recorded through `pg_store_plans.slow_statement_duration`.
    #[column(c)]
    pub slow_log_calls: i64,
    /// Total execution time in milliseconds.
    #[column(c)]
    pub total_time: f64,
    /// Minimum execution time in milliseconds (resettable).
    #[column(g)]
    pub min_time: f64,
    /// Maximum execution time in milliseconds (resettable).
    #[column(g)]
    pub max_time: f64,
    /// Mean execution time in milliseconds (resettable).
    #[column(g)]
    pub mean_time: f64,
    /// Population standard deviation of execution time, milliseconds.
    #[column(g)]
    pub stddev_time: f64,
    /// Rows retrieved or affected.
    #[column(c)]
    pub rows: i64,
    /// Shared-block buffer hits.
    #[column(c)]
    pub shared_blks_hit: i64,
    /// Shared blocks read.
    #[column(c)]
    pub shared_blks_read: i64,
    /// Shared blocks dirtied.
    #[column(c)]
    pub shared_blks_dirtied: i64,
    /// Shared blocks written.
    #[column(c)]
    pub shared_blks_written: i64,
    /// Local-block buffer hits.
    #[column(c)]
    pub local_blks_hit: i64,
    /// Local blocks read.
    #[column(c)]
    pub local_blks_read: i64,
    /// Local blocks dirtied.
    #[column(c)]
    pub local_blks_dirtied: i64,
    /// Local blocks written.
    #[column(c)]
    pub local_blks_written: i64,
    /// Temp blocks read.
    #[column(c)]
    pub temp_blks_read: i64,
    /// Temp blocks written.
    #[column(c)]
    pub temp_blks_written: i64,
    /// Time reading blocks, milliseconds; `0` without `track_io_timing`.
    #[column(c)]
    pub blk_read_time: f64,
    /// Time writing blocks, milliseconds; `0` without `track_io_timing`.
    #[column(c)]
    pub blk_write_time: f64,
    /// When statistics for this entry began accumulating.
    #[column(g)]
    pub first_call: Ts,
    /// When the entry was last executed.
    #[column(g)]
    pub last_call: Ts,
    /// Total planning time in milliseconds; `0` without `track_planning`.
    #[column(c)]
    pub total_plan_time: f64,
    /// Minimum planning time in milliseconds; `0` without `track_planning`.
    #[column(g)]
    pub min_plan_time: f64,
    /// Maximum planning time in milliseconds; `0` without `track_planning`.
    #[column(g)]
    pub max_plan_time: f64,
    /// Mean planning time in milliseconds; `0` without `track_planning`.
    #[column(g)]
    pub mean_plan_time: f64,
}

#[cfg(test)]
mod tests {
    use super::PgStorePlansVadvV1;
    use crate::{Section, StrId, Ts, VerifiedSection, lint};

    fn row(ts: i64, dbid: u32, userid: u32, plan: Option<StrId>) -> PgStorePlansVadvV1 {
        PgStorePlansVadvV1 {
            ts: Ts(ts),
            queryid_stat_statements: 4_242_424_242_424,
            planid: -7_000_000_001,
            userid,
            dbid,
            datname: Some(StrId(1)),
            usename: Some(StrId(2)),
            plan,
            calls: 12,
            slow_log_calls: 1,
            total_time: 1_234.5,
            min_time: 0.5,
            max_time: 900.0,
            mean_time: 102.9,
            stddev_time: 3.3,
            rows: 400,
            shared_blks_hit: 10,
            shared_blks_read: 11,
            shared_blks_dirtied: 12,
            shared_blks_written: 13,
            local_blks_hit: 14,
            local_blks_read: 15,
            local_blks_dirtied: 16,
            local_blks_written: 17,
            temp_blks_read: 18,
            temp_blks_written: 19,
            blk_read_time: 20.5,
            blk_write_time: 21.5,
            first_call: Ts(ts - 5_000_000),
            last_call: Ts(ts - 1),
            total_plan_time: 7.5,
            min_plan_time: 0.1,
            max_plan_time: 2.0,
            mean_plan_time: 0.6,
        }
    }

    #[test]
    fn vadv_v1_contract_shape() {
        let c = PgStorePlansVadvV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_004_001);
        assert_eq!(c.columns.len(), 34);
        assert_eq!(c.sort_key, ["dbid", "userid", "planid"]);
        assert_eq!(c.column("ts").map(|col| col.nullable), Some(false));
        // The extension keys entries with a zero query id; the always-zero
        // column is not sealed.
        assert!(c.column("queryid").is_none());
        assert_eq!(
            c.column("queryid_stat_statements").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(c.column("plan").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("datname").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("usename").map(|col| col.nullable), Some(true));
        assert!(c.column("slow_log_calls").is_some());
        assert!(c.column("local_blks_hit").is_some());
        assert!(c.column("local_blks_dirtied").is_some());
        assert!(c.column("mean_plan_time").is_some());
        // The vadv fork sums I/O timings; the split ossc columns must not leak in.
        assert!(c.column("shared_blk_read_time").is_none());
        assert_eq!(lint(&[c]), Ok(()));
    }

    #[test]
    fn vadv_v1_roundtrip_preserves_null_plan() {
        crate::assert_roundtrips(&[row(1_000, 5, 10, Some(StrId(77))), row(1_000, 5, 11, None)]);
        let bytes = PgStorePlansVadvV1::encode(&[row(1_000, 5, 11, None)]).expect("encode");
        let decoded =
            PgStorePlansVadvV1::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].plan, None);
        assert_eq!(decoded[0].queryid_stat_statements, 4_242_424_242_424);
        assert!((decoded[0].total_time - 1_234.5).abs() < f64::EPSILON);
        assert_eq!(decoded[0].first_call, Ts(1_000 - 5_000_000));
    }

    #[test]
    fn vadv_v1_encode_sorts_by_key() {
        let bytes = PgStorePlansVadvV1::encode(&[
            row(1_000, 9, 3, None),
            row(1_000, 1, 8, None),
            row(1_000, 1, 2, None),
        ])
        .expect("encode");
        let decoded =
            PgStorePlansVadvV1::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(
            decoded
                .iter()
                .map(|r| (r.dbid, r.userid))
                .collect::<Vec<_>>(),
            [(1, 2), (1, 8), (9, 3)]
        );
    }
}

/// Type `1_003_001`: `pg_store_plans` (ossc upstream, extension 1.10).
///
/// One row per plan entry, top-N by `total_time`; unlike the vadv fork the
/// upstream keys an entry by `(userid, dbid, queryid, planid)` with the real
/// 64-bit core query id, so plans stay per-statement and `queryid` joins
/// section `1_002` directly. The extension does not record entries at all
/// when `compute_query_id` is off. I/O timings are split by block class
/// (extension 1.10); every `*_time` column is `0` without `track_io_timing`.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_003_001,
    name = "pg_store_plans_ossc",
    semantics = snapshot_full,
    sort_key("dbid", "userid", "queryid", "planid")
)]
pub struct PgStorePlansOsscV1 {
    /// Collection time, unix microseconds; one value for all rows of a read.
    #[column(t)]
    pub ts: Ts,
    /// Core query id, part of the entry identity; joins section `1_002`.
    #[column(l)]
    pub queryid: i64,
    /// Plan id derived from the normalized plan representation.
    #[column(l)]
    pub planid: i64,
    /// Role oid the statements ran as.
    #[column(l)]
    pub userid: u32,
    /// Database oid the statements ran in.
    #[column(l)]
    pub dbid: u32,
    /// Database name resolved from `dbid`; `None` when `dbid` has no
    /// `pg_database` row.
    #[column(l)]
    pub datname: Option<StrId>,
    /// Role name resolved from `userid`; `None` when `userid` has no
    /// `pg_roles` row.
    #[column(l)]
    pub usename: Option<StrId>,
    /// Plan text from the view, server-truncated per row; `None` when the
    /// per-cycle plan-text budget was exhausted before this row.
    #[column(l)]
    pub plan: Option<StrId>,
    /// Executions accumulated for this plan entry.
    #[column(c)]
    pub calls: i64,
    /// Total execution time in milliseconds.
    #[column(c)]
    pub total_time: f64,
    /// Minimum execution time in milliseconds (resettable).
    #[column(g)]
    pub min_time: f64,
    /// Maximum execution time in milliseconds (resettable).
    #[column(g)]
    pub max_time: f64,
    /// Mean execution time in milliseconds (resettable).
    #[column(g)]
    pub mean_time: f64,
    /// Population standard deviation of execution time, milliseconds.
    #[column(g)]
    pub stddev_time: f64,
    /// Rows retrieved or affected.
    #[column(c)]
    pub rows: i64,
    /// Shared-block buffer hits.
    #[column(c)]
    pub shared_blks_hit: i64,
    /// Shared blocks read.
    #[column(c)]
    pub shared_blks_read: i64,
    /// Shared blocks dirtied.
    #[column(c)]
    pub shared_blks_dirtied: i64,
    /// Shared blocks written.
    #[column(c)]
    pub shared_blks_written: i64,
    /// Local-block buffer hits.
    #[column(c)]
    pub local_blks_hit: i64,
    /// Local blocks read.
    #[column(c)]
    pub local_blks_read: i64,
    /// Local blocks dirtied.
    #[column(c)]
    pub local_blks_dirtied: i64,
    /// Local blocks written.
    #[column(c)]
    pub local_blks_written: i64,
    /// Temp blocks read.
    #[column(c)]
    pub temp_blks_read: i64,
    /// Temp blocks written.
    #[column(c)]
    pub temp_blks_written: i64,
    /// Time reading shared blocks, milliseconds.
    #[column(c)]
    pub shared_blk_read_time: f64,
    /// Time writing shared blocks, milliseconds.
    #[column(c)]
    pub shared_blk_write_time: f64,
    /// Time reading local blocks, milliseconds.
    #[column(c)]
    pub local_blk_read_time: f64,
    /// Time writing local blocks, milliseconds.
    #[column(c)]
    pub local_blk_write_time: f64,
    /// Time reading temp blocks, milliseconds.
    #[column(c)]
    pub temp_blk_read_time: f64,
    /// Time writing temp blocks, milliseconds.
    #[column(c)]
    pub temp_blk_write_time: f64,
    /// When statistics for this entry began accumulating.
    #[column(g)]
    pub first_call: Ts,
    /// When the entry was last executed.
    #[column(g)]
    pub last_call: Ts,
}

#[cfg(test)]
mod ossc_tests {
    use super::PgStorePlansOsscV1;
    use crate::{Section, StrId, Ts, VerifiedSection, lint};

    fn row(ts: i64, dbid: u32, queryid: i64, plan: Option<StrId>) -> PgStorePlansOsscV1 {
        PgStorePlansOsscV1 {
            ts: Ts(ts),
            queryid,
            planid: -7,
            userid: 10,
            dbid,
            datname: Some(StrId(1)),
            usename: Some(StrId(2)),
            plan,
            calls: 4,
            total_time: 99.5,
            min_time: 1.0,
            max_time: 50.0,
            mean_time: 24.9,
            stddev_time: 2.2,
            rows: 40,
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
            first_call: Ts(ts - 1_000),
            last_call: Ts(ts - 1),
        }
    }

    #[test]
    fn ossc_v1_contract_shape() {
        let c = PgStorePlansOsscV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_003_001);
        assert_eq!(c.columns.len(), 33);
        assert_eq!(c.sort_key, ["dbid", "userid", "queryid", "planid"]);
        // Upstream keys entries with the real core query id.
        assert_eq!(c.column("queryid").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("plan").map(|col| col.nullable), Some(true));
        assert!(c.column("shared_blk_read_time").is_some());
        assert!(c.column("temp_blk_write_time").is_some());
        // vadv-only columns must not leak into the upstream layout.
        assert!(c.column("queryid_stat_statements").is_none());
        assert!(c.column("slow_log_calls").is_none());
        assert!(c.column("total_plan_time").is_none());
        assert_eq!(lint(&[c]), Ok(()));
    }

    #[test]
    fn ossc_v1_roundtrip_preserves_null_plan() {
        crate::assert_roundtrips(&[row(1_000, 5, 42, Some(StrId(77))), row(1_000, 5, 43, None)]);
        let bytes = PgStorePlansOsscV1::encode(&[row(1_000, 5, 43, None)]).expect("encode");
        let decoded =
            PgStorePlansOsscV1::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].plan, None);
        assert_eq!(decoded[0].queryid, 43);
        assert!((decoded[0].shared_blk_read_time - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn ossc_v1_encode_sorts_by_key() {
        let bytes = PgStorePlansOsscV1::encode(&[
            row(1_000, 9, 3, None),
            row(1_000, 1, 8, None),
            row(1_000, 1, 2, None),
        ])
        .expect("encode");
        let decoded =
            PgStorePlansOsscV1::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(
            decoded
                .iter()
                .map(|r| (r.dbid, r.queryid))
                .collect::<Vec<_>>(),
            [(1, 2), (1, 8), (9, 3)]
        );
    }
}
