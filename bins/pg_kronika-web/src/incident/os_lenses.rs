//! Linux host lenses over bounded counter and gauge evidence.

use std::fmt::Write as _;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::cluster::Cluster;
use super::dispatch::{LimitHit, SectionColumn};
use super::engine::EvalContext;
use super::evidence::sink::FindingSink;
use super::evidence::{
    ConfidenceCap, Evidence, FindingDraft, FindingScope, GaugeEntity, GaugeEvidence, GaugeRatio,
    GaugeUnit, Role, ThresholdKind,
};
use super::lens::Lens;
use super::model::IdentityValue;
use super::series::SeriesSet;
use super::typed::{AlignedSums, TypedInputs};

const OS_CPU: &str = "os_cpu";
const OS_PSI: &str = "os_psi";
const OS_DISKSTATS: &str = "os_diskstats";
const OS_PROCESS: &str = "os_process";

pub(crate) struct HostCpuLens;

impl HostCpuLens {
    const COLUMNS: [&'static str; 8] = [
        "user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal",
    ];
    const MIN_INTERVALS: usize = 3;
    const BUSY_FLOOR: f64 = 0.8;
    const STEAL_FLOOR: f64 = 0.05;

    fn append_psi_evidence(
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
        evidence: &mut Vec<GaugeEvidence>,
    ) -> Result<(), LimitHit> {
        let identity = [IdentityValue::U64(0)];
        let Some(window) = typed.gauge_window(
            OS_PSI,
            "some_avg10",
            &identity,
            context.incident_start_us,
            context.incident_end_us,
        ) else {
            return Ok(());
        };
        sink.charge_points(window.inspected_points())?;
        if let Some(reading) = window.max()
            && (0.0..=100.0).contains(&reading.value)
            && let Some(value) = GaugeEvidence::value(
                reading.value / 100.0,
                GaugeUnit::Ratio,
                0.0,
                ThresholdKind::AtLeast,
                reading.observed_at_us,
                reading.samples,
                GaugeEntity::new(OS_PSI, Arc::from(identity)),
            )
        {
            evidence.push(value);
        }
        Ok(())
    }
}

impl Lens for HostCpuLens {
    fn id(&self) -> &'static str {
        "OS-CPU-020"
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        const INPUTS: &[SectionColumn] = &[
            SectionColumn {
                section: OS_CPU,
                column: "user",
            },
            SectionColumn {
                section: OS_CPU,
                column: "steal",
            },
            SectionColumn {
                section: OS_PSI,
                column: "some_avg10",
            },
        ];
        INPUTS
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
        let Some(member) = cluster
            .members
            .iter()
            .find(|member| member.logical_section == OS_CPU)
        else {
            return Ok(());
        };
        let identity = [IdentityValue::I64(-1)];
        sink.charge_points(typed.aligned_counter_points(OS_CPU, &identity, &Self::COLUMNS))?;
        let Some(sums) = typed.aligned_delta_sums(
            OS_CPU,
            &identity,
            &Self::COLUMNS,
            context.incident_start_us,
            context.incident_end_us,
        ) else {
            return Ok(());
        };
        if sums.intervals < Self::MIN_INTERVALS || sums.sums[..sums.len].iter().any(|v| *v < 0.0) {
            return Ok(());
        }
        // Linux reports guest time inside user/nice already. Guest columns are
        // therefore deliberately absent instead of added a second time.
        let total: f64 = sums.sums[..sums.len].iter().sum();
        let idle = sums.sums[3];
        let iowait = sums.sums[4];
        let steal = sums.sums[7];
        let busy = total - idle - iowait;
        if total <= 0.0 || busy < 0.0 {
            return Ok(());
        }
        let busy_share = busy / total;
        let steal_share = steal / total;
        if !busy_share.is_finite()
            || !steal_share.is_finite()
            || (busy_share < Self::BUSY_FLOOR && steal_share < Self::STEAL_FLOOR)
        {
            return Ok(());
        }
        let entity_identity: Arc<[IdentityValue]> = Arc::from(identity);
        let entity = || GaugeEntity::new(OS_CPU, Arc::clone(&entity_identity));
        let evidence = [
            GaugeEvidence::ratio(
                GaugeRatio::new(busy, total, GaugeUnit::Count),
                Self::BUSY_FLOOR,
                ThresholdKind::AtLeast,
                sums.last_end_us,
                sums.intervals,
                entity(),
            ),
            GaugeEvidence::ratio(
                GaugeRatio::new(steal, total, GaugeUnit::Count),
                Self::STEAL_FLOOR,
                ThresholdKind::AtLeast,
                sums.last_end_us,
                sums.intervals,
                entity(),
            ),
            GaugeEvidence::ratio(
                GaugeRatio::new(iowait, total, GaugeUnit::Count),
                0.0,
                ThresholdKind::AtLeast,
                sums.last_end_us,
                sums.intervals,
                entity(),
            ),
        ]
        .into_iter()
        .collect::<Option<Vec<_>>>();
        let Some(mut evidence) = evidence else {
            return Ok(());
        };
        Self::append_psi_evidence(typed, context, sink, &mut evidence)?;
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_episode(member),
            evidence
                .into_iter()
                .map(Evidence::GaugeObservation)
                .collect(),
            None,
        ))?;
        Ok(())
    }
}

