//! Synthetic load scenarios: OLTP writes, sequential scans, lock contention,
//! background errors, and audit inserts.
//!
//! Every worker owns one connection and loops until the shared stop flag is
//! set; SQL failures inside a scenario count as observations, not aborts —
//! deadlocks and timeouts are part of the profile.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::{interval, sleep, timeout};

use crate::cluster::connect;
use crate::config::Config;

const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(15);

/// Shared counters printed with the final load summary.
#[derive(Debug, Default)]
struct Counters {
    transactions: AtomicU64,
    scans: AtomicU64,
    expected_errors: AtomicU64,
    lock_conflicts: AtomicU64,
    /// Set by whichever worker prints the first conflict, so the log shows
    /// one concrete error text instead of a bare counter.
    conflict_printed: AtomicBool,
}

impl Counters {
    fn note_conflict(&self, scenario: &str, error: &tokio_postgres::Error) {
        self.lock_conflicts.fetch_add(1, Ordering::Relaxed);
        if !self.conflict_printed.swap(true, Ordering::Relaxed) {
            // Debug keeps the cause chain; Display stops at the top frame.
            println!("load: first {scenario} conflict: {error:?}");
        }
    }
}

/// Deterministic xorshift64* stream; no external randomness in the stand.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    const fn seeded(worker: u64) -> Self {
        Self(0x9E37_79B9_7F4A_7C15 ^ (worker.wrapping_add(1) << 17))
    }

    const fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish value in `1..=bound`; bias is irrelevant for load shaping.
    fn pick(&mut self, bound: u64) -> i64 {
        i64::try_from(self.next() % bound.max(1)).unwrap_or(0) + 1
    }

    /// [`pick`](Self::pick) for `integer` columns; the driver rejects an
    /// `i64` parameter bound to an `int4` slot.
    fn pick_i32(&mut self, bound: u64) -> i32 {
        i32::try_from(self.pick(u64::from(u32::try_from(bound).unwrap_or(1)))).unwrap_or(1)
    }
}

