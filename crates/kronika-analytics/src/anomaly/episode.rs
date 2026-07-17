//! Fold a timeline of window scores into contiguous anomaly episodes.

use super::score::{Evaluated, Scored};

/// A contiguous run of above-threshold positions with its peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Episode {
    /// Timestamp of the first above-threshold position, unix microseconds.
    pub start: i64,
    /// Timestamp of the last above-threshold position, unix microseconds.
    pub end: i64,
    /// Timestamp of the peak position.
    pub peak_ts: i64,
    /// The score at the peak — the largest `|m|` inside the episode.
    pub peak: Evaluated,
}

/// Group consecutive above-threshold positions of `profile` into episodes.
///
/// `profile` must be in ascending `ts` order. A position belongs to an episode
/// when it is `Evaluated` with `|m| > threshold`; a below-threshold or
/// not-evaluated position ends the current episode. The peak is the position
/// with the largest `|m|`, ties keeping the earlier position. A run that flips
/// sign while staying above the threshold (a spike then a plunge with no return
/// between) stays one episode whose direction is the peak's — the opposite
/// excursion shows in the timeline, not the summary.
///
/// A non-finite or negative `threshold` is meaningless as a robust-sigma cutoff
/// (the endpoint validates the query parameter); it yields no episodes rather
/// than flooding the output or silently disabling the detector on `NaN`.
#[must_use]
pub fn episodes(profile: &[(i64, Scored)], threshold: f64) -> Vec<Episode> {
    let threshold = if threshold.is_finite() && threshold >= 0.0 {
        threshold
    } else {
        f64::INFINITY
    };
    debug_assert!(
        profile.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "episodes expects positions in ascending ts order"
    );

    let mut out = Vec::new();
    let mut open: Option<Episode> = None;

    for &(ts, scored) in profile {
        let hit = match scored {
            Scored::Evaluated(e) if e.m.abs() > threshold => Some(e),
            Scored::Evaluated(_) | Scored::NotEvaluated(_) => None,
        };
        match (hit, open.as_mut()) {
            (Some(e), Some(episode)) => {
                episode.end = ts;
                if e.m.abs() > episode.peak.m.abs() {
                    episode.peak = e;
                    episode.peak_ts = ts;
                }
            }
            (Some(e), None) => {
                open = Some(Episode {
                    start: ts,
                    end: ts,
                    peak_ts: ts,
                    peak: e,
                });
            }
            (None, Some(_)) => {
                out.extend(open.take());
            }
            (None, None) => {}
        }
    }
    out.extend(open);
    out
}

#[cfg(test)]
mod tests {
    use super::super::score::{Direction, Evaluated, NotEvaluatedReason, Scored};
    use super::episodes;

    /// An `Evaluated` carrying score `m`; the explanation fields are filled
    /// with placeholder numbers consistent with the score's sign.
    fn eval(m: f64) -> Scored {
        let dir = if m > 0.0 {
            Direction::Up
        } else if m < 0.0 {
            Direction::Down
        } else {
            Direction::Flat
        };
        Scored::Evaluated(Evaluated {
            m,
            dir,
            med_cur: 0.0,
            med_ref: 0.0,
            mad_ref: 1.0,
            sigma_used: 1.4826,
            n_cur: 4,
            n_ref: 40,
        })
    }

    #[test]
    fn single_spike_yields_one_episode_with_exact_bounds() {
        let profile = vec![
            (0, eval(1.0)),
            (10, eval(4.0)),
            (20, eval(6.0)),
            (30, eval(4.2)),
            (40, eval(1.0)),
        ];
        let found = episodes(&profile, 3.5);
        assert_eq!(found.len(), 1);
        let e = &found[0];
        assert_eq!((e.start, e.end), (10, 30));
        assert_eq!(e.peak_ts, 20);
        assert!((e.peak.m - 6.0).abs() < 1e-12);
    }

    #[test]
    fn two_separate_spikes_yield_two_episodes() {
        let profile = vec![
            (0, eval(5.0)),
            (10, eval(1.0)),
            (20, eval(-5.0)),
            (30, eval(-6.0)),
        ];
        let found = episodes(&profile, 3.5);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].start, found[0].end), (0, 0));
        assert_eq!((found[1].start, found[1].end), (20, 30));
        assert!(found[1].peak.m < 0.0, "peak keeps the drop's sign");
        assert_eq!(found[1].peak_ts, 30);
    }

    #[test]
    fn touching_the_threshold_is_not_an_episode() {
        let profile = vec![(0, eval(3.5)), (10, eval(-3.5))];
        assert!(episodes(&profile, 3.5).is_empty());
    }

    #[test]
    fn not_evaluated_position_splits_an_episode() {
        let profile = vec![
            (0, eval(4.0)),
            (10, Scored::NotEvaluated(NotEvaluatedReason::CurTooSmall)),
            (20, eval(4.0)),
        ];
        let found = episodes(&profile, 3.5);
        assert_eq!(found.len(), 2, "a gap in evaluation ends the run");
    }

    #[test]
    fn episode_open_at_profile_end_is_emitted() {
        let profile = vec![(0, eval(1.0)), (10, eval(7.0))];
        let found = episodes(&profile, 3.5);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start, found[0].end), (10, 10));
    }

    #[test]
    fn a_non_finite_or_negative_threshold_yields_no_episodes() {
        let profile = vec![(0, eval(5.0)), (10, eval(6.0))];
        assert!(
            episodes(&profile, f64::NAN).is_empty(),
            "NaN threshold must not silently disable, it yields nothing"
        );
        assert!(
            episodes(&profile, -1.0).is_empty(),
            "a negative threshold must not flood every position into episodes"
        );
    }

    #[test]
    fn a_sign_flip_within_a_run_stays_one_episode_with_the_peak_direction() {
        let profile = vec![(0, eval(4.0)), (10, eval(-6.0)), (20, eval(-5.0))];
        let found = episodes(&profile, 3.5);
        assert_eq!(
            found.len(),
            1,
            "a contiguous above-threshold run is one episode"
        );
        assert_eq!((found[0].start, found[0].end), (0, 20));
        assert!(
            found[0].peak.m < 0.0,
            "the peak keeps the larger excursion's sign"
        );
        assert_eq!(found[0].peak_ts, 10);
    }

    #[test]
    fn a_tie_in_absolute_score_keeps_the_earlier_peak() {
        let profile = vec![(0, eval(6.0)), (10, eval(-6.0))];
        let found = episodes(&profile, 3.5);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].peak_ts, 0, "a tie keeps the earlier position");
        assert!(found[0].peak.m > 0.0, "the earlier peak's sign is kept");
    }

    #[test]
    fn empty_profile_yields_no_episodes() {
        assert!(episodes(&[], 3.5).is_empty());
    }
}
