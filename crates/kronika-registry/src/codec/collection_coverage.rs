//! Type `1_023_001`: collection coverage for truncated top-N sources.
//!
//! Without it a top-N section reads as complete data. One row per source
//! section truncated in this segment records how many rows the collector could
//! count, how many were written, and why the rest is missing. Coverage does not
//! make the source complete; it tells the reader what part of it the collector
//! saw.

use crate::{Section, StrId, Ts};

/// One row of type `1_023_001`; one truncated source section.
///
/// `reason` encodes why rows are missing: `0` top-N selection, `1` a
/// statement timeout skipped part of the source, `2` insufficient
/// privileges, `3` other skips. `cutoff_value` is the selection metric at
/// the boundary when the selection has a single axis; `None` for a union of
/// axes.
#[derive(Debug, Clone, Copy, PartialEq, Section)]
#[section(
    id = 1_023_001,
    name = "collection_coverage",
    semantics = snapshot_full,
    sort_key("source_type_id", "ts")
)]
pub struct CollectionCoverageV1 {
    /// Collection time, unix microseconds.
    #[column(t)]
    pub ts: Ts,
    /// `type_id` of the truncated section.
    #[column(l)]
    pub source_type_id: u32,
    /// Known lower bound for rows in the source at collection time.
    #[column(g)]
    pub total: u32,
    /// Whether at least one source count failed, so `total` is not exact.
    #[column(l)]
    pub unknown_total: bool,
    /// Rows written into the source section.
    #[column(g)]
    pub collected: u32,
    /// The collector's configured limit (per axis for multi-axis selections).
    #[column(l)]
    pub max_n: u32,
    /// The selection metric or axis union, e.g. `total_time` or
    /// `reads|writes|relpages`.
    #[column(l)]
    pub order_by: StrId,
    /// Selection metric at the boundary; `None` when unknown.
    #[column(g)]
    pub cutoff_value: Option<f64>,
    /// `0` top-N, `1` timeout, `2` permission, `3` other.
    #[column(l)]
    pub reason: u8,
}

#[cfg(test)]
mod tests {
    use super::CollectionCoverageV1;
    use crate::{Section, StrId, Ts, lint};

    fn row(source: u32) -> CollectionCoverageV1 {
        CollectionCoverageV1 {
            ts: Ts(1_000_000),
            source_type_id: source,
            total: 4_000,
            unknown_total: false,
            collected: 500,
            max_n: 500,
            order_by: StrId(1),
            cutoff_value: Some(12.5),
            reason: 0,
        }
    }

    #[test]
    fn contract_passes_the_linter() {
        assert_eq!(lint(&[CollectionCoverageV1::CONTRACT]), Ok(()));
    }

    #[test]
    fn contract_shape() {
        let c = CollectionCoverageV1::CONTRACT;
        assert_eq!(c.type_id.get(), 1_023_001);
        assert_eq!(c.columns.len(), 9);
        assert_eq!(c.sort_key, ["source_type_id", "ts"]);
        assert_eq!(
            c.column("unknown_total").map(|col| col.nullable),
            Some(false)
        );
        assert_eq!(c.column("cutoff_value").map(|col| col.nullable), Some(true));
        assert_eq!(c.column("reason").map(|col| col.nullable), Some(false));
    }

    #[test]
    fn roundtrip_preserves_values_and_nulls() {
        let union_axes = CollectionCoverageV1 {
            source_type_id: 1_013_003,
            cutoff_value: None,
            reason: 1,
            ..row(1_013_003)
        };
        crate::assert_roundtrips(&[row(1_002_006), union_axes]);
    }
}
