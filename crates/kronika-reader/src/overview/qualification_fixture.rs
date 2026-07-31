//! Versioned canonical fixtures shared by overview qualification tests.
//!
//! These fixtures deliberately use only registry-backed source rows. Tests
//! must not construct canonical fact blocks directly because that would skip
//! the production extraction, identity, reset, coverage, and loss rules that
//! the qualification suite is intended to prove.

use std::collections::BTreeMap;

use arrow_array::RecordBatch;
use kronika_format::{PartMeta, SectionInput, build_part, crc32c};
use kronika_registry::collection_coverage::CollectionCoverageV1;
use kronika_registry::incident_gauges::{
    PgProcessCgroupMemoryV1, PgReplicationPhysicalV1, PgReplicationSlotRetentionV3,
    PgStorageMountV2,
};
use kronika_registry::instance_metadata::InstanceMetadata;
use kronika_registry::os_cgroup_memory::OsCgroupMemory;
use kronika_registry::os_vmstat::OsVmstat;
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::pg_stat_database::PgStatDatabaseV4;
use kronika_registry::replication_instance::ReplicationInstance;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{
    Bytes, Section, StrId, Ts, VerifiedSection, decode_any, encode_sealed_batches,
};

/// Schema version for [`all_family_fixture`].
#[cfg(test)]
pub(super) const ALL_FAMILY_SCHEMA_VERSION: u16 = 3;

/// One encoded registry section retained by a fixture part.
#[derive(Debug, Clone)]
pub(super) struct FixtureSection {
    pub type_id: u32,
    pub rows: u32,
    pub body: Vec<u8>,
}

/// One completed active part and its canonical registry sections.
#[derive(Debug, Clone)]
pub(super) struct FixturePart {
    sections: Vec<FixtureSection>,
    min_ts_us: i64,
    max_ts_us: i64,
}

impl FixturePart {
    /// Registry sections in catalog order.
    pub(super) fn sections(&self) -> &[FixtureSection] {
        &self.sections
    }
}

/// A fixed source stream plus its active-part and sealed representations.
#[derive(Debug, Clone)]
pub(super) struct VersionedFixture {
    #[cfg(test)]
    pub schema_version: u16,
    #[cfg(test)]
    #[cfg(test)]
    pub cadence_us: i64,
    pub parts: Vec<FixturePart>,
}

impl VersionedFixture {
    /// Every contiguous grouping of the fixture's completed parts.
    ///
    /// Every cut mask is represented. Each group coalesces rows by `type_id`,
    /// so every emitted PGM is canonical while all variants retain the same
    /// source rows and temporal partition boundaries.
    #[cfg(test)]
    pub(super) fn contiguous_partitions(&self) -> Vec<Vec<Vec<u8>>> {
        let boundary_count = self.parts.len().saturating_sub(1);
        let variant_count = 1_usize
            .checked_shl(u32::try_from(boundary_count).expect("fixture boundary count fits u32"))
            .expect("fixture partition variants fit usize");
        (0..variant_count)
            .map(|cut_mask| {
                let mut encoded = Vec::new();
                let mut sections = Vec::new();
                let mut group_min = None;
                let mut group_max = None;
                for (index, part) in self.parts.iter().enumerate() {
                    sections.extend(part.sections().iter().cloned());
                    group_min = Some(
                        group_min.map_or(part.min_ts_us, |value: i64| value.min(part.min_ts_us)),
                    );
                    group_max = Some(
                        group_max.map_or(part.max_ts_us, |value: i64| value.max(part.max_ts_us)),
                    );
                    let cut_after =
                        index + 1 == self.parts.len() || cut_mask & (1_usize << index) != 0;
                    if cut_after {
                        encoded.push(build_fixture_part(
                            &sections,
                            group_min.expect("group has a minimum"),
                            group_max.expect("group has a maximum"),
                        ));
                        sections.clear();
                        group_min = None;
                        group_max = None;
                    }
                }
                encoded
            })
            .collect()
    }

