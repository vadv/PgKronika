use crate::buffer_row;
use crate::config::env_u64;
use crate::logging::{
    LogLevel, field, layout_id, log_collection_finish, log_count_degraded, log_event, section_name,
};
use crate::scheduler::{DueSet, SourceKind};
use anyhow::Result;
use kronika_registry::os_cgroup_cpu::OsCgroupCpu;
use kronika_registry::os_cgroup_io::OsCgroupIo;
use kronika_registry::os_cgroup_mapping::OsCgroupMapping;
use kronika_registry::os_cgroup_memory::OsCgroupMemory;
use kronika_registry::os_cgroup_pids::OsCgroupPids;
use kronika_registry::os_cpu::OsCpu;
use kronika_registry::os_diskstats::OsDiskstats;
use kronika_registry::os_loadavg::OsLoadavg;
use kronika_registry::os_meminfo::OsMeminfo;
use kronika_registry::os_mountinfo::OsMountinfo;
use kronika_registry::os_netdev::OsNetdev;
use kronika_registry::os_netstat::OsNetstat;
use kronika_registry::os_process::OsProcess;
use kronika_registry::os_process_status::OsProcessStatus;
use kronika_registry::os_psi::OsPsi;
use kronika_registry::os_snmp::OsSnmp;
use kronika_registry::os_stat::OsStat;
use kronika_registry::os_topology::OsTopology;
use kronika_registry::os_vmstat::OsVmstat;
use kronika_registry::{StrId, Ts};
use kronika_source_os::proc::cpuinfo;
use kronika_source_os::proc::loadavg::parse_loadavg;
use kronika_source_os::proc::meminfo::parse_meminfo;
use kronika_source_os::proc::pressure::parse_pressure;
use kronika_source_os::proc::process::{ProcessError, process_facts, read_process};
use kronika_source_os::proc::stat::{parse_cpu, parse_stat_misc};
use kronika_source_os::proc::vmstat::parse_vmstat;
use kronika_source_os::proc::{diskstats, net_dev, net_netstat, net_snmp};
use kronika_source_os::{
    MountEntry, OsScope, ProcFs, SysFs, cgroup, container_device_set, mount_row, net_scope,
    parse_dev_pair, parse_mountinfo, statvfs,
};
use kronika_writer::{Interner, SectionBuffers};
use std::io::ErrorKind;
use std::time::Instant;

/// OS procfs sections collected synchronously in the read phase.
pub(crate) struct OsSources {
    cpu: Vec<OsCpu>,
    stat: Option<OsStat>,
    meminfo: Option<OsMeminfo>,
    loadavg: Option<OsLoadavg>,
    vmstat: Option<OsVmstat>,
    psi: Vec<OsPsi>,
    diskstats: Vec<OsDiskstats>,
    netdev: Vec<OsNetdev>,
    snmp: Option<OsSnmp>,
    netstat: Option<OsNetstat>,
    mountinfo: Vec<OsMountinfo>,
    topology: Vec<OsTopology>,
    processes: Vec<OsProcess>,
    process_status: Vec<OsProcessStatus>,
    cgroup_mapping: Vec<OsCgroupMapping>,
    cgroup_cpu: Vec<OsCgroupCpu>,
    cgroup_memory: Vec<OsCgroupMemory>,
    cgroup_io: Vec<OsCgroupIo>,
    cgroup_pids: Vec<OsCgroupPids>,
}

impl OsSources {
    const fn empty() -> Self {
        Self {
            cpu: Vec::new(),
            stat: None,
            meminfo: None,
            loadavg: None,
            vmstat: None,
            psi: Vec::new(),
            diskstats: Vec::new(),
            netdev: Vec::new(),
            snmp: None,
            netstat: None,
            mountinfo: Vec::new(),
            topology: Vec::new(),
            processes: Vec::new(),
            process_status: Vec::new(),
            cgroup_mapping: Vec::new(),
            cgroup_cpu: Vec::new(),
            cgroup_memory: Vec::new(),
            cgroup_io: Vec::new(),
            cgroup_pids: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn diskstats_empty(&self) -> bool {
        self.diskstats.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn mountinfo_empty(&self) -> bool {
        self.mountinfo.is_empty()
    }
}

fn read_optional_os_file(fs: &ProcFs, rel: &'static str, type_id: u32) -> Option<String> {
    match fs.read_raw(rel) {
        Ok(content) => Some(content),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", rel),
                    field("reason", &err),
                ],
            );
            None
        }
    }
}

