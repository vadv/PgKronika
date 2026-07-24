//! Pure configuration and readiness helpers for the web server.
//!
//! All functions here are side-effect-free: no I/O, no environment reads.
//! `WebConfig::from_env` is the only entry point that touches `std::env`.

use std::path::PathBuf;
use std::time::Duration;

use kronika_reader::{FallbackConfig, FallbackConfigError};

use crate::OverviewConfig;

const OVERVIEW_CACHE_DIR_ENV: &str = "KRONIKA_WEB_OVERVIEW_CACHE_DIR";
const OVERVIEW_NAMESPACE_ENV: &str = "KRONIKA_WEB_OVERVIEW_NAMESPACE";
const FALLBACK_SEGMENT_HOURS_ENV: &str = "KRONIKA_WEB_OVERVIEW_FALLBACK_SEGMENT_HOURS";
const FALLBACK_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_FALLBACK_BYTES";
const RESPONSE_CACHE_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_BYTES";
const RESPONSE_CACHE_ENTRIES_ENV: &str = "KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_ENTRIES";
const CURSOR_MAX_VIEWS_ENV: &str = "KRONIKA_WEB_OVERVIEW_CURSOR_MAX_VIEWS";
const CURSOR_MAX_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_CURSOR_MAX_BYTES";
const CURSOR_TTL_ENV: &str = "KRONIKA_WEB_OVERVIEW_CURSOR_TTL_S";

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
        Some("/v1/timeline/overview") => "/v1/timeline/overview",
        Some("/v1/timeline/events") => "/v1/timeline/events",
        Some("/v1/timeline/health") => "/v1/timeline/health",
        Some("/v1/anomalies") => "/v1/anomalies",
        Some("/v1/incidents") => "/v1/incidents",
        Some("/v1/sources") => "/v1/sources",
        Some("/v1/sections") => "/v1/sections",
        Some("/v1/segments") => "/v1/segments",
        Some("/v1/section/{name}") => "/v1/section/{name}",
        Some("/v1/section/{name}/diff") => "/v1/section/{name}/diff",
        Some("/v1/sections/batch") => "/v1/sections/batch",
        Some("/v1/sections/batch/diff") => "/v1/sections/batch/diff",
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
        .ok_or_else(|| "KRONIKA_WEB_BASIC_AUTH must contain ':'".to_owned())?;
    if user.is_empty() {
        return Err("KRONIKA_WEB_BASIC_AUTH user must not be empty".to_owned());
    }
    Ok((user.to_owned(), pass.to_owned()))
}

#[derive(Clone, Copy, Default)]
struct OverviewConfigRaw<'a> {
    cache_dir: Option<&'a str>,
    namespace: Option<&'a str>,
    fallback_segment_hours: Option<&'a str>,
    fallback_bytes: Option<&'a str>,
    response_cache_bytes: Option<&'a str>,
    response_cache_entries: Option<&'a str>,
    cursor_max_views: Option<&'a str>,
    cursor_max_bytes: Option<&'a str>,
    cursor_ttl_secs: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedOverviewConfig {
    cache_dir: PathBuf,
    namespace: Option<Vec<u8>>,
    fallback: FallbackConfig,
    response_cache_bytes: usize,
    response_cache_entries: usize,
    cursor_max_views: usize,
    cursor_max_bytes: usize,
    cursor_ttl: Duration,
}

