//! The event-observations block and its payload codec.
//!
//! Observations round-trip through [`kronika_analytics::overview::EventObservation`]:
//! the decoder rebuilds each record through the validating constructor, so a
//! decoded observation carries the same derived identity as the encoded one and
//! never bypasses the invariants analytics enforces. The segment lineage is
//! decode context — it is not stored per row — because every observation in a
//! segment shares it.

use kronika_analytics::overview::{
    CheckpointPayload, CoverageSpan, DictionaryContextId, DroppedFieldCount, ErrorCategory,
    ErrorGroupPayload, EventObservation, EvidenceQuality, FiniteF64, LifecyclePayload,
    LockWaitPayload, LogGapPayload, LossReason, LossSummary, MaintenancePayload,
    ObservationPayload, ObservationProvenance, ObservationShape, ObservationTime, QualityFlags,
    SectionBodyId, SegmentIdentity, SegmentLocator, Severity, SlowQueryPayload, SourceLocator,
    SqlState, TempFilePayload, TimeQuality,
};

use super::block::{BlockError, BlockKind, EncodableBlock};
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

fn write_opt_str(writer: &mut ByteWriter, value: Option<&str>) {
    match value {
        Some(inner) => {
            writer.u8(1);
            writer.length_prefixed(inner.as_bytes());
        }
        None => writer.u8(0),
    }
}

fn read_opt_str(reader: &mut ByteReader<'_>, bound: u64) -> Result<Option<Box<str>>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let bytes = reader.length_prefixed(bound)?;
            let text = str::from_utf8(bytes).map_err(|_error| BlockError::Malformed)?;
            Ok(Some(text.into()))
        }
        _ => Err(BlockError::Malformed),
    }
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

