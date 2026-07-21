use super::coverage::SourceWindow;
use super::gauge::GaugeEntity;
use super::value::{FiniteValue, GaugeUnit, ThresholdKind};

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
    observed_period_us: Option<u64>,
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
    /// Median of the usable interval durations; `None` when the window held too
    /// few intervals to fix a stable source cadence.
    pub observed_period_us: Option<u64>,
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
            observed_period_us,
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
                observed_period_us,
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

    /// Incident-window coverage: the window is bounded by the incident, not by
    /// the usable intervals, so a collection that started late or stopped early
    /// shrinks completeness instead of hiding.
    pub(crate) fn source_window(&self) -> SourceWindow {
        SourceWindow::from_bounds(
            self.from_us,
            self.to_us,
            self.observed_period_us,
            usize::try_from(self.usable_intervals).unwrap_or(usize::MAX),
        )
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