    /// Encodes the same registry stream as one sealed PGM unit.
    pub(super) fn sealed_bytes(&self) -> Vec<u8> {
        let sections: Vec<_> = self
            .parts
            .iter()
            .flat_map(|part| part.sections().iter().cloned())
            .collect();
        let (min_ts_us, max_ts_us) = fixture_range(&self.parts);
        build_fixture_part(&sections, min_ts_us, max_ts_us)
    }
}

/// Builds the canonical small qualification fixture.
///
/// The four snapshots intentionally span every populated overview family:
///
/// - retained lifecycle observations and policy-neutral event facts;
/// - every allow-listed `PostgreSQL`, OS and cgroup counter and gauge factor;
/// - all three reset families and explicit metadata changes;
/// - sender and slot state transitions plus complete-empty disappearance;
/// - filesystem capacity and zero-capacity transition facts;
/// - every stable factor inventory row, including explicit unsupported rows;
/// - exact and failed source coverage, including a collector failure fact.
pub(super) fn all_family_fixture() -> VersionedFixture {
    VersionedFixture {
        #[cfg(test)]
        schema_version: ALL_FAMILY_SCHEMA_VERSION,
        #[cfg(test)]
        #[cfg(test)]
        cadence_us: 10,
        parts: vec![
            fixture_part(10, 0),
            fixture_part(20, 1),
            fixture_part(30, 2),
            fixture_part(40, 3),
        ],
    }
}

fn fixture_part(ts_us: i64, snapshot: u8) -> FixturePart {
    let resets = [reset_row(ts_us, snapshot)];
    let instance = [instance_row(ts_us, snapshot)];
    let database = [database_row(ts_us, snapshot)];
    let replication = [replication_instance_row(ts_us, snapshot)];
    let storage = [storage_row(ts_us, snapshot)];
    let process_cgroup = [process_cgroup_row(ts_us, snapshot)];
    let vmstat = [vmstat_row(ts_us, snapshot)];
    let cgroup = [cgroup_row(ts_us, snapshot)];
    let lifecycle = [lifecycle_row(
        ts_us,
        [2, 0, 1, 2][usize::from(snapshot)],
        (snapshot == 1).then_some(42),
        (snapshot == 1).then_some(9),
    )];
    let coverage = coverage_rows(ts_us, snapshot);
    let collection_coverage = collection_coverage_rows(ts_us);

    let mut sections = vec![
        encoded_section(1_005_004, &database),
        encoded_section(1_015_001, &replication),
        encoded_section(1_020_001, &resets),
        encoded_section(1_021_001, &instance),
        encoded_section(1_023_001, &collection_coverage),
        encoded_section(1_028_001, &lifecycle),
        encoded_section(1_036_002, &storage),
        encoded_section(1_037_001, &process_cgroup),
        encoded_section(1_106_001, &vmstat),
        encoded_section(1_202_001, &cgroup),
    ];
    if snapshot < 2 {
        let sender = sender_row(ts_us, [3, 4][usize::from(snapshot)]);
        sections.push(encoded_section(1_033_001, &[sender]));
    }
    let slot = slot_row(ts_us, snapshot);
    sections.push(encoded_section(1_034_003, &[slot]));
    sections.extend([encoded_section(1_038_001, &coverage)]);
    sections.sort_unstable_by_key(|section| section.type_id);

    FixturePart {
        sections,
        min_ts_us: ts_us,
        max_ts_us: ts_us,
    }
}

fn canonical_fixture_sections(sections: &[FixtureSection]) -> Vec<FixtureSection> {
    let mut grouped = BTreeMap::<u32, (u32, Vec<RecordBatch>)>::new();
    for section in sections {
        let verified = VerifiedSection::verify(
            Bytes::copy_from_slice(&section.body),
            crc32c(&section.body),
            crc32c,
        )
        .expect("qualification section CRC matches");
        let decoded = decode_any(section.type_id, verified).expect("decode qualification section");
        assert_eq!(
            decoded.stats.rows, section.rows as usize,
            "qualification section row count matches its catalog entry"
        );
        let (rows, batches) = grouped.entry(section.type_id).or_default();
        *rows = rows
            .checked_add(section.rows)
            .expect("qualification row count fits u32");
        batches.extend(decoded.batches);
    }

    grouped
        .into_iter()
        .map(|(type_id, (rows, batches))| FixtureSection {
            type_id,
            rows,
            body: encode_sealed_batches(type_id, batches)
                .expect("encode sealed qualification section"),
        })
        .collect()
}

