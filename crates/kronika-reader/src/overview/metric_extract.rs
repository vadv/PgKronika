//! Bounded extraction of proven metric/reset/entity mappings.
//!
//! This module is intentionally an allow-list. A registered column becomes an
//! overview sample only when its source type, unit, entity identity and reset
//! family are declared here. Other registry data remains visible in the source
//! manifest and, where it belongs to the target factor inventory, is emitted as
//! explicit unsupported coverage rather than a fabricated zero.

use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
use kronika_analytics::overview::AlignmentId;
use kronika_analytics::overview::{
    Applicability, BoundaryQuality, CadenceEpochId, CounterSample, CoverageSpan, CoverageState,
    EntityKind, EventFact, EventKind, FactorCoverage, FactorId, GaugeSample, LossReason,
    MetricFactor, MetricSeriesDescriptor, MetricSeriesId, MetricUnit, PeriodQuality,
    PhysicalCountSemantics, PopulationTotalQuality, ResetFamily, RetainedExactness,
    SourceCompleteness, SourcePopulation, derive_alignment, derive_entity,
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
const COLLECTION_COVERAGE: u32 = 1_023_001;
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
    pub event_facts: Vec<EventFact>,
    pub pgm_body_read_stats: PgmBodyReadStats,
}

impl MetricExtraction {
    const fn empty(
        descriptor_replacements: Vec<(usize, ManifestEntryDescriptor)>,
        pgm_body_read_stats: PgmBodyReadStats,
    ) -> Self {
        Self {
            descriptor_replacements,
            counter_series: Vec::new(),
            counters: Vec::new(),
            gauge_series: Vec::new(),
            gauges: Vec::new(),
            reset_markers: Vec::new(),
            entity_states: Vec::new(),
            factor_coverage: Vec::new(),
            event_facts: Vec::new(),
            pgm_body_read_stats,
        }
    }
}

struct DecodedMetricSection {
    type_id: u32,
    rows: Vec<Row>,
}

fn decoded_rows_resident_bytes(rows: &[Row], row_capacity: usize) -> Result<u64, BuildError> {
    let row_slots = row_capacity
        .checked_mul(size_of::<Row>())
        .ok_or(BuildError::Overflow)?;
    let bytes = rows.iter().try_fold(row_slots, |total, row| {
        let cell_slots = row
            .cells()
            .len()
            .checked_mul(size_of::<Cell>())
            .ok_or(BuildError::Overflow)?;
        row.cells().iter().try_fold(
            total.checked_add(cell_slots).ok_or(BuildError::Overflow)?,
            |total, cell| {
                let list_bytes = match cell {
                    Cell::ListI32(values) => values
                        .capacity()
                        .checked_mul(size_of::<i32>())
                        .ok_or(BuildError::Overflow)?,
                    _ => 0,
                };
                total.checked_add(list_bytes).ok_or(BuildError::Overflow)
            },
        )
    })?;
    u64::try_from(bytes).map_err(|_error| BuildError::Overflow)
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

    fn pg_database_epoch(self, row_reset_us: Option<i64>, sample_ts_us: i64) -> u64 {
        epoch(&[
            self.postmaster_start_us.unwrap_or(i64::MIN).to_le_bytes(),
            row_reset_us
                .or(self.database_reset_us)
                .unwrap_or_else(|| {
                    if self.postmaster_start_us.is_some() {
                        i64::MIN
                    } else {
                        sample_ts_us
                    }
                })
                .to_le_bytes(),
        ])
    }

    fn os_epoch(self, sample_ts_us: i64) -> u64 {
        epoch(&[
            self.boot_id.unwrap_or(0).to_le_bytes(),
            self.boot_time_us.unwrap_or(sample_ts_us).to_le_bytes(),
        ])
    }

    const fn has_pg_context(self) -> bool {
        self.postmaster_start_us.is_some()
    }

    const fn has_os_context(self) -> bool {
        self.boot_id.is_some() && self.boot_time_us.is_some()
    }
}

#[derive(Debug, Default)]
struct ResetTimeline {
    pg: Vec<(i64, i64, Option<i64>)>,
    os: Vec<(i64, u64, i64)>,
}

impl ResetTimeline {
    fn context_at(&self, ts_us: i64) -> ResetContext {
        let mut context = ResetContext::missing();
        if let Some((_context_ts, postmaster_start_us, database_reset_us)) =
            latest_at(&self.pg, ts_us, |point| point.0)
                .or_else(|| self.pg.first().filter(|point| point.1 <= ts_us))
        {
            context.postmaster_start_us = Some(*postmaster_start_us);
            context.database_reset_us = database_reset_us.filter(|reset| *reset <= ts_us);
        }
        if let Some((_context_ts, boot_id, boot_time_us)) =
            latest_at(&self.os, ts_us, |point| point.0)
                .or_else(|| self.os.first().filter(|point| point.2 <= ts_us))
        {
            context.boot_id = Some(*boot_id);
            context.boot_time_us = Some(*boot_time_us);
        }
        context
    }

    const fn has_pg_context(&self) -> bool {
        !self.pg.is_empty()
    }

    const fn has_os_context(&self) -> bool {
        !self.os.is_empty()
    }
}

fn latest_at<T>(values: &[T], ts_us: i64, timestamp: impl Fn(&T) -> i64) -> Option<&T> {
    values
        .partition_point(|value| timestamp(value) <= ts_us)
        .checked_sub(1)
        .map(|index| &values[index])
}

