//! Bounded extraction of proven metric/reset/entity mappings.
//!
//! This module is intentionally an allow-list. A registered column becomes an
//! overview sample only when its source type, unit, entity identity and reset
//! family are declared here. Other registry data remains visible in the source
//! manifest and, where it belongs to the target factor inventory, is emitted as
//! explicit unsupported coverage rather than a fabricated zero.

use std::collections::{BTreeMap, BTreeSet};

use kronika_analytics::overview::{
    AlignmentId, Applicability, BoundaryQuality, CadenceEpochId, CounterSample, CoverageSpan,
    CoverageState, EntityKind, FactorCoverage, FactorId, GaugeSample, LossReason, MetricFactor,
    MetricSeriesDescriptor, MetricSeriesId, MetricUnit, PeriodQuality, PhysicalCountSemantics,
    PopulationTotalQuality, ResetFamily, RetainedExactness, SourceCompleteness,
    SourcePopulation, SourceScopeId, derive_alignment, derive_entity,
};
use kronika_format::ReadAt;
use kronika_registry::{Cell, Row};
use sha2::{Digest as _, Sha256};

use super::block::{EntityStateRecord, ResetMarker};
use super::descriptors::ManifestEntryDescriptor;
use super::facts::{BuildError, SourceError};
use super::limits::Bounds;
use crate::{PgmBodyReadStats, PgmUnit};

const PG_STAT_DATABASE_TYPES: [u32; 4] = [1_005_001, 1_005_002, 1_005_003, 1_005_004];
const REPLICATION_INSTANCE: u32 = 1_015_001;
const RESET_METADATA: u32 = 1_020_001;
const INSTANCE_METADATA: u32 = 1_021_001;
const PG_REPLICATION_PHYSICAL: u32 = 1_033_001;
const PG_REPLICATION_SLOT_TYPES: [u32; 3] = [1_034_001, 1_034_002, 1_034_003];
const PG_STORAGE_MOUNT_TYPES: [u32; 2] = [1_036_001, 1_036_002];
const PG_PROCESS_CGROUP_MEMORY: u32 = 1_037_001;
const SNAPSHOT_COVERAGE: u32 = 1_038_001;
const OS_VMSTAT: u32 = 1_106_001;
const OS_CGROUP_MEMORY: u32 = 1_202_001;

/// Typed metric materialization from one PGM unit.
pub(super) struct MetricExtraction {
    pub descriptor_replacements: Vec<(usize, ManifestEntryDescriptor)>,
    pub counter_series: Vec<MetricSeriesDescriptor>,
    pub counters: Vec<CounterSample>,
    pub gauge_series: Vec<MetricSeriesDescriptor>,
    pub gauges: Vec<GaugeSample>,
    pub reset_markers: Vec<ResetMarker>,
    pub entity_states: Vec<EntityStateRecord>,
    pub factor_coverage: Vec<FactorCoverage>,
    pub pgm_body_read_stats: PgmBodyReadStats,
}

struct DecodedMetricSection {
    type_id: u32,
    rows: Vec<Row>,
}

#[derive(Debug, Clone, Copy)]
struct ResetContext {
    postmaster_start_us: Option<i64>,
    database_reset_us: Option<i64>,
    boot_id: Option<u64>,
    boot_time_us: Option<i64>,
}

impl ResetContext {
    const fn missing() -> Self {
        Self {
            postmaster_start_us: None,
            database_reset_us: None,
            boot_id: None,
            boot_time_us: None,
        }
    }

    fn pg_database_epoch(self, row_reset_us: Option<i64>) -> u64 {
        epoch(&[
            self.postmaster_start_us.unwrap_or(i64::MIN).to_le_bytes(),
            row_reset_us
                .or(self.database_reset_us)
                .unwrap_or(i64::MIN)
                .to_le_bytes(),
        ])
    }

    fn os_epoch(self) -> u64 {
        epoch(&[
            self.boot_id.unwrap_or(0).to_le_bytes(),
            self.boot_time_us.unwrap_or(i64::MIN).to_le_bytes(),
        ])
    }

    const fn has_pg_context(self) -> bool {
        self.postmaster_start_us.is_some()
    }

    const fn has_os_context(self) -> bool {
        self.boot_id.is_some() && self.boot_time_us.is_some()
    }
}

#[derive(Debug, Clone, Copy)]
struct CoverageRecord {
    read_state: u8,
    visibility: u8,
    source_total: u64,
    collected: u64,
}

