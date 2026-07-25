//! Normative operational metric inventory for the overview data path.
//!
//! Metric labels are emitted only from closed enums or bounded source/factor
//! inventories. Descriptions are installed when the router is built so a
//! recorder receives the complete metric catalog without synthetic samples.

#[derive(Clone, Copy)]
enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Clone, Copy)]
struct MetricDescriptor {
    name: &'static str,
    kind: MetricKind,
    help: &'static str,
}

const OPERATIONAL_METRICS: &[MetricDescriptor] = &[
    counter(
        "overview_fact_lookup_total",
        "Fact and response lookups by bounded layer, result, and reason.",
    ),
    counter(
        "overview_fact_build_total",
        "Cold fact builds by result and source type.",
    ),
    histogram(
        "overview_fact_build_seconds",
        "Elapsed seconds spent building one cold fact set.",
    ),
    counter(
        "overview_fact_read_bytes",
        "Stored durable fact bytes read.",
    ),
    counter(
        "overview_fact_write_bytes",
        "Stored durable fact bytes written.",
    ),
    counter(
        "overview_pgm_body_read_bytes",
        "Stored PGM section-body bytes read during fact extraction.",
    ),
    counter(
        "overview_pgm_sections_decoded",
        "PGM section bodies decoded during fact extraction.",
    ),
    gauge(
        "overview_cache_mode",
        "One-hot persistent-cache mode and reason.",
    ),
    gauge("overview_cache_entries", "Entries by bounded cache class."),
    gauge("overview_cache_bytes", "Bytes by bounded cache class."),
    counter(
        "overview_cache_evictions_total",
        "Cache evictions by bounded class and reason.",
    ),
    counter(
        "overview_persist_failures_total",
        "Persistent-cache failures by bounded reason.",
    ),
    gauge(
        "overview_persist_backoff_seconds",
        "Current persistent-cache retry backoff in seconds.",
    ),
    counter(
        "overview_singleflight_builds",
        "Single-flight leader builds started.",
    ),
    counter(
        "overview_singleflight_waiters",
        "Requests joined to an existing single-flight build.",
    ),
    gauge(
        "overview_cold_work_inflight",
        "Cold work currently admitted by bounded work kind.",
    ),
    gauge(
        "overview_cold_queue_depth",
        "Requests currently waiting for cold-work admission.",
    ),
    counter(
        "overview_cold_reject_total",
        "Cold-work admission rejections by bounded reason.",
    ),
    gauge(
        "overview_open_files",
        "File descriptors charged to admitted cold work.",
    ),
    gauge(
        "overview_inflight_bytes",
        "Bytes charged to admitted cold work by bounded class.",
    ),
    gauge(
        "overview_live_state",
        "One-hot live-index state and bounded reason.",
    ),
    counter(
        "overview_live_folded_parts_total",
        "Journal parts folded into the live index.",
    ),
    gauge(
        "overview_live_data_through_us",
        "Latest live-index watermark in Unix microseconds.",
    ),
    gauge(
        "overview_live_visibility_lag_seconds",
        "Wall-clock lag behind the live-index watermark.",
    ),
    gauge(
        "overview_view_generation",
        "Currently published immutable overview generation.",
    ),
    gauge(
        "overview_cursor_views",
        "Immutable views pinned by timeline cursors.",
    ),
    gauge(
        "overview_cursor_view_bytes",
        "Logical bytes charged to cursor-pinned views.",
    ),
    counter(
        "overview_cursor_expired_total",
        "Cursor views removed by bounded expiry reason.",
    ),
    counter(
        "overview_source_failures_total",
        "Source fact-load failures by bounded reason.",
    ),
    counter(
        "overview_coverage_loss_total",
        "Proven factor-coverage loss by bounded source, factor, and reason.",
    ),
    counter(
        "overview_retained_observations_total",
        "Retained observations by stable event kind.",
    ),
    counter(
        "overview_overflow_total",
        "Checked bound or arithmetic overflows by bounded kind.",
    ),
    counter(
        "overview_raw_fallback_total",
        "Raw PGM fallbacks by bounded rebuild reason.",
    ),
    counter(
        "overview_gc_files_total",
        "Cache files processed by bounded GC action and reason.",
    ),
    counter(
        "overview_gc_bytes_total",
        "Cache bytes processed by bounded GC action.",
    ),
];

const fn counter(name: &'static str, help: &'static str) -> MetricDescriptor {
    MetricDescriptor {
        name,
        kind: MetricKind::Counter,
        help,
    }
}

const fn gauge(name: &'static str, help: &'static str) -> MetricDescriptor {
    MetricDescriptor {
        name,
        kind: MetricKind::Gauge,
        help,
    }
}

const fn histogram(name: &'static str, help: &'static str) -> MetricDescriptor {
    MetricDescriptor {
        name,
        kind: MetricKind::Histogram,
        help,
    }
}

pub(crate) fn describe_operational_metrics() {
    for descriptor in OPERATIONAL_METRICS {
        match descriptor.kind {
            MetricKind::Counter => metrics::describe_counter!(descriptor.name, descriptor.help),
            MetricKind::Gauge => metrics::describe_gauge!(descriptor.name, descriptor.help),
            MetricKind::Histogram => {
                metrics::describe_histogram!(descriptor.name, descriptor.help);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const EMISSION_SOURCES: &str = concat!(
        include_str!("admission.rs"),
        include_str!("cache.rs"),
        include_str!("cursor.rs"),
        include_str!("live.rs"),
        include_str!("loader.rs"),
        include_str!("memory_cache.rs"),
        include_str!("resilience.rs"),
        include_str!("singleflight.rs"),
        include_str!("../lib.rs"),
    );

    #[test]
    fn normative_inventory_is_unique_and_every_metric_has_an_emitter() {
        let expected = BTreeSet::from([
            "overview_fact_lookup_total",
            "overview_fact_build_total",
            "overview_fact_build_seconds",
            "overview_fact_read_bytes",
            "overview_fact_write_bytes",
            "overview_pgm_body_read_bytes",
            "overview_pgm_sections_decoded",
            "overview_cache_mode",
            "overview_cache_entries",
            "overview_cache_bytes",
            "overview_cache_evictions_total",
            "overview_persist_failures_total",
            "overview_persist_backoff_seconds",
            "overview_singleflight_builds",
            "overview_singleflight_waiters",
            "overview_cold_work_inflight",
            "overview_cold_queue_depth",
            "overview_cold_reject_total",
            "overview_open_files",
            "overview_inflight_bytes",
            "overview_live_state",
            "overview_live_folded_parts_total",
            "overview_live_data_through_us",
            "overview_live_visibility_lag_seconds",
            "overview_view_generation",
            "overview_cursor_views",
            "overview_cursor_view_bytes",
            "overview_cursor_expired_total",
            "overview_source_failures_total",
            "overview_coverage_loss_total",
            "overview_retained_observations_total",
            "overview_overflow_total",
            "overview_raw_fallback_total",
            "overview_gc_files_total",
            "overview_gc_bytes_total",
        ]);
        let actual = OPERATIONAL_METRICS
            .iter()
            .map(|descriptor| descriptor.name)
            .collect::<BTreeSet<_>>();

        assert_eq!(actual.len(), OPERATIONAL_METRICS.len());
        assert_eq!(actual, expected);
        for descriptor in OPERATIONAL_METRICS {
            assert!(!descriptor.help.is_empty());
            assert!(
                EMISSION_SOURCES.contains(&format!("\"{}\"", descriptor.name)),
                "{} is described but has no production emitter",
                descriptor.name
            );
        }
    }
}
