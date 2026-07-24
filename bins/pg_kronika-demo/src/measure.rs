//! Builds `.ovf` fact files from sealed `.pgm` segments and reports sizes.
//!
//! The report answers the sizing question the stand exists for: how big the
//! raw segments are, how big the overview index is, which block dominates,
//! and what adding chart series would cost.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use kronika_analytics::overview::{NamingContractId, SegmentLocator};
use kronika_reader::{
    BlockKind, FactFileReader, FactKey, FactStore, FileKind, LIMIT, PgmUnit, SegmentContext,
};

/// Stored/decoded sizes of one fact-file block.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockStat {
    pub(crate) kind_code: u32,
    pub(crate) stored_len: u64,
    pub(crate) decoded_len: u64,
    pub(crate) item_count: u32,
    pub(crate) min_ts_us: i64,
    pub(crate) max_ts_us: i64,
}

/// Sizes for one sealed segment and its fact file.
#[derive(Debug, Clone)]
pub(crate) struct SegmentReport {
    pub(crate) name: String,
    pub(crate) pgm_bytes: u64,
    pub(crate) ovf_bytes: u64,
    pub(crate) blocks: Vec<BlockStat>,
}

/// Chart cost estimate from the observed gauge block, spec §2.4.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChartExtrapolation {
    pub(crate) extra_series: u32,
    pub(crate) snapshots: u64,
    pub(crate) decoded_bytes_per_sample: f64,
    pub(crate) zstd_ratio: f64,
    pub(crate) extra_stored_bytes: u64,
}

/// The complete measurement outcome.
#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub(crate) segments: Vec<SegmentReport>,
    pub(crate) extrapolation: Option<ChartExtrapolation>,
}

/// Builds fact files for every sealed segment under `segments` and measures
/// both sides.
pub(crate) fn measure(segments: &Path, cache_root: &Path, chart_series: u32) -> Result<Report> {
    let store = FactStore::new(cache_root);
    let mut reports = Vec::new();
    for pgm_path in sealed_segments(segments)? {
        reports.push(measure_segment(&store, cache_root, &pgm_path)?);
    }
    ensure!(
        !reports.is_empty(),
        "no sealed .pgm segments under {}; run the stand longer than one segment age",
        segments.display()
    );
    let extrapolation = extrapolate_charts(&summed_gauge_block(&reports), chart_series);
    Ok(Report {
        segments: reports,
        extrapolation,
    })
}

/// Sealed segment files, name-ordered; `active.parts` is not a segment.
fn sealed_segments(segments: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let entries = std::fs::read_dir(segments)
        .with_context(|| format!("list segments under {}", segments.display()))?;
    for entry in entries {
        let path = entry.context("read a segments directory entry")?.path();
        if path.extension().is_some_and(|ext| ext == "pgm") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn measure_segment(store: &FactStore, cache_root: &Path, pgm_path: &Path) -> Result<SegmentReport> {
    let name = pgm_path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("segment name is not UTF-8: {}", pgm_path.display()))?
        .to_owned();
    let pgm_bytes = std::fs::metadata(pgm_path)
        .with_context(|| format!("stat {}", pgm_path.display()))?
        .len();

    let file = File::open(pgm_path).with_context(|| format!("open {}", pgm_path.display()))?;
    let unit = PgmUnit::open(file).with_context(|| format!("parse {}", pgm_path.display()))?;
    let context = SegmentContext::new(
        b"demo-stand".to_vec(),
        NamingContractId([0; 16]),
        segment_locator(&name),
    )
    .context("build the segment context")?;
    let load = store
        .load_or_build(&unit, &context, &LIMIT)
        .with_context(|| format!("build facts for {name}"))?;
    if let Some(persist_error) = load.persist_error() {
        anyhow::bail!("facts for {name} were not persisted: {persist_error:?}");
    }

    let facts = load.facts();
    let key = FactKey::for_identity(facts.identity(), FileKind::SegmentFacts);
    let ovf_path = find_fact_file(cache_root, &key)
        .with_context(|| format!("locate the fact file for {name}"))?;
    let ovf_bytes = std::fs::metadata(&ovf_path)
        .with_context(|| format!("stat {}", ovf_path.display()))?
        .len();

    let ovf_file = File::open(&ovf_path).with_context(|| format!("open {}", ovf_path.display()))?;
    let reader = FactFileReader::open(ovf_file, facts.identity(), &LIMIT)
        .map_err(|error| anyhow::anyhow!("read the fact file for {name}: {error:?}"))?;
    let blocks = reader
        .directory()
        .iter()
        .map(|entry| BlockStat {
            kind_code: entry.block_kind,
            stored_len: entry.stored_len,
            decoded_len: entry.decoded_len,
            item_count: entry.item_count,
            min_ts_us: entry.min_ts_us,
            max_ts_us: entry.max_ts_us,
        })
        .collect();

    Ok(SegmentReport {
        name,
        pgm_bytes,
        ovf_bytes,
        blocks,
    })
}

/// A deterministic per-segment locator derived from the file name.
fn segment_locator(name: &str) -> SegmentLocator {
    let mut locator = [0_u8; 32];
    for (index, byte) in name.bytes().enumerate() {
        locator[index % 32] = locator[index % 32].wrapping_mul(31).wrapping_add(byte);
    }
    locator[31] = locator[31].wrapping_add(u8::try_from(name.len() % 251).unwrap_or(0));
    SegmentLocator(locator)
}

/// Finds `<key>.ovf` under the cache; the key hex is globally unique.
fn find_fact_file(cache_root: &Path, key: &FactKey) -> Result<PathBuf> {
    let wanted = format!("{}.ovf", key.hex());
    let mut stack = vec![cache_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy() == *wanted)
            {
                return Ok(path);
            }
        }
    }
    anyhow::bail!("{wanted} not found under {}", cache_root.display())
}

