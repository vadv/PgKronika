//! Type `1_012_001`: `pg_stat_progress_vacuum`.
//!
//! One row per backend running `VACUUM`; absence means no active rows.

use crate::{Section, StrId, Ts};

/// Type `1_012_001`: `pg_stat_progress_vacuum`.
///
/// Nullable columns preserve `PostgreSQL` catalog-era differences.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_012_001,
    name = "pg_stat_progress_vacuum",
    semantics = conditional_full,
    sort_key("ts", "pid")
)]
pub struct PgStatProgressVacuum {
    /// Snapshot time, unix microseconds; one value for all rows of a snapshot.
    #[column(t)]
    pub ts: Ts,
    /// Process id of the backend running this vacuum.
    #[column(l)]
    pub pid: i32,
    /// OID of the database being vacuumed.
    #[column(l)]
    pub datid: u32,
    /// Database being vacuumed, interned in the segment dictionary.
    #[column(l)]
    pub datname: StrId,
    /// OID of the table being vacuumed.
    #[column(l)]
    pub relid: u32,
    /// Whether the backend is an autovacuum worker.
    #[column(l)]
    pub is_autovacuum: bool,
    /// Current vacuum phase, such as `scanning heap`, interned.
    #[column(l)]
    pub phase: StrId,
    /// Heap blocks in the table at scan start.
    #[column(g)]
    pub heap_blks_total: i64,
    /// Heap blocks scanned so far.
    #[column(g)]
    pub heap_blks_scanned: i64,
    /// Heap blocks vacuumed so far.
    #[column(g)]
    pub heap_blks_vacuumed: i64,
    /// Completed index-vacuum cycles.
    #[column(g)]
    pub index_vacuum_count: i64,
    /// Dead-tuple capacity (PG10-16).
    #[column(g)]
    pub max_dead_tuples: Option<i64>,
    /// Dead tuples collected in the current cycle (PG10-16).
    #[column(g)]
    pub num_dead_tuples: Option<i64>,
    /// Dead-tuple TID store capacity, bytes (PG17+).
    #[column(g)]
    pub max_dead_tuple_bytes: Option<i64>,
    /// Dead-tuple TID store usage, bytes (PG17+).
    #[column(g)]
    pub dead_tuple_bytes: Option<i64>,
    /// Dead item identifiers collected (PG17+).
    #[column(g)]
    pub num_dead_item_ids: Option<i64>,
    /// Indexes to process in this cycle (PG17+).
    #[column(g)]
    pub indexes_total: Option<i64>,
    /// Indexes processed in this cycle (PG17+).
    #[column(g)]
    pub indexes_processed: Option<i64>,
    /// Cost-delay sleep time, ms (PG18+).
    #[column(g)]
    pub delay_time: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::PgStatProgressVacuum;
    use crate::{Section, Semantics, StrId, Ts, VerifiedSection, lint};

    fn pre17_row(ts: i64, pid: i32) -> PgStatProgressVacuum {
        PgStatProgressVacuum {
            ts: Ts(ts),
            pid,
            datid: 16_385,
            datname: StrId(7),
            relid: 16_384,
            is_autovacuum: false,
            phase: StrId(9),
            heap_blks_total: 10_000,
            heap_blks_scanned: 4_200,
            heap_blks_vacuumed: 4_000,
            index_vacuum_count: 1,
            max_dead_tuples: Some(291_271),
            num_dead_tuples: Some(120_000),
            max_dead_tuple_bytes: None,
            dead_tuple_bytes: None,
            num_dead_item_ids: None,
            indexes_total: None,
            indexes_processed: None,
            delay_time: None,
        }
    }

    fn pg17_row(ts: i64, pid: i32) -> PgStatProgressVacuum {
        PgStatProgressVacuum {
            max_dead_tuples: None,
            num_dead_tuples: None,
            max_dead_tuple_bytes: Some(67_108_864),
            dead_tuple_bytes: Some(2_500_000),
            num_dead_item_ids: Some(120_000),
            indexes_total: Some(3),
            indexes_processed: Some(1),
            ..pre17_row(ts, pid)
        }
    }

    fn pg18_row(ts: i64, pid: i32) -> PgStatProgressVacuum {
        PgStatProgressVacuum {
            delay_time: Some(1234.5),
            ..pg17_row(ts, pid)
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[PgStatProgressVacuum::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape_matches_the_source() {
        let c = PgStatProgressVacuum::CONTRACT;
        assert_eq!(c.type_id.get(), 1_012_001);
        assert_eq!(c.semantics, Semantics::ConditionalFull);
        assert_eq!(c.columns.len(), 19);
        assert_eq!(c.sort_key, ["ts", "pid"]);
        assert_eq!(c.column("pid").map(|col| col.nullable), Some(false));
        assert_eq!(c.column("datid").map(|col| col.nullable), Some(false));
        assert_eq!(
            c.column("is_autovacuum").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(
            c.column("heap_blks_total").map(|col| col.nullable),
            Some(false)
        );
        // The dead-tuple era columns and delay_time are version-specific.
        assert_eq!(
            c.column("num_dead_tuples").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(
            c.column("dead_tuple_bytes").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(c.column("delay_time").map(|col| col.nullable), Some(true));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        // One section may hold rows from all three eras.
        crate::assert_roundtrips(&[
            pre17_row(1_000_000, 100),
            pg17_row(1_000_000, 200),
            pg18_row(1_000_000, 300),
        ]);
    }

    #[test]
    fn nulls_survive_distinct_from_zero() {
        // A pre-PG17 row must keep the byte-era columns NULL, not Some(0).
        let bytes = PgStatProgressVacuum::encode(&[pre17_row(5, 1)]).expect("encode");
        let decoded =
            PgStatProgressVacuum::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].dead_tuple_bytes, None);
        assert_eq!(decoded[0].delay_time, None);
    }
}
