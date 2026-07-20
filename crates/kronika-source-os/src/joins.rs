//! Bounded co-located PostgreSQL-to-Linux joins.
#![allow(
    missing_docs,
    reason = "stable numeric failure codes are documented by the durable registry contract"
)]
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::cgroup::{CgroupMemoryRow, read_memory_path};
use crate::proc::process::{ProcessFacts, parse_stat};
use crate::{MountEntry, ProcFs, SysFs, statvfs};

/// Fixed-width redacted identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hash128 {
    /// High 64 bits.
    pub hi: u64,
    /// Low 64 bits.
    pub lo: u64,
}

/// Typed absence or provenance failure for a co-located join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JoinFailure {
    ProcUnavailable = 2,
    PidMismatch = 3,
    StartMismatch = 4,
    MembershipUnavailable = 5,
    ProcessMigrated = 6,
    CgroupUnavailable = 7,
    NamespaceUnavailable = 8,
    PathUnavailable = 9,
    MountUnavailable = 10,
    CapacityUnavailable = 11,
}

impl JoinFailure {
    /// Stable numeric state written to durable rows.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Membership {
    path: String,
    unified_v2: bool,
}

/// A cgroup memory reading proven to contain the sampled process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCgroupMemory {
    /// Redacted `(pid,starttime)` identity.
    pub process_hash: Hash128,
    /// Redacted cgroup identity.
    pub cgroup_hash: Hash128,
    /// `1=v1`, `2=v2`.
    pub hierarchy: u8,
    /// Mount namespace inode shared with the collector for storage joins.
    pub mount_namespace: u64,
    /// Direct memory-controller reading.
    pub memory: CgroupMemoryRow,
    /// Whether the controller reports no finite maximum.
    pub max_unlimited: bool,
}

/// A redacted `PostgreSQL` storage path mapped to one local mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageMount {
    /// `1=data`, `2=wal`, `3=tablespace`.
    pub role: u8,
    /// Hash of role, role key, and resolved path.
    pub path_hash: Hash128,
    /// Hash of namespace and mount identity.
    pub mount_hash: Hash128,
    /// Mount namespace inode.
    pub mount_namespace: u64,
    /// `1=mapped`, otherwise a [`JoinFailure`] code.
    pub mapping_state: u8,
    /// Total filesystem capacity.
    pub total_bytes: Option<i64>,
    /// Bytes available to an unprivileged writer (`f_bavail`).
    pub available_bytes: Option<i64>,
    /// Local block-device major number when the mount maps to one.
    pub major: Option<i64>,
    /// Local block-device minor number when the mount maps to one.
    pub minor: Option<i64>,
    /// `true` only for a nonzero local block-device pair.
    pub block_device_exact: bool,
}

fn hash128(parts: &[&[u8]]) -> Hash128 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    let mut hi = [0_u8; 8];
    let mut lo = [0_u8; 8];
    hi.copy_from_slice(&digest[..8]);
    lo.copy_from_slice(&digest[8..16]);
    Hash128 {
        hi: u64::from_be_bytes(hi),
        lo: u64::from_be_bytes(lo),
    }
}

fn starttime_us(facts: ProcessFacts, ticks: i64) -> Option<i64> {
    if facts.clock_ticks_per_sec <= 0 || ticks < 0 {
        return None;
    }
    let whole = ticks.checked_div(facts.clock_ticks_per_sec)?;
    let remainder = ticks.checked_rem(facts.clock_ticks_per_sec)?;
    facts
        .btime_usec
        .checked_add(whole.checked_mul(1_000_000)?)?
        .checked_add(
            remainder
                .checked_mul(1_000_000)?
                .checked_div(facts.clock_ticks_per_sec)?,
        )
}

fn memory_membership(content: &str) -> Option<Membership> {
    for line in content.lines() {
        let mut fields = line.splitn(3, ':');
        let _hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let raw_path = fields.next()?.trim();
        if !controllers.is_empty() && !controllers.split(',').any(|item| item == "memory") {
            continue;
        }
        let path = if raw_path.is_empty() { "/" } else { raw_path };
        let parsed = Path::new(path);
        if !parsed.is_absolute()
            || parsed
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            return None;
        }
        return Some(Membership {
            path: path.to_owned(),
            unified_v2: controllers.is_empty(),
        });
    }
    None
}

