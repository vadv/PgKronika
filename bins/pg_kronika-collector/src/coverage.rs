use crate::buffering::buffer_row;
use crate::config::Config;
use crate::plans_source::{PlansRead, PlansSnapshot};
use anyhow::Result;
use kronika_registry::collection_coverage::CollectionCoverageV1;
use kronika_registry::snapshot_coverage::SnapshotCoverageV1;
use kronika_registry::{StrId, Ts};
use kronika_writer::{Interner, SectionBuffers};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn collector_started_at_us() -> i64 {
    static STARTED_AT: OnceLock<i64> = OnceLock::new();
    *STARTED_AT.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_micros()).ok())
            .unwrap_or(0)
    })
}

/// Build immutable provenance for one attempted multi-row snapshot.
pub(crate) fn snapshot_coverage(
    ts: i64,
    section_type_id: u32,
    read_state: u8,
    visibility: u8,
    source_total: u64,
    collected: usize,
) -> SnapshotCoverageV1 {
    SnapshotCoverageV1 {
        ts: Ts(ts),
        section_type_id,
        collector_pid: std::process::id(),
        collector_started_at: Ts(collector_started_at_us()),
        read_state,
        visibility,
        source_total: u32::try_from(source_total).unwrap_or(u32::MAX),
        collected: u32::try_from(collected).unwrap_or(u32::MAX),
    }
}

/// Counters accumulated while collecting one top-N source, for `1_023_001`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SourceCoverage {
    /// A physical source read was started. This distinguishes an empty source
    /// from a source that was not due or was deferred before the read.
    pub(crate) attempted: bool,
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
    /// Collector-side arithmetic or buffering loss made the counters unsafe.
    pub(crate) collector_loss: bool,
}

impl SourceCoverage {
    /// Start one source attempt before reading any physical source.
    pub(crate) const fn new_attempt() -> Self {
        Self {
            attempted: true,
            total: 0,
            collected: 0,
            unknown_total: false,
            timeouts: 0,
            permission_skips: 0,
            other_skips: 0,
            collector_loss: false,
        }
    }

    /// Build one successful attempt from its exact source and output counts.
    pub(crate) fn successful(source_total: u64, collected: usize) -> Self {
        let mut coverage = Self::new_attempt();
        coverage.record_success(collected, source_total);
        coverage
    }

    /// Add one successfully read source partition with checked accumulation.
    pub(crate) fn record_success(&mut self, collected: usize, source_total: u64) {
        self.attempted = true;
        let collected = u64::try_from(collected).unwrap_or_else(|_| {
            self.collector_loss = true;
            u64::MAX
        });
        self.total = self.total.checked_add(source_total).unwrap_or_else(|| {
            self.collector_loss = true;
            u64::MAX
        });
        self.collected = self.collected.checked_add(collected).unwrap_or_else(|| {
            self.collector_loss = true;
            u64::MAX
        });
        if self.collector_loss {
            self.unknown_total = true;
        }
    }

    /// Record one source partition lost to `statement_timeout`.
    pub(crate) const fn record_timeout(&mut self) {
        self.attempted = true;
        self.unknown_total = true;
        Self::increment_failure(&mut self.timeouts, &mut self.collector_loss);
    }

    /// Record one source partition that could not be read due to permissions.
    pub(crate) const fn record_permission_failure(&mut self) {
        self.attempted = true;
        self.unknown_total = true;
        Self::increment_failure(&mut self.permission_skips, &mut self.collector_loss);
    }

    /// Record restricted visibility when the source total remains exact.
    pub(crate) const fn record_permission_restriction(&mut self) {
        self.attempted = true;
        Self::increment_failure(&mut self.permission_skips, &mut self.collector_loss);
    }

    /// Record one source partition lost to another read failure.
    pub(crate) const fn record_other_failure(&mut self) {
        self.attempted = true;
        self.unknown_total = true;
        Self::increment_failure(&mut self.other_skips, &mut self.collector_loss);
    }

    /// Record rows made unreachable by a collector-side cap or loss.
    pub(crate) const fn record_collector_loss(&mut self) {
        self.attempted = true;
        self.unknown_total = true;
        self.collector_loss = true;
    }

