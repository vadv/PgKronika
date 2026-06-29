//! Multi-database connection pool: one main connection for instance-wide
//! metrics (reopened on failover), one per database for database-local
//! metrics.
//!
//! Pool setup returns `anyhow::Result`; per-query errors stay
//! `tokio_postgres::Error` via the handed-out `Client`, so callers can match
//! SQLSTATE 57014/55P03.

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Context;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use crate::server_major;

/// Databases this role may actually connect to (not just `datallowconn`),
/// templates excluded, deterministic order.
pub const ENUMERATE_SQL: &str = "/* pg_kronika pool */ SELECT datname \
    FROM pg_catalog.pg_database \
    WHERE datallowconn AND NOT datistemplate \
      AND pg_catalog.has_database_privilege(datname, 'CONNECT') \
    ORDER BY datname";

/// List target databases for the pool, minus the configured exclude set.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn enumerate_databases<S: std::hash::BuildHasher + Sync>(
    client: &Client,
    exclude: &HashSet<String, S>,
) -> Result<Vec<String>, tokio_postgres::Error> {
    let rows = client.query(ENUMERATE_SQL, &[]).await?;
    Ok(rows
        .iter()
        .map(|r| r.get::<_, String>(0))
        .filter(|db| !exclude.contains(db))
        .collect())
}

/// Replace (or append) `dbname=` in a libpq key=value connection string.
#[must_use]
pub fn replace_dbname(dsn: &str, datname: &str) -> String {
    let mut found = false;
    let mut parts: Vec<String> = dsn
        .split_whitespace()
        .map(|tok| {
            if tok.starts_with("dbname=") {
                found = true;
                format!("dbname={datname}")
            } else {
                tok.to_owned()
            }
        })
        .collect();
    if !found {
        parts.push(format!("dbname={datname}"));
    }
    parts.join(" ")
}

/// Session GUCs applied to every pool connection (main and per-db) via the
/// connection string, so they take effect before the first query.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "field names follow PostgreSQL GUC naming convention"
)]
pub struct SessionConfig {
    /// Maximum query execution time in milliseconds.
    pub statement_timeout_ms: u64,
    /// Maximum time to acquire a lock in milliseconds.
    pub lock_timeout_ms: u64,
    /// Maximum time to hold an open transaction without activity in milliseconds.
    pub idle_in_tx_timeout_ms: u64,
}

/// `jit=off`: collector queries are short, JIT costs more than it saves.
/// `lock_timeout` must stay below `statement_timeout` or it never fires.
#[must_use]
pub fn session_options(cfg: &SessionConfig) -> String {
    format!(
        "options='-c statement_timeout={} -c lock_timeout={} \
         -c idle_in_transaction_session_timeout={} -c jit=off'",
        cfg.statement_timeout_ms, cfg.lock_timeout_ms, cfg.idle_in_tx_timeout_ms
    )
}

/// Append session options and keepalives to a base DSN. Keepalives let a dead
/// connection to a failed primary surface in seconds, not the system default.
#[must_use]
pub fn apply_session_dsn(base_dsn: &str, cfg: &SessionConfig) -> String {
    format!(
        "{base_dsn} {} connect_timeout=5 \
         keepalives=1 keepalives_idle=30 keepalives_interval=10 keepalives_count=3",
        session_options(cfg)
    )
}

/// Adaptive `statement_timeout` for heavy queries (sizes/schema): one per
/// `PgKronika` instance, ratchets up only.
///
/// The server-side timeout is a backstop — Postgres kills the query even if
/// the collector hangs or is OOM. The caller calls `grow` only on a `57014`
/// (`statement_timeout`) kill; on `55P03` (`lock_timeout`) it does NOT — that
/// is a foreign lock, not query cost.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveTimeout {
    current_ms: u64,
    cap_ms: u64,
}

impl AdaptiveTimeout {
    /// Create a new instance; clamps `start_ms` to `cap_ms` if it exceeds it.
    #[must_use]
    pub fn new(start_ms: u64, cap_ms: u64) -> Self {
        Self {
            current_ms: start_ms.min(cap_ms),
            cap_ms,
        }
    }
    /// Current timeout value in milliseconds.
    #[must_use]
    pub const fn current_ms(&self) -> u64 {
        self.current_ms
    }
    /// Double, clamped to the cap. No-op at the cap.
    pub fn grow(&mut self) {
        self.current_ms = self.current_ms.saturating_mul(2).min(self.cap_ms);
    }
    /// Returns `true` when the timeout has reached the cap.
    #[must_use]
    pub const fn at_cap(&self) -> bool {
        self.current_ms >= self.cap_ms
    }
}

