//! Pure configuration and readiness helpers for the web server.
//!
//! All functions here are side-effect-free: no I/O, no environment reads.
//! `WebConfig::from_env` is the only entry point that touches `std::env`.
// Items in this module are consumed by later tasks (T5, T7); allow
// dead_code so the gate stays green before those tasks are implemented.
#![allow(dead_code, reason = "consumed by T5 auth and T7 main")]

use std::path::PathBuf;
use std::time::Duration;

/// Normalises a request's method and matched path into metric label values.
///
/// `matched_path` must come from axum's `MatchedPath` extension, not
/// `uri().path()`, to avoid high-cardinality labels.
/// When no route matched, path is reported as `"other"`.
pub(crate) fn metric_labels(method: &str, matched_path: Option<&str>) -> (String, &'static str) {
    let path: &'static str = match matched_path {
        Some("/healthz") => "/healthz",
        Some("/readyz") => "/readyz",
        Some("/metrics") => "/metrics",
        Some("/v1/version") => "/v1/version",
        Some("/v1/sources") => "/v1/sources",
        Some("/v1/sections") => "/v1/sections",
        Some("/v1/segments") => "/v1/segments",
        Some("/v1/section/{name}") => "/v1/section/{name}",
        Some("/v1/sections/batch") => "/v1/sections/batch",
        _ => "other",
    };
    (method.to_owned(), path)
}

/// Returns `true` if the store data is stale.
///
/// Stale means `now_secs - last_refresh_secs > stale_after`. Saturating
/// subtraction: when `last_refresh_secs > now_secs` (clock skew), returns
/// `false` (treat as fresh rather than infinitely stale).
pub(crate) const fn staleness(
    now_secs: u64,
    last_refresh_secs: u64,
    stale_after: Duration,
) -> bool {
    let age = now_secs.saturating_sub(last_refresh_secs);
    age > stale_after.as_secs()
}

/// Splits `"user:password"` on the first colon.
///
/// Returns `Err` when there is no colon or the user part is empty.
pub(crate) fn parse_basic_auth(raw: &str) -> Result<(String, String), String> {
    let (user, pass) = raw
        .split_once(':')
        .ok_or_else(|| format!("basic auth value has no ':': {raw:?}"))?;
    if user.is_empty() {
        return Err(format!("basic auth user is empty in {raw:?}"));
    }
    Ok((user.to_owned(), pass.to_owned()))
}

/// Validated server configuration parsed from env-var strings.
pub(crate) struct WebConfig {
    pub dir: PathBuf,
    pub addr: String,
    pub basic_auth: Option<(String, String)>,
    pub stale_after: Duration,
    pub log: String,
}

impl WebConfig {
    /// Parse and validate configuration from raw string arguments.
    ///
    /// `basic_auth_raw`: `Some("user:pass")` or `None` (auth disabled).
    /// `stale_raw`: seconds as a decimal string; `None` defaults to 10 s.
    /// Returns `Err` with a human-readable message on any validation failure.
    pub(crate) fn parse(
        dir: &str,
        addr: &str,
        basic_auth_raw: Option<&str>,
        stale_raw: Option<&str>,
        log: Option<&str>,
    ) -> Result<Self, String> {
        let basic_auth = basic_auth_raw.map(parse_basic_auth).transpose()?;

        let stale_after = match stale_raw {
            None => Duration::from_secs(10),
            Some(s) => {
                let secs = s
                    .parse::<u64>()
                    .map_err(|e| format!("KRONIKA_WEB_STALE_AFTER_S must be a u64: {e}"))?;
                Duration::from_secs(secs)
            }
        };

        Ok(Self {
            dir: PathBuf::from(dir),
            addr: addr.to_owned(),
            basic_auth,
            stale_after,
            log: log.unwrap_or("info").to_owned(),
        })
    }

