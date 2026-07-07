//! Parse and read per-process `/proc/PID/*` files.

use std::io;

use crate::ProcFs;

/// Parse error for process procfs content.
pub use crate::proc::stat::ParseError;

mod model;
mod parse;
mod sections;

pub use model::{
    ProcIo, ProcStat, ProcStatus, ProcessCgroupRow, ProcessError, ProcessFacts, ProcessHotRow,
    ProcessRead, ProcessStatusRow,
};
pub use parse::{parse_cgroup_path, parse_io, parse_stat, parse_status};
pub use sections::{to_hot_section, to_status_section};

use parse::{
    i8_from_i64, i16_from_i64, i32_from_i64, parse_btime, process_starttime_usec, rss_kb,
    u8_from_i64, u32_from_i64,
};

/// Read process conversion facts from the same procfs root as process rows.
///
/// # Errors
/// Returns an [`io::Error`] when `/proc/stat` cannot be read or `btime` is
/// absent/malformed.
pub fn process_facts(fs: &ProcFs) -> io::Result<ProcessFacts> {
    let stat = fs.read("stat")?;
    let btime_usec =
        parse_btime(&stat).ok_or_else(|| io::Error::other("stat: no parsable btime line"))?;
    Ok(ProcessFacts {
        btime_usec,
        clock_ticks_per_sec: i64::try_from(rustix::param::clock_ticks_per_second())
            .map_err(io::Error::other)?,
        page_size_bytes: i64::try_from(rustix::param::page_size()).map_err(io::Error::other)?,
    })
}

/// Read one process from `/proc/PID`.
///
/// # Errors
/// Returns [`ProcessError`] when a required file disappears, cannot be read, or
/// cannot be parsed. Optional `io`, `schedstat`, `cmdline`, `comm`, and cgroup
/// read failures are recorded in the returned row instead.
pub fn read_process(
    fs: &ProcFs,
    pid: i32,
    facts: ProcessFacts,
    ts: i64,
) -> Result<ProcessRead, ProcessError> {
    let stat_path = format!("{pid}/stat");
    let stat_content = read_required(fs, pid, &stat_path)?;
    let stat = parse_stat(&stat_content).map_err(|source| ProcessError::Parse {
        path: stat_path,
        source,
    })?;

    let status_path = format!("{pid}/status");
    let status_content = read_required(fs, pid, &status_path)?;
    let status = parse_status(&status_content).map_err(|source| ProcessError::Parse {
        path: status_path,
        source,
    })?;

    let io = read_io(fs, pid, status.uid, status.gid);
    let rundelay_ns = read_schedstat(fs, pid).unwrap_or(0);
    let cmdline = read_cmdline(fs, pid);
    let comm = read_comm(fs, pid).unwrap_or_else(|| stat.comm.clone());
    let starttime = process_starttime_usec(facts, stat.starttime_ticks);
    let cgroup = read_cgroup_path(fs, pid).map(|cgroup_path| ProcessCgroupRow {
        ts,
        pid,
        starttime,
        cgroup_path,
    });

    let hot = ProcessHotRow {
        ts,
        pid: stat.pid,
        starttime,
        ppid: stat.ppid,
        uid: status.uid,
        euid: status.euid,
        gid: status.gid,
        egid: status.egid,
        state: stat.state,
        num_threads: u32_from_i64(stat.num_threads),
        tty: u16::try_from(stat.tty_nr).unwrap_or(0),
        comm,
        cmdline,
        utime: stat.utime,
        stime: stat.stime,
        nice: i8_from_i64(stat.nice),
        prio: i16_from_i64(stat.priority),
        rtprio: i16_from_i64(stat.rt_priority),
        policy: u8_from_i64(stat.policy),
        curcpu: i32_from_i64(stat.processor),
        rundelay_ns,
        blkdelay_ticks: stat.delayacct_blkio_ticks,
        nvcsw: status.voluntary_ctxt_switches,
        nivcsw: status.nonvoluntary_ctxt_switches,
        minflt: stat.minflt,
        majflt: stat.majflt,
        vmem_kb: stat.vsize_bytes / 1024,
        rmem_kb: rss_kb(stat.rss_pages, facts.page_size_bytes),
        vswap_kb: status.vm_swap,
        io,
        exit_signal: i32_from_i64(stat.exit_signal),
    };
    let status = ProcessStatusRow {
        ts,
        pid: stat.pid,
        starttime,
        status,
    };
    Ok(ProcessRead {
        hot,
        status,
        cgroup,
    })
}

fn read_required(fs: &ProcFs, pid: i32, rel: &str) -> Result<String, ProcessError> {
    fs.read_raw(rel).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessError::Gone(pid)
        } else {
            ProcessError::Read {
                path: rel.to_owned(),
                source,
            }
        }
    })
}

fn read_io(fs: &ProcFs, pid: i32, uid: u32, gid: u32) -> Option<ProcIo> {
    let rel = format!("{pid}/io");
    match fs.read_raw(&rel) {
        Ok(content) => Some(parse_io(&content)),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            read_io_with_fs_creds(fs, &rel, uid, gid)
        }
        Err(_) => None,
    }
}

#[cfg(target_os = "linux")]
fn read_io_with_fs_creds(fs: &ProcFs, rel: &str, uid: u32, gid: u32) -> Option<ProcIo> {
    let _guard = FsCredGuard::switch(uid, gid);
    fs.read_raw(rel).ok().map(|content| parse_io(&content))
}

#[cfg(not(target_os = "linux"))]
fn read_io_with_fs_creds(_fs: &ProcFs, _rel: &str, _uid: u32, _gid: u32) -> Option<ProcIo> {
    None
}

#[cfg(target_os = "linux")]
struct FsCredGuard {
    uid: nix::unistd::Uid,
    gid: nix::unistd::Gid,
}

#[cfg(target_os = "linux")]
impl FsCredGuard {
    fn switch(uid: u32, gid: u32) -> Self {
        let saved_group = nix::unistd::setfsgid(nix::unistd::Gid::from_raw(gid));
        Self {
            uid: nix::unistd::setfsuid(nix::unistd::Uid::from_raw(uid)),
            gid: saved_group,
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for FsCredGuard {
    fn drop(&mut self) {
        nix::unistd::setfsuid(self.uid);
        nix::unistd::setfsgid(self.gid);
    }
}

fn read_schedstat(fs: &ProcFs, pid: i32) -> Option<i64> {
    let content = fs.read_raw(&format!("{pid}/schedstat")).ok()?;
    let mut fields = content.split_whitespace();
    let _run_time_ns = fields.next()?;
    fields.next()?.parse().ok()
}

fn read_cmdline(fs: &ProcFs, pid: i32) -> Option<String> {
    let content = fs.read_raw(&format!("{pid}/cmdline")).ok()?;
    let cmdline = content.replace('\0', " ").trim().to_owned();
    (!cmdline.is_empty()).then_some(cmdline)
}

fn read_comm(fs: &ProcFs, pid: i32) -> Option<String> {
    let comm = fs.read_raw(&format!("{pid}/comm")).ok()?;
    let comm = comm.trim().to_owned();
    (!comm.is_empty()).then_some(comm)
}

fn read_cgroup_path(fs: &ProcFs, pid: i32) -> Option<String> {
    let content = fs.read_raw(&format!("{pid}/cgroup")).ok()?;
    parse_cgroup_path(&content)
}
