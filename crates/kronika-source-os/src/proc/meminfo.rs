//! Parse `/proc/meminfo` into the `1_104_001` registry section.

use kronika_registry::Ts;
use kronika_registry::os_meminfo::OsMeminfo;

use super::stat::ParseError;

/// Parsed fields from a single `/proc/meminfo` snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeminfoRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Total usable RAM, KiB. Required; error if absent.
    pub mem_total: i64,
    /// Free (completely unused) RAM, KiB.
    pub mem_free: Option<i64>,
    /// Estimate of available RAM for new allocations, KiB.
    pub mem_available: Option<i64>,
    /// In-memory block device cache, KiB.
    pub buffers: Option<i64>,
    /// Page cache, KiB.
    pub cached: Option<i64>,
    /// Total swap space, KiB.
    pub swap_total: Option<i64>,
    /// Unused swap space, KiB.
    pub swap_free: Option<i64>,
    /// Active (recently used) memory, KiB.
    pub active: Option<i64>,
    /// Inactive (candidate for reclaim) memory, KiB.
    pub inactive: Option<i64>,
    /// Dirty pages waiting to be written back, KiB.
    pub dirty: Option<i64>,
    /// Pages currently being written back, KiB.
    pub writeback: Option<i64>,
    /// Total slab memory, KiB.
    pub slab: Option<i64>,
    /// Slab memory reclaimable under pressure, KiB.
    pub s_reclaimable: Option<i64>,
    /// Slab memory not reclaimable, KiB.
    pub s_unreclaim: Option<i64>,
    /// Non-file-backed pages mapped into page tables, KiB.
    pub anon_pages: Option<i64>,
    /// Files mapped into memory, KiB.
    pub mapped: Option<i64>,
    /// Memory used by shared memory (`tmpfs`), KiB.
    pub shmem: Option<i64>,
    /// Memory used by page tables, KiB.
    pub page_tables: Option<i64>,
    /// Upper limit of committed virtual memory, KiB.
    pub commit_limit: Option<i64>,
    /// Total committed virtual memory, KiB.
    pub committed_as: Option<i64>,
    /// Total huge pages in the pool.
    pub huge_pages_total: Option<i64>,
    /// Free huge pages in the pool.
    pub huge_pages_free: Option<i64>,
    /// Size of one huge page, KiB.
    pub hugepagesize: Option<i64>,
}

