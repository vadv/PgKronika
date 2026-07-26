use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::ParserKind;

/// Final result of one `PostgreSQL` log-source observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSourceState {
    /// The current supported file was opened and processed.
    Collecting,
    /// The last path was processed, but discovery could not be refreshed.
    CollectingDegraded,
    /// No supported file could be read.
    Unavailable,
    /// The operator explicitly disabled the source.
    Disabled,
}

impl LogSourceState {
    /// Numeric code stored in `pg_log_source_status`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Collecting => 0,
            Self::CollectingDegraded => 1,
            Self::Unavailable => 2,
            Self::Disabled => 3,
        }
    }

    /// Stable name used by process logs and the web API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Collecting => "collecting",
            Self::CollectingDegraded => "collecting_degraded",
            Self::Unavailable => "unavailable",
            Self::Disabled => "disabled",
        }
    }

    const fn proves_read(self) -> bool {
        matches!(self, Self::Collecting | Self::CollectingDegraded)
    }
}

/// Why the source has its final state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSourceReason {
    /// No degradation.
    None,
    /// No `PostgreSQL` client was available for discovery.
    PostgresUnavailable,
    /// `PostgreSQL` reported no current stderr file.
    NoCurrentLogfile,
    /// The selected log format is unsupported.
    UnsupportedFormat,
    /// A discovery SQL query failed.
    DiscoveryQueryFailed,
    /// The known path did not exist.
    MissingFile,
    /// The collector lacked permission to open the path.
    PermissionDenied,
    /// Another I/O error prevented reading.
    ReadError,
}

impl LogSourceReason {
    /// Numeric code stored in `pg_log_source_status`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::PostgresUnavailable => 1,
            Self::NoCurrentLogfile => 2,
            Self::UnsupportedFormat => 3,
            Self::DiscoveryQueryFailed => 4,
            Self::MissingFile => 5,
            Self::PermissionDenied => 6,
            Self::ReadError => 7,
        }
    }

    /// Stable name used by process logs and the web API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PostgresUnavailable => "postgres_unavailable",
            Self::NoCurrentLogfile => "no_current_logfile",
            Self::UnsupportedFormat => "unsupported_format",
            Self::DiscoveryQueryFailed => "discovery_query_failed",
            Self::MissingFile => "missing_file",
            Self::PermissionDenied => "permission_denied",
            Self::ReadError => "read_error",
        }
    }
}

/// One source-status row before dictionary interning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSourceStatus {
    /// Observation time, unix microseconds.
    pub ts: i64,
    /// Final availability state.
    pub state: LogSourceState,
    /// Reason for the final state.
    pub reason: LogSourceReason,
    /// Parser selected for the known source.
    pub parser_kind: ParserKind,
    /// Current or last known path.
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusKey {
    state: LogSourceState,
    reason: LogSourceReason,
    parser_kind: ParserKind,
    source_path: Option<PathBuf>,
}

