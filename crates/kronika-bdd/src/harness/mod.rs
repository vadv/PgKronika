//! Reusable BDD harness that implements `docs/bdd-testing-guide.md`.
//!
//! The harness is the transport under a scenario: it boots the matrix once,
//! opens named sessions, snapshots the collector, decodes a section generically,
//! and compares its rows to values written in the `.feature`. The scenario's
//! meaning — the setup SQL, the expected values, the oracle kind, and the row
//! key — stays in the `.feature`; the harness never hides it.
//!
//! ## What a per-feature conversion calls
//!
//! - [`HarnessState`] is embedded in the cucumber `World`; every step operates
//!   on `world.harness`.
//! - Session steps ([`session`]): `open`, `open_holding`, `open_blocking`.
//! - Snapshot step ([`snapshot`]): [`snapshot::take`].
//! - Row assertion ([`assert_row`]): [`assert_row::assert_row`].
//! - Oracle ([`oracle`]): [`oracle::assert_oracle`].
//! - Failure context ([`dump`]): [`dump::section_dump`], used by the assertion
//!   error paths.
//! - Cleanup ([`HarnessState::cleanup`]): called from the `after` hook.
//!
//! A scenario selects its cluster with [`HarnessState::use_database`], which
//! borrows the shared matrix and creates a uniquely named database for state
//! isolation.

pub(crate) mod assert_row;
pub(crate) mod dump;
pub(crate) mod expected;
pub(crate) mod oracle;
pub(crate) mod session;
pub(crate) mod snapshot;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::cluster::{Cluster, shared_matrix};
use session::Session;

/// Per-scenario harness state, embedded in the cucumber `World`.
///
/// Holds the selected cluster, the isolated database name, the named sessions,
/// and the last snapshot's sealed-segment path. All of it is torn down by
/// [`cleanup`](Self::cleanup) in the `after` hook.
#[derive(Debug, Default)]
pub(crate) struct HarnessState {
    /// The cluster this scenario runs against, once selected.
    cluster: Option<&'static Cluster>,
    /// The isolated database created for this scenario, if any.
    database: Option<String>,
    /// A second isolated database for per-database fan-out scenarios, if any.
    extra_database: Option<String>,
    /// Named sessions opened by `session "X" runs ...` steps.
    sessions: BTreeMap<String, Session>,
    /// Sealed segment from the most recent `snapshots the segment` step.
    segment: Option<PathBuf>,
    /// The collector's stderr from the most recent snapshot, for failure dumps.
    collector_log: Option<String>,
    /// Prepared transactions that must be rolled back before the database is
    /// dropped. A prepared transaction is not tied to a session, so
    /// `ROLLBACK PREPARED` must run explicitly; otherwise `DROP DATABASE` fails
    /// because the prepared xact still holds locks in the database.
    pending_rollbacks: Vec<String>,
}

impl HarnessState {
    /// Select the cluster of the given major and create an isolated database.
    ///
    /// Boots the shared matrix on first use. The database name is made unique
    /// per call so scenarios never share table state. Returns the connection
    /// string of the new database for sessions to use.
    ///
    /// # Errors
    ///
    /// Returns an error if the matrix has no cluster of that major, or if the
    /// database cannot be created.
    pub(crate) async fn use_database(&mut self, major: u32, label: &str) -> Result<String> {
        let matrix = shared_matrix().await?;
        let cluster = matrix
            .iter()
            .find(|cluster| cluster.major() == major)
            .with_context(|| format!("matrix has no PostgreSQL {major} cluster"))?;
        let dbname = unique_database_name(label, major);
        create_database(cluster, &dbname).await?;
        let dsn = cluster.conn_string_db(&dbname);
        self.cluster = Some(cluster);
        self.database = Some(dbname);
        Ok(dsn)
    }

