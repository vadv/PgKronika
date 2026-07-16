//! Robust window score: modified z-score of a window against a reference.

/// Scoring knobs. Values are clamped to safe ranges by [`ScoreParams::new`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreParams {
    /// Minimum reference points required to evaluate.
    pub min_ref: usize,
    /// Minimum window points required to evaluate.
    pub min_cur: usize,
    /// Absolute floor for the deviation scale, in value units. Must be
    /// positive: it keeps the score finite when the reference is constant.
    pub eps_abs: f64,
    /// Relative floor as a fraction of `|median(reference)|`.
    pub eps_rel: f64,
}

impl ScoreParams {
    /// Clamp count gates and scale floors to valid ranges.
    #[must_use]
    pub const fn new(min_ref: usize, min_cur: usize, eps_abs: f64, eps_rel: f64) -> Self {
        Self {
            min_ref: if min_ref == 0 { 1 } else { min_ref },
            min_cur: if min_cur == 0 { 1 } else { min_cur },
            eps_abs: eps_abs.max(f64::MIN_POSITIVE),
            eps_rel: eps_rel.max(0.0),
        }
    }
}

impl Default for ScoreParams {
    /// Defaults: at least 20 reference points, 3 current points, and a 5% relative floor.
    fn default() -> Self {
        Self::new(20, 3, 1e-9, 0.05)
    }
}

/// Which way the window deviates from the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Window median above the reference median.
    Up,
    /// Window median below the reference median.
    Down,
    /// Window median equals the reference median.
    Flat,
}

/// A computed score with every number needed to explain it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Evaluated {
    /// Modified z-score of the window against the reference.
    pub m: f64,
    /// Deviation direction (the sign of `m`).
    pub dir: Direction,
    /// Median of the window values.
    pub med_cur: f64,
    /// Median of the reference values.
    pub med_ref: f64,
    /// Median absolute deviation of the reference.
    pub mad_ref: f64,
    /// The scale actually used: `max(1.4826 * mad_ref, floor)`.
    pub sigma_used: f64,
    /// Window points used.
    pub n_cur: usize,
    /// Reference points used.
    pub n_ref: usize,
}

/// Why a window was not evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotEvaluatedReason {
    /// Fewer reference points than `min_ref`.
    RefTooSmall,
    /// Fewer window points than `min_cur`.
    CurTooSmall,
    /// Both the window and the reference are empty.
    AllNoData,
    /// A window or reference value is `NaN` or infinity. Scoring it would
    /// silently misclassify corrupt data as a verdict, so it is not scored.
    NonFinite,
}

/// Outcome of scoring one window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scored {
    /// The window was evaluated.
    Evaluated(Evaluated),
    /// The window could not be evaluated; the reason is exact.
    NotEvaluated(NotEvaluatedReason),
}

/// Consistency constant: `MAD * 1.4826` estimates the standard deviation of
/// a normal distribution (1 / Phi^-1(3/4)).
const MAD_TO_SIGMA: f64 = 1.4826;

/// Score `cur` against `ref_` with the modified z-score.
///
/// A window or reference carrying a non-finite value is reported as
/// [`NotEvaluatedReason::NonFinite`] rather than scored: a `NaN` would otherwise
/// pass through the median as a `Flat` verdict and drop out of episodes
/// silently. Deterministic: same inputs, same output.
#[must_use]
pub fn score_window(cur: &[f64], ref_: &[f64], params: &ScoreParams) -> Scored {
    if cur.is_empty() && ref_.is_empty() {
        return Scored::NotEvaluated(NotEvaluatedReason::AllNoData);
    }
    if ref_.len() < params.min_ref {
        return Scored::NotEvaluated(NotEvaluatedReason::RefTooSmall);
    }
    if cur.len() < params.min_cur {
        return Scored::NotEvaluated(NotEvaluatedReason::CurTooSmall);
    }
    if !cur.iter().all(|v| v.is_finite()) || !ref_.iter().all(|v| v.is_finite()) {
        return Scored::NotEvaluated(NotEvaluatedReason::NonFinite);
    }

    let ref_median = median(ref_);
    let ref_mad = mad(ref_, ref_median);
    let floor = params.eps_abs.max(params.eps_rel * ref_median.abs());
    let sigma_used = (MAD_TO_SIGMA * ref_mad).max(floor);

    let cur_median = median(cur);
    let m = (cur_median - ref_median) / sigma_used;
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
        med_cur: cur_median,
        med_ref: ref_median,
        mad_ref: ref_mad,
        sigma_used,
        n_cur: cur.len(),
        n_ref: ref_.len(),
    })
}