fn parse_overview_config(
    raw: OverviewConfigRaw<'_>,
    default_cache_dir: PathBuf,
) -> Result<ParsedOverviewConfig, String> {
    let defaults = OverviewConfig::new(default_cache_dir, b"default".to_vec());
    let cache_dir = match raw.cache_dir {
        Some("") => return Err(format!("{OVERVIEW_CACHE_DIR_ENV} must not be empty")),
        Some(path) => PathBuf::from(path),
        None => defaults.cache_root,
    };
    let namespace = match raw.namespace {
        Some("") => return Err(format!("{OVERVIEW_NAMESPACE_ENV} must not be empty")),
        Some(namespace) => Some(namespace.as_bytes().to_vec()),
        None => None,
    };
    let fallback_segment_hours = parse_nonzero_u64(
        raw.fallback_segment_hours,
        FALLBACK_SEGMENT_HOURS_ENV,
        defaults.fallback.segment_hours(),
    )?;
    let fallback_bytes = parse_nonzero_u64(
        raw.fallback_bytes,
        FALLBACK_BYTES_ENV,
        defaults.fallback.bytes(),
    )?;
    let fallback =
        FallbackConfig::new(fallback_segment_hours, fallback_bytes).map_err(|error| {
            let name = match error {
                FallbackConfigError::ZeroSegmentHours
                | FallbackConfigError::SegmentHoursAboveMaximum => FALLBACK_SEGMENT_HOURS_ENV,
                FallbackConfigError::ZeroBytes | FallbackConfigError::BytesAboveMaximum => {
                    FALLBACK_BYTES_ENV
                }
            };
            format!("{name}: {error}")
        })?;
    let response_cache_bytes = parse_nonzero_usize(
        raw.response_cache_bytes,
        RESPONSE_CACHE_BYTES_ENV,
        defaults.response_cache_bytes,
    )?;
    let response_cache_entries = parse_nonzero_usize(
        raw.response_cache_entries,
        RESPONSE_CACHE_ENTRIES_ENV,
        defaults.response_cache_entries,
    )?;
    let cursor_max_views = parse_nonzero_usize(
        raw.cursor_max_views,
        CURSOR_MAX_VIEWS_ENV,
        defaults.cursor_max_views,
    )?;
    let cursor_max_bytes = parse_nonzero_usize(
        raw.cursor_max_bytes,
        CURSOR_MAX_BYTES_ENV,
        defaults.cursor_max_bytes,
    )?;
    let cursor_ttl_secs = parse_nonzero_u64(
        raw.cursor_ttl_secs,
        CURSOR_TTL_ENV,
        defaults.cursor_ttl.as_secs(),
    )?;
    Ok(ParsedOverviewConfig {
        cache_dir,
        namespace,
        fallback,
        response_cache_bytes,
        response_cache_entries,
        cursor_max_views,
        cursor_max_bytes,
        cursor_ttl: Duration::from_secs(cursor_ttl_secs),
    })
}

fn parse_nonzero_u64(raw: Option<&str>, name: &str, default: u64) -> Result<u64, String> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    let value = raw
        .parse::<u64>()
        .map_err(|error| format!("{name} must be a u64: {error}"))?;
    if value == 0 {
        return Err(format!("{name} must be non-zero"));
    }
    Ok(value)
}

fn parse_nonzero_usize(raw: Option<&str>, name: &str, default: usize) -> Result<usize, String> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    let value = raw
        .parse::<u128>()
        .map_err(|error| format!("{name} must be an unsigned integer: {error}"))?;
    if value == 0 {
        return Err(format!("{name} must be non-zero"));
    }
    usize::try_from(value).map_err(|_error| format!("{name} does not fit usize"))
}

/// Validated server configuration parsed from env-var strings.
pub struct WebConfig {
    /// Store directory to serve.
    pub dir: PathBuf,
    /// Listen address (`host:port`).
    pub addr: String,
    /// Basic Auth credential; `None` leaves the API open.
    pub basic_auth: Option<(String, String)>,
    /// Age after which `/readyz` reports the store stale.
    pub stale_after: Duration,
    /// Log filter directive (e.g. `info`).
    pub log: String,
    /// Durable overview fact-cache directory.
    pub overview_cache_dir: PathBuf,
    /// Explicit stable overview namespace; `None` derives it from the canonical
    /// store path at startup.
    pub overview_namespace: Option<Vec<u8>>,
    /// Bounded fallback used only after recoverable durable-publication failure.
    pub overview_fallback: FallbackConfig,
    /// Serialized overview/health response-cache byte ceiling.
    pub overview_response_cache_bytes: usize,
    /// Serialized overview/health response-cache entry ceiling.
    pub overview_response_cache_entries: usize,
    /// Maximum simultaneously pinned event views.
    pub overview_cursor_max_views: usize,
    /// Logical-byte ceiling for cursor-pinned event views.
    pub overview_cursor_max_bytes: usize,
    /// Lifetime of an event cursor and its pinned view.
    pub overview_cursor_ttl: Duration,
}

