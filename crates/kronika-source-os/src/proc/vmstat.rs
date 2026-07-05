//! Parse `/proc/vmstat` into the `1_106_001` registry section.

use kronika_registry::Ts;
use kronika_registry::os_vmstat::OsVmstat;

use super::stat::ParseError;

/// Parsed fields from a single `/proc/vmstat` snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmstatRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// Pages paged in from disk.
    pub pgpgin: Option<i64>,
    /// Pages paged out to disk.
    pub pgpgout: Option<i64>,
    /// Swap pages swapped in.
    pub pswpin: Option<i64>,
    /// Swap pages swapped out.
    pub pswpout: Option<i64>,
    /// Minor page faults (no disk I/O needed).
    pub pgfault: Option<i64>,
    /// Major page faults (disk I/O required).
    pub pgmajfault: Option<i64>,
    /// Pages stolen by kswapd during reclaim.
    pub pgsteal_kswapd: Option<i64>,
    /// Pages stolen directly during reclaim.
    pub pgsteal_direct: Option<i64>,
    /// Pages scanned by kswapd.
    pub pgscan_kswapd: Option<i64>,
    /// Pages scanned directly.
    pub pgscan_direct: Option<i64>,
    /// OOM killer invocations.
    pub oom_kill: Option<i64>,
}

/// Parse `/proc/vmstat` content into a [`VmstatRow`].
///
/// Each line has the form `key value` (space-separated, no colon, no unit).
/// All fields are optional; absent keys decode as `None`, never as zero.
///
/// # Errors
///
/// Returns [`ParseError`] when a present value cannot be parsed as `i64`.
pub fn parse_vmstat(content: &str, ts: i64) -> Result<VmstatRow, ParseError> {
    let mut pgpgin: Option<i64> = None;
    let mut pgpgout: Option<i64> = None;
    let mut pswpin: Option<i64> = None;
    let mut pswpout: Option<i64> = None;
    let mut pgfault: Option<i64> = None;
    let mut pgmajfault: Option<i64> = None;
    let mut pgsteal_kswapd: Option<i64> = None;
    let mut pgsteal_direct: Option<i64> = None;
    let mut pgscan_kswapd: Option<i64> = None;
    let mut pgscan_direct: Option<i64> = None;
    let mut oom_kill: Option<i64> = None;

    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(value_str) = parts.next() else {
            continue;
        };
        let parse = || {
            value_str
                .parse::<i64>()
                .map_err(|e| ParseError(format!("/proc/vmstat {key:?}: {e}")))
        };
        match key {
            "pgpgin" => pgpgin = Some(parse()?),
            "pgpgout" => pgpgout = Some(parse()?),
            "pswpin" => pswpin = Some(parse()?),
            "pswpout" => pswpout = Some(parse()?),
            "pgfault" => pgfault = Some(parse()?),
            "pgmajfault" => pgmajfault = Some(parse()?),
            "pgsteal_kswapd" => pgsteal_kswapd = Some(parse()?),
            "pgsteal_direct" => pgsteal_direct = Some(parse()?),
            "pgscan_kswapd" => pgscan_kswapd = Some(parse()?),
            "pgscan_direct" => pgscan_direct = Some(parse()?),
            "oom_kill" => oom_kill = Some(parse()?),
            _ => {}
        }
    }

    Ok(VmstatRow {
        ts,
        pgpgin,
        pgpgout,
        pswpin,
        pswpout,
        pgfault,
        pgmajfault,
        pgsteal_kswapd,
        pgsteal_direct,
        pgscan_kswapd,
        pgscan_direct,
        oom_kill,
    })
}

impl VmstatRow {
    /// Registry row for `1_106_001` with the given scope.
    #[must_use]
    pub const fn to_section(self, scope: u8) -> OsVmstat {
        OsVmstat {
            ts: Ts(self.ts),
            pgpgin: self.pgpgin,
            pgpgout: self.pgpgout,
            pswpin: self.pswpin,
            pswpout: self.pswpout,
            pgfault: self.pgfault,
            pgmajfault: self.pgmajfault,
            pgsteal_kswapd: self.pgsteal_kswapd,
            pgsteal_direct: self.pgsteal_direct,
            pgscan_kswapd: self.pgscan_kswapd,
            pgscan_direct: self.pgscan_direct,
            oom_kill: self.oom_kill,
            scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_vmstat;

    const FULL_SAMPLE: &str = "\
pgpgin 1000000\n\
pgpgout 2000000\n\
pswpin 0\n\
pswpout 0\n\
pgfault 5000000\n\
pgmajfault 1024\n\
pgsteal_kswapd 512000\n\
pgsteal_direct 4096\n\
pgscan_kswapd 768000\n\
pgscan_direct 8192\n\
oom_kill 0\n";

    const SPARSE_SAMPLE: &str = "\
pgpgin 100\n\
pgpgout 200\n\
nr_free_pages 12345\n";

    #[test]
    fn parses_full_sample() {
        let row = parse_vmstat(FULL_SAMPLE, 9_999).expect("parse");
        assert_eq!(row.ts, 9_999);
        assert_eq!(row.pgpgin, Some(1_000_000));
        assert_eq!(row.pgpgout, Some(2_000_000));
        assert_eq!(row.pswpin, Some(0));
        assert_eq!(row.pswpout, Some(0));
        assert_eq!(row.pgfault, Some(5_000_000));
        assert_eq!(row.pgmajfault, Some(1024));
        assert_eq!(row.pgsteal_kswapd, Some(512_000));
        assert_eq!(row.pgsteal_direct, Some(4096));
        assert_eq!(row.pgscan_kswapd, Some(768_000));
        assert_eq!(row.pgscan_direct, Some(8192));
        assert_eq!(row.oom_kill, Some(0));
    }

    #[test]
    fn missing_oom_kill_yields_none() {
        let row = parse_vmstat(SPARSE_SAMPLE, 1).expect("parse");
        assert_eq!(row.pgpgin, Some(100));
        assert_eq!(row.pgpgout, Some(200));
        assert_eq!(row.pswpin, None);
        assert_eq!(row.pswpout, None);
        assert_eq!(row.pgfault, None);
        assert_eq!(row.pgmajfault, None);
        assert_eq!(row.pgsteal_kswapd, None);
        assert_eq!(row.pgsteal_direct, None);
        assert_eq!(row.pgscan_kswapd, None);
        assert_eq!(row.pgscan_direct, None);
        assert_eq!(row.oom_kill, None);
    }

    #[test]
    fn to_section_carries_all_floor_fields_and_scope() {
        let row = parse_vmstat(FULL_SAMPLE, 9_999).expect("parse");
        let section = row.to_section(1);
        assert_eq!(section.ts.0, 9_999);
        assert_eq!(section.pgpgin, Some(1_000_000));
        assert_eq!(section.pgpgout, Some(2_000_000));
        assert_eq!(section.pswpin, Some(0));
        assert_eq!(section.pswpout, Some(0));
        assert_eq!(section.pgfault, Some(5_000_000));
        assert_eq!(section.pgmajfault, Some(1024));
        assert_eq!(section.pgsteal_kswapd, Some(512_000));
        assert_eq!(section.pgsteal_direct, Some(4096));
        assert_eq!(section.pgscan_kswapd, Some(768_000));
        assert_eq!(section.pgscan_direct, Some(8192));
        assert_eq!(section.oom_kill, Some(0));
        assert_eq!(section.scope, 1);
    }
}
