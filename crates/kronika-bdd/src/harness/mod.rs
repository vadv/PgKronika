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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use crate::cluster::{Cluster, shared_matrix};
use session::Session;

const SETTINGS_RELOAD_TIMEOUT: Duration = Duration::from_secs(10);
const SETTINGS_RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy)]
enum SettingsState {
    Loaded,
    Reset,
}

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
    /// Extra isolated databases for per-database fan-out scenarios.
    extra_databases: Vec<String>,
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
    pending_rollbacks: Vec<PreparedRollback>,
    /// Collector output directories retained until assertion steps finish.
    collector_output_dirs: Vec<TempDir>,
    /// Scenario-specific environment for the spawned collector.
    collector_env: Vec<(String, String)>,
    /// Pre-snapshot oracle reads for window assertions, keyed by section column.
    window_floors: BTreeMap<String, i64>,
    /// Cluster-level GUCs changed with `ALTER SYSTEM SET` and reset in cleanup.
    altered_system_settings: BTreeSet<String>,
}

#[derive(Debug)]
struct PreparedRollback {
    database: String,
    gid: String,
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

    /// Create a second isolated database on the already-selected cluster.
    ///
    /// The cluster must have been selected by [`use_database`](Self::use_database) first.
    /// The new database is dropped in [`cleanup`](Self::cleanup).
    pub(crate) async fn add_database(&mut self, label: &str) -> Result<String> {
        let cluster = self.cluster()?;
        let dbname = unique_database_name(label, cluster.major());
        create_database(cluster, &dbname).await?;
        let dsn = cluster.conn_string_db(&dbname);
        self.extra_databases.push(dbname);
        Ok(dsn)
    }

    /// The nth extra database name (0-indexed).
    ///
    /// # Errors
    ///
    /// Returns an error if no extra database exists at `idx`.
    pub(crate) fn extra_database_name(&self, idx: usize) -> Result<&str> {
        self.extra_databases
            .get(idx)
            .map(String::as_str)
            .with_context(|| format!("no extra database at index {idx}"))
    }

