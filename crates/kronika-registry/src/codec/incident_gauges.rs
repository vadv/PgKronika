//! Durable inputs for incident gauge lenses.
#![allow(
    missing_docs,
    reason = "column contracts are documented in docs/type-registry/postgresql.md"
)]

use crate::{Section, StrId, Ts};

/// Effective XID and MXID freeze limits for one bounded relation candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_031_001,
    name = "pg_freeze_horizon",
    semantics = snapshot_full,
    sort_key("datid", "relid", "ts")
)]
pub struct PgFreezeHorizonV1 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub datid: u32,
    #[column(l)]
    pub datname: StrId,
    #[column(l)]
    pub relid: u32,
    #[column(l)]
    pub schemaname: StrId,
    #[column(l)]
    pub relname: StrId,
    #[column(g)]
    pub xid_age: i64,
    #[column(g)]
    pub xid_limit: i64,
    #[column(g)]
    pub xid_is_toast: bool,
    #[column(g)]
    pub mxid_age: i64,
    #[column(g)]
    pub mxid_limit: i64,
    #[column(g)]
    pub mxid_is_toast: bool,
}

/// One running vacuum with same-statement activity and server-clock context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_032_001,
    name = "pg_vacuum_observation",
    semantics = conditional_full,
    sort_key("pid", "session_start_key", "ts")
)]
pub struct PgVacuumObservationV1 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub pid: i32,
    #[column(l)]
    pub session_start_key: i64,
    #[column(l)]
    pub datid: u32,
    #[column(l)]
    pub datname: StrId,
    #[column(l)]
    pub relid: u32,
    #[column(l)]
    pub phase: StrId,
    #[column(l)]
    pub backend_type: StrId,
    #[column(g)]
    pub activity_present: bool,
    #[column(g)]
    pub is_autovacuum: Option<bool>,
    #[column(g)]
    pub backend_start: Option<Ts>,
    #[column(g)]
    pub query_start: Option<Ts>,
    #[column(g)]
    pub elapsed_us: Option<i64>,
    #[column(g)]
    pub clock_valid: Option<bool>,
}

/// Same-snapshot replication state and byte gaps for one walsender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_033_001,
    name = "pg_replication_physical",
    semantics = snapshot_full,
    sort_key("pid", "ts")
)]
pub struct PgReplicationPhysicalV1 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub pid: i32,
    #[column(l)]
    pub application_name: StrId,
    #[column(l)]
    pub slot_name: StrId,
    #[column(l)]
    pub slot_type: StrId,
    #[column(l)]
    pub state: StrId,
    #[column(l)]
    pub sync_state: StrId,
    /// `0=unknown`, `1=physical`, `2=logical`.
    #[column(g)]
    pub scope_code: u8,
    /// `0=unknown`, `1=startup`, `2=catchup`, `3=streaming`, `4=backup`, `5=stopping`.
    #[column(g)]
    pub state_code: u8,
    #[column(g)]
    pub current_to_sent_bytes: Option<i64>,
    #[column(g)]
    pub sent_to_write_bytes: Option<i64>,
    #[column(g)]
    pub write_to_flush_bytes: Option<i64>,
    #[column(g)]
    pub flush_to_replay_bytes: Option<i64>,
    #[column(g)]
    pub write_lag_us: Option<i64>,
    #[column(g)]
    pub flush_lag_us: Option<i64>,
    #[column(g)]
    pub replay_lag_us: Option<i64>,
}

/// PG15 replication-slot retention and headroom state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_034_001,
    name = "pg_replication_slot_retention",
    semantics = snapshot_full,
    sort_key("slot_name", "ts")
)]
pub struct PgReplicationSlotRetentionV1 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub slot_name: StrId,
    #[column(l)]
    pub slot_type: StrId,
    #[column(l)]
    pub wal_status: StrId,
    #[column(g)]
    pub active: bool,
    #[column(g)]
    pub active_pid: Option<i32>,
    #[column(g)]
    pub restart_lsn: Option<i64>,
    #[column(g)]
    pub retained_bytes: Option<i64>,
    #[column(g)]
    pub safe_wal_size: Option<i64>,
    #[column(g)]
    pub max_slot_wal_keep_size_bytes: Option<i64>,
    /// `0=unknown`, `1=reserved`, `2=extended`, `3=unreserved`, `4=lost`.
    #[column(g)]
    pub wal_status_code: u8,
    #[column(g)]
    pub is_in_recovery: bool,
}

/// PG16 adds the recovery-conflict flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_034_002,
    name = "pg_replication_slot_retention",
    semantics = snapshot_full,
    sort_key("slot_name", "ts")
)]
pub struct PgReplicationSlotRetentionV2 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub slot_name: StrId,
    #[column(l)]
    pub slot_type: StrId,
    #[column(l)]
    pub wal_status: StrId,
    #[column(g)]
    pub active: bool,
    #[column(g)]
    pub active_pid: Option<i32>,
    #[column(g)]
    pub restart_lsn: Option<i64>,
    #[column(g)]
    pub retained_bytes: Option<i64>,
    #[column(g)]
    pub safe_wal_size: Option<i64>,
    #[column(g)]
    pub max_slot_wal_keep_size_bytes: Option<i64>,
    #[column(g)]
    pub wal_status_code: u8,
    #[column(g)]
    pub is_in_recovery: bool,
    #[column(g)]
    pub conflicting: Option<bool>,
}

