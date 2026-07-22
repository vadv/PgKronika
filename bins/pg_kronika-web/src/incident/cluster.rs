//! Sweep-line clustering of anomaly episodes.

use super::model::EpisodeRefV1;

/// A time-contiguous group of episodes and its covering interval.
pub(crate) struct Cluster {
    pub start_us: i64,
    pub end_us: i64,
    pub members: Vec<EpisodeRefV1>,
}

impl Cluster {
    fn singleton(episode: EpisodeRefV1) -> Self {
        Self {
            start_us: episode.start_us,
            end_us: episode.end_us,
            members: vec![episode],
        }
    }

    /// Cross-section entity join: members from a different section whose identity
    /// shares a scalar with `anchor` — the same table, database, or backend
    /// observed elsewhere in the same incident. A shared identity scalar is the
    /// join key; sections that never co-cluster yield nothing. The caller decides
    /// whether a shared scalar is semantically the same entity for its lens.
    pub(crate) fn joined_by_identity<'a>(
        &'a self,
        anchor: &'a EpisodeRefV1,
    ) -> impl Iterator<Item = &'a EpisodeRefV1> {
        self.members.iter().filter(move |member| {
            member.logical_section != anchor.logical_section
                && member
                    .identity
                    .iter()
                    .any(|scalar| anchor.identity.contains(scalar))
        })
    }
}

pub(crate) struct ClusterOutcome {
    pub clusters: Vec<Cluster>,
    pub span_splits: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClusterError {
    NegativeEpsilon,
    NegativeMaxSpan,
    EpsilonExceedsMaxSpan,
    InvalidInterval,
    EpisodeExceedsMaxSpan,
    SplitCountOverflow,
}

/// Group episodes separated by at most `epsilon_us` without exceeding the span
/// cap. Input order does not affect the result.
pub(crate) fn cluster_episodes(
    mut episodes: Vec<EpisodeRefV1>,
    epsilon_us: i64,
    max_cluster_span_us: i64,
) -> Result<ClusterOutcome, ClusterError> {
    if epsilon_us < 0 {
        return Err(ClusterError::NegativeEpsilon);
    }
    if max_cluster_span_us < 0 {
        return Err(ClusterError::NegativeMaxSpan);
    }
    if epsilon_us > max_cluster_span_us {
        return Err(ClusterError::EpsilonExceedsMaxSpan);
    }
    if episodes
        .iter()
        .any(|episode| episode.start_us > episode.end_us)
    {
        return Err(ClusterError::InvalidInterval);
    }
    if episodes.iter().any(|episode| {
        episode
            .end_us
            .checked_sub(episode.start_us)
            .is_none_or(|span| span > max_cluster_span_us)
    }) {
        return Err(ClusterError::EpisodeExceedsMaxSpan);
    }

    episodes.sort();
    let mut clusters = Vec::new();
    let mut span_splits = 0_u64;
    let mut open: Option<Cluster> = None;

    for episode in episodes {
        let previous = open.take();
        let Some(mut current) = previous else {
            open = Some(Cluster::singleton(episode));
            continue;
        };
        let joinable = episode.start_us <= current.end_us
            || episode
                .start_us
                .checked_sub(current.end_us)
                .is_some_and(|gap| gap <= epsilon_us);
        let merged_end = current.end_us.max(episode.end_us);
        let span_fits = merged_end
            .checked_sub(current.start_us)
            .is_some_and(|span| span <= max_cluster_span_us);
        if joinable && span_fits {
            current.end_us = merged_end;
            current.members.push(episode);
            open = Some(current);
        } else {
            if joinable {
                span_splits = span_splits
                    .checked_add(1)
                    .ok_or(ClusterError::SplitCountOverflow)?;
            }
            clusters.push(current);
            open = Some(Cluster::singleton(episode));
        }
    }
    clusters.extend(open);
    Ok(ClusterOutcome {
        clusters,
        span_splits,
    })
}

#[cfg(test)]
mod tests {
    use super::super::model::IdentityValue;
    use super::*;
    use std::sync::Arc;

    fn ep(start: i64, end: i64, id: i64) -> EpisodeRefV1 {
        EpisodeRefV1 {
            logical_section: "s",
            column: "c",
            identity: Arc::from(vec![IdentityValue::I64(id)]),
            start_us: start,
            end_us: end,
        }
    }

    fn ep_in(section: &'static str, id: i64) -> EpisodeRefV1 {
        EpisodeRefV1 {
            logical_section: section,
            column: "c",
            identity: Arc::from(vec![IdentityValue::I64(id)]),
            start_us: 0,
            end_us: 10,
        }
    }

    #[test]
    fn joined_by_identity_links_a_shared_scalar_across_sections() {
        let anchor = ep_in("pg_stat_user_tables", 42);
        let cluster = Cluster {
            start_us: 0,
            end_us: 10,
            members: vec![
                anchor.clone(),
                ep_in("pg_statio_user_tables", 42),
                ep_in("pg_statio_user_tables", 99),
                ep_in("pg_stat_user_tables", 42),
            ],
        };
        let joined: Vec<_> = cluster
            .joined_by_identity(&anchor)
            .map(|member| (member.logical_section, member.identity[0].clone()))
            .collect();
        // The same relid in another section joins; the same section (even with
        // the same relid) and a different relid do not.
        assert_eq!(
            joined,
            vec![("pg_statio_user_tables", IdentityValue::I64(42))]
        );
    }

