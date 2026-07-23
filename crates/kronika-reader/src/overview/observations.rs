//! The event-observations block and its payload codec.
//!
//! The decoder rebuilds every record through
//! [`kronika_analytics::overview::EventObservation`], preserving its validated
//! identity. Segment lineage is shared decode context. Text fields are
//! references into the canonical [`StringTableBlock`].

use kronika_analytics::overview::{
    CheckpointPayload, CoverageSpan, DictionaryContextId, DroppedFieldCount, ErrorCategory,
    ErrorGroupPayload, EventObservation, EvidenceQuality, FiniteF64, LifecyclePayload,
    LockWaitPayload, LogGapPayload, LossReason, LossSummary, MaintenancePayload,
    ObservationPayload, ObservationProvenance, ObservationShape, ObservationTime, QualityFlags,
    SectionBodyId, SegmentIdentity, SegmentLocator, Severity, SlowQueryPayload, SourceLocator,
    SqlState, TempFilePayload, TimeQuality,
};

use super::block::{BlockError, BlockKind, EncodableBlock, StringTableBlock};
use super::bytes::{ByteReader, ByteWriter};
use super::limits::Bounds;

fn write_opt_i32(writer: &mut ByteWriter, value: Option<i32>) {
    match value {
        Some(inner) => {
            writer.u8(1);
            writer.i32_le(inner);
        }
        None => writer.u8(0),
    }
}

fn read_opt_i32(reader: &mut ByteReader<'_>) -> Result<Option<i32>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.i32_le()?)),
        _ => Err(BlockError::Malformed),
    }
}

fn write_opt_i64(writer: &mut ByteWriter, value: Option<i64>) {
    match value {
        Some(inner) => {
            writer.u8(1);
            writer.i64_le(inner);
        }
        None => writer.u8(0),
    }
}

fn read_opt_i64(reader: &mut ByteReader<'_>) -> Result<Option<i64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.i64_le()?)),
        _ => Err(BlockError::Malformed),
    }
}

fn write_opt_u64(writer: &mut ByteWriter, value: Option<u64>) {
    match value {
        Some(inner) => {
            writer.u8(1);
            writer.u64_le(inner);
        }
        None => writer.u8(0),
    }
}

fn read_opt_u64(reader: &mut ByteReader<'_>) -> Result<Option<u64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64_le()?)),
        _ => Err(BlockError::Malformed),
    }
}

fn write_opt_f64(writer: &mut ByteWriter, value: Option<FiniteF64>) {
    match value {
        Some(inner) => {
            writer.u8(1);
            writer.f64_le(inner.get());
        }
        None => writer.u8(0),
    }
}

fn read_opt_f64(reader: &mut ByteReader<'_>) -> Result<Option<FiniteF64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let value = reader.f64_finite()?;
            Ok(Some(
                FiniteF64::new(value).ok_or(BlockError::NonFiniteFloat)?,
            ))
        }
        _ => Err(BlockError::Malformed),
    }
}

fn write_opt_str(writer: &mut ByteWriter, value: Option<&str>, strings: &StringTableBlock) {
    match value {
        Some(inner) => {
            let reference = strings
                .values()
                .binary_search_by(|candidate| candidate.as_ref().cmp(inner.as_bytes()))
                .ok()
                .and_then(|index| u64::try_from(index).ok())
                .and_then(|index| index.checked_add(1))
                .unwrap_or(u64::MAX);
            writer.uvarint(reference);
        }
        None => writer.uvarint(0),
    }
}

