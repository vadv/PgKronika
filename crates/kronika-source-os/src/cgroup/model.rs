//! Cgroup rows before string interning.

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
