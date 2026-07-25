//! Bounded FIFO admission for sealed-fact work.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kronika_reader::PgmUnit;
use tokio::sync::Notify;

use crate::OverviewColdConfig;

const BYTE_WEIGHT_QUANTUM: u64 = 16 * 1024 * 1024;
const ROW_WEIGHT_QUANTUM: u64 = 65_536;

/// Multi-resource charge for one sealed-fact operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ColdWorkWeight {
    pub(crate) workers: u32,
    pub(crate) pgm_bytes: u32,
    pub(crate) decoded_bytes: u32,
    pub(crate) cpu: u32,
    pub(crate) file_descriptors: u32,
    pub(crate) read_bytes: u32,
    pub(crate) write_bytes: u32,
    pub(crate) publications: u32,
}

impl ColdWorkWeight {
    pub(crate) fn for_unit(unit: &PgmUnit<std::fs::File>) -> Self {
        let catalog = unit.catalog();
        let stored_bytes = catalog
            .entries
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.len));
        let rows = catalog.entries.iter().fold(0_u64, |total, entry| {
            total.saturating_add(u64::from(entry.rows))
        });
        let pgm_bytes = byte_units(unit.source_file_len(), 64);
        let decoded_bytes = byte_units(stored_bytes.saturating_mul(4), 64);
        Self {
            workers: 1,
            pgm_bytes,
            decoded_bytes,
            cpu: row_units(rows, 32),
            file_descriptors: 4,
            read_bytes: pgm_bytes.max(decoded_bytes),
            write_bytes: decoded_bytes,
            publications: 1,
        }
    }

    const fn fits(self, capacity: Self) -> bool {
        self.workers <= capacity.workers
            && self.pgm_bytes <= capacity.pgm_bytes
            && self.decoded_bytes <= capacity.decoded_bytes
            && self.cpu <= capacity.cpu
            && self.file_descriptors <= capacity.file_descriptors
            && self.read_bytes <= capacity.read_bytes
            && self.write_bytes <= capacity.write_bytes
            && self.publications <= capacity.publications
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            workers: self.workers.checked_add(other.workers)?,
            pgm_bytes: self.pgm_bytes.checked_add(other.pgm_bytes)?,
            decoded_bytes: self.decoded_bytes.checked_add(other.decoded_bytes)?,
            cpu: self.cpu.checked_add(other.cpu)?,
            file_descriptors: self.file_descriptors.checked_add(other.file_descriptors)?,
            read_bytes: self.read_bytes.checked_add(other.read_bytes)?,
            write_bytes: self.write_bytes.checked_add(other.write_bytes)?,
            publications: self.publications.checked_add(other.publications)?,
        })
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            workers: self.workers.checked_sub(other.workers)?,
            pgm_bytes: self.pgm_bytes.checked_sub(other.pgm_bytes)?,
            decoded_bytes: self.decoded_bytes.checked_sub(other.decoded_bytes)?,
            cpu: self.cpu.checked_sub(other.cpu)?,
            file_descriptors: self.file_descriptors.checked_sub(other.file_descriptors)?,
            read_bytes: self.read_bytes.checked_sub(other.read_bytes)?,
            write_bytes: self.write_bytes.checked_sub(other.write_bytes)?,
            publications: self.publications.checked_sub(other.publications)?,
        })
    }
}

fn byte_units(bytes: u64, maximum: u32) -> u32 {
    units(bytes, BYTE_WEIGHT_QUANTUM, maximum)
}

fn row_units(rows: u64, maximum: u32) -> u32 {
    units(rows, ROW_WEIGHT_QUANTUM, maximum)
}

fn units(value: u64, quantum: u64, maximum: u32) -> u32 {
    let rounded = value.saturating_add(quantum - 1) / quantum;
    u32::try_from(rounded.max(1))
        .unwrap_or(u32::MAX)
        .min(maximum)
}

