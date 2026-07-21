//! Closed machine reasons used by successful analysis responses.

use serde::Serialize;
use serde::ser::SerializeMap as _;

use crate::problem::count_u64;

closed_string_enum! {
    /// Stable reason kinds owned by the product rather than a source.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ReasonKind {
        MaterializationLimit => "materialization_limit",
        IncompletePage => "incomplete_page",
        ScoringWorkBudget => "scoring_work_budget",
        ScanBudget => "scan_budget",
        ConflictingTimestamp => "conflicting_timestamp",
        IdentityByteLimit => "identity_byte_limit",
        SeriesPointLimit => "series_point_limit",
        TypedGaugePointLimit => "typed_gauge_point_limit",
        SnapshotRowLimit => "snapshot_row_limit",
        IncompleteSnapshot => "incomplete_snapshot",
        RetentionLimit => "retention_limit",
        NoData => "no_data",
        MissingNodeIdentity => "missing_node_identity",
        ConflictingNodeIdentity => "conflicting_node_identity",
        ProducerUnavailable => "producer_unavailable",
        ProvenanceOrInputMissing => "provenance_or_input_missing",
        CompleteProvenance => "complete_provenance",
        SectionAbsent => "section_absent",
        CompleteCoverage => "complete_coverage",
        CoverageGap => "coverage_gap",
    }
}

closed_string_enum! {
    /// Materialized resource named by a success reason.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum MaterializationResource {
        Cells => "cells",
        Bytes => "bytes",
    }
}

#[derive(Debug, Clone, Copy)]
struct EmptyParams;

impl Serialize for EmptyParams {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_map(Some(0))?.end()
    }
}

#[derive(Debug, Serialize)]
struct MaterializationParams {
    resource: MaterializationResource,
    limit: u64,
}

#[derive(Debug, Serialize)]
struct WorkParams {
    required: u64,
    available: u64,
}

#[derive(Debug, Serialize)]
struct TimestampParams {
    timestamp: i64,
}

#[derive(Debug, Serialize)]
struct ObservedLimitParams {
    observed: u64,
    limit: u64,
}

#[derive(Debug, Serialize)]
struct DroppedParams {
    dropped: u64,
}

#[derive(Debug, Serialize)]
struct GapCountParams {
    gap_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ReasonParams {
    Empty(EmptyParams),
    Materialization(MaterializationParams),
    Work(WorkParams),
    Timestamp(TimestampParams),
    ObservedLimit(ObservedLimitParams),
    Dropped(DroppedParams),
    GapCount(GapCountParams),
}

/// A reason with schema-selected typed parameters.
#[derive(Debug, Serialize)]
pub(crate) struct ApiReason {
    kind: ReasonKind,
    params: ReasonParams,
}

impl ApiReason {
    pub(crate) fn materialization_limit(resource: MaterializationResource, limit: usize) -> Self {
        Self {
            kind: ReasonKind::MaterializationLimit,
            params: ReasonParams::Materialization(MaterializationParams {
                resource,
                limit: count_u64(limit),
            }),
        }
    }

    pub(crate) const fn incomplete_page() -> Self {
        Self::empty(ReasonKind::IncompletePage)
    }

    pub(crate) fn scoring_work_budget(required: usize, available: usize) -> Self {
        Self::work(ReasonKind::ScoringWorkBudget, required, available)
    }

    pub(crate) fn scan_budget(required: usize, available: usize) -> Self {
        Self::work(ReasonKind::ScanBudget, required, available)
    }

    pub(crate) const fn conflicting_timestamp(timestamp: i64) -> Self {
        Self {
            kind: ReasonKind::ConflictingTimestamp,
            params: ReasonParams::Timestamp(TimestampParams { timestamp }),
        }
    }

    pub(crate) fn identity_byte_limit(observed: usize, limit: usize) -> Self {
        Self::observed_limit(ReasonKind::IdentityByteLimit, observed, limit)
    }

    pub(crate) fn series_point_limit(observed: usize, limit: usize) -> Self {
        Self::observed_limit(ReasonKind::SeriesPointLimit, observed, limit)
    }

    pub(crate) fn typed_gauge_point_limit(observed: usize, limit: usize) -> Self {
        Self::observed_limit(ReasonKind::TypedGaugePointLimit, observed, limit)
    }

    pub(crate) fn snapshot_row_limit(observed: usize, limit: usize) -> Self {
        Self::observed_limit(ReasonKind::SnapshotRowLimit, observed, limit)
    }

    pub(crate) const fn incomplete_snapshot() -> Self {
        Self::empty(ReasonKind::IncompleteSnapshot)
    }

    pub(crate) const fn retention_limit(dropped: u64) -> Self {
        Self {
            kind: ReasonKind::RetentionLimit,
            params: ReasonParams::Dropped(DroppedParams { dropped }),
        }
    }

    pub(crate) const fn no_data() -> Self {
        Self::empty(ReasonKind::NoData)
    }

    pub(crate) const fn missing_node_identity() -> Self {
        Self::empty(ReasonKind::MissingNodeIdentity)
    }

    pub(crate) const fn conflicting_node_identity() -> Self {
        Self::empty(ReasonKind::ConflictingNodeIdentity)
    }

    pub(crate) const fn producer_unavailable() -> Self {
        Self::empty(ReasonKind::ProducerUnavailable)
    }

    pub(crate) const fn provenance_or_input_missing() -> Self {
        Self::empty(ReasonKind::ProvenanceOrInputMissing)
    }

    pub(crate) fn complete_provenance(gap_count: usize) -> Self {
        Self::gap_count(ReasonKind::CompleteProvenance, gap_count)
    }

    pub(crate) const fn section_absent() -> Self {
        Self::empty(ReasonKind::SectionAbsent)
    }

    pub(crate) fn complete_coverage(gap_count: usize) -> Self {
        Self::gap_count(ReasonKind::CompleteCoverage, gap_count)
    }

    pub(crate) fn coverage_gap(gap_count: usize) -> Self {
        Self::gap_count(ReasonKind::CoverageGap, gap_count)
    }

    const fn empty(kind: ReasonKind) -> Self {
        Self {
            kind,
            params: ReasonParams::Empty(EmptyParams),
        }
    }

    fn work(kind: ReasonKind, required: usize, available: usize) -> Self {
        Self {
            kind,
            params: ReasonParams::Work(WorkParams {
                required: count_u64(required),
                available: count_u64(available),
            }),
        }
    }

    fn observed_limit(kind: ReasonKind, observed: usize, limit: usize) -> Self {
        Self {
            kind,
            params: ReasonParams::ObservedLimit(ObservedLimitParams {
                observed: count_u64(observed),
                limit: count_u64(limit),
            }),
        }
    }

    fn gap_count(kind: ReasonKind, gap_count: usize) -> Self {
        Self {
            kind,
            params: ReasonParams::GapCount(GapCountParams {
                gap_count: count_u64(gap_count),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) const fn kind(&self) -> ReasonKind {
        self.kind
    }
}