/// Read every procfs OS section synchronously.
///
/// Counter sections (cpu, stat, meminfo, loadavg, vmstat, psi, diskstats,
/// netdev, snmp, netstat) are gated on `due.has(SourceKind::OsCore)` and are
/// never emitted on an OsMountTopo-only tick. Mountinfo is parsed on every
/// `OsCore` tick for diskstats attribution and emitted, together with topology,
/// only when `due.has(SourceKind::OsMountTopo)` is true.
/// On file read or parse failure the affected section is skipped and a
/// `collection_degraded` event is logged; zeros are never fabricated. `scope`
/// is the host scope for device-local sections; network sections carry their
/// own `net_scope`.
///
/// The `interner` is the segment's interner: device, interface, and mount
/// strings are interned here so the built rows already hold their `StrId`s.
#[allow(
    clippy::too_many_lines,
    reason = "independent procfs reads with per-source degradation logging kept adjacent"
)]
pub(crate) fn collect_os_sources(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    in_container: bool,
    due: &DueSet,
) -> OsSources {
    if !due.has(SourceKind::OsCore)
        && !due.has(SourceKind::OsMountTopo)
        && !due.has(SourceKind::OsProcesses)
        && !due.has(SourceKind::OsProcessStatus)
        && !due.has(SourceKind::OsCgroup)
        && !due.has(SourceKind::OsCgroupMapping)
    {
        return OsSources::empty();
    }

    let mut os = OsSources::empty();

    if due.has(SourceKind::OsCore) {
        // stat — read once, feed to both cpu and stat-misc parsers.
        let stat_started = Instant::now();
        match fs.read("stat") {
            Ok(content) => {
                // CPU rows (1_102_001)
                let cpu_type_id = 1_102_001_u32;
                match parse_cpu(&content, ts) {
                    Ok(rows) => {
                        let n = rows.len();
                        os.cpu = rows.into_iter().map(|r| r.to_section(scope)).collect();
                        log_collection_finish(cpu_type_id, "procfs", n, stat_started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(cpu_type_id)),
                                field("type_id", cpu_type_id),
                                field("layout_id", layout_id(cpu_type_id)),
                                field("source", "stat"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                }
                // Stat-misc row (1_103_001) — same content, separate parser.
                // Its own clock so the reported latency excludes the CPU parse above.
                let stat_misc_started = Instant::now();
                let stat_type_id = 1_103_001_u32;
                match parse_stat_misc(&content, ts) {
                    Ok(row) => {
                        os.stat = Some(row.to_section(scope));
                        log_collection_finish(
                            stat_type_id,
                            "procfs",
                            1,
                            stat_misc_started.elapsed(),
                        );
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(stat_type_id)),
                                field("type_id", stat_type_id),
                                field("layout_id", layout_id(stat_type_id)),
                                field("source", "stat"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                }
            }
            Err(err) => {
                let cpu_type_id = 1_102_001_u32;
                let stat_type_id = 1_103_001_u32;
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(cpu_type_id)),
                        field("type_id", cpu_type_id),
                        field("layout_id", layout_id(cpu_type_id)),
                        field("source", "stat"),
                        field("reason", &err),
                    ],
                );
                log_event(
                    LogLevel::Warn,
                    "collection_degraded",
                    &[
                        field("collection", section_name(stat_type_id)),
                        field("type_id", stat_type_id),
                        field("layout_id", layout_id(stat_type_id)),
                        field("source", "stat"),
                        field("reason", &err),
                    ],
                );
            }
        }

        // meminfo (1_104_001)
        {
            let type_id = 1_104_001_u32;
            let started = Instant::now();
            match fs.read("meminfo") {
                Ok(content) => match parse_meminfo(&content, ts) {
                    Ok(row) => {
                        os.meminfo = Some(row.to_section(scope));
                        log_collection_finish(type_id, "procfs", 1, started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "meminfo"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                },
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "meminfo"),
                            field("reason", &err),
                        ],
                    );
                }
            }
        }

        // loadavg (1_105_001)
        {
            let type_id = 1_105_001_u32;
            let started = Instant::now();
            match fs.read("loadavg") {
                Ok(content) => match parse_loadavg(&content, ts) {
                    Ok(row) => {
                        os.loadavg = Some(row.to_section(scope));
                        log_collection_finish(type_id, "procfs", 1, started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "loadavg"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                },
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "loadavg"),
                            field("reason", &err),
                        ],
                    );
                }
            }
        }

        // vmstat (1_106_001)
        {
            let type_id = 1_106_001_u32;
            let started = Instant::now();
            match fs.read("vmstat") {
                Ok(content) => match parse_vmstat(&content, ts) {
                    Ok(row) => {
                        os.vmstat = Some(row.to_section(scope));
                        log_collection_finish(type_id, "procfs", 1, started.elapsed());
                    }
                    Err(err) => {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "vmstat"),
                                field("reason", &err.0),
                            ],
                        );
                    }
                },
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "vmstat"),
                            field("reason", &err),
                        ],
                    );
                }
            }
        }

        // PSI — cpu/memory/io as Option<String>; missing file → None (1_107_001)
        {
            let type_id = 1_107_001_u32;
            let started = Instant::now();
            let psi_cpu = read_optional_os_file(fs, "pressure/cpu", type_id);
            let psi_memory = read_optional_os_file(fs, "pressure/memory", type_id);
            let psi_io = read_optional_os_file(fs, "pressure/io", type_id);
            match parse_pressure(
                psi_cpu.as_deref(),
                psi_memory.as_deref(),
                psi_io.as_deref(),
                ts,
            ) {
                Ok(rows) => {
                    let n = rows.len();
                    if n == 0 {
                        log_event(
                            LogLevel::Warn,
                            "collection_degraded",
                            &[
                                field("collection", section_name(type_id)),
                                field("type_id", type_id),
                                field("layout_id", layout_id(type_id)),
                                field("source", "pressure/{cpu,memory,io}"),
                                field("reason", "no pressure files available"),
                            ],
                        );
                    } else {
                        os.psi = rows.into_iter().map(|r| r.to_section(scope)).collect();
                        log_collection_finish(type_id, "procfs", n, started.elapsed());
                    }
                }
                Err(err) => {
                    log_event(
                        LogLevel::Warn,
                        "collection_degraded",
                        &[
                            field("collection", section_name(type_id)),
                            field("type_id", type_id),
                            field("layout_id", layout_id(type_id)),
                            field("source", "pressure/{cpu,memory,io}"),
                            field("reason", &err.0),
                        ],
                    );
                }
            }
        }
    } // end if due.has(SourceKind::OsCore) — stat, meminfo, loadavg, vmstat, psi

    // Mountinfo is parsed whenever either OsCore or OsMountTopo is due:
    // OsCore needs it for the container device filter in diskstats;
    // OsMountTopo needs it to build the attribution section rows.
    let mounts = mountinfo_entries(fs);

    if due.has(SourceKind::OsCore) {
        // Counters: disk and network. Network sections carry the pod's
        // network-namespace scope inside a container, not the host scope.
        let net_scope_id = net_scope(fs).as_u8();
        os.diskstats = collect_diskstats(fs, interner, scope, ts, in_container, &mounts);
        os.netdev = collect_netdev(fs, interner, net_scope_id, ts);
        collect_net_singletons(fs, net_scope_id, ts, &mut os);
    }

    if due.has(SourceKind::OsMountTopo) {
        os.mountinfo = collect_mountinfo(interner, scope, ts, &mounts);
        os.topology = collect_topology(fs, &SysFs::from_env(), interner, scope, ts);
    }

    let entity_scope = os_entity_scope(in_container);
    collect_process_sections(fs, interner, entity_scope, ts, due, &mut os);
    collect_cgroup_sections(
        &SysFs::from_env(),
        interner,
        entity_scope,
        ts,
        fs,
        due,
        &mut os,
    );

    os
}

