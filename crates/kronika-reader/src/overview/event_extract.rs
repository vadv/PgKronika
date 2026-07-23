//! Bounded extraction of registered log-event rows into canonical observations.

use kronika_analytics::overview::{
    CheckpointPayload, Coverage, CoverageSpan, DictionaryContextId, DroppedFieldCount,
    ErrorCategory, ErrorGroupPayload, EventObservation, EvidenceQuality, FiniteF64,
    LifecyclePayload, LockWaitPayload, LogGapPayload, LossReason, LossSummary, MaintenancePayload,
    ObservationPayload, ObservationProvenance, ObservationShape, ObservationTime, QualityFlags,
    SectionBodyId, SegmentIdentity, SegmentLocator, Severity, SlowQueryPayload, SqlState,
    TempFilePayload, TimeQuality,
};
use kronika_format::ReadAt;
use kronika_registry::{Cell, Row, Semantics, registry};
use std::collections::{BTreeMap, BTreeSet};

use super::descriptors::{DictionaryContextEntry, ManifestEntryDescriptor, dictionary_context_id};
use super::dictionary::ResolvedPattern;
use super::facts::{BuildError, SourceError};
use super::limits::Bounds;
use crate::{PgmBodyReadStats, PgmUnit};

const ERROR_TYPE_ID: u32 = 1_022_001;
const CHECKPOINT_TYPE_ID: u32 = 1_024_001;
const AUTOVACUUM_TYPE_ID: u32 = 1_025_001;
const SLOW_QUERY_TYPE_ID: u32 = 1_026_001;
const LOCK_WAIT_TYPE_ID: u32 = 1_027_001;
const LIFECYCLE_TYPE_ID: u32 = 1_028_001;
const LOG_GAP_TYPE_ID: u32 = 1_029_001;
const TEMP_FILE_TYPE_ID: u32 = 1_030_001;
const SUPPORTED_EVENT_TYPE_IDS: [u32; 8] = [
    ERROR_TYPE_ID,
    CHECKPOINT_TYPE_ID,
    AUTOVACUUM_TYPE_ID,
    SLOW_QUERY_TYPE_ID,
    LOCK_WAIT_TYPE_ID,
    LIFECYCLE_TYPE_ID,
    LOG_GAP_TYPE_ID,
    TEMP_FILE_TYPE_ID,
];

/// The log-gap reason whose presence forces a segment-wide timestamp fallback.
///
/// It is monotone across a full segment on a cold rebuild but only per-part in a
/// live fold, so a live promotion consults it to decide whether the folded times
/// would still match the rebuild.
pub(super) const TIMESTAMP_FALLBACK_GAP_REASON: u8 = 15;

pub(super) struct EventExtraction {
    pub manifest_entries: Vec<ManifestEntryDescriptor>,
    pub observations: Vec<EventObservation>,
    pub known_gaps: Coverage,
    pub dropped_lower_bound: u64,
    pub pgm_body_read_stats: PgmBodyReadStats,
    pub retained_text_bytes: u64,
    pub dictionary_fingerprints: Vec<DictionaryFingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DictionaryFingerprint {
    pub str_id: u64,
    pub context_id: DictionaryContextId,
}

struct PendingObservation {
    type_id: u32,
    catalog_entry_ordinal: u32,
    row_ordinal: u32,
    section_body_id: SectionBodyId,
    row: Row,
}

struct TextBudget {
    limit: u64,
    remaining: u64,
}

impl TextBudget {
    const fn new(bounds: &Bounds) -> Self {
        Self {
            limit: bounds.decoded_block_len,
            remaining: bounds.decoded_block_len,
        }
    }

    fn retain(&mut self, bytes: &[u8]) -> Result<Box<str>, BuildError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or(BuildError::LimitExceeded)?;
        std::str::from_utf8(bytes)
            .map(Box::from)
            .map_err(|_error| BuildError::Source(SourceError::Corrupt))
    }

