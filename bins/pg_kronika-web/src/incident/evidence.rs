//! Evidence-gated confidence and direction.

use std::sync::Arc;
use std::{cmp::Ordering, hash::Hash};

use super::engine::TemporalDirectionPermit;
use super::model::{EpisodeRefV1, IdentityValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Confidence(u8);

impl Confidence {
    pub(crate) const LOW: Self = Self(0);
    pub(crate) const MEDIUM: Self = Self(1);
    pub(crate) const HIGH: Self = Self(2);

    pub(crate) const fn label(self) -> &'static str {
        match self.0 {
            0 => "low",
            1 => "medium",
            _ => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfidenceCap {
    Low,
    Medium,
    High,
}

impl ConfidenceCap {
    const fn confidence(self) -> Confidence {
        match self {
            Self::Low => Confidence::LOW,
            Self::Medium => Confidence::MEDIUM,
            Self::High => Confidence::HIGH,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Lead,
    Amplifier,
    Downstream,
    Coincident,
}

impl Role {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Amplifier => "amplifier",
            Self::Downstream => "downstream",
            Self::Coincident => "coincident",
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DirectEvidence {
    kind: DirectEvidenceKind,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DirectEvidenceKind {
    SampledLockEdge(SampledLockEdge),
    ResourceLimitEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LockParticipant {
    Blocker,
    Waiter,
}

impl LockParticipant {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Blocker => "blocker",
            Self::Waiter => "waiter",
        }
    }

    const fn proves_role(self, requested_role: Role) -> bool {
        matches!(
            (self, requested_role),
            (Self::Blocker, Role::Lead) | (Self::Waiter, Role::Downstream)
        )
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SampledLockEdge {
    observed_at_us: i64,
    waiter_pid: i64,
    blocker_pid: i64,
    participant: LockParticipant,
}

impl SampledLockEdge {
    pub(crate) const fn observed_at_us(&self) -> i64 {
        self.observed_at_us
    }

    pub(crate) const fn waiter_pid(&self) -> i64 {
        self.waiter_pid
    }

    pub(crate) const fn blocker_pid(&self) -> i64 {
        self.blocker_pid
    }

    pub(crate) const fn participant(&self) -> LockParticipant {
        self.participant
    }
}

impl DirectEvidence {
    /// A sampled `pg_locks` blocking edge: `blocked_by` names a process that
    /// prevented the waiter from acquiring the lock. It can be a queue
    /// predecessor rather than a lock holder. This proves the sampled edge's
    /// direction; the lens still controls the confidence ceiling.
    pub(crate) const fn sampled_lock_edge(
        observed_at_us: i64,
        waiter_pid: i64,
        blocker_pid: i64,
        participant: LockParticipant,
    ) -> Self {
        Self {
            kind: DirectEvidenceKind::SampledLockEdge(SampledLockEdge {
                observed_at_us,
                waiter_pid,
                blocker_pid,
                participant,
            }),
        }
    }

    #[cfg(test)]
    const fn resource_limit_event() -> Self {
        Self {
            kind: DirectEvidenceKind::ResourceLimitEvent,
        }
    }

    const fn proves_structural_direction(&self, requested_role: Role) -> bool {
        match &self.kind {
            DirectEvidenceKind::SampledLockEdge(edge) => {
                edge.participant.proves_role(requested_role)
            }
            DirectEvidenceKind::ResourceLimitEvent => false,
        }
    }

    pub(crate) const fn lock_edge(&self) -> Option<&SampledLockEdge> {
        match &self.kind {
            DirectEvidenceKind::SampledLockEdge(edge) => Some(edge),
            DirectEvidenceKind::ResourceLimitEvent => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FiniteValue(f64);

impl FiniteValue {
    pub(crate) fn new(value: f64) -> Option<Self> {
        value
            .is_finite()
            .then_some(Self(if value == 0.0 { 0.0 } else { value }))
    }

    pub(crate) const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for FiniteValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for FiniteValue {}

impl PartialOrd for FiniteValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FiniteValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for FiniteValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GaugeUnit {
    Count,
    Bytes,
    Kibibytes,
    Microseconds,
    Milliseconds,
    Ratio,
    BytesPerSecond,
    MicrosecondsPerSecond,
    MillisecondsPerRead,
    MillisecondsPerCall,
    MillisecondsPerOperation,
    RowsPerCall,
    BlocksPerCall,
}

impl GaugeUnit {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Bytes => "bytes",
            Self::Kibibytes => "KiB",
            Self::Microseconds => "microseconds",
            Self::Milliseconds => "milliseconds",
            Self::Ratio => "ratio",
            Self::BytesPerSecond => "bytes_per_second",
            Self::MicrosecondsPerSecond => "microseconds_per_second",
            Self::MillisecondsPerRead => "milliseconds_per_read",
            Self::MillisecondsPerCall => "milliseconds_per_call",
            Self::MillisecondsPerOperation => "milliseconds_per_operation",
            Self::RowsPerCall => "rows_per_call",
            Self::BlocksPerCall => "blocks_per_call",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ThresholdKind {
    AtLeast,
    Above,
    Below,
}

impl ThresholdKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::AtLeast => "at_least",
            Self::Above => "above",
            Self::Below => "below",
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GaugeMeasurement {
    Value {
        operand: &'static str,
        value: FiniteValue,
    },
    Ratio {
        numerator_name: &'static str,
        numerator: FiniteValue,
        numerator_unit: GaugeUnit,
        denominator_name: &'static str,
        denominator: FiniteValue,
        denominator_unit: GaugeUnit,
    },
    Trend {
        operand: &'static str,
        first: FiniteValue,
        last: FiniteValue,
        elapsed_us: u64,
        operand_unit: GaugeUnit,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GaugeRatio {
    numerator_name: &'static str,
    numerator: f64,
    numerator_unit: GaugeUnit,
    denominator_name: &'static str,
    denominator: f64,
    denominator_unit: GaugeUnit,
    result_unit: GaugeUnit,
}

pub(crate) struct GaugeTrendInput {
    pub operand: &'static str,
    pub first: f64,
    pub last: f64,
    pub operand_unit: GaugeUnit,
    pub threshold_per_second: f64,
    pub threshold_kind: ThresholdKind,
    pub first_at_us: i64,
    pub last_at_us: i64,
    pub samples: usize,
    pub entity: GaugeEntity,
}

pub(crate) struct GaugeValueInput {
    pub operand: &'static str,
    pub value: f64,
    pub unit: GaugeUnit,
    pub threshold: f64,
    pub threshold_kind: ThresholdKind,
    pub observed_at_us: i64,
    pub samples: usize,
    pub entity: GaugeEntity,
}

impl GaugeRatio {
    pub(crate) const fn new(
        numerator_name: &'static str,
        numerator: f64,
        denominator_name: &'static str,
        denominator: f64,
        operand_unit: GaugeUnit,
    ) -> Self {
        Self {
            numerator_name,
            numerator,
            numerator_unit: operand_unit,
            denominator_name,
            denominator,
            denominator_unit: operand_unit,
            result_unit: GaugeUnit::Ratio,
        }
    }

    pub(crate) const fn with_units(
        numerator_name: &'static str,
        numerator: f64,
        numerator_unit: GaugeUnit,
        denominator_name: &'static str,
        denominator: f64,
        denominator_unit: GaugeUnit,
        result_unit: GaugeUnit,
    ) -> Self {
        Self {
            numerator_name,
            numerator,
            numerator_unit,
            denominator_name,
            denominator,
            denominator_unit,
            result_unit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GaugeEntity {
    section: &'static str,
    identity: Arc<[IdentityValue]>,
}

impl GaugeEntity {
    pub(crate) const fn new(section: &'static str, identity: Arc<[IdentityValue]>) -> Self {
        Self { section, identity }
    }

    pub(crate) const fn section(&self) -> &'static str {
        self.section
    }

    pub(crate) fn identity(&self) -> &[IdentityValue] {
        &self.identity
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GaugeEvidence {
    measurement: GaugeMeasurement,
    unit: GaugeUnit,
    threshold: FiniteValue,
    threshold_kind: ThresholdKind,
    observed_at_us: i64,
    samples: u64,
    entity: GaugeEntity,
}

impl GaugeEvidence {
    pub(crate) fn value(input: GaugeValueInput) -> Option<Self> {
        let GaugeValueInput {
            operand,
            value,
            unit,
            threshold,
            threshold_kind,
            observed_at_us,
            samples,
            entity,
        } = input;
        let samples = u64::try_from(samples).ok()?;
        (samples > 0 && !operand.is_empty() && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Value {
                operand,
                value: FiniteValue::new(value)?,
            },
            unit,
            threshold: FiniteValue::new(threshold)?,
            threshold_kind,
            observed_at_us,
            samples,
            entity,
        })
    }

    pub(crate) fn ratio(
        ratio: GaugeRatio,
        threshold: f64,
        threshold_kind: ThresholdKind,
        observed_at_us: i64,
        samples: usize,
        entity: GaugeEntity,
    ) -> Option<Self> {
        (ratio.denominator > 0.0
            && !ratio.numerator_name.is_empty()
            && !ratio.denominator_name.is_empty())
        .then_some(())?;
        FiniteValue::new(ratio.numerator / ratio.denominator)?;
        let samples = u64::try_from(samples).ok()?;
        (samples > 0 && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Ratio {
                numerator_name: ratio.numerator_name,
                numerator: FiniteValue::new(ratio.numerator)?,
                numerator_unit: ratio.numerator_unit,
                denominator_name: ratio.denominator_name,
                denominator: FiniteValue::new(ratio.denominator)?,
                denominator_unit: ratio.denominator_unit,
            },
            unit: ratio.result_unit,
            threshold: FiniteValue::new(threshold)?,
            threshold_kind,
            observed_at_us,
            samples,
            entity,
        })
    }

    pub(crate) fn trend(input: GaugeTrendInput) -> Option<Self> {
        let GaugeTrendInput {
            operand,
            first,
            last,
            operand_unit,
            threshold_per_second,
            threshold_kind,
            first_at_us,
            last_at_us,
            samples,
            entity,
        } = input;
        let elapsed_us = u64::try_from(last_at_us.checked_sub(first_at_us)?).ok()?;
        (elapsed_us > 0).then_some(())?;
        let elapsed_seconds = std::time::Duration::from_micros(elapsed_us).as_secs_f64();
        FiniteValue::new((last - first) / elapsed_seconds)?;
        let samples = u64::try_from(samples).ok()?;
        (samples >= 2 && !operand.is_empty() && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Trend {
                operand,
                first: FiniteValue::new(first)?,
                last: FiniteValue::new(last)?,
                elapsed_us,
                operand_unit,
            },
            unit: GaugeUnit::BytesPerSecond,
            threshold: FiniteValue::new(threshold_per_second)?,
            threshold_kind,
            observed_at_us: last_at_us,
            samples,
            entity,
        })
    }

    pub(crate) const fn measurement(&self) -> &GaugeMeasurement {
        &self.measurement
    }

    pub(crate) const fn unit(&self) -> GaugeUnit {
        self.unit
    }

    pub(crate) const fn threshold(&self) -> FiniteValue {
        self.threshold
    }

    pub(crate) const fn threshold_kind(&self) -> ThresholdKind {
        self.threshold_kind
    }

    pub(crate) const fn observed_at_us(&self) -> i64 {
        self.observed_at_us
    }

    pub(crate) const fn samples(&self) -> u64 {
        self.samples
    }

    pub(crate) const fn entity(&self) -> &GaugeEntity {
        &self.entity
    }

    fn operand_name_bytes(&self) -> u64 {
        let length = match &self.measurement {
            GaugeMeasurement::Value { operand, .. } | GaugeMeasurement::Trend { operand, .. } => {
                operand.len()
            }
            GaugeMeasurement::Ratio {
                numerator_name,
                denominator_name,
                ..
            } => numerator_name.len().saturating_add(denominator_name.len()),
        };
        u64::try_from(length).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CounterMeasurementKind {
    Sum,
    Ratio,
    Rate,
}

impl CounterMeasurementKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Ratio => "ratio",
            Self::Rate => "rate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CounterOperandPurpose {
    Formula,
    Qualification,
    AlignedContext,
}

impl CounterOperandPurpose {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Formula => "formula",
            Self::Qualification => "qualification",
            Self::AlignedContext => "aligned_context",
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CounterOperand {
    name: &'static str,
    value: FiniteValue,
    unit: GaugeUnit,
    purpose: CounterOperandPurpose,
}

impl CounterOperand {
    pub(crate) fn new(
        name: &'static str,
        value: f64,
        unit: GaugeUnit,
        purpose: CounterOperandPurpose,
    ) -> Option<Self> {
        (!name.is_empty() && value >= 0.0).then_some(Self {
            name,
            value: FiniteValue::new(value)?,
            unit,
            purpose,
        })
    }

    pub(crate) const fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) const fn value(&self) -> FiniteValue {
        self.value
    }

    pub(crate) const fn unit(&self) -> GaugeUnit {
        self.unit
    }

    pub(crate) const fn purpose(&self) -> CounterOperandPurpose {
        self.purpose
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CounterEvidenceWindow {
    from_us: i64,
    to_us: i64,
    first_interval_start_us: i64,
    first_interval_end_us: i64,
    last_interval_end_us: i64,
    usable_intervals: u64,
    candidate_intervals: u64,
    unmatched_endpoint_intervals: u64,
    unusable_delta_intervals: u64,
    unaligned_duration_intervals: u64,
    numeric_limit_intervals: u64,
    elapsed_us: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct CounterEvidenceWindowInput {
    pub selection_from_us: i64,
    pub selection_to_us: i64,
    pub first_interval_start_us: i64,
    pub first_interval_end_us: i64,
    pub last_interval_end_us: i64,
    pub usable_intervals: usize,
    pub candidate_intervals: usize,
    pub unmatched_endpoint_intervals: usize,
    pub unusable_delta_intervals: usize,
    pub unaligned_duration_intervals: usize,
    pub numeric_limit_intervals: usize,
    pub elapsed_us: u64,
}

impl CounterEvidenceWindow {
    pub(crate) fn new(input: CounterEvidenceWindowInput) -> Option<Self> {
        let CounterEvidenceWindowInput {
            selection_from_us: from_us,
            selection_to_us: to_us,
            first_interval_start_us,
            first_interval_end_us,
            last_interval_end_us,
            usable_intervals,
            candidate_intervals,
            unmatched_endpoint_intervals,
            unusable_delta_intervals,
            unaligned_duration_intervals,
            numeric_limit_intervals,
            elapsed_us,
        } = input;
        let usable_intervals = u64::try_from(usable_intervals).ok()?;
        let candidate_intervals = u64::try_from(candidate_intervals).ok()?;
        let unmatched_endpoint_intervals = u64::try_from(unmatched_endpoint_intervals).ok()?;
        let unusable_delta_intervals = u64::try_from(unusable_delta_intervals).ok()?;
        let unaligned_duration_intervals = u64::try_from(unaligned_duration_intervals).ok()?;
        let numeric_limit_intervals = u64::try_from(numeric_limit_intervals).ok()?;
        let classified_intervals = usable_intervals
            .checked_add(unmatched_endpoint_intervals)?
            .checked_add(unusable_delta_intervals)?
            .checked_add(unaligned_duration_intervals)?
            .checked_add(numeric_limit_intervals)?;
        (first_interval_start_us <= first_interval_end_us
            && from_us <= first_interval_end_us
            && first_interval_end_us <= last_interval_end_us
            && last_interval_end_us <= to_us
            && usable_intervals > 0
            && usable_intervals <= candidate_intervals
            && classified_intervals == candidate_intervals
            && elapsed_us > 0)
            .then_some(Self {
                from_us,
                to_us,
                first_interval_start_us,
                first_interval_end_us,
                last_interval_end_us,
                usable_intervals,
                candidate_intervals,
                unmatched_endpoint_intervals,
                unusable_delta_intervals,
                unaligned_duration_intervals,
                numeric_limit_intervals,
                elapsed_us,
            })
    }

    pub(crate) const fn selection_from_us(&self) -> i64 {
        self.from_us
    }

    pub(crate) const fn selection_to_us(&self) -> i64 {
        self.to_us
    }

    pub(crate) const fn first_interval_start_us(&self) -> i64 {
        self.first_interval_start_us
    }

    pub(crate) const fn first_interval_end_us(&self) -> i64 {
        self.first_interval_end_us
    }

    pub(crate) const fn last_interval_end_us(&self) -> i64 {
        self.last_interval_end_us
    }

    pub(crate) const fn usable_intervals(&self) -> u64 {
        self.usable_intervals
    }

    pub(crate) const fn candidate_intervals(&self) -> u64 {
        self.candidate_intervals
    }

    pub(crate) const fn excluded_intervals(&self) -> u64 {
        self.candidate_intervals - self.usable_intervals
    }

    pub(crate) const fn unmatched_endpoint_intervals(&self) -> u64 {
        self.unmatched_endpoint_intervals
    }

    pub(crate) const fn unusable_delta_intervals(&self) -> u64 {
        self.unusable_delta_intervals
    }

    pub(crate) const fn unaligned_duration_intervals(&self) -> u64 {
        self.unaligned_duration_intervals
    }

    pub(crate) const fn numeric_limit_intervals(&self) -> u64 {
        self.numeric_limit_intervals
    }

    pub(crate) const fn elapsed_us(&self) -> u64 {
        self.elapsed_us
    }
}

pub(crate) struct CounterEvidenceInput {
    pub kind: CounterMeasurementKind,
    pub formula: &'static str,
    pub value: f64,
    pub unit: GaugeUnit,
    pub threshold: f64,
    pub threshold_kind: ThresholdKind,
    pub operands: Vec<CounterOperand>,
    pub window: CounterEvidenceWindow,
    pub entity: GaugeEntity,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CounterEvidence {
    kind: CounterMeasurementKind,
    formula: &'static str,
    value: FiniteValue,
    unit: GaugeUnit,
    threshold: FiniteValue,
    threshold_kind: ThresholdKind,
    operands: Box<[CounterOperand]>,
    window: CounterEvidenceWindow,
    entity: GaugeEntity,
}

impl CounterEvidence {
    const MAX_OPERANDS: usize = 3;

    pub(crate) fn new(input: CounterEvidenceInput) -> Option<Self> {
        (1..=Self::MAX_OPERANDS)
            .contains(&input.operands.len())
            .then_some(())?;
        input
            .operands
            .iter()
            .any(|operand| operand.purpose() == CounterOperandPurpose::Formula)
            .then_some(())?;
        input
            .operands
            .iter()
            .enumerate()
            .all(|(index, operand)| {
                input.operands[..index]
                    .iter()
                    .all(|previous| previous.name() != operand.name())
            })
            .then_some(())?;
        (!input.formula.is_empty() && !input.entity.section().is_empty()).then_some(Self {
            kind: input.kind,
            formula: input.formula,
            value: FiniteValue::new(input.value)?,
            unit: input.unit,
            threshold: FiniteValue::new(input.threshold)?,
            threshold_kind: input.threshold_kind,
            operands: input.operands.into_boxed_slice(),
            window: input.window,
            entity: input.entity,
        })
    }

    pub(crate) const fn kind(&self) -> CounterMeasurementKind {
        self.kind
    }

    pub(crate) const fn formula(&self) -> &'static str {
        self.formula
    }

    pub(crate) const fn value(&self) -> FiniteValue {
        self.value
    }

    pub(crate) const fn unit(&self) -> GaugeUnit {
        self.unit
    }

    pub(crate) const fn threshold(&self) -> FiniteValue {
        self.threshold
    }

    pub(crate) const fn threshold_kind(&self) -> ThresholdKind {
        self.threshold_kind
    }

    pub(crate) const fn operands(&self) -> &[CounterOperand] {
        &self.operands
    }

    pub(crate) const fn window(&self) -> &CounterEvidenceWindow {
        &self.window
    }

    pub(crate) const fn entity(&self) -> &GaugeEntity {
        &self.entity
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Evidence {
    Direct(DirectEvidence),
    Ratio,
    GaugeObservation(GaugeEvidence),
    CounterAggregate(CounterEvidence),
    Gauge,
    Counter,
    Event,
}

impl Evidence {
    const fn justifies_high(&self) -> bool {
        matches!(self, Self::Direct(_))
    }

    const fn proves_structural_direction(&self, requested_role: Role) -> bool {
        matches!(self, Self::Direct(direct) if direct.proves_structural_direction(requested_role))
    }

    const fn is_sampled_lock_edge(&self) -> bool {
        matches!(self, Self::Direct(direct) if direct.lock_edge().is_some())
    }

    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct",
            Self::Ratio => "ratio",
            Self::GaugeObservation(_) | Self::Gauge => "gauge",
            Self::CounterAggregate(_) | Self::Counter => "counter",
            Self::Event => "event",
        }
    }
}

fn evidence_ceiling(evidence: &[Evidence]) -> Confidence {
    if evidence.is_empty() {
        Confidence::LOW
    } else if evidence.iter().any(Evidence::justifies_high) {
        Confidence::HIGH
    } else {
        Confidence::MEDIUM
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FindingScope {
    logical_section: &'static str,
    column: &'static str,
    identity: Arc<[IdentityValue]>,
}

impl FindingScope {
    pub(crate) fn from_episode(reference: &EpisodeRefV1) -> Self {
        Self {
            logical_section: reference.logical_section,
            column: reference.column,
            identity: Arc::clone(&reference.identity),
        }
    }

    /// Scope built from a log event's own typed fields rather than an anomaly
    /// episode. The identity must carry only non-sensitive fields, since it is
    /// serialized into the response.
    pub(crate) const fn from_parts(
        logical_section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
    ) -> Self {
        Self {
            logical_section,
            column,
            identity,
        }
    }

    pub(crate) const fn logical_section(&self) -> &'static str {
        self.logical_section
    }

    pub(crate) const fn column(&self) -> &'static str {
        self.column
    }

    pub(crate) fn identity(&self) -> &[IdentityValue] {
        &self.identity
    }
}

pub(crate) struct Finding {
    lens_id: &'static str,
    role: Role,
    confidence: Confidence,
    scope: FindingScope,
    evidence: Vec<Evidence>,
}

pub(crate) struct FindingDraft {
    requested_role: Role,
    scope: FindingScope,
    evidence: Vec<Evidence>,
    temporal_direction: bool,
}

impl FindingDraft {
    pub(crate) const fn new(
        requested_role: Role,
        scope: FindingScope,
        evidence: Vec<Evidence>,
        temporal_direction: Option<&TemporalDirectionPermit<'_>>,
    ) -> Self {
        Self {
            requested_role,
            scope,
            evidence,
            temporal_direction: temporal_direction.is_some(),
        }
    }

    pub(crate) const fn evidence_len(&self) -> usize {
        self.evidence.len()
    }

    fn output_bytes_upper_bound(&self, lens_id: &str) -> u64 {
        let evidence = self.evidence.iter().fold(0_u64, |total, item| {
            let bytes = match item {
                Evidence::GaugeObservation(gauge) => 512_u64
                    .saturating_add(gauge.operand_name_bytes())
                    .saturating_add(
                        u64::try_from(gauge.entity().section().len()).unwrap_or(u64::MAX),
                    )
                    .saturating_add(identity_json_upper_bound(gauge.entity().identity())),
                Evidence::CounterAggregate(counter) => 1_024_u64
                    .saturating_add(u64::try_from(counter.formula().len()).unwrap_or(u64::MAX))
                    .saturating_add(counter.operands().iter().fold(0_u64, |bytes, operand| {
                        bytes
                            .saturating_add(u64::try_from(operand.name().len()).unwrap_or(u64::MAX))
                    }))
                    .saturating_add(
                        u64::try_from(counter.entity().section().len()).unwrap_or(u64::MAX),
                    )
                    .saturating_add(identity_json_upper_bound(counter.entity().identity())),
                Evidence::Direct(_) => 512,
                Evidence::Ratio | Evidence::Gauge | Evidence::Counter | Evidence::Event => 32,
            };
            total.saturating_add(bytes)
        });
        512_u64
            .saturating_add(u64::try_from(lens_id.len()).unwrap_or(u64::MAX))
            .saturating_add(identity_json_upper_bound(self.scope.identity()))
            .saturating_add(evidence)
    }
}

fn identity_json_upper_bound(identity: &[IdentityValue]) -> u64 {
    identity.iter().fold(2_u64, |total, value| {
        let bytes = match value {
            IdentityValue::I64(_) | IdentityValue::U64(_) => 21,
            IdentityValue::Bool(_) => 5,
            IdentityValue::Text(text) => u64::try_from(text.len())
                .unwrap_or(u64::MAX)
                .saturating_mul(6)
                .saturating_add(2),
        };
        total.saturating_add(bytes).saturating_add(1)
    })
}

impl Finding {
    fn from_draft(lens_id: &'static str, cap: ConfidenceCap, draft: FindingDraft) -> Self {
        let FindingDraft {
            requested_role,
            scope,
            evidence,
            temporal_direction,
        } = draft;
        let structural_direction = evidence
            .iter()
            .any(|item| item.proves_structural_direction(requested_role));
        let role = match requested_role {
            Role::Lead | Role::Downstream if !structural_direction && !temporal_direction => {
                Role::Coincident
            }
            role => role,
        };
        let confidence = cap.confidence().min(evidence_ceiling(&evidence));
        Self {
            lens_id,
            role,
            confidence,
            scope,
            evidence,
        }
    }

    pub(crate) const fn lens_id(&self) -> &'static str {
        self.lens_id
    }

    pub(crate) const fn role(&self) -> Role {
        self.role
    }

    /// Apply observation-time ordering only as a fallback. A sampled lock edge
    /// owns its structural direction, including when a malformed draft was
    /// downgraded to coincident.
    pub(crate) fn apply_temporal_role(&mut self, role: Role) {
        if self.role == Role::Coincident
            && matches!(role, Role::Lead | Role::Downstream)
            && !self.evidence.iter().any(Evidence::is_sampled_lock_edge)
        {
            self.role = role;
        }
    }

    pub(crate) const fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub(crate) const fn scope(&self) -> &FindingScope {
        &self.scope
    }

    pub(crate) fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }
}

pub(super) mod sink {
    use super::{ConfidenceCap, Finding, FindingDraft};
    use crate::incident::dispatch::{LimitAxis, LimitHit, WorkBudget};

    pub(crate) struct OutputCounts {
        findings: u64,
        evidence_rows: u64,
        output_bytes: u64,
    }

    impl OutputCounts {
        pub(crate) const fn new() -> Self {
            Self {
                findings: 0,
                evidence_rows: 0,
                output_bytes: 0,
            }
        }
    }

    #[derive(Clone, Copy)]
    pub(crate) struct OutputLimits {
        findings: u64,
        evidence_rows: u64,
        output_bytes: u64,
    }

    impl OutputLimits {
        pub(crate) const fn new(findings: u64, evidence_rows: u64) -> Self {
            Self {
                findings,
                evidence_rows,
                output_bytes: u64::MAX,
            }
        }

        pub(crate) const fn bounded(findings: u64, evidence_rows: u64, output_bytes: u64) -> Self {
            Self {
                findings,
                evidence_rows,
                output_bytes,
            }
        }
    }

    pub(crate) struct FindingSink<'a> {
        findings: &'a mut Vec<Finding>,
        budget: &'a mut WorkBudget,
        counts: &'a mut OutputCounts,
        limits: OutputLimits,
        hit: Option<LimitHit>,
        lens_id: &'static str,
        confidence_cap: ConfidenceCap,
    }

    impl<'a> FindingSink<'a> {
        pub(crate) const fn new(
            findings: &'a mut Vec<Finding>,
            budget: &'a mut WorkBudget,
            counts: &'a mut OutputCounts,
            limits: OutputLimits,
            lens_id: &'static str,
            confidence_cap: ConfidenceCap,
        ) -> Self {
            Self {
                findings,
                budget,
                counts,
                limits,
                hit: None,
                lens_id,
                confidence_cap,
            }
        }

        pub(crate) fn charge_points(&mut self, points: usize) -> Result<(), LimitHit> {
            let units = u64::try_from(points).unwrap_or(u64::MAX);
            self.charge_work(units)
        }

        pub(crate) fn emit(&mut self, draft: FindingDraft) -> Result<(), LimitHit> {
            if let Some(hit) = self.hit {
                return Err(hit);
            }
            let findings_observed = self.counts.findings.saturating_add(1);
            if findings_observed > self.limits.findings {
                return self.fail(LimitHit {
                    axis: LimitAxis::Findings,
                    observed: findings_observed,
                    limit: self.limits.findings,
                });
            }

            let evidence_rows = u64::try_from(draft.evidence_len()).unwrap_or(u64::MAX);
            let evidence_observed = self.counts.evidence_rows.saturating_add(evidence_rows);
            if evidence_observed > self.limits.evidence_rows {
                return self.fail(LimitHit {
                    axis: LimitAxis::EvidenceRows,
                    observed: evidence_observed,
                    limit: self.limits.evidence_rows,
                });
            }

            let finding_bytes = draft.output_bytes_upper_bound(self.lens_id);
            let output_observed = self.counts.output_bytes.saturating_add(finding_bytes);
            if output_observed > self.limits.output_bytes {
                return self.fail(LimitHit {
                    axis: LimitAxis::OutputBytes,
                    observed: output_observed,
                    limit: self.limits.output_bytes,
                });
            }

            self.charge_work(evidence_rows)?;
            self.counts.findings = findings_observed;
            self.counts.evidence_rows = evidence_observed;
            self.counts.output_bytes = output_observed;
            self.findings.push(Finding::from_draft(
                self.lens_id,
                self.confidence_cap,
                draft,
            ));
            Ok(())
        }

        pub(crate) const fn limit_hit(&self) -> Option<LimitHit> {
            self.hit
        }

        fn charge_work(&mut self, units: u64) -> Result<(), LimitHit> {
            if let Some(hit) = self.hit {
                return Err(hit);
            }
            if self.budget.charge(units) {
                return Ok(());
            }
            self.fail(LimitHit {
                axis: LimitAxis::Work,
                observed: self.budget.spent().saturating_add(units),
                limit: self.budget.limit(),
            })
        }

        fn fail(&mut self, hit: LimitHit) -> Result<(), LimitHit> {
            self.hit.get_or_insert(hit);
            Err(hit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(id: i64) -> FindingScope {
        FindingScope {
            logical_section: "section",
            column: "column",
            identity: Arc::from(vec![IdentityValue::I64(id)]),
        }
    }

    #[test]
    fn confidence_orders_low_medium_high() {
        assert!(Confidence::LOW < Confidence::MEDIUM);
        assert!(Confidence::MEDIUM < Confidence::HIGH);
    }

    #[test]
    fn confidence_cap_strings_are_stable() {
        assert_eq!(ConfidenceCap::Low.as_str(), "low");
        assert_eq!(ConfidenceCap::Medium.as_str(), "medium");
        assert_eq!(ConfidenceCap::High.as_str(), "high");
    }

    #[test]
    fn weak_evidence_cannot_reach_high() {
        for evidence in [
            Evidence::Ratio,
            Evidence::Gauge,
            Evidence::Counter,
            Evidence::Event,
        ] {
            let finding = Finding::from_draft(
                "L",
                ConfidenceCap::High,
                FindingDraft::new(Role::Amplifier, scope(1), vec![evidence], None),
            );
            assert_eq!(finding.confidence(), Confidence::MEDIUM);
        }
    }

    #[test]
    fn sampled_lock_edge_can_reach_high_and_prove_direction() {
        let finding = Finding::from_draft(
            "PG-LOCK-012",
            ConfidenceCap::High,
            FindingDraft::new(
                Role::Lead,
                scope(1),
                vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                    10,
                    20,
                    30,
                    LockParticipant::Blocker,
                ))],
                None,
            ),
        );
        assert_eq!(finding.confidence(), Confidence::HIGH);
        assert_eq!(finding.role(), Role::Lead);
    }

    #[test]
    fn sampled_lock_edge_only_proves_the_role_of_its_participant() {
        let mut finding = Finding::from_draft(
            "PG-LOCK-012",
            ConfidenceCap::High,
            FindingDraft::new(
                Role::Downstream,
                scope(1),
                vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                    10,
                    20,
                    30,
                    LockParticipant::Blocker,
                ))],
                None,
            ),
        );
        assert_eq!(finding.role(), Role::Coincident);
        finding.apply_temporal_role(Role::Lead);
        assert_eq!(
            finding.role(),
            Role::Coincident,
            "observation time cannot reinterpret a conflicting lock edge"
        );

        let finding = Finding::from_draft(
            "PG-LOCK-012",
            ConfidenceCap::High,
            FindingDraft::new(
                Role::Downstream,
                scope(1),
                vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                    10,
                    20,
                    30,
                    LockParticipant::Waiter,
                ))],
                None,
            ),
        );
        assert_eq!(finding.role(), Role::Downstream);
    }

    #[test]
    fn resource_event_does_not_prove_direction() {
        let finding = Finding::from_draft(
            "OS-MEM-022",
            ConfidenceCap::High,
            FindingDraft::new(
                Role::Lead,
                scope(1),
                vec![Evidence::Direct(DirectEvidence::resource_limit_event())],
                None,
            ),
        );
        assert_eq!(finding.confidence(), Confidence::HIGH);
        assert_eq!(finding.role(), Role::Coincident);
    }

    #[test]
    fn unknown_clock_downgrades_unproven_direction() {
        let finding = Finding::from_draft(
            "TEMPORAL",
            ConfidenceCap::Medium,
            FindingDraft::new(Role::Downstream, scope(1), vec![Evidence::Counter], None),
        );
        assert_eq!(finding.role(), Role::Coincident);
    }

    #[test]
    fn same_clock_keeps_temporal_direction() {
        let context = super::super::engine::EvalContext::for_test(
            super::super::engine::ClockRelation::SameDomain,
        );
        let permit = context.temporal_direction();
        let finding = Finding::from_draft(
            "TEMPORAL",
            ConfidenceCap::Medium,
            FindingDraft::new(
                Role::Downstream,
                scope(1),
                vec![Evidence::Counter],
                permit.as_ref(),
            ),
        );
        assert_eq!(finding.role(), Role::Downstream);
    }

    #[test]
    fn empty_evidence_forces_low() {
        let finding = Finding::from_draft(
            "L",
            ConfidenceCap::High,
            FindingDraft::new(Role::Coincident, scope(1), vec![], None),
        );
        assert_eq!(finding.confidence(), Confidence::LOW);
    }

    #[test]
    fn scope_order_is_total() {
        assert!(scope(1) < scope(2));
    }

    #[test]
    fn gauge_evidence_rejects_non_finite_values_and_zero_denominators() {
        let entity = Arc::from(vec![IdentityValue::I64(1)]);
        assert!(
            GaugeEvidence::value(GaugeValueInput {
                operand: "bytes",
                value: f64::NAN,
                unit: GaugeUnit::Bytes,
                threshold: 1.0,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: 10,
                samples: 1,
                entity: GaugeEntity::new("section", Arc::clone(&entity)),
            })
            .is_none()
        );
        assert!(
            GaugeEvidence::ratio(
                GaugeRatio::new("a", 1.0, "b", 0.0, GaugeUnit::Count),
                0.5,
                ThresholdKind::AtLeast,
                10,
                1,
                GaugeEntity::new("section", entity),
            )
            .is_none()
        );
        assert!(
            GaugeEvidence::ratio(
                GaugeRatio::new("a", f64::MAX, "b", f64::MIN_POSITIVE, GaugeUnit::Bytes,),
                0.5,
                ThresholdKind::AtLeast,
                10,
                1,
                GaugeEntity::new("section", Arc::from([])),
            )
            .is_none()
        );
        assert!(
            GaugeEvidence::value(GaugeValueInput {
                operand: "count",
                value: 1.0,
                unit: GaugeUnit::Count,
                threshold: 1.0,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: 10,
                samples: 0,
                entity: GaugeEntity::new("section", Arc::from([])),
            })
            .is_none()
        );

        let per_call = GaugeEvidence::ratio(
            GaugeRatio::with_units(
                "total_exec_time",
                100.0,
                GaugeUnit::Milliseconds,
                "calls",
                2.0,
                GaugeUnit::Count,
                GaugeUnit::MillisecondsPerCall,
            ),
            50.0,
            ThresholdKind::AtLeast,
            10,
            1,
            GaugeEntity::new("section", Arc::from([])),
        )
        .expect("finite mixed-unit ratio");
        assert_eq!(per_call.unit(), GaugeUnit::MillisecondsPerCall);
    }

    #[test]
    fn counter_evidence_bounds_and_names_its_operands() {
        let operand = |name, purpose| {
            CounterOperand::new(name, 1.0, GaugeUnit::Count, purpose).expect("valid operand")
        };
        let build = |operands| {
            CounterEvidence::new(CounterEvidenceInput {
                kind: CounterMeasurementKind::Ratio,
                formula: "a / b",
                value: 1.0,
                unit: GaugeUnit::Ratio,
                threshold: 0.5,
                threshold_kind: ThresholdKind::AtLeast,
                operands,
                window: CounterEvidenceWindow::new(CounterEvidenceWindowInput {
                    selection_from_us: 0,
                    selection_to_us: 10,
                    first_interval_start_us: 0,
                    first_interval_end_us: 1,
                    last_interval_end_us: 1,
                    usable_intervals: 1,
                    candidate_intervals: 1,
                    unmatched_endpoint_intervals: 0,
                    unusable_delta_intervals: 0,
                    unaligned_duration_intervals: 0,
                    numeric_limit_intervals: 0,
                    elapsed_us: 1_000_000,
                })
                .expect("valid window"),
                entity: GaugeEntity::new("section", Arc::from([])),
            })
        };

        assert!(
            build(vec![
                operand("a", CounterOperandPurpose::Formula),
                operand("b", CounterOperandPurpose::Formula),
            ])
            .is_some()
        );
        assert!(
            build(vec![
                operand("same", CounterOperandPurpose::Formula),
                operand("same", CounterOperandPurpose::Formula),
            ])
            .is_none()
        );
        assert!(
            build(vec![
                operand("a", CounterOperandPurpose::Formula),
                operand("b", CounterOperandPurpose::Formula),
                operand("c", CounterOperandPurpose::AlignedContext),
                operand("d", CounterOperandPurpose::AlignedContext),
            ])
            .is_none()
        );
    }
}