fn process_snapshot(
    procfs: &ProcFs,
    pid: i32,
    facts: ProcessFacts,
) -> Result<(i64, Membership, u64), JoinFailure> {
    let stat_content = procfs
        .read_raw(&format!("{pid}/stat"))
        .map_err(|_error| JoinFailure::ProcUnavailable)?;
    let parsed = parse_stat(&stat_content).map_err(|_error| JoinFailure::ProcUnavailable)?;
    if parsed.pid != pid {
        return Err(JoinFailure::PidMismatch);
    }
    let start_us = starttime_us(facts, parsed.starttime_ticks).ok_or(JoinFailure::StartMismatch)?;
    let cgroup = procfs
        .read_raw(&format!("{pid}/cgroup"))
        .map_err(|_error| JoinFailure::MembershipUnavailable)?;
    let membership = memory_membership(&cgroup).ok_or(JoinFailure::MembershipUnavailable)?;
    let mount_namespace = std::fs::metadata(
        procfs
            .path(&format!("{pid}/ns/mnt"))
            .map_err(|_error| JoinFailure::NamespaceUnavailable)?,
    )
    .map_err(|_error| JoinFailure::NamespaceUnavailable)?
    .ino();
    Ok((start_us, membership, mount_namespace))
}

fn validate_stable_process(
    before_start: i64,
    before_membership: &Membership,
    before_mount_namespace: u64,
    after_start: i64,
    after_membership: &Membership,
    after_mount_namespace: u64,
) -> Result<(), JoinFailure> {
    if before_start != after_start
        || before_membership != after_membership
        || before_mount_namespace != after_mount_namespace
    {
        Err(JoinFailure::ProcessMigrated)
    } else {
        Ok(())
    }
}

/// Read one validated `PostgreSQL` PID and its memory cgroup without `/proc` scans.
///
/// # Errors
/// Returns a stable [`JoinFailure`] when process identity, membership, namespace,
/// or the bounded memory-controller reading cannot be proven.
pub fn collect_process_cgroup_memory(
    procfs: &ProcFs,
    sysfs: &SysFs,
    pid: i32,
    expected_start_us: i64,
    ts: i64,
) -> Result<ProcessCgroupMemory, JoinFailure> {
    let facts = crate::proc::process::process_facts(procfs)
        .map_err(|_error| JoinFailure::ProcUnavailable)?;
    let (start_before, membership_before, mount_namespace_before) =
        process_snapshot(procfs, pid, facts)?;
    let tick_us = 1_000_000_i64
        .checked_add(facts.clock_ticks_per_sec - 1)
        .and_then(|value| value.checked_div(facts.clock_ticks_per_sec))
        .ok_or(JoinFailure::StartMismatch)?;
    if start_before.abs_diff(expected_start_us) > u64::try_from(tick_us).unwrap_or(u64::MAX) {
        return Err(JoinFailure::StartMismatch);
    }
    let (memory, max_unlimited) = read_memory_path(
        sysfs,
        ts,
        &membership_before.path,
        membership_before.unified_v2,
    )
    .ok_or(JoinFailure::CgroupUnavailable)?;
    let (start_after, membership_after, mount_namespace_after) =
        process_snapshot(procfs, pid, facts)?;
    validate_stable_process(
        start_before,
        &membership_before,
        mount_namespace_before,
        start_after,
        &membership_after,
        mount_namespace_after,
    )?;
    let pid_bytes = pid.to_be_bytes();
    let start_bytes = start_before.to_be_bytes();
    Ok(ProcessCgroupMemory {
        process_hash: hash128(&[&pid_bytes, &start_bytes]),
        cgroup_hash: hash128(&[
            &[u8::from(membership_before.unified_v2)],
            membership_before.path.as_bytes(),
        ]),
        hierarchy: if membership_before.unified_v2 { 2 } else { 1 },
        mount_namespace: mount_namespace_before,
        memory,
        max_unlimited,
    })
}

fn mount_namespace(metadata: &Metadata) -> u64 {
    metadata.ino()
}

fn mount_for_path<'a>(path: &Path, mounts: &'a [MountEntry]) -> Option<&'a MountEntry> {
    mounts
        .iter()
        .filter(|mount| !mount.deleted && path.starts_with(Path::new(&mount.mount_point)))
        .max_by_key(|mount| Path::new(&mount.mount_point).components().count())
}