#[derive(Default)]
struct MetricAccumulator {
    counter_series: BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    counters: Vec<CounterSample>,
    gauge_series: BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    gauges: Vec<GaugeSample>,
    entity_states: Vec<EntityStateRecord>,
    factor_sources: BTreeMap<FactorId, BTreeSet<u32>>,
    factor_times: BTreeMap<FactorId, Vec<i64>>,
    factor_losses: BTreeMap<FactorId, BTreeSet<LossReason>>,
}

impl MetricAccumulator {
    fn counter(
        &mut self,
        descriptor: MetricSeriesDescriptor,
        ts_us: i64,
        value: i64,
        reset_epoch: u64,
    ) -> Result<(), BuildError> {
        self.factor_sources
            .entry(descriptor.factor_id)
            .or_default()
            .insert(descriptor.source_type_id);
        if value < 0 {
            self.factor_losses
                .entry(descriptor.factor_id)
                .or_default()
                .insert(LossReason::InvalidCounterValue);
            return Ok(());
        }
        let alignment_id = derive_alignment(descriptor.source_scope_id, descriptor.entity);
        self.counters.push(CounterSample::new(
            descriptor.series_id,
            alignment_id,
            ts_us,
            u64::try_from(value).map_err(|_error| BuildError::Overflow)?,
            reset_epoch,
        ));
        self.factor_times
            .entry(descriptor.factor_id)
            .or_default()
            .push(ts_us);
        insert_descriptor(&mut self.counter_series, descriptor)
    }

    fn gauge(
        &mut self,
        descriptor: MetricSeriesDescriptor,
        ts_us: i64,
        value: f64,
    ) -> Result<(), BuildError> {
        self.factor_sources
            .entry(descriptor.factor_id)
            .or_default()
            .insert(descriptor.source_type_id);
        let Some(sample) = GaugeSample::new(descriptor.series_id, ts_us, value) else {
            return Err(BuildError::Source(SourceError::Corrupt));
        };
        self.gauges.push(sample);
        self.factor_times
            .entry(descriptor.factor_id)
            .or_default()
            .push(ts_us);
        insert_descriptor(&mut self.gauge_series, descriptor)
    }

    fn state(
        &mut self,
        descriptor: MetricSeriesDescriptor,
        ts_us: i64,
        state_code: u32,
        population_total: u64,
    ) -> Result<(), BuildError> {
        self.gauge(descriptor, ts_us, f64::from(state_code))?;
        self.entity_states.push(EntityStateRecord {
            series_id: descriptor.series_id,
            ts_us,
            state_code,
            population_total,
        });
        Ok(())
    }
}