#[derive(Debug, Clone, Copy)]
struct CoverageRecord {
    ts_us: i64,
    read_state: u8,
    visibility: u8,
    source_total: u64,
    collected: u64,
    total_exact: bool,
}

struct MetricAccumulator {
    item_limit: u64,
    counter_series: BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    counters: Vec<CounterSample>,
    gauge_series: BTreeMap<MetricSeriesId, MetricSeriesDescriptor>,
    gauges: Vec<GaugeSample>,
    entity_states: Vec<EntityStateRecord>,
    factor_sources: BTreeMap<FactorId, BTreeSet<u32>>,
    factor_times: BTreeMap<FactorId, Vec<i64>>,
    factor_losses: BTreeMap<FactorId, BTreeSet<LossReason>>,
}

impl Default for MetricAccumulator {
    fn default() -> Self {
        Self::with_item_limit(u64::MAX)
    }
}

impl MetricAccumulator {
    const fn with_item_limit(item_limit: u64) -> Self {
        Self {
            item_limit,
            counter_series: BTreeMap::new(),
            counters: Vec::new(),
            gauge_series: BTreeMap::new(),
            gauges: Vec::new(),
            entity_states: Vec::new(),
            factor_sources: BTreeMap::new(),
            factor_times: BTreeMap::new(),
            factor_losses: BTreeMap::new(),
        }
    }

    const fn admit_item(&self, current_len: usize) -> Result<(), BuildError> {
        if current_len as u64 >= self.item_limit {
            return Err(BuildError::LimitExceeded);
        }
        Ok(())
    }

    fn register_factor(&mut self, factor: MetricFactor, source_type_id: u32) {
        self.factor_sources
            .entry(factor.id())
            .or_default()
            .insert(source_type_id);
    }

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
        self.admit_item(self.counters.len())?;
        let alignment_id = derive_alignment(descriptor.source_id, descriptor.entity);
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
        self.admit_item(self.gauges.len())?;
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
        population_total: Option<u64>,
    ) -> Result<(), BuildError> {
        if population_total.is_some() {
            self.admit_item(self.entity_states.len())?;
        }
        self.gauge(descriptor, ts_us, f64::from(state_code))?;
        if let Some(population_total) = population_total {
            self.entity_states.push(EntityStateRecord {
                series_id: descriptor.series_id,
                ts_us,
                state_code,
                population_total,
            });
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments are the complete identity and value of one snapshot boundary"
    )]
    fn snapshot_boundary(
        &mut self,
        factor: MetricFactor,
        source_type_id: u32,
        scope: u64,
        entity_kind: EntityKind,
        ts_us: i64,
        population_total: u64,
        postmaster_epoch: u64,
    ) -> Result<(), BuildError> {
        let source_type = source_type_id.to_le_bytes();
        let epoch = postmaster_epoch.to_le_bytes();
        let identity = [
            b"complete-snapshot".as_slice(),
            source_type.as_slice(),
            epoch.as_slice(),
        ]
        .concat();
        let entity = derive_entity(scope, entity_kind, &identity);
        self.state(
            series(
                factor,
                scope,
                source_type_id,
                MetricUnit::StateCode,
                Some(entity),
                None,
                &identity,
            ),
            ts_us,
            0,
            Some(population_total),
        )
    }
}

