//! Parse and collect cgroup v2 plus focused cgroup v1 metrics.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use kronika_registry::os_cgroup_cpu::OsCgroupCpu;
use kronika_registry::os_cgroup_io::OsCgroupIo;
use kronika_registry::os_cgroup_memory::OsCgroupMemory;
use kronika_registry::os_cgroup_pids::OsCgroupPids;
use kronika_registry::{StrId, Ts};

use crate::SysFs;

const CGROUP_ROOT: &str = "fs/cgroup";
const DEFAULT_CPU_PERIOD_USEC: i64 = 100_000;

const CPU_V1_DIRS: &[&str] = &["cpu,cpuacct", "cpuacct,cpu", "cpu", "cpuacct", ""];
const MEMORY_V1_DIRS: &[&str] = &["memory", ""];
const PIDS_V1_DIRS: &[&str] = &["pids", ""];
const BLKIO_V1_DIRS: &[&str] = &["blkio", ""];

/// Cgroup collection output before string interning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupCollection {
    /// CPU rows.
    pub cpu: Vec<CgroupCpuRow>,
    /// Memory rows.
    pub memory: Vec<CgroupMemoryRow>,
    /// I/O rows.
    pub io: Vec<CgroupIoRow>,
    /// PIDs rows.
    pub pids: Vec<CgroupPidsRow>,
    /// Cgroup directories skipped because `max_cgroups` fired.
    pub dropped_cgroups: usize,
    /// Cgroup I/O rows skipped because `max_io_rows` fired.
    pub dropped_io_rows: usize,
}

/// CPU metrics for one cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupCpuRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Normalized cgroup path.
    pub cgroup_path: String,
    /// Total usage, microseconds.
    pub usage_usec: i64,
    /// User usage, microseconds.
    pub user_usec: i64,
    /// System usage, microseconds.
    pub system_usec: i64,
    /// Throttled time, microseconds.
    pub throttled_usec: i64,
    /// Number of throttling events.
    pub nr_throttled: i64,
    /// Quota, microseconds; `-1` means unlimited.
    pub quota_usec: i64,
    /// Period, microseconds.
    pub period_usec: i64,
}

/// Memory metrics for one cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupMemoryRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Normalized cgroup path.
    pub cgroup_path: String,
    /// Current usage, bytes.
    pub current: i64,
    /// Limit, bytes; `None` means unlimited.
    pub max: Option<i64>,
    /// Anonymous memory, bytes.
    pub anon: i64,
    /// File-backed memory, bytes.
    pub file: i64,
    /// Kernel memory, bytes.
    pub kernel: i64,
    /// Slab memory, bytes.
    pub slab: i64,
    /// Low memory events.
    pub low_events: i64,
    /// High memory events.
    pub high_events: i64,
    /// Max boundary events.
    pub max_events: i64,
    /// OOM events.
    pub oom_events: i64,
    /// OOM kills.
    pub oom_kill: i64,
}

/// I/O metrics for one cgroup/device pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupIoRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Normalized cgroup path.
    pub cgroup_path: String,
    /// Device major number.
    pub major: u32,
    /// Device minor number.
    pub minor: u32,
    /// Bytes read.
    pub rbytes: i64,
    /// Bytes written.
    pub wbytes: i64,
    /// Read operations.
    pub rios: i64,
    /// Write operations.
    pub wios: i64,
}

/// PID controller metrics for one cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupPidsRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Normalized cgroup path.
    pub cgroup_path: String,
    /// Current process count.
    pub current: i64,
    /// Process limit; `None` means unlimited.
    pub max: Option<i64>,
}

/// Collect cgroup v2 or v1 rows from `KRONIKA_SYS_ROOT/fs/cgroup`.
#[must_use]
pub fn collect(
    sys: &SysFs,
    ts: i64,
    clock_ticks_per_sec: i64,
    max_cgroups: usize,
    max_io_rows: usize,
    max_depth: usize,
) -> CgroupCollection {
    if is_v2(sys) {
        collect_v2(sys, ts, max_cgroups, max_io_rows, max_depth)
    } else {
        collect_v1(
            sys,
            ts,
            clock_ticks_per_sec,
            max_cgroups,
            max_io_rows,
            max_depth,
        )
    }
}

