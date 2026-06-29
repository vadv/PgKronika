//! Multi-database connection pool: one main connection for instance-wide
//! metrics (reopened on failover), one per database for database-local
//! metrics.
//!
//! Pool setup returns `anyhow::Result`; per-query errors stay
//! `tokio_postgres::Error` via the handed-out `Client`, so callers can match
//! SQLSTATE 57014/55P03.

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
#[allow(clippy::struct_field_names, reason = "field names follow PostgreSQL GUC naming convention")]
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
}
