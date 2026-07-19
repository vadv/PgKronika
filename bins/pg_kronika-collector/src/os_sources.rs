use crate::buffering::buffer_row;
use crate::config::env_u64;
use crate::logging::{
    LogLevel, field, layout_id, log_collection_finish, log_count_degraded, log_event, section_name,
};
use crate::scheduler::{DueSet, SourceKind};
use anyhow::Result;
use kronika_registry::incident_gauges::{PgProcessCgroupMemoryV1, PgStorageMountV1};
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
    Hash128, JoinFailure, collect_process_cgroup_memory, map_postgresql_storage,
};
use kronika_source_os::{
    MountEntry, OsScope, ProcFs, SysFs, cgroup, container_device_set, mount_row, net_scope,
    parse_dev_pair, parse_mountinfo, statvfs,
};
use kronika_source_pg::incident_gauges::LocalJoinFacts;
use kronika_writer::{Interner, SectionBuffers};
use std::io::ErrorKind;
use std::path::Path;
use std::time::Instant;

mod buffering;
mod cgroups;
mod process;
mod procfs_sections;

pub(crate) use buffering::push_os_sources;
pub(crate) use procfs_sections::collect_mountinfo;
#[cfg(test)]
pub(crate) use procfs_sections::{cap_disks, cpu_max_mhz, resolve_major_zero};

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
    mount_entries: Vec<MountEntry>,
    pg_storage_mounts: Vec<PgStorageMountV1>,
    pg_process_cgroup_memory: Option<PgProcessCgroupMemoryV1>,
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
            mount_entries: Vec::new(),
            pg_storage_mounts: Vec::new(),
            pg_process_cgroup_memory: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn diskstats_empty(&self) -> bool {
        self.diskstats.is_empty()
    }

    #[cfg(test)]
    pub(crate) const fn mountinfo_empty(&self) -> bool {
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
    let mounts = procfs_sections::mountinfo_entries(fs);
    os.mount_entries.clone_from(&mounts);

    if due.has(SourceKind::OsCore) {
        // Counters: disk and network. Network sections carry the pod's
        // network-namespace scope inside a container, not the host scope.
        let net_scope_id = net_scope(fs).as_u8();
        os.diskstats =
            procfs_sections::collect_diskstats(fs, interner, scope, ts, in_container, &mounts);
        os.netdev = procfs_sections::collect_netdev(fs, interner, net_scope_id, ts);
        procfs_sections::collect_net_singletons(fs, net_scope_id, ts, &mut os);
    }

    if due.has(SourceKind::OsMountTopo) {
        os.mountinfo = collect_mountinfo(interner, scope, ts, &mounts);
        os.topology =
            procfs_sections::collect_topology(fs, &SysFs::from_env(), interner, scope, ts);
    }

    let entity_scope = os_entity_scope(in_container);
    process::collect_process_sections(fs, interner, entity_scope, ts, due, &mut os);
    cgroups::collect_cgroup_sections(
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

const fn empty_hash() -> Hash128 {
    Hash128 { hi: 0, lo: 0 }
}

/// Build bounded cross-source rows only from the `PostgreSQL` PID supplied by
/// the same collection tick. Raw paths never leave this orchestration step.
pub(crate) fn collect_pg_os_joins(fs: &ProcFs, facts: Option<&LocalJoinFacts>, os: &mut OsSources) {
    let Some(facts) = facts else {
        return;
    };
    let sys = SysFs::from_env();
    let process =
        collect_process_cgroup_memory(fs, &sys, facts.backend_pid, facts.backend_start, facts.ts);
    let local_identity_verified = process.is_ok();
    let process_hash = process
        .as_ref()
        .map_or_else(|_| empty_hash(), |row| row.process_hash);
    os.pg_process_cgroup_memory = Some(match process {
        Ok(row) => PgProcessCgroupMemoryV1 {
            ts: Ts(facts.ts),
            process_hash_hi: row.process_hash.hi,
            process_hash_lo: row.process_hash.lo,
            cgroup_hash_hi: row.cgroup_hash.hi,
            cgroup_hash_lo: row.cgroup_hash.lo,
            hierarchy: row.hierarchy,
            mapping_state: 1,
            current_bytes: Some(row.memory.current),
            max_bytes: row.memory.max,
            max_unlimited: row.max_unlimited,
        },
        Err(reason) => PgProcessCgroupMemoryV1 {
            ts: Ts(facts.ts),
            process_hash_hi: process_hash.hi,
            process_hash_lo: process_hash.lo,
            cgroup_hash_hi: 0,
            cgroup_hash_lo: 0,
            hierarchy: 0,
            mapping_state: reason.code(),
            current_bytes: None,
            max_bytes: None,
            max_unlimited: false,
        },
    });

    let storage = if local_identity_verified && facts.tablespaces_complete {
        map_postgresql_storage(
            fs,
            Path::new(&facts.data_directory),
            &facts.tablespaces,
            &os.mount_entries,
            258,
        )
    } else {
        Err(JoinFailure::ProcUnavailable)
    };
    match storage {
        Ok(rows) => {
            os.pg_storage_mounts = rows
                .into_iter()
                .map(|row| PgStorageMountV1 {
                    ts: Ts(facts.ts),
                    role: row.role,
                    path_hash_hi: row.path_hash.hi,
                    path_hash_lo: row.path_hash.lo,
                    mount_hash_hi: row.mount_hash.hi,
                    mount_hash_lo: row.mount_hash.lo,
                    mount_namespace: row.mount_namespace,
                    mapping_state: row.mapping_state,
                    total_bytes: row.total_bytes,
                    available_bytes: row.available_bytes,
                })
                .collect();
        }
        Err(reason) => os.pg_storage_mounts.push(PgStorageMountV1 {
            ts: Ts(facts.ts),
            role: 0,
            path_hash_hi: 0,
            path_hash_lo: 0,
            mount_hash_hi: 0,
            mount_hash_lo: 0,
            mount_namespace: 0,
            mapping_state: reason.code(),
            total_bytes: None,
            available_bytes: None,
        }),
    }
}

const fn os_entity_scope(in_container: bool) -> u8 {
    if in_container {
        OsScope::Container.as_u8()
    } else {
        OsScope::Host.as_u8()
    }
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