/// PG17+ adds an explicit invalidation reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_034_003,
    name = "pg_replication_slot_retention",
    semantics = snapshot_full,
    sort_key("slot_name", "ts")
)]
pub struct PgReplicationSlotRetentionV3 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub slot_name: StrId,
    #[column(l)]
    pub slot_type: StrId,
    #[column(l)]
    pub wal_status: StrId,
    #[column(l)]
    pub invalidation_reason: StrId,
    #[column(g)]
    pub active: bool,
    #[column(g)]
    pub active_pid: Option<i32>,
    #[column(g)]
    pub restart_lsn: Option<i64>,
    #[column(g)]
    pub retained_bytes: Option<i64>,
    #[column(g)]
    pub safe_wal_size: Option<i64>,
    #[column(g)]
    pub max_slot_wal_keep_size_bytes: Option<i64>,
    #[column(g)]
    pub wal_status_code: u8,
    #[column(g)]
    pub is_in_recovery: bool,
    #[column(g)]
    pub conflicting: Option<bool>,
    /// `0=valid`, `1=wal_removed`, `2=rows_removed`, `3=wal_level`, `4=idle_timeout`, `255=other`.
    #[column(g)]
    pub invalidation_code: u8,
}

/// Redacted mapping from a PostgreSQL storage role to a proven local mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_036_001,
    name = "pg_storage_mount",
    semantics = snapshot_full,
    sort_key("role", "path_hash_hi", "path_hash_lo", "ts")
)]
pub struct PgStorageMountV1 {
    #[column(t)]
    pub ts: Ts,
    /// `1=data`, `2=wal`, `3=tablespace`.
    #[column(l)]
    pub role: u8,
    #[column(l)]
    pub path_hash_hi: u64,
    #[column(l)]
    pub path_hash_lo: u64,
    #[column(l)]
    pub mount_hash_hi: u64,
    #[column(l)]
    pub mount_hash_lo: u64,
    #[column(l)]
    pub mount_namespace: u64,
    /// `1=mapped`; other values describe typed collection failures.
    #[column(g)]
    pub mapping_state: u8,
    #[column(g)]
    pub total_bytes: Option<i64>,
    #[column(g)]
    pub available_bytes: Option<i64>,
}

/// Redacted, race-checked PostgreSQL process to cgroup-memory observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_037_001,
    name = "pg_process_cgroup_memory",
    semantics = snapshot_full,
    sort_key("process_hash_hi", "process_hash_lo", "ts")
)]
pub struct PgProcessCgroupMemoryV1 {
    #[column(t)]
    pub ts: Ts,
    #[column(l)]
    pub process_hash_hi: u64,
    #[column(l)]
    pub process_hash_lo: u64,
    #[column(l)]
    pub cgroup_hash_hi: u64,
    #[column(l)]
    pub cgroup_hash_lo: u64,
    /// `1=v1`, `2=v2`.
    #[column(l)]
    pub hierarchy: u8,
    /// `1=verified`; other values describe typed collection failures.
    #[column(g)]
    pub mapping_state: u8,
    #[column(g)]
    pub current_bytes: Option<i64>,
    #[column(g)]
    pub max_bytes: Option<i64>,
    #[column(g)]
    pub max_unlimited: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint;

    #[test]
    fn contracts_are_bounded_and_versioned() {
        let contracts = [
            PgFreezeHorizonV1::CONTRACT,
            PgVacuumObservationV1::CONTRACT,
            PgReplicationPhysicalV1::CONTRACT,
            PgReplicationSlotRetentionV1::CONTRACT,
            PgReplicationSlotRetentionV2::CONTRACT,
            PgReplicationSlotRetentionV3::CONTRACT,
            PgStorageMountV1::CONTRACT,
            PgProcessCgroupMemoryV1::CONTRACT,
        ];
        assert_eq!(lint(&contracts), Ok(()));
        assert_eq!(contracts[0].type_id.get(), 1_031_001);
        assert_eq!(contracts[3].columns.len() + 1, contracts[4].columns.len());
        assert_eq!(contracts[4].columns.len() + 2, contracts[5].columns.len());
    }

    #[test]
    fn null_and_zero_remain_distinct() {
        crate::assert_roundtrips(&[PgReplicationSlotRetentionV1 {
            ts: Ts(1),
            slot_name: StrId(1),
            slot_type: StrId(2),
            wal_status: StrId(3),
            active: false,
            active_pid: None,
            restart_lsn: None,
            retained_bytes: None,
            safe_wal_size: Some(0),
            max_slot_wal_keep_size_bytes: None,
            wal_status_code: 0,
            is_in_recovery: true,
        }]);
    }
}