    const fn retained(&self) -> u64 {
        self.limit - self.remaining
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "catalog admission, bounded decoding, and provenance assembly share one extraction state"
)]
pub(super) fn extract_events<R: ReadAt>(
    unit: &PgmUnit<R>,
    lineage: SegmentIdentity,
    segment_locator: Option<SegmentLocator>,
    bounds: &Bounds,
) -> Result<EventExtraction, BuildError> {
    if !bounds.is_within_absolute_limits()
        || unit.catalog().entries.len() as u64 > u64::from(bounds.directory_entries)
    {
        return Err(BuildError::LimitExceeded);
    }

    let mut manifest_entries = Vec::with_capacity(unit.catalog().entries.len());
    let mut pending = Vec::new();
    let mut pending_bytes = 0_u64;
    let mut wanted = BTreeSet::new();
    let mut pgm_body_read_stats = PgmBodyReadStats::default();

    for (index, entry) in unit.catalog().entries.iter().enumerate() {
        if !is_registered_event_stream(entry.type_id) {
            manifest_entries.push(ManifestEntryDescriptor::from_catalog(entry));
            continue;
        }
        if !is_supported_event_section(entry.type_id) {
            return Err(BuildError::Source(SourceError::UnsupportedLayout));
        }

        let next_count = (pending.len() as u64)
            .checked_add(u64::from(entry.rows))
            .ok_or(BuildError::Overflow)?;
        if next_count > bounds.items_per_block {
            return Err(BuildError::LimitExceeded);
        }
        let ordinal = u32::try_from(index).map_err(|_error| BuildError::Overflow)?;
        let (descriptor, rows) = unit.decode_overview_rows(ordinal)?;
        pgm_body_read_stats.read_calls = pgm_body_read_stats
            .read_calls
            .checked_add(1)
            .ok_or(BuildError::Overflow)?;
        pgm_body_read_stats.stored_bytes_read = pgm_body_read_stats
            .stored_bytes_read
            .checked_add(entry.len)
            .ok_or(BuildError::Overflow)?;
        let section_body_id = descriptor
            .section_body_id
            .ok_or(BuildError::Source(SourceError::Corrupt))?;
        manifest_entries.push(descriptor);

        for (row_index, row) in rows.into_iter().enumerate() {
            validate_row_contract(entry.type_id, &row)?;
            pending_bytes = pending_bytes
                .checked_add(estimated_row_bytes(&row)?)
                .ok_or(BuildError::Overflow)?;
            if pending_bytes > bounds.decoded_block_len {
                return Err(BuildError::LimitExceeded);
            }
            for cell in row.cells() {
                if let Cell::StrId(str_id) = cell {
                    wanted.insert(*str_id);
                }
            }
            let wanted_bytes = (wanted.len() as u64)
                .checked_mul(64)
                .ok_or(BuildError::Overflow)?;
            if wanted.len() as u64 > bounds.items_per_block
                || pending_bytes
                    .checked_add(wanted_bytes)
                    .is_none_or(|bytes| bytes > bounds.decoded_file_bytes)
            {
                return Err(BuildError::LimitExceeded);
            }
            pending.push(PendingObservation {
                type_id: entry.type_id,
                catalog_entry_ordinal: ordinal,
                row_ordinal: u32::try_from(row_index).map_err(|_error| BuildError::Overflow)?,
                section_body_id,
                row,
            });
        }
    }

    let dictionary = unit.resolve_overview_dictionary(&wanted, bounds)?;
    if dictionary.values.len() != wanted.len() {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    let dictionary_fingerprints = fingerprint_dictionary(&dictionary.values)?;
    pgm_body_read_stats.read_calls = pgm_body_read_stats
        .read_calls
        .checked_add(dictionary.stats.sections_read)
        .ok_or(BuildError::Overflow)?;
    pgm_body_read_stats.stored_bytes_read = pgm_body_read_stats
        .stored_bytes_read
        .checked_add(dictionary.stats.stored_bytes_read)
        .ok_or(BuildError::Overflow)?;
    let mut timestamp_fallback = false;
    for pending in &pending {
        if pending.type_id == LOG_GAP_TYPE_ID
            && cell_u32(&pending.row, "reason")? == u32::from(TIMESTAMP_FALLBACK_GAP_REASON)
        {
            timestamp_fallback = true;
        }
    }

    let mut observations = Vec::with_capacity(pending.len());
    let mut known_gap_spans = Vec::new();
    let mut dropped_lower_bound = 0_u64;
    let mut text_budget = TextBudget::new(bounds);
    for pending in pending {
        let row_has_truncated_value = row_has_truncated_value(&pending.row, &dictionary.values)?;
        let provenance = ObservationProvenance {
            segment_locator,
            section_body_id: pending.section_body_id,
            catalog_entry_ordinal: pending.catalog_entry_ordinal,
            row_ordinal: pending.row_ordinal,
            dictionary_context_id: row_context_id(&pending.row, &dictionary.values)?,
            source_locator: None,
        };
        let observation = observation_from_row(
            pending.type_id,
            &pending.row,
            lineage,
            provenance,
            &dictionary.values,
            row_has_truncated_value,
            timestamp_fallback,
            &mut text_budget,
        )?;
        if let ObservationPayload::LogGap(gap) = observation.payload() {
            let interval = observation
                .time()
                .observed_interval
                .ok_or(BuildError::Internal)?;
            known_gap_spans.push(interval);
            let lost_lines = gap_dropped_lower_bound(gap)?;
            dropped_lower_bound = dropped_lower_bound
                .checked_add(lost_lines)
                .ok_or(BuildError::Overflow)?;
        }
        observations.push(observation);
    }
    observations.sort_by(EventObservation::canonical_cmp);
    if observations
        .windows(2)
        .any(|pair| pair[0].observation_id() == pair[1].observation_id())
    {
        return Err(BuildError::Source(SourceError::Corrupt));
    }

    Ok(EventExtraction {
        manifest_entries,
        observations,
        known_gaps: Coverage::from_spans(known_gap_spans),
        dropped_lower_bound,
        pgm_body_read_stats,
        retained_text_bytes: text_budget.retained(),
        dictionary_fingerprints,
    })
}

pub(super) fn fingerprint_dictionary(
    dictionary: &BTreeMap<u64, ResolvedPattern>,
) -> Result<Vec<DictionaryFingerprint>, BuildError> {
    dictionary
        .iter()
        .map(|(str_id, value)| {
            let (bytes, full_len, truncated) = resolved_parts(value);
            let entry = DictionaryContextEntry {
                str_id: *str_id,
                bytes,
                full_len,
                truncated,
            };
            let context_id =
                dictionary_context_id(&[entry]).ok_or(BuildError::Source(SourceError::Corrupt))?;
            Ok(DictionaryFingerprint {
                str_id: *str_id,
                context_id,
            })
        })
        .collect()
}

fn estimated_row_bytes(row: &Row) -> Result<u64, BuildError> {
    let fixed = size_of::<PendingObservation>()
        .checked_add(
            row.cells()
                .len()
                .checked_mul(size_of::<Cell>())
                .ok_or(BuildError::Overflow)?,
        )
        .ok_or(BuildError::Overflow)?;
    let lists = row.cells().iter().try_fold(0_usize, |total, cell| {
        let values = match cell {
            Cell::ListI32(values) => values.len(),
            _ => 0,
        };
        total
            .checked_add(
                values
                    .checked_mul(size_of::<i32>())
                    .ok_or(BuildError::Overflow)?,
            )
            .ok_or(BuildError::Overflow)
    })?;
    u64::try_from(fixed.checked_add(lists).ok_or(BuildError::Overflow)?)
        .map_err(|_error| BuildError::Overflow)
}

fn is_registered_event_stream(type_id: u32) -> bool {
    registry().iter().any(|contract| {
        contract.type_id.get() == type_id && contract.semantics == Semantics::EventStream
    })
}

fn is_supported_event_section(type_id: u32) -> bool {
    SUPPORTED_EVENT_TYPE_IDS.contains(&type_id)
}

const fn validate_row_contract(type_id: u32, row: &Row) -> Result<(), BuildError> {
    if row.contract().type_id.get() == type_id {
        Ok(())
    } else {
        Err(BuildError::Source(SourceError::UnsupportedLayout))
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the dispatcher carries the shared extraction context into each layout decoder"
)]
fn observation_from_row(
    type_id: u32,
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    row_has_truncated_value: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    match type_id {
        ERROR_TYPE_ID => error_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        CHECKPOINT_TYPE_ID => checkpoint_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        AUTOVACUUM_TYPE_ID => maintenance_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        SLOW_QUERY_TYPE_ID => slow_query_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        LOCK_WAIT_TYPE_ID => lock_wait_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        LIFECYCLE_TYPE_ID => lifecycle_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        LOG_GAP_TYPE_ID => gap_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            text_budget,
        ),
        TEMP_FILE_TYPE_ID => temp_file_observation(
            row,
            lineage,
            provenance,
            dictionary,
            row_has_truncated_value,
            timestamp_fallback,
            text_budget,
        ),
        _ => Err(BuildError::Source(SourceError::UnsupportedLayout)),
    }
}

