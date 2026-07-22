//! The raw oracle contract: a forced path the index must semantically equal.
//!
//! Every supported query has a raw counterpart that bypasses derived caches
//! and recomputes from source rows. The index path earns trust only by
//! matching it: same retained observations with the same IDs, counts, and
//! order; same exact count sets; same coverage. The only permitted
//! differences are explicitly versioned wire encodings and a documented
//! floating-point tolerance — never a missing or reinterpreted record.
//!
//! [`MemoryOracle`] is the reference implementation golden fixtures are built
//! on; [`semantic_divergences`] is the comparator both suites share.

use std::collections::BTreeMap;

use super::counts::{CountOverflow, EventCounts, JointErrorKey, LifecycleCounts};
use super::coverage::{Coverage, CoverageSpan};
use super::observation::{EventObservation, ObservationPayload, TimeQuality};

/// A forced raw query path over one retained data set.
///
/// Implementations recompute answers from source rows on every call; nothing
/// here may consult a derived cache. All ranges are half-open
/// `[from_us, to_us)`.
pub trait RawOracle {
    /// The retained observations in the range, in canonical order.
    fn observations(&self, range: CoverageSpan) -> Vec<EventObservation>;

    /// The exact count set over the retained observations in the range.
    ///
    /// # Errors
    /// Returns [`CountOverflow`] if a checked sum exceeds [`u64::MAX`].
    fn counts(&self, range: CoverageSpan) -> Result<EventCounts, CountOverflow>;

    /// The coverage actually delivered inside the range.
    fn coverage(&self, range: CoverageSpan) -> Coverage;
}

/// Whether an observation belongs to a half-open range.
///
/// A point observation belongs to exactly one bucket by its sort timestamp;
/// a grouped row belongs wholly to the bucket of its first retained
/// timestamp and is never spread. An interval-only observation crosses every
/// range its interval intersects — it is an interval fact, not a point
/// event.
#[must_use]
pub fn observation_in_range(observation: &EventObservation, range: CoverageSpan) -> bool {
    if observation.time.quality == TimeQuality::IntervalOnly
        && let Some(interval) = observation.time.observed_interval
    {
        return interval.start_us() < range.end_us() && interval.end_us() > range.start_us();
    }
    let ts = observation.time.sort_ts_us;
    range.start_us() <= ts && ts < range.end_us()
}

/// Folds retained observations into the exact count set.
///
/// Error groups accumulate into the joint dimension with their retained
/// `occurrence_count`; lifecycle observations accumulate into lifecycle
/// counts. Other retained kinds carry no counts here. This is the raw
/// reduction the index's stored counts must reproduce.
///
/// # Errors
/// Returns [`CountOverflow`] if a checked sum exceeds [`u64::MAX`].
pub fn fold_counts(observations: &[EventObservation]) -> Result<EventCounts, CountOverflow> {
    let mut joint: Vec<(JointErrorKey, u64)> = Vec::new();
    let mut lifecycle = LifecycleCounts::default();
    let mut signals: BTreeMap<i32, u64> = BTreeMap::new();

    for observation in observations {
        let count = observation.occurrence_count;
        match &observation.payload {
            ObservationPayload::ErrorGroup(group) => {
                joint.push((
                    JointErrorKey {
                        severity: group.severity,
                        category: group.category,
                        sqlstate: group.sqlstate,
                    },
                    count,
                ));
            }
            ObservationPayload::ChildSignalTermination { signal } => {
                lifecycle.crashes = lifecycle.crashes.checked_add(count).ok_or(CountOverflow)?;
                let slot = signals.entry(*signal).or_insert(0);
                *slot = slot.checked_add(count).ok_or(CountOverflow)?;
            }
            ObservationPayload::ShutdownRequested => {
                lifecycle.shutdowns = lifecycle
                    .shutdowns
                    .checked_add(count)
                    .ok_or(CountOverflow)?;
            }
            ObservationPayload::ReadyObserved => {
                lifecycle.ready = lifecycle.ready.checked_add(count).ok_or(CountOverflow)?;
            }
            _ => {}
        }
    }

    lifecycle.signals = signals.into_iter().collect();
    EventCounts::from_joint(joint, lifecycle)
}

/// The in-memory reference oracle golden fixtures run against.
///
/// Holds a canonical retained set and recomputes every answer from it on
/// each call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryOracle {
    observations: Vec<EventObservation>,
    coverage: Coverage,
}

