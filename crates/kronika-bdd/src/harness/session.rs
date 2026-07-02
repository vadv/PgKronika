//! Named, persistent database sessions for BDD scenarios.
//!
//! A [`Session`] is one backend connection whose `pg_backend_pid()` is recorded
//! when it opens, so an assertion can find that session's row by pid and resolve
//! a `[Name]` placeholder. Four flavours match the guide's step vocabulary:
//!
//! - [`Session::open`] runs a statement and returns.
//! - [`Session::open_holding`] runs a statement (typically `BEGIN; …`) and keeps
//!   the transaction open until cleanup.
//! - [`Session::open_blocking`] runs a statement that is expected to block on a
//!   lock, on a spawned task, and waits — with a bounded timeout, no fixed
//!   sleep — until the backend is observed in a lock wait state.
//! - [`Session::open_background`] runs a statement on a background task without
//!   waiting for any specific wait state. Useful for long-running operations such
//!   as `VACUUM` that do not block on a lock.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// How long [`Session::open_blocking`] waits for the backend to reach a lock
/// wait state before giving up.
const BLOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll interval while waiting for a backend's lock wait state to appear.
const BLOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long [`Session::wait_for_vacuum_progress`] polls before giving up.
pub(crate) const VACUUM_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll interval while waiting for a `pg_stat_progress_vacuum` row.
const VACUUM_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One named backend connection held for the length of a scenario.
#[derive(Debug)]
pub(crate) struct Session {
    /// The session's client, shared with a blocking task when one runs.
    client: Arc<tokio_postgres::Client>,
    /// This backend's `pg_backend_pid()`, recorded at open.
    backend_pid: i32,
    /// The tokio-postgres protocol driver task; aborted in [`Session::close`].
    driver: JoinHandle<()>,
    /// The blocking statement's task, if this session was opened with `blocks`.
    blocking: Option<JoinHandle<Result<(), tokio_postgres::Error>>>,
}

impl Session {
    /// Open a session on `dsn`, run `sql`, and return once it completes.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection, the pid query, or `sql` fails.
    pub(crate) async fn open(dsn: &str, sql: &str) -> Result<Self> {
        let session = Self::connect(dsn).await?;
        session
            .client
            .batch_execute(sql)
            .await
            .context("run session SQL")?;
        Ok(session)
    }

    /// Open a session on `dsn` and run `sql` (typically `BEGIN; …`), leaving any
    /// transaction it opens uncommitted until cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection, the pid query, or `sql` fails.
    pub(crate) async fn open_holding(dsn: &str, sql: &str) -> Result<Self> {
        // Identical mechanics to `open`; the difference is intent — the caller's
        // `sql` starts a transaction and the harness never commits it, so the
        // locks it takes persist until `close`.
        Self::open(dsn, sql).await
    }

    /// Open a session on `dsn` and run `sql` on a background task, returning
    /// immediately without waiting for any particular wait state.
    ///
    /// Used for long-running statements such as `VACUUM` that do not block on a
    /// lock and whose progress is observed through a system view on a separate
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or pid query fails.
    pub(crate) async fn open_background(dsn: &str, sql: &str) -> Result<Self> {
        let mut session = Self::connect(dsn).await?;
        let (setup_sql, background_sql) = split_background_sql(sql);
        if let Some(setup_sql) = setup_sql {
            session
                .client
                .batch_execute(setup_sql)
                .await
                .context("run background session setup SQL")?;
        }
        let client = Arc::clone(&session.client);
        let sql = background_sql.to_owned();
        let blocking = tokio::spawn(async move { client.batch_execute(&sql).await });
        session.blocking = Some(blocking);
        Ok(session)
    }

    /// Open a session on `dsn`, run `sql` on a background task, and wait until
    /// this backend is observed in a lock wait state.
    ///
    /// `sql` is expected to block on a lock held by another session. The wait is
    /// bounded by [`BLOCK_WAIT_TIMEOUT`]; there is no fixed sleep.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or pid query fails, if the backend
    /// never reaches a lock wait state within the timeout, or if the statement
    /// fails before it blocks.
    pub(crate) async fn open_blocking(dsn: &str, sql: &str) -> Result<Self> {
        let mut session = Self::connect(dsn).await?;
        let client = Arc::clone(&session.client);
        let sql = sql.to_owned();
        let blocking = tokio::spawn(async move { client.batch_execute(&sql).await });
        session.blocking = Some(blocking);

        // A separate admin connection observes the blocked backend; the session's
        // own client is busy running the blocking statement.
        let (probe, probe_conn) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
            .await
            .context("connect the blocking observer")?;
        let probe_driver = tokio::spawn(async move {
            drop(probe_conn.await);
        });
        let observed = wait_for_lock(&probe, session.backend_pid, &mut session.blocking).await;
        probe_driver.abort();
        observed?;
        Ok(session)
    }

    /// This session's backend pid.
    pub(crate) const fn backend_pid(&self) -> i32 {
        self.backend_pid
    }

    /// Poll `pg_stat_progress_vacuum` on `dsn` until a row appears for this
    /// session's backend pid.
    ///
    /// Connects a probe client to `dsn` to observe vacuum progress. Bounded
    /// by [`VACUUM_WAIT_TIMEOUT`]; fails early if the background statement
    /// completes before a row is observed.
    ///
    /// # Errors
    ///
    /// Returns an error if the probe connection fails, the background task
    /// finishes before a row appears, or the timeout elapses.
    pub(crate) async fn wait_for_vacuum_progress(&mut self, dsn: &str) -> Result<()> {
        let (probe, probe_conn) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
            .await
            .context("connect the vacuum observer")?;
        let probe_driver = tokio::spawn(async move {
            drop(probe_conn.await);
        });
        let result = wait_for_progress_vacuum_row(&probe, self.backend_pid, &mut self.blocking)
            .await
            .map(|_| ());
        probe_driver.abort();
        result
    }

