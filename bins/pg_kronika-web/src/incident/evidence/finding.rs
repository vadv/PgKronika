use std::sync::Arc;

use super::super::model::{EpisodeRefV1, IdentityValue};
use super::confidence::{Confidence, ConfidenceCap, Role};
use super::counter::CounterEvidence;
use super::direct::DirectEvidence;
use super::gauge::GaugeEvidence;

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
    pub(super) logical_section: &'static str,
    pub(super) column: &'static str,
    pub(super) identity: Arc<[IdentityValue]>,
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
}

impl FindingDraft {
    pub(crate) const fn new(
        requested_role: Role,
        scope: FindingScope,
        evidence: Vec<Evidence>,
    ) -> Self {
        Self {
            requested_role,
            scope,
            evidence,
        }
    }

    pub(crate) const fn evidence_len(&self) -> usize {
        self.evidence.len()
    }

    pub(super) fn output_bytes_upper_bound(&self, lens_id: &str) -> u64 {
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
    pub(super) fn from_draft(
        lens_id: &'static str,
        cap: ConfidenceCap,
        draft: FindingDraft,
    ) -> Self {
        let FindingDraft {
            requested_role,
            scope,
            evidence,
        } = draft;
        let structural_direction = evidence
            .iter()
            .any(|item| item.proves_structural_direction(requested_role));
        // Observation timestamps never authorize causal direction. A lead or
        // downstream role must match explicit structural evidence.
        let role = match requested_role {
            Role::Lead | Role::Downstream if !structural_direction => Role::Coincident,
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
