use super::{OsSources, Result, SectionBuffers, buffer_row};

/// Buffer every collected OS section into the snapshot window.
///
/// Rows are pre-built with their string ids already interned, so this only
/// moves them into the buffers.
///
/// # Errors
/// Returns an error if a section buffer is full.
pub(crate) fn push_os_sources(buffers: &mut SectionBuffers, os: &OsSources) -> Result<()> {
    for row in &os.cpu {
        buffer_row(buffers, *row)?;
    }
    if let Some(row) = os.stat {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.meminfo {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.loadavg {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.vmstat {
        buffer_row(buffers, row)?;
    }
    for row in &os.psi {
        buffer_row(buffers, *row)?;
    }
    for row in &os.diskstats {
        buffer_row(buffers, *row)?;
    }
    for row in &os.netdev {
        buffer_row(buffers, *row)?;
    }
    if let Some(row) = os.snmp {
        buffer_row(buffers, row)?;
    }
    if let Some(row) = os.netstat {
        buffer_row(buffers, row)?;
    }
    for row in &os.mountinfo {
        buffer_row(buffers, *row)?;
    }
    for row in &os.topology {
        buffer_row(buffers, *row)?;
    }
    for row in &os.processes {
        buffer_row(buffers, *row)?;
    }
    for row in &os.process_status {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_mapping {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_cpu {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_memory {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_io {
        buffer_row(buffers, *row)?;
    }
    for row in &os.cgroup_pids {
        buffer_row(buffers, *row)?;
    }
    for row in &os.pg_storage_mounts {
        buffer_row(buffers, *row)?;
    }
    if let Some(row) = os.pg_process_cgroup_memory {
        buffer_row(buffers, row)?;
    }
    for row in &os.snapshot_coverage {
        buffer_row(buffers, *row)?;
    }
    Ok(())
}
