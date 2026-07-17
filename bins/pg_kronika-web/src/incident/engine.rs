//! Bounded, deterministic evaluation: episodes become clustered incidents, each
//! run through only its candidate lenses under a shared work budget.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::cluster::cluster_episodes;
use super::dispatch::{SectionColumn, WorkBudget, candidate_lenses, section_index};
use super::evidence::Finding;
use super::lens::{ClockRelation, EvalContext, Lens};
use super::model::{EnrichedEpisode, EpisodeRefV1, IncidentKeyV1};
use super::series::SeriesSet;

/// Validated inputs for one analysis run.
pub(crate) struct IncidentConfig {
    pub node_self_id: String,
    pub epsilon_us: i64,
    pub max_cluster_span_us: i64,
    pub clock_relation: ClockRelation,
    pub work_limit: u64,
}

/// One incident: a cluster of episodes, its canonical key, and the findings of
/// the lenses that applied.
pub(crate) struct Incident {
    pub key: IncidentKeyV1,
    pub start_us: i64,
    pub end_us: i64,
    pub members: Vec<EpisodeRefV1>,
    pub findings: Vec<Finding>,
}

/// Result of one run, including the honesty accounting the caller reports.
pub(crate) struct EngineOutcome {
    pub incidents: Vec<Incident>,
    pub span_splits: u64,
    pub work_exhausted: bool,
}

/// Presentation order: strongest confidence first, then role (lead first), then
/// lens id. Stable sort keeps a lens' own deterministic order for equal keys.
fn finding_order(a: &Finding, b: &Finding) -> Ordering {
    b.confidence()
        .cmp(&a.confidence())
        .then_with(|| a.role().cmp(&b.role()))
        .then_with(|| a.lens_id().cmp(b.lens_id()))
}

