//! Type `1_106_001`: paging and swap counters from `/proc/vmstat`.

use crate::{Section, Ts};

/// Paging and swap counters from the `/proc/vmstat` singleton.
///
/// All fields are raw event counts as reported by the kernel.
/// Fields absent on the running kernel decode as `None`, never as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_106_001,
    name = "os_vmstat",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct OsVmstat {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Pages paged in from disk.
    #[column(c)]
    pub pgpgin: Option<i64>,
    /// Pages paged out to disk.
    #[column(c)]
    pub pgpgout: Option<i64>,
    /// Swap pages swapped in.
    #[column(c)]
    pub pswpin: Option<i64>,
    /// Swap pages swapped out.
    #[column(c)]
    pub pswpout: Option<i64>,
    /// Minor page faults (no disk I/O needed).
    #[column(c)]
    pub pgfault: Option<i64>,
    /// Major page faults (disk I/O required).
    #[column(c)]
    pub pgmajfault: Option<i64>,
    /// Pages stolen by kswapd during reclaim.
    #[column(c)]
    pub pgsteal_kswapd: Option<i64>,
    /// Pages stolen directly during reclaim.
    #[column(c)]
    pub pgsteal_direct: Option<i64>,
    /// Pages scanned by kswapd.
    #[column(c)]
    pub pgscan_kswapd: Option<i64>,
    /// Pages scanned directly.
    #[column(c)]
    pub pgscan_direct: Option<i64>,
    /// OOM killer invocations.
    #[column(c)]
    pub oom_kill: Option<i64>,
    /// Source scope (`0=host`). See `kronika_source_os::OsScope`.
    #[column(l)]
    pub scope: u8,
}

#[cfg(test)]
mod tests {
    use super::OsVmstat;
    use crate::{Section, Ts, VerifiedSection, lint};

    fn full_row(ts: i64) -> OsVmstat {
        OsVmstat {
            ts: Ts(ts),
            pgpgin: Some(1_000_000),
            pgpgout: Some(2_000_000),
            pswpin: Some(0),
            pswpout: Some(0),
            pgfault: Some(5_000_000),
            pgmajfault: Some(1024),
            pgsteal_kswapd: Some(512_000),
            pgsteal_direct: Some(4096),
            pgscan_kswapd: Some(768_000),
            pgscan_direct: Some(8192),
            oom_kill: Some(0),
            scope: 0,
        }
    }

    fn sparse_row(ts: i64) -> OsVmstat {
        OsVmstat {
            ts: Ts(ts),
            pgpgin: Some(100),
            pgpgout: Some(200),
            pswpin: None,
            pswpout: None,
            pgfault: None,
            pgmajfault: None,
            pgsteal_kswapd: None,
            pgsteal_direct: None,
            pgscan_kswapd: None,
            pgscan_direct: None,
            oom_kill: None,
            scope: 0,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[OsVmstat::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = OsVmstat::CONTRACT;
        assert_eq!(c.type_id.get(), 1_106_001);
        assert_eq!(c.sort_key, ["ts"]);
        assert_eq!(c.column("pgpgin").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("oom_kill").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("scope").map(|col| col.nullable), Some(false));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        crate::assert_roundtrips(&[full_row(1_000), sparse_row(2_000)]);
    }

    #[test]
    fn nulls_survive_distinct_from_zero() {
        let bytes = OsVmstat::encode(&[sparse_row(5)]).expect("encode");
        let decoded = OsVmstat::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].pswpin, None);
        assert_eq!(decoded[0].pswpout, None);
        assert_eq!(decoded[0].pgfault, None);
        assert_eq!(decoded[0].pgmajfault, None);
        assert_eq!(decoded[0].pgsteal_kswapd, None);
        assert_eq!(decoded[0].pgsteal_direct, None);
        assert_eq!(decoded[0].pgscan_kswapd, None);
        assert_eq!(decoded[0].pgscan_direct, None);
        assert_eq!(decoded[0].oom_kill, None);
    }
}
