//! Typed log-event input and the self-contained event-fact lenses.
//!
//! The event branch is separate from the numeric series path: a lens here reads
//! bounded, typed records the log source already grouped, never an anomaly
//! episode. Lenses report observed events, infer nothing from absence, and do
//! not restate a logged fact as a cause. SQLSTATE-like tokens from stderr are heuristic evidence:
//! the current source cannot prove a structured server error code.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::dispatch::{LimitAxis, LimitHit, WorkBudget};
use super::engine::{EngineSkip, finding_order};
use super::evidence::sink::{FindingSink, OutputCounts, OutputLimits};
use super::evidence::{ConfidenceCap, Evidence, Finding, FindingDraft, FindingScope, Role};
use super::model::IdentityValue;

const PG_LOG_ERRORS: &str = "pg_log_errors";
const PG_LOG_LIFECYCLE: &str = "pg_log_lifecycle";

/// Source severity taxonomy: `2` is `PANIC`.
const SEVERITY_PANIC: u8 = 2;
/// Signal 9 (`SIGKILL`) cannot be caught, so it is an external kill, not a fault.
const SIGKILL: i32 = 9;

const SQLSTATE_DISK_FULL: &str = "53100";
const SQLSTATE_OUT_OF_MEMORY: &str = "53200";
const SQLSTATE_TOO_MANY_CONNECTIONS: &str = "53300";
const SQLSTATE_DEADLOCK: &str = "40P01";
const SQLSTATE_DATA_CORRUPTED: &str = "XX001";
const SQLSTATE_INDEX_CORRUPTED: &str = "XX002";

/// Versioned public metadata for one bounded log-evidence branch.
pub(crate) struct EventCatalogEntry {
    pub lens_id: &'static str,
    pub slug: &'static str,
    pub question: &'static str,
}

const EVENT_CATALOG_METADATA: &[EventCatalogEntry] = &[
    EventCatalogEntry {
        lens_id: "PG-EVT-001",
        slug: "server_child_sigkill",
        question: "Зафиксировал ли stderr завершение процесса PostgreSQL сигналом 9?",
    },
    EventCatalogEntry {
        lens_id: "PG-EVT-002",
        slug: "server_child_signal_termination",
        question: "Зафиксировал ли stderr аварийное завершение процесса PostgreSQL сигналом?",
    },
    EventCatalogEntry {
        lens_id: "PG-EVT-003",
        slug: "panic_severity_observation",
        question: "Наблюдалась ли в stderr запись с уровнем PANIC?",
    },
    EventCatalogEntry {
        lens_id: "OS-FS-027",
        slug: "filesystem_space",
        question: "Есть ли признак отказа из-за нехватки пространства или связанного лимита?",
    },
    EventCatalogEntry {
        lens_id: "PG-EVT-005",
        slug: "postgres_out_of_memory_observation",
        question: "Наблюдался ли в stderr признак ошибки PostgreSQL out_of_memory?",
    },
    EventCatalogEntry {
        lens_id: "PG-CONN-014",
        slug: "connection_saturation",
        question: "Есть ли признак отклонённого подключения из-за лимита соединений?",
    },
    EventCatalogEntry {
        lens_id: "PG-EVT-007",
        slug: "deadlock_observation",
        question: "Наблюдался ли в stderr признак обнаруженного PostgreSQL deadlock?",
    },
    EventCatalogEntry {
        lens_id: "PG-EVT-008",
        slug: "corruption_sqlstate_observation",
        question: "Наблюдался ли в stderr признак SQLSTATE XX001 или XX002?",
    },
];

pub(crate) const fn event_catalog_metadata() -> &'static [EventCatalogEntry] {
    EVENT_CATALOG_METADATA
}

/// How much of a log section the request actually saw.
///
/// There is no `Complete`: effective `PostgreSQL` logging configuration and the
/// full byte sequence are never proven from the stored rows, so absence of an
/// event can never be read as absence of the condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogCoverage {
    /// Read cleanly, but full coverage is unproven; absence means nothing.
    Unknown,
    /// A coverage gap intersects the window; absence never crosses it.
    Gap,
    /// Absent, disabled, unsupported, or skipped: no rows, not a measured zero.
    NotCollected,
}

impl LogCoverage {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Gap => "gap",
            Self::NotCollected => "not_collected",
        }
    }
}

/// A grouped `pg_log_errors` row: a count of occurrences sharing one
/// `(severity, sqlstate)`. It is not a per-event entity, so per-backend identity
/// and ordering cannot be recovered from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogErrorGroup {
    observed_at_us: i64,
    severity: u8,
    sqlstate: Option<String>,
    count: u64,
}

