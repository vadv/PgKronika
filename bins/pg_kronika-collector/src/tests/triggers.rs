use crate::main_sources::{activity_needs_acceleration, replication_needs_acceleration};
use kronika_source_pg::ActivityRow;
use kronika_source_pg::replication_details::{ReplicaRow, SlotRow};
use kronika_source_pg::replication_instance::ReplicationInstanceRow;

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

fn trigger_replica_row(replay_lag_us: Option<i64>) -> ReplicaRow {
    ReplicaRow {
        ts: 1_000,
        pid: 9,
        usename: "repl".to_owned(),
        application_name: "walreceiver".to_owned(),
        client_addr: None,
        state: "streaming".to_owned(),
        sync_state: "async".to_owned(),
        sync_priority: Some(0),
        sent_lsn: Some(2_048),
        write_lsn: Some(2_048),
        flush_lsn: Some(2_048),
        replay_lsn: Some(1_024),
        write_lag_us: None,
        flush_lag_us: None,
        replay_lag_us,
    }
}

fn trigger_slot_row(retained_bytes: Option<i64>) -> SlotRow {
    SlotRow {
        ts: 1_000,
        slot_name: "standby_a".to_owned(),
        plugin: None,
        slot_type: "physical".to_owned(),
        active: true,
        restart_lsn: Some(1_024),
        confirmed_flush_lsn: None,
        retained_bytes,
        wal_status: Some("reserved".to_owned()),
    }
}
#[test]
fn activity_accelerates_on_lock_waiters_or_active_pressure() {
    let mut waiter = client_row(1);
    waiter.wait_event_type = Some("Lock".to_owned());
    assert!(activity_needs_acceleration(&[waiter], 100));

    let busy: Vec<_> = (0..3).map(client_row).collect();
    assert!(activity_needs_acceleration(&busy, 3));
    assert!(
        !activity_needs_acceleration(&busy, 4),
        "below the threshold, no lock waiters: base pace"
    );

    let mut walsender = client_row(5);
    walsender.backend_type = "walsender".to_owned();
    assert!(
        !activity_needs_acceleration(&[walsender], 1),
        "only client backends count toward the threshold"
    );
}

#[test]
fn replication_accelerates_on_lag_or_retained_wal() {
    let gib = 1_024 * 1_024 * 1_024;
    let calm_instance = ReplicationInstanceRow {
        replay_lag_s: None,
        ..replication_instance_row()
    };

    assert!(
        !replication_needs_acceleration(&calm_instance, &[], &[], 10, gib),
        "nothing lags, nothing is retained"
    );
    assert!(
        replication_needs_acceleration(&replication_instance_row(), &[], &[], 1, gib),
        "this standby replays behind the trigger"
    );
    assert!(
        replication_needs_acceleration(
            &calm_instance,
            &[trigger_replica_row(Some(11_000_000))],
            &[],
            10,
            gib
        ),
        "a replica replays behind the trigger"
    );
    assert!(
        !replication_needs_acceleration(
            &calm_instance,
            &[trigger_replica_row(Some(9_000_000))],
            &[],
            10,
            gib
        ),
        "a replica under the trigger stays at base pace"
    );
    assert!(
        replication_needs_acceleration(
            &calm_instance,
            &[],
            &[trigger_slot_row(Some(gib))],
            10,
            gib
        ),
        "a slot retains enough WAL to trip the trigger"
    );
}