/// Extracts every allow-listed metric section body exactly once.
pub(super) fn extract_metrics<R: ReadAt>(
    unit: &PgmUnit<R>,
    source_scope_id: SourceScopeId,
    segment_range: CoverageSpan,
    bounds: &Bounds,
) -> Result<MetricExtraction, BuildError> {
    let mut decoded = Vec::new();
    let mut descriptor_replacements = Vec::new();
    let mut stats = PgmBodyReadStats::default();
    for (index, entry) in unit.catalog().entries.iter().enumerate() {
        if !supported_metric_source(entry.type_id) {
            continue;
        }
        let ordinal = u32::try_from(index).map_err(|_error| BuildError::Overflow)?;
        let (descriptor, rows) = unit.decode_overview_rows(ordinal)?;
        descriptor_replacements.push((index, descriptor));
        stats.read_calls = stats
            .read_calls
            .checked_add(1)
            .ok_or(BuildError::Overflow)?;
        stats.stored_bytes_read = stats
            .stored_bytes_read
            .checked_add(entry.len)
            .ok_or(BuildError::Overflow)?;
        decoded.push(DecodedMetricSection {
            type_id: entry.type_id,
            rows,
        });
    }

    let reset_context = reset_context(&decoded)?;
    let snapshot_coverage = snapshot_coverage(&decoded)?;
    let mut metrics = MetricAccumulator::default();
    for section in &decoded {
        match section.type_id {
            type_id if PG_STAT_DATABASE_TYPES.contains(&type_id) => extract_pg_database(
                &mut metrics,
                section,
                source_scope_id,
                reset_context,
            )?,
            OS_CGROUP_MEMORY => extract_cgroup_memory(
                &mut metrics,
                section,
                source_scope_id,
                reset_context,
            )?,
            OS_VMSTAT => {
                extract_vmstat(&mut metrics, section, source_scope_id, reset_context)?;
            }
            REPLICATION_INSTANCE => {
                extract_replication_instance(&mut metrics, section, source_scope_id)?;
            }
            PG_REPLICATION_PHYSICAL => extract_replication_senders(
                &mut metrics,
                section,
                source_scope_id,
                &snapshot_coverage,
            )?,
            type_id if PG_REPLICATION_SLOT_TYPES.contains(&type_id) => extract_replication_slots(
                &mut metrics,
                section,
                source_scope_id,
                &snapshot_coverage,
            )?,
            type_id if PG_STORAGE_MOUNT_TYPES.contains(&type_id) => {
                extract_storage_mounts(&mut metrics, section, source_scope_id)?;
            }
            PG_PROCESS_CGROUP_MEMORY => {
                extract_process_cgroup_memory(&mut metrics, section, source_scope_id)?;
            }
            RESET_METADATA | INSTANCE_METADATA | SNAPSHOT_COVERAGE => {}
            _ => return Err(BuildError::Internal),
        }
    }

    if !reset_context.has_pg_context() {
        for factor in [
            MetricFactor::PgDatabaseDeadlocks,
            MetricFactor::PgDatabaseRecoveryConflicts,
            MetricFactor::PgDatabaseChecksumFailures,
            MetricFactor::PgDatabaseSessionsAbandoned,
            MetricFactor::PgDatabaseSessionsFatal,
            MetricFactor::PgDatabaseSessionsKilled,
        ] {
            metrics
                .factor_losses
                .entry(factor.id())
                .or_default()
                .insert(LossReason::MissingResetContext);
        }
    }
    if !reset_context.has_os_context() {
        for factor in [
            MetricFactor::OsCgroupMemoryHighEvents,
            MetricFactor::OsCgroupMemoryMaxEvents,
            MetricFactor::OsCgroupOomEvents,
            MetricFactor::OsCgroupOomKills,
            MetricFactor::OsHostOomKills,
        ] {
            metrics
                .factor_losses
                .entry(factor.id())
                .or_default()
                .insert(LossReason::MissingResetContext);
        }
    }

    metrics.counters.sort_unstable_by_key(|sample| {
        (
            sample.series_id().0,
            sample.alignment_id().0,
            sample.ts_us(),
        )
    });
    metrics
        .gauges
        .sort_unstable_by_key(|sample| (sample.series_id().0, sample.ts_us()));
    metrics
        .entity_states
        .sort_unstable_by_key(|state| (state.series_id.0, state.ts_us));
    let reset_markers = reset_markers(&metrics.counters);
    let factor_coverage = factor_coverage(
        &metrics,
        &snapshot_coverage,
        segment_range,
        bounds,
    )?;
    Ok(MetricExtraction {
        descriptor_replacements,
        counter_series: metrics.counter_series.into_values().collect(),
        counters: metrics.counters,
        gauge_series: metrics.gauge_series.into_values().collect(),
        gauges: metrics.gauges,
        reset_markers,
        entity_states: metrics.entity_states,
        factor_coverage,
        pgm_body_read_stats: stats,
    })
}

fn extract_pg_database(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
    reset: ResetContext,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let ts = required_ts(row, "ts")?;
        let datid = required_u32(row, "datid")?;
        let entity = derive_entity(scope, EntityKind::Database, &datid.to_le_bytes());
        let epoch = reset.pg_database_epoch(optional_ts(row, "stats_reset")?);
        for (factor, field) in [
            (MetricFactor::PgDatabaseDeadlocks, "deadlocks"),
            (
                MetricFactor::PgDatabaseRecoveryConflicts,
                "conflicts",
            ),
            (
                MetricFactor::PgDatabaseChecksumFailures,
                "checksum_failures",
            ),
            (
                MetricFactor::PgDatabaseSessionsAbandoned,
                "sessions_abandoned",
            ),
            (MetricFactor::PgDatabaseSessionsFatal, "sessions_fatal"),
            (MetricFactor::PgDatabaseSessionsKilled, "sessions_killed"),
        ] {
            let Some(value) = optional_i64(row, field)? else {
                continue;
            };
            let descriptor = series(
                factor,
                scope,
                section.type_id,
                MetricUnit::Count,
                Some(entity),
                Some(ResetFamily::PgStatDatabase),
                &datid.to_le_bytes(),
            );
            out.counter(descriptor, ts, value, epoch)?;
        }
        for (factor, field, unit) in [
            (
                MetricFactor::PgDatabaseConnections,
                "numbackends",
                MetricUnit::Connections,
            ),
            (
                MetricFactor::PgDatabaseConnectionLimit,
                "datconnlimit",
                MetricUnit::Connections,
            ),
            (
                MetricFactor::PgDatabaseFrozenXidAge,
                "frozen_xid_age",
                MetricUnit::Transactions,
            ),
            (
                MetricFactor::PgDatabaseMinMxidAge,
                "min_mxid_age",
                MetricUnit::Multixacts,
            ),
        ] {
            if let Some(value) = optional_f64(row, field)? {
                out.gauge(
                    series(
                        factor,
                        scope,
                        section.type_id,
                        unit,
                        Some(entity),
                        None,
                        &datid.to_le_bytes(),
                    ),
                    ts,
                    value,
                )?;
            }
        }
    }
    Ok(())
}

