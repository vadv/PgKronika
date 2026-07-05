//! Parse and read per-process `/proc/PID/*` files.

use std::fmt;
use std::io;

use kronika_registry::os_process::OsProcess;
use kronika_registry::os_process_status::OsProcessStatus;
use kronika_registry::{StrId, Ts};

use crate::ProcFs;

/// Parse error for process procfs content.
pub use crate::proc::stat::ParseError;

/// Linux time and page-size facts needed to convert process identity fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessFacts {
    /// Kernel boot time, unix microseconds.
    pub btime_usec: i64,
    /// Kernel scheduler ticks per second.
    pub clock_ticks_per_sec: i64,
    /// Kernel page size in bytes.
    pub page_size_bytes: i64,
}

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

/// Parsed data from `/proc/PID/stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStat {
    /// PID from field 1.
    pub pid: i32,
    /// Process name from field 2, without surrounding parentheses.
    pub comm: String,
    /// Process state byte from field 3.
    pub state: u8,
    /// Parent PID.
    pub ppid: i32,
    /// Raw controlling terminal number.
    pub tty_nr: i32,
    /// Minor page faults.
    pub minflt: i64,
    /// Major page faults.
    pub majflt: i64,
    /// User CPU time, ticks.
    pub utime: i64,
    /// System CPU time, ticks.
    pub stime: i64,
    /// Scheduler priority.
    pub priority: i64,
    /// Nice value.
    pub nice: i64,
    /// Thread count.
    pub num_threads: i64,
    /// Process start time since boot, ticks.
    pub starttime_ticks: i64,
    /// Virtual memory size, bytes.
    pub vsize_bytes: i64,
    /// Resident set size, pages.
    pub rss_pages: i64,
    /// Exit signal.
    pub exit_signal: i64,
    /// Last/current CPU.
    pub processor: i64,
    /// Real-time priority.
    pub rt_priority: i64,
    /// Scheduler policy.
    pub policy: i64,
    /// Block I/O delay, ticks.
    pub delayacct_blkio_ticks: i64,
}

/// Parsed data from `/proc/PID/status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcStatus {
    /// Real UID.
    pub uid: u32,
    /// Effective UID.
    pub euid: u32,
    /// Real GID.
    pub gid: u32,
    /// Effective GID.
    pub egid: u32,
    /// `VmData`, kB.
    pub vm_data: i64,
    /// `VmStk`, kB.
    pub vm_stk: i64,
    /// `VmLib`, kB.
    pub vm_lib: i64,
    /// `VmSwap`, kB.
    pub vm_swap: i64,
    /// `VmLck`, kB.
    pub vm_lck: i64,
    /// `VmPTE`, kB.
    pub vm_pte: i64,
    /// `VmPeak`, kB.
    pub vm_peak: i64,
    /// `VmHWM`, kB.
    pub vm_hwm: i64,
    /// `Threads`.
    pub threads: u32,
    /// `FDSize`.
    pub fdsize: u32,
    /// Voluntary context switches.
    pub voluntary_ctxt_switches: i64,
    /// Nonvoluntary context switches.
    pub nonvoluntary_ctxt_switches: i64,
}

/// Parsed data from `/proc/PID/io`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcIo {
    /// Characters read, including page-cache hits.
    pub rchar: i64,
    /// Characters written, including page-cache writes.
    pub wchar: i64,
    /// Read syscalls.
    pub syscr: i64,
    /// Write syscalls.
    pub syscw: i64,
    /// Bytes really read from storage.
    pub read_bytes: i64,
    /// Bytes really written to storage.
    pub write_bytes: i64,
    /// Cancelled write bytes.
    pub cancelled_write_bytes: i64,
}