fn read_opt_str(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Option<Box<str>>, BlockError> {
    let reference = reader.uvarint(bounds.items_per_block)?;
    if reference == 0 {
        return Ok(None);
    }
    let index = reference
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or(BlockError::Malformed)?;
    let bytes = strings.values().get(index).ok_or(BlockError::Malformed)?;
    *text_budget = text_budget
        .checked_sub(bytes.len() as u64)
        .ok_or(BlockError::AboveBound)?;
    let text = str::from_utf8(bytes).map_err(|_error| BlockError::Malformed)?;
    Ok(Some(text.into()))
}

const fn shape_code(shape: ObservationShape) -> u8 {
    match shape {
        ObservationShape::Individual => 0,
        ObservationShape::GroupedCount => 1,
        ObservationShape::Gap => 2,
    }
}

const fn shape_from(code: u8) -> Result<ObservationShape, BlockError> {
    match code {
        0 => Ok(ObservationShape::Individual),
        1 => Ok(ObservationShape::GroupedCount),
        2 => Ok(ObservationShape::Gap),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn time_quality_code(quality: TimeQuality) -> u8 {
    match quality {
        TimeQuality::Exact => 0,
        TimeQuality::FirstInGroup => 1,
        TimeQuality::RepresentativeSample => 2,
        TimeQuality::MaxDurationSample => 3,
        TimeQuality::ParsedWithoutVerifiedOffset => 4,
        TimeQuality::CollectionFallback => 5,
        TimeQuality::IntervalOnly => 6,
    }
}

const fn time_quality_from(code: u8) -> Result<TimeQuality, BlockError> {
    match code {
        0 => Ok(TimeQuality::Exact),
        1 => Ok(TimeQuality::FirstInGroup),
        2 => Ok(TimeQuality::RepresentativeSample),
        3 => Ok(TimeQuality::MaxDurationSample),
        4 => Ok(TimeQuality::ParsedWithoutVerifiedOffset),
        5 => Ok(TimeQuality::CollectionFallback),
        6 => Ok(TimeQuality::IntervalOnly),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn evidence_quality_code(quality: EvidenceQuality) -> u8 {
    match quality {
        EvidenceQuality::Structured => 0,
        EvidenceQuality::Parsed => 1,
        EvidenceQuality::Heuristic => 2,
        EvidenceQuality::DerivedExact => 3,
    }
}

const fn evidence_quality_from(code: u8) -> Result<EvidenceQuality, BlockError> {
    match code {
        0 => Ok(EvidenceQuality::Structured),
        1 => Ok(EvidenceQuality::Parsed),
        2 => Ok(EvidenceQuality::Heuristic),
        3 => Ok(EvidenceQuality::DerivedExact),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn severity_code(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 0,
        Severity::Fatal => 1,
        Severity::Panic => 2,
        Severity::Warning => 3,
        Severity::Log => 4,
    }
}

const fn severity_from(code: u8) -> Result<Severity, BlockError> {
    match code {
        0 => Ok(Severity::Error),
        1 => Ok(Severity::Fatal),
        2 => Ok(Severity::Panic),
        3 => Ok(Severity::Warning),
        4 => Ok(Severity::Log),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn category_code(category: ErrorCategory) -> u8 {
    match category {
        ErrorCategory::Lock => 0,
        ErrorCategory::Constraint => 1,
        ErrorCategory::Serialization => 2,
        ErrorCategory::Timeout => 3,
        ErrorCategory::Connection => 4,
        ErrorCategory::Auth => 5,
        ErrorCategory::Syntax => 6,
        ErrorCategory::Resource => 7,
        ErrorCategory::DataCorruption => 8,
        ErrorCategory::System => 9,
        ErrorCategory::Other => 10,
    }
}

const fn category_from(code: u8) -> Result<ErrorCategory, BlockError> {
    match code {
        0 => Ok(ErrorCategory::Lock),
        1 => Ok(ErrorCategory::Constraint),
        2 => Ok(ErrorCategory::Serialization),
        3 => Ok(ErrorCategory::Timeout),
        4 => Ok(ErrorCategory::Connection),
        5 => Ok(ErrorCategory::Auth),
        6 => Ok(ErrorCategory::Syntax),
        7 => Ok(ErrorCategory::Resource),
        8 => Ok(ErrorCategory::DataCorruption),
        9 => Ok(ErrorCategory::System),
        10 => Ok(ErrorCategory::Other),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn loss_reason_code(reason: LossReason) -> u8 {
    match reason {
        LossReason::GroupCapExceeded => 0,
        LossReason::LifecycleCapExceeded => 1,
        LossReason::ParserBound => 2,
        LossReason::TailerBound => 3,
        LossReason::DictionaryBound => 4,
    }
}

const fn loss_reason_from(code: u8) -> Result<LossReason, BlockError> {
    match code {
        0 => Ok(LossReason::GroupCapExceeded),
        1 => Ok(LossReason::LifecycleCapExceeded),
        2 => Ok(LossReason::ParserBound),
        3 => Ok(LossReason::TailerBound),
        4 => Ok(LossReason::DictionaryBound),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn dropped_field_count(value: DroppedFieldCount) -> u32 {
    value.0
}

fn write_optional_id32(writer: &mut ByteWriter, value: Option<[u8; 32]>) {
    match value {
        Some(inner) => {
            writer.u8(1);
            writer.bytes(&inner);
        }
        None => writer.u8(0),
    }
}

fn read_optional_id32(reader: &mut ByteReader<'_>) -> Result<Option<[u8; 32]>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.array()?)),
        _ => Err(BlockError::Malformed),
    }
}

fn write_provenance(writer: &mut ByteWriter, provenance: &ObservationProvenance) {
    write_optional_id32(writer, provenance.segment_locator.map(|locator| locator.0));
    writer.bytes(&provenance.section_body_id.0);
    writer.u32_le(provenance.catalog_entry_ordinal);
    writer.u32_le(provenance.row_ordinal);
    writer.bytes(&provenance.dictionary_context_id.0);
    match provenance.source_locator {
        Some(locator) => {
            writer.u8(1);
            writer.bytes(&locator.source_unit_id);
            writer.u64_le(locator.byte_offset);
        }
        None => writer.u8(0),
    }
}

fn read_provenance(reader: &mut ByteReader<'_>) -> Result<ObservationProvenance, BlockError> {
    let segment_locator = read_optional_id32(reader)?.map(SegmentLocator);
    let section_body_id = SectionBodyId(reader.array()?);
    let catalog_entry_ordinal = reader.u32_le()?;
    let row_ordinal = reader.u32_le()?;
    let dictionary_context_id = DictionaryContextId(reader.array()?);
    let source_locator = match reader.u8()? {
        0 => None,
        1 => Some(SourceLocator {
            source_unit_id: reader.array()?,
            byte_offset: reader.u64_le()?,
        }),
        _ => return Err(BlockError::Malformed),
    };
    Ok(ObservationProvenance {
        segment_locator,
        section_body_id,
        catalog_entry_ordinal,
        row_ordinal,
        dictionary_context_id,
        source_locator,
    })
}

fn write_time(writer: &mut ByteWriter, time: ObservationTime) {
    writer.i64_le(time.sort_ts_us);
    write_opt_i64(writer, time.occurred_at_us);
    match time.observed_interval {
        Some(span) => {
            writer.u8(1);
            writer.i64_le(span.start_us());
            writer.i64_le(span.end_us());
        }
        None => writer.u8(0),
    }
    writer.u8(time_quality_code(time.quality));
}

fn read_time(reader: &mut ByteReader<'_>) -> Result<ObservationTime, BlockError> {
    let sort_ts_us = reader.i64_le()?;
    let occurred_at_us = read_opt_i64(reader)?;
    let observed_interval = match reader.u8()? {
        0 => None,
        1 => {
            let from_us = reader.i64_le()?;
            let to_us = reader.i64_le()?;
            Some(CoverageSpan::new(from_us, to_us).ok_or(BlockError::Malformed)?)
        }
        _ => return Err(BlockError::Malformed),
    };
    let quality = time_quality_from(reader.u8()?)?;
    Ok(ObservationTime {
        sort_ts_us,
        occurred_at_us,
        observed_interval,
        quality,
    })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the closed set has at most five loss reasons"
)]
fn write_loss(writer: &mut ByteWriter, loss: Option<&LossSummary>) {
    match loss {
        Some(summary) => {
            writer.u8(1);
            writer.u8(summary.reasons().len() as u8);
            for &reason in summary.reasons() {
                writer.u8(loss_reason_code(reason));
            }
            write_opt_u64(writer, summary.lost_count_lower_bound);
        }
        None => writer.u8(0),
    }
}

fn read_loss(reader: &mut ByteReader<'_>) -> Result<Option<LossSummary>, BlockError> {
    // Five closed loss reasons exist; a longer run is malformed.
    const CLOSED_LOSS_REASONS: u8 = 5;
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let count = reader.u8()?;
            if count > CLOSED_LOSS_REASONS {
                return Err(BlockError::AboveBound);
            }
            let mut reasons = Vec::with_capacity(count as usize);
            for _ in 0..count {
                reasons.push(loss_reason_from(reader.u8()?)?);
            }
            let lost_count_lower_bound = read_opt_u64(reader)?;
            Ok(Some(LossSummary::new(reasons, lost_count_lower_bound)))
        }
        _ => Err(BlockError::Malformed),
    }
}

fn write_error_group(
    writer: &mut ByteWriter,
    payload: &ErrorGroupPayload,
    strings: &StringTableBlock,
) {
    writer.u8(severity_code(payload.severity));
    writer.u8(category_code(payload.category));
    match payload.sqlstate {
        Some(state) => {
            writer.u8(1);
            writer.bytes(&state.0);
        }
        None => writer.u8(0),
    }
    write_opt_str(writer, payload.normalized_pattern.as_deref(), strings);
    write_opt_str(writer, payload.sample.as_deref(), strings);
    write_opt_str(writer, payload.detail.as_deref(), strings);
    write_opt_str(writer, payload.hint.as_deref(), strings);
    write_opt_str(writer, payload.context.as_deref(), strings);
    write_opt_str(writer, payload.statement.as_deref(), strings);
    write_opt_str(writer, payload.database.as_deref(), strings);
    write_opt_str(writer, payload.user.as_deref(), strings);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_error_group(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<ErrorGroupPayload>, BlockError> {
    let severity = severity_from(reader.u8()?)?;
    let category = category_from(reader.u8()?)?;
    let sqlstate = match reader.u8()? {
        0 => None,
        1 => Some(SqlState(reader.array()?)),
        _ => return Err(BlockError::Malformed),
    };
    let normalized_pattern = read_opt_str(reader, strings, bounds, text_budget)?;
    let sample = read_opt_str(reader, strings, bounds, text_budget)?;
    let detail = read_opt_str(reader, strings, bounds, text_budget)?;
    let hint = read_opt_str(reader, strings, bounds, text_budget)?;
    let context = read_opt_str(reader, strings, bounds, text_budget)?;
    let statement = read_opt_str(reader, strings, bounds, text_budget)?;
    let database = read_opt_str(reader, strings, bounds, text_budget)?;
    let user = read_opt_str(reader, strings, bounds, text_budget)?;
    let dropped_field_count = DroppedFieldCount(reader.u32_le()?);
    Ok(Box::new(ErrorGroupPayload {
        severity,
        category,
        sqlstate,
        normalized_pattern,
        sample,
        detail,
        hint,
        context,
        statement,
        database,
        user,
        dropped_field_count,
    }))
}

fn write_lifecycle(
    writer: &mut ByteWriter,
    payload: &LifecyclePayload,
    strings: &StringTableBlock,
) {
    write_opt_i32(writer, payload.pid);
    write_opt_i32(writer, payload.signal);
    write_opt_str(writer, payload.shutdown_mode.as_deref(), strings);
    write_opt_str(writer, payload.message.as_deref(), strings);
    write_opt_str(writer, payload.query_detail.as_deref(), strings);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_lifecycle(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<LifecyclePayload>, BlockError> {
    let pid = read_opt_i32(reader)?;
    let signal = read_opt_i32(reader)?;
    let shutdown_mode = read_opt_str(reader, strings, bounds, text_budget)?;
    let message = read_opt_str(reader, strings, bounds, text_budget)?;
    let query_detail = read_opt_str(reader, strings, bounds, text_budget)?;
    let dropped_field_count = DroppedFieldCount(reader.u32_le()?);
    Ok(Box::new(LifecyclePayload {
        pid,
        signal,
        shutdown_mode,
        message,
        query_detail,
        dropped_field_count,
    }))
}

fn write_checkpoint(
    writer: &mut ByteWriter,
    payload: &CheckpointPayload,
    strings: &StringTableBlock,
) {
    write_opt_str(writer, payload.reason.as_deref(), strings);
    write_opt_i64(writer, payload.seconds_apart);
    write_opt_i64(writer, payload.buffers_written);
    write_opt_f64(writer, payload.write_ms);
    write_opt_f64(writer, payload.sync_ms);
    write_opt_f64(writer, payload.total_ms);
    write_opt_i64(writer, payload.distance_kb);
    write_opt_i64(writer, payload.estimate_kb);
    write_opt_i64(writer, payload.wal_added);
    write_opt_i64(writer, payload.wal_removed);
    write_opt_i64(writer, payload.wal_recycled);
    write_opt_i64(writer, payload.sync_files);
    write_opt_f64(writer, payload.longest_sync_ms);
    write_opt_f64(writer, payload.average_sync_ms);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_checkpoint(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<CheckpointPayload>, BlockError> {
    Ok(Box::new(CheckpointPayload {
        reason: read_opt_str(reader, strings, bounds, text_budget)?,
        seconds_apart: read_opt_i64(reader)?,
        buffers_written: read_opt_i64(reader)?,
        write_ms: read_opt_f64(reader)?,
        sync_ms: read_opt_f64(reader)?,
        total_ms: read_opt_f64(reader)?,
        distance_kb: read_opt_i64(reader)?,
        estimate_kb: read_opt_i64(reader)?,
        wal_added: read_opt_i64(reader)?,
        wal_removed: read_opt_i64(reader)?,
        wal_recycled: read_opt_i64(reader)?,
        sync_files: read_opt_i64(reader)?,
        longest_sync_ms: read_opt_f64(reader)?,
        average_sync_ms: read_opt_f64(reader)?,
        dropped_field_count: DroppedFieldCount(reader.u32_le()?),
    }))
}

fn write_maintenance(
    writer: &mut ByteWriter,
    payload: &MaintenancePayload,
    strings: &StringTableBlock,
) {
    write_opt_str(writer, payload.relation.as_deref(), strings);
    write_opt_i64(writer, payload.index_scans);
    write_opt_i64(writer, payload.pages_removed);
    write_opt_i64(writer, payload.pages_remaining);
    write_opt_i64(writer, payload.tuples_removed);
    write_opt_i64(writer, payload.tuples_remaining);
    write_opt_i64(writer, payload.tuples_dead_not_removable);
    write_opt_f64(writer, payload.elapsed_ms);
    write_opt_i64(writer, payload.buffer_hits);
    write_opt_i64(writer, payload.buffer_misses);
    write_opt_i64(writer, payload.buffer_dirtied);
    write_opt_f64(writer, payload.avg_read_rate_mbs);
    write_opt_f64(writer, payload.avg_write_rate_mbs);
    write_opt_f64(writer, payload.cpu_user_ms);
    write_opt_f64(writer, payload.cpu_system_ms);
    write_opt_i64(writer, payload.wal_records);
    write_opt_i64(writer, payload.wal_fpi);
    write_opt_i64(writer, payload.wal_bytes);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_maintenance(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<MaintenancePayload>, BlockError> {
    Ok(Box::new(MaintenancePayload {
        relation: read_opt_str(reader, strings, bounds, text_budget)?,
        index_scans: read_opt_i64(reader)?,
        pages_removed: read_opt_i64(reader)?,
        pages_remaining: read_opt_i64(reader)?,
        tuples_removed: read_opt_i64(reader)?,
        tuples_remaining: read_opt_i64(reader)?,
        tuples_dead_not_removable: read_opt_i64(reader)?,
        elapsed_ms: read_opt_f64(reader)?,
        buffer_hits: read_opt_i64(reader)?,
        buffer_misses: read_opt_i64(reader)?,
        buffer_dirtied: read_opt_i64(reader)?,
        avg_read_rate_mbs: read_opt_f64(reader)?,
        avg_write_rate_mbs: read_opt_f64(reader)?,
        cpu_user_ms: read_opt_f64(reader)?,
        cpu_system_ms: read_opt_f64(reader)?,
        wal_records: read_opt_i64(reader)?,
        wal_fpi: read_opt_i64(reader)?,
        wal_bytes: read_opt_i64(reader)?,
        dropped_field_count: DroppedFieldCount(reader.u32_le()?),
    }))
}

fn write_slow_query(
    writer: &mut ByteWriter,
    payload: &SlowQueryPayload,
    strings: &StringTableBlock,
) {
    write_opt_str(writer, payload.pattern.as_deref(), strings);
    write_opt_str(writer, payload.sample.as_deref(), strings);
    writer.f64_le(payload.max_duration_ms.get());
    writer.f64_le(payload.total_duration_ms.get());
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_slow_query(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<SlowQueryPayload>, BlockError> {
    let pattern = read_opt_str(reader, strings, bounds, text_budget)?;
    let sample = read_opt_str(reader, strings, bounds, text_budget)?;
    let max_duration_ms = FiniteF64::new(reader.f64_finite()?).ok_or(BlockError::NonFiniteFloat)?;
    let total_duration_ms =
        FiniteF64::new(reader.f64_finite()?).ok_or(BlockError::NonFiniteFloat)?;
    let dropped_field_count = DroppedFieldCount(reader.u32_le()?);
    Ok(Box::new(SlowQueryPayload {
        pattern,
        sample,
        max_duration_ms,
        total_duration_ms,
        dropped_field_count,
    }))
}

fn write_lock_wait(writer: &mut ByteWriter, payload: &LockWaitPayload, strings: &StringTableBlock) {
    write_opt_i32(writer, payload.pid);
    write_opt_str(writer, payload.lock_mode.as_deref(), strings);
    write_opt_str(writer, payload.lock_target.as_deref(), strings);
    write_opt_f64(writer, payload.duration_ms);
    write_opt_str(writer, payload.detail.as_deref(), strings);
    write_opt_str(writer, payload.context.as_deref(), strings);
    write_opt_str(writer, payload.statement.as_deref(), strings);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_lock_wait(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<LockWaitPayload>, BlockError> {
    Ok(Box::new(LockWaitPayload {
        pid: read_opt_i32(reader)?,
        lock_mode: read_opt_str(reader, strings, bounds, text_budget)?,
        lock_target: read_opt_str(reader, strings, bounds, text_budget)?,
        duration_ms: read_opt_f64(reader)?,
        detail: read_opt_str(reader, strings, bounds, text_budget)?,
        context: read_opt_str(reader, strings, bounds, text_budget)?,
        statement: read_opt_str(reader, strings, bounds, text_budget)?,
        dropped_field_count: DroppedFieldCount(reader.u32_le()?),
    }))
}

fn write_temp_file(writer: &mut ByteWriter, payload: &TempFilePayload, strings: &StringTableBlock) {
    write_opt_str(writer, payload.path.as_deref(), strings);
    writer.i64_le(payload.size_bytes);
    write_opt_str(writer, payload.statement.as_deref(), strings);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_temp_file(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<TempFilePayload>, BlockError> {
    let path = read_opt_str(reader, strings, bounds, text_budget)?;
    let size_bytes = reader.i64_le()?;
    let statement = read_opt_str(reader, strings, bounds, text_budget)?;
    let dropped_field_count = DroppedFieldCount(reader.u32_le()?);
    Ok(Box::new(TempFilePayload {
        path,
        size_bytes,
        statement,
        dropped_field_count,
    }))
}

fn write_log_gap(writer: &mut ByteWriter, payload: &LogGapPayload, strings: &StringTableBlock) {
    write_opt_str(writer, payload.source_path.as_deref(), strings);
    writer.u8(payload.parser_kind);
    writer.u8(payload.reason);
    write_opt_u64(writer, payload.dev);
    write_opt_u64(writer, payload.inode);
    write_opt_u64(writer, payload.offset);
    writer.u64_le(payload.bytes_skipped);
    writer.u32_le(payload.truncated_lines);
    writer.u32_le(payload.invalid_utf8);
    writer.u32_le(payload.binary_dropped);
    writer.u32_le(payload.rotations);
    writer.u32_le(payload.missing_files);
    writer.u32_le(payload.budget_exhaustions);
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
    writer.u32_le(payload.parser_dropped_lines);
}

fn read_log_gap(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<Box<LogGapPayload>, BlockError> {
    Ok(Box::new(LogGapPayload {
        source_path: read_opt_str(reader, strings, bounds, text_budget)?,
        parser_kind: reader.u8()?,
        reason: reader.u8()?,
        dev: read_opt_u64(reader)?,
        inode: read_opt_u64(reader)?,
        offset: read_opt_u64(reader)?,
        bytes_skipped: reader.u64_le()?,
        truncated_lines: reader.u32_le()?,
        invalid_utf8: reader.u32_le()?,
        binary_dropped: reader.u32_le()?,
        rotations: reader.u32_le()?,
        missing_files: reader.u32_le()?,
        budget_exhaustions: reader.u32_le()?,
        dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        parser_dropped_lines: reader.u32_le()?,
    }))
}

const fn payload_tag(payload: &ObservationPayload) -> u8 {
    match payload {
        ObservationPayload::ErrorGroup(_) => 0,
        ObservationPayload::ChildSignalTermination(_) => 1,
        ObservationPayload::ShutdownRequested(_) => 2,
        ObservationPayload::ReadyObserved(_) => 3,
        ObservationPayload::CheckpointStarted(_) => 4,
        ObservationPayload::CheckpointCompleted(_) => 5,
        ObservationPayload::CheckpointTooFrequent(_) => 6,
        ObservationPayload::AutovacuumReported(_) => 7,
        ObservationPayload::AutoanalyzeReported(_) => 8,
        ObservationPayload::SlowQueryGroup(_) => 9,
        ObservationPayload::LockWaitReported(_) => 10,
        ObservationPayload::LockAcquiredAfterWait(_) => 11,
        ObservationPayload::TempFileReported(_) => 12,
        ObservationPayload::LogGap(_) => 13,
        ObservationPayload::ChildProcessCrash(_) => 14,
    }
}

fn write_payload(
    writer: &mut ByteWriter,
    payload: &ObservationPayload,
    strings: &StringTableBlock,
) {
    writer.u8(payload_tag(payload));
    match payload {
        ObservationPayload::ErrorGroup(inner) => write_error_group(writer, inner, strings),
        ObservationPayload::ChildSignalTermination(inner)
        | ObservationPayload::ChildProcessCrash(inner)
        | ObservationPayload::ShutdownRequested(inner)
        | ObservationPayload::ReadyObserved(inner) => write_lifecycle(writer, inner, strings),
        ObservationPayload::CheckpointStarted(inner)
        | ObservationPayload::CheckpointCompleted(inner)
        | ObservationPayload::CheckpointTooFrequent(inner) => {
            write_checkpoint(writer, inner, strings);
        }
        ObservationPayload::AutovacuumReported(inner)
        | ObservationPayload::AutoanalyzeReported(inner) => {
            write_maintenance(writer, inner, strings);
        }
        ObservationPayload::SlowQueryGroup(inner) => write_slow_query(writer, inner, strings),
        ObservationPayload::LockWaitReported(inner)
        | ObservationPayload::LockAcquiredAfterWait(inner) => {
            write_lock_wait(writer, inner, strings);
        }
        ObservationPayload::TempFileReported(inner) => write_temp_file(writer, inner, strings),
        ObservationPayload::LogGap(inner) => write_log_gap(writer, inner, strings),
    }
}

fn read_payload(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    text_budget: &mut u64,
) -> Result<ObservationPayload, BlockError> {
    let tag = reader.u8()?;
    Ok(match tag {
        0 => {
            ObservationPayload::ErrorGroup(read_error_group(reader, strings, bounds, text_budget)?)
        }
        1 => ObservationPayload::ChildSignalTermination(read_lifecycle(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        2 => ObservationPayload::ShutdownRequested(read_lifecycle(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        3 => {
            ObservationPayload::ReadyObserved(read_lifecycle(reader, strings, bounds, text_budget)?)
        }
        4 => ObservationPayload::CheckpointStarted(read_checkpoint(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        5 => ObservationPayload::CheckpointCompleted(read_checkpoint(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        6 => ObservationPayload::CheckpointTooFrequent(read_checkpoint(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        7 => ObservationPayload::AutovacuumReported(read_maintenance(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        8 => ObservationPayload::AutoanalyzeReported(read_maintenance(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        9 => ObservationPayload::SlowQueryGroup(read_slow_query(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        10 => ObservationPayload::LockWaitReported(read_lock_wait(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        11 => ObservationPayload::LockAcquiredAfterWait(read_lock_wait(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        12 => ObservationPayload::TempFileReported(read_temp_file(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        13 => ObservationPayload::LogGap(read_log_gap(reader, strings, bounds, text_budget)?),
        14 => ObservationPayload::ChildProcessCrash(read_lifecycle(
            reader,
            strings,
            bounds,
            text_budget,
        )?),
        _ => return Err(BlockError::InvalidEnum),
    })
}

/// A canonical, sorted set of retained observations.
///
/// The canonical order is `(sort_ts_us, observation_id)`. Decoding rebuilds
/// every record through the analytics constructor with the segment lineage
/// supplied as context, so the derived identities match the encoded ones.
#[derive(Clone, PartialEq, Eq)]
pub struct EventObservationsBlock {
    observations: Vec<EventObservation>,
    strings: StringTableBlock,
}

impl std::fmt::Debug for EventObservationsBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventObservationsBlock")
            .field("observations", &self.observations.len())
            .field("time_range", &self.time_range())
            .field("strings", &self.strings)
            .finish()
    }
}

impl EventObservationsBlock {
    /// Normalizes observations into canonical order.
    ///
    /// # Errors
    /// Returns [`BlockError::Duplicate`] for two records with the same derived
    /// identity, and [`BlockError::AboveBound`] past the item bound.
    pub fn new(
        mut observations: Vec<EventObservation>,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() {
            return Err(BlockError::AboveBound);
        }
        if observations.len() as u64 > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
        if observations
            .iter()
            .any(|observation| !payload_is_bounded(observation.payload(), bounds))
        {
            return Err(BlockError::AboveBound);
        }
        if observations
            .iter()
            .any(|observation| !payload_has_valid_sqlstate(observation.payload()))
        {
            return Err(BlockError::Malformed);
        }
        if observations
            .iter()
            .any(|observation| !is_event_source_type(observation.source_type_id()))
        {
            return Err(BlockError::InvalidEnum);
        }
        let mut values = Vec::new();
        for observation in &observations {
            collect_payload_text(observation.payload(), &mut values);
        }
        let strings = StringTableBlock::new(values, bounds)?;
        observations.sort_by(EventObservation::canonical_cmp);
        if observations
            .windows(2)
            .any(|w| w[0].observation_id() == w[1].observation_id())
        {
            return Err(BlockError::Duplicate);
        }
        Ok(Self {
            observations,
            strings,
        })
    }

    /// The canonical observations.
    #[must_use]
    pub fn observations(&self) -> &[EventObservation] {
        &self.observations
    }

    /// Canonical table referenced by encoded observation text fields.
    #[must_use]
    pub const fn string_table(&self) -> &StringTableBlock {
        &self.strings
    }

    pub(super) fn into_observations(self) -> Vec<EventObservation> {
        self.observations
    }

    /// Decodes an observations block body against a segment lineage.
    ///
    /// All observations in the body must belong to `lineage`; the decoder
    /// rebuilds them through the validating constructor.
    ///
    /// # Errors
    /// Returns [`BlockError`] for a truncated, out-of-order, invalid-enum,
    /// out-of-bound, or trailing-byte body, and [`BlockError::Reconstruct`]
    /// when the analytics constructor rejects a decoded record.
    pub fn decode(
        body: &[u8],
        lineage: &SegmentIdentity,
        strings: &StringTableBlock,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        let mut text_budget = bounds.decoded_block_len;
        let mut item_budget = bounds.items_per_block;
        Self::decode_with_budgets(
            body,
            lineage,
            strings,
            bounds,
            &mut item_budget,
            &mut text_budget,
        )
    }

    pub(super) fn decode_with_budgets(
        body: &[u8],
        lineage: &SegmentIdentity,
        strings: &StringTableBlock,
        bounds: &Bounds,
        item_budget: &mut u64,
        text_budget: &mut u64,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Self::new(Vec::new(), bounds);
        }
        let mut reader = ByteReader::new(body);
        let count = reader.uvarint(*item_budget)?;
        *item_budget = item_budget
            .checked_sub(count)
            .ok_or(BlockError::AboveBound)?;
        let mut observations = Vec::with_capacity(count.min(4_096) as usize);
        for _ in 0..count {
            let source_type_id = reader.u32_le()?;
            let provenance = read_provenance(&mut reader)?;
            let shape = shape_from(reader.u8()?)?;
            let time = read_time(&mut reader)?;
            let occurrence_count = reader.u64_le()?;
            let evidence_quality = evidence_quality_from(reader.u8()?)?;
            let quality_flags = QualityFlags(reader.u32_le()?);
            let loss = read_loss(&mut reader)?;
            let payload = read_payload(&mut reader, strings, bounds, text_budget)?;
            if !payload_has_valid_sqlstate(&payload) {
                return Err(BlockError::Malformed);
            }
            let observation = EventObservation::new(
                *lineage,
                source_type_id,
                provenance,
                shape,
                time,
                occurrence_count,
                payload,
                evidence_quality,
                quality_flags,
                loss,
            )
            .map_err(|_error| BlockError::Reconstruct)?;
            observations.push(observation);
        }
        reader.finish()?;
        for w in observations.windows(2) {
            match w[0].canonical_cmp(&w[1]) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
        Self::new(observations, bounds)
    }
}

impl EncodableBlock for EventObservationsBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::EventObservations
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.observations.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        let mut range: Option<(i64, i64)> = None;
        for observation in &self.observations {
            let time = observation.time();
            let item_range =
                time.observed_interval
                    .map_or((time.sort_ts_us, time.sort_ts_us), |span| {
                        (
                            span.start_us(),
                            span.end_us()
                                .checked_sub(1)
                                .unwrap_or_else(|| span.start_us()),
                        )
                    });
            range = Some(range.map_or(item_range, |(lo, hi)| {
                (lo.min(item_range.0), hi.max(item_range.1))
            }));
        }
        range
    }

    fn encode(&self) -> Vec<u8> {
        if self.observations.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        writer.uvarint(self.observations.len() as u64);
        for observation in &self.observations {
            writer.u32_le(observation.source_type_id());
            write_provenance(&mut writer, observation.provenance());
            writer.u8(shape_code(observation.shape()));
            write_time(&mut writer, observation.time());
            writer.u64_le(observation.occurrence_count());
            writer.u8(evidence_quality_code(observation.evidence_quality()));
            writer.u32_le(observation.quality_flags().0);
            write_loss(&mut writer, observation.loss());
            write_payload(&mut writer, observation.payload(), &self.strings);
        }
        writer.into_bytes()
    }
}

fn collect_payload_text(payload: &ObservationPayload, values: &mut Vec<Box<[u8]>>) {
    let mut retain = |value: Option<&str>| {
        if let Some(value) = value {
            values.push(value.as_bytes().into());
        }
    };
    match payload {
        ObservationPayload::ErrorGroup(value) => {
            retain(value.normalized_pattern.as_deref());
            retain(value.sample.as_deref());
            retain(value.detail.as_deref());
            retain(value.hint.as_deref());
            retain(value.context.as_deref());
            retain(value.statement.as_deref());
            retain(value.database.as_deref());
            retain(value.user.as_deref());
        }
        ObservationPayload::ChildSignalTermination(value)
        | ObservationPayload::ChildProcessCrash(value)
        | ObservationPayload::ShutdownRequested(value)
        | ObservationPayload::ReadyObserved(value) => {
            retain(value.shutdown_mode.as_deref());
            retain(value.message.as_deref());
            retain(value.query_detail.as_deref());
        }
        ObservationPayload::CheckpointStarted(value)
        | ObservationPayload::CheckpointCompleted(value)
        | ObservationPayload::CheckpointTooFrequent(value) => retain(value.reason.as_deref()),
        ObservationPayload::AutovacuumReported(value)
        | ObservationPayload::AutoanalyzeReported(value) => retain(value.relation.as_deref()),
        ObservationPayload::SlowQueryGroup(value) => {
            retain(value.pattern.as_deref());
            retain(value.sample.as_deref());
        }
        ObservationPayload::LockWaitReported(value)
        | ObservationPayload::LockAcquiredAfterWait(value) => {
            retain(value.lock_mode.as_deref());
            retain(value.lock_target.as_deref());
            retain(value.detail.as_deref());
            retain(value.context.as_deref());
            retain(value.statement.as_deref());
        }
        ObservationPayload::TempFileReported(value) => {
            retain(value.path.as_deref());
            retain(value.statement.as_deref());
        }
        ObservationPayload::LogGap(value) => retain(value.source_path.as_deref()),
    }
}

fn payload_is_bounded(payload: &ObservationPayload, bounds: &Bounds) -> bool {
    let valid =
        |value: Option<&str>| value.is_none_or(|value| value.len() as u64 <= bounds.pattern_bytes);
    match payload {
        ObservationPayload::ErrorGroup(value) => {
            valid(value.normalized_pattern.as_deref())
                && valid(value.sample.as_deref())
                && valid(value.detail.as_deref())
                && valid(value.hint.as_deref())
                && valid(value.context.as_deref())
                && valid(value.statement.as_deref())
                && valid(value.database.as_deref())
                && valid(value.user.as_deref())
        }
        ObservationPayload::ChildSignalTermination(value)
        | ObservationPayload::ChildProcessCrash(value)
        | ObservationPayload::ShutdownRequested(value)
        | ObservationPayload::ReadyObserved(value) => {
            valid(value.shutdown_mode.as_deref())
                && valid(value.message.as_deref())
                && valid(value.query_detail.as_deref())
        }
        ObservationPayload::CheckpointStarted(value)
        | ObservationPayload::CheckpointCompleted(value)
        | ObservationPayload::CheckpointTooFrequent(value) => valid(value.reason.as_deref()),
        ObservationPayload::AutovacuumReported(value)
        | ObservationPayload::AutoanalyzeReported(value) => valid(value.relation.as_deref()),
        ObservationPayload::SlowQueryGroup(value) => {
            valid(value.pattern.as_deref()) && valid(value.sample.as_deref())
        }
        ObservationPayload::LockWaitReported(value)
        | ObservationPayload::LockAcquiredAfterWait(value) => {
            valid(value.lock_mode.as_deref())
                && valid(value.lock_target.as_deref())
                && valid(value.detail.as_deref())
                && valid(value.context.as_deref())
                && valid(value.statement.as_deref())
        }
        ObservationPayload::TempFileReported(value) => {
            valid(value.path.as_deref()) && valid(value.statement.as_deref())
        }
        ObservationPayload::LogGap(value) => valid(value.source_path.as_deref()),
    }
}

fn payload_has_valid_sqlstate(payload: &ObservationPayload) -> bool {
    let ObservationPayload::ErrorGroup(value) = payload else {
        return true;
    };
    value.sqlstate.is_none_or(|state| {
        state
            .0
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

const fn is_event_source_type(type_id: u32) -> bool {
    matches!(
        type_id,
        1_022_001
            | 1_024_001
            | 1_025_001
            | 1_026_001
            | 1_027_001
            | 1_028_001
            | 1_029_001
            | 1_030_001
    )
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::{NamingContractId, SourceScopeId};

    use super::super::limits::LIMIT;
    use super::*;

    fn lineage() -> SegmentIdentity {
        SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([2; 16]),
            SegmentLocator([3; 32]),
            7,
            b"descriptor",
        )
    }

    fn provenance(row_ordinal: u32) -> ObservationProvenance {
        ObservationProvenance {
            segment_locator: Some(SegmentLocator([3; 32])),
            section_body_id: SectionBodyId([0xAA; 32]),
            catalog_entry_ordinal: 0,
            row_ordinal,
            dictionary_context_id: DictionaryContextId([0xBB; 32]),
            source_locator: Some(SourceLocator {
                source_unit_id: [0xCC; 32],
                byte_offset: 4_096,
            }),
        }
    }

    fn ready(row: u32) -> EventObservation {
        ready_with_message(row, "database system is ready")
    }

    fn ready_with_message(row: u32, message: &str) -> EventObservation {
        let payload = LifecyclePayload {
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: Some(message.into()),
            query_detail: None,
            dropped_field_count: DroppedFieldCount(0),
        };
        EventObservation::new(
            lineage(),
            1_028_001,
            provenance(row),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: 1_000,
                occurred_at_us: Some(1_000),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ReadyObserved(Box::new(payload)),
            EvidenceQuality::Structured,
            QualityFlags(0b101),
            None,
        )
        .expect("valid ready observation")
    }

    fn error_group(row: u32) -> EventObservation {
        let payload = ErrorGroupPayload {
            severity: Severity::Fatal,
            category: ErrorCategory::Resource,
            sqlstate: Some(SqlState(*b"53300")),
            normalized_pattern: Some("out of memory for query result".into()),
            sample: Some("could not resize shared memory".into()),
            detail: None,
            hint: None,
            context: None,
            statement: None,
            database: Some("postgres".into()),
            user: Some("alice".into()),
            dropped_field_count: DroppedFieldCount(2),
        };
        EventObservation::new(
            lineage(),
            1_022_001,
            provenance(row),
            ObservationShape::GroupedCount,
            ObservationTime {
                sort_ts_us: 2_000,
                occurred_at_us: Some(2_000),
                observed_interval: None,
                quality: TimeQuality::FirstInGroup,
            },
            5,
            ObservationPayload::ErrorGroup(Box::new(payload)),
            EvidenceQuality::Parsed,
            QualityFlags(0),
            Some(LossSummary::new(
                [LossReason::GroupCapExceeded, LossReason::DictionaryBound],
                Some(3),
            )),
        )
        .expect("valid error group")
    }

    fn child_signal(row: u32) -> EventObservation {
        let payload = LifecyclePayload {
            pid: Some(4_321),
            signal: Some(9),
            shutdown_mode: None,
            message: Some("terminated by signal 9: Killed".into()),
            query_detail: None,
            dropped_field_count: DroppedFieldCount(0),
        };
        EventObservation::new(
            lineage(),
            1_028_001,
            provenance(row),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: 3_000,
                occurred_at_us: Some(3_000),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ChildSignalTermination(Box::new(payload)),
            EvidenceQuality::Structured,
            QualityFlags(0),
            None,
        )
        .expect("valid child signal")
    }

    fn child_crash(row: u32) -> EventObservation {
        let payload = LifecyclePayload {
            pid: Some(4_322),
            signal: None,
            shutdown_mode: None,
            message: Some("server process exited with exit code 1".into()),
            query_detail: Some("select 1".into()),
            dropped_field_count: DroppedFieldCount(0),
        };
        EventObservation::new(
            lineage(),
            1_028_001,
            provenance(row),
            ObservationShape::Individual,
            ObservationTime {
                sort_ts_us: 3_500,
                occurred_at_us: Some(3_500),
                observed_interval: None,
                quality: TimeQuality::Exact,
            },
            1,
            ObservationPayload::ChildProcessCrash(Box::new(payload)),
            EvidenceQuality::Structured,
            QualityFlags(0),
            None,
        )
        .expect("valid child crash")
    }

    fn log_gap(row: u32) -> EventObservation {
        let payload = LogGapPayload {
            source_path: Some("/var/log/postgresql/pg.log".into()),
            parser_kind: 2,
            reason: 10,
            dev: Some(2_049),
            inode: Some(123_456),
            offset: Some(9_999),
            bytes_skipped: 4_096,
            truncated_lines: 1,
            invalid_utf8: 0,
            binary_dropped: 0,
            rotations: 1,
            missing_files: 0,
            budget_exhaustions: 0,
            dropped_field_count: DroppedFieldCount(0),
            parser_dropped_lines: 2,
        };
        let interval = CoverageSpan::new(10, 30).expect("valid span");
        EventObservation::new(
            lineage(),
            1_029_001,
            provenance(row),
            ObservationShape::Gap,
            ObservationTime {
                sort_ts_us: 10,
                occurred_at_us: None,
                observed_interval: Some(interval),
                quality: TimeQuality::IntervalOnly,
            },
            1,
            ObservationPayload::LogGap(Box::new(payload)),
            EvidenceQuality::DerivedExact,
            QualityFlags(0),
            None,
        )
        .expect("valid log gap")
    }

    #[test]
    fn observations_round_trip_across_payload_kinds() {
        let block = EventObservationsBlock::new(
            vec![
                ready(1),
                error_group(2),
                child_signal(3),
                child_crash(4),
                log_gap(5),
            ],
            &LIMIT,
        )
        .expect("valid block");
        let decoded = EventObservationsBlock::decode(
            &block.encode(),
            &lineage(),
            block.string_table(),
            &LIMIT,
        )
        .expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.observations().len(), 5);
        assert_eq!(
            payload_tag(child_crash(4).payload()),
            14,
            "new payload tags are append-only"
        );
        assert!(decoded.observations().iter().any(|observation| matches!(
            observation.payload(),
            ObservationPayload::ChildProcessCrash(_)
        )));
    }

    #[test]
    fn a_single_observation_keeps_its_derived_identity() {
        let block = EventObservationsBlock::new(vec![error_group(7)], &LIMIT).expect("valid");
        let decoded = EventObservationsBlock::decode(
            &block.encode(),
            &lineage(),
            block.string_table(),
            &LIMIT,
        )
        .expect("decode");
        assert_eq!(
            decoded.observations()[0].observation_id(),
            error_group(7).observation_id()
        );
    }

    #[test]
    fn observations_reject_a_duplicate_identity() {
        assert_eq!(
            EventObservationsBlock::new(vec![ready(1), ready(1)], &LIMIT),
            Err(BlockError::Duplicate)
        );
    }

    #[test]
    fn a_count_above_the_item_bound_is_rejected() {
        let tight = Bounds {
            items_per_block: 0,
            ..LIMIT
        };
        assert_eq!(
            EventObservationsBlock::new(vec![ready(1)], &tight),
            Err(BlockError::AboveBound)
        );
    }

    #[test]
    fn a_truncated_observation_body_is_rejected() {
        let block = EventObservationsBlock::new(vec![ready(1)], &LIMIT).expect("valid");
        let mut body = block.encode();
        body.truncate(body.len() - 1);
        assert_eq!(
            EventObservationsBlock::decode(&body, &lineage(), block.string_table(), &LIMIT),
            Err(BlockError::Truncated)
        );
    }

    #[test]
    fn trailing_bytes_after_observations_are_rejected() {
        let block = EventObservationsBlock::new(vec![ready(1)], &LIMIT).expect("valid");
        let mut body = block.encode();
        body.push(0);
        assert_eq!(
            EventObservationsBlock::decode(&body, &lineage(), block.string_table(), &LIMIT),
            Err(BlockError::TrailingBytes)
        );
    }

    #[test]
    fn decoding_with_the_wrong_lineage_is_a_reconstruct_error() {
        let block = EventObservationsBlock::new(vec![ready(1)], &LIMIT).expect("valid");
        let wrong = SegmentIdentity::sealed(
            SourceScopeId([1; 32]),
            NamingContractId([2; 16]),
            SegmentLocator([9; 32]),
            7,
            b"descriptor",
        );
        assert_eq!(
            EventObservationsBlock::decode(&block.encode(), &wrong, block.string_table(), &LIMIT),
            Err(BlockError::Reconstruct)
        );
    }

    #[test]
    fn repeated_text_decoding_enforces_the_aggregate_budget() {
        let message = "x".repeat(50 * 1_024);
        let block = EventObservationsBlock::new(
            vec![
                ready_with_message(1, &message),
                ready_with_message(2, &message),
            ],
            &LIMIT,
        )
        .expect("block");
        let tight = Bounds {
            decoded_block_len: 80 * 1_024,
            ..LIMIT
        };
        assert_eq!(
            EventObservationsBlock::decode(
                &block.encode(),
                &lineage(),
                block.string_table(),
                &tight
            ),
            Err(BlockError::AboveBound)
        );
    }
}
