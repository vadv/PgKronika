//! Parse `/proc/self/mountinfo` and attribute block-device I/O to a pod's real
//! backing devices.
//!
//! In a Kubernetes pod, `/proc/diskstats` reports the whole node's per-device
//! counters. Charging all of them to the pod would double-count the node. This
//! module keeps only the devices the pod actually mounts for data, and drops
//! the bind-mounted infrastructure files (`/etc/hosts`, service-account
//! secrets, ...) that share the node's root device but carry no pod I/O.
//!
//! Pure string logic: no filesystem or syscall reads. The `/sys/class/block`
//! resolution of `major == 0` subvolume devices (btrfs, ZFS) is a later step.

use std::collections::{HashMap, HashSet};

use kronika_registry::StrId;
use kronika_registry::Ts;
use kronika_registry::os_mountinfo::OsMountinfo;

use crate::FsSpace;

/// One `/proc/self/mountinfo` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// Device major number (`0` for pseudo/subvolume filesystems).
    pub major: i32,
    /// Device minor number.
    pub minor: i32,
    /// Where the filesystem is mounted (mountinfo field 5).
    pub mount_point: String,
    /// Filesystem type after the ` - ` separator (e.g. `ext4`, `btrfs`).
    pub fstype: String,
    /// Mount source after the ` - ` separator (e.g. `/dev/sda1`).
    pub source: String,
    /// Whether [`mount_point`](Self::mount_point) is a Kubernetes bind-mounted
    /// infrastructure path that shares the node's device but carries no pod I/O.
    pub is_k8s_infra: bool,
}

/// Kubernetes infrastructure mount paths bind-mounted from the node's root
/// disk. In a pod they cause false I/O attribution: `/proc/diskstats` reports
/// the whole node's I/O for the shared device, none of it the pod's.
const K8S_INFRA_MOUNTS: &[&str] = &[
    "/etc/hosts",
    "/etc/hostname",
    "/etc/resolv.conf",
    "/dev/termination-log",
    "/run/secrets/",
    "/var/run/secrets/",
];

/// Returns `true` if `path` is a Kubernetes infrastructure bind-mount: an exact
/// match or a prefix match against the known infrastructure paths.
#[must_use]
pub fn is_k8s_infra_mount(path: &str) -> bool {
    K8S_INFRA_MOUNTS
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(prefix))
}

/// Parse every `/proc/self/mountinfo` line into a [`MountEntry`].
///
/// Keeps `major == 0` entries; `is_k8s_infra` is computed per mount point.
/// Lines without the ` - ` separator or a required field are skipped.
#[must_use]
pub fn parse_mountinfo(content: &str) -> Vec<MountEntry> {
    let mut entries = Vec::new();

    for line in content.lines() {
        // The ` - ` separator divides the fixed head plus optional fields from
        // the tail `fstype source superopts`.
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };

        let head_fields: Vec<&str> = head.split_whitespace().collect();
        // Head layout: mount_id parent_id major:minor root mount_point ...
        let (Some(dev), Some(mount_point)) = (head_fields.get(2), head_fields.get(4)) else {
            continue;
        };

        let Some((major_s, minor_s)) = dev.split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major_s.parse::<i32>(), minor_s.parse::<i32>()) else {
            continue;
        };

        let mut tail_fields = tail.split_whitespace();
        let (Some(fstype), Some(source)) = (tail_fields.next(), tail_fields.next()) else {
            continue;
        };

        entries.push(MountEntry {
            major,
            minor,
            is_k8s_infra: is_k8s_infra_mount(mount_point),
            mount_point: (*mount_point).to_owned(),
            fstype: fstype.to_owned(),
            source: source.to_owned(),
        });
    }

    entries
}

/// Maps `(major, minor)` to its mount points, dropping `major == 0` entries.
#[must_use]
pub fn device_map(entries: &[MountEntry]) -> HashMap<(i32, i32), Vec<String>> {
    let mut map: HashMap<(i32, i32), Vec<String>> = HashMap::new();
    for entry in entries {
        if entry.major == 0 {
            continue;
        }
        map.entry((entry.major, entry.minor))
            .or_default()
            .push(entry.mount_point.clone());
    }
    map
}

/// Real backing devices a pod should be charged for: `(major, minor)` where
/// `major != 0` and at least one mount point is not Kubernetes infrastructure.
#[must_use]
pub fn container_device_set(entries: &[MountEntry]) -> HashSet<(i32, i32)> {
    entries
        .iter()
        .filter(|entry| entry.major != 0 && !entry.is_k8s_infra)
        .map(|entry| (entry.major, entry.minor))
        .collect()
}

