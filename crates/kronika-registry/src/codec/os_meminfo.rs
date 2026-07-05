//! Type `1_104_001`: memory stats from `/proc/meminfo`.

use crate::{Section, Ts};

/// Memory statistics from the `/proc/meminfo` singleton.
///
/// All size fields are raw KiB values as reported by the kernel.
/// Fields absent on the running kernel decode as `None`, never as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_104_001,
    name = "os_meminfo",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct OsMeminfo {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Total usable RAM, KiB.
    #[column(g)]
    pub mem_total: i64,
    /// Free (completely unused) RAM, KiB.
    #[column(g)]
    pub mem_free: Option<i64>,
    /// Estimate of available RAM for new allocations, KiB.
    #[column(g)]
    pub mem_available: Option<i64>,
    /// In-memory block device cache (buffers), KiB.
    #[column(g)]
    pub buffers: Option<i64>,
    /// Page cache (excluding `SwapCached`), KiB.
    #[column(g)]
    pub cached: Option<i64>,
    /// Total swap space, KiB.
    #[column(g)]
    pub swap_total: Option<i64>,
    /// Unused swap space, KiB.
    #[column(g)]
    pub swap_free: Option<i64>,
    /// Active (recently used) memory, KiB.
    #[column(g)]
    pub active: Option<i64>,
    /// Inactive (candidate for reclaim) memory, KiB.
    #[column(g)]
    pub inactive: Option<i64>,
    /// Dirty pages waiting to be written back, KiB.
    #[column(g)]
    pub dirty: Option<i64>,
    /// Pages currently being written back, KiB.
    #[column(g)]
    pub writeback: Option<i64>,
    /// Total slab memory (reclaimable + unreclaimable), KiB.
    #[column(g)]
    pub slab: Option<i64>,
    /// Slab memory reclaimable under pressure, KiB.
    #[column(g)]
    pub s_reclaimable: Option<i64>,
    /// Slab memory not reclaimable, KiB.
    #[column(g)]
    pub s_unreclaim: Option<i64>,
    /// Non-file-backed pages mapped into page tables, KiB.
    #[column(g)]
    pub anon_pages: Option<i64>,
    /// Files mapped into memory, KiB.
    #[column(g)]
    pub mapped: Option<i64>,
    /// Memory used by shared memory (`tmpfs`), KiB.
    #[column(g)]
    pub shmem: Option<i64>,
    /// Memory used by page tables, KiB.
    #[column(g)]
    pub page_tables: Option<i64>,
    /// Upper limit of committed virtual memory, KiB.
    #[column(g)]
    pub commit_limit: Option<i64>,
    /// Total committed virtual memory, KiB.
    #[column(g)]
    pub committed_as: Option<i64>,
    /// Total huge pages in the pool.
    #[column(g)]
    pub huge_pages_total: Option<i64>,
    /// Free huge pages in the pool.
    #[column(g)]
    pub huge_pages_free: Option<i64>,
    /// Size of one huge page, KiB.
    #[column(g)]
    pub hugepagesize: Option<i64>,
    /// Source scope (`0=host`). See `kronika_source_os::OsScope`.
    #[column(l)]
    pub scope: u8,
}

#[cfg(test)]
mod tests {
    use super::OsMeminfo;
    use crate::{Section, Ts, VerifiedSection, lint};

    fn full_row(ts: i64) -> OsMeminfo {
        OsMeminfo {
            ts: Ts(ts),
            mem_total: 16_777_216,
            mem_free: Some(4_096_000),
            mem_available: Some(8_192_000),
            buffers: Some(512_000),
            cached: Some(3_145_728),
            swap_total: Some(8_388_608),
            swap_free: Some(8_000_000),
            active: Some(6_291_456),
            inactive: Some(2_097_152),
            dirty: Some(1024),
            writeback: Some(0),
            slab: Some(524_288),
            s_reclaimable: Some(262_144),
            s_unreclaim: Some(262_144),
            anon_pages: Some(4_194_304),
            mapped: Some(1_048_576),
            shmem: Some(32_768),
            page_tables: Some(16_384),
            commit_limit: Some(12_582_912),
            committed_as: Some(10_485_760),
            huge_pages_total: Some(0),
            huge_pages_free: Some(0),
            hugepagesize: Some(2048),
            scope: 0,
        }
    }

    fn sparse_row(ts: i64) -> OsMeminfo {
        OsMeminfo {
            ts: Ts(ts),
            mem_total: 8_388_608,
            mem_free: Some(1_000_000),
            mem_available: None,
            buffers: None,
            cached: None,
            swap_total: None,
            swap_free: None,
            active: None,
            inactive: None,
            dirty: None,
            writeback: None,
            slab: None,
            s_reclaimable: None,
            s_unreclaim: None,
            anon_pages: None,
            mapped: None,
            shmem: None,
            page_tables: None,
            commit_limit: None,
            committed_as: None,
            huge_pages_total: None,
            huge_pages_free: None,
            hugepagesize: None,
            scope: 0,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[OsMeminfo::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = OsMeminfo::CONTRACT;
        assert_eq!(c.type_id.get(), 1_104_001);
        assert_eq!(c.sort_key, ["ts"]);
        assert_eq!(c.column("mem_total").map(|col| col.nullable), Some(false));
        assert_eq!(
            c.column("s_reclaimable").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(c.column("s_unreclaim").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        crate::assert_roundtrips(&[full_row(1_000), sparse_row(2_000)]);
    }

    #[test]
    fn nulls_survive_distinct_from_zero() {
        let bytes = OsMeminfo::encode(&[sparse_row(5)]).expect("encode");
        let decoded = OsMeminfo::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].mem_available, None);
        assert_eq!(decoded[0].slab, None);
        assert_eq!(decoded[0].s_reclaimable, None);
        assert_eq!(decoded[0].s_unreclaim, None);
        assert_eq!(decoded[0].dirty, None);
        assert_eq!(decoded[0].writeback, None);
    }
}