    /// The connection string for the nth extra database.
    ///
    /// # Errors
    ///
    /// Returns an error if the cluster is not selected or no extra database at `idx`.
    pub(crate) fn extra_database_dsn(&self, idx: usize) -> Result<String> {
        let cluster = self.cluster()?;
        Ok(cluster.conn_string_db(self.extra_database_name(idx)?))
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
    /// Add one environment override for the collector this scenario spawns.
    pub(crate) fn add_collector_env(&mut self, key: String, value: String) {
        self.collector_env.push((key, value));
    }

    /// Scenario-specific collector environment overrides.
    pub(crate) fn collector_env(&self) -> &[(String, String)] {
        &self.collector_env
    }

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
    pub(crate) fn add_rollback_prepared(&mut self, gid: String) -> Result<()> {
        let database = self.database()?.to_owned();
        self.pending_rollbacks
            .push(PreparedRollback { database, gid });
        Ok(())
    }

    /// Register a cluster-level GUC changed through `ALTER SYSTEM SET`.
    pub(crate) fn add_altered_system_setting(&mut self, name: String) {
        if !name.is_empty() {
            self.altered_system_settings.insert(name);
        }
    }

    /// Keep a collector output directory alive until scenario cleanup.
    pub(crate) fn retain_collector_output_dir(&mut self, dir: TempDir) {
        self.collector_output_dirs.push(dir);
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

    /// Record a pre-snapshot oracle read as the window floor for `column`.
    pub(crate) fn set_window_floor(&mut self, column: &str, value: i64) {
        self.window_floors.insert(column.to_owned(), value);
    }

    /// The window floor captured for `column`, or an error naming the missing
    /// capture step.
    pub(crate) fn window_floor(&self, column: &str) -> Result<i64> {
        self.window_floors
            .get(column)
            .copied()
            .with_context(|| format!("no window floor captured for column {column:?}"))
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
        // Roll back prepared transactions before dropping the databases; a
        // prepared xact prevents DROP DATABASE from completing.
        if let Some(cluster) = self.cluster {
            let altered_settings: Vec<_> = std::mem::take(&mut self.altered_system_settings)
                .into_iter()
                .collect();
            if !altered_settings.is_empty()
                && let Err(err) = reset_system_settings(cluster, &altered_settings).await
            {
                eprintln!("=== BDD cleanup: ALTER SYSTEM RESET {altered_settings:?}: {err:#} ===");
            }
            for prepared in std::mem::take(&mut self.pending_rollbacks) {
                if let Err(err) =
                    rollback_prepared(cluster, &prepared.database, &prepared.gid).await
                {
                    eprintln!(
                        "=== BDD cleanup: ROLLBACK PREPARED {:?} in database {:?}: {err:#} ===",
                        prepared.gid, prepared.database
                    );
                }
            }
            for dbname in std::mem::take(&mut self.extra_databases) {
                if let Err(err) = drop_database(cluster, &dbname).await {
                    eprintln!("=== BDD cleanup: drop extra database {dbname:?}: {err:#} ===");
                }
            }
        }
        if let Some(dbname) = self.database.take()
            && let Some(cluster) = self.cluster
            && let Err(err) = drop_database(cluster, &dbname).await
        {
            eprintln!("=== BDD cleanup: drop database {dbname:?}: {err:#} ===");
        }
        self.window_floors.clear();
        self.segment = None;
        self.collector_log = None;
        self.collector_output_dirs.clear();
        self.collector_env.clear();
        self.altered_system_settings.clear();
    }
}

/// Extract the GUC name from an `ALTER SYSTEM SET <name> ...` statement.
pub(crate) fn altered_system_setting(statement: &str) -> Option<String> {
    let rest = strip_prefix_ascii_case(statement.trim_start(), "ALTER")?.trim_start();
    let rest = strip_prefix_ascii_case(rest, "SYSTEM")?.trim_start();
    let rest = strip_prefix_ascii_case(rest, "SET")?.trim_start();
    if rest.is_empty() {
        return None;
    }
    if let Some(rest) = rest.strip_prefix('"') {
        let (name, _) = parse_quoted_identifier(rest)?;
        return Some(name);
    }
    let end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '=')
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest.get(..end).map(str::to_owned)
}

/// Wait until `ALTER SYSTEM SET` changes are visible after `pg_reload_conf()`.
pub(crate) async fn wait_for_altered_system_settings(
    client: &tokio_postgres::Client,
    settings: &[String],
) -> Result<()> {
    wait_for_settings_state(client, settings, SettingsState::Loaded)
        .await
        .context("wait for ALTER SYSTEM settings to load")
}

async fn reset_system_settings(cluster: &Cluster, settings: &[String]) -> Result<()> {
    let admin = cluster.connect().await?;
    for setting in settings {
        admin
            .client()
            .batch_execute(&format!(
                "ALTER SYSTEM RESET {}",
                quote_config_identifier(setting)
            ))
            .await
            .with_context(|| format!("ALTER SYSTEM RESET {setting:?}"))?;
    }
    admin
        .client()
        .batch_execute("SELECT pg_reload_conf()")
        .await
        .context("reload PostgreSQL configuration after ALTER SYSTEM RESET")?;
    wait_for_settings_state(admin.client(), settings, SettingsState::Reset)
        .await
        .context("wait for ALTER SYSTEM RESET to apply")
}

async fn wait_for_settings_state(
    client: &tokio_postgres::Client,
    settings: &[String],
    state: SettingsState,
) -> Result<()> {
    let poll = async {
        loop {
            let mut ready = true;
            for setting in settings {
                let reached = match state {
                    SettingsState::Loaded => settings_loaded(client, setting).await?,
                    SettingsState::Reset => settings_reset(client, setting).await?,
                };
                if !reached {
                    ready = false;
                    break;
                }
            }
            if ready {
                return Ok(());
            }
            sleep(SETTINGS_RELOAD_POLL_INTERVAL).await;
        }
    };
    match timeout(SETTINGS_RELOAD_TIMEOUT, poll).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "settings {settings:?} did not reach the expected state within {SETTINGS_RELOAD_TIMEOUT:?}"
        ),
    }
}