/// Process hot row before string interning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessHotRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Process ID.
    pub pid: i32,
    /// Process start timestamp, unix microseconds.
    pub starttime: i64,
    /// Parent PID.
    pub ppid: i32,
    /// Real UID.
    pub uid: u32,
    /// Effective UID.
    pub euid: u32,
    /// Real GID.
    pub gid: u32,
    /// Effective GID.
    pub egid: u32,
    /// Process state as ASCII byte.
    pub state: u8,
    /// Number of threads.
    pub num_threads: u32,
    /// Controlling terminal.
    pub tty: u16,
    /// Process name.
    pub comm: String,
    /// Command line with NUL separators converted to spaces.
    pub cmdline: Option<String>,
    /// User CPU time, ticks.
    pub utime: i64,
    /// System CPU time, ticks.
    pub stime: i64,
    /// Nice value.
    pub nice: i8,
    /// Scheduler priority.
    pub prio: i16,
    /// Real-time priority.
    pub rtprio: i16,
    /// Scheduler policy.
    pub policy: u8,
    /// Last/current CPU.
    pub curcpu: i32,
    /// Run-queue delay, ns.
    pub rundelay_ns: i64,
    /// Block I/O delay, ticks.
    pub blkdelay_ticks: i64,
    /// Voluntary context switches.
    pub nvcsw: i64,
    /// Nonvoluntary context switches.
    pub nivcsw: i64,
    /// Minor page faults.
    pub minflt: i64,
    /// Major page faults.
    pub majflt: i64,
    /// Virtual memory, kB.
    pub vmem_kb: i64,
    /// Resident memory, kB.
    pub rmem_kb: i64,
    /// Swap, kB.
    pub vswap_kb: i64,
    /// Optional process I/O counters.
    pub io: Option<ProcIo>,
    /// Exit signal.
    pub exit_signal: i32,
}

/// Extended `/proc/PID/status` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStatusRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Process ID.
    pub pid: i32,
    /// Process start timestamp, unix microseconds.
    pub starttime: i64,
    /// Parsed `/proc/PID/status` fields.
    pub status: ProcStatus,
}

/// PID to cgroup mapping row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCgroupRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Process ID.
    pub pid: i32,
    /// Process start timestamp, unix microseconds.
    pub starttime: i64,
    /// Normalized cgroup path.
    pub cgroup_path: String,
}

/// Result of reading one PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRead {
    /// Hot process row.
    pub hot: ProcessHotRow,
    /// Extended status row.
    pub status: ProcessStatusRow,
    /// Optional cgroup mapping.
    pub cgroup: Option<ProcessCgroupRow>,
}