impl MemoryOracle {
    /// Builds the oracle, normalizing the observations into canonical order.
    #[must_use]
    pub fn new(mut observations: Vec<EventObservation>, coverage: Coverage) -> Self {
        observations.sort_by(EventObservation::canonical_cmp);
        Self {
            observations,
            coverage,
        }
    }
}

impl RawOracle for MemoryOracle {
    fn observations(&self, range: CoverageSpan) -> Vec<EventObservation> {
        self.observations
            .iter()
            .filter(|observation| observation_in_range(observation, range))
            .cloned()
            .collect()
    }

    fn counts(&self, range: CoverageSpan) -> Result<EventCounts, CountOverflow> {
        fold_counts(&self.observations(range))
    }

    fn coverage(&self, range: CoverageSpan) -> Coverage {
        let clipped: Vec<CoverageSpan> = self
            .coverage
            .spans()
            .iter()
            .filter_map(|span| {
                CoverageSpan::new(
                    span.start_us().max(range.start_us()),
                    span.end_us().min(range.end_us()),
                )
            })
            .collect();
        Coverage::from_spans(clipped)
    }
}

/// One semantic difference between an index path and the raw oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticDivergence {
    /// The paths retained different numbers of observations.
    ObservationCount {
        /// How many the index path returned.
        index: usize,
        /// How many the oracle returned.
        oracle: usize,
    },
    /// The observation at this position differs in identity, fields, or
    /// order.
    ObservationAt {
        /// The first differing position in canonical order.
        position: usize,
    },
    /// The exact count sets differ, including differing overflow outcomes.
    Counts,
    /// The delivered coverage differs.
    Coverage,
}

/// Compares an index path against the raw oracle over one range.
///
/// Returns every detected divergence, empty when the paths semantically
/// agree. Exact surfaces are compared exactly; there is no tolerance here —
/// versioned encoding differences must be normalized by the caller before
/// comparing.
#[must_use]
pub fn semantic_divergences<A, B>(
    index: &A,
    oracle: &B,
    range: CoverageSpan,
) -> Vec<SemanticDivergence>
where
    A: RawOracle + ?Sized,
    B: RawOracle + ?Sized,
{
    let mut divergences = Vec::new();

    let index_observations = index.observations(range);
    let oracle_observations = oracle.observations(range);
    if index_observations.len() != oracle_observations.len() {
        divergences.push(SemanticDivergence::ObservationCount {
            index: index_observations.len(),
            oracle: oracle_observations.len(),
        });
    }
    let first_mismatch = index_observations
        .iter()
        .zip(&oracle_observations)
        .position(|(a, b)| a != b);
    if let Some(position) = first_mismatch {
        divergences.push(SemanticDivergence::ObservationAt { position });
    }

    if index.counts(range) != oracle.counts(range) {
        divergences.push(SemanticDivergence::Counts);
    }
    if index.coverage(range) != oracle.coverage(range) {
        divergences.push(SemanticDivergence::Coverage);
    }
    divergences
}

#[cfg(test)]
mod tests {
    use super::super::counts::{ErrorCategory, Severity, SqlState};
    use super::super::observation::{
        DictionaryContextId, DroppedFields, ErrorGroupPayload, EvidenceQuality, IdentityQuality,
        ObservationId, ObservationProvenance, ObservationShape, ObservationTime, QualityFlags,
        SectionBodyId, SegmentLineageId, SourceScopeId,
    };
    use super::*;

    fn observation(
        row_ordinal: u32,
        sort_ts_us: i64,
        payload: ObservationPayload,
    ) -> EventObservation {
        let provenance = ObservationProvenance {
            section_body_id: SectionBodyId([0xAA; 32]),
            section_instance_ordinal: 0,
            row_ordinal,
            dictionary_context_id: DictionaryContextId([0xBB; 32]),
            source_locator: None,
        };
        let lineage = SegmentLineageId::derive(SourceScopeId([1; 32]), 7, b"fixture");
        let shape = match payload {
            ObservationPayload::ErrorGroup(_) => ObservationShape::GroupedCount,
            _ => ObservationShape::Individual,
        };
        let occurrence_count = match &payload {
            ObservationPayload::ErrorGroup(group) => match group.severity {
                Severity::Fatal => 7,
                _ => 5,
            },
            _ => 1,
        };
        EventObservation {
            observation_id: ObservationId::derive(lineage, 7, &provenance),
            identity_quality: IdentityQuality::ContentDerived,
            source_scope_id: SourceScopeId([1; 32]),
            source_type_id: 7,
            provenance,
            shape,
            time: ObservationTime {
                sort_ts_us,
                occurred_at_us: Some(sort_ts_us),
                observed_interval: None,
                quality: TimeQuality::FirstInGroup,
            },
            occurrence_count,
            payload,
            evidence_quality: EvidenceQuality::Structured,
            quality_flags: QualityFlags::default(),
            loss: None,
        }
    }

