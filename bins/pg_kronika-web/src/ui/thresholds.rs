//! Web-owned bindings from frame columns to numeric threshold policies.

use kronika_analytics::MetricId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperandKind {
    ActivityQueryDuration,
    ActivityTransactionDuration,
    StatementMillisecondsPerRow,
    StatementMeanMilliseconds,
    StatementTimePercent,
    StatementPlanTimePercent,
    TableDeadTupleRatio,
    TableDeadTuples,
    TableSequentialScanPercent,
    TableModifiedSinceAnalyze,
    TableInsertedSinceVacuum,
    TableAutovacuumAge,
    TableAutoanalyzeAge,
    ProcessRssKib,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredBindingReason {
    AggregateNotCell,
    MissingView,
    MissingCollectedOperand,
    IncompatibleUnit,
    NoStableCellMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    variant_size_differences,
    reason = "the explicit deferred reason is intentionally smaller than a complete binding"
)]
pub(crate) enum BindingDisposition {
    Bound {
        view: &'static str,
        column: &'static str,
        operand: OperandKind,
    },
    Deferred(DeferredBindingReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThresholdProjection {
    pub metric_id: MetricId,
    pub disposition: BindingDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThresholdBinding {
    pub metric_id: MetricId,
    pub view: &'static str,
    pub column: &'static str,
    pub operand: OperandKind,
}

const fn bound(
    metric_id: MetricId,
    view: &'static str,
    column: &'static str,
    operand: OperandKind,
) -> ThresholdProjection {
    ThresholdProjection {
        metric_id,
        disposition: BindingDisposition::Bound {
            view,
            column,
            operand,
        },
    }
}

const fn deferred(metric_id: MetricId, reason: DeferredBindingReason) -> ThresholdProjection {
    ThresholdProjection {
        metric_id,
        disposition: BindingDisposition::Deferred(reason),
    }
}

const THRESHOLD_PROJECTIONS: [ThresholdProjection; 69] = [
    deferred(
        MetricId::OsProcessCpuPercent,
        DeferredBindingReason::MissingCollectedOperand,
    ),
    deferred(
        MetricId::OsLoadAvg1PerCore,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCpuIdlePercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCpuIoWaitPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCpuStealPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsLoadProcsBlocked,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::PgActivityBackendLoadPerCore,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::OsMemoryUsedPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsProcessVirtualGrowthKib,
        DeferredBindingReason::NoStableCellMapping,
    ),
    deferred(
        MetricId::OsProcessResidentGrowthKib,
        DeferredBindingReason::NoStableCellMapping,
    ),
    deferred(
        MetricId::OsProcessVirtualSwapKib,
        DeferredBindingReason::NoStableCellMapping,
    ),
    deferred(
        MetricId::OsMemorySwapUsedKib,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsVmstatSwapInPerSecond,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsVmstatSwapOutPerSecond,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsProcessMajorFaultsDelta,
        DeferredBindingReason::NoStableCellMapping,
    ),
    bound(
        MetricId::OsProcessRssKib,
        "processes",
        "rss",
        OperandKind::ProcessRssKib,
    ),
    deferred(
        MetricId::OsPsiCpuSomePercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsPsiMemorySomePercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsPsiIoSomePercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCgroupCpuUsedPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCgroupCpuThrottledMillisecondsDelta,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCgroupCpuThrottleEventsDelta,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCgroupMemoryAnonPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCgroupMemoryHeadroomPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsCgroupMemoryOomKillsDelta,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsDiskUtilPercent,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsDiskMaxAwaitMilliseconds,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsDiskReadAwaitMilliseconds,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsDiskWriteAwaitMilliseconds,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsFilesystemFreeCapacity,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsProcessBlockDelaySecondsDelta,
        DeferredBindingReason::IncompatibleUnit,
    ),
    deferred(
        MetricId::OsDiskBlocksReadPerSecond,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsNetworkErrorsPerSecond,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::OsNetworkDropsPerSecond,
        DeferredBindingReason::MissingView,
    ),
    bound(
        MetricId::PgTablesDeadTuplePercent,
        "tables",
        "dead_pct",
        OperandKind::TableDeadTupleRatio,
    ),
    bound(
        MetricId::PgTablesDeadTuples,
        "tables",
        "dead_tuples",
        OperandKind::TableDeadTuples,
    ),
    bound(
        MetricId::PgTablesSequentialScanPercent,
        "tables",
        "seq_scan_pct",
        OperandKind::TableSequentialScanPercent,
    ),
    bound(
        MetricId::PgTablesModifiedSinceAnalyze,
        "tables",
        "modified_since_analyze",
        OperandKind::TableModifiedSinceAnalyze,
    ),
    bound(
        MetricId::PgTablesInsertedSinceVacuum,
        "tables",
        "inserted_since_vacuum",
        OperandKind::TableInsertedSinceVacuum,
    ),
    bound(
        MetricId::PgTablesAutovacuumAgeSeconds,
        "tables",
        "autovacuum_age_seconds",
        OperandKind::TableAutovacuumAge,
    ),
    bound(
        MetricId::PgTablesAutoanalyzeAgeSeconds,
        "tables",
        "autoanalyze_age_seconds",
        OperandKind::TableAutoanalyzeAge,
    ),
    deferred(
        MetricId::PgTablesTempBytesPerSecond,
        DeferredBindingReason::AggregateNotCell,
    ),
    bound(
        MetricId::PgActivityQueryDurationSeconds,
        "activity",
        "query_duration_us",
        OperandKind::ActivityQueryDuration,
    ),
    bound(
        MetricId::PgActivityTransactionDurationSeconds,
        "activity",
        "transaction_duration_us",
        OperandKind::ActivityTransactionDuration,
    ),
    deferred(
        MetricId::PgActivityClientBackendCapacity,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgActivityIdleInTransactionSessions,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgActivityBlockedSessions,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgActivityLongQueries,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgActivityLongTransactions,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgDatabaseRollbackPercent,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgDatabaseDeadlocksDelta,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgDatabaseCacheHitPercent,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgDatabaseIoCacheHitPercent,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgDatabaseEffectiveCacheHitPercent,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgCheckpointerCheckpointsPerMinute,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgCheckpointerWriteTimeMillisecondsDelta,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgBgwriterBuffersBackendPerSecond,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgBgwriterMaxwrittenCleanDelta,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgBgwriterClientEvictionsPerSecond,
        DeferredBindingReason::AggregateNotCell,
    ),
    bound(
        MetricId::PgStatementsMillisecondsPerRow,
        "statements",
        "ms_per_row",
        OperandKind::StatementMillisecondsPerRow,
    ),
    bound(
        MetricId::PgStatementsMeanTimeMilliseconds,
        "statements",
        "mean",
        OperandKind::StatementMeanMilliseconds,
    ),
    bound(
        MetricId::PgStatementsTimePercent,
        "statements",
        "time_pct",
        OperandKind::StatementTimePercent,
    ),
    bound(
        MetricId::PgStatementsPlanTimePercent,
        "statements",
        "plan_time_pct",
        OperandKind::StatementPlanTimePercent,
    ),
    deferred(
        MetricId::PgStatementsPlanCount,
        DeferredBindingReason::NoStableCellMapping,
    ),
    deferred(
        MetricId::PgReplicationReplayLagSeconds,
        DeferredBindingReason::MissingView,
    ),
    deferred(
        MetricId::PgDatabaseRecoveryConflictsDelta,
        DeferredBindingReason::AggregateNotCell,
    ),
    deferred(
        MetricId::PgTablesVacuumThresholdExceeded,
        DeferredBindingReason::MissingCollectedOperand,
    ),
    deferred(
        MetricId::PgTablesAnalyzeThresholdExceeded,
        DeferredBindingReason::MissingCollectedOperand,
    ),
    deferred(
        MetricId::PgTablesInsertVacuumThresholdExceeded,
        DeferredBindingReason::MissingCollectedOperand,
    ),
];

pub(crate) const fn threshold_projections() -> &'static [ThresholdProjection; 69] {
    &THRESHOLD_PROJECTIONS
}

pub(crate) fn binding_for(view: &str, column: &str) -> Option<ThresholdBinding> {
    threshold_projections().iter().find_map(|entry| {
        let BindingDisposition::Bound {
            view: bound_view,
            column: bound_column,
            operand,
        } = entry.disposition
        else {
            return None;
        };
        (bound_view == view && bound_column == column).then_some(ThresholdBinding {
            metric_id: entry.metric_id,
            view: bound_view,
            column: bound_column,
            operand,
        })
    })
}
