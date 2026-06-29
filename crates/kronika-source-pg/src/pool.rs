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

#[cfg(test)]
mod tests {
    use super::replace_dbname;

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
}