fn error_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let severity = severity(cell_u32(row, "severity")?)?;
    let category = category(cell_u32(row, "category")?)?;
    let count = cell_u32(row, "count")?;
    if count == 0 {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = ObservationPayload::ErrorGroup(Box::new(ErrorGroupPayload {
        severity,
        category,
        sqlstate: resolve_sqlstate(dictionary, cell_opt_str_id(row, "sqlstate")?)?,
        normalized_pattern: resolve_text(
            dictionary,
            cell_opt_str_id(row, "pattern")?,
            text_budget,
        )?,
        sample: resolve_text(dictionary, cell_opt_str_id(row, "sample")?, text_budget)?,
        detail: resolve_text(dictionary, cell_opt_str_id(row, "detail")?, text_budget)?,
        hint: resolve_text(dictionary, cell_opt_str_id(row, "hint")?, text_budget)?,
        context: resolve_text(dictionary, cell_opt_str_id(row, "context")?, text_budget)?,
        statement: resolve_text(dictionary, cell_opt_str_id(row, "statement")?, text_budget)?,
        database: resolve_text(dictionary, cell_opt_str_id(row, "database")?, text_budget)?,
        user: resolve_text(dictionary, cell_opt_str_id(row, "username")?, text_budget)?,
        dropped_field_count: DroppedFieldCount(dropped),
    }));
    new_observation(
        lineage,
        ERROR_TYPE_ID,
        provenance,
        ObservationShape::GroupedCount,
        event_time(
            ts,
            TimeQuality::ParsedWithoutVerifiedOffset,
            timestamp_fallback,
        ),
        u64::from(count),
        payload,
        EvidenceQuality::Heuristic,
        dictionary_loss(dropped, truncated),
    )
}

