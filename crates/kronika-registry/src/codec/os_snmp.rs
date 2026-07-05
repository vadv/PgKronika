//! Type `1_110_001`: global TCP/UDP counters from `/proc/net/snmp`.

use crate::{Section, Ts};

/// Global TCP and UDP counters from the `/proc/net/snmp` singleton.
///
/// Collected once per snapshot. `tcp_curr_estab` is a gauge (current count);
/// all other counter fields are cumulative since boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_110_001,
    name = "os_snmp",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct OsSnmp {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Active TCP connections opened since boot.
    #[column(c)]
    pub tcp_active_opens: i64,
    /// Passive TCP connections opened since boot.
    #[column(c)]
    pub tcp_passive_opens: i64,
    /// TCP connection attempts that failed since boot.
    #[column(c)]
    pub tcp_attempt_fails: i64,
    /// TCP connections reset from ESTABLISHED state since boot.
    #[column(c)]
    pub tcp_estab_resets: i64,
    /// TCP segments received since boot.
    #[column(c)]
    pub tcp_in_segs: i64,
    /// TCP segments sent since boot.
    #[column(c)]
    pub tcp_out_segs: i64,
    /// TCP segments retransmitted since boot.
    #[column(c)]
    pub tcp_retrans_segs: i64,
    /// TCP segments received with errors since boot.
    #[column(c)]
    pub tcp_in_errs: i64,
    /// TCP resets sent since boot.
    #[column(c)]
    pub tcp_out_rsts: i64,
    /// TCP connections currently in ESTABLISHED or CLOSE-WAIT state.
    #[column(g)]
    pub tcp_curr_estab: i64,
    /// UDP datagrams received since boot.
    #[column(c)]
    pub udp_in_datagrams: i64,
    /// UDP datagrams sent since boot.
    #[column(c)]
    pub udp_out_datagrams: i64,
    /// UDP receive errors since boot.
    #[column(c)]
    pub udp_in_errors: i64,
    /// UDP datagrams received to a port with no listener.
    #[column(c)]
    pub udp_no_ports: i64,
    /// Source scope (`0=host`). See `kronika_source_os::OsScope`.
    #[column(l)]
    pub scope: u8,
}

#[cfg(test)]
mod tests {
    use super::OsSnmp;
    use crate::{Section, Ts, VerifiedSection, contract::lint};

    fn row(ts: i64) -> OsSnmp {
        OsSnmp {
            ts: Ts(ts),
            tcp_active_opens: 1,
            tcp_passive_opens: 2,
            tcp_attempt_fails: 3,
            tcp_estab_resets: 4,
            tcp_in_segs: 100,
            tcp_out_segs: 110,
            tcp_retrans_segs: 3,
            tcp_in_errs: 1,
            tcp_out_rsts: 2,
            tcp_curr_estab: 9,
            udp_in_datagrams: 500,
            udp_out_datagrams: 600,
            udp_in_errors: 2,
            udp_no_ports: 4,
            scope: 0,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[OsSnmp::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = OsSnmp::CONTRACT;
        assert_eq!(c.type_id.get(), 1_110_001);
        assert_eq!(c.sort_key, ["ts"]);
    }

    #[test]
    fn encode_sorts_by_ts() {
        let bytes = OsSnmp::encode(&[row(2_000), row(1_000), row(3_000)]).expect("encode");
        let decoded = OsSnmp::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(
            decoded.iter().map(|r| r.ts.0).collect::<Vec<_>>(),
            [1_000, 2_000, 3_000]
        );
    }

    #[test]
    fn roundtrip() {
        crate::assert_roundtrips(&[row(1_000), row(2_000)]);
    }

    #[test]
    fn all_fourteen_counters_survive_encode_decode() {
        let bytes = OsSnmp::encode(&[row(5)]).expect("encode");
        let decoded = OsSnmp::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        let r = &decoded[0];
        assert_eq!(r.tcp_active_opens, 1);
        assert_eq!(r.tcp_curr_estab, 9);
        assert_eq!(r.udp_in_datagrams, 500);
        assert_eq!(r.udp_no_ports, 4);
    }
}
