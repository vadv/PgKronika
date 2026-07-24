//! Event-driven health line: floor evidence over request buckets.
//!
//! This module computes a health line from retained event observations alone.
//! Continuous resource pressure needs gauge and counter factors, which the
//! current fact schema does not yet retain, so every bucket's continuous and
//! overall numeric scores are `None`: a missing required domain never becomes a
//! false green. Only structured evidence that satisfies the floor policy can
//! drive a bucket to `Critical`; parsed `PostgreSQL` log text remains notable
//! evidence but is not an outage, mount, or corruption proof.
//!
//! The formula lives here, not in the web layer: the caller buckets a range,
//! calls [`health_line`], and serializes the returned points.

use super::counts::{Severity, SqlState};
use super::coverage::{
    Applicability, BoundaryQuality, CoverageSpan, CoverageState, PeriodQuality,
    PhysicalCountSemantics, RetainedExactness, SourceCompleteness,
};
use super::health::{
    DomainId, FactorCoverage, FactorId, FloorClass, FloorEvidence, HealthEvaluationError,
    HealthLimits, HealthPoint, HealthPolicy, HealthResource, InvalidFactorProfile,
    InvalidHealthPolicy, RequiredFactorProfile,
};
use super::observation::{EventObservation, EvidenceQuality, FactId, ObservationPayload};

const SQLSTATE_DATA_CORRUPTED: SqlState = SqlState(*b"XX001");
const SQLSTATE_INDEX_CORRUPTED: SqlState = SqlState(*b"XX002");

/// The single required health factor, held uncovered until gauge extraction
/// lands: its absence keeps continuous scores honestly unknown.
pub const OVERVIEW_HEALTH_FACTOR: FactorId = FactorId(1);

/// The domain the required factor belongs to.
pub const OVERVIEW_HEALTH_DOMAIN: DomainId = DomainId::DatabaseErrorPressure;

/// Bounds for overview health evaluation.
pub const OVERVIEW_HEALTH_LIMITS: HealthLimits = HealthLimits {
    max_profile_factors: 8,
    max_cell_factors: 8,
    max_coverage_entries: 8,
    max_floor_evidence: 65_536,
    max_downsample_points: 10_000,
};

/// A failure while configuring the overview health policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthConfigError {
    /// The fixed required-factor profile was rejected.
    Profile(InvalidFactorProfile),
    /// The fixed health policy was rejected.
    Policy(InvalidHealthPolicy),
}

/// Builds the v1 overview health policy.
///
/// The policy requires one domain whose factor is never covered from events
/// alone, so numeric scores stay `None`; floors still classify availability.
///
/// # Errors
///
/// Returns [`HealthConfigError`] if the fixed configuration is ever made
/// inconsistent.
pub fn overview_health_policy() -> Result<HealthPolicy, HealthConfigError> {
    let profile = RequiredFactorProfile::new(
        1,
        vec![(OVERVIEW_HEALTH_DOMAIN, vec![OVERVIEW_HEALTH_FACTOR])],
        Vec::new(),
        OVERVIEW_HEALTH_LIMITS,
    )
    .map_err(HealthConfigError::Profile)?;
    HealthPolicy::new(
        super::HEALTH_POLICY_VERSION,
        super::REDUCTION_SEMANTICS_VERSION,
        profile,
        0.8,
        0.5,
        OVERVIEW_HEALTH_LIMITS,
    )
    .map_err(HealthConfigError::Policy)
}

/// The trusted floor class of an observation, if it is one.
///
/// A lifecycle line does not prove postmaster unavailability. Parsed or
/// heuristic error text also cannot prove a floor. A structured `PANIC` can
/// prove an availability floor and a structured `XX001`/`XX002` can prove an
/// integrity floor. `53100` remains non-floor evidence without a mount-specific
/// capacity fact.
#[must_use]
pub fn observation_floor(observation: &EventObservation) -> Option<FloorClass> {
    match observation.payload() {
        ObservationPayload::ErrorGroup(payload) => error_floor(
            observation.evidence_quality(),
            payload.severity,
            payload.sqlstate,
        ),
        _ => None,
    }
}

fn error_floor(
    evidence_quality: EvidenceQuality,
    severity: Severity,
    sqlstate: Option<SqlState>,
) -> Option<FloorClass> {
    if !matches!(
        evidence_quality,
        EvidenceQuality::Structured | EvidenceQuality::DerivedExact
    ) {
        return None;
    }
    if severity == Severity::Panic {
        return Some(FloorClass::Availability);
    }
    match sqlstate {
        Some(SQLSTATE_DATA_CORRUPTED | SQLSTATE_INDEX_CORRUPTED) => Some(FloorClass::Integrity),
        _ => None,
    }
}