impl std::fmt::Debug for WebConfig {
    /// Redacts the Basic Auth credential so it never reaches a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("dir", &self.dir)
            .field("addr", &self.addr)
            .field(
                "basic_auth",
                &self.basic_auth.as_ref().map(|_| "<redacted>"),
            )
            .field("stale_after", &self.stale_after)
            .field("log", &self.log)
            .field("overview_cache_dir", &self.overview_cache_dir)
            .field(
                "overview_namespace",
                &self
                    .overview_namespace
                    .as_ref()
                    .map(|namespace| format!("<{} bytes>", namespace.len())),
            )
            .field("overview_fallback", &self.overview_fallback)
            .field(
                "overview_response_cache_bytes",
                &self.overview_response_cache_bytes,
            )
            .field(
                "overview_response_cache_entries",
                &self.overview_response_cache_entries,
            )
            .field("overview_cursor_max_views", &self.overview_cursor_max_views)
            .field("overview_cursor_max_bytes", &self.overview_cursor_max_bytes)
            .field("overview_cursor_ttl", &self.overview_cursor_ttl)
            .finish()
    }
}

impl WebConfig {
    /// Parse and validate configuration from raw string arguments.
    ///
    /// `basic_auth_raw`: `Some("user:pass")` or `None` (auth disabled).
    /// `stale_raw`: seconds as a decimal string; `None` defaults to 10 s.
    /// Returns `Err` with a human-readable message on any validation failure.
    #[cfg(test)]
    pub(crate) fn parse(
        dir: &str,
        addr: &str,
        basic_auth_raw: Option<&str>,
        stale_raw: Option<&str>,
        log: Option<&str>,
    ) -> Result<Self, String> {
        Self::parse_with_overview(
            dir,
            addr,
            basic_auth_raw,
            stale_raw,
            log,
            OverviewConfigRaw::default(),
        )
    }

    fn parse_with_overview(
        dir: &str,
        addr: &str,
        basic_auth_raw: Option<&str>,
        stale_raw: Option<&str>,
        log: Option<&str>,
        overview_raw: OverviewConfigRaw<'_>,
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

        let dir = PathBuf::from(dir);
        let overview = parse_overview_config(overview_raw, dir.join(".pgkronika-overview-cache"))?;
        Ok(Self {
            overview_cache_dir: overview.cache_dir,
            dir,
            addr: addr.to_owned(),
            basic_auth,
            stale_after,
            log: log.unwrap_or("info").to_owned(),
            overview_namespace: overview.namespace,
            overview_fallback: overview.fallback,
            overview_response_cache_bytes: overview.response_cache_bytes,
            overview_response_cache_entries: overview.response_cache_entries,
            overview_cursor_max_views: overview.cursor_max_views,
            overview_cursor_max_bytes: overview.cursor_max_bytes,
            overview_cursor_ttl: overview.cursor_ttl,
        })
    }

