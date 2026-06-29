//! Connection pool for `PostgreSQL` collection.
//!
//! The main connection serves instance-wide metrics. Per-database connections
//! serve database-local metrics. Setup errors use `anyhow::Result`; query
//! errors stay as `tokio_postgres::Error` so callers can inspect SQLSTATE.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use crate::server_major;

/// Connectable, non-template databases in deterministic order.
pub const ENUMERATE_SQL: &str = "/* pg_kronika pool */ SELECT datname \
    FROM pg_catalog.pg_database \
    WHERE datallowconn AND NOT datistemplate \
      AND pg_catalog.has_database_privilege(datname, 'CONNECT') \
    ORDER BY datname";

/// Maximum per-database connections the pool opens by default.
pub const DEFAULT_MAX_DATABASES: usize = 20;

/// Session GUCs applied to every pool connection (main and per-db) via the
/// connection config, so they take effect before the first query.
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

/// Build a connection config from the base DSN with session settings applied.
/// `dbname` overrides the target database for per-db connections.
///
/// Settings go through structured `tokio_postgres::Config` setters (not string
/// concatenation), so any libpq DSN form — key=value or URI — is handled, and
/// `application_name` carried in `base_dsn` is preserved.
///
/// # Errors
/// Fails if `base_dsn` is not a parseable connection string.
fn build_config(
    base_dsn: &str,
    session: &SessionConfig,
    dbname: Option<&str>,
) -> anyhow::Result<tokio_postgres::Config> {
    let mut cfg: tokio_postgres::Config = base_dsn.parse().context("parse DSN")?;
    if let Some(db) = dbname {
        cfg.dbname(db);
    }
    cfg.connect_timeout(Duration::from_secs(5));
    cfg.keepalives(true);
    cfg.keepalives_idle(Duration::from_secs(30));
    cfg.keepalives_interval(Duration::from_secs(10));
    cfg.keepalives_retries(3);
    // jit omitted from startup options: the jit GUC predates PG11, and the
    // collector's short queries would not benefit from it.
    cfg.options(format!(
        "-c statement_timeout={} -c lock_timeout={} -c idle_in_transaction_session_timeout={}",
        session.statement_timeout_ms, session.lock_timeout_ms, session.idle_in_tx_timeout_ms
    ));
    Ok(cfg)
}

/// Apply the exclude set and enforce the database cap on enumerated names.
///
/// # Errors
/// Returns an error if more than `max_databases` connectable databases remain
/// after exclusion — narrow the set via the exclude list rather than have the
/// collector silently open an unbounded number of backends.
fn select_targets<S: std::hash::BuildHasher>(
    names: Vec<String>,
    exclude: &HashSet<String, S>,
    max_databases: usize,
) -> anyhow::Result<Vec<String>> {
    let targets: Vec<String> = names
        .into_iter()
        .filter(|db| !exclude.contains(db))
        .collect();
    anyhow::ensure!(
        targets.len() <= max_databases,
        "connectable databases ({}) exceed the cap ({}); narrow the set via KRONIKA_PG_EXCLUDE_DATABASES",
        targets.len(),
        max_databases
    );
    Ok(targets)
}

/// List target databases for the pool, minus the configured exclude set.
///
/// # Errors
/// Returns an error if the query fails or the connectable database count
/// exceeds `max_databases` after filtering.
pub async fn enumerate_databases<S: std::hash::BuildHasher + Sync>(
    client: &Client,
    exclude: &HashSet<String, S>,
    max_databases: usize,
) -> anyhow::Result<Vec<String>> {
    let rows = client
        .query(ENUMERATE_SQL, &[])
        .await
        .context("enumerate databases query")?;
    let names: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    select_targets(names, exclude, max_databases)
}

/// Adaptive `statement_timeout` for heavy database-local queries.
///
/// Call `grow` after SQLSTATE `57014` (`statement_timeout`). Do not grow after
/// `55P03` (`lock_timeout`); that indicates lock contention, not query cost.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveTimeout {
    current_ms: u64,
    cap_ms: u64,
}

