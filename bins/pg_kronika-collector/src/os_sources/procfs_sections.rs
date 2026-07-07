use super::{
    Instant, Interner, LogLevel, MountEntry, OsDiskstats, OsMountinfo, OsNetdev, OsSources,
    OsTopology, ProcFs, SysFs, container_device_set, cpuinfo, diskstats, env_u64, field,
    intern_str, layout_id, log_collection_finish, log_degraded, log_event, mount_row, net_dev,
    net_netstat, net_snmp, parse_dev_pair, parse_mountinfo, read_optional_os_file, section_name,
    statvfs,
};

/// Read and parse `/proc/diskstats`, interning device names into rows.
///
/// Inside a container the pod's real backing devices are the only ones charged
/// to it: `/proc/diskstats` reports the whole node, so rows are filtered to the
/// mountinfo-derived device set. Over `KRONIKA_OS_MAX_DISKS` rows the lowest
/// `(major, minor)` devices are kept and the overflow is logged, not dropped
/// silently.
pub(super) fn collect_diskstats(
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
pub(super) fn collect_netdev(
    fs: &ProcFs,
    interner: &mut Interner,
    scope: u8,
    ts: i64,
) -> Vec<OsNetdev> {
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
pub(super) fn collect_net_singletons(fs: &ProcFs, scope: u8, ts: i64, os: &mut OsSources) {
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
pub(super) fn mountinfo_entries(fs: &ProcFs) -> Vec<MountEntry> {
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
pub(super) fn collect_topology(
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

pub(crate) fn cpu_max_mhz(sys: &SysFs, cpu_id: i32) -> Option<f64> {
    let rel = format!("devices/system/cpu/cpu{cpu_id}/cpufreq/cpuinfo_max_freq");
    let khz = sys.read(&rel).ok()?.parse::<f64>().ok()?;
    (khz.is_finite() && khz >= 0.0).then_some(khz / 1000.0)
}
