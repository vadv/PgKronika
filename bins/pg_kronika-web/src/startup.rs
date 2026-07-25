//! Pure configuration and readiness helpers for the web server.
//!
//! All functions here are side-effect-free: no I/O, no environment reads.
//! `WebConfig::from_env` is the only entry point that touches `std::env`.

use std::path::PathBuf;
use std::time::Duration;

use kronika_reader::{FallbackConfig, FallbackConfigError, GcConfig, GcConfigError};

use crate::overview::selection::ABSOLUTE_MAX_SELECTED_SEGMENTS;
use crate::{OverviewColdConfig, OverviewConfig};

const FALLBACK_SEGMENT_HOURS_ENV: &str = "KRONIKA_WEB_OVERVIEW_FALLBACK_SEGMENT_HOURS";
const FALLBACK_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_FALLBACK_BYTES";
const GC_MAX_ENTRIES_ENV: &str = "KRONIKA_WEB_OVERVIEW_GC_MAX_ENTRIES";
const GC_GRACE_GENERATIONS_ENV: &str = "KRONIKA_WEB_OVERVIEW_GC_GRACE_GENERATIONS";
const GC_WALL_GRACE_ENV: &str = "KRONIKA_WEB_OVERVIEW_GC_WALL_GRACE_S";
const GC_ARTIFACT_GRACE_ENV: &str = "KRONIKA_WEB_OVERVIEW_GC_ARTIFACT_GRACE_S";
const CACHE_MAX_LOGICAL_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_CACHE_MAX_LOGICAL_BYTES";
const CACHE_MAX_FILES_ENV: &str = "KRONIKA_WEB_OVERVIEW_CACHE_MAX_FILES";
const RESPONSE_CACHE_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_BYTES";
const RESPONSE_CACHE_ENTRIES_ENV: &str = "KRONIKA_WEB_OVERVIEW_RESPONSE_CACHE_ENTRIES";
const DECODED_CACHE_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_DECODED_CACHE_BYTES";
const DECODED_CACHE_ENTRIES_ENV: &str = "KRONIKA_WEB_OVERVIEW_DECODED_CACHE_ENTRIES";
const SOURCE_SCRUB_INTERVAL_ENV: &str = "KRONIKA_WEB_OVERVIEW_SOURCE_SCRUB_INTERVAL_S";
const CURSOR_MAX_VIEWS_ENV: &str = "KRONIKA_WEB_OVERVIEW_CURSOR_MAX_VIEWS";
const CURSOR_MAX_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_CURSOR_MAX_BYTES";
const CURSOR_TTL_ENV: &str = "KRONIKA_WEB_OVERVIEW_CURSOR_TTL_S";
const MAX_SELECTED_SEGMENTS_ENV: &str = "KRONIKA_WEB_OVERVIEW_MAX_SELECTED_SEGMENTS";
const COLD_MAX_WORKERS_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_MAX_WORKERS";
const COLD_MAX_QUEUE_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_MAX_QUEUE";
const COLD_PER_REQUEST_PARALLELISM_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_PER_REQUEST_PARALLELISM";
const COLD_WAIT_TIMEOUT_MS_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_WAIT_TIMEOUT_MS";
const COLD_RETRY_AFTER_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_RETRY_AFTER_S";
const COLD_PGM_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_PGM_BYTES";
const COLD_DECODED_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_DECODED_BYTES";
const COLD_CPU_ROWS_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_CPU_ROWS";
const COLD_FILE_DESCRIPTORS_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_FILE_DESCRIPTORS";
const COLD_READ_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_READ_BYTES";
const COLD_WRITE_BYTES_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_WRITE_BYTES";
const COLD_PUBLICATIONS_ENV: &str = "KRONIKA_WEB_OVERVIEW_COLD_PUBLICATIONS";

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
    fallback_segment_hours: Option<&'a str>,
    fallback_bytes: Option<&'a str>,
    gc_max_entries: Option<&'a str>,
    gc_grace_generations: Option<&'a str>,
    gc_wall_grace_secs: Option<&'a str>,
    gc_artifact_grace_secs: Option<&'a str>,
    cache_max_logical_bytes: Option<&'a str>,
    cache_max_files: Option<&'a str>,
    response_cache_bytes: Option<&'a str>,
    response_cache_entries: Option<&'a str>,
    decoded_cache_bytes: Option<&'a str>,
    decoded_cache_entries: Option<&'a str>,
    source_scrub_interval_secs: Option<&'a str>,
    cursor_max_views: Option<&'a str>,
    cursor_max_bytes: Option<&'a str>,
    cursor_ttl_secs: Option<&'a str>,
    max_selected_segments: Option<&'a str>,
    cold_max_workers: Option<&'a str>,
    cold_max_queue: Option<&'a str>,
    cold_per_request_parallelism: Option<&'a str>,
    cold_wait_timeout_ms: Option<&'a str>,
    cold_retry_after_secs: Option<&'a str>,
    cold_pgm_bytes: Option<&'a str>,
    cold_decoded_bytes: Option<&'a str>,
    cold_cpu_rows: Option<&'a str>,
    cold_file_descriptors: Option<&'a str>,
    cold_read_bytes: Option<&'a str>,
    cold_write_bytes: Option<&'a str>,
    cold_publications: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedOverviewConfig {
    fallback: FallbackConfig,
    gc: GcConfig,
    response_cache_bytes: usize,
    response_cache_entries: usize,
    decoded_cache_bytes: usize,
    decoded_cache_entries: usize,
    source_scrub_interval: Duration,
    cursor_max_views: usize,
    cursor_max_bytes: usize,
    cursor_ttl: Duration,
    max_selected_segments: usize,
    cold: OverviewColdConfig,
}