fn write_error_group(writer: &mut ByteWriter, payload: &ErrorGroupPayload, bounds: &Bounds) {
    let _ = bounds;
    writer.u8(severity_code(payload.severity));
    writer.u8(category_code(payload.category));
    match payload.sqlstate {
        Some(state) => {
            writer.u8(1);
            writer.bytes(&state.0);
        }
        None => writer.u8(0),
    }
    write_opt_str(writer, payload.normalized_pattern.as_deref());
    write_opt_str(writer, payload.sample.as_deref());
    write_opt_str(writer, payload.detail.as_deref());
    write_opt_str(writer, payload.hint.as_deref());
    write_opt_str(writer, payload.context.as_deref());
    write_opt_str(writer, payload.statement.as_deref());
    write_opt_str(writer, payload.database.as_deref());
    write_opt_str(writer, payload.user.as_deref());
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_error_group(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Box<ErrorGroupPayload>, BlockError> {
    let severity = severity_from(reader.u8()?)?;
    let category = category_from(reader.u8()?)?;
    let sqlstate = match reader.u8()? {
        0 => None,
        1 => Some(SqlState(reader.array()?)),
        _ => return Err(BlockError::Malformed),
    };
    let normalized_pattern = read_opt_str(reader, bounds.pattern_bytes)?;
    let sample = read_opt_str(reader, bounds.decoded_block_len)?;
    let detail = read_opt_str(reader, bounds.decoded_block_len)?;
    let hint = read_opt_str(reader, bounds.decoded_block_len)?;
    let context = read_opt_str(reader, bounds.decoded_block_len)?;
    let statement = read_opt_str(reader, bounds.decoded_block_len)?;
    let database = read_opt_str(reader, bounds.decoded_block_len)?;
    let user = read_opt_str(reader, bounds.decoded_block_len)?;
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

fn write_lifecycle(writer: &mut ByteWriter, payload: &LifecyclePayload) {
    write_opt_i32(writer, payload.pid);
    write_opt_i32(writer, payload.signal);
    write_opt_str(writer, payload.shutdown_mode.as_deref());
    write_opt_str(writer, payload.message.as_deref());
    write_opt_str(writer, payload.query_detail.as_deref());
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_lifecycle(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Box<LifecyclePayload>, BlockError> {
    let pid = read_opt_i32(reader)?;
    let signal = read_opt_i32(reader)?;
    let shutdown_mode = read_opt_str(reader, bounds.decoded_block_len)?;
    let message = read_opt_str(reader, bounds.decoded_block_len)?;
    let query_detail = read_opt_str(reader, bounds.decoded_block_len)?;
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

fn write_checkpoint(writer: &mut ByteWriter, payload: &CheckpointPayload) {
    write_opt_str(writer, payload.reason.as_deref());
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
    bounds: &Bounds,
) -> Result<Box<CheckpointPayload>, BlockError> {
    Ok(Box::new(CheckpointPayload {
        reason: read_opt_str(reader, bounds.decoded_block_len)?,
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

fn write_maintenance(writer: &mut ByteWriter, payload: &MaintenancePayload) {
    write_opt_str(writer, payload.relation.as_deref());
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
    bounds: &Bounds,
) -> Result<Box<MaintenancePayload>, BlockError> {
    Ok(Box::new(MaintenancePayload {
        relation: read_opt_str(reader, bounds.decoded_block_len)?,
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

fn write_slow_query(writer: &mut ByteWriter, payload: &SlowQueryPayload) {
    write_opt_str(writer, payload.pattern.as_deref());
    write_opt_str(writer, payload.sample.as_deref());
    writer.f64_le(payload.max_duration_ms.get());
    writer.f64_le(payload.total_duration_ms.get());
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_slow_query(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Box<SlowQueryPayload>, BlockError> {
    let pattern = read_opt_str(reader, bounds.pattern_bytes)?;
    let sample = read_opt_str(reader, bounds.decoded_block_len)?;
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

fn write_lock_wait(writer: &mut ByteWriter, payload: &LockWaitPayload) {
    write_opt_i32(writer, payload.pid);
    write_opt_str(writer, payload.lock_mode.as_deref());
    write_opt_str(writer, payload.lock_target.as_deref());
    write_opt_f64(writer, payload.duration_ms);
    write_opt_str(writer, payload.detail.as_deref());
    write_opt_str(writer, payload.context.as_deref());
    write_opt_str(writer, payload.statement.as_deref());
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_lock_wait(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Box<LockWaitPayload>, BlockError> {
    Ok(Box::new(LockWaitPayload {
        pid: read_opt_i32(reader)?,
        lock_mode: read_opt_str(reader, bounds.decoded_block_len)?,
        lock_target: read_opt_str(reader, bounds.decoded_block_len)?,
        duration_ms: read_opt_f64(reader)?,
        detail: read_opt_str(reader, bounds.decoded_block_len)?,
        context: read_opt_str(reader, bounds.decoded_block_len)?,
        statement: read_opt_str(reader, bounds.decoded_block_len)?,
        dropped_field_count: DroppedFieldCount(reader.u32_le()?),
    }))
}

fn write_temp_file(writer: &mut ByteWriter, payload: &TempFilePayload) {
    write_opt_str(writer, payload.path.as_deref());
    writer.i64_le(payload.size_bytes);
    write_opt_str(writer, payload.statement.as_deref());
    writer.u32_le(dropped_field_count(payload.dropped_field_count));
}

fn read_temp_file(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Box<TempFilePayload>, BlockError> {
    let path = read_opt_str(reader, bounds.decoded_block_len)?;
    let size_bytes = reader.i64_le()?;
    let statement = read_opt_str(reader, bounds.decoded_block_len)?;
    let dropped_field_count = DroppedFieldCount(reader.u32_le()?);
    Ok(Box::new(TempFilePayload {
        path,
        size_bytes,
        statement,
        dropped_field_count,
    }))
}

fn write_log_gap(writer: &mut ByteWriter, payload: &LogGapPayload) {
    write_opt_str(writer, payload.source_path.as_deref());
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
    bounds: &Bounds,
) -> Result<Box<LogGapPayload>, BlockError> {
    Ok(Box::new(LogGapPayload {
        source_path: read_opt_str(reader, bounds.decoded_block_len)?,
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
    }
}

fn write_payload(writer: &mut ByteWriter, payload: &ObservationPayload, bounds: &Bounds) {
    writer.u8(payload_tag(payload));
    match payload {
        ObservationPayload::ErrorGroup(inner) => write_error_group(writer, inner, bounds),
        ObservationPayload::ChildSignalTermination(inner)
        | ObservationPayload::ShutdownRequested(inner)
        | ObservationPayload::ReadyObserved(inner) => write_lifecycle(writer, inner),
        ObservationPayload::CheckpointStarted(inner)
        | ObservationPayload::CheckpointCompleted(inner)
        | ObservationPayload::CheckpointTooFrequent(inner) => write_checkpoint(writer, inner),
        ObservationPayload::AutovacuumReported(inner)
        | ObservationPayload::AutoanalyzeReported(inner) => write_maintenance(writer, inner),
        ObservationPayload::SlowQueryGroup(inner) => write_slow_query(writer, inner),
        ObservationPayload::LockWaitReported(inner)
        | ObservationPayload::LockAcquiredAfterWait(inner) => write_lock_wait(writer, inner),
        ObservationPayload::TempFileReported(inner) => write_temp_file(writer, inner),
        ObservationPayload::LogGap(inner) => write_log_gap(writer, inner),
    }
}

fn read_payload(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<ObservationPayload, BlockError> {
    let tag = reader.u8()?;
    Ok(match tag {
        0 => ObservationPayload::ErrorGroup(read_error_group(reader, bounds)?),
        1 => ObservationPayload::ChildSignalTermination(read_lifecycle(reader, bounds)?),
        2 => ObservationPayload::ShutdownRequested(read_lifecycle(reader, bounds)?),
        3 => ObservationPayload::ReadyObserved(read_lifecycle(reader, bounds)?),
        4 => ObservationPayload::CheckpointStarted(read_checkpoint(reader, bounds)?),
        5 => ObservationPayload::CheckpointCompleted(read_checkpoint(reader, bounds)?),
        6 => ObservationPayload::CheckpointTooFrequent(read_checkpoint(reader, bounds)?),
        7 => ObservationPayload::AutovacuumReported(read_maintenance(reader, bounds)?),
        8 => ObservationPayload::AutoanalyzeReported(read_maintenance(reader, bounds)?),
        9 => ObservationPayload::SlowQueryGroup(read_slow_query(reader, bounds)?),
        10 => ObservationPayload::LockWaitReported(read_lock_wait(reader, bounds)?),
        11 => ObservationPayload::LockAcquiredAfterWait(read_lock_wait(reader, bounds)?),
        12 => ObservationPayload::TempFileReported(read_temp_file(reader, bounds)?),
        13 => ObservationPayload::LogGap(read_log_gap(reader, bounds)?),
        _ => return Err(BlockError::InvalidEnum),
    })
}

/// A canonical, sorted set of retained observations.
///
/// The canonical order is `(sort_ts_us, observation_id)`. Decoding rebuilds
/// every record through the analytics constructor with the segment lineage
/// supplied as context, so the derived identities match the encoded ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventObservationsBlock {
    observations: Vec<EventObservation>,
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
        if observations.len() as u64 > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
        observations.sort_by(EventObservation::canonical_cmp);
        if observations
            .windows(2)
            .any(|w| w[0].observation_id() == w[1].observation_id())
        {
            return Err(BlockError::Duplicate);
        }
        Ok(Self { observations })
    }

    /// The canonical observations.
    #[must_use]
    pub fn observations(&self) -> &[EventObservation] {
        &self.observations
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
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        let mut reader = ByteReader::new(body);
        let count = reader.uvarint(bounds.items_per_block)?;
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
            let payload = read_payload(&mut reader, bounds)?;
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
        Ok(Self { observations })
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
        let mut iter = self.observations.iter().map(|obs| obs.time().sort_ts_us);
        let first = iter.next()?;
        Some(iter.fold((first, first), |(lo, hi), ts| (lo.min(ts), hi.max(ts))))
    }

    fn encode(&self) -> Vec<u8> {
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
            write_payload(&mut writer, observation.payload(), &super::limits::LIMIT);
        }
        writer.into_bytes()
    }
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
        let payload = LifecyclePayload {
            pid: None,
            signal: None,
            shutdown_mode: None,
            message: Some("database system is ready".into()),
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
            1_099_001,
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
            vec![ready(1), error_group(2), child_signal(3), log_gap(4)],
            &LIMIT,
        )
        .expect("valid block");
        let decoded =
            EventObservationsBlock::decode(&block.encode(), &lineage(), &LIMIT).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.observations().len(), 4);
    }

    #[test]
    fn a_single_observation_keeps_its_derived_identity() {
        let block = EventObservationsBlock::new(vec![error_group(7)], &LIMIT).expect("valid");
        let decoded =
            EventObservationsBlock::decode(&block.encode(), &lineage(), &LIMIT).expect("decode");
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
            EventObservationsBlock::decode(&body, &lineage(), &LIMIT),
            Err(BlockError::Truncated)
        );
    }

    #[test]
    fn trailing_bytes_after_observations_are_rejected() {
        let block = EventObservationsBlock::new(vec![ready(1)], &LIMIT).expect("valid");
        let mut body = block.encode();
        body.push(0);
        assert_eq!(
            EventObservationsBlock::decode(&body, &lineage(), &LIMIT),
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
            EventObservationsBlock::decode(&block.encode(), &wrong, &LIMIT),
            Err(BlockError::Reconstruct)
        );
    }
}