pub(crate) struct BlockDeviceLens;

struct BlockCandidate {
    identity: Arc<[IdentityValue]>,
    sums: AlignedSums,
    operations: f64,
    elapsed_ms: f64,
    score: f64,
}

impl BlockDeviceLens {
    const COLUMNS: [&'static str; 5] = [
        "reads",
        "read_time_ms",
        "writes",
        "write_time_ms",
        "io_weighted_time_ms",
    ];
    const MIN_INTERVALS: usize = 3;
    const MS_PER_OP_FLOOR: f64 = 20.0;
    const AVG_IN_FLIGHT_FLOOR: f64 = 1.0;

    fn device_pair(identity: &[IdentityValue]) -> Option<(i64, i64)> {
        match identity {
            [IdentityValue::I64(major), IdentityValue::I64(minor)] if *major > 0 && *minor >= 0 => {
                Some((*major, *minor))
            }
            _ => None,
        }
    }

    fn best_candidate(
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
    ) -> Result<Option<BlockCandidate>, LimitHit> {
        sink.charge_points(typed.counter_identity_count(OS_DISKSTATS, "reads"))?;
        let mut best: Option<BlockCandidate> = None;
        for identity in typed.counter_identities(OS_DISKSTATS, "reads") {
            sink.charge_points(typed.aligned_counter_points(
                OS_DISKSTATS,
                &identity,
                &Self::COLUMNS,
            ))?;
            let Some(sums) = typed.aligned_delta_sums(
                OS_DISKSTATS,
                &identity,
                &Self::COLUMNS,
                context.incident_start_us,
                context.incident_end_us,
            ) else {
                continue;
            };
            let operations = sums.sums[0] + sums.sums[2];
            let operation_time_ms = sums.sums[1] + sums.sums[3];
            let elapsed_ms =
                std::time::Duration::from_micros(sums.elapsed_us).as_secs_f64() * 1_000.0;
            if sums.intervals < Self::MIN_INTERVALS
                || operations <= 0.0
                || operation_time_ms < 0.0
                || sums.sums[4] < 0.0
                || elapsed_ms <= 0.0
            {
                continue;
            }
            let ms_per_op = operation_time_ms / operations;
            let avg_in_flight = sums.sums[4] / elapsed_ms;
            if !ms_per_op.is_finite()
                || !avg_in_flight.is_finite()
                || (ms_per_op < Self::MS_PER_OP_FLOOR && avg_in_flight < Self::AVG_IN_FLIGHT_FLOOR)
            {
                continue;
            }
            let candidate = BlockCandidate {
                identity,
                sums,
                operations,
                elapsed_ms,
                score: ms_per_op.max(avg_in_flight * Self::MS_PER_OP_FLOOR),
            };
            if best
                .as_ref()
                .is_none_or(|current| candidate.score > current.score)
            {
                best = Some(candidate);
            }
        }
        Ok(best)
    }
}