/// Fixed process-wide cold-work limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColdAdmissionConfig {
    /// Maximum number of active fact workers.
    pub max_workers: u32,
    /// Maximum number of queued exact fact builds.
    pub max_queue: usize,
    /// Maximum concurrent fact loads started by one request.
    pub per_request_parallelism: usize,
    /// Maximum time a request may wait in the process-wide FIFO.
    pub wait_timeout: Duration,
    /// Stable overload retry hint returned to HTTP clients.
    pub retry_after_seconds: u64,
    /// Aggregate weighted capacities.
    pub(crate) capacity: ColdWorkWeight,
}

impl Default for ColdAdmissionConfig {
    fn default() -> Self {
        Self {
            max_workers: 4,
            max_queue: 64,
            per_request_parallelism: 4,
            wait_timeout: Duration::from_secs(5),
            retry_after_seconds: 1,
            capacity: ColdWorkWeight {
                workers: 4,
                pgm_bytes: 64,
                decoded_bytes: 64,
                cpu: 32,
                file_descriptors: 16,
                read_bytes: 64,
                write_bytes: 64,
                publications: 4,
            },
        }
    }
}

impl ColdAdmissionConfig {
    pub(crate) fn from_operator(
        policy: OverviewColdConfig,
    ) -> Result<Self, ColdAdmissionConfigError> {
        let capacity = ColdWorkWeight {
            workers: policy.max_workers,
            pgm_bytes: capacity_units(policy.pgm_bytes, BYTE_WEIGHT_QUANTUM)?,
            decoded_bytes: capacity_units(policy.decoded_bytes, BYTE_WEIGHT_QUANTUM)?,
            cpu: capacity_units(policy.cpu_rows, ROW_WEIGHT_QUANTUM)?,
            file_descriptors: policy.file_descriptors,
            read_bytes: capacity_units(policy.read_bytes, BYTE_WEIGHT_QUANTUM)?,
            write_bytes: capacity_units(policy.write_bytes, BYTE_WEIGHT_QUANTUM)?,
            publications: policy.publications,
        };
        Self {
            max_workers: policy.max_workers,
            max_queue: policy.max_queue,
            per_request_parallelism: policy.per_request_parallelism,
            wait_timeout: policy.wait_timeout,
            retry_after_seconds: policy.retry_after_seconds,
            capacity,
        }
        .validate()
    }

    pub(crate) const fn validate(self) -> Result<Self, ColdAdmissionConfigError> {
        if self.max_workers == 0
            || self.max_queue == 0
            || self.per_request_parallelism == 0
            || self.wait_timeout.is_zero()
            || self.retry_after_seconds == 0
            || self.capacity.workers != self.max_workers
            || !(ColdWorkWeight {
                workers: 1,
                pgm_bytes: 1,
                decoded_bytes: 1,
                cpu: 1,
                file_descriptors: 1,
                read_bytes: 1,
                write_bytes: 1,
                publications: 1,
            })
            .fits(self.capacity)
        {
            return Err(ColdAdmissionConfigError);
        }
        Ok(self)
    }
}

fn capacity_units(value: u64, quantum: u64) -> Result<u32, ColdAdmissionConfigError> {
    if value == 0 {
        return Err(ColdAdmissionConfigError);
    }
    let rounded = value.saturating_add(quantum - 1) / quantum;
    u32::try_from(rounded).map_err(|_error| ColdAdmissionConfigError)
}

/// Invalid cold-admission bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColdAdmissionConfigError;

impl std::fmt::Display for ColdAdmissionConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cold admission limits must be positive and internally consistent")
    }
}

impl std::error::Error for ColdAdmissionConfigError {}

/// A request cannot enter the bounded cold-work scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColdAdmissionError {
    QueueFull,
    WeightExceedsCapacity,
    TimedOut,
}

impl ColdAdmissionError {
    pub(crate) const fn metric_reason(self) -> &'static str {
        match self {
            Self::QueueFull => "queue_full",
            Self::WeightExceedsCapacity => "weight_exceeds_capacity",
            Self::TimedOut => "timeout",
        }
    }
}