/// Why one process could not be read.
#[derive(Debug)]
pub enum ProcessError {
    /// Process vanished before required files could be read.
    Gone(i32),
    /// Required process file could not be read.
    Read {
        /// Relative procfs path.
        path: String,
        /// I/O error.
        source: io::Error,
    },
    /// Required process file could not be parsed.
    Parse {
        /// Relative procfs path.
        path: String,
        /// Parse error.
        source: ParseError,
    },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone(pid) => write!(f, "process {pid} disappeared"),
            Self::Read { path, source } => write!(f, "{path}: {source}"),
            Self::Parse { path, source } => write!(f, "{path}: {}", source.0),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Read one process from `/proc/PID`.
///
/// # Errors
/// Returns [`ProcessError`] when a required file disappears, cannot be read, or
/// cannot be parsed. Optional `io`, `schedstat`, `cmdline`, `comm`, and cgroup
/// reads degrade inside the returned row instead.
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

/// Parse `/proc/PID/stat`.
///
/// # Errors
/// Returns [`ParseError`] when required fields are missing or malformed.
pub fn parse_stat(content: &str) -> Result<ProcStat, ParseError> {
    let content = content.trim();
    let open = content
        .find('(')
        .ok_or_else(|| ParseError("stat: missing '('".to_owned()))?;
    let close = content
        .rfind(')')
        .ok_or_else(|| ParseError("stat: missing ')'".to_owned()))?;
    if close <= open {
        return Err(ParseError("stat: invalid comm parentheses".to_owned()));
    }
    let pid_text = content
        .get(..open)
        .ok_or_else(|| ParseError("stat: invalid pid slice".to_owned()))?;
    let comm = content
        .get(open + '('.len_utf8()..close)
        .ok_or_else(|| ParseError("stat: invalid comm slice".to_owned()))?
        .to_owned();
    let rest = content
        .get(close + ')'.len_utf8()..)
        .ok_or_else(|| ParseError("stat: invalid field slice".to_owned()))?;
    let pid = pid_text
        .trim()
        .parse::<i32>()
        .map_err(|err| ParseError(format!("stat pid: {err}")))?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 40 {
        return Err(ParseError(format!(
            "stat: expected at least 40 fields after comm, got {}",
            fields.len()
        )));
    }
    Ok(ProcStat {
        pid,
        comm,
        state: fields[0].bytes().next().unwrap_or(b'?'),
        ppid: parse_i32(fields[1], "ppid")?,
        tty_nr: parse_i32(fields[4], "tty_nr")?,
        minflt: parse_i64(fields[7], "minflt")?,
        majflt: parse_i64(fields[9], "majflt")?,
        utime: parse_i64(fields[11], "utime")?,
        stime: parse_i64(fields[12], "stime")?,
        priority: parse_i64(fields[15], "priority")?,
        nice: parse_i64(fields[16], "nice")?,
        num_threads: parse_i64(fields[17], "num_threads")?,
        starttime_ticks: parse_i64(fields[19], "starttime")?,
        vsize_bytes: parse_i64(fields[20], "vsize")?,
        rss_pages: parse_i64(fields[21], "rss")?,
        exit_signal: parse_i64(fields[35], "exit_signal")?,
        processor: parse_i64(fields[36], "processor")?,
        rt_priority: parse_i64(fields[37], "rt_priority")?,
        policy: parse_i64(fields[38], "policy")?,
        delayacct_blkio_ticks: parse_i64(fields[39], "delayacct_blkio_ticks")?,
    })
}

/// Parse `/proc/PID/status`.
///
/// # Errors
/// Returns [`ParseError`] when UID/GID lines are malformed.
pub fn parse_status(content: &str) -> Result<ProcStatus, ParseError> {
    let mut status = ProcStatus::default();
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Uid" => {
                let ids = parse_id_quad(value, "Uid")?;
                status.uid = ids[0];
                status.euid = ids[1];
            }
            "Gid" => {
                let ids = parse_id_quad(value, "Gid")?;
                status.gid = ids[0];
                status.egid = ids[1];
            }
            "VmData" => status.vm_data = parse_kb(value),
            "VmStk" => status.vm_stk = parse_kb(value),
            "VmLib" => status.vm_lib = parse_kb(value),
            "VmSwap" => status.vm_swap = parse_kb(value),
            "VmLck" => status.vm_lck = parse_kb(value),
            "VmPTE" => status.vm_pte = parse_kb(value),
            "VmPeak" => status.vm_peak = parse_kb(value),
            "VmHWM" => status.vm_hwm = parse_kb(value),
            "Threads" => status.threads = value.parse().unwrap_or(0),
            "FDSize" => status.fdsize = value.parse().unwrap_or(0),
            "voluntary_ctxt_switches" => {
                status.voluntary_ctxt_switches = value.parse().unwrap_or(0);
            }
            "nonvoluntary_ctxt_switches" => {
                status.nonvoluntary_ctxt_switches = value.parse().unwrap_or(0);
            }
            _ => {}
        }
    }
    Ok(status)
}

/// Parse `/proc/PID/io`.
#[must_use]
pub fn parse_io(content: &str) -> ProcIo {
    let mut io = ProcIo::default();
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().parse::<i64>().unwrap_or(0);
        match key.trim() {
            "rchar" => io.rchar = value,
            "wchar" => io.wchar = value,
            "syscr" => io.syscr = value,
            "syscw" => io.syscw = value,
            "read_bytes" => io.read_bytes = value,
            "write_bytes" => io.write_bytes = value,
            "cancelled_write_bytes" => io.cancelled_write_bytes = value,
            _ => {}
        }
    }
    io
}

