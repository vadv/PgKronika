use crate::buffering::{
    activity_dict_limits, push_activity, push_archiver, push_database, push_io, push_locks,
    push_prepared_xacts, push_progress_vacuum, push_replication_instance, push_statements,
    push_user_indexes, push_user_tables,
};
use kronika_source_pg::archiver::ArchiverRow;
use kronika_source_pg::database::{DatabaseRow, DatabaseVersion};
use kronika_source_pg::io::{IoRow, IoVersion};
use kronika_source_pg::locks::{LocksRow, LocksVersion};
use kronika_source_pg::prepared_xacts::PreparedXactsRow;
use kronika_source_pg::progress_vacuum::ProgressVacuumRow;
use kronika_source_pg::replication_instance::ReplicationInstanceRow;
use kronika_source_pg::statements::{StatementsRow, StatementsVersion};
use kronika_source_pg::user_indexes::{UserIndexesRow, UserIndexesVersion};
use kronika_source_pg::user_tables::{UserTablesRow, UserTablesVersion};
use kronika_source_pg::{ActivityRow, ActivityVersion};
use kronika_writer::{Interner, SectionBuffers, dict};

fn client_row(pid: i32) -> ActivityRow {
    ActivityRow {
        ts: 1_000,
        pid,
        leader_pid: None,
        datname: Some("appdb".to_owned()),
        usename: Some("alice".to_owned()),
        application_name: "psql".to_owned(),
        client_addr: String::new(),
        backend_type: "client backend".to_owned(),
        state: Some("active".to_owned()),
        wait_event_type: None,
        wait_event: None,
        query: Some("select 1".to_owned()),
        query_id: Some(42),
        backend_xid_age: None,
        backend_xmin_age: Some(7),
        backend_start: 100,
        xact_start: Some(500),
        query_start: Some(800),
        state_change: Some(900),
    }
}

#[test]
fn push_activity_buffers_rows_and_interns_their_strings() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_activity(
        &mut buffers,
        &mut interner,
        ActivityVersion::V3,
        &[client_row(1), client_row(2)],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "rows were buffered");

    // The buffered rows use dictionary ids, and the part carries the V3
    // activity section.
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "strings reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_001_003),
        "the part carries the pg_stat_activity section"
    );
}
fn db_row(datid: u32) -> DatabaseRow {
    DatabaseRow {
        ts: 1_000,
        datid,
        datname: if datid == 0 {
            None
        } else {
            Some("appdb".to_owned())
        },
        numbackends: if datid == 0 { Some(0) } else { Some(4) },
        xact_commit: 100,
        xact_rollback: 2,
        blks_read: 4_000,
        blks_hit: 90_000,
        tup_returned: 500,
        tup_fetched: 400,
        tup_inserted: 50,
        tup_updated: 30,
        tup_deleted: 10,
        conflicts: 0,
        temp_files: 1,
        temp_bytes: 8_192,
        deadlocks: 0,
        blk_read_time: 12.5,
        blk_write_time: 3.0,
        stats_reset: Some(1_500),
        checksum_failures: Some(0),
        checksum_last_failure: None,
        session_time: Some(1_000.0),
        active_time: Some(250.0),
        idle_in_transaction_time: Some(50.0),
        sessions: Some(7),
        sessions_abandoned: Some(1),
        sessions_fatal: Some(0),
        sessions_killed: Some(0),
        parallel_workers_to_launch: Some(9),
        parallel_workers_launched: Some(8),
        frozen_xid_age: if datid == 0 { None } else { Some(150_000_000) },
        min_mxid_age: if datid == 0 { None } else { Some(5_000_000) },
        datconnlimit: if datid == 0 { None } else { Some(-1) },
        datallowconn: if datid == 0 { None } else { Some(true) },
        datistemplate: if datid == 0 { None } else { Some(false) },
    }
}

#[test]
fn push_database_buffers_rows_and_interns_datname() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_database(
        &mut buffers,
        &mut interner,
        DatabaseVersion::V4,
        &[db_row(0), db_row(1)],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "rows were buffered");

    // The non-shared row's datname should be interned, and the part should
    // contain the V4 database section.
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "datname was interned");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_005_004),
        "the part carries the pg_stat_database section"
    );
}