fn parse_overview_config(raw: &OverviewConfigRaw<'_>) -> Result<ParsedOverviewConfig, String> {
    let defaults = OverviewConfig::new();
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
    let gc = parse_gc_config(raw, defaults.gc)?;
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
    let decoded_cache_bytes = parse_nonzero_usize(
        raw.decoded_cache_bytes,
        DECODED_CACHE_BYTES_ENV,
        defaults.decoded_cache_bytes,
    )?;
    let decoded_cache_entries = parse_nonzero_usize(
        raw.decoded_cache_entries,
        DECODED_CACHE_ENTRIES_ENV,
        defaults.decoded_cache_entries,
    )?;
    let source_scrub_interval = Duration::from_secs(parse_nonzero_u64(
        raw.source_scrub_interval_secs,
        SOURCE_SCRUB_INTERVAL_ENV,
        defaults.source_scrub_interval.as_secs(),
    )?);
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
    let max_selected_segments = parse_nonzero_usize(
        raw.max_selected_segments,
        MAX_SELECTED_SEGMENTS_ENV,
        defaults.max_selected_segments,
    )?;
    if max_selected_segments > ABSOLUTE_MAX_SELECTED_SEGMENTS {
        return Err(format!(
            "{MAX_SELECTED_SEGMENTS_ENV} must be at most {ABSOLUTE_MAX_SELECTED_SEGMENTS}"
        ));
    }
    let cold = parse_cold_config(raw, defaults.cold)?;
    Ok(ParsedOverviewConfig {
        fallback,
        gc,
        response_cache_bytes,
        response_cache_entries,
        decoded_cache_bytes,
        decoded_cache_entries,
        source_scrub_interval,
        cursor_max_views,
        cursor_max_bytes,
        cursor_ttl: Duration::from_secs(cursor_ttl_secs),
        max_selected_segments,
        cold,
    })
}

