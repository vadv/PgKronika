//! Cancellation-safe exact-key coordination for sealed-fact loads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kronika_reader::FactBuildKey;
use tokio::sync::Notify;

#[derive(Debug)]
struct Flight<T> {
    result: Mutex<Option<T>>,
    notify: Notify,
}

impl<T> Flight<T> {
    fn pending() -> Self {
        Self {
            result: Mutex::new(None),
            notify: Notify::new(),
        }
    }
}

#[derive(Debug)]
struct RegistryInner<T> {
    max_entries: usize,
    active: Mutex<HashMap<FactBuildKey, Arc<Flight<T>>>>,
    leaders: AtomicU64,
    waiters: AtomicU64,
}

/// Exact-`FactBuildKey` work registry with a hard active-entry bound.
#[derive(Debug)]
pub(crate) struct FactSingleflight<T> {
    inner: Arc<RegistryInner<T>>,
}

impl<T> Clone for FactSingleflight<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// A new exact-key flight cannot be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SingleflightError {
    CapacityUnavailable,
}

impl<T> FactSingleflight<T>
where
    T: Clone + Send + 'static,
{
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                max_entries,
                active: Mutex::new(HashMap::new()),
                leaders: AtomicU64::new(0),
                waiters: AtomicU64::new(0),
            }),
        }
    }

    /// Joins one exact-key flight or starts a detached leader.
    ///
    /// The detached coordinator owns completion after the caller is cancelled.
    pub(crate) async fn run<F, Fut, J>(
        &self,
        key: FactBuildKey,
        work: F,
        join_failure: J,
    ) -> Result<T, SingleflightError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
        J: FnOnce() -> T + Send + 'static,
    {
        let (flight, leader) = {
            let mut active = lock_active(&self.inner);
            if let Some(flight) = active.get(&key).cloned() {
                drop(active);
                (flight, false)
            } else {
                if active.len() >= self.inner.max_entries {
                    return Err(SingleflightError::CapacityUnavailable);
                }
                let flight = Arc::new(Flight::pending());
                active.insert(key, Arc::clone(&flight));
                drop(active);
                (flight, true)
            }
        };

        if leader {
            self.inner.leaders.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("overview_singleflight_builds").increment(1);
            let inner = Arc::clone(&self.inner);
            let worker_flight = Arc::clone(&flight);
            tokio::spawn(async move {
                let worker = tokio::spawn(work());
                let result = worker.await.unwrap_or_else(|_error| join_failure());
                *worker_flight
                    .result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                worker_flight.notify.notify_waiters();
                let mut active = lock_active(&inner);
                if active
                    .get(&key)
                    .is_some_and(|registered| Arc::ptr_eq(registered, &worker_flight))
                {
                    active.remove(&key);
                }
                drop(active);
            });
        } else {
            self.inner.waiters.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("overview_singleflight_waiters").increment(1);
        }

        loop {
            let notified = flight.notify.notified();
            let result = flight
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(result) = result {
                return Ok(result);
            }
            notified.await;
        }
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        lock_active(&self.inner).len()
    }

    #[cfg(feature = "qualification")]
    pub(crate) fn qualification_snapshot(&self) -> SingleflightSnapshot {
        SingleflightSnapshot {
            leaders: self.inner.leaders.load(Ordering::Relaxed),
            waiters: self.inner.waiters.load(Ordering::Relaxed),
            active: lock_active(&self.inner).len(),
        }
    }
}

#[cfg(feature = "qualification")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SingleflightSnapshot {
    pub(crate) leaders: u64,
    pub(crate) waiters: u64,
    pub(crate) active: usize,
}

