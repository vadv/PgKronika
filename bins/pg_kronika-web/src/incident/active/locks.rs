use std::sync::Arc;

use super::super::cluster::Cluster;
use super::super::dispatch::{LimitHit, SectionColumn};
use super::super::engine::EvalContext;
use super::super::evidence::sink::FindingSink;
use super::super::evidence::{
    ConfidenceCap, DirectEvidence, Evidence, FindingDraft, FindingScope, LockParticipant, Role,
};
use super::super::lens::Lens;
use super::super::model::IdentityValue;
use super::super::series::SeriesSet;
use super::super::typed::TypedInputs;
use super::shared::PG_LOCKS;

/// `PG-LOCK-012` (`lock_wait_graph`): a sampled lock wait-for graph. Each
/// `blocked_by` edge in a `pg_locks` snapshot is direct evidence of a process
/// that prevented a waiter from acquiring the requested lock (it may name a
/// queue predecessor rather than the holder). The blocker is reported as the
/// lead and the waiter as its downstream for that sampled edge. Confidence is
/// capped at medium until lock target and mode join the evidence.
pub(crate) struct LockWaitGraphLens;

impl LockWaitGraphLens {
    const ID: &'static str = "PG-LOCK-012";
    const INPUTS: &'static [SectionColumn] = &[SectionColumn {
        section: PG_LOCKS,
        column: "blocked_by",
    }];
}

impl Lens for LockWaitGraphLens {
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
        let Some(_member) = cluster
            .members
            .iter()
            .find(|member| member.logical_section == PG_LOCKS)
        else {
            return Ok(());
        };
        let (start, end) = (context.incident_start_us, context.incident_end_us);
        let snapshots: Vec<_> = typed.lock_window(start, end).collect();
        let examined: usize = snapshots.iter().map(|snapshot| snapshot.edges.len()).sum();
        sink.charge_points(examined)?;
        for snapshot in snapshots {
            for edge in &snapshot.edges {
                let blocker: Arc<[IdentityValue]> = Arc::from(vec![
                    IdentityValue::I64(snapshot.ts),
                    IdentityValue::I64(edge.blocker_pid),
                    IdentityValue::I64(edge.waiter_pid),
                ]);
                sink.emit(FindingDraft::new(
                    Role::Lead,
                    FindingScope::from_parts(PG_LOCKS, "blocked_by", blocker),
                    vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                        snapshot.ts,
                        edge.waiter_pid,
                        edge.blocker_pid,
                        LockParticipant::Blocker,
                    ))],
                ))?;
                let waiter: Arc<[IdentityValue]> = Arc::from(vec![
                    IdentityValue::I64(snapshot.ts),
                    IdentityValue::I64(edge.waiter_pid),
                    IdentityValue::I64(edge.blocker_pid),
                ]);
                sink.emit(FindingDraft::new(
                    Role::Downstream,
                    FindingScope::from_parts(PG_LOCKS, "blocked_by", waiter),
                    vec![Evidence::Direct(DirectEvidence::sampled_lock_edge(
                        snapshot.ts,
                        edge.waiter_pid,
                        edge.blocker_pid,
                        LockParticipant::Waiter,
                    ))],
                ))?;
            }
        }
        Ok(())
    }
}
