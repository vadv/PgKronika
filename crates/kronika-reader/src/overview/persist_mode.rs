//! Persistent-cache write mode with retry backoff.
//!
//! A recoverable publication failure (read-only mount, no space, quota,
//! permission, transient I/O) must not make every subsequent build retry the
//! same failing write. The store records a mode and a next-retry deadline;
//! while backed off, builds stay memory-only and never touch the disk until
//! the deadline passes. One success resets the schedule.

use std::time::{Duration, Instant};

use super::publish::PersistError;

/// Initial backoff after the first failure.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Backoff never grows past this.
const MAX_BACKOFF: Duration = Duration::from_mins(5);

/// Write capability of the persistent cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistMode {
    /// Writes are attempted normally.
    ReadWrite,
    /// The filesystem rejects writes but committed facts may still be read
    /// (read-only mount, permission denied).
    ReadOnlyBackoff,
    /// Writes fail for a condition that may clear (no space, quota, transient
    /// I/O); reads may also be affected.
    UnavailableBackoff,
}

/// A snapshot of the store's persistence mode for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistModeSnapshot {
    /// Current write mode.
    pub mode: PersistMode,
    /// Consecutive recoverable failures since the last success.
    pub failures: u32,
    /// Standing failure reason while backed off.
    pub reason: Option<PersistError>,
}

/// Retry-backoff state machine for the persistent cache.
#[derive(Debug)]
pub(super) struct PersistState {
    mode: PersistMode,
    failures: u32,
    reason: Option<PersistError>,
    next_retry_at: Option<Instant>,
}

impl Default for PersistState {
    fn default() -> Self {
        Self {
            mode: PersistMode::ReadWrite,
            failures: 0,
            reason: None,
            next_retry_at: None,
        }
    }
}

impl PersistState {
    /// Whether a disk write should be attempted now, or suppressed by an
    /// unexpired backoff.
    pub(super) fn should_attempt_write(&self, now: Instant) -> bool {
        self.next_retry_at.is_none_or(|deadline| now >= deadline)
    }

    /// Records a successful publication: clears backoff and returns to
    /// [`PersistMode::ReadWrite`].
    pub(super) const fn on_success(&mut self) {
        self.mode = PersistMode::ReadWrite;
        self.failures = 0;
        self.reason = None;
        self.next_retry_at = None;
    }

    /// Records a recoverable failure, arming the next backoff deadline.
    pub(super) fn on_failure(&mut self, error: PersistError, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.reason = Some(error);
        self.mode = mode_for(error);
        self.next_retry_at = Some(now + backoff_delay(self.failures));
    }

    /// The reason a suppressed write reports without touching the disk.
    pub(super) const fn standing_reason(&self) -> PersistError {
        match self.reason {
            Some(error) => error,
            None => PersistError::Io,
        }
    }

    pub(super) const fn snapshot(&self) -> PersistModeSnapshot {
        PersistModeSnapshot {
            mode: self.mode,
            failures: self.failures,
            reason: self.reason,
        }
    }
}

/// Read-only conditions keep read access; the rest may lose it.
const fn mode_for(error: PersistError) -> PersistMode {
    match error {
        PersistError::ReadOnlyFilesystem | PersistError::PermissionDenied => {
            PersistMode::ReadOnlyBackoff
        }
        _ => PersistMode::UnavailableBackoff,
    }
}

/// Exponential backoff `INITIAL_BACKOFF * 2^(failures-1)` capped at
/// [`MAX_BACKOFF`], with a deterministic ±20% perturbation of the ladder.
///
/// The perturbation is derived from `failures` alone: it keeps the schedule
/// off round numbers without pulling in a randomness dependency and stays
/// reproducible in tests. It does not desynchronize independent stores —
/// they share the same delay at the same failure count; process refresh
/// clocks already run out of phase.
///
/// `failures` is 1-based (the first failure yields the initial delay).
fn backoff_delay(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let shift = failures - 1;
    let base = if shift >= 32 {
        MAX_BACKOFF
    } else {
        INITIAL_BACKOFF
            .checked_mul(1_u32 << shift)
            .map_or(MAX_BACKOFF, |scaled| scaled.min(MAX_BACKOFF))
    };
    let millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    // Jitter factor in [0.8, 1.2), derived from the failure count so a test
    // is deterministic and no randomness dependency enters the reader.
    let spread = spread_permille(failures); // 0..=400
    let jittered = millis.saturating_mul(800 + u64::from(spread)) / 1000;
    Duration::from_millis(jittered)
}

/// Deterministic spread in `0..=400` (the width of ±20%) from the failure
/// count, so consecutive counts do not land on the same factor.
const fn spread_permille(failures: u32) -> u32 {
    let mixed = failures.wrapping_mul(2_654_435_761);
    mixed % 401
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_always_attempts() {
        let state = PersistState::default();
        assert!(
            state.should_attempt_write(Instant::now()),
            "a fresh store writes without backoff"
        );
    }

    #[test]
    fn a_failure_suppresses_until_the_deadline() {
        let mut state = PersistState::default();
        let now = Instant::now();
        state.on_failure(PersistError::NoSpace, now);
        assert!(
            !state.should_attempt_write(now),
            "the write is suppressed immediately after a failure"
        );
        assert!(
            state.should_attempt_write(now + Duration::from_secs(2)),
            "the write is retried once the backoff elapses"
        );
    }

    #[test]
    fn success_clears_backoff_and_reason() {
        let mut state = PersistState::default();
        state.on_failure(PersistError::QuotaExceeded, Instant::now());
        state.on_success();
        let snapshot = state.snapshot();
        assert_eq!(
            snapshot.mode,
            PersistMode::ReadWrite,
            "success returns to read-write"
        );
        assert_eq!(snapshot.failures, 0, "success clears the failure count");
        assert!(
            snapshot.reason.is_none(),
            "success clears the standing reason"
        );
    }

    #[test]
    fn read_only_and_unavailable_modes_split_by_reason() {
        let mut read_only = PersistState::default();
        read_only.on_failure(PersistError::ReadOnlyFilesystem, Instant::now());
        assert_eq!(read_only.snapshot().mode, PersistMode::ReadOnlyBackoff);

        let mut unavailable = PersistState::default();
        unavailable.on_failure(PersistError::NoSpace, Instant::now());
        assert_eq!(unavailable.snapshot().mode, PersistMode::UnavailableBackoff);
    }

    #[test]
    fn backoff_grows_and_caps() {
        // First failure ≈ 1 s (± 20%), and it never exceeds the cap + jitter.
        let first = backoff_delay(1);
        assert!(
            first >= Duration::from_millis(800) && first < Duration::from_millis(1200),
            "first backoff is one second ± 20%, got {first:?}"
        );
        let capped = backoff_delay(100);
        assert!(
            capped <= Duration::from_mins(6),
            "backoff never exceeds the cap plus jitter, got {capped:?}"
        );
        assert!(
            capped >= Duration::from_mins(4),
            "a deep failure count still backs off near the cap, got {capped:?}"
        );
    }

    #[test]
    fn zero_failures_has_no_delay() {
        assert_eq!(backoff_delay(0), Duration::ZERO, "no failure means no wait");
    }

    #[test]
    fn standing_reason_defaults_when_absent() {
        let state = PersistState::default();
        assert_eq!(
            state.standing_reason(),
            PersistError::Io,
            "absent reason falls back to Io"
        );
    }
}