fn ut_row(relid: u32) -> UserTablesRow {
    UserTablesRow {
        ts: 1_000,
        datid: 5,
        relid,
        schemaname: "public".to_owned(),
        relname: "accounts".to_owned(),
        tablespace: "pg_default".to_owned(),
        seq_scan: 10,
        seq_tup_read: 1_000,
        idx_scan: Some(7),
        idx_tup_fetch: Some(700),
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
        total_vacuum_time: None,
        total_autovacuum_time: None,
        total_analyze_time: None,
        total_autoanalyze_time: None,
        main_fork_bytes: 8_192,
        toast_bytes: None,
        toast_n_live_tup: None,
        toast_n_dead_tup: None,
        toast_last_autovacuum: None,
        xid_age: 100_000_000,
        mxid_age: 5_000_000,
        reltuples: 900,
        heap_blks_read: 400,
        heap_blks_hit: 90_000,
        idx_blks_read: Some(40),
        idx_blks_hit: Some(9_000),
        toast_blks_read: None,
        toast_blks_hit: None,
        tidx_blks_read: None,
        tidx_blks_hit: None,
    }
}

#[test]
fn push_user_tables_buffers_rows_and_interns_strings() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_user_tables(
        &mut buffers,
        &mut interner,
        &[(
            "appdb".to_owned(),
            UserTablesVersion::V3,
            vec![ut_row(16_384), ut_row(16_385)],
        )],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "rows were buffered");

    // The buffered rows use dictionary ids, and the part carries the V3
    // user-tables section.
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "strings reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_013_003),
        "the part carries the pg_stat_user_tables section"
    );
}

fn ui_row(indexrelid: u32) -> UserIndexesRow {
    UserIndexesRow {
        ts: 1_000,
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
        last_idx_scan: Some(900),
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
fn push_user_indexes_buffers_rows_and_interns_strings() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_user_indexes(
        &mut buffers,
        &mut interner,
        &[(
            "appdb".to_owned(),
            UserIndexesVersion::V2,
            vec![ui_row(16_385), ui_row(16_387)],
        )],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "rows were buffered");

    // The buffered rows use dictionary ids, and the part carries the V2
    // user-indexes section.
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "strings reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_014_002),
        "the part carries the pg_stat_user_indexes section"
    );
}

fn statements_row(queryid: i64) -> StatementsRow {
    StatementsRow {
        ts: 1_000,
        queryid: Some(queryid),
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
        stats_since: Some(500),
        minmax_stats_since: Some(800),
    }
}
#[test]
fn push_statements_buffers_rows_and_interns_strings() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_statements(
        &mut buffers,
        &mut interner,
        StatementsVersion::V6,
        &[statements_row(777), statements_row(888)],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "rows were buffered");

    // The buffered rows use dictionary ids, and the part carries the V6
    // statements section.
    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "strings reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_002_006),
        "the part carries the pg_stat_statements section"
    );
}
fn io_row(object: &str) -> IoRow {
    IoRow {
        ts: 1_000,
        backend_type: "client backend".to_owned(),
        object: object.to_owned(),
        context: "normal".to_owned(),
        reads: Some(100),
        read_bytes: Some(819_200),
        read_time: Some(12.5),
        writes: Some(50),
        write_bytes: Some(409_600),
        write_time: Some(3.0),
        writebacks: Some(0),
        writeback_time: None,
        extends: Some(7),
        extend_bytes: Some(57_344),
        extend_time: None,
        op_bytes: Some(8192),
        hits: Some(9000),
        evictions: Some(2),
        reuses: None,
        fsyncs: Some(1),
        fsync_time: None,
        stats_reset: Some(500),
    }
}

fn archiver_row() -> ArchiverRow {
    ArchiverRow {
        ts: 1_000,
        archived_count: 3,
        last_archived_wal: Some("00000001000000000000000A".to_owned()),
        last_archived_time: Some(900),
        failed_count: 1,
        last_failed_wal: Some("00000001000000000000000B".to_owned()),
        last_failed_time: Some(950),
        stats_reset: None,
    }
}

fn prepared_row() -> PreparedXactsRow {
    PreparedXactsRow {
        ts: 1_000,
        datname: "appdb".to_owned(),
        prepared_count: 1,
        max_age_us: 50_000,
        max_xid_age_tx: 4,
    }
}

fn progress_vacuum_row(phase: &str) -> ProgressVacuumRow {
    ProgressVacuumRow {
        ts: 1_000,
        pid: 42,
        datid: 16_385,
        datname: "appdb".to_owned(),
        relid: 16_384,
        is_autovacuum: true,
        phase: phase.to_owned(),
        heap_blks_total: 10_000,
        heap_blks_scanned: 4_200,
        heap_blks_vacuumed: 4_000,
        index_vacuum_count: 1,
        max_dead_tuples: Some(291_271),
        num_dead_tuples: Some(120_000),
        max_dead_tuple_bytes: None,
        dead_tuple_bytes: None,
        num_dead_item_ids: None,
        indexes_total: None,
        indexes_processed: None,
        delay_time: None,
    }
}

