//! Versioned canonical fixtures shared by overview qualification tests.
//!
//! These fixtures deliberately use only registry-backed source rows. Tests
//! must not construct canonical fact blocks directly because that would skip
//! the production extraction, identity, reset, coverage, and loss rules that
//! the qualification suite is intended to prove.

use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::incident_gauges::PgReplicationPhysicalV1;
use kronika_registry::instance_metadata::InstanceMetadata;
use kronika_registry::os_cgroup_memory::OsCgroupMemory;
use kronika_registry::pg_log::PgLogLifecycleV1;
use kronika_registry::pg_stat_database::PgStatDatabaseV1;
use kronika_registry::reset_metadata::ResetMetadata;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{Section, StrId, Ts};

/// Schema version for [`all_family_fixture`].
pub(super) const ALL_FAMILY_SCHEMA_VERSION: u16 = 1;

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
    pub schema_version: u16,
    pub source_id: u64,
    pub cadence_us: i64,
    pub parts: Vec<FixturePart>,
}

impl VersionedFixture {
    /// Every contiguous grouping of the fixture's completed parts.
    ///
    /// With three source snapshots this returns all four possible cut masks:
    /// one, either two-way split, and three independent active parts. Section
    /// order remains identical to the sealed catalog, which is the provenance
    /// condition required for a lossless promotion.
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
        let min_ts_us = self
            .parts
            .iter()
            .map(|part| part.min_ts_us)
            .min()
            .expect("qualification fixture has parts");
        let max_ts_us = self
            .parts
            .iter()
            .map(|part| part.max_ts_us)
            .max()
            .expect("qualification fixture has parts");
        build_fixture_part(&sections, min_ts_us, max_ts_us)
    }
}

/// Builds the canonical small qualification fixture.
///
/// The three snapshots intentionally span every populated overview block:
///
/// - retained lifecycle observations and policy-neutral event facts;
/// - `PostgreSQL` and OS counters plus gauges;
/// - `PostgreSQL` and boot reset epochs;
/// - replication entity states and a complete-empty disappearance boundary;
/// - exact and failed source coverage, including a collector failure fact.
pub(super) fn all_family_fixture() -> VersionedFixture {
    VersionedFixture {
        schema_version: ALL_FAMILY_SCHEMA_VERSION,
        source_id: 7,
        cadence_us: 10,
        parts: vec![
            fixture_part(
                10,
                database_row(10, 100, 7, 3),
                cgroup_row(10, 1_024, 1, 1, 0, 0),
                Some(sender_row(10, 3)),
                lifecycle_row(10, 2, None, None),
                &[
                    coverage_row(1_005_001, 10, 0, 1, 1),
                    coverage_row(1_033_001, 10, 0, 1, 1),
                    coverage_row(1_202_001, 10, 0, 1, 1),
                ],
            ),
            fixture_part(
                20,
                database_row(20, 105, 8, 4),
                cgroup_row(20, 2_048, 2, 1, 1, 1),
                Some(sender_row(20, 4)),
                lifecycle_row(20, 0, Some(42), Some(9)),
                &[
                    coverage_row(1_005_001, 20, 0, 1, 1),
                    coverage_row(1_033_001, 20, 0, 1, 1),
                    coverage_row(1_202_001, 20, 0, 1, 1),
                ],
            ),
            fixture_part(
                30,
                database_row(30, 105, 8, 4),
                cgroup_row(30, 2_048, 2, 1, 1, 1),
                None,
                lifecycle_row(30, 1, None, None),
                &[
                    coverage_row(1_005_001, 30, 3, 1, 0),
                    coverage_row(1_033_001, 30, 0, 0, 0),
                    coverage_row(1_202_001, 30, 0, 1, 1),
                ],
            ),
        ],
    }
}