/// Read and parse `/proc/diskstats`, interning device names into rows.
///
/// Inside a container the pod's real backing devices are the only ones charged
/// to it: `/proc/diskstats` reports the whole node, so rows are filtered to the
/// mountinfo-derived device set. Over `KRONIKA_OS_MAX_DISKS` rows the lowest
/// `(major, minor)` devices are kept and the overflow is logged, not dropped
/// silently.
fn collect_diskstats(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    in_container: bool,
    mounts: &[MountEntry],
) -> Vec<OsDiskstats> {
    let type_id = 1_108_001_u32;
    let started = Instant::now();
    let Some(content) = read_optional_os_file(fs, "diskstats", type_id) else {
        return Vec::new();
    };
    let mut rows = match diskstats::parse(&content) {
        Ok(rows) => rows,
        Err(err) => {
            log_degraded(type_id, "diskstats", &err.0);
            return Vec::new();
        }
    };

    if in_container {
        let devices = container_device_set(mounts);
        rows.retain(|row| devices.contains(&(row.major, row.minor)));
    }

    apply_disk_cap(&mut rows, type_id);

    let built: Vec<OsDiskstats> = rows
        .iter()
        .filter_map(|row| {
            let device = intern_str(interner, type_id, "diskstats", &row.device)?;
            Some(row.to_section(scope, ts, device))
        })
        .collect();
    log_collection_finish(type_id, "procfs", built.len(), started.elapsed());
    built
}

