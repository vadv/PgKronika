//! Sliding-window anomaly scan over one period's folded series.
//!
//! The derivative (or raw gauge) series of the period is built once; the
//! window then slides over it by index, scoring each position against the
//! rest of the period. The design note picks this non-causal reference for
//! retrospective scans: data after the window anchors the median as well as
//! data before it.

use kronika_anomaly::{Episode, NotEvaluatedReason, ScoreParams, Scored, episodes, score_window};
use kronika_reader::{DiffPoint, SeriesDiff, SeriesValues, Value};
use kronika_registry::ColumnClass;

/// Sufficiency gates from the design note: a position is evaluated only over
/// at least 20 reference and 3 window points.
const MIN_REF: usize = 20;
const MIN_CUR: usize = 3;

/// Per-class absolute floors, pinned at compile time: a scorable class that
/// stops declaring one fails the build here, not at request time.
const EPS_ABS_CUMULATIVE: f64 = match ColumnClass::Cumulative.eps_abs() {
    Some(eps) => eps,
    None => panic!("Cumulative must declare eps_abs"),
};
const EPS_ABS_GAUGE: f64 = match ColumnClass::Gauge.eps_abs() {
    Some(eps) => eps,
    None => panic!("Gauge must declare eps_abs"),
};

/// Validated scan parameters; all times are unix microseconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanParams {
    /// Period start.
    pub from: i64,
    /// Period end.
    pub to: i64,
    /// Window length.
    pub window: i64,
    /// Distance between window positions.
    pub step: i64,
    /// Episode cutoff in robust sigmas.
    pub threshold: f64,
    /// Relative floor as a fraction of the reference median.
    pub eps_rel: f64,
}

/// Honesty counters of one section scan: every window position lands in
/// exactly one bucket, and every input point that could not feed a series is
/// in `nodata_points`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanCounts {
    /// Series seen (identity groups), whether or not any position evaluated.
    pub series_total: u64,
    /// Positions scored.
    pub evaluated: u64,
    /// Positions with fewer than 20 reference points.
    pub ref_too_small: u64,
    /// Positions with fewer than 3 window points.
    pub cur_too_small: u64,
    /// Positions whose window and reference were both empty.
    pub all_no_data: u64,
    /// Positions rejected on a NaN or infinite input value.
    pub non_finite: u64,
    /// Diff points carrying no value (reset/gap/first point) plus gauge rows
    /// whose value was NULL or non-numeric.
    pub nodata_points: u64,
}

/// One above-threshold episode of one series' column.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EpisodeHit {
    /// Identity values of the series, in identity-column order.
    pub key: Vec<Value>,
    /// Scored column name.
    pub column: String,
    /// The episode with its peak score.
    pub episode: Episode,
}