impl LogErrorGroup {
    /// A non-conforming SQLSTATE-like token is dropped to `None`. Syntax alone
    /// does not make the stderr token a structured server error code.
    pub(crate) fn new(
        observed_at_us: i64,
        severity: u8,
        sqlstate: Option<String>,
        count: u64,
    ) -> Self {
        Self {
            observed_at_us,
            severity,
            sqlstate: sqlstate.filter(|code| is_sqlstate(code)),
            count,
        }
    }

    const fn severity(&self) -> u8 {
        self.severity
    }

    fn sqlstate(&self) -> Option<&str> {
        self.sqlstate.as_deref()
    }

    const fn count(&self) -> u64 {
        self.count
    }

    const fn observed_at_us(&self) -> i64 {
        self.observed_at_us
    }
}

/// A `pg_log_lifecycle` record: a start, signal, or stop the postmaster logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleKind {
    Crash,
    Shutdown,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LifecycleEvent {
    observed_at_us: i64,
    pid: Option<i64>,
    kind: LifecycleKind,
    signal: Option<i32>,
}

impl LifecycleEvent {
    pub(crate) const fn new(
        observed_at_us: i64,
        pid: Option<i64>,
        kind: LifecycleKind,
        signal: Option<i32>,
    ) -> Self {
        Self {
            observed_at_us,
            pid,
            kind,
            signal,
        }
    }

    const fn kind(&self) -> LifecycleKind {
        self.kind
    }

    const fn signal(&self) -> Option<i32> {
        self.signal
    }

    const fn observed_at_us(&self) -> i64 {
        self.observed_at_us
    }

    const fn pid(&self) -> Option<i64> {
        self.pid
    }
}

/// A five-character `PostgreSQL` SQLSTATE: digits and uppercase letters only.
fn is_sqlstate(code: &str) -> bool {
    code.len() == 5
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// Request ceilings for retained event rows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EventInputLimits {
    max_error_groups: usize,
    max_lifecycle_events: usize,
}

impl EventInputLimits {
    pub(crate) const fn production() -> Self {
        Self {
            max_error_groups: 4_096,
            max_lifecycle_events: 4_096,
        }
    }

    #[cfg(test)]
    pub(crate) const fn with(max_error_groups: usize, max_lifecycle_events: usize) -> Self {
        Self {
            max_error_groups,
            max_lifecycle_events,
        }
    }
}

/// Bounded, typed log events for one request, plus per-section coverage.
///
/// `overflow` latches when a section reached its row ceiling: the event set is
/// then incomplete, and no consumer may treat it as exhaustive.
pub(crate) struct LogEventInputs {
    errors: Vec<LogErrorGroup>,
    lifecycle: Vec<LifecycleEvent>,
    coverage: BTreeMap<&'static str, LogCoverage>,
    limits: EventInputLimits,
    overflow: bool,
}

impl LogEventInputs {
    pub(crate) const fn new(limits: EventInputLimits) -> Self {
        Self {
            errors: Vec::new(),
            lifecycle: Vec::new(),
            coverage: BTreeMap::new(),
            limits,
            overflow: false,
        }
    }

    /// Record one grouped error, or drop it and latch `overflow` at the ceiling.
    pub(crate) fn push_error(&mut self, group: LogErrorGroup) -> bool {
        if self.errors.len() >= self.limits.max_error_groups {
            self.overflow = true;
            return false;
        }
        self.errors.push(group);
        true
    }

    /// Record one lifecycle event, or drop it and latch `overflow` at the ceiling.
    pub(crate) fn push_lifecycle(&mut self, event: LifecycleEvent) -> bool {
        if self.lifecycle.len() >= self.limits.max_lifecycle_events {
            self.overflow = true;
            return false;
        }
        self.lifecycle.push(event);
        true
    }

    pub(crate) fn set_coverage(&mut self, section: &'static str, coverage: LogCoverage) {
        self.coverage.insert(section, coverage);
    }

    fn errors(&self) -> &[LogErrorGroup] {
        &self.errors
    }

    fn lifecycle(&self) -> &[LifecycleEvent] {
        &self.lifecycle
    }

    pub(crate) const fn coverage(&self) -> &BTreeMap<&'static str, LogCoverage> {
        &self.coverage
    }

    pub(crate) const fn overflow(&self) -> bool {
        self.overflow
    }
}