/// Sums gauge blocks across segments for a stand-wide extrapolation basis.
fn summed_gauge_block(segments: &[SegmentReport]) -> BlockStat {
    let mut total = BlockStat {
        kind_code: BlockKind::GaugeSamples.code(),
        stored_len: 0,
        decoded_len: 0,
        item_count: 0,
        min_ts_us: i64::MAX,
        max_ts_us: i64::MIN,
    };
    for block in segments
        .iter()
        .flat_map(|segment| &segment.blocks)
        .filter(|block| block.kind_code == BlockKind::GaugeSamples.code())
    {
        total.stored_len += block.stored_len;
        total.decoded_len += block.decoded_len;
        total.item_count = total.item_count.saturating_add(block.item_count);
        total.min_ts_us = total.min_ts_us.min(block.min_ts_us);
        total.max_ts_us = total.max_ts_us.max(block.max_ts_us);
    }
    total
}

/// Chart cost from observed gauge economics: `extra_series` new series, one
/// sample per observed snapshot, stored at the observed ZSTD ratio.
///
/// Returns `None` when the stand produced no gauge samples to price against.
fn extrapolate_charts(gauge: &BlockStat, extra_series: u32) -> Option<ChartExtrapolation> {
    if gauge.item_count == 0 || gauge.decoded_len == 0 || gauge.max_ts_us < gauge.min_ts_us {
        return None;
    }
    let span_us = gauge.max_ts_us.checked_sub(gauge.min_ts_us)?;
    // Collector gauge cadence is one snapshot per KRONIKA_INTERVAL_S (5 s
    // default); derive the observed cadence instead of trusting the default.
    let snapshots = distinct_snapshots(span_us, gauge.item_count);
    let decoded_bytes_per_sample =
        approx_ratio(gauge.decoded_len, u64::from(gauge.item_count.max(1)));
    let zstd_ratio = approx_ratio(gauge.stored_len, gauge.decoded_len.max(1));
    let extra_decoded =
        decoded_bytes_per_sample * snapshots_f64(snapshots) * f64::from(extra_series);
    let extra_stored_bytes = to_bytes(extra_decoded * zstd_ratio);
    Some(ChartExtrapolation {
        extra_series,
        snapshots,
        decoded_bytes_per_sample,
        zstd_ratio,
        extra_stored_bytes,
    })
}

/// Default collector gauge cadence, one snapshot per tick.
const GAUGE_CADENCE_S: u64 = 5;

/// Conservative snapshot count: the samples cannot be denser than one item
/// per timestamp, and a series is sampled at most once per snapshot.
fn distinct_snapshots(span_us: i64, item_count: u32) -> u64 {
    let whole_seconds = u64::try_from(span_us / 1_000_000).unwrap_or(0);
    let by_cadence = whole_seconds / GAUGE_CADENCE_S + 1;
    by_cadence.min(u64::from(item_count))
}

