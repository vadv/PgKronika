//! Bounded, deterministic incident evaluation.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::marker::PhantomData;

use super::cluster::{ClusterError, ClusterOutcome, cluster_episodes};
use super::dispatch::{
    LimitAxis, LimitHit, SectionColumn, WorkBudget, candidate_lenses, section_index,
};
use super::evidence::Finding;
use super::evidence::sink::{FindingSink, OutputCounts, OutputLimits};
use super::lens::Lens;
use super::model::{EnrichedEpisode, EpisodeRefV1, IncidentKeyV1, KeyTooLarge};
use super::series::SeriesSet;
use super::typed::TypedInputs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockRelation {
    SameDomain,
    Unknown,
}

pub(crate) struct EvalContext {
    pub incident_start_us: i64,
    pub incident_end_us: i64,
    clock_relation: ClockRelation,
}

pub(crate) struct TemporalDirectionPermit<'a> {
    _context: PhantomData<&'a EvalContext>,
}

impl EvalContext {
    pub(crate) fn temporal_direction(&self) -> Option<TemporalDirectionPermit<'_>> {
        matches!(self.clock_relation, ClockRelation::SameDomain).then_some(
            TemporalDirectionPermit {
                _context: PhantomData,
            },
        )
    }

    #[cfg(test)]
    pub(crate) const fn for_test(clock_relation: ClockRelation) -> Self {
        Self {
            incident_start_us: 0,
            incident_end_us: 10,
            clock_relation,
        }
    }
}

pub(crate) struct IncidentConfig {
    node_self_id: String,
    epsilon_us: i64,
    max_cluster_span_us: i64,
    clock_relation: ClockRelation,
    work_limit: u64,
    max_episodes: usize,
    max_clusters: usize,
    max_key_bytes: usize,
    max_total_key_bytes: usize,
    max_lens_evaluations: u64,
    max_findings: u64,
    max_evidence_rows: u64,
    max_output_bytes: u64,
}

impl IncidentConfig {
    /// Fixed ceilings for collection sizes and work units.
    ///
    /// These values do not claim a resident-memory budget.
    pub(crate) fn production(
        node_self_id: &str,
        epsilon_us: i64,
        max_cluster_span_us: i64,
        clock_relation: ClockRelation,
    ) -> Self {
        Self {
            node_self_id: node_self_id.to_owned(),
            epsilon_us,
            max_cluster_span_us,
            clock_relation,
            work_limit: 50_000_000,
            max_episodes: 20_000,
            max_clusters: 5_000,
            max_key_bytes: 262_144,
            max_total_key_bytes: 4 << 20,
            max_lens_evaluations: 1_000_000,
            max_findings: 50_000,
            max_evidence_rows: 200_000,
            max_output_bytes: 32 << 20,
        }
    }
}

#[cfg(test)]
impl IncidentConfig {
    /// Relaxed collection and work ceilings for focused engine tests.
    pub(crate) fn for_test(
        node_self_id: &str,
        epsilon_us: i64,
        max_cluster_span_us: i64,
        clock_relation: ClockRelation,
    ) -> Self {
        Self {
            node_self_id: node_self_id.to_owned(),
            epsilon_us,
            max_cluster_span_us,
            clock_relation,
            work_limit: u64::MAX,
            max_episodes: usize::MAX,
            max_clusters: usize::MAX,
            max_key_bytes: 1 << 20,
            max_total_key_bytes: usize::MAX,
            max_lens_evaluations: u64::MAX,
            max_findings: u64::MAX,
            max_evidence_rows: u64::MAX,
            max_output_bytes: u64::MAX,
        }
    }

    pub(crate) fn for_test_with_work_limit(
        node_self_id: &str,
        epsilon_us: i64,
        max_cluster_span_us: i64,
        clock_relation: ClockRelation,
        work_limit: u64,
    ) -> Self {
        let mut config = Self::for_test(
            node_self_id,
            epsilon_us,
            max_cluster_span_us,
            clock_relation,
        );
        config.work_limit = work_limit;
        config
    }
}

