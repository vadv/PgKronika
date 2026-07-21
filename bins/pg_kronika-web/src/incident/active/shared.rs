use std::sync::Arc;

use super::super::cluster::Cluster;
use super::super::dispatch::LimitHit;
use super::super::engine::EvalContext;
use super::super::evidence::sink::FindingSink;
use super::super::evidence::{
    CounterEvidence, CounterEvidenceInput, CounterEvidenceWindow, CounterEvidenceWindowInput,
    CounterMeasurementKind, CounterOperand, CounterOperandPurpose, GaugeEntity, GaugeUnit,
    ThresholdKind,
};
use super::super::model::IdentityValue;
use super::super::typed::{PairedSums, TypedInputs};

pub(super) const PG_STAT_DATABASE: &str = "pg_stat_database";
pub(super) const PG_STAT_WAL: &str = "pg_stat_wal";
pub(super) const CHECKPOINTER: &str = "pg_stat_bgwriter + pg_stat_checkpointer";
pub(super) const PG_STAT_IO: &str = "pg_stat_io";
pub(super) const PG_STAT_USER_TABLES: &str = "pg_stat_user_tables";
pub(super) const PG_STAT_ARCHIVER: &str = "pg_stat_archiver";
pub(super) const PG_STAT_ACTIVITY: &str = "pg_stat_activity";
pub(super) const PG_LOCKS: &str = "pg_locks";
pub(super) const OS_NETDEV: &str = "os_netdev";
pub(super) const OS_CGROUP_CPU: &str = "os_cgroup_cpu";
pub(super) const OS_MEMINFO: &str = "os_meminfo";

pub(super) struct CounterEvidenceSpec {
    pub(super) kind: CounterMeasurementKind,
    pub(super) formula: &'static str,
    pub(super) value: f64,
    pub(super) unit: GaugeUnit,
    pub(super) threshold: f64,
    pub(super) threshold_kind: ThresholdKind,
    pub(super) section: &'static str,
    pub(super) operands: Vec<CounterOperandSpec>,
}

pub(super) struct CounterOperandSpec {
    pub(super) name: &'static str,
    pub(super) value: f64,
    pub(super) unit: GaugeUnit,
    pub(super) purpose: CounterOperandPurpose,
}

pub(super) fn counter_evidence(
    sums: PairedSums,
    context: &EvalContext,
    identity: &Arc<[IdentityValue]>,
    spec: CounterEvidenceSpec,
) -> Option<CounterEvidence> {
    let window = CounterEvidenceWindow::new(CounterEvidenceWindowInput {
        selection_from_us: context.incident_start_us,
        selection_to_us: context.incident_end_us,
        first_interval_start_us: sums.first_start_us?,
        first_interval_end_us: sums.first_end_us?,
        last_interval_end_us: sums.last_end_us?,
        usable_intervals: sums.intervals,
        candidate_intervals: sums.candidate_intervals,
        unmatched_endpoint_intervals: sums.unmatched_endpoint_intervals,
        unusable_delta_intervals: sums.unusable_delta_intervals,
        unaligned_duration_intervals: sums.unaligned_duration_intervals,
        numeric_limit_intervals: sums.numeric_limit_intervals,
        elapsed_us: sums.elapsed_us,
        observed_period_us: sums.observed_period_us,
    })?;
    CounterEvidence::new(CounterEvidenceInput {
        kind: spec.kind,
        formula: spec.formula,
        value: spec.value,
        unit: spec.unit,
        threshold: spec.threshold,
        threshold_kind: spec.threshold_kind,
        operands: spec
            .operands
            .into_iter()
            .map(|operand| {
                CounterOperand::new(operand.name, operand.value, operand.unit, operand.purpose)
            })
            .collect::<Option<Vec<_>>>()?,
        window,
        entity: GaugeEntity::new(spec.section, Arc::clone(identity)),
    })
}

pub(super) fn paired_sums(
    typed: &TypedInputs,
    section: &'static str,
    identity: &[IdentityValue],
    column_a: &'static str,
    column_b: &'static str,
    context: &EvalContext,
    sink: &mut FindingSink<'_>,
) -> Result<Option<PairedSums>, LimitHit> {
    sink.charge_points(typed.aligned_counter_points(section, identity, &[column_a, column_b]))?;
    Ok(typed.paired_delta_sums(
        section,
        identity,
        column_a,
        column_b,
        context.incident_start_us,
        context.incident_end_us,
    ))
}

pub(super) const fn exact_u64_as_f64(value: u64) -> Option<f64> {
    const MAX_EXACT_INTEGER: u64 = 1_u64 << 53;
    if value > MAX_EXACT_INTEGER {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "the preceding bound proves exact IEEE-754 integer representation"
    )]
    Some(value as f64)
}

/// The first `pg_stat_activity` episode of a cluster, or `None` when the section
/// is not represented. The activity lenses report once per incident, scoped to
/// this episode, rather than once per matching member.
pub(super) fn activity_member(cluster: &Cluster) -> Option<&super::super::model::EpisodeRefV1> {
    cluster
        .members
        .iter()
        .find(|member| member.logical_section == PG_STAT_ACTIVITY)
}

/// Total backends the activity lenses scan over the incident window, charged as
/// work before analysis.
pub(super) fn activity_backends_examined(typed: &TypedInputs, start: i64, end: i64) -> usize {
    typed
        .activity_window(start, end)
        .map(|snapshot| snapshot.backends.len())
        .sum()
}

/// Whether a session state is an open but idle transaction, which pins the
/// vacuum horizon without doing work.
pub(super) fn idle_in_transaction(state: Option<&str>) -> bool {
    matches!(
        state,
        Some("idle in transaction" | "idle in transaction (aborted)")
    )
}

/// Whether a wait event is the synchronous-replication commit wait.
pub(super) fn is_syncrep(wait_event: Option<&str>) -> bool {
    wait_event == Some("SyncRep")
}

pub(super) fn exact_i64_as_f64(value: i64) -> Option<f64> {
    const MAX_EXACT_INTEGER: i64 = 1_i64 << 53;
    if !(-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&value) {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "the preceding bound proves exact IEEE-754 integer representation"
    )]
    Some(value as f64)
}

/// Whether a wait-event class is an internal (non-client) contention point.
pub(super) fn is_internal_wait(wait_event_type: Option<&str>) -> bool {
    matches!(wait_event_type, Some("LWLock" | "BufferPin" | "IO"))
}