fn checkpoint_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let phase = cell_u32(row, "phase")?;
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = CheckpointPayload {
        reason: resolve_text(dictionary, cell_opt_str_id(row, "reason")?, text_budget)?,
        seconds_apart: cell_opt_nonnegative_i64(row, "seconds_apart")?,
        buffers_written: cell_opt_nonnegative_i64(row, "buffers_written")?,
        write_ms: cell_opt_nonnegative_finite(row, "write_ms")?,
        sync_ms: cell_opt_nonnegative_finite(row, "sync_ms")?,
        total_ms: cell_opt_nonnegative_finite(row, "total_ms")?,
        distance_kb: cell_opt_nonnegative_i64(row, "distance_kb")?,
        estimate_kb: cell_opt_nonnegative_i64(row, "estimate_kb")?,
        wal_added: cell_opt_nonnegative_i64(row, "wal_added")?,
        wal_removed: cell_opt_nonnegative_i64(row, "wal_removed")?,
        wal_recycled: cell_opt_nonnegative_i64(row, "wal_recycled")?,
        sync_files: cell_opt_nonnegative_i64(row, "sync_files")?,
        longest_sync_ms: cell_opt_nonnegative_finite(row, "longest_sync_ms")?,
        average_sync_ms: cell_opt_nonnegative_finite(row, "average_sync_ms")?,
        dropped_field_count: DroppedFieldCount(dropped),
    };
    let payload = match phase {
        0 => ObservationPayload::CheckpointStarted(Box::new(payload)),
        1 => ObservationPayload::CheckpointCompleted(Box::new(payload)),
        2 => ObservationPayload::CheckpointTooFrequent(Box::new(payload)),
        _ => return Err(BuildError::Source(SourceError::Corrupt)),
    };
    new_observation(
        lineage,
        CHECKPOINT_TYPE_ID,
        provenance,
        ObservationShape::Individual,
        event_time(
            ts,
            TimeQuality::ParsedWithoutVerifiedOffset,
            timestamp_fallback,
        ),
        1,
        payload,
        EvidenceQuality::Parsed,
        dictionary_loss(dropped, truncated),
    )
}

fn maintenance_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let kind = cell_u32(row, "kind")?;
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = MaintenancePayload {
        relation: resolve_text(dictionary, cell_opt_str_id(row, "relation")?, text_budget)?,
        index_scans: cell_opt_nonnegative_i64(row, "index_scans")?,
        pages_removed: cell_opt_nonnegative_i64(row, "pages_removed")?,
        pages_remaining: cell_opt_nonnegative_i64(row, "pages_remaining")?,
        tuples_removed: cell_opt_nonnegative_i64(row, "tuples_removed")?,
        tuples_remaining: cell_opt_nonnegative_i64(row, "tuples_remaining")?,
        tuples_dead_not_removable: cell_opt_nonnegative_i64(row, "tuples_dead_not_removable")?,
        elapsed_ms: cell_opt_nonnegative_finite(row, "elapsed_ms")?,
        buffer_hits: cell_opt_nonnegative_i64(row, "buffer_hits")?,
        buffer_misses: cell_opt_nonnegative_i64(row, "buffer_misses")?,
        buffer_dirtied: cell_opt_nonnegative_i64(row, "buffer_dirtied")?,
        avg_read_rate_mbs: cell_opt_nonnegative_finite(row, "avg_read_rate_mbs")?,
        avg_write_rate_mbs: cell_opt_nonnegative_finite(row, "avg_write_rate_mbs")?,
        cpu_user_ms: cell_opt_nonnegative_finite(row, "cpu_user_ms")?,
        cpu_system_ms: cell_opt_nonnegative_finite(row, "cpu_system_ms")?,
        wal_records: cell_opt_nonnegative_i64(row, "wal_records")?,
        wal_fpi: cell_opt_nonnegative_i64(row, "wal_fpi")?,
        wal_bytes: cell_opt_nonnegative_i64(row, "wal_bytes")?,
        dropped_field_count: DroppedFieldCount(dropped),
    };
    let payload = match kind {
        0 => ObservationPayload::AutovacuumReported(Box::new(payload)),
        1 => ObservationPayload::AutoanalyzeReported(Box::new(payload)),
        _ => return Err(BuildError::Source(SourceError::Corrupt)),
    };
    new_observation(
        lineage,
        AUTOVACUUM_TYPE_ID,
        provenance,
        ObservationShape::Individual,
        event_time(
            ts,
            TimeQuality::ParsedWithoutVerifiedOffset,
            timestamp_fallback,
        ),
        1,
        payload,
        EvidenceQuality::Parsed,
        dictionary_loss(dropped, truncated),
    )
}