/// Convert a CPU row to the registry row.
#[must_use]
pub const fn to_cpu_section(row: &CgroupCpuRow, scope: u8, cgroup_path: StrId) -> OsCgroupCpu {
    OsCgroupCpu {
        ts: Ts(row.ts),
        cgroup_path,
        usage_usec: row.usage_usec,
        user_usec: row.user_usec,
        system_usec: row.system_usec,
        throttled_usec: row.throttled_usec,
        nr_throttled: row.nr_throttled,
        quota_usec: row.quota_usec,
        period_usec: row.period_usec,
        scope,
    }
}

/// Convert a memory row to the registry row.
#[must_use]
pub const fn to_memory_section(
    row: &CgroupMemoryRow,
    scope: u8,
    cgroup_path: StrId,
) -> OsCgroupMemory {
    OsCgroupMemory {
        ts: Ts(row.ts),
        cgroup_path,
        current: row.current,
        max: row.max,
        anon: row.anon,
        file: row.file,
        kernel: row.kernel,
        slab: row.slab,
        low_events: row.low_events,
        high_events: row.high_events,
        max_events: row.max_events,
        oom_events: row.oom_events,
        oom_kill: row.oom_kill,
        scope,
    }
}

/// Convert an I/O row to the registry row.
#[must_use]
pub const fn to_io_section(row: &CgroupIoRow, scope: u8, cgroup_path: StrId) -> OsCgroupIo {
    OsCgroupIo {
        ts: Ts(row.ts),
        cgroup_path,
        major: row.major,
        minor: row.minor,
        rbytes: row.rbytes,
        wbytes: row.wbytes,
        rios: row.rios,
        wios: row.wios,
        scope,
    }
}

/// Convert a PIDs row to the registry row.
#[must_use]
pub const fn to_pids_section(row: &CgroupPidsRow, scope: u8, cgroup_path: StrId) -> OsCgroupPids {
    OsCgroupPids {
        ts: Ts(row.ts),
        cgroup_path,
        current: row.current,
        max: row.max,
        scope,
    }
}

fn is_v2(sys: &SysFs) -> bool {
    sys.read(&rel("/", "cgroup.controllers")).is_ok()
        || sys.read(&rel("/", "cpu.stat")).is_ok()
        || sys.read(&rel("/", "memory.current")).is_ok()
        || sys.read(&rel("/", "io.stat")).is_ok()
}

fn collect_v2(
    sys: &SysFs,
    ts: i64,
    max_cgroups: usize,
    max_io_rows: usize,
    max_depth: usize,
) -> CgroupCollection {
    let (paths, dropped_cgroups) = discover_v2_paths(sys, max_cgroups, max_depth);
    let mut out = CgroupCollection {
        dropped_cgroups,
        ..CgroupCollection::default()
    };
    for path in paths {
        if let Some(cpu) = read_cpu_v2(sys, ts, &path) {
            out.cpu.push(cpu);
        }
        if let Some(memory) = read_memory_v2(sys, ts, &path) {
            out.memory.push(memory);
        }
        if let Some(pids) = read_pids_v2(sys, ts, &path) {
            out.pids.push(pids);
        }
        if let Ok(content) = sys.read(&rel(&path, "io.stat")) {
            push_io_rows(
                &mut out.io,
                &mut out.dropped_io_rows,
                max_io_rows,
                parse_io_stat(&content, ts, &path),
            );
        }
    }
    out
}

