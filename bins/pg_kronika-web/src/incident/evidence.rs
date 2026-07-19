//! Evidence-gated confidence and direction.

use std::sync::Arc;

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

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Evidence {
    Direct(DirectEvidence),
    Ratio,
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
    }

    impl OutputCounts {
        pub(crate) const fn new() -> Self {
            Self {
                findings: 0,
                evidence_rows: 0,
            }
        }
    }

    #[derive(Clone, Copy)]
    pub(crate) struct OutputLimits {
        findings: u64,
        evidence_rows: u64,
    }

    impl OutputLimits {
        pub(crate) const fn new(findings: u64, evidence_rows: u64) -> Self {
            Self {
                findings,
                evidence_rows,
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

            self.charge_work(evidence_rows)?;
            self.counts.findings = findings_observed;
            self.counts.evidence_rows = evidence_observed;
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
            "lock_wait_graph",
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
            "memory_reclaim",
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
}
