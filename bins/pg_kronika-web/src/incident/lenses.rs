//! Concrete diagnostic lenses over clustered anomaly episodes.

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::sink::FindingSink;
use super::evidence::{ConfidenceCap, Evidence, FindingDraft, FindingScope, Role};
use super::lens::Lens;
use super::series::SeriesSet;

/// Logical section holding the `pg_locks` wait tree. It carries rows only while
/// backends block, so its presence in a cluster is itself the contention signal.
const PG_LOCKS_SECTION: &str = "pg_locks";

/// Flags that lock contention coincided with an incident, without naming a
/// cause. Attributing a blocker would need the wait-graph edges, which this lens
/// does not read: it reports presence only, so its findings stay `Coincident`.
pub(crate) struct LockContentionLens;

impl LockContentionLens {
    const ID: &'static str = "PG-LOCK-001";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_LOCKS_SECTION,
        column: "depth",
    }];
}

impl Lens for LockContentionLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        _context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        let Some(member) = cluster
            .members
            .iter()
            .find(|member| member.logical_section == PG_LOCKS_SECTION)
        else {
            return Ok(());
        };
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_episode(member),
            vec![Evidence::Gauge],
            None,
        ))
    }
}

/// The diagnostic lenses applied to every request, in catalog order.
pub(crate) fn catalog() -> Vec<Box<dyn Lens>> {
    vec![Box::new(LockContentionLens)]
}

/// The ids of the catalog lenses, in catalog order.
pub(crate) fn catalog_ids() -> Vec<&'static str> {
    catalog().iter().map(|lens| lens.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::evidence::Confidence;
    use crate::incident::model::{EnrichedEpisode, EpisodeRefV1, IdentityValue};
    use crate::incident::{ClockRelation, EngineOutcome, IncidentConfig, analyze};
    use kronika_analytics::{Direction, Episode, Evaluated};
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

    fn run(episodes: Vec<EnrichedEpisode>) -> EngineOutcome {
        let lens = LockContentionLens;
        let lenses: [&dyn Lens; 1] = [&lens];
        let config = IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown);
        analyze(episodes, &SeriesSet::for_test(0), &lenses, &config).expect("valid analysis")
    }

    #[test]
    fn a_pg_locks_member_yields_one_coincident_finding() {
        let outcome = run(vec![episode("pg_locks", 1, 0, 10)]);
        let findings = &outcome.incidents[0].findings;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].role(), Role::Coincident);
        assert_eq!(findings[0].confidence(), Confidence::MEDIUM);
        assert_eq!(findings[0].lens_id(), "PG-LOCK-001");
    }

    #[test]
    fn a_cluster_without_pg_locks_produces_no_lock_finding() {
        let outcome = run(vec![episode("pg_stat_database", 1, 0, 10)]);
        assert!(outcome.incidents[0].findings.is_empty());
    }

    #[test]
    fn one_incident_reports_the_lock_fact_once_despite_several_lock_members() {
        let outcome = run(vec![
            episode("pg_locks", 1, 0, 10),
            episode("pg_locks", 2, 12, 20),
        ]);
        assert_eq!(outcome.incidents.len(), 1, "both lock episodes co-cluster");
        assert_eq!(
            outcome.incidents[0].findings.len(),
            1,
            "one lock fact per incident, not one per member"
        );
    }

    #[test]
    fn a_lock_member_mixed_with_others_still_reports_contention() {
        let outcome = run(vec![
            episode("pg_stat_database", 9, 0, 10),
            episode("pg_locks", 1, 2, 8),
        ]);
        let findings = &outcome.incidents[0].findings;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].lens_id(), "PG-LOCK-001");
    }

    #[test]
    fn the_lock_finding_never_claims_a_lead() {
        // Presence alone must never be promoted past coincidence: no wait-graph
        // edge is read, so there is no proof of who caused whom.
        let outcome = run(vec![episode("pg_locks", 1, 0, 10)]);
        assert_eq!(outcome.incidents[0].findings[0].role(), Role::Coincident);
    }

    #[test]
    fn the_catalog_carries_the_lock_lens() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id(), "PG-LOCK-001");
    }
}
