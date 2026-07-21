use std::collections::BTreeMap;
use std::sync::Arc;

use super::super::cluster::Cluster;
use super::super::dispatch::{LimitHit, SectionColumn};
use super::super::engine::EvalContext;
use super::super::evidence::sink::FindingSink;
use super::super::evidence::{
    ConfidenceCap, Evidence, FindingDraft, FindingScope, GaugeEntity, GaugeEvidence, GaugeRatio,
    GaugeUnit, GaugeValueInput, Role, SourceWindow, ThresholdKind,
};
use super::super::lens::Lens;
use super::super::model::IdentityValue;
use super::super::series::SeriesSet;
use super::super::typed::{ActivityBackend, TypedInputs};
use super::shared::{
    PG_STAT_ACTIVITY, activity_backends_examined, activity_member, exact_i64_as_f64,
    idle_in_transaction, is_syncrep,
};

/// `PG-HORIZON-013` (`xmin_horizon_hold`): an observed old transaction with an
/// assigned xmin capable of holding the vacuum horizon. Prepared transactions,
/// slots, and standby feedback are independent alternatives outside this row
/// observation. Unseen snapshots prevent a global-cause or persistence claim.
pub(crate) struct XminHorizonHoldLens;

impl XminHorizonHoldLens {
    const ID: &'static str = "PG-HORIZON-013";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_ACTIVITY,
            column: "backend_xmin_age",
        },
        SectionColumn {
            section: PG_STAT_ACTIVITY,
            column: "xact_start",
        },
    ];
    /// Below this xmin age the hold is ordinary transaction churn, not a horizon
    /// a vacuum is waiting on.
    const MIN_XMIN_AGE: i64 = 1_000_000;
    const MIN_XMIN_AGE_F64: f64 = 1_000_000.0;
    /// A running (non-idle) transaction must be at least this old, in
    /// microseconds, to count as a horizon-holding long transaction (5 minutes).
    const MIN_LONG_XACT_US: i64 = 300_000_000;
    const MIN_LONG_XACT_US_F64: f64 = 300_000_000.0;

    fn holds_horizon(backend: &ActivityBackend) -> bool {
        backend
            .xmin_age
            .is_some_and(|age| age >= Self::MIN_XMIN_AGE)
            && (idle_in_transaction(backend.state.as_deref())
                || backend
                    .xact_age_us
                    .is_some_and(|age| age >= Self::MIN_LONG_XACT_US))
    }
}

impl Lens for XminHorizonHoldLens {
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
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        let Some(member) = activity_member(cluster) else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        sink.charge_points(activity_backends_examined(typed, start, end))?;
        let candidate = typed
            .activity_window(start, end)
            .flat_map(|snapshot| {
                snapshot.backends.iter().filter_map(move |backend| {
                    (backend.pid > 0 && backend.backend_start > 0 && Self::holds_horizon(backend))
                        .then_some((snapshot.ts, backend))
                })
            })
            .max_by_key(|(_, backend)| backend.xmin_age.unwrap_or_default());
        if let Some((observed_at_us, backend)) = candidate {
            let identity: Arc<[IdentityValue]> = Arc::from(vec![
                IdentityValue::I64(backend.pid),
                IdentityValue::I64(backend.backend_start),
            ]);
            let Some(xmin_age) = backend.xmin_age.and_then(exact_i64_as_f64) else {
                return Ok(());
            };
            let Some(xmin_evidence) = GaugeEvidence::value(GaugeValueInput {
                operand: "xmin_age",
                value: xmin_age,
                unit: GaugeUnit::Count,
                threshold: Self::MIN_XMIN_AGE_F64,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us,
                samples: 1,
                source_window: SourceWindow::from_bounds(start, end, None, 1),
                entity: GaugeEntity::new(PG_STAT_ACTIVITY, Arc::clone(&identity)),
            }) else {
                return Ok(());
            };
            let mut evidence = vec![Evidence::GaugeObservation(xmin_evidence)];
            if let Some(xact_age_us) = backend.xact_age_us.and_then(exact_i64_as_f64)
                && let Some(xact_evidence) = GaugeEvidence::value(GaugeValueInput {
                    operand: "xact_age_us",
                    value: xact_age_us,
                    unit: GaugeUnit::Microseconds,
                    threshold: Self::MIN_LONG_XACT_US_F64,
                    threshold_kind: ThresholdKind::AtLeast,
                    observed_at_us,
                    samples: 1,
                    source_window: SourceWindow::from_bounds(start, end, None, 1),
                    entity: GaugeEntity::new(PG_STAT_ACTIVITY, identity),
                })
            {
                evidence.push(Evidence::GaugeObservation(xact_evidence));
            }
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                evidence,
            ))?;
        }
        Ok(())
    }
}

/// `PG-SYNC-018` (`sync_replication_wait`): the same backend session sampled on
/// `wait_event='SyncRep'` in three consecutive snapshots. This is a positive
/// sampled observation, not elapsed wait duration or a standby root cause.
pub(crate) struct SyncReplicationWaitLens;

impl SyncReplicationWaitLens {
    const ID: &'static str = "PG-SYNC-018";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_STAT_ACTIVITY,
        column: "wait_event",
    }];
    /// Require the same session in three consecutive stored snapshots. This is
    /// sampled persistence, not elapsed wait duration.
    const MIN_CONSECUTIVE_SAMPLES: usize = 3;
}