fn lock_active<T>(
    inner: &RegistryInner<T>,
) -> std::sync::MutexGuard<'_, HashMap<FactBuildKey, Arc<Flight<T>>>> {
    inner
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use kronika_analytics::overview::SegmentLineageId;
    use kronika_reader::{FactKey, FileKind};

    use super::*;

    fn key(seed: u8) -> FactBuildKey {
        let fact = FactKey::derive(
            u64::from(seed),
            kronika_reader::SourceDescriptor([seed.wrapping_add(1); 32]),
            FileKind::SegmentFacts,
            1,
            1,
            1,
        );
        FactBuildKey::new(fact, SegmentLineageId([seed.wrapping_add(2); 32]))
    }

    async fn wait_for_active<T: Clone + Send + 'static>(
        flights: &FactSingleflight<T>,
        expected: usize,
    ) {
        for _ in 0..1_000 {
            if flights.active() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("flight registry did not reach {expected}");
    }

    #[tokio::test]
    async fn equal_keys_execute_the_leader_once() {
        let flights = FactSingleflight::new(4);
        let executions = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());

        let first_flights = flights.clone();
        let first_executions = Arc::clone(&executions);
        let first_release = Arc::clone(&release);
        let first = tokio::spawn(async move {
            first_flights
                .run(
                    key(1),
                    move || async move {
                        first_executions.fetch_add(1, Ordering::SeqCst);
                        first_release.notified().await;
                        7_u8
                    },
                    || 0,
                )
                .await
        });
        wait_for_active(&flights, 1).await;

        let second_flights = flights.clone();
        let second =
            tokio::spawn(async move { second_flights.run(key(1), || async { 9_u8 }, || 0).await });
        tokio::task::yield_now().await;
        release.notify_one();

        assert_eq!(first.await.expect("first task"), Ok(7));
        assert_eq!(second.await.expect("second task"), Ok(7));
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn same_fact_key_with_distinct_lineages_runs_independently() {
        let flights = FactSingleflight::new(4);
        let fact_key = key(7).fact_key();
        let first_key = FactBuildKey::new(fact_key, SegmentLineageId([0x11; 32]));
        let second_key = FactBuildKey::new(fact_key, SegmentLineageId([0x22; 32]));
        let executions = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());

        let first_flights = flights.clone();
        let first_executions = Arc::clone(&executions);
        let first_release = Arc::clone(&release);
        let first = tokio::spawn(async move {
            first_flights
                .run(
                    first_key,
                    move || async move {
                        first_executions.fetch_add(1, Ordering::SeqCst);
                        first_release.notified().await;
                        11_u8
                    },
                    || 0,
                )
                .await
        });
        let second_flights = flights.clone();
        let second_executions = Arc::clone(&executions);
        let second_release = Arc::clone(&release);
        let second = tokio::spawn(async move {
            second_flights
                .run(
                    second_key,
                    move || async move {
                        second_executions.fetch_add(1, Ordering::SeqCst);
                        second_release.notified().await;
                        22_u8
                    },
                    || 0,
                )
                .await
        });

        wait_for_active(&flights, 2).await;
        assert_eq!(
            executions.load(Ordering::SeqCst),
            2,
            "equal logical facts in distinct physical lineages must not share work"
        );
        release.notify_waiters();
        assert_eq!(first.await.expect("first lineage task"), Ok(11));
        assert_eq!(second.await.expect("second lineage task"), Ok(22));
        wait_for_active(&flights, 0).await;
    }

    #[tokio::test]
    async fn cancelling_the_request_does_not_cancel_the_leader() {
        let flights = FactSingleflight::new(4);
        let release = Arc::new(Notify::new());
        let first_flights = flights.clone();
        let first_release = Arc::clone(&release);
        let first = tokio::spawn(async move {
            first_flights
                .run(
                    key(2),
                    move || async move {
                        first_release.notified().await;
                        11_u8
                    },
                    || 0,
                )
                .await
        });
        wait_for_active(&flights, 1).await;
        first.abort();
        drop(first.await);

        let second_flights = flights.clone();
        let second =
            tokio::spawn(async move { second_flights.run(key(2), || async { 99_u8 }, || 0).await });
        release.notify_one();
        assert_eq!(second.await.expect("second task"), Ok(11));
        wait_for_active(&flights, 0).await;
    }

    #[tokio::test]
    async fn unique_key_capacity_is_typed() {
        let flights = FactSingleflight::new(1);
        let release = Arc::new(Notify::new());
        let first_flights = flights.clone();
        let first_release = Arc::clone(&release);
        let first = tokio::spawn(async move {
            first_flights
                .run(
                    key(3),
                    move || async move {
                        first_release.notified().await;
                        1_u8
                    },
                    || 0,
                )
                .await
        });
        wait_for_active(&flights, 1).await;
        assert_eq!(
            flights
                .run(key(4), || async { 2_u8 }, || 0)
                .await
                .expect_err("registry is full"),
            SingleflightError::CapacityUnavailable
        );
        release.notify_one();
        assert_eq!(first.await.expect("first task"), Ok(1));
    }

    #[tokio::test]
    async fn a_panicking_worker_completes_waiters_and_releases_the_key() {
        let flights = FactSingleflight::new(1);
        assert_eq!(
            flights
                .run(
                    key(5),
                    || async { panic!("fixture worker panic") },
                    || 13_u8,
                )
                .await,
            Ok(13)
        );
        wait_for_active(&flights, 0).await;
        assert_eq!(flights.run(key(5), || async { 21_u8 }, || 0).await, Ok(21));
    }
}