fn extract_cgroup_memory(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
    reset: ResetContext,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let ts = required_ts(row, "ts")?;
        let path = required_str_id(row, "cgroup_path")?;
        let source_scope = required_u32(row, "scope")?;
        let identity = [path.to_le_bytes().as_slice(), source_scope.to_le_bytes().as_slice()]
            .concat();
        let entity = derive_entity(scope, EntityKind::Cgroup, &identity);
        for (factor, field) in [
            (MetricFactor::OsCgroupMemoryCurrentBytes, "current"),
            (MetricFactor::OsCgroupMemoryMaxBytes, "max"),
        ] {
            if let Some(value) = optional_f64(row, field)? {
                out.gauge(
                    series(
                        factor,
                        scope,
                        section.type_id,
                        MetricUnit::Bytes,
                        Some(entity),
                        None,
                        &identity,
                    ),
                    ts,
                    value,
                )?;
            }
        }
        for (factor, field) in [
            (MetricFactor::OsCgroupMemoryHighEvents, "high_events"),
            (MetricFactor::OsCgroupMemoryMaxEvents, "max_events"),
            (MetricFactor::OsCgroupOomEvents, "oom_events"),
            (MetricFactor::OsCgroupOomKills, "oom_kill"),
        ] {
            let value = required_i64(row, field)?;
            out.counter(
                series(
                    factor,
                    scope,
                    section.type_id,
                    MetricUnit::Count,
                    Some(entity),
                    Some(ResetFamily::CgroupBoot),
                    &identity,
                ),
                ts,
                value,
                reset.os_epoch(),
            )?;
        }
    }
    Ok(())
}

fn extract_vmstat(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
    reset: ResetContext,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let Some(value) = optional_i64(row, "oom_kill")? else {
            continue;
        };
        let source_scope = required_u32(row, "scope")?;
        let identity = source_scope.to_le_bytes();
        let entity = derive_entity(scope, EntityKind::Host, &identity);
        out.counter(
            series(
                MetricFactor::OsHostOomKills,
                scope,
                section.type_id,
                MetricUnit::Count,
                Some(entity),
                Some(ResetFamily::HostBoot),
                &identity,
            ),
            required_ts(row, "ts")?,
            value,
            reset.os_epoch(),
        )?;
    }
    Ok(())
}

fn extract_replication_instance(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
) -> Result<(), BuildError> {
    let entity = derive_entity(scope, EntityKind::Postmaster, b"instance");
    for row in &section.rows {
        let ts = required_ts(row, "ts")?;
        out.state(
            series(
                MetricFactor::PgRecoveryRole,
                scope,
                section.type_id,
                MetricUnit::StateCode,
                Some(entity),
                None,
                b"instance",
            ),
            ts,
            u32::from(required_bool(row, "is_in_recovery")?),
            1,
        )?;
        let timeline = required_i64(row, "timeline_id")?;
        let timeline = u32::try_from(timeline).map_err(|_error| BuildError::Source(SourceError::Corrupt))?;
        out.state(
            series(
                MetricFactor::PgTimeline,
                scope,
                section.type_id,
                MetricUnit::StateCode,
                Some(entity),
                None,
                b"instance",
            ),
            ts,
            timeline,
            1,
        )?;
        if let Some(seconds) = optional_i64(row, "replay_lag_s")? {
            let micros = seconds.checked_mul(1_000_000).ok_or(BuildError::Overflow)?;
            out.gauge(
                series(
                    MetricFactor::PgReplicationReplayLag,
                    scope,
                    section.type_id,
                    MetricUnit::Microseconds,
                    Some(entity),
                    None,
                    b"instance",
                ),
                ts,
                micros as f64,
            )?;
        }
    }
    Ok(())
}

fn extract_replication_senders(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
) -> Result<(), BuildError> {
    let population = proven_population(section.type_id, section.rows.len(), coverage);
    for row in &section.rows {
        let pid = required_i64(row, "pid")?;
        let backend_start = required_i64(row, "backend_start_key")?;
        let identity = [pid.to_le_bytes().as_slice(), backend_start.to_le_bytes().as_slice()]
            .concat();
        let entity = derive_entity(scope, EntityKind::ReplicationSender, &identity);
        let state_code = required_u32(row, "state_code")?;
        out.state(
            series(
                MetricFactor::PgReplicationSenderState,
                scope,
                section.type_id,
                MetricUnit::StateCode,
                Some(entity),
                None,
                &identity,
            ),
            required_ts(row, "ts")?,
            state_code,
            population,
        )?;
    }
    Ok(())
}