    /// Build `WebConfig` from environment variables.
    ///
    /// Required: `KRONIKA_WEB_DIR`, `KRONIKA_WEB_ADDR`.
    /// Optional: `KRONIKA_WEB_BASIC_AUTH`, `KRONIKA_WEB_STALE_AFTER_S`,
    /// `KRONIKA_WEB_LOG`, and the `KRONIKA_WEB_OVERVIEW_*` policy variables.
    ///
    /// # Errors
    /// Returns a message when a required variable is unset or a value is invalid.
    pub fn from_env() -> Result<Self, String> {
        let dir = std::env::var("KRONIKA_WEB_DIR")
            .map_err(|_e| "KRONIKA_WEB_DIR is not set".to_owned())?;
        let addr = std::env::var("KRONIKA_WEB_ADDR")
            .map_err(|_e| "KRONIKA_WEB_ADDR is not set".to_owned())?;
        let basic_auth_raw = std::env::var("KRONIKA_WEB_BASIC_AUTH").ok();
        let stale_raw = std::env::var("KRONIKA_WEB_STALE_AFTER_S").ok();
        let log = std::env::var("KRONIKA_WEB_LOG").ok();
        let overview_cache_dir = std::env::var(OVERVIEW_CACHE_DIR_ENV).ok();
        let overview_namespace = std::env::var(OVERVIEW_NAMESPACE_ENV).ok();
        let fallback_segment_hours = std::env::var(FALLBACK_SEGMENT_HOURS_ENV).ok();
        let fallback_bytes = std::env::var(FALLBACK_BYTES_ENV).ok();
        let response_cache_bytes = std::env::var(RESPONSE_CACHE_BYTES_ENV).ok();
        let response_cache_entries = std::env::var(RESPONSE_CACHE_ENTRIES_ENV).ok();
        let cursor_max_views = std::env::var(CURSOR_MAX_VIEWS_ENV).ok();
        let cursor_max_bytes = std::env::var(CURSOR_MAX_BYTES_ENV).ok();
        let cursor_ttl_secs = std::env::var(CURSOR_TTL_ENV).ok();

        Self::parse_with_overview(
            &dir,
            &addr,
            basic_auth_raw.as_deref(),
            stale_raw.as_deref(),
            log.as_deref(),
            OverviewConfigRaw {
                cache_dir: overview_cache_dir.as_deref(),
                namespace: overview_namespace.as_deref(),
                fallback_segment_hours: fallback_segment_hours.as_deref(),
                fallback_bytes: fallback_bytes.as_deref(),
                response_cache_bytes: response_cache_bytes.as_deref(),
                response_cache_entries: response_cache_entries.as_deref(),
                cursor_max_views: cursor_max_views.as_deref(),
                cursor_max_bytes: cursor_max_bytes.as_deref(),
                cursor_ttl_secs: cursor_ttl_secs.as_deref(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn analytic_metric_paths_use_fixed_labels() {
        for path in [
            "/v1/anomalies",
            "/v1/incidents",
            "/v1/timeline/overview",
            "/v1/timeline/events",
            "/v1/timeline/health",
        ] {
            assert_eq!(metric_labels("GET", Some(path)).1, path);
        }
    }

    #[test]
    fn metric_labels_none_path_becomes_other() {
        let (method, path) = metric_labels("GET", None);
        assert_eq!(method, "GET", "method is forwarded unchanged");
        assert_eq!(path, "other", "unmatched path becomes 'other'");
    }

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
        let secret = "secret-without-delimiter";
        let err = parse_basic_auth(secret).expect_err("input without ':' must fail");
        assert!(!err.contains(secret));
    }

    #[test]
    fn parse_basic_auth_empty_user_is_error() {
        let secret = ":secret-password";
        let err = parse_basic_auth(secret).expect_err("empty user must fail");
        assert!(!err.contains(secret));
    }

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
        assert_eq!(
            cfg.overview_cache_dir,
            PathBuf::from("/data/.pgkronika-overview-cache")
        );
        assert!(cfg.overview_namespace.is_none());
    }

    #[test]
    fn overview_raw_defaults_match_runtime_overview_defaults() {
        let cache_dir = PathBuf::from("/data/cache");
        let parsed = parse_overview_config(OverviewConfigRaw::default(), cache_dir.clone())
            .expect("default overview policy is valid");
        let defaults = OverviewConfig::new(cache_dir.clone(), b"default".to_vec());
        let expected = ParsedOverviewConfig {
            cache_dir,
            namespace: None,
            fallback: defaults.fallback,
            response_cache_bytes: defaults.response_cache_bytes,
            response_cache_entries: defaults.response_cache_entries,
            cursor_max_views: defaults.cursor_max_views,
            cursor_max_bytes: defaults.cursor_max_bytes,
            cursor_ttl: defaults.cursor_ttl,
        };
        assert_eq!(parsed, expected);
    }

    #[test]
    fn overview_raw_custom_values_reach_web_config() {
        let cfg = WebConfig::parse_with_overview(
            "/data",
            "127.0.0.1:9000",
            None,
            None,
            None,
            OverviewConfigRaw {
                cache_dir: Some("/cache"),
                namespace: Some("deployment-a"),
                fallback_segment_hours: Some("48"),
                fallback_bytes: Some("1048576"),
                response_cache_bytes: Some("2097152"),
                response_cache_entries: Some("128"),
                cursor_max_views: Some("16"),
                cursor_max_bytes: Some("4194304"),
                cursor_ttl_secs: Some("60"),
            },
        )
        .expect("custom overview policy is valid");
        assert_eq!(cfg.overview_cache_dir, PathBuf::from("/cache"));
        assert_eq!(cfg.overview_namespace, Some(b"deployment-a".to_vec()));
        assert_eq!(
            cfg.overview_fallback,
            FallbackConfig::new(48, 1_048_576).expect("fixture fallback is valid")
        );
        assert_eq!(cfg.overview_response_cache_bytes, 2_097_152);
        assert_eq!(cfg.overview_response_cache_entries, 128);
        assert_eq!(cfg.overview_cursor_max_views, 16);
        assert_eq!(cfg.overview_cursor_max_bytes, 4_194_304);
        assert_eq!(cfg.overview_cursor_ttl, Duration::from_mins(1));
    }

    #[test]
    fn web_config_debug_redacts_credentials_and_namespace() {
        let cfg = WebConfig::parse_with_overview(
            "/data",
            "127.0.0.1:9000",
            Some("alice:secret-password"),
            None,
            None,
            OverviewConfigRaw {
                namespace: Some("secret-deployment"),
                ..OverviewConfigRaw::default()
            },
        )
        .expect("config with secrets is valid");
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("secret-password"), "{debug}");
        assert!(!debug.contains("secret-deployment"), "{debug}");
        assert!(debug.contains("overview_cursor_max_bytes"), "{debug}");
    }

    #[test]
    fn overview_raw_rejects_zero_budgets() {
        let cases = [
            (
                FALLBACK_SEGMENT_HOURS_ENV,
                OverviewConfigRaw {
                    fallback_segment_hours: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                FALLBACK_BYTES_ENV,
                OverviewConfigRaw {
                    fallback_bytes: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                RESPONSE_CACHE_BYTES_ENV,
                OverviewConfigRaw {
                    response_cache_bytes: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                RESPONSE_CACHE_ENTRIES_ENV,
                OverviewConfigRaw {
                    response_cache_entries: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                CURSOR_MAX_VIEWS_ENV,
                OverviewConfigRaw {
                    cursor_max_views: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                CURSOR_MAX_BYTES_ENV,
                OverviewConfigRaw {
                    cursor_max_bytes: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                CURSOR_TTL_ENV,
                OverviewConfigRaw {
                    cursor_ttl_secs: Some("0"),
                    ..OverviewConfigRaw::default()
                },
            ),
        ];
        for (name, raw) in cases {
            let error = parse_overview_config(raw, PathBuf::from("/cache"))
                .expect_err("zero budget must fail");
            assert!(error.contains(name), "wrong error for {name}: {error}");
        }
    }

    #[test]
    fn overview_raw_rejects_fallback_hours_above_hard_maximum() {
        let value = (kronika_reader::MAX_FALLBACK_SEGMENT_HOURS + 1).to_string();
        let error = parse_overview_config(
            OverviewConfigRaw {
                fallback_segment_hours: Some(&value),
                ..OverviewConfigRaw::default()
            },
            PathBuf::from("/cache"),
        )
        .expect_err("fallback hours above the hard maximum must fail");
        assert!(error.contains(FALLBACK_SEGMENT_HOURS_ENV), "{error}");
        assert!(error.contains("hard ceiling"), "{error}");
    }

    #[test]
    fn overview_raw_rejects_fallback_bytes_above_hard_maximum() {
        let value = (kronika_reader::MAX_FALLBACK_BYTES + 1).to_string();
        let error = parse_overview_config(
            OverviewConfigRaw {
                fallback_bytes: Some(&value),
                ..OverviewConfigRaw::default()
            },
            PathBuf::from("/cache"),
        )
        .expect_err("fallback bytes above the hard maximum must fail");
        assert!(error.contains(FALLBACK_BYTES_ENV), "{error}");
        assert!(error.contains("hard ceiling"), "{error}");
    }

    #[test]
    fn overview_usize_budget_rejects_platform_overflow() {
        let value = u128::MAX.to_string();
        let error = parse_nonzero_usize(Some(&value), RESPONSE_CACHE_BYTES_ENV, 1)
            .expect_err("a value wider than usize must fail");
        assert!(error.contains("does not fit usize"), "{error}");
    }

    #[test]
    fn overview_raw_rejects_empty_path_and_namespace() {
        for (name, raw) in [
            (
                OVERVIEW_CACHE_DIR_ENV,
                OverviewConfigRaw {
                    cache_dir: Some(""),
                    ..OverviewConfigRaw::default()
                },
            ),
            (
                OVERVIEW_NAMESPACE_ENV,
                OverviewConfigRaw {
                    namespace: Some(""),
                    ..OverviewConfigRaw::default()
                },
            ),
        ] {
            let error = parse_overview_config(raw, PathBuf::from("/cache"))
                .expect_err("empty overview identity input must fail");
            assert!(error.contains(name), "wrong error for {name}: {error}");
        }
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
