//! Bounded `PostgreSQL` inputs used by incident gauge lenses.
#![allow(
    missing_docs,
    reason = "owned source rows mirror documented registry columns"
)]
use kronika_registry::incident_gauges::{
    PgFreezeHorizonV1, PgReplicationPhysicalV1, PgReplicationSlotRetentionV1,
    PgReplicationSlotRetentionV2, PgReplicationSlotRetentionV3, PgVacuumObservationV1,
};
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

use crate::replication_instance::LSN_BYTE_OFFSET_CEILING;

/// Local-process proof and `PostgreSQL` storage paths collected in one statement.
#[derive(Debug, Clone)]
pub struct LocalJoinFacts {
    pub ts: i64,
    pub backend_pid: i32,
    pub backend_start: i64,
    pub data_directory: String,
    pub tablespaces: Vec<(u32, String)>,
    pub tablespaces_complete: bool,
}

/// Collect the bounded facts needed for co-located OS joins.
///
/// # Errors
/// Returns the `PostgreSQL` query error unchanged.
pub async fn collect_local_join_facts(
    client: &Client,
    max_tablespaces: i64,
) -> Result<Option<LocalJoinFacts>, tokio_postgres::Error> {
    let rows = client
        .query(local_join_facts_query(), &[&max_tablespaces])
        .await?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    if !first.get::<_, bool>("local_connection") {
        return Ok(None);
    }
    let tablespaces_complete =
        i64::try_from(rows.len()).is_ok_and(|count| count <= max_tablespaces);
    Ok(Some(LocalJoinFacts {
        ts: first.get("ts_us"),
        backend_pid: first.get("backend_pid"),
        backend_start: first.get("backend_start_us"),
        data_directory: first.get("data_directory"),
        tablespaces: rows
            .iter()
            .take(usize::try_from(max_tablespaces).unwrap_or(0))
            .filter_map(|row| Some((row.get::<_, Option<u32>>("spcoid")?, row.get("location"))))
            .collect(),
        tablespaces_complete,
    }))
}

macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/incident_gauges.rs */ ",
            $sql,
        )
    };
}

/// Effective freeze limits for the oldest bounded relation candidates.
#[derive(Debug, Clone)]
pub struct FreezeHorizonRow {
    pub ts: i64,
    pub datid: u32,
    pub datname: String,
    pub relid: u32,
    pub schemaname: String,
    pub relname: String,
    pub xid_age: i64,
    pub xid_limit: i64,
    pub xid_is_toast: bool,
    pub mxid_age: i64,
    pub mxid_limit: i64,
    pub mxid_is_toast: bool,
}

/// SQL for the cross-source local join facts.
///
/// The `$1::int8` cast is load-bearing: without it `PostgreSQL` infers `$1`
/// from the `+ 1` integer literal as `int4`, and binding the `i64` argument
/// fails to serialize.
#[must_use]
pub const fn local_join_facts_query() -> &'static str {
    marked!(
        "WITH local AS ( \
           SELECT statement_timestamp() AS observed_at, pg_backend_pid() AS backend_pid, \
                  (SELECT backend_start FROM pg_stat_activity WHERE pid = pg_backend_pid()) AS backend_start, \
                  current_setting('data_directory')::text AS data_directory, \
                  inet_client_addr() IS NULL AS local_connection \
         ), spaces AS ( \
           SELECT oid, pg_tablespace_location(oid)::text AS location \
             FROM pg_tablespace WHERE pg_tablespace_location(oid) <> '' \
            ORDER BY oid LIMIT ($1::int8 + 1) \
         ) \
         SELECT (extract(epoch from l.observed_at) * 1e6)::int8 AS ts_us, \
                l.backend_pid, (extract(epoch from l.backend_start) * 1e6)::int8 AS backend_start_us, \
                l.data_directory, l.local_connection, s.oid AS spcoid, s.location \
           FROM local l LEFT JOIN spaces s ON true"
    )
}

