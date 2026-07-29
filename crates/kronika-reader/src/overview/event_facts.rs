//! Canonical `EVENT_FACTS` block codec.
//!
//! Facts are sorted by `(interval.start, fact_id)`. Text is represented by
//! indices into the file-wide [`StringTableBlock`], so normalized values are
//! retained once even when an observation and its canonical fact both refer to
//! them. Every count and text reference is bounded before allocation.

use kronika_analytics::overview::{
    CapacityFactPayload, CheckpointFactPayload, CounterDeltaFactPayload, CoverageRef, CoverageSpan,
    DroppedFieldCount, EntityKind, EntityRef, ErrorCategory, ErrorFactPayload, EventFact,
    EventKind, EventPayload, EvidenceQuality, FactId, FactShape, FiniteF64, InvalidEventFact,
    LifecycleFactPayload, LockWaitFactPayload, LossReason, LossSummary, MaintenanceFactPayload,
    ObservationId, RetainedExactness, Severity, SlowQueryFactPayload, SqlState,
    StateTransitionFactPayload, TempFileFactPayload,
};

use super::block::{BlockError, BlockKind, EncodableBlock, StringTableBlock};
use super::bytes::{ByteReader, ByteWriter};
use super::limits::Bounds;

/// Canonical sorted facts plus the file-wide text table used by their refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFactsBlock {
    facts: Vec<EventFact>,
    strings: StringTableBlock,
}

impl EventFactsBlock {
    /// Normalizes facts and validates their text references.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::Duplicate`] for duplicate fact IDs,
    /// [`BlockError::AboveBound`] for count/text limits, and
    /// [`BlockError::Malformed`] when the supplied file-wide string table does
    /// not contain a referenced normalized value.
    pub fn new(
        mut facts: Vec<EventFact>,
        strings: &StringTableBlock,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || facts.len() as u64 > bounds.items_per_block {
            return Err(BlockError::AboveBound);
        }
        facts.sort_by(EventFact::canonical_cmp);
        if has_duplicate_fact_id(&facts) {
            return Err(BlockError::Duplicate);
        }
        for fact in &facts {
            validate_fact_text(fact.payload(), strings)?;
            if fact.supporting_observation_ids().len() as u64 > bounds.items_per_block {
                return Err(BlockError::AboveBound);
            }
        }
        Ok(Self {
            facts,
            strings: strings.clone(),
        })
    }

    /// Canonical facts.
    #[must_use]
    pub fn facts(&self) -> &[EventFact] {
        &self.facts
    }

    /// File-wide string table used by this block.
    #[must_use]
    pub const fn string_table(&self) -> &StringTableBlock {
        &self.strings
    }

    /// Consumes the block into canonical facts.
    #[must_use]
    pub fn into_facts(self) -> Vec<EventFact> {
        self.facts
    }

    /// Decodes a canonical fact block against its admitted string table.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for malformed, non-canonical, out-of-bound or
    /// trailing input.
    pub fn decode(
        body: &[u8],
        strings: &StringTableBlock,
        bounds: &Bounds,
    ) -> Result<Self, BlockError> {
        if !bounds.is_within_absolute_limits() || body.len() as u64 > bounds.decoded_block_len {
            return Err(BlockError::AboveBound);
        }
        if body.is_empty() {
            return Ok(Self {
                facts: Vec::new(),
                strings: strings.clone(),
            });
        }
        let mut reader = ByteReader::new(body);
        let count = reader.uvarint(bounds.items_per_block)?;
        let mut facts = Vec::with_capacity(count.min(4_096) as usize);
        let mut evidence_budget = bounds.items_per_block;
        for _ in 0..count {
            facts.push(read_fact(
                &mut reader,
                strings,
                bounds,
                &mut evidence_budget,
            )?);
        }
        reader.finish()?;
        for pair in facts.windows(2) {
            match pair[0].canonical_cmp(&pair[1]) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => return Err(BlockError::Duplicate),
                std::cmp::Ordering::Greater => return Err(BlockError::Unsorted),
            }
        }
        if has_duplicate_fact_id(&facts) {
            return Err(BlockError::Duplicate);
        }
        Ok(Self {
            facts,
            strings: strings.clone(),
        })
    }
}

impl EncodableBlock for EventFactsBlock {
    fn kind(&self) -> BlockKind {
        BlockKind::EventFacts
    }

    fn canonically_sorted(&self) -> bool {
        true
    }