fn slow_query_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let count = cell_u32(row, "count")?;
    if count == 0 {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    let max_duration_ms = nonnegative_finite(cell_f64(row, "max_duration_ms")?)?;
    let total_duration_ms = nonnegative_finite(cell_f64(row, "total_duration_ms")?)?;
    if total_duration_ms.get() < max_duration_ms.get() {
        return Err(BuildError::Source(SourceError::Corrupt));
    }
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = ObservationPayload::SlowQueryGroup(Box::new(SlowQueryPayload {
        pattern: resolve_text(dictionary, cell_opt_str_id(row, "pattern")?, text_budget)?,
        sample: resolve_text(dictionary, cell_opt_str_id(row, "sample")?, text_budget)?,
        max_duration_ms,
        total_duration_ms,
        dropped_field_count: DroppedFieldCount(dropped),
    }));
    new_observation(
        lineage,
        SLOW_QUERY_TYPE_ID,
        provenance,
        ObservationShape::GroupedCount,
        event_time(ts, TimeQuality::MaxDurationSample, timestamp_fallback),
        u64::from(count),
        payload,
        EvidenceQuality::Parsed,
        dictionary_loss(dropped, truncated),
    )
}

fn lock_wait_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let kind = cell_u32(row, "kind")?;
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = LockWaitPayload {
        pid: cell_opt_i32(row, "pid")?,
        lock_mode: resolve_text(dictionary, cell_opt_str_id(row, "lock_mode")?, text_budget)?,
        lock_target: resolve_text(
            dictionary,
            cell_opt_str_id(row, "lock_target")?,
            text_budget,
        )?,
        duration_ms: cell_opt_nonnegative_finite(row, "duration_ms")?,
        detail: resolve_text(dictionary, cell_opt_str_id(row, "detail")?, text_budget)?,
        context: resolve_text(dictionary, cell_opt_str_id(row, "context")?, text_budget)?,
        statement: resolve_text(dictionary, cell_opt_str_id(row, "statement")?, text_budget)?,
        dropped_field_count: DroppedFieldCount(dropped),
    };
    let payload = match kind {
        0 => ObservationPayload::LockWaitReported(Box::new(payload)),
        1 => ObservationPayload::LockAcquiredAfterWait(Box::new(payload)),
        _ => return Err(BuildError::Source(SourceError::Corrupt)),
    };
    new_observation(
        lineage,
        LOCK_WAIT_TYPE_ID,
        provenance,
        ObservationShape::Individual,
        event_time(
            ts,
            TimeQuality::ParsedWithoutVerifiedOffset,
            timestamp_fallback,
        ),
        1,
        payload,
        EvidenceQuality::Parsed,
        dictionary_loss(dropped, truncated),
    )
}

fn lifecycle_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let kind = cell_u32(row, "kind")?;
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let retained = LifecyclePayload {
        pid: cell_opt_i32(row, "pid")?,
        signal: cell_opt_i32(row, "signal")?,
        shutdown_mode: resolve_text(
            dictionary,
            cell_opt_str_id(row, "shutdown_mode")?,
            text_budget,
        )?,
        message: resolve_text(dictionary, cell_opt_str_id(row, "message")?, text_budget)?,
        query_detail: resolve_text(
            dictionary,
            cell_opt_str_id(row, "query_detail")?,
            text_budget,
        )?,
        dropped_field_count: DroppedFieldCount(dropped),
    };
    let payload = match kind {
        0 if retained.signal.is_some() => {
            ObservationPayload::ChildSignalTermination(Box::new(retained))
        }
        0 => ObservationPayload::ChildProcessCrash(Box::new(retained)),
        1 => ObservationPayload::ShutdownRequested(Box::new(retained)),
        2 => ObservationPayload::ReadyObserved(Box::new(retained)),
        _ => return Err(BuildError::Source(SourceError::Corrupt)),
    };
    new_observation(
        lineage,
        LIFECYCLE_TYPE_ID,
        provenance,
        ObservationShape::Individual,
        event_time(
            ts,
            TimeQuality::ParsedWithoutVerifiedOffset,
            timestamp_fallback,
        ),
        1,
        payload,
        EvidenceQuality::Parsed,
        dictionary_loss(dropped, truncated),
    )
}