    fn spans(outcome: &ClusterOutcome) -> Vec<(i64, i64, usize)> {
        outcome
            .clusters
            .iter()
            .map(|c| (c.start_us, c.end_us, c.members.len()))
            .collect()
    }

    fn cluster(
        episodes: Vec<EpisodeRefV1>,
        epsilon_us: i64,
        max_cluster_span_us: i64,
    ) -> ClusterOutcome {
        cluster_episodes(episodes, epsilon_us, max_cluster_span_us).expect("valid cluster input")
    }

    #[test]
    fn no_episodes_yield_no_clusters() {
        let outcome = cluster(vec![], 10, 1000);
        assert!(outcome.clusters.is_empty());
        assert_eq!(outcome.span_splits, 0);
    }

    #[test]
    fn a_single_episode_is_its_own_cluster() {
        let outcome = cluster(vec![ep(10, 20, 1)], 5, 1000);
        assert_eq!(spans(&outcome), vec![(10, 20, 1)]);
    }

    #[test]
    fn episodes_within_epsilon_merge() {
        let outcome = cluster(vec![ep(0, 10, 1), ep(15, 25, 2)], 5, 1000);
        assert_eq!(spans(&outcome), vec![(0, 25, 2)]);
        assert_eq!(outcome.span_splits, 0);
    }

    #[test]
    fn a_gap_wider_than_epsilon_splits_without_counting_a_span_split() {
        let outcome = cluster(vec![ep(0, 10, 1), ep(100, 110, 2)], 15, 1000);
        assert_eq!(spans(&outcome), vec![(0, 10, 1), (100, 110, 1)]);
        assert_eq!(outcome.span_splits, 0, "a natural gap is not a span split");
    }

    #[test]
    fn merge_extends_the_end_to_the_maximum() {
        let outcome = cluster(vec![ep(0, 50, 1), ep(10, 100, 2)], 20, 1000);
        assert_eq!(spans(&outcome), vec![(0, 100, 2)]);
    }

    #[test]
    fn a_nested_episode_does_not_shrink_the_cluster_end() {
        let outcome = cluster(vec![ep(0, 100, 1), ep(10, 20, 2)], 5, 1000);
        assert_eq!(
            spans(&outcome),
            vec![(0, 100, 2)],
            "end stays at the furthest, not next.end"
        );
    }

    #[test]
    fn the_epsilon_boundary_is_inclusive() {
        let outcome = cluster(vec![ep(0, 10, 1), ep(15, 25, 2)], 5, 1000);
        assert_eq!(
            spans(&outcome),
            vec![(0, 25, 2)],
            "start == end + epsilon joins"
        );
    }

    #[test]
    fn the_span_cap_splits_a_joinable_chain_and_counts_it() {
        let outcome = cluster(vec![ep(0, 10, 1), ep(20, 30, 2), ep(40, 50, 3)], 15, 25);
        assert_eq!(spans(&outcome), vec![(0, 10, 1), (20, 30, 1), (40, 50, 1)]);
        assert_eq!(
            outcome.span_splits, 2,
            "each joinable-but-too-wide break counts"
        );
    }

    #[test]
    fn a_fresh_singleton_after_a_span_split_can_grow_again() {
        let outcome = cluster(vec![ep(0, 10, 1), ep(20, 30, 2), ep(32, 40, 3)], 15, 25);
        assert_eq!(spans(&outcome), vec![(0, 10, 1), (20, 40, 2)]);
        assert_eq!(outcome.span_splits, 1);
    }

    #[test]
    fn clustering_is_independent_of_input_order() {
        let ordered = cluster(vec![ep(0, 10, 1), ep(15, 25, 2), ep(100, 110, 3)], 5, 1000);
        let shuffled = cluster(vec![ep(100, 110, 3), ep(15, 25, 2), ep(0, 10, 1)], 5, 1000);
        assert_eq!(spans(&ordered), spans(&shuffled));
    }

    #[test]
    fn an_unrepresentable_gap_does_not_become_joinable() {
        let outcome = cluster(
            vec![ep(i64::MIN, i64::MIN, 1), ep(i64::MAX, i64::MAX, 2)],
            i64::MAX,
            i64::MAX,
        );
        assert_eq!(outcome.clusters.len(), 2);
    }

    #[test]
    fn invalid_configuration_and_intervals_are_typed_errors() {
        assert!(matches!(
            cluster_episodes(vec![], -1, 1),
            Err(ClusterError::NegativeEpsilon)
        ));
        assert!(matches!(
            cluster_episodes(vec![], 2, 1),
            Err(ClusterError::EpsilonExceedsMaxSpan)
        ));
        assert!(matches!(
            cluster_episodes(vec![ep(2, 1, 1)], 0, 1),
            Err(ClusterError::InvalidInterval)
        ));
        assert!(matches!(
            cluster_episodes(vec![ep(0, 2, 1)], 0, 1),
            Err(ClusterError::EpisodeExceedsMaxSpan)
        ));
    }
}
