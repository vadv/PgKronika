use super::super::cluster::Cluster;
use super::super::dispatch::{LimitHit, SectionColumn};
use super::super::engine::EvalContext;
use super::super::evidence::sink::FindingSink;
use super::super::evidence::{
    ConfidenceCap, CounterMeasurementKind, CounterOperandPurpose, Evidence, FindingDraft,
    FindingScope, GaugeUnit, Role, ThresholdKind,
};
use super::super::lens::Lens;
use super::super::series::SeriesSet;
use super::super::typed::TypedInputs;
use super::shared::{
    CHECKPOINTER, CounterEvidenceSpec, CounterOperandSpec, OS_CGROUP_CPU, OS_NETDEV,
    PG_STAT_ARCHIVER, PG_STAT_DATABASE, PG_STAT_IO, PG_STAT_USER_TABLES, PG_STAT_WAL,
    counter_evidence, exact_u64_as_f64, paired_sums,
};

/// `PG-CACHE-010` (`shared_buffer_misses`): shared-buffer miss pressure. Reports
/// an elevated `sum(d(blks_read)) / sum(d(blks_read) + d(blks_hit))` over the
/// incident as an amplifier. Direction needs clock provenance, so the lens never
/// leads; without direct evidence its confidence stays capped at medium.
pub(crate) struct SharedBufferMissesLens;

impl SharedBufferMissesLens {
    const ID: &'static str = "PG-CACHE-010";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "blks_read",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "blks_hit",
        },
    ];
    /// A regular signal needs at least three valid pairs (data-quality policy).
    const MIN_INTERVALS: usize = 3;
    /// Only an unusually cold cache is worth an incident finding.
    const MISS_THRESHOLD: f64 = 0.2;
}