fn collect_v1(
    sys: &SysFs,
    ts: i64,
    clock_ticks_per_sec: i64,
    max_cgroups: usize,
    max_io_rows: usize,
    max_depth: usize,
) -> CgroupCollection {
    let (paths, dropped_cgroups) = discover_v1_paths(sys, max_cgroups, max_depth);
    let mut out = CgroupCollection {
        dropped_cgroups,
        ..CgroupCollection::default()
    };
    for path in paths {
        if let Some(cpu) = read_cpu_v1(sys, ts, &path, clock_ticks_per_sec) {
            out.cpu.push(cpu);
        }
        if let Some(memory) = read_memory_v1(sys, ts, &path) {
            out.memory.push(memory);
        }
        if let Some(pids) = read_pids_v1(sys, ts, &path) {
            out.pids.push(pids);
        }
        let bytes = read_first_v1(sys, BLKIO_V1_DIRS, &path, "blkio.throttle.io_service_bytes")
            .or_else(|| read_first_v1(sys, BLKIO_V1_DIRS, &path, "blkio.io_service_bytes"));
        let ops = read_first_v1(sys, BLKIO_V1_DIRS, &path, "blkio.throttle.io_serviced")
            .or_else(|| read_first_v1(sys, BLKIO_V1_DIRS, &path, "blkio.io_serviced"));
        if bytes.is_some() || ops.is_some() {
            push_io_rows(
                &mut out.io,
                &mut out.dropped_io_rows,
                max_io_rows,
                parse_blkio_service_stats(
                    bytes.as_deref().unwrap_or_default(),
                    ops.as_deref().unwrap_or_default(),
                    ts,
                    &path,
                ),
            );
        }
    }
    out
}

fn read_cpu_v2(sys: &SysFs, ts: i64, path: &str) -> Option<CgroupCpuRow> {
    let stat = sys.read(&rel(path, "cpu.stat")).ok()?;
    let mut row = parse_cpu_stat(&stat, ts, path);
    if let Ok(max) = sys.read(&rel(path, "cpu.max")) {
        let (quota, period) = parse_cpu_max(&max);
        row.quota_usec = quota;
        row.period_usec = period;
    }
    Some(row)
}

fn read_cpu_v1(sys: &SysFs, ts: i64, path: &str, clock_ticks_per_sec: i64) -> Option<CgroupCpuRow> {
    let mut row = CgroupCpuRow {
        ts,
        cgroup_path: path.to_owned(),
        usage_usec: 0,
        user_usec: 0,
        system_usec: 0,
        throttled_usec: 0,
        nr_throttled: 0,
        quota_usec: -1,
        period_usec: DEFAULT_CPU_PERIOD_USEC,
    };
    let usage_found =
        read_first_v1(sys, CPU_V1_DIRS, path, "cpuacct.usage").is_some_and(|content| {
            row.usage_usec = parse_i64(&content).unwrap_or(0) / 1000;
            true
        });
    let acct_found = read_first_v1(sys, CPU_V1_DIRS, path, "cpuacct.stat").is_some_and(|content| {
        parse_cpuacct_stat(&content, clock_ticks_per_sec, &mut row);
        true
    });
    if let Some(content) = read_first_v1(sys, CPU_V1_DIRS, path, "cpu.cfs_quota_us") {
        row.quota_usec = parse_i64(&content).unwrap_or(-1);
    }
    if let Some(content) = read_first_v1(sys, CPU_V1_DIRS, path, "cpu.cfs_period_us") {
        row.period_usec = parse_i64(&content).unwrap_or(DEFAULT_CPU_PERIOD_USEC);
    }
    let throttle_found = read_first_v1(sys, CPU_V1_DIRS, path, "cpu.stat").is_some_and(|content| {
        parse_v1_cpu_stat(&content, &mut row);
        true
    });
    (usage_found || acct_found || throttle_found).then_some(row)
}