/// Median of a non-empty slice; the even case averages the two middles.
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    }
}

/// Median absolute deviation around a precomputed median.
fn mad(values: &[f64], med: f64) -> f64 {
    let deviations: Vec<f64> = values.iter().map(|v| (v - med).abs()).collect();
    median(&deviations)
}

#[cfg(test)]
mod tests {
    use super::{Direction, NotEvaluatedReason, ScoreParams, Scored, score_window};

    /// A reference of `n` points oscillating tightly around `base`.
    fn calm_ref(base: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| base + if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect()
    }

    fn evaluated(scored: Scored) -> super::Evaluated {
        match scored {
            Scored::Evaluated(e) => e,
            Scored::NotEvaluated(reason) => panic!("expected Evaluated, got {reason:?}"),
        }
    }

    #[test]
    fn spike_scores_above_threshold_and_points_up() {
        let refs = calm_ref(10.0, 40);
        let cur = vec![50.0, 52.0, 51.0, 49.0];
        let e = evaluated(score_window(&cur, &refs, &ScoreParams::default()));
        assert!(e.m > 3.5, "x5 spike clears the 3.5 threshold, got {}", e.m);
        assert_eq!(e.dir, Direction::Up);
        assert_eq!(e.n_cur, 4);
        assert_eq!(e.n_ref, 40);
    }

    #[test]
    fn calm_window_scores_below_one() {
        let refs = calm_ref(10.0, 40);
        let cur = vec![10.4, 9.6, 10.2, 9.9];
        let e = evaluated(score_window(&cur, &refs, &ScoreParams::default()));
        assert!(e.m.abs() < 1.0, "calm window stays low, got {}", e.m);
    }

    #[test]
    fn contaminated_reference_still_flags_the_spike() {
        let mut refs = calm_ref(10.0, 40);
        for v in refs.iter_mut().take(8) {
            *v = 55.0;
        }
        let cur = vec![50.0, 52.0, 51.0];
        let e = evaluated(score_window(&cur, &refs, &ScoreParams::default()));
        assert!(e.m > 3.5, "robust stats hold at 20% contamination: {}", e.m);
    }

    #[test]
    fn constant_reference_floors_the_scale_and_keeps_m_finite() {
        let refs = vec![10.0; 40];
        let cur = vec![11.0, 11.0, 11.0];
        let e = evaluated(score_window(&cur, &refs, &ScoreParams::default()));
        assert!(
            e.mad_ref.abs() < f64::EPSILON,
            "constant reference has zero MAD"
        );
        assert!(e.sigma_used > 0.0, "floor replaces the collapsed scale");
        assert!(e.m.is_finite());
        assert!(e.m > 0.0);
    }

    #[test]
    fn drop_scores_negative_and_points_down() {
        let refs = calm_ref(100.0, 40);
        let cur = vec![10.0, 12.0, 11.0];
        let e = evaluated(score_window(&cur, &refs, &ScoreParams::default()));
        assert!(e.m < -3.5, "a drop mirrors a spike, got {}", e.m);
        assert_eq!(e.dir, Direction::Down);
    }

