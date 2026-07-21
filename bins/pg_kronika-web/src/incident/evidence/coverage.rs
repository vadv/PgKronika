//! Observed source cadence and incident-window coverage.
//!
//! The collection period is derived from a series' own sample spacing, not from
//! runtime configuration, so a finding can state how much of its incident
//! window the source actually covered — or honestly report `unknown` when the
//! cadence is unproven.

/// Fewest usable intervals needed to trust the observed period. With three
/// durations a single gap-inflated interval cannot move the median off the true
/// cadence: the median of three is the central value, and two true-period
/// samples bracket it. Two durations would let one gap corrupt the median.
pub(crate) const MIN_INTERVALS_FOR_PERIOD: usize = 3;

/// Observed source cadence over one series' incident window, carried alongside
/// gauge evidence, which has no interval window of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourceWindow {
    window_span_us: u64,
    observed_period_us: Option<u64>,
    usable_samples: u64,
}

impl SourceWindow {
    pub(crate) fn new(
        window_span_us: u64,
        observed_period_us: Option<u64>,
        samples: usize,
    ) -> Self {
        Self {
            window_span_us,
            observed_period_us,
            usable_samples: u64::try_from(samples).unwrap_or(u64::MAX),
        }
    }

    /// Builds a window from its incident bounds, clamping a degenerate span to
    /// zero (invalid bounds are rejected upstream). A `None` period is honest:
    /// the caller had no series to derive a cadence from.
    pub(crate) fn from_bounds(
        from_us: i64,
        to_us: i64,
        observed_period_us: Option<u64>,
        samples: usize,
    ) -> Self {
        let window_span_us = to_us
            .checked_sub(from_us)
            .and_then(|delta| u64::try_from(delta).ok())
            .unwrap_or(0);
        Self::new(window_span_us, observed_period_us, samples)
    }

    pub(crate) const fn observed_period_us(&self) -> Option<u64> {
        self.observed_period_us
    }

    pub(crate) fn expected_interval_count(&self) -> Option<u64> {
        expected_interval_count(self.window_span_us, self.observed_period_us?)
    }

    pub(crate) fn source_window_completeness(&self) -> Option<f64> {
        source_window_completeness(self.usable_samples, self.expected_interval_count()?)
    }

    pub(crate) fn completeness_gap_reason(&self) -> Option<&'static str> {
        source_window_gap_reason(Some(self.window_span_us), self.observed_period_us)
    }
}

/// Median of interval durations in microseconds. Even lengths average the two
/// central samples, rounded to nearest. Sorts `durations` in place; `None` when
/// empty.
fn median_us(durations: &mut [u64]) -> Option<u64> {
    if durations.is_empty() {
        return None;
    }
    durations.sort_unstable();
    let middle = durations.len() / 2;
    if durations.len() % 2 == 1 {
        Some(durations[middle])
    } else {
        let low = u128::from(durations[middle - 1]);
        let high = u128::from(durations[middle]);
        u64::try_from((low + high).div_ceil(2)).ok()
    }
}

/// Observed period from the durations of a series' usable intervals. Needs at
/// least [`MIN_INTERVALS_FOR_PERIOD`] durations for the median to resist a
/// single gap; otherwise the cadence is unproven and the caller reports
/// `unknown`.
pub(crate) fn observed_period_from_durations(durations: &mut [u64]) -> Option<u64> {
    (durations.len() >= MIN_INTERVALS_FOR_PERIOD).then_some(())?;
    median_us(durations)
}

/// Observed period from a series' sample timestamps, ascending. The gaps
/// between adjacent samples are the candidate cadences; their median is the
/// period. Needs at least [`MIN_INTERVALS_FOR_PERIOD`] gaps (one more sample).
pub(crate) fn observed_period_from_timestamps(sorted_ts: &[i64]) -> Option<u64> {
    let mut gaps: Vec<u64> = sorted_ts
        .windows(2)
        .filter_map(|pair| u64::try_from(pair[1].checked_sub(pair[0])?).ok())
        .filter(|&gap| gap > 0)
        .collect();
    observed_period_from_durations(&mut gaps)
}

/// Intervals the source should have delivered across the window at the observed
/// cadence, `round(span / period)`. `None` on a degenerate span or period, or
/// when the window is shorter than half a period (it rounds to zero, leaving
/// nothing to divide completeness by).
pub(crate) fn expected_interval_count(window_span_us: u64, observed_period_us: u64) -> Option<u64> {
    if window_span_us == 0 || observed_period_us == 0 {
        return None;
    }
    let rounded = (window_span_us + observed_period_us / 2) / observed_period_us;
    (rounded > 0).then_some(rounded)
}

