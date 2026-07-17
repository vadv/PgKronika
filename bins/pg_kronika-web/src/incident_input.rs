//! Adapter from the store reader to the incident engine's owned input.
//!
//! It reads each requested logical section the way `/v1/anomalies` does
//! (`section` -> `diff_section`/`gauge_section` -> collection gating -> numeric
//! scan), then converts scan episodes into engine input: `EnrichedEpisode`s
//! keyed by canonical identity and a `SeriesSet` of the scanned timelines. The
//! engine sees only owned buffers; no snapshot handle escapes.

use std::collections::{BTreeMap, BTreeSet};

use kronika_reader::{
    ColumnValues, DiffPoint, Gap, LocalDirSnapshot, LogicalSection, QueryError, SeriesDiff,
    SeriesValues, Value, diff_section, gauge_section, logical_section, section as query_section,
};
use kronika_registry::ColumnClass;

use crate::anomaly::{EpisodeHit, ScanParams, scan_section};
use crate::handlers::v1::{DIFF_MAX_ROWS, Gates};
use crate::incident::{
    EnrichedEpisode, EpisodeRefV1, IdentityValue, Series, SeriesInsertError, SeriesSet,
};

/// One counted honesty signal from building the engine input.
///
/// Every dropped episode and filtered point lands in exactly one of these, so
/// the response can report degradation instead of silently smoothing it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputQuality {
    /// Episodes dropped because an identity value was not a canonical scalar
    /// (`Null`, float, timestamp, blob, or list).
    pub non_canonical_identity: u64,
    /// Series points dropped because their diff rate was non-finite.
    pub non_finite_points: u64,
    /// Points collapsed because two samples of one series shared a timestamp.
    pub duplicate_timestamps: u64,
    /// Series skipped because the same identity was already inserted for the
    /// section and column.
    pub duplicate_series: u64,
}

/// Why building the engine input failed before analysis could start.
#[derive(Debug)]
pub(crate) enum InputError {
    /// A requested name is not a registered logical section.
    UnknownSection(String),
    /// Reading or decoding a section failed, or the registry contract was
    /// inconsistent — never masked as an absence of anomalies.
    Read(QueryError),
    /// A scanned episode named a column absent from its section's union schema.
    UnknownColumn {
        section: &'static str,
        column: String,
    },
    /// The scanned series exceeded the engine's owned-point limit.
    SeriesLimit { observed: usize, limit: usize },
}

/// One section that could not be scanned in full and was skipped, leaving the
/// others usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionSkip {
    pub section: &'static str,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The period exceeded the reader materialization budget.
    ResultTooLarge,
    /// The period had more rows than one page could hold.
    IncompletePage,
    /// Scoring the section would exceed the request's numeric-scan budget.
    ScanBudget,
}

/// Owned engine input plus the coverage and honesty counters gathered building
/// it. No field borrows the snapshot.
pub(crate) struct PreparedInput {
    pub episodes: Vec<EnrichedEpisode>,
    pub series: SeriesSet,
    pub coverage_by_section: BTreeMap<&'static str, Vec<Gap>>,
    pub quality: InputQuality,
    pub skipped: Vec<SectionSkip>,
}