fn approx_ratio(numerator: u64, denominator: u64) -> f64 {
    let numerator = u32::try_from(numerator.min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
    let denominator = u32::try_from(denominator.clamp(1, u64::from(u32::MAX))).unwrap_or(u32::MAX);
    f64::from(numerator) / f64::from(denominator)
}

fn snapshots_f64(snapshots: u64) -> f64 {
    f64::from(u32::try_from(snapshots.min(u64::from(u32::MAX))).unwrap_or(u32::MAX))
}

fn to_bytes(value: f64) -> u64 {
    if value.is_finite() && value >= 0.0 {
        let clamped = value.min(9_007_199_254_740_992.0); // 2^53 keeps the cast exact.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked non-negative finite value clamped below 2^53"
        )]
        {
            clamped as u64
        }
    } else {
        0
    }
}

/// The canonical block name, or the raw code when the container is newer
/// than this tool.
fn block_label(code: u32) -> String {
    BlockKind::from_code(code)
        .map_or_else(|| format!("Unknown({code})"), |kind| format!("{kind:?}"))
}

/// Human-readable size: bytes with a MiB/KiB suffix.
fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", approx_ratio(bytes, 1024 * 1024))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", approx_ratio(bytes, 1024))
    } else {
        format!("{bytes} B")
    }
}

/// Renders the report as the stand's stdout summary.
pub(crate) fn render(report: &Report) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut pgm_total = 0_u64;
    let mut ovf_total = 0_u64;
    let _ = writeln!(out, "== segment sizes ==");
    for segment in &report.segments {
        pgm_total += segment.pgm_bytes;
        ovf_total += segment.ovf_bytes;
        let _ = writeln!(
            out,
            "{}: pgm {} -> ovf {} ({:.1}%)",
            segment.name,
            human(segment.pgm_bytes),
            human(segment.ovf_bytes),
            100.0 * approx_ratio(segment.ovf_bytes, segment.pgm_bytes.max(1)),
        );
        for block in &segment.blocks {
            if block.stored_len == 0 {
                continue;
            }
            let _ = writeln!(
                out,
                "  {:>18}: stored {:>10} decoded {:>10} items {}",
                block_label(block.kind_code),
                human(block.stored_len),
                human(block.decoded_len),
                block.item_count,
            );
        }
    }
    let _ = writeln!(
        out,
        "total: pgm {} -> ovf {} ({:.1}%)",
        human(pgm_total),
        human(ovf_total),
        100.0 * approx_ratio(ovf_total, pgm_total.max(1)),
    );
    if let Some(extra) = &report.extrapolation {
        let with_charts = ovf_total + extra.extra_stored_bytes;
        let _ = writeln!(
            out,
            "charts +{} series over {} snapshots: +{} stored ({:.1} B/sample decoded, zstd x{:.2}) -> ovf {} ({:.1}% of pgm)",
            extra.extra_series,
            extra.snapshots,
            human(extra.extra_stored_bytes),
            extra.decoded_bytes_per_sample,
            extra.zstd_ratio,
            human(with_charts),
            100.0 * approx_ratio(with_charts, pgm_total.max(1)),
        );
    } else {
        let _ = writeln!(out, "charts extrapolation: no gauge samples observed");
    }
    out
}