fn fixture_range(parts: &[FixturePart]) -> (i64, i64) {
    let min_ts_us = parts
        .iter()
        .map(|part| part.min_ts_us)
        .min()
        .expect("qualification fixture has parts");
    let max_ts_us = parts
        .iter()
        .map(|part| part.max_ts_us)
        .max()
        .expect("qualification fixture has parts");
    (min_ts_us, max_ts_us)
}

fn build_fixture_part(sections: &[FixtureSection], min_ts_us: i64, max_ts_us: i64) -> Vec<u8> {
    let bodies = canonical_fixture_sections(sections);
    let inputs: Vec<_> = bodies
        .iter()
        .map(|section| SectionInput {
            type_id: section.type_id,
            rows: section.rows,
            body: section.body.as_slice(),
        })
        .collect();
    build_part(
        &inputs,
        PartMeta {
            min_ts: min_ts_us,
            max_ts: max_ts_us,
        },
    )
}

fn encoded_section<S: Section>(type_id: u32, rows: &[S]) -> FixtureSection {
    FixtureSection {
        type_id,
        rows: u32::try_from(rows.len()).expect("qualification row count fits u32"),
        body: S::encode(rows).expect("encode qualification section"),
    }
}

const fn lifecycle_row(
    ts_us: i64,
    kind: u8,
    pid: Option<i32>,
    signal: Option<i32>,
) -> PgLogLifecycleV1 {
    PgLogLifecycleV1 {
        ts: Ts(ts_us),
        kind,
        pid,
        signal,
        shutdown_mode: None,
        message: None,
        query_detail: None,
        dict_dropped_fields: 0,
    }
}

const fn reset_row(ts_us: i64, snapshot: u8) -> ResetMetadata {
    ResetMetadata {
        ts: Ts(ts_us),
        postmaster_start_time: Ts(if snapshot < 3 { 1 } else { 35 }),
        pg_stat_database_reset_max_at: Some(Ts(if snapshot < 3 { 1 } else { 35 })),
        pg_stat_statements_reset_at: None,
        pg_store_plans_reset_at: None,
        pg_stat_bgwriter_reset_at: None,
        pg_stat_checkpointer_reset_at: None,
        pg_stat_wal_reset_at: None,
        pg_stat_archiver_reset_at: None,
        pg_stat_io_reset_at: None,
        ext_pg_stat_statements_version: None,
        ext_pg_store_plans_version: None,
        compute_query_id: None,
        track_io_timing: None,
        track_wal_io_timing: None,
    }
}

const fn instance_row(ts_us: i64, snapshot: u8) -> InstanceMetadata {
    InstanceMetadata {
        ts: Ts(ts_us),
        hostname: StrId(1),
        pg_version_num: 18_00_00,
        kernel_version: StrId(3),
        pg_system_identifier: Some(4),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4_096,
        boot_id: StrId(if snapshot < 3 { 5 } else { 6 }),
        btime: Ts(if snapshot < 3 { 2 } else { 35 }),
    }
}

