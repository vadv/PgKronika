//! Background-writer / checkpointer activity, family `1_006`.
//!
//! `PostgreSQL` 17 split the combined `pg_stat_bgwriter` view into
//! `pg_stat_bgwriter` plus `pg_stat_checkpointer`, renamed checkpoint counters,
//! and moved the backend-write counters to `pg_stat_io`. The two layouts are
//! distinct schemas, so each gets its own `type_id` (see the `type_id` rule in
//! the crate README):
//! [`Bgwriter`] = `1_006_001` (PG 15–16), [`BgwriterCheckpointer`] = `1_006_002`
//! (PG 17+). The collector emits exactly one, chosen by major version; neither
//! carries a column the other lacks.

use crate::{Section, Ts};

/// One `1_006_001` row: `pg_stat_bgwriter` on `PostgreSQL` 15-16, where
/// checkpoint and background-writer counters share one view.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_006_001,
    name = "pg_stat_bgwriter",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct Bgwriter {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// `checkpoints_timed`: scheduled checkpoints.
    #[column(c)]
    pub checkpoints_timed: i64,
    /// `checkpoints_req`: requested checkpoints.
    #[column(c)]
    pub checkpoints_req: i64,
    /// `checkpoint_write_time`, ms.
    #[column(c)]
    pub checkpoint_write_time: f64,
    /// `checkpoint_sync_time`, ms.
    #[column(c)]
    pub checkpoint_sync_time: f64,
    /// `buffers_checkpoint`: buffers written during checkpoints.
    #[column(c)]
    pub buffers_checkpoint: i64,
    /// `buffers_clean`: buffers written by the background writer.
    #[column(c)]
    pub buffers_clean: i64,
    /// `maxwritten_clean`: cleaning scans stopped early for hitting the limit.
    #[column(c)]
    pub maxwritten_clean: i64,
    /// `buffers_backend`: buffers written directly by backends. PG17 moved this
    /// to `pg_stat_io`, which is why it lives only in this version's schema.
    #[column(c)]
    pub buffers_backend: i64,
    /// `buffers_backend_fsync`: backend fsync calls. PG17 dropped it from this
    /// view.
    #[column(c)]
    pub buffers_backend_fsync: i64,
    /// `buffers_alloc`: buffers allocated.
    #[column(c)]
    pub buffers_alloc: i64,
    /// `pg_stat_bgwriter.stats_reset`: rate baseline for every counter here.
    #[column(g)]
    pub stats_reset: Ts,
}

/// One `1_006_002` row: `pg_stat_checkpointer` joined with the slimmed
/// `pg_stat_bgwriter` on `PostgreSQL` 17+.
///
/// Counter names follow PG17's catalogs, not the 15–16 names: `num_timed` counts
/// skipped checkpoints that `checkpoints_timed` did not, and the times include
/// restartpoints — different definitions, hence a different `type_id`.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_006_002,
    name = "pg_stat_checkpointer + pg_stat_bgwriter",
    semantics = snapshot_full,
    sort_key("ts")
)]
pub struct BgwriterCheckpointer {
    /// Collection timestamp, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// `pg_stat_checkpointer.num_timed`: scheduled checkpoints, including ones
    /// skipped because nothing had changed.
    #[column(c)]
    pub num_timed: i64,
    /// `pg_stat_checkpointer.num_requested`: requested checkpoints.
    #[column(c)]
    pub num_requested: i64,
    /// `restartpoints_timed`: scheduled restartpoints (the checkpoint path on a
    /// hot standby).
    #[column(c)]
    pub restartpoints_timed: i64,
    /// `restartpoints_req`: requested restartpoints.
    #[column(c)]
    pub restartpoints_req: i64,
    /// `restartpoints_done`: completed restartpoints; the recovery-checkpoint
    /// rate on a standby.
    #[column(c)]
    pub restartpoints_done: i64,
    /// `write_time`, ms: checkpoint and restartpoint buffer writes.
    #[column(c)]
    pub write_time: f64,
    /// `sync_time`, ms: checkpoint and restartpoint syncs.
    #[column(c)]
    pub sync_time: f64,
    /// `buffers_written`: buffers written by the checkpointer.
    #[column(c)]
    pub buffers_written: i64,
    /// `pg_stat_bgwriter.buffers_clean`: buffers written by the background
    /// writer.
    #[column(c)]
    pub buffers_clean: i64,
    /// `pg_stat_bgwriter.maxwritten_clean`: cleaning scans stopped early.
    #[column(c)]
    pub maxwritten_clean: i64,
    /// `pg_stat_bgwriter.buffers_alloc`: buffers allocated.
    #[column(c)]
    pub buffers_alloc: i64,
    /// `pg_stat_bgwriter.stats_reset`: rate baseline for the background-writer
    /// counters.
    #[column(g)]
    pub bgwriter_stats_reset: Ts,
    /// `pg_stat_checkpointer.stats_reset`: rate baseline for the checkpointer
    /// counters; resettable independently of the background-writer view.
    #[column(g)]
    pub checkpointer_stats_reset: Ts,
}

#[cfg(test)]
mod tests {
    use super::{Bgwriter, BgwriterCheckpointer};
    use crate::{
        CodecError, MAX_SECTION_BYTES, MAX_SECTION_ROWS, Section, Ts, VerifiedSection, lint,
    };

