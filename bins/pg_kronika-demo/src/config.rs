//! Stand tunables read from `DEMO_*` environment variables.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Stand profile; every field has a saturation-oriented default.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Root directory for cluster data, segments, fact files, and the report.
    pub(crate) root: PathBuf,
    /// Concurrent OLTP connections.
    pub(crate) backends: u32,
    /// Target OLTP transactions per second across all backends.
    pub(crate) tps: u32,
    /// Extra small tables created to push the collector toward its table cap.
    pub(crate) filler_tables: u32,
    /// Extra indexes spread over the filler tables.
    pub(crate) filler_indexes: u32,
    /// Rows in `staging.large_scan`; ~400 bytes per row.
    pub(crate) large_scan_rows: u32,
    /// Load phase duration.
    pub(crate) duration: Duration,
    /// Chart series count used for the size extrapolation in the report.
    pub(crate) chart_series: u32,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            root: PathBuf::from(env_var("DEMO_ROOT").unwrap_or_else(|| "/data".to_owned())),
            backends: parse_u32("DEMO_BACKENDS", env_var("DEMO_BACKENDS"), 20)?,
            tps: parse_u32("DEMO_TPS", env_var("DEMO_TPS"), 200)?,
            filler_tables: parse_u32("DEMO_TABLES", env_var("DEMO_TABLES"), 300)?,
            filler_indexes: parse_u32("DEMO_INDEXES", env_var("DEMO_INDEXES"), 500)?,
            large_scan_rows: parse_u32(
                "DEMO_LARGE_SCAN_ROWS",
                env_var("DEMO_LARGE_SCAN_ROWS"),
                500_000,
            )?,
            duration: Duration::from_secs(
                u64::from(parse_u32(
                    "DEMO_DURATION_MIN",
                    env_var("DEMO_DURATION_MIN"),
                    30,
                )?) * 60,
            ),
            chart_series: parse_u32("DEMO_CHART_SERIES", env_var("DEMO_CHART_SERIES"), 19)?,
        })
    }
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Parses an optional raw value, falling back to `default` when absent.
fn parse_u32(key: &str, raw: Option<String>, default: u32) -> Result<u32> {
    raw.map_or(Ok(default), |value| {
        value
            .trim()
            .parse::<u32>()
            .with_context(|| format!("{key} must be an unsigned integer, got {value:?}"))
    })
}

/// Fixed directory layout under the stand root.
#[derive(Debug, Clone)]
pub(crate) struct StandPaths {
    /// `PostgreSQL` data directory.
    pub(crate) pgdata: PathBuf,
    /// Tablespace locations.
    pub(crate) tablespaces: [PathBuf; 2],
    /// Collector output directory with `.pgm` segments.
    pub(crate) segments: PathBuf,
    /// Fact-file cache root for built `.ovf` files.
    pub(crate) ovf_cache: PathBuf,
    /// Final JSON report path.
    pub(crate) report: PathBuf,
}

impl StandPaths {
    pub(crate) fn under(root: &Path) -> Self {
        Self {
            pgdata: root.join("pgdata"),
            tablespaces: [root.join("ts_hot"), root.join("ts_cold")],
            segments: root.join("segments"),
            ovf_cache: root.join("ovf-cache"),
            report: root.join("report.json"),
        }
    }

    /// Creates the durable stand directories; `pgdata` and the tablespaces
    /// are throwaway and belong to [`Cluster::boot`](crate::cluster::Cluster).
    pub(crate) fn create(&self) -> Result<()> {
        for dir in [&self.segments, &self.ovf_cache] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create directory {}", dir.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_u32_uses_default_when_absent() {
        assert_eq!(
            parse_u32("K", None, 42).expect("default"),
            42,
            "absent -> default"
        );
    }

    #[test]
    fn parse_u32_accepts_a_trimmed_number() {
        let parsed = parse_u32("K", Some(" 7 ".to_owned()), 42).expect("number parses");
        assert_eq!(parsed, 7, "explicit value wins over the default");
    }

    #[test]
    fn parse_u32_rejects_garbage() {
        assert!(
            parse_u32("K", Some("x".to_owned()), 42).is_err(),
            "non-numeric value is an error, not the default"
        );
    }

    #[test]
    fn stand_paths_nest_under_root() {
        let paths = StandPaths::under(Path::new("/data"));
        assert_eq!(paths.pgdata, Path::new("/data/pgdata"), "pgdata under root");
        assert_eq!(
            paths.report,
            Path::new("/data/report.json"),
            "report under root"
        );
    }
}