    fn item_count(&self) -> u64 {
        self.facts.len() as u64
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        let first = self.facts.first()?.interval().start_us();
        let last = self
            .facts
            .iter()
            .map(|fact| fact.interval().end_us().saturating_sub(1))
            .max()?;
        Some((first, last))
    }

    fn encode(&self) -> Vec<u8> {
        if self.facts.is_empty() {
            return Vec::new();
        }
        let mut writer = ByteWriter::new();
        writer.uvarint(self.facts.len() as u64);
        for fact in &self.facts {
            write_fact(&mut writer, fact, &self.strings);
        }
        writer.into_bytes()
    }
}

fn write_fact(writer: &mut ByteWriter, fact: &EventFact, strings: &StringTableBlock) {
    writer.bytes(&fact.fact_id().0);
    writer.u16_le(fact.kind().code());
    writer.u8(shape_code(fact.shape()));
    writer.i64_le(fact.interval().start_us());
    writer.i64_le(fact.interval().end_us());
    writer.u64_le(fact.count());
    write_entity(writer, fact.entity());
    writer.u8(evidence_code(fact.evidence_quality()));
    writer.u8(exactness_code(fact.coverage().retained_exactness));
    write_loss(writer, fact.coverage().loss.as_ref());
    writer.uvarint(fact.supporting_observation_ids().len() as u64);
    for observation_id in fact.supporting_observation_ids() {
        writer.bytes(&observation_id.0);
    }
    write_payload(writer, fact.payload(), strings);
}

fn read_fact(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
    bounds: &Bounds,
    evidence_budget: &mut u64,
) -> Result<EventFact, BlockError> {
    let fact_id = FactId(reader.array()?);
    let kind = EventKind::from_code(reader.u16_le()?).ok_or(BlockError::InvalidEnum)?;
    let shape = shape_from(reader.u8()?)?;
    let from_us = reader.i64_le()?;
    let to_us = reader.i64_le()?;
    let interval = CoverageSpan::new(from_us, to_us).ok_or(BlockError::Malformed)?;
    let count = reader.u64_le()?;
    let entity = read_entity(reader)?;
    let evidence_quality = evidence_from(reader.u8()?)?;
    let retained_exactness = exactness_from(reader.u8()?)?;
    let loss = read_loss(reader, bounds)?;
    let supporting_count = reader.uvarint(*evidence_budget)?;
    *evidence_budget = evidence_budget
        .checked_sub(supporting_count)
        .ok_or(BlockError::AboveBound)?;
    let mut supporting = Vec::with_capacity(supporting_count.min(4_096) as usize);
    for _ in 0..supporting_count {
        supporting.push(ObservationId(reader.array()?));
    }
    let payload = read_payload(reader, strings)?;
    EventFact::new(
        fact_id,
        kind,
        shape,
        interval,
        count,
        entity,
        payload,
        supporting,
        evidence_quality,
        CoverageRef {
            retained_exactness,
            loss,
        },
    )
    .map_err(map_invalid_fact)
}

const fn map_invalid_fact(error: InvalidEventFact) -> BlockError {
    match error {
        InvalidEventFact::NonCanonicalSupportingEvidence => BlockError::Unsorted,
        InvalidEventFact::MissingSupportingEvidence
        | InvalidEventFact::ZeroCount
        | InvalidEventFact::IndividualCountNotOne
        | InvalidEventFact::TimestampOverflow
        | InvalidEventFact::SemanticMismatch => BlockError::Reconstruct,
    }
}