/// A pure lens over bounded, typed log events. Output passes through `sink`, so
/// the same evidence gate and work budget bound it as the numeric lenses.
pub(crate) trait EventLens {
    fn id(&self) -> &'static str;
    fn confidence_cap(&self) -> ConfidenceCap;
    fn evaluate(&self, events: &LogEventInputs, sink: &mut FindingSink<'_>)
    -> Result<(), LimitHit>;
}

/// A lifecycle template is still parsed from configurable stderr text; it is
/// positive evidence but cannot justify exact/high confidence.
fn typed_occurrence() -> Vec<Evidence> {
    vec![Evidence::Event]
}

/// stderr severity and SQLSTATE-like tokens are parser observations, not a
/// structured error code. They therefore cannot justify high confidence.
fn stderr_observation() -> Vec<Evidence> {
    vec![Evidence::Event]
}

/// Emit coincident occurrence facts for the selected SQLSTATEs.
///
/// Groups with the same code and observation time produce one fact. Counts are
/// omitted because a batch timestamp does not describe every grouped record.
fn emit_error_sqlstate(
    events: &LogEventInputs,
    sink: &mut FindingSink<'_>,
    codes: &[&'static str],
) -> Result<(), LimitHit> {
    sink.charge_points(events.errors().len())?;
    let mut emitted = BTreeSet::new();
    for group in events.errors() {
        let Some(sqlstate) = group.sqlstate() else {
            continue;
        };
        if group.count() == 0 || !codes.contains(&sqlstate) {
            continue;
        }
        if !emitted.insert((sqlstate, group.observed_at_us())) {
            continue;
        }
        let identity: Arc<[IdentityValue]> = Arc::from(vec![
            IdentityValue::Text(sqlstate.to_owned()),
            IdentityValue::I64(group.observed_at_us()),
        ]);
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_parts(PG_LOG_ERRORS, "sqlstate", identity),
            stderr_observation(),
        ))?;
    }
    Ok(())
}

/// Emit one fact per unique matching `(time, pid, signal)` crash record.
fn emit_crash_signal(
    events: &LogEventInputs,
    sink: &mut FindingSink<'_>,
    matches: impl Fn(i32) -> bool,
) -> Result<(), LimitHit> {
    sink.charge_points(events.lifecycle().len())?;
    let mut emitted = BTreeSet::new();
    for event in events.lifecycle() {
        if event.kind() != LifecycleKind::Crash {
            continue;
        }
        let Some(signal) = event.signal() else {
            continue;
        };
        if !matches(signal) {
            continue;
        }
        if !emitted.insert((event.observed_at_us(), event.pid(), signal)) {
            continue;
        }
        let identity: Arc<[IdentityValue]> = Arc::from(vec![
            IdentityValue::I64(event.observed_at_us()),
            event
                .pid()
                .map_or(IdentityValue::Bool(false), IdentityValue::I64),
            IdentityValue::I64(i64::from(signal)),
        ]);
        sink.emit(FindingDraft::new(
            Role::Coincident,
            FindingScope::from_parts(PG_LOG_LIFECYCLE, "signal", identity),
            typed_occurrence(),
        ))?;
    }
    Ok(())
}

/// `PG-EVT-001` (`backend_sigkill`): stderr reports signal 9 termination. It
/// does not prove kernel OOM, which needs
/// a victim record this source does not carry; `kill -9`, a watchdog, or a
/// container runtime look identical here.
pub(crate) struct BackendSigkillLens;

impl BackendSigkillLens {
    const ID: &'static str = "PG-EVT-001";
}

impl EventLens for BackendSigkillLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_crash_signal(events, sink, |signal| signal == SIGKILL)
    }
}

/// `PG-EVT-002` (`backend_crash`): stderr reports a process terminated by a
/// signal other than `SIGKILL`. It does not identify the cause: an
/// extension bug, assertion, hardware fault, or admin signal all reach here, and
/// the terminated child is not necessarily a client backend.
pub(crate) struct BackendCrashLens;

impl BackendCrashLens {
    const ID: &'static str = "PG-EVT-002";
}

impl EventLens for BackendCrashLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_crash_signal(events, sink, |signal| signal != SIGKILL)
    }
}

/// `PG-EVT-003` (`panic_shutdown`): stderr contains a parsed `PANIC` severity.
/// This does not prove data corruption or a completed outage: `PANIC`
/// aborts all sessions, but its cause ranges over I/O, shared-memory, and
/// internal-invariant failures.
pub(crate) struct PanicShutdownLens;

