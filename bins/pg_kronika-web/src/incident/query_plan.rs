//! Statement and plan lenses over privacy-reduced typed evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::sink::FindingSink;
use super::evidence::{
    ConfidenceCap, Evidence, FindingDraft, FindingScope, GaugeEntity, GaugeEvidence, GaugeRatio,
    GaugeUnit, GaugeValueInput, Role, SourceWindow, ThresholdKind,
};
use super::lens::Lens;
use super::model::IdentityValue;
use super::series::SeriesSet;
use super::typed::{AlignedSums, PlanFork, PlanSample, TypedInputs};

const STATEMENTS: &str = "pg_stat_statements";
const PLANS_OSSC: &str = "pg_store_plans_ossc";
const PLANS_VADV: &str = "pg_store_plans_vadv";

pub(crate) struct QueryWorkLens;

impl QueryWorkLens {
    const MIN_INTERVALS: usize = 3;
    const MIN_MS_PER_CALL: f64 = 50.0;
    const MIN_BLOCKS_PER_CALL: f64 = 1_000.0;
    const EXEC_COLUMNS: [&'static str; 13] = [
        "calls",
        "total_exec_time",
        "rows",
        "shared_blks_hit",
        "shared_blks_read",
        "shared_blks_dirtied",
        "shared_blks_written",
        "local_blks_hit",
        "local_blks_read",
        "local_blks_dirtied",
        "local_blks_written",
        "temp_blks_read",
        "temp_blks_written",
    ];
    const LEGACY_COLUMNS: [&'static str; 13] = [
        "calls",
        "total_time",
        "rows",
        "shared_blks_hit",
        "shared_blks_read",
        "shared_blks_dirtied",
        "shared_blks_written",
        "local_blks_hit",
        "local_blks_read",
        "local_blks_dirtied",
        "local_blks_written",
        "temp_blks_read",
        "temp_blks_written",
    ];

    fn evidence(
        aligned: AlignedSums,
        time_column: &'static str,
        identity: &Arc<[IdentityValue]>,
    ) -> Option<Vec<Evidence>> {
        let calls = aligned.sums[0];
        let total_ms = aligned.sums[1];
        let rows = aligned.sums[2];
        let block_work: f64 = aligned.sums[3..aligned.len].iter().sum();
        if calls <= 0.0 || total_ms < 0.0 || rows < 0.0 || block_work < 0.0 {
            return None;
        }
        let ms_per_call = total_ms / calls;
        let blocks_per_call = block_work / calls;
        if !ms_per_call.is_finite()
            || !blocks_per_call.is_finite()
            || (ms_per_call < Self::MIN_MS_PER_CALL && blocks_per_call < Self::MIN_BLOCKS_PER_CALL)
        {
            return None;
        }
        let entity = || GaugeEntity::new(STATEMENTS, Arc::clone(identity));
        [
            GaugeEvidence::value(GaugeValueInput {
                operand: "calls_delta_sum",
                value: calls,
                unit: GaugeUnit::Count,
                threshold: 1.0,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: aligned.last_end_us,
                samples: aligned.intervals,
                source_window: aligned.source_window,
                entity: entity(),
            }),
            GaugeEvidence::ratio(
                GaugeRatio::with_units(
                    time_column,
                    total_ms,
                    GaugeUnit::Milliseconds,
                    "calls",
                    calls,
                    GaugeUnit::Count,
                    GaugeUnit::MillisecondsPerCall,
                ),
                Self::MIN_MS_PER_CALL,
                ThresholdKind::AtLeast,
                aligned.last_end_us,
                aligned.intervals,
                aligned.source_window,
                entity(),
            ),
            GaugeEvidence::ratio(
                GaugeRatio::with_units(
                    "rows",
                    rows,
                    GaugeUnit::Count,
                    "calls",
                    calls,
                    GaugeUnit::Count,
                    GaugeUnit::RowsPerCall,
                ),
                0.0,
                ThresholdKind::AtLeast,
                aligned.last_end_us,
                aligned.intervals,
                aligned.source_window,
                entity(),
            ),
            GaugeEvidence::ratio(
                GaugeRatio::with_units(
                    "block_accesses_sum",
                    block_work,
                    GaugeUnit::Count,
                    "calls",
                    calls,
                    GaugeUnit::Count,
                    GaugeUnit::BlocksPerCall,
                ),
                Self::MIN_BLOCKS_PER_CALL,
                ThresholdKind::AtLeast,
                aligned.last_end_us,
                aligned.intervals,
                aligned.source_window,
                entity(),
            ),
        ]
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .map(|evidence| {
            evidence
                .into_iter()
                .map(Evidence::GaugeObservation)
                .collect()
        })
    }
}

