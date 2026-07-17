//! The lens trait and the context a lens evaluates against.

use super::cluster::Cluster;
use super::dispatch::{SectionColumn, WorkBudget};
use super::evidence::{Confidence, Finding};
use super::series::SeriesSet;

/// Relationship between the timestamp domains being compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockRelation {
    /// One clock: the order of observations is meaningful.
    SameDomain,
    /// Unknown relationship: temporal order is not claimed.
    Unknown,
}

/// Per-incident context passed to every lens.
pub(crate) struct EvalContext {
    pub incident_start_us: i64,
    pub incident_end_us: i64,
    pub clock_relation: ClockRelation,
}

impl EvalContext {
    /// Whether a lens may assign a temporal `lead`/`downstream` role. False under
    /// an unknown clock: there only a structural lock edge proves direction.
    pub(crate) const fn allows_temporal_direction(&self) -> bool {
        matches!(self.clock_relation, ClockRelation::SameDomain)
    }
}

/// A domain lens: given a cluster and its preloaded series, it emits findings.
/// `evaluate` is pure — everything it needs is already decoded into `series`,
/// so it does no I/O — and it charges `budget` for the work it does.
pub(crate) trait Lens {
    fn id(&self) -> &'static str;
    fn inputs(&self) -> &'static [SectionColumn];
    fn confidence_cap(&self) -> Confidence;
    fn evaluate(
        &self,
        cluster: &Cluster,
        series: &SeriesSet,
        context: &EvalContext,
        budget: &mut WorkBudget,
    ) -> Vec<Finding>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SilentLens;

    impl Lens for SilentLens {
        fn id(&self) -> &'static str {
            "TEST-000"
        }
        fn inputs(&self) -> &'static [SectionColumn] {
            &[]
        }
        fn confidence_cap(&self) -> Confidence {
            Confidence::Low
        }
        fn evaluate(
            &self,
            _cluster: &Cluster,
            _series: &SeriesSet,
            _context: &EvalContext,
            _budget: &mut WorkBudget,
        ) -> Vec<Finding> {
            Vec::new()
        }
    }

    fn context(clock: ClockRelation) -> EvalContext {
        EvalContext {
            incident_start_us: 0,
            incident_end_us: 10,
            clock_relation: clock,
        }
    }

    #[test]
    fn a_same_domain_clock_allows_temporal_direction() {
        assert!(context(ClockRelation::SameDomain).allows_temporal_direction());
    }

    #[test]
    fn an_unknown_clock_forbids_temporal_direction() {
        assert!(!context(ClockRelation::Unknown).allows_temporal_direction());
    }

    #[test]
    fn the_lens_trait_is_object_safe_and_callable() {
        let lens: Box<dyn Lens> = Box::new(SilentLens);
        assert_eq!(lens.id(), "TEST-000");
        assert!(lens.inputs().is_empty());
        assert_eq!(lens.confidence_cap(), Confidence::Low);

        let cluster = Cluster {
            start_us: 0,
            end_us: 10,
            members: Vec::new(),
        };
        let mut budget = WorkBudget::new(10);
        let findings = lens.evaluate(
            &cluster,
            &SeriesSet::new(),
            &context(ClockRelation::Unknown),
            &mut budget,
        );
        assert!(findings.is_empty());
    }
}