impl Lens for BlockDeviceLens {
    fn id(&self) -> &'static str {
        "OS-BLOCK-024"
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        const INPUTS: &[SectionColumn] = &[
            SectionColumn {
                section: OS_DISKSTATS,
                column: "read_time_ms",
            },
            SectionColumn {
                section: OS_DISKSTATS,
                column: "write_time_ms",
            },
            SectionColumn {
                section: OS_DISKSTATS,
                column: "io_weighted_time_ms",
            },
        ];
        INPUTS
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
        let Some(member) = cluster
            .members
            .iter()
            .find(|member| member.logical_section == OS_DISKSTATS)
        else {
            return Ok(());
        };
        let Some(candidate) = Self::best_candidate(typed, context, sink)? else {
            return Ok(());
        };
        let BlockCandidate {
            identity,
            sums,
            operations,
            elapsed_ms,
            ..
        } = candidate;
        let Some((major, minor)) = Self::device_pair(&identity) else {
            return Ok(());
        };
        let mapping = if typed.is_postgres_storage_device(major, minor) {
            "postgres_storage_exact"
        } else {
            "postgres_storage_unproven"
        };
        let public_identity: Arc<[IdentityValue]> = Arc::from(vec![
            IdentityValue::Text(mapping.to_owned()),
            IdentityValue::I64(major),
            IdentityValue::I64(minor),
        ]);
        let entity = || GaugeEntity::new(OS_DISKSTATS, Arc::clone(&public_identity));
        let mut evidence = Vec::new();
        if let Some(value) = GaugeEvidence::ratio(
            GaugeRatio::new(
                sums.sums[1] + sums.sums[3],
                operations,
                GaugeUnit::Milliseconds,
            ),
            Self::MS_PER_OP_FLOOR,
            ThresholdKind::AtLeast,
            sums.last_end_us,
            sums.intervals,
            entity(),
        ) {
            evidence.push(Evidence::GaugeObservation(value));
        }
        // weighted_time / wall time is average in-flight I/O, not utilization.
        if let Some(value) = GaugeEvidence::ratio(
            GaugeRatio::new(sums.sums[4], elapsed_ms, GaugeUnit::Milliseconds),
            Self::AVG_IN_FLIGHT_FLOOR,
            ThresholdKind::AtLeast,
            sums.last_end_us,
            sums.intervals,
            entity(),
        ) {
            evidence.push(Evidence::GaugeObservation(value));
        }
        if let Some(window) = typed.gauge_window(
            OS_DISKSTATS,
            "io_in_progress",
            &identity,
            context.incident_start_us,
            context.incident_end_us,
        ) {
            sink.charge_points(window.inspected_points())?;
            if let Some(reading) = window.max()
                && let Some(value) = GaugeEvidence::value(
                    reading.value,
                    GaugeUnit::Count,
                    0.0,
                    ThresholdKind::AtLeast,
                    reading.observed_at_us,
                    reading.samples,
                    entity(),
                )
            {
                evidence.push(Evidence::GaugeObservation(value));
            }
        }
        if evidence.is_empty() {
            return Ok(());
        }
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_episode(member),
            evidence,
            None,
        ))?;
        Ok(())
    }
}

pub(crate) struct ProcessIoWhoLens;

struct ProcessIoCandidate {
    identity: Arc<[IdentityValue]>,
    bytes: f64,
    intervals: usize,
    observed_at_us: i64,
    postgres_backend: bool,
    device: Option<(u64, u64)>,
}

