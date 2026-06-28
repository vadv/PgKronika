//! Stats-reset context per segment, family `1_020`.
//!
//! Lets a reader tell whether a counter was reset between two segments, so a
//! delta across the reset is not mistaken for real activity. The columns are
//! reset timestamps only: extension versions live once per segment in
//! `instance_metadata`, and GUCs live in the settings family — neither is
//! smeared across stats types (see the `type_id` rule in the crate README).
//!
//! `pg_stat_io` arrived in `PostgreSQL` 16, so its reset is a schema difference,
//! not a nullable value: [`ResetMetadata`] = `1_020_001` (PG 15) omits it,
//! [`ResetMetadataIo`] = `1_020_002` (PG 16+) carries it. The collector emits one
//! per segment, chosen by major version.

use crate::{Section, Ts};

/// One row of type `1_020_001`: reset context on `PostgreSQL` 15 (no
/// `pg_stat_io`). One row per segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_020_001,
    name = "reset_metadata",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct ResetMetadata {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// `pg_postmaster_start_time()`. A change marks a restart, not a stats reset
    /// by itself: cumulative stats survive a clean shutdown.
    #[column(g)]
    pub postmaster_start_time: Ts,
    /// Max `stats_reset` across `pg_stat_database`; coalesced to the postmaster
    /// start when no database has ever been reset.
    #[column(g)]
    pub pg_stat_database_reset_max_at: Ts,
    /// `pg_stat_wal.stats_reset`; coalesced to the postmaster start when unset.
    #[column(g)]
    pub pg_stat_wal_reset_at: Ts,
    /// `pg_stat_archiver.stats_reset`; coalesced to the postmaster start when
    /// unset.
    #[column(g)]
    pub pg_stat_archiver_reset_at: Ts,
    /// `pg_stat_statements_info.stats_reset`; `None` when the extension is absent
    /// or older than 1.9 (install config, not a version-shape difference).
    #[column(g)]
    pub pg_stat_statements_reset_at: Option<Ts>,
    /// `pg_store_plans_info.stats_reset`; `None` when the extension is absent.
    #[column(g)]
    pub pg_store_plans_reset_at: Option<Ts>,
}

/// One row of type `1_020_002`: reset context on `PostgreSQL` 16+, identical to
/// [`ResetMetadata`] plus the `pg_stat_io` reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_020_002,
    name = "reset_metadata + pg_stat_io",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct ResetMetadataIo {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// See [`ResetMetadata::postmaster_start_time`].
    #[column(g)]
    pub postmaster_start_time: Ts,
    /// See [`ResetMetadata::pg_stat_database_reset_max_at`].
    #[column(g)]
    pub pg_stat_database_reset_max_at: Ts,
    /// See [`ResetMetadata::pg_stat_wal_reset_at`].
    #[column(g)]
    pub pg_stat_wal_reset_at: Ts,
    /// See [`ResetMetadata::pg_stat_archiver_reset_at`].
    #[column(g)]
    pub pg_stat_archiver_reset_at: Ts,
    /// Max `stats_reset` across `pg_stat_io`; coalesced to the postmaster start
    /// when unset. Present only on PG 16+, which is why this is a separate type.
    #[column(g)]
    pub pg_stat_io_reset_at: Ts,
    /// See [`ResetMetadata::pg_stat_statements_reset_at`].
    #[column(g)]
    pub pg_stat_statements_reset_at: Option<Ts>,
    /// See [`ResetMetadata::pg_store_plans_reset_at`].
    #[column(g)]
    pub pg_store_plans_reset_at: Option<Ts>,
}

#[cfg(test)]
mod tests {
    use super::{ResetMetadata, ResetMetadataIo};
    use crate::{Section, Ts, VerifiedSection, lint};

    fn pg15_row(ts: i64) -> ResetMetadata {
        ResetMetadata {
            ts: Ts(ts),
            postmaster_start_time: Ts(1_700_000_000_000_000),
            pg_stat_database_reset_max_at: Ts(1_700_000_500_000_000),
            pg_stat_wal_reset_at: Ts(1_700_000_200_000_000),
            pg_stat_archiver_reset_at: Ts(1_700_000_100_000_000),
            pg_stat_statements_reset_at: None,
            pg_store_plans_reset_at: None,
        }
    }

    fn pg16_row(ts: i64) -> ResetMetadataIo {
        ResetMetadataIo {
            ts: Ts(ts),
            postmaster_start_time: Ts(1_700_000_000_000_000),
            pg_stat_database_reset_max_at: Ts(1_700_000_500_000_000),
            pg_stat_wal_reset_at: Ts(1_700_000_200_000_000),
            pg_stat_archiver_reset_at: Ts(1_700_000_100_000_000),
            pg_stat_io_reset_at: Ts(1_700_000_600_000_000),
            pg_stat_statements_reset_at: Some(Ts(1_700_000_400_000_000)),
            pg_store_plans_reset_at: None,
        }
    }

    #[test]
    fn both_contracts_pass_the_linter() {
        assert_eq!(
            lint(&[ResetMetadata::CONTRACT, ResetMetadataIo::CONTRACT]),
            Ok(())
        );
    }

    #[test]
    fn pg15_contract_omits_io() {
        let c = ResetMetadata::CONTRACT;
        assert_eq!(c.type_id.get(), 1_020_001);
        assert_eq!(c.columns.len(), 7);
        assert!(
            c.column("pg_stat_io_reset_at").is_none(),
            "PG15 has no pg_stat_io"
        );
    }

    #[test]
    fn pg16_contract_adds_io_as_a_plain_column() {
        let c = ResetMetadataIo::CONTRACT;
        assert_eq!(c.type_id.get(), 1_020_002);
        assert_eq!(c.columns.len(), 8);
        assert_eq!(
            c.column("pg_stat_io_reset_at").map(|col| col.nullable),
            Some(false)
        );
    }

    #[test]
    fn roundtrips_preserve_values_and_extension_nulls() {
        crate::assert_roundtrips(&[pg15_row(1_000_000), pg15_row(2_000_000)]);
        crate::assert_roundtrips(&[pg16_row(1_000_000), pg16_row(2_000_000)]);
    }

    #[test]
    fn absent_extension_resets_survive_distinct_from_zero() {
        let bytes = ResetMetadataIo::encode(&[pg16_row(5)]).expect("encode");
        let decoded =
            ResetMetadataIo::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].pg_store_plans_reset_at, None);
    }
}