impl Lens for SharedBufferMissesLens {
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
        for member in &cluster.members {
            if member.logical_section != PG_STAT_DATABASE || member.column != "blks_read" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                PG_STAT_DATABASE,
                &member.identity,
                "blks_read",
                "blks_hit",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            let total = sums.sum_a + sums.sum_b;
            if total <= 0.0 || sums.sum_a / total < Self::MISS_THRESHOLD {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Ratio,
                    formula: "blks_read / (blks_read + blks_hit)",
                    value: sums.sum_a / total,
                    unit: GaugeUnit::Ratio,
                    threshold: Self::MISS_THRESHOLD,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: PG_STAT_DATABASE,
                    operands: vec![
                        CounterOperandSpec {
                            name: "blks_read",
                            value: sums.sum_a,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "blks_hit",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-WAL-009` (`wal_amplification`): WAL write amplification. Reports an
/// elevated `sum(d(wal_fpi)) / sum(d(wal_records))` (full-page images per WAL
/// record) as an amplifier. Direction needs clock provenance, so the lens never
/// leads; without direct evidence its confidence stays capped at medium.
pub(crate) struct WalAmplificationLens;

impl WalAmplificationLens {
    const ID: &'static str = "PG-WAL-009";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_WAL,
            column: "wal_fpi",
        },
        SectionColumn {
            section: PG_STAT_WAL,
            column: "wal_records",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// Half the WAL records carrying a full-page image is heavy amplification.
    const FPI_THRESHOLD: f64 = 0.5;
}

impl Lens for WalAmplificationLens {
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
        for member in &cluster.members {
            if member.logical_section != PG_STAT_WAL || member.column != "wal_fpi" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                PG_STAT_WAL,
                &member.identity,
                "wal_fpi",
                "wal_records",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            if sums.sum_b <= 0.0 || sums.sum_a / sums.sum_b < Self::FPI_THRESHOLD {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Ratio,
                    formula: "wal_fpi / wal_records",
                    value: sums.sum_a / sums.sum_b,
                    unit: GaugeUnit::Ratio,
                    threshold: Self::FPI_THRESHOLD,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: PG_STAT_WAL,
                    operands: vec![
                        CounterOperandSpec {
                            name: "wal_fpi",
                            value: sums.sum_a,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "wal_records",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-TEMP-003` (`temp_spill`): spill into temporary files. Reports an amplifier
/// when both `temp_bytes` and `temp_files` advanced over the incident, the honest
/// signature of query work spilling to disk. It publishes the spilled
/// `temp_bytes` volume; confidence is capped at medium.
pub(crate) struct TempSpillLens;

impl TempSpillLens {
    const ID: &'static str = "PG-TEMP-003";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "temp_bytes",
        },
        SectionColumn {
            section: PG_STAT_DATABASE,
            column: "temp_files",
        },
    ];
    const MIN_INTERVALS: usize = 3;
}

impl Lens for TempSpillLens {
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
        for member in &cluster.members {
            if member.logical_section != PG_STAT_DATABASE || member.column != "temp_bytes" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                PG_STAT_DATABASE,
                &member.identity,
                "temp_bytes",
                "temp_files",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            if sums.sum_a <= 0.0 || sums.sum_b <= 0.0 {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Sum,
                    formula: "temp_bytes",
                    value: sums.sum_a,
                    unit: GaugeUnit::Bytes,
                    threshold: 0.0,
                    threshold_kind: ThresholdKind::Above,
                    section: PG_STAT_DATABASE,
                    operands: vec![
                        CounterOperandSpec {
                            name: "temp_bytes",
                            value: sums.sum_a,
                            unit: GaugeUnit::Bytes,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "temp_files",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Qualification,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-CHKPT-008` (`requested_checkpoints`): checkpoints forced by demand rather
/// than by the timer. Reports an elevated
/// `sum(d(checkpoints_req)) / sum(d(checkpoints_req + checkpoints_timed))` as an
/// amplifier. Confidence is capped at medium.
pub(crate) struct RequestedCheckpointsLens;

impl RequestedCheckpointsLens {
    const ID: &'static str = "PG-CHKPT-008";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: CHECKPOINTER,
            column: "checkpoints_req",
        },
        SectionColumn {
            section: CHECKPOINTER,
            column: "checkpoints_timed",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// More requested than timed checkpoints inverts the healthy ratio.
    const REQUESTED_THRESHOLD: f64 = 0.5;
}

impl Lens for RequestedCheckpointsLens {
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
        for member in &cluster.members {
            if member.logical_section != CHECKPOINTER || member.column != "checkpoints_req" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                CHECKPOINTER,
                &member.identity,
                "checkpoints_req",
                "checkpoints_timed",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            let total = sums.sum_a + sums.sum_b;
            if total <= 0.0 || sums.sum_a / total < Self::REQUESTED_THRESHOLD {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Ratio,
                    formula: "checkpoints_req / (checkpoints_req + checkpoints_timed)",
                    value: sums.sum_a / total,
                    unit: GaugeUnit::Ratio,
                    threshold: Self::REQUESTED_THRESHOLD,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: CHECKPOINTER,
                    operands: vec![
                        CounterOperandSpec {
                            name: "checkpoints_req",
                            value: sums.sum_a,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "checkpoints_timed",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-IO-011` (`backend_io_latency`): slow reads inside `PostgreSQL`. Reports an
/// elevated `sum(d(read_time)) / sum(d(reads))` (milliseconds per read) as an
/// amplifier. `read_time` needs `track_io_timing`; confidence is capped at medium.
pub(crate) struct BackendIoLatencyLens;

impl BackendIoLatencyLens {
    const ID: &'static str = "PG-IO-011";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_IO,
            column: "read_time",
        },
        SectionColumn {
            section: PG_STAT_IO,
            column: "reads",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// One millisecond per read is slow for the buffered-read path.
    const LATENCY_MS_THRESHOLD: f64 = 1.0;
}

impl Lens for BackendIoLatencyLens {
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
        for member in &cluster.members {
            if member.logical_section != PG_STAT_IO || member.column != "read_time" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                PG_STAT_IO,
                &member.identity,
                "read_time",
                "reads",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            if sums.sum_b <= 0.0 || sums.sum_a / sums.sum_b < Self::LATENCY_MS_THRESHOLD {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Ratio,
                    formula: "read_time / reads",
                    value: sums.sum_a / sums.sum_b,
                    unit: GaugeUnit::MillisecondsPerRead,
                    threshold: Self::LATENCY_MS_THRESHOLD,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: PG_STAT_IO,
                    operands: vec![
                        CounterOperandSpec {
                            name: "read_time",
                            value: sums.sum_a,
                            unit: GaugeUnit::Milliseconds,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "reads",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-HOT-007` (`hot_update_failure`): updates that miss the HOT path. Reports an
/// elevated non-HOT fraction
/// `sum(d(n_tup_upd - n_tup_hot_upd)) / sum(d(n_tup_upd))` as an amplifier of
/// index and WAL work. Confidence is capped at medium.
pub(crate) struct HotUpdateFailureLens;

impl HotUpdateFailureLens {
    const ID: &'static str = "PG-HOT-007";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "n_tup_hot_upd",
        },
        SectionColumn {
            section: PG_STAT_USER_TABLES,
            column: "n_tup_upd",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// A majority of updates missing HOT is an index-write amplifier.
    const NON_HOT_THRESHOLD: f64 = 0.5;
}

impl Lens for HotUpdateFailureLens {
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
        for member in &cluster.members {
            if member.logical_section != PG_STAT_USER_TABLES || member.column != "n_tup_upd" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                PG_STAT_USER_TABLES,
                &member.identity,
                "n_tup_hot_upd",
                "n_tup_upd",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            // `sum_a` (HOT) never exceeds `sum_b` (all updates), so the fraction
            // stays in `[0, 1]`.
            if sums.sum_b <= 0.0 || (sums.sum_b - sums.sum_a) / sums.sum_b < Self::NON_HOT_THRESHOLD
            {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Ratio,
                    formula: "(n_tup_upd - n_tup_hot_upd) / n_tup_upd",
                    value: (sums.sum_b - sums.sum_a) / sums.sum_b,
                    unit: GaugeUnit::Ratio,
                    threshold: Self::NON_HOT_THRESHOLD,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: PG_STAT_USER_TABLES,
                    operands: vec![
                        CounterOperandSpec {
                            name: "n_tup_hot_upd",
                            value: sums.sum_a,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "n_tup_upd",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Amplifier,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `PG-ARCH-017` (`wal_archiving_failure`): the archiver rejecting WAL segments.
/// Reports a coincident finding when `failed_count` advanced during the incident,
/// summed over the intervals it shares with the `archived_count` beside it.
/// It publishes the `failed_count` total; confidence is capped at medium.
pub(crate) struct WalArchivingFailureLens;

impl WalArchivingFailureLens {
    const ID: &'static str = "PG-ARCH-017";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: PG_STAT_ARCHIVER,
            column: "failed_count",
        },
        SectionColumn {
            section: PG_STAT_ARCHIVER,
            column: "archived_count",
        },
    ];
    /// One recorded failure between two real snapshots is already actionable.
    const MIN_INTERVALS: usize = 1;
    const MIN_FAILURES: f64 = 1.0;
}

impl Lens for WalArchivingFailureLens {
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
        for member in &cluster.members {
            if member.logical_section != PG_STAT_ARCHIVER || member.column != "failed_count" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                PG_STAT_ARCHIVER,
                &member.identity,
                "failed_count",
                "archived_count",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) || sums.sum_a < Self::MIN_FAILURES
            {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Sum,
                    formula: "failed_count",
                    value: sums.sum_a,
                    unit: GaugeUnit::Count,
                    threshold: Self::MIN_FAILURES,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: PG_STAT_ARCHIVER,
                    operands: vec![
                        CounterOperandSpec {
                            name: "failed_count",
                            value: sums.sum_a,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "archived_count",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::AlignedContext,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `OS-NET-028` (`network_errors`): a network interface logging receive errors.
/// Reports a coincident finding when the receive error ratio
/// `sum(d(rx_errs)) / sum(d(rx_packets))` is elevated. The design confidence is
/// low, so its findings stay capped there.
pub(crate) struct NetworkErrorsLens;

impl NetworkErrorsLens {
    const ID: &'static str = "OS-NET-028";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_NETDEV,
            column: "rx_errs",
        },
        SectionColumn {
            section: OS_NETDEV,
            column: "rx_packets",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// One erroring packet in a hundred is far above a healthy interface.
    const ERROR_THRESHOLD: f64 = 0.01;
}

impl Lens for NetworkErrorsLens {
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
        for member in &cluster.members {
            if member.logical_section != OS_NETDEV || member.column != "rx_errs" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                OS_NETDEV,
                &member.identity,
                "rx_errs",
                "rx_packets",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            if sums.sum_b <= 0.0 || sums.sum_a / sums.sum_b < Self::ERROR_THRESHOLD {
                continue;
            }
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Ratio,
                    formula: "rx_errs / rx_packets",
                    value: sums.sum_a / sums.sum_b,
                    unit: GaugeUnit::Ratio,
                    threshold: Self::ERROR_THRESHOLD,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: OS_NETDEV,
                    operands: vec![
                        CounterOperandSpec {
                            name: "rx_errs",
                            value: sums.sum_a,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "rx_packets",
                            value: sums.sum_b,
                            unit: GaugeUnit::Count,
                            purpose: CounterOperandPurpose::Formula,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}

/// `OS-CGRP-021` (`cgroup_cpu_throttling`): a cgroup denied the CPU it asked for.
/// Reports elevated throttled microseconds per covered second. `usage_usec` is
/// retained as aligned context, not used as a wall-time denominator. As an
/// ordinary metric finding, its role stays coincident.
pub(crate) struct CgroupCpuThrottlingLens;

impl CgroupCpuThrottlingLens {
    const ID: &'static str = "OS-CGRP-021";
    const INPUTS: &'static [SectionColumn] = &[
        SectionColumn {
            section: OS_CGROUP_CPU,
            column: "throttled_usec",
        },
        SectionColumn {
            section: OS_CGROUP_CPU,
            column: "usage_usec",
        },
    ];
    const MIN_INTERVALS: usize = 3;
    /// One hundred milliseconds throttled per covered second is material.
    const THROTTLE_RATE_THRESHOLD_US_PER_S: f64 = 100_000.0;
}

impl Lens for CgroupCpuThrottlingLens {
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
        for member in &cluster.members {
            if member.logical_section != OS_CGROUP_CPU || member.column != "throttled_usec" {
                continue;
            }
            let Some(sums) = paired_sums(
                typed,
                OS_CGROUP_CPU,
                &member.identity,
                "throttled_usec",
                "usage_usec",
                context,
                sink,
            )?
            else {
                continue;
            };
            if !sums.meets_pairing_coverage(Self::MIN_INTERVALS) {
                continue;
            }
            let elapsed_seconds = std::time::Duration::from_micros(sums.elapsed_us).as_secs_f64();
            let throttle_rate = sums.sum_a / elapsed_seconds;
            if throttle_rate < Self::THROTTLE_RATE_THRESHOLD_US_PER_S {
                continue;
            }
            let Some(elapsed_us) = exact_u64_as_f64(sums.elapsed_us) else {
                continue;
            };
            let Some(evidence) = counter_evidence(
                sums,
                context,
                &member.identity,
                CounterEvidenceSpec {
                    kind: CounterMeasurementKind::Rate,
                    formula: "throttled_usec * 1000000 / summed_interval_duration_us",
                    value: throttle_rate,
                    unit: GaugeUnit::MicrosecondsPerSecond,
                    threshold: Self::THROTTLE_RATE_THRESHOLD_US_PER_S,
                    threshold_kind: ThresholdKind::AtLeast,
                    section: OS_CGROUP_CPU,
                    operands: vec![
                        CounterOperandSpec {
                            name: "throttled_usec",
                            value: sums.sum_a,
                            unit: GaugeUnit::Microseconds,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "summed_interval_duration_us",
                            value: elapsed_us,
                            unit: GaugeUnit::Microseconds,
                            purpose: CounterOperandPurpose::Formula,
                        },
                        CounterOperandSpec {
                            name: "usage_usec",
                            value: sums.sum_b,
                            unit: GaugeUnit::Microseconds,
                            purpose: CounterOperandPurpose::AlignedContext,
                        },
                    ],
                },
            ) else {
                continue;
            };
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_episode(member),
                vec![Evidence::CounterAggregate(evidence)],
            ))?;
        }
        Ok(())
    }
}