fn database_row(ts_us: i64, snapshot: u8) -> PgStatDatabaseV4 {
    let increment = i64::from(snapshot.min(1));
    PgStatDatabaseV4 {
        ts: Ts(ts_us),
        datid: 16_384,
        datname: None,
        numbackends: Some(7 + i32::from(snapshot)),
        xact_commit: 100 + i64::from(snapshot) * 5,
        xact_rollback: 1,
        blks_read: 2,
        blks_hit: 3,
        tup_returned: 4,
        tup_fetched: 5,
        tup_inserted: 6,
        tup_updated: 7,
        tup_deleted: 8,
        conflicts: 2 + increment,
        temp_files: 1,
        temp_bytes: 256,
        deadlocks: 1 + increment,
        blk_read_time: 1.5,
        blk_write_time: 2.5,
        stats_reset: Some(Ts(if snapshot < 3 { 1 } else { 35 })),
        frozen_xid_age: Some(1_000),
        min_mxid_age: Some(100),
        datconnlimit: Some(100),
        datallowconn: Some(true),
        datistemplate: Some(false),
        checksum_failures: 3 + increment,
        checksum_last_failure: (snapshot == 1).then_some(Ts(ts_us)),
        session_time: 10.0 + f64::from(snapshot),
        active_time: 5.0 + f64::from(snapshot),
        idle_in_transaction_time: 1.0,
        sessions: 20 + i64::from(snapshot),
        sessions_abandoned: 4 + increment,
        sessions_fatal: 5 + increment,
        sessions_killed: 6 + increment,
        parallel_workers_to_launch: 0,
        parallel_workers_launched: 0,
    }
}

fn cgroup_row(ts_us: i64, snapshot: u8) -> OsCgroupMemory {
    let current = 1_024 * (i64::from(snapshot) + 1);
    let increment = i64::from(snapshot.min(1));
    OsCgroupMemory {
        ts: Ts(ts_us),
        cgroup_path: StrId(10),
        current,
        max: Some(8_192),
        anon: current / 2,
        file: current / 4,
        kernel: current / 8,
        slab: current / 16,
        low_events: 0,
        high_events: 1 + increment,
        max_events: 1 + increment,
        oom_events: increment,
        oom_kill: increment,
        scope: 1,
    }
}

fn replication_instance_row(ts_us: i64, snapshot: u8) -> ReplicationInstance {
    ReplicationInstance {
        ts: Ts(ts_us),
        is_in_recovery: snapshot != 0,
        timeline_id: if snapshot == 0 { 1 } else { 2 },
        synchronous_standby_names: StrId(20),
        synchronous_commit: StrId(21),
        wal_receiver_status: (snapshot != 0).then_some(StrId(22)),
        sender_host: (snapshot != 0).then_some(StrId(23)),
        sender_port: (snapshot != 0).then_some(5_432),
        slot_name: (snapshot != 0).then_some(StrId(24)),
        streaming_replicas: i32::from(snapshot == 0),
        replay_lag_s: (snapshot != 0).then_some(3 - i64::from(snapshot)),
        standby_receive_lsn: (snapshot != 0).then_some(1_000 + i64::from(snapshot)),
        standby_replay_lsn: (snapshot != 0).then_some(900 + i64::from(snapshot)),
        standby_last_replay_at: (snapshot != 0).then_some(Ts(ts_us - 1)),
        current_wal_lsn: (snapshot == 0).then_some(800),
        latest_end_lsn: (snapshot != 0).then_some(1_100 + i64::from(snapshot)),
        latest_end_time: (snapshot != 0).then_some(Ts(ts_us - 1)),
        received_tli: (snapshot != 0).then_some(2),
    }
}

fn slot_row(ts_us: i64, snapshot: u8) -> PgReplicationSlotRetentionV3 {
    let state_code = [1, 2, 4, 4][usize::from(snapshot)];
    PgReplicationSlotRetentionV3 {
        ts: Ts(ts_us),
        slot_name: StrId(30),
        slot_type: StrId(31),
        wal_status: StrId(32 + u64::from(snapshot.min(2))),
        invalidation_reason: StrId(if snapshot < 2 { 34 } else { 35 }),
        active: snapshot == 0,
        active_pid: (snapshot == 0).then_some(42),
        restart_lsn: Some(1_000),
        retained_bytes: Some(256),
        safe_wal_size: Some(4_096),
        max_slot_wal_keep_size_bytes: Some(8_192),
        wal_status_code: state_code,
        is_in_recovery: false,
        conflicting: Some(false),
        invalidation_code: snapshot / 2,
    }
}

