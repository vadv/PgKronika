//! Type `1_008_001`: `pg_stat_archiver`.
//!
//! WAL archiver singleton.

use crate::{Section, StrId, Ts};

/// Type `1_008_001`: `pg_stat_archiver`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Section)]
#[section(
    id = 1_008_001,
    name = "pg_stat_archiver",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct PgStatArchiver {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// WAL files successfully archived.
    #[column(c)]
    pub archived_count: i64,
    /// Last successfully archived WAL file; `None` until the first archive.
    #[column(l)]
    pub last_archived_wal: Option<StrId>,
    /// Time of the last successful archive.
    #[column(g)]
    pub last_archived_time: Option<Ts>,
    /// Failed archive attempts.
    #[column(c)]
    pub failed_count: i64,
    /// WAL file of the last failed attempt; `None` until the first failure.
    #[column(l)]
    pub last_failed_wal: Option<StrId>,
    /// Time of the last failed attempt.
    #[column(g)]
    pub last_failed_time: Option<Ts>,
    /// Time of the last statistics reset; `None` if never.
    #[column(g)]
    pub stats_reset: Option<Ts>,
}

#[cfg(test)]
mod tests {
    use super::PgStatArchiver;
    use crate::{Section, StrId, Ts, VerifiedSection, lint};

    fn row(ts: i64, with_archive: bool) -> PgStatArchiver {
        PgStatArchiver {
            ts: Ts(ts),
            archived_count: 100,
            last_archived_wal: with_archive.then_some(StrId(1)),
            last_archived_time: with_archive.then(|| Ts(ts - 1000)),
            failed_count: 2,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: Some(Ts(ts - 100_000)),
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[PgStatArchiver::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape_matches_the_registry() {
        let c = PgStatArchiver::CONTRACT;
        assert_eq!(c.type_id.get(), 1_008_001);
        assert_eq!(c.columns.len(), 8);
        assert_eq!(c.sort_key, ["ts"]);
        assert_eq!(
            c.column("last_archived_wal").map(|col| col.nullable),
            Some(true)
        );
        assert_eq!(
            c.column("archived_count").map(|col| col.nullable),
            Some(false)
        );
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        crate::assert_roundtrips(&[row(1_000, true)]);
    }

    #[test]
    fn nulls_survive_distinct_from_zero() {
        let bytes = PgStatArchiver::encode(&[row(5, false)]).expect("encode");
        let decoded =
            PgStatArchiver::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(decoded[0].last_archived_wal, None);
        assert_eq!(decoded[0].last_archived_time, None);
    }
}