/// Read `sections` for one source over `[from, to)`, scan each for anomaly
/// episodes, and assemble the engine's owned input.
///
/// `series_point_limit` caps the total points held in the returned `SeriesSet`;
/// `scan_budget` and `hit_limit` bound the numeric scan exactly as the anomaly
/// endpoint does. Oversized or incomplete sections are skipped, not fatal; a
/// read, decode, or registry-contract failure aborts with a typed error.
pub(crate) fn prepare_input(
    snap: &mut LocalDirSnapshot,
    source: u64,
    scan: &ScanParams,
    sections: &[&'static str],
    series_point_limit: usize,
    scan_budget: usize,
    hit_limit: usize,
) -> Result<PreparedInput, InputError> {
    let logicals: Vec<LogicalSection> = sections
        .iter()
        .map(|&name| {
            logical_section(name).ok_or_else(|| InputError::UnknownSection(name.to_owned()))
        })
        .collect::<Result<_, _>>()?;

    let gates = load_gates(snap, &logicals, source, scan.from, scan.to)?;

    let mut episodes = Vec::new();
    let mut series = SeriesSet::new(series_point_limit);
    let mut coverage_by_section = BTreeMap::new();
    let mut quality = InputQuality::default();
    let mut skipped = Vec::new();
    let mut remaining_scan = scan_budget;

    for logical in &logicals {
        let page = match query_section(
            snap,
            logical.name,
            source,
            scan.from,
            scan.to,
            DIFF_MAX_ROWS,
            None,
        ) {
            Ok(page) => page,
            Err(QueryError::ResultTooLarge { .. }) => {
                skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::ResultTooLarge,
                });
                continue;
            }
            Err(err) => return Err(InputError::Read(err)),
        };
        if page.next_cursor.is_some() {
            skipped.push(SectionSkip {
                section: logical.name,
                reason: SkipReason::IncompletePage,
            });
            continue;
        }
        coverage_by_section.insert(logical.name, page.gaps.clone());

        let identity = logical.diff_key();
        let (cumulative, gauges) = scorable_columns(logical);
        let mut diffs = diff_section(&identity, &cumulative, &page.rows, &page.gaps);
        gates.apply(logical, &mut diffs);
        let gauge_series = gauge_section(&identity, &gauges, &page.rows);

        let scanned = match scan_section(&diffs, &gauge_series, scan, remaining_scan, hit_limit) {
            Ok((hits, _counts, work)) => {
                remaining_scan -= work;
                hits
            }
            Err(_limit) => {
                skipped.push(SectionSkip {
                    section: logical.name,
                    reason: SkipReason::ScanBudget,
                });
                continue;
            }
        };

        ingest_section(
            logical,
            &diffs,
            &gauge_series,
            scanned,
            &mut series,
            &mut episodes,
            &mut quality,
        )?;
    }

    Ok(PreparedInput {
        episodes,
        series,
        coverage_by_section,
        quality,
        skipped,
    })
}

/// Fetch the gate sections the logical sections declare and fold them into the
/// gating timelines, reusing the anomaly endpoint's contract.
fn load_gates(
    snap: &mut LocalDirSnapshot,
    logicals: &[LogicalSection],
    source: u64,
    from: i64,
    to: i64,
) -> Result<Gates, InputError> {
    let mut pages = BTreeMap::new();
    for name in Gates::sections(logicals) {
        let page = query_section(snap, name, source, from, to, DIFF_MAX_ROWS, None)
            .map_err(InputError::Read)?;
        pages.insert(name.to_owned(), page);
    }
    Ok(Gates::from_pages(logicals, &pages))
}

/// Turn one section's scan hits and scanned series into engine input.
///
/// Both the episode references and the inserted series carry the same
/// canonical identity, so a lens looks a member's series up by its reference.
fn ingest_section(
    logical: &LogicalSection,
    diffs: &[SeriesDiff],
    gauges: &[SeriesValues],
    hits: Vec<EpisodeHit>,
    series: &mut SeriesSet,
    episodes: &mut Vec<EnrichedEpisode>,
    quality: &mut InputQuality,
) -> Result<(), InputError> {
    let mut inserted: BTreeSet<(&'static str, Vec<IdentityValue>)> = BTreeSet::new();
    for hit in hits {
        let Some(identity) = canonical_identity(&hit.key, quality) else {
            continue;
        };
        let column = resolve_column(logical, &hit.column)?;
        let reference = EpisodeRefV1 {
            logical_section: logical.name,
            column,
            identity: identity.clone().into(),
            start_us: hit.episode.start,
            end_us: hit.episode.end,
        };

        let key = (column, identity.clone());
        if inserted.insert(key) {
            let points = column_points(diffs, gauges, &hit.key, column, quality);
            insert_series(series, logical.name, column, identity, points, quality)?;
        }

        episodes.push(EnrichedEpisode {
            episode: hit.episode,
            reference,
        });
    }
    Ok(())
}

/// Convert an identity row to canonical scalars, or count the drop.
///
/// Only signed/unsigned integers, booleans, and resolved text encode into a
/// series identity; a `Null`, float, timestamp, blob, or list drops the whole
/// episode with a counted reason (§5.2 conservative rejection).
fn canonical_identity(key: &[Value], quality: &mut InputQuality) -> Option<Vec<IdentityValue>> {
    let mut identity = Vec::with_capacity(key.len());
    for value in key {
        let scalar = match value {
            Value::I64(v) => IdentityValue::I64(*v),
            Value::U64(v) => IdentityValue::U64(*v),
            Value::Bool(v) => IdentityValue::Bool(*v),
            Value::Str(text) => IdentityValue::Text(text.clone()),
            Value::Null | Value::F64(_) | Value::Ts(_) | Value::Blob { .. } | Value::ListI32(_) => {
                quality.non_canonical_identity += 1;
                return None;
            }
        };
        identity.push(scalar);
    }
    Some(identity)
}

/// Resolve a scanned column name to the section's static union name.
///
/// A scan hit always names a column of the section it scanned; a miss means the
/// registry contract and the scan disagree, which is a typed error, not a drop.
fn resolve_column(logical: &LogicalSection, column: &str) -> Result<&'static str, InputError> {
    logical
        .columns
        .iter()
        .find(|candidate| candidate.name == column)
        .map(|candidate| candidate.name)
        .ok_or_else(|| InputError::UnknownColumn {
            section: logical.name,
            column: column.to_owned(),
        })
}