/// Keep at most `KRONIKA_OS_MAX_DISKS` devices, ordered by `(major, minor)`.
///
/// When the cap trims rows, a `collection_degraded` event with `reason=disk_cap`
/// records how many devices were dropped so the gap is visible, not silent.
fn apply_disk_cap(rows: &mut Vec<diskstats::DiskstatsRow>, type_id: u32) {
    let cap = os_max_disks(type_id);
    let cap = usize::try_from(cap).unwrap_or(usize::MAX);
    let dropped = cap_disks(rows, cap);
    if dropped == 0 {
        return;
    }
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", "diskstats"),
            field("reason", "disk_cap"),
            field("dropped", dropped),
            field("cap", cap),
        ],
    );
}

fn os_max_disks(type_id: u32) -> u64 {
    match env_u64("KRONIKA_OS_MAX_DISKS", 256) {
        Ok(cap) => cap,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", "KRONIKA_OS_MAX_DISKS"),
                    field("reason", &err),
                    field("cap", 256_u64),
                ],
            );
            256
        }
    }
}

/// Trim `rows` to the `cap` lowest `(major, minor)` devices in place.
///
/// Returns the number of devices dropped (`0` when already within the cap).
pub(crate) fn cap_disks(rows: &mut Vec<diskstats::DiskstatsRow>, cap: usize) -> usize {
    if rows.len() <= cap {
        return 0;
    }
    rows.sort_by_key(|row| (row.major, row.minor));
    let dropped = rows.len() - cap;
    rows.truncate(cap);
    dropped
}

/// Read and parse `/proc/net/dev`, interning interface names into rows.
fn collect_netdev(fs: &ProcFs, interner: &mut Interner, scope: u8, ts: i64) -> Vec<OsNetdev> {
    let type_id = 1_109_001_u32;
    let started = Instant::now();
    let Some(content) = read_optional_os_file(fs, "net/dev", type_id) else {
        return Vec::new();
    };
    let rows = match net_dev::parse(&content) {
        Ok(rows) => rows,
        Err(err) => {
            log_degraded(type_id, "net/dev", &err.0);
            return Vec::new();
        }
    };
    let built: Vec<OsNetdev> = rows
        .iter()
        .filter_map(|row| {
            let iface = intern_str(interner, type_id, "net/dev", &row.iface)?;
            Some(row.to_section(scope, ts, iface))
        })
        .collect();
    log_collection_finish(type_id, "procfs", built.len(), started.elapsed());
    built
}