fn replication_instance_row() -> ReplicationInstanceRow {
    ReplicationInstanceRow {
        ts: 1_000,
        is_in_recovery: true,
        timeline_id: 2,
        synchronous_standby_names: b"*".to_vec(),
        synchronous_commit: b"remote_apply".to_vec(),
        wal_receiver_status: Some(b"streaming".to_vec()),
        sender_host: Some(b"primary.local".to_vec()),
        sender_port: Some(5432),
        slot_name: Some(b"standby_a".to_vec()),
        streaming_replicas: 0,
        replay_lag_s: Some(1),
        standby_receive_lsn: Some(1_024),
        standby_replay_lsn: Some(1_024),
        standby_last_replay_at: Some(900),
        current_wal_lsn: None,
        latest_end_lsn: Some(1_024),
        latest_end_time: Some(950),
        received_tli: Some(2),
    }
}
#[test]
fn push_progress_vacuum_buffers_rows_and_interns_labels() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_progress_vacuum(
        &mut buffers,
        &mut interner,
        &[progress_vacuum_row("scanning heap")],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "row was buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "labels reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_012_001),
        "the part carries the pg_stat_progress_vacuum section"
    );
}

#[test]
fn push_prepared_xacts_buffers_rows_and_interns_datname() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_prepared_xacts(&mut buffers, &mut interner, &[prepared_row()])
        .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "row was buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "datname reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_010_001),
        "the part carries the pg_prepared_xacts section"
    );
}

#[test]
fn push_archiver_buffers_row_and_interns_wal_names() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_archiver(&mut buffers, &mut interner, &archiver_row()).expect("push interns and buffers");
    assert!(!buffers.is_empty(), "row was buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(
        !dict_sections.is_empty(),
        "wal names reached the dictionary"
    );
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_008_001),
        "the part carries the pg_stat_archiver section"
    );
}

#[test]
fn push_io_buffers_rows_and_interns_labels() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_io(
        &mut buffers,
        &mut interner,
        IoVersion::V2,
        &[io_row("relation"), io_row("wal")],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "rows were buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(!dict_sections.is_empty(), "labels reached the dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_009_002),
        "the part carries the pg_stat_io section"
    );
}

#[test]
fn push_replication_instance_buffers_row_and_interns_labels() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_replication_instance(&mut buffers, &mut interner, &replication_instance_row())
        .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "row was buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    assert!(
        !dict_sections.is_empty(),
        "replication labels reached the dictionary"
    );
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_015_001),
        "the part carries the replication_instance section"
    );
}
fn locks_root_row() -> LocksRow {
    LocksRow {
        ts: 1_000_000,
        pid: 10,
        blocked_by: Vec::new(),
        depth: 0,
        root_pid: 10,
        datid: 16_384,
        datname: "app".to_owned(),
        usename: Some("postgres".to_owned()),
        application_name: "psql".to_owned(),
        client_addr: String::new(),
        backend_type: "client backend".to_owned(),
        state: Some("active".to_owned()),
        wait_event_type: None,
        wait_event: None,
        query: "select 1".to_owned(),
        backend_xid_age: None,
        backend_xmin_age: None,
        backend_start: Some(940_000),
        xact_start: Some(995_000),
        query_start: Some(999_000),
        state_change: Some(999_000),
        lock_locktype: None,
        lock_mode: None,
        lock_granted: None,
        lock_database: None,
        lock_relation: None,
        lock_relname: None,
        lock_page: None,
        lock_tuple: None,
        lock_virtualxid: None,
        lock_transactionid: None,
        lock_classid: None,
        lock_objid: None,
        lock_objsubid: None,
        lock_fastpath: None,
        lock_target: None,
        waitstart: None,
    }
}

#[test]
fn push_locks_buffers_v2_row_into_1_011_002_section() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_locks(
        &mut buffers,
        &mut interner,
        LocksVersion::V2,
        &[locks_root_row()],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "row was buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_011_002),
        "the part carries the pg_locks wait-tree V2 section"
    );
}

#[test]
fn push_locks_buffers_v1_row_into_1_011_001_section() {
    let mut buffers = SectionBuffers::new();
    let mut interner = Interner::new(activity_dict_limits());
    push_locks(
        &mut buffers,
        &mut interner,
        LocksVersion::V1,
        &[locks_root_row()],
    )
    .expect("push interns and buffers");
    assert!(!buffers.is_empty(), "row was buffered");

    let dict_sections = dict::encode(interner.window()).expect("encode dictionary");
    let part = buffers
        .flush(&dict_sections, 0)
        .expect("flush encodes the window")
        .expect("buffered rows produce a part");
    let catalog = kronika_format::validate_part(&part).expect("a valid container");
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.type_id == 1_011_001),
        "the part carries the pg_locks wait-tree V1 section"
    );
}