/// Window positions of the scan: `from + window`, stepping by `step`, with
/// `to` always the final position so the right edge of the period is covered.
/// Empty when the window does not fit the period.
pub(crate) fn positions(params: &ScanParams) -> Vec<i64> {
    let Some(first) = params.from.checked_add(params.window) else {
        return Vec::new();
    };
    if first > params.to {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut position = first;
    while position < params.to {
        out.push(position);
        let Some(next) = position.checked_add(params.step) else {
            break;
        };
        position = next;
    }
    out.push(params.to);
    out
}

/// Score one series' timeline at every window position.
///
/// `ts`/`values` are the parallel time-ordered point arrays; the window is
/// `[position - window, position]` by timestamp, the reference is everything
/// else. `ref_buf` is caller-provided scratch reused across series.
fn score_series(
    ts: &[i64],
    values: &[f64],
    scan_positions: &[i64],
    window: i64,
    score: &ScoreParams,
    ref_buf: &mut Vec<f64>,
) -> Vec<(i64, Scored)> {
    scan_positions
        .iter()
        .map(|&position| {
            let lo = ts.partition_point(|&t| t < position - window);
            let hi = ts.partition_point(|&t| t <= position);
            ref_buf.clear();
            ref_buf.extend_from_slice(&values[..lo]);
            ref_buf.extend_from_slice(&values[hi..]);
            (position, score_window(&values[lo..hi], ref_buf, score))
        })
        .collect()
}

/// Fold one scored profile into the counters.
fn tally(profile: &[(i64, Scored)], counts: &mut ScanCounts) {
    for &(_, scored) in profile {
        match scored {
            Scored::Evaluated(_) => counts.evaluated += 1,
            Scored::NotEvaluated(NotEvaluatedReason::RefTooSmall) => counts.ref_too_small += 1,
            Scored::NotEvaluated(NotEvaluatedReason::CurTooSmall) => counts.cur_too_small += 1,
            Scored::NotEvaluated(NotEvaluatedReason::AllNoData) => counts.all_no_data += 1,
            Scored::NotEvaluated(NotEvaluatedReason::NonFinite) => counts.non_finite += 1,
        }
    }
}

/// Scan one section's folded series: cumulative columns over their derivative
/// rates, gauge columns over raw readings.
///
/// `diffs` and `gauges` come from the same rows and identity, so they group
/// into the same series set; either may be empty when the section has no
/// columns of that class.
pub(crate) fn scan_section(
    diffs: &[SeriesDiff],
    gauges: &[SeriesValues],
    params: &ScanParams,
) -> (Vec<EpisodeHit>, ScanCounts) {
    let scan_positions = positions(params);
    let cumulative_score = ScoreParams::new(MIN_REF, MIN_CUR, EPS_ABS_CUMULATIVE, params.eps_rel);
    let gauge_score = ScoreParams::new(MIN_REF, MIN_CUR, EPS_ABS_GAUGE, params.eps_rel);

    let mut counts = ScanCounts {
        series_total: diffs.len().max(gauges.len()) as u64,
        ..ScanCounts::default()
    };
    let mut hits = Vec::new();
    let mut ts_buf = Vec::new();
    let mut value_buf = Vec::new();
    let mut ref_buf = Vec::new();

    for series in diffs {
        for column in &series.columns {
            ts_buf.clear();
            value_buf.clear();
            for at in &column.points {
                match at.point {
                    DiffPoint::Value { rate, .. } => {
                        ts_buf.push(at.ts);
                        value_buf.push(rate);
                    }
                    DiffPoint::NoData { .. } => counts.nodata_points += 1,
                }
            }
            scan_timeline(
                &ts_buf,
                &value_buf,
                &scan_positions,
                params,
                &cumulative_score,
                &series.key,
                &column.name,
                &mut ref_buf,
                &mut counts,
                &mut hits,
            );
        }
    }

    for series in gauges {
        for column in &series.columns {
            counts.nodata_points += column.skipped as u64;
            ts_buf.clear();
            value_buf.clear();
            for &(ts, value) in &column.points {
                ts_buf.push(ts);
                value_buf.push(value);
            }
            scan_timeline(
                &ts_buf,
                &value_buf,
                &scan_positions,
                params,
                &gauge_score,
                &series.key,
                &column.name,
                &mut ref_buf,
                &mut counts,
                &mut hits,
            );
        }
    }

    (hits, counts)
}

/// Score one timeline, tally the profile, and collect its episodes.
#[allow(
    clippy::too_many_arguments,
    reason = "an internal fold over shared scratch buffers and accumulators; \
              a struct would only rename the argument list"
)]
fn scan_timeline(
    ts: &[i64],
    values: &[f64],
    scan_positions: &[i64],
    params: &ScanParams,
    score: &ScoreParams,
    key: &[Value],
    column: &str,
    ref_buf: &mut Vec<f64>,
    counts: &mut ScanCounts,
    hits: &mut Vec<EpisodeHit>,
) {
    let profile = score_series(ts, values, scan_positions, params.window, score, ref_buf);
    tally(&profile, counts);
    for episode in episodes(&profile, params.threshold) {
        hits.push(EpisodeHit {
            key: key.to_vec(),
            column: column.to_owned(),
            episode,
        });
    }
}