impl Lens for QueryWorkLens {
    fn id(&self) -> &'static str {
        "PG-QRY-001"
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        const INPUTS: &[SectionColumn] = &[
            SectionColumn {
                section: STATEMENTS,
                column: "calls",
            },
            SectionColumn {
                section: STATEMENTS,
                column: "total_exec_time",
            },
            SectionColumn {
                section: STATEMENTS,
                column: "total_time",
            },
        ];
        INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        for member in cluster
            .members
            .iter()
            .filter(|member| member.logical_section == STATEMENTS)
        {
            let mut aligned = None;
            for columns in [&Self::EXEC_COLUMNS[..], &Self::LEGACY_COLUMNS[..]] {
                sink.charge_points(typed.aligned_counter_points(
                    STATEMENTS,
                    &member.identity,
                    columns,
                ))?;
                if let Some(candidate) = typed.aligned_delta_sums(
                    STATEMENTS,
                    &member.identity,
                    columns,
                    context.incident_start_us,
                    context.incident_end_us,
                ) && candidate.intervals >= Self::MIN_INTERVALS
                {
                    aligned = Some((candidate, columns[1]));
                    break;
                }
            }
            let Some((aligned, time_column)) = aligned else {
                continue;
            };
            let Some(evidence) = Self::evidence(aligned, time_column, &member.identity) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                evidence,
            ))?;
        }
        Ok(())
    }
}

pub(crate) struct PlanChurnLens;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Bridge {
    fork: PlanFork,
    dbid: u64,
    userid: u64,
    queryid: i64,
}

struct PlanCandidate {
    bridge: Bridge,
    planid: i64,
    attributed_queryid: i64,
    calls: f64,
    total_time_ms: f64,
    calls_denominator: f64,
    observed_at_us: i64,
}

impl PlanChurnLens {
    const MAX_PAIRED_GAP_US: i64 = 10 * 60 * 1_000_000;

    fn bridge(sample: &PlanSample) -> Bridge {
        Bridge {
            fork: sample.fork,
            dbid: sample.dbid,
            userid: sample.userid,
            queryid: if sample.fork == PlanFork::Ossc {
                sample.queryid
            } else {
                0
            },
        }
    }

    fn candidate(samples: &[PlanSample]) -> Option<PlanCandidate> {
        let mut grouped: BTreeMap<Bridge, BTreeMap<i64, BTreeMap<i64, PlanSample>>> =
            BTreeMap::new();
        for &sample in samples {
            grouped
                .entry(Self::bridge(&sample))
                .or_default()
                .entry(sample.ts)
                .or_default()
                .insert(sample.planid, sample);
        }
        let mut best: Option<PlanCandidate> = None;
        for (bridge, snapshots) in grouped {
            let snapshots: Vec<_> = snapshots.into_iter().collect();
            for window in snapshots.windows(3) {
                let [(before_ts, before), (change_ts, changed), (after_ts, after)] = window else {
                    continue;
                };
                if change_ts
                    .checked_sub(*before_ts)
                    .is_none_or(|gap| gap <= 0 || gap > Self::MAX_PAIRED_GAP_US)
                    || after_ts
                        .checked_sub(*change_ts)
                        .is_none_or(|gap| gap <= 0 || gap > Self::MAX_PAIRED_GAP_US)
                {
                    continue;
                }
                let before_set: BTreeSet<_> = before.keys().copied().collect();
                let changed_set: BTreeSet<_> = changed.keys().copied().collect();
                if before_set == changed_set {
                    continue;
                }
                let mut interval_calls = 0.0;
                for (planid, current) in changed {
                    if let Some(next) = after.get(planid) {
                        let delta = next.calls - current.calls;
                        if delta.is_finite() && delta > 0.0 {
                            interval_calls += delta;
                        }
                    }
                }
                if interval_calls <= 0.0 || !interval_calls.is_finite() {
                    continue;
                }
                for planid in changed_set.difference(&before_set) {
                    let (Some(current), Some(next)) = (changed.get(planid), after.get(planid))
                    else {
                        continue;
                    };
                    let calls = next.calls - current.calls;
                    let total_time_ms = next.total_time_ms - current.total_time_ms;
                    if calls <= 0.0
                        || total_time_ms <= 0.0
                        || !calls.is_finite()
                        || !total_time_ms.is_finite()
                    {
                        continue;
                    }
                    let candidate = PlanCandidate {
                        bridge,
                        planid: *planid,
                        attributed_queryid: next.queryid,
                        calls,
                        total_time_ms,
                        calls_denominator: interval_calls,
                        observed_at_us: *after_ts,
                    };
                    if best
                        .as_ref()
                        .is_none_or(|current| candidate.total_time_ms > current.total_time_ms)
                    {
                        best = Some(candidate);
                    }
                }
            }
        }
        best
    }
}