/// One per-database connection. The spawned connection-future is aborted on
/// drop (a dropped `JoinHandle` does NOT cancel the task by itself), so a
/// removed database leaves no driver task running.
#[derive(Debug)]
pub struct DatabaseConn {
    /// Name of the database this connection targets.
    pub datname: String,
    client: Client,
    conn: JoinHandle<()>,
}

impl DatabaseConn {
    /// Returns the underlying client for issuing queries.
    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }
}

impl Drop for DatabaseConn {
    fn drop(&mut self) {
        self.conn.abort();
    }
}

/// Pool: one main connection (instance-wide) plus one per database.
///
/// `target` is the last enumerated database set, so coverage (which databases
/// were reachable) is computable from pool state.
#[derive(Debug)]
pub struct ConnectionPool {
    base_dsn: String,
    session: SessionConfig,
    exclude: HashSet<String>,
    main: Client,
    main_conn: JoinHandle<()>,
    server_major: u32,
    per_db: Vec<DatabaseConn>,
    target: Vec<String>,
    last_refresh: Instant,
}

/// Open a connection with the session DSN already applied; spawn its driver.
async fn open(dsn: &str) -> anyhow::Result<(Client, JoinHandle<()>, Option<u32>)> {
    let cfg: tokio_postgres::Config = dsn.parse().context("parse DSN")?;
    let (client, connection) = cfg.connect(NoTls).await.context("connect")?;
    let major = server_major(connection.parameter("server_version"));
    let handle = tokio::spawn(async move {
        drop(connection.await);
    });
    Ok((client, handle, major))
}

impl ConnectionPool {
    /// Open the main connection. The per-db pool is filled by `refresh`.
    ///
    /// # Errors
    /// Fails if the main connection cannot be established or reports no version.
    pub async fn connect(
        base_dsn: &str,
        session: SessionConfig,
        exclude: HashSet<String>,
    ) -> anyhow::Result<Self> {
        let dsn = apply_session_dsn(base_dsn, &session);
        let (main, main_conn, major) = open(&dsn).await?;
        let server_major = major.context("server reported no parseable server_version")?;
        Ok(Self {
            base_dsn: base_dsn.to_owned(),
            session,
            exclude,
            main,
            main_conn,
            server_major,
            per_db: Vec::new(),
            target: Vec::new(),
            last_refresh: Instant::now(),
        })
    }

    /// Returns the main (instance-wide) client.
    #[must_use]
    pub const fn main(&self) -> &Client {
        &self.main
    }

    /// Returns the per-database connections opened by the last `refresh`.
    #[must_use]
    pub fn per_db(&self) -> &[DatabaseConn] {
        &self.per_db
    }

    /// Returns the `PostgreSQL` major version detected at connection time.
    #[must_use]
    pub const fn server_major(&self) -> u32 {
        self.server_major
    }

    /// Reconcile the per-db pool with the live database list. Cheap to call
    /// every snapshot: works only once `interval` elapsed (or while the pool is
    /// empty). A database that fails to connect is logged and skipped — it
    /// retries next refresh. Pool order is not stable; address by `datname`.
    ///
    /// # Errors
    /// Fails only if enumerating databases on the main connection fails.
    pub async fn refresh(&mut self, interval: std::time::Duration) -> anyhow::Result<()> {
        if !self.per_db.is_empty() && self.last_refresh.elapsed() < interval {
            return Ok(());
        }
        let target = enumerate_databases(&self.main, &self.exclude)
            .await
            .context("enumerate databases")?;
        let target_set: HashSet<&str> = target.iter().map(String::as_str).collect();
        self.per_db
            .retain(|c| target_set.contains(c.datname.as_str()));
        let have: HashSet<String> = self.per_db.iter().map(|c| c.datname.clone()).collect();
        for db in &target {
            if have.contains(db) {
                continue;
            }
            let dsn = apply_session_dsn(&replace_dbname(&self.base_dsn, db), &self.session);
            match open(&dsn).await {
                Ok((client, conn, _)) => {
                    self.per_db.push(DatabaseConn {
                        datname: db.clone(),
                        client,
                        conn,
                    });
                }
                Err(err) => eprintln!("pg_kronika: per-db connect to {db} failed: {err:#}"),
            }
        }
        self.target = target;
        self.last_refresh = Instant::now();
        Ok(())
    }