/// Extracts every allow-listed metric section body exactly once.
#[allow(
    clippy::too_many_lines,
    reason = "the function is the bounded dispatcher for every allow-listed metric section"
)]
pub(super) fn extract_metrics<R: ReadAt>(
    unit: &PgmUnit<R>,
    source_id: u64,
    segment_range: Option<CoverageSpan>,
    bounds: &Bounds,
) -> Result<MetricExtraction, BuildError> {
    if !bounds.is_within_absolute_limits() {
        return Err(BuildError::LimitExceeded);
    }
    let supported_sections = unit
        .catalog()
        .entries
        .iter()
        .filter(|entry| supported_metric_source(entry.type_id))
        .count();
    let mut decoded = Vec::with_capacity(supported_sections);
    let mut descriptor_replacements = Vec::new();
    let mut stats = PgmBodyReadStats::default();
    let mut decoded_rows = 0_u64;
    let mut decoded_bytes = u64::try_from(
        decoded
            .capacity()
            .checked_mul(size_of::<DecodedMetricSection>())
            .ok_or(BuildError::Overflow)?,
    )
    .map_err(|_error| BuildError::Overflow)?;
    for (index, entry) in unit.catalog().entries.iter().enumerate() {
        if !supported_metric_source(entry.type_id) {
            continue;
        }
        decoded_rows = decoded_rows
            .checked_add(u64::from(entry.rows))
            .ok_or(BuildError::Overflow)?;
        if decoded_rows > bounds.items_per_block {
            return Err(BuildError::LimitExceeded);
        }
        let ordinal = u32::try_from(index).map_err(|_error| BuildError::Overflow)?;
        let (descriptor, rows) = unit.decode_overview_rows(ordinal)?;
        decoded_bytes = decoded_bytes
            .checked_add(decoded_rows_resident_bytes(&rows, rows.capacity())?)
            .ok_or(BuildError::Overflow)?;
        if decoded_bytes > bounds.decoded_file_bytes {
            return Err(BuildError::LimitExceeded);
        }
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

    let Some(segment_range) = segment_range else {
        if decoded.iter().any(|section| !section.rows.is_empty()) {
            return Err(BuildError::Source(SourceError::UnsupportedLayout));
        }
        return Ok(MetricExtraction::empty(descriptor_replacements, stats));
    };
    let reset_timeline = reset_timeline(&decoded)?;
    let snapshot_coverage = source_coverage(&decoded)?;
    let mut metrics = MetricAccumulator::with_item_limit(bounds.items_per_block);
    for section in &decoded {
        register_factor_inventory(&mut metrics, section.type_id);
        match section.type_id {
            type_id if PG_STAT_DATABASE_TYPES.contains(&type_id) => {
                extract_pg_database(&mut metrics, section, source_id, &reset_timeline)?;
            }
            OS_CGROUP_MEMORY => {
                extract_cgroup_memory(&mut metrics, section, source_id, &reset_timeline)?;
            }
            OS_VMSTAT => {
                extract_vmstat(&mut metrics, section, source_id, &reset_timeline)?;
            }
            REPLICATION_INSTANCE => {
                extract_replication_instance(&mut metrics, section, source_id)?;
            }
            PG_REPLICATION_PHYSICAL => extract_replication_senders(
                &mut metrics,
                section,
                source_id,
                &snapshot_coverage,
            )?,
            type_id if PG_REPLICATION_SLOT_TYPES.contains(&type_id) => extract_replication_slots(
                &mut metrics,
                section,
                source_id,
                &snapshot_coverage,
            )?,
            type_id if PG_STORAGE_MOUNT_TYPES.contains(&type_id) => {
                extract_storage_mounts(&mut metrics, section, source_id)?;
            }
            PG_PROCESS_CGROUP_MEMORY => {
                extract_process_cgroup_memory(&mut metrics, section, source_id)?;
            }
            RESET_METADATA | INSTANCE_METADATA | COLLECTION_COVERAGE | SNAPSHOT_COVERAGE => {}
            _ => return Err(BuildError::Internal),
        }
    }
    extract_reset_metadata(&mut metrics, &decoded, source_id)?;
    extract_snapshot_boundaries(
        &mut metrics,
        &snapshot_coverage,
        source_id,
        &reset_timeline,
    )?;
    let event_facts = collector_event_facts(&snapshot_coverage, source_id, bounds)?;

    if !reset_timeline.has_pg_context() {
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
    if !reset_timeline.has_os_context() {
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
    let factor_coverage = factor_coverage(&metrics, &snapshot_coverage, segment_range, bounds)?;
    Ok(MetricExtraction {
        descriptor_replacements,
        counter_series: metrics.counter_series.into_values().collect(),
        counters: metrics.counters,
        gauge_series: metrics.gauge_series.into_values().collect(),
        gauges: metrics.gauges,
        reset_markers,
        entity_states: metrics.entity_states,
        factor_coverage,
        event_facts,
        pgm_body_read_stats: stats,
    })
}

fn register_factor_inventory(out: &mut MetricAccumulator, source_type_id: u32) {
    let factors: &[MetricFactor] = if PG_STAT_DATABASE_TYPES.contains(&source_type_id) {
        &[
            MetricFactor::PgDatabaseDeadlocks,
            MetricFactor::PgDatabaseRecoveryConflicts,
            MetricFactor::PgDatabaseChecksumFailures,
            MetricFactor::PgDatabaseSessionsAbandoned,
            MetricFactor::PgDatabaseSessionsFatal,
            MetricFactor::PgDatabaseSessionsKilled,
            MetricFactor::PgDatabaseConnections,
            MetricFactor::PgDatabaseConnectionLimit,
            MetricFactor::PgDatabaseFrozenXidAge,
            MetricFactor::PgDatabaseMinMxidAge,
        ]
    } else if source_type_id == RESET_METADATA {
        &[
            MetricFactor::PgStatisticsResetAt,
            MetricFactor::PgPostmasterStartTime,
        ]
    } else if source_type_id == REPLICATION_INSTANCE {
        &[
            MetricFactor::PgRecoveryRole,
            MetricFactor::PgTimeline,
            MetricFactor::PgReplicationReplayLag,
        ]
    } else if source_type_id == PG_REPLICATION_PHYSICAL {
        &[
            MetricFactor::PgReplicationSenderState,
            MetricFactor::PgReplicationSenderSnapshotPopulation,
        ]
    } else if PG_REPLICATION_SLOT_TYPES.contains(&source_type_id) {
        &[
            MetricFactor::PgReplicationSlotState,
            MetricFactor::PgReplicationSlotSnapshotPopulation,
        ]
    } else if PG_STORAGE_MOUNT_TYPES.contains(&source_type_id) {
        &[
            MetricFactor::PgFilesystemTotalBytes,
            MetricFactor::PgFilesystemAvailableBytes,
        ]
    } else if source_type_id == PG_PROCESS_CGROUP_MEMORY {
        &[
            MetricFactor::OsCgroupMemoryCurrentBytes,
            MetricFactor::OsCgroupMemoryMaxBytes,
        ]
    } else if source_type_id == OS_CGROUP_MEMORY {
        &[
            MetricFactor::OsCgroupMemoryCurrentBytes,
            MetricFactor::OsCgroupMemoryMaxBytes,
            MetricFactor::OsCgroupMemoryHighEvents,
            MetricFactor::OsCgroupMemoryMaxEvents,
            MetricFactor::OsCgroupOomEvents,
            MetricFactor::OsCgroupOomKills,
        ]
    } else if source_type_id == OS_VMSTAT {
        &[MetricFactor::OsHostOomKills]
    } else {
        &[]
    };
    for factor in factors {
        out.register_factor(*factor, source_type_id);
    }
}

fn extract_pg_database(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: u64,
    reset: &ResetTimeline,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let ts = required_ts(row, "ts")?;
        let datid = required_u32(row, "datid")?;
        let entity = derive_entity(scope, EntityKind::Database, &datid.to_le_bytes());
        let context = reset.context_at(ts);
        let epoch = context.pg_database_epoch(optional_ts(row, "stats_reset")?, ts);
        for (factor, field) in [
            (MetricFactor::PgDatabaseDeadlocks, "deadlocks"),
            (MetricFactor::PgDatabaseRecoveryConflicts, "conflicts"),
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
            if !context.has_pg_context() {
                out.factor_losses
                    .entry(factor.id())
                    .or_default()
                    .insert(LossReason::MissingResetContext);
            }
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
    scope: u64,
    reset: &ResetTimeline,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let ts = required_ts(row, "ts")?;
        let context = reset.context_at(ts);
        let path = required_str_id(row, "cgroup_path")?;
        let source_scope = required_u32(row, "scope")?;
        let identity = [
            path.to_le_bytes().as_slice(),
            source_scope.to_le_bytes().as_slice(),
        ]
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
            if !context.has_os_context() {
                out.factor_losses
                    .entry(factor.id())
                    .or_default()
                    .insert(LossReason::MissingResetContext);
            }
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
                context.os_epoch(ts),
            )?;
        }
    }
    Ok(())
}

fn extract_vmstat(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: u64,
    reset: &ResetTimeline,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let Some(value) = optional_i64(row, "oom_kill")? else {
            continue;
        };
        let ts = required_ts(row, "ts")?;
        let context = reset.context_at(ts);
        if !context.has_os_context() {
            out.factor_losses
                .entry(MetricFactor::OsHostOomKills.id())
                .or_default()
                .insert(LossReason::MissingResetContext);
        }
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
            ts,
            value,
            context.os_epoch(ts),
        )?;
    }
    Ok(())
}