    fn bgwriter_row(ts: i64) -> Bgwriter {
        Bgwriter {
            ts: Ts(ts),
            checkpoints_timed: 10,
            checkpoints_req: 2,
            checkpoint_write_time: 1234.5,
            checkpoint_sync_time: 67.0,
            buffers_checkpoint: 4096,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_backend: 128,
            buffers_backend_fsync: 0,
            buffers_alloc: 9000,
            stats_reset: Ts(ts - 100_000),
        }
    }

    fn checkpointer_row(ts: i64) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts: Ts(ts),
            num_timed: 11,
            num_requested: 2,
            restartpoints_timed: 7,
            restartpoints_req: 1,
            restartpoints_done: 6,
            write_time: 1234.5,
            sync_time: 67.0,
            buffers_written: 4096,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_alloc: 9000,
            bgwriter_stats_reset: Ts(ts - 100_000),
            checkpointer_stats_reset: Ts(ts - 50_000),
        }
    }

    #[test]
    fn both_contracts_pass_the_linter() {
        assert_eq!(
            lint(&[Bgwriter::CONTRACT, BgwriterCheckpointer::CONTRACT]),
            Ok(())
        );
    }

    #[test]
    fn pre17_contract_is_exactly_pg_stat_bgwriter() {
        let c = Bgwriter::CONTRACT;
        assert_eq!(c.type_id.get(), 1_006_001);
        assert_eq!(c.columns.len(), 12);
        assert_eq!(c.sort_key, ["ts"]);
        // No version-Option columns: every field exists on PG 15–16.
        assert!(
            c.columns
                .iter()
                .all(|col| col.name == "ts" || !col.nullable)
        );
    }

    #[test]
    fn pg17_contract_is_exactly_the_split_views() {
        let c = BgwriterCheckpointer::CONTRACT;
        assert_eq!(c.type_id.get(), 1_006_002);
        assert_eq!(c.columns.len(), 14);
        // restartpoints and the checkpointer reset are plain columns here, not
        // the nullable PG17-only fields the old merged type carried.
        assert_eq!(
            c.column("restartpoints_done").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(
            c.column("checkpointer_stats_reset").map(|col| col.nullable),
            Some(false)
        );
        assert!(
            c.column("buffers_backend").is_none(),
            "PG17 has no buffers_backend"
        );
    }

    #[test]
    fn pre17_roundtrips() {
        crate::assert_roundtrips(&[bgwriter_row(1_000_000), bgwriter_row(2_000_000)]);
    }

    #[test]
    fn pg17_roundtrips() {
        crate::assert_roundtrips(&[checkpointer_row(1_000_000), checkpointer_row(2_000_000)]);
    }

    #[test]
    fn encode_sorts_rows_by_the_sort_key() {
        // Rows given out of `ts` order come back sorted for compression.
        let bytes =
            Bgwriter::encode(&[bgwriter_row(2_000_000), bgwriter_row(1_000_000)]).expect("encode");
        let decoded = Bgwriter::decode(VerifiedSection::for_test(bytes.into())).expect("decode");
        assert_eq!(
            decoded.iter().map(|row| row.ts.0).collect::<Vec<_>>(),
            [1_000_000, 2_000_000]
        );
    }

    #[test]
    fn encode_is_deterministic() {
        let rows = [bgwriter_row(1), bgwriter_row(2)];
        assert_eq!(
            Bgwriter::encode(&rows).expect("a"),
            Bgwriter::encode(&rows).expect("b")
        );
    }

    #[test]
    fn section_is_a_self_contained_parquet_file() {
        let bytes = Bgwriter::encode(&[bgwriter_row(1)]).expect("encode");
        assert_eq!(&bytes[..4], b"PAR1");
        assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
    }

    #[test]
    fn encode_rejects_too_many_rows() {
        let rows = vec![bgwriter_row(0); MAX_SECTION_ROWS + 1];
        assert!(matches!(
            Bgwriter::encode(&rows),
            Err(CodecError::TooManyRows { .. })
        ));
    }

    #[test]
    fn decode_rejects_an_oversized_section() {
        let bytes = vec![0_u8; MAX_SECTION_BYTES + 1];
        assert!(matches!(
            Bgwriter::decode(VerifiedSection::for_test(bytes.into())),
            Err(CodecError::SectionTooLarge { .. })
        ));
    }

    #[test]
    fn typed_decode_rejects_a_schema_that_does_not_match_the_contract() {
        use std::sync::Arc;

        use arrow_array::{ArrayRef, Int64Array, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};
        use parquet::arrow::ArrowWriter;

        // A two-column body is structurally valid Parquet but not this contract;
        // typed decode must reject it rather than gather the columns it knows.
        let schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, false),
            Field::new("checkpoints_timed", DataType::Int64, false),
        ]));
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from_iter_values([1_i64])),
            Arc::new(Int64Array::from_iter_values([10_i64])),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).expect("batch");
        let mut buf = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut buf, schema, None).expect("writer");
        writer.write(&batch).expect("write");
        writer.close().expect("close");

        assert!(matches!(
            Bgwriter::decode(VerifiedSection::for_test(buf.into())),
            Err(CodecError::SchemaMismatch)
        ));
    }
}