fn extract_replication_slots(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
) -> Result<(), BuildError> {
    let population = proven_population(section.type_id, section.rows.len(), coverage);
    for row in &section.rows {
        let slot_name = required_str_id(row, "slot_name")?;
        let identity = slot_name.to_le_bytes();
        let entity = derive_entity(scope, EntityKind::ReplicationSlot, &identity);
        out.state(
            series(
                MetricFactor::PgReplicationSlotState,
                scope,
                section.type_id,
                MetricUnit::StateCode,
                Some(entity),
                None,
                &identity,
            ),
            required_ts(row, "ts")?,
            required_u32(row, "wal_status_code")?,
            population,
        )?;
    }
    Ok(())
}

fn extract_storage_mounts(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
) -> Result<(), BuildError> {
    for row in &section.rows {
        if required_u32(row, "mapping_state")? != 1 {
            continue;
        }
        let mut identity = Vec::with_capacity(41);
        for field in [
            "role",
            "path_hash_hi",
            "path_hash_lo",
            "mount_hash_hi",
            "mount_hash_lo",
            "mount_namespace",
        ] {
            identity.extend_from_slice(&required_u64(row, field)?.to_le_bytes());
        }
        let entity = derive_entity(scope, EntityKind::Filesystem, &identity);
        let ts = required_ts(row, "ts")?;
        for (factor, field) in [
            (MetricFactor::PgFilesystemTotalBytes, "total_bytes"),
            (
                MetricFactor::PgFilesystemAvailableBytes,
                "available_bytes",
            ),
        ] {
            if let Some(value) = optional_f64(row, field)? {
                if value < 0.0 {
                    continue;
                }
                out.gauge(
                    series(
                        factor,
                        scope,
                        section.type_id,
                        MetricUnit::Bytes,
                        Some(entity),
                        None,
                        &identity,
                    ),
                    ts,
                    value,
                )?;
            }
        }
    }
    Ok(())
}

fn extract_process_cgroup_memory(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: SourceScopeId,
) -> Result<(), BuildError> {
    for row in &section.rows {
        if required_u32(row, "mapping_state")? != 1 {
            continue;
        }
        let mut identity = Vec::with_capacity(40);
        for field in ["cgroup_hash_hi", "cgroup_hash_lo", "hierarchy"] {
            identity.extend_from_slice(&required_u64(row, field)?.to_le_bytes());
        }
        let entity = derive_entity(scope, EntityKind::Cgroup, &identity);
        let ts = required_ts(row, "ts")?;
        for (factor, field) in [
            (MetricFactor::OsCgroupMemoryCurrentBytes, "current_bytes"),
            (MetricFactor::OsCgroupMemoryMaxBytes, "max_bytes"),
        ] {
            if let Some(value) = optional_f64(row, field)? {
                out.gauge(
                    series(
                        factor,
                        scope,
                        section.type_id,
                        MetricUnit::Bytes,
                        Some(entity),
                        None,
                        &identity,
                    ),
                    ts,
                    value,
                )?;
            }
        }
    }
    Ok(())
}

fn reset_context(sections: &[DecodedMetricSection]) -> Result<ResetContext, BuildError> {
    let mut context = ResetContext::missing();
    for section in sections {
        for row in &section.rows {
            match section.type_id {
                RESET_METADATA => {
                    context.postmaster_start_us =
                        Some(required_ts(row, "postmaster_start_time")?);
                    context.database_reset_us =
                        optional_ts(row, "pg_stat_database_reset_max_at")?;
                }
                INSTANCE_METADATA => {
                    context.boot_id = Some(required_str_id(row, "boot_id")?);
                    context.boot_time_us = Some(required_ts(row, "btime")?);
                }
                _ => {}
            }
        }
    }
    Ok(context)
}

fn snapshot_coverage(
    sections: &[DecodedMetricSection],
) -> Result<BTreeMap<u32, Vec<CoverageRecord>>, BuildError> {
    let mut coverage: BTreeMap<u32, Vec<CoverageRecord>> = BTreeMap::new();
    for section in sections {
        if section.type_id != SNAPSHOT_COVERAGE {
            continue;
        }
        for row in &section.rows {
            coverage
                .entry(required_u32(row, "source_type_id")?)
                .or_default()
                .push(CoverageRecord {
                    read_state: u8::try_from(required_u32(row, "read_state")?)
                        .map_err(|_error| BuildError::Source(SourceError::Corrupt))?,
                    visibility: u8::try_from(required_u32(row, "visibility")?)
                        .map_err(|_error| BuildError::Source(SourceError::Corrupt))?,
                    source_total: u64::from(required_u32(row, "source_total")?),
                    collected: u64::from(required_u32(row, "collected")?),
                });
        }
    }
    Ok(coverage)
}

