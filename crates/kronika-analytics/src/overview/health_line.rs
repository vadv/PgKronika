//! Event-driven health line: floor evidence over request buckets.
//!
//! This module computes a health line from retained event observations alone.
//! Continuous resource pressure needs gauge and counter factors, which the
//! current fact schema does not yet retain, so every bucket's continuous and
//! overall numeric scores are `None`: a missing required domain never becomes a
//! false green. Trusted floor observations — a crash, a `PANIC`, a disk-full or
//! an integrity error — still drive a bucket to `Critical` through the health
//! decision table.
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
    HealthLimits, HealthPoint, HealthPolicy, InvalidFactorProfile, InvalidHealthPolicy,
    RequiredFactorProfile,
};
use super::observation::{EventObservation, FactId, ObservationPayload};

const SQLSTATE_DISK_FULL: SqlState = SqlState(*b"53100");
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
/// Only evidence that stands on its own is a floor: a crash or `PANIC` is an
/// availability floor, a disk-full error a storage floor, an `XX001`/`XX002`
/// error an integrity floor. An out-of-memory error is not a floor without
/// kernel OOM-kill evidence, which events do not carry.
#[must_use]
pub fn observation_floor(observation: &EventObservation) -> Option<FloorClass> {
    match observation.payload() {
        ObservationPayload::ChildProcessCrash(_)
        | ObservationPayload::ChildSignalTermination(_) => Some(FloorClass::Availability),
        ObservationPayload::ErrorGroup(payload) => error_floor(payload.severity, payload.sqlstate),
        _ => None,
    }
}

fn error_floor(severity: Severity, sqlstate: Option<SqlState>) -> Option<FloorClass> {
    if severity == Severity::Panic {
        return Some(FloorClass::Availability);
    }
    match sqlstate {
        Some(SQLSTATE_DISK_FULL) => Some(FloorClass::DiskFull),
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
    let mut points = Vec::new();
    let mut start = range.start_us();
    while start < range.end_us() {
        let end = start.saturating_add(step).min(range.end_us());
        let Some(bucket) = CoverageSpan::new(start, end) else {
            break;
        };
        let floors = bucket_floors(observations, start, end);
        let coverage = vec![uncovered_required_coverage(bucket)];
        points.push(policy.evaluate_cell(bucket, &[], coverage, floors)?);
        start = end;
    }
    Ok(points)
}

fn bucket_floors(observations: &[EventObservation], start: i64, end: i64) -> Vec<FloorEvidence> {
    observations
        .iter()
        .filter(|observation| {
            let ts = observation.time().sort_ts_us;
            ts >= start && ts < end
        })
        .filter_map(|observation| {
            observation_floor(observation).map(|class| FloorEvidence {
                class,
                supporting_fact_id: FactId(observation.observation_id().0),
            })
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
        DroppedFieldCount, ErrorGroupPayload, EvidenceQuality, LifecyclePayload, NamingContractId,
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

    fn panic(row: u32, ts: i64) -> EventObservation {
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
            EvidenceQuality::Parsed,
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

    fn error(row: u32, ts: i64, sqlstate: [u8; 5]) -> EventObservation {
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
            EvidenceQuality::Parsed,
            QualityFlags::default(),
            None,
        )
        .expect("valid error fixture")
    }

    #[test]
    fn only_trusted_evidence_is_a_floor() {
        assert_eq!(
            observation_floor(&panic(0, 1)),
            Some(FloorClass::Availability)
        );
        assert_eq!(
            observation_floor(&crash(0, 1)),
            Some(FloorClass::Availability)
        );
        assert_eq!(
            observation_floor(&error(0, 1, *b"53100")),
            Some(FloorClass::DiskFull)
        );
        assert_eq!(
            observation_floor(&error(0, 1, *b"XX001")),
            Some(FloorClass::Integrity)
        );
        // Out-of-memory needs kernel evidence; a 53200 error is not a floor.
        assert_eq!(observation_floor(&error(0, 1, *b"53200")), None);
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
        let points = health_line(&[panic(0, 5)], span(0, 100), 100, &policy).expect("health line");
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
        let points =
            health_line(&[panic(0, 5), crash(1, 55)], span(0, 100), 50, &policy).expect("line");
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
}
