//! Type `1_111_001`: extended TCP counters from `/proc/net/netstat`.

use crate::{Section, Ts};

/// Extended TCP counters from the `/proc/net/netstat` singleton.
///
/// All fields are cumulative counters since boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_111_001,
    name = "os_netstat",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct OsNetstat {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// TCP listen queue overflows since boot.
    #[column(c)]
    pub listen_overflows: i64,
    /// Connections dropped while listening since boot.
    #[column(c)]
    pub listen_drops: i64,
    /// TCP timeout events since boot.
    #[column(c)]
    pub tcp_timeouts: i64,
    /// TCP fast retransmissions since boot.
    #[column(c)]
    pub tcp_fast_retrans: i64,
    /// TCP slow-start retransmissions since boot.
    #[column(c)]
    pub tcp_slow_start_retrans: i64,
    /// Packets placed in the out-of-order queue since boot.
    #[column(c)]
    pub tcp_ofo_queue: i64,
    /// SYN retransmissions since boot.
    #[column(c)]
    pub tcp_syn_retrans: i64,
    /// Source scope (`0=host`). See `kronika_source_os::OsScope`.
    #[column(l)]
    pub scope: u8,
}

#[cfg(test)]
mod tests {
    use super::OsNetstat;
    use crate::{Section, Ts, VerifiedSection, contract::lint};

    fn row(ts: i64) -> OsNetstat {
        OsNetstat {
            ts: Ts(ts),
            listen_overflows: 10,
            listen_drops: 20,
            tcp_timeouts: 30,
            tcp_fast_retrans: 40,
            tcp_slow_start_retrans: 50,
            tcp_ofo_queue: 60,
            tcp_syn_retrans: 70,
            scope: 0,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[OsNetstat::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = OsNetstat::CONTRACT;
        assert_eq!(c.type_id.get(), 1_111_001);
        assert_eq!(c.sort_key, ["ts"]);
    }

    #[test]
    fn encode_sorts_by_ts() {
        let bytes = OsNetstat::encode(&[row(2_000), row(1_000), row(3_000)]).expect("encode");
        let decoded = OsNetstat::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
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
    fn all_seven_counters_survive_encode_decode() {
        let bytes = OsNetstat::encode(&[row(5)]).expect("encode");
        let decoded = OsNetstat::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        let r = &decoded[0];
        assert_eq!(r.listen_overflows, 10);
        assert_eq!(r.listen_drops, 20);
        assert_eq!(r.tcp_timeouts, 30);
        assert_eq!(r.tcp_fast_retrans, 40);
        assert_eq!(r.tcp_slow_start_retrans, 50);
        assert_eq!(r.tcp_ofo_queue, 60);
        assert_eq!(r.tcp_syn_retrans, 70);
    }
}