fn extract_replication_instance(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: u64,
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
            Some(1),
        )?;
        let timeline = required_i64(row, "timeline_id")?;
        let timeline =
            u32::try_from(timeline).map_err(|_error| BuildError::Source(SourceError::Corrupt))?;
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
            Some(1),
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
                exact_i64_to_f64(micros)?,
            )?;
        }
    }
    Ok(())
}

fn extract_replication_senders(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: u64,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let ts_us = required_ts(row, "ts")?;
        let population = proven_population(section.type_id, ts_us, coverage);
        let pid = required_i64(row, "pid")?;
        let backend_start = required_i64(row, "backend_start_key")?;
        let identity = [
            pid.to_le_bytes().as_slice(),
            backend_start.to_le_bytes().as_slice(),
        ]
        .concat();
        let entity = derive_entity(scope, EntityKind::ReplicationSender, &identity);
        let state_code = required_u32(row, "state_code")?;
        if state_code > 5 {
            return Err(BuildError::Source(SourceError::Corrupt));
        }
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
            ts_us,
            state_code,
            population,
        )?;
    }
    Ok(())
}

fn extract_replication_slots(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: u64,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
) -> Result<(), BuildError> {
    for row in &section.rows {
        let ts_us = required_ts(row, "ts")?;
        let population = proven_population(section.type_id, ts_us, coverage);
        let slot_name = required_str_id(row, "slot_name")?;
        let identity = slot_name.to_le_bytes();
        let entity = derive_entity(scope, EntityKind::ReplicationSlot, &identity);
        let state_code = required_u32(row, "wal_status_code")?;
        if state_code > 4 {
            return Err(BuildError::Source(SourceError::Corrupt));
        }
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
            ts_us,
            state_code,
            population,
        )?;
    }
    Ok(())
}