/// Cluster `episodes` and evaluate each incident's candidate lenses under one
/// shared budget. Once the budget is exhausted, remaining lenses and clusters
/// simply add nothing; the exhaustion is reported, never hidden.
pub(crate) fn analyze(
    episodes: Vec<EnrichedEpisode>,
    series: &SeriesSet,
    lenses: &[&dyn Lens],
    config: &IncidentConfig,
) -> EngineOutcome {
    let references = episodes.into_iter().map(|e| e.reference).collect();
    let clustered = cluster_episodes(references, config.epsilon_us, config.max_cluster_span_us);

    let inputs: Vec<&'static [SectionColumn]> = lenses.iter().map(|lens| lens.inputs()).collect();
    let index = section_index(&inputs);

    let mut budget = WorkBudget::new(config.work_limit);
    let mut incidents = Vec::with_capacity(clustered.clusters.len());

    for cluster in clustered.clusters {
        let context = EvalContext {
            incident_start_us: cluster.start_us,
            incident_end_us: cluster.end_us,
            clock_relation: config.clock_relation,
        };
        let present: BTreeSet<&'static str> =
            cluster.members.iter().map(|m| m.logical_section).collect();

        let mut findings = Vec::new();
        for lens_index in candidate_lenses(&index, &present) {
            if !budget.charge(1) {
                break;
            }
            findings.extend(lenses[lens_index].evaluate(&cluster, series, &context, &mut budget));
        }
        findings.sort_by(finding_order);

        incidents.push(Incident {
            key: IncidentKeyV1::new(
                config.node_self_id.clone(),
                cluster.start_us,
                cluster.end_us,
                cluster.members.clone(),
            ),
            start_us: cluster.start_us,
            end_us: cluster.end_us,
            members: cluster.members,
            findings,
        });
    }

    EngineOutcome {
        incidents,
        span_splits: clustered.span_splits,
        work_exhausted: budget.is_exhausted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::cluster::Cluster;
    use crate::incident::evidence::{Confidence, Evidence, Role};
    use crate::incident::model::IdentityValue;
    use kronika_anomaly::{Direction, Episode, Evaluated};
    use std::sync::Arc;

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

    /// A lens bound to one section that always emits a single finding.
    struct FixedLens {
        id: &'static str,
        inputs: &'static [SectionColumn],
        cap: Confidence,
        role: Role,
        evidence: Evidence,
    }

    impl Lens for FixedLens {
        fn id(&self) -> &'static str {
            self.id
        }
        fn inputs(&self) -> &'static [SectionColumn] {
            self.inputs
        }
        fn confidence_cap(&self) -> Confidence {
            self.cap
        }
        fn evaluate(
            &self,
            _cluster: &Cluster,
            _series: &SeriesSet,
            _context: &EvalContext,
            _budget: &mut WorkBudget,
        ) -> Vec<Finding> {
            vec![Finding::new(
                self.id,
                self.role,
                self.cap,
                vec![self.evidence],
            )]
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
        }
    }

    fn lock_lens() -> FixedLens {
        FixedLens {
            id: "PG-LOCK-012",
            inputs: LOCKS,
            cap: Confidence::High,
            role: Role::Lead,
            evidence: Evidence::LockEdge,
        }
    }

    fn cache_lens() -> FixedLens {
        FixedLens {
            id: "PG-CACHE-010",
            inputs: CACHE,
            cap: Confidence::Medium,
            role: Role::Amplifier,
            evidence: Evidence::Ratio,
        }
    }

    #[test]
    fn no_episodes_yield_no_incidents() {
        let outcome = analyze(vec![], &SeriesSet::new(), &[], &config(100));
        assert!(outcome.incidents.is_empty());
        assert!(!outcome.work_exhausted);
    }

    #[test]
    fn a_cluster_without_a_matching_lens_has_no_findings() {
        let lens = cache_lens();
        let lenses: &[&dyn Lens] = &[&lens];
        let outcome = analyze(
            vec![episode("pg_locks", 1, 0, 10)],
            &SeriesSet::new(),
            lenses,
            &config(100),
        );
        assert_eq!(outcome.incidents.len(), 1);
        assert!(
            outcome.incidents[0].findings.is_empty(),
            "cache lens does not read pg_locks"
        );
    }

    #[test]
    fn a_matching_lens_produces_a_finding() {
        let lens = lock_lens();
        let lenses: &[&dyn Lens] = &[&lens];
        let outcome = analyze(
            vec![episode("pg_locks", 1, 0, 10)],
            &SeriesSet::new(),
            lenses,
            &config(100),
        );
        assert_eq!(outcome.incidents[0].findings.len(), 1);
        assert_eq!(
            outcome.incidents[0].findings[0].confidence(),
            Confidence::High
        );
    }

    #[test]
    fn separate_clusters_become_separate_incidents() {
        let lens = lock_lens();
        let lenses: &[&dyn Lens] = &[&lens];
        let outcome = analyze(
            vec![
                episode("pg_locks", 1, 0, 10),
                episode("pg_locks", 2, 500, 510),
            ],
            &SeriesSet::new(),
            lenses,
            &config(100),
        );
        assert_eq!(outcome.incidents.len(), 2);
    }

    #[test]
    fn findings_sort_by_confidence_then_role_then_lens() {
        let lock = lock_lens();
        let cache = cache_lens();
        let lenses: &[&dyn Lens] = &[&cache, &lock];
        let outcome = analyze(
            vec![
                episode("pg_locks", 1, 0, 10),
                episode("pg_stat_database", 1, 2, 8),
            ],
            &SeriesSet::new(),
            lenses,
            &config(100),
        );
        let ids: Vec<&str> = outcome.incidents[0]
            .findings
            .iter()
            .map(Finding::lens_id)
            .collect();
        assert_eq!(
            ids,
            vec!["PG-LOCK-012", "PG-CACHE-010"],
            "High lead before Medium amplifier"
        );
    }

    #[test]
    fn the_key_is_deterministic_across_runs() {
        let lens = lock_lens();
        let lenses: &[&dyn Lens] = &[&lens];
        let run = || {
            analyze(
                vec![episode("pg_locks", 1, 0, 10)],
                &SeriesSet::new(),
                lenses,
                &config(100),
            )
            .incidents
            .remove(0)
            .key
            .canonical_bytes()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn an_exhausted_budget_is_reported_and_stops_further_lenses() {
        let lock = lock_lens();
        let cache = cache_lens();
        let lenses: &[&dyn Lens] = &[&lock, &cache];
        // One lens evaluation fits; the second charge fails and latches.
        let outcome = analyze(
            vec![
                episode("pg_locks", 1, 0, 10),
                episode("pg_stat_database", 1, 2, 8),
            ],
            &SeriesSet::new(),
            lenses,
            &config(1),
        );
        assert!(outcome.work_exhausted);
        assert_eq!(
            outcome.incidents[0].findings.len(),
            1,
            "only the first lens ran before the budget latched"
        );
    }
}
