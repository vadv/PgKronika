//! Type `1_202_001`: cgroup memory usage, limits, and events.

use crate::{Section, StrId, Ts};

/// Memory usage and OOM/event counters for one cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_202_001,
    name = "os_cgroup_memory",
    semantics = snapshot_full,
    sort_key("cgroup_path", "ts")
)]
pub struct OsCgroupMemory {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// Cgroup path as a string dictionary reference.
    #[column(l)]
    pub cgroup_path: StrId,
    /// Current memory usage, bytes.
    #[column(g)]
    pub current: i64,
    /// Memory limit, bytes; `None` means unlimited.
    #[column(g)]
    pub max: Option<i64>,
    /// Anonymous memory, bytes.
    #[column(g)]
    pub anon: i64,
    /// File-backed memory, bytes.
    #[column(g)]
    pub file: i64,
    /// Kernel memory, bytes.
    #[column(g)]
    pub kernel: i64,
    /// Slab memory, bytes.
    #[column(g)]
    pub slab: i64,
    /// `memory.events low`.
    #[column(c)]
    pub low_events: i64,
    /// `memory.events high`.
    #[column(c)]
    pub high_events: i64,
    /// `memory.events max` or v1 `memory.failcnt`.
    #[column(c)]
    pub max_events: i64,
    /// `memory.events oom`.
    #[column(c)]
    pub oom_events: i64,
    /// `memory.events oom_kill`.
    #[column(c)]
    pub oom_kill: i64,
    /// Source scope. See `kronika_source_os::OsScope`.
    #[column(l)]
    pub scope: u8,
}

#[cfg(test)]
mod tests {
    use super::OsCgroupMemory;
    use crate::{Section, StrId, Ts, VerifiedSection, contract::lint};

    fn row(max: Option<i64>) -> OsCgroupMemory {
        OsCgroupMemory {
            ts: Ts(1),
            cgroup_path: StrId(10),
            current: 1024,
            max,
            anon: 100,
            file: 200,
            kernel: 30,
            slab: 20,
            low_events: 1,
            high_events: 2,
            max_events: 3,
            oom_events: 4,
            oom_kill: 5,
            scope: 1,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[OsCgroupMemory::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = OsCgroupMemory::CONTRACT;
        assert_eq!(c.type_id.get(), 1_202_001);
        assert_eq!(c.sort_key, ["cgroup_path", "ts"]);
    }

    #[test]
    fn roundtrip() {
        crate::assert_roundtrips(&[row(Some(2048)), row(None)]);
    }

    #[test]
    fn unlimited_limit_survives_as_null() {
        let bytes = OsCgroupMemory::encode(&[row(None)]).expect("encode");
        let decoded =
            OsCgroupMemory::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].max, None);
    }
}
