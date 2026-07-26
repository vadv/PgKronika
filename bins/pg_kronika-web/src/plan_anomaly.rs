//! Query-plan anomaly evidence over stored `pg_store_plans` snapshots.
//!
//! The adapter keeps PostgreSQL-specific applicability, snapshot completeness,
//! reset, version, and identity rules outside the source-independent analytics
//! kernels. Both detectors are retrospective: each current window is compared
//! with the rest of its continuous selected-period segment.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::Excluded;

use kronika_analytics::{
    CategoryCount, ChangeNotEvaluatedReason, DistributionEvidence, DistributionOutcome,
    DistributionParams, PerUnitEvidence, PerUnitOutcome, PerUnitParams, WorkTotals,
    compare_distributions, compare_per_unit,
};
use kronika_reader::{DiffPoint, OutRow, Scalar, SectionPage, SeriesDiff, Value};
use serde_json::{Value as JsonValue, json};

use crate::anomaly::{ScanParams, positions};

/// Stable machine id for call-normalized query plan-mixture changes.
pub(crate) const PLAN_DISTRIBUTION_SIGNAL_ID: &str = "pg.query.plan_distribution_shift.v1";
/// Stable machine id for same-plan buffer work per call increases.
pub(crate) const PLAN_BUFFER_SIGNAL_ID: &str = "pg.plan.buffer_work_per_call_increase.v1";

/// Supporting storage sections decoded once when a plan section is requested.
pub(crate) const PLAN_CONTEXT_SECTIONS: [&str; 4] = [
    "snapshot_coverage",
    "collection_coverage",
    "reset_metadata",
    "instance_metadata",
];

const OSSC_TYPE_ID: u32 = 1_003_001;
const VADV_TYPE_ID: u32 = 1_004_001;
const MAX_PLAN_SHARE_EVIDENCE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanFamily {
    Ossc,
    Vadv,
}

impl PlanFamily {
    const fn from_section(section: &str) -> Option<Self> {
        match section.as_bytes() {
            b"pg_store_plans_ossc" => Some(Self::Ossc),
            b"pg_store_plans_vadv" => Some(Self::Vadv),
            _ => None,
        }
    }

    const fn section(self) -> &'static str {
        match self {
            Self::Ossc => "pg_store_plans_ossc",
            Self::Vadv => "pg_store_plans_vadv",
        }
    }

    const fn type_id(self) -> u32 {
        match self {
            Self::Ossc => OSSC_TYPE_ID,
            Self::Vadv => VADV_TYPE_ID,
        }
    }

    const fn wire_name(self) -> &'static str {
        match self {
            Self::Ossc => "ossc",
            Self::Vadv => "vadv",
        }
    }

    const fn supports_query_distribution(self) -> bool {
        matches!(self, Self::Ossc)
    }

    fn supports_extension_version(self, version: &str) -> bool {
        match self {
            Self::Ossc => version.starts_with("1."),
            Self::Vadv => version.starts_with("2."),
        }
    }
}