    /// The selected cluster, or an error if no `use_database` step ran.
    pub(crate) fn cluster(&self) -> Result<&'static Cluster> {
        self.cluster
            .context("no cluster selected; a scenario must open a database first")
    }

    /// The isolated database name, or an error if none was created.
    pub(crate) fn database(&self) -> Result<&str> {
        self.database
            .as_deref()
            .context("no database selected; a scenario must open a database first")
    }

    /// The connection string of the scenario's isolated database.
    ///
    /// # Errors
    ///
    /// Returns an error if no `use_database` step has run.
    pub(crate) fn database_dsn(&self) -> Result<String> {
        let cluster = self.cluster()?;
        Ok(cluster.conn_string_db(self.database()?))
    }

    /// Create a second uniquely named database on the scenario's cluster and
    /// return its connection string.
    ///
    /// Serves per-database fan-out scenarios that need more than one pool
    /// target; dropped by [`cleanup`](Self::cleanup) like the primary database.
    ///
    /// # Errors
    ///
    /// Returns an error if no cluster is selected, an extra database already
    /// exists, or creation fails.
    pub(crate) async fn create_extra_database(&mut self, label: &str) -> Result<String> {
        anyhow::ensure!(
            self.extra_database.is_none(),
            "the scenario already created an extra database"
        );
        let cluster = self.cluster()?;
        let dbname = unique_database_name(label, cluster.major());
        create_database(cluster, &dbname).await?;
        let dsn = cluster.conn_string_db(&dbname);
        self.extra_database = Some(dbname);
        Ok(dsn)
    }

    /// The extra database's name, or an error if none was created.
    pub(crate) fn extra_database(&self) -> Result<&str> {
        self.extra_database
            .as_deref()
            .context("no extra database; run the extra pool-target database step first")
    }

    /// Insert an opened session under `name`, replacing any previous one.
    pub(crate) fn insert_session(&mut self, name: String, session: Session) {
        self.sessions.insert(name, session);
    }

    /// The session opened under `name`, or an error naming it.
    pub(crate) fn session(&self, name: &str) -> Result<&Session> {
        self.sessions
            .get(name)
            .with_context(|| format!("no session named {name:?} was opened"))
    }

    /// Record the sealed segment produced by a snapshot step.
    pub(crate) fn set_segment(&mut self, path: PathBuf) {
        self.segment = Some(path);
    }

    /// The most recent sealed-segment path, or an error if none was taken.
    pub(crate) fn segment(&self) -> Result<&PathBuf> {
        self.segment
            .as_ref()
            .context("no snapshot taken; run `When the collector snapshots the segment` first")
    }

    /// Record the collector's stderr from the most recent snapshot.
    pub(crate) fn set_collector_log(&mut self, log: String) {
        self.collector_log = Some(log);
    }

    /// The subprocess logs for a failure dump: the cluster's `server.log`
    /// followed by the collector's captured stderr.
    ///
    /// The guide requires both `postgres` and collector output in a failure
    /// report; an assertion step passes this to the dump helper.
    ///
    /// # Errors
    ///
    /// Returns an error if no cluster has been selected.
    pub(crate) fn failure_log(&self) -> Result<String> {
        let server_log = self.cluster()?.server_log();
        let collector_log = self.collector_log.as_deref().unwrap_or("(not captured)");
        Ok(format!(
            "--- postgres server.log ---\n{server_log}\n--- collector stderr ---\n{collector_log}"
        ))
    }

    /// Resolve a `[Name]` placeholder to that session's backend pid.
    ///
    /// # Errors
    ///
    /// Returns an error if no session was opened under `name`.
    pub(crate) fn placeholder_pid(&self, name: &str) -> Result<i32> {
        Ok(self.session(name)?.backend_pid())
    }

    /// Register a prepared transaction GID for cleanup.
    ///
    /// `ROLLBACK PREPARED` runs in [`cleanup`](Self::cleanup) before the
    /// database is dropped. A prepared transaction is not tied to any session;
    /// it persists until explicitly committed or rolled back, and it holds locks
    /// that prevent `DROP DATABASE` from succeeding.
    pub(crate) fn add_rollback_prepared(&mut self, gid: String) {
        self.pending_rollbacks.push(gid);
    }

    /// Poll `pg_stat_progress_vacuum` until a row for session `name` appears.
    ///
    /// # Errors
    ///
    /// Returns an error if no session named `name` exists, the probe connection
    /// fails, the vacuum completes before being observed, or the timeout elapses.
    pub(crate) async fn wait_for_vacuum_progress(&mut self, name: &str) -> Result<()> {
        let dsn = self.database_dsn()?;
        let session = self
            .sessions
            .get_mut(name)
            .with_context(|| format!("no session named {name:?} was opened"))?;
        session.wait_for_vacuum_progress(&dsn).await
    }

    /// Roll back held transactions, abort blocking tasks, and drop the scenario
    /// database. Runs from the `after` hook, so it must not itself panic; every
    /// failure is logged and the rest of the teardown still runs.
    pub(crate) async fn cleanup(&mut self) {
        for (name, session) in std::mem::take(&mut self.sessions) {
            if let Err(err) = session.close().await {
                eprintln!("=== BDD cleanup: session {name:?}: {err:#} ===");
            }
        }
<<<<<<< HEAD
        // Roll back prepared transactions before dropping the database; a
        // prepared xact prevents DROP DATABASE from completing.
        if let Some(cluster) = self.cluster {
            for gid in std::mem::take(&mut self.pending_rollbacks) {
                if let Err(err) = rollback_prepared(cluster, &gid).await {
                    eprintln!("=== BDD cleanup: ROLLBACK PREPARED {gid:?}: {err:#} ===");
                }
            }
        }
        if let (Some(cluster), Some(dbname)) = (self.cluster, self.database.take())
            && let Err(err) = drop_database(cluster, &dbname).await
        {
            eprintln!("=== BDD cleanup: drop database {dbname:?}: {err:#} ===");
=======
        let scenario_dbs = [self.database.take(), self.extra_database.take()];
        for dbname in scenario_dbs.into_iter().flatten() {
            if let Some(cluster) = self.cluster
                && let Err(err) = drop_database(cluster, &dbname).await
            {
                eprintln!("=== BDD cleanup: drop database {dbname:?}: {err:#} ===");
            }
>>>>>>> 3ee4369 (bdd: connection_pool — конвертация на стандарт (SQL + оракулы))
        }
        self.segment = None;
        self.collector_log = None;
    }
}