fn parse_cold_config(
    raw: &OverviewConfigRaw<'_>,
    defaults: OverviewColdConfig,
) -> Result<OverviewColdConfig, String> {
    Ok(OverviewColdConfig {
        max_workers: parse_nonzero_u32(
            raw.cold_max_workers,
            COLD_MAX_WORKERS_ENV,
            defaults.max_workers,
        )?,
        max_queue: parse_nonzero_usize(raw.cold_max_queue, COLD_MAX_QUEUE_ENV, defaults.max_queue)?,
        per_request_parallelism: parse_nonzero_usize(
            raw.cold_per_request_parallelism,
            COLD_PER_REQUEST_PARALLELISM_ENV,
            defaults.per_request_parallelism,
        )?,
        wait_timeout: Duration::from_millis(parse_nonzero_u64(
            raw.cold_wait_timeout_ms,
            COLD_WAIT_TIMEOUT_MS_ENV,
            u64::try_from(defaults.wait_timeout.as_millis()).unwrap_or(u64::MAX),
        )?),
        retry_after_seconds: parse_nonzero_u64(
            raw.cold_retry_after_secs,
            COLD_RETRY_AFTER_ENV,
            defaults.retry_after_seconds,
        )?,
        pgm_bytes: parse_nonzero_u64(raw.cold_pgm_bytes, COLD_PGM_BYTES_ENV, defaults.pgm_bytes)?,
        decoded_bytes: parse_nonzero_u64(
            raw.cold_decoded_bytes,
            COLD_DECODED_BYTES_ENV,
            defaults.decoded_bytes,
        )?,
        cpu_rows: parse_nonzero_u64(raw.cold_cpu_rows, COLD_CPU_ROWS_ENV, defaults.cpu_rows)?,
        file_descriptors: parse_nonzero_u32(
            raw.cold_file_descriptors,
            COLD_FILE_DESCRIPTORS_ENV,
            defaults.file_descriptors,
        )?,
        read_bytes: parse_nonzero_u64(
            raw.cold_read_bytes,
            COLD_READ_BYTES_ENV,
            defaults.read_bytes,
        )?,
        write_bytes: parse_nonzero_u64(
            raw.cold_write_bytes,
            COLD_WRITE_BYTES_ENV,
            defaults.write_bytes,
        )?,
        publications: parse_nonzero_u32(
            raw.cold_publications,
            COLD_PUBLICATIONS_ENV,
            defaults.publications,
        )?,
    })
}

