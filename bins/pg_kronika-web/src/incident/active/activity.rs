use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::super::cluster::Cluster;
use super::super::dispatch::{LimitHit, SectionColumn};
use super::super::engine::EvalContext;
use super::super::entity_join::{
    EntityJoinIndex, EntityJoinInsert, EntityJoinKey, EntityScope, TypedEntityIdentity,
};
use super::super::evidence::sink::FindingSink;
use super::super::evidence::{
    ConfidenceCap, DirectEvidence, Evidence, FindingDraft, FindingScope, GaugeEntity,
    GaugeEvidence, GaugeRatio, GaugeUnit, GaugeValueInput, LockParticipant, Role, SourceWindow,
    ThresholdKind,
};
use super::super::lens::Lens;
use super::super::model::IdentityValue;
use super::super::series::SeriesSet;
use super::super::typed::{ActivityBackend, TypedInputs};
use super::shared::{
    PG_LOCKS, PG_STAT_ACTIVITY, activity_backends_examined, activity_member, exact_i64_as_f64,
    idle_in_transaction, is_syncrep, lock_edges_examined,
};

const MAX_LOCK_EDGE_EVIDENCE: usize = 128;

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
        SectionColumn {
            section: PG_LOCKS,
            column: "blocked_by",
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

    fn lock_edge_witnesses(
        typed: &TypedInputs,
        start: i64,
        end: i64,
        snapshot_ts: i64,
        backend: &ActivityBackend,
    ) -> BTreeSet<(i64, i64, i64)> {
        let mut witnesses = BTreeSet::new();
        for snapshot in typed.lock_window(start, end).filter(|snapshot| {
            snapshot.ts == snapshot_ts && snapshot.activity_snapshot_ts == Some(snapshot_ts)
        }) {
            for edge in &snapshot.edges {
                if edge.waiter_pid != backend.pid
                    || edge.waiter_backend_start != Some(backend.backend_start)
                    || edge.waiter_pid <= 0
                    || edge.blocker_pid < 0
                    || edge.blocker_pid == edge.waiter_pid
                {
                    continue;
                }
                witnesses.insert((snapshot.ts, edge.waiter_pid, edge.blocker_pid));
                if witnesses.len() > MAX_LOCK_EDGE_EVIDENCE {
                    let _ = witnesses.pop_last();
                }
            }
        }
        witnesses
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
            sink.charge_points(lock_edges_examined(typed, start, end))?;
            let lock_edges = Self::lock_edge_witnesses(typed, start, end, observed_at_us, backend);
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
            evidence.extend(lock_edges.into_iter().map(|(edge_ts, waiter, blocker)| {
                Evidence::Direct(DirectEvidence::sampled_lock_edge(
                    edge_ts,
                    waiter,
                    blocker,
                    LockParticipant::Waiter,
                ))
            }));
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

/// `PG-WAIT-019` (`internal_wait_concentration`): sampled wait-class
/// concentration. A heavyweight `Lock` sample additionally requires an exact
/// same-snapshot, same-session `blocked_by` edge.
pub(crate) struct InternalWaitConcentrationLens;

type LockEdgeIndex<'scope> = EntityJoinIndex<'scope>;
type LockEdgeWitnesses = BTreeSet<(i64, i64, i64)>;

struct WaitConcentration {
    snapshots: usize,
    active_total: usize,
    class_totals: [usize; 4],
    lock_edge_witnesses: LockEdgeWitnesses,
    observed_at_us: i64,
}

impl InternalWaitConcentrationLens {
    const ID: &'static str = "PG-WAIT-019";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_ACTIVITY,
            column: "wait_event_type",
        },
        SectionColumn {
            section: PG_LOCKS,
            column: "blocked_by",
        },
    ];
    /// A fraction over too few active backends is noise; require a floor so the
    /// concentration is meaningful.
    const MIN_ACTIVE: usize = 3;

    const MIN_SNAPSHOTS: usize = 3;
    const WAIT_FRACTION: f64 = 0.5;

    fn index_lock_edges<'scope>(
        typed: &TypedInputs,
        start: i64,
        end: i64,
        scope: EntityScope<'scope>,
        relation_limit: usize,
    ) -> Option<LockEdgeIndex<'scope>> {
        let mut indexed = EntityJoinIndex::new(scope, relation_limit);
        for snapshot in typed.lock_window(start, end) {
            if snapshot.activity_snapshot_ts != Some(snapshot.ts) {
                continue;
            }
            for edge in &snapshot.edges {
                let Some(waiter_start) = edge.waiter_backend_start.filter(|start| *start > 0)
                else {
                    continue;
                };
                if edge.waiter_pid <= 0
                    || edge.blocker_pid < 0
                    || edge.blocker_pid == edge.waiter_pid
                {
                    continue;
                }
                let Some(identity) =
                    TypedEntityIdentity::postgres_backend_session(edge.waiter_pid, waiter_start)
                else {
                    continue;
                };
                let key = EntityJoinKey::shared_snapshot(snapshot.ts, snapshot.ts, identity);
                if matches!(
                    indexed.insert(key, edge.blocker_pid),
                    EntityJoinInsert::LimitExceeded { .. }
                ) {
                    return None;
                }
            }
        }
        Some(indexed)
    }

    fn record_lock_witnesses(
        indexed: &LockEdgeIndex<'_>,
        scope: EntityScope<'_>,
        snapshot_ts: i64,
        backend: &ActivityBackend,
        witnesses: &mut LockEdgeWitnesses,
    ) -> bool {
        let Some(identity) =
            TypedEntityIdentity::postgres_backend_session(backend.pid, backend.backend_start)
        else {
            return false;
        };
        let key = EntityJoinKey::shared_snapshot(snapshot_ts, snapshot_ts, identity);
        let Some(matching) = indexed.matches(scope, &key) else {
            return false;
        };
        for &blocker in matching {
            witnesses.insert((snapshot_ts, backend.pid, blocker));
            if witnesses.len() > MAX_LOCK_EDGE_EVIDENCE {
                let _ = witnesses.pop_last();
            }
        }
        true
    }

    fn measure(
        typed: &TypedInputs,
        start: i64,
        end: i64,
        indexed: &LockEdgeIndex<'_>,
        scope: EntityScope<'_>,
    ) -> WaitConcentration {
        let mut measured = WaitConcentration {
            snapshots: 0,
            active_total: 0,
            class_totals: [0; 4],
            lock_edge_witnesses: BTreeSet::new(),
            observed_at_us: start,
        };
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
            measured.snapshots = measured.snapshots.saturating_add(1);
            measured.active_total = measured.active_total.saturating_add(active);
            measured.observed_at_us = snapshot.ts;
            for backend in snapshot
                .backends
                .iter()
                .filter(|backend| backend.state.as_deref() == Some("active"))
            {
                let class = match backend.wait_event_type.as_deref() {
                    Some("LWLock") => Some(0),
                    Some("BufferPin") => Some(1),
                    Some("IO") => Some(2),
                    Some("Lock")
                        if Self::record_lock_witnesses(
                            indexed,
                            scope,
                            snapshot.ts,
                            backend,
                            &mut measured.lock_edge_witnesses,
                        ) =>
                    {
                        Some(3)
                    }
                    _ => None,
                };
                if let Some(class) = class {
                    measured.class_totals[class] = measured.class_totals[class].saturating_add(1);
                }
            }
        }
        measured
    }

    fn emit(
        measured: &WaitConcentration,
        start: i64,
        end: i64,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        let Some(active_total) = u32::try_from(measured.active_total).ok().map(f64::from) else {
            return Ok(());
        };
        for (class, waiting) in
            measured
                .class_totals
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, count)| {
                    measured.snapshots >= Self::MIN_SNAPSHOTS
                        && measured.active_total > 0
                        && count.saturating_mul(2) >= measured.active_total
                })
        {
            let class = ["LWLock", "BufferPin", "IO", "Lock"][class];
            let Some(waiting) = u32::try_from(waiting).ok().map(f64::from) else {
                continue;
            };
            let identity: Arc<[IdentityValue]> =
                Arc::from(vec![IdentityValue::Text(class.to_owned())]);
            let Some(gauge) = GaugeEvidence::ratio(
                GaugeRatio::new(
                    "waiting_backends",
                    waiting,
                    "active_backends",
                    active_total,
                    GaugeUnit::Count,
                ),
                Self::WAIT_FRACTION,
                ThresholdKind::AtLeast,
                measured.observed_at_us,
                measured.snapshots,
                SourceWindow::from_bounds(start, end, None, measured.snapshots),
                GaugeEntity::new(PG_STAT_ACTIVITY, Arc::clone(&identity)),
            ) else {
                continue;
            };
            let mut evidence = vec![Evidence::GaugeObservation(gauge)];
            if class == "Lock" {
                evidence.extend(measured.lock_edge_witnesses.iter().map(
                    |&(ts, waiter, blocker)| {
                        Evidence::Direct(DirectEvidence::sampled_lock_edge(
                            ts,
                            waiter,
                            blocker,
                            LockParticipant::Waiter,
                        ))
                    },
                ));
            }
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_parts(PG_STAT_ACTIVITY, "wait_event_type", identity),
                evidence,
            ))?;
        }
        Ok(())
    }
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
        if activity_member(cluster).is_none() {
            return Ok(());
        }
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        let Some(scope) = context.entity_scope() else {
            return Ok(());
        };
        let activity_points = activity_backends_examined(typed, start, end);
        sink.charge_points(activity_points)?;
        let lock_edges = lock_edges_examined(typed, start, end);
        sink.charge_points(lock_edges)?;
        // Every activity row is the conservative ceiling for one membership probe.
        sink.charge_points(activity_points)?;
        let Some(lock_edges_by_waiter) =
            Self::index_lock_edges(typed, start, end, scope, lock_edges)
        else {
            return Ok(());
        };
        let measured = Self::measure(typed, start, end, &lock_edges_by_waiter, scope);
        Self::emit(&measured, start, end, sink)
    }
}