fn factor_coverage(
    metrics: &MetricAccumulator,
    snapshot: &BTreeMap<u32, Vec<CoverageRecord>>,
    interval: CoverageSpan,
    bounds: &Bounds,
) -> Result<Vec<FactorCoverage>, BuildError> {
    let mut factor_ids = metrics.factor_sources.keys().copied().collect::<BTreeSet<_>>();
    for factor in [
        MetricFactor::CpuPressureUnsupported,
        MetricFactor::MemoryPsiUnsupported,
        MetricFactor::StorageThroughputUnsupported,
        MetricFactor::BlockedSessionsUnsupported,
    ] {
        factor_ids.insert(factor.id());
    }
    if factor_ids.len() as u64 > bounds.items_per_block {
        return Err(BuildError::LimitExceeded);
    }
    let mut result = Vec::with_capacity(factor_ids.len());
    for factor_id in factor_ids {
        let explicit_unsupported = MetricFactor::from_id(factor_id).is_some_and(|factor| {
            matches!(
                factor,
                MetricFactor::CpuPressureUnsupported
                    | MetricFactor::MemoryPsiUnsupported
                    | MetricFactor::StorageThroughputUnsupported
                    | MetricFactor::BlockedSessionsUnsupported
            )
        });
        if explicit_unsupported {
            result.push(unsupported_coverage(factor_id, interval));
            continue;
        }
        let mut losses = metrics
            .factor_losses
            .get(&factor_id)
            .cloned()
            .unwrap_or_default();
        let sources = metrics
            .factor_sources
            .get(&factor_id)
            .cloned()
            .unwrap_or_default();
        let records = sources
            .iter()
            .flat_map(|source| snapshot.get(source).into_iter().flatten())
            .copied()
            .collect::<Vec<_>>();
        for record in &records {
            match record.read_state {
                0 => {}
                1 => {
                    losses.insert(LossReason::SnapshotSourceLimit);
                }
                2 => {
                    losses.insert(LossReason::PermissionDenied);
                }
                3 => {
                    losses.insert(LossReason::SourceReadFailure);
                }
                _ => {
                    losses.insert(LossReason::CollectorLimit);
                }
            }
            if record.visibility != 0 {
                losses.insert(LossReason::VisibilityRestricted);
            }
        }
        let source_complete = !records.is_empty()
            && records
                .iter()
                .all(|record| record.read_state == 0 && record.visibility == 0);
        let times = metrics
            .factor_times
            .get(&factor_id)
            .cloned()
            .unwrap_or_default();
        let cadence = observed_cadence(&times);
        let present_samples =
            u64::try_from(times.len()).map_err(|_error| BuildError::Overflow)?;
        let covered_duration = if present_samples == 0 {
            0
        } else if source_complete {
            interval.duration_us()
        } else {
            present_samples.min(interval.duration_us())
        };
        let state = if present_samples == 0 {
            CoverageState::NotCollected
        } else if source_complete {
            CoverageState::Complete
        } else if covered_duration < interval.duration_us() {
            CoverageState::Partial
        } else {
            CoverageState::Gap
        };
        let population = records.last().map(|record| SourcePopulation {
            collected: record.collected,
            total: Some(record.source_total),
            total_quality: PopulationTotalQuality::Exact,
        });
        result.push(FactorCoverage {
            factor_id,
            applicability: Applicability::Applicable,
            state,
            interval,
            expected_period_us: cadence.map(|(period, _)| period),
            period_quality: cadence.map_or(PeriodQuality::Unknown, |_| {
                PeriodQuality::ObservedStable
            }),
            cadence_epoch_id: cadence.map(|(_, id)| id),
            crosses_cadence_boundary: false,
            present_samples,
            covered_duration_us: covered_duration,
            source_population: population,
            loss_reasons: losses.into_iter().collect(),
            lost_count_lower_bound: None,
            retained_exactness: if losses_is_empty(metrics, factor_id) {
                RetainedExactness::Exact
            } else {
                RetainedExactness::Unknown
            },
            source_completeness: if source_complete {
                SourceCompleteness::Full
            } else if records.is_empty() {
                SourceCompleteness::Unknown
            } else {
                SourceCompleteness::BoundedSubset
            },
            physical_count_semantics: PhysicalCountSemantics::NotApplicable,
            boundary_quality: BoundaryQuality::Exact,
        });
    }
    Ok(result)
}