/// Collect the `(ts, value)` timeline the scan scored for one series column:
/// finite diff rates for cumulative columns, raw readings for gauges.
fn column_points(
    diffs: &[SeriesDiff],
    gauges: &[SeriesValues],
    key: &[Value],
    column: &'static str,
    quality: &mut InputQuality,
) -> Vec<(i64, f64)> {
    if let Some(series) = diffs.iter().find(|series| series.key == key)
        && let Some(diff) = series.columns.iter().find(|diff| diff.name == column)
    {
        let mut points = Vec::with_capacity(diff.points.len());
        for at in &diff.points {
            match at.point {
                DiffPoint::Value { rate, .. } if rate.is_finite() => points.push((at.ts, rate)),
                _ => quality.non_finite_points += 1,
            }
        }
        return points;
    }
    if let Some(series) = gauges.iter().find(|series| series.key == key)
        && let Some(values) = series.columns.iter().find(|values| values.name == column)
    {
        return gauge_points(values);
    }
    Vec::new()
}

/// A gauge column's raw points; every value is already finite (`gauge_section`
/// counts non-finite readings as skipped).
fn gauge_points(values: &ColumnValues) -> Vec<(i64, f64)> {
    values.points.clone()
}

/// Insert one series into the set, collapsing duplicate timestamps so the
/// strictly-increasing contract of `Series::new` holds. On equal timestamps the
/// last value wins, matching a monotone snapshot stream's freshest read.
fn insert_series(
    series: &mut SeriesSet,
    section: &'static str,
    column: &'static str,
    identity: Vec<IdentityValue>,
    points: Vec<(i64, f64)>,
    quality: &mut InputQuality,
) -> Result<(), InputError> {
    let (ts, values) = dedup_timestamps(points, quality);
    // A non-finite value is filtered upstream and duplicate timestamps are
    // collapsed here, so `Series::new` cannot reject a prepared timeline; an
    // empty timeline is valid and simply carries no series.
    let Ok(built) = Series::new(ts, values) else {
        return Ok(());
    };
    if built.is_empty() {
        return Ok(());
    }
    match series.insert(section, column, identity.into(), built) {
        Ok(()) => Ok(()),
        Err(SeriesInsertError::Duplicate) => {
            quality.duplicate_series += 1;
            Ok(())
        }
        Err(SeriesInsertError::PointLimit { observed, limit }) => {
            Err(InputError::SeriesLimit { observed, limit })
        }
    }
}

