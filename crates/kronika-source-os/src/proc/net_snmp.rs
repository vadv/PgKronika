//! Parse `/proc/net/snmp` global TCP/UDP counters (`1_110`).

use std::collections::HashMap;

use kronika_registry::Ts;
use kronika_registry::os_snmp::OsSnmp;

/// Parse error for procfs lines.
pub use crate::proc::stat::ParseError;

/// Global TCP and UDP counters from `/proc/net/snmp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetSnmpRow {
    /// Active TCP connections opened since boot.
    pub tcp_active_opens: i64,
    /// Passive TCP connections opened since boot.
    pub tcp_passive_opens: i64,
    /// TCP connection attempts that failed since boot.
    pub tcp_attempt_fails: i64,
    /// TCP connections reset from ESTABLISHED state since boot.
    pub tcp_estab_resets: i64,
    /// TCP segments received since boot.
    pub tcp_in_segs: i64,
    /// TCP segments sent since boot.
    pub tcp_out_segs: i64,
    /// TCP segments retransmitted since boot.
    pub tcp_retrans_segs: i64,
    /// TCP segments received with errors since boot.
    pub tcp_in_errs: i64,
    /// TCP resets sent since boot.
    pub tcp_out_rsts: i64,
    /// TCP connections currently in ESTABLISHED or CLOSE-WAIT state.
    pub tcp_curr_estab: i64,
    /// UDP datagrams received since boot.
    pub udp_in_datagrams: i64,
    /// UDP datagrams sent since boot.
    pub udp_out_datagrams: i64,
    /// UDP receive errors since boot.
    pub udp_in_errors: i64,
    /// UDP datagrams received to a port with no listener.
    pub udp_no_ports: i64,
}

/// Parse `/proc/net/snmp` content into a singleton [`NetSnmpRow`].
///
/// Groups are two-line pairs sharing the same `<Proto>:` prefix: the first
/// line names the columns, the second line holds the values. Values are matched
/// by column name, not position. Missing groups or keys default to `0`.
///
/// # Errors
///
/// Returns [`ParseError`] when a value token is not a valid `i64`.
pub fn parse(content: &str) -> Result<NetSnmpRow, ParseError> {
    // Collect header lines indexed by proto prefix.
    let mut header_lines: HashMap<&str, &str> = HashMap::new();
    let mut value_lines: HashMap<&str, &str> = HashMap::new();

    for line in content.lines() {
        let Some((prefix, rest)) = line.split_once(':') else {
            continue;
        };
        let prefix = prefix.trim();
        if header_lines.contains_key(prefix) {
            value_lines.insert(prefix, rest);
        } else {
            header_lines.insert(prefix, rest);
        }
    }

    let lookup = |proto: &str, key: &str| -> Result<i64, ParseError> {
        let headers = header_lines.get(proto).copied().unwrap_or("");
        let values = value_lines.get(proto).copied().unwrap_or("");
        let map: HashMap<&str, &str> = headers
            .split_whitespace()
            .zip(values.split_whitespace())
            .collect();
        map.get(key).map_or(Ok(0), |v| {
            v.parse::<i64>()
                .map_err(|e| ParseError(format!("{proto}/{key}: {e}")))
        })
    };

    Ok(NetSnmpRow {
        tcp_active_opens: lookup("Tcp", "ActiveOpens")?,
        tcp_passive_opens: lookup("Tcp", "PassiveOpens")?,
        tcp_attempt_fails: lookup("Tcp", "AttemptFails")?,
        tcp_estab_resets: lookup("Tcp", "EstabResets")?,
        tcp_in_segs: lookup("Tcp", "InSegs")?,
        tcp_out_segs: lookup("Tcp", "OutSegs")?,
        tcp_retrans_segs: lookup("Tcp", "RetransSegs")?,
        tcp_in_errs: lookup("Tcp", "InErrs")?,
        tcp_out_rsts: lookup("Tcp", "OutRsts")?,
        tcp_curr_estab: lookup("Tcp", "CurrEstab")?,
        udp_in_datagrams: lookup("Udp", "InDatagrams")?,
        udp_out_datagrams: lookup("Udp", "OutDatagrams")?,
        udp_in_errors: lookup("Udp", "InErrors")?,
        udp_no_ports: lookup("Udp", "NoPorts")?,
    })
}

