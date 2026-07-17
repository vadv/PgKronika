//! Confidence, evidence and the finding smart constructor.
//!
//! A finding's `confidence` is private and set only by [`Finding::new`], which
//! caps it at what the evidence supports. A lens may declare a `high` cap, but a
//! ratio or gauge cannot be minted into a `High` finding: the ceiling gates it.

/// Strength of a finding, ordered `Low < Medium < High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Confidence {
    Low,
    Medium,
    High,
}

/// Direction of a finding relative to the incident. Ordered so `lead` ranks
/// first when findings are sorted for presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Lead,
    Amplifier,
    Downstream,
    Coincident,
}

/// What a finding rests on. Only a lock edge or a stored resource-limit event
/// proves enough for a `High` ceiling; ratios, gauges and counters do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Evidence {
    /// A `pg_locks.blocked_by` edge: the blocking direction at the snapshot.
    LockEdge,
    /// A stored resource-limit event with known scope: OOM kill, ENOSPC, PANIC.
    ResourceLimit,
    /// A ratio of paired deltas.
    Ratio,
    /// A gauge reading.
    Gauge,
    /// A counter delta or rate.
    Counter,
    /// A typed log event that is not itself directional.
    Event,
}

impl Evidence {
    /// Whether this evidence, alone, justifies a `High` ceiling.
    const fn justifies_high(self) -> bool {
        matches!(self, Self::LockEdge | Self::ResourceLimit)
    }
}

/// The strongest confidence the given evidence supports. Empty evidence proves
/// nothing (`Low`); direct evidence reaches `High`; everything else caps at
/// `Medium`.
pub(crate) fn evidence_ceiling(evidence: &[Evidence]) -> Confidence {
    if evidence.is_empty() {
        Confidence::Low
    } else if evidence.iter().any(|e| e.justifies_high()) {
        Confidence::High
    } else {
        Confidence::Medium
    }
}

/// One lens verdict. `confidence` is private: it can only be set through
/// [`Finding::new`], so it never exceeds what the evidence supports.
pub(crate) struct Finding {
    lens_id: &'static str,
    role: Role,
    confidence: Confidence,
    evidence: Vec<Evidence>,
}

impl Finding {
    /// Build a finding, capping confidence at `min(cap, evidence_ceiling)`.
    pub(crate) fn new(
        lens_id: &'static str,
        role: Role,
        cap: Confidence,
        evidence: Vec<Evidence>,
    ) -> Self {
        let confidence = cap.min(evidence_ceiling(&evidence));
        Self {
            lens_id,
            role,
            confidence,
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

    pub(crate) fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_orders_low_medium_high() {
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
    }

    #[test]
    fn roles_rank_lead_first() {
        assert!(Role::Lead < Role::Amplifier);
        assert!(Role::Amplifier < Role::Downstream);
        assert!(Role::Downstream < Role::Coincident);
    }

    #[test]
    fn only_lock_edge_and_resource_limit_justify_high() {
        assert!(Evidence::LockEdge.justifies_high());
        assert!(Evidence::ResourceLimit.justifies_high());
        for weak in [
            Evidence::Ratio,
            Evidence::Gauge,
            Evidence::Counter,
            Evidence::Event,
        ] {
            assert!(!weak.justifies_high(), "{weak:?} must not reach high");
        }
    }

    #[test]
    fn empty_evidence_proves_nothing() {
        assert_eq!(evidence_ceiling(&[]), Confidence::Low);
    }

    #[test]
    fn ratio_and_gauge_cap_at_medium() {
        assert_eq!(evidence_ceiling(&[Evidence::Ratio]), Confidence::Medium);
        assert_eq!(
            evidence_ceiling(&[Evidence::Gauge, Evidence::Counter]),
            Confidence::Medium
        );
    }

    #[test]
    fn any_direct_evidence_reaches_high() {
        assert_eq!(evidence_ceiling(&[Evidence::LockEdge]), Confidence::High);
        assert_eq!(
            evidence_ceiling(&[Evidence::Ratio, Evidence::ResourceLimit]),
            Confidence::High,
            "one direct item lifts the ceiling"
        );
    }

    #[test]
    fn a_high_cap_lens_cannot_mint_high_from_a_ratio() {
        let finding = Finding::new(
            "PG-CACHE-010",
            Role::Amplifier,
            Confidence::High,
            vec![Evidence::Ratio],
        );
        assert_eq!(
            finding.confidence(),
            Confidence::Medium,
            "evidence ceiling overrides the cap"
        );
    }

    #[test]
    fn a_lock_edge_reaches_high_only_under_a_high_cap() {
        let capped = Finding::new(
            "PG-LOCK-012",
            Role::Lead,
            Confidence::High,
            vec![Evidence::LockEdge],
        );
        assert_eq!(capped.confidence(), Confidence::High);

        let lens_capped = Finding::new(
            "PG-LOCK-012",
            Role::Lead,
            Confidence::Medium,
            vec![Evidence::LockEdge],
        );
        assert_eq!(
            lens_capped.confidence(),
            Confidence::Medium,
            "the lens cap still bounds a direct edge"
        );
    }

    #[test]
    fn no_evidence_forces_low_regardless_of_cap() {
        let finding = Finding::new("X", Role::Coincident, Confidence::High, vec![]);
        assert_eq!(finding.confidence(), Confidence::Low);
    }

    #[test]
    fn a_finding_keeps_its_lens_role_and_evidence() {
        let finding = Finding::new(
            "PG-WAL-009",
            Role::Downstream,
            Confidence::Medium,
            vec![Evidence::Counter],
        );
        assert_eq!(finding.lens_id(), "PG-WAL-009");
        assert_eq!(finding.role(), Role::Downstream);
        assert_eq!(finding.evidence(), &[Evidence::Counter]);
    }
}