/// Computes a health point per `step_us` bucket across `range`.
///
/// Each bucket carries floor evidence from the observations whose sort time
/// falls in it, plus one uncovered required-factor coverage entry. The last
/// bucket may be shorter than `step_us`; the range is not rounded.
///
/// # Errors
///
/// Returns [`HealthEvaluationError`] when a cell exceeds a health limit or the
/// coverage/floor inputs violate an evaluation invariant.
pub fn health_line(
    observations: &[EventObservation],
    range: CoverageSpan,
    step_us: u64,
    policy: &HealthPolicy,
) -> Result<Vec<HealthPoint>, HealthEvaluationError> {
    let step = i64::try_from(step_us.max(1)).unwrap_or(i64::MAX);
    let mut buckets = Vec::new();
    let mut start = range.start_us();
    while start < range.end_us() {
        if buckets.len() == OVERVIEW_HEALTH_LIMITS.max_downsample_points {
            return Err(HealthEvaluationError::LimitExceeded(
                HealthResource::DownsamplePoints,
            ));
        }
        let end = start.saturating_add(step).min(range.end_us());
        let Some(bucket) = CoverageSpan::new(start, end) else {
            break;
        };
        buckets.push((bucket, Vec::new()));
        start = end;
    }

    for observation in observations {
        let ts = observation.time().sort_ts_us;
        if ts < range.start_us() || ts >= range.end_us() {
            continue;
        }
        let relative = i128::from(ts) - i128::from(range.start_us());
        let index = usize::try_from(relative / i128::from(step)).map_err(|_error| {
            HealthEvaluationError::LimitExceeded(HealthResource::DownsamplePoints)
        })?;
        let Some((_, floors)) = buckets.get_mut(index) else {
            return Err(HealthEvaluationError::LimitExceeded(
                HealthResource::DownsamplePoints,
            ));
        };
        if let Some(class) = observation_floor(observation) {
            if floors.len() == OVERVIEW_HEALTH_LIMITS.max_floor_evidence {
                return Err(HealthEvaluationError::LimitExceeded(
                    HealthResource::FloorEvidence,
                ));
            }
            floors.push(FloorEvidence {
                class,
                supporting_fact_id: FactId(observation.observation_id().0),
            });
        }
    }

    buckets
        .into_iter()
        .map(|(bucket, floors)| {
            let coverage = vec![uncovered_required_coverage(bucket)];
            policy.evaluate_cell(bucket, &[], coverage, floors)
        })
        .collect()
}

