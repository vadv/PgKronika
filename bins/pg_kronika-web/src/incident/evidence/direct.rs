use super::confidence::Role;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DirectEvidence {
    kind: DirectEvidenceKind,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DirectEvidenceKind {
    SampledLockEdge(SampledLockEdge),
    ResourceLimitEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LockParticipant {
    Blocker,
    Waiter,
}

impl LockParticipant {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Blocker => "blocker",
            Self::Waiter => "waiter",
        }
    }

    const fn proves_role(self, requested_role: Role) -> bool {
        matches!(
            (self, requested_role),
            (Self::Blocker, Role::Lead) | (Self::Waiter, Role::Downstream)
        )
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SampledLockEdge {
    observed_at_us: i64,
    waiter_pid: i64,
    blocker_pid: i64,
    participant: LockParticipant,
}

impl SampledLockEdge {
    pub(crate) const fn observed_at_us(&self) -> i64 {
        self.observed_at_us
    }

    pub(crate) const fn waiter_pid(&self) -> i64 {
        self.waiter_pid
    }

    pub(crate) const fn blocker_pid(&self) -> i64 {
        self.blocker_pid
    }

    pub(crate) const fn participant(&self) -> LockParticipant {
        self.participant
    }
}

impl DirectEvidence {
    /// A sampled `pg_locks` blocking edge: `blocked_by` names a process that
    /// prevented the waiter from acquiring the lock. It can be a queue
    /// predecessor rather than a lock holder. This proves the sampled edge's
    /// direction; the lens still controls the confidence ceiling.
    pub(crate) const fn sampled_lock_edge(
        observed_at_us: i64,
        waiter_pid: i64,
        blocker_pid: i64,
        participant: LockParticipant,
    ) -> Self {
        Self {
            kind: DirectEvidenceKind::SampledLockEdge(SampledLockEdge {
                observed_at_us,
                waiter_pid,
                blocker_pid,
                participant,
            }),
        }
    }

    #[cfg(test)]
    pub(super) const fn resource_limit_event() -> Self {
        Self {
            kind: DirectEvidenceKind::ResourceLimitEvent,
        }
    }

    pub(super) const fn proves_structural_direction(&self, requested_role: Role) -> bool {
        match &self.kind {
            DirectEvidenceKind::SampledLockEdge(edge) => {
                edge.participant.proves_role(requested_role)
            }
            DirectEvidenceKind::ResourceLimitEvent => false,
        }
    }

    pub(crate) const fn lock_edge(&self) -> Option<&SampledLockEdge> {
        match &self.kind {
            DirectEvidenceKind::SampledLockEdge(edge) => Some(edge),
            DirectEvidenceKind::ResourceLimitEvent => None,
        }
    }
}