impl NetSnmpRow {
    /// Registry row for `1_110_001` with the given scope and timestamp.
    #[must_use]
    pub const fn to_section(self, scope: u8, ts: i64) -> OsSnmp {
        OsSnmp {
            ts: Ts(ts),
            tcp_active_opens: self.tcp_active_opens,
            tcp_passive_opens: self.tcp_passive_opens,
            tcp_attempt_fails: self.tcp_attempt_fails,
            tcp_estab_resets: self.tcp_estab_resets,
            tcp_in_segs: self.tcp_in_segs,
            tcp_out_segs: self.tcp_out_segs,
            tcp_retrans_segs: self.tcp_retrans_segs,
            tcp_in_errs: self.tcp_in_errs,
            tcp_out_rsts: self.tcp_out_rsts,
            tcp_curr_estab: self.tcp_curr_estab,
            udp_in_datagrams: self.udp_in_datagrams,
            udp_out_datagrams: self.udp_out_datagrams,
            udp_in_errors: self.udp_in_errors,
            udp_no_ports: self.udp_no_ports,
            scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn matches_by_name_not_position() {
        let c = "\
Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts\n\
Tcp: 1 200 120000 -1 5 6 7 8 9 100 110 3 1 2\n\
Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors\n\
Udp: 500 4 2 600 0 0\n";
        let r = parse(c).unwrap();
        assert_eq!(r.tcp_active_opens, 5);
        assert_eq!(r.tcp_curr_estab, 9);
        assert_eq!(r.tcp_out_rsts, 2);
        assert_eq!(r.udp_in_datagrams, 500);
        assert_eq!(r.udp_no_ports, 4);
    }

    #[test]
    fn missing_udp_group_yields_zeros_no_error() {
        let c = "\
Tcp: RtoAlgorithm ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts\n\
Tcp: 1 5 6 7 8 9 100 110 3 1 2\n";
        let r = parse(c).unwrap();
        assert_eq!(r.tcp_active_opens, 5);
        assert_eq!(r.udp_in_datagrams, 0);
        assert_eq!(r.udp_no_ports, 0);
        assert_eq!(r.udp_in_errors, 0);
        assert_eq!(r.udp_out_datagrams, 0);
    }

    #[test]
    fn missing_key_within_group_yields_zero() {
        // No InErrs in the header → defaults to 0.
        let c = "\
Tcp: ActiveOpens PassiveOpens\n\
Tcp: 42 17\n\
Udp: InDatagrams OutDatagrams\n\
Udp: 10 20\n";
        let r = parse(c).unwrap();
        assert_eq!(r.tcp_active_opens, 42);
        assert_eq!(r.tcp_passive_opens, 17);
        assert_eq!(r.tcp_in_errs, 0);
        assert_eq!(r.udp_in_datagrams, 10);
    }

    #[test]
    fn garbled_value_is_an_error() {
        let c = "\
Tcp: ActiveOpens\n\
Tcp: notanumber\n";
        assert!(parse(c).is_err());
    }

    #[test]
    fn to_section_carries_all_fields_and_scope() {
        let c = "\
Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts\n\
Tcp: 1 200 120000 -1 5 6 7 8 9 100 110 3 1 2\n\
Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors\n\
Udp: 500 4 2 600 0 0\n";
        let section = parse(c).unwrap().to_section(3, 9_999);
        assert_eq!(section.ts.0, 9_999);
        assert_eq!(section.scope, 3);
        assert_eq!(section.tcp_active_opens, 5);
        assert_eq!(section.tcp_curr_estab, 9);
        assert_eq!(section.tcp_out_rsts, 2);
        assert_eq!(section.udp_in_datagrams, 500);
        assert_eq!(section.udp_no_ports, 4);
        assert_eq!(section.udp_out_datagrams, 600);
        assert_eq!(section.udp_in_errors, 2);
    }
}
