//! Type `1_038_001`: immutable completeness provenance for multi-row snapshots.
//!
//! A row describes one attempted source read. Consumers must not infer a
//! complete snapshot from its row count: old segments have no marker and are
//! therefore coverage-unknown.

use crate::{Section, Ts};

/// One attempted multi-row snapshot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_038_001,
    name = "snapshot_coverage",
    semantics = snapshot_full,
    sort_key("source_type_id", "ts")
)]
pub struct SnapshotCoverageV1 {
    /// Source snapshot timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// The attempted snapshot layout.
    #[column(l)]
    pub source_type_id: u32,
    /// Collector process identity within `collector_started_at`.
    #[column(l)]
    pub collector_pid: u32,
    /// Collector process-session start, unix microseconds.
    #[column(l)]
    pub collector_started_at: Ts,
    /// `0=complete`, `1=source_limit`, `2=permission`, `3=read_failure`,
    /// `4=collector_limit_or_loss`.
    #[column(l)]
    pub read_state: u8,
    /// `0=full`, `1=restricted`, `2=unknown`.
    #[column(l)]
    pub visibility: u8,
    /// Rows observed before any collector-side withholding.
    #[column(g)]
    pub source_total: u32,
    /// Rows durably written for this snapshot.
    #[column(g)]
    pub collected: u32,
}

#[cfg(test)]
mod tests {
    use super::SnapshotCoverageV1;
    use crate::{Section, Ts, lint};

    fn row(read_state: u8, visibility: u8) -> SnapshotCoverageV1 {
        SnapshotCoverageV1 {
            ts: Ts(10),
            source_type_id: 1_001_003,
            collector_pid: 42,
            collector_started_at: Ts(1),
            read_state,
            visibility,
            source_total: 12,
            collected: 12,
        }
    }

    #[test]
    fn contract_is_versioned_and_roundtrips_states() {
        assert_eq!(SnapshotCoverageV1::CONTRACT.type_id.get(), 1_038_001);
        assert_eq!(lint(&[SnapshotCoverageV1::CONTRACT]), Ok(()));
        crate::assert_roundtrips(&[row(0, 0), row(1, 2), row(2, 1)]);
    }
}