/// Collapse points that share a timestamp, keeping the last value, and count
/// each collapse. Input is in snapshot-time order, so a stable pass over
/// consecutive equal timestamps suffices.
fn dedup_timestamps(points: Vec<(i64, f64)>, quality: &mut InputQuality) -> (Vec<i64>, Vec<f64>) {
    let mut ts = Vec::with_capacity(points.len());
    let mut values = Vec::with_capacity(points.len());
    for (at, value) in points {
        match values.last_mut() {
            Some(last) if ts.last() == Some(&at) => {
                quality.duplicate_timestamps += 1;
                *last = value;
            }
            _ => {
                ts.push(at);
                values.push(value);
            }
        }
    }
    (ts, values)
}

/// Split a section's columns into cumulative and gauge name lists, matching the
/// anomaly scan's scorable-column selection.
fn scorable_columns(logical: &LogicalSection) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut cumulative = Vec::new();
    let mut gauges = Vec::new();
    for column in &logical.columns {
        match column.class {
            ColumnClass::Cumulative => cumulative.push(column.name),
            ColumnClass::Gauge => gauges.push(column.name),
            ColumnClass::Label | ColumnClass::Timestamp => {}
        }
    }
    (cumulative, gauges)
}

#[cfg(test)]
mod tests {
    use kronika_reader::{ColumnDiff, DiffAt, Reason, Scalar};

    use super::*;
    use crate::incident::{ClockRelation, IncidentConfig, analyze};

    const MINUTE: i64 = 60 * 1_000_000;

    fn quality() -> InputQuality {
        InputQuality::default()
    }

    #[test]
    fn every_canonical_scalar_converts_and_the_rest_drop_the_episode() {
        let mut q = quality();
        let accepted = canonical_identity(
            &[
                Value::I64(-7),
                Value::U64(9),
                Value::Bool(true),
                Value::Str("db".to_owned()),
            ],
            &mut q,
        );
        assert_eq!(
            accepted,
            Some(vec![
                IdentityValue::I64(-7),
                IdentityValue::U64(9),
                IdentityValue::Bool(true),
                IdentityValue::Text("db".to_owned()),
            ])
        );
        assert_eq!(q.non_canonical_identity, 0);

        for rejected in [
            Value::Null,
            Value::F64(1.0),
            Value::Ts(1),
            Value::Blob {
                text: "x".to_owned(),
                full_len: 1,
                truncated: false,
            },
            Value::ListI32(vec![1]),
        ] {
            let mut q = quality();
            assert_eq!(
                canonical_identity(&[Value::I64(1), rejected], &mut q),
                None,
                "a non-canonical value drops the whole identity"
            );
            assert_eq!(q.non_canonical_identity, 1);
        }
    }

    #[test]
    fn an_empty_identity_is_the_canonical_singleton_key() {
        let mut q = quality();
        assert_eq!(canonical_identity(&[], &mut q), Some(Vec::new()));
        assert_eq!(q.non_canonical_identity, 0);
    }

    #[test]
    fn resolve_column_returns_the_static_name_or_a_typed_error() {
        let logical = logical_section("pg_stat_archiver").expect("archiver in the registry");
        let resolved = resolve_column(&logical, "archived_count").expect("column exists");
        assert_eq!(resolved, "archived_count");
        assert!(matches!(
            resolve_column(&logical, "no_such_column"),
            Err(InputError::UnknownColumn { section, column })
                if section == "pg_stat_archiver" && column == "no_such_column"
        ));
    }

    #[test]
    fn equal_timestamps_collapse_to_the_last_value_and_are_counted() {
        let mut q = quality();
        let (ts, values) = dedup_timestamps(
            vec![(1, 10.0), (1, 11.0), (2, 20.0), (2, 21.0), (2, 22.0)],
            &mut q,
        );
        assert_eq!(ts, vec![1, 2]);
        assert_eq!(values, vec![11.0, 22.0], "the freshest read wins on a tie");
        assert_eq!(q.duplicate_timestamps, 3);
    }

    #[test]
    fn distinct_timestamps_pass_through_unchanged() {
        let mut q = quality();
        let (ts, values) = dedup_timestamps(vec![(1, 1.0), (2, 2.0), (3, 3.0)], &mut q);
        assert_eq!(ts, vec![1, 2, 3]);
        assert_eq!(values, vec![1.0, 2.0, 3.0]);
        assert_eq!(q.duplicate_timestamps, 0);
    }