fn extract_storage_mounts(
    out: &mut MetricAccumulator,
    section: &DecodedMetricSection,
    scope: u64,
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
            (MetricFactor::PgFilesystemAvailableBytes, "available_bytes"),
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
    scope: u64,
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

fn reset_timeline(sections: &[DecodedMetricSection]) -> Result<ResetTimeline, BuildError> {
    let mut pg = BTreeMap::new();
    let mut os = BTreeMap::new();
    for section in sections {
        for row in &section.rows {
            match section.type_id {
                RESET_METADATA => {
                    let ts_us = required_ts(row, "ts")?;
                    let postmaster_start_us = required_ts(row, "postmaster_start_time")?;
                    let database_reset_us = optional_ts(row, "pg_stat_database_reset_max_at")?;
                    if postmaster_start_us > ts_us
                        || database_reset_us.is_some_and(|reset| reset > ts_us)
                    {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                    let point = (postmaster_start_us, database_reset_us);
                    if pg
                        .insert(ts_us, point)
                        .is_some_and(|previous| previous != point)
                    {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                }
                INSTANCE_METADATA => {
                    let ts_us = required_ts(row, "ts")?;
                    let boot_time_us = required_ts(row, "btime")?;
                    if boot_time_us > ts_us {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                    let point = (required_str_id(row, "boot_id")?, boot_time_us);
                    if os
                        .insert(ts_us, point)
                        .is_some_and(|previous| previous != point)
                    {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(ResetTimeline {
        pg: pg
            .into_iter()
            .map(|(ts_us, (postmaster_start_us, database_reset_us))| {
                (ts_us, postmaster_start_us, database_reset_us)
            })
            .collect(),
        os: os
            .into_iter()
            .map(|(ts_us, (boot_id, boot_time_us))| (ts_us, boot_id, boot_time_us))
            .collect(),
    })
}

fn extract_reset_metadata(
    out: &mut MetricAccumulator,
    sections: &[DecodedMetricSection],
    scope: u64,
) -> Result<(), BuildError> {
    let entity = derive_entity(scope, EntityKind::Postmaster, b"instance");
    for section in sections
        .iter()
        .filter(|section| section.type_id == RESET_METADATA)
    {
        for row in &section.rows {
            let ts_us = required_ts(row, "ts")?;
            out.gauge(
                series(
                    MetricFactor::PgPostmasterStartTime,
                    scope,
                    section.type_id,
                    MetricUnit::Microseconds,
                    Some(entity),
                    None,
                    b"postmaster-start",
                ),
                ts_us,
                exact_i64_to_f64(required_ts(row, "postmaster_start_time")?)?,
            )?;
            if let Some(reset_at) = optional_ts(row, "pg_stat_database_reset_max_at")? {
                out.gauge(
                    series(
                        MetricFactor::PgStatisticsResetAt,
                        scope,
                        section.type_id,
                        MetricUnit::Microseconds,
                        Some(entity),
                        None,
                        b"pg-stat-database-reset",
                    ),
                    ts_us,
                    exact_i64_to_f64(reset_at)?,
                )?;
            }
        }
    }
    Ok(())
}

fn extract_snapshot_boundaries(
    out: &mut MetricAccumulator,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
    scope: u64,
    reset: &ResetTimeline,
) -> Result<(), BuildError> {
    let mut seen = BTreeSet::new();
    for (source_type, records) in coverage {
        let (factor, entity_kind) = if *source_type == PG_REPLICATION_PHYSICAL {
            (
                MetricFactor::PgReplicationSenderSnapshotPopulation,
                EntityKind::ReplicationSender,
            )
        } else if PG_REPLICATION_SLOT_TYPES.contains(source_type) {
            (
                MetricFactor::PgReplicationSlotSnapshotPopulation,
                EntityKind::ReplicationSlot,
            )
        } else {
            continue;
        };
        for record in records.iter().filter(|record| complete_snapshot(record)) {
            let Some(postmaster_epoch) = reset
                .context_at(record.ts_us)
                .postmaster_start_us
                .and_then(|value| u64::try_from(value).ok())
            else {
                continue;
            };
            if !seen.insert((*source_type, record.ts_us)) {
                continue;
            }
            out.snapshot_boundary(
                factor,
                *source_type,
                scope,
                entity_kind,
                record.ts_us,
                record.source_total,
                postmaster_epoch,
            )?;
        }
    }
    Ok(())
}

fn collector_event_facts(
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
    scope: u64,
    bounds: &Bounds,
) -> Result<Vec<EventFact>, BuildError> {
    let mut facts = Vec::new();
    for (source_type, records) in coverage {
        for record in records {
            let lost = record.source_total.checked_sub(record.collected);
            let primary = match record.read_state {
                0 if record.collected == record.source_total => None,
                0 | 1 => Some((
                    EventKind::CollectorSnapshotGap,
                    LossReason::SnapshotSourceLimit,
                )),
                2 => Some((
                    EventKind::CollectorVisibilityRestricted,
                    LossReason::PermissionDenied,
                )),
                3 => Some((
                    EventKind::CollectorSourceReadFailure,
                    LossReason::SourceReadFailure,
                )),
                _ => Some((EventKind::CollectorSnapshotGap, LossReason::CollectorLimit)),
            };
            if let Some((kind, reason)) = primary
                && let Some(fact) = EventFact::from_collector_loss(
                    scope,
                    *source_type,
                    record.ts_us,
                    kind,
                    &[reason],
                    lost,
                )
                .map_err(|_error| BuildError::Internal)?
            {
                push_collector_fact(&mut facts, fact, bounds)?;
            }
            if record.visibility != 0
                && !matches!(primary, Some((EventKind::CollectorVisibilityRestricted, _)))
                && let Some(fact) = EventFact::from_collector_loss(
                    scope,
                    *source_type,
                    record.ts_us,
                    EventKind::CollectorVisibilityRestricted,
                    &[LossReason::VisibilityRestricted],
                    lost,
                )
                .map_err(|_error| BuildError::Internal)?
            {
                push_collector_fact(&mut facts, fact, bounds)?;
            }
        }
    }
    facts.sort_by(EventFact::canonical_cmp);
    let mut ids = BTreeSet::new();
    if facts.iter().any(|fact| !ids.insert(fact.fact_id())) {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    Ok(facts)
}

fn push_collector_fact(
    facts: &mut Vec<EventFact>,
    fact: EventFact,
    bounds: &Bounds,
) -> Result<(), BuildError> {
    if facts.len() as u64 == bounds.items_per_block {
        return Err(BuildError::LimitExceeded);
    }
    facts.push(fact);
    Ok(())
}

const fn complete_snapshot(record: &CoverageRecord) -> bool {
    record.read_state == 0
        && record.visibility == 0
        && record.total_exact
        && record.collected == record.source_total
}

fn source_coverage(
    sections: &[DecodedMetricSection],
) -> Result<BTreeMap<u32, Vec<CoverageRecord>>, BuildError> {
    let mut canonical = BTreeMap::<(u32, i64), CoverageRecord>::new();
    for section in sections {
        match section.type_id {
            SNAPSHOT_COVERAGE => {
                for row in &section.rows {
                    let source_type = required_u32(row, "source_type_id")?;
                    let ts_us = required_ts(row, "ts")?;
                    let read_state = required_u32(row, "read_state")?;
                    let visibility = required_u32(row, "visibility")?;
                    let source_total = u64::from(required_u32(row, "source_total")?);
                    let collected = u64::from(required_u32(row, "collected")?);
                    if read_state > 4
                        || visibility > 2
                        || collected > source_total
                        || read_state == 0 && collected != source_total
                    {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                    let record = CoverageRecord {
                        ts_us,
                        read_state: u8::try_from(read_state)
                            .map_err(|_error| BuildError::Source(SourceError::Corrupt))?,
                        visibility: u8::try_from(visibility)
                            .map_err(|_error| BuildError::Source(SourceError::Corrupt))?,
                        source_total,
                        collected,
                        total_exact: true,
                    };
                    insert_coverage(&mut canonical, source_type, record)?;
                }
            }
            COLLECTION_COVERAGE => {
                for row in &section.rows {
                    let reason = required_u32(row, "reason")?;
                    if reason > 3 {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                    let unknown_total = required_bool(row, "unknown_total")?;
                    let source_type = required_u32(row, "source_type_id")?;
                    let source_total = u64::from(required_u32(row, "total")?);
                    let collected = u64::from(required_u32(row, "collected")?);
                    if collected > source_total {
                        return Err(BuildError::Source(SourceError::Corrupt));
                    }
                    let record = CoverageRecord {
                        ts_us: required_ts(row, "ts")?,
                        read_state: match reason {
                            2 => 2,
                            1 | 3 => 3,
                            _ => 1,
                        },
                        visibility: u8::from(reason == 2),
                        source_total,
                        collected,
                        total_exact: !unknown_total,
                    };
                    insert_coverage(&mut canonical, source_type, record)?;
                }
            }
            _ => {}
        }
    }
    let mut coverage: BTreeMap<u32, Vec<CoverageRecord>> = BTreeMap::new();
    for ((source_type, _ts_us), record) in canonical {
        coverage.entry(source_type).or_default().push(record);
    }
    Ok(coverage)
}

fn insert_coverage(
    coverage: &mut BTreeMap<(u32, i64), CoverageRecord>,
    source_type: u32,
    record: CoverageRecord,
) -> Result<(), BuildError> {
    if coverage
        .insert((source_type, record.ts_us), record)
        .is_some()
    {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the function projects the complete canonical factor-coverage record in one bounded pass"
)]
fn factor_coverage(
    metrics: &MetricAccumulator,
    snapshot: &BTreeMap<u32, Vec<CoverageRecord>>,
    interval: CoverageSpan,
    bounds: &Bounds,
) -> Result<Vec<FactorCoverage>, BuildError> {
    let mut factor_ids = metrics
        .factor_sources
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
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
        let mut records = sources
            .iter()
            .flat_map(|source| snapshot.get(source).into_iter().flatten())
            .copied()
            .collect::<Vec<_>>();
        records.sort_unstable_by_key(|record| record.ts_us);
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
            if record.collected < record.source_total {
                losses.insert(LossReason::SnapshotSourceLimit);
            }
        }
        let source_complete = !sources.is_empty()
            && sources.iter().all(|source| {
                snapshot.get(source).is_some_and(|records| {
                    !records.is_empty() && records.iter().all(complete_snapshot)
                })
            });
        let times = metrics
            .factor_times
            .get(&factor_id)
            .cloned()
            .unwrap_or_default();
        let cadence = observed_cadence(&times);
        let present_samples = u64::try_from(times.len()).map_err(|_error| BuildError::Overflow)?;
        let covered_duration = cadence.map_or(0, |(period, _cadence_id)| {
            cadence_covered_duration(&times, interval, period)
        });
        let state = if present_samples == 0 {
            if losses.is_empty() {
                CoverageState::NotCollected
            } else {
                CoverageState::Gap
            }
        } else if cadence.is_none() {
            if losses.is_empty() {
                CoverageState::Unknown
            } else {
                CoverageState::Gap
            }
        } else if covered_duration == interval.duration_us() {
            CoverageState::Complete
        } else if covered_duration == 0 {
            if losses.is_empty() {
                CoverageState::Unknown
            } else {
                CoverageState::Gap
            }
        } else {
            CoverageState::Partial
        };
        let population = (sources.len() == 1)
            .then(|| {
                records.last().map(|record| SourcePopulation {
                    collected: record.collected,
                    total: Some(record.source_total),
                    total_quality: if record.total_exact {
                        PopulationTotalQuality::Exact
                    } else {
                        PopulationTotalQuality::LowerBound
                    },
                })
            })
            .flatten();
        let lost_count_lower_bound = records
            .iter()
            .map(|record| record.source_total.saturating_sub(record.collected))
            .max()
            .filter(|lost| *lost != 0);
        let retained_exact = losses.is_empty();
        result.push(FactorCoverage {
            factor_id,
            applicability: Applicability::Applicable,
            state,
            interval,
            expected_period_us: cadence.map(|(period, _)| period),
            period_quality: cadence
                .map_or(PeriodQuality::Unknown, |_| PeriodQuality::ObservedStable),
            cadence_epoch_id: cadence.map(|(_, id)| id),
            crosses_cadence_boundary: false,
            present_samples,
            covered_duration_us: covered_duration,
            source_population: population,
            loss_reasons: losses.into_iter().collect(),
            lost_count_lower_bound,
            retained_exactness: if retained_exact {
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
            boundary_quality: BoundaryQuality::Contained,
        });
    }
    Ok(result)
}

pub(super) fn cadence_covered_duration(
    times: &[i64],
    interval: CoverageSpan,
    period_us: u64,
) -> u64 {
    let period = i64::try_from(period_us).unwrap_or(i64::MAX);
    let mut times = times.to_vec();
    times.sort_unstable();
    times.dedup();
    let mut covered = 0_u64;
    let mut covered_through = interval.start_us();
    for ts_us in times {
        let start = ts_us.max(interval.start_us()).max(covered_through);
        let end = ts_us.saturating_add(period).min(interval.end_us());
        if end <= start {
            continue;
        }
        covered = covered.saturating_add(
            u64::try_from(end - start).expect("positive timestamp duration fits u64"),
        );
        covered_through = covered_through.max(end);
    }
    covered.min(interval.duration_us())
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

pub(super) fn observed_cadence(times: &[i64]) -> Option<(u64, CadenceEpochId)> {
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
    ts_us: i64,
    coverage: &BTreeMap<u32, Vec<CoverageRecord>>,
) -> Option<u64> {
    coverage
        .get(&source_type)
        .and_then(|records| records.iter().find(|record| record.ts_us == ts_us))
        .filter(|record| complete_snapshot(record))
        .map(|record| record.source_total)
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
    source_id: u64,
    source_type_id: u32,
    unit: MetricUnit,
    entity: Option<kronika_analytics::overview::EntityRef>,
    reset_family: Option<ResetFamily>,
    discriminator: &[u8],
) -> MetricSeriesDescriptor {
    MetricSeriesDescriptor::new(
        factor,
        source_id,
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
                | COLLECTION_COVERAGE
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
        Some(Cell::I64(value)) => exact_i64_to_f64(*value).map(Some),
        Some(Cell::U32(value)) => Ok(Some(f64::from(*value))),
        Some(Cell::U64(value)) => exact_u64_to_f64(*value).map(Some),
        Some(_) => Err(BuildError::Source(SourceError::Corrupt)),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the explicit 53-bit range check guarantees exact IEEE-754 integer conversion"
)]
const fn exact_i64_to_f64(value: i64) -> Result<f64, BuildError> {
    const EXACT_INTEGER_LIMIT: u64 = 1_u64 << 53;
    if value.unsigned_abs() <= EXACT_INTEGER_LIMIT {
        Ok(value as f64)
    } else {
        Err(BuildError::Source(SourceError::Corrupt))
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the explicit 53-bit range check guarantees exact IEEE-754 integer conversion"
)]
const fn exact_u64_to_f64(value: u64) -> Result<f64, BuildError> {
    const EXACT_INTEGER_LIMIT: u64 = 1_u64 << 53;
    if value <= EXACT_INTEGER_LIMIT {
        Ok(value as f64)
    } else {
        Err(BuildError::Source(SourceError::Corrupt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage_for(
        factor: MetricFactor,
        sources: &[u32],
        times: &[i64],
        snapshot: &BTreeMap<u32, Vec<CoverageRecord>>,
        interval: CoverageSpan,
    ) -> FactorCoverage {
        let mut metrics = MetricAccumulator::default();
        metrics
            .factor_sources
            .insert(factor.id(), sources.iter().copied().collect());
        metrics.factor_times.insert(factor.id(), times.to_vec());
        factor_coverage(&metrics, snapshot, interval, &super::super::limits::LIMIT)
            .expect("coverage")
            .into_iter()
            .find(|coverage| coverage.factor_id == factor.id())
            .expect("requested factor")
    }

    const fn complete_record(ts_us: i64, population: u64) -> CoverageRecord {
        CoverageRecord {
            ts_us,
            read_state: 0,
            visibility: 0,
            source_total: population,
            collected: population,
            total_exact: true,
        }
    }

    #[test]
    fn context_falls_forward_only_when_the_epoch_predates_the_sample() {
        let timeline = ResetTimeline {
            pg: vec![(200, 50, Some(150))],
            os: vec![(200, 7, 40)],
        };
        let context = timeline.context_at(100);
        assert_eq!(context.postmaster_start_us, Some(50));
        assert_eq!(context.database_reset_us, None);
        assert_eq!(context.boot_id, Some(7));
        assert_eq!(context.boot_time_us, Some(40));

        let restarted = ResetTimeline {
            pg: vec![(200, 150, None)],
            os: vec![(200, 8, 150)],
        }
        .context_at(100);
        assert!(!restarted.has_pg_context());
        assert!(!restarted.has_os_context());
    }

    #[test]
    fn expanded_metric_samples_stop_at_the_output_bound() {
        let scope = 1;
        let descriptor = series(
            MetricFactor::PgDatabaseConnections,
            scope,
            PG_STAT_DATABASE_TYPES[0],
            MetricUnit::Connections,
            None,
            None,
            b"bounded",
        );
        let mut metrics = MetricAccumulator::with_item_limit(1);
        assert_eq!(metrics.gauge(descriptor, 10, 1.0), Ok(()));
        assert_eq!(
            metrics.gauge(descriptor, 20, 2.0),
            Err(BuildError::LimitExceeded)
        );
        assert_eq!(metrics.gauges.len(), 1);
    }

    #[test]
    fn integer_gauges_reject_values_outside_the_exact_f64_range() {
        const EXACT_LIMIT: u64 = 1_u64 << 53;
        const EXACT_LIMIT_I64: i64 = 9_007_199_254_740_992;
        const EXACT_LIMIT_F64: f64 = 9_007_199_254_740_992.0;
        assert_eq!(exact_i64_to_f64(-EXACT_LIMIT_I64), Ok(-EXACT_LIMIT_F64));
        assert_eq!(exact_u64_to_f64(EXACT_LIMIT), Ok(EXACT_LIMIT_F64));
        assert_eq!(
            exact_i64_to_f64(i64::try_from(EXACT_LIMIT + 1).expect("fits i64")),
            Err(BuildError::Source(SourceError::Corrupt))
        );
        assert_eq!(
            exact_u64_to_f64(EXACT_LIMIT + 1),
            Err(BuildError::Source(SourceError::Corrupt))
        );
    }

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
        let coverage = unsupported_coverage(MetricFactor::CpuPressureUnsupported.id(), interval);
        assert_eq!(coverage.applicability, Applicability::Unsupported);
        assert_eq!(coverage.state, CoverageState::NotCollected);
        assert_eq!(coverage.present_samples, 0);
    }

    #[test]
    fn one_snapshot_is_unknown_without_inventing_a_historical_period() {
        let interval = CoverageSpan::new(10, 21).expect("interval");
        let coverage = coverage_for(
            MetricFactor::PgDatabaseDeadlocks,
            &[PG_STAT_DATABASE_TYPES[0]],
            &[10],
            &BTreeMap::from([(PG_STAT_DATABASE_TYPES[0], vec![complete_record(10, 1)])]),
            interval,
        );

        assert_eq!(coverage.state, CoverageState::Unknown);
        assert_eq!(coverage.expected_period_us, None);
        assert_eq!(coverage.period_quality, PeriodQuality::Unknown);
        assert_eq!(coverage.present_samples, 1);
        assert_eq!(coverage.covered_duration_us, 0);
        assert_eq!(coverage.source_completeness, SourceCompleteness::Full);
    }

    #[test]
    fn stable_samples_cover_time_independently_from_population_completeness() {
        let source = PG_STAT_DATABASE_TYPES[0];
        let interval = CoverageSpan::new(10, 21).expect("interval");
        let coverage = coverage_for(
            MetricFactor::PgDatabaseDeadlocks,
            &[source],
            &[10, 20],
            &BTreeMap::from([(source, vec![complete_record(10, 1), complete_record(20, 1)])]),
            interval,
        );

        assert_eq!(coverage.state, CoverageState::Complete);
        assert_eq!(coverage.expected_period_us, Some(10));
        assert_eq!(coverage.period_quality, PeriodQuality::ObservedStable);
        assert_eq!(coverage.present_samples, 2);
        assert_eq!(coverage.covered_duration_us, interval.duration_us());
        assert_eq!(coverage.source_completeness, SourceCompleteness::Full);
        assert_eq!(
            coverage.source_population,
            Some(SourcePopulation {
                collected: 1,
                total: Some(1),
                total_quality: PopulationTotalQuality::Exact,
            })
        );
    }

    #[test]
    fn every_contributing_source_must_prove_its_population() {
        let first = PG_PROCESS_CGROUP_MEMORY;
        let second = OS_CGROUP_MEMORY;
        let interval = CoverageSpan::new(10, 21).expect("interval");
        let coverage = coverage_for(
            MetricFactor::OsCgroupMemoryCurrentBytes,
            &[first, second],
            &[10, 20],
            &BTreeMap::from([(first, vec![complete_record(10, 1)])]),
            interval,
        );

        assert_eq!(coverage.state, CoverageState::Complete);
        assert_eq!(
            coverage.source_completeness,
            SourceCompleteness::BoundedSubset
        );
        assert_eq!(
            coverage.source_population, None,
            "populations from unlike source families must not be added together"
        );
    }

    #[test]
    fn missing_population_members_have_a_nonzero_proven_loss_bound() {
        let source = PG_STAT_DATABASE_TYPES[0];
        let interval = CoverageSpan::new(10, 21).expect("interval");
        let coverage = coverage_for(
            MetricFactor::PgDatabaseDeadlocks,
            &[source],
            &[10, 20],
            &BTreeMap::from([(
                source,
                vec![
                    CoverageRecord {
                        collected: 2,
                        source_total: 5,
                        ..complete_record(10, 5)
                    },
                    CoverageRecord {
                        collected: 4,
                        source_total: 5,
                        ..complete_record(20, 5)
                    },
                ],
            )]),
            interval,
        );

        assert_eq!(coverage.lost_count_lower_bound, Some(3));
        assert_eq!(coverage.loss_reasons, vec![LossReason::SnapshotSourceLimit]);
        assert_eq!(
            coverage.source_completeness,
            SourceCompleteness::BoundedSubset
        );
    }
}
