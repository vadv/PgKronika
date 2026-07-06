use crate::config::validate_settings_row_count;
use crate::logging::{layout_id, section_name};
use crate::main_sources::MainConnSources;
use crate::plans_source::PlansRead;
use crate::service_sections::{InstanceFacts, ServiceSections};
use anyhow::Result;
use kronika_registry::instance_metadata::InstanceMetadata;
use kronika_registry::{StrId, Ts};
use kronika_source_pg::archiver::{ArchiverRow, to_archiver};
use kronika_source_pg::database::{self, DatabaseRow, DatabaseVersion};
use kronika_source_pg::io::{self, IoRow, IoVersion};
use kronika_source_pg::locks::{
    LocksRow, LocksVersion, locks_version, to_v1 as locks_to_v1, to_v2 as locks_to_v2,
};
use kronika_source_pg::prepared_xacts::{PreparedXactsRow, to_prepared_xacts};
use kronika_source_pg::progress_vacuum::{ProgressVacuumRow, to_progress_vacuum};
use kronika_source_pg::replication_details::{ReplicaRow, SlotRow, to_replicas_v1, to_slots_v1};
use kronika_source_pg::replication_instance::{ReplicationInstanceRow, to_replication_instance};
use kronika_source_pg::reset_metadata::{ResetBase, ResetExtensions, to_reset_metadata};
use kronika_source_pg::settings::{SettingsRow, to_settings_v1};
use kronika_source_pg::statements::{self, StatementsRow, StatementsVersion};
use kronika_source_pg::store_plans::{self, StorePlansOsscRow, StorePlansRow};
use kronika_source_pg::user_indexes::{self, UserIndexesRow, UserIndexesVersion};
use kronika_source_pg::user_tables::{self, UserTablesRow, UserTablesVersion};
use kronika_source_pg::wal::WalSnapshot;
use kronika_source_pg::{ActivityRow, ActivityVersion, to_v1, to_v2, to_v3};
use kronika_writer::{Interner, SectionBuffers};

/// Buffer the main-connection sections that were read this tick.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_main_conn_sections(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    major: u32,
    src: &MainConnSources,
) -> Result<()> {
    if let Some(bgwriter) = src.bgwriter {
        buffer_row(buffers, bgwriter)?;
    }
    if let Some((activity_version, activity_rows)) = &src.activity {
        push_activity(buffers, interner, *activity_version, activity_rows)?;
    }
    if let Some((database_version, database_rows)) = &src.database {
        push_database(buffers, interner, *database_version, database_rows)?;
    }
    push_progress_vacuum(buffers, interner, &src.progress_vacuum_rows)?;
    push_prepared_xacts(buffers, interner, &src.prepared_rows)?;
    push_wal(buffers, src.wal)?;
    if let Some((io_version, io_rows)) = &src.io {
        push_io(buffers, interner, *io_version, io_rows)?;
    }
    if let Some(archiver) = &src.archiver {
        push_archiver(buffers, interner, archiver)?;
    }
    if let Some((instance_row, replica_rows, slot_rows)) = &src.replication {
        push_replication_instance(buffers, interner, instance_row)?;
        push_replication_details(buffers, interner, replica_rows, slot_rows)?;
    }
    if !src.lock_rows.is_empty() {
        push_locks(buffers, interner, locks_version(major), &src.lock_rows)?;
    }
    Ok(())
}

/// Buffer the `pg_stat_wal` singleton; PG10-13 produce no row.
///
/// # Errors
/// Returns an error when the section buffer is full.
pub(crate) fn push_wal(buffers: &mut SectionBuffers, wal: Option<WalSnapshot>) -> Result<()> {
    match wal {
        Some(WalSnapshot::V1(row)) => buffer_row(buffers, row),
        Some(WalSnapshot::V2(row)) => buffer_row(buffers, row),
        None => Ok(()),
    }
}