fn gap_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let parser_kind =
        u8::try_from(cell_u32(row, "parser_kind")?).map_err(|_error| source_corrupt())?;
    let reason = u8::try_from(cell_u32(row, "reason")?).map_err(|_error| source_corrupt())?;
    if parser_kind > 2 || reason > 15 {
        return Err(source_corrupt());
    }
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = LogGapPayload {
        source_path: resolve_text(
            dictionary,
            cell_opt_str_id(row, "source_path")?,
            text_budget,
        )?,
        parser_kind,
        reason,
        dev: cell_opt_u64(row, "dev")?,
        inode: cell_opt_u64(row, "inode")?,
        offset: cell_opt_u64(row, "offset")?,
        bytes_skipped: cell_u64(row, "bytes_skipped")?,
        truncated_lines: cell_u32(row, "truncated_lines")?,
        invalid_utf8: cell_u32(row, "invalid_utf8")?,
        binary_dropped: cell_u32(row, "binary_dropped")?,
        rotations: cell_u32(row, "rotations")?,
        missing_files: cell_u32(row, "missing_files")?,
        budget_exhaustions: cell_u32(row, "budget_exhaustions")?,
        dropped_field_count: DroppedFieldCount(dropped),
        parser_dropped_lines: cell_u32(row, "parser_dropped_lines")?,
    };
    let lower_bound = gap_dropped_lower_bound(&payload)?;
    let reasons = gap_loss_reasons(reason, dropped != 0 || truncated);
    let interval = CoverageSpan::new(ts, ts.checked_add(1).ok_or(BuildError::Overflow)?)
        .ok_or(BuildError::Internal)?;
    new_observation(
        lineage,
        LOG_GAP_TYPE_ID,
        provenance,
        ObservationShape::Gap,
        ObservationTime {
            sort_ts_us: ts,
            occurred_at_us: None,
            observed_interval: Some(interval),
            quality: TimeQuality::IntervalOnly,
        },
        1,
        ObservationPayload::LogGap(Box::new(payload)),
        EvidenceQuality::DerivedExact,
        (!reasons.is_empty() || lower_bound != 0).then(|| {
            LossSummary::new(
                reasons,
                if lower_bound == 0 {
                    None
                } else {
                    Some(lower_bound)
                },
            )
        }),
    )
}

fn temp_file_observation(
    row: &Row,
    lineage: SegmentIdentity,
    provenance: ObservationProvenance,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    truncated: bool,
    timestamp_fallback: bool,
    text_budget: &mut TextBudget,
) -> Result<EventObservation, BuildError> {
    let ts = cell_ts(row, "ts")?;
    let size_bytes = cell_i64(row, "size_bytes")?;
    if size_bytes < 0 {
        return Err(source_corrupt());
    }
    let dropped = cell_u32(row, "dict_dropped_fields")?;
    let payload = ObservationPayload::TempFileReported(Box::new(TempFilePayload {
        path: resolve_text(dictionary, cell_opt_str_id(row, "path")?, text_budget)?,
        size_bytes,
        statement: resolve_text(dictionary, cell_opt_str_id(row, "statement")?, text_budget)?,
        dropped_field_count: DroppedFieldCount(dropped),
    }));
    new_observation(
        lineage,
        TEMP_FILE_TYPE_ID,
        provenance,
        ObservationShape::Individual,
        event_time(
            ts,
            TimeQuality::ParsedWithoutVerifiedOffset,
            timestamp_fallback,
        ),
        1,
        payload,
        EvidenceQuality::Parsed,
        dictionary_loss(dropped, truncated),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "arguments mirror EventObservation's validated constructor"
)]
fn new_observation(
    lineage: SegmentIdentity,
    source_type_id: u32,
    provenance: ObservationProvenance,
    shape: ObservationShape,
    time: ObservationTime,
    occurrence_count: u64,
    payload: ObservationPayload,
    evidence_quality: EvidenceQuality,
    loss: Option<LossSummary>,
) -> Result<EventObservation, BuildError> {
    EventObservation::new(
        lineage,
        source_type_id,
        provenance,
        shape,
        time,
        occurrence_count,
        payload,
        evidence_quality,
        QualityFlags(0),
        loss,
    )
    .map_err(|_error| source_corrupt())
}

const fn point_time(ts: i64, quality: TimeQuality) -> ObservationTime {
    ObservationTime {
        sort_ts_us: ts,
        occurred_at_us: Some(ts),
        observed_interval: None,
        quality,
    }
}

const fn event_time(
    ts: i64,
    preferred_quality: TimeQuality,
    timestamp_fallback: bool,
) -> ObservationTime {
    if timestamp_fallback {
        ObservationTime {
            sort_ts_us: ts,
            occurred_at_us: None,
            observed_interval: None,
            quality: TimeQuality::CollectionFallback,
        }
    } else {
        point_time(ts, preferred_quality)
    }
}

fn gap_dropped_lower_bound(payload: &LogGapPayload) -> Result<u64, BuildError> {
    u64::from(payload.invalid_utf8)
        .max(u64::from(payload.parser_dropped_lines))
        .checked_add(u64::from(payload.binary_dropped))
        .ok_or(BuildError::Overflow)
}

fn gap_loss_reasons(reason: u8, dictionary_degraded: bool) -> Vec<LossReason> {
    let mut reasons = Vec::new();
    if dictionary_degraded || reason == 9 {
        reasons.push(LossReason::DictionaryBound);
    }
    match reason {
        2 | 10 => reasons.push(LossReason::ParserBound),
        9 | 11..=13 | 15 => {}
        _ => reasons.push(LossReason::TailerBound),
    }
    reasons
}

