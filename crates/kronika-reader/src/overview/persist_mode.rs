//! Persistent-cache write state with exclusive due-probe reservation.
//!
//! Durable reads never consult this state. Recoverable write failures arm a
//! bounded delay; once due, exactly one caller owns the probe or publication
//! attempt. A dropped reservation restores a bounded retry rather than leaving
//! the store permanently in flight.

use std::time::{Duration, Instant};

use super::publish::PersistError;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_mins(5);

/// Write capability of the persistent cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistMode {
    /// Writes are attempted normally.
    ReadWrite,
    /// A read-only mount or permission policy is backed off; reads continue.
    ReadOnlyBackoff,
    /// Capacity or transient write I/O is backed off; reads still continue.
    UnavailableBackoff,
}

/// A snapshot of the store's write state for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistModeSnapshot {
    /// Current write mode.
    pub mode: PersistMode,
    /// Consecutive recoverable failures since the last write/probe success.
    pub failures: u32,
    /// Standing recoverable failure reason.
    pub reason: Option<PersistError>,
    /// Remaining delay before one caller may reserve recovery.
    pub retry_after: Duration,
    /// Whether one write or recovery probe currently owns the reservation.
    pub probe_in_flight: bool,
}

#[allow(
    variant_size_differences,
    reason = "the u64 reservation token prevents ABA reuse; outcomes are short-lived"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reservation {
    Acquired(u64),
    Backoff(PersistError),
    InFlight,
    NotNeeded,
}

/// Retry-backoff state machine for one `FactStore`.
#[derive(Debug)]
pub(super) struct PersistState {
    mode: PersistMode,
    failures: u32,
    reason: Option<PersistError>,
    next_retry_at: Option<Instant>,
    reservation: Option<u64>,
    next_reservation: u64,
    jitter_seed: u64,
}

impl PersistState {
    pub(super) const fn new(jitter_seed: u64) -> Self {
        Self {
            mode: PersistMode::ReadWrite,
            failures: 0,
            reason: None,
            next_retry_at: None,
            reservation: None,
            next_reservation: 1,
            jitter_seed,
        }
    }

    /// Reserves any permitted write, including the single due recovery attempt.
    pub(super) fn reserve_write(&mut self, now: Instant) -> Reservation {
        if self.reservation.is_some() {
            return Reservation::InFlight;
        }
        if let Some(deadline) = self.next_retry_at
            && now < deadline
        {
            return Reservation::Backoff(self.standing_reason());
        }
        self.acquire_reservation()
    }

    /// Reserves recovery only when a standing failure is due.
    pub(super) fn reserve_due_probe(&mut self, now: Instant) -> Reservation {
        if self.failures == 0 {
            return Reservation::NotNeeded;
        }
        self.reserve_write(now)
    }

    /// Completes an owned reservation.
    pub(super) fn finish(
        &mut self,
        reservation: u64,
        result: Result<(), PersistError>,
        now: Instant,
    ) {
        if self.reservation != Some(reservation) {
            return;
        }
        self.reservation = None;
        match result {
            Err(error) if error.is_backoff_eligible() => self.on_failure(error, now),
            Err(_) if self.failures != 0 => self.defer_standing_failure(now),
            Ok(()) | Err(_) => self.on_success(),
        }
    }

    /// Releases a cancelled/panicked owner without leaving a stuck flight.
    pub(super) fn cancel(&mut self, reservation: u64, now: Instant) {
        if self.reservation != Some(reservation) {
            return;
        }
        self.reservation = None;
        if self.failures == 0 {
            self.on_success();
        } else {
            self.defer_standing_failure(now);
        }
    }

    pub(super) fn snapshot(&self, now: Instant) -> PersistModeSnapshot {
        PersistModeSnapshot {
            mode: self.mode,
            failures: self.failures,
            reason: self.reason,
            retry_after: self.next_retry_at.map_or(Duration::ZERO, |deadline| {
                deadline.saturating_duration_since(now)
            }),
            probe_in_flight: self.reservation.is_some(),
        }
    }

    pub(super) const fn standing_reason(&self) -> PersistError {
        match self.reason {
            Some(error) => error,
            None => PersistError::TransientIo,
        }
    }

    fn acquire_reservation(&mut self) -> Reservation {
        let reservation = self.next_reservation;
        self.next_reservation = self.next_reservation.wrapping_add(1).max(1);
        self.reservation = Some(reservation);
        Reservation::Acquired(reservation)
    }

    const fn on_success(&mut self) {
        self.mode = PersistMode::ReadWrite;
        self.failures = 0;
        self.reason = None;
        self.next_retry_at = None;
    }

    fn on_failure(&mut self, error: PersistError, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.reason = Some(error);
        self.mode = mode_for(error);
        self.next_retry_at = Some(deadline_after(
            now,
            backoff_delay(error, self.failures, self.jitter_seed),
        ));
    }

    fn defer_standing_failure(&mut self, now: Instant) {
        self.next_retry_at = Some(deadline_after(now, INITIAL_BACKOFF));
    }

    #[cfg(test)]
    pub(super) const fn force_due(&mut self, now: Instant) {
        if self.failures != 0 {
            self.next_retry_at = Some(now);
        }
    }
}

fn deadline_after(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay).unwrap_or(now)
}

const fn mode_for(error: PersistError) -> PersistMode {
    match error {
        PersistError::ReadOnlyFilesystem | PersistError::PermissionDenied => {
            PersistMode::ReadOnlyBackoff
        }
        _ => PersistMode::UnavailableBackoff,
    }
}

fn backoff_delay(error: PersistError, failures: u32, seed: u64) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    if matches!(
        error,
        PersistError::ReadOnlyFilesystem | PersistError::PermissionDenied
    ) {
        return MAX_BACKOFF;
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
    let spread = spread_permille(seed, failures);
    let jittered = millis.saturating_mul(800 + u64::from(spread)) / 1_000;
    Duration::from_millis(jittered).min(MAX_BACKOFF)
}

fn spread_permille(seed: u64, failures: u32) -> u32 {
    let mut mixed = seed ^ u64::from(failures).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    mixed ^= mixed >> 30;
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    mixed = mixed.wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    (mixed % 401) as u32
}

#[cfg(test)]
mod tests;