/// SQL for separate XID and MXID top-N axes.
#[must_use]
pub const fn freeze_horizon_query() -> &'static str {
    marked!(
        "WITH settings AS ( \
           SELECT current_setting('autovacuum_freeze_max_age')::int8 AS xid_limit, \
                  current_setting('autovacuum_multixact_freeze_max_age')::int8 AS mxid_limit \
         ), ages AS ( \
           SELECT t.relid, t.schemaname::text AS schemaname, t.relname::text AS relname, \
                  c.reloptions, tc.reloptions AS toast_reloptions, \
                  age(c.relfrozenxid)::int8 AS base_xid_age, \
                  COALESCE(age(tc.relfrozenxid)::int8, -1) AS toast_xid_age, \
                  mxid_age(c.relminmxid)::int8 AS base_mxid_age, \
                  COALESCE(mxid_age(tc.relminmxid)::int8, -1) AS toast_mxid_age \
             FROM pg_stat_user_tables t \
             JOIN pg_class c ON c.oid = t.relid \
             LEFT JOIN pg_class tc ON tc.oid = c.reltoastrelid \
         ), candidates AS ( \
           (SELECT relid FROM ages ORDER BY GREATEST(base_xid_age, toast_xid_age) DESC, relid LIMIT $1) \
           UNION \
           (SELECT relid FROM ages ORDER BY GREATEST(base_mxid_age, toast_mxid_age) DESC, relid LIMIT $1) \
         ), database_age AS ( \
           SELECT oid AS datid, datname::text AS datname, age(datfrozenxid)::int8 AS xid_age, \
                  mxid_age(datminmxid)::int8 AS mxid_age FROM pg_database WHERE datname = current_database() \
         ), selected AS ( \
         SELECT (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                d.datid, d.datname, a.relid, a.schemaname, a.relname, \
                GREATEST(a.base_xid_age, a.toast_xid_age) AS xid_age, \
                COALESCE((SELECT option_value::int8 FROM pg_options_to_table( \
                    CASE WHEN a.toast_xid_age > a.base_xid_age THEN a.toast_reloptions ELSE a.reloptions END \
                ) WHERE option_name = 'autovacuum_freeze_max_age'), s.xid_limit) AS xid_limit, \
                a.toast_xid_age > a.base_xid_age AS xid_is_toast, \
                GREATEST(a.base_mxid_age, a.toast_mxid_age) AS mxid_age, \
                COALESCE((SELECT option_value::int8 FROM pg_options_to_table( \
                    CASE WHEN a.toast_mxid_age > a.base_mxid_age THEN a.toast_reloptions ELSE a.reloptions END \
                ) WHERE option_name = 'autovacuum_multixact_freeze_max_age'), s.mxid_limit) AS mxid_limit, \
                a.toast_mxid_age > a.base_mxid_age AS mxid_is_toast, \
                ((SELECT count(*) FROM pg_stat_user_tables) + 1)::int8 AS source_total \
           FROM ages a JOIN candidates c USING (relid) CROSS JOIN settings s \
           CROSS JOIN database_age d \
         ) \
         SELECT * FROM selected \
         UNION ALL \
         SELECT (extract(epoch from statement_timestamp()) * 1e6)::int8, d.datid, d.datname, \
                0::oid, ''::text, ''::text, d.xid_age, s.xid_limit, false, \
                d.mxid_age, s.mxid_limit, false, \
                ((SELECT count(*) FROM pg_stat_user_tables) + 1)::int8 \
           FROM database_age d CROSS JOIN settings s \
          ORDER BY relid"
    )
}

/// Collect freeze candidates and the source population size.
///
/// # Errors
/// Returns the `PostgreSQL` query error unchanged.
pub async fn collect_freeze_horizons(
    client: &Client,
    max_relations: i64,
) -> Result<(Vec<FreezeHorizonRow>, u64), tokio_postgres::Error> {
    let rows = client
        .query(freeze_horizon_query(), &[&max_relations])
        .await?;
    let total = rows
        .first()
        .map_or(0_i64, |row| row.get::<_, i64>("source_total"));
    Ok((
        rows.iter()
            .map(|row| FreezeHorizonRow {
                ts: row.get("ts_us"),
                datid: row.get("datid"),
                datname: row.get("datname"),
                relid: row.get("relid"),
                schemaname: row.get("schemaname"),
                relname: row.get("relname"),
                xid_age: row.get("xid_age"),
                xid_limit: row.get("xid_limit"),
                xid_is_toast: row.get("xid_is_toast"),
                mxid_age: row.get("mxid_age"),
                mxid_limit: row.get("mxid_limit"),
                mxid_is_toast: row.get("mxid_is_toast"),
            })
            .collect(),
        u64::try_from(total.max(0)).unwrap_or(u64::MAX),
    ))
}