    #[test]
    fn column_points_keeps_finite_rates_and_counts_the_rest() {
        let key = vec![Value::I64(1)];
        let diffs = vec![SeriesDiff {
            key: key.clone(),
            columns: vec![ColumnDiff {
                name: "c".to_owned(),
                points: vec![
                    DiffAt {
                        ts: 0,
                        point: DiffPoint::NoData {
                            reason: Reason::FirstPoint,
                        },
                    },
                    DiffAt {
                        ts: 60,
                        point: DiffPoint::Value {
                            delta: Scalar::Int(5),
                            rate: 5.0,
                            dt_micros: 60,
                        },
                    },
                    DiffAt {
                        ts: 120,
                        point: DiffPoint::Value {
                            delta: Scalar::Float(f64::NAN),
                            rate: f64::NAN,
                            dt_micros: 60,
                        },
                    },
                    DiffAt {
                        ts: 180,
                        point: DiffPoint::NoData {
                            reason: Reason::Reset,
                        },
                    },
                ],
            }],
        }];
        let mut q = quality();
        let points = column_points(&diffs, &[], &key, "c", &mut q);
        assert_eq!(points, vec![(60, 5.0)], "only the one finite rate survives");
        assert_eq!(
            q.non_finite_points, 3,
            "FirstPoint, NaN, and Reset each leave no point"
        );
    }

    #[test]
    fn column_points_reads_gauge_readings_when_the_column_is_a_gauge() {
        let key = vec![Value::I64(1)];
        let gauges = vec![SeriesValues {
            key: key.clone(),
            columns: vec![ColumnValues {
                name: "g".to_owned(),
                points: vec![(0, 1.0), (60, 2.0)],
                skipped: 0,
            }],
        }];
        let mut q = quality();
        assert_eq!(
            column_points(&[], &gauges, &key, "g", &mut q),
            vec![(0, 1.0), (60, 2.0)]
        );
        assert_eq!(q.non_finite_points, 0);
    }

    /// Split forty per-minute archiver snapshots across two segments; the
    /// counter climbs by one per minute except minutes 20..25, where it climbs
    /// by fifty. Returns the last snapshot time.
    fn write_two_segment_spike(dir: &std::path::Path) -> i64 {
        use kronika_registry::pg_stat_archiver::PgStatArchiver;
        use kronika_registry::{Section, Ts};

        let row = |ts: i64, archived: i64| PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        };

        let mut count = 0_i64;
        let mut rows = Vec::new();
        for minute in 0..40_i64 {
            count += if (20..25).contains(&minute) { 50 } else { 1 };
            rows.push(row(minute * MINUTE, count));
        }

        let write_segment = |file: &str, rows: &[PgStatArchiver], min_ts: i64, max_ts: i64| {
            let body = PgStatArchiver::encode(rows).expect("encode archiver");
            let bytes = kronika_format::build_part(
                &[kronika_format::SectionInput {
                    type_id: 1_008_001,
                    rows: u32::try_from(rows.len()).expect("row count fits u32"),
                    body: &body,
                }],
                kronika_format::PartMeta {
                    min_ts,
                    max_ts,
                    source_id: 7,
                },
            );
            std::fs::write(dir.join(file), &bytes).expect("write segment");
        };