fn parse_gc_config(raw: &OverviewConfigRaw<'_>, defaults: GcConfig) -> Result<GcConfig, String> {
    let max_entries = parse_nonzero_usize(
        raw.gc_max_entries,
        GC_MAX_ENTRIES_ENV,
        defaults.max_entries(),
    )?;
    let grace_generations = parse_nonzero_u32(
        raw.gc_grace_generations,
        GC_GRACE_GENERATIONS_ENV,
        defaults.grace_generations(),
    )?;
    let wall_grace_secs = parse_nonzero_u64(
        raw.gc_wall_grace_secs,
        GC_WALL_GRACE_ENV,
        defaults.wall_grace().as_secs(),
    )?;
    let artifact_grace_secs = parse_nonzero_u64(
        raw.gc_artifact_grace_secs,
        GC_ARTIFACT_GRACE_ENV,
        defaults.artifact_grace().as_secs(),
    )?;
    let max_logical_bytes =
        parse_optional_nonzero_u64(raw.cache_max_logical_bytes, CACHE_MAX_LOGICAL_BYTES_ENV)?;
    let max_files = parse_optional_nonzero_u64(raw.cache_max_files, CACHE_MAX_FILES_ENV)?;
    GcConfig::new(
        max_entries,
        grace_generations,
        Duration::from_secs(wall_grace_secs),
        Duration::from_secs(artifact_grace_secs),
        max_logical_bytes,
        max_files,
    )
    .map_err(|error| {
        let name = match error {
            GcConfigError::EntryLimit => GC_MAX_ENTRIES_ENV,
            GcConfigError::GenerationGrace => GC_GRACE_GENERATIONS_ENV,
            GcConfigError::Quota => "KRONIKA_WEB_OVERVIEW_CACHE_MAX_LOGICAL_BYTES/CACHE_MAX_FILES",
        };
        format!("{name}: {error}")
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

fn parse_optional_nonzero_u64(raw: Option<&str>, name: &str) -> Result<Option<u64>, String> {
    raw.map(|value| parse_nonzero_u64(Some(value), name, 1))
        .transpose()
}

fn parse_nonzero_u32(raw: Option<&str>, name: &str, default: u32) -> Result<u32, String> {
    let value = parse_nonzero_u64(raw, name, u64::from(default))?;
    u32::try_from(value).map_err(|_error| format!("{name} does not fit u32"))
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
    /// Bounded fallback used only after recoverable durable-publication failure.
    pub overview_fallback: FallbackConfig,
    /// Bounded GC and optional hard durable-cache quota.
    pub overview_gc: GcConfig,
    /// Serialized overview/health response-cache byte ceiling.
    pub overview_response_cache_bytes: usize,
    /// Serialized overview/health response-cache entry ceiling.
    pub overview_response_cache_entries: usize,
    /// Decoded sealed-fact L2 byte ceiling.
    pub overview_decoded_cache_bytes: usize,
    /// Decoded sealed-fact L2 entry ceiling.
    pub overview_decoded_cache_entries: usize,
    /// Streaming source CRC scrub cadence.
    pub overview_source_scrub_interval: Duration,
    /// Maximum simultaneously pinned event views.
    pub overview_cursor_max_views: usize,
    /// Logical-byte ceiling for cursor-pinned event views.
    pub overview_cursor_max_bytes: usize,
    /// Lifetime of an event cursor and its pinned view.
    pub overview_cursor_ttl: Duration,
    /// Effective selected sealed-segment request limit.
    pub overview_max_selected_segments: usize,
    /// Process-wide cold sealed-fact construction policy.
    pub overview_cold: OverviewColdConfig,
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
            .field("overview_fallback", &self.overview_fallback)
            .field("overview_gc", &self.overview_gc)
            .field(
                "overview_response_cache_bytes",
                &self.overview_response_cache_bytes,
            )
            .field(
                "overview_response_cache_entries",
                &self.overview_response_cache_entries,
            )
            .field(
                "overview_decoded_cache_bytes",
                &self.overview_decoded_cache_bytes,
            )
            .field(
                "overview_decoded_cache_entries",
                &self.overview_decoded_cache_entries,
            )
            .field(
                "overview_source_scrub_interval",
                &self.overview_source_scrub_interval,
            )
            .field("overview_cursor_max_views", &self.overview_cursor_max_views)
            .field("overview_cursor_max_bytes", &self.overview_cursor_max_bytes)
            .field("overview_cursor_ttl", &self.overview_cursor_ttl)
            .field(
                "overview_max_selected_segments",
                &self.overview_max_selected_segments,
            )
            .field("overview_cold", &self.overview_cold)
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
            &OverviewConfigRaw::default(),
        )
    }

    fn parse_with_overview(
        dir: &str,
        addr: &str,
        basic_auth_raw: Option<&str>,
        stale_raw: Option<&str>,
        log: Option<&str>,
        overview_raw: &OverviewConfigRaw<'_>,
    ) -> Result<Self, String> {
        if dir.is_empty() {
            return Err("KRONIKA_WEB_DIR must not be empty".to_owned());
        }
        if dir.as_bytes().contains(&0) {
            return Err("KRONIKA_WEB_DIR contains a NUL byte".to_owned());
        }
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
        let overview = parse_overview_config(overview_raw)?;
        Ok(Self {
            dir,
            addr: addr.to_owned(),
            basic_auth,
            stale_after,
            log: log.unwrap_or("info").to_owned(),
            overview_fallback: overview.fallback,
            overview_gc: overview.gc,
            overview_response_cache_bytes: overview.response_cache_bytes,
            overview_response_cache_entries: overview.response_cache_entries,
            overview_decoded_cache_bytes: overview.decoded_cache_bytes,
            overview_decoded_cache_entries: overview.decoded_cache_entries,
            overview_source_scrub_interval: overview.source_scrub_interval,
            overview_cursor_max_views: overview.cursor_max_views,
            overview_cursor_max_bytes: overview.cursor_max_bytes,
            overview_cursor_ttl: overview.cursor_ttl,
            overview_max_selected_segments: overview.max_selected_segments,
            overview_cold: overview.cold,
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
        let fallback_segment_hours = std::env::var(FALLBACK_SEGMENT_HOURS_ENV).ok();
        let fallback_bytes = std::env::var(FALLBACK_BYTES_ENV).ok();
        let gc_max_entries = std::env::var(GC_MAX_ENTRIES_ENV).ok();
        let gc_grace_generations = std::env::var(GC_GRACE_GENERATIONS_ENV).ok();
        let gc_wall_grace_secs = std::env::var(GC_WALL_GRACE_ENV).ok();
        let gc_artifact_grace_secs = std::env::var(GC_ARTIFACT_GRACE_ENV).ok();
        let cache_max_logical_bytes = std::env::var(CACHE_MAX_LOGICAL_BYTES_ENV).ok();
        let cache_max_files = std::env::var(CACHE_MAX_FILES_ENV).ok();
        let response_cache_bytes = std::env::var(RESPONSE_CACHE_BYTES_ENV).ok();
        let response_cache_entries = std::env::var(RESPONSE_CACHE_ENTRIES_ENV).ok();
        let decoded_cache_bytes = std::env::var(DECODED_CACHE_BYTES_ENV).ok();
        let decoded_cache_entries = std::env::var(DECODED_CACHE_ENTRIES_ENV).ok();
        let source_scrub_interval_secs = std::env::var(SOURCE_SCRUB_INTERVAL_ENV).ok();
        let cursor_max_views = std::env::var(CURSOR_MAX_VIEWS_ENV).ok();
        let cursor_max_bytes = std::env::var(CURSOR_MAX_BYTES_ENV).ok();
        let cursor_ttl_secs = std::env::var(CURSOR_TTL_ENV).ok();
        let max_selected_segments = std::env::var(MAX_SELECTED_SEGMENTS_ENV).ok();
        let cold_max_workers = std::env::var(COLD_MAX_WORKERS_ENV).ok();
        let cold_max_queue = std::env::var(COLD_MAX_QUEUE_ENV).ok();
        let cold_per_request_parallelism = std::env::var(COLD_PER_REQUEST_PARALLELISM_ENV).ok();
        let cold_wait_timeout_ms = std::env::var(COLD_WAIT_TIMEOUT_MS_ENV).ok();
        let cold_retry_after_secs = std::env::var(COLD_RETRY_AFTER_ENV).ok();
        let cold_pgm_bytes = std::env::var(COLD_PGM_BYTES_ENV).ok();
        let cold_decoded_bytes = std::env::var(COLD_DECODED_BYTES_ENV).ok();
        let cold_cpu_rows = std::env::var(COLD_CPU_ROWS_ENV).ok();
        let cold_file_descriptors = std::env::var(COLD_FILE_DESCRIPTORS_ENV).ok();
        let cold_read_bytes = std::env::var(COLD_READ_BYTES_ENV).ok();
        let cold_write_bytes = std::env::var(COLD_WRITE_BYTES_ENV).ok();
        let cold_publications = std::env::var(COLD_PUBLICATIONS_ENV).ok();

        Self::parse_with_overview(
            &dir,
            &addr,
            basic_auth_raw.as_deref(),
            stale_raw.as_deref(),
            log.as_deref(),
            &OverviewConfigRaw {
                fallback_segment_hours: fallback_segment_hours.as_deref(),
                fallback_bytes: fallback_bytes.as_deref(),
                gc_max_entries: gc_max_entries.as_deref(),
                gc_grace_generations: gc_grace_generations.as_deref(),
                gc_wall_grace_secs: gc_wall_grace_secs.as_deref(),
                gc_artifact_grace_secs: gc_artifact_grace_secs.as_deref(),
                cache_max_logical_bytes: cache_max_logical_bytes.as_deref(),
                cache_max_files: cache_max_files.as_deref(),
                response_cache_bytes: response_cache_bytes.as_deref(),
                response_cache_entries: response_cache_entries.as_deref(),
                decoded_cache_bytes: decoded_cache_bytes.as_deref(),
                decoded_cache_entries: decoded_cache_entries.as_deref(),
                source_scrub_interval_secs: source_scrub_interval_secs.as_deref(),
                cursor_max_views: cursor_max_views.as_deref(),
                cursor_max_bytes: cursor_max_bytes.as_deref(),
                cursor_ttl_secs: cursor_ttl_secs.as_deref(),
                max_selected_segments: max_selected_segments.as_deref(),
                cold_max_workers: cold_max_workers.as_deref(),
                cold_max_queue: cold_max_queue.as_deref(),
                cold_per_request_parallelism: cold_per_request_parallelism.as_deref(),
                cold_wait_timeout_ms: cold_wait_timeout_ms.as_deref(),
                cold_retry_after_secs: cold_retry_after_secs.as_deref(),
                cold_pgm_bytes: cold_pgm_bytes.as_deref(),
                cold_decoded_bytes: cold_decoded_bytes.as_deref(),
                cold_cpu_rows: cold_cpu_rows.as_deref(),
                cold_file_descriptors: cold_file_descriptors.as_deref(),
                cold_read_bytes: cold_read_bytes.as_deref(),
                cold_write_bytes: cold_write_bytes.as_deref(),
                cold_publications: cold_publications.as_deref(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_overview_raw() -> OverviewConfigRaw<'static> {
        OverviewConfigRaw::default()
    }

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
    }

    #[test]
    fn overview_raw_defaults_match_runtime_overview_defaults() {
        let parsed =
            parse_overview_config(&OverviewConfigRaw::default()).expect("default policy is valid");
        let defaults = OverviewConfig::new();
        let expected = ParsedOverviewConfig {
            fallback: defaults.fallback,
            gc: defaults.gc,
            response_cache_bytes: defaults.response_cache_bytes,
            response_cache_entries: defaults.response_cache_entries,
            decoded_cache_bytes: defaults.decoded_cache_bytes,
            decoded_cache_entries: defaults.decoded_cache_entries,
            source_scrub_interval: defaults.source_scrub_interval,
            cursor_max_views: defaults.cursor_max_views,
            cursor_max_bytes: defaults.cursor_max_bytes,
            cursor_ttl: defaults.cursor_ttl,
            max_selected_segments: defaults.max_selected_segments,
            cold: defaults.cold,
        };
        assert_eq!(parsed, expected);
        assert_eq!(
            parsed.max_selected_segments, 1_024,
            "the normal deployment default is below the absolute v1 ceiling"
        );
    }

    #[test]
    fn selected_segment_policy_accepts_the_documented_boundaries() {
        for (raw, expected) in [("1", 1), ("1024", 1_024), ("4096", 4_096)] {
            let parsed = parse_overview_config(&OverviewConfigRaw {
                max_selected_segments: Some(raw),
                ..valid_overview_raw()
            })
            .expect("documented selected-segment limit is valid");
            assert_eq!(parsed.max_selected_segments, expected);
        }
    }

    #[test]
    fn selected_segment_policy_rejects_zero_ceiling_plus_one_and_platform_overflow() {
        for raw in ["0", "4097", "340282366920938463463374607431768211455"] {
            let error = parse_overview_config(&OverviewConfigRaw {
                max_selected_segments: Some(raw),
                ..valid_overview_raw()
            })
            .expect_err("invalid selected-segment limit must fail");
            assert!(error.contains(MAX_SELECTED_SEGMENTS_ENV), "{error}");
        }
    }

    #[test]
    fn overview_raw_custom_values_reach_web_config() {
        let cfg = WebConfig::parse_with_overview(
            "/data",
            "127.0.0.1:9000",
            None,
            None,
            None,
            &OverviewConfigRaw {
                fallback_segment_hours: Some("48"),
                fallback_bytes: Some("1048576"),
                gc_max_entries: Some("1000"),
                gc_grace_generations: Some("3"),
                gc_wall_grace_secs: Some("180"),
                gc_artifact_grace_secs: Some("900"),
                cache_max_logical_bytes: Some("8388608"),
                cache_max_files: Some("100"),
                response_cache_bytes: Some("2097152"),
                response_cache_entries: Some("128"),
                cursor_max_views: Some("16"),
                cursor_max_bytes: Some("4194304"),
                cursor_ttl_secs: Some("60"),
                max_selected_segments: Some("4096"),
                decoded_cache_bytes: Some("16777216"),
                decoded_cache_entries: Some("64"),
                source_scrub_interval_secs: Some("30"),
                cold_max_workers: Some("2"),
                cold_max_queue: Some("8"),
                cold_per_request_parallelism: Some("2"),
                cold_wait_timeout_ms: Some("250"),
                cold_retry_after_secs: Some("3"),
                cold_pgm_bytes: Some("33554432"),
                cold_decoded_bytes: Some("67108864"),
                cold_cpu_rows: Some("131072"),
                cold_file_descriptors: Some("8"),
                cold_read_bytes: Some("134217728"),
                cold_write_bytes: Some("67108864"),
                cold_publications: Some("2"),
            },
        )
        .expect("custom overview policy is valid");
        assert_eq!(
            cfg.overview_fallback,
            FallbackConfig::new(48, 1_048_576).expect("fixture fallback is valid")
        );
        assert_eq!(
            cfg.overview_gc,
            GcConfig::new(
                1_000,
                3,
                Duration::from_mins(3),
                Duration::from_mins(15),
                Some(8_388_608),
                Some(100),
            )
            .expect("fixture GC policy is valid")
        );
        assert_eq!(cfg.overview_response_cache_bytes, 2_097_152);
        assert_eq!(cfg.overview_response_cache_entries, 128);
        assert_eq!(cfg.overview_decoded_cache_bytes, 16_777_216);
        assert_eq!(cfg.overview_decoded_cache_entries, 64);
        assert_eq!(cfg.overview_source_scrub_interval, Duration::from_secs(30));
        assert_eq!(cfg.overview_cursor_max_views, 16);
        assert_eq!(cfg.overview_cursor_max_bytes, 4_194_304);
        assert_eq!(cfg.overview_cursor_ttl, Duration::from_mins(1));
        assert_eq!(cfg.overview_max_selected_segments, 4_096);
        assert_eq!(
            cfg.overview_cold,
            OverviewColdConfig {
                max_workers: 2,
                max_queue: 8,
                per_request_parallelism: 2,
                wait_timeout: Duration::from_millis(250),
                retry_after_seconds: 3,
                pgm_bytes: 33_554_432,
                decoded_bytes: 67_108_864,
                cpu_rows: 131_072,
                file_descriptors: 8,
                read_bytes: 134_217_728,
                write_bytes: 67_108_864,
                publications: 2,
            }
        );
    }

    #[test]
    fn web_config_debug_redacts_credentials() {
        let cfg = WebConfig::parse_with_overview(
            "/data",
            "127.0.0.1:9000",
            Some("alice:secret-password"),
            None,
            None,
            &OverviewConfigRaw::default(),
        )
        .expect("config with secrets is valid");
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("secret-password"), "{debug}");
        assert!(debug.contains("overview_cursor_max_bytes"), "{debug}");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the table exhaustively checks every independently bounded overview setting"
    )]
    fn overview_raw_rejects_zero_budgets() {
        let cases = [
            (
                FALLBACK_SEGMENT_HOURS_ENV,
                OverviewConfigRaw {
                    fallback_segment_hours: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                FALLBACK_BYTES_ENV,
                OverviewConfigRaw {
                    fallback_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                GC_MAX_ENTRIES_ENV,
                OverviewConfigRaw {
                    gc_max_entries: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                GC_GRACE_GENERATIONS_ENV,
                OverviewConfigRaw {
                    gc_grace_generations: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                GC_WALL_GRACE_ENV,
                OverviewConfigRaw {
                    gc_wall_grace_secs: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                GC_ARTIFACT_GRACE_ENV,
                OverviewConfigRaw {
                    gc_artifact_grace_secs: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                CACHE_MAX_LOGICAL_BYTES_ENV,
                OverviewConfigRaw {
                    cache_max_logical_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                CACHE_MAX_FILES_ENV,
                OverviewConfigRaw {
                    cache_max_files: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                RESPONSE_CACHE_BYTES_ENV,
                OverviewConfigRaw {
                    response_cache_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                RESPONSE_CACHE_ENTRIES_ENV,
                OverviewConfigRaw {
                    response_cache_entries: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                DECODED_CACHE_BYTES_ENV,
                OverviewConfigRaw {
                    decoded_cache_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                DECODED_CACHE_ENTRIES_ENV,
                OverviewConfigRaw {
                    decoded_cache_entries: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                SOURCE_SCRUB_INTERVAL_ENV,
                OverviewConfigRaw {
                    source_scrub_interval_secs: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                CURSOR_MAX_VIEWS_ENV,
                OverviewConfigRaw {
                    cursor_max_views: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                CURSOR_MAX_BYTES_ENV,
                OverviewConfigRaw {
                    cursor_max_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                CURSOR_TTL_ENV,
                OverviewConfigRaw {
                    cursor_ttl_secs: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_MAX_WORKERS_ENV,
                OverviewConfigRaw {
                    cold_max_workers: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_MAX_QUEUE_ENV,
                OverviewConfigRaw {
                    cold_max_queue: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_PER_REQUEST_PARALLELISM_ENV,
                OverviewConfigRaw {
                    cold_per_request_parallelism: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_WAIT_TIMEOUT_MS_ENV,
                OverviewConfigRaw {
                    cold_wait_timeout_ms: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_RETRY_AFTER_ENV,
                OverviewConfigRaw {
                    cold_retry_after_secs: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_PGM_BYTES_ENV,
                OverviewConfigRaw {
                    cold_pgm_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_DECODED_BYTES_ENV,
                OverviewConfigRaw {
                    cold_decoded_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_CPU_ROWS_ENV,
                OverviewConfigRaw {
                    cold_cpu_rows: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_FILE_DESCRIPTORS_ENV,
                OverviewConfigRaw {
                    cold_file_descriptors: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_READ_BYTES_ENV,
                OverviewConfigRaw {
                    cold_read_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_WRITE_BYTES_ENV,
                OverviewConfigRaw {
                    cold_write_bytes: Some("0"),
                    ..valid_overview_raw()
                },
            ),
            (
                COLD_PUBLICATIONS_ENV,
                OverviewConfigRaw {
                    cold_publications: Some("0"),
                    ..valid_overview_raw()
                },
            ),
        ];
        for (name, raw) in cases {
            let error = parse_overview_config(&raw).expect_err("zero budget must fail");
            assert!(error.contains(name), "wrong error for {name}: {error}");
        }
    }

    #[test]
    fn overview_raw_rejects_fallback_hours_above_hard_maximum() {
        let value = (kronika_reader::MAX_FALLBACK_SEGMENT_HOURS + 1).to_string();
        let error = parse_overview_config(&OverviewConfigRaw {
            fallback_segment_hours: Some(&value),
            ..valid_overview_raw()
        })
        .expect_err("fallback hours above the hard maximum must fail");
        assert!(error.contains(FALLBACK_SEGMENT_HOURS_ENV), "{error}");
        assert!(error.contains("hard ceiling"), "{error}");
    }

    #[test]
    fn overview_raw_rejects_fallback_bytes_above_hard_maximum() {
        let value = (kronika_reader::MAX_FALLBACK_BYTES + 1).to_string();
        let error = parse_overview_config(&OverviewConfigRaw {
            fallback_bytes: Some(&value),
            ..valid_overview_raw()
        })
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
    fn overview_policy_has_no_separate_directory_or_identity_input() {
        let first =
            WebConfig::parse("/data", "127.0.0.1:9000", None, None, None).expect("first config");
        let repeated =
            WebConfig::parse("/data", "127.0.0.1:9000", None, None, None).expect("repeated config");
        let isolated = WebConfig::parse("/other", "127.0.0.1:9000", None, None, None)
            .expect("other data directory");
        assert_eq!(first.dir, repeated.dir);
        assert_ne!(first.dir, isolated.dir);
        assert_eq!(
            parse_overview_config(&OverviewConfigRaw::default()),
            parse_overview_config(&OverviewConfigRaw::default())
        );
    }

    #[test]
    fn empty_and_invalid_store_directories_are_rejected_deterministically() {
        let empty = WebConfig::parse("", "127.0.0.1:9000", None, None, None)
            .expect_err("an empty store directory must fail");
        assert_eq!(empty, "KRONIKA_WEB_DIR must not be empty");

        let nul = WebConfig::parse("/data\0other", "127.0.0.1:9000", None, None, None)
            .expect_err("a NUL byte cannot name a filesystem path");
        assert_eq!(nul, "KRONIKA_WEB_DIR contains a NUL byte");
    }

    #[test]
    fn from_env_accepts_the_owned_data_directory_without_extra_storage_inputs() {
        let executable = std::env::current_exe().expect("current test executable");
        let status = std::process::Command::new(&executable)
            .env_clear()
            .env("PGKRONIKA_STARTUP_ENV_CHILD", "1")
            .env("KRONIKA_WEB_DIR", "/data")
            .env("KRONIKA_WEB_ADDR", "127.0.0.1:9000")
            .arg("--exact")
            .arg("startup::tests::from_env_accepts_only_the_owned_data_directory_child")
            .status()
            .expect("run isolated environment probe");
        assert!(status.success(), "minimal startup failed: {status}");
    }

    #[test]
    fn from_env_accepts_only_the_owned_data_directory_child() {
        if std::env::var_os("PGKRONIKA_STARTUP_ENV_CHILD").is_none() {
            return;
        }
        let config = WebConfig::from_env().expect("minimal startup environment");
        assert_eq!(config.dir, PathBuf::from("/data"));
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