/// A database name unique to one scenario run: `kronika_<label>_<major>_<pid>_<seq>`.
///
/// The process id and a monotonic counter keep concurrent scenarios and repeats
/// from colliding on the shared cluster.
fn unique_database_name(label: &str, major: u32) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("kronika_{safe}_{major}_{}_{seq}", std::process::id())
}

/// Create `dbname` on `cluster`; the name is generated, so it is a safe ident.
async fn create_database(cluster: &Cluster, dbname: &str) -> Result<()> {
    ensure_safe_ident(dbname)?;
    let admin = cluster.connect().await?;
    admin
        .client()
        .batch_execute(&format!("CREATE DATABASE {dbname}"))
        .await
        .with_context(|| format!("create database {dbname}"))?;
    Ok(())
}

/// Run `ROLLBACK PREPARED` for `gid` on `cluster`'s admin connection.
///
/// Called from cleanup to release a prepared transaction that is not tied to
/// any session. The GID must contain only ASCII alphanumerics and underscores.
async fn rollback_prepared(cluster: &Cluster, gid: &str) -> Result<()> {
    anyhow::ensure!(
        !gid.is_empty() && gid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "prepared transaction gid {gid:?} contains unsafe characters"
    );
    let admin = cluster.connect().await?;
    admin
        .client()
        .batch_execute(&format!("ROLLBACK PREPARED '{gid}'"))
        .await
        .with_context(|| format!("ROLLBACK PREPARED {gid:?}"))?;
    Ok(())
}

/// Drop `dbname` on `cluster`, forcing out any lingering connection.
async fn drop_database(cluster: &Cluster, dbname: &str) -> Result<()> {
    ensure_safe_ident(dbname)?;
    let admin = cluster.connect().await?;
    admin
        .client()
        .batch_execute(&format!("DROP DATABASE IF EXISTS {dbname} WITH (FORCE)"))
        .await
        .with_context(|| format!("drop database {dbname}"))?;
    Ok(())
}

/// Reject a database name that is not a bare lowercase identifier.
///
/// Names are generated by [`unique_database_name`], so this only guards against
/// a future caller passing raw input into an interpolated `CREATE DATABASE`.
fn ensure_safe_ident(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "database name {name:?} is not a safe identifier"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ensure_safe_ident, unique_database_name};

    #[test]
    fn unique_names_differ_across_calls() {
        let a = unique_database_name("archiver", 17);
        let b = unique_database_name("archiver", 17);
        assert_ne!(a, b, "the sequence counter makes each name unique");
        assert!(
            a.starts_with("kronika_archiver_17_"),
            "name carries the label"
        );
    }

    #[test]
    fn label_is_sanitized_into_a_safe_ident() {
        let name = unique_database_name("row lock!", 14);
        assert!(
            ensure_safe_ident(&name).is_ok(),
            "spaces and punctuation become underscores: {name}"
        );
    }

    #[test]
    fn rejects_an_unsafe_ident() {
        assert!(ensure_safe_ident("drop; --").is_err());
        assert!(ensure_safe_ident("").is_err());
        assert!(ensure_safe_ident("Uppercase").is_err());
    }

    #[test]
    fn accepts_a_generated_name() {
        assert!(ensure_safe_ident("kronika_archiver_17_1234_0").is_ok());
    }
}