impl Lens for SyncReplicationWaitLens {
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
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(cluster.members.len())?;
        let Some(member) = activity_member(cluster) else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        sink.charge_points(activity_backends_examined(typed, start, end))?;
        let mut runs: BTreeMap<(i64, i64), (usize, usize, i64)> = BTreeMap::new();
        for (snapshot_index, snapshot) in typed.activity_window(start, end).enumerate() {
            for backend in snapshot.backends.iter().filter(|backend| {
                backend.pid > 0
                    && backend.backend_start > 0
                    && is_syncrep(backend.wait_event.as_deref())
            }) {
                let entry = runs.entry((backend.pid, backend.backend_start)).or_insert((
                    snapshot_index,
                    0,
                    snapshot.ts,
                ));
                entry.1 = if entry.1 > 0 && entry.0.checked_add(1) == Some(snapshot_index) {
                    entry.1.saturating_add(1)
                } else {
                    1
                };
                entry.0 = snapshot_index;
                entry.2 = snapshot.ts;
            }
        }
        let persistent = runs
            .into_iter()
            .filter(|(_, (_, samples, _))| *samples >= Self::MIN_CONSECUTIVE_SAMPLES)
            .max_by_key(|(_, (_, samples, _))| *samples);
        if let Some(((pid, backend_start), (_, samples, observed_at_us))) = persistent {
            let Some(samples_value) = u32::try_from(samples).ok().map(f64::from) else {
                return Ok(());
            };
            let identity: Arc<[IdentityValue]> = Arc::from(vec![
                IdentityValue::I64(pid),
                IdentityValue::I64(backend_start),
            ]);
            let Some(evidence) = GaugeEvidence::value(GaugeValueInput {
                operand: "consecutive_syncrep_samples",
                value: samples_value,
                unit: GaugeUnit::Count,
                threshold: 3.0,
                threshold_kind: ThresholdKind::AtLeast,
                observed_at_us,
                samples,
                source_window: SourceWindow::from_bounds(start, end, None, samples),
                entity: GaugeEntity::new(PG_STAT_ACTIVITY, identity),
            }) else {
                return Ok(());
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-WAIT-019` (`internal_wait_concentration`): active backends piling onto
/// internal waits. Reports each bounded wait class separately only when at
/// least three snapshots carry explicit complete-visibility provenance and the
/// class accounts for at least half of the visible active-backend denominator.
pub(crate) struct InternalWaitConcentrationLens;

impl InternalWaitConcentrationLens {
    const ID: &'static str = "PG-WAIT-019";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_STAT_ACTIVITY,
        column: "wait_event_type",
    }];
    /// A fraction over too few active backends is noise; require a floor so the
    /// concentration is meaningful.
    const MIN_ACTIVE: usize = 3;

    const MIN_SNAPSHOTS: usize = 3;
    const WAIT_FRACTION: f64 = 0.5;
}

impl Lens for InternalWaitConcentrationLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        Self::INPUTS
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::Low
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
        let Some(member) = activity_member(cluster) else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        sink.charge_points(activity_backends_examined(typed, start, end))?;
        let mut snapshots = 0_usize;
        let mut active_total = 0_usize;
        let mut class_totals = [0_usize; 3];
        let mut observed_at_us = start;
        for snapshot in typed
            .activity_window(start, end)
            .filter(|snapshot| snapshot.completeness.denominator_usable())
        {
            let active = snapshot
                .backends
                .iter()
                .filter(|backend| backend.state.as_deref() == Some("active"))
                .count();
            if active < Self::MIN_ACTIVE {
                continue;
            }
            snapshots = snapshots.saturating_add(1);
            active_total = active_total.saturating_add(active);
            observed_at_us = snapshot.ts;
            for backend in snapshot
                .backends
                .iter()
                .filter(|backend| backend.state.as_deref() == Some("active"))
            {
                match backend.wait_event_type.as_deref() {
                    Some("LWLock") => class_totals[0] = class_totals[0].saturating_add(1),
                    Some("BufferPin") => class_totals[1] = class_totals[1].saturating_add(1),
                    Some("IO") => class_totals[2] = class_totals[2].saturating_add(1),
                    _ => {}
                }
            }
        }
        let concentrated = class_totals
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, count)| {
                snapshots >= Self::MIN_SNAPSHOTS
                    && active_total > 0
                    && count.saturating_mul(2) >= active_total
            })
            .max_by_key(|(_, count)| *count);
        if let Some((class, waiting)) = concentrated {
            let class = match class {
                0 => "LWLock",
                1 => "BufferPin",
                _ => "IO",
            };
            let identity: Arc<[IdentityValue]> =
                Arc::from(vec![IdentityValue::Text(class.to_owned())]);
            let (Some(waiting), Some(active_total)) = (
                u32::try_from(waiting).ok().map(f64::from),
                u32::try_from(active_total).ok().map(f64::from),
            ) else {
                return Ok(());
            };
            let Some(evidence) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "waiting_backends",
                    waiting,
                    "active_backends",
                    active_total,
                    GaugeUnit::Count,
                ),
                Self::WAIT_FRACTION,
                ThresholdKind::AtLeast,
                observed_at_us,
                snapshots,
                SourceWindow::from_bounds(start, end, None, snapshots),
                GaugeEntity::new(PG_STAT_ACTIVITY, identity),
            ) else {
                return Ok(());
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::GaugeObservation(evidence)],
            ))?;
        }
        Ok(())
    }
}
