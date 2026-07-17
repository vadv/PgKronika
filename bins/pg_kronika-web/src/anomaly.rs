//! Sliding-window anomaly scan over one period's folded series.
//!
//! Each position is scored against the rest of the period, including later
//! samples. This is a retrospective, non-causal scan.

use kronika_anomaly::{Episode, NotEvaluatedReason, ScoreParams, Scored, episodes, score_window};
use kronika_reader::{DiffPoint, Reason, SeriesDiff, SeriesValues, Value};
use kronika_registry::ColumnClass;

/// A score requires at least 20 reference and 3 window points.
const MIN_REF: usize = 20;
const MIN_CUR: usize = 3;

/// Maximum point-position pairs scored by one HTTP request.
pub(crate) const MAX_SCORE_WORK: usize = 50_000_000;

/// Per-class absolute scale floors.
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
    /// Positions that cross an explicit timeline break.
    pub discontinuity: u64,
    /// Diff points carrying no value (reset/gap/first point) or a non-finite
    /// rate, plus gauge rows whose value was NULL, non-numeric, or non-finite.
    pub nodata_points: u64,
    /// Above-threshold episodes omitted by the caller's retained-hit ceiling.
    pub episodes_truncated: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanLimit {
    pub required: usize,
    pub available: usize,
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
    breaks: &[i64],
    scan_positions: &[i64],
    window: i64,
    score: &ScoreParams,
    ref_buf: &mut Vec<f64>,
) -> Vec<(i64, Scored)> {
    scan_positions
        .iter()
        .enumerate()
        .map(|(index, &position)| {
            let window_start = position.checked_sub(window).unwrap_or(i64::MIN);
            let break_end = breaks.partition_point(|&at| at <= position);
            let previous_position = index
                .checked_sub(1)
                .map_or(window_start, |previous| scan_positions[previous]);
            let break_start = breaks.partition_point(|&at| at <= previous_position);
            if break_start < break_end {
                return (
                    position,
                    Scored::NotEvaluated(NotEvaluatedReason::Discontinuity),
                );
            }

            let segment_lo = break_end
                .checked_sub(1)
                .map_or(0, |last| ts.partition_point(|&t| t <= breaks[last]));
            let segment_hi = breaks
                .get(break_end)
                .map_or(ts.len(), |&next| ts.partition_point(|&t| t < next));
            let lo = ts.partition_point(|&t| t < window_start).max(segment_lo);
            let hi = ts.partition_point(|&t| t <= position);
            ref_buf.clear();
            ref_buf.extend_from_slice(&values[segment_lo..lo]);
            ref_buf.extend_from_slice(&values[hi..segment_hi]);
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
            Scored::NotEvaluated(NotEvaluatedReason::Discontinuity) => {
                counts.discontinuity += 1;
            }
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
    max_work: usize,
    hit_limit: usize,
) -> Result<(Vec<EpisodeHit>, ScanCounts, usize), ScanLimit> {
    let scan_positions = positions(params);
    let work = score_work(diffs, gauges, scan_positions.len()).ok_or(ScanLimit {
        required: usize::MAX,
        available: max_work,
    })?;
    if work > max_work {
        return Err(ScanLimit {
            required: work,
            available: max_work,
        });
    }
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
    let mut breaks = Vec::new();

    for series in diffs {
        for column in &series.columns {
            ts_buf.clear();
            value_buf.clear();
            breaks.clear();
            for at in &column.points {
                match at.point {
                    DiffPoint::Value { rate, .. } if rate.is_finite() => {
                        ts_buf.push(at.ts);
                        value_buf.push(rate);
                    }
                    DiffPoint::NoData {
                        reason: Reason::FirstPoint,
                    } => counts.nodata_points += 1,
                    _ => {
                        counts.nodata_points += 1;
                        breaks.push(at.ts);
                    }
                }
            }
            scan_timeline(
                &ts_buf,
                &value_buf,
                &breaks,
                &scan_positions,
                params,
                &cumulative_score,
                &series.key,
                &column.name,
                &mut ref_buf,
                &mut counts,
                &mut hits,
                hit_limit,
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
                &column.breaks,
                &scan_positions,
                params,
                &gauge_score,
                &series.key,
                &column.name,
                &mut ref_buf,
                &mut counts,
                &mut hits,
                hit_limit,
            );
        }
    }

    counts.episodes_truncated = counts
        .episodes_truncated
        .saturating_add(rank_section(&mut hits, hit_limit) as u64);
    Ok((hits, counts, work))
}

fn score_work(diffs: &[SeriesDiff], gauges: &[SeriesValues], positions: usize) -> Option<usize> {
    let diff_points = diffs
        .iter()
        .flat_map(|series| &series.columns)
        .try_fold(0_usize, |sum, column| sum.checked_add(column.points.len()))?;
    let gauge_points = gauges
        .iter()
        .flat_map(|series| &series.columns)
        .try_fold(0_usize, |sum, column| sum.checked_add(column.points.len()))?;
    diff_points
        .checked_add(gauge_points)?
        .checked_mul(positions)
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
    breaks: &[i64],
    scan_positions: &[i64],
    params: &ScanParams,
    score: &ScoreParams,
    key: &[Value],
    column: &str,
    ref_buf: &mut Vec<f64>,
    counts: &mut ScanCounts,
    hits: &mut Vec<EpisodeHit>,
    hit_limit: usize,
) {
    let profile = score_series(
        ts,
        values,
        breaks,
        scan_positions,
        params.window,
        score,
        ref_buf,
    );
    tally(&profile, counts);
    for episode in episodes(&profile, params.threshold) {
        hits.push(EpisodeHit {
            key: key.to_vec(),
            column: column.to_owned(),
            episode,
        });
    }
    counts.episodes_truncated = counts
        .episodes_truncated
        .saturating_add(rank_section(hits, hit_limit) as u64);
}

fn rank_section(hits: &mut Vec<EpisodeHit>, limit: usize) -> usize {
    if hits.len() <= limit {
        return 0;
    }
    let removed = hits.len() - limit;
    hits.sort_by(|a, b| b.episode.peak.m.abs().total_cmp(&a.episode.peak.m.abs()));
    hits.truncate(limit);
    removed
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
    use kronika_anomaly::{Direction, NotEvaluatedReason, ScoreParams, Scored};
    use kronika_reader::{ColumnValues, SeriesValues, Value};

    use super::{EpisodeHit, ScanCounts, ScanParams, positions, rank, scan_section, score_series};

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

    fn scan_all(
        diffs: &[kronika_reader::SeriesDiff],
        gauges: &[SeriesValues],
        params: &ScanParams,
    ) -> (Vec<EpisodeHit>, ScanCounts) {
        let (hits, counts, _) =
            scan_section(diffs, gauges, params, usize::MAX, usize::MAX).expect("within budget");
        (hits, counts)
    }

    fn gauge_series(id: i64, points: Vec<(i64, f64)>, skipped: usize) -> SeriesValues {
        SeriesValues {
            key: vec![Value::I64(id)],
            columns: vec![ColumnValues {
                name: "g".to_owned(),
                points,
                breaks: Vec::new(),
                skipped,
            }],
        }
    }

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
        assert!(positions(&params(i64::MAX - 5, i64::MAX, 10, 1)).is_empty());
    }

    #[test]
    fn a_spike_in_one_series_becomes_one_episode_with_up_direction() {
        let points = spiky_points(60, 30, 5, 50.0);
        let to = points.last().expect("points not empty").0;
        let series = vec![gauge_series(1, points, 0)];
        let scan = params(0, to, 6 * 60 * SEC, 2 * 60 * SEC);
        let (hits, counts) = scan_all(&[], &series, &scan);

        assert_eq!(hits.len(), 1, "one contiguous spike, one episode");
        let episode = &hits[0].episode;
        assert_eq!(episode.peak.dir, Direction::Up);
        assert!(episode.peak.m > 3.5);
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
        let (hits, counts) = scan_all(&[], &series, &params(0, to, 6 * 60 * SEC, 2 * 60 * SEC));
        assert!(hits.is_empty());
        assert!(counts.evaluated > 0);
    }

    #[test]
    fn skipped_gauge_rows_count_as_nodata_points() {
        let points = spiky_points(30, 0, 0, 10.0);
        let to = points.last().expect("points not empty").0;
        let series = vec![gauge_series(1, points, 4)];
        let (_, counts) = scan_all(&[], &series, &params(0, to, 10 * 60 * SEC, 2 * 60 * SEC));
        assert_eq!(counts.nodata_points, 4);
    }

    #[test]
    fn an_empty_timeline_reports_all_no_data_positions() {
        let series = vec![gauge_series(1, Vec::new(), 0)];
        let (hits, counts) = scan_all(&[], &series, &params(0, 100 * SEC, 20 * SEC, 20 * SEC));
        assert!(hits.is_empty());
        assert_eq!(counts.evaluated, 0);
        assert!(
            counts.all_no_data > 0,
            "every position is honestly unscored"
        );
    }

    #[test]
    fn sparse_series_positions_fail_the_sufficiency_gates() {
        let series = vec![gauge_series(1, vec![(0, 10.0), (60 * SEC, 11.0)], 0)];
        let (hits, counts) = scan_all(&[], &series, &params(0, 60 * SEC, 20 * SEC, 20 * SEC));
        assert!(hits.is_empty());
        assert_eq!(counts.evaluated, 0);
        assert!(counts.ref_too_small + counts.cur_too_small > 0);
    }

    #[test]
    fn a_break_excludes_the_other_runs_from_the_reference() {
        let ts: Vec<i64> = (0..30).collect();
        let values: Vec<f64> = (0..30).map(f64::from).collect();
        let mut reference = Vec::new();
        let profile = score_series(
            &ts,
            &values,
            &[24],
            &[25, 28],
            3,
            &ScoreParams::new(20, 3, 0.1, 0.05),
            &mut reference,
        );

        assert_eq!(
            profile[0].1,
            Scored::NotEvaluated(NotEvaluatedReason::Discontinuity)
        );
        assert_eq!(
            profile[1].1,
            Scored::NotEvaluated(NotEvaluatedReason::RefTooSmall)
        );
    }

    #[test]
    fn ranking_puts_the_strongest_peak_first_and_truncates() {
        let strong = spiky_points(60, 30, 5, 100.0);
        let weak = spiky_points(60, 30, 5, 15.0);
        let to = strong.last().expect("points not empty").0;
        let series = vec![gauge_series(1, weak, 0), gauge_series(2, strong, 0)];
        let scan = params(0, to, 6 * 60 * SEC, 2 * 60 * SEC);
        let (hits, _) = scan_all(&[], &series, &scan);
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
    fn a_non_finite_rate_is_one_nodata_point_not_a_blinded_series() {
        use kronika_reader::{ColumnDiff, DiffAt, DiffPoint, Scalar, SeriesDiff};
        let points: Vec<DiffAt> = (0..40_i64)
            .map(|i| {
                let rate = if i == 5 { f64::NAN } else { 10.0 };
                DiffAt {
                    ts: i * 60 * SEC,
                    point: DiffPoint::Value {
                        delta: Scalar::Float(rate),
                        rate,
                        dt_micros: 60 * SEC,
                    },
                }
            })
            .collect();
        let diffs = vec![SeriesDiff {
            key: vec![Value::I64(1)],
            columns: vec![ColumnDiff {
                name: "c".to_owned(),
                points,
            }],
        }];
        let to = 39 * 60 * SEC;
        let (_, counts) = scan_all(&diffs, &[], &params(0, to, 10 * 60 * SEC, 5 * 60 * SEC));
        assert_eq!(
            counts.nodata_points, 1,
            "the non-finite rate is one nodata point"
        );
        assert!(counts.evaluated > 0, "the rest of the series still scores");
        assert_eq!(
            counts.non_finite, 0,
            "no position is blinded by the bad point"
        );
    }

    #[test]
    fn counts_default_to_zero() {
        assert_eq!(ScanCounts::default().evaluated, 0);
    }

    #[test]
    fn scan_rejects_work_above_the_request_budget() {
        let points = spiky_points(30, 0, 0, 10.0);
        let to = points.last().expect("points not empty").0;
        let series = vec![gauge_series(1, points, 0)];
        let scan = params(0, to, 10 * 60 * SEC, 2 * 60 * SEC);
        let err = scan_section(&[], &series, &scan, 1, 10).expect_err("budget must apply");
        assert!(err.required > err.available);
    }

    #[test]
    fn scan_retains_only_the_requested_number_of_episodes() {
        let to = 59 * 60 * SEC;
        let series = vec![
            gauge_series(1, spiky_points(60, 30, 5, 100.0), 0),
            gauge_series(2, spiky_points(60, 30, 5, 50.0), 0),
        ];
        let scan = params(0, to, 6 * 60 * SEC, 2 * 60 * SEC);
        let (hits, _, _) = scan_section(&[], &series, &scan, usize::MAX, 1).expect("within budget");
        assert_eq!(hits.len(), 1);
    }
}