/// Parse `/proc/meminfo` content into a [`MeminfoRow`].
///
/// Each line has the form `Key:   value kB` (trailing `kB` is optional for
/// counts like `HugePages_Total`). `MemTotal` is required; all other fields
/// default to `None` when the kernel does not emit them.
///
/// # Errors
///
/// Returns [`ParseError`] when `MemTotal` is absent or any present value
/// cannot be parsed as `i64`.
pub fn parse_meminfo(content: &str, ts: i64) -> Result<MeminfoRow, ParseError> {
    let mut mem_total: Option<i64> = None;
    let mut mem_free: Option<i64> = None;
    let mut mem_available: Option<i64> = None;
    let mut buffers: Option<i64> = None;
    let mut cached: Option<i64> = None;
    let mut swap_total: Option<i64> = None;
    let mut swap_free: Option<i64> = None;
    let mut active: Option<i64> = None;
    let mut inactive: Option<i64> = None;
    let mut dirty: Option<i64> = None;
    let mut writeback: Option<i64> = None;
    let mut slab: Option<i64> = None;
    let mut s_reclaimable: Option<i64> = None;
    let mut s_unreclaim: Option<i64> = None;
    let mut anon_pages: Option<i64> = None;
    let mut mapped: Option<i64> = None;
    let mut shmem: Option<i64> = None;
    let mut page_tables: Option<i64> = None;
    let mut commit_limit: Option<i64> = None;
    let mut committed_as: Option<i64> = None;
    let mut huge_pages_total: Option<i64> = None;
    let mut huge_pages_free: Option<i64> = None;
    let mut hugepagesize: Option<i64> = None;

    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        // Value is the first whitespace-separated token after the colon (the
        // trailing `kB` unit token is intentionally ignored).
        let value_str = rest.split_whitespace().next().unwrap_or("");
        if value_str.is_empty() {
            continue;
        }
        let parse = || {
            value_str
                .parse::<i64>()
                .map_err(|e| ParseError(format!("/proc/meminfo {key:?}: {e}")))
        };
        match key {
            "MemTotal" => mem_total = Some(parse()?),
            "MemFree" => mem_free = Some(parse()?),
            "MemAvailable" => mem_available = Some(parse()?),
            "Buffers" => buffers = Some(parse()?),
            "Cached" => cached = Some(parse()?),
            "SwapTotal" => swap_total = Some(parse()?),
            "SwapFree" => swap_free = Some(parse()?),
            "Active" => active = Some(parse()?),
            "Inactive" => inactive = Some(parse()?),
            "Dirty" => dirty = Some(parse()?),
            "Writeback" => writeback = Some(parse()?),
            "Slab" => slab = Some(parse()?),
            "SReclaimable" => s_reclaimable = Some(parse()?),
            "SUnreclaim" => s_unreclaim = Some(parse()?),
            "AnonPages" => anon_pages = Some(parse()?),
            "Mapped" => mapped = Some(parse()?),
            "Shmem" => shmem = Some(parse()?),
            "PageTables" => page_tables = Some(parse()?),
            "CommitLimit" => commit_limit = Some(parse()?),
            "Committed_AS" => committed_as = Some(parse()?),
            "HugePages_Total" => huge_pages_total = Some(parse()?),
            "HugePages_Free" => huge_pages_free = Some(parse()?),
            "Hugepagesize" => hugepagesize = Some(parse()?),
            _ => {}
        }
    }

    let mem_total = mem_total.ok_or_else(|| {
        ParseError("/proc/meminfo: missing required field \"MemTotal\"".to_owned())
    })?;

    Ok(MeminfoRow {
        ts,
        mem_total,
        mem_free,
        mem_available,
        buffers,
        cached,
        swap_total,
        swap_free,
        active,
        inactive,
        dirty,
        writeback,
        slab,
        s_reclaimable,
        s_unreclaim,
        anon_pages,
        mapped,
        shmem,
        page_tables,
        commit_limit,
        committed_as,
        huge_pages_total,
        huge_pages_free,
        hugepagesize,
    })
}