fn fixture_part(
    ts_us: i64,
    database: PgStatDatabaseV1,
    cgroup: OsCgroupMemory,
    sender: Option<PgReplicationPhysicalV1>,
    lifecycle: PgLogLifecycleV1,
    coverage: &[SnapshotCoverageV1],
) -> FixturePart {
    let resets = [reset_row(ts_us)];
    let instance = [instance_row(ts_us)];
    let database = [database];
    let cgroup = [cgroup];
    let lifecycle = [lifecycle];

    let mut sections = vec![
        encoded_section(1_005_001, &database),
        encoded_section(1_020_001, &resets),
        encoded_section(1_021_001, &instance),
        encoded_section(1_028_001, &lifecycle),
    ];
    if let Some(sender) = sender {
        sections.push(encoded_section(1_033_001, &[sender]));
    }
    sections.extend([
        encoded_section(1_038_001, coverage),
        encoded_section(1_202_001, &cgroup),
    ]);
    sections.sort_unstable_by_key(|section| section.type_id);

    FixturePart {
        sections,
        min_ts_us: ts_us,
        max_ts_us: ts_us,
    }
}

fn build_fixture_part(sections: &[FixtureSection], min_ts_us: i64, max_ts_us: i64) -> Vec<u8> {
    let inputs: Vec<_> = sections
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
            source_id: 7,
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

fn lifecycle_row(ts_us: i64, kind: u8, pid: Option<i32>, signal: Option<i32>) -> PgLogLifecycleV1 {
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

fn reset_row(ts_us: i64) -> ResetMetadata {
    ResetMetadata {
        ts: Ts(ts_us),
        postmaster_start_time: Ts(1),
        pg_stat_database_reset_max_at: None,
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

fn instance_row(ts_us: i64) -> InstanceMetadata {
    InstanceMetadata {
        ts: Ts(ts_us),
        hostname: StrId(1),
        node_self_id: StrId(2),
        pg_version_num: 18_00_00,
        kernel_version: StrId(3),
        pg_system_identifier: Some(4),
        clock_ticks_per_sec: 100,
        page_size_bytes: 4_096,
        boot_id: StrId(5),
        btime: Ts(2),
    }
}

fn database_row(
    ts_us: i64,
    xact_commit: i64,
    deadlocks: i64,
    numbackends: i32,
) -> PgStatDatabaseV1 {
    PgStatDatabaseV1 {
        ts: Ts(ts_us),
        datid: 16_384,
        datname: None,
        numbackends: Some(numbackends),
        xact_commit,
        xact_rollback: 1,
        blks_read: 2,
        blks_hit: 3,
        tup_returned: 4,
        tup_fetched: 5,
        tup_inserted: 6,
        tup_updated: 7,
        tup_deleted: 8,
        conflicts: 0,
        temp_files: 1,
        temp_bytes: 256,
        deadlocks,
        blk_read_time: 1.5,
        blk_write_time: 2.5,
        stats_reset: None,
        frozen_xid_age: Some(1_000),
        min_mxid_age: Some(100),
        datconnlimit: Some(100),
        datallowconn: Some(true),
        datistemplate: Some(false),
    }
}

fn cgroup_row(
    ts_us: i64,
    current: i64,
    high_events: i64,
    max_events: i64,
    oom_events: i64,
    oom_kill: i64,
) -> OsCgroupMemory {
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
        high_events,
        max_events,
        oom_events,
        oom_kill,
        scope: 1,
    }
}

fn sender_row(ts_us: i64, state_code: u8) -> PgReplicationPhysicalV1 {
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

fn coverage_row(
    source_type_id: u32,
    ts_us: i64,
    read_state: u8,
    source_total: u32,
    collected: u32,
) -> SnapshotCoverageV1 {
    SnapshotCoverageV1 {
        ts: Ts(ts_us),
        source_type_id,
        collector_pid: 99,
        collector_started_at: Ts(1),
        read_state,
        visibility: 0,
        source_total,
        collected,
    }
}