/// Convert a freeze row using the caller's bounded interner.
///
/// # Errors
/// Returns the interner error unchanged.
pub fn to_freeze_horizon<E>(
    row: &FreezeHorizonRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgFreezeHorizonV1, E> {
    Ok(PgFreezeHorizonV1 {
        ts: Ts(row.ts),
        datid: row.datid,
        datname: intern(row.datname.as_bytes())?,
        relid: row.relid,
        schemaname: intern(row.schemaname.as_bytes())?,
        relname: intern(row.relname.as_bytes())?,
        xid_age: row.xid_age,
        xid_limit: row.xid_limit,
        xid_is_toast: row.xid_is_toast,
        mxid_age: row.mxid_age,
        mxid_limit: row.mxid_limit,
        mxid_is_toast: row.mxid_is_toast,
    })
}

/// Running-vacuum row before string interning.
#[derive(Debug, Clone)]
pub struct VacuumObservationRow {
    pub ts: i64,
    pub pid: i32,
    pub session_start_key: i64,
    pub query_start_key: i64,
    pub datid: u32,
    pub datname: String,
    pub relid: u32,
    pub phase: String,
    pub backend_type: Option<String>,
    pub activity_present: bool,
    pub is_autovacuum: Option<bool>,
    pub backend_start: Option<i64>,
    pub query_start: Option<i64>,
    pub elapsed_us: Option<i64>,
    pub clock_valid: Option<bool>,
}

#[must_use]
pub const fn vacuum_observation_query() -> &'static str {
    marked!(
        "WITH clock AS (SELECT statement_timestamp() AS observed_at) \
         SELECT (extract(epoch from c.observed_at) * 1e6)::int8 AS ts_us, \
                v.pid, COALESCE((extract(epoch from a.backend_start) * 1e6)::int8, 0) AS session_start_key, \
                COALESCE((extract(epoch from a.query_start) * 1e6)::int8, 0) AS query_start_key, \
                v.datid, v.datname::text AS datname, v.relid, v.phase::text AS phase, \
                a.backend_type::text AS backend_type, a.pid IS NOT NULL AS activity_present, \
                CASE WHEN a.pid IS NULL THEN NULL ELSE a.backend_type = 'autovacuum worker' END AS is_autovacuum, \
                (extract(epoch from a.backend_start) * 1e6)::int8 AS backend_start_us, \
                (extract(epoch from a.query_start) * 1e6)::int8 AS query_start_us, \
                CASE WHEN a.query_start IS NULL OR c.observed_at < a.query_start THEN NULL \
                     ELSE (extract(epoch from c.observed_at - a.query_start) * 1e6)::int8 END AS elapsed_us, \
                CASE WHEN a.query_start IS NULL THEN NULL ELSE c.observed_at >= a.query_start END AS clock_valid \
           FROM pg_stat_progress_vacuum v CROSS JOIN clock c \
           LEFT JOIN pg_stat_activity a ON a.pid = v.pid"
    )
}

/// Collect currently running vacuums with same-statement clock context.
///
/// # Errors
/// Returns the `PostgreSQL` query error unchanged.
pub async fn collect_vacuum_observations(
    client: &Client,
) -> Result<Vec<VacuumObservationRow>, tokio_postgres::Error> {
    let rows = client.query(vacuum_observation_query(), &[]).await?;
    Ok(rows
        .iter()
        .map(|row| VacuumObservationRow {
            ts: row.get("ts_us"),
            pid: row.get("pid"),
            session_start_key: row.get("session_start_key"),
            query_start_key: row.get("query_start_key"),
            datid: row.get("datid"),
            datname: row.get("datname"),
            relid: row.get("relid"),
            phase: row.get("phase"),
            backend_type: row.get("backend_type"),
            activity_present: row.get("activity_present"),
            is_autovacuum: row.get("is_autovacuum"),
            backend_start: row.get("backend_start_us"),
            query_start: row.get("query_start_us"),
            elapsed_us: row.get("elapsed_us"),
            clock_valid: row.get("clock_valid"),
        })
        .collect())
}