fn read_memory_v2(sys: &SysFs, ts: i64, path: &str) -> Option<CgroupMemoryRow> {
    let current = parse_i64(&sys.read(&rel(path, "memory.current")).ok()?).unwrap_or(0);
    let mut row = CgroupMemoryRow {
        ts,
        cgroup_path: path.to_owned(),
        current,
        max: sys
            .read(&rel(path, "memory.max"))
            .ok()
            .and_then(|content| parse_optional_max(&content)),
        anon: 0,
        file: 0,
        kernel: 0,
        slab: 0,
        low_events: 0,
        high_events: 0,
        max_events: 0,
        oom_events: 0,
        oom_kill: 0,
    };
    if let Ok(content) = sys.read(&rel(path, "memory.stat")) {
        parse_memory_stat_v2(&content, &mut row);
    }
    if let Ok(content) = sys.read(&rel(path, "memory.events")) {
        parse_memory_events(&content, &mut row);
    }
    Some(row)
}

fn read_memory_v1(sys: &SysFs, ts: i64, path: &str) -> Option<CgroupMemoryRow> {
    let current = parse_i64(&read_first_v1(
        sys,
        MEMORY_V1_DIRS,
        path,
        "memory.usage_in_bytes",
    )?)
    .unwrap_or(0);
    let mut row = CgroupMemoryRow {
        ts,
        cgroup_path: path.to_owned(),
        current,
        max: read_first_v1(sys, MEMORY_V1_DIRS, path, "memory.limit_in_bytes")
            .and_then(|content| parse_v1_memory_limit(&content)),
        anon: 0,
        file: 0,
        kernel: 0,
        slab: 0,
        low_events: 0,
        high_events: 0,
        max_events: 0,
        oom_events: 0,
        oom_kill: 0,
    };
    if let Some(content) = read_first_v1(sys, MEMORY_V1_DIRS, path, "memory.stat") {
        parse_memory_stat_v1(&content, &mut row);
    }
    if let Some(content) = read_first_v1(sys, MEMORY_V1_DIRS, path, "memory.failcnt") {
        row.max_events = parse_i64(&content).unwrap_or(0);
    }
    Some(row)
}

fn read_pids_v2(sys: &SysFs, ts: i64, path: &str) -> Option<CgroupPidsRow> {
    let current = parse_i64(&sys.read(&rel(path, "pids.current")).ok()?).unwrap_or(0);
    Some(CgroupPidsRow {
        ts,
        cgroup_path: path.to_owned(),
        current,
        max: sys
            .read(&rel(path, "pids.max"))
            .ok()
            .and_then(|content| parse_optional_max(&content)),
    })
}

fn read_pids_v1(sys: &SysFs, ts: i64, path: &str) -> Option<CgroupPidsRow> {
    let current = parse_i64(&read_first_v1(sys, PIDS_V1_DIRS, path, "pids.current")?).unwrap_or(0);
    Some(CgroupPidsRow {
        ts,
        cgroup_path: path.to_owned(),
        current,
        max: read_first_v1(sys, PIDS_V1_DIRS, path, "pids.max")
            .and_then(|content| parse_optional_max(&content)),
    })
}

fn discover_v2_paths(sys: &SysFs, max_cgroups: usize, max_depth: usize) -> (Vec<String>, usize) {
    discover_tree(sys, CGROUP_ROOT, "/", max_cgroups, max_depth)
}

fn discover_v1_paths(sys: &SysFs, max_cgroups: usize, max_depth: usize) -> (Vec<String>, usize) {
    let mut paths = BTreeSet::new();
    let mut dropped = 0_usize;
    for controller in [
        "cpu,cpuacct",
        "cpuacct,cpu",
        "cpu",
        "cpuacct",
        "memory",
        "pids",
        "blkio",
    ] {
        let base = format!("{CGROUP_ROOT}/{controller}");
        let (found, local_dropped) = discover_tree(sys, &base, "/", max_cgroups, max_depth);
        dropped = dropped.saturating_add(local_dropped);
        for path in found {
            if paths.len() < max_cgroups {
                paths.insert(path);
            } else if !paths.contains(&path) {
                dropped = dropped.saturating_add(1);
            }
        }
    }
    if paths.is_empty()
        && ["cpuacct.usage", "memory.usage_in_bytes", "pids.current"]
            .iter()
            .any(|file| sys.read(&format!("{CGROUP_ROOT}/{file}")).is_ok())
        && max_cgroups > 0
    {
        paths.insert("/".to_owned());
    }
    (paths.into_iter().collect(), dropped)
}