/// Picks the path to display for a device: the shortest non-infrastructure
/// path, or the shortest overall when every path is infrastructure.
#[must_use]
pub fn display_path(paths: &[String]) -> Option<&str> {
    paths
        .iter()
        .filter(|p| !is_k8s_infra_mount(p))
        .min_by_key(|p| p.len())
        .or_else(|| paths.iter().min_by_key(|p| p.len()))
        .map(String::as_str)
}

/// Build a registry row for `1_112_001` from a parsed mount entry and optional
/// capacity snapshot.
///
/// The caller interns `entry.mount_point`, `entry.fstype`, and `entry.source`
/// and passes the resulting [`StrId`]s. `space` is `None` when `statvfs` failed.
#[must_use]
pub fn mount_row(
    entry: &MountEntry,
    space: Option<FsSpace>,
    scope: u8,
    ts: i64,
    mount_point_id: StrId,
    fstype_id: StrId,
    source_id: StrId,
) -> OsMountinfo {
    OsMountinfo {
        ts: Ts(ts),
        major: entry.major,
        minor: entry.minor,
        mount_point: mount_point_id,
        fstype: fstype_id,
        source: source_id,
        is_k8s_infra: entry.is_k8s_infra,
        total_bytes: space.map(|s| s.total_bytes),
        free_bytes: space.map(|s| s.free_bytes),
        scope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries_and_flags_k8s_infra() {
        let c = "\
30 25 8:1 / /data rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
31 25 8:1 /sub /var/lib/postgresql rw shared:2 - ext4 /dev/sda1 rw\n\
40 25 0:35 / /etc/hosts rw - tmpfs tmpfs rw\n";
        let e = parse_mountinfo(c);
        assert_eq!(e.len(), 3);
        assert_eq!(
            (e[0].major, e[0].minor, e[0].mount_point.as_str()),
            (8, 1, "/data")
        );
        assert_eq!(e[0].fstype, "ext4");
        assert_eq!(e[0].source, "/dev/sda1");
        assert!(!e[0].is_k8s_infra);
        assert!(e[2].is_k8s_infra); // /etc/hosts
    }

    #[test]
    fn container_set_excludes_infra_only_devices_and_picks_short_path() {
        let c = "\
30 25 8:1 / /data rw - ext4 /dev/sda1 rw\n\
31 25 8:1 / /data/postgres/pgdata rw - ext4 /dev/sda1 rw\n\
40 25 253:0 / /etc/hosts rw - ext4 /dev/dm-0 rw\n";
        let e = parse_mountinfo(c);
        let set = container_device_set(&e);
        assert!(set.contains(&(8, 1))); // has non-infra mount /data
        assert!(!set.contains(&(253, 0))); // only /etc/hosts -> excluded
        let map = device_map(&e);
        assert_eq!(display_path(&map[&(8, 1)]), Some("/data")); // shortest
    }

    #[test]
    fn skips_line_without_separator() {
        let c = "30 25 8:1 / /data rw,relatime shared:1 ext4 /dev/sda1 rw\n";
        let e = parse_mountinfo(c);
        assert!(e.is_empty());
    }

    #[test]
    fn device_map_drops_major_zero_but_parse_keeps_it() {
        let c = "40 25 0:35 / /etc/hosts rw - tmpfs tmpfs rw\n";
        let e = parse_mountinfo(c);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].major, e[0].minor), (0, 35));
        let map = device_map(&e);
        assert!(!map.contains_key(&(0, 35)));
    }

    #[test]
    fn container_set_excludes_all_infra_device() {
        let c = "\
40 25 253:0 / /etc/hosts rw - ext4 /dev/dm-0 rw\n\
41 25 253:0 / /run/secrets/token rw - ext4 /dev/dm-0 rw\n";
        let e = parse_mountinfo(c);
        let set = container_device_set(&e);
        assert!(!set.contains(&(253, 0)));
        assert!(set.is_empty());
    }

    #[test]
    fn display_path_falls_back_to_shortest_when_all_infra() {
        let paths = vec![
            "/run/secrets/kubernetes.io/serviceaccount".to_owned(),
            "/etc/hosts".to_owned(),
        ];
        assert_eq!(display_path(&paths), Some("/etc/hosts"));
    }
}