fn dictionary_loss(dropped: u32, truncated: bool) -> Option<LossSummary> {
    (dropped != 0 || truncated).then(|| LossSummary::new([LossReason::DictionaryBound], None))
}

fn row_has_truncated_value(
    row: &Row,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
) -> Result<bool, BuildError> {
    let mut truncated = false;
    for cell in row.cells() {
        let Cell::StrId(str_id) = cell else {
            continue;
        };
        let value = dictionary.get(str_id).ok_or_else(source_corrupt)?;
        truncated |= matches!(
            value,
            ResolvedPattern::Blob {
                truncated: true,
                ..
            }
        );
    }
    Ok(truncated)
}

fn row_context_id(
    row: &Row,
    dictionary: &BTreeMap<u64, ResolvedPattern>,
) -> Result<DictionaryContextId, BuildError> {
    let ids: BTreeSet<_> = row
        .cells()
        .iter()
        .filter_map(|cell| match cell {
            Cell::StrId(str_id) => Some(*str_id),
            _ => None,
        })
        .collect();
    let entries: Vec<_> = ids
        .iter()
        .map(|str_id| {
            let value = dictionary.get(str_id).ok_or_else(source_corrupt)?;
            let (bytes, full_len, truncated) = resolved_parts(value);
            Ok(DictionaryContextEntry {
                str_id: *str_id,
                bytes,
                full_len,
                truncated,
            })
        })
        .collect::<Result<_, BuildError>>()?;
    dictionary_context_id(&entries).ok_or(BuildError::Internal)
}

fn resolve_text(
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    str_id: Option<u64>,
    budget: &mut TextBudget,
) -> Result<Option<Box<str>>, BuildError> {
    let Some(str_id) = str_id else {
        return Ok(None);
    };
    let value = dictionary.get(&str_id).ok_or_else(source_corrupt)?;
    let (bytes, _full_len, _truncated) = resolved_parts(value);
    budget.retain(bytes).map(Some)
}

fn resolve_sqlstate(
    dictionary: &BTreeMap<u64, ResolvedPattern>,
    str_id: Option<u64>,
) -> Result<Option<SqlState>, BuildError> {
    let Some(str_id) = str_id else {
        return Ok(None);
    };
    let value = dictionary.get(&str_id).ok_or_else(source_corrupt)?;
    let (bytes, full_len, truncated) = resolved_parts(value);
    if truncated
        || full_len != 5
        || bytes.len() != 5
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
    {
        return Err(source_corrupt());
    }
    let mut code = [0_u8; 5];
    code.copy_from_slice(bytes);
    Ok(Some(SqlState(code)))
}

fn resolved_parts(value: &ResolvedPattern) -> (&[u8], u64, bool) {
    match value {
        ResolvedPattern::Text(bytes) => (bytes, bytes.len() as u64, false),
        ResolvedPattern::Blob {
            bytes,
            full_len,
            truncated,
        } => (bytes, *full_len, *truncated),
    }
}

const fn severity(code: u32) -> Result<Severity, BuildError> {
    match code {
        0 => Ok(Severity::Error),
        1 => Ok(Severity::Fatal),
        2 => Ok(Severity::Panic),
        3 => Ok(Severity::Warning),
        4 => Ok(Severity::Log),
        _ => Err(source_corrupt()),
    }
}

const fn category(code: u32) -> Result<ErrorCategory, BuildError> {
    match code {
        0 => Ok(ErrorCategory::Lock),
        1 => Ok(ErrorCategory::Constraint),
        2 => Ok(ErrorCategory::Serialization),
        3 => Ok(ErrorCategory::Timeout),
        4 => Ok(ErrorCategory::Resource),
        5 => Ok(ErrorCategory::DataCorruption),
        6 => Ok(ErrorCategory::System),
        7 => Ok(ErrorCategory::Connection),
        8 => Ok(ErrorCategory::Auth),
        9 => Ok(ErrorCategory::Syntax),
        10 => Ok(ErrorCategory::Other),
        _ => Err(source_corrupt()),
    }
}

fn nonnegative_finite(value: f64) -> Result<FiniteF64, BuildError> {
    if value < 0.0 {
        return Err(source_corrupt());
    }
    FiniteF64::new(value).ok_or_else(source_corrupt)
}

fn cell_ts(row: &Row, name: &'static str) -> Result<i64, BuildError> {
    match row.get(name) {
        Some(Cell::Ts(value)) => Ok(*value),
        _ => Err(unsupported_layout()),
    }
}

fn cell_i64(row: &Row, name: &'static str) -> Result<i64, BuildError> {
    match row.get(name) {
        Some(Cell::I64(value)) => Ok(*value),
        _ => Err(unsupported_layout()),
    }
}

fn cell_u32(row: &Row, name: &'static str) -> Result<u32, BuildError> {
    match row.get(name) {
        Some(Cell::U32(value)) => Ok(*value),
        _ => Err(unsupported_layout()),
    }
}