fn storage_mount(
    role: u8,
    role_key: u32,
    raw_path: &Path,
    mounts: &[MountEntry],
    namespace: u64,
) -> StorageMount {
    let role_bytes = [role];
    let role_key_bytes = role_key.to_be_bytes();
    let fallback_hash = hash128(&[
        &role_bytes,
        &role_key_bytes,
        raw_path.as_os_str().as_encoded_bytes(),
    ]);
    let failure = |reason: JoinFailure| StorageMount {
        role,
        path_hash: fallback_hash,
        mount_hash: Hash128 { hi: 0, lo: 0 },
        mount_namespace: namespace,
        mapping_state: reason.code(),
        total_bytes: None,
        available_bytes: None,
        major: None,
        minor: None,
        block_device_exact: false,
    };
    if !raw_path.is_absolute() {
        return failure(JoinFailure::PathUnavailable);
    }
    let Ok(resolved) = std::fs::canonicalize(raw_path) else {
        return failure(JoinFailure::PathUnavailable);
    };
    let Some(mount) = mount_for_path(&resolved, mounts) else {
        return failure(JoinFailure::MountUnavailable);
    };
    let namespace_bytes = namespace.to_be_bytes();
    let mount_id = mount.mount_id.to_be_bytes();
    let parent_id = mount.parent_id.to_be_bytes();
    let major = mount.major.to_be_bytes();
    let minor = mount.minor.to_be_bytes();
    let path_hash = hash128(&[
        &role_bytes,
        &role_key_bytes,
        resolved.as_os_str().as_encoded_bytes(),
    ]);
    let mount_hash = hash128(&[
        &namespace_bytes,
        &mount_id,
        &parent_id,
        &major,
        &minor,
        mount.root.as_bytes(),
        mount.mount_point.as_bytes(),
    ]);
    let block_device_exact = mount.major > 0 && mount.source.starts_with("/dev/");
    let Some(space) = statvfs(&mount.mount_point) else {
        return StorageMount {
            role,
            path_hash,
            mount_hash,
            mount_namespace: namespace,
            mapping_state: JoinFailure::CapacityUnavailable.code(),
            total_bytes: None,
            available_bytes: None,
            major: block_device_exact.then_some(i64::from(mount.major)),
            minor: block_device_exact.then_some(i64::from(mount.minor)),
            block_device_exact,
        };
    };
    StorageMount {
        role,
        path_hash,
        mount_hash,
        mount_namespace: namespace,
        mapping_state: 1,
        total_bytes: Some(space.total_bytes),
        available_bytes: Some(space.free_bytes),
        major: block_device_exact.then_some(i64::from(mount.major)),
        minor: block_device_exact.then_some(i64::from(mount.minor)),
        block_device_exact,
    }
}

