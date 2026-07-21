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