    /// Databases the pool last expected to cover.
    #[must_use]
    pub fn expected(&self) -> &[String] {
        &self.target
    }

    /// Expected databases with no live connection (failed/locked out).
    #[must_use]
    pub fn uncovered(&self) -> Vec<String> {
        let have: HashSet<&str> = self.per_db.iter().map(|c| c.datname.as_str()).collect();
        self.target
            .iter()
            .filter(|d| !have.contains(d.as_str()))
            .cloned()
            .collect()
    }

    /// Reopen the main connection if it died (failover/restart). Refreshes
    /// `server_major` from the new handshake. Call before every snapshot —
    /// without it a collector survives a primary failover as a live-but-blind
    /// process and would tag metrics with a stale major.
    ///
    /// # Errors
    /// Fails if reconnection fails or the new server reports no version.
    pub async fn ensure_main(&mut self) -> anyhow::Result<()> {
        if !self.main.is_closed() {
            return Ok(());
        }
        let dsn = apply_session_dsn(&self.base_dsn, &self.session);
        let (client, conn, major) = open(&dsn).await?;
        self.main_conn.abort();
        self.main = client;
        self.main_conn = conn;
        self.server_major = major.context("server reported no parseable server_version")?;
        Ok(())
    }
}

impl Drop for ConnectionPool {
    fn drop(&mut self) {
        // Per-db drivers self-abort via DatabaseConn::drop when per_db drops;
        // only the bare main handle needs an explicit abort here.
        self.main_conn.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_dbname() {
        assert_eq!(
            replace_dbname("host=h dbname=old user=u", "new"),
            "host=h dbname=new user=u"
        );
    }

    #[test]
    fn appends_when_absent() {
        assert_eq!(
            replace_dbname("host=h user=u", "new"),
            "host=h user=u dbname=new"
        );
    }

    #[test]
    fn session_options_carry_timeouts_and_jit_off() {
        let cfg = SessionConfig {
            statement_timeout_ms: 15_000,
            lock_timeout_ms: 1_000,
            idle_in_tx_timeout_ms: 10_000,
        };
        let o = session_options(&cfg);
        assert!(o.contains("statement_timeout=15000") && o.contains("lock_timeout=1000"));
        assert!(o.contains("idle_in_transaction_session_timeout=10000") && o.contains("jit=off"));
    }

    #[test]
    fn apply_session_dsn_adds_keepalives() {
        let cfg = SessionConfig {
            statement_timeout_ms: 15_000,
            lock_timeout_ms: 1_000,
            idle_in_tx_timeout_ms: 10_000,
        };
        let d = apply_session_dsn("host=h dbname=d", &cfg);
        assert!(
            d.starts_with("host=h dbname=d ")
                && d.contains("keepalives_idle=30")
                && d.contains("connect_timeout=5")
        );
    }

    #[test]
    fn adaptive_doubles_up_to_cap() {
        let mut t = AdaptiveTimeout::new(15_000, 60_000);
        assert_eq!(t.current_ms(), 15_000);
        t.grow();
        assert_eq!(t.current_ms(), 30_000);
        t.grow();
        assert_eq!(t.current_ms(), 60_000);
        t.grow();
        assert_eq!(t.current_ms(), 60_000);
        assert!(t.at_cap());
    }

    #[test]
    fn adaptive_start_above_cap_clamps() {
        let t = AdaptiveTimeout::new(120_000, 60_000);
        assert_eq!(t.current_ms(), 60_000);
        assert!(t.at_cap());
    }

    #[test]
    fn enumerate_sql_filters_templates_noconn_and_privilege() {
        assert!(ENUMERATE_SQL.contains("datallowconn"));
        assert!(ENUMERATE_SQL.contains("NOT datistemplate"));
        assert!(ENUMERATE_SQL.contains("has_database_privilege"));
        assert!(ENUMERATE_SQL.contains("ORDER BY datname"));
    }
}