/// Resolve data, WAL, and bounded tablespace paths to local mounts.
///
/// # Errors
/// Returns a stable [`JoinFailure`] when namespace provenance is unavailable,
/// the namespace differs from the validated process, or `max_paths` is exceeded.
pub fn map_postgresql_storage(
    procfs: &ProcFs,
    expected_mount_namespace: u64,
    data_directory: &Path,
    tablespaces: &[(u32, String)],
    mounts: &[MountEntry],
    max_paths: usize,
) -> Result<Vec<StorageMount>, JoinFailure> {
    let metadata = std::fs::metadata(
        procfs
            .path("self/ns/mnt")
            .map_err(|_error| JoinFailure::NamespaceUnavailable)?,
    )
    .map_err(|_error| JoinFailure::NamespaceUnavailable)?;
    let namespace = mount_namespace(&metadata);
    if namespace != expected_mount_namespace {
        return Err(JoinFailure::NamespaceUnavailable);
    }
    let required = tablespaces
        .len()
        .checked_add(2)
        .ok_or(JoinFailure::PathUnavailable)?;
    if required > max_paths {
        return Err(JoinFailure::PathUnavailable);
    }
    let mut mapped = Vec::with_capacity(required);
    mapped.push(storage_mount(1, 0, data_directory, mounts, namespace));
    mapped.push(storage_mount(
        2,
        0,
        &data_directory.join("pg_wal"),
        mounts,
        namespace,
    ));
    for (oid, path) in tablespaces {
        mapped.push(storage_mount(3, *oid, Path::new(path), mounts, namespace));
    }
    mapped.sort_by_key(|row| (row.role, row.path_hash));
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn mount(id: i32, point: &str) -> MountEntry {
        MountEntry {
            mount_id: id,
            parent_id: 1,
            major: 8,
            minor: id,
            root: "/".to_owned(),
            mount_point: point.to_owned(),
            fstype: "ext4".to_owned(),
            source: "/dev/test".to_owned(),
            deleted: false,
            is_k8s_infra: false,
        }
    }

    #[test]
    fn longest_prefix_is_component_aware() {
        let mounts = [
            mount(1, "/data"),
            mount(2, "/data/pg"),
            mount(3, "/database"),
        ];
        assert_eq!(
            mount_for_path(Path::new("/data/pg/base"), &mounts).map(|m| m.mount_id),
            Some(2)
        );
        assert_eq!(
            mount_for_path(Path::new("/database/x"), &mounts).map(|m| m.mount_id),
            Some(3)
        );
        assert_eq!(mount_for_path(Path::new("/data2"), &mounts), None);
    }

    #[test]
    fn memory_membership_prefers_the_memory_controller() {
        assert_eq!(
            memory_membership("2:cpu:/a\n5:memory:/db\n"),
            Some(Membership {
                path: "/db".to_owned(),
                unified_v2: false
            })
        );
        assert_eq!(
            memory_membership("0::/slice/db\n"),
            Some(Membership {
                path: "/slice/db".to_owned(),
                unified_v2: true
            })
        );
    }

    #[test]
    fn hashes_do_not_embed_raw_paths() {
        let hash = hash128(&[b"/secret/postgresql/data"]);
        let rendered = format!("{hash:?}");
        assert!(!rendered.contains("secret"));
        assert_eq!(rendered, format!("{hash:?}"));
    }

    fn stat_line(pid: i32, ticks: i64) -> String {
        format!(
            "{pid} (postgres) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 -5 16 17 {ticks} 204800 12 21 22 23 24 25 26 27 28 29 30 31 32 33 15 2 7 8 9 10 11 12 13 14 15"
        )
    }

    fn fixture_roots(cgroup: &str) -> (tempfile::TempDir, ProcFs, SysFs, i64) {
        let root = tempdir().expect("fixture root");
        let proc_root = root.path().join("proc");
        let sys_root = root.path().join("sys");
        fs::create_dir_all(proc_root.join("42/ns")).expect("proc pid");
        fs::create_dir_all(proc_root.join("self/ns")).expect("collector proc");
        fs::write(proc_root.join("mount-namespace"), "fixture").expect("mount namespace");
        fs::hard_link(
            proc_root.join("mount-namespace"),
            proc_root.join("42/ns/mnt"),
        )
        .expect("process mount namespace");
        fs::hard_link(
            proc_root.join("mount-namespace"),
            proc_root.join("self/ns/mnt"),
        )
        .expect("collector mount namespace");
        fs::create_dir_all(sys_root.join("fs/cgroup/db")).expect("cgroup path");
        fs::write(proc_root.join("stat"), "cpu 1 2 3\nbtime 1000\n").expect("proc stat");
        fs::write(proc_root.join("42/stat"), stat_line(42, 190)).expect("pid stat");
        fs::write(proc_root.join("42/cgroup"), cgroup).expect("pid cgroup");
        let procfs = ProcFs::new(proc_root);
        let sysfs = SysFs::new(sys_root);
        let facts = crate::proc::process::process_facts(&procfs).expect("process facts");
        let start = starttime_us(facts, 190).expect("start time");
        (root, procfs, sysfs, start)
    }

    #[test]
    fn reads_one_v2_process_membership_and_literal_unlimited() {
        let (_root, procfs, sysfs, start) = fixture_roots("0::/db\n");
        fs::write(
            sysfs.path("fs/cgroup/db/memory.current").expect("path"),
            "90\n",
        )
        .expect("current");
        fs::write(
            sysfs.path("fs/cgroup/db/memory.max").expect("path"),
            "max\n",
        )
        .expect("max");
        let row = collect_process_cgroup_memory(&procfs, &sysfs, 42, start, 2_000)
            .expect("verified cgroup");
        assert_eq!(row.hierarchy, 2);
        assert_ne!(row.mount_namespace, 0);
        assert_eq!(row.memory.current, 90);
        assert_eq!(row.memory.max, None);
        assert!(row.max_unlimited);
    }

    #[test]
    fn reads_v1_memory_controller_and_rejects_wrong_process_start() {
        let (_root, procfs, sysfs, start) = fixture_roots("2:cpu:/other\n5:memory:/db\n");
        fs::create_dir_all(sysfs.path("fs/cgroup/memory/db").expect("path"))
            .expect("memory controller");
        fs::write(
            sysfs
                .path("fs/cgroup/memory/db/memory.usage_in_bytes")
                .expect("path"),
            "70\n",
        )
        .expect("usage");
        fs::write(
            sysfs
                .path("fs/cgroup/memory/db/memory.limit_in_bytes")
                .expect("path"),
            "100\n",
        )
        .expect("limit");
        let row = collect_process_cgroup_memory(&procfs, &sysfs, 42, start, 2_000)
            .expect("verified cgroup");
        assert_eq!(row.hierarchy, 1);
        assert_eq!(row.memory.max, Some(100));
        assert_eq!(
            collect_process_cgroup_memory(&procfs, &sysfs, 42, start + 10_000_000, 2_000),
            Err(JoinFailure::StartMismatch)
        );
    }

    #[test]
    fn detects_pid_reuse_and_cgroup_migration_between_reads() {
        let first = Membership {
            path: "/db".to_owned(),
            unified_v2: true,
        };
        let moved = Membership {
            path: "/other".to_owned(),
            unified_v2: true,
        };
        assert_eq!(
            validate_stable_process(10, &first, 7, 11, &first, 7),
            Err(JoinFailure::ProcessMigrated)
        );
        assert_eq!(
            validate_stable_process(10, &first, 7, 10, &moved, 7),
            Err(JoinFailure::ProcessMigrated)
        );
        assert_eq!(
            validate_stable_process(10, &first, 7, 10, &first, 8),
            Err(JoinFailure::ProcessMigrated)
        );
    }

    #[test]
    fn maps_relative_wal_symlink_to_the_longest_local_mount() {
        let root = tempdir().expect("fixture root");
        let proc_root = root.path().join("proc");
        let data = root.path().join("data");
        let wal = root.path().join("wal");
        fs::create_dir_all(proc_root.join("self/ns")).expect("namespace path");
        fs::write(proc_root.join("self/ns/mnt"), "namespace fixture").expect("namespace");
        let namespace = fs::metadata(proc_root.join("self/ns/mnt"))
            .expect("namespace metadata")
            .ino();
        fs::create_dir_all(&data).expect("data");
        fs::create_dir_all(&wal).expect("wal");
        symlink("../wal", data.join("pg_wal")).expect("relative wal symlink");
        let mounts = [
            mount(1, root.path().to_str().expect("utf8 root")),
            mount(2, wal.to_str().expect("utf8 wal")),
        ];
        let rows = map_postgresql_storage(
            &ProcFs::new(proc_root.clone()),
            namespace,
            &data,
            &[],
            &mounts,
            2,
        )
        .expect("bounded map");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.mapping_state == 1));
        assert_ne!(rows[0].mount_hash, rows[1].mount_hash);
        assert!(rows.iter().all(|row| row.total_bytes.is_some()));
        assert!(rows.iter().all(|row| row.block_device_exact));
        assert_eq!((rows[0].major, rows[0].minor), (Some(8), Some(1)));
        assert_eq!((rows[1].major, rows[1].minor), (Some(8), Some(2)));

        let mut remote_mounts = mounts.clone();
        for mount in &mut remote_mounts {
            mount.source = "server:/volume".to_owned();
        }
        let remote = map_postgresql_storage(
            &ProcFs::new(proc_root),
            namespace,
            &data,
            &[],
            &remote_mounts,
            2,
        )
        .expect("remote mount map");
        assert!(
            remote.iter().all(|row| {
                !row.block_device_exact && row.major.is_none() && row.minor.is_none()
            })
        );
    }

    #[test]
    fn storage_join_rejects_a_different_process_mount_namespace() {
        let root = tempdir().expect("fixture root");
        let proc_root = root.path().join("proc");
        let data = root.path().join("data");
        fs::create_dir_all(proc_root.join("self/ns")).expect("namespace path");
        fs::create_dir_all(&data).expect("data");
        fs::write(proc_root.join("self/ns/mnt"), "collector namespace").expect("namespace");
        assert_eq!(
            map_postgresql_storage(
                &ProcFs::new(proc_root),
                u64::MAX,
                &data,
                &[],
                &[mount(1, root.path().to_str().expect("utf8 root"))],
                2,
            ),
            Err(JoinFailure::NamespaceUnavailable)
        );
    }
}