impl ProcessIoWhoLens {
    const COLUMNS: [&'static str; 3] = ["read_bytes", "write_bytes", "cancelled_write_bytes"];
    const FALLBACK_COLUMNS: [&'static str; 2] = ["read_bytes", "write_bytes"];
    const MIN_INTERVALS: usize = 2;
    const MAX_CONTRIBUTORS: usize = 8;

    fn process_identity(identity: &[IdentityValue]) -> Option<(i64, i64)> {
        match identity {
            [IdentityValue::I64(pid), IdentityValue::I64(starttime)]
                if *pid > 0 && *starttime > 0 =>
            {
                Some((*pid, *starttime))
            }
            _ => None,
        }
    }

    fn hash_identity(pid: i64, starttime: i64) -> String {
        let mut hasher = Sha256::new();
        hasher.update(pid.to_be_bytes());
        hasher.update(starttime.to_be_bytes());
        let digest = hasher.finalize();
        let mut out = String::with_capacity(24);
        for byte in &digest[..12] {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    fn associated_device(
        typed: &TypedInputs,
        pid: i64,
        starttime: i64,
        observed_at_us: i64,
        sink: &mut FindingSink<'_>,
    ) -> Result<Option<(u64, u64)>, LimitHit> {
        let Some(cgroup_path) = typed.process_cgroup_at(pid, starttime, observed_at_us) else {
            return Ok(None);
        };
        let devices = typed.cgroup_devices(cgroup_path);
        sink.charge_points(devices.len())?;
        let mut best = None;
        let mut best_bytes = 0.0_f64;
        for identity in devices {
            sink.charge_points(typed.aligned_counter_points(
                "os_cgroup_io",
                identity,
                &["rbytes", "wbytes"],
            ))?;
            let Some(sums) = typed.aligned_delta_sums(
                "os_cgroup_io",
                identity,
                &["rbytes", "wbytes"],
                observed_at_us,
                observed_at_us,
            ) else {
                continue;
            };
            let [
                IdentityValue::Text(_),
                IdentityValue::U64(major),
                IdentityValue::U64(minor),
            ] = identity.as_ref()
            else {
                continue;
            };
            let bytes = sums.sums[0] + sums.sums[1];
            if bytes > best_bytes && bytes.is_finite() {
                best_bytes = bytes;
                best = Some((*major, *minor));
            }
        }
        Ok(best)
    }

    fn candidate(
        typed: &TypedInputs,
        context: &EvalContext,
        sink: &mut FindingSink<'_>,
        identity: Arc<[IdentityValue]>,
    ) -> Result<Option<ProcessIoCandidate>, LimitHit> {
        let Some((pid, starttime)) = Self::process_identity(&identity) else {
            return Ok(None);
        };
        let columns = if typed.has_counter(OS_PROCESS, "cancelled_write_bytes", &identity) {
            &Self::COLUMNS[..]
        } else {
            &Self::FALLBACK_COLUMNS[..]
        };
        sink.charge_points(typed.aligned_counter_points(OS_PROCESS, &identity, columns))?;
        let Some(sums) = typed.aligned_delta_sums(
            OS_PROCESS,
            &identity,
            columns,
            context.incident_start_us,
            context.incident_end_us,
        ) else {
            return Ok(None);
        };
        if sums.intervals < Self::MIN_INTERVALS {
            return Ok(None);
        }
        let cancelled = if sums.len == 3 {
            sums.sums[2].max(0.0)
        } else {
            0.0
        };
        let bytes = sums.sums[0] + (sums.sums[1] - cancelled).max(0.0);
        if bytes <= 0.0 || !bytes.is_finite() {
            return Ok(None);
        }
        let observed_at_us = sums.last_end_us;
        Ok(Some(ProcessIoCandidate {
            identity,
            bytes,
            intervals: sums.intervals,
            observed_at_us,
            postgres_backend: typed.process_is_postgres_backend(
                pid,
                starttime,
                context.incident_start_us,
                context.incident_end_us,
            ),
            device: Self::associated_device(typed, pid, starttime, observed_at_us, sink)?,
        }))
    }
}

impl Lens for ProcessIoWhoLens {
    fn id(&self) -> &'static str {
        "OS-IOWHO-026"
    }

    fn inputs(&self) -> &'static [SectionColumn] {
        const INPUTS: &[SectionColumn] = &[
            SectionColumn {
                section: OS_PROCESS,
                column: "read_bytes",
            },
            SectionColumn {
                section: OS_PROCESS,
                column: "write_bytes",
            },
        ];
        INPUTS
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
        let Some(member) = cluster
            .members
            .iter()
            .find(|member| member.logical_section == OS_PROCESS)
        else {
            return Ok(());
        };
        sink.charge_points(typed.counter_identity_count(OS_PROCESS, "read_bytes"))?;
        let identities = typed.counter_identities(OS_PROCESS, "read_bytes");
        let mut candidates = Vec::new();
        for identity in identities {
            if let Some(candidate) = Self::candidate(typed, context, sink, identity)? {
                candidates.push(candidate);
            }
        }
        candidates.sort_by(|left, right| {
            right
                .bytes
                .total_cmp(&left.bytes)
                .then_with(|| left.identity.cmp(&right.identity))
        });
        candidates.truncate(Self::MAX_CONTRIBUTORS);
        let mut evidence = Vec::new();
        for candidate in candidates {
            let Some((pid, starttime)) = Self::process_identity(&candidate.identity) else {
                continue;
            };
            let category = if candidate.postgres_backend {
                "postgres_backend"
            } else if candidate.device.is_some() {
                "cgroup_device_association"
            } else {
                "process_association"
            };
            let mut public_identity = vec![
                IdentityValue::Text(category.to_owned()),
                IdentityValue::Text(Self::hash_identity(pid, starttime)),
            ];
            if let Some((major, minor)) = candidate.device {
                public_identity.push(IdentityValue::U64(major));
                public_identity.push(IdentityValue::U64(minor));
            }
            if let Some(row) = GaugeEvidence::value(
                candidate.bytes,
                GaugeUnit::Bytes,
                1.0,
                ThresholdKind::AtLeast,
                candidate.observed_at_us,
                candidate.intervals,
                GaugeEntity::new(OS_PROCESS, Arc::from(public_identity)),
            ) {
                evidence.push(Evidence::GaugeObservation(row));
            }
        }
        if evidence.is_empty() {
            return Ok(());
        }
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_episode(member),
            evidence,
            None,
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::model::{EnrichedEpisode, EpisodeRefV1};
    use crate::incident::{ClockRelation, IncidentConfig, analyze};
    use kronika_analytics::{DiffPoint, Direction, Episode, Evaluated, Reason, Scalar};

    fn point(value: f64) -> DiffPoint {
        DiffPoint::Value {
            delta: Scalar::Int(0),
            rate: value,
            dt_micros: 1_000_000,
        }
    }

    fn episode(
        section: &'static str,
        column: &'static str,
        identity: Arc<[IdentityValue]>,
    ) -> EnrichedEpisode {
        EnrichedEpisode {
            episode: Episode {
                start: 0,
                end: 4_000_000,
                peak_ts: 3_000_000,
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
            reference: EpisodeRefV1 {
                logical_section: section,
                column,
                identity,
                start_us: 0,
                end_us: 4_000_000,
            },
        }
    }

    fn outcome(
        lens: &dyn Lens,
        episode: EnrichedEpisode,
        typed: &TypedInputs,
    ) -> crate::incident::EngineOutcome {
        analyze(
            vec![episode],
            &SeriesSet::for_test(0),
            typed,
            &[lens],
            &IncidentConfig::for_test("node", 5, 10_000_000, ClockRelation::Unknown),
        )
        .expect("analysis")
    }

    #[test]
    fn cpu_uses_aggregate_ticks_without_double_counting_guest() {
        let identity: Arc<[IdentityValue]> = Arc::from(vec![IdentityValue::I64(-1)]);
        let mut typed = TypedInputs::new();
        for (column, value) in HostCpuLens::COLUMNS
            .iter()
            .zip([80.0, 0.0, 10.0, 10.0, 0.0, 0.0, 0.0, 0.0])
        {
            typed.insert_counter(
                OS_CPU,
                column,
                Arc::clone(&identity),
                vec![(1, point(value)), (2, point(value)), (3, point(value))],
            );
        }
        typed.insert_gauge(
            OS_PSI,
            "some_avg10",
            Arc::from(vec![IdentityValue::U64(0)]),
            vec![(1, 10.0), (2, 20.0), (3, 30.0)],
        );
        let detected = outcome(
            &HostCpuLens,
            episode(OS_CPU, "user", Arc::clone(&identity)),
            &typed,
        );
        assert_eq!(detected.incidents[0].findings.len(), 1);
        assert_eq!(detected.incidents[0].findings[0].evidence().len(), 4);

        typed.insert_counter(
            OS_CPU,
            "system",
            Arc::clone(&identity),
            vec![
                (1, point(10.0)),
                (
                    2,
                    DiffPoint::NoData {
                        reason: Reason::Reset,
                    },
                ),
                (3, point(10.0)),
            ],
        );
        let reset = outcome(&HostCpuLens, episode(OS_CPU, "user", identity), &typed);
        assert!(reset.incidents[0].findings.is_empty());
    }

    #[test]
    fn disk_pairs_one_device_and_keeps_zero_operations_out() {
        let identity: Arc<[IdentityValue]> =
            Arc::from(vec![IdentityValue::I64(8), IdentityValue::I64(0)]);
        let mut typed = TypedInputs::new();
        for (column, value) in BlockDeviceLens::COLUMNS
            .iter()
            .zip([1.0, 30.0, 0.0, 0.0, 2_000.0])
        {
            typed.insert_counter(
                OS_DISKSTATS,
                column,
                Arc::clone(&identity),
                vec![(1, point(value)), (2, point(value)), (3, point(value))],
            );
        }
        typed.insert_gauge(
            OS_DISKSTATS,
            "io_in_progress",
            Arc::clone(&identity),
            vec![(1, 2.0), (2, 3.0), (3, 1.0)],
        );
        typed.insert_postgres_storage_device(8, 0);
        let detected = outcome(
            &BlockDeviceLens,
            episode(OS_DISKSTATS, "read_time_ms", Arc::clone(&identity)),
            &typed,
        );
        assert_eq!(detected.incidents[0].findings.len(), 1);
        let Evidence::GaugeObservation(first) = &detected.incidents[0].findings[0].evidence()[0]
        else {
            panic!("typed observation");
        };
        assert_eq!(
            first.entity().identity()[0],
            IdentityValue::Text("postgres_storage_exact".to_owned())
        );
        typed.insert_counter(
            OS_DISKSTATS,
            "reads",
            Arc::clone(&identity),
            vec![(1, point(0.0)), (2, point(0.0)), (3, point(0.0))],
        );
        assert!(
            outcome(
                &BlockDeviceLens,
                episode(OS_DISKSTATS, "read_time_ms", identity),
                &typed,
            )
            .incidents[0]
                .findings
                .is_empty()
        );
    }

    #[test]
    fn process_io_keeps_pid_reuse_separate_and_redacts_raw_identity() {
        let first: Arc<[IdentityValue]> =
            Arc::from(vec![IdentityValue::I64(7), IdentityValue::I64(100)]);
        let reused: Arc<[IdentityValue]> =
            Arc::from(vec![IdentityValue::I64(7), IdentityValue::I64(200)]);
        let mut typed = TypedInputs::new();
        for identity in [&first, &reused] {
            for (column, value) in ProcessIoWhoLens::COLUMNS.iter().zip([100.0, 200.0, 0.0]) {
                typed.insert_counter(
                    OS_PROCESS,
                    column,
                    Arc::clone(identity),
                    vec![(1, point(value)), (2, point(value))],
                );
            }
        }
        let result = outcome(
            &ProcessIoWhoLens,
            episode(OS_PROCESS, "read_bytes", Arc::clone(&first)),
            &typed,
        );
        let evidence = result.incidents[0].findings[0].evidence();
        assert_eq!(evidence.len(), 2);
        for row in evidence {
            let Evidence::GaugeObservation(row) = row else {
                panic!("typed observation");
            };
            assert_eq!(row.entity().identity().len(), 2);
            assert!(matches!(row.entity().identity()[1], IdentityValue::Text(_)));
            assert!(!row.entity().identity().contains(&IdentityValue::I64(7)));
        }

        for identity in [&first, &reused] {
            typed.insert_counter(
                OS_PROCESS,
                "cancelled_write_bytes",
                Arc::clone(identity),
                vec![
                    (1, point(0.0)),
                    (
                        2,
                        DiffPoint::NoData {
                            reason: Reason::Reset,
                        },
                    ),
                ],
            );
        }
        let reset = outcome(
            &ProcessIoWhoLens,
            episode(OS_PROCESS, "read_bytes", first),
            &typed,
        );
        assert!(reset.incidents[0].findings.is_empty());
    }
}