/// Runs every scenario against `dsn` until `stop_when` resolves.
pub(crate) async fn run(
    dsn: &str,
    config: &Config,
    stop_when: impl Future<Output = ()> + Send,
) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(Counters::default());
    let mut workers = tokio::task::JoinSet::new();

    for worker in 0..config.backends {
        workers.spawn(oltp_worker(
            dsn.to_owned(),
            u64::from(worker),
            oltp_pace(config),
            Arc::clone(&stop),
            Arc::clone(&counters),
        ));
    }
    workers.spawn(seq_scan_worker(
        dsn.to_owned(),
        Arc::clone(&stop),
        Arc::clone(&counters),
    ));
    for worker in 0..3_u64 {
        workers.spawn(lock_worker(
            dsn.to_owned(),
            worker,
            Arc::clone(&stop),
            Arc::clone(&counters),
        ));
    }
    workers.spawn(error_worker(
        dsn.to_owned(),
        Arc::clone(&stop),
        Arc::clone(&counters),
    ));

    let progress_stop = Arc::clone(&stop);
    let progress_counters = Arc::clone(&counters);
    let progress = tokio::spawn(async move {
        let mut tick = interval(Duration::from_mins(1));
        tick.tick().await;
        while !progress_stop.load(Ordering::Relaxed) {
            tick.tick().await;
            println!(
                "load: {} tx, {} scans, {} lock conflicts, {} expected errors",
                progress_counters.transactions.load(Ordering::Relaxed),
                progress_counters.scans.load(Ordering::Relaxed),
                progress_counters.lock_conflicts.load(Ordering::Relaxed),
                progress_counters.expected_errors.load(Ordering::Relaxed),
            );
        }
    });

    stop_when.await;
    stop.store(true, Ordering::Relaxed);
    progress.abort();

    let deadline = timeout(WORKER_JOIN_TIMEOUT, async {
        while let Some(joined) = workers.join_next().await {
            joined
                .context("a load worker panicked")?
                .context("a load worker failed")?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    if let Ok(joined) = deadline {
        joined?;
    } else {
        // Long pg_sleep calls may outlive the deadline; the stand is done.
        workers.abort_all();
    }

    println!(
        "load: finished with {} tx, {} scans, {} lock conflicts, {} expected errors",
        counters.transactions.load(Ordering::Relaxed),
        counters.scans.load(Ordering::Relaxed),
        counters.lock_conflicts.load(Ordering::Relaxed),
        counters.expected_errors.load(Ordering::Relaxed),
    );
    Ok(())
}

/// Per-worker pause that spreads `tps` across `backends` connections.
fn oltp_pace(config: &Config) -> Duration {
    let tps = config.tps.max(1);
    Duration::from_millis(u64::from(config.backends.max(1)) * 1000 / u64::from(tps))
}

/// One OLTP loop: update an account, insert an order, read a row back, and
/// append audit traffic on a fraction of iterations.
async fn oltp_worker(
    dsn: String,
    worker: u64,
    pace: Duration,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
) -> Result<()> {
    let mut conn = connect(&dsn).await?;
    let mut rng = Rng::seeded(worker);
    let mut tick = interval(pace.max(Duration::from_millis(1)));
    while !stop.load(Ordering::Relaxed) {
        tick.tick().await;
        let account = rng.pick(10_000);
        let other = rng.pick(10_000);
        let amount = rng.pick(50_000);
        let result = async {
            let tx = conn.client_mut().transaction().await?;
            tx.execute(
                "UPDATE accounts SET balance = balance + $2, updated_at = now() WHERE id = $1",
                &[&account, &amount],
            )
            .await?;
            tx.execute(
                "INSERT INTO orders (account_id, amount, status) VALUES ($1, $2, 'new')",
                &[&account, &amount],
            )
            .await?;
            tx.query_opt("SELECT balance FROM accounts WHERE id = $1", &[&other])
                .await?;
            if account % 5 == 0 {
                let message = format!("oltp account {account}");
                tx.execute("INSERT INTO audit.logs (message) VALUES ($1)", &[&message])
                    .await?;
            }
            if account % 7 == 0 {
                let data = format!("amount={amount}");
                tx.execute(
                    "INSERT INTO audit.events (kind, data) VALUES ($1, $2)",
                    &[&i32::try_from(account % 10).unwrap_or(0), &data],
                )
                .await?;
            }
            tx.commit().await
        }
        .await;
        match result {
            Ok(()) => {
                counters.transactions.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                counters.note_conflict("oltp", &error);
                conn = reconnect_if_needed(conn, &dsn).await?;
            }
        }
    }
    Ok(())
}

/// Full scans over `staging.large_scan` evict shared buffers.
async fn seq_scan_worker(
    dsn: String,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
) -> Result<()> {
    let conn = connect(&dsn).await?;
    let mut tick = interval(Duration::from_secs(10));
    while !stop.load(Ordering::Relaxed) {
        tick.tick().await;
        if conn
            .client()
            .query_one(
                "SELECT sum(length(payload))::bigint FROM staging.large_scan",
                &[],
            )
            .await
            .is_ok()
        {
            counters.scans.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// Overlapping `FOR UPDATE` holds with worker-dependent lock order: steady
/// lock waits past `deadlock_timeout`, occasionally a real deadlock.
async fn lock_worker(
    dsn: String,
    worker: u64,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
) -> Result<()> {
    let mut conn = connect(&dsn).await?;
    let mut rng = Rng::seeded(worker.wrapping_add(100));
    while !stop.load(Ordering::Relaxed) {
        let first = rng.pick_i32(10);
        let second = rng.pick_i32(10);
        let hold_ms = f64::from(u32::try_from(300 + rng.next() % 1_200).unwrap_or(300)) / 1_000.0;
        let result = async {
            let tx = conn.client_mut().transaction().await?;
            tx.query_opt(
                "SELECT value FROM locked_resource WHERE id = $1 FOR UPDATE",
                &[&first],
            )
            .await?;
            tx.query_one("SELECT pg_sleep($1)", &[&hold_ms]).await?;
            tx.query_opt(
                "SELECT value FROM locked_resource WHERE id = $1 FOR UPDATE",
                &[&second],
            )
            .await?;
            tx.execute(
                "UPDATE locked_resource SET value = value + 1 WHERE id IN ($1, $2)",
                &[&first, &second],
            )
            .await?;
            tx.commit().await
        }
        .await;
        if let Err(error) = result {
            counters.note_conflict("lock", &error);
            conn = reconnect_if_needed(conn, &dsn).await?;
        }
        sleep(Duration::from_millis(100 + rng.next() % 400)).await;
    }
    Ok(())
}

/// Rare expected failures for the event blocks: a unique violation and a
/// statement timeout, every ~15 seconds.
async fn error_worker(dsn: String, stop: Arc<AtomicBool>, counters: Arc<Counters>) -> Result<()> {
    let conn = connect(&dsn).await?;
    let mut tick = interval(Duration::from_secs(15));
    while !stop.load(Ordering::Relaxed) {
        tick.tick().await;
        if conn
            .client()
            .execute("INSERT INTO locked_resource (id, value) VALUES (1, 0)", &[])
            .await
            .is_err()
        {
            counters.expected_errors.fetch_add(1, Ordering::Relaxed);
        }
        let timed_out = conn
            .client()
            .batch_execute(
                "SET statement_timeout = 50; SELECT pg_sleep(0.2); SET statement_timeout = 0",
            )
            .await
            .is_err();
        if timed_out {
            counters.expected_errors.fetch_add(1, Ordering::Relaxed);
            drop(
                conn.client()
                    .batch_execute("SET statement_timeout = 0")
                    .await,
            );
        }
    }
    Ok(())
}

/// Replaces a connection the server dropped (deadlock victims stay usable,
/// terminated backends do not).
async fn reconnect_if_needed(
    conn: crate::cluster::Conn,
    dsn: &str,
) -> Result<crate::cluster::Conn> {
    if conn.client().is_closed() {
        connect(dsn).await
    } else {
        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_streams_differ_per_worker() {
        let mut a = Rng::seeded(0);
        let mut b = Rng::seeded(1);
        assert_ne!(a.next(), b.next(), "seeds separate worker streams");
    }

    #[test]
    fn rng_pick_stays_in_bounds() {
        let mut rng = Rng::seeded(7);
        for _ in 0..1_000 {
            let value = rng.pick(10);
            assert!(
                (1..=10).contains(&value),
                "pick({value}) must stay in 1..=10"
            );
        }
    }

    #[test]
    fn oltp_pace_spreads_tps_across_backends() {
        let config = Config {
            root: std::path::PathBuf::from("/data"),
            backends: 20,
            tps: 200,
            filler_tables: 0,
            filler_indexes: 0,
            large_scan_rows: 0,
            duration: Duration::from_mins(1),
            chart_series: 19,
        };
        assert_eq!(
            oltp_pace(&config),
            Duration::from_millis(100),
            "20 workers at 200 tps"
        );
    }
}