#[derive(Debug)]
struct Waiter {
    ticket: u64,
    weight: ColdWorkWeight,
    granted: AtomicBool,
    notify: Notify,
}

#[derive(Debug, Default)]
struct AdmissionState {
    used: ColdWorkWeight,
    queue: VecDeque<Arc<Waiter>>,
    next_ticket: u64,
}

#[derive(Debug)]
struct AdmissionInner {
    config: ColdAdmissionConfig,
    state: Mutex<AdmissionState>,
}

/// Process-wide FIFO scheduler for fact-cache and source work.
#[derive(Debug, Clone)]
pub(crate) struct ColdAdmission {
    inner: Arc<AdmissionInner>,
}

impl ColdAdmission {
    pub(crate) fn new(config: ColdAdmissionConfig) -> Result<Self, ColdAdmissionConfigError> {
        Ok(Self {
            inner: Arc::new(AdmissionInner {
                config: config.validate()?,
                state: Mutex::new(AdmissionState::default()),
            }),
        })
    }

    pub(crate) fn config(&self) -> ColdAdmissionConfig {
        self.inner.config
    }

    pub(crate) async fn acquire(
        &self,
        weight: ColdWorkWeight,
    ) -> Result<ColdPermit, ColdAdmissionError> {
        if !weight.fits(self.inner.config.capacity) {
            record_rejection(ColdAdmissionError::WeightExceedsCapacity);
            return Err(ColdAdmissionError::WeightExceedsCapacity);
        }

        let started = Instant::now();
        let waiter = {
            let mut state = lock_state(&self.inner);
            if state.queue.is_empty() && can_admit(state.used, weight, self.inner.config.capacity) {
                state.used = state
                    .used
                    .checked_add(weight)
                    .expect("validated admission charge fits");
                record_admission_state(&state);
                metrics::histogram!("overview_cold_wait_seconds").record(started.elapsed());
                return Ok(ColdPermit {
                    inner: Arc::clone(&self.inner),
                    weight,
                });
            }
            if state.queue.len() >= self.inner.config.max_queue {
                drop(state);
                record_rejection(ColdAdmissionError::QueueFull);
                return Err(ColdAdmissionError::QueueFull);
            }
            let waiter = Arc::new(Waiter {
                ticket: state.next_ticket,
                weight,
                granted: AtomicBool::new(false),
                notify: Notify::new(),
            });
            state.next_ticket = state.next_ticket.wrapping_add(1);
            state.queue.push_back(Arc::clone(&waiter));
            record_admission_state(&state);
            waiter
        };

        let mut queued = QueuedAdmission {
            inner: Arc::clone(&self.inner),
            waiter,
            claimed: false,
        };
        let wait = async {
            loop {
                let notified = queued.waiter.notify.notified();
                if queued.waiter.granted.load(Ordering::Acquire) {
                    queued.claimed = true;
                    return ColdPermit {
                        inner: Arc::clone(&queued.inner),
                        weight: queued.waiter.weight,
                    };
                }
                notified.await;
            }
        };
        match tokio::time::timeout(self.inner.config.wait_timeout, wait).await {
            Ok(permit) => {
                metrics::histogram!("overview_cold_wait_seconds").record(started.elapsed());
                Ok(permit)
            }
            Err(_elapsed) => {
                record_rejection(ColdAdmissionError::TimedOut);
                Err(ColdAdmissionError::TimedOut)
            }
        }
    }

    #[cfg(test)]
    fn queued(&self) -> usize {
        lock_state(&self.inner).queue.len()
    }
}

#[derive(Debug)]
struct QueuedAdmission {
    inner: Arc<AdmissionInner>,
    waiter: Arc<Waiter>,
    claimed: bool,
}

