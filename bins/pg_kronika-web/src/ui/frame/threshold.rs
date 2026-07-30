use kronika_analytics::{Classified, MetricId, MetricInput};

use super::projection::{DeltaOperand, ProjectedRow};
use crate::ui::catalog::ColumnSpec;
use crate::ui::thresholds::{OperandKind, binding_for};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameThresholdContext;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CellClassification {
    pub column: &'static str,
    pub metric_id: MetricId,
    pub result: Classified,
}

#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    reason = "the exhaustive adapter match mirrors the stable manifest and its planned context API"
)]
pub(crate) fn prepare_input(
    kind: OperandKind,
    row: &ProjectedRow,
    _context: &FrameThresholdContext,
) -> MetricInput {
    match kind {
        OperandKind::ActivityQueryDuration => {
            if row.operands.activity_state.as_deref() != Some("active") {
                return MetricInput::NotApplicable;
            }
            duration_input(row.operands.snapshot_ts_us, row.operands.query_start_us)
        }
        OperandKind::ActivityTransactionDuration => duration_input(
            row.operands.snapshot_ts_us,
            row.operands.transaction_start_us,
        ),
        OperandKind::StatementMillisecondsPerRow => {
            let Some(statement) = &row.operands.statements else {
                return MetricInput::Missing;
            };
            quotient(statement.exec_ms, statement.rows)
        }
        OperandKind::StatementMeanMilliseconds => {
            let Some(statement) = &row.operands.statements else {
                return MetricInput::Missing;
            };
            quotient(statement.exec_ms, statement.calls)
        }
        OperandKind::StatementTimePercent => {
            let Some(statement) = &row.operands.statements else {
                return MetricInput::Missing;
            };
            let Some(total) = statement.snapshot_exec_ms_delta_sum else {
                return MetricInput::NotApplicable;
            };
            scalar_delta(statement.exec_ms).map_or(MetricInput::Missing, |value| {
                MetricInput::Scalar(100.0 * value / total)
            })
        }
        OperandKind::StatementPlanTimePercent => {
            let Some(statement) = &row.operands.statements else {
                return MetricInput::Missing;
            };
            if !statement.planning_fields || !statement.track_planning {
                return MetricInput::NotApplicable;
            }
            match (
                scalar_delta(statement.plan_ms),
                scalar_delta(statement.exec_ms),
            ) {
                (Some(plan), Some(exec)) if plan + exec > 0.0 => {
                    MetricInput::Scalar(100.0 * plan / (plan + exec))
                }
                (Some(_), Some(_)) => MetricInput::NotApplicable,
                _ => MetricInput::Missing,
            }
        }
        OperandKind::TableDeadTupleRatio => {
            let Some(table) = &row.operands.table else {
                return MetricInput::Missing;
            };
            match (table.dead, table.live) {
                (Some(dead), Some(live)) if dead + live > 0.0 => MetricInput::RatioWithFloor {
                    ratio: dead / (dead + live),
                    count: dead,
                },
                (Some(_), Some(_)) => MetricInput::NotApplicable,
                _ => MetricInput::Missing,
            }
        }
        OperandKind::TableDeadTuples => table_scalar(row, |table| table.dead),
        OperandKind::TableSequentialScanPercent => {
            let Some(table) = &row.operands.table else {
                return MetricInput::Missing;
            };
            match (scalar_delta(table.seq_scan), scalar_delta(table.idx_scan)) {
                (Some(seq), Some(index)) if seq + index > 0.0 => {
                    MetricInput::Scalar(100.0 * seq / (seq + index))
                }
                (Some(_), Some(_)) => MetricInput::NotApplicable,
                _ => MetricInput::Missing,
            }
        }
        OperandKind::TableModifiedSinceAnalyze => table_scalar(row, |table| table.modified),
        OperandKind::TableInsertedSinceVacuum => {
            let Some(table) = &row.operands.table else {
                return MetricInput::Missing;
            };
            match table.inserted {
                None => MetricInput::NotApplicable,
                Some(None) => MetricInput::Missing,
                Some(Some(value)) => MetricInput::Scalar(value),
            }
        }
        OperandKind::TableAutovacuumAge => {
            let Some(table) = &row.operands.table else {
                return MetricInput::Missing;
            };
            age_input(
                row.operands.snapshot_ts_us,
                table.last_autovacuum_us,
                table.dead.map(|dead| dead > 0.0),
            )
        }
        OperandKind::TableAutoanalyzeAge => {
            let Some(table) = &row.operands.table else {
                return MetricInput::Missing;
            };
            age_input(
                row.operands.snapshot_ts_us,
                table.last_autoanalyze_us,
                table.modified.map(|modified| modified >= 10_000.0),
            )
        }
        OperandKind::ProcessRssKib => row
            .operands
            .process_rss_kib
            .map_or(MetricInput::Missing, MetricInput::Scalar),
    }
}

#[must_use]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the context is passed by reference because later revisions may add snapshot operands"
)]
pub(crate) fn classify_row(
    view: &str,
    columns: &[&ColumnSpec],
    row: &ProjectedRow,
    context: &FrameThresholdContext,
) -> Vec<CellClassification> {
    columns
        .iter()
        .filter_map(|column| {
            let binding = binding_for(view, column.code)?;
            let input = prepare_input(binding.operand, row, context);
            Some(CellClassification {
                column: column.code,
                metric_id: binding.metric_id,
                result: kronika_analytics::threshold::classify(binding.metric_id, input),
            })
        })
        .collect()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "unix microseconds become the analytics catalog's f64 seconds operand"
)]
fn duration_input(now_us: i64, start_us: Option<i64>) -> MetricInput {
    start_us.map_or(MetricInput::NotApplicable, |start_us| {
        MetricInput::Scalar((now_us - start_us) as f64 / 1_000_000.0)
    })
}

const fn scalar_delta(delta: DeltaOperand) -> Option<f64> {
    match delta {
        DeltaOperand::Value(value) => Some(value),
        DeltaOperand::Missing | DeltaOperand::Reset | DeltaOperand::Gap => None,
    }
}

fn quotient(numerator: DeltaOperand, denominator: DeltaOperand) -> MetricInput {
    match (scalar_delta(numerator), scalar_delta(denominator)) {
        (Some(numerator), Some(denominator)) if denominator > 0.0 => {
            MetricInput::Scalar(numerator / denominator)
        }
        (Some(_), Some(_)) => MetricInput::NotApplicable,
        _ => MetricInput::Missing,
    }
}

fn table_scalar(
    row: &ProjectedRow,
    select: impl FnOnce(&super::projection::TableOperands) -> Option<f64>,
) -> MetricInput {
    row.operands
        .table
        .as_ref()
        .and_then(select)
        .map_or(MetricInput::Missing, MetricInput::Scalar)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "unix microseconds become the analytics catalog's f64 seconds operand"
)]
fn age_input(now_us: i64, epoch_us: Option<i64>, gate: Option<bool>) -> MetricInput {
    match (epoch_us, gate) {
        (Some(epoch_us), Some(gate)) => MetricInput::Age {
            epoch_seconds: epoch_us as f64 / 1_000_000.0,
            now_seconds: now_us as f64 / 1_000_000.0,
            gate,
        },
        _ => MetricInput::Missing,
    }
}