    #[test]
    fn insufficient_data_yields_the_exact_reason() {
        let params = ScoreParams::default();
        assert_eq!(
            score_window(&[], &[], &params),
            Scored::NotEvaluated(NotEvaluatedReason::AllNoData)
        );
        let short_ref = calm_ref(10.0, 10);
        assert_eq!(
            score_window(&[10.0, 10.0, 10.0], &short_ref, &params),
            Scored::NotEvaluated(NotEvaluatedReason::RefTooSmall)
        );
        let full_ref = calm_ref(10.0, 40);
        assert_eq!(
            score_window(&[10.0, 10.0], &full_ref, &params),
            Scored::NotEvaluated(NotEvaluatedReason::CurTooSmall)
        );
    }

    #[test]
    fn floor_takes_the_larger_of_absolute_and_relative() {
        let refs = vec![100.0; 40];
        let cur = vec![110.0, 110.0, 110.0];
        let params = ScoreParams::new(20, 3, 1.0, 0.05);
        let e = evaluated(score_window(&cur, &refs, &params));
        assert!((e.sigma_used - 5.0).abs() < 1e-12, "relative floor 5.0");

        let refs = vec![1.0; 40];
        let cur = vec![2.0, 2.0, 2.0];
        let e = evaluated(score_window(&cur, &refs, &params));
        assert!((e.sigma_used - 1.0).abs() < 1e-12, "absolute floor 1.0");
    }

    #[test]
    fn non_finite_input_is_reported_not_silently_scored() {
        let refs = calm_ref(10.0, 40);
        let cur_nan = vec![50.0, f64::NAN, 51.0];
        assert_eq!(
            score_window(&cur_nan, &refs, &ScoreParams::default()),
            Scored::NotEvaluated(NotEvaluatedReason::NonFinite)
        );
        let cur_neg_nan = vec![-f64::NAN, 1.0, 2.0];
        assert_eq!(
            score_window(&cur_neg_nan, &refs, &ScoreParams::default()),
            Scored::NotEvaluated(NotEvaluatedReason::NonFinite)
        );
        let mut ref_inf = calm_ref(10.0, 40);
        ref_inf[0] = f64::INFINITY;
        assert_eq!(
            score_window(&[10.0, 10.0, 10.0], &ref_inf, &ScoreParams::default()),
            Scored::NotEvaluated(NotEvaluatedReason::NonFinite)
        );
    }

    #[test]
    fn flat_window_reports_flat_direction() {
        let refs = calm_ref(10.0, 40);
        let cur = vec![10.0, 10.0, 10.0];
        let e = evaluated(score_window(&cur, &refs, &ScoreParams::default()));
        assert!(e.m.abs() < f64::EPSILON);
        assert_eq!(e.dir, Direction::Flat);
    }

    #[test]
    fn params_new_clamps_degenerate_floors() {
        let params = ScoreParams::new(20, 3, 0.0, -1.0);
        assert!(params.eps_abs > 0.0, "zero abs floor is clamped positive");
        assert!(
            params.eps_rel.abs() < f64::EPSILON,
            "negative relative floor clamps to 0"
        );
    }

    #[test]
    fn params_new_clamps_zero_count_gates() {
        let params = ScoreParams::new(0, 0, 1e-9, 0.05);
        assert_eq!(params.min_ref, 1, "zero ref gate clamps to one");
        assert_eq!(params.min_cur, 1, "zero cur gate clamps to one");
        assert_eq!(
            score_window(&[1.0], &[], &params),
            Scored::NotEvaluated(NotEvaluatedReason::RefTooSmall)
        );
        assert_eq!(
            score_window(&[], &[1.0], &params),
            Scored::NotEvaluated(NotEvaluatedReason::CurTooSmall)
        );
    }

    #[test]
    fn median_handles_even_and_odd_lengths() {
        assert!((super::median(&[3.0, 1.0, 2.0]) - 2.0).abs() < 1e-12);
        assert!((super::median(&[4.0, 1.0, 3.0, 2.0]) - 2.5).abs() < 1e-12);
    }
}
