//! Bounded snapshot contract for raw/index semantic comparisons.
//!
//! This module does not read PGM files. [`MemoryOracle`] is a deterministic
//! fixture adapter; production reader and index adapters implement
//! [`RawOracle`] later and must return one atomic result for a pinned view.

use std::collections::BTreeMap;

use super::counts::{
    CountError, CountLimits, CountResource, EventCounts, JointErrorKey, LifecycleCounts,
};
use super::coverage::{Coverage, CoverageSpan};
use super::observation::{EventObservation, ObservationPayload, TimeQuality};

/// Output and sparse-dimension bounds for one oracle query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleLimits {
    /// Maximum returned observations.
    pub max_observations: usize,
    /// Maximum clipped coverage spans.
    pub max_coverage_spans: usize,
    /// Count-dimension bounds.
    pub count_limits: CountLimits,
}

/// Resource that exceeded its configured oracle limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleResource {
    /// Returned observations.
    Observations,
    /// Returned coverage spans.
    CoverageSpans,
}

/// Source failure class available to production adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleSourceError {
    /// Source data could not be read.
    ReadFailed,
    /// A checksum or frame validation failed.
    IntegrityFailure,
    /// The source layout is unsupported or invalid.
    InvalidLayout,
    /// A pinned atomic view could not be obtained.
    SnapshotUnavailable,
}

/// Oracle query or fixture failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleError {
    /// A query output limit was exceeded.
    LimitExceeded(OracleResource),
    /// Count aggregation failed.
    Counts(CountError),
    /// Production source adapter failed.
    Source(OracleSourceError),
    /// One identity refers to different retained records.
    ObservationIdCollision,
}

impl From<CountError> for OracleError {
    fn from(error: CountError) -> Self {
        Self::Counts(error)
    }
}

/// Atomic result from one pinned oracle view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleResult {
    observations: Vec<EventObservation>,
    counts: EventCounts,
    coverage: Coverage,
}

impl OracleResult {
    /// Canonically ordered observations.
    #[must_use]
    pub fn observations(&self) -> &[EventObservation] {
        &self.observations
    }

    /// Counts derived from exactly `observations`.
    #[must_use]
    pub const fn counts(&self) -> &EventCounts {
        &self.counts
    }

    /// Delivered coverage clipped to the query range.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage {
        &self.coverage
    }
}

/// Atomic semantic query over one pinned data view.
pub trait RawOracle {
    /// Returns observations, counts, and coverage from the same view.
    ///
    /// # Errors
    /// Returns [`OracleError`] for source failures represented by an adapter,
    /// invalid data, overflow, or configured bounds.
    fn query(&self, range: CoverageSpan, limits: OracleLimits)
    -> Result<OracleResult, OracleError>;
}

/// Whether an observation intersects a half-open range.
#[must_use]
pub fn observation_in_range(observation: &EventObservation, range: CoverageSpan) -> bool {
    let time = observation.time();
    if time.quality == TimeQuality::IntervalOnly
        && let Some(interval) = time.observed_interval
    {
        return interval.start_us() < range.end_us() && interval.end_us() > range.start_us();
    }
    range.start_us() <= time.sort_ts_us && time.sort_ts_us < range.end_us()
}

/// Folds a bounded observation set into exact joint and lifecycle counts.
///
/// # Errors
/// Returns [`CountError`] for overflow or a sparse-dimension limit.
pub fn fold_counts(
    observations: &[EventObservation],
    limits: CountLimits,
) -> Result<EventCounts, CountError> {
    let mut joint = Vec::new();
    let mut crashes = 0_u64;
    let mut shutdowns = 0_u64;
    let mut ready = 0_u64;
    let mut signals = Vec::new();

    for observation in observations {
        let count = observation.occurrence_count();
        match observation.payload() {
            ObservationPayload::ErrorGroup(group) => {
                if joint.len() == limits.max_input_entries {
                    return Err(CountError::LimitExceeded(CountResource::InputEntries));
                }
                joint.push((
                    JointErrorKey {
                        severity: group.severity,
                        category: group.category,
                        sqlstate: group.sqlstate,
                    },
                    count,
                ));
            }
            ObservationPayload::ChildSignalTermination(retained) => {
                crashes = crashes.checked_add(count).ok_or(CountError::Overflow)?;
                if let Some(signal) = retained.signal {
                    if signals.len() == limits.max_input_entries {
                        return Err(CountError::LimitExceeded(CountResource::InputEntries));
                    }
                    signals.push((signal, count));
                }
            }
            ObservationPayload::ShutdownRequested(_) => {
                shutdowns = shutdowns.checked_add(count).ok_or(CountError::Overflow)?;
            }
            ObservationPayload::ReadyObserved(_) => {
                ready = ready.checked_add(count).ok_or(CountError::Overflow)?;
            }
            _ => {}
        }
    }

    let lifecycle = LifecycleCounts::new(crashes, shutdowns, ready, signals, limits)?;
    EventCounts::from_joint(joint, lifecycle, limits)
}

