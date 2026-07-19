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

    const fn high() -> Self {
        Self(2)
    }

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
            Self::High => Confidence::high(),
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
    SampledLockEdge,
    ResourceLimitEvent,
}

impl DirectEvidence {
    #[cfg(test)]
    const fn sampled_lock_edge() -> Self {
        Self {
            kind: DirectEvidenceKind::SampledLockEdge,
        }
    }

    #[cfg(test)]
    const fn resource_limit_event() -> Self {
        Self {
            kind: DirectEvidenceKind::ResourceLimitEvent,
        }
    }

    const fn proves_structural_direction(&self) -> bool {
        matches!(self.kind, DirectEvidenceKind::SampledLockEdge)
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
    Ratio,
    BytesPerSecond,
}

impl GaugeUnit {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Bytes => "bytes",
            Self::Kibibytes => "KiB",
            Self::Microseconds => "microseconds",
            Self::Ratio => "ratio",
            Self::BytesPerSecond => "bytes_per_second",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ThresholdKind {
    AtLeast,
    Below,
}

impl ThresholdKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::AtLeast => "at_least",
            Self::Below => "below",
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GaugeMeasurement {
    Value(FiniteValue),
    Ratio {
        numerator: FiniteValue,
        denominator: FiniteValue,
        operand_unit: GaugeUnit,
    },
    Trend {
        first: FiniteValue,
        last: FiniteValue,
        elapsed_us: u64,
        operand_unit: GaugeUnit,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GaugeRatio {
    numerator: f64,
    denominator: f64,
    operand_unit: GaugeUnit,
}

pub(crate) struct GaugeTrendInput {
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

impl GaugeRatio {
    pub(crate) const fn new(numerator: f64, denominator: f64, operand_unit: GaugeUnit) -> Self {
        Self {
            numerator,
            denominator,
            operand_unit,
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
    pub(crate) fn value(
        value: f64,
        unit: GaugeUnit,
        threshold: f64,
        threshold_kind: ThresholdKind,
        observed_at_us: i64,
        samples: usize,
        entity: GaugeEntity,
    ) -> Option<Self> {
        let samples = u64::try_from(samples).ok()?;
        (samples > 0 && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Value(FiniteValue::new(value)?),
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
        (ratio.denominator > 0.0).then_some(())?;
        FiniteValue::new(ratio.numerator / ratio.denominator)?;
        let samples = u64::try_from(samples).ok()?;
        (samples > 0 && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Ratio {
                numerator: FiniteValue::new(ratio.numerator)?,
                denominator: FiniteValue::new(ratio.denominator)?,
                operand_unit: ratio.operand_unit,
            },
            unit: GaugeUnit::Ratio,
            threshold: FiniteValue::new(threshold)?,
            threshold_kind,
            observed_at_us,
            samples,
            entity,
        })
    }

    pub(crate) fn trend(input: GaugeTrendInput) -> Option<Self> {
        let GaugeTrendInput {
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
        (samples >= 2 && !entity.section().is_empty()).then_some(())?;
        Some(Self {
            measurement: GaugeMeasurement::Trend {
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
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Evidence {
    Direct(DirectEvidence),
    Ratio,
    GaugeObservation(GaugeEvidence),
    Gauge,
    Counter,
    Event,
}

impl Evidence {
    const fn justifies_high(&self) -> bool {
        matches!(self, Self::Direct(_))
    }

    const fn proves_structural_direction(&self) -> bool {
        matches!(self, Self::Direct(direct) if direct.proves_structural_direction())
    }

    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct",
            Self::Ratio => "ratio",
            Self::GaugeObservation(_) | Self::Gauge => "gauge",
            Self::Counter => "counter",
            Self::Event => "event",
        }
    }
}

fn evidence_ceiling(evidence: &[Evidence]) -> Confidence {
    if evidence.is_empty() {
        Confidence::LOW
    } else if evidence.iter().any(Evidence::justifies_high) {
        Confidence::high()
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
                    .saturating_add(
                        u64::try_from(gauge.entity().section().len()).unwrap_or(u64::MAX),
                    )
                    .saturating_add(identity_json_upper_bound(gauge.entity().identity())),
                Evidence::Direct(_)
                | Evidence::Ratio
                | Evidence::Gauge
                | Evidence::Counter
                | Evidence::Event => 32,
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
        let structural_direction = evidence.iter().any(Evidence::proves_structural_direction);
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
        assert!(Confidence::MEDIUM < Confidence::high());
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
                vec![Evidence::Direct(DirectEvidence::sampled_lock_edge())],
                None,
            ),
        );
        assert_eq!(finding.confidence(), Confidence::high());
        assert_eq!(finding.role(), Role::Lead);
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
        assert_eq!(finding.confidence(), Confidence::high());
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
            GaugeEvidence::value(
                f64::NAN,
                GaugeUnit::Bytes,
                1.0,
                ThresholdKind::AtLeast,
                10,
                1,
                GaugeEntity::new("section", Arc::clone(&entity)),
            )
            .is_none()
        );
        assert!(
            GaugeEvidence::ratio(
                GaugeRatio::new(1.0, 0.0, GaugeUnit::Count),
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
                GaugeRatio::new(f64::MAX, f64::MIN_POSITIVE, GaugeUnit::Bytes),
                0.5,
                ThresholdKind::AtLeast,
                10,
                1,
                GaugeEntity::new("section", Arc::from([])),
            )
            .is_none()
        );
        assert!(
            GaugeEvidence::value(
                1.0,
                GaugeUnit::Count,
                1.0,
                ThresholdKind::AtLeast,
                10,
                0,
                GaugeEntity::new("section", Arc::from([])),
            )
            .is_none()
        );
    }
}