impl From<&LogSourceStatus> for StatusKey {
    fn from(status: &LogSourceStatus) -> Self {
        Self {
            state: status.state,
            reason: status.reason,
            parser_kind: status.parser_kind,
            source_path: status.source_path.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StatusTracker {
    heartbeat_interval: Duration,
    current: Option<LogSourceStatus>,
    next_heartbeat: Option<Instant>,
    ever_collected: bool,
    outage_reported: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusUpdate {
    pub(crate) row: Option<LogSourceStatus>,
    pub(crate) previous: Option<LogSourceStatus>,
    pub(crate) changed: bool,
    pub(crate) outage_started: bool,
    next: StatusTracker,
}

impl StatusTracker {
    pub(crate) const fn new(heartbeat_interval: Duration, had_success: bool) -> Self {
        Self {
            heartbeat_interval,
            current: None,
            next_heartbeat: None,
            ever_collected: had_success,
            outage_reported: false,
        }
    }

    pub(crate) fn observe(&self, status: LogSourceStatus, now: Instant) -> StatusUpdate {
        let mut next = self.clone();
        let previous = next.current.clone();
        let key = StatusKey::from(&status);
        let changed = previous.as_ref().map(StatusKey::from).as_ref() != Some(&key);
        let heartbeat_due = next.next_heartbeat.is_some_and(|deadline| now >= deadline);
        let emit = changed || previous.is_none() || heartbeat_due;

        let outage_started = status.state == LogSourceState::Unavailable
            && next.ever_collected
            && !next.outage_reported;
        if status.state.proves_read() {
            next.ever_collected = true;
            next.outage_reported = false;
        } else if outage_started {
            next.outage_reported = true;
        }

        next.current = Some(status.clone());
        if emit {
            next.next_heartbeat = Some(now + next.heartbeat_interval);
        }
        StatusUpdate {
            row: emit.then_some(status),
            previous,
            changed,
            outage_started,
            next,
        }
    }

    pub(crate) fn commit(&mut self, update: &StatusUpdate) {
        *self = update.next.clone();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::{LogSourceReason, LogSourceState, LogSourceStatus, StatusTracker};
    use crate::ParserKind;

    fn status(ts: i64, state: LogSourceState, reason: LogSourceReason) -> LogSourceStatus {
        LogSourceStatus {
            ts,
            state,
            reason,
            parser_kind: ParserKind::Stderr,
            source_path: Some(PathBuf::from("/pg/log/postgresql.log")),
        }
    }

    #[test]
    fn first_observation_change_and_heartbeat_emit_exactly_once() {
        let started = Instant::now();
        let mut tracker = StatusTracker::new(Duration::from_mins(5), false);

        let first = tracker.observe(
            status(10, LogSourceState::Collecting, LogSourceReason::None),
            started,
        );
        assert!(first.changed);
        assert_eq!(first.row.as_ref().map(|row| row.ts), Some(10));
        tracker.commit(&first);

        let quiet = tracker.observe(
            status(20, LogSourceState::Collecting, LogSourceReason::None),
            started + Duration::from_secs(299),
        );
        assert!(quiet.row.is_none());
        tracker.commit(&quiet);

        let heartbeat = tracker.observe(
            status(30, LogSourceState::Collecting, LogSourceReason::None),
            started + Duration::from_mins(5),
        );
        assert!(!heartbeat.changed);
        assert_eq!(heartbeat.row.as_ref().map(|row| row.ts), Some(30));
    }

    #[test]
    fn an_uncommitted_transition_is_offered_again() {
        let now = Instant::now();
        let tracker = StatusTracker::new(Duration::from_mins(5), false);
        let first = tracker.observe(
            status(
                10,
                LogSourceState::Unavailable,
                LogSourceReason::MissingFile,
            ),
            now,
        );
        let retry = tracker.observe(
            status(
                11,
                LogSourceState::Unavailable,
                LogSourceReason::MissingFile,
            ),
            now + Duration::from_secs(1),
        );
        assert!(first.row.is_some());
        assert!(retry.row.is_some());
        assert!(retry.changed);
    }

    #[test]
    fn one_outage_gap_is_allowed_until_a_successful_recovery() {
        let now = Instant::now();
        let mut tracker = StatusTracker::new(Duration::from_mins(5), false);
        let healthy = tracker.observe(
            status(1, LogSourceState::Collecting, LogSourceReason::None),
            now,
        );
        tracker.commit(&healthy);

        let first_failure = tracker.observe(
            status(2, LogSourceState::Unavailable, LogSourceReason::MissingFile),
            now + Duration::from_secs(1),
        );
        assert!(first_failure.outage_started);
        tracker.commit(&first_failure);

        let repeated = tracker.observe(
            status(3, LogSourceState::Unavailable, LogSourceReason::MissingFile),
            now + Duration::from_secs(2),
        );
        assert!(!repeated.outage_started);

        let recovered = tracker.observe(
            status(4, LogSourceState::Collecting, LogSourceReason::None),
            now + Duration::from_secs(3),
        );
        tracker.commit(&recovered);
        let second_failure = tracker.observe(
            status(5, LogSourceState::Unavailable, LogSourceReason::ReadError),
            now + Duration::from_secs(4),
        );
        assert!(second_failure.outage_started);
    }

    #[test]
    fn persisted_tail_state_allows_one_restart_outage_gap() {
        let now = Instant::now();
        let tracker = StatusTracker::new(Duration::from_mins(5), true);
        let failure = tracker.observe(
            status(
                1,
                LogSourceState::Unavailable,
                LogSourceReason::PermissionDenied,
            ),
            now,
        );
        assert!(failure.outage_started);
    }
}