fn discover_tree(
    sys: &SysFs,
    base_rel: &str,
    root_path: &str,
    max_cgroups: usize,
    max_depth: usize,
) -> (Vec<String>, usize) {
    let Ok(root_children) = sys.read_dir(base_rel) else {
        return (Vec::new(), 0);
    };
    if max_cgroups == 0 {
        return (Vec::new(), 1);
    }

    let mut out = Vec::new();
    let mut dropped = 0_usize;
    out.push(normalize_path(root_path, ""));

    let mut queue = VecDeque::new();
    if max_depth > 0 {
        for child in root_children.into_iter().filter(|entry| entry.is_dir) {
            queue.push_back((child.name, 1_usize));
        }
    }
    while let Some((relative, depth)) = queue.pop_front() {
        let path = normalize_path(root_path, &relative);
        if out.len() < max_cgroups {
            out.push(path);
        } else {
            dropped = dropped.saturating_add(1);
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        let rel = if relative.is_empty() {
            base_rel.to_owned()
        } else {
            format!("{base_rel}/{relative}")
        };
        let Ok(children) = sys.read_dir(&rel) else {
            continue;
        };
        for child in children.into_iter().filter(|entry| entry.is_dir) {
            let child_rel = if relative.is_empty() {
                child.name
            } else {
                format!("{relative}/{}", child.name)
            };
            queue.push_back((child_rel, depth + 1));
        }
    }
    (out, dropped)
}

fn normalize_path(root_path: &str, relative: &str) -> String {
    if relative.is_empty() {
        root_path.to_owned()
    } else if root_path == "/" {
        format!("/{relative}")
    } else {
        format!("{root_path}/{relative}")
    }
}

fn rel(path: &str, file: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() {
        format!("{CGROUP_ROOT}/{file}")
    } else {
        format!("{CGROUP_ROOT}/{path}/{file}")
    }
}

fn read_first_v1(sys: &SysFs, dirs: &[&str], path: &str, file: &str) -> Option<String> {
    let relative = path.trim_matches('/');
    for dir in dirs {
        let candidate = match (dir.is_empty(), relative.is_empty()) {
            (true, true) => format!("{CGROUP_ROOT}/{file}"),
            (true, false) => format!("{CGROUP_ROOT}/{relative}/{file}"),
            (false, true) => format!("{CGROUP_ROOT}/{dir}/{file}"),
            (false, false) => format!("{CGROUP_ROOT}/{dir}/{relative}/{file}"),
        };
        if let Ok(content) = sys.read(&candidate) {
            return Some(content);
        }
    }
    None
}

fn push_io_rows(
    rows: &mut Vec<CgroupIoRow>,
    dropped: &mut usize,
    max_rows: usize,
    incoming: Vec<CgroupIoRow>,
) {
    for row in incoming {
        if rows.len() < max_rows {
            rows.push(row);
        } else {
            *dropped = dropped.saturating_add(1);
        }
    }
}

/// Parse cgroup v2 `cpu.max`.
#[must_use]
pub fn parse_cpu_max(content: &str) -> (i64, i64) {
    let mut fields = content.split_whitespace();
    let quota = match fields.next() {
        Some("max") | None => -1,
        Some(value) => value.parse().unwrap_or(-1),
    };
    let period = fields
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CPU_PERIOD_USEC);
    (quota, period)
}

/// Parse cgroup v2 `cpu.stat`.
#[must_use]
pub fn parse_cpu_stat(content: &str, ts: i64, cgroup_path: &str) -> CgroupCpuRow {
    let mut row = CgroupCpuRow {
        ts,
        cgroup_path: cgroup_path.to_owned(),
        usage_usec: 0,
        user_usec: 0,
        system_usec: 0,
        throttled_usec: 0,
        nr_throttled: 0,
        quota_usec: -1,
        period_usec: DEFAULT_CPU_PERIOD_USEC,
    };
    for (key, value) in key_value_lines(content) {
        match key {
            "usage_usec" => row.usage_usec = value,
            "user_usec" => row.user_usec = value,
            "system_usec" => row.system_usec = value,
            "throttled_usec" => row.throttled_usec = value,
            "nr_throttled" => row.nr_throttled = value,
            _ => {}
        }
    }
    row
}