/// Convert a vacuum row using the caller's bounded interner.
///
/// # Errors
/// Returns the interner error unchanged.
pub fn to_vacuum_observation<E>(
    row: &VacuumObservationRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgVacuumObservationV1, E> {
    Ok(PgVacuumObservationV1 {
        ts: Ts(row.ts),
        pid: row.pid,
        session_start_key: row.session_start_key,
        query_start_key: row.query_start_key,
        datid: row.datid,
        datname: intern(row.datname.as_bytes())?,
        relid: row.relid,
        phase: intern(row.phase.as_bytes())?,
        backend_type: intern(row.backend_type.as_deref().unwrap_or("").as_bytes())?,
        activity_present: row.activity_present,
        is_autovacuum: row.is_autovacuum,
        backend_start: row.backend_start.map(Ts),
        query_start: row.query_start.map(Ts),
        elapsed_us: row.elapsed_us,
        clock_valid: row.clock_valid,
    })
}

/// Physical/logical scope and stage gaps for one walsender.
#[derive(Debug, Clone)]
pub struct ReplicationPhysicalRow {
    pub ts: i64,
    pub pid: i32,
    pub backend_start_key: i64,
    pub application_name: String,
    pub slot_name: Option<String>,
    pub slot_type: Option<String>,
    pub state: String,
    pub sync_state: String,
    pub scope_code: u8,
    pub state_code: u8,
    pub current_to_sent_bytes: Option<i64>,
    pub sent_to_write_bytes: Option<i64>,
    pub write_to_flush_bytes: Option<i64>,
    pub flush_to_replay_bytes: Option<i64>,
    pub write_lag_us: Option<i64>,
    pub flush_lag_us: Option<i64>,
    pub replay_lag_us: Option<i64>,
}

#[must_use]
pub const fn replication_physical_query() -> &'static str {
    marked!(
        "WITH clock AS (SELECT statement_timestamp() AS observed_at, \
                                CASE WHEN pg_is_in_recovery() THEN pg_last_wal_receive_lsn() ELSE pg_current_wal_lsn() END AS current_lsn) \
         SELECT (extract(epoch from c.observed_at) * 1e6)::int8 AS ts_us, r.pid, \
                (extract(epoch from r.backend_start) * 1e6)::int8 AS backend_start_key, \
                COALESCE(r.application_name, '')::text AS application_name, s.slot_name::text, s.slot_type, \
                COALESCE(r.state, '')::text AS state, COALESCE(r.sync_state, '')::text AS sync_state, \
                CASE s.slot_type WHEN 'physical' THEN 1 WHEN 'logical' THEN 2 ELSE 0 END::int2 AS scope_code, \
                CASE r.state WHEN 'startup' THEN 1 WHEN 'catchup' THEN 2 WHEN 'streaming' THEN 3 \
                     WHEN 'backup' THEN 4 WHEN 'stopping' THEN 5 ELSE 0 END::int2 AS state_code, \
                CASE WHEN r.sent_lsn IS NOT NULL AND c.current_lsn >= r.sent_lsn \
                     THEN LEAST(pg_wal_lsn_diff(c.current_lsn, r.sent_lsn), $1::int8::numeric)::int8 END AS current_to_sent_bytes, \
                CASE WHEN r.sent_lsn IS NOT NULL AND r.write_lsn IS NOT NULL AND r.sent_lsn >= r.write_lsn \
                     THEN LEAST(pg_wal_lsn_diff(r.sent_lsn, r.write_lsn), $1::int8::numeric)::int8 END AS sent_to_write_bytes, \
                CASE WHEN r.write_lsn IS NOT NULL AND r.flush_lsn IS NOT NULL AND r.write_lsn >= r.flush_lsn \
                     THEN LEAST(pg_wal_lsn_diff(r.write_lsn, r.flush_lsn), $1::int8::numeric)::int8 END AS write_to_flush_bytes, \
                CASE WHEN r.flush_lsn IS NOT NULL AND r.replay_lsn IS NOT NULL AND r.flush_lsn >= r.replay_lsn \
                     THEN LEAST(pg_wal_lsn_diff(r.flush_lsn, r.replay_lsn), $1::int8::numeric)::int8 END AS flush_to_replay_bytes, \
                (extract(epoch from r.write_lag) * 1e6)::int8 AS write_lag_us, \
                (extract(epoch from r.flush_lag) * 1e6)::int8 AS flush_lag_us, \
                (extract(epoch from r.replay_lag) * 1e6)::int8 AS replay_lag_us \
           FROM pg_stat_replication r CROSS JOIN clock c \
           LEFT JOIN LATERAL (SELECT slot_name, slot_type FROM pg_replication_slots \
                              WHERE active_pid = r.pid ORDER BY slot_name LIMIT 1) s ON true"
    )
}

