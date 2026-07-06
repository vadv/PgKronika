use crate::buffering::buffer_row;
use crate::config::Config;
use crate::plans_source::PlansRead;
use crate::pool_sources::{user_indexes_type_id, user_tables_type_id};
use crate::statements_source::statements_type_id;
use anyhow::Result;
use kronika_registry::collection_coverage::CollectionCoverageV1;
use kronika_registry::{StrId, Ts};
use kronika_source_pg::statements::{StatementsRow, StatementsVersion};
use kronika_writer::{Interner, SectionBuffers};

/// Counters accumulated while collecting one top-N source, for `1_023_001`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SourceCoverage {
    /// Known lower bound for source rows.
    pub(crate) total: u64,
    /// Rows collected.
    pub(crate) collected: u64,
    /// At least one count failed, so `total` is not exact.
    pub(crate) unknown_total: bool,
    /// Databases skipped after the adaptive timeout hit its cap.
    pub(crate) timeouts: u32,
    /// Databases skipped on a privilege failure (SQLSTATE 42501).
    pub(crate) permission_skips: u32,
    /// Databases skipped for any other error.
    pub(crate) other_skips: u32,
}

impl SourceCoverage {
    /// The `1_023_001` reason code: a timeout outranks a privilege failure,
    /// which outranks other skips; plain top-N selection is the default.
    pub(crate) const fn reason(&self) -> u8 {
        if self.timeouts > 0 {
            1
        } else if self.permission_skips > 0 {
            2
        } else if self.other_skips > 0 || self.unknown_total {
            3
        } else {
            0
        }
    }

    /// Whether any source rows are missing from the section.
    pub(crate) const fn truncated(&self) -> bool {
        self.total > self.collected
            || self.unknown_total
            || self.timeouts > 0
            || self.permission_skips > 0
            || self.other_skips > 0
    }
}

/// One pending `1_023_001` row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CoverageRecord {
    source_type_id: u32,
    coverage: SourceCoverage,
    max_n: u32,
    order_by: &'static str,
    cutoff_value: Option<f64>,
}

/// Inputs needed to assemble coverage for this snapshot's top-N reads.
pub(crate) struct CoverageInputs<'a> {
    pub(crate) tables: SourceCoverage,
    pub(crate) indexes: SourceCoverage,
    pub(crate) statements: &'a Option<(StatementsVersion, Vec<StatementsRow>, u64)>,
    pub(crate) plans: &'a Option<(PlansRead, u64)>,
}

/// Assemble the `1_023_001` rows for every truncated top-N source.
pub(crate) fn collect_coverage_records(
    major: u32,
    config: &Config,
    inputs: &CoverageInputs<'_>,
) -> Vec<CoverageRecord> {
    let mut records = Vec::new();
    if inputs.tables.truncated() {
        records.push(CoverageRecord {
            source_type_id: user_tables_type_id(major),
            coverage: inputs.tables,
            max_n: u32::try_from(config.max_tables).unwrap_or(u32::MAX),
            order_by: "reads|writes|relpages|n_dead_tup|xid_age|mxid_age",
            cutoff_value: None,
        });
    }
    if inputs.indexes.truncated() {
        records.push(CoverageRecord {
            source_type_id: user_indexes_type_id(major),
            coverage: inputs.indexes,
            max_n: u32::try_from(config.max_indexes).unwrap_or(u32::MAX),
            order_by: user_indexes_order_by(major),
            cutoff_value: None,
        });
    }
    if let Some(record) = statements_coverage(config, inputs) {
        records.push(record);
    }
    if let Some(record) = plans_coverage(config, inputs) {
        records.push(record);
    }
    records
}

/// Coverage for the collected `pg_stat_statements` read, if it was truncated.
///
/// The total rides in the same statement as the collected rows, so it
/// describes exactly the population they were cut from.
fn statements_coverage(config: &Config, inputs: &CoverageInputs<'_>) -> Option<CoverageRecord> {
    let (version, rows, source_total) = inputs.statements.as_ref()?;
    let coverage = SourceCoverage {
        total: *source_total,
        collected: rows.len() as u64,
        unknown_total: false,
        timeouts: 0,
        permission_skips: 0,
        other_skips: 0,
    };
    coverage.truncated().then(|| CoverageRecord {
        source_type_id: statements_type_id(*version),
        coverage,
        max_n: u32::try_from(config.max_statements).unwrap_or(u32::MAX),
        order_by: "total_exec_time|calls",
        cutoff_value: None,
    })
}

/// Coverage for the collected `pg_store_plans` read, if it was truncated.
///
/// The single selection axis makes the boundary meaningful: `cutoff_value`
/// is the smallest `total_time` that still made it into the section. The
/// total rides in the enumeration statement itself.
fn plans_coverage(config: &Config, inputs: &CoverageInputs<'_>) -> Option<CoverageRecord> {
    let (read, source_total) = inputs.plans.as_ref()?;
    let (collected, cutoff_value) = match read {
        PlansRead::Vadv(rows) => (
            rows.len() as u64,
            min_total_time(rows.iter().map(|r| r.total_time)),
        ),
        PlansRead::Ossc(rows) => (
            rows.len() as u64,
            min_total_time(rows.iter().map(|r| r.total_time)),
        ),
    };
    let coverage = SourceCoverage {
        total: *source_total,
        collected,
        unknown_total: false,
        timeouts: 0,
        permission_skips: 0,
        other_skips: 0,
    };
    coverage.truncated().then(|| CoverageRecord {
        source_type_id: read.type_id(),
        coverage,
        max_n: u32::try_from(config.max_plans).unwrap_or(u32::MAX),
        order_by: "total_time",
        cutoff_value,
    })
}

/// The smallest selection metric among the collected rows; `None` when empty.
pub(crate) fn min_total_time(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.fold(None, |acc, v| {
        Some(acc.map_or(v, |a: f64| if v < a { v } else { a }))
    })
}

const fn user_indexes_order_by(major: u32) -> &'static str {
    if major >= 16 {
        "idx_scan|idx_tup_read|relpages|last_idx_scan"
    } else {
        "idx_scan|idx_tup_read|relpages"
    }
}

/// Buffer one `1_023_001` row per truncated source.
///
/// # Errors
/// Returns an error if `order_by` cannot be interned (dictionary full) or the
/// section buffer is full.
pub(crate) fn push_coverage(
    buffers: &mut SectionBuffers,
    interner: &mut Interner,
    ts: i64,
    records: &[CoverageRecord],
) -> Result<()> {
    for record in records {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        let row = CollectionCoverageV1 {
            ts: Ts(ts),
            source_type_id: record.source_type_id,
            total: u32::try_from(record.coverage.total).unwrap_or(u32::MAX),
            unknown_total: record.coverage.unknown_total,
            collected: u32::try_from(record.coverage.collected).unwrap_or(u32::MAX),
            max_n: record.max_n,
            order_by: intern(record.order_by.as_bytes())?,
            cutoff_value: record.cutoff_value,
            reason: record.coverage.reason(),
        };
        buffer_row(buffers, row)?;
    }
    Ok(())
}