fn parse_cpuacct_stat(content: &str, clock_ticks_per_sec: i64, row: &mut CgroupCpuRow) {
    for (key, value) in key_value_lines(content) {
        let usec = ticks_to_usec(value, clock_ticks_per_sec);
        match key {
            "user" => row.user_usec = usec,
            "system" => row.system_usec = usec,
            _ => {}
        }
    }
}

fn parse_v1_cpu_stat(content: &str, row: &mut CgroupCpuRow) {
    for (key, value) in key_value_lines(content) {
        match key {
            "nr_throttled" => row.nr_throttled = value,
            "throttled_time" => row.throttled_usec = value / 1000,
            _ => {}
        }
    }
}

fn parse_memory_stat_v2(content: &str, row: &mut CgroupMemoryRow) {
    for (key, value) in key_value_lines(content) {
        match key {
            "anon" => row.anon = value,
            "file" => row.file = value,
            "kernel" => row.kernel = value,
            "slab" => row.slab = value,
            _ => {}
        }
    }
}

fn parse_memory_stat_v1(content: &str, row: &mut CgroupMemoryRow) {
    let mut kernel_stack = 0_i64;
    for (key, value) in key_value_lines(content) {
        match key {
            "rss" | "total_rss" => row.anon = value,
            "cache" | "total_cache" => row.file = value,
            "slab" | "total_slab" => row.slab = value,
            "kernel_stack" | "total_kernel_stack" => kernel_stack = value,
            _ => {}
        }
    }
    row.kernel = row.slab.saturating_add(kernel_stack);
}

fn parse_memory_events(content: &str, row: &mut CgroupMemoryRow) {
    for (key, value) in key_value_lines(content) {
        match key {
            "low" => row.low_events = value,
            "high" => row.high_events = value,
            "max" => row.max_events = value,
            "oom" => row.oom_events = value,
            "oom_kill" => row.oom_kill = value,
            _ => {}
        }
    }
}

/// Parse cgroup v2 `io.stat`.
#[must_use]
pub fn parse_io_stat(content: &str, ts: i64, cgroup_path: &str) -> Vec<CgroupIoRow> {
    let mut rows = Vec::new();
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next() else {
            continue;
        };
        let Some((major, minor)) = parse_device(device) else {
            continue;
        };
        let mut row = CgroupIoRow {
            ts,
            cgroup_path: cgroup_path.to_owned(),
            major,
            minor,
            rbytes: 0,
            wbytes: 0,
            rios: 0,
            wios: 0,
        };
        for field in fields {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let value = value.parse().unwrap_or(0);
            match key {
                "rbytes" => row.rbytes = value,
                "wbytes" => row.wbytes = value,
                "rios" => row.rios = value,
                "wios" => row.wios = value,
                _ => {}
            }
        }
        rows.push(row);
    }
    rows
}

/// Parse cgroup v1 blkio service byte/op files.
#[must_use]
pub fn parse_blkio_service_stats(
    bytes_content: &str,
    ops_content: &str,
    ts: i64,
    cgroup_path: &str,
) -> Vec<CgroupIoRow> {
    let mut rows: BTreeMap<(u32, u32), CgroupIoRow> = BTreeMap::new();
    parse_blkio_service_file(bytes_content, ts, cgroup_path, true, &mut rows);
    parse_blkio_service_file(ops_content, ts, cgroup_path, false, &mut rows);
    rows.into_values().collect()
}

