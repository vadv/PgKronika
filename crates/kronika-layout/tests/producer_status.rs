//! Producer-status codec and layout-scan integration.

use rustix as _;
use serde as _;
use serde_json as _;

use kronika_layout::{
    DataRoot, LayoutLimits, PRODUCER_STATUS_TEMP_NAME, ProducerState, ProducerStatus,
    RetentionStatus, read_producer_status, write_producer_status,
};

#[test]
fn producer_status_round_trips_running_and_stopped_states_atomically() {
    let directory = tempfile::tempdir().expect("tempdir");
    let running = ProducerStatus::running(
        42,
        1_000,
        2_000,
        Some(RetentionStatus::fixed(1_073_741_824)),
    );
    write_producer_status(directory.path(), &running).expect("write running");
    assert_eq!(
        read_producer_status(directory.path()).expect("read running"),
        Some(running)
    );
    assert!(
        !directory.path().join(PRODUCER_STATUS_TEMP_NAME).exists(),
        "atomic temporary must not remain after publication"
    );

    let stopped = running.stopped(3_000);
    assert_eq!(stopped.state, ProducerState::Stopped);
    write_producer_status(directory.path(), &stopped).expect("write stopped");
    assert_eq!(
        read_producer_status(directory.path()).expect("read stopped"),
        Some(stopped)
    );
}

#[test]
fn producer_status_and_its_atomic_temporary_are_reserved_layout_controls() {
    let directory = tempfile::tempdir().expect("tempdir");
    write_producer_status(
        directory.path(),
        &ProducerStatus::running(
            7,
            10,
            20,
            Some(RetentionStatus::auto(80).expect("retention")),
        ),
    )
    .expect("write status");
    std::fs::write(directory.path().join(PRODUCER_STATUS_TEMP_NAME), b"pending")
        .expect("write simulated temporary");

    let root = DataRoot::open(directory.path()).expect("open root");
    let snapshot = root.scan(LayoutLimits::default()).expect("scan root");
    assert!(snapshot.foreign_entries.is_empty());
}