/// Convert the hot row to the registry row after interning strings.
#[must_use]
pub fn to_hot_section(
    row: &ProcessHotRow,
    scope: u8,
    comm: StrId,
    cmdline: Option<StrId>,
) -> OsProcess {
    OsProcess {
        ts: Ts(row.ts),
        pid: row.pid,
        starttime: Ts(row.starttime),
        ppid: row.ppid,
        uid: row.uid,
        euid: row.euid,
        gid: row.gid,
        egid: row.egid,
        state: row.state,
        num_threads: row.num_threads,
        tty: row.tty,
        comm,
        cmdline,
        utime: row.utime,
        stime: row.stime,
        nice: row.nice,
        prio: row.prio,
        rtprio: row.rtprio,
        policy: row.policy,
        curcpu: row.curcpu,
        rundelay_ns: row.rundelay_ns,
        blkdelay_ticks: row.blkdelay_ticks,
        nvcsw: row.nvcsw,
        nivcsw: row.nivcsw,
        minflt: row.minflt,
        majflt: row.majflt,
        vmem_kb: row.vmem_kb,
        rmem_kb: row.rmem_kb,
        vswap_kb: row.vswap_kb,
        syscr: row.io.map(|io| io.syscr),
        syscw: row.io.map(|io| io.syscw),
        rchar: row.io.map(|io| io.rchar),
        wchar: row.io.map(|io| io.wchar),
        read_bytes: row.io.map(|io| io.read_bytes),
        write_bytes: row.io.map(|io| io.write_bytes),
        cancelled_write_bytes: row.io.map(|io| io.cancelled_write_bytes),
        exit_signal: row.exit_signal,
        scope,
    }
}

/// Convert the extended status row to the registry row.
#[must_use]
pub const fn to_status_section(row: &ProcessStatusRow, scope: u8) -> OsProcessStatus {
    OsProcessStatus {
        ts: Ts(row.ts),
        pid: row.pid,
        starttime: Ts(row.starttime),
        vm_data: row.status.vm_data,
        vm_stk: row.status.vm_stk,
        vm_lib: row.status.vm_lib,
        vm_lck: row.status.vm_lck,
        vm_pte: row.status.vm_pte,
        vm_peak: row.status.vm_peak,
        vm_hwm: row.status.vm_hwm,
        threads: row.status.threads,
        fdsize: row.status.fdsize,
        voluntary_ctxt_switches: row.status.voluntary_ctxt_switches,
        nonvoluntary_ctxt_switches: row.status.nonvoluntary_ctxt_switches,
        scope,
    }
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

/// Pick one process cgroup path from `/proc/PID/cgroup`.
#[must_use]
pub fn parse_cgroup_path(content: &str) -> Option<String> {
    let mut first = None;
    let mut preferred = None;
    for line in content.lines() {
        let mut parts = line.splitn(3, ':');
        let _hierarchy = parts.next();
        let controllers = parts.next()?;
        let path = normalize_cgroup_path(parts.next()?);
        if controllers.is_empty() {
            return Some(path);
        }
        if first.is_none() {
            first = Some(path.clone());
        }
        if preferred.is_none()
            && controllers
                .split(',')
                .any(|c| matches!(c, "pids" | "cpu" | "cpuacct" | "memory"))
        {
            preferred = Some(path);
        }
    }
    preferred.or(first)
}

fn normalize_cgroup_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".to_owned()
    } else if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

fn parse_btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|rest| rest.trim().parse::<i64>().ok())
        .and_then(|secs| secs.checked_mul(1_000_000))
}

fn process_starttime_usec(facts: ProcessFacts, starttime_ticks: i64) -> i64 {
    if facts.clock_ticks_per_sec <= 0 {
        return facts.btime_usec;
    }
    let delta = i128::from(starttime_ticks).saturating_mul(1_000_000)
        / i128::from(facts.clock_ticks_per_sec);
    i64::try_from(i128::from(facts.btime_usec).saturating_add(delta)).unwrap_or(i64::MAX)
}

fn rss_kb(rss_pages: i64, page_size_bytes: i64) -> i64 {
    if rss_pages <= 0 || page_size_bytes <= 0 {
        return 0;
    }
    i64::try_from(i128::from(rss_pages).saturating_mul(i128::from(page_size_bytes)) / 1024)
        .unwrap_or(i64::MAX)
}

fn parse_id_quad(value: &str, key: &str) -> Result<[u32; 4], ParseError> {
    let mut ids = [0_u32; 4];
    for (idx, part) in value.split_whitespace().take(4).enumerate() {
        ids[idx] = part
            .parse()
            .map_err(|err| ParseError(format!("{key}[{idx}]: {err}")))?;
    }
    Ok(ids)
}