impl PanicShutdownLens {
    const ID: &'static str = "PG-EVT-003";
}

impl EventLens for PanicShutdownLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        sink.charge_points(events.errors().len())?;
        let mut emitted = BTreeSet::new();
        for group in events.errors() {
            if group.severity() != SEVERITY_PANIC || group.count() == 0 {
                continue;
            }
            if !emitted.insert(group.observed_at_us()) {
                continue;
            }
            let identity: Arc<[IdentityValue]> =
                Arc::from(vec![IdentityValue::I64(group.observed_at_us())]);
            sink.emit(FindingDraft::new(
                Role::Coincident,
                FindingScope::from_parts(PG_LOG_ERRORS, "severity_panic", identity),
                stderr_observation(),
            ))?;
        }
        Ok(())
    }
}

/// Log branch of `OS-FS-027`: a stderr token resembles SQLSTATE `53100`.
/// It does not identify a mount or distinguish space, inode, quota, or shared
/// memory exhaustion.
pub(crate) struct DiskFullLogLens;

impl DiskFullLogLens {
    const ID: &'static str = "OS-FS-027";
}

impl EventLens for DiskFullLogLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_error_sqlstate(events, sink, &[SQLSTATE_DISK_FULL])
    }
}

/// `PG-EVT-005` (`out_of_memory_log`): stderr contains a token resembling
/// `53200` (out of memory). This does not prove physical RAM exhaustion:
/// the same code covers allocator, lock-table, and shared-memory limits.
pub(crate) struct OutOfMemoryLogLens;

impl OutOfMemoryLogLens {
    const ID: &'static str = "PG-EVT-005";
}

impl EventLens for OutOfMemoryLogLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_error_sqlstate(events, sink, &[SQLSTATE_OUT_OF_MEMORY])
    }
}

/// Log branch of `PG-CONN-014`: a stderr token resembles SQLSTATE `53300`.
/// It is compatible with a rejected connection, not proof of sustained slot
/// saturation or its cause.
pub(crate) struct ConnectionSlotsExhaustedLens;

impl ConnectionSlotsExhaustedLens {
    const ID: &'static str = "PG-CONN-014";
}

impl EventLens for ConnectionSlotsExhaustedLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_error_sqlstate(events, sink, &[SQLSTATE_TOO_MANY_CONNECTIONS])
    }
}

/// `PG-EVT-007` (`deadlock`): stderr contains a token resembling `40P01`
/// (deadlock detected). It does not identify the cause or cycle: the row
/// is a count by SQLSTATE, so the participating transactions and their order are
/// not recoverable, and the victim is not necessarily the initiator.
pub(crate) struct DeadlockLens;

impl DeadlockLens {
    const ID: &'static str = "PG-EVT-007";
}

impl EventLens for DeadlockLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_error_sqlstate(events, sink, &[SQLSTATE_DEADLOCK])
    }
}

/// `PG-EVT-008` (`data_corruption_log`): stderr contains a token resembling
/// `XX001`/`XX002`. It is not proof of cluster-wide corruption, and `PANIC`,
/// generic I/O text, and "invalid record length" do not match this lens.
pub(crate) struct DataCorruptionLogLens;

impl DataCorruptionLogLens {
    const ID: &'static str = "PG-EVT-008";
}

impl EventLens for DataCorruptionLogLens {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn confidence_cap(&self) -> ConfidenceCap {
        ConfidenceCap::High
    }

    fn evaluate(
        &self,
        events: &LogEventInputs,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), LimitHit> {
        emit_error_sqlstate(
            events,
            sink,
            &[SQLSTATE_DATA_CORRUPTED, SQLSTATE_INDEX_CORRUPTED],
        )
    }
}

/// The self-contained log-event lenses, applied to every request.
pub(crate) fn event_catalog() -> Vec<Box<dyn EventLens>> {
    vec![
        Box::new(BackendSigkillLens),
        Box::new(BackendCrashLens),
        Box::new(PanicShutdownLens),
        Box::new(DiskFullLogLens),
        Box::new(OutOfMemoryLogLens),
        Box::new(ConnectionSlotsExhaustedLens),
        Box::new(DeadlockLens),
        Box::new(DataCorruptionLogLens),
    ]
}

/// The ids of the event lenses, in catalog order.
pub(crate) fn event_catalog_ids() -> Vec<&'static str> {
    event_catalog().iter().map(|lens| lens.id()).collect()
}

/// Request ceilings for the event-evaluation pass.
pub(crate) struct EventConfig {
    work_limit: u64,
    max_lens_evaluations: u64,
    max_findings: u64,
    max_evidence_rows: u64,
    max_output_bytes: u64,
}