impl MeminfoRow {
    /// Registry row for `1_104_001` with the given scope.
    #[must_use]
    pub const fn to_section(self, scope: u8) -> OsMeminfo {
        OsMeminfo {
            ts: Ts(self.ts),
            mem_total: self.mem_total,
            mem_free: self.mem_free,
            mem_available: self.mem_available,
            buffers: self.buffers,
            cached: self.cached,
            swap_total: self.swap_total,
            swap_free: self.swap_free,
            active: self.active,
            inactive: self.inactive,
            dirty: self.dirty,
            writeback: self.writeback,
            slab: self.slab,
            s_reclaimable: self.s_reclaimable,
            s_unreclaim: self.s_unreclaim,
            anon_pages: self.anon_pages,
            mapped: self.mapped,
            shmem: self.shmem,
            page_tables: self.page_tables,
            commit_limit: self.commit_limit,
            committed_as: self.committed_as,
            huge_pages_total: self.huge_pages_total,
            huge_pages_free: self.huge_pages_free,
            hugepagesize: self.hugepagesize,
            scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_meminfo;

    const FULL_SAMPLE: &str = "\
MemTotal:       16777216 kB\n\
MemFree:         4096000 kB\n\
MemAvailable:    8192000 kB\n\
Buffers:          512000 kB\n\
Cached:          3145728 kB\n\
SwapCached:            0 kB\n\
Active:          6291456 kB\n\
Inactive:        2097152 kB\n\
Dirty:              1024 kB\n\
Writeback:             0 kB\n\
AnonPages:       4194304 kB\n\
Mapped:          1048576 kB\n\
Shmem:             32768 kB\n\
Slab:             524288 kB\n\
SReclaimable:     262144 kB\n\
SUnreclaim:       262144 kB\n\
PageTables:        16384 kB\n\
SwapTotal:       8388608 kB\n\
SwapFree:        8000000 kB\n\
CommitLimit:    12582912 kB\n\
Committed_AS:   10485760 kB\n\
HugePages_Total:       0\n\
HugePages_Free:        0\n\
Hugepagesize:       2048 kB\n";

    const SPARSE_SAMPLE: &str = "\
MemTotal:        8388608 kB\n\
MemFree:         1000000 kB\n";

    #[test]
    fn parses_full_sample() {
        let row = parse_meminfo(FULL_SAMPLE, 9_999).expect("parse");
        assert_eq!(row.ts, 9_999);
        assert_eq!(row.mem_total, 16_777_216);
        assert_eq!(row.mem_free, Some(4_096_000));
        assert_eq!(row.mem_available, Some(8_192_000));
        assert_eq!(row.buffers, Some(512_000));
        assert_eq!(row.cached, Some(3_145_728));
        assert_eq!(row.swap_total, Some(8_388_608));
        assert_eq!(row.swap_free, Some(8_000_000));
        assert_eq!(row.active, Some(6_291_456));
        assert_eq!(row.inactive, Some(2_097_152));
        assert_eq!(row.dirty, Some(1024));
        assert_eq!(row.writeback, Some(0));
        assert_eq!(row.slab, Some(524_288));
        assert_eq!(row.s_reclaimable, Some(262_144));
        assert_eq!(row.s_unreclaim, Some(262_144));
        assert_eq!(row.anon_pages, Some(4_194_304));
        assert_eq!(row.mapped, Some(1_048_576));
        assert_eq!(row.shmem, Some(32_768));
        assert_eq!(row.page_tables, Some(16_384));
        assert_eq!(row.commit_limit, Some(12_582_912));
        assert_eq!(row.committed_as, Some(10_485_760));
        assert_eq!(row.huge_pages_total, Some(0));
        assert_eq!(row.huge_pages_free, Some(0));
        assert_eq!(row.hugepagesize, Some(2048));
    }

    #[test]
    fn missing_optional_keys_yield_none() {
        let row = parse_meminfo(SPARSE_SAMPLE, 1).expect("parse");
        assert_eq!(row.mem_total, 8_388_608);
        assert_eq!(row.mem_free, Some(1_000_000));
        assert_eq!(row.mem_available, None);
        assert_eq!(row.slab, None);
        assert_eq!(row.s_reclaimable, None);
        assert_eq!(row.s_unreclaim, None);
        assert_eq!(row.dirty, None);
        assert_eq!(row.writeback, None);
        assert_eq!(row.huge_pages_total, None);
    }

    #[test]
    fn missing_mem_total_is_an_error() {
        let no_total = "MemFree: 4096 kB\nMemAvailable: 8192 kB\n";
        assert!(parse_meminfo(no_total, 1).is_err());
    }

    #[test]
    fn to_section_carries_all_floor_fields_and_scope() {
        let row = parse_meminfo(FULL_SAMPLE, 9_999).expect("parse");
        let section = row.to_section(1);
        assert_eq!(section.ts.0, 9_999);
        assert_eq!(section.mem_total, 16_777_216);
        assert_eq!(section.mem_free, Some(4_096_000));
        assert_eq!(section.mem_available, Some(8_192_000));
        assert_eq!(section.buffers, Some(512_000));
        assert_eq!(section.cached, Some(3_145_728));
        assert_eq!(section.slab, Some(524_288));
        assert_eq!(section.s_reclaimable, Some(262_144));
        assert_eq!(section.s_unreclaim, Some(262_144));
        assert_eq!(section.swap_total, Some(8_388_608));
        assert_eq!(section.swap_free, Some(8_000_000));
        assert_eq!(section.dirty, Some(1024));
        assert_eq!(section.writeback, Some(0));
        assert_eq!(section.scope, 1);
    }
}