/// Deterministic fixture adapter over an already decoded retained set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryOracle {
    observations: Vec<EventObservation>,
    coverage: Coverage,
}

impl MemoryOracle {
    /// Builds a canonical fixture, deduplicating identical records by ID.
    ///
    /// # Errors
    /// Returns [`OracleError::ObservationIdCollision`] when the same ID names
    /// records with different content.
    pub fn new(
        mut observations: Vec<EventObservation>,
        coverage: Coverage,
    ) -> Result<Self, OracleError> {
        observations.sort_by(EventObservation::canonical_cmp);
        let mut canonical: Vec<EventObservation> = Vec::with_capacity(observations.len());
        let mut seen: BTreeMap<_, usize> = BTreeMap::new();
        for observation in observations {
            if let Some(&position) = seen.get(&observation.observation_id()) {
                if canonical[position] == observation {
                    continue;
                }
                return Err(OracleError::ObservationIdCollision);
            }
            seen.insert(observation.observation_id(), canonical.len());
            canonical.push(observation);
        }
        Ok(Self {
            observations: canonical,
            coverage,
        })
    }
}

impl RawOracle for MemoryOracle {
    fn query(
        &self,
        range: CoverageSpan,
        limits: OracleLimits,
    ) -> Result<OracleResult, OracleError> {
        let mut observations =
            Vec::with_capacity(self.observations.len().min(limits.max_observations));
        for observation in &self.observations {
            if !observation_in_range(observation, range) {
                continue;
            }
            if observations.len() == limits.max_observations {
                return Err(OracleError::LimitExceeded(OracleResource::Observations));
            }
            observations.push(observation.clone());
        }
        let counts = fold_counts(&observations, limits.count_limits)?;
        let mut clipped =
            Vec::with_capacity(self.coverage.spans().len().min(limits.max_coverage_spans));
        for span in self.coverage.spans() {
            let Some(span) = CoverageSpan::new(
                span.start_us().max(range.start_us()),
                span.end_us().min(range.end_us()),
            ) else {
                continue;
            };
            if clipped.len() == limits.max_coverage_spans {
                return Err(OracleError::LimitExceeded(OracleResource::CoverageSpans));
            }
            clipped.push(span);
        }
        Ok(OracleResult {
            observations,
            counts,
            coverage: Coverage::from_spans(clipped),
        })
    }
}

/// One semantic difference between two query paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticDivergence {
    /// Observation counts differ.
    ObservationCount {
        /// Left path count.
        index: usize,
        /// Reference path count.
        oracle: usize,
    },
    /// First differing canonical observation position.
    ObservationAt {
        /// Zero-based position.
        position: usize,
    },
    /// Joint or lifecycle counts differ.
    Counts,
    /// Coverage differs.
    Coverage,
}

/// Compares two atomic bounded results.
///
/// # Errors
/// Returns an adapter error from either path.
pub fn semantic_divergences<A, B>(
    index: &A,
    oracle: &B,
    range: CoverageSpan,
    limits: OracleLimits,
) -> Result<Vec<SemanticDivergence>, OracleError>
where
    A: RawOracle + ?Sized,
    B: RawOracle + ?Sized,
{
    let index = index.query(range, limits)?;
    let oracle = oracle.query(range, limits)?;
    let mut divergences = Vec::new();
    if index.observations.len() != oracle.observations.len() {
        divergences.push(SemanticDivergence::ObservationCount {
            index: index.observations.len(),
            oracle: oracle.observations.len(),
        });
    }
    if let Some(position) = index
        .observations
        .iter()
        .zip(&oracle.observations)
        .position(|(left, right)| left != right)
    {
        divergences.push(SemanticDivergence::ObservationAt { position });
    }
    if index.counts != oracle.counts {
        divergences.push(SemanticDivergence::Counts);
    }
    if index.coverage != oracle.coverage {
        divergences.push(SemanticDivergence::Coverage);
    }
    Ok(divergences)
}

