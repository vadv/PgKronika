use crate::config::{Config, validate_replication_detail_bounds};
use crate::coverage::snapshot_coverage;
use crate::logging::{
    CollectionFamily, LogLevel, duration_ms, field, layout_id, log_collection_failure,
    log_collection_finish, log_collection_start, log_event, section_name,
};
use crate::scheduler::{DueSet, SourceKind};
use anyhow::{Context, Result};
use kronika_registry::Ts;
use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_source_pg::archiver::{ArchiverRow, collect_archiver};
use kronika_source_pg::database::{DatabaseRow, DatabaseVersion, collect_database};
use kronika_source_pg::incident_gauges::{
    LocalJoinFacts, ReplicationPhysicalRow, SlotRetentionRow, SlotRetentionVersion,
    VacuumObservationRow, collect_local_join_facts, collect_replication_physical,
    collect_slot_retention, collect_vacuum_observations,
};
use kronika_source_pg::io::{IoRow, IoVersion, collect_io};
use kronika_source_pg::locks::{LocksRow, LocksVersion, collect_locks, locks_version};
use kronika_source_pg::prepared_xacts::{PreparedXactsRow, collect_prepared_xacts};
use kronika_source_pg::progress_vacuum::{ProgressVacuumRow, collect_progress_vacuum};
use kronika_source_pg::replication_details::{
    ReplicaRow, SlotRow, collect_replication_detail_bounds, collect_replication_replicas,
    collect_replication_slots,
};
use kronika_source_pg::replication_instance::{
    ReplicationInstanceRow, collect_replication_instance,
};
use kronika_source_pg::wal::{WalSnapshot, collect_wal};
use kronika_source_pg::{
    ActivityRow, ActivityVersion, activity_version, collect_activity, collect_bgwriter_checkpointer,
};
use std::time::Instant;
use tokio_postgres::Client;

/// The `1_001` layout collected on this server major.
const fn activity_type_id(version: ActivityVersion) -> u32 {
    match version {
        ActivityVersion::V1 => 1_001_001,
        ActivityVersion::V2 => 1_001_002,
        ActivityVersion::V3 => 1_001_003,
    }
}

/// The `1_005` layout collected on this server major.
const fn database_type_id(version: DatabaseVersion) -> u32 {
    match version {
        DatabaseVersion::V1 => 1_005_001,
        DatabaseVersion::V2 => 1_005_002,
        DatabaseVersion::V3 => 1_005_003,
        DatabaseVersion::V4 => 1_005_004,
    }
}

/// The `1_007` layout returned by `pg_stat_wal`.
const fn wal_type_id(wal: &WalSnapshot) -> u32 {
    match wal {
        WalSnapshot::V1(_) => 1_007_001,
        WalSnapshot::V2(_) => 1_007_002,
    }
}

/// The `1_009` layout collected on this server major.
const fn io_type_id(version: IoVersion) -> u32 {
    match version {
        IoVersion::V1 => 1_009_001,
        IoVersion::V2 => 1_009_002,
    }
}

/// The `1_011` layout collected on this server major.
const fn locks_type_id(version: LocksVersion) -> u32 {
    match version {
        LocksVersion::V1 => 1_011_001,
        LocksVersion::V2 => 1_011_002,
    }
}

/// Everything one tick reads from the main connection, gated by `due`.
type ReplicationSources = (
    ReplicationInstanceRow,
    Vec<ReplicaRow>,
    Vec<SlotRow>,
    Vec<ReplicationPhysicalRow>,
    SlotRetentionVersion,
    Vec<SlotRetentionRow>,
);

pub(crate) struct MainConnSources {
    pub(crate) ts: Ts,
    pub(crate) local_join: Option<LocalJoinFacts>,
    pub(crate) bgwriter: Option<BgwriterCheckpointer>,
    pub(crate) activity: Option<(ActivityVersion, Vec<ActivityRow>)>,
    pub(crate) activity_coverage: Option<SnapshotCoverageV1>,
    pub(crate) database: Option<(DatabaseVersion, Vec<DatabaseRow>)>,
    pub(crate) progress_vacuum_rows: Vec<ProgressVacuumRow>,
    pub(crate) vacuum_observation_rows: Vec<VacuumObservationRow>,
    pub(crate) prepared_rows: Vec<PreparedXactsRow>,
    pub(crate) wal: Option<WalSnapshot>,
    pub(crate) io: Option<(IoVersion, Vec<IoRow>)>,
    pub(crate) archiver: Option<ArchiverRow>,
    pub(crate) replication: Option<ReplicationSources>,
    pub(crate) lock_rows: Vec<LocksRow>,
}