/// Rank episodes across sections by peak `|m|`, descending, and truncate to
/// `limit`. `sort_by` is stable, so ties keep the deterministic section and
/// identity order they were collected in.
pub(crate) fn rank(hits: &mut Vec<(&'static str, EpisodeHit)>, limit: usize) {
    hits.sort_by(|a, b| {
        b.1.episode
            .peak
            .m
            .abs()
            .total_cmp(&a.1.episode.peak.m.abs())
    });
    hits.truncate(limit);
}

#[cfg(test)]
mod tests {
    use kronika_anomaly::Direction;
    use kronika_reader::{ColumnValues, SeriesValues, Value};

    use super::{EpisodeHit, ScanCounts, ScanParams, positions, rank, scan_section};

    const SEC: i64 = 1_000_000;

    fn params(from: i64, to: i64, window: i64, step: i64) -> ScanParams {
        ScanParams {
            from,
            to,
            window,
            step,
            threshold: 3.5,
            eps_rel: 0.05,
        }
    }

    /// One gauge series named `g` for identity `id`, from raw points.
    fn gauge_series(id: i64, points: Vec<(i64, f64)>, skipped: usize) -> SeriesValues {
        SeriesValues {
            key: vec![Value::I64(id)],
            columns: vec![ColumnValues {
                name: "g".to_owned(),
                points,
                skipped,
            }],
        }
    }

    /// A calm minute-grid gauge timeline with a plateau of `spike` values
    /// spanning positions `[spike_at, spike_at + spike_len)`.
    fn spiky_points(n: i64, spike_at: i64, spike_len: i64, spike: f64) -> Vec<(i64, f64)> {
        (0..n)
            .map(|i| {
                let value = if i >= spike_at && i < spike_at + spike_len {
                    spike
                } else if i % 2 == 0 {
                    10.0
                } else {
                    11.0
                };
                (i * 60 * SEC, value)
            })
            .collect()
    }

    #[test]
    fn positions_step_from_window_end_and_always_finish_at_to() {
        let p = positions(&params(0, 100 * SEC, 40 * SEC, 25 * SEC));
        assert_eq!(p, vec![40 * SEC, 65 * SEC, 90 * SEC, 100 * SEC]);
    }

    #[test]
    fn a_window_wider_than_the_period_yields_no_positions() {
        assert!(positions(&params(0, 10 * SEC, 20 * SEC, 5 * SEC)).is_empty());
        // Overflow near i64::MAX must not wrap into fake positions.
        assert!(positions(&params(i64::MAX - 5, i64::MAX, 10, 1)).is_empty());
    }

    #[test]
    fn a_spike_in_one_series_becomes_one_episode_with_up_direction() {
        // 60 minute-spaced points, values ~10, a 50.0 plateau at minutes
        // 30..35. The plateau must dominate a window's median (the core scores
        // median(cur), so a blip shorter than half the window stays quiet):
        // a 6-minute window over 5 spike points flips its median to 50.
        let points = spiky_points(60, 30, 5, 50.0);
        let to = points.last().expect("points not empty").0;
        let series = vec![gauge_series(1, points, 0)];
        let scan = params(0, to, 6 * 60 * SEC, 2 * 60 * SEC);
        let (hits, counts) = scan_section(&[], &series, &scan);

        assert_eq!(hits.len(), 1, "one contiguous spike, one episode");
        let episode = &hits[0].episode;
        assert_eq!(episode.peak.dir, Direction::Up);
        assert!(episode.peak.m > 3.5);
        // Positions are window right edges: the episode's windows must cover
        // the spike interval (minutes 30..35).
        assert!(episode.start - scan.window <= 34 * 60 * SEC);
        assert!(30 * 60 * SEC <= episode.end);
        assert!(counts.evaluated > 0);
        assert_eq!(counts.series_total, 1);
    }

    #[test]
    fn a_calm_series_yields_no_episodes() {
        let points = spiky_points(60, 0, 0, 10.0);
        let to = points.last().expect("points not empty").0;
        let series = vec![gauge_series(1, points, 0)];
        let (hits, counts) = scan_section(&[], &series, &params(0, to, 6 * 60 * SEC, 2 * 60 * SEC));
        assert!(hits.is_empty());
        assert!(counts.evaluated > 0);
    }

    #[test]
    fn skipped_gauge_rows_count_as_nodata_points() {
        let points = spiky_points(30, 0, 0, 10.0);
        let to = points.last().expect("points not empty").0;
        let series = vec![gauge_series(1, points, 4)];
        let (_, counts) = scan_section(&[], &series, &params(0, to, 10 * 60 * SEC, 2 * 60 * SEC));
        assert_eq!(counts.nodata_points, 4);
    }

    #[test]
    fn an_empty_timeline_reports_all_no_data_positions() {
        let series = vec![gauge_series(1, Vec::new(), 0)];
        let (hits, counts) = scan_section(&[], &series, &params(0, 100 * SEC, 20 * SEC, 20 * SEC));
        assert!(hits.is_empty());
        assert_eq!(counts.evaluated, 0);
        assert!(
            counts.all_no_data > 0,
            "every position is honestly unscored"
        );
    }

    #[test]
    fn sparse_series_positions_fail_the_sufficiency_gates() {
        // Two points cannot satisfy ref >= 20 at any position.
        let series = vec![gauge_series(1, vec![(0, 10.0), (60 * SEC, 11.0)], 0)];
        let (hits, counts) = scan_section(&[], &series, &params(0, 60 * SEC, 20 * SEC, 20 * SEC));
        assert!(hits.is_empty());
        assert_eq!(counts.evaluated, 0);
        assert!(counts.ref_too_small + counts.cur_too_small > 0);
    }

    #[test]
    fn ranking_puts_the_strongest_peak_first_and_truncates() {
        let strong = spiky_points(60, 30, 5, 100.0);
        let weak = spiky_points(60, 30, 5, 15.0);
        let to = strong.last().expect("points not empty").0;
        let series = vec![gauge_series(1, weak, 0), gauge_series(2, strong, 0)];
        let scan = params(0, to, 6 * 60 * SEC, 2 * 60 * SEC);
        let (hits, _) = scan_section(&[], &series, &scan);
        assert!(hits.len() >= 2, "both spikes must surface");

        let mut ranked: Vec<(&'static str, EpisodeHit)> =
            hits.into_iter().map(|hit| ("s", hit)).collect();
        rank(&mut ranked, 1);
        assert_eq!(ranked.len(), 1);
        assert_eq!(
            ranked[0].1.key,
            vec![Value::I64(2)],
            "the stronger spike ranks first"
        );
    }

    #[test]
    fn counts_default_to_zero() {
        assert_eq!(ScanCounts::default().evaluated, 0);
    }
}