fn losses_is_empty(metrics: &MetricAccumulator, factor_id: FactorId) -> bool {
    metrics
        .factor_losses
        .get(&factor_id)
        .is_none_or(BTreeSet::is_empty)
}

const fn unsupported_coverage(factor_id: FactorId, interval: CoverageSpan) -> FactorCoverage {
    FactorCoverage {
        factor_id,
        applicability: Applicability::Unsupported,
        state: CoverageState::NotCollected,
        interval,
        expected_period_us: None,
        period_quality: PeriodQuality::Unknown,
        cadence_epoch_id: None,
        crosses_cadence_boundary: false,
        present_samples: 0,
        covered_duration_us: 0,
        source_population: None,
        loss_reasons: Vec::new(),
        lost_count_lower_bound: None,
        retained_exactness: RetainedExactness::Unknown,
        source_completeness: SourceCompleteness::Unknown,
        physical_count_semantics: PhysicalCountSemantics::NotApplicable,
        boundary_quality: BoundaryQuality::Unknown,
    }
}

fn observed_cadence(times: &[i64]) -> Option<(u64, CadenceEpochId)> {
    let mut times = times.to_vec();
    times.sort_unstable();
    times.dedup();
    let mut deltas = times
        .windows(2)
        .map(|pair| u64::try_from(pair[1].checked_sub(pair[0])?).ok())
        .collect::<Option<Vec<_>>>()?;
    let period = deltas.pop()?;
    if period == 0 || deltas.iter().any(|delta| *delta != period) {
        return None;
    }
    let digest = Sha256::digest(period.to_le_bytes());
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    Some((period, CadenceEpochId(id)))
}

fn reset_markers(samples: &[CounterSample]) -> Vec<ResetMarker> {
    let mut markers = Vec::new();
    let mut previous: Option<(MetricSeriesId, u64)> = None;
    for sample in samples {
        let identity = (sample.series_id(), sample.reset_epoch());
        if previous != Some(identity) {
            markers.push(ResetMarker {
                series_id: sample.series_id(),
                ts_us: sample.ts_us(),
                reset_epoch: sample.reset_epoch(),
            });
            previous = Some(identity);
        }
    }
    markers
}

fn proven_population(
    source_type: u32,
    retained: usize,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
) -> u64 {
    coverage
        .get(&source_type)
        .and_then(|records| records.last())
        .filter(|record| record.read_state == 0 && record.visibility == 0)
        .map_or_else(
            || u64::try_from(retained).unwrap_or(u64::MAX),
            |record| record.source_total,
        )
}

fn insert_descriptor(
    destination: &mut BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    descriptor: MetricSeriesDescriptor,
) -> Result<(), BuildError> {
    match destination.get(&descriptor.series_id) {
        Some(existing) if *existing != descriptor => Err(BuildError::Internal),
        Some(_) => Ok(()),
        None => {
            destination.insert(descriptor.series_id, descriptor);
            Ok(())
        }
    }
}

fn series(
    factor: MetricFactor,
    source_scope_id: SourceScopeId,
    source_type_id: u32,
    unit: MetricUnit,
    entity: Option<kronika_analytics::overview::EntityRef>,
    reset_family: Option<ResetFamily>,
    discriminator: &[u8],
) -> MetricSeriesDescriptor {
    MetricSeriesDescriptor::new(
        factor,
        source_scope_id,
        source_type_id,
        unit,
        entity,
        reset_family,
        discriminator,
    )
}

fn supported_metric_source(type_id: u32) -> bool {
    PG_STAT_DATABASE_TYPES.contains(&type_id)
        || PG_REPLICATION_SLOT_TYPES.contains(&type_id)
        || PG_STORAGE_MOUNT_TYPES.contains(&type_id)
        || matches!(
            type_id,
            REPLICATION_INSTANCE
                | RESET_METADATA
                | INSTANCE_METADATA
                | PG_REPLICATION_PHYSICAL
                | PG_PROCESS_CGROUP_MEMORY
                | SNAPSHOT_COVERAGE
                | OS_VMSTAT
                | OS_CGROUP_MEMORY
        )
}

fn epoch<const N: usize>(parts: &[[u8; N]]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"pgk-overview-reset-epoch-v1");
    for part in parts {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 has eight bytes"))
}

fn cell<'a>(row: &'a Row, field: &str) -> Result<&'a Cell, BuildError> {
    row.get(field)
        .ok_or(BuildError::Source(SourceError::UnsupportedLayout))
}

