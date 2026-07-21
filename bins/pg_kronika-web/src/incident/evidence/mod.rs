//! Evidence-gated confidence and direction.

mod confidence;
mod counter;
mod direct;
mod finding;
mod gauge;
pub(super) mod sink;
#[cfg(test)]
mod tests;
mod value;

#[allow(
    unused_imports,
    reason = "facade preserves the pre-split crate-private evidence paths"
)]
pub(crate) use confidence::Confidence;
pub(crate) use confidence::{ConfidenceCap, Role};
pub(crate) use counter::{
    CounterEvidence, CounterEvidenceInput, CounterEvidenceWindow, CounterEvidenceWindowInput,
    CounterMeasurementKind, CounterOperand, CounterOperandPurpose,
};
pub(crate) use direct::{DirectEvidence, LockParticipant, SampledLockEdge};
pub(crate) use finding::{Evidence, Finding, FindingDraft, FindingScope};
pub(crate) use gauge::{
    GaugeEntity, GaugeEvidence, GaugeMeasurement, GaugeRatio, GaugeTrendInput, GaugeValueInput,
};
#[allow(
    unused_imports,
    reason = "facade preserves the pre-split crate-private evidence paths"
)]
pub(crate) use value::FiniteValue;
pub(crate) use value::{GaugeUnit, ThresholdKind};