    const fn increment_failure(counter: &mut u32, collector_loss: &mut bool) {
        if let Some(next) = counter.checked_add(1) {
            *counter = next;
        } else {
            *collector_loss = true;
        }
    }

    const fn exceeds_wire_bounds(self) -> bool {
        self.total > u32::MAX as u64 || self.collected > u32::MAX as u64
    }

    const fn has_collector_loss(self) -> bool {
        self.collector_loss || self.exceeds_wire_bounds() || self.collected > self.total
    }

    /// Canonical `SnapshotCoverageV1` state and visibility.
    ///
    /// Priority is collector loss, read failure, permission, source limit,
    /// complete.
    pub(crate) const fn read_state(self) -> (u8, u8) {
        if self.has_collector_loss() {
            (4, 2)
        } else if self.timeouts > 0 || self.other_skips > 0 {
            (3, 2)
        } else if self.permission_skips > 0 {
            (2, 1)
        } else if self.unknown_total {
            (3, 2)
        } else if self.total > self.collected {
            (1, 0)
        } else {
            (0, 0)
        }
    }

    /// Exact source total, or `None` when no attempt or any count is unknown.
    pub(crate) const fn exact_total(self) -> Option<u64> {
        if !self.attempted || self.unknown_total || self.has_collector_loss() {
            None
        } else {
            Some(self.total)
        }
    }

    /// Encode the canonical attempt marker. Callers emit it only when
    /// [`Self::attempted`] is true.
    pub(crate) fn snapshot_marker(self, ts: i64, section_type_id: u32) -> SnapshotCoverageV1 {
        let (read_state, visibility) = self.read_state();
        snapshot_coverage(
            ts,
            section_type_id,
            read_state,
            visibility,
            self.total,
            usize::try_from(self.collected).unwrap_or(usize::MAX),
        )
    }

    /// The `1_023_001` reason code: a timeout outranks a privilege failure,
    /// which outranks other skips; plain top-N selection is the default.
    pub(crate) const fn reason(&self) -> u8 {
        if self.timeouts > 0 {
            1
        } else if self.permission_skips > 0 {
            2
        } else if self.other_skips > 0 || self.unknown_total || self.has_collector_loss() {
            3
        } else {
            0
        }
    }

    /// Whether any source rows are missing from the section.
    pub(crate) const fn truncated(&self) -> bool {
        self.attempted && self.read_state().0 != 0
    }
}

/// Coverage facts for one physical section attempt.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CoverageAttempt {
    pub(crate) ts: i64,
    pub(crate) section_type_id: u32,
    pub(crate) coverage: SourceCoverage,
}

impl CoverageAttempt {
    /// Describe a source that belongs to this cycle but was not read.
    pub(crate) const fn not_attempted(ts: i64, section_type_id: u32) -> Self {
        Self {
            ts,
            section_type_id,
            coverage: SourceCoverage {
                attempted: false,
                total: 0,
                collected: 0,
                unknown_total: false,
                timeouts: 0,
                permission_skips: 0,
                other_skips: 0,
                collector_loss: false,
            },
        }
    }
}

/// Build a typed failed-query attempt from its SQLSTATE, if available.
pub(crate) fn query_failure_attempt(
    ts: i64,
    section_type_id: u32,
    sqlstate: Option<&str>,
) -> CoverageAttempt {
    let mut coverage = SourceCoverage::new_attempt();
    match sqlstate {
        Some("42501") => coverage.record_permission_failure(),
        Some("57014") => coverage.record_timeout(),
        _ => coverage.record_other_failure(),
    }
    CoverageAttempt {
        ts,
        section_type_id,
        coverage,
    }
}

/// Retain the highest-priority attempt when several candidate connections fail.
pub(crate) fn prefer_attempt(current: &mut Option<CoverageAttempt>, candidate: CoverageAttempt) {
    if current
        .as_ref()
        .is_none_or(|current| candidate.coverage.read_state().0 > current.coverage.read_state().0)
    {
        *current = Some(candidate);
    }
}

/// One pending `1_023_001` row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CoverageRecord {
    ts: i64,
    section_type_id: u32,
    coverage: SourceCoverage,
    max_n: u32,
    order_by: &'static str,
    cutoff_value: Option<f64>,
}