/// Read the due main-connection sources.
#[allow(
    clippy::too_many_lines,
    reason = "main-connection reads share one snapshot timestamp and failure policy"
)]
pub(crate) async fn collect_main_conn_sources(
    client: &Client,
    major: u32,
    config: &Config,
    due: &DueSet,
) -> Result<MainConnSources> {
    let ts = kronika_source_pg::snapshot_ts(client)
        .await
        .context("read the snapshot timestamp")?;
    let local_join = if due.has(SourceKind::OsMountTopo) || due.has(SourceKind::OsCgroup) {
        match collect_local_join_facts(client, 256).await {
            Ok(facts) => facts,
            Err(err) => {
                log_event(
                    LogLevel::Warn,
                    "collection_skip",
                    &[
                        field("collection", "cross_source_join"),
                        field("source", "main"),
                        field("reason", "local_join_facts_unavailable"),
                        field("error", &err),
                    ],
                );
                None
            }
        }
    } else {
        None
    };
    let bgwriter = if due.has(SourceKind::Bgwriter) {
        let type_id = 1_006_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_bgwriter_checkpointer(client, major).await {
            Ok(row) => {
                log_collection_finish(type_id, "main", 1, started.elapsed());
                Some(row)
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_stat_bgwriter + pg_stat_checkpointer");
            }
        }
    } else {
        None
    };
    let (activity, activity_coverage) = if due.has(SourceKind::Activity) {
        let started = Instant::now();
        let source = "main";
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Activity.field(), field("source", source)],
        );
        match collect_activity(client, major).await {
            Ok(read) => {
                let type_id = activity_type_id(read.version);
                let marker_ts = read.rows.first().map_or(ts.0, |row| row.ts);
                if read.truncated {
                    log_event(
                        LogLevel::Warn,
                        "collection_skip",
                        &[
                            CollectionFamily::Activity.field(),
                            field("source", source),
                            field("reason", "source_row_limit"),
                            field("limit", kronika_source_pg::MAX_ACTIVITY_ROWS),
                        ],
                    );
                    (
                        None,
                        Some(snapshot_coverage(
                            marker_ts,
                            type_id,
                            1,
                            u8::from(!read.full_visibility),
                            read.source_rows,
                            0,
                        )),
                    )
                } else {
                    log_collection_finish(type_id, source, read.rows.len(), started.elapsed());
                    let collected = read.rows.len();
                    (
                        Some((read.version, read.rows)),
                        Some(snapshot_coverage(
                            marker_ts,
                            type_id,
                            0,
                            u8::from(!read.full_visibility),
                            read.source_rows,
                            collected,
                        )),
                    )
                }
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Activity.field(),
                        field("source", source),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                let read_state = if err.code().is_some_and(|code| code.code() == "42501") {
                    2
                } else {
                    3
                };
                (
                    None,
                    Some(snapshot_coverage(
                        ts.0,
                        activity_type_id(activity_version(major)),
                        read_state,
                        2,
                        0,
                        0,
                    )),
                )
            }
        }
    } else {
        (None, None)
    };
    let database = if due.has(SourceKind::Database) {
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Database.field(), field("source", "main")],
        );
        match collect_database(client, major).await {
            Ok((version, rows)) => {
                let type_id = database_type_id(version);
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                Some((version, rows))
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Database.field(),
                        field("source", "main"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_database");
            }
        }
    } else {
        None
    };
    let progress_vacuum_rows = if due.has(SourceKind::ProgressVacuum) {
        let type_id = 1_012_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_progress_vacuum(client, major).await {
            Ok(rows) => {
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                rows
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_stat_progress_vacuum");
            }
        }
    } else {
        Vec::new()
    };
    let vacuum_observation_rows = if due.has(SourceKind::ProgressVacuum) {
        let type_id = 1_032_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_vacuum_observations(client).await {
            Ok(rows) => {
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                rows
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect running vacuum observations");
            }
        }
    } else {
        Vec::new()
    };
    let prepared_rows = if due.has(SourceKind::PreparedXacts) {
        let type_id = 1_010_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_prepared_xacts(client).await {
            Ok(rows) => {
                log_collection_finish(type_id, "main", rows.len(), started.elapsed());
                rows
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_prepared_xacts");
            }
        }
    } else {
        Vec::new()
    };
    let wal = if due.has(SourceKind::Wal) {
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Wal.field(), field("source", "main")],
        );
        match collect_wal(client, major).await {
            Ok(Some(wal)) => {
                log_collection_finish(wal_type_id(&wal), "main", 1, started.elapsed());
                Some(wal)
            }
            Ok(None) => {
                log_event(
                    LogLevel::Debug,
                    "collection_skip",
                    &[
                        CollectionFamily::Wal.field(),
                        field("source", "main"),
                        field("server_major", major),
                        field("reason", "unsupported_server_major"),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                None
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Wal.field(),
                        field("source", "main"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_wal");
            }
        }
    } else {
        None
    };
    let io = if due.has(SourceKind::Io) {
        let started = Instant::now();
        log_event(
            LogLevel::Debug,
            "collection_start",
            &[CollectionFamily::Io.field(), field("source", "main")],
        );
        match collect_io(client, major).await {
            Ok(Some((version, rows))) => {
                log_collection_finish(io_type_id(version), "main", rows.len(), started.elapsed());
                Some((version, rows))
            }
            Ok(None) => {
                log_event(
                    LogLevel::Debug,
                    "collection_skip",
                    &[
                        CollectionFamily::Io.field(),
                        field("source", "main"),
                        field("server_major", major),
                        field("reason", "unsupported_server_major"),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                None
            }
            Err(err) => {
                log_event(
                    LogLevel::Error,
                    "collection_failure",
                    &[
                        CollectionFamily::Io.field(),
                        field("source", "main"),
                        field("error", &err),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                return Err(err).context("collect pg_stat_io");
            }
        }
    } else {
        None
    };
    let archiver = if due.has(SourceKind::Archiver) {
        let type_id = 1_008_001;
        let started = Instant::now();
        log_collection_start(type_id, "main");
        match collect_archiver(client).await {
            Ok(row) => {
                log_collection_finish(type_id, "main", 1, started.elapsed());
                Some(row)
            }
            Err(err) => {
                log_collection_failure(type_id, "main", &err, started.elapsed());
                return Err(err).context("collect pg_stat_archiver");
            }
        }
    } else {
        None
    };
    let replication = if due.has(SourceKind::Replication) {
        let started = Instant::now();
        log_collection_start(1_015_001, "main");
        let instance_row = match collect_replication_instance(client, major).await {
            Ok(row) => {
                log_collection_finish(1_015_001, "main", 1, started.elapsed());
                row
            }
            Err(err) => {
                log_collection_failure(1_015_001, "main", &err, started.elapsed());
                return Err(err).context("collect replication instance status");
            }
        };
        log_collection_start(1_016_001, "main");
        log_collection_start(1_017_001, "main");
        let details_started = Instant::now();
        let (replica_rows, slot_rows) = collect_replication_details(client, major).await?;
        let physical_rows = collect_replication_physical(client)
            .await
            .context("collect typed physical replication gaps")?;
        let (slot_retention_version, slot_retention_rows) = collect_slot_retention(client, major)
            .await
            .context("collect typed replication slot retention")?;
        log_collection_finish(
            1_016_001,
            "main",
            replica_rows.len(),
            details_started.elapsed(),
        );
        log_collection_finish(
            1_017_001,
            "main",
            slot_rows.len(),
            details_started.elapsed(),
        );
        log_collection_finish(
            1_033_001,
            "main",
            physical_rows.len(),
            details_started.elapsed(),
        );
        let retention_type_id = match slot_retention_version {
            SlotRetentionVersion::Pg15 => 1_034_001,
            SlotRetentionVersion::Pg16 => 1_034_002,
            SlotRetentionVersion::Pg17Plus => 1_034_003,
        };
        log_collection_finish(
            retention_type_id,
            "main",
            slot_retention_rows.len(),
            details_started.elapsed(),
        );
        Some((
            instance_row,
            replica_rows,
            slot_rows,
            physical_rows,
            slot_retention_version,
            slot_retention_rows,
        ))
    } else {
        None
    };
    // The lock-wait graph has no interval of its own: the freshest activity
    // snapshot already says whether any backend waits on a heavyweight lock.
    let lock_rows = match &activity {
        Some((_, rows)) if activity_has_lock_waiters(rows) => {
            collect_lock_rows(client, major, config.max_lock_rows).await
        }
        _ => {
            let type_id = locks_type_id(locks_version(major));
            log_event(
                LogLevel::Debug,
                "collection_skip",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "main"),
                    field("reason", "no_lock_waiters"),
                ],
            );
            Vec::new()
        }
    };
    Ok(MainConnSources {
        ts,
        local_join,
        bgwriter,
        activity,
        activity_coverage,
        database,
        progress_vacuum_rows,
        vacuum_observation_rows,
        prepared_rows,
        wal,
        io,
        archiver,
        replication,
        lock_rows,
    })
}

/// Whether the activity snapshot shows a heavyweight lock waiter.
fn activity_has_lock_waiters(rows: &[ActivityRow]) -> bool {
    rows.iter()
        .any(|row| row.wait_event_type.as_deref() == Some("Lock"))
}

/// Whether the activity snapshot justifies the accelerated pace: a backend
/// waits on a heavyweight lock, or active client backends reach `threshold`.
pub(crate) fn activity_needs_acceleration(rows: &[ActivityRow], threshold: usize) -> bool {
    if activity_has_lock_waiters(rows) {
        return true;
    }
    let active_clients = rows
        .iter()
        .filter(|row| {
            row.backend_type == "client backend" && row.state.as_deref() == Some("active")
        })
        .count();
    active_clients >= threshold
}

/// Whether the replication snapshot justifies the accelerated pace: a replica
/// or this standby replays behind `lag_trigger_s`, or a slot retains at least
/// `retained_trigger_bytes` of WAL.
pub(crate) fn replication_needs_acceleration(
    instance: &ReplicationInstanceRow,
    replicas: &[ReplicaRow],
    slots: &[SlotRow],
    lag_trigger_s: i64,
    retained_trigger_bytes: i64,
) -> bool {
    if instance
        .replay_lag_s
        .is_some_and(|lag| lag >= lag_trigger_s)
    {
        return true;
    }
    let replica_lag_floor_us = lag_trigger_s.saturating_mul(1_000_000);
    if replicas.iter().any(|replica| {
        replica
            .replay_lag_us
            .is_some_and(|lag| lag >= replica_lag_floor_us)
    }) {
        return true;
    }
    slots.iter().any(|slot| {
        slot.retained_bytes
            .is_some_and(|bytes| bytes >= retained_trigger_bytes)
    })
}

/// Collect lock-wait rows or degrade by skipping only section `1_011`.
async fn collect_lock_rows(client: &Client, major: u32, max_lock_rows: i64) -> Vec<LocksRow> {
    let type_id = locks_type_id(locks_version(major));
    let started = Instant::now();
    log_collection_start(type_id, "main");
    match collect_locks(client, major, max_lock_rows).await {
        Ok(snapshot) => {
            if let Some(skipped) = snapshot.skipped {
                log_event(
                    LogLevel::Warn,
                    "collection_skip",
                    &[
                        field("collection", section_name(type_id)),
                        field("type_id", type_id),
                        field("layout_id", layout_id(type_id)),
                        field("source", "main"),
                        field("reason", "lock_graph_too_large"),
                        field("max_rows", skipped.max_rows),
                        field("waiters", skipped.waiters),
                        field("edges", skipped.edges),
                        field("nodes", skipped.nodes),
                        field("elapsed_ms", duration_ms(started.elapsed())),
                    ],
                );
                Vec::new()
            } else {
                log_collection_finish(type_id, "main", snapshot.rows.len(), started.elapsed());
                snapshot.rows
            }
        }
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_skip",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "main"),
                    field("reason", "query_failed"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            Vec::new()
        }
    }
}

/// Collect the walsender and slot detail rows from the main connection.
async fn collect_replication_details(
    client: &Client,
    major: u32,
) -> Result<(Vec<ReplicaRow>, Vec<SlotRow>)> {
    let started = Instant::now();
    let bounds = match collect_replication_detail_bounds(client).await {
        Ok(bounds) => bounds,
        Err(err) => {
            log_event(
                LogLevel::Error,
                "collection_failure",
                &[
                    CollectionFamily::ReplicationDetails.field(),
                    field("source", "main"),
                    field("reason", "bounds_query_failed"),
                    field("error", &err),
                    field("elapsed_ms", duration_ms(started.elapsed())),
                ],
            );
            return Err(err).context("collect replication detail row bounds");
        }
    };
    if let Err(err) = validate_replication_detail_bounds(bounds) {
        log_event(
            LogLevel::Error,
            "collection_failure",
            &[
                CollectionFamily::ReplicationDetails.field(),
                field("source", "main"),
                field("reason", "bounds_validation_failed"),
                field("error", format!("{err:#}")),
                field("elapsed_ms", duration_ms(started.elapsed())),
            ],
        );
        return Err(err);
    }
    let replicas = match collect_replication_replicas(client).await {
        Ok(rows) => rows,
        Err(err) => {
            log_collection_failure(1_016_001, "main", &err, started.elapsed());
            return Err(err).context("collect pg_stat_replication");
        }
    };
    let slots = match collect_replication_slots(client, major).await {
        Ok(rows) => rows,
        Err(err) => {
            log_collection_failure(1_017_001, "main", &err, started.elapsed());
            return Err(err).context("collect pg_replication_slots");
        }
    };
    Ok((replicas, slots))
}