impl EventConfig {
    pub(crate) const fn production() -> Self {
        Self {
            work_limit: 5_000_000,
            max_lens_evaluations: 10_000,
            max_findings: 50_000,
            max_evidence_rows: 200_000,
            max_output_bytes: 8 << 20,
        }
    }

    #[cfg(test)]
    const fn with(
        work_limit: u64,
        max_lens_evaluations: u64,
        max_findings: u64,
        max_evidence_rows: u64,
        max_output_bytes: u64,
    ) -> Self {
        Self {
            work_limit,
            max_lens_evaluations,
            max_findings,
            max_evidence_rows,
            max_output_bytes,
        }
    }

    #[cfg(test)]
    const fn for_test() -> Self {
        Self::with(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventError {
    DuplicateLensId(&'static str),
}

pub(crate) struct EventOutcome {
    pub findings: Vec<Finding>,
    pub coverage: BTreeMap<&'static str, LogCoverage>,
    pub complete: bool,
    pub skipped: Vec<EngineSkip>,
}

const fn admit_event_lens(
    lens_id: &'static str,
    evaluations: &mut u64,
    evaluation_limit: u64,
    budget: &mut WorkBudget,
) -> Result<(), EngineSkip> {
    let observed = evaluations.saturating_add(1);
    if observed > evaluation_limit {
        return Err(EngineSkip {
            lens_id: Some(lens_id),
            limit: LimitHit {
                axis: LimitAxis::LensEvaluations,
                observed,
                limit: evaluation_limit,
            },
        });
    }
    if !budget.charge(1) {
        return Err(EngineSkip {
            lens_id: Some(lens_id),
            limit: LimitHit {
                axis: LimitAxis::Work,
                observed: budget.spent().saturating_add(1),
                limit: budget.limit(),
            },
        });
    }
    *evaluations = observed;
    Ok(())
}

/// Run the event lenses over bounded, typed log events.
///
/// A truncated event set (`overflow`) forces an incomplete result, so the caller
/// never presents the findings as exhaustive. Findings share the numeric pass's
/// evidence gate, work budget, output caps, and deterministic ordering.
pub(crate) fn evaluate_events(
    events: &LogEventInputs,
    lenses: &[&dyn EventLens],
    config: &EventConfig,
) -> Result<EventOutcome, EventError> {
    let mut lens_ids = BTreeSet::new();
    for lens in lenses {
        if !lens_ids.insert(lens.id()) {
            return Err(EventError::DuplicateLensId(lens.id()));
        }
    }

    let mut budget = WorkBudget::new(config.work_limit);
    let mut counts = OutputCounts::new();
    let limits = OutputLimits::bounded(
        config.max_findings,
        config.max_evidence_rows,
        config.max_output_bytes,
    );
    let mut findings = Vec::new();
    let mut skipped = Vec::new();
    // The current source proves positive occurrences only. It cannot prove
    // complete format, rotation, configuration, or byte coverage, so this
    // branch is never exhaustive even when evaluation itself finishes.
    let mut complete = false;
    let mut evaluations = 0_u64;

    for lens in lenses {
        if let Err(skip) = admit_event_lens(
            lens.id(),
            &mut evaluations,
            config.max_lens_evaluations,
            &mut budget,
        ) {
            skipped.push(skip);
            complete = false;
            break;
        }
        let mut sink = FindingSink::new(
            &mut findings,
            &mut budget,
            &mut counts,
            limits,
            lens.id(),
            lens.confidence_cap(),
        );
        let evaluation = lens.evaluate(events, &mut sink);
        let limit = evaluation.err().or_else(|| sink.limit_hit());
        if let Some(limit) = limit {
            skipped.push(EngineSkip {
                lens_id: Some(lens.id()),
                limit,
            });
            complete = false;
            break;
        }
    }
    findings.sort_by(finding_order);

    Ok(EventOutcome {
        findings,
        coverage: events.coverage().clone(),
        complete,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::super::evidence::Confidence;
    use super::*;

    fn error(severity: u8, sqlstate: Option<&str>, count: u64) -> LogErrorGroup {
        error_at(100, severity, sqlstate, count)
    }

    fn error_at(
        observed_at_us: i64,
        severity: u8,
        sqlstate: Option<&str>,
        count: u64,
    ) -> LogErrorGroup {
        LogErrorGroup::new(observed_at_us, severity, sqlstate.map(str::to_owned), count)
    }

    fn crash(signal: Option<i32>) -> LifecycleEvent {
        LifecycleEvent::new(100, Some(42), LifecycleKind::Crash, signal)
    }

    fn inputs_with(errors: Vec<LogErrorGroup>, lifecycle: Vec<LifecycleEvent>) -> LogEventInputs {
        let mut inputs = LogEventInputs::new(EventInputLimits::production());
        for group in errors {
            assert!(inputs.push_error(group));
        }
        for event in lifecycle {
            assert!(inputs.push_lifecycle(event));
        }
        inputs
    }

    fn run(events: &LogEventInputs, lens: &dyn EventLens) -> EventOutcome {
        evaluate_events(events, &[lens], &EventConfig::for_test()).expect("valid event analysis")
    }

    fn findings(events: &LogEventInputs, lens: &dyn EventLens) -> Vec<(Role, Confidence, String)> {
        run(events, lens)
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.role(),
                    finding.confidence(),
                    finding.lens_id().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn sqlstate_is_only_valid_at_five_uppercase_alnum_chars() {
        assert!(is_sqlstate("53100"));
        assert!(is_sqlstate("40P01"));
        assert!(is_sqlstate("XX001"));
        assert!(!is_sqlstate("5310")); // too short
        assert!(!is_sqlstate("531000")); // too long
        assert!(!is_sqlstate("40p01")); // lowercase
        assert!(!is_sqlstate("53-00")); // punctuation
    }

    #[test]
    fn a_malformed_sqlstate_is_dropped_to_none() {
        let group = error(0, Some("bad"), 3);
        assert_eq!(
            group.sqlstate(),
            None,
            "a non-conforming code is not retained"
        );
        assert_eq!(group.count(), 3, "the group still carries its count");
    }

    type EventCase = (
        Box<dyn EventLens>,
        LogEventInputs,
        &'static str,
        &'static str,
        Confidence,
    );

    #[test]
    fn each_event_lens_emits_a_bounded_coincident_occurrence_fact() {
        let cases: Vec<EventCase> = vec![
            (
                Box::new(BackendSigkillLens),
                inputs_with(Vec::new(), vec![crash(Some(9))]),
                "PG-EVT-001",
                "signal",
                Confidence::MEDIUM,
            ),
            (
                Box::new(BackendCrashLens),
                inputs_with(Vec::new(), vec![crash(Some(11))]),
                "PG-EVT-002",
                "signal",
                Confidence::MEDIUM,
            ),
            (
                Box::new(PanicShutdownLens),
                inputs_with(vec![error(SEVERITY_PANIC, None, 1)], Vec::new()),
                "PG-EVT-003",
                "severity_panic",
                Confidence::MEDIUM,
            ),
            (
                Box::new(DiskFullLogLens),
                inputs_with(vec![error(1, Some("53100"), 2)], Vec::new()),
                "OS-FS-027",
                "sqlstate",
                Confidence::MEDIUM,
            ),
            (
                Box::new(OutOfMemoryLogLens),
                inputs_with(vec![error(1, Some("53200"), 4)], Vec::new()),
                "PG-EVT-005",
                "sqlstate",
                Confidence::MEDIUM,
            ),
            (
                Box::new(ConnectionSlotsExhaustedLens),
                inputs_with(vec![error(1, Some("53300"), 7)], Vec::new()),
                "PG-CONN-014",
                "sqlstate",
                Confidence::MEDIUM,
            ),
            (
                Box::new(DeadlockLens),
                inputs_with(vec![error(0, Some("40P01"), 1)], Vec::new()),
                "PG-EVT-007",
                "sqlstate",
                Confidence::MEDIUM,
            ),
            (
                Box::new(DataCorruptionLogLens),
                inputs_with(vec![error(0, Some("XX001"), 1)], Vec::new()),
                "PG-EVT-008",
                "sqlstate",
                Confidence::MEDIUM,
            ),
        ];
        for (lens, events, id, column, confidence) in cases {
            let outcome = run(&events, lens.as_ref());
            assert_eq!(outcome.findings.len(), 1, "{id} emits one fact");
            let finding = &outcome.findings[0];
            assert_eq!(finding.lens_id(), id);
            assert_eq!(
                finding.confidence(),
                confidence,
                "{id} respects source evidence quality"
            );
            assert_eq!(
                finding.role(),
                Role::Coincident,
                "{id} stays coincident without proven direction"
            );
            assert_eq!(finding.evidence().len(), 1);
            assert_eq!(finding.scope().column(), column);
        }
    }

    #[test]
    fn data_corruption_matches_both_checksum_and_index_codes() {
        let events = inputs_with(
            vec![error(0, Some("XX002"), 1), error(0, Some("XX001"), 1)],
            Vec::new(),
        );
        let outcome = run(&events, &DataCorruptionLogLens);
        assert_eq!(outcome.findings.len(), 2, "both corruption codes are facts");
    }

    #[test]
    fn panic_and_generic_errors_are_not_corruption_facts() {
        let events = inputs_with(
            vec![
                error(SEVERITY_PANIC, None, 1),
                error(0, None, 1), // includes messages such as invalid record length
                error(0, Some("58030"), 1),
            ],
            Vec::new(),
        );
        assert!(findings(&events, &DataCorruptionLogLens).is_empty());
    }

    #[test]
    fn sigkill_and_crash_lenses_partition_signals() {
        let events = inputs_with(Vec::new(), vec![crash(Some(9)), crash(Some(11))]);
        assert_eq!(
            findings(&events, &BackendSigkillLens).len(),
            1,
            "only signal 9 is a sigkill fact"
        );
        assert_eq!(
            findings(&events, &BackendCrashLens).len(),
            1,
            "only the fault signal is a crash fact"
        );
    }

    #[test]
    fn a_crash_without_a_signal_is_no_signal_fact() {
        let events = inputs_with(Vec::new(), vec![crash(None)]);
        assert!(findings(&events, &BackendSigkillLens).is_empty());
        assert!(findings(&events, &BackendCrashLens).is_empty());
    }

    #[test]
    fn shutdown_and_ready_are_not_crashes() {
        let events = inputs_with(
            Vec::new(),
            vec![
                LifecycleEvent::new(100, Some(42), LifecycleKind::Shutdown, Some(9)),
                LifecycleEvent::new(100, None, LifecycleKind::Ready, None),
            ],
        );
        assert!(
            findings(&events, &BackendSigkillLens).is_empty(),
            "a shutdown signal is not a crash termination"
        );
    }

    #[test]
    fn a_non_matching_sqlstate_reports_nothing() {
        let events = inputs_with(vec![error(0, Some("00000"), 5)], Vec::new());
        assert!(findings(&events, &DeadlockLens).is_empty());
    }

    #[test]
    fn a_zero_count_group_is_not_an_occurrence() {
        let events = inputs_with(vec![error(0, Some("40P01"), 0)], Vec::new());
        assert!(findings(&events, &DeadlockLens).is_empty());
    }

    #[test]
    fn duplicate_stored_records_emit_one_public_fact() {
        let errors = inputs_with(
            vec![error(0, Some("40P01"), 1), error(0, Some("40P01"), 2)],
            Vec::new(),
        );
        assert_eq!(findings(&errors, &DeadlockLens).len(), 1);

        let lifecycle = inputs_with(Vec::new(), vec![crash(Some(SIGKILL)), crash(Some(SIGKILL))]);
        assert_eq!(findings(&lifecycle, &BackendSigkillLens).len(), 1);

        let panics = inputs_with(
            vec![
                error(SEVERITY_PANIC, None, 1),
                error(SEVERITY_PANIC, Some("XX000"), 1),
            ],
            Vec::new(),
        );
        assert_eq!(findings(&panics, &PanicShutdownLens).len(), 1);
    }

    #[test]
    fn absent_events_yield_no_findings_and_never_an_all_clear() {
        let mut events = LogEventInputs::new(EventInputLimits::production());
        events.set_coverage(PG_LOG_ERRORS, LogCoverage::NotCollected);
        let outcome =
            evaluate_events(&events, &[&DeadlockLens], &EventConfig::for_test()).expect("valid");
        assert!(
            outcome.findings.is_empty(),
            "no event is no finding, not a proof of health"
        );
        assert_eq!(
            outcome.coverage.get(PG_LOG_ERRORS),
            Some(&LogCoverage::NotCollected),
            "coverage keeps the absence honest",
        );
    }

    #[test]
    fn the_finding_scope_exposes_only_the_code_and_observation_time() {
        let events = inputs_with(vec![error(0, Some("40P01"), 3)], Vec::new());
        let outcome = run(&events, &DeadlockLens);
        let identity = outcome.findings[0].scope().identity();
        assert_eq!(
            identity,
            &[
                IdentityValue::Text("40P01".to_owned()),
                IdentityValue::I64(100),
            ],
            "no count, sample, statement, user, or address reaches the scope",
        );
    }

    #[test]
    fn overflow_forces_an_incomplete_result() {
        let mut events = LogEventInputs::new(EventInputLimits::with(1, 1));
        assert!(events.push_error(error(0, Some("40P01"), 1)));
        assert!(
            !events.push_error(error(0, Some("40P01"), 1)),
            "the second group is dropped at the ceiling"
        );
        assert!(events.overflow());
        let outcome =
            evaluate_events(&events, &[&DeadlockLens], &EventConfig::for_test()).expect("valid");
        assert_eq!(outcome.findings.len(), 1, "the retained group still emits");
        assert!(!outcome.complete, "a truncated event set is never complete");
    }

    #[test]
    fn the_finding_limit_is_reported_without_retaining_excess() {
        let events = inputs_with(
            vec![
                error_at(100, 0, Some("40P01"), 1),
                error_at(101, 0, Some("40P01"), 2),
            ],
            Vec::new(),
        );
        let config = EventConfig::with(1_000, 1_000, 1, 1_000, 1_000_000);
        let outcome = evaluate_events(&events, &[&DeadlockLens], &config).expect("bounded partial");
        assert!(!outcome.complete);
        assert_eq!(outcome.findings.len(), 1);
        assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Findings);
    }

    #[test]
    fn the_work_limit_bounds_the_pass() {
        let events = inputs_with(vec![error(0, Some("40P01"), 1)], Vec::new());
        // One unit admits the lens; charging the scanned rows then fails.
        let config = EventConfig::with(1, 1_000, 1_000, 1_000, 1_000_000);
        let outcome = evaluate_events(&events, &[&DeadlockLens], &config).expect("bounded partial");
        assert!(!outcome.complete);
        assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::Work);
    }

    #[test]
    fn the_event_output_byte_limit_is_enforced() {
        let events = inputs_with(vec![error(0, Some("40P01"), 1)], Vec::new());
        let config = EventConfig::with(1_000, 1_000, 1_000, 1_000, 1);
        let outcome = evaluate_events(&events, &[&DeadlockLens], &config).expect("bounded partial");
        assert!(!outcome.complete);
        assert!(outcome.findings.is_empty());
        assert_eq!(outcome.skipped[0].limit.axis, LimitAxis::OutputBytes);
    }

    #[test]
    fn duplicate_lens_ids_are_rejected() {
        let result = evaluate_events(
            &LogEventInputs::new(EventInputLimits::production()),
            &[&DeadlockLens, &DeadlockLens],
            &EventConfig::for_test(),
        );
        assert!(matches!(
            result,
            Err(EventError::DuplicateLensId("PG-EVT-007"))
        ));
    }

    #[test]
    fn the_event_catalog_lists_every_wired_lens_once() {
        let ids = event_catalog_ids();
        assert_eq!(
            ids,
            vec![
                "PG-EVT-001",
                "PG-EVT-002",
                "PG-EVT-003",
                "OS-FS-027",
                "PG-EVT-005",
                "PG-CONN-014",
                "PG-EVT-007",
                "PG-EVT-008",
            ]
        );
        let unique: BTreeSet<_> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "event ids are unique");
        assert_eq!(event_catalog().len(), ids.len());
        assert_eq!(event_catalog_metadata().len(), ids.len());
        assert_eq!(
            event_catalog_metadata()
                .iter()
                .map(|entry| entry.lens_id)
                .collect::<Vec<_>>(),
            ids
        );
        let slugs: BTreeSet<_> = event_catalog_metadata()
            .iter()
            .map(|entry| entry.slug)
            .collect();
        assert_eq!(slugs.len(), ids.len(), "event slugs are unique");
    }

    #[test]
    fn the_whole_catalog_produces_one_fact_per_matched_event() {
        let events = inputs_with(
            vec![error(0, Some("40P01"), 1), error(SEVERITY_PANIC, None, 1)],
            vec![crash(Some(9))],
        );
        let catalog = event_catalog();
        let lenses: Vec<&dyn EventLens> = catalog.iter().map(AsRef::as_ref).collect();
        let outcome =
            evaluate_events(&events, &lenses, &EventConfig::for_test()).expect("valid analysis");
        let ids: Vec<_> = outcome.findings.iter().map(Finding::lens_id).collect();
        assert_eq!(
            ids,
            vec!["PG-EVT-001", "PG-EVT-003", "PG-EVT-007"],
            "sigkill, panic, and deadlock each fire once; nothing else matches",
        );
        assert!(!outcome.complete, "stderr coverage is never exhaustive");
    }
}
