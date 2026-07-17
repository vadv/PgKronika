//! Deterministic sweep-line clustering of anomaly episodes into incidents.

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
}

/// Clusters plus honesty accounting: how many components were closed only
/// because a joinable episode would have pushed the span past the cap.
pub(crate) struct ClusterOutcome {
    pub clusters: Vec<Cluster>,
    pub span_splits: u64,
}

/// Group episodes whose intervals lie within `epsilon_us`, bounding each
/// cluster's span (first start to furthest end) to `max_cluster_span_us`.
///
/// Input order is irrelevant: episodes are sorted first, so the clustering is
/// deterministic. Time arithmetic saturates rather than wraps; validated
/// intervals stay far from the `i64` bounds, so saturation never alters a real
/// clustering decision.
pub(crate) fn cluster_episodes(
    mut episodes: Vec<EpisodeRefV1>,
    epsilon_us: i64,
    max_cluster_span_us: i64,
) -> ClusterOutcome {
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
        let joinable = episode.start_us <= current.end_us.saturating_add(epsilon_us);
        let merged_end = current.end_us.max(episode.end_us);
        let merged_span = merged_end.saturating_sub(current.start_us);
        if joinable && merged_span <= max_cluster_span_us {
            current.end_us = merged_end;
            current.members.push(episode);
            open = Some(current);
        } else {
            if joinable {
                span_splits += 1;
            }
            clusters.push(current);
            open = Some(Cluster::singleton(episode));
        }
    }
    clusters.extend(open);
    ClusterOutcome {
        clusters,
        span_splits,
    }
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

    fn spans(outcome: &ClusterOutcome) -> Vec<(i64, i64, usize)> {
        outcome
            .clusters
            .iter()
            .map(|c| (c.start_us, c.end_us, c.members.len()))
            .collect()
    }

    #[test]
    fn no_episodes_yield_no_clusters() {
        let outcome = cluster_episodes(vec![], 10, 1000);
        assert!(outcome.clusters.is_empty());
        assert_eq!(outcome.span_splits, 0);
    }

    #[test]
    fn a_single_episode_is_its_own_cluster() {
        let outcome = cluster_episodes(vec![ep(10, 20, 1)], 5, 1000);
        assert_eq!(spans(&outcome), vec![(10, 20, 1)]);
    }

    #[test]
    fn episodes_within_epsilon_merge() {
        let outcome = cluster_episodes(vec![ep(0, 10, 1), ep(15, 25, 2)], 5, 1000);
        assert_eq!(spans(&outcome), vec![(0, 25, 2)]);
        assert_eq!(outcome.span_splits, 0);
    }

    #[test]
    fn a_gap_wider_than_epsilon_splits_without_counting_a_span_split() {
        let outcome = cluster_episodes(vec![ep(0, 10, 1), ep(100, 110, 2)], 15, 1000);
        assert_eq!(spans(&outcome), vec![(0, 10, 1), (100, 110, 1)]);
        assert_eq!(outcome.span_splits, 0, "a natural gap is not a span split");
    }

    #[test]
    fn merge_extends_the_end_to_the_maximum() {
        let outcome = cluster_episodes(vec![ep(0, 50, 1), ep(10, 100, 2)], 20, 1000);
        assert_eq!(spans(&outcome), vec![(0, 100, 2)]);
    }

    #[test]
    fn a_nested_episode_does_not_shrink_the_cluster_end() {
        let outcome = cluster_episodes(vec![ep(0, 100, 1), ep(10, 20, 2)], 5, 1000);
        assert_eq!(
            spans(&outcome),
            vec![(0, 100, 2)],
            "end stays at the furthest, not next.end"
        );
    }

    #[test]
    fn the_epsilon_boundary_is_inclusive() {
        let outcome = cluster_episodes(vec![ep(0, 10, 1), ep(15, 25, 2)], 5, 1000);
        assert_eq!(
            spans(&outcome),
            vec![(0, 25, 2)],
            "start == end + epsilon joins"
        );
    }

    #[test]
    fn the_span_cap_splits_a_joinable_chain_and_counts_it() {
        let outcome = cluster_episodes(vec![ep(0, 10, 1), ep(20, 30, 2), ep(40, 50, 3)], 15, 25);
        assert_eq!(spans(&outcome), vec![(0, 10, 1), (20, 30, 1), (40, 50, 1)]);
        assert_eq!(
            outcome.span_splits, 2,
            "each joinable-but-too-wide break counts"
        );
    }

    #[test]
    fn a_fresh_singleton_after_a_span_split_can_grow_again() {
        let outcome = cluster_episodes(vec![ep(0, 10, 1), ep(20, 30, 2), ep(32, 40, 3)], 15, 25);
        assert_eq!(spans(&outcome), vec![(0, 10, 1), (20, 40, 2)]);
        assert_eq!(outcome.span_splits, 1);
    }

    #[test]
    fn clustering_is_independent_of_input_order() {
        let ordered = cluster_episodes(vec![ep(0, 10, 1), ep(15, 25, 2), ep(100, 110, 3)], 5, 1000);
        let shuffled =
            cluster_episodes(vec![ep(100, 110, 3), ep(15, 25, 2), ep(0, 10, 1)], 5, 1000);
        assert_eq!(spans(&ordered), spans(&shuffled));
    }

    #[test]
    fn saturating_arithmetic_does_not_panic_near_the_bounds() {
        // `end + epsilon` overflows and saturates to i64::MAX, keeping the far
        // episode joinable rather than panicking on the add.
        let outcome = cluster_episodes(
            vec![ep(0, 10, 1), ep(i64::MAX - 5, i64::MAX, 2)],
            i64::MAX,
            i64::MAX,
        );
        assert_eq!(outcome.clusters.len(), 1);
    }
}