/// Read the two singleton network counter files into `os`.
fn collect_net_singletons(fs: &ProcFs, scope: u8, ts: i64, os: &mut OsSources) {
    let snmp_type_id = 1_110_001_u32;
    let started = Instant::now();
    if let Some(content) = read_optional_os_file(fs, "net/snmp", snmp_type_id) {
        match net_snmp::parse(&content) {
            Ok(row) => {
                os.snmp = Some(row.to_section(scope, ts));
                log_collection_finish(snmp_type_id, "procfs", 1, started.elapsed());
            }
            Err(err) => log_degraded(snmp_type_id, "net/snmp", &err.0),
        }
    }

    let netstat_type_id = 1_111_001_u32;
    let started = Instant::now();
    if let Some(content) = read_optional_os_file(fs, "net/netstat", netstat_type_id) {
        match net_netstat::parse(&content) {
            Ok(row) => {
                os.netstat = Some(row.to_section(scope, ts));
                log_collection_finish(netstat_type_id, "procfs", 1, started.elapsed());
            }
            Err(err) => log_degraded(netstat_type_id, "net/netstat", &err.0),
        }
    }
}

/// Read and parse `/proc/self/mountinfo`, resolving `major == 0` subvolume
/// devices via `/sys`.
fn mountinfo_entries(fs: &ProcFs) -> Vec<MountEntry> {
    let type_id = 1_112_001_u32;
    let Some(content) = read_optional_os_file(fs, "self/mountinfo", type_id) else {
        return Vec::new();
    };
    let mut entries = parse_mountinfo(&content);
    resolve_major_zero(&SysFs::from_env(), &mut entries);
    entries
}

/// Recover the real `(major, minor)` of `major == 0` subvolume mounts (btrfs,
/// ZFS) whose source is a `/dev/` node, by reading `class/block/<name>/dev`.
/// Entries that cannot be resolved keep `major == 0` and are dropped by
/// `device_map`/`container_device_set` downstream.
pub(crate) fn resolve_major_zero(sys: &SysFs, entries: &mut [MountEntry]) {
    for entry in entries.iter_mut().filter(|e| e.major == 0) {
        let Some(name) = entry.source.strip_prefix("/dev/") else {
            continue;
        };
        let rel = format!("class/block/{name}/dev");
        if let Ok(content) = sys.read(&rel)
            && let Some((major, minor)) = parse_dev_pair(&content)
        {
            entry.major = major;
            entry.minor = minor;
        }
    }
}

/// Build one `os_mountinfo` row per parsed mount entry.
///
/// Mount point, fstype, and source strings are interned here. Filesystem
/// capacity is nullable because `statvfs` can fail for pseudo-filesystems or
/// mounts that vanish during collection.
pub(crate) fn collect_mountinfo(
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    entries: &[MountEntry],
) -> Vec<OsMountinfo> {
    let type_id = 1_112_001_u32;
    let started = Instant::now();
    if entries.is_empty() {
        return Vec::new();
    }

    let mut rows = Vec::new();
    for entry in entries {
        let (Some(mount_point), Some(fstype), Some(source)) = (
            intern_str(interner, type_id, "self/mountinfo", &entry.mount_point),
            intern_str(interner, type_id, "self/mountinfo", &entry.fstype),
            intern_str(interner, type_id, "self/mountinfo", &entry.source),
        ) else {
            continue;
        };
        let space = statvfs(&entry.mount_point);
        rows.push(mount_row(
            entry,
            space,
            scope,
            ts,
            mount_point,
            fstype,
            source,
        ));
    }
    log_collection_finish(type_id, "procfs", rows.len(), started.elapsed());
    rows
}

