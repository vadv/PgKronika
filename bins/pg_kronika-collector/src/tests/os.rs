use crate::os_sources::{
    cap_disks, collect_mountinfo, collect_os_sources, cpu_max_mhz, resolve_major_zero,
};
use crate::scheduler::{DueSet, SourceKind};
use crate::source_contracts::activity_dict_limits;
use kronika_source_os::proc::diskstats;
use kronika_source_os::{MountEntry, ProcFs, SysFs};
use kronika_writer::Interner;

fn disk_row(major: i32, minor: i32) -> diskstats::DiskstatsRow {
    let line = format!("{major} {minor} dev{minor} 1 0 8 2 3 0 24 4 0 6 6\n");
    diskstats::parse(&line)
        .expect("valid diskstats line")
        .remove(0)
}

fn mount_entry(major: i32, minor: i32, source: &str) -> MountEntry {
    MountEntry {
        mount_id: minor,
        parent_id: 1,
        major,
        minor,
        root: "/".to_owned(),
        mount_point: "/data".to_owned(),
        fstype: "btrfs".to_owned(),
        source: source.to_owned(),
        deleted: false,
        is_k8s_infra: false,
    }
}

#[test]
fn cap_disks_keeps_lowest_devices_and_reports_drop() {
    let mut rows = vec![disk_row(8, 5), disk_row(8, 0), disk_row(259, 0)];
    let dropped = cap_disks(&mut rows, 2);
    assert_eq!(dropped, 1);
    // Kept devices are the two lowest (major, minor) pairs.
    assert_eq!(
        rows.iter().map(|r| (r.major, r.minor)).collect::<Vec<_>>(),
        vec![(8, 0), (8, 5)]
    );
}

#[test]
fn cap_disks_is_a_noop_within_the_cap() {
    let mut rows = vec![disk_row(8, 0), disk_row(8, 1)];
    assert_eq!(cap_disks(&mut rows, 2), 0);
    assert_eq!(cap_disks(&mut rows, 5), 0);
    assert_eq!(rows.len(), 2);
}

#[test]
fn resolve_major_zero_rewrites_dev_backed_subvolumes() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("class/block/nvme0n1p2")).expect("mkdir");
    std::fs::write(dir.path().join("class/block/nvme0n1p2/dev"), "259:2\n").expect("write");
    let sys = SysFs::new(dir.path().to_path_buf());

    let mut entries = vec![
        mount_entry(0, 42, "/dev/nvme0n1p2"), // resolvable btrfs subvolume
        mount_entry(0, 43, "tmpfs"),          // no /dev/ source: unchanged
        mount_entry(8, 1, "/dev/sda1"),       // already real: unchanged
    ];
    resolve_major_zero(&sys, &mut entries);

    assert_eq!((entries[0].major, entries[0].minor), (259, 2));
    assert_eq!((entries[1].major, entries[1].minor), (0, 43));
    assert_eq!((entries[2].major, entries[2].minor), (8, 1));
}

#[test]
fn resolve_major_zero_leaves_entry_when_sysfs_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sys = SysFs::new(dir.path().to_path_buf());
    let mut entries = vec![mount_entry(0, 42, "/dev/nvme0n1p2")];
    resolve_major_zero(&sys, &mut entries);
    // Unresolvable major==0 stays 0 and is dropped downstream by device_map.
    assert_eq!((entries[0].major, entries[0].minor), (0, 42));
}

#[test]
fn collect_mountinfo_emits_every_mount_entry() {
    let entries = vec![
        MountEntry {
            mount_id: 10,
            parent_id: 1,
            major: 8,
            minor: 1,
            root: "/".to_owned(),
            mount_point: "/data".to_owned(),
            fstype: "ext4".to_owned(),
            source: "/dev/sda1".to_owned(),
            deleted: false,
            is_k8s_infra: false,
        },
        MountEntry {
            mount_id: 11,
            parent_id: 10,
            major: 8,
            minor: 1,
            root: "/".to_owned(),
            mount_point: "/data/pg wal".to_owned(),
            fstype: "ext4".to_owned(),
            source: "/dev/sda1".to_owned(),
            deleted: false,
            is_k8s_infra: false,
        },
    ];
    let mut interner = Interner::new(activity_dict_limits());
    let rows = collect_mountinfo(&mut interner, 0, 1_000_000, &entries);

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().map(|r| (r.major, r.minor)).collect::<Vec<_>>(),
        vec![(8, 1), (8, 1)]
    );
    assert_ne!(rows[0].mount_point, rows[1].mount_point);
}

#[test]
fn cpu_max_mhz_reads_sysfs_khz() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rel = "devices/system/cpu/cpu0/cpufreq";
    std::fs::create_dir_all(dir.path().join(rel)).expect("mkdir");
    std::fs::write(dir.path().join(rel).join("cpuinfo_max_freq"), "3600000\n").expect("write");
    let sys = SysFs::new(dir.path().to_path_buf());

    assert_eq!(cpu_max_mhz(&sys, 0), Some(3600.0));
    assert_eq!(cpu_max_mhz(&sys, 1), None);
}
// Verify that diskstats rows are not emitted on an OsMountTopo-only tick.
#[test]
fn collect_os_sources_no_diskstats_on_mount_topo_only_tick() {
    let dir = tempfile::tempdir().expect("tempdir");
    let proc_root = dir.path();

    // diskstats: one device (8:1)
    let diskstats_line = "8 1 sda1 1 0 8 2 3 0 24 4 0 6 6\n";
    std::fs::write(proc_root.join("diskstats"), diskstats_line).expect("write diskstats");

    // self/mountinfo: sda1 mounted at /data
    std::fs::create_dir_all(proc_root.join("self")).expect("mkdir self");
    let mountinfo_line = "30 25 8:1 / /data rw - ext4 /dev/sda1 rw\n";
    std::fs::write(proc_root.join("self/mountinfo"), mountinfo_line).expect("write mountinfo");

    let fs = ProcFs::new(proc_root.to_path_buf());
    let mut interner = Interner::new(activity_dict_limits());
    let due = DueSet::for_test(vec![SourceKind::OsMountTopo]);

    let os = collect_os_sources(&fs, &mut interner, 0, 0, false, &due);

    assert!(
        os.diskstats_empty(),
        "diskstats must not be emitted on an OsMountTopo-only tick"
    );
    assert!(!os.mountinfo_empty(), "mountinfo rows must still be built");
}