/// Whether one logical section carries a specialized plan detector.
pub(crate) const fn is_plan_section(section: &str) -> bool {
    PlanFamily::from_section(section).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct QueryIdentity {
    dbid: u64,
    userid: u64,
    queryid: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PlanIdentity {
    dbid: u64,
    userid: u64,
    queryid: Option<i64>,
    planid: i64,
}

impl PlanIdentity {
    const fn query(self) -> Option<QueryIdentity> {
        match self.queryid {
            Some(queryid) => Some(QueryIdentity {
                dbid: self.dbid,
                userid: self.userid,
                queryid,
            }),
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotCoverage {
    read_state: u8,
    visibility: u8,
    source_total: u64,
    collected: u64,
}

impl SnapshotCoverage {
    const fn is_full(self) -> bool {
        self.read_state == 0 && self.visibility == 0 && self.source_total == self.collected
    }

    const fn is_retained_row_usable(self) -> bool {
        self.visibility == 0
            && ((self.read_state == 0 && self.source_total == self.collected)
                || (self.read_state == 1 && self.source_total > self.collected))
    }

    const fn is_truncated(self) -> bool {
        self.visibility == 0 && (self.read_state == 1 || self.source_total > self.collected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResetContext {
    ts: i64,
    plan_reset_at: Option<i64>,
    extension_version: Option<String>,
    compute_query_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceContext {
    ts: i64,
    node_self_id: Option<String>,
    pg_version_num: Option<i64>,
    system_identifier: Option<i64>,
}

/// Parsed provenance shared by both plan section scans.
#[derive(Debug, Default, Clone)]
pub(crate) struct PlanContext {
    coverage: BTreeMap<u32, BTreeMap<i64, SnapshotCoverage>>,
    conflicting_coverage: BTreeSet<(u32, i64)>,
    top_n_coverage: BTreeSet<(u32, i64)>,
    reset: Vec<ResetContext>,
    instance: Vec<InstanceContext>,
    reset_gaps: Vec<(i64, i64)>,
    instance_gaps: Vec<(i64, i64)>,
    pages_incomplete: bool,
}

impl PlanContext {
    /// Parse the four support pages. Missing or paged support stays explicit.
    #[must_use]
    pub(crate) fn from_pages(pages: &BTreeMap<String, SectionPage>) -> Self {
        let mut context = Self::default();
        for name in PLAN_CONTEXT_SECTIONS {
            let Some(page) = pages.get(name) else {
                context.pages_incomplete = true;
                continue;
            };
            context.pages_incomplete |= page.next_cursor.is_some();
            match name {
                "snapshot_coverage" => context.parse_snapshot_coverage(page),
                "collection_coverage" => context.parse_collection_coverage(page),
                "reset_metadata" => {
                    context.reset_gaps = gaps(page);
                    context.parse_reset(page);
                }
                "instance_metadata" => {
                    context.instance_gaps = gaps(page);
                    context.parse_instance(page);
                }
                _ => unreachable!("PLAN_CONTEXT_SECTIONS is closed"),
            }
        }
        context.reset.sort_by_key(|row| row.ts);
        context.instance.sort_by_key(|row| row.ts);
        context
    }

    fn parse_snapshot_coverage(&mut self, page: &SectionPage) {
        for row in &page.rows {
            let Some((ts, type_id, coverage)) = snapshot_coverage_row(row) else {
                continue;
            };
            let by_ts = self.coverage.entry(type_id).or_default();
            if by_ts
                .insert(ts, coverage)
                .is_some_and(|previous| previous != coverage)
            {
                self.conflicting_coverage.insert((type_id, ts));
            }
        }
    }

    fn parse_collection_coverage(&mut self, page: &SectionPage) {
        for row in &page.rows {
            let (Some(ts), Some(type_id), Some(reason)) = (
                timestamp(row, "ts"),
                unsigned(row, "source_type_id").and_then(|value| u32::try_from(value).ok()),
                unsigned(row, "reason"),
            ) else {
                continue;
            };
            if reason == 0 {
                self.top_n_coverage.insert((type_id, ts));
            }
        }
    }

    fn parse_reset(&mut self, page: &SectionPage) {
        for row in &page.rows {
            let Some(ts) = timestamp(row, "ts") else {
                continue;
            };
            self.reset.push(ResetContext {
                ts,
                plan_reset_at: timestamp(row, "pg_store_plans_reset_at"),
                extension_version: text(row, "ext_pg_store_plans_version"),
                compute_query_id: text(row, "compute_query_id"),
            });
        }
    }

    fn parse_instance(&mut self, page: &SectionPage) {
        for row in &page.rows {
            let Some(ts) = timestamp(row, "ts") else {
                continue;
            };
            self.instance.push(InstanceContext {
                ts,
                node_self_id: text(row, "node_self_id"),
                pg_version_num: signed(row, "pg_version_num"),
                system_identifier: signed(row, "pg_system_identifier"),
            });
        }
    }

    fn coverage_at(&self, family: PlanFamily, ts: i64) -> Option<SnapshotCoverage> {
        (!self.conflicting_coverage.contains(&(family.type_id(), ts)))
            .then(|| {
                self.coverage
                    .get(&family.type_id())
                    .and_then(|by_ts| by_ts.get(&ts))
                    .copied()
            })
            .flatten()
    }

    fn has_top_n_coverage(&self, family: PlanFamily) -> bool {
        self.top_n_coverage
            .iter()
            .any(|&(type_id, _)| type_id == family.type_id())
    }

    fn extension_family_seen(&self, family: PlanFamily) -> bool {
        self.reset.iter().any(|row| {
            row.extension_version
                .as_deref()
                .is_some_and(|version| family.supports_extension_version(version))
        })
    }

    fn reset_at(&self, ts: i64) -> Option<&ResetContext> {
        latest_before(&self.reset, ts, |row| row.ts)
            .filter(|row| !spans_gap(row.ts, ts, &self.reset_gaps))
    }

    fn instance_at(&self, ts: i64) -> Option<&InstanceContext> {
        latest_before(&self.instance, ts, |row| row.ts)
            .filter(|row| !spans_gap(row.ts, ts, &self.instance_gaps))
    }
}

fn latest_before<T>(rows: &[T], ts: i64, row_ts: impl Fn(&T) -> i64) -> Option<&T> {
    let end = rows.partition_point(|row| row_ts(row) <= ts);
    end.checked_sub(1).and_then(|index| rows.get(index))
}

fn spans_gap(from: i64, to: i64, gaps: &[(i64, i64)]) -> bool {
    gaps.iter()
        .any(|&(gap_from, gap_to)| gap_from < to && from < gap_to)
}

fn gaps(page: &SectionPage) -> Vec<(i64, i64)> {
    page.gaps.iter().map(|gap| (gap.from, gap.to)).collect()
}

fn snapshot_coverage_row(row: &OutRow) -> Option<(i64, u32, SnapshotCoverage)> {
    let ts = timestamp(row, "ts")?;
    let type_id = u32::try_from(unsigned(row, "source_type_id")?).ok()?;
    let read_state = u8::try_from(unsigned(row, "read_state")?).ok()?;
    let visibility = u8::try_from(unsigned(row, "visibility")?).ok()?;
    Some((
        ts,
        type_id,
        SnapshotCoverage {
            read_state,
            visibility,
            source_total: unsigned(row, "source_total")?,
            collected: unsigned(row, "collected")?,
        },
    ))
}

fn column<'a>(row: &'a OutRow, name: &str) -> Option<&'a Value> {
    row.iter()
        .find(|(column, _)| column == name)
        .map(|(_, value)| value)
}

fn timestamp(row: &OutRow, name: &str) -> Option<i64> {
    match column(row, name)? {
        Value::Ts(value) => Some(*value),
        _ => None,
    }
}

fn unsigned(row: &OutRow, name: &str) -> Option<u64> {
    match column(row, name)? {
        Value::U64(value) => Some(*value),
        Value::I64(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

fn signed(row: &OutRow, name: &str) -> Option<i64> {
    match column(row, name)? {
        Value::I64(value) => Some(*value),
        Value::U64(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn text(row: &OutRow, name: &str) -> Option<String> {
    match column(row, name)? {
        Value::Str(value) => Some(value.clone()),
        _ => None,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DetectorCounts {
    candidates: u64,
    positions: u64,
    evaluated: u64,
    stable: u64,
    changed: u64,
    reference_too_small: u64,
    current_too_small: u64,
    count_overflow: u64,
    discontinuity: u64,
    not_applicable: u64,
}

impl DetectorCounts {
    fn to_json(self) -> JsonValue {
        json!({
            "candidates": self.candidates,
            "positions": self.positions,
            "evaluated": self.evaluated,
            "stable": self.stable,
            "changed": self.changed,
            "not_evaluated": {
                "reference_too_small": self.reference_too_small,
                "current_too_small": self.current_too_small,
                "count_overflow": self.count_overflow,
                "discontinuity": self.discontinuity,
                "not_applicable": self.not_applicable,
            },
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct QualityCounts {
    snapshot_rows: u64,
    snapshots_total: u64,
    full_snapshots: u64,
    truncated_snapshots: u64,
    restricted_or_failed_snapshots: u64,
    coverage_unknown_snapshots: u64,
    collection_coverage_missing: u64,
    invalid_rows: u64,
    membership_boundaries: u64,
    counter_epoch_boundaries: u64,
    reset_boundaries: u64,
    metadata_unknown_intervals: u64,
    extension_version_boundaries: u64,
    instance_boundaries: u64,
    instance_identity_fallback_intervals: u64,
    unsupported_version_intervals: u64,
    query_id_disabled_intervals: u64,
    invalid_counter_intervals: u64,
}

impl QualityCounts {
    fn to_json(self) -> JsonValue {
        json!({
            "snapshot_rows": self.snapshot_rows,
            "snapshots_total": self.snapshots_total,
            "full_snapshots": self.full_snapshots,
            "truncated_snapshots": self.truncated_snapshots,
            "restricted_or_failed_snapshots": self.restricted_or_failed_snapshots,
            "coverage_unknown_snapshots": self.coverage_unknown_snapshots,
            "collection_coverage_missing": self.collection_coverage_missing,
            "invalid_rows": self.invalid_rows,
            "membership_boundaries": self.membership_boundaries,
            "counter_epoch_boundaries": self.counter_epoch_boundaries,
            "reset_boundaries": self.reset_boundaries,
            "metadata_unknown_intervals": self.metadata_unknown_intervals,
            "extension_version_boundaries": self.extension_version_boundaries,
            "instance_boundaries": self.instance_boundaries,
            "instance_identity_fallback_intervals": self.instance_identity_fallback_intervals,
            "unsupported_version_intervals": self.unsupported_version_intervals,
            "query_id_disabled_intervals": self.query_id_disabled_intervals,
            "invalid_counter_intervals": self.invalid_counter_intervals,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuityFailure {
    MetadataUnknown,
    UnsupportedVersion,
    ExtensionVersion,
    Instance,
    Reset,
    QueryIdDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Continuity {
    reset_marker: &'static str,
    instance_identity: &'static str,
}

fn continuity(
    context: &PlanContext,
    family: PlanFamily,
    previous: i64,
    current: i64,
    require_query_id: bool,
) -> Result<Continuity, ContinuityFailure> {
    let previous_reset = context
        .reset_at(previous)
        .ok_or(ContinuityFailure::MetadataUnknown)?;
    let current_reset = context
        .reset_at(current)
        .ok_or(ContinuityFailure::MetadataUnknown)?;
    let (Some(previous_version), Some(current_version)) = (
        previous_reset.extension_version.as_deref(),
        current_reset.extension_version.as_deref(),
    ) else {
        return Err(ContinuityFailure::MetadataUnknown);
    };
    if !family.supports_extension_version(previous_version)
        || !family.supports_extension_version(current_version)
    {
        return Err(ContinuityFailure::UnsupportedVersion);
    }
    if previous_version != current_version {
        return Err(ContinuityFailure::ExtensionVersion);
    }

    if require_query_id {
        let query_id_enabled = |value: Option<&str>| {
            value.is_some_and(|value| {
                value.eq_ignore_ascii_case("auto")
                    || value.eq_ignore_ascii_case("on")
                    || value.eq_ignore_ascii_case("regress")
            })
        };
        if !query_id_enabled(previous_reset.compute_query_id.as_deref())
            || !query_id_enabled(current_reset.compute_query_id.as_deref())
        {
            return Err(ContinuityFailure::QueryIdDisabled);
        }
    }

    let reset_marker = match (previous_reset.plan_reset_at, current_reset.plan_reset_at) {
        (Some(previous), Some(current)) if previous == current => "reset_metadata",
        (None, None) if family == PlanFamily::Vadv => "entry_first_call",
        (None, None) => return Err(ContinuityFailure::MetadataUnknown),
        (Some(_) | None, Some(_)) | (Some(_), None) => {
            return Err(ContinuityFailure::Reset);
        }
    };

    let previous_instance = context
        .instance_at(previous)
        .ok_or(ContinuityFailure::MetadataUnknown)?;
    let current_instance = context
        .instance_at(current)
        .ok_or(ContinuityFailure::MetadataUnknown)?;
    let (Some(previous_node), Some(current_node)) = (
        previous_instance.node_self_id.as_deref(),
        current_instance.node_self_id.as_deref(),
    ) else {
        return Err(ContinuityFailure::MetadataUnknown);
    };
    let (Some(previous_version_num), Some(current_version_num)) = (
        previous_instance.pg_version_num,
        current_instance.pg_version_num,
    ) else {
        return Err(ContinuityFailure::MetadataUnknown);
    };
    let previous_major = previous_version_num / 10_000;
    let current_major = current_version_num / 10_000;
    if !(15..=18).contains(&previous_major) || !(15..=18).contains(&current_major) {
        return Err(ContinuityFailure::UnsupportedVersion);
    }
    if previous_major != current_major || previous_node != current_node {
        return Err(ContinuityFailure::Instance);
    }
    let instance_identity = match (
        previous_instance.system_identifier,
        current_instance.system_identifier,
    ) {
        (Some(previous), Some(current)) if previous == current => "pg_system_identifier",
        (None, None) => "source_node_fallback",
        (Some(_) | None, Some(_)) | (Some(_), None) => {
            return Err(ContinuityFailure::Instance);
        }
    };
    Ok(Continuity {
        reset_marker,
        instance_identity,
    })
}

const fn tally_continuity_failure(quality: &mut QualityCounts, failure: ContinuityFailure) {
    match failure {
        ContinuityFailure::MetadataUnknown => quality.metadata_unknown_intervals += 1,
        ContinuityFailure::UnsupportedVersion => quality.unsupported_version_intervals += 1,
        ContinuityFailure::ExtensionVersion => quality.extension_version_boundaries += 1,
        ContinuityFailure::Instance => quality.instance_boundaries += 1,
        ContinuityFailure::Reset => quality.reset_boundaries += 1,
        ContinuityFailure::QueryIdDisabled => quality.query_id_disabled_intervals += 1,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PlanShareEvidence {
    planid: i64,
    reference_calls: u64,
    current_calls: u64,
    reference_share: f64,
    current_share: f64,
    share_delta: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct DistributionPeak {
    reference_calls: u64,
    current_calls: u64,
    total_variation: f64,
    max_abs_share_delta: f64,
    plans_total: usize,
    plans_omitted: usize,
    plans: Vec<PlanShareEvidence>,
}

impl DistributionPeak {
    fn from_evidence(evidence: &DistributionEvidence) -> Self {
        let mut categories = evidence.categories.clone();
        categories.sort_by(|left, right| {
            right
                .share_delta
                .abs()
                .total_cmp(&left.share_delta.abs())
                .then_with(|| left.category.cmp(&right.category))
        });
        let plans_total = categories.len();
        categories.truncate(MAX_PLAN_SHARE_EVIDENCE);
        let plans = categories
            .into_iter()
            .map(|category| PlanShareEvidence {
                planid: category.category,
                reference_calls: category.reference_count,
                current_calls: category.current_count,
                reference_share: category.reference_share,
                current_share: category.current_share,
                share_delta: category.share_delta,
            })
            .collect();
        Self {
            reference_calls: evidence.reference_total,
            current_calls: evidence.current_total,
            total_variation: evidence.total_variation,
            max_abs_share_delta: evidence.max_abs_share_delta,
            plans_total,
            plans_omitted: plans_total.saturating_sub(MAX_PLAN_SHARE_EVIDENCE),
            plans,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DistributionHit {
    query: QueryIdentity,
    start: i64,
    end: i64,
    peak_ts: i64,
    severity: f64,
    evidence: DistributionPeak,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BufferHit {
    family: PlanFamily,
    plan: PlanIdentity,
    dimension: BufferDimension,
    start: i64,
    end: i64,
    peak_ts: i64,
    severity: f64,
    evidence: PerUnitEvidence,
    reset_marker: &'static str,
    instance_identity: &'static str,
}

/// One retained specialized plan signal.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PlanSignal {
    Distribution(DistributionHit),
    Buffer(BufferHit),
}

impl PlanSignal {
    const fn signal_id(&self) -> &'static str {
        match self {
            Self::Distribution(_) => PLAN_DISTRIBUTION_SIGNAL_ID,
            Self::Buffer(_) => PLAN_BUFFER_SIGNAL_ID,
        }
    }

    const fn severity(&self) -> f64 {
        match self {
            Self::Distribution(hit) => hit.severity,
            Self::Buffer(hit) => hit.severity,
        }
    }

    const fn peak_ts(&self) -> i64 {
        match self {
            Self::Distribution(hit) => hit.peak_ts,
            Self::Buffer(hit) => hit.peak_ts,
        }
    }

    /// Serialize stable, locale-neutral parameters and evidence.
    pub(crate) fn to_json(&self, scan: &ScanParams) -> JsonValue {
        match self {
            Self::Distribution(hit) => distribution_to_json(hit, scan),
            Self::Buffer(hit) => buffer_to_json(hit, scan),
        }
    }
}

fn signal_order(left: &PlanSignal, right: &PlanSignal) -> Ordering {
    right
        .severity()
        .total_cmp(&left.severity())
        .then_with(|| left.signal_id().cmp(right.signal_id()))
        .then_with(|| left.peak_ts().cmp(&right.peak_ts()))
        .then_with(|| match (left, right) {
            (PlanSignal::Distribution(left), PlanSignal::Distribution(right)) => {
                left.query.cmp(&right.query)
            }
            (PlanSignal::Buffer(left), PlanSignal::Buffer(right)) => left
                .plan
                .cmp(&right.plan)
                .then_with(|| left.dimension.column.cmp(right.dimension.column)),
            (PlanSignal::Distribution(_), PlanSignal::Buffer(_))
            | (PlanSignal::Buffer(_), PlanSignal::Distribution(_)) => Ordering::Equal,
        })
}

/// Deterministically rank plan signals and return the number removed.
pub(crate) fn rank_plan_signals(signals: &mut Vec<PlanSignal>, limit: usize) -> usize {
    signals.sort_by(signal_order);
    let removed = signals.len().saturating_sub(limit);
    signals.truncate(limit);
    removed
}

fn distribution_to_json(hit: &DistributionHit, scan: &ScanParams) -> JsonValue {
    let parameters = DistributionParams::default();
    let plans: Vec<_> = hit
        .evidence
        .plans
        .iter()
        .map(|plan| {
            json!({
                "planid": plan.planid,
                "reference_calls": plan.reference_calls,
                "current_calls": plan.current_calls,
                "reference_share": plan.reference_share,
                "current_share": plan.current_share,
                "share_delta": plan.share_delta,
            })
        })
        .collect();
    json!({
        "signal_id": PLAN_DISTRIBUTION_SIGNAL_ID,
        "section": PlanFamily::Ossc.section(),
        "kind": "plan_distribution_shift",
        "scope": {
            "dbid": hit.query.dbid,
            "userid": hit.query.userid,
            "queryid": hit.query.queryid,
            "query_identity": "dbid_userid_core_queryid",
            "query_text_used": false,
        },
        "start": hit.start,
        "end": hit.end,
        "peak_ts": hit.peak_ts,
        "severity": hit.severity,
        "parameters": {
            "reference_model": "rest_of_continuous_period",
            "retrospective": true,
            "window_us": scan.window,
            "step_us": scan.step,
            "count_basis": "calls_delta",
            "min_reference_calls": parameters.reference_count,
            "min_current_calls": parameters.current_count,
            "min_total_variation": parameters.total_variation,
        },
        "coverage": {
            "complete_for_evidence": true,
            "population": "full_pg_store_plans_snapshots",
            "queryid_applicability": "exact_identity",
        },
        "evidence": {
            "reference_calls": hit.evidence.reference_calls,
            "current_calls": hit.evidence.current_calls,
            "total_variation": hit.evidence.total_variation,
            "max_abs_share_delta": hit.evidence.max_abs_share_delta,
            "plans_total": hit.evidence.plans_total,
            "plans_omitted": hit.evidence.plans_omitted,
            "plans": plans,
        },
        "interpretation": "observed_distribution_change",
    })
}

fn buffer_to_json(hit: &BufferHit, scan: &ScanParams) -> JsonValue {
    let parameters = PerUnitParams::default();
    json!({
        "signal_id": PLAN_BUFFER_SIGNAL_ID,
        "section": hit.family.section(),
        "kind": "buffer_work_per_call_increase",
        "scope": {
            "dbid": hit.plan.dbid,
            "userid": hit.plan.userid,
            "queryid": hit.plan.queryid,
            "planid": hit.plan.planid,
            "plan_identity": match hit.family {
                PlanFamily::Ossc => "dbid_userid_queryid_planid",
                PlanFamily::Vadv => "dbid_userid_planid",
            },
            "query_attribution": match hit.family {
                PlanFamily::Ossc => "exact_identity",
                PlanFamily::Vadv => "unavailable",
            },
        },
        "dimension": {
            "buffer_class": hit.dimension.buffer_class,
            "operation": hit.dimension.operation,
            "column": hit.dimension.column,
            "unit": "blocks_per_call",
        },
        "start": hit.start,
        "end": hit.end,
        "peak_ts": hit.peak_ts,
        "severity": hit.severity,
        "parameters": {
            "reference_model": "rest_of_continuous_period",
            "retrospective": true,
            "window_us": scan.window,
            "step_us": scan.step,
            "normalization": "calls_delta",
            "min_reference_calls": parameters.reference_operations,
            "min_current_calls": parameters.current_operations,
            "min_absolute_increase_blocks_per_call": parameters.absolute_increase,
            "min_relative_increase": parameters.relative_increase,
        },
        "coverage": {
            "complete_for_evidence": true,
            "retained_plan_continuity": "every_observed_plan_snapshot",
            "reset_marker": hit.reset_marker,
            "instance_identity": hit.instance_identity,
            "parallelism": "extension_execution_aggregate_not_worker_normalized",
        },
        "evidence": {
            "reference_calls": hit.evidence.reference.operations,
            "reference_blocks": hit.evidence.reference.work,
            "current_calls": hit.evidence.current.operations,
            "current_blocks": hit.evidence.current.work,
            "reference_blocks_per_call": hit.evidence.reference_per_unit,
            "current_blocks_per_call": hit.evidence.current_per_unit,
            "delta_blocks_per_call": hit.evidence.delta_per_unit,
            "relative_delta": hit.evidence.relative_delta,
            "absolute_effect_met": hit.evidence.absolute_effect_met,
            "relative_effect_met": hit.evidence.relative_effect_met,
        },
        "interpretation": "observed_same_plan_association_not_causation",
    })
}

#[derive(Debug, Clone)]
pub(crate) struct PlanScan {
    family: PlanFamily,
    source_present: bool,
    complete: bool,
    work: usize,
    work_required: usize,
    work_available: usize,
    quality: QualityCounts,
    distribution: DetectorCounts,
    buffers: DetectorCounts,
    signals: Vec<PlanSignal>,
    signals_truncated: u64,
}

impl PlanScan {
    /// Scoring work actually performed after preflight.
    pub(crate) const fn work(&self) -> usize {
        self.work
    }

    /// Whether all applicable plan analysis work and population coverage were complete.
    pub(crate) const fn complete(&self) -> bool {
        self.complete
    }

    /// Move retained signals into the request-global ranker.
    pub(crate) fn take_signals(&mut self) -> Vec<PlanSignal> {
        std::mem::take(&mut self.signals)
    }

    /// Signals dropped by this section-local bounded ranker.
    pub(crate) const fn signals_truncated(&self) -> u64 {
        self.signals_truncated
    }

    /// Closed analysis and reason counters for this plan source.
    pub(crate) fn to_json(&self) -> JsonValue {
        let status = if !self.source_present {
            "source_absent"
        } else if self.work_required > self.work_available {
            "work_limited"
        } else if self.complete {
            "complete"
        } else {
            "partial"
        };
        json!({
            "family": self.family.wire_name(),
            "status": status,
            "complete": self.complete,
            "applicability": {
                "plan_distribution": if self.family.supports_query_distribution() {
                    "exact_queryid_identity"
                } else {
                    "not_applicable_queryid_not_in_identity"
                },
                "same_plan_buffers": "calls_normalized_identity",
            },
            "quality": self.quality.to_json(),
            "distribution": self.distribution.to_json(),
            "buffers": self.buffers.to_json(),
            "work": {
                "performed": self.work,
                "required": self.work_required,
                "available": self.work_available,
            },
            "truncation": {
                "signals_dropped": self.signals_truncated,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BufferDimension {
    column: &'static str,
    buffer_class: &'static str,
    operation: &'static str,
}

const BUFFER_DIMENSIONS: [BufferDimension; 10] = [
    BufferDimension {
        column: "shared_blks_hit",
        buffer_class: "shared",
        operation: "hit",
    },
    BufferDimension {
        column: "shared_blks_read",
        buffer_class: "shared",
        operation: "read",
    },
    BufferDimension {
        column: "shared_blks_dirtied",
        buffer_class: "shared",
        operation: "dirtied",
    },
    BufferDimension {
        column: "shared_blks_written",
        buffer_class: "shared",
        operation: "written",
    },
    BufferDimension {
        column: "local_blks_hit",
        buffer_class: "local",
        operation: "hit",
    },
    BufferDimension {
        column: "local_blks_read",
        buffer_class: "local",
        operation: "read",
    },
    BufferDimension {
        column: "local_blks_dirtied",
        buffer_class: "local",
        operation: "dirtied",
    },
    BufferDimension {
        column: "local_blks_written",
        buffer_class: "local",
        operation: "written",
    },
    BufferDimension {
        column: "temp_blks_read",
        buffer_class: "temp",
        operation: "read",
    },
    BufferDimension {
        column: "temp_blks_written",
        buffer_class: "temp",
        operation: "written",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CounterDelta {
    Value(u64),
    Invalid,
}

fn integer_delta(point: DiffPoint) -> CounterDelta {
    match point {
        DiffPoint::Value {
            delta: Scalar::Int(value),
            ..
        } => u64::try_from(value).map_or(CounterDelta::Invalid, CounterDelta::Value),
        DiffPoint::Value {
            delta: Scalar::Float(_),
            ..
        }
        | DiffPoint::NoData { .. } => CounterDelta::Invalid,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlanInterval {
    ts: i64,
    calls: CounterDelta,
    buffers: [CounterDelta; BUFFER_DIMENSIONS.len()],
    reset_marker: &'static str,
    instance_identity: &'static str,
}

#[derive(Debug, Clone)]
struct PlanTimeline {
    identity: PlanIdentity,
    intervals: Vec<PlanInterval>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DistributionPoint {
    ts: i64,
    counts: Vec<CategoryCount>,
}

#[derive(Debug, Default, Clone)]
struct QueryTimeline {
    points: Vec<DistributionPoint>,
    breaks: Vec<i64>,
}

#[derive(Debug, Clone, Copy)]
struct RawPlanRow {
    first_call: i64,
}

type Snapshots = BTreeMap<i64, BTreeMap<PlanIdentity, RawPlanRow>>;

/// Scan one already-decoded plan page and its once-built typed diff.
#[must_use]
pub(crate) fn scan_plan_section(
    section: &str,
    page: &SectionPage,
    diffs: &[SeriesDiff],
    context: &PlanContext,
    scan: &ScanParams,
    max_work: usize,
    signal_limit: usize,
) -> Option<PlanScan> {
    let family = PlanFamily::from_section(section)?;
    Some(scan_plan_family(
        family,
        page,
        diffs,
        context,
        scan,
        max_work,
        signal_limit,
    ))
}

fn scan_plan_family(
    family: PlanFamily,
    page: &SectionPage,
    diffs: &[SeriesDiff],
    context: &PlanContext,
    scan: &ScanParams,
    max_work: usize,
    signal_limit: usize,
) -> PlanScan {
    let mut quality = QualityCounts {
        snapshot_rows: u64::try_from(page.rows.len()).unwrap_or(u64::MAX),
        ..QualityCounts::default()
    };
    let snapshots = parse_plan_rows(family, &page.rows, &mut quality);
    tally_snapshot_coverage(family, &snapshots, context, &mut quality);
    let source_present = !snapshots.is_empty()
        || context
            .coverage
            .get(&family.type_id())
            .is_some_and(|coverage| !coverage.is_empty())
        || context.extension_family_seen(family);
    if source_present
        && snapshots.is_empty()
        && context
            .coverage
            .get(&family.type_id())
            .is_none_or(BTreeMap::is_empty)
    {
        quality.coverage_unknown_snapshots += 1;
    }
    let (plan_timelines, calls) =
        build_plan_timelines(family, diffs, &snapshots, context, &mut quality);
    let query_timelines = if family.supports_query_distribution() {
        build_query_timelines(family, &snapshots, &calls, context, &mut quality)
    } else {
        BTreeMap::new()
    };
    let scan_positions = positions(scan);
    let work_required = required_plan_work(&plan_timelines, &query_timelines, scan_positions.len());
    let mut result = PlanScan {
        family,
        source_present,
        complete: false,
        work: 0,
        work_required,
        work_available: max_work,
        quality,
        distribution: DetectorCounts::default(),
        buffers: DetectorCounts::default(),
        signals: Vec::new(),
        signals_truncated: 0,
    };
    if !source_present {
        result.complete = true;
        return result;
    }
    if work_required > max_work {
        return result;
    }

    if family.supports_query_distribution() {
        scan_distributions(
            &query_timelines,
            &scan_positions,
            scan,
            signal_limit,
            &mut result,
        );
    } else {
        result.distribution.not_applicable = 1;
    }
    scan_buffers(
        family,
        &plan_timelines,
        &scan_positions,
        scan,
        signal_limit,
        &mut result,
    );
    result.work = work_required;
    result.complete = !context.pages_incomplete
        && result.quality.coverage_unknown_snapshots == 0
        && result.quality.truncated_snapshots == 0
        && result.quality.restricted_or_failed_snapshots == 0
        && result.quality.collection_coverage_missing == 0
        && result.quality.metadata_unknown_intervals == 0
        && result.quality.instance_identity_fallback_intervals == 0
        && result.signals_truncated == 0;
    result
}

fn parse_plan_rows(family: PlanFamily, rows: &[OutRow], quality: &mut QualityCounts) -> Snapshots {
    let mut snapshots = Snapshots::new();
    for row in rows {
        let Some(ts) = timestamp(row, "ts") else {
            quality.invalid_rows += 1;
            continue;
        };
        let Some(identity) = plan_identity_from_row(family, row) else {
            quality.invalid_rows += 1;
            continue;
        };
        let Some(first_call) = timestamp(row, "first_call") else {
            quality.invalid_rows += 1;
            continue;
        };
        let snapshot = snapshots.entry(ts).or_default();
        if snapshot
            .insert(identity, RawPlanRow { first_call })
            .is_some()
        {
            quality.invalid_rows += 1;
        }
    }
    snapshots
}

fn plan_identity_from_row(family: PlanFamily, row: &OutRow) -> Option<PlanIdentity> {
    Some(PlanIdentity {
        dbid: unsigned(row, "dbid")?,
        userid: unsigned(row, "userid")?,
        queryid: match family {
            PlanFamily::Ossc => Some(signed(row, "queryid")?),
            PlanFamily::Vadv => None,
        },
        planid: signed(row, "planid")?,
    })
}

fn plan_identity_from_key(family: PlanFamily, key: &[Value]) -> Option<PlanIdentity> {
    let unsigned_value = |index: usize| match key.get(index)? {
        Value::U64(value) => Some(*value),
        Value::I64(value) => u64::try_from(*value).ok(),
        _ => None,
    };
    let signed_value = |index: usize| match key.get(index)? {
        Value::I64(value) => Some(*value),
        Value::U64(value) => i64::try_from(*value).ok(),
        _ => None,
    };
    match family {
        PlanFamily::Ossc => Some(PlanIdentity {
            dbid: unsigned_value(0)?,
            userid: unsigned_value(1)?,
            queryid: Some(signed_value(2)?),
            planid: signed_value(3)?,
        }),
        PlanFamily::Vadv => Some(PlanIdentity {
            dbid: unsigned_value(0)?,
            userid: unsigned_value(1)?,
            queryid: None,
            planid: signed_value(2)?,
        }),
    }
}

fn tally_snapshot_coverage(
    family: PlanFamily,
    snapshots: &Snapshots,
    context: &PlanContext,
    quality: &mut QualityCounts,
) {
    let mut timestamps: BTreeSet<i64> = snapshots.keys().copied().collect();
    if let Some(coverage) = context.coverage.get(&family.type_id()) {
        timestamps.extend(coverage.keys().copied());
    }
    timestamps.extend(
        context
            .conflicting_coverage
            .iter()
            .filter_map(|&(type_id, ts)| (type_id == family.type_id()).then_some(ts)),
    );
    quality.snapshots_total = u64::try_from(timestamps.len()).unwrap_or(u64::MAX);
    for ts in timestamps {
        match context.coverage_at(family, ts) {
            Some(coverage) if coverage.is_full() => quality.full_snapshots += 1,
            Some(coverage) if coverage.is_truncated() => {
                quality.truncated_snapshots += 1;
                if !context.has_top_n_coverage(family) {
                    quality.collection_coverage_missing += 1;
                }
            }
            Some(_) => quality.restricted_or_failed_snapshots += 1,
            None => quality.coverage_unknown_snapshots += 1,
        }
    }
}

fn build_plan_timelines(
    family: PlanFamily,
    diffs: &[SeriesDiff],
    snapshots: &Snapshots,
    context: &PlanContext,
    quality: &mut QualityCounts,
) -> (
    Vec<PlanTimeline>,
    BTreeMap<(PlanIdentity, i64), CounterDelta>,
) {
    let mut timelines = Vec::new();
    let mut calls_by_identity_ts = BTreeMap::new();
    for series in diffs {
        let Some(identity) = plan_identity_from_key(family, &series.key) else {
            quality.invalid_rows += 1;
            continue;
        };
        let Some(calls_column) = series.columns.iter().find(|column| column.name == "calls") else {
            quality.invalid_rows += 1;
            continue;
        };
        let buffer_columns: [Option<&kronika_reader::ColumnDiff>; BUFFER_DIMENSIONS.len()] =
            std::array::from_fn(|index| {
                series
                    .columns
                    .iter()
                    .find(|column| column.name == BUFFER_DIMENSIONS[index].column)
            });
        let mut intervals = Vec::with_capacity(calls_column.points.len().saturating_sub(1));
        for index in 1..calls_column.points.len() {
            let previous_ts = calls_column.points[index - 1].ts;
            let current_ts = calls_column.points[index].ts;
            let continuity = validate_plan_interval(
                family,
                identity,
                previous_ts,
                current_ts,
                snapshots,
                context,
                quality,
            );
            let calls = if continuity.is_some() {
                integer_delta(calls_column.points[index].point)
            } else {
                CounterDelta::Invalid
            };
            if calls == CounterDelta::Invalid {
                quality.invalid_counter_intervals += 1;
            }
            calls_by_identity_ts.insert((identity, current_ts), calls);

            let mut buffers = [CounterDelta::Invalid; BUFFER_DIMENSIONS.len()];
            if calls != CounterDelta::Invalid {
                for (dimension, column) in buffer_columns.iter().enumerate() {
                    let value = column
                        .and_then(|column| column.points.get(index))
                        .filter(|point| point.ts == current_ts)
                        .map_or(CounterDelta::Invalid, |point| integer_delta(point.point));
                    buffers[dimension] = match (calls, value) {
                        (CounterDelta::Value(0), CounterDelta::Value(work)) if work != 0 => {
                            CounterDelta::Invalid
                        }
                        (_, value) => value,
                    };
                    if buffers[dimension] == CounterDelta::Invalid {
                        quality.invalid_counter_intervals += 1;
                    }
                }
            }
            let continuity = continuity.unwrap_or(Continuity {
                reset_marker: "unknown",
                instance_identity: "unknown",
            });
            intervals.push(PlanInterval {
                ts: current_ts,
                calls,
                buffers,
                reset_marker: continuity.reset_marker,
                instance_identity: continuity.instance_identity,
            });
        }
        timelines.push(PlanTimeline {
            identity,
            intervals,
        });
    }
    timelines.sort_by_key(|timeline| timeline.identity);
    (timelines, calls_by_identity_ts)
}

fn validate_plan_interval(
    family: PlanFamily,
    identity: PlanIdentity,
    previous_ts: i64,
    current_ts: i64,
    snapshots: &Snapshots,
    context: &PlanContext,
    quality: &mut QualityCounts,
) -> Option<Continuity> {
    if current_ts <= previous_ts {
        quality.invalid_counter_intervals += 1;
        return None;
    }
    let previous_coverage = context.coverage_at(family, previous_ts);
    let current_coverage = context.coverage_at(family, current_ts);
    if !previous_coverage.is_some_and(SnapshotCoverage::is_retained_row_usable)
        || !current_coverage.is_some_and(SnapshotCoverage::is_retained_row_usable)
    {
        quality.metadata_unknown_intervals += 1;
        return None;
    }
    let continuity = match continuity(context, family, previous_ts, current_ts, false) {
        Ok(continuity) => continuity,
        Err(failure) => {
            tally_continuity_failure(quality, failure);
            return None;
        }
    };

    let previous_row = snapshots
        .get(&previous_ts)
        .and_then(|rows| rows.get(&identity));
    let current_row = snapshots
        .get(&current_ts)
        .and_then(|rows| rows.get(&identity));
    let (Some(previous_row), Some(current_row)) = (previous_row, current_row) else {
        quality.membership_boundaries += 1;
        return None;
    };
    if previous_row.first_call != current_row.first_call {
        quality.counter_epoch_boundaries += 1;
        return None;
    }

    let missing_intermediate_row = snapshots
        .range((Excluded(previous_ts), Excluded(current_ts)))
        .any(|(_, rows)| !rows.contains_key(&identity));
    let unusable_intermediate_marker =
        context
            .coverage
            .get(&family.type_id())
            .is_some_and(|coverage| {
                coverage
                    .range((Excluded(previous_ts), Excluded(current_ts)))
                    .any(|(ts, coverage)| {
                        !coverage.is_retained_row_usable()
                            || snapshots
                                .get(ts)
                                .is_none_or(|rows| !rows.contains_key(&identity))
                    })
            });
    if missing_intermediate_row || unusable_intermediate_marker {
        quality.membership_boundaries += 1;
        return None;
    }

    if continuity.instance_identity == "source_node_fallback" {
        quality.instance_identity_fallback_intervals += 1;
    }
    Some(continuity)
}

fn build_query_timelines(
    family: PlanFamily,
    snapshots: &Snapshots,
    calls: &BTreeMap<(PlanIdentity, i64), CounterDelta>,
    context: &PlanContext,
    quality: &mut QualityCounts,
) -> BTreeMap<QueryIdentity, QueryTimeline> {
    let Some(coverage) = context.coverage.get(&family.type_id()) else {
        return BTreeMap::new();
    };
    let times: Vec<i64> = coverage.keys().copied().collect();
    let mut timelines = BTreeMap::<QueryIdentity, QueryTimeline>::new();
    for pair in times.windows(2) {
        let previous_ts = pair[0];
        let current_ts = pair[1];
        let previous_rows = snapshots.get(&previous_ts);
        let current_rows = snapshots.get(&current_ts);
        let mut queries = BTreeSet::new();
        for rows in [previous_rows, current_rows].into_iter().flatten() {
            queries.extend(rows.keys().filter_map(|identity| identity.query()));
        }
        for query in queries {
            let timeline = timelines.entry(query).or_default();
            let valid = distribution_point(
                family,
                query,
                previous_ts,
                current_ts,
                previous_rows,
                current_rows,
                calls,
                context,
                quality,
            );
            match valid {
                Some(point) => timeline.points.push(point),
                None => timeline.breaks.push(current_ts),
            }
        }
    }
    for timeline in timelines.values_mut() {
        timeline.breaks.sort_unstable();
        timeline.breaks.dedup();
    }
    timelines
}

#[allow(
    clippy::too_many_arguments,
    reason = "one distribution interval validates two snapshots, their query members, and shared provenance"
)]
fn distribution_point(
    family: PlanFamily,
    query: QueryIdentity,
    previous_ts: i64,
    current_ts: i64,
    previous_rows: Option<&BTreeMap<PlanIdentity, RawPlanRow>>,
    current_rows: Option<&BTreeMap<PlanIdentity, RawPlanRow>>,
    calls: &BTreeMap<(PlanIdentity, i64), CounterDelta>,
    context: &PlanContext,
    quality: &mut QualityCounts,
) -> Option<DistributionPoint> {
    if query.queryid == 0 {
        quality.query_id_disabled_intervals += 1;
        return None;
    }
    let previous_coverage = context.coverage_at(family, previous_ts);
    let current_coverage = context.coverage_at(family, current_ts);
    if !previous_coverage.is_some_and(SnapshotCoverage::is_full)
        || !current_coverage.is_some_and(SnapshotCoverage::is_full)
    {
        return None;
    }
    match continuity(context, family, previous_ts, current_ts, true) {
        Ok(continuity) => {
            if continuity.instance_identity == "source_node_fallback" {
                quality.instance_identity_fallback_intervals += 1;
            }
        }
        Err(failure) => {
            tally_continuity_failure(quality, failure);
            return None;
        }
    }

    let previous_members = query_members(previous_rows?, query);
    let current_members = query_members(current_rows?, query);
    if previous_members.is_empty() || previous_members != current_members {
        quality.membership_boundaries += 1;
        return None;
    }
    let mut counts = Vec::with_capacity(current_members.len());
    for identity in current_members {
        let CounterDelta::Value(count) = calls
            .get(&(identity, current_ts))
            .copied()
            .unwrap_or(CounterDelta::Invalid)
        else {
            quality.invalid_counter_intervals += 1;
            return None;
        };
        counts.push(CategoryCount::new(identity.planid, count));
    }
    Some(DistributionPoint {
        ts: current_ts,
        counts,
    })
}

fn query_members(
    rows: &BTreeMap<PlanIdentity, RawPlanRow>,
    query: QueryIdentity,
) -> BTreeSet<PlanIdentity> {
    rows.keys()
        .copied()
        .filter(|identity| identity.query() == Some(query))
        .collect()
}

fn required_plan_work(
    plans: &[PlanTimeline],
    queries: &BTreeMap<QueryIdentity, QueryTimeline>,
    scan_positions: usize,
) -> usize {
    let buffer_units = plans.iter().try_fold(0_usize, |total, timeline| {
        timeline
            .intervals
            .len()
            .checked_mul(BUFFER_DIMENSIONS.len())
            .and_then(|units| total.checked_add(units))
    });
    let distribution_units = queries.values().try_fold(0_usize, |total, timeline| {
        timeline
            .points
            .iter()
            .try_fold(total, |total, point| total.checked_add(point.counts.len()))
    });
    buffer_units
        .and_then(|buffer| {
            distribution_units.and_then(|distribution| buffer.checked_add(distribution))
        })
        .and_then(|units| units.checked_mul(scan_positions))
        .unwrap_or(usize::MAX)
}

#[derive(Debug, Clone)]
struct OpenDistribution {
    start: i64,
    end: i64,
    peak_ts: i64,
    severity: f64,
    evidence: DistributionPeak,
}

fn scan_distributions(
    timelines: &BTreeMap<QueryIdentity, QueryTimeline>,
    scan_positions: &[i64],
    scan: &ScanParams,
    signal_limit: usize,
    result: &mut PlanScan,
) {
    let params = DistributionParams::default();
    let mut reference = Vec::new();
    let mut current = Vec::new();
    result.distribution.candidates = u64::try_from(timelines.len()).unwrap_or(u64::MAX);
    for (&query, timeline) in timelines {
        let mut open: Option<OpenDistribution> = None;
        for (index, &position) in scan_positions.iter().enumerate() {
            result.distribution.positions += 1;
            let previous_position = index.checked_sub(1).map_or_else(
                || position.checked_sub(scan.window).unwrap_or(i64::MIN),
                |previous| scan_positions[previous],
            );
            let outcome = if has_break(&timeline.breaks, previous_position, position) {
                result.distribution.discontinuity += 1;
                None
            } else {
                fill_distribution_windows(
                    timeline,
                    position,
                    scan.window,
                    &mut reference,
                    &mut current,
                );
                Some(compare_distributions(&reference, &current, &params))
            };
            match outcome {
                Some(DistributionOutcome::Shift(evidence)) => {
                    result.distribution.evaluated += 1;
                    result.distribution.changed += 1;
                    let severity = evidence.total_variation / params.total_variation;
                    match open.as_mut() {
                        Some(hit) => {
                            hit.end = position;
                            if severity > hit.severity {
                                hit.peak_ts = position;
                                hit.severity = severity;
                                hit.evidence = DistributionPeak::from_evidence(&evidence);
                            }
                        }
                        None => {
                            open = Some(OpenDistribution {
                                start: position,
                                end: position,
                                peak_ts: position,
                                severity,
                                evidence: DistributionPeak::from_evidence(&evidence),
                            });
                        }
                    }
                }
                Some(DistributionOutcome::Stable(_)) => {
                    result.distribution.evaluated += 1;
                    result.distribution.stable += 1;
                    close_distribution(query, &mut open, signal_limit, result);
                }
                Some(DistributionOutcome::NotEvaluated(reason)) => {
                    tally_change_reason(&mut result.distribution, reason);
                    close_distribution(query, &mut open, signal_limit, result);
                }
                None => close_distribution(query, &mut open, signal_limit, result),
            }
        }
        close_distribution(query, &mut open, signal_limit, result);
    }
}

fn fill_distribution_windows(
    timeline: &QueryTimeline,
    position: i64,
    window: i64,
    reference: &mut Vec<CategoryCount>,
    current: &mut Vec<CategoryCount>,
) {
    reference.clear();
    current.clear();
    let window_start = position.checked_sub(window).unwrap_or(i64::MIN);
    let (segment_start, segment_end) = segment_bounds(&timeline.breaks, position);
    for point in &timeline.points {
        if point.ts <= segment_start || point.ts >= segment_end {
            continue;
        }
        if point.ts >= window_start && point.ts <= position {
            current.extend_from_slice(&point.counts);
        } else {
            reference.extend_from_slice(&point.counts);
        }
    }
}

fn close_distribution(
    query: QueryIdentity,
    open: &mut Option<OpenDistribution>,
    signal_limit: usize,
    result: &mut PlanScan,
) {
    let Some(open) = open.take() else {
        return;
    };
    retain_plan_signal(
        PlanSignal::Distribution(DistributionHit {
            query,
            start: open.start,
            end: open.end,
            peak_ts: open.peak_ts,
            severity: open.severity,
            evidence: open.evidence,
        }),
        signal_limit,
        result,
    );
}

#[derive(Debug, Clone, Copy)]
struct WorkPoint {
    ts: i64,
    calls: u64,
    work: u64,
    reset_marker: &'static str,
    instance_identity: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct OpenBuffer {
    start: i64,
    end: i64,
    peak_ts: i64,
    severity: f64,
    evidence: PerUnitEvidence,
    reset_marker: &'static str,
    instance_identity: &'static str,
}

fn scan_buffers(
    family: PlanFamily,
    timelines: &[PlanTimeline],
    scan_positions: &[i64],
    scan: &ScanParams,
    signal_limit: usize,
    result: &mut PlanScan,
) {
    let params = PerUnitParams::default();
    result.buffers.candidates =
        u64::try_from(timelines.len().saturating_mul(BUFFER_DIMENSIONS.len())).unwrap_or(u64::MAX);
    let mut points = Vec::new();
    let mut breaks = Vec::new();
    for timeline in timelines {
        for (dimension_index, &dimension) in BUFFER_DIMENSIONS.iter().enumerate() {
            points.clear();
            breaks.clear();
            for interval in &timeline.intervals {
                match (interval.calls, interval.buffers[dimension_index]) {
                    (CounterDelta::Value(0), CounterDelta::Value(0)) => {}
                    (CounterDelta::Value(calls), CounterDelta::Value(work)) if calls != 0 => {
                        points.push(WorkPoint {
                            ts: interval.ts,
                            calls,
                            work,
                            reset_marker: interval.reset_marker,
                            instance_identity: interval.instance_identity,
                        });
                    }
                    (CounterDelta::Value(_), CounterDelta::Value(_) | CounterDelta::Invalid)
                    | (CounterDelta::Invalid, _) => breaks.push(interval.ts),
                }
            }
            breaks.sort_unstable();
            breaks.dedup();
            scan_buffer_dimension(
                family,
                timeline.identity,
                dimension,
                &points,
                &breaks,
                scan_positions,
                scan,
                &params,
                signal_limit,
                result,
            );
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one bounded detector fold carries immutable identity, dimension, timeline, parameters, and output"
)]
fn scan_buffer_dimension(
    family: PlanFamily,
    plan: PlanIdentity,
    dimension: BufferDimension,
    points: &[WorkPoint],
    breaks: &[i64],
    scan_positions: &[i64],
    scan: &ScanParams,
    params: &PerUnitParams,
    signal_limit: usize,
    result: &mut PlanScan,
) {
    let mut open: Option<OpenBuffer> = None;
    for (index, &position) in scan_positions.iter().enumerate() {
        result.buffers.positions += 1;
        let previous_position = index.checked_sub(1).map_or_else(
            || position.checked_sub(scan.window).unwrap_or(i64::MIN),
            |previous| scan_positions[previous],
        );
        let outcome = if has_break(breaks, previous_position, position) {
            result.buffers.discontinuity += 1;
            None
        } else if let Some((reference, current)) =
            aggregate_work_windows(points, breaks, position, scan.window)
        {
            Some(compare_per_unit(reference, current, params))
        } else {
            result.buffers.count_overflow += 1;
            None
        };
        match outcome {
            Some(PerUnitOutcome::Increase(evidence)) => {
                result.buffers.evaluated += 1;
                result.buffers.changed += 1;
                let (reset_marker, instance_identity) =
                    evidence_provenance(points, breaks, position);
                let severity = per_unit_severity(&evidence, params);
                match open.as_mut() {
                    Some(hit) => {
                        hit.end = position;
                        if severity > hit.severity {
                            hit.peak_ts = position;
                            hit.severity = severity;
                            hit.evidence = evidence;
                            hit.reset_marker = reset_marker;
                            hit.instance_identity = instance_identity;
                        }
                    }
                    None => {
                        open = Some(OpenBuffer {
                            start: position,
                            end: position,
                            peak_ts: position,
                            severity,
                            evidence,
                            reset_marker,
                            instance_identity,
                        });
                    }
                }
            }
            Some(PerUnitOutcome::Stable(_)) => {
                result.buffers.evaluated += 1;
                result.buffers.stable += 1;
                close_buffer(family, plan, dimension, &mut open, signal_limit, result);
            }
            Some(PerUnitOutcome::NotEvaluated(reason)) => {
                tally_change_reason(&mut result.buffers, reason);
                close_buffer(family, plan, dimension, &mut open, signal_limit, result);
            }
            None => close_buffer(family, plan, dimension, &mut open, signal_limit, result),
        }
    }
    close_buffer(family, plan, dimension, &mut open, signal_limit, result);
}

fn aggregate_work_windows(
    points: &[WorkPoint],
    breaks: &[i64],
    position: i64,
    window: i64,
) -> Option<(WorkTotals, WorkTotals)> {
    let window_start = position.checked_sub(window).unwrap_or(i64::MIN);
    let (segment_start, segment_end) = segment_bounds(breaks, position);
    let mut reference = WorkTotals::new(0, 0);
    let mut current = WorkTotals::new(0, 0);
    for point in points {
        if point.ts <= segment_start || point.ts >= segment_end {
            continue;
        }
        let target = if point.ts >= window_start && point.ts <= position {
            &mut current
        } else {
            &mut reference
        };
        target.operations = target.operations.checked_add(point.calls)?;
        target.work = target.work.checked_add(point.work)?;
    }
    Some((reference, current))
}

fn evidence_provenance(
    points: &[WorkPoint],
    breaks: &[i64],
    position: i64,
) -> (&'static str, &'static str) {
    let (segment_start, segment_end) = segment_bounds(breaks, position);
    points
        .iter()
        .find(|point| point.ts > segment_start && point.ts < segment_end)
        .map_or(("unknown", "unknown"), |point| {
            (point.reset_marker, point.instance_identity)
        })
}

fn per_unit_severity(evidence: &PerUnitEvidence, params: &PerUnitParams) -> f64 {
    let absolute = evidence.delta_per_unit / params.absolute_increase;
    let relative = evidence
        .relative_delta
        .map_or(absolute, |delta| delta / params.relative_increase);
    absolute.min(relative)
}

fn close_buffer(
    family: PlanFamily,
    plan: PlanIdentity,
    dimension: BufferDimension,
    open: &mut Option<OpenBuffer>,
    signal_limit: usize,
    result: &mut PlanScan,
) {
    let Some(open) = open.take() else {
        return;
    };
    retain_plan_signal(
        PlanSignal::Buffer(BufferHit {
            family,
            plan,
            dimension,
            start: open.start,
            end: open.end,
            peak_ts: open.peak_ts,
            severity: open.severity,
            evidence: open.evidence,
            reset_marker: open.reset_marker,
            instance_identity: open.instance_identity,
        }),
        signal_limit,
        result,
    );
}

fn retain_plan_signal(signal: PlanSignal, limit: usize, result: &mut PlanScan) {
    result.signals.push(signal);
    result.signals_truncated = result.signals_truncated.saturating_add(
        u64::try_from(rank_plan_signals(&mut result.signals, limit)).unwrap_or(u64::MAX),
    );
}

const fn tally_change_reason(counts: &mut DetectorCounts, reason: ChangeNotEvaluatedReason) {
    match reason {
        ChangeNotEvaluatedReason::ReferenceTooSmall => counts.reference_too_small += 1,
        ChangeNotEvaluatedReason::CurrentTooSmall => counts.current_too_small += 1,
        ChangeNotEvaluatedReason::CountOverflow => counts.count_overflow += 1,
    }
}

fn has_break(breaks: &[i64], previous_position: i64, position: i64) -> bool {
    let start = breaks.partition_point(|&at| at <= previous_position);
    let end = breaks.partition_point(|&at| at <= position);
    start < end
}

fn segment_bounds(breaks: &[i64], position: i64) -> (i64, i64) {
    let end = breaks.partition_point(|&at| at <= position);
    let start = end
        .checked_sub(1)
        .and_then(|index| breaks.get(index))
        .copied()
        .unwrap_or(i64::MIN);
    let finish = breaks.get(end).copied().unwrap_or(i64::MAX);
    (start, finish)
}