    fn error_group(severity: Severity) -> ObservationPayload {
        ObservationPayload::ErrorGroup(Box::new(ErrorGroupPayload {
            severity,
            category: ErrorCategory::Resource,
            sqlstate: Some(SqlState(*b"53300")),
            normalized_pattern: None,
            database: None,
            user: None,
            dropped_fields: DroppedFields::default(),
        }))
    }

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid span in fixture")
    }

    #[test]
    fn observations_come_back_in_canonical_order() {
        let oracle = MemoryOracle::new(
            vec![
                observation(2, 300, ObservationPayload::ReadyObserved),
                observation(1, 100, ObservationPayload::ShutdownRequested),
                observation(3, 200, ObservationPayload::LogGap),
            ],
            Coverage::empty(),
        );
        let ts: Vec<i64> = oracle
            .observations(span(0, 1_000))
            .iter()
            .map(|o| o.time.sort_ts_us)
            .collect();
        assert_eq!(ts, vec![100, 200, 300]);
    }

    #[test]
    fn range_edges_are_half_open() {
        let oracle = MemoryOracle::new(
            vec![
                observation(1, 100, ObservationPayload::ReadyObserved),
                observation(2, 200, ObservationPayload::ReadyObserved),
            ],
            Coverage::empty(),
        );
        // The start is inclusive, the end is exclusive.
        let hits = oracle.observations(span(100, 200));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].time.sort_ts_us, 100);
    }

    #[test]
    fn every_point_observation_belongs_to_exactly_one_bucket() {
        let observations = vec![
            observation(1, 0, ObservationPayload::ReadyObserved),
            observation(2, 99, ObservationPayload::ReadyObserved),
            observation(3, 100, ObservationPayload::ReadyObserved),
            observation(4, 250, ObservationPayload::ReadyObserved),
        ];
        let oracle = MemoryOracle::new(observations.clone(), Coverage::empty());
        let buckets = [span(0, 100), span(100, 200), span(200, 300)];
        let total: usize = buckets
            .iter()
            .map(|&bucket| oracle.observations(bucket).len())
            .sum();
        assert_eq!(total, observations.len());
    }

    #[test]
    fn a_grouped_row_stays_one_item_with_its_retained_count() {
        let mut grouped = observation(1, 150, error_group(Severity::Fatal));
        grouped.occurrence_count = 42;
        let oracle = MemoryOracle::new(vec![grouped], Coverage::empty());

        // Wholly owned by the bucket of its first timestamp.
        assert_eq!(oracle.observations(span(100, 200)).len(), 1);
        assert_eq!(oracle.observations(span(200, 300)).len(), 0);
        let counts = oracle.counts(span(100, 200)).expect("no overflow");
        assert_eq!(counts.total_occurrences(), Ok(42));
    }

    #[test]
    fn an_interval_only_observation_crosses_intersecting_ranges() {
        let mut interval_fact = observation(1, 10, ObservationPayload::LogGap);
        interval_fact.time.quality = TimeQuality::IntervalOnly;
        interval_fact.time.observed_interval = Some(span(10, 30));
        interval_fact.shape = ObservationShape::Gap;
        let oracle = MemoryOracle::new(vec![interval_fact], Coverage::empty());

        assert_eq!(oracle.observations(span(0, 15)).len(), 1);
        assert_eq!(oracle.observations(span(15, 40)).len(), 1);
        assert_eq!(oracle.observations(span(40, 50)).len(), 0);
    }

    #[test]
    fn byte_identical_rows_with_distinct_provenance_are_both_retained() {
        // Same timestamp and payload; only the row ordinal differs.
        let a = observation(1, 100, ObservationPayload::ReadyObserved);
        let b = observation(2, 100, ObservationPayload::ReadyObserved);
        assert_ne!(a.observation_id, b.observation_id);
        let oracle = MemoryOracle::new(vec![a, b], Coverage::empty());
        assert_eq!(oracle.observations(span(0, 1_000)).len(), 2);
    }

    #[test]
    fn fold_counts_accumulates_joint_and_lifecycle_dimensions() {
        let observations = vec![
            observation(1, 10, error_group(Severity::Fatal)),
            observation(2, 20, error_group(Severity::Fatal)),
            observation(3, 30, error_group(Severity::Error)),
            observation(
                4,
                40,
                ObservationPayload::ChildSignalTermination { signal: 9 },
            ),
            observation(
                5,
                50,
                ObservationPayload::ChildSignalTermination { signal: 6 },
            ),
            observation(6, 60, ObservationPayload::ShutdownRequested),
            observation(7, 70, ObservationPayload::ReadyObserved),
            // Kinds that carry no counts must fold to nothing.
            observation(8, 80, ObservationPayload::CheckpointCompleted),
        ];
        let counts = fold_counts(&observations).expect("no overflow");

        // Two Fatal groups of 7 and one Error group of 5.
        assert_eq!(counts.total_occurrences(), Ok(19));
        assert_eq!(counts.lifecycle.crashes, 2);
        assert_eq!(counts.lifecycle.signals, vec![(6, 1), (9, 1)]);
        assert_eq!(counts.lifecycle.shutdowns, 1);
        assert_eq!(counts.lifecycle.ready, 1);
    }

    #[test]
    fn fold_counts_reports_overflow_instead_of_saturating() {
        let mut a = observation(
            1,
            10,
            ObservationPayload::ChildSignalTermination { signal: 9 },
        );
        a.occurrence_count = u64::MAX;
        let b = observation(
            2,
            20,
            ObservationPayload::ChildSignalTermination { signal: 9 },
        );
        assert_eq!(fold_counts(&[a, b]), Err(CountOverflow));
    }

    #[test]
    fn identical_paths_have_no_divergence() {
        let observations = vec![
            observation(1, 100, error_group(Severity::Fatal)),
            observation(2, 200, ObservationPayload::ReadyObserved),
        ];
        let coverage = Coverage::from_spans(vec![span(0, 500)]);
        let index = MemoryOracle::new(observations.clone(), coverage.clone());
        let oracle = MemoryOracle::new(observations, coverage);
        assert_eq!(
            semantic_divergences(&index, &oracle, span(0, 1_000)),
            vec![]
        );
    }

    #[test]
    fn a_dropped_observation_is_detected() {
        let full = vec![
            observation(1, 100, error_group(Severity::Fatal)),
            observation(2, 200, ObservationPayload::ReadyObserved),
        ];
        let index = MemoryOracle::new(full[..1].to_vec(), Coverage::empty());
        let oracle = MemoryOracle::new(full, Coverage::empty());
        let divergences = semantic_divergences(&index, &oracle, span(0, 1_000));
        assert!(divergences.contains(&SemanticDivergence::ObservationCount {
            index: 1,
            oracle: 2
        }));
        // Counts differ too: the dropped observation carried lifecycle
        // counts.
        assert!(divergences.contains(&SemanticDivergence::Counts));
    }

    #[test]
    fn a_mutated_occurrence_count_is_detected() {
        let base = observation(1, 100, error_group(Severity::Fatal));
        let mut mutated = base.clone();
        mutated.occurrence_count += 1;
        let index = MemoryOracle::new(vec![mutated], Coverage::empty());
        let oracle = MemoryOracle::new(vec![base], Coverage::empty());
        let divergences = semantic_divergences(&index, &oracle, span(0, 1_000));
        assert!(divergences.contains(&SemanticDivergence::ObservationAt { position: 0 }));
        assert!(divergences.contains(&SemanticDivergence::Counts));
    }

    #[test]
    fn a_coverage_mismatch_is_detected() {
        let index = MemoryOracle::new(Vec::new(), Coverage::from_spans(vec![span(0, 100)]));
        let oracle = MemoryOracle::new(Vec::new(), Coverage::from_spans(vec![span(0, 50)]));
        assert_eq!(
            semantic_divergences(&index, &oracle, span(0, 1_000)),
            vec![SemanticDivergence::Coverage]
        );
    }

    #[test]
    fn oracle_coverage_is_clipped_to_the_range() {
        let oracle = MemoryOracle::new(Vec::new(), Coverage::from_spans(vec![span(-100, 100)]));
        assert_eq!(oracle.coverage(span(0, 50)).spans(), &[span(0, 50)]);
        assert!(oracle.coverage(span(200, 300)).is_empty());
    }
}
