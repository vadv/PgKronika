//! Type `1_203_001`: per-device cgroup I/O counters.

use crate::{Section, StrId, Ts};

/// Per-device cgroup I/O counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_203_001,
    name = "os_cgroup_io",
    semantics = snapshot_full,
    sort_key("cgroup_path", "major", "minor", "ts")
)]
pub struct OsCgroupIo {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Cgroup path as a string dictionary reference.
    #[column(l)]
    pub cgroup_path: StrId,
    /// Block device major number.
    #[column(l)]
    pub major: u32,
    /// Block device minor number.
    #[column(l)]
    pub minor: u32,
    /// Bytes read.
    #[column(c)]
    pub rbytes: i64,
    /// Bytes written.
    #[column(c)]
    pub wbytes: i64,
    /// Read I/O operations.
    #[column(c)]
    pub rios: i64,
    /// Write I/O operations.
    #[column(c)]
    pub wios: i64,
    /// Source scope. See `kronika_source_os::OsScope`.
    #[column(l)]
    pub scope: u8,
}

#[cfg(test)]
mod tests {
    use super::OsCgroupIo;
    use crate::{Section, StrId, Ts, contract::lint};

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[OsCgroupIo::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = OsCgroupIo::CONTRACT;
        assert_eq!(c.type_id.get(), 1_203_001);
        assert_eq!(c.sort_key, ["cgroup_path", "major", "minor", "ts"]);
    }

    #[test]
    fn roundtrip() {
        crate::assert_roundtrips(&[OsCgroupIo {
            ts: Ts(1),
            cgroup_path: StrId(10),
            major: 8,
            minor: 0,
            rbytes: 100,
            wbytes: 200,
            rios: 3,
            wios: 4,
            scope: 1,
        }]);
    }
}
