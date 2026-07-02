//! Type `1_017_001`: `pg_replication_slots`, one row per slot.
//!
//! Slots are cluster-wide and survive restarts, so a forgotten slot retains
//! WAL forever — `retained_bytes` is the primary signal. LSN columns are
//! absolute byte offsets from `0/0`, saturated to `i64::MAX`. A physical slot
//! created without reserving WAL has no `restart_lsn` yet, and every derived
//! column stays `None` with it.

use crate::{Section, StrId, Ts};

/// One row of type `1_017_001`; one `pg_replication_slots` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_017_001,
    name = "pg_replication_slots",
    semantics = snapshot_full,
    sort_key("slot_name", "ts")
)]
pub struct ReplicationSlotsV1 {
    /// Collection time, unix microseconds; one value for all rows of a read.
    #[column(t)]
    pub ts: Ts,
    /// Slot name.
    #[column(l)]
    pub slot_name: StrId,
    /// Logical decoding plugin; `None` for a physical slot.
    #[column(l)]
    pub plugin: Option<StrId>,
    /// `physical` or `logical`.
    #[column(l)]
    pub slot_type: StrId,
    /// Whether a connection is currently streaming from the slot.
    #[column(g)]
    pub active: bool,
    /// Oldest WAL the slot still needs, byte offset from `0/0`; `None` until
    /// the slot reserves WAL.
    #[column(g)]
    pub restart_lsn: Option<i64>,
    /// Position the consumer confirmed, byte offset from `0/0`; logical slots
    /// only.
    #[column(g)]
    pub confirmed_flush_lsn: Option<i64>,
    /// WAL held back by this slot: `pg_current_wal_lsn() - restart_lsn`.
    /// `None` on a standby (no current LSN there) or without `restart_lsn`.
    #[column(g)]
    pub retained_bytes: Option<i64>,
    /// `reserved`, `extended`, `unreserved`, or `lost`; `None` before PG13 and
    /// for a slot with no reserved WAL.
    #[column(l)]
    pub wal_status: Option<StrId>,
}

#[cfg(test)]
mod tests {
    use super::ReplicationSlotsV1;
    use crate::{Section, StrId, Ts, lint};

    fn logical_row() -> ReplicationSlotsV1 {
        ReplicationSlotsV1 {
            ts: Ts(1_000_000),
            slot_name: StrId(1),
            plugin: Some(StrId(2)),
            slot_type: StrId(3),
            active: true,
            restart_lsn: Some(16_000_000),
            confirmed_flush_lsn: Some(16_100_000),
            retained_bytes: Some(4_096),
            wal_status: Some(StrId(4)),
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[ReplicationSlotsV1::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = ReplicationSlotsV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_017_001);
        assert_eq!(c.columns.len(), 9);
        assert_eq!(c.sort_key, ["slot_name", "ts"]);
        assert_eq!(c.column("slot_name").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("restart_lsn").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("wal_status").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        // A physical slot created without reserving WAL.
        let unreserved = ReplicationSlotsV1 {
            ts: Ts(2_000_000),
            slot_name: StrId(5),
            plugin: None,
            active: false,
            restart_lsn: None,
            confirmed_flush_lsn: None,
            retained_bytes: None,
            wal_status: None,
            ..logical_row()
        };
        crate::assert_roundtrips(&[logical_row(), unreserved]);
    }
}