/// Read `/proc/cpuinfo` and build one `os_topology` row per logical CPU.
///
/// On read or parse failure the section is skipped and a `collection_degraded`
/// event is logged; zeros are never fabricated.
fn collect_topology(
    fs: &ProcFs,
    sys: &SysFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
) -> Vec<OsTopology> {
    let type_id = 1_113_001_u32;
    let started = Instant::now();
    let Some(content) = read_optional_os_file(fs, "cpuinfo", type_id) else {
        return Vec::new();
    };
    let mut rows = match cpuinfo::parse(&content) {
        Ok(rows) => rows,
        Err(err) => {
            log_degraded(type_id, "cpuinfo", &err.0);
            return Vec::new();
        }
    };
    for row in &mut rows {
        row.mhz_max = cpu_max_mhz(sys, row.cpu_id);
    }
    let built: Vec<OsTopology> = rows
        .iter()
        .filter_map(|row| {
            let model_name_id = intern_str(interner, type_id, "cpuinfo", &row.model_name)?;
            Some(row.to_section(scope, ts, model_name_id))
        })
        .collect();
    log_collection_finish(type_id, "procfs", built.len(), started.elapsed());
    built
}

const fn os_entity_scope(in_container: bool) -> u8 {
    if in_container {
        OsScope::Container.as_u8()
    } else {
        OsScope::Host.as_u8()
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "process sections share procfs enumeration and degradation counters"
)]
fn collect_process_sections(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    due: &DueSet,
    os: &mut OsSources,
) {
    let hot_due = due.has(SourceKind::OsProcesses);
    let status_due = due.has(SourceKind::OsProcessStatus);
    let mapping_due = due.has(SourceKind::OsCgroupMapping);
    if !hot_due && !status_due && !mapping_due {
        return;
    }

    let hot_type_id = 1_100_001_u32;
    let status_type_id = 1_101_001_u32;
    let mapping_type_id = 1_200_001_u32;
    let started = Instant::now();
    let facts = match process_facts(fs) {
        Ok(facts) => facts,
        Err(err) => {
            for type_id in [hot_type_id, status_type_id, mapping_type_id] {
                if (type_id == hot_type_id && hot_due)
                    || (type_id == status_type_id && status_due)
                    || (type_id == mapping_type_id && mapping_due)
                {
                    log_degraded(type_id, "process", &err);
                }
            }
            return;
        }
    };
    let max_procs = usize::try_from(os_max_procs(hot_type_id)).unwrap_or(usize::MAX);
    let capped = match fs.pid_dirs_capped(max_procs) {
        Ok(capped) => capped,
        Err(err) => {
            for type_id in [hot_type_id, status_type_id, mapping_type_id] {
                if (type_id == hot_type_id && hot_due)
                    || (type_id == status_type_id && status_due)
                    || (type_id == mapping_type_id && mapping_due)
                {
                    log_degraded(type_id, "process", &err);
                }
            }
            return;
        }
    };
    if capped.dropped > 0 {
        for type_id in [hot_type_id, status_type_id, mapping_type_id] {
            if (type_id == hot_type_id && hot_due)
                || (type_id == status_type_id && status_due)
                || (type_id == mapping_type_id && mapping_due)
            {
                log_cap_degraded(type_id, "process", "process_cap", capped.dropped, max_procs);
            }
        }
    }

    let mut skipped = 0_usize;
    let mut io_nulls = 0_usize;
    let mut mapping_nulls = 0_usize;
    for pid in capped.pids {
        let read = match read_process(fs, pid, facts, ts) {
            Ok(read) => read,
            Err(ProcessError::Gone(_)) => continue,
            Err(_) => {
                skipped = skipped.saturating_add(1);
                continue;
            }
        };
        if hot_due {
            if read.hot.io.is_none() {
                io_nulls = io_nulls.saturating_add(1);
            }
            let Some(comm) = intern_str(interner, hot_type_id, "process", &read.hot.comm) else {
                continue;
            };
            let cmdline = read
                .hot
                .cmdline
                .as_deref()
                .and_then(|value| intern_str(interner, hot_type_id, "process", value));
            os.processes
                .push(kronika_source_os::proc::process::to_hot_section(
                    &read.hot, scope, comm, cmdline,
                ));
        }
        if status_due {
            os.process_status
                .push(kronika_source_os::proc::process::to_status_section(
                    &read.status,
                    scope,
                ));
        }
        if mapping_due {
            if let Some(mapping) = read.cgroup {
                if let Some(cgroup_path) = intern_str(
                    interner,
                    mapping_type_id,
                    "process/cgroup",
                    &mapping.cgroup_path,
                ) {
                    os.cgroup_mapping.push(OsCgroupMapping {
                        ts: Ts(mapping.ts),
                        pid: mapping.pid,
                        starttime: Ts(mapping.starttime),
                        cgroup_path,
                        scope,
                    });
                }
            } else {
                mapping_nulls = mapping_nulls.saturating_add(1);
            }
        }
    }

    if skipped > 0 {
        for type_id in [hot_type_id, status_type_id, mapping_type_id] {
            if (type_id == hot_type_id && hot_due)
                || (type_id == status_type_id && status_due)
                || (type_id == mapping_type_id && mapping_due)
            {
                log_count_degraded(type_id, "process", "process_skipped", skipped);
            }
        }
    }
    if hot_due && io_nulls > 0 {
        log_count_degraded(
            hot_type_id,
            "process/io",
            "process_io_unavailable",
            io_nulls,
        );
    }
    if mapping_due && mapping_nulls > 0 {
        log_count_degraded(
            mapping_type_id,
            "process/cgroup",
            "process_cgroup_unavailable",
            mapping_nulls,
        );
    }
    if hot_due {
        log_collection_finish(hot_type_id, "procfs", os.processes.len(), started.elapsed());
    }
    if status_due {
        log_collection_finish(
            status_type_id,
            "procfs",
            os.process_status.len(),
            started.elapsed(),
        );
    }
    if mapping_due {
        log_collection_finish(
            mapping_type_id,
            "procfs",
            os.cgroup_mapping.len(),
            started.elapsed(),
        );
    }
}