fn storage_row(ts_us: i64, snapshot: u8) -> PgStorageMountV2 {
    PgStorageMountV2 {
        ts: Ts(ts_us),
        role: 1,
        path_hash_hi: 1,
        path_hash_lo: 2,
        mount_hash_hi: 3,
        mount_hash_lo: 4,
        mount_namespace: 5,
        mapping_state: 1,
        total_bytes: Some(16_384),
        available_bytes: Some([4_096, 0, 1_024, 2_048][usize::from(snapshot)]),
        major: Some(8),
        minor: Some(1),
        block_device_exact: true,
    }
}

fn process_cgroup_row(ts_us: i64, snapshot: u8) -> PgProcessCgroupMemoryV1 {
    PgProcessCgroupMemoryV1 {
        ts: Ts(ts_us),
        process_hash_hi: 1,
        process_hash_lo: 2,
        cgroup_hash_hi: 3,
        cgroup_hash_lo: 4,
        hierarchy: 2,
        mapping_state: 1,
        current_bytes: Some(2_048 + i64::from(snapshot) * 128),
        max_bytes: Some(8_192),
        max_unlimited: false,
    }
}

fn vmstat_row(ts_us: i64, snapshot: u8) -> OsVmstat {
    OsVmstat {
        ts: Ts(ts_us),
        pgpgin: Some(10),
        pgpgout: Some(20),
        pswpin: Some(0),
        pswpout: Some(0),
        pgfault: Some(30),
        pgmajfault: Some(1),
        pgsteal_kswapd: Some(0),
        pgsteal_direct: Some(0),
        pgscan_kswapd: Some(0),
        pgscan_direct: Some(0),
        oom_kill: Some(i64::from(snapshot.min(1))),
        scope: 0,
    }
}

const fn sender_row(ts_us: i64, state_code: u8) -> PgReplicationPhysicalV1 {
    PgReplicationPhysicalV1 {
        ts: Ts(ts_us),
        pid: 42,
        backend_start_key: 5,
        application_name: StrId(11),
        slot_name: StrId(12),
        slot_type: StrId(13),
        state: StrId(14),
        sync_state: StrId(15),
        scope_code: 1,
        state_code,
        current_to_sent_bytes: Some(1),
        sent_to_write_bytes: Some(2),
        write_to_flush_bytes: Some(3),
        flush_to_replay_bytes: Some(4),
        write_lag_us: Some(5),
        flush_lag_us: Some(6),
        replay_lag_us: Some(7),
    }
}

fn coverage_rows(ts_us: i64, snapshot: u8) -> Vec<SnapshotCoverageV1> {
    [
        1_005_004, 1_014_001, 1_015_001, 1_020_001, 1_033_001, 1_034_003, 1_036_002, 1_037_001,
        1_106_001, 1_202_001,
    ]
    .into_iter()
    .map(|section_type_id| {
        if section_type_id == 1_014_001 {
            coverage_row(section_type_id, ts_us, 1, 2, 1)
        } else if snapshot == 2 && section_type_id == 1_005_004 {
            coverage_row(section_type_id, ts_us, 3, 1, 0)
        } else if snapshot >= 2 && section_type_id == 1_033_001 {
            coverage_row(section_type_id, ts_us, 0, 0, 0)
        } else {
            coverage_row(section_type_id, ts_us, 0, 1, 1)
        }
    })
    .collect()
}

const fn collection_coverage_rows(ts_us: i64) -> [CollectionCoverageV1; 1] {
    [CollectionCoverageV1 {
        ts: Ts(ts_us),
        section_type_id: 1_014_001,
        total: 2,
        unknown_total: false,
        collected: 1,
        max_n: 1,
        order_by: StrId(40),
        cutoff_value: None,
        reason: 0,
    }]
}

const fn coverage_row(
    section_type_id: u32,
    ts_us: i64,
    read_state: u8,
    source_total: u32,
    collected: u32,
) -> SnapshotCoverageV1 {
    SnapshotCoverageV1 {
        ts: Ts(ts_us),
        section_type_id,
        collector_pid: 99,
        collector_started_at: Ts(1),
        read_state,
        visibility: 0,
        source_total,
        collected,
    }
}