fn has_duplicate_fact_id(facts: &[EventFact]) -> bool {
    let mut ids = facts.iter().map(EventFact::fact_id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.windows(2).any(|pair| pair[0] == pair[1])
}

const fn shape_code(shape: FactShape) -> u8 {
    match shape {
        FactShape::Individual => 0,
        FactShape::GroupedCount => 1,
        FactShape::Interval => 2,
    }
}

const fn shape_from(code: u8) -> Result<FactShape, BlockError> {
    match code {
        0 => Ok(FactShape::Individual),
        1 => Ok(FactShape::GroupedCount),
        2 => Ok(FactShape::Interval),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn evidence_code(value: EvidenceQuality) -> u8 {
    match value {
        EvidenceQuality::Structured => 0,
        EvidenceQuality::Parsed => 1,
        EvidenceQuality::Heuristic => 2,
        EvidenceQuality::DerivedExact => 3,
    }
}

const fn evidence_from(code: u8) -> Result<EvidenceQuality, BlockError> {
    match code {
        0 => Ok(EvidenceQuality::Structured),
        1 => Ok(EvidenceQuality::Parsed),
        2 => Ok(EvidenceQuality::Heuristic),
        3 => Ok(EvidenceQuality::DerivedExact),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn exactness_code(value: RetainedExactness) -> u8 {
    match value {
        RetainedExactness::Exact => 0,
        RetainedExactness::LowerBound => 1,
        RetainedExactness::Unknown => 2,
    }
}

const fn exactness_from(code: u8) -> Result<RetainedExactness, BlockError> {
    match code {
        0 => Ok(RetainedExactness::Exact),
        1 => Ok(RetainedExactness::LowerBound),
        2 => Ok(RetainedExactness::Unknown),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_entity(writer: &mut ByteWriter, entity: Option<EntityRef>) {
    match entity {
        Some(entity) => {
            writer.u8(1);
            writer.u8(entity.kind.code());
            writer.bytes(&entity.id);
        }
        None => writer.u8(0),
    }
}

fn read_entity(reader: &mut ByteReader<'_>) -> Result<Option<EntityRef>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let kind = EntityKind::from_code(reader.u8()?).ok_or(BlockError::InvalidEnum)?;
            Ok(Some(EntityRef {
                kind,
                id: reader.array()?,
            }))
        }
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_loss(writer: &mut ByteWriter, loss: Option<&LossSummary>) {
    match loss {
        Some(loss) => {
            writer.u8(1);
            writer.uvarint(loss.reasons().len() as u64);
            for reason in loss.reasons() {
                writer.u8(reason.code());
            }
            write_optional_u64(writer, loss.lost_count_lower_bound);
        }
        None => writer.u8(0),
    }
}

fn read_loss(
    reader: &mut ByteReader<'_>,
    bounds: &Bounds,
) -> Result<Option<LossSummary>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let count = reader.uvarint(bounds.items_per_block)?;
            let mut reasons = Vec::with_capacity(count.min(64) as usize);
            for _ in 0..count {
                reasons.push(LossReason::from_code(reader.u8()?).ok_or(BlockError::InvalidEnum)?);
            }
            if reasons.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(BlockError::Unsorted);
            }
            Ok(Some(LossSummary::new(reasons, read_optional_u64(reader)?)))
        }
        _ => Err(BlockError::InvalidEnum),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the match is the complete stable payload schema"
)]
fn write_payload(writer: &mut ByteWriter, payload: &EventPayload, strings: &StringTableBlock) {
    match payload {
        EventPayload::Error(value) => {
            writer.u8(1);
            writer.u8(severity_code(value.severity));
            writer.u8(category_code(value.category));
            write_sqlstate(writer, value.sqlstate);
            write_text_ref(writer, value.normalized_pattern.as_deref(), strings);
            write_text_ref(writer, value.database.as_deref(), strings);
            write_text_ref(writer, value.user.as_deref(), strings);
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::Lifecycle(value) => {
            writer.u8(2);
            write_optional_i32(writer, value.pid);
            write_optional_i32(writer, value.signal);
            write_text_ref(writer, value.shutdown_mode.as_deref(), strings);
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::Checkpoint(value) => {
            writer.u8(3);
            write_text_ref(writer, value.reason.as_deref(), strings);
            write_optional_i64(writer, value.seconds_apart);
            write_optional_i64(writer, value.buffers_written);
            write_optional_f64(writer, value.write_ms);
            write_optional_f64(writer, value.sync_ms);
            write_optional_f64(writer, value.total_ms);
            write_optional_i64(writer, value.distance_kb);
            write_optional_i64(writer, value.estimate_kb);
            write_optional_i64(writer, value.wal_added);
            write_optional_i64(writer, value.wal_removed);
            write_optional_i64(writer, value.wal_recycled);
            write_optional_i64(writer, value.sync_files);
            write_optional_f64(writer, value.longest_sync_ms);
            write_optional_f64(writer, value.average_sync_ms);
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::Maintenance(value) => {
            writer.u8(4);
            write_text_ref(writer, value.relation.as_deref(), strings);
            for field in [
                value.index_scans,
                value.pages_removed,
                value.pages_remaining,
                value.tuples_removed,
                value.tuples_remaining,
                value.tuples_dead_not_removable,
            ] {
                write_optional_i64(writer, field);
            }
            write_optional_f64(writer, value.elapsed_ms);
            for field in [value.buffer_hits, value.buffer_misses, value.buffer_dirtied] {
                write_optional_i64(writer, field);
            }
            write_optional_f64(writer, value.avg_read_rate_mbs);
            write_optional_f64(writer, value.avg_write_rate_mbs);
            write_optional_f64(writer, value.cpu_user_ms);
            write_optional_f64(writer, value.cpu_system_ms);
            for field in [value.wal_records, value.wal_fpi, value.wal_bytes] {
                write_optional_i64(writer, field);
            }
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::SlowQuery(value) => {
            writer.u8(5);
            write_text_ref(writer, value.pattern.as_deref(), strings);
            writer.f64_le(value.max_duration_ms.get());
            writer.f64_le(value.total_duration_ms.get());
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::LockWait(value) => {
            writer.u8(6);
            write_optional_i32(writer, value.pid);
            write_text_ref(writer, value.lock_mode.as_deref(), strings);
            write_text_ref(writer, value.lock_target.as_deref(), strings);
            write_optional_f64(writer, value.duration_ms);
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::TempFile(value) => {
            writer.u8(7);
            writer.i64_le(value.size_bytes);
            writer.u32_le(value.dropped_field_count.0);
        }
        EventPayload::CounterDelta(value) => {
            writer.u8(8);
            writer.u32_le(value.factor_id.0);
            writer.u64_le(value.delta);
            writer.u64_le(value.duration_us);
            writer.u64_le(value.reset_epoch);
        }
        EventPayload::StateTransition(value) => {
            writer.u8(9);
            writer.u32_le(value.factor_id.0);
            writer.u32_le(value.previous_state);
            writer.u32_le(value.current_state);
            writer.u64_le(value.population_total);
        }
        EventPayload::Capacity(value) => {
            writer.u8(10);
            writer.u64_le(value.total_bytes);
            writer.u64_le(value.available_bytes);
        }
        EventPayload::Marker => writer.u8(11),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the match is the complete stable payload schema"
)]
fn read_payload(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
) -> Result<EventPayload, BlockError> {
    let payload = match reader.u8()? {
        1 => EventPayload::Error(Box::new(ErrorFactPayload {
            severity: severity_from(reader.u8()?)?,
            category: category_from(reader.u8()?)?,
            sqlstate: read_sqlstate(reader)?,
            normalized_pattern: read_text_ref(reader, strings)?,
            database: read_text_ref(reader, strings)?,
            user: read_text_ref(reader, strings)?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        })),
        2 => EventPayload::Lifecycle(Box::new(LifecycleFactPayload {
            pid: read_optional_i32(reader)?,
            signal: read_optional_i32(reader)?,
            shutdown_mode: read_text_ref(reader, strings)?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        })),
        3 => EventPayload::Checkpoint(Box::new(CheckpointFactPayload {
            reason: read_text_ref(reader, strings)?,
            seconds_apart: read_optional_i64(reader)?,
            buffers_written: read_optional_i64(reader)?,
            write_ms: read_optional_f64(reader)?,
            sync_ms: read_optional_f64(reader)?,
            total_ms: read_optional_f64(reader)?,
            distance_kb: read_optional_i64(reader)?,
            estimate_kb: read_optional_i64(reader)?,
            wal_added: read_optional_i64(reader)?,
            wal_removed: read_optional_i64(reader)?,
            wal_recycled: read_optional_i64(reader)?,
            sync_files: read_optional_i64(reader)?,
            longest_sync_ms: read_optional_f64(reader)?,
            average_sync_ms: read_optional_f64(reader)?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        })),
        4 => EventPayload::Maintenance(Box::new(MaintenanceFactPayload {
            relation: read_text_ref(reader, strings)?,
            index_scans: read_optional_i64(reader)?,
            pages_removed: read_optional_i64(reader)?,
            pages_remaining: read_optional_i64(reader)?,
            tuples_removed: read_optional_i64(reader)?,
            tuples_remaining: read_optional_i64(reader)?,
            tuples_dead_not_removable: read_optional_i64(reader)?,
            elapsed_ms: read_optional_f64(reader)?,
            buffer_hits: read_optional_i64(reader)?,
            buffer_misses: read_optional_i64(reader)?,
            buffer_dirtied: read_optional_i64(reader)?,
            avg_read_rate_mbs: read_optional_f64(reader)?,
            avg_write_rate_mbs: read_optional_f64(reader)?,
            cpu_user_ms: read_optional_f64(reader)?,
            cpu_system_ms: read_optional_f64(reader)?,
            wal_records: read_optional_i64(reader)?,
            wal_fpi: read_optional_i64(reader)?,
            wal_bytes: read_optional_i64(reader)?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        })),
        5 => EventPayload::SlowQuery(Box::new(SlowQueryFactPayload {
            pattern: read_text_ref(reader, strings)?,
            max_duration_ms: FiniteF64::new(reader.f64_finite()?)
                .ok_or(BlockError::NonFiniteFloat)?,
            total_duration_ms: FiniteF64::new(reader.f64_finite()?)
                .ok_or(BlockError::NonFiniteFloat)?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        })),
        6 => EventPayload::LockWait(Box::new(LockWaitFactPayload {
            pid: read_optional_i32(reader)?,
            lock_mode: read_text_ref(reader, strings)?,
            lock_target: read_text_ref(reader, strings)?,
            duration_ms: read_optional_f64(reader)?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        })),
        7 => EventPayload::TempFile(TempFileFactPayload {
            size_bytes: reader.i64_le()?,
            dropped_field_count: DroppedFieldCount(reader.u32_le()?),
        }),
        8 => EventPayload::CounterDelta(CounterDeltaFactPayload {
            factor_id: kronika_analytics::overview::FactorId(reader.u32_le()?),
            delta: reader.u64_le()?,
            duration_us: reader.u64_le()?,
            reset_epoch: reader.u64_le()?,
        }),
        9 => EventPayload::StateTransition(StateTransitionFactPayload {
            factor_id: kronika_analytics::overview::FactorId(reader.u32_le()?),
            previous_state: reader.u32_le()?,
            current_state: reader.u32_le()?,
            population_total: reader.u64_le()?,
        }),
        10 => EventPayload::Capacity(CapacityFactPayload {
            total_bytes: reader.u64_le()?,
            available_bytes: reader.u64_le()?,
        }),
        11 => EventPayload::Marker,
        _ => return Err(BlockError::InvalidEnum),
    };
    Ok(payload)
}

fn validate_fact_text(
    payload: &EventPayload,
    strings: &StringTableBlock,
) -> Result<(), BlockError> {
    let values: &[&Option<Box<str>>] = match payload {
        EventPayload::Error(value) => &[&value.normalized_pattern, &value.database, &value.user],
        EventPayload::Lifecycle(value) => &[&value.shutdown_mode],
        EventPayload::Checkpoint(value) => &[&value.reason],
        EventPayload::Maintenance(value) => &[&value.relation],
        EventPayload::SlowQuery(value) => &[&value.pattern],
        EventPayload::LockWait(value) => &[&value.lock_mode, &value.lock_target],
        EventPayload::TempFile(_)
        | EventPayload::CounterDelta(_)
        | EventPayload::StateTransition(_)
        | EventPayload::Capacity(_)
        | EventPayload::Marker => &[],
    };
    for value in values.iter().filter_map(|value| value.as_deref()) {
        if string_index(strings, value.as_bytes()).is_none() {
            return Err(BlockError::Malformed);
        }
    }
    Ok(())
}

fn write_text_ref(writer: &mut ByteWriter, value: Option<&str>, strings: &StringTableBlock) {
    match value {
        Some(value) => {
            writer.u8(1);
            let index = string_index(strings, value.as_bytes())
                .expect("EventFactsBlock constructor validates text references");
            writer.uvarint(index as u64);
        }
        None => writer.u8(0),
    }
}

fn read_text_ref(
    reader: &mut ByteReader<'_>,
    strings: &StringTableBlock,
) -> Result<Option<Box<str>>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let maximum = strings.values().len().saturating_sub(1) as u64;
            let index = usize::try_from(reader.uvarint(maximum)?)
                .map_err(|_error| BlockError::AboveBound)?;
            let bytes = strings.values().get(index).ok_or(BlockError::Malformed)?;
            let text = std::str::from_utf8(bytes).map_err(|_error| BlockError::Malformed)?;
            Ok(Some(text.into()))
        }
        _ => Err(BlockError::InvalidEnum),
    }
}

fn string_index(strings: &StringTableBlock, value: &[u8]) -> Option<usize> {
    strings
        .values()
        .binary_search_by(|candidate| candidate.as_ref().cmp(value))
        .ok()
}

fn write_optional_i32(writer: &mut ByteWriter, value: Option<i32>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.i32_le(value);
        }
        None => writer.u8(0),
    }
}

fn read_optional_i32(reader: &mut ByteReader<'_>) -> Result<Option<i32>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.i32_le()?)),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_optional_i64(writer: &mut ByteWriter, value: Option<i64>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.i64_le(value);
        }
        None => writer.u8(0),
    }
}

fn read_optional_i64(reader: &mut ByteReader<'_>) -> Result<Option<i64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.i64_le()?)),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_optional_u64(writer: &mut ByteWriter, value: Option<u64>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.u64_le(value);
        }
        None => writer.u8(0),
    }
}