/// Inputs needed to assemble coverage for this snapshot's top-N reads.
pub(crate) struct CoverageInputs<'a> {
    pub(crate) tables: CoverageAttempt,
    pub(crate) indexes: CoverageAttempt,
    pub(crate) statements: Option<CoverageAttempt>,
    pub(crate) plans: Option<CoverageAttempt>,
    pub(crate) plans_read: &'a Option<PlansSnapshot>,
}

/// Assemble the `1_023_001` rows for every truncated top-N source.
pub(crate) fn collect_coverage_records(
    major: u32,
    config: &Config,
    inputs: &CoverageInputs<'_>,
) -> Vec<CoverageRecord> {
    let mut records = Vec::new();
    if inputs.tables.coverage.truncated() {
        records.push(CoverageRecord {
            ts: inputs.tables.ts,
            section_type_id: inputs.tables.section_type_id,
            coverage: inputs.tables.coverage,
            max_n: u32::try_from(config.max_tables).unwrap_or(u32::MAX),
            order_by: "reads|writes|relpages|n_dead_tup|xid_age|mxid_age",
            cutoff_value: None,
        });
    }
    if inputs.indexes.coverage.truncated() {
        records.push(CoverageRecord {
            ts: inputs.indexes.ts,
            section_type_id: inputs.indexes.section_type_id,
            coverage: inputs.indexes.coverage,
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

/// Coverage for a non-complete typed `pg_stat_statements` attempt.
///
/// The total rides in the same statement as the collected rows, so it
/// describes exactly the population they were cut from.
fn statements_coverage(config: &Config, inputs: &CoverageInputs<'_>) -> Option<CoverageRecord> {
    let attempt = inputs.statements?;
    let coverage = attempt.coverage;
    coverage.truncated().then(|| CoverageRecord {
        ts: attempt.ts,
        section_type_id: attempt.section_type_id,
        coverage,
        max_n: u32::try_from(config.max_statements).unwrap_or(u32::MAX),
        order_by: "total_exec_time|calls",
        cutoff_value: None,
    })
}

/// Coverage for a non-complete typed `pg_store_plans` attempt.
///
/// The single selection axis makes the boundary meaningful: `cutoff_value`
/// For successful reads, the single selection axis makes the boundary
/// meaningful: `cutoff_value` is the smallest `total_time` that still made it
/// into the section. Failed attempts have no boundary.
fn plans_coverage(config: &Config, inputs: &CoverageInputs<'_>) -> Option<CoverageRecord> {
    let attempt = inputs.plans?;
    let coverage = attempt.coverage;
    let cutoff_value = inputs
        .plans_read
        .as_ref()
        .and_then(|snapshot| match &snapshot.read {
            PlansRead::Vadv(rows) => min_total_time(rows.iter().map(|r| r.total_time)),
            PlansRead::Ossc(rows) => min_total_time(rows.iter().map(|r| r.total_time)),
        });
    coverage.truncated().then(|| CoverageRecord {
        ts: attempt.ts,
        section_type_id: attempt.section_type_id,
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
    records: &[CoverageRecord],
) -> Result<()> {
    for record in records {
        let mut intern = |bytes: &[u8]| interner.intern(bytes).map(|id| StrId(id.get()));
        let row = CollectionCoverageV1 {
            ts: Ts(record.ts),
            section_type_id: record.section_type_id,
            total: u32::try_from(record.coverage.total).unwrap_or(u32::MAX),
            unknown_total: record.coverage.exact_total().is_none(),
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

#[cfg(test)]
mod snapshot_tests {
    use super::snapshot_coverage;

    #[test]
    fn marker_keeps_complete_and_failure_states_distinct() {
        let complete = snapshot_coverage(10, 1_001_003, 0, 0, 12, 12);
        let failed = snapshot_coverage(20, 1_001_003, 3, 2, 0, 0);
        assert_eq!((complete.read_state, complete.visibility), (0, 0));
        assert_eq!((complete.source_total, complete.collected), (12, 12));
        assert_eq!((failed.read_state, failed.visibility), (3, 2));
        assert_eq!(complete.collector_pid, failed.collector_pid);
        assert_eq!(complete.collector_started_at, failed.collector_started_at);
    }
}