/// Buffer the paced `pg_store_plans` read under its fork's section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or the
/// section buffer is full.
pub(crate) fn push_plans_read(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    read: Option<&PlansRead>,
) -> Result<()> {
    match read {
        Some(PlansRead::Vadv(rows)) => push_store_plans(buffers, interner, rows),
        Some(PlansRead::Ossc(rows)) => push_store_plans_ossc(buffers, interner, rows),
        None => Ok(()),
    }
}
/// Buffer the service sections collected for this tick.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_service_sections(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    service: &ServiceSections,
) -> Result<()> {
    if let Some((reset_base, reset_ext)) = &service.reset {
        push_reset_metadata(buffers, interner, reset_base, reset_ext)?;
    }
    if let Some(instance) = &service.instance {
        push_instance_metadata(buffers, interner, instance)?;
    }
    push_settings(buffers, interner, &service.settings)
}
/// Intern each row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_activity(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: ActivityVersion,
    rows: &[ActivityRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            ActivityVersion::V1 => buffer_row(buffers, to_v1(row, &mut intern)?)?,
            ActivityVersion::V2 => buffer_row(buffers, to_v2(row, &mut intern)?)?,
            ActivityVersion::V3 => buffer_row(buffers, to_v3(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each row's `datname` and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if `datname` cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_database(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: DatabaseVersion,
    rows: &[DatabaseRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            DatabaseVersion::V1 => buffer_row(buffers, database::to_v1(row, &mut intern)?)?,
            DatabaseVersion::V2 => buffer_row(buffers, database::to_v2(row, &mut intern)?)?,
            DatabaseVersion::V3 => buffer_row(buffers, database::to_v3(row, &mut intern)?)?,
            DatabaseVersion::V4 => buffer_row(buffers, database::to_v4(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each table row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_user_tables(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    collected: &[(String, UserTablesVersion, Vec<UserTablesRow>)],
) -> Result<()> {
    for (datname, version, rows) in collected {
        for row in rows {
            let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
            match version {
                UserTablesVersion::V1 => {
                    buffer_row(buffers, user_tables::to_v1(row, datname, &mut intern)?)?;
                }
                UserTablesVersion::V2 => {
                    buffer_row(buffers, user_tables::to_v2(row, datname, &mut intern)?)?;
                }
                UserTablesVersion::V3 => {
                    buffer_row(buffers, user_tables::to_v3(row, datname, &mut intern)?)?;
                }
                UserTablesVersion::V4 => {
                    buffer_row(buffers, user_tables::to_v4(row, datname, &mut intern)?)?;
                }
            }
        }
    }
    Ok(())
}

/// Intern each index row's strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_user_indexes(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    collected: &[(String, UserIndexesVersion, Vec<UserIndexesRow>)],
) -> Result<()> {
    for (datname, version, rows) in collected {
        for row in rows {
            let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
            match version {
                UserIndexesVersion::V1 => {
                    buffer_row(buffers, user_indexes::to_v1(row, datname, &mut intern)?)?;
                }
                UserIndexesVersion::V2 => {
                    buffer_row(buffers, user_indexes::to_v2(row, datname, &mut intern)?)?;
                }
            }
        }
    }
    Ok(())
}

/// Intern each statement row's strings and buffer it as the version's section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_statements(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: StatementsVersion,
    rows: &[StatementsRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            StatementsVersion::V1 => buffer_row(buffers, statements::to_v1(row, &mut intern)?)?,
            StatementsVersion::V2 => buffer_row(buffers, statements::to_v2(row, &mut intern)?)?,
            StatementsVersion::V3 => buffer_row(buffers, statements::to_v3(row, &mut intern)?)?,
            StatementsVersion::V4 => buffer_row(buffers, statements::to_v4(row, &mut intern)?)?,
            StatementsVersion::V5 => buffer_row(buffers, statements::to_v5(row, &mut intern)?)?,
            StatementsVersion::V6 => buffer_row(buffers, statements::to_v6(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern the two settings strings and buffer the instance replication row.
///
/// # Errors
/// Returns an error if a setting cannot be interned (dictionary full) or the
/// section buffer is full.
pub(crate) fn push_replication_instance(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    row: &ReplicationInstanceRow,
) -> Result<()> {
    let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    buffer_row(buffers, to_replication_instance(row, &mut intern)?)
}

/// Intern each row's labels and buffer it as the progress-vacuum section.
///
/// # Errors
/// Returns an error if a label cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_progress_vacuum(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[ProgressVacuumRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_progress_vacuum(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern each row's `datname` and buffer it as the prepared-xacts section.
///
/// # Errors
/// Returns an error if `datname` cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_prepared_xacts(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[PreparedXactsRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_prepared_xacts(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern WAL file names and buffer the singleton `pg_stat_archiver` row.
///
/// # Errors
/// Returns an error if a WAL name cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_archiver(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    row: &ArchiverRow,
) -> Result<()> {
    let intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    buffer_row(buffers, to_archiver(row, intern)?)
}

/// Intern each row's label strings and buffer it as the version's section type.
///
/// # Errors
/// Returns an error if a label cannot be interned (dictionary full) or a section
/// buffer is full.
pub(crate) fn push_io(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: IoVersion,
    rows: &[IoRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            IoVersion::V1 => buffer_row(buffers, io::to_v1(row, &mut intern)?)?,
            IoVersion::V2 => buffer_row(buffers, io::to_v2(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern each row's strings and buffer it as the version's lock-wait section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_locks(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    version: LocksVersion,
    rows: &[LocksRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        match version {
            LocksVersion::V1 => buffer_row(buffers, locks_to_v1(row, &mut intern)?)?,
            LocksVersion::V2 => buffer_row(buffers, locks_to_v2(row, &mut intern)?)?,
        }
    }
    Ok(())
}

/// Intern the label strings and buffer the singleton `reset_metadata` row.
///
/// # Errors
/// Returns an error if a label cannot be interned (dictionary full) or the
/// section buffer is full.
pub(crate) fn push_reset_metadata(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    base: &ResetBase,
    ext: &ResetExtensions,
) -> Result<()> {
    let intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    buffer_row(buffers, to_reset_metadata(base, ext, intern)?)
}

/// Intern the identity strings and buffer the singleton `instance_metadata`
/// row.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or the
/// section buffer is full.
pub(crate) fn push_instance_metadata(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    facts: &InstanceFacts,
) -> Result<()> {
    let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
    let row = InstanceMetadata {
        ts: Ts(facts.pg.ts),
        hostname: intern(facts.os.hostname.as_bytes())?,
        node_self_id: intern(facts.node_self_id.as_bytes())?,
        pg_version_num: facts.pg.pg_version_num,
        kernel_version: intern(facts.os.kernel_version.as_bytes())?,
        pg_system_identifier: facts.system_identifier,
        clock_ticks_per_sec: facts.os.clock_ticks_per_sec,
        page_size_bytes: facts.os.page_size_bytes,
        boot_id: intern(facts.os.boot_id.as_bytes())?,
        btime: Ts(facts.os.btime),
    };
    buffer_row(buffers, row)
}

/// Intern and buffer the `pg_stat_replication` and `pg_replication_slots`
/// rows.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_replication_details(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    replicas: &[ReplicaRow],
    slots: &[SlotRow],
) -> Result<()> {
    for row in replicas {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_replicas_v1(row, &mut intern)?)?;
    }
    for row in slots {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_slots_v1(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern each row's strings and buffer it as the `pg_settings` section.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or the
/// section buffer is full.
pub(crate) fn push_settings(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[SettingsRow],
) -> Result<()> {
    validate_settings_row_count(rows.len())?;
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, to_settings_v1(row, &mut intern)?)?;
    }
    Ok(())
}
/// Buffer one typed snapshot row, mapping a full buffer to an error.
pub(crate) fn buffer_row<S: kronika_registry::Section + 'static>(
    buffers: &mut SectionBuffers,
    row: S,
) -> Result<()> {
    let type_id = S::CONTRACT.type_id.get();
    buffers.push(row).map_err(|_row| {
        anyhow::anyhow!(
            "section buffer is full: collection={} type_id={} layout_id={}",
            section_name(type_id),
            type_id,
            layout_id(type_id)
        )
    })
}
/// Intern each plan row's strings and buffer it as section `1_004_001`.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_store_plans(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[StorePlansRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, store_plans::to_vadv_v1(row, &mut intern)?)?;
    }
    Ok(())
}

/// Intern each ossc plan row's strings and buffer it as section `1_003_001`.
///
/// # Errors
/// Returns an error if a string cannot be interned (dictionary full) or a
/// section buffer is full.
pub(crate) fn push_store_plans_ossc(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    rows: &[StorePlansOsscRow],
) -> Result<()> {
    for row in rows {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        buffer_row(buffers, store_plans::to_ossc_v1(row, &mut intern)?)?;
    }
    Ok(())
}