#[cfg(test)]
mod tests {
    use super::super::counts::{ErrorCategory, Severity, SqlState};
    use super::super::observation::{
        DictionaryContextId, DroppedFieldCount, ErrorGroupPayload, EvidenceQuality,
        LifecyclePayload, NamingContractId, ObservationProvenance, ObservationShape,
        ObservationTime, QualityFlags, SectionBodyId, SegmentIdentity, SegmentLocator,
        SourceScopeId,
    };
    use super::*;

    const LIMITS: OracleLimits = OracleLimits {
        max_observations: 32,
        max_coverage_spans: 32,
        count_limits: CountLimits {
            max_input_entries: 32,
            max_joint_keys: 32,
            max_signal_keys: 32,
        },
    };

    fn span(from_us: i64, to_us: i64) -> CoverageSpan {
        CoverageSpan::new(from_us, to_us).expect("valid fixture span")
    }

    #[allow(
        clippy::unnecessary_box_returns,
        reason = "the fixture is passed directly to boxed payload variants"
    )]
    fn lifecycle() -> Box<LifecyclePayload> {
        Box::new(LifecyclePayload {
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: None,
            query_detail: None,
            dropped_field_count: DroppedFieldCount::default(),
        })
    }

    fn error_group() -> ObservationPayload {
        ObservationPayload::ErrorGroup(Box::new(ErrorGroupPayload {
            severity: Severity::Fatal,
            category: ErrorCategory::Resource,
            sqlstate: Some(SqlState(*b"53300")),
            normalized_pattern: Some("too many connections".into()),
            sample: Some("remaining slots".into()),
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: Some("postgres".into()),
            user: None,
            dropped_field_count: DroppedFieldCount::default(),
        }))
    }

    fn observation(
        row_ordinal: u32,
        sort_ts_us: i64,
        occurrence_count: u64,
        payload: ObservationPayload,
    ) -> EventObservation {
        let locator = SegmentLocator([3; 32]);
        let lineage = SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([2; 16]),
            locator,
            7,
            b"fixture",
        );
        let shape = match payload {
            ObservationPayload::ErrorGroup(_) | ObservationPayload::SlowQueryGroup(_) => {
                ObservationShape::GroupedCount
            }
            ObservationPayload::LogGap(_) => ObservationShape::Gap,
            _ => ObservationShape::Individual,
        };
        let quality = if shape == ObservationShape::GroupedCount {
            TimeQuality::FirstInGroup
        } else {
            TimeQuality::Exact
        };
        EventObservation::new(
            lineage,
            7,
            ObservationProvenance {
                segment_locator: Some(locator),
                section_body_id: SectionBodyId([0xAA; 32]),
                catalog_entry_ordinal: 0,
                row_ordinal,
                dictionary_context_id: DictionaryContextId([0xBB; 32]),
                source_locator: None,
            },
            shape,
            ObservationTime {
                sort_ts_us,
                occurred_at_us: Some(sort_ts_us),
                observed_interval: None,
                quality,
            },
            occurrence_count,
            payload,
            EvidenceQuality::Structured,
            QualityFlags::default(),
            None,
        )
        .expect("valid fixture observation")
    }

    #[test]
    fn query_returns_one_canonical_atomic_result() {
        let oracle = MemoryOracle::new(
            vec![
                observation(2, 300, 1, ObservationPayload::ReadyObserved(lifecycle())),
                observation(1, 100, 7, error_group()),
            ],
            Coverage::from_spans(vec![span(0, 500)]),
        )
        .expect("valid fixture");
        let result = oracle.query(span(100, 300), LIMITS).expect("bounded query");
        assert_eq!(result.observations().len(), 1);
        assert_eq!(result.observations()[0].time().sort_ts_us, 100);
        assert_eq!(result.counts().total_occurrences(), Ok(7));
        assert_eq!(result.coverage().spans(), &[span(100, 300)]);
    }

    #[test]
    fn ranges_are_half_open() {
        let oracle = MemoryOracle::new(
            vec![
                observation(1, 100, 1, ObservationPayload::ReadyObserved(lifecycle())),
                observation(2, 200, 1, ObservationPayload::ReadyObserved(lifecycle())),
            ],
            Coverage::empty(),
        )
        .expect("valid fixture");
        let result = oracle.query(span(100, 200), LIMITS).expect("bounded query");
        assert_eq!(result.observations().len(), 1);
        assert_eq!(result.observations()[0].time().sort_ts_us, 100);
    }

    #[test]
    fn grouped_count_is_not_expanded() {
        let oracle = MemoryOracle::new(
            vec![observation(1, 150, 42, error_group())],
            Coverage::empty(),
        )
        .expect("valid fixture");
        let result = oracle.query(span(100, 200), LIMITS).expect("bounded query");
        assert_eq!(result.observations().len(), 1);
        assert_eq!(result.counts().total_occurrences(), Ok(42));
    }

    #[test]
    fn exact_duplicate_is_deduplicated_but_identity_collision_fails() {
        let base = observation(1, 100, 1, ObservationPayload::ReadyObserved(lifecycle()));
        let deduplicated = MemoryOracle::new(vec![base.clone(), base.clone()], Coverage::empty())
            .expect("identical duplicate");
        assert_eq!(
            deduplicated
                .query(span(0, 200), LIMITS)
                .expect("bounded query")
                .observations()
                .len(),
            1
        );
        let collision = observation(
            1,
            100,
            1,
            ObservationPayload::ShutdownRequested(lifecycle()),
        );
        assert!(matches!(
            MemoryOracle::new(vec![base, collision], Coverage::empty()),
            Err(OracleError::ObservationIdCollision)
        ));
    }

    #[test]
    fn query_enforces_observation_and_sparse_key_limits() {
        let oracle = MemoryOracle::new(
            vec![
                observation(1, 100, 1, ObservationPayload::ReadyObserved(lifecycle())),
                observation(2, 200, 1, ObservationPayload::ReadyObserved(lifecycle())),
            ],
            Coverage::empty(),
        )
        .expect("valid fixture");
        let tight = OracleLimits {
            max_observations: 1,
            max_coverage_spans: LIMITS.max_coverage_spans,
            count_limits: LIMITS.count_limits,
        };
        assert_eq!(
            oracle.query(span(0, 300), tight),
            Err(OracleError::LimitExceeded(OracleResource::Observations))
        );

        let count_oracle = MemoryOracle::new(
            vec![
                observation(1, 100, 1, error_group()),
                observation(2, 200, 1, error_group()),
            ],
            Coverage::empty(),
        )
        .expect("valid fixture");
        let count_tight = OracleLimits {
            max_observations: LIMITS.max_observations,
            max_coverage_spans: LIMITS.max_coverage_spans,
            count_limits: CountLimits {
                max_input_entries: 1,
                ..LIMITS.count_limits
            },
        };
        assert_eq!(
            count_oracle.query(span(0, 300), count_tight),
            Err(OracleError::Counts(CountError::LimitExceeded(
                CountResource::InputEntries,
            )))
        );
    }

    #[test]
    fn semantic_comparison_detects_observations_counts_and_coverage() {
        let full = vec![
            observation(1, 100, 7, error_group()),
            observation(2, 200, 1, ObservationPayload::ReadyObserved(lifecycle())),
        ];
        let index =
            MemoryOracle::new(full[..1].to_vec(), Coverage::empty()).expect("valid fixture");
        let oracle = MemoryOracle::new(full, Coverage::from_spans(vec![span(0, 500)]))
            .expect("valid fixture");
        let differences = semantic_divergences(&index, &oracle, span(0, 1_000), LIMITS)
            .expect("bounded comparison");
        assert!(differences.contains(&SemanticDivergence::ObservationCount {
            index: 1,
            oracle: 2,
        }));
        assert!(differences.contains(&SemanticDivergence::Coverage));
    }
}