/// Collect typed replication state and same-snapshot stage gaps.
///
/// # Errors
/// Returns the `PostgreSQL` query error unchanged.
pub async fn collect_replication_physical(
    client: &Client,
) -> Result<Vec<ReplicationPhysicalRow>, tokio_postgres::Error> {
    let rows = client
        .query(replication_physical_query(), &[&LSN_BYTE_OFFSET_CEILING])
        .await?;
    Ok(rows
        .iter()
        .map(|row| ReplicationPhysicalRow {
            ts: row.get("ts_us"),
            pid: row.get("pid"),
            backend_start_key: row.get("backend_start_key"),
            application_name: row.get("application_name"),
            slot_name: row.get("slot_name"),
            slot_type: row.get("slot_type"),
            state: row.get("state"),
            sync_state: row.get("sync_state"),
            scope_code: u8::try_from(row.get::<_, i16>("scope_code")).unwrap_or(0),
            state_code: u8::try_from(row.get::<_, i16>("state_code")).unwrap_or(0),
            current_to_sent_bytes: row.get("current_to_sent_bytes"),
            sent_to_write_bytes: row.get("sent_to_write_bytes"),
            write_to_flush_bytes: row.get("write_to_flush_bytes"),
            flush_to_replay_bytes: row.get("flush_to_replay_bytes"),
            write_lag_us: row.get("write_lag_us"),
            flush_lag_us: row.get("flush_lag_us"),
            replay_lag_us: row.get("replay_lag_us"),
        })
        .collect())
}

