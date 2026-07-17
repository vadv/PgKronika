//! Lens contract and clock context.

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::ConfidenceCap;
use super::evidence::sink::FindingSink;
use super::series::SeriesSet;

/// A pure lens over preloaded series. Output and inspected points must pass
/// through `sink`.
pub(crate) trait Lens {
    fn id(&self) -> &'static str;
    fn inputs(&self) -> &'static [SectionColumn];
    fn confidence_cap(&self) -> ConfidenceCap;
    fn evaluate(
        &self,
        cluster: &Cluster,
        series: &SeriesSet,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit>;
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
        fn confidence_cap(&self) -> ConfidenceCap {
            ConfidenceCap::Low
        }
        fn evaluate(
            &self,
            _cluster: &Cluster,
            _series: &SeriesSet,
            _context: &EvalContext,
            _sink: &mut FindingSink<'_>,
        ) -> Result<(), LimitHit> {
            Ok(())
        }
    }

    #[test]
    fn a_same_domain_clock_allows_temporal_direction() {
        assert!(
            EvalContext::for_test(super::super::engine::ClockRelation::SameDomain)
                .temporal_direction()
                .is_some()
        );
    }

    #[test]
    fn an_unknown_clock_forbids_temporal_direction() {
        assert!(
            EvalContext::for_test(super::super::engine::ClockRelation::Unknown)
                .temporal_direction()
                .is_none()
        );
    }

    #[test]
    fn the_lens_trait_is_object_safe_and_callable() {
        let lens: Box<dyn Lens> = Box::new(SilentLens);
        assert_eq!(lens.id(), "TEST-000");
        assert!(lens.inputs().is_empty());
        assert_eq!(lens.confidence_cap(), ConfidenceCap::Low);

        let cluster = Cluster {
            start_us: 0,
            end_us: 10,
            members: Vec::new(),
        };
        let mut findings = Vec::new();
        let mut budget = super::super::dispatch::WorkBudget::new(10);
        let mut counts = super::super::evidence::sink::OutputCounts::new();
        let mut sink = FindingSink::new(
            &mut findings,
            &mut budget,
            &mut counts,
            super::super::evidence::sink::OutputLimits::new(1, 1),
            "TEST-000",
            ConfidenceCap::Low,
        );
        lens.evaluate(
            &cluster,
            &SeriesSet::for_test(0),
            &EvalContext::for_test(super::super::engine::ClockRelation::Unknown),
            &mut sink,
        )
        .expect("silent lens");
        assert!(findings.is_empty());
    }
}