    /// Build `WebConfig` from environment variables.
    ///
    /// Required: `KRONIKA_WEB_DIR`, `KRONIKA_WEB_ADDR`.
    /// Optional: `KRONIKA_WEB_BASIC_AUTH`, `KRONIKA_WEB_STALE_AFTER_S`,
    /// `KRONIKA_WEB_LOG`.
    pub(crate) fn from_env() -> Result<Self, String> {
        let dir = std::env::var("KRONIKA_WEB_DIR")
            .map_err(|_e| "KRONIKA_WEB_DIR is not set".to_owned())?;
        let addr = std::env::var("KRONIKA_WEB_ADDR")
            .map_err(|_e| "KRONIKA_WEB_ADDR is not set".to_owned())?;
        let basic_auth_raw = std::env::var("KRONIKA_WEB_BASIC_AUTH").ok();
        let stale_raw = std::env::var("KRONIKA_WEB_STALE_AFTER_S").ok();
        let log = std::env::var("KRONIKA_WEB_LOG").ok();

        Self::parse(
            &dir,
            &addr,
            basic_auth_raw.as_deref(),
            stale_raw.as_deref(),
            log.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- metric_labels ---

    #[test]
    fn metric_labels_known_path_is_preserved() {
        let (method, path) = metric_labels("GET", Some("/v1/section/{name}"));
        assert_eq!(method, "GET", "method is forwarded unchanged");
        assert_eq!(
            path, "/v1/section/{name}",
            "known matched path is preserved"
        );
    }

    #[test]
    fn metric_labels_none_path_becomes_other() {
        let (method, path) = metric_labels("GET", None);
        assert_eq!(method, "GET", "method is forwarded unchanged");
        assert_eq!(path, "other", "unmatched path becomes 'other'");
    }

    // --- staleness ---

    #[test]
    fn staleness_fresh_data_within_threshold() {
        assert!(
            !staleness(100, 99, Duration::from_secs(10)),
            "age=1s is within the 10s threshold"
        );
    }

    #[test]
    fn staleness_stale_data_exceeds_threshold() {
        assert!(
            staleness(100, 80, Duration::from_secs(10)),
            "age=20s exceeds the 10s threshold"
        );
    }

    #[test]
    fn staleness_exactly_at_threshold_is_not_stale() {
        // Contract: stale means STRICTLY greater than `stale_after`.
        assert!(
            !staleness(110, 100, Duration::from_secs(10)),
            "age == stale_after is not stale (strict greater-than)"
        );
    }

    #[test]
    fn staleness_last_greater_than_now_is_not_stale() {
        assert!(
            !staleness(100, 200, Duration::from_secs(10)),
            "clock skew (last>now) must not be treated as stale"
        );
    }

    // --- parse_basic_auth ---

    #[test]
    fn parse_basic_auth_simple_user_password() {
        assert_eq!(
            parse_basic_auth("u:p"),
            Ok(("u".to_owned(), "p".to_owned())),
            "simple user:pass splits on the colon"
        );
    }

    #[test]
    fn parse_basic_auth_password_contains_colon() {
        assert_eq!(
            parse_basic_auth("u:p:x"),
            Ok(("u".to_owned(), "p:x".to_owned())),
            "only the first colon is the delimiter; password may contain colons"
        );
    }

    #[test]
    fn parse_basic_auth_no_colon_is_error() {
        assert!(
            parse_basic_auth("nocodon").is_err(),
            "input without ':' must return Err"
        );
    }

    #[test]
    fn parse_basic_auth_empty_user_is_error() {
        assert!(
            parse_basic_auth(":pass").is_err(),
            "empty user before ':' must return Err"
        );
    }

    // --- WebConfig::parse ---

    #[test]
    fn web_config_parse_minimal_valid() {
        let cfg = WebConfig::parse("/data", "0.0.0.0:8080", None, None, None)
            .expect("minimal config is valid");
        assert_eq!(cfg.dir, PathBuf::from("/data"));
        assert_eq!(cfg.addr, "0.0.0.0:8080");
        assert!(cfg.basic_auth.is_none(), "no auth when not provided");
        assert_eq!(
            cfg.stale_after,
            Duration::from_secs(10),
            "default stale_after is 10s"
        );
        assert_eq!(cfg.log, "info", "default log level is info");
    }

    #[test]
    fn web_config_parse_with_basic_auth() {
        let cfg = WebConfig::parse("/data", "127.0.0.1:9000", Some("alice:secret"), None, None)
            .expect("config with basic auth is valid");
        assert_eq!(
            cfg.basic_auth,
            Some(("alice".to_owned(), "secret".to_owned())),
            "basic auth is parsed correctly"
        );
    }

    #[test]
    fn web_config_parse_broken_basic_auth_no_colon_is_error() {
        let err = WebConfig::parse("/data", "127.0.0.1:9000", Some("nocolon"), None, None);
        assert!(err.is_err(), "basic auth without ':' must be rejected");
    }

    #[test]
    fn web_config_parse_broken_basic_auth_empty_user_is_error() {
        let err = WebConfig::parse("/data", "127.0.0.1:9000", Some(":pass"), None, None);
        assert!(err.is_err(), "basic auth with empty user must be rejected");
    }

    #[test]
    fn web_config_parse_custom_stale_after() {
        let cfg = WebConfig::parse("/data", "127.0.0.1:9000", None, Some("30"), None)
            .expect("custom stale_after is valid");
        assert_eq!(
            cfg.stale_after,
            Duration::from_secs(30),
            "stale_after is parsed from the raw string"
        );
    }

    #[test]
    fn web_config_parse_invalid_stale_after_is_error() {
        let err = WebConfig::parse("/data", "127.0.0.1:9000", None, Some("notanumber"), None);
        assert!(err.is_err(), "non-numeric stale_after must be rejected");
    }

    #[test]
    fn web_config_parse_custom_log_level() {
        let cfg = WebConfig::parse("/data", "127.0.0.1:9000", None, None, Some("debug"))
            .expect("custom log level is valid");
        assert_eq!(cfg.log, "debug", "log level is forwarded from the argument");
    }
}