/// Convert a replication row using the caller's bounded interner.
///
/// # Errors
/// Returns the interner error unchanged.
pub fn to_replication_physical<E>(
    row: &ReplicationPhysicalRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgReplicationPhysicalV1, E> {
    Ok(PgReplicationPhysicalV1 {
        ts: Ts(row.ts),
        pid: row.pid,
        backend_start_key: row.backend_start_key,
        application_name: intern(row.application_name.as_bytes())?,
        slot_name: intern(row.slot_name.as_deref().unwrap_or("").as_bytes())?,
        slot_type: intern(row.slot_type.as_deref().unwrap_or("").as_bytes())?,
        state: intern(row.state.as_bytes())?,
        sync_state: intern(row.sync_state.as_bytes())?,
        scope_code: row.scope_code,
        state_code: row.state_code,
        current_to_sent_bytes: row.current_to_sent_bytes,
        sent_to_write_bytes: row.sent_to_write_bytes,
        write_to_flush_bytes: row.write_to_flush_bytes,
        flush_to_replay_bytes: row.flush_to_replay_bytes,
        write_lag_us: row.write_lag_us,
        flush_lag_us: row.flush_lag_us,
        replay_lag_us: row.replay_lag_us,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRetentionVersion {
    Pg15,
    Pg16,
    Pg17Plus,
}

#[must_use]
pub const fn slot_retention_version(major: u32) -> SlotRetentionVersion {
    if major >= 17 {
        SlotRetentionVersion::Pg17Plus
    } else if major >= 16 {
        SlotRetentionVersion::Pg16
    } else {
        SlotRetentionVersion::Pg15
    }
}

#[must_use]
pub const fn slot_retention_query(version: SlotRetentionVersion) -> &'static str {
    match version {
        SlotRetentionVersion::Pg15 => marked!(
            "WITH clock AS (SELECT statement_timestamp() AS observed_at, pg_is_in_recovery() AS recovery), \
             pos AS (SELECT c.*, CASE WHEN recovery THEN pg_last_wal_receive_lsn() ELSE pg_current_wal_lsn() END AS current_lsn FROM clock c), \
             cfg AS (SELECT setting::int8 AS keep_mb FROM pg_settings WHERE name = 'max_slot_wal_keep_size') \
             SELECT (extract(epoch from p.observed_at) * 1e6)::int8 AS ts_us, s.slot_name::text, s.slot_type, s.wal_status, \
                    s.active, s.active_pid, CASE WHEN s.restart_lsn IS NULL THEN NULL ELSE LEAST(pg_wal_lsn_diff(s.restart_lsn,'0/0'),$1::int8::numeric)::int8 END AS restart_lsn, \
                    CASE WHEN p.current_lsn IS NOT NULL AND s.restart_lsn IS NOT NULL AND p.current_lsn >= s.restart_lsn THEN LEAST(pg_wal_lsn_diff(p.current_lsn,s.restart_lsn),$1::int8::numeric)::int8 END AS retained_bytes, \
                    s.safe_wal_size, CASE WHEN cfg.keep_mb < 0 THEN NULL ELSE LEAST(cfg.keep_mb::numeric*1048576,$1::int8::numeric)::int8 END AS max_slot_wal_keep_size_bytes, \
                    CASE s.wal_status WHEN 'reserved' THEN 1 WHEN 'extended' THEN 2 WHEN 'unreserved' THEN 3 WHEN 'lost' THEN 4 ELSE 0 END::int2 AS wal_status_code, \
                    p.recovery AS is_in_recovery, NULL::bool AS conflicting, NULL::text AS invalidation_reason \
               FROM pg_replication_slots s CROSS JOIN pos p CROSS JOIN cfg"
        ),
        SlotRetentionVersion::Pg16 => marked!(
            "WITH clock AS (SELECT statement_timestamp() AS observed_at, pg_is_in_recovery() AS recovery), \
             pos AS (SELECT c.*, CASE WHEN recovery THEN pg_last_wal_receive_lsn() ELSE pg_current_wal_lsn() END AS current_lsn FROM clock c), \
             cfg AS (SELECT setting::int8 AS keep_mb FROM pg_settings WHERE name = 'max_slot_wal_keep_size') \
             SELECT (extract(epoch from p.observed_at) * 1e6)::int8 AS ts_us, s.slot_name::text, s.slot_type, s.wal_status, \
                    s.active, s.active_pid, CASE WHEN s.restart_lsn IS NULL THEN NULL ELSE LEAST(pg_wal_lsn_diff(s.restart_lsn,'0/0'),$1::int8::numeric)::int8 END AS restart_lsn, \
                    CASE WHEN p.current_lsn IS NOT NULL AND s.restart_lsn IS NOT NULL AND p.current_lsn >= s.restart_lsn THEN LEAST(pg_wal_lsn_diff(p.current_lsn,s.restart_lsn),$1::int8::numeric)::int8 END AS retained_bytes, \
                    s.safe_wal_size, CASE WHEN cfg.keep_mb < 0 THEN NULL ELSE LEAST(cfg.keep_mb::numeric*1048576,$1::int8::numeric)::int8 END AS max_slot_wal_keep_size_bytes, \
                    CASE s.wal_status WHEN 'reserved' THEN 1 WHEN 'extended' THEN 2 WHEN 'unreserved' THEN 3 WHEN 'lost' THEN 4 ELSE 0 END::int2 AS wal_status_code, \
                    p.recovery AS is_in_recovery, s.conflicting, NULL::text AS invalidation_reason \
               FROM pg_replication_slots s CROSS JOIN pos p CROSS JOIN cfg"
        ),
        SlotRetentionVersion::Pg17Plus => marked!(
            "WITH clock AS (SELECT statement_timestamp() AS observed_at, pg_is_in_recovery() AS recovery), \
             pos AS (SELECT c.*, CASE WHEN recovery THEN pg_last_wal_receive_lsn() ELSE pg_current_wal_lsn() END AS current_lsn FROM clock c), \
             cfg AS (SELECT setting::int8 AS keep_mb FROM pg_settings WHERE name = 'max_slot_wal_keep_size') \
             SELECT (extract(epoch from p.observed_at) * 1e6)::int8 AS ts_us, s.slot_name::text, s.slot_type, s.wal_status, \
                    s.active, s.active_pid, CASE WHEN s.restart_lsn IS NULL THEN NULL ELSE LEAST(pg_wal_lsn_diff(s.restart_lsn,'0/0'),$1::int8::numeric)::int8 END AS restart_lsn, \
                    CASE WHEN p.current_lsn IS NOT NULL AND s.restart_lsn IS NOT NULL AND p.current_lsn >= s.restart_lsn THEN LEAST(pg_wal_lsn_diff(p.current_lsn,s.restart_lsn),$1::int8::numeric)::int8 END AS retained_bytes, \
                    s.safe_wal_size, CASE WHEN cfg.keep_mb < 0 THEN NULL ELSE LEAST(cfg.keep_mb::numeric*1048576,$1::int8::numeric)::int8 END AS max_slot_wal_keep_size_bytes, \
                    CASE s.wal_status WHEN 'reserved' THEN 1 WHEN 'extended' THEN 2 WHEN 'unreserved' THEN 3 WHEN 'lost' THEN 4 ELSE 0 END::int2 AS wal_status_code, \
                    p.recovery AS is_in_recovery, s.conflicting, s.invalidation_reason \
               FROM pg_replication_slots s CROSS JOIN pos p CROSS JOIN cfg"
        ),
    }
}

/// Version-neutral slot row.
#[derive(Debug, Clone)]
pub struct SlotRetentionRow {
    pub ts: i64,
    pub slot_name: String,
    pub slot_type: String,
    pub wal_status: Option<String>,
    pub active: bool,
    pub active_pid: Option<i32>,
    pub restart_lsn: Option<i64>,
    pub retained_bytes: Option<i64>,
    pub safe_wal_size: Option<i64>,
    pub max_slot_wal_keep_size_bytes: Option<i64>,
    pub wal_status_code: u8,
    pub is_in_recovery: bool,
    pub conflicting: Option<bool>,
    pub invalidation_reason: Option<String>,
}

/// Collect the slot layout supported by `major`.
///
/// # Errors
/// Returns the `PostgreSQL` query error unchanged.
pub async fn collect_slot_retention(
    client: &Client,
    major: u32,
) -> Result<(SlotRetentionVersion, Vec<SlotRetentionRow>), tokio_postgres::Error> {
    let version = slot_retention_version(major);
    let rows = client
        .query(slot_retention_query(version), &[&LSN_BYTE_OFFSET_CEILING])
        .await?;
    Ok((
        version,
        rows.iter()
            .map(|row| SlotRetentionRow {
                ts: row.get("ts_us"),
                slot_name: row.get("slot_name"),
                slot_type: row.get("slot_type"),
                wal_status: row.get("wal_status"),
                active: row.get("active"),
                active_pid: row.get("active_pid"),
                restart_lsn: row.get("restart_lsn"),
                retained_bytes: row.get("retained_bytes"),
                safe_wal_size: row.get("safe_wal_size"),
                max_slot_wal_keep_size_bytes: row.get("max_slot_wal_keep_size_bytes"),
                wal_status_code: u8::try_from(row.get::<_, i16>("wal_status_code")).unwrap_or(0),
                is_in_recovery: row.get("is_in_recovery"),
                conflicting: row.get("conflicting"),
                invalidation_reason: row.get("invalidation_reason"),
            })
            .collect(),
    ))
}

fn intern_or_empty<E>(
    value: Option<&str>,
    intern: &mut impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<StrId, E> {
    intern(value.unwrap_or("").as_bytes())
}

fn invalidation_code(reason: Option<&str>) -> u8 {
    match reason {
        None => 0,
        Some("wal_removed") => 1,
        Some("rows_removed") => 2,
        Some("wal_level_insufficient") => 3,
        Some("idle_timeout") => 4,
        Some(_) => 255,
    }
}

/// Convert a PG15 slot row using the caller's bounded interner.
///
/// # Errors
/// Returns the interner error unchanged.
pub fn to_slot_retention_v1<E>(
    row: &SlotRetentionRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgReplicationSlotRetentionV1, E> {
    Ok(PgReplicationSlotRetentionV1 {
        ts: Ts(row.ts),
        slot_name: intern(row.slot_name.as_bytes())?,
        slot_type: intern(row.slot_type.as_bytes())?,
        wal_status: intern_or_empty(row.wal_status.as_deref(), &mut intern)?,
        active: row.active,
        active_pid: row.active_pid,
        restart_lsn: row.restart_lsn,
        retained_bytes: row.retained_bytes,
        safe_wal_size: row.safe_wal_size,
        max_slot_wal_keep_size_bytes: row.max_slot_wal_keep_size_bytes,
        wal_status_code: row.wal_status_code,
        is_in_recovery: row.is_in_recovery,
    })
}

/// Convert a PG16 slot row using the caller's bounded interner.
///
/// # Errors
/// Returns the interner error unchanged.
pub fn to_slot_retention_v2<E>(
    row: &SlotRetentionRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgReplicationSlotRetentionV2, E> {
    Ok(PgReplicationSlotRetentionV2 {
        ts: Ts(row.ts),
        slot_name: intern(row.slot_name.as_bytes())?,
        slot_type: intern(row.slot_type.as_bytes())?,
        wal_status: intern_or_empty(row.wal_status.as_deref(), &mut intern)?,
        active: row.active,
        active_pid: row.active_pid,
        restart_lsn: row.restart_lsn,
        retained_bytes: row.retained_bytes,
        safe_wal_size: row.safe_wal_size,
        max_slot_wal_keep_size_bytes: row.max_slot_wal_keep_size_bytes,
        wal_status_code: row.wal_status_code,
        is_in_recovery: row.is_in_recovery,
        conflicting: row.conflicting,
    })
}

/// Convert a PG17+ slot row using the caller's bounded interner.
///
/// # Errors
/// Returns the interner error unchanged.
pub fn to_slot_retention_v3<E>(
    row: &SlotRetentionRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgReplicationSlotRetentionV3, E> {
    let invalidation_reason = intern_or_empty(row.invalidation_reason.as_deref(), &mut intern)?;
    Ok(PgReplicationSlotRetentionV3 {
        ts: Ts(row.ts),
        slot_name: intern(row.slot_name.as_bytes())?,
        slot_type: intern(row.slot_type.as_bytes())?,
        wal_status: intern_or_empty(row.wal_status.as_deref(), &mut intern)?,
        invalidation_reason,
        active: row.active,
        active_pid: row.active_pid,
        restart_lsn: row.restart_lsn,
        retained_bytes: row.retained_bytes,
        safe_wal_size: row.safe_wal_size,
        max_slot_wal_keep_size_bytes: row.max_slot_wal_keep_size_bytes,
        wal_status_code: row.wal_status_code,
        is_in_recovery: row.is_in_recovery,
        conflicting: row.conflicting,
        invalidation_code: invalidation_code(row.invalidation_reason.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_join_query_casts_the_limit_parameter() {
        let sql = local_join_facts_query();
        assert!(
            sql.contains("LIMIT ($1::int8 + 1)"),
            "the LIMIT parameter must be cast to int8 or an i64 bind fails to serialize"
        );
        assert!(sql.contains("inet_client_addr() IS NULL AS local_connection"));
    }

    #[test]
    fn freeze_query_has_separate_axes_and_effective_overrides() {
        let sql = freeze_horizon_query();
        assert!(sql.contains("base_xid_age"));
        assert!(sql.contains("base_mxid_age"));
        assert!(sql.contains("autovacuum_freeze_max_age"));
        assert!(sql.contains("autovacuum_multixact_freeze_max_age"));
        assert_eq!(sql.matches("LIMIT $1").count(), 2);
    }

    #[test]
    fn vacuum_query_keeps_missing_activity_and_clock_errors_nullable() {
        let sql = vacuum_observation_query();
        assert!(sql.contains("LEFT JOIN pg_stat_activity"));
        assert!(sql.contains("query_start_key"));
        assert!(sql.contains("c.observed_at < a.query_start THEN NULL"));
        assert!(!sql.contains("COALESCE(a.backend_type"));
    }

    #[test]
    fn replication_query_uses_order_checked_same_snapshot_gaps() {
        let sql = replication_physical_query();
        assert!(sql.contains("active_pid = r.pid"));
        assert!(sql.contains("backend_start_key"));
        assert!(sql.contains("slot_type"));
        assert!(sql.contains("r.flush_lsn >= r.replay_lsn"));
        assert!(!sql.contains("GREATEST(0"));
    }

    #[test]
    fn slot_layouts_follow_pg15_through_pg18() {
        assert_eq!(slot_retention_version(15), SlotRetentionVersion::Pg15);
        assert_eq!(slot_retention_version(16), SlotRetentionVersion::Pg16);
        assert_eq!(slot_retention_version(17), SlotRetentionVersion::Pg17Plus);
        assert_eq!(slot_retention_version(18), SlotRetentionVersion::Pg17Plus);
        assert!(slot_retention_query(SlotRetentionVersion::Pg15).contains("safe_wal_size"));
        assert!(slot_retention_query(SlotRetentionVersion::Pg16).contains("s.conflicting"));
        assert!(
            slot_retention_query(SlotRetentionVersion::Pg17Plus).contains("s.invalidation_reason")
        );
    }

    #[test]
    fn invalidation_codes_are_stable_and_unknown_is_not_valid() {
        assert_eq!(invalidation_code(None), 0);
        assert_eq!(invalidation_code(Some("wal_removed")), 1);
        assert_eq!(invalidation_code(Some("future_reason")), 255);
    }
}
