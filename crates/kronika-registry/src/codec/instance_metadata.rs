//! Type `1_021_001`: `instance_metadata`, the per-segment instance fingerprint
//! (README.md, "Service Sections").
//!
//! Mandatory in every segment carrying `PostgreSQL` or OS snapshots. It records
//! the source identity and version used to interpret other sections:
//! `pg_version_num` explains version-dependent columns (a PG17 layout drops
//! `pg_stat_bgwriter` counters), and the OS fields (`clock_ticks_per_sec`,
//! `page_size_bytes`, `boot_id`, `btime`) make OS sections self-contained.

use crate::{Section, StrId, Ts};

/// One row of type `1_021_001`; one row per segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_021_001,
    name = "instance_metadata",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct InstanceMetadata {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Collector hostname.
    #[column(l)]
    pub hostname: StrId,
    /// Stable node id the collector assigns itself.
    #[column(l)]
    pub node_self_id: StrId,
    /// `server_version_num`, e.g. 170000 for PG17.
    #[column(l)]
    pub pg_version_num: i32,
    /// OS kernel version string.
    #[column(l)]
    pub kernel_version: StrId,
    /// `pg_control` system identifier; survives restart, changes on `initdb`.
    #[column(l)]
    pub pg_system_identifier: i64,
    /// `sysconf(_SC_CLK_TCK)`; needed to convert OS tick counters.
    #[column(l)]
    pub clock_ticks_per_sec: i64,
    /// OS page size, bytes.
    #[column(l)]
    pub page_size_bytes: i64,
    /// `/proc/sys/kernel/random/boot_id`.
    #[column(l)]
    pub boot_id: StrId,
    /// Kernel boot time (`/proc/stat` btime), unix microseconds.
    #[column(l)]
    pub btime: Ts,
}

#[cfg(test)]
mod tests {
    use super::InstanceMetadata;
    use crate::{Section, StrId, Ts, lint};

    fn row() -> InstanceMetadata {
        InstanceMetadata {
            ts: Ts(1_000_000),
            hostname: StrId(1),
            node_self_id: StrId(2),
            pg_version_num: 170_000,
            kernel_version: StrId(3),
            pg_system_identifier: 7_300_000_000_000_000_000,
            clock_ticks_per_sec: 100,
            page_size_bytes: 4096,
            boot_id: StrId(4),
            btime: Ts(1_700_000_000_000_000),
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[InstanceMetadata::CONTRACT]), Ok(()));
    }

    #[test]
    fn roundtrip_preserves_values() {
        crate::assert_roundtrips(&[row()]);
    }
}