impl Drop for QueuedAdmission {
    fn drop(&mut self) {
        if self.claimed {
            return;
        }
        let mut state = lock_state(&self.inner);
        if self.waiter.granted.swap(false, Ordering::AcqRel) {
            state.used = state
                .used
                .checked_sub(self.waiter.weight)
                .expect("granted admission charge is resident");
        } else if let Some(position) = state
            .queue
            .iter()
            .position(|waiter| waiter.ticket == self.waiter.ticket)
        {
            drop(state.queue.remove(position));
        }
        schedule(&self.inner, &mut state);
        record_admission_state(&state);
        drop(state);
    }
}

/// Owned charge released when sealed-fact work completes.
#[derive(Debug)]
pub(crate) struct ColdPermit {
    inner: Arc<AdmissionInner>,
    weight: ColdWorkWeight,
}

impl Drop for ColdPermit {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner);
        state.used = state
            .used
            .checked_sub(self.weight)
            .expect("active admission charge is resident");
        schedule(&self.inner, &mut state);
        record_admission_state(&state);
        drop(state);
    }
}

fn lock_state(inner: &AdmissionInner) -> std::sync::MutexGuard<'_, AdmissionState> {
    inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn can_admit(used: ColdWorkWeight, requested: ColdWorkWeight, capacity: ColdWorkWeight) -> bool {
    used.checked_add(requested)
        .is_some_and(|total| total.fits(capacity))
}

fn schedule(inner: &AdmissionInner, state: &mut AdmissionState) {
    while let Some(waiter) = state.queue.front() {
        if !can_admit(state.used, waiter.weight, inner.config.capacity) {
            break;
        }
        let waiter = state.queue.pop_front().expect("front waiter exists");
        state.used = state
            .used
            .checked_add(waiter.weight)
            .expect("scheduled admission charge fits");
        waiter.granted.store(true, Ordering::Release);
        waiter.notify.notify_one();
    }
}