fn read_optional_u64(reader: &mut ByteReader<'_>) -> Result<Option<u64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64_le()?)),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_optional_f64(writer: &mut ByteWriter, value: Option<FiniteF64>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.f64_le(value.get());
        }
        None => writer.u8(0),
    }
}

fn read_optional_f64(reader: &mut ByteReader<'_>) -> Result<Option<FiniteF64>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => FiniteF64::new(reader.f64_finite()?)
            .map(Some)
            .ok_or(BlockError::NonFiniteFloat),
        _ => Err(BlockError::InvalidEnum),
    }
}

fn write_sqlstate(writer: &mut ByteWriter, value: Option<SqlState>) {
    match value {
        Some(value) => {
            writer.u8(1);
            writer.bytes(&value.0);
        }
        None => writer.u8(0),
    }
}

fn read_sqlstate(reader: &mut ByteReader<'_>) -> Result<Option<SqlState>, BlockError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(SqlState(reader.array()?))),
        _ => Err(BlockError::InvalidEnum),
    }
}

const fn severity_code(value: Severity) -> u8 {
    match value {
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

const fn category_code(value: ErrorCategory) -> u8 {
    match value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::limits::LIMIT;

    fn fact(id: u8, ts: i64, text: Option<&str>) -> EventFact {
        EventFact::new(
            FactId([id; 32]),
            EventKind::PgLifecycleShutdownRequested,
            FactShape::Individual,
            CoverageSpan::new(ts, ts + 1).expect("interval"),
            1,
            None,
            EventPayload::Lifecycle(Box::new(LifecycleFactPayload {
                pid: Some(42),
                signal: None,
                shutdown_mode: text.map(Into::into),
                dropped_field_count: DroppedFieldCount(0),
            })),
            vec![ObservationId([id; 32])],
            EvidenceQuality::Parsed,
            CoverageRef {
                retained_exactness: RetainedExactness::Exact,
                loss: None,
            },
        )
        .expect("fact")
    }

    #[test]
    fn event_facts_round_trip_in_canonical_order() {
        let strings = StringTableBlock::new(vec![b"smart".to_vec().into_boxed_slice()], &LIMIT)
            .expect("strings");
        let block = EventFactsBlock::new(
            vec![fact(2, 20, None), fact(1, 10, Some("smart"))],
            &strings,
            &LIMIT,
        )
        .expect("block");
        let decoded = EventFactsBlock::decode(&block.encode(), &strings, &LIMIT).expect("decode");
        assert_eq!(decoded, block);
        assert_eq!(decoded.facts()[0].interval().start_us(), 10);
    }

    #[test]
    fn missing_text_reference_is_rejected() {
        let strings = StringTableBlock::new(Vec::new(), &LIMIT).expect("strings");
        assert_eq!(
            EventFactsBlock::new(vec![fact(1, 10, Some("smart"))], &strings, &LIMIT),
            Err(BlockError::Malformed)
        );
    }

    #[test]
    fn duplicate_fact_identity_is_rejected() {
        let strings = StringTableBlock::new(Vec::new(), &LIMIT).expect("strings");
        assert_eq!(
            EventFactsBlock::new(
                vec![fact(1, 10, None), fact(2, 15, None), fact(1, 20, None)],
                &strings,
                &LIMIT
            ),
            Err(BlockError::Duplicate)
        );
    }

    #[test]
    fn decode_rejects_an_interleaved_duplicate_fact_identity() {
        let strings = StringTableBlock::new(Vec::new(), &LIMIT).expect("strings");
        let facts = [fact(1, 10, None), fact(2, 15, None), fact(1, 20, None)];
        let mut writer = ByteWriter::new();
        writer.uvarint(facts.len() as u64);
        for fact in &facts {
            write_fact(&mut writer, fact, &strings);
        }
        assert_eq!(
            EventFactsBlock::decode(&writer.into_bytes(), &strings, &LIMIT),
            Err(BlockError::Duplicate)
        );
    }
}
