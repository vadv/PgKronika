//! Parse `/proc/stat` CPU lines (`1_102`) and the misc counters (`1_103`).

use std::num::ParseIntError;

use kronika_registry::Ts;
use kronika_registry::os_cpu::OsCpu;

/// One CPU's ticks; `cpu_id = -1` is the aggregate line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuRow {
    /// Collection timestamp, unix microseconds.
    pub ts: i64,
    /// `-1` for the aggregate `cpu` line, else the CPU index.
    pub cpu_id: i32,
    /// Ticks in user mode.
    pub user: i64,
    /// Ticks in user mode with low priority (nice).
    pub nice: i64,
    /// Ticks in system (kernel) mode.
    pub system: i64,
    /// Ticks idle.
    pub idle: i64,
    /// Ticks waiting for I/O to complete.
    pub iowait: i64,
    /// Ticks serving hardware interrupts.
    pub irq: i64,
    /// Ticks serving software interrupts.
    pub softirq: i64,
    /// Ticks stolen by a hypervisor.
    pub steal: i64,
    /// Ticks spent running a virtual CPU for a guest OS.
    pub guest: i64,
    /// Ticks spent running a niced guest.
    pub guest_nice: i64,
}

/// Parse error for procfs lines.
#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ParseError {}
impl From<ParseIntError> for ParseError {
    fn from(e: ParseIntError) -> Self {
        Self(format!("integer: {e}"))
    }
}

/// Parse every `cpu`/`cpuN` line. Ten time fields; missing trailing fields
/// (older kernels) default to `0` per the `/proc/stat` contract.
///
/// # Errors
///
/// Returns [`ParseError`] when an integer field cannot be parsed, or when no
/// `cpu` lines are present.
pub fn parse_cpu(content: &str, ts: i64) -> Result<Vec<CpuRow>, ParseError> {
    let mut rows = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        let Some((id_part, values)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let cpu_id = if id_part.is_empty() {
            -1
        } else {
            id_part
                .parse::<i32>()
                .map_err(|e| ParseError(format!("cpu id {id_part:?}: {e}")))?
        };
        let mut f = values.split_whitespace();
        let mut next = || -> Result<i64, ParseError> {
            f.next()
                .map_or(Ok(0), |s| s.parse::<i64>().map_err(Into::into))
        };
        rows.push(CpuRow {
            ts,
            cpu_id,
            user: next()?,
            nice: next()?,
            system: next()?,
            idle: next()?,
            iowait: next()?,
            irq: next()?,
            softirq: next()?,
            steal: next()?,
            guest: next()?,
            guest_nice: next()?,
        });
    }
    if rows.is_empty() {
        return Err(ParseError("/proc/stat: no cpu lines".to_owned()));
    }
    Ok(rows)
}

impl CpuRow {
    /// Registry row for `1_102_001` with the given scope.
    #[must_use]
    pub const fn to_section(self, scope: u8) -> OsCpu {
        OsCpu {
            ts: Ts(self.ts),
            cpu_id: self.cpu_id,
            user: self.user,
            nice: self.nice,
            system: self.system,
            idle: self.idle,
            iowait: self.iowait,
            irq: self.irq,
            softirq: self.softirq,
            steal: self.steal,
            guest: self.guest,
            guest_nice: self.guest_nice,
            scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_cpu;

    const SAMPLE: &str = "cpu  100 20 30 400 5 6 7 8 9 10\n\
                          cpu0 50 10 15 200 2 3 3 4 4 5\n\
                          cpu1 50 10 15 200 3 3 4 4 5 5\n\
                          intr 999\nctxt 12345\n";

    #[test]
    fn parses_aggregate_and_per_cpu() {
        let rows = parse_cpu(SAMPLE, 1_700).expect("parse");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].cpu_id, -1);
        assert_eq!(rows[0].user, 100);
        assert_eq!(rows[1].cpu_id, 0);
        assert_eq!(rows[2].cpu_id, 1);
        assert_eq!(rows[0].guest_nice, 10);
    }

    #[test]
    fn old_kernel_missing_trailing_fields_default_to_zero() {
        let rows = parse_cpu("cpu 100 20 30 400 5 6 7\n", 1).expect("parse");
        assert_eq!(rows[0].steal, 0);
        assert_eq!(rows[0].guest, 0);
    }

    #[test]
    fn garbled_cpu_line_errors() {
        assert!(parse_cpu("cpu notanumber 2 3\n", 1).is_err());
    }
}