fn collect_cgroup_sections(
    sys: &SysFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
    fs: &ProcFs,
    due: &DueSet,
    os: &mut OsSources,
) {
    if !due.has(SourceKind::OsCgroup) {
        return;
    }

    let cpu_type_id = 1_201_001_u32;
    let memory_type_id = 1_202_001_u32;
    let io_type_id = 1_203_001_u32;
    let pids_type_id = 1_204_001_u32;
    let started = Instant::now();
    let clock_ticks = process_facts(fs).map_or_else(
        |err| {
            log_degraded(cpu_type_id, "cgroup", &err);
            0
        },
        |facts| facts.clock_ticks_per_sec,
    );
    let max_cgroups = usize::try_from(os_max_cgroups(cpu_type_id)).unwrap_or(usize::MAX);
    let max_io_rows = usize::try_from(os_max_cgroup_io_rows(io_type_id)).unwrap_or(usize::MAX);
    let max_depth = usize::try_from(os_cgroup_max_depth(cpu_type_id)).unwrap_or(usize::MAX);
    let rows = cgroup::collect(sys, ts, clock_ticks, max_cgroups, max_io_rows, max_depth);
    if rows.dropped_cgroups > 0 {
        for type_id in [cpu_type_id, memory_type_id, io_type_id, pids_type_id] {
            log_cap_degraded(
                type_id,
                "cgroup",
                "cgroup_cap",
                rows.dropped_cgroups,
                max_cgroups,
            );
        }
    }
    if rows.dropped_io_rows > 0 {
        log_cap_degraded(
            io_type_id,
            "cgroup/io",
            "cgroup_io_cap",
            rows.dropped_io_rows,
            max_io_rows,
        );
    }

    for row in &rows.cpu {
        if let Some(cgroup_path) = intern_str(interner, cpu_type_id, "cgroup/cpu", &row.cgroup_path)
        {
            os.cgroup_cpu
                .push(cgroup::to_cpu_section(row, scope, cgroup_path));
        }
    }
    for row in &rows.memory {
        if let Some(cgroup_path) =
            intern_str(interner, memory_type_id, "cgroup/memory", &row.cgroup_path)
        {
            os.cgroup_memory
                .push(cgroup::to_memory_section(row, scope, cgroup_path));
        }
    }
    for row in &rows.io {
        if let Some(cgroup_path) = intern_str(interner, io_type_id, "cgroup/io", &row.cgroup_path) {
            os.cgroup_io
                .push(cgroup::to_io_section(row, scope, cgroup_path));
        }
    }
    for row in &rows.pids {
        if let Some(cgroup_path) =
            intern_str(interner, pids_type_id, "cgroup/pids", &row.cgroup_path)
        {
            os.cgroup_pids
                .push(cgroup::to_pids_section(row, scope, cgroup_path));
        }
    }
    log_collection_finish(
        cpu_type_id,
        "cgroup",
        os.cgroup_cpu.len(),
        started.elapsed(),
    );
    log_collection_finish(
        memory_type_id,
        "cgroup",
        os.cgroup_memory.len(),
        started.elapsed(),
    );
    log_collection_finish(io_type_id, "cgroup", os.cgroup_io.len(), started.elapsed());
    log_collection_finish(
        pids_type_id,
        "cgroup",
        os.cgroup_pids.len(),
        started.elapsed(),
    );
}