impl AdaptiveTimeout {
    /// Construct a timeout, clamping `start_ms` to `cap_ms`.
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

/// Per-database client plus its driver task.
///
/// Drop aborts the driver because dropping a `JoinHandle` detaches the task.
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

/// Connection pool state for one `PostgreSQL` instance.
///
/// `target` records the last enumerated database set used by coverage accessors.
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

/// Open a connection from a structured config and spawn its driver.
async fn open(
    cfg: tokio_postgres::Config,
) -> anyhow::Result<(Client, JoinHandle<()>, Option<u32>)> {
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
        let cfg = build_config(base_dsn, &session, None)?;
        let (main, main_conn, major) = open(cfg).await?;
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

    /// Reconcile per-db clients with current connectable databases.
    ///
    /// Skips work until `interval` elapses unless the pool is empty. Failed
    /// per-db connects are logged and retried on the next refresh. Order is not
    /// stable; callers should use `datname`.
    ///
    /// # Errors
    /// Fails if enumerating databases fails or the connectable count exceeds
    /// `max_databases` after applying the exclude set.
    pub async fn refresh(
        &mut self,
        interval: Duration,
        max_databases: usize,
    ) -> anyhow::Result<()> {
        if !self.per_db.is_empty() && self.last_refresh.elapsed() < interval {
            return Ok(());
        }
        let target = enumerate_databases(&self.main, &self.exclude, max_databases).await?;
        let target_set: HashSet<&str> = target.iter().map(String::as_str).collect();
        self.per_db
            .retain(|c| target_set.contains(c.datname.as_str()));
        let have: HashSet<String> = self.per_db.iter().map(|c| c.datname.clone()).collect();
        for db in &target {
            if have.contains(db) {
                continue;
            }
            let cfg = build_config(&self.base_dsn, &self.session, Some(db))?;
            match open(cfg).await {
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

    /// Reopen the main connection after failover or restart.
    ///
    /// Refreshes `server_major` from the new handshake. Call before each
    /// snapshot so recovered collectors use the new server version.
    ///
    /// # Errors
    /// Fails if reconnection fails or the new server reports no version.
    pub async fn ensure_main(&mut self) -> anyhow::Result<()> {
        if !self.main.is_closed() {
            return Ok(());
        }
        let cfg = build_config(&self.base_dsn, &self.session, None)?;
        let (client, conn, major) = open(cfg).await?;
        let Some(major) = major else {
            conn.abort();
            anyhow::bail!("server reported no parseable server_version");
        };
        self.main_conn.abort();
        self.main = client;
        self.main_conn = conn;
        self.server_major = major;
        Ok(())
    }
}

impl Drop for ConnectionPool {
    fn drop(&mut self) {
        // DatabaseConn::drop aborts per-db drivers; abort only the main driver here.
        self.main_conn.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn test_session() -> SessionConfig {
        SessionConfig {
            statement_timeout_ms: 15_000,
            lock_timeout_ms: 1_000,
            idle_in_tx_timeout_ms: 10_000,
        }
    }

    #[test]
    fn build_config_sets_keepalives_retries_without_jit() {
        let cfg = build_config("host=h dbname=postgres user=u", &test_session(), None)
            .expect("a valid DSN must build");
        assert_eq!(cfg.get_keepalives_retries(), Some(3));
        let options = cfg.get_options().unwrap_or_default();
        assert!(
            options.contains("statement_timeout=15000"),
            "session timeouts must reach startup options: {options}"
        );
        assert!(
            !options.contains("jit"),
            "jit must stay out of startup options for PG10 safety: {options}"
        );
    }

    #[test]
    fn build_config_overrides_dbname_and_preserves_application_name() {
        let cfg = build_config(
            "host=h dbname=postgres application_name=pg_kronika-collector/9.9",
            &test_session(),
            Some("payments"),
        )
        .expect("a valid DSN must build");
        assert_eq!(cfg.get_dbname(), Some("payments"));
        assert_eq!(cfg.get_application_name(), Some("pg_kronika-collector/9.9"));
    }

    #[test]
    fn build_config_rejects_unparseable_dsn() {
        assert!(build_config("host=h port=not_a_number", &test_session(), None).is_err());
    }

    #[test]
    fn select_targets_filters_excluded_names() {
        let names = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let exclude: HashSet<String> = ["b".to_owned()].into();
        let targets = select_targets(names, &exclude, 10).expect("under cap");
        assert_eq!(targets, ["a", "c"]);
    }

    #[test]
    fn select_targets_accepts_exactly_max() {
        let names: Vec<String> = (0..5).map(|i| format!("db{i}")).collect();
        let exclude = HashSet::new();
        let targets = select_targets(names, &exclude, 5).expect("at cap");
        assert_eq!(targets.len(), 5);
    }

    #[test]
    fn select_targets_rejects_over_cap() {
        let names: Vec<String> = (0..6).map(|i| format!("db{i}")).collect();
        let exclude = HashSet::new();
        assert!(select_targets(names, &exclude, 5).is_err());
    }
}
