//! Health-line serialization for the timeline endpoints.
//!
//! The health formula lives in `kronika-analytics`; this module only clamps the
//! step, calls [`health_line`], and serializes the points. Continuous scores are
//! `None` until gauge factors are retained, so a no-data bucket serializes as
//! `unknown` with null scores — never a false green.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kronika_analytics::overview::{
    Applicability, CoverageSpan, CoverageState, DomainPenalty, EventObservation, FactorCoverage,
    FloorClass, FloorEvidence, HealthPoint, HealthState, OVERVIEW_HEALTH_LIMITS, RetainedExactness,
    downsample_worst, health_line, overview_health_policy,
};
use serde_json::{Value, json};

/// Default and absolute health-point ceilings (§14.3).
pub(crate) const MAX_HEALTH_POINTS: u64 = 2_000;

/// The effective step in microseconds (§6.2).
///
/// The step is at least `ceil(span / MAX_HEALTH_POINTS)` so a range never yields
/// more than the point ceiling, and at least the requested step. The range is
/// not rounded; the last bucket may be shorter.
pub(crate) fn effective_step_us(from_us: i64, to_us: i64, requested: Option<u64>) -> u64 {
    let span = u64::try_from(to_us.saturating_sub(from_us)).unwrap_or(u64::MAX);
    let floor = span.div_ceil(MAX_HEALTH_POINTS).max(1);
    requested.unwrap_or(0).max(floor)
}

/// The serialized health line plus its overview summary.
pub(crate) struct HealthLineJson {
    pub(crate) policy_version: u32,
    pub(crate) points: Vec<Value>,
    pub(crate) factor_set_ids: Vec<Value>,
    pub(crate) coverage: Vec<Value>,
    pub(crate) worst_point: Value,
    pub(crate) latest_point: Value,
}

/// Computes and serializes the health line over `range` at `step_us`.
///
/// Returns `None` only if the fixed policy configuration is invalid or a cell
/// exceeds a health limit — both configuration faults, mapped to a 500 by the
/// caller.
pub(crate) fn compute_health(
    observations: &[EventObservation],
    range: CoverageSpan,
    step_us: u64,
) -> Option<HealthLineJson> {
    let policy = overview_health_policy().ok()?;
    let points = health_line(observations, range, step_us, &policy).ok()?;

    let points_json: Vec<Value> = points.iter().map(health_point_json).collect();
    let mut factor_set_ids: Vec<String> = points
        .iter()
        .map(|point| URL_SAFE_NO_PAD.encode(point.factor_set_id().0))
        .collect();
    factor_set_ids.sort_unstable();
    factor_set_ids.dedup();
    let factor_set_ids = factor_set_ids.into_iter().map(Value::from).collect();

    let coverage = points
        .last()
        .map(|point| point.coverage().iter().map(factor_coverage_json).collect())
        .unwrap_or_default();
    let worst_point = downsample_worst(&points, range, OVERVIEW_HEALTH_LIMITS)
        .ok()
        .flatten()
        .map_or(Value::Null, |downsampled| {
            health_point_json(downsampled.representative())
        });
    let latest_point = points.last().map_or(Value::Null, health_point_json);

    Some(HealthLineJson {
        policy_version: policy.version(),
        points: points_json,
        factor_set_ids,
        coverage,
        worst_point,
        latest_point,
    })
}

/// The `health_summary` and `coverage` values for the overview response.
pub(crate) fn overview_health_summary(
    observations: &[EventObservation],
    range: CoverageSpan,
) -> (Value, Value) {
    let from_us = range.start_us();
    let to_us = range.end_us();
    let step = effective_step_us(from_us, to_us, None);
    match compute_health(observations, range, step) {
        Some(line) => (
            json!({ "worst_point": line.worst_point, "latest_point": line.latest_point }),
            Value::Array(line.coverage),
        ),
        None => (
            json!({ "worst_point": Value::Null, "latest_point": Value::Null }),
            Value::Array(Vec::new()),
        ),
    }
}