pub(crate) fn cpu_max_mhz(sys: &SysFs, cpu_id: i32) -> Option<f64> {
    let rel = format!("devices/system/cpu/cpu{cpu_id}/cpufreq/cpuinfo_max_freq");
    let khz = sys.read(&rel).ok()?.parse::<f64>().ok()?;
    (khz.is_finite() && khz >= 0.0).then_some(khz / 1000.0)
}

/// Intern one OS string, logging degradation and returning `None` on failure so
/// the caller skips only the affected row.
fn intern_str(
    interner: &mut Interner,
    type_id: u32,
    source: &'static str,
    value: &str,
) -> Option<StrId> {
    match interner.intern(value.as_bytes()) {
        Ok(id) => Some(StrId(id.get())),
        Err(err) => {
            log_degraded(type_id, source, &err);
            None
        }
    }
}

/// Emit a `collection_degraded` event with the section identity and reason.
fn log_degraded(type_id: u32, source: &'static str, reason: &dyn std::fmt::Display) {
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", source),
            field("reason", reason),
        ],
    );
}

fn log_cap_degraded(
    type_id: u32,
    source: &'static str,
    reason: &'static str,
    dropped: usize,
    cap: usize,
) {
    log_event(
        LogLevel::Warn,
        "collection_degraded",
        &[
            field("collection", section_name(type_id)),
            field("type_id", type_id),
            field("layout_id", layout_id(type_id)),
            field("source", source),
            field("reason", reason),
            field("dropped", dropped),
            field("cap", cap),
        ],
    );
}

fn os_max_procs(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_MAX_PROCS", 4096)
}

fn os_max_cgroups(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_MAX_CGROUPS", 1024)
}

fn os_max_cgroup_io_rows(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_MAX_CGROUP_IO_ROWS", 4096)
}

fn os_cgroup_max_depth(type_id: u32) -> u64 {
    os_cap_from_env(type_id, "KRONIKA_OS_CGROUP_MAX_DEPTH", 8)
}

fn os_cap_from_env(type_id: u32, key: &'static str, default: u64) -> u64 {
    match env_u64(key, default) {
        Ok(cap) => cap,
        Err(err) => {
            log_event(
                LogLevel::Warn,
                "collection_degraded",
                &[
                    field("collection", section_name(type_id)),
                    field("type_id", type_id),
                    field("layout_id", layout_id(type_id)),
                    field("source", key),
                    field("reason", &err),
                    field("cap", default),
                ],
            );
            default
        }
    }
}

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
    Ok(())
}
