//! Catalog dispatch and request work limits.

use std::collections::{BTreeMap, BTreeSet};

/// One input a lens reads: a logical section and one of its columns. The section
/// name resolves to concrete `type_id`s at read time, so a lens never names a
/// version-specific layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SectionColumn {
    pub section: &'static str,
    pub column: &'static str,
}

pub(crate) struct WorkBudget {
    limit: u64,
    remaining: u64,
    exhausted: bool,
}

impl WorkBudget {
    pub(crate) const fn new(limit: u64) -> Self {
        Self {
            limit,
            remaining: limit,
            exhausted: false,
        }
    }

    pub(crate) const fn charge(&mut self, units: u64) -> bool {
        if self.exhausted {
            return false;
        }
        if let Some(rest) = self.remaining.checked_sub(units) {
            self.remaining = rest;
            true
        } else {
            self.exhausted = true;
            false
        }
    }

    pub(crate) const fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    pub(crate) const fn spent(&self) -> u64 {
        self.limit - self.remaining
    }

    pub(crate) const fn limit(&self) -> u64 {
        self.limit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LimitAxis {
    Work,
    LensEvaluations,
    Findings,
    EvidenceRows,
    OutputBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LimitHit {
    pub axis: LimitAxis,
    pub observed: u64,
    pub limit: u64,
}

/// Map each logical section to the lenses that read it. Built once from the
/// catalog so the engine dispatches only candidate lenses per cluster instead
/// of scanning every lens.
pub(crate) fn section_index(
    lens_inputs: &[&[SectionColumn]],
) -> BTreeMap<&'static str, Vec<usize>> {
    let mut index: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
    for (lens, inputs) in lens_inputs.iter().enumerate() {
        for input in *inputs {
            let entry = index.entry(input.section).or_default();
            if entry.last() != Some(&lens) {
                entry.push(lens);
            }
        }
    }
    index
}

/// The lenses whose input sections appear among `sections`, in ascending index
/// order.
pub(crate) fn candidate_lenses(
    index: &BTreeMap<&'static str, Vec<usize>>,
    sections: &BTreeSet<&'static str>,
) -> BTreeSet<usize> {
    let mut candidates = BTreeSet::new();
    for section in sections {
        if let Some(lenses) = index.get(section) {
            candidates.extend(lenses.iter().copied());
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::evidence::sink::{FindingSink, OutputCounts, OutputLimits};
    use crate::incident::evidence::{ConfidenceCap, Evidence, FindingDraft, FindingScope, Role};
    use crate::incident::model::{EpisodeRefV1, IdentityValue};
    use std::sync::Arc;

    fn sc(section: &'static str, column: &'static str) -> SectionColumn {
        SectionColumn { section, column }
    }

    fn finding(evidence: Vec<Evidence>) -> FindingDraft {
        let reference = EpisodeRefV1 {
            logical_section: "s",
            column: "c",
            identity: Arc::from(vec![IdentityValue::I64(1)]),
            start_us: 0,
            end_us: 1,
        };
        FindingDraft::new(
            Role::Coincident,
            FindingScope::from_episode(&reference),
            evidence,
        )
    }

    #[test]
    fn a_new_budget_is_unspent_and_available() {
        let budget = WorkBudget::new(100);
        assert_eq!(budget.spent(), 0);
        assert!(!budget.is_exhausted());
    }

    #[test]
    fn a_charge_within_budget_is_spent() {
        let mut budget = WorkBudget::new(100);
        assert!(budget.charge(30));
        assert!(budget.charge(20));
        assert_eq!(budget.spent(), 50);
        assert!(!budget.is_exhausted());
    }

    #[test]
    fn a_charge_to_exactly_zero_is_allowed() {
        let mut budget = WorkBudget::new(40);
        assert!(budget.charge(40));
        assert_eq!(budget.spent(), 40);
        assert!(!budget.is_exhausted(), "an exact fit is not exhaustion");
    }

    #[test]
    fn an_overcharge_latches_exhaustion_and_spends_nothing() {
        let mut budget = WorkBudget::new(40);
        assert!(budget.charge(30));
        assert!(!budget.charge(20), "20 does not fit in the remaining 10");
        assert!(budget.is_exhausted());
        assert_eq!(
            budget.spent(),
            30,
            "the failed charge adds nothing; spent stays the work that fit"
        );
    }

    #[test]
    fn charging_after_exhaustion_stays_false() {
        let mut budget = WorkBudget::new(10);
        assert!(!budget.charge(11));
        assert!(
            !budget.charge(1),
            "exhaustion is sticky, even for a tiny charge"
        );
        assert!(budget.is_exhausted());
    }

    #[test]
    fn a_zero_charge_is_a_noop() {
        let mut budget = WorkBudget::new(10);
        assert!(budget.charge(0));
        assert_eq!(budget.spent(), 0);
        assert!(!budget.is_exhausted());
    }

    #[test]
    fn a_zero_limit_exhausts_on_the_first_positive_charge() {
        let mut budget = WorkBudget::new(0);
        assert!(budget.charge(0), "zero fits in zero");
        assert!(!budget.charge(1));
        assert!(budget.is_exhausted());
    }

    #[test]
    fn an_empty_catalog_indexes_nothing() {
        assert!(section_index(&[]).is_empty());
    }

    #[test]
    fn a_lens_indexes_each_of_its_sections_once() {
        let lens0: &[SectionColumn] = &[sc("a", "x"), sc("a", "y"), sc("b", "z")];
        let index = section_index(&[lens0]);
        assert_eq!(
            index.get("a"),
            Some(&vec![0]),
            "two columns of a add lens 0 once"
        );
        assert_eq!(index.get("b"), Some(&vec![0]));
    }

    #[test]
    fn multiple_lenses_on_a_section_are_both_listed() {
        let lens0: &[SectionColumn] = &[sc("a", "x")];
        let lens1: &[SectionColumn] = &[sc("a", "y")];
        let index = section_index(&[lens0, lens1]);
        assert_eq!(index.get("a"), Some(&vec![0, 1]));
    }

    #[test]
    fn candidates_union_the_lenses_of_present_sections() {
        let lens0: &[SectionColumn] = &[sc("a", "x")];
        let lens1: &[SectionColumn] = &[sc("b", "y")];
        let lens2: &[SectionColumn] = &[sc("a", "z"), sc("c", "w")];
        let index = section_index(&[lens0, lens1, lens2]);
        let present = BTreeSet::from(["a", "b"]);
        assert_eq!(
            candidate_lenses(&index, &present),
            BTreeSet::from([0, 1, 2]),
            "a -> {{0,2}}, b -> {{1}}"
        );
    }

    #[test]
    fn candidates_ignore_absent_sections() {
        let lens0: &[SectionColumn] = &[sc("a", "x")];
        let index = section_index(&[lens0]);
        let present = BTreeSet::from(["z"]);
        assert!(candidate_lenses(&index, &present).is_empty());
    }

    #[test]
    fn sink_applies_finding_and_evidence_limits_before_retaining_output() {
        let mut findings = Vec::new();
        let mut budget = WorkBudget::new(10);
        let mut counts = OutputCounts::new();
        let mut sink = FindingSink::new(
            &mut findings,
            &mut budget,
            &mut counts,
            OutputLimits::new(1, 1),
            "L",
            ConfidenceCap::Medium,
        );
        sink.emit(finding(vec![Evidence::Ratio]))
            .expect("first finding fits");
        assert_eq!(
            sink.emit(finding(vec![])),
            Err(LimitHit {
                axis: LimitAxis::Findings,
                observed: 2,
                limit: 1,
            })
        );
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn failed_evidence_charge_is_all_or_nothing() {
        let mut findings = Vec::new();
        let mut budget = WorkBudget::new(10);
        let mut counts = OutputCounts::new();
        let mut sink = FindingSink::new(
            &mut findings,
            &mut budget,
            &mut counts,
            OutputLimits::new(2, 1),
            "L",
            ConfidenceCap::Medium,
        );
        assert_eq!(
            sink.emit(finding(vec![Evidence::Ratio, Evidence::Gauge])),
            Err(LimitHit {
                axis: LimitAxis::EvidenceRows,
                observed: 2,
                limit: 1,
            })
        );
        assert!(findings.is_empty());
        assert_eq!(budget.spent(), 0);
    }

    #[test]
    fn output_byte_limit_is_checked_before_retaining_a_finding() {
        let mut findings = Vec::new();
        let mut budget = WorkBudget::new(10);
        let mut counts = OutputCounts::new();
        let mut sink = FindingSink::new(
            &mut findings,
            &mut budget,
            &mut counts,
            OutputLimits::bounded(10, 10, 1),
            "L",
            ConfidenceCap::Medium,
        );
        let hit = sink
            .emit(finding(vec![Evidence::Ratio]))
            .expect_err("one byte cannot hold a finding");
        assert_eq!(hit.axis, LimitAxis::OutputBytes);
        assert!(findings.is_empty());
        assert_eq!(budget.spent(), 0);
    }
}