fn parse_blkio_service_file(
    content: &str,
    ts: i64,
    cgroup_path: &str,
    bytes: bool,
    rows: &mut BTreeMap<(u32, u32), CgroupIoRow>,
) {
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next() else {
            continue;
        };
        let Some(op) = fields.next() else {
            continue;
        };
        if op == "Total" {
            continue;
        }
        let Some(value) = fields.next().and_then(|value| value.parse().ok()) else {
            continue;
        };
        let Some((major, minor)) = parse_device(device) else {
            continue;
        };
        let row = rows.entry((major, minor)).or_insert_with(|| CgroupIoRow {
            ts,
            cgroup_path: cgroup_path.to_owned(),
            major,
            minor,
            rbytes: 0,
            wbytes: 0,
            rios: 0,
            wios: 0,
        });
        match (op, bytes) {
            ("Read", true) => row.rbytes = value,
            ("Write", true) => row.wbytes = value,
            ("Read", false) => row.rios = value,
            ("Write", false) => row.wios = value,
            _ => {}
        }
    }
}

fn key_value_lines(content: &str) -> impl Iterator<Item = (&str, i64)> {
    content.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        Some((fields.next()?, fields.next()?.parse().unwrap_or(0)))
    })
}

fn parse_optional_max(content: &str) -> Option<i64> {
    let trimmed = content.trim();
    if trimmed == "max" {
        None
    } else {
        parse_i64(trimmed)
    }
}

fn parse_v1_memory_limit(content: &str) -> Option<i64> {
    let value = parse_i64(content)?;
    (value < i64::MAX / 2).then_some(value)
}

fn parse_i64(content: &str) -> Option<i64> {
    content.trim().parse().ok()
}

fn parse_device(device: &str) -> Option<(u32, u32)> {
    let (major, minor) = device.split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn ticks_to_usec(ticks: i64, hz: i64) -> i64 {
    if hz <= 0 {
        return 0;
    }
    i64::try_from(i128::from(ticks).saturating_mul(1_000_000) / i128::from(hz)).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_max_handles_unlimited() {
        assert_eq!(parse_cpu_max("max 100000\n"), (-1, 100_000));
        assert_eq!(parse_cpu_max("200000 100000\n"), (200_000, 100_000));
    }

    #[test]
    fn cpuacct_stat_uses_hz() {
        let mut row = parse_cpu_stat("", 1, "/");
        parse_cpuacct_stat("user 250\nsystem 125\n", 250, &mut row);
        assert_eq!(row.user_usec, 1_000_000);
        assert_eq!(row.system_usec, 500_000);
    }

    #[test]
    fn memory_v1_maps_total_fields_and_kernel_stack() {
        let mut row = CgroupMemoryRow {
            ts: 1,
            cgroup_path: "/x".to_owned(),
            current: 0,
            max: None,
            anon: 0,
            file: 0,
            kernel: 0,
            slab: 0,
            low_events: 0,
            high_events: 0,
            max_events: 0,
            oom_events: 0,
            oom_kill: 0,
        };
        parse_memory_stat_v1(
            "total_rss 10\ntotal_cache 20\ntotal_slab 3\ntotal_kernel_stack 4\n",
            &mut row,
        );
        assert_eq!(row.anon, 10);
        assert_eq!(row.file, 20);
        assert_eq!(row.slab, 3);
        assert_eq!(row.kernel, 7);
    }

    #[test]
    fn io_stat_parses_per_device_counters() {
        let rows = parse_io_stat("8:0 rbytes=1 wbytes=2 rios=3 wios=4 dbytes=9\n", 5, "/x");
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].major, rows[0].minor), (8, 0));
        assert_eq!(rows[0].rbytes, 1);
        assert_eq!(rows[0].wios, 4);
    }

    #[test]
    fn blkio_parser_skips_total_and_merges_ops() {
        let rows = parse_blkio_service_stats(
            "8:0 Read 4096\n8:0 Write 8192\n8:0 Total 12288\n",
            "8:0 Read 4\n8:0 Write 8\n8:0 Total 12\n",
            1,
            "/x",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rbytes, 4096);
        assert_eq!(rows[0].wbytes, 8192);
        assert_eq!(rows[0].rios, 4);
        assert_eq!(rows[0].wios, 8);
    }
}