fn record_rejection(error: ColdAdmissionError) {
    metrics::counter!(
        "overview_cold_reject_total",
        "reason" => error.metric_reason()
    )
    .increment(1);
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Prometheus gauges use f64 and configured admission bounds are small"
)]
fn record_admission_state(state: &AdmissionState) {
    metrics::gauge!("overview_cold_queue_depth").set(state.queue.len() as f64);
    metrics::gauge!("overview_cold_work_inflight", "kind" => "workers")
        .set(f64::from(state.used.workers));
    metrics::gauge!("overview_inflight_bytes", "kind" => "pgm")
        .set(f64::from(state.used.pgm_bytes) * BYTE_WEIGHT_QUANTUM as f64);
    metrics::gauge!("overview_inflight_bytes", "kind" => "decoded")
        .set(f64::from(state.used.decoded_bytes) * BYTE_WEIGHT_QUANTUM as f64);
    metrics::gauge!("overview_open_files").set(f64::from(state.used.file_descriptors));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(workers: u32, queue: usize) -> ColdAdmissionConfig {
        ColdAdmissionConfig {
            max_workers: workers,
            max_queue: queue,
            per_request_parallelism: 1,
            wait_timeout: Duration::from_secs(30),
            retry_after_seconds: 1,
            capacity: ColdWorkWeight {
                workers,
                pgm_bytes: workers,
                decoded_bytes: workers,
                cpu: workers,
                file_descriptors: workers,
                read_bytes: workers,
                write_bytes: workers,
                publications: workers,
            },
        }
    }

    fn one() -> ColdWorkWeight {
        ColdWorkWeight {
            workers: 1,
            pgm_bytes: 1,
            decoded_bytes: 1,
            cpu: 1,
            file_descriptors: 1,
            read_bytes: 1,
            write_bytes: 1,
            publications: 1,
        }
    }

    async fn wait_for_queue(admission: &ColdAdmission, expected: usize) {
        for _ in 0..1_000 {
            if admission.queued() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("queue did not reach {expected}");
    }

    #[tokio::test]
    async fn fifo_waiters_are_admitted_in_ticket_order() {
        let admission = ColdAdmission::new(config(1, 4)).expect("valid config");
        let active = admission.acquire(one()).await.expect("first permit");
        let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
        let release_second = Arc::new(Notify::new());

        let second_admission = admission.clone();
        let second_order = order_tx.clone();
        let second_release = Arc::clone(&release_second);
        let second = tokio::spawn(async move {
            let _permit = second_admission
                .acquire(one())
                .await
                .expect("second permit");
            second_order.send(2_u8).expect("record second");
            second_release.notified().await;
        });
        wait_for_queue(&admission, 1).await;

        let third_admission = admission.clone();
        let third_order = order_tx;
        let third = tokio::spawn(async move {
            let _permit = third_admission.acquire(one()).await.expect("third permit");
            third_order.send(3_u8).expect("record third");
        });
        wait_for_queue(&admission, 2).await;
        drop(active);

        assert_eq!(order_rx.recv().await, Some(2));
        assert!(matches!(
            order_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        release_second.notify_one();
        assert_eq!(order_rx.recv().await, Some(3));
        second.await.expect("second task");
        third.await.expect("third task");
        assert_eq!(admission.queued(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_waiter_removes_its_ticket() {
        let admission = ColdAdmission::new(config(1, 1)).expect("valid config");
        let active = admission.acquire(one()).await.expect("first permit");
        let waiting_admission = admission.clone();
        let waiting = tokio::spawn(async move {
            let _permit = waiting_admission
                .acquire(one())
                .await
                .expect("queued permit");
        });
        wait_for_queue(&admission, 1).await;
        waiting.abort();
        drop(waiting.await);
        wait_for_queue(&admission, 0).await;
        drop(active);
        let _replacement = admission.acquire(one()).await.expect("replacement permit");
    }

    #[tokio::test]
    async fn one_exhausted_weight_dimension_blocks_otherwise_free_workers() {
        let mut weighted = config(2, 2);
        weighted.capacity.pgm_bytes = 1;
        let admission = ColdAdmission::new(weighted).expect("valid config");
        let active = admission.acquire(one()).await.expect("first permit");
        let waiting_admission = admission.clone();
        let waiting = tokio::spawn(async move {
            let _permit = waiting_admission
                .acquire(one())
                .await
                .expect("weighted permit");
        });
        wait_for_queue(&admission, 1).await;
        drop(active);
        waiting.await.expect("waiting task");
        assert_eq!(admission.queued(), 0);
    }

    #[tokio::test]
    async fn a_full_queue_and_an_oversized_weight_are_typed() {
        let admission = ColdAdmission::new(config(1, 1)).expect("valid config");
        let active = admission.acquire(one()).await.expect("first permit");
        let waiting_admission = admission.clone();
        let waiting = tokio::spawn(async move { waiting_admission.acquire(one()).await });
        wait_for_queue(&admission, 1).await;

        assert_eq!(
            admission.acquire(one()).await.expect_err("queue is full"),
            ColdAdmissionError::QueueFull
        );
        assert_eq!(
            admission
                .acquire(ColdWorkWeight {
                    workers: 2,
                    ..one()
                })
                .await
                .expect_err("weight exceeds capacity"),
            ColdAdmissionError::WeightExceedsCapacity
        );
        waiting.abort();
        drop(waiting.await);
        drop(active);
    }

    #[tokio::test]
    async fn timed_out_waiter_releases_its_fifo_ticket() {
        let mut limits = config(1, 2);
        limits.wait_timeout = Duration::from_millis(10);
        let admission = ColdAdmission::new(limits).expect("valid config");
        let active = admission.acquire(one()).await.expect("first permit");
        let waiting_admission = admission.clone();
        let waiting = tokio::spawn(async move { waiting_admission.acquire(one()).await });
        wait_for_queue(&admission, 1).await;

        assert!(matches!(
            waiting.await.expect("waiting task"),
            Err(ColdAdmissionError::TimedOut)
        ));
        assert_eq!(admission.queued(), 0);
        drop(active);
        let _replacement = admission.acquire(one()).await.expect("replacement permit");
    }
}