    /// The session's client, for an oracle that must run inside this session
    /// (e.g. to observe uncommitted state a held transaction produced).
    #[allow(
        dead_code,
        reason = "harness API for per-feature steps that run an in-session oracle; archiver uses a fresh connection"
    )]
    pub(crate) fn client(&self) -> &tokio_postgres::Client {
        &self.client
    }

    /// Roll back any open transaction, abort a blocking task, and drop the
    /// connection. Errors are returned for the caller to log; they never panic.
    pub(crate) async fn close(mut self) -> Result<()> {
        if let Some(task) = self.blocking.take() {
            task.abort();
        }
        // A held transaction is rolled back explicitly so its locks release
        // before the scenario's database is dropped. A session with no open
        // transaction treats this as a no-op.
        let rollback = self.client.batch_execute("ROLLBACK").await;
        self.driver.abort();
        rollback.context("roll back session transaction")?;
        Ok(())
    }

    /// Connect and record `pg_backend_pid()`, without running scenario SQL.
    async fn connect(dsn: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
            .await
            .context("open session connection")?;
        let driver = tokio::spawn(async move {
            drop(connection.await);
        });
        let backend_pid: i32 = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .context("read session backend pid")?
            .get(0);
        Ok(Self {
            client: Arc::new(client),
            backend_pid,
            driver,
            blocking: None,
        })
    }
}

/// Split a background session docstring into setup and the statement that must
/// keep running. This keeps `SET; VACUUM` out of `PostgreSQL`'s implicit
/// multi-statement transaction block while preserving the setup on the same
/// session.
fn split_background_sql(sql: &str) -> (Option<&str>, &str) {
    let sql = sql.trim();
    let without_trailing = sql.trim_end_matches(';').trim_end();
    if let Some((setup, statement)) = without_trailing.rsplit_once(';') {
        let setup = setup.trim();
        let statement = statement.trim();
        if !setup.is_empty() && !statement.is_empty() {
            return (Some(setup), statement);
        }
    }
    (None, sql)
}

/// Poll `pg_stat_activity` until `pid` shows a lock wait, the blocking task
/// fails, or the timeout elapses.
async fn wait_for_lock(
    observer: &tokio_postgres::Client,
    pid: i32,
    blocking: &mut Option<JoinHandle<Result<(), tokio_postgres::Error>>>,
) -> Result<()> {
    let poll = async {
        loop {
            if is_lock_waiting(observer, pid).await? {
                return Ok(());
            }
            // If the statement returned instead of blocking, the scenario's
            // premise is wrong; surface that rather than time out.
            if let Some(task) = blocking.as_mut()
                && task.is_finished()
            {
                let joined = task.await;
                anyhow::bail!(
                    "blocking statement for pid {pid} finished without blocking: {joined:?}"
                );
            }
            sleep(BLOCK_POLL_INTERVAL).await;
        }
    };
    match timeout(BLOCK_WAIT_TIMEOUT, poll).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "backend pid {pid} did not reach a lock wait state within {BLOCK_WAIT_TIMEOUT:?}"
        ),
    }
}

/// Whether `pid` is currently blocked waiting on a lock.
async fn is_lock_waiting(observer: &tokio_postgres::Client, pid: i32) -> Result<bool> {
    let row = observer
        .query_opt(
            "SELECT 1 FROM pg_stat_activity \
             WHERE pid = $1 AND wait_event_type = 'Lock'",
            &[&pid],
        )
        .await
        .context("poll pg_stat_activity for a lock wait")?;
    Ok(row.is_some())
}

/// Poll `pg_stat_progress_vacuum` on `observer` until a row appears for `pid`.
///
/// Returns the `relid` of that row. Bounded by [`VACUUM_WAIT_TIMEOUT`]; fails
/// if `pid`'s background task finishes first or the timeout elapses.
async fn wait_for_progress_vacuum_row(
    observer: &tokio_postgres::Client,
    pid: i32,
    blocking: &mut Option<JoinHandle<Result<(), tokio_postgres::Error>>>,
) -> Result<u32> {
    let poll = async {
        loop {
            if let Some(relid) = progress_vacuum_relid(observer, pid).await? {
                return Ok(relid);
            }
            if let Some(task) = blocking.as_mut()
                && task.is_finished()
            {
                anyhow::bail!(
                    "VACUUM for pid {pid} finished before pg_stat_progress_vacuum was observed"
                );
            }
            sleep(VACUUM_POLL_INTERVAL).await;
        }
    };
    match timeout(VACUUM_WAIT_TIMEOUT, poll).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "pg_stat_progress_vacuum row for pid {pid} did not appear within {VACUUM_WAIT_TIMEOUT:?}"
        ),
    }
}

/// Return the `relid` from `pg_stat_progress_vacuum` for `pid`, if a row exists.
async fn progress_vacuum_relid(observer: &tokio_postgres::Client, pid: i32) -> Result<Option<u32>> {
    let row = observer
        .query_opt(
            "SELECT relid FROM pg_stat_progress_vacuum WHERE pid = $1",
            &[&pid],
        )
        .await
        .context("poll pg_stat_progress_vacuum")?;
    Ok(row.map(|r| r.get(0)))
}