        write_segment("0.pgm", &rows[..20], 0, 19 * MINUTE);
        write_segment("2000.pgm", &rows[20..], 20 * MINUTE, 39 * MINUTE);
        39 * MINUTE
    }

    fn spike_scan(to: i64) -> ScanParams {
        ScanParams {
            from: 0,
            to,
            window: 6 * MINUTE,
            step: 2 * MINUTE,
            threshold: 3.5,
            eps_rel: 0.05,
        }
    }

    #[test]
    fn a_real_two_segment_spike_becomes_an_incident_with_matching_members() {
        let dir = tempfile::tempdir().expect("tempdir");
        let to = write_two_segment_spike(dir.path());
        let mut snap = LocalDirSnapshot::open(dir.path()).expect("open two-segment snapshot");

        let scan = spike_scan(to);
        let prepared = prepare_input(
            &mut snap,
            7,
            &scan,
            &["pg_stat_archiver"],
            1_000_000,
            crate::anomaly::MAX_SCORE_WORK,
            50,
        )
        .expect("prepare input from real segments");

        assert!(
            prepared.skipped.is_empty(),
            "the two segments read and scan cleanly: {:?}",
            prepared.skipped
        );
        let episode = prepared
            .episodes
            .iter()
            .find(|episode| {
                episode.reference.logical_section == "pg_stat_archiver"
                    && episode.reference.column == "archived_count"
            })
            .expect("the archived_count spike surfaces as an episode");
        assert!(
            episode.reference.identity.is_empty(),
            "pg_stat_archiver is a singleton series"
        );
        assert_eq!(
            episode.episode.peak.dir,
            kronika_anomaly::Direction::Up,
            "the counter jump is an upward rate anomaly"
        );
        let (start_us, end_us) = (episode.reference.start_us, episode.reference.end_us);

        let config =
            IncidentConfig::for_test("node-7", MINUTE, 3_600 * MINUTE, ClockRelation::Unknown);
        let outcome = analyze(prepared.episodes, &prepared.series, &[], &config)
            .expect("engine analyzes prepared input");

        assert_eq!(
            outcome.incidents.len(),
            1,
            "one contiguous spike yields one incident cluster"
        );
        let incident = &outcome.incidents[0];
        assert!(
            incident
                .members
                .iter()
                .any(|member| member.logical_section == "pg_stat_archiver"
                    && member.column == "archived_count"),
            "the incident's members carry the real anomalous series"
        );
        assert!(
            incident.start_us <= start_us && incident.end_us >= end_us,
            "the incident interval covers the episode it was built from"
        );
        assert!(
            outcome.complete,
            "no lens is registered, so nothing is skipped"
        );
        assert!(
            incident.findings.is_empty(),
            "no lens catalog yet, so the cluster carries no findings"
        );
    }

    #[test]
    fn a_calm_two_segment_series_yields_no_incident() {
        use kronika_registry::pg_stat_archiver::PgStatArchiver;
        use kronika_registry::{Section, Ts};

        let dir = tempfile::tempdir().expect("tempdir");
        let row = |ts: i64, archived: i64| PgStatArchiver {
            ts: Ts(ts),
            archived_count: archived,
            last_archived_wal: None,
            last_archived_time: None,
            failed_count: 0,
            last_failed_wal: None,
            last_failed_time: None,
            stats_reset: None,
        };
        let rows: Vec<PgStatArchiver> = (0..40_i64).map(|m| row(m * MINUTE, m + 1)).collect();
        let write = |file: &str, rows: &[PgStatArchiver], min_ts: i64, max_ts: i64| {
            let body = PgStatArchiver::encode(rows).expect("encode archiver");
            let bytes = kronika_format::build_part(
                &[kronika_format::SectionInput {
                    type_id: 1_008_001,
                    rows: u32::try_from(rows.len()).expect("row count fits u32"),
                    body: &body,
                }],
                kronika_format::PartMeta {
                    min_ts,
                    max_ts,
                    source_id: 7,
                },
            );
            std::fs::write(dir.path().join(file), &bytes).expect("write segment");
        };
        write("0.pgm", &rows[..20], 0, 19 * MINUTE);
        write("2000.pgm", &rows[20..], 20 * MINUTE, 39 * MINUTE);

        let mut snap = LocalDirSnapshot::open(dir.path()).expect("open snapshot");
        let scan = spike_scan(39 * MINUTE);
        let prepared = prepare_input(
            &mut snap,
            7,
            &scan,
            &["pg_stat_archiver"],
            1_000_000,
            crate::anomaly::MAX_SCORE_WORK,
            50,
        )
        .expect("prepare input");

        let config =
            IncidentConfig::for_test("node-7", MINUTE, 3_600 * MINUTE, ClockRelation::Unknown);
        let outcome = analyze(prepared.episodes, &prepared.series, &[], &config).expect("analyze");
        assert!(
            outcome.incidents.is_empty(),
            "a steady counter produces no anomaly and no incident"
        );
    }
}