pub(crate) struct Incident {
    pub key: IncidentKeyV1,
    pub start_us: i64,
    pub end_us: i64,
    pub members: Vec<EpisodeRefV1>,
    pub findings: Vec<Finding>,
    pub evaluation_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EngineSkip {
    pub lens_id: Option<&'static str>,
    pub limit: LimitHit,
}

pub(crate) struct EngineOutcome {
    pub incidents: Vec<Incident>,
    pub span_splits: u64,
    pub complete: bool,
    pub skipped: Vec<EngineSkip>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalyzeError {
    MissingNodeIdentity,
    EpisodeLimit { observed: usize, limit: usize },
    ClusterLimit { observed: usize, limit: usize },
    DuplicateLensId(&'static str),
    Key(KeyTooLarge),
    KeyBudget { observed: usize, limit: usize },
    Cluster(ClusterError),
}

fn finding_order(a: &Finding, b: &Finding) -> Ordering {
    b.confidence()
        .cmp(&a.confidence())
        .then_with(|| a.role().cmp(&b.role()))
        .then_with(|| a.lens_id().cmp(b.lens_id()))
        .then_with(|| a.scope().cmp(b.scope()))
        .then_with(|| a.evidence().cmp(b.evidence()))
}

fn prepare_clusters(
    episodes: Vec<EnrichedEpisode>,
    config: &IncidentConfig,
) -> Result<ClusterOutcome, AnalyzeError> {
    if config.node_self_id.is_empty() {
        return Err(AnalyzeError::MissingNodeIdentity);
    }
    if episodes.len() > config.max_episodes {
        return Err(AnalyzeError::EpisodeLimit {
            observed: episodes.len(),
            limit: config.max_episodes,
        });
    }

    let references = episodes
        .into_iter()
        .map(|episode| episode.reference)
        .collect();
    let clustered = cluster_episodes(references, config.epsilon_us, config.max_cluster_span_us)
        .map_err(AnalyzeError::Cluster)?;
    if clustered.clusters.len() > config.max_clusters {
        return Err(AnalyzeError::ClusterLimit {
            observed: clustered.clusters.len(),
            limit: config.max_clusters,
        });
    }
    Ok(clustered)
}

const fn admit_lens_evaluation(
    lens_id: &'static str,
    evaluations: &mut u64,
    evaluation_limit: u64,
    budget: &mut WorkBudget,
) -> Result<(), EngineSkip> {
    let observed = evaluations.saturating_add(1);
    if observed > evaluation_limit {
        return Err(EngineSkip {
            lens_id: Some(lens_id),
            limit: LimitHit {
                axis: LimitAxis::LensEvaluations,
                observed,
                limit: evaluation_limit,
            },
        });
    }
    if !budget.charge(1) {
        return Err(EngineSkip {
            lens_id: Some(lens_id),
            limit: LimitHit {
                axis: LimitAxis::Work,
                observed: budget.spent().saturating_add(1),
                limit: budget.limit(),
            },
        });
    }
    *evaluations = observed;
    Ok(())
}

fn charge_key_bytes(
    spent: usize,
    key: &IncidentKeyV1,
    limit: usize,
) -> Result<usize, AnalyzeError> {
    let observed =
        spent
            .checked_add(key.canonical_bytes().len())
            .ok_or(AnalyzeError::KeyBudget {
                observed: usize::MAX,
                limit,
            })?;
    if observed > limit {
        return Err(AnalyzeError::KeyBudget { observed, limit });
    }
    Ok(observed)
}

pub(crate) fn analyze(
    episodes: Vec<EnrichedEpisode>,
    series: &SeriesSet,
    typed: &TypedInputs,
    lenses: &[&dyn Lens],
    config: &IncidentConfig,
) -> Result<EngineOutcome, AnalyzeError> {
    let clustered = prepare_clusters(episodes, config)?;

    let mut lens_ids = BTreeSet::new();
    for lens in lenses {
        if !lens_ids.insert(lens.id()) {
            return Err(AnalyzeError::DuplicateLensId(lens.id()));
        }
    }
    let inputs: Vec<&'static [SectionColumn]> = lenses.iter().map(|lens| lens.inputs()).collect();
    let index = section_index(&inputs);

    let mut budget = WorkBudget::new(config.work_limit);
    let mut output_counts = OutputCounts::new();
    let output_limits = OutputLimits::bounded(
        config.max_findings,
        config.max_evidence_rows,
        config.max_output_bytes,
    );
    let mut lens_evaluations = 0_u64;
    let mut incidents = Vec::with_capacity(clustered.clusters.len());
    let mut skipped = Vec::new();
    let mut complete = true;
    let mut key_bytes = 0_usize;

    'clusters: for cluster in clustered.clusters {
        let context = EvalContext {
            incident_start_us: cluster.start_us,
            incident_end_us: cluster.end_us,
            clock_relation: config.clock_relation,
        };
        let present: BTreeSet<&'static str> = cluster
            .members
            .iter()
            .map(|member| member.logical_section)
            .collect();
        let mut findings = Vec::new();
        let mut incident_complete = true;

        for lens_index in candidate_lenses(&index, &present) {
            let lens = lenses[lens_index];
            if let Err(skip) = admit_lens_evaluation(
                lens.id(),
                &mut lens_evaluations,
                config.max_lens_evaluations,
                &mut budget,
            ) {
                skipped.push(skip);
                incident_complete = false;
                complete = false;
                break;
            }

            let mut sink = FindingSink::new(
                &mut findings,
                &mut budget,
                &mut output_counts,
                output_limits,
                lens.id(),
                lens.confidence_cap(),
            );
            let evaluation = lens.evaluate(&cluster, series, typed, &context, &mut sink);
            let limit = evaluation.err().or_else(|| sink.limit_hit());
            if let Some(limit) = limit {
                skipped.push(EngineSkip {
                    lens_id: Some(lens.id()),
                    limit,
                });
                incident_complete = false;
                complete = false;
                break;
            }
        }
        findings.sort_by(finding_order);

        let key = IncidentKeyV1::new(
            &config.node_self_id,
            cluster.start_us,
            cluster.end_us,
            &cluster.members,
            config.max_key_bytes,
        )
        .map_err(AnalyzeError::Key)?;
        key_bytes = charge_key_bytes(key_bytes, &key, config.max_total_key_bytes)?;
        incidents.push(Incident {
            key,
            start_us: cluster.start_us,
            end_us: cluster.end_us,
            members: cluster.members,
            findings,
            evaluation_complete: incident_complete,
        });

        if !incident_complete {
            break 'clusters;
        }
    }

    Ok(EngineOutcome {
        incidents,
        span_splits: clustered.span_splits,
        complete,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::cluster::Cluster;
    use crate::incident::evidence::{
        Confidence, ConfidenceCap, Evidence, FindingDraft, FindingScope, Role,
    };
    use crate::incident::model::IdentityValue;
    use kronika_analytics::{Direction, Episode, Evaluated};
    use std::sync::Arc;

    #[derive(Clone, Copy)]
    enum TestEvidence {
        Ratio,
        Gauge,
        Counter,
    }

    impl TestEvidence {
        const fn build(self) -> Evidence {
            match self {
                Self::Ratio => Evidence::Ratio,
                Self::Gauge => Evidence::Gauge,
                Self::Counter => Evidence::Counter,
            }
        }
    }

    struct FixedLens {
        id: &'static str,
        inputs: &'static [SectionColumn],
        cap: ConfidenceCap,
        role: Role,
        evidence: TestEvidence,
    }

    impl Lens for FixedLens {
        fn id(&self) -> &'static str {
            self.id
        }

        fn inputs(&self) -> &'static [SectionColumn] {
            self.inputs
        }

        fn confidence_cap(&self) -> ConfidenceCap {
            self.cap
        }

        fn evaluate(
            &self,
            cluster: &Cluster,
            _series: &SeriesSet,
            _typed: &TypedInputs,
            context: &EvalContext,
            sink: &mut FindingSink<'_>,
        ) -> Result<(), LimitHit> {
            let scope = FindingScope::from_episode(&cluster.members[0]);
            sink.emit(FindingDraft::new(
                self.role,
                scope,
                vec![self.evidence.build()],
                context.temporal_direction().as_ref(),
            ))
        }
    }

    const LOCKS: &[SectionColumn] = &[SectionColumn {
        section: "pg_locks",
        column: "blocked_by",
    }];
    const CACHE: &[SectionColumn] = &[SectionColumn {
        section: "pg_stat_database",
        column: "blks_read",
    }];

    fn placeholder_episode() -> Episode {
        Episode {
            start: 0,
            end: 0,
            peak_ts: 0,
            peak: Evaluated {
                m: 0.0,
                dir: Direction::Flat,
                med_cur: 0.0,
                med_ref: 0.0,
                mad_ref: 1.0,
                sigma_used: 1.4826,
                n_cur: 0,
                n_ref: 0,
            },
        }
    }

    fn episode(section: &'static str, id: i64, start: i64, end: i64) -> EnrichedEpisode {
        EnrichedEpisode {
            episode: placeholder_episode(),
            reference: EpisodeRefV1 {
                logical_section: section,
                column: "c",
                identity: Arc::from(vec![IdentityValue::I64(id)]),
                start_us: start,
                end_us: end,
            },
        }
    }

    fn config(work_limit: u64) -> IncidentConfig {
        IncidentConfig {
            node_self_id: "node".to_owned(),
            epsilon_us: 5,
            max_cluster_span_us: 1_000,
            clock_relation: ClockRelation::Unknown,
            work_limit,
            max_episodes: 100,
            max_clusters: 100,
            max_key_bytes: 4_096,
            max_total_key_bytes: 64 * 1_024,
            max_lens_evaluations: 100,
            max_findings: 100,
            max_evidence_rows: 100,
            max_output_bytes: 1 << 20,
        }
    }

    fn cache_lens(id: &'static str, evidence: TestEvidence) -> FixedLens {
        FixedLens {
            id,
            inputs: CACHE,
            cap: ConfidenceCap::Medium,
            role: Role::Amplifier,
            evidence,
        }
    }

    #[test]
    fn no_episodes_yield_no_incidents() {
        let outcome = analyze(
            vec![],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[],
            &config(100),
        )
        .expect("valid");
        assert!(outcome.incidents.is_empty());
        assert!(outcome.complete);
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn total_key_bytes_are_bounded_across_incidents() {
        let mut limits = config(100);
        limits.max_total_key_bytes = 1;
        let error = analyze(
            vec![episode("pg_stat_database", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[],
            &limits,
        )
        .err()
        .expect("the request-wide key budget is smaller than one key");
        assert!(matches!(error, AnalyzeError::KeyBudget { limit: 1, .. }));
    }

    #[test]
    fn a_cluster_without_a_matching_lens_has_no_findings() {
        let lens = cache_lens("CACHE", TestEvidence::Ratio);
        let outcome = analyze(
            vec![episode("pg_locks", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&lens],
            &config(100),
        )
        .expect("valid");
        assert!(outcome.incidents[0].findings.is_empty());
    }

    #[test]
    fn a_matching_lens_produces_a_finding() {
        let lens = cache_lens("CACHE", TestEvidence::Ratio);
        let outcome = analyze(
            vec![episode("pg_stat_database", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&lens],
            &config(100),
        )
        .expect("valid");
        assert_eq!(outcome.incidents[0].findings.len(), 1);
        assert_eq!(
            outcome.incidents[0].findings[0].confidence(),
            Confidence::MEDIUM
        );
    }

    #[test]
    fn separate_clusters_become_separate_incidents() {
        let lens = FixedLens {
            id: "LOCK",
            inputs: LOCKS,
            cap: ConfidenceCap::Low,
            role: Role::Coincident,
            evidence: TestEvidence::Counter,
        };
        let outcome = analyze(
            vec![
                episode("pg_locks", 1, 0, 10),
                episode("pg_locks", 2, 500, 510),
            ],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&lens],
            &config(100),
        )
        .expect("valid");
        assert_eq!(outcome.incidents.len(), 2);
    }

    #[test]
    fn finding_order_is_independent_of_lens_registration_order() {
        let ratio = cache_lens("SAME", TestEvidence::Ratio);
        let gauge = cache_lens("SAME", TestEvidence::Gauge);
        let run = |reverse: bool| {
            let reference = episode("pg_stat_database", 1, 0, 10).reference;
            let scope = FindingScope::from_episode(&reference);
            let mut findings = Vec::new();
            let mut budget = WorkBudget::new(10);
            let mut counts = OutputCounts::new();
            for evidence in [ratio.evidence.build(), gauge.evidence.build()] {
                FindingSink::new(
                    &mut findings,
                    &mut budget,
                    &mut counts,
                    OutputLimits::new(2, 2),
                    "SAME",
                    ConfidenceCap::Medium,
                )
                .emit(FindingDraft::new(
                    Role::Amplifier,
                    scope.clone(),
                    vec![evidence],
                    None,
                ))
                .expect("within limits");
            }
            if reverse {
                findings.reverse();
            }
            findings.sort_by(finding_order);
            findings
                .into_iter()
                .map(|finding| format!("{:?}", finding.evidence()[0]))
                .collect::<Vec<_>>()
        };

        assert_eq!(run(false), run(true));
    }

    #[test]
    fn unknown_clock_rejects_an_unproven_lead() {
        let lens = FixedLens {
            id: "TEMPORAL",
            inputs: CACHE,
            cap: ConfidenceCap::Medium,
            role: Role::Lead,
            evidence: TestEvidence::Ratio,
        };
        let outcome = analyze(
            vec![episode("pg_stat_database", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&lens],
            &config(100),
        )
        .expect("valid");
        assert_eq!(outcome.incidents[0].findings[0].role(), Role::Coincident);
    }

    #[test]
    fn key_is_independent_of_episode_order() {
        let run = |episodes| {
            analyze(
                episodes,
                &SeriesSet::for_test(0),
                &TypedInputs::new(),
                &[],
                &config(100),
            )
            .expect("valid")
            .incidents
            .remove(0)
            .key
            .canonical_bytes()
            .to_vec()
        };
        let first = episode("pg_locks", 1, 0, 10);
        let second = episode("pg_stat_database", 2, 2, 8);
        let forward = run(vec![first, second]);
        let reversed = run(vec![
            episode("pg_stat_database", 2, 2, 8),
            episode("pg_locks", 1, 0, 10),
        ]);
        assert_eq!(forward, reversed);
    }

    #[test]
    fn duplicate_lens_ids_are_rejected() {
        let one = cache_lens("DUP", TestEvidence::Ratio);
        let two = cache_lens("DUP", TestEvidence::Gauge);
        assert!(matches!(
            analyze(
                vec![episode("pg_stat_database", 1, 0, 10)],
                &SeriesSet::for_test(0),
                &TypedInputs::new(),
                &[&one, &two],
                &config(100),
            ),
            Err(AnalyzeError::DuplicateLensId("DUP"))
        ));
    }

    #[test]
    fn missing_node_and_input_limits_are_typed_errors() {
        let mut missing_node = config(100);
        missing_node.node_self_id.clear();
        assert!(matches!(
            analyze(
                vec![],
                &SeriesSet::for_test(0),
                &TypedInputs::new(),
                &[],
                &missing_node
            ),
            Err(AnalyzeError::MissingNodeIdentity)
        ));

        let mut episode_limited = config(100);
        episode_limited.max_episodes = 0;
        assert!(matches!(
            analyze(
                vec![episode("s", 1, 0, 1)],
                &SeriesSet::for_test(0),
                &TypedInputs::new(),
                &[],
                &episode_limited,
            ),
            Err(AnalyzeError::EpisodeLimit {
                observed: 1,
                limit: 0
            })
        ));

        let mut cluster_limited = config(100);
        cluster_limited.max_clusters = 0;
        assert!(matches!(
            analyze(
                vec![episode("s", 1, 0, 1)],
                &SeriesSet::for_test(0),
                &TypedInputs::new(),
                &[],
                &cluster_limited,
            ),
            Err(AnalyzeError::ClusterLimit {
                observed: 1,
                limit: 0
            })
        ));

        let mut key_limited = config(100);
        key_limited.max_key_bytes = 1;
        assert!(matches!(
            analyze(
                vec![episode("s", 1, 0, 1)],
                &SeriesSet::for_test(0),
                &TypedInputs::new(),
                &[],
                &key_limited,
            ),
            Err(AnalyzeError::Key(KeyTooLarge {
                observed: 88,
                limit: 1
            }))
        ));
    }

    #[test]
    fn exhausted_work_marks_the_partial_incident_and_response() {
        let first = cache_lens("A", TestEvidence::Ratio);
        let second = cache_lens("B", TestEvidence::Gauge);
        let outcome = analyze(
            vec![episode("pg_stat_database", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&first, &second],
            &config(2),
        )
        .expect("bounded partial result");
        assert!(!outcome.complete);
        assert!(!outcome.incidents[0].evaluation_complete);
        assert_eq!(outcome.incidents[0].findings.len(), 1);
        assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Work);
    }

    #[test]
    fn finding_limit_is_reported_without_retaining_excess_output() {
        let first = cache_lens("A", TestEvidence::Ratio);
        let second = cache_lens("B", TestEvidence::Gauge);
        let mut cfg = config(100);
        cfg.max_findings = 1;
        let outcome = analyze(
            vec![episode("pg_stat_database", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&first, &second],
            &cfg,
        )
        .expect("bounded partial result");
        assert!(!outcome.complete);
        assert_eq!(outcome.incidents[0].findings.len(), 1);
        assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Findings);
    }

    #[test]
    fn lens_evaluation_limit_is_reported_before_calling_the_excess_lens() {
        let first = cache_lens("A", TestEvidence::Ratio);
        let second = cache_lens("B", TestEvidence::Gauge);
        let mut cfg = config(100);
        cfg.max_lens_evaluations = 1;
        let outcome = analyze(
            vec![episode("pg_stat_database", 1, 0, 10)],
            &SeriesSet::for_test(0),
            &TypedInputs::new(),
            &[&first, &second],
            &cfg,
        )
        .expect("bounded partial result");
        assert!(!outcome.complete);
        assert_eq!(outcome.incidents[0].findings.len(), 1);
        assert_eq!(
            outcome.skipped[0].limit,
            LimitHit {
                axis: LimitAxis::LensEvaluations,
                observed: 2,
                limit: 1,
            }
        );
    }
}