async fn settings_loaded(client: &tokio_postgres::Client, setting: &str) -> Result<bool> {
    // Postmaster-context GUCs show up in pg_file_settings with
    // "setting could not be applied" until restart; pg_settings.pending_restart
    // is the contract we wait for in that case.
    let row = client
        .query_one(
            "SELECT \
                EXISTS ( \
                    SELECT 1 FROM pg_file_settings \
                    WHERE name = $1 \
                      AND sourcefile = current_setting('data_directory') || '/postgresql.auto.conf' \
                ) AS file_seen, \
                EXISTS ( \
                    SELECT 1 FROM pg_settings \
                    WHERE name = $1 \
                      AND ( \
                          sourcefile = current_setting('data_directory') || '/postgresql.auto.conf' \
                          OR pending_restart \
                      ) \
                ) AS settings_visible",
            &[&setting],
        )
        .await
        .with_context(|| format!("poll loaded ALTER SYSTEM setting {setting:?}"))?;
    Ok(row.get::<_, bool>("file_seen") && row.get::<_, bool>("settings_visible"))
}

async fn settings_reset(client: &tokio_postgres::Client, setting: &str) -> Result<bool> {
    let row = client
        .query_one(
            "SELECT \
                EXISTS ( \
                    SELECT 1 FROM pg_file_settings \
                    WHERE name = $1 \
                      AND sourcefile = current_setting('data_directory') || '/postgresql.auto.conf' \
                ) AS in_auto_conf, \
                COALESCE(( \
                    SELECT pending_restart FROM pg_settings WHERE name = $1 \
                ), false) AS pending_restart",
            &[&setting],
        )
        .await
        .with_context(|| format!("poll reset ALTER SYSTEM setting {setting:?}"))?;
    Ok(!row.get::<_, bool>("in_auto_conf") && !row.get::<_, bool>("pending_restart"))
}

fn quote_config_identifier(name: &str) -> String {
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('"');
    for ch in name.chars() {
        if ch == '"' {
            quoted.push('"');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn parse_quoted_identifier(rest: &str) -> Option<(String, &str)> {
    let mut name = String::new();
    let mut chars = rest.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch == '"' {
            if chars.next_if(|(_, next)| *next == '"').is_some() {
                name.push('"');
                continue;
            }
            let tail = rest.get(idx + ch.len_utf8()..)?;
            return Some((name, tail));
        }
        name.push(ch);
    }
    None
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() {
        return None;
    }
    let head = value.get(..prefix.len())?;
    let tail = value.get(prefix.len()..)?;
    head.eq_ignore_ascii_case(prefix).then_some(tail)
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
async fn rollback_prepared(cluster: &Cluster, dbname: &str, gid: &str) -> Result<()> {
    ensure_safe_ident(dbname)?;
    anyhow::ensure!(
        !gid.is_empty() && gid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "prepared transaction gid {gid:?} contains unsafe characters"
    );
    let dsn = cluster.conn_string_db(dbname);
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .with_context(|| format!("connect to database {dbname} for ROLLBACK PREPARED"))?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let result = client
        .batch_execute(&format!("ROLLBACK PREPARED '{gid}'"))
        .await
        .with_context(|| format!("ROLLBACK PREPARED {gid:?}"));
    driver.abort();
    result
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
    use super::{
        HarnessState, altered_system_setting, ensure_safe_ident, quote_config_identifier,
        unique_database_name,
    };

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

    #[test]
    fn extracts_altered_system_setting_names() {
        assert_eq!(
            altered_system_setting("ALTER SYSTEM SET work_mem = '7539kB'").as_deref(),
            Some("work_mem")
        );
        assert_eq!(
            altered_system_setting("alter system set shared_buffers TO '190MB'").as_deref(),
            Some("shared_buffers")
        );
        assert_eq!(
            altered_system_setting("ALTER SYSTEM SET \"custom.guc\" = 'on'").as_deref(),
            Some("custom.guc")
        );
        assert_eq!(altered_system_setting("SELECT pg_reload_conf()"), None);
    }

    #[test]
    fn quotes_config_identifiers_for_alter_system_reset() {
        assert_eq!(quote_config_identifier("work_mem"), "\"work_mem\"");
        assert_eq!(quote_config_identifier("custom\"guc"), "\"custom\"\"guc\"");
    }

    #[test]
    fn extra_database_name_returns_error_when_empty() {
        let state = HarnessState::default();
        assert!(
            state.extra_database_name(0).is_err(),
            "no extra databases returns an error"
        );
    }

    #[test]
    fn window_floor_roundtrips_and_errors_when_missing() {
        let mut state = HarnessState::default();
        assert!(
            state.window_floor("current_wal_lsn").is_err(),
            "a floor that was never captured is an error"
        );
        state.set_window_floor("current_wal_lsn", 42);
        assert_eq!(state.window_floor("current_wal_lsn").unwrap(), 42);
    }
}