fn cell_u64(row: &Row, name: &'static str) -> Result<u64, BuildError> {
    match row.get(name) {
        Some(Cell::U64(value)) => Ok(*value),
        _ => Err(unsupported_layout()),
    }
}

fn cell_f64(row: &Row, name: &'static str) -> Result<f64, BuildError> {
    match row.get(name) {
        Some(Cell::F64(value)) => Ok(*value),
        _ => Err(unsupported_layout()),
    }
}

fn cell_opt_i32(row: &Row, name: &'static str) -> Result<Option<i32>, BuildError> {
    match row.get(name) {
        Some(Cell::Null) => Ok(None),
        Some(Cell::I32(value)) => Ok(Some(*value)),
        _ => Err(unsupported_layout()),
    }
}

fn cell_opt_i64(row: &Row, name: &'static str) -> Result<Option<i64>, BuildError> {
    match row.get(name) {
        Some(Cell::Null) => Ok(None),
        Some(Cell::I64(value)) => Ok(Some(*value)),
        _ => Err(unsupported_layout()),
    }
}

fn cell_opt_nonnegative_i64(row: &Row, name: &'static str) -> Result<Option<i64>, BuildError> {
    let value = cell_opt_i64(row, name)?;
    if value.is_some_and(|value| value < 0) {
        return Err(source_corrupt());
    }
    Ok(value)
}

fn cell_opt_u64(row: &Row, name: &'static str) -> Result<Option<u64>, BuildError> {
    match row.get(name) {
        Some(Cell::Null) => Ok(None),
        Some(Cell::U64(value)) => Ok(Some(*value)),
        _ => Err(unsupported_layout()),
    }
}

fn cell_opt_f64(row: &Row, name: &'static str) -> Result<Option<f64>, BuildError> {
    match row.get(name) {
        Some(Cell::Null) => Ok(None),
        Some(Cell::F64(value)) => Ok(Some(*value)),
        _ => Err(unsupported_layout()),
    }
}

fn cell_opt_nonnegative_finite(
    row: &Row,
    name: &'static str,
) -> Result<Option<FiniteF64>, BuildError> {
    cell_opt_f64(row, name)?.map(nonnegative_finite).transpose()
}

fn cell_opt_str_id(row: &Row, name: &'static str) -> Result<Option<u64>, BuildError> {
    match row.get(name) {
        Some(Cell::Null) => Ok(None),
        Some(Cell::StrId(value)) => Ok(Some(*value)),
        _ => Err(unsupported_layout()),
    }
}

const fn source_corrupt() -> BuildError {
    BuildError::Source(SourceError::Corrupt)
}

const fn unsupported_layout() -> BuildError {
    BuildError::Source(SourceError::UnsupportedLayout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_layouts_equal_the_registered_event_stream_set() {
        let registered: BTreeSet<_> = registry()
            .iter()
            .filter(|contract| contract.semantics == Semantics::EventStream)
            .map(|contract| contract.type_id.get())
            .collect();
        assert_eq!(registered, BTreeSet::from(SUPPORTED_EVENT_TYPE_IDS));
    }

    #[test]
    fn source_category_codes_map_to_the_analytics_taxonomy() {
        let expected = [
            ErrorCategory::Lock,
            ErrorCategory::Constraint,
            ErrorCategory::Serialization,
            ErrorCategory::Timeout,
            ErrorCategory::Resource,
            ErrorCategory::DataCorruption,
            ErrorCategory::System,
            ErrorCategory::Connection,
            ErrorCategory::Auth,
            ErrorCategory::Syntax,
            ErrorCategory::Other,
        ];
        for (code, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                category(u32::try_from(code).expect("category code fits u32")),
                Ok(expected)
            );
        }
        assert!(category(11).is_err());
    }

    #[test]
    fn every_gap_reason_has_an_explicit_loss_disposition() {
        for reason in 0..=15 {
            let expected = match reason {
                2 | 10 => vec![LossReason::ParserBound],
                9 => vec![LossReason::DictionaryBound],
                11..=13 | 15 => Vec::new(),
                _ => vec![LossReason::TailerBound],
            };
            assert_eq!(gap_loss_reasons(reason, false), expected, "reason {reason}");
        }
        assert_eq!(
            gap_loss_reasons(2, true),
            vec![LossReason::DictionaryBound, LossReason::ParserBound]
        );
    }

    #[test]
    fn overlapping_invalid_utf8_and_parser_counts_are_counted_once() {
        let payload = LogGapPayload {
            source_path: None,
            parser_kind: 0,
            reason: 2,
            dev: None,
            inode: None,
            offset: None,
            bytes_skipped: 0,
            truncated_lines: 0,
            invalid_utf8: 3,
            binary_dropped: 2,
            rotations: 0,
            missing_files: 0,
            budget_exhaustions: 0,
            dropped_field_count: DroppedFieldCount(0),
            parser_dropped_lines: 3,
        };
        assert_eq!(gap_dropped_lower_bound(&payload), Ok(5));
    }
}