/// The report as machine-readable JSON for `report.json`.
pub(crate) fn to_json(report: &Report) -> serde_json::Value {
    let segments: Vec<serde_json::Value> = report
        .segments
        .iter()
        .map(|segment| {
            let blocks: Vec<serde_json::Value> = segment
                .blocks
                .iter()
                .map(|block| {
                    serde_json::json!({
                        "kind": block_label(block.kind_code),
                        "stored_len": block.stored_len,
                        "decoded_len": block.decoded_len,
                        "item_count": block.item_count,
                    })
                })
                .collect();
            serde_json::json!({
                "name": segment.name,
                "pgm_bytes": segment.pgm_bytes,
                "ovf_bytes": segment.ovf_bytes,
                "blocks": blocks,
            })
        })
        .collect();
    let extrapolation = report.extrapolation.as_ref().map(|extra| {
        serde_json::json!({
            "extra_series": extra.extra_series,
            "snapshots": extra.snapshots,
            "decoded_bytes_per_sample": extra.decoded_bytes_per_sample,
            "zstd_ratio": extra.zstd_ratio,
            "extra_stored_bytes": extra.extra_stored_bytes,
        })
    });
    serde_json::json!({
        "segments": segments,
        "extrapolation": extrapolation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauge(stored: u64, decoded: u64, items: u32, span_s: i64) -> BlockStat {
        BlockStat {
            kind_code: BlockKind::GaugeSamples.code(),
            stored_len: stored,
            decoded_len: decoded,
            item_count: items,
            min_ts_us: 1_000_000,
            max_ts_us: 1_000_000 + span_s * 1_000_000,
        }
    }

    #[test]
    fn extrapolation_prices_series_at_observed_economics() {
        // 900 s span -> 181 snapshots; 24 B/sample decoded; zstd halves it.
        let extra =
            extrapolate_charts(&gauge(12_000, 24_000, 1_000, 900), 19).expect("gauge data present");
        assert_eq!(extra.snapshots, 181, "900 s at 5 s cadence");
        // 24 B/sample x 181 snapshots x 19 series x 0.5 zstd = 41268 B.
        assert_eq!(
            extra.extra_stored_bytes, 41_268,
            "series x snapshots x B/sample x zstd"
        );
    }

    #[test]
    fn extrapolation_needs_gauge_samples() {
        assert!(
            extrapolate_charts(&gauge(0, 0, 0, 900), 19).is_none(),
            "no samples -> no extrapolation"
        );
    }

    #[test]
    fn snapshots_never_exceed_item_count() {
        assert_eq!(
            distinct_snapshots(900 * 1_000_000, 7),
            7,
            "items bound the cadence estimate"
        );
    }

    #[test]
    fn summed_gauge_block_filters_and_accumulates() {
        let counter = BlockStat {
            kind_code: BlockKind::CounterSamples.code(),
            stored_len: 999,
            decoded_len: 999,
            item_count: 9,
            min_ts_us: 0,
            max_ts_us: 0,
        };
        let segment = |gauge_block: BlockStat| SegmentReport {
            name: "s.pgm".to_owned(),
            pgm_bytes: 0,
            ovf_bytes: 0,
            blocks: vec![counter, gauge_block],
        };
        let total = summed_gauge_block(&[
            segment(gauge(100, 200, 10, 100)),
            segment(gauge(50, 100, 5, 100)),
        ]);
        assert_eq!(
            total.stored_len, 150,
            "gauge stored bytes sum, counters excluded"
        );
        assert_eq!(total.decoded_len, 300, "gauge decoded bytes sum");
        assert_eq!(total.item_count, 15, "gauge items sum");
        assert_eq!(total.min_ts_us, 1_000_000, "min over gauge blocks only");
    }

    #[test]
    fn summed_gauge_block_of_nothing_yields_no_extrapolation() {
        let total = summed_gauge_block(&[]);
        assert!(
            total.max_ts_us < total.min_ts_us,
            "empty input keeps the sentinel range"
        );
        assert!(
            extrapolate_charts(&total, 19).is_none(),
            "sentinel range must not extrapolate"
        );
    }

    #[test]
    fn locators_differ_for_different_segment_names() {
        assert_ne!(
            segment_locator("143000.pgm").0,
            segment_locator("143900.pgm").0,
            "distinct names must map to distinct locators"
        );
    }

    #[test]
    fn block_label_names_known_and_unknown_codes() {
        assert_eq!(
            block_label(BlockKind::GaugeSamples.code()),
            "GaugeSamples",
            "known code"
        );
        assert_eq!(block_label(0), "Unknown(0)", "unknown code stays visible");
    }

    #[test]
    fn human_sizes_pick_the_right_unit() {
        assert_eq!(human(512), "512 B", "bytes stay bytes");
        assert_eq!(human(2048), "2.0 KiB", "KiB for kilobytes");
        assert_eq!(human(3 * 1024 * 1024), "3.0 MiB", "MiB for megabytes");
    }

    #[test]
    fn render_reports_totals_and_extrapolation() {
        let report = Report {
            segments: vec![SegmentReport {
                name: "a.pgm".to_owned(),
                pgm_bytes: 10 * 1024 * 1024,
                ovf_bytes: 1024 * 1024,
                blocks: vec![gauge(12_000, 24_000, 1_000, 900)],
            }],
            extrapolation: extrapolate_charts(&gauge(12_000, 24_000, 1_000, 900), 19),
        };
        let rendered = render(&report);
        assert!(
            rendered.contains("total: pgm 10.0 MiB -> ovf 1.0 MiB (10.0%)"),
            "{rendered}"
        );
        assert!(rendered.contains("charts +19 series"), "{rendered}");
    }
}