fn parse_kb(value: &str) -> i64 {
    value
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn parse_i32(value: &str, name: &str) -> Result<i32, ParseError> {
    value
        .parse()
        .map_err(|err| ParseError(format!("stat {name}: {err}")))
}

fn parse_i64(value: &str, name: &str) -> Result<i64, ParseError> {
    value
        .parse()
        .map_err(|err| ParseError(format!("stat {name}: {err}")))
}

fn u32_from_i64(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

fn i32_from_i64(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

fn i16_from_i64(value: i64) -> i16 {
    i16::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i16::MIN
        } else {
            i16::MAX
        }
    })
}

fn i8_from_i64(value: i64) -> i8 {
    i8::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i8::MIN
        } else {
            i8::MAX
        }
    })
}

fn u8_from_i64(value: i64) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat_line(comm: &str) -> String {
        format!(
            "123 ({comm}) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 -5 16 17 190 204800 12 21 22 23 24 25 26 27 28 29 30 31 32 33 15 2 7 8 9 10 11 12 13 14 15"
        )
    }

    #[test]
    fn stat_comm_uses_the_last_parenthesis() {
        let row = parse_stat(&stat_line("worker (bg) 1")).expect("stat");
        assert_eq!(row.comm, "worker (bg) 1");
        assert_eq!(row.pid, 123);
        assert_eq!(row.state, b'S');
        assert_eq!(row.ppid, 1);
        assert_eq!(row.minflt, 7);
        assert_eq!(row.majflt, 9);
        assert_eq!(row.utime, 11);
        assert_eq!(row.stime, 12);
        assert_eq!(row.nice, -5);
        assert_eq!(row.starttime_ticks, 190);
        assert_eq!(row.exit_signal, 15);
        assert_eq!(row.processor, 2);
        assert_eq!(row.rt_priority, 7);
        assert_eq!(row.policy, 8);
        assert_eq!(row.delayacct_blkio_ticks, 9);
    }

    #[test]
    fn status_parses_identity_memory_and_switches() {
        let status = parse_status(
            "Uid:\t1000\t1001\t1002\t1003\n\
             Gid:\t2000\t2001\t2002\t2003\n\
             VmData:\t10 kB\nVmStk:\t11 kB\nVmLib:\t12 kB\nVmSwap:\t13 kB\n\
             VmLck:\t14 kB\nVmPTE:\t15 kB\nVmPeak:\t16 kB\nVmHWM:\t17 kB\n\
             Threads:\t3\nFDSize:\t64\n\
             voluntary_ctxt_switches:\t20\nnonvoluntary_ctxt_switches:\t21\n",
        )
        .expect("status");
        assert_eq!((status.uid, status.euid), (1000, 1001));
        assert_eq!((status.gid, status.egid), (2000, 2001));
        assert_eq!(status.vm_swap, 13);
        assert_eq!(status.vm_pte, 15);
        assert_eq!(status.vm_hwm, 17);
        assert_eq!(status.threads, 3);
        assert_eq!(status.fdsize, 64);
        assert_eq!(status.nonvoluntary_ctxt_switches, 21);
    }

    #[test]
    fn io_parser_keeps_all_seven_fields() {
        let io = parse_io(
            "rchar: 1\nwchar: 2\nsyscr: 3\nsyscw: 4\nread_bytes: 5\n\
             write_bytes: 6\ncancelled_write_bytes: 7\n",
        );
        assert_eq!(
            io,
            ProcIo {
                rchar: 1,
                wchar: 2,
                syscr: 3,
                syscw: 4,
                read_bytes: 5,
                write_bytes: 6,
                cancelled_write_bytes: 7,
            }
        );
    }

    #[test]
    fn cgroup_path_prefers_v2_then_known_v1_controllers() {
        assert_eq!(
            parse_cgroup_path("0::/kubepods/pod-a/container\n"),
            Some("/kubepods/pod-a/container".to_owned())
        );
        assert_eq!(
            parse_cgroup_path("1:name=systemd:/x\n4:pids:/docker/abc\n"),
            Some("/docker/abc".to_owned())
        );
    }

    #[test]
    fn starttime_uses_boot_time_and_hz() {
        let facts = ProcessFacts {
            btime_usec: 1_700_000_000_000_000,
            clock_ticks_per_sec: 250,
            page_size_bytes: 8192,
        };
        assert_eq!(process_starttime_usec(facts, 500), 1_700_000_002_000_000);
        assert_eq!(rss_kb(2, facts.page_size_bytes), 16);
    }
}