/// Usable samples over expected intervals. Unclamped by design; `None` only
/// when `expected` is zero.
#[expect(
    clippy::cast_precision_loss,
    reason = "interval counts stay far below 2^53, so the f64 division is exact"
)]
pub(crate) fn source_window_completeness(usable: u64, expected: u64) -> Option<f64> {
    (expected > 0).then(|| usable as f64 / expected as f64)
}

/// The reason completeness cannot be reported, or `None` when it can.
fn source_window_gap_reason(
    window_span_us: Option<u64>,
    observed_period_us: Option<u64>,
) -> Option<&'static str> {
    let Some(span) = window_span_us else {
        return Some("empty_incident_window");
    };
    let Some(period) = observed_period_us else {
        return Some("insufficient_intervals_for_observed_period");
    };
    match expected_interval_count(span, period) {
        Some(_) => None,
        None if span == 0 => Some("empty_incident_window"),
        None => Some("incident_window_shorter_than_observed_period"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_us_odd_even_empty() {
        assert_eq!(median_us(&mut [30, 10, 20]), Some(20));
        assert_eq!(median_us(&mut [10, 20, 30, 40]), Some(25));
        assert_eq!(median_us(&mut []), None);
    }

    #[test]
    fn observed_period_needs_three_durations_and_resists_one_gap() {
        // Two durations: unproven, even if identical.
        assert_eq!(observed_period_from_durations(&mut [1_000, 1_000]), None);
        // Three durations, one gap-inflated: the median holds the true cadence.
        assert_eq!(
            observed_period_from_durations(&mut [1_000, 1_000, 9_000]),
            Some(1_000)
        );
    }

    #[test]
    fn observed_period_from_timestamps_uses_adjacent_gaps() {
        // Four samples → three 1s gaps → 1s period.
        assert_eq!(
            observed_period_from_timestamps(&[0, 1_000_000, 2_000_000, 3_000_000]),
            Some(1_000_000)
        );
        // Too few gaps for a stable median.
        assert_eq!(observed_period_from_timestamps(&[0, 1_000_000]), None);
    }

    #[test]
    fn expected_interval_count_rounds_the_ratio() {
        assert_eq!(expected_interval_count(10_000_000, 1_000_000), Some(10));
        assert_eq!(expected_interval_count(9_400_000, 1_000_000), Some(9));
        assert_eq!(expected_interval_count(9_600_000, 1_000_000), Some(10));
    }

    #[test]
    fn expected_interval_count_guards_degenerate_inputs() {
        assert_eq!(expected_interval_count(0, 1_000_000), None);
        assert_eq!(expected_interval_count(10_000_000, 0), None);
        assert_eq!(expected_interval_count(400_000, 1_000_000), None);
    }

    #[test]
    fn completeness_is_usable_over_expected_and_unclamped() {
        assert_eq!(source_window_completeness(8, 10), Some(0.8));
        assert_eq!(source_window_completeness(10, 10), Some(1.0));
        assert_eq!(source_window_completeness(12, 10), Some(1.2));
        assert_eq!(source_window_completeness(5, 0), None);
    }

    #[test]
    fn source_window_reports_completeness_or_an_honest_reason() {
        let covered = SourceWindow::new(10_000_000, Some(1_000_000), 8);
        assert_eq!(covered.expected_interval_count(), Some(10));
        assert_eq!(covered.source_window_completeness(), Some(0.8));
        assert_eq!(covered.completeness_gap_reason(), None);

        let unproven = SourceWindow::new(10_000_000, None, 2);
        assert_eq!(unproven.expected_interval_count(), None);
        assert_eq!(unproven.source_window_completeness(), None);
        assert_eq!(
            unproven.completeness_gap_reason(),
            Some("insufficient_intervals_for_observed_period")
        );

        let narrow = SourceWindow::new(400_000, Some(1_000_000), 1);
        assert_eq!(
            narrow.completeness_gap_reason(),
            Some("incident_window_shorter_than_observed_period")
        );
    }

    #[test]
    fn source_window_reports_empty_incident_window() {
        let empty = SourceWindow::new(0, Some(1_000_000), 5);
        assert_eq!(empty.expected_interval_count(), None);
        assert_eq!(empty.source_window_completeness(), None);
        assert_eq!(
            empty.completeness_gap_reason(),
            Some("empty_incident_window")
        );
    }
}