fn health_point_json(point: &HealthPoint) -> Value {
    json!({
        "interval": span_json(point.interval()),
        "continuous_score": point.continuous_score(),
        "overall_score": point.overall_score(),
        "overall_state": health_state_name(point.overall_state()),
        "health_policy_version": point.health_policy_version(),
        "factor_set_id": URL_SAFE_NO_PAD.encode(point.factor_set_id().0),
        "domains": point.domain_penalties().iter().map(domain_penalty_json).collect::<Vec<_>>(),
        "floor_evidence": point.floor_evidence().iter().map(floor_evidence_json).collect::<Vec<_>>(),
        "coverage": point.coverage().iter().map(factor_coverage_json).collect::<Vec<_>>(),
    })
}

fn domain_penalty_json(penalty: &DomainPenalty) -> Value {
    json!({
        "domain": penalty.domain().code(),
        "penalty": penalty.penalty(),
        "driving_factor_ids": penalty.driving_factor_ids().iter().map(|id| id.0).collect::<Vec<_>>(),
    })
}

fn floor_evidence_json(evidence: &FloorEvidence) -> Value {
    json!({
        "class": floor_class_name(evidence.class),
        "supporting_fact_id": URL_SAFE_NO_PAD.encode(evidence.supporting_fact_id.0),
    })
}

/// Serializes one factor's per-point coverage (§8.5); exposed for the overview
/// response.
pub(crate) fn factor_coverage_json(coverage: &FactorCoverage) -> Value {
    json!({
        "factor_id": coverage.factor_id.0,
        "applicability": applicability_name(coverage.applicability),
        "state": coverage_state_name(coverage.state),
        "interval": span_json(coverage.interval),
        "present_samples": coverage.present_samples,
        "covered_duration_us": coverage.covered_duration_us,
        "retained_exactness": retained_exactness_name(coverage.retained_exactness),
    })
}

fn span_json(span: CoverageSpan) -> Value {
    json!({ "from_us": span.start_us(), "to_us": span.end_us() })
}

const fn health_state_name(state: HealthState) -> &'static str {
    match state {
        HealthState::Unknown => "unknown",
        HealthState::Normal => "normal",
        HealthState::Degraded => "degraded",
        HealthState::Critical => "critical",
    }
}

const fn floor_class_name(class: FloorClass) -> &'static str {
    match class {
        FloorClass::Availability => "availability",
        FloorClass::Integrity => "integrity",
        FloorClass::OomKill => "oom_kill",
        FloorClass::DiskFull => "disk_full",
    }
}

const fn applicability_name(applicability: Applicability) -> &'static str {
    match applicability {
        Applicability::Applicable => "applicable",
        Applicability::NotApplicable => "not_applicable",
        Applicability::Unsupported => "unsupported",
    }
}

const fn coverage_state_name(state: CoverageState) -> &'static str {
    match state {
        CoverageState::Complete => "complete",
        CoverageState::Partial => "partial",
        CoverageState::Gap => "gap",
        CoverageState::Unknown => "unknown",
        CoverageState::NotCollected => "not_collected",
    }
}

const fn retained_exactness_name(exactness: RetainedExactness) -> &'static str {
    match exactness {
        RetainedExactness::Exact => "retained_exact",
        RetainedExactness::LowerBound => "lower_bound",
        RetainedExactness::Unknown => "unknown",
    }
}

/// The domain codes are stable; assert the mapping stays covered.
#[cfg(test)]
mod tests {
    use kronika_analytics::overview::DomainId;

    use super::*;

    #[test]
    fn the_effective_step_never_exceeds_the_point_ceiling() {
        // A one-second range with a tiny requested step still floors the step so
        // the range yields at most MAX_HEALTH_POINTS buckets.
        let span = 1_000_000_u64;
        let step = effective_step_us(0, i64::try_from(span).expect("fits"), Some(1));
        let buckets = span.div_ceil(step);
        assert!(buckets <= MAX_HEALTH_POINTS, "{buckets} buckets");
    }

    #[test]
    fn a_requested_step_wins_when_coarser_than_the_floor() {
        let step = effective_step_us(0, 1_000_000, Some(500_000));
        assert_eq!(step, 500_000, "a coarse requested step is honoured");
    }

    #[test]
    fn domain_codes_are_stable() {
        assert_eq!(
            DomainId::DatabaseErrorPressure.code(),
            "database_error_pressure"
        );
    }
}