/// A coverage entry marking the required factor as not collected, so the domain
/// is unknown and the continuous score stays `None`.
const fn uncovered_required_coverage(bucket: CoverageSpan) -> FactorCoverage {
    FactorCoverage {
        factor_id: OVERVIEW_HEALTH_FACTOR,
        applicability: Applicability::Applicable,
        state: CoverageState::NotCollected,
        interval: bucket,
        expected_period_us: None,
        period_quality: PeriodQuality::Unknown,
        cadence_epoch_id: None,
        crosses_cadence_boundary: false,
        present_samples: 0,
        covered_duration_us: 0,
        source_population: None,
        loss_reasons: Vec::new(),
        lost_count_lower_bound: None,
        retained_exactness: RetainedExactness::Unknown,
        source_completeness: SourceCompleteness::Unknown,
        physical_count_semantics: PhysicalCountSemantics::NotApplicable,
        boundary_quality: BoundaryQuality::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::super::health::HealthState;
    use super::*;
    use crate::overview::observation::{
        DroppedFieldCount, ErrorGroupPayload, LifecyclePayload, NamingContractId,
        ObservationProvenance, ObservationShape, ObservationTime, QualityFlags, SectionBodyId,
        SegmentIdentity, SegmentLocator, SourceScopeId, TimeQuality,
    };
    use crate::overview::{DictionaryContextId, ErrorCategory};

    fn span(from: i64, to: i64) -> CoverageSpan {
        CoverageSpan::new(from, to).expect("valid span")
    }

    fn lineage() -> SegmentIdentity {
        SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([2; 16]),
            SegmentLocator([3; 32]),
            7,
            b"type=7",
        )
    }

    fn provenance(row: u32) -> ObservationProvenance {
        ObservationProvenance {
            segment_locator: Some(SegmentLocator([3; 32])),
            section_body_id: SectionBodyId([0xAA; 32]),
            catalog_entry_ordinal: 0,
            row_ordinal: row,
            dictionary_context_id: DictionaryContextId([0xBB; 32]),
            source_locator: None,
        }
    }

    fn panic(row: u32, ts: i64, evidence_quality: EvidenceQuality) -> EventObservation {
        let payload = ErrorGroupPayload {
            severity: Severity::Panic,
            category: ErrorCategory::System,
            sqlstate: None,
            normalized_pattern: None,
            sample: None,
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: None,
            user: None,
            dropped_field_count: DroppedFieldCount::default(),
        };
        EventObservation::new(
            lineage(),
            7,
            provenance(row),
            ObservationShape::GroupedCount,
            ObservationTime {
                sort_ts_us: ts,
                occurred_at_us: Some(ts),
                observed_interval: None,
                quality: TimeQuality::FirstInGroup,
            },
            1,
            ObservationPayload::ErrorGroup(Box::new(payload)),
            evidence_quality,
            QualityFlags::default(),
            None,
        )
        .expect("valid panic fixture")
    }

    fn crash(row: u32, ts: i64) -> EventObservation {
        let payload = LifecyclePayload {
            pid: Some(1),
            signal: Some(6),
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dropped_field_count: DroppedFieldCount::default(),
        };
        EventObservation::new(
            lineage(),
            7,
            provenance(row),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: ts,
                occurred_at_us: Some(ts),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ChildSignalTermination(Box::new(payload)),
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
        .expect("valid crash fixture")
    }

    fn error(
        row: u32,
        ts: i64,
        sqlstate: [u8; 5],
        evidence_quality: EvidenceQuality,
    ) -> EventObservation {
        let payload = ErrorGroupPayload {
            severity: Severity::Error,
            category: ErrorCategory::Resource,
            sqlstate: Some(SqlState(sqlstate)),
            normalized_pattern: None,
            sample: None,
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: None,
            user: None,
            dropped_field_count: DroppedFieldCount::default(),
        };
        EventObservation::new(
            lineage(),
            7,
            provenance(row),
            ObservationShape::GroupedCount,
            ObservationTime {
                sort_ts_us: ts,
                occurred_at_us: Some(ts),
                observed_interval: None,
                quality: TimeQuality::FirstInGroup,
            },
            1,
            ObservationPayload::ErrorGroup(Box::new(payload)),
            evidence_quality,
            QualityFlags::default(),
            None,
        )
        .expect("valid error fixture")
    }

    #[test]
    fn parsed_log_evidence_and_child_termination_are_not_floors() {
        assert_eq!(
            observation_floor(&panic(0, 1, EvidenceQuality::Parsed)),
            None
        );
        assert_eq!(observation_floor(&crash(0, 1)), None);
        assert_eq!(
            observation_floor(&error(0, 1, *b"53100", EvidenceQuality::Parsed)),
            None
        );
        assert_eq!(
            observation_floor(&error(0, 1, *b"XX001", EvidenceQuality::Parsed)),
            None
        );
    }

    #[test]
    fn structured_panic_and_integrity_evidence_can_set_floors() {
        assert_eq!(
            observation_floor(&panic(0, 1, EvidenceQuality::Structured)),
            Some(FloorClass::Availability)
        );
        assert_eq!(
            observation_floor(&error(0, 1, *b"XX001", EvidenceQuality::Structured)),
            Some(FloorClass::Integrity)
        );
        assert_eq!(
            observation_floor(&error(0, 1, *b"53100", EvidenceQuality::Structured)),
            None,
            "a SQLSTATE alone does not identify a full mount"
        );
    }

    #[test]
    fn an_empty_bucket_is_unknown_with_no_numeric_score() {
        let policy = overview_health_policy().expect("valid policy");
        let points = health_line(&[], span(0, 100), 100, &policy).expect("health line");
        assert_eq!(points.len(), 1);
        let point = &points[0];
        assert_eq!(point.overall_state(), HealthState::Unknown);
        assert_eq!(point.continuous_score(), None, "no gauge factor is a green");
        assert_eq!(point.overall_score(), None);
    }

    #[test]
    fn a_floor_bucket_is_critical_without_inventing_a_zero_score() {
        let policy = overview_health_policy().expect("valid policy");
        let points = health_line(
            &[panic(0, 5, EvidenceQuality::Structured)],
            span(0, 100),
            100,
            &policy,
        )
        .expect("health line");
        let point = &points[0];
        assert_eq!(point.overall_state(), HealthState::Critical);
        assert_eq!(
            point.overall_score(),
            None,
            "the required domain stays unknown"
        );
        assert_eq!(point.floor_evidence().len(), 1);
    }

    #[test]
    fn buckets_partition_the_range_and_own_their_floors() {
        let policy = overview_health_policy().expect("valid policy");
        let points = health_line(
            &[
                panic(0, 5, EvidenceQuality::Structured),
                panic(1, 55, EvidenceQuality::Structured),
            ],
            span(0, 100),
            50,
            &policy,
        )
        .expect("line");
        assert_eq!(points.len(), 2, "two 50us buckets partition the range");
        assert_eq!(
            points[0].floor_evidence().len(),
            1,
            "first floor in bucket 0"
        );
        assert_eq!(
            points[1].floor_evidence().len(),
            1,
            "second floor in bucket 1"
        );
    }

    #[test]
    fn bucket_work_is_linear_and_accepts_unsorted_input() {
        let policy = overview_health_policy().expect("valid policy");
        let observations = [
            panic(2, 95, EvidenceQuality::Structured),
            panic(0, 5, EvidenceQuality::Structured),
            panic(1, 55, EvidenceQuality::Structured),
        ];
        let points = health_line(&observations, span(0, 100), 10, &policy).expect("line");
        assert_eq!(points.len(), 10);
        assert_eq!(
            points
                .iter()
                .map(|point| point.floor_evidence().len())
                .sum::<usize>(),
            3
        );
    }
}