impl Lens for PlanChurnLens {
    fn id(&self) -> &'static str {
        "PG-PLAN-002"
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        const INPUTS: &[SectionColumn] = &[
            SectionColumn {
                section: PLANS_OSSC,
                column: "planid",
            },
            SectionColumn {
                section: PLANS_VADV,
                column: "planid",
            },
        ];
        INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Medium
    }

    fn evaluate(
        &self,
        cluster: &Cluster,
        _series: &SeriesSet,
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        let Some(member) = cluster
            .members
            .iter()
            .find(|member| matches!(member.logical_section, PLANS_OSSC | PLANS_VADV))
        else {
            return Ok(());
        };
        let samples = typed.plan_window(context.incident_start_us, context.incident_end_us);
        sink.charge_points(samples.len())?;
        let Some(candidate) = Self::candidate(samples) else {
            return Ok(());
        };
        let fork = match candidate.bridge.fork {
            PlanFork::Ossc => "ossc_exact_queryid",
            PlanFork::Vadv => "vadv_plan_identity",
        };
        let identity: Arc<[IdentityValue]> = Arc::from(vec![
            IdentityValue::Text(fork.to_owned()),
            IdentityValue::U64(candidate.bridge.dbid),
            IdentityValue::U64(candidate.bridge.userid),
            IdentityValue::I64(candidate.attributed_queryid),
            IdentityValue::I64(candidate.planid),
        ]);
        let entity = || GaugeEntity::new(member.logical_section, Arc::clone(&identity));
        // A plan change is a two-point bridge, not a sampled series, so there is
        // no cadence to derive: source-window completeness stays unproven.
        let source_window =
            SourceWindow::from_bounds(context.incident_start_us, context.incident_end_us, None, 2);
        let evidence = [
            GaugeEvidence::value(GaugeValueInput {
                operand: "new_plan_calls_delta",
                value: candidate.calls,
                unit: GaugeUnit::Count,
                threshold: 1.0,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: candidate.observed_at_us,
                samples: 2,
                source_window,
                entity: entity(),
            }),
            GaugeEvidence::value(GaugeValueInput {
                operand: "new_plan_total_time_ms_delta",
                value: candidate.total_time_ms,
                unit: GaugeUnit::Milliseconds,
                threshold: 0.0,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us: candidate.observed_at_us,
                samples: 2,
                source_window,
                entity: entity(),
            }),
            GaugeEvidence::ratio(
                GaugeRatio::new(
                    "new_plan_calls_delta",
                    candidate.calls,
                    "all_plan_calls_delta",
                    candidate.calls_denominator,
                    GaugeUnit::Count,
                ),
                0.0,
                ThresholdKind::AtLeast,
                candidate.observed_at_us,
                2,
                source_window,
                entity(),
            ),
        ]
        .into_iter()
        .collect::<Option<Vec<_>>>();
        let Some(evidence) = evidence else {
            return Ok(());
        };
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_episode(member),
            evidence
                .into_iter()
                .map(Evidence::GaugeObservation)
                .collect(),
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::model::EnrichedEpisode;
    use crate::incident::{ClockRelation, IncidentConfig, analyze};
    use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Scalar};

    fn identity() -> Arc<[IdentityValue]> {
        Arc::from(vec![
            IdentityValue::I64(42),
            IdentityValue::U64(10),
            IdentityValue::U64(20),
            IdentityValue::Bool(true),
        ])
    }

    fn episode(section: &'static str, column: &'static str) -> EnrichedEpisode {
        EnrichedEpisode {
            episode: Episode {
                start: 0,
                end: 2,
                peak_ts: 2,
                peak: Evaluated {
                    m: 5.0,
                    dir: Direction::Up,
                    med_cur: 5.0,
                    med_ref: 1.0,
                    mad_ref: 1.0,
                    sigma_used: 1.0,
                    n_cur: 3,
                    n_ref: 3,
                },
            },
            reference: super::super::model::EpisodeRefV1 {
                logical_section: section,
                column,
                identity: identity(),
                start_us: 0,
                end_us: 10,
            },
        }
    }

    fn delta(value: f64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Float(value),
            rate: value,
            dt_micros: 1_000_000,
        }
    }

    fn query_input(reset_rows: bool) -> TypedInputs {
        let mut typed = TypedInputs::new();
        for (index, column) in QueryWorkLens::EXEC_COLUMNS.iter().enumerate() {
            let value = match index {
                0 => 2.0,
                1 => 200.0,
                2 => 4.0,
                _ => 100.0,
            };
            let mut points = vec![(1, delta(value)), (2, delta(value)), (3, delta(value))];
            if reset_rows && *column == "rows" {
                points[1].1 = DiffPoint::NoData {
                    reason: kronika_analytics::Reason::Reset,
                };
            }
            typed.insert_counter(STATEMENTS, column, identity(), points);
        }
        typed
    }

    fn outcome(
        lens: &dyn Lens,
        episode: EnrichedEpisode,
        typed: &TypedInputs,
    ) -> crate::incident::EngineOutcome {
        let lenses = [lens];
        analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            typed,
            &lenses,
            &IncidentConfig::for_test("node", 5, 1_000, ClockRelation::Unknown),
        )
        .expect("analysis")
    }

    fn findings(lens: &dyn Lens, episode: EnrichedEpisode, typed: &TypedInputs) -> usize {
        outcome(lens, episode, typed).incidents[0].findings.len()
    }

    #[test]
    fn query_work_uses_one_shared_valid_interval_set() {
        let input = query_input(false);
        assert_eq!(
            findings(&QueryWorkLens, episode(STATEMENTS, "calls"), &input),
            1
        );
        let detected = outcome(&QueryWorkLens, episode(STATEMENTS, "calls"), &input);
        let units: Vec<_> = detected.incidents[0].findings[0]
            .evidence()
            .iter()
            .map(|evidence| match evidence {
                Evidence::GaugeObservation(gauge) => gauge.unit(),
                _ => panic!("query evidence is numeric"),
            })
            .collect();
        assert_eq!(
            units,
            [
                GaugeUnit::Count,
                GaugeUnit::MillisecondsPerCall,
                GaugeUnit::RowsPerCall,
                GaugeUnit::BlocksPerCall,
            ]
        );
        assert_eq!(
            findings(
                &QueryWorkLens,
                episode(STATEMENTS, "calls"),
                &query_input(true)
            ),
            0,
            "a reset in rows invalidates the whole paired interval"
        );
    }

    fn plan(ts: i64, fork: PlanFork, planid: i64, calls: f64, total_time_ms: f64) -> PlanSample {
        plan_for_query(ts, fork, 42, planid, calls, total_time_ms)
    }

    fn plan_for_query(
        ts: i64,
        fork: PlanFork,
        queryid: i64,
        planid: i64,
        calls: f64,
        total_time_ms: f64,
    ) -> PlanSample {
        PlanSample {
            ts,
            fork,
            queryid,
            planid,
            userid: 10,
            dbid: 20,
            calls,
            total_time_ms,
        }
    }

    #[test]
    fn plan_churn_requires_set_change_and_paired_new_plan_work() {
        let mut typed = TypedInputs::new();
        typed.insert_plan_samples(vec![
            plan(1, PlanFork::Ossc, 1, 10.0, 100.0),
            plan(2, PlanFork::Ossc, 1, 20.0, 200.0),
            plan(2, PlanFork::Ossc, 2, 1.0, 10.0),
            plan(3, PlanFork::Ossc, 1, 25.0, 250.0),
            plan(3, PlanFork::Ossc, 2, 11.0, 510.0),
        ]);
        assert_eq!(
            findings(&PlanChurnLens, episode(PLANS_OSSC, "planid"), &typed),
            1
        );

        let mut no_work = TypedInputs::new();
        no_work.insert_plan_samples(vec![
            plan(1, PlanFork::Vadv, 1, 10.0, 100.0),
            plan(2, PlanFork::Vadv, 1, 20.0, 200.0),
            plan(2, PlanFork::Vadv, 2, 1.0, 10.0),
            plan(3, PlanFork::Vadv, 1, 25.0, 250.0),
            plan(3, PlanFork::Vadv, 2, 1.0, 10.0),
        ]);
        assert_eq!(
            findings(&PlanChurnLens, episode(PLANS_VADV, "planid"), &no_work),
            0
        );

        let mut changing_attribution = TypedInputs::new();
        changing_attribution.insert_plan_samples(vec![
            plan_for_query(1, PlanFork::Vadv, 10, 1, 10.0, 100.0),
            plan_for_query(2, PlanFork::Vadv, 11, 1, 20.0, 200.0),
            plan_for_query(2, PlanFork::Vadv, 12, 2, 1.0, 10.0),
            plan_for_query(3, PlanFork::Vadv, 13, 1, 25.0, 250.0),
            plan_for_query(3, PlanFork::Vadv, 14, 2, 11.0, 510.0),
        ]);
        assert_eq!(
            findings(
                &PlanChurnLens,
                episode(PLANS_VADV, "planid"),
                &changing_attribution,
            ),
            1,
            "best-effort query attribution does not gate plan identity churn"
        );
    }
}