fn required_ts(row: &Row, field: &str) -> Result<i64, BuildError> {
    match cell(row, field)? {
        Cell::Ts(value) => Ok(*value),
        _ => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn optional_ts(row: &Row, field: &str) -> Result<Option<i64>, BuildError> {
    match row.get(field) {
        None | Some(Cell::Null) => Ok(None),
        Some(Cell::Ts(value)) => Ok(Some(*value)),
        Some(_) => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn required_bool(row: &Row, field: &str) -> Result<bool, BuildError> {
    match cell(row, field)? {
        Cell::Bool(value) => Ok(*value),
        _ => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn required_str_id(row: &Row, field: &str) -> Result<u64, BuildError> {
    match cell(row, field)? {
        Cell::StrId(value) => Ok(*value),
        _ => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn required_u32(row: &Row, field: &str) -> Result<u32, BuildError> {
    match cell(row, field)? {
        Cell::U32(value) => Ok(*value),
        Cell::I16(value) => {
            u32::try_from(*value).map_err(|_error| BuildError::Source(SourceError::Corrupt))
        }
        Cell::I32(value) => {
            u32::try_from(*value).map_err(|_error| BuildError::Source(SourceError::Corrupt))
        }
        Cell::I64(value) => {
            u32::try_from(*value).map_err(|_error| BuildError::Source(SourceError::Corrupt))
        }
        _ => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn required_u64(row: &Row, field: &str) -> Result<u64, BuildError> {
    match cell(row, field)? {
        Cell::U64(value) => Ok(*value),
        Cell::U32(value) => Ok(u64::from(*value)),
        Cell::I16(value) => {
            u64::try_from(*value).map_err(|_error| BuildError::Source(SourceError::Corrupt))
        }
        Cell::I32(value) => {
            u64::try_from(*value).map_err(|_error| BuildError::Source(SourceError::Corrupt))
        }
        Cell::I64(value) => {
            u64::try_from(*value).map_err(|_error| BuildError::Source(SourceError::Corrupt))
        }
        _ => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn required_i64(row: &Row, field: &str) -> Result<i64, BuildError> {
    optional_i64(row, field)?.ok_or(BuildError::Source(SourceError::Corrupt))
}

fn optional_i64(row: &Row, field: &str) -> Result<Option<i64>, BuildError> {
    match row.get(field) {
        None | Some(Cell::Null) => Ok(None),
        Some(Cell::I16(value)) => Ok(Some(i64::from(*value))),
        Some(Cell::I32(value)) => Ok(Some(i64::from(*value))),
        Some(Cell::I64(value)) => Ok(Some(*value)),
        Some(Cell::U32(value)) => Ok(Some(i64::from(*value))),
        Some(Cell::U64(value)) => i64::try_from(*value)
            .map(Some)
            .map_err(|_error| BuildError::Source(SourceError::Corrupt)),
        Some(_) => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

fn optional_f64(row: &Row, field: &str) -> Result<Option<f64>, BuildError> {
    match row.get(field) {
        None | Some(Cell::Null) => Ok(None),
        Some(Cell::F64(value)) if value.is_finite() => Ok(Some(*value)),
        Some(Cell::I16(value)) => Ok(Some(f64::from(*value))),
        Some(Cell::I32(value)) => Ok(Some(f64::from(*value))),
        Some(Cell::I64(value)) => Ok(Some(*value as f64)),
        Some(Cell::U32(value)) => Ok(Some(f64::from(*value))),
        Some(Cell::U64(value)) => Ok(Some(*value as f64)),
        Some(_) => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_cadence_requires_equal_positive_intervals() {
        assert_eq!(observed_cadence(&[10]), None);
        assert_eq!(observed_cadence(&[10, 20, 31]), None);
        assert_eq!(
            observed_cadence(&[30, 10, 20]).map(|(period, _)| period),
            Some(10)
        );
    }

    #[test]
    fn reset_markers_include_first_epoch_and_each_change() {
        let series = MetricSeriesId([1; 16]);
        let alignment = AlignmentId([2; 16]);
        let samples = vec![
            CounterSample::new(series, alignment, 10, 1, 7),
            CounterSample::new(series, alignment, 20, 2, 7),
            CounterSample::new(series, alignment, 30, 1, 8),
        ];
        let markers = reset_markers(&samples);
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].ts_us, 10);
        assert_eq!(markers[1].reset_epoch, 8);
    }

    #[test]
    fn unsupported_factor_coverage_is_explicit() {
        let interval = CoverageSpan::new(0, 10).expect("interval");
        let coverage =
            unsupported_coverage(MetricFactor::CpuPressureUnsupported.id(), interval);
        assert_eq!(coverage.applicability, Applicability::Unsupported);
        assert_eq!(coverage.state, CoverageState::NotCollected);
        assert_eq!(coverage.present_samples, 0);
    }
}
